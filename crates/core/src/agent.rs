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
use crate::config::Config;
use crate::tools;
use crate::workspace::Workspace;
use serde_json::{json, Value};
use std::io;
use std::path::Path;
use tokio::sync::mpsc;

// ToolCall 再导出，外部从 core::ToolCall 拿（经 lib.rs 二次再导出）
pub use crate::api::ToolCall;

/// system prompt：控制在 200 token 内（约 300 字内）。
/// 一句话角色 + 工具纪律 + 维护 HANDOFF.md 的义务。
pub const SYSTEM_PROMPT: &str = "你是 DoAgent，嵌入式编程副驾驶。只能用 6 个工具(read/write/edit/ls/grep/start)读写代码，无 shell；改完代码用 start 拿编译反馈。随时在工作区根维护 HANDOFF.md（当前目标/进展/关键决策/下一步），有进展就更新。回答简洁，直接行动。";

/// TUI 发给 agent 的命令
pub enum Cmd {
    /// 用户输入的一句话
    Chat(String),
    /// /new：清空历史（第一条用户消息由 TUI 重新注入，通常是 HANDOFF.md）
    Reset,
}

/// agent 推给 TUI 的事件
pub enum Evt {
    /// 正文增量（流式）
    Text(String),
    /// 思考增量（流式）
    Reasoning(String),
    /// 一次工具调用完成（附截断后的结果）
    Tool {
        name: String,
        args: String,
        result: String,
    },
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
}

impl AgentHandle {
    /// 以 `root` 为工作区启动 agent 后台任务。
    pub fn start(root: &Path) -> io::Result<AgentHandle> {
        let ws = Workspace::new(root)?;
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Cmd>();
        let (evt_tx, evt_rx) = mpsc::unbounded_channel::<Evt>();
        // `move` 把 ws/cmd_rx/evt_tx 的所有权移进后台任务；
        // tokio::spawn 类似 Task.Run，但要求被捕获的东西满足 Send。
        tokio::spawn(async move {
            actor_loop(ws, cmd_rx, evt_tx).await;
        });
        Ok(AgentHandle { tx: cmd_tx, rx: evt_rx })
    }

    /// 发命令（不阻塞；agent 正忙时消息排队）
    pub fn send(&self, cmd: Cmd) {
        // 接收端已关闭时 send 会失败——agent 不在了，静默丢弃即可
        let _ = self.tx.send(cmd);
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
) {
    let defs = tools::defs();
    let mut messages: Vec<Value> = vec![json!({
        "role": "system",
        "content": SYSTEM_PROMPT,
    })];

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Cmd::Reset => {
                messages.truncate(1); // 只留 system prompt
                let _ = evt.send(Evt::Tokens(estimate_tokens(&messages)));
            }
            Cmd::Chat(text) => {
                messages.push(json!({"role": "user", "content": text}));
                chat_round(&ws, &defs, &mut messages, &evt).await;
            }
        }
    }
}

/// 一轮完整对话：可能包含多次 API 调用（工具往返）。
async fn chat_round(
    ws: &Workspace,
    defs: &[tools::ToolDef],
    messages: &mut Vec<Value>,
    evt: &mpsc::UnboundedSender<Evt>,
) {
    // 工具往返设上限，防模型死循环
    for _ in 0..16 {
        // 生效配置 = 工作区层 > 全局便携层 > 默认；current_exe 失败自动降级
        let cfg = Config::load_merged(ws.root(), crate::config::exe_dir().as_deref());
        // 回调把流式增量就地转成事件推给 TUI
        let reply = api::chat(&cfg, messages, defs, |d| {
            let _ = match d {
                api::Delta::Text(t) => evt.send(Evt::Text(t)),
                api::Delta::Reasoning(r) => evt.send(Evt::Reasoning(r)),
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

        if reply.tool_calls.is_empty() {
            // 纯文本回复：assistant 消息入列，本轮结束
            messages.push(json!({"role": "assistant", "content": reply.content}));
            break;
        }

        // 有工具调用：先把 assistant 的调用请求原样入列（协议要求）
        messages.push(assistant_msg(&reply));
        // 顺序执行每个工具，结果以 role=tool 消息入列
        for tc in &reply.tool_calls {
            let args: Value = serde_json::from_str(&tc.arguments)
                .unwrap_or_else(|_| json!({}));
            let result = tools::run(ws, &tc.name, &args).await;
            let _ = evt.send(Evt::Tool {
                name: tc.name.clone(),
                args: summarize_args(&args),
                result: result.clone(),
            });
            messages.push(json!({
                "role": "tool",
                "tool_call_id": tc.id,
                "content": result,
            }));
        }
        // 继续循环：把工具结果交给模型，等下一步指示
    }
    let _ = evt.send(Evt::Done);
    let _ = evt.send(Evt::Tokens(estimate_tokens(messages)));
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

/// 工具调用的单行摘要（TUI 折叠态显示用），如 `read(src/main.rs)`
fn summarize_args(args: &Value) -> String {
    args.get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
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
