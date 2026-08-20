//! Agent 对话循环（actor 模式：消息进、事件出）
//!
//! # 模块导读
//! 这是 core 对 TUI 的唯一接口层，采用 actor 模式：
//! - TUI 通过 [`AgentHandle::send`] 发 [`Cmd`]（聊天 / 清空历史）；
//! - agent 在后台任务里跑对话循环，通过 [`AgentHandle::next`] 推 [`Evt`]
//!   （正文增量 / 思考增量 / 工具调用 / 结束 / 错误 / token 估算）。
//!
//! # 核心概念
//! - `mpsc` channel（multi-producer single-consumer）≈ C# 的
//!   `Channel<T>` / BlockingCollection：跨任务传消息，天然免锁。
//! - `tokio::spawn` ≈ `Task.Run`：把 async 块丢进 runtime 后台跑。
//! - `move` 闭包：把捕获变量的**所有权**移交进新任务（≈ C# 闭包捕获，
//!   但 Rust 必须显式声明谁拥有这些变量，编译器据此保证线程安全）。
//!
//! # 对话循环
//! 每轮 Chat：user 消息入列 → 调 API → 若模型返回 tool_calls，
//! 就地执行工具、结果入列、继续调 API，直到模型只回正文为止。

use crate::api::{self, Reply};
use crate::commands::{self, ApprovedCommand};
use crate::config::Config;
use crate::tools;
use crate::workspace::Workspace;
use serde_json::{json, Value};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

// ToolCall 再导出，外部从 core::ToolCall 拿（经 lib.rs 二次再导出）
pub use crate::api::ToolCall;

/// system prompt：控制在 200 token 内。
/// 估算口径：CJK 按 cl100k 类分词器 ≈1 token/字、ASCII 按 chars/4 粗估——
/// 本文 201 字符（118 CJK + 83 ASCII）≈ 140 token，明显低于上限。
/// 内容 = 一句话角色 + 工具纪律 + 维护 HANDOFF.md 的义务。
pub const SYSTEM_PROMPT: &str = "你是 DoAgent，极简编程助手。用内建工具(read/write/edit/ls/grep)读写代码，无自由 shell；批准的固定命令用 runcmd 列出与执行，新命令用 addcmd 提案（人类批准后生效）；改完代码用 runcmd 跑构建拿反馈。始终维护工作区根的 HANDOFF.md（目标/进展/决策/下一步），有进展即更新；新对话开始若它存在，先 read 续接。回答简洁，直接行动。";

/// 单轮对话的工具往返上限（= 最大 API 调用次数），防模型陷入
/// "调工具 → 再调工具"的死循环。16 是经验值，没有强理由：
/// 足够完成正常的多步任务，失控时又能及时止损。
/// 打满不是静默结束——见 chat_round 末尾的兜底注入。
const MAX_TOOL_ROUNDS: usize = 16;

/// TUI 发给 agent 的命令
pub enum Cmd {
    /// 用户输入的一句话
    Chat(String),
    /// /new：清空历史（不注入 HANDOFF.md——system prompt 已要求
    /// AI 新对话开始时自行 read 续接，注入只是白费 token）
    Reset,
    /// Esc：中断当前轮。不走消息队列（actor 正忙时读不到队列），
    /// 而是直接置位共享的取消标志（见 AgentHandle.cancel）
    Cancel,
    /// 系统通知：以 user 角色注入消息历史（不触发 API 调用）。
    /// 用途：/addcmd 批准后立刻让模型知道新命令可用
    Notify(String),
}

/// agent 推给 TUI 的事件
pub enum Evt {
    /// 正文增量（流式）
    Text(String),
    /// 思考增量（流式）
    Reasoning(String),
    /// 一次工具调用即将派发（进行中态；TUI 立即显示，不等结果）
    ToolStart { name: String, args: String },
    /// 一次工具调用完成（附截断后的结果；更新 ToolStart 创建的块）
    Tool {
        name: String,
        args: String,
        result: String,
    },
    /// AI 提交了一条命令提案（addcmd），等待人类 /addcmd 审批
    Proposal(ApprovedCommand),
    /// 本轮对话结束
    Done,
    /// 出错（网络/协议/工具外的错误）
    Error(String),
    /// 当前上下文 token 粗估（chars/4）
    Tokens(usize),
}

/// agent 的 TUI 侧句柄：拿着它就能发命令、收事件。
/// struct 里一个发送端一个接收端——通道两端分开是 mpsc 的标准用法。
pub struct AgentHandle {
    tx: mpsc::UnboundedSender<Cmd>,
    rx: mpsc::UnboundedReceiver<Evt>,
    /// 取消标志，TUI 与 agent 各持一份。
    /// Arc<AtomicBool> ≈ C# volatile bool 的跨线程共享版：
    /// Arc ≈ 共享引用（≈ C# 对象引用天然共享），AtomicBool 保证并发读写
    /// 不撕裂。Ordering 用 Relaxed 即可——它是纯标志位，没有"看到 true
    /// 后还必须看到另一块数据"的依赖关系，不需要更强的内存序。
    cancel: Arc<AtomicBool>,
}

impl AgentHandle {
    /// 以 `root` 为工作区启动 agent 后台任务。
    pub fn start(root: &Path) -> io::Result<AgentHandle> {
        let ws = Workspace::new(root)?;
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Cmd>();
        let (evt_tx, evt_rx) = mpsc::unbounded_channel::<Evt>();
        let cancel = Arc::new(AtomicBool::new(false));
        // clone() 克隆的是 Arc（引用计数 +1），两侧指向同一个 AtomicBool
        let actor_cancel = cancel.clone();
        // `move` 把 ws/cmd_rx/evt_tx 的所有权移进后台任务；
        // tokio::spawn 类似 Task.Run，但要求被捕获的东西满足 Send。
        tokio::spawn(async move {
            actor_loop(ws, cmd_rx, evt_tx, actor_cancel).await;
        });
        Ok(AgentHandle { tx: cmd_tx, rx: evt_rx, cancel })
    }

    /// 发命令（不阻塞；agent 正忙时消息排队）
    pub fn send(&self, cmd: Cmd) {
        match cmd {
            // Cancel 走原子标志直接置位——actor 正在 chat_round 里跑，
            // 读不到命令队列，等它回来再处理就太迟了
            Cmd::Cancel => self.cancel.store(true, Ordering::Relaxed),
            // 接收端已关闭时 send 会失败——agent 不在了，静默丢弃即可
            other => {
                let _ = self.tx.send(other);
            }
        }
    }

    /// 取消标志当前值（测试与调试用）
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// 等下一个事件（agent 关闭时返回 None，≈ C# 的 await ReceiveAsync）
    pub async fn next(&mut self) -> Option<Evt> {
        self.rx.recv().await
    }
}

/// actor 主循环：顺序处理每条命令。
/// 持有全部可变状态（消息历史、配置），所以天然不需要任何锁——
/// 这就是 actor 模式的核心收益：状态只有一个所有者。
async fn actor_loop(
    ws: Workspace,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    evt: mpsc::UnboundedSender<Evt>,
    cancel: Arc<AtomicBool>,
) {
    let defs = tools::defs();
    let mut messages: Vec<Value> = vec![json!({
        "role": "system",
        "content": SYSTEM_PROMPT,
    })];
    // 审计日志（exe 旁 do.audit.jsonl）；不可写时整体降级关闭
    let mut audit = crate::audit::Audit::new(ws.root());

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Cmd::Reset => {
                messages.truncate(1); // 只留 system prompt
                let _ = evt.send(Evt::Tokens(estimate_tokens(&messages)));
            }
            Cmd::Chat(text) => {
                // 审计：用户输入（在 core 统一记录，TUI 不插手）
                audit.log("input", json!({ "text": text }));
                // 新一轮开始前清掉上一轮可能残留的取消标志
                cancel.store(false, Ordering::Relaxed);
                messages.push(json!({"role": "user", "content": text}));
                chat_round(&ws, &defs, &mut messages, &evt, &cancel, &mut audit).await;
            }
            // 系统通知（如"命令已获批准"）：以 user 角色注入历史，不触发 API
            Cmd::Notify(text) => {
                messages.push(json!({"role": "user", "content": text}));
            }
            // Cancel 在 send 里已直接置位，队列里不会收到；穷尽匹配所需
            Cmd::Cancel => {}
        }
    }
}

/// 一轮完整对话：可能包含多次 API 调用（工具往返）。
///
/// # 缓存论点（为什么 tools 数组冻结为固定 7 个）
/// prompt 缓存按请求**前缀**匹配，前缀顺序是 system + tools + messages。
/// 若把批准的命令动态注入 tools，/addcmd 每批准一条前缀就变一次，
/// 之前攒下的缓存全部击穿。冻结 tools 后批准动作零缓存代价；
/// 白名单内容走 messages（runcmd 的工具结果）——messages 本来就逐轮
/// 增长，不造成额外 miss。
async fn chat_round(
    ws: &Workspace,
    defs: &[tools::ToolDef],
    messages: &mut Vec<Value>,
    evt: &mpsc::UnboundedSender<Evt>,
    cancel: &AtomicBool,
    audit: &mut crate::audit::Audit,
) {
    // HTTP client 一轮对话只建一次：内部是连接池 + TLS 会话缓存，
    // 工具往返的多次 API 调用复用它（≈ C# 复用 HttpClient 的纪律）
    let client = reqwest::Client::new();
    // 生效配置同样一轮加载一次、整轮复用（原先在循环体内：单轮最多
    // 16×2 次磁盘读取，且中途改配置会导致同一轮内前后请求的
    // model/url 不一致）。语义：配置改动从下一轮对话开始生效。
    let cfg = Config::load_merged(ws.root(), crate::config::exe_dir().as_deref());
    // 区分循环的两种出口：break 是正常结束，走到底是打满上限
    let mut exhausted = true;
    for _ in 0..MAX_TOOL_ROUNDS {
        // 回调把流式增量就地转成事件推给 TUI。
        // announced 记录哪些 tool_call 已在流式中段宣告过（ToolBegin），
        // 供下方派发阶段判重——同一次调用不许出两个 doing 块
        let mut announced = std::collections::HashSet::new();
        let reply = api::chat(&client, &cfg, messages, defs, cancel, |d| {
            let _ = match d {
                api::Delta::Text(t) => evt.send(Evt::Text(t)),
                api::Delta::Reasoning(r) => evt.send(Evt::Reasoning(r)),
                api::Delta::ToolBegin(idx, name) => {
                    announced.insert(idx);
                    // args 还没拼完，先空串——TUI 立即显示进行中状态块
                    evt.send(Evt::ToolStart { name, args: String::new() })
                }
            };
        })
        .await;
        let reply: Reply = match reply {
            Ok(r) => r,
            Err(e) => {
                let _ = evt.send(Evt::Error(e));
                let _ = evt.send(Evt::Done);
                return;
            }
        };

        // SSE 流中途被取消：半截正文保留入历史，本轮就此打住
        if reply.cancelled {
            if !reply.content.is_empty() {
                messages.push(json!({"role": "assistant", "content": reply.content}));
            }
            exhausted = false;
            break;
        }

        if reply.tool_calls.is_empty() {
            // 纯文本回复：assistant 消息入列，本轮结束
            messages.push(json!({"role": "assistant", "content": reply.content}));
            exhausted = false;
            break;
        }

        // 有工具调用：先把 assistant 的调用请求原样入列（协议要求）
        messages.push(assistant_msg(&reply));
        // 顺序执行每个工具，结果以 role=tool 消息入列
        for (call_idx, tc) in reply.tool_calls.iter().enumerate() {
            // 取消检查点②：每次工具执行前看标志。置位则不执行，
            // 但仍回填一条 tool 结果——协议要求每个 tool_call 都有应答
            if cancel.load(Ordering::Relaxed) {
                // 协议对称：Tool 之前必有 ToolStart。取消路径若该调用
                // 未在流式中段宣告过（ToolBegin），先补发 ToolStart，
                // 否则 TUI 只能靠兜底补块
                if !announced.contains(&call_idx) {
                    let _ = evt.send(Evt::ToolStart {
                        name: tc.name.clone(),
                        args: tc.arguments.clone(),
                    });
                }
                let _ = evt.send(Evt::Tool {
                    name: tc.name.clone(),
                    args: tc.arguments.clone(),
                    result: "（已取消）".to_string(),
                });
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tc.id,
                    "content": "（已取消）",
                }));
                continue;
            }
            // 派发前先发 ToolStart：TUI 立即显示进行中块，不等结果。
            // 流式中段已通过 ToolBegin 宣告过的不再重发（判重）
            if !announced.contains(&call_idx) {
                let _ = evt.send(Evt::ToolStart {
                    name: tc.name.clone(),
                    args: tc.arguments.clone(),
                });
            }
            let t0 = std::time::Instant::now();
            let result = match check_call(&tc.name, &tc.arguments) {
                // 参数校验失败：不执行工具，把明确错误回填给模型
                // （自愈原则：模型知道自己错在哪，下一轮会修正）
                Err(e) => e,
                Ok(args) => {
                    if tc.name == "addcmd" {
                        // 提案不执行任何东西：推给 TUI 等人类审批
                        let p = ApprovedCommand {
                            name: args["name"].as_str().unwrap_or_default().to_string(),
                            command: args["command"].as_str().unwrap_or_default().to_string(),
                            description: args["description"].as_str().unwrap_or_default().to_string(),
                            mode: args["mode"].as_str().unwrap_or_default().to_string(),
                            // AI 提案永远只去工作区层——AI 不能获得跨项目
                            // 生效的命令（安全边界，审批落盘时再次强制）
                            global: false,
                        };
                        let _ = evt.send(Evt::Proposal(p));
                        "已提交人类审批，结果待人工确认".to_string()
                    } else {
                        tools::run(ws, &tc.name, &args).await
                    }
                }
            };
            // 审计：工具执行（result 只留尾部 200 字符，防日志膨胀）
            audit.log(
                "tool",
                json!({
                    "name": tc.name,
                    "args": tc.arguments,
                    "duration_ms": t0.elapsed().as_millis(),
                    "result": tail_chars(&result, 200),
                }),
            );
            let _ = evt.send(Evt::Tool {
                name: tc.name.clone(),
                args: tc.arguments.clone(), // 原始 JSON，展示层决定怎么渲染
                result: result.clone(),
            });
            messages.push(json!({
                "role": "tool",
                "tool_call_id": tc.id,
                "content": result,
            }));
        }
        // 工具循环期间被取消：结果已回填完毕，不再发起下一轮 API
        if cancel.load(Ordering::Relaxed) {
            exhausted = false;
            break;
        }
        // 继续循环：把工具结果交给模型，等下一步指示
    }
    if exhausted {
        // 工具往返打满：历史此刻以 tool 消息结尾，不说明的话模型不知道
        // 被截断、用户也看不到原因。注入一条 user 说明（本轮不再发起
        // API，该消息从下一轮对话起生效），并记一笔审计
        messages.push(json!({
            "role": "user",
            "content": format!("（系统提示）工具往返已达上限 {MAX_TOOL_ROUNDS} 次，请基于现有工具结果直接作答，不要再调用工具"),
        }));
        audit.log(
            "reply",
            json!({ "truncated": "tool_rounds_limit", "max": MAX_TOOL_ROUNDS }),
        );
    }
    let _ = evt.send(Evt::Done);
    let tokens = estimate_tokens(messages);
    // 审计：一轮对话完成（粗估 token 即可）
    audit.log("reply", json!({ "tokens": tokens }));
    let _ = evt.send(Evt::Tokens(tokens));
}

/// 取字符串尾部 n 个字符（审计 result 截断用，按 char 不切 UTF-8）
fn tail_chars(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars.iter().skip(chars.len().saturating_sub(n)).collect()
}

/// 派发前校验：解析 arguments + 按各工具的必填/类型规则检查。
/// 7 个内建工具的 schema 很简单，手写检查即可，不引 JSON Schema 库。
/// 返回 Ok(解析后的参数) 或 Err(给模型看的错误文案)。
/// ⚠ 互指注释：这里的参数校验规则与 tools.rs 的 schema（defs()）是两份
/// 手写真相，改动任一处必须同步另一处；工具名单以 tools::TOOL_NAMES 为准。
fn check_call(name: &str, raw_args: &str) -> Result<Value, String> {
    let args: Value = serde_json::from_str(raw_args)
        .map_err(|_| "参数校验失败：arguments 不是合法 JSON".to_string())?;
    // 必填 string 字段检查
    let need = |key: &str| -> Result<(), String> {
        match args.get(key) {
            Some(v) if v.is_string() => Ok(()),
            Some(_) => Err(format!("参数校验失败：{key} 应为 string")),
            None => Err(format!("参数校验失败：缺少必填参数 {key}")),
        }
    };
    // 可选 string 字段：出现就必须是 string
    let opt = |key: &str| -> Result<(), String> {
        match args.get(key) {
            Some(v) if !v.is_string() => Err(format!("参数校验失败：{key} 应为 string")),
            _ => Ok(()),
        }
    };
    match name {
        "read" => need("path")?,
        "write" => {
            need("path")?;
            need("content")?;
        }
        "edit" => {
            need("path")?;
            need("old")?;
            need("new")?;
        }
        "ls" => opt("path")?,
        "grep" => {
            need("pattern")?;
            opt("path")?;
        }
        "addcmd" => {
            need("name")?;
            need("command")?;
            need("description")?;
            need("mode")?;
            // name 是未来的命令名，字符集硬限制（规格红线之一）
            let n = args["name"].as_str().unwrap_or_default();
            if !commands::valid_name(n) {
                return Err("参数校验失败：name 只能包含字母/数字/_/-".to_string());
            }
            match args["mode"].as_str().unwrap_or_default() {
                "once" | "daemon" => {}
                _ => return Err("参数校验失败：mode 只能是 once 或 daemon".to_string()),
            }
        }
        "runcmd" => opt("name")?,
        other => return Err(format!("未知工具 {other}")),
    }
    Ok(args)
}

/// 把模型的 tool_calls 序列化成 assistant 历史消息（OpenAI 协议格式）
fn assistant_msg(reply: &Reply) -> Value {
    let calls: Vec<Value> = reply
        .tool_calls
        .iter()
        .map(|tc| {
            json!({
                "id": tc.id,
                "type": "function",
                "function": { "name": tc.name, "arguments": tc.arguments },
            })
        })
        .collect();
    json!({
        "role": "assistant",
        "content": if reply.content.is_empty() { Value::Null } else { json!(reply.content) },
        "tool_calls": calls,
    })
}

/// 上下文 token 粗估：全部消息序列化后 chars/4。
/// 程序员看着这个数字自己决定何时 /new——这是设计，不是偷懒。
fn estimate_tokens(messages: &[Value]) -> usize {
    let chars: usize = messages
        .iter()
        .map(|m| m.to_string().len())
        .sum();
    chars / 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_call_validation() {
        // 缺必填参数 → 明确错误（回填给模型自愈）
        let e = check_call("read", "{}").unwrap_err();
        assert!(e.contains("缺少必填参数 path"), "{e}");
        // 类型错
        let e = check_call("read", "{\"path\":1}").unwrap_err();
        assert!(e.contains("应为 string"), "{e}");
        // 非法 JSON（旧的静默降级为 {} 已移除）
        let e = check_call("read", "{oops").unwrap_err();
        assert!(e.contains("不是合法 JSON"), "{e}");
        // 正常参数不受影响
        assert!(check_call("read", "{\"path\":\"a.rs\"}").is_ok());
        // grep：pattern 必填、path 可选
        assert!(check_call("grep", "{\"pattern\":\"x\"}").is_ok());
        assert!(check_call("grep", "{}").is_err());
        assert!(check_call("grep", "{\"pattern\":\"x\",\"path\":2}").is_err());
        // runcmd：name 可选字符串
        assert!(check_call("runcmd", "{}").is_ok());
        assert!(check_call("runcmd", "{\"name\":\"deploy\"}").is_ok());
        assert!(check_call("runcmd", "{\"name\":1}").is_err());
        // 白名单名字不再是"已知工具"（发现式注入后校验只认 7 个内建名）
        assert!(check_call("deploy", "{}").unwrap_err().contains("未知工具"));
        // start 已彻底删除（字段/工具/隐式条目全清）
        assert!(check_call("start", "{}").unwrap_err().contains("未知工具"));
    }

    #[test]
    fn check_call_addcmd() {
        // 合法提案
        let good = "{\"name\":\"dev\",\"command\":\"npm run dev\",\"description\":\"开发服务器\",\"mode\":\"daemon\"}";
        assert!(check_call("addcmd", good).is_ok());
        // 非法 name（含注入字符）→ 拒绝
        let bad = "{\"name\":\"a;b\",\"command\":\"x\",\"description\":\"d\",\"mode\":\"once\"}";
        assert!(check_call("addcmd", bad).unwrap_err().contains("name"));
        // 非法 mode → 拒绝
        let badm = "{\"name\":\"ok\",\"command\":\"x\",\"description\":\"d\",\"mode\":\"forever\"}";
        assert!(check_call("addcmd", badm).unwrap_err().contains("mode"));
        // 缺 description → 拒绝
        let nod = "{\"name\":\"ok\",\"command\":\"x\",\"mode\":\"once\"}";
        assert!(check_call("addcmd", nod).unwrap_err().contains("description"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_cmd_sets_shared_flag() {
        let dir = std::env::temp_dir().join(format!("doagent-agent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let h = AgentHandle::start(&dir).unwrap();
        assert!(!h.cancelled());
        h.send(Cmd::Cancel);
        assert!(h.cancelled()); // 置位立即对 TUI 侧可见（Arc 共享同一原子量）
        let _ = std::fs::remove_dir_all(&dir);
    }
}
