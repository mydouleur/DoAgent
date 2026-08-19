//! OpenAI 兼容 API 的 SSE 流式客户端 + tool_calls 增量累加器
//!
//! # 模块导读
//! 不依赖任何 SDK：reqwest 直连 `POST {url}/chat/completions`（stream: true，
//! Bearer 认证），响应是 SSE（Server-Sent Events）事件流，每帧一个 JSON chunk。
//! 用 eventsource-stream 解帧后，本模块做两件有状态的事：
//! 1. 正文 / 思考（reasoning_content）增量通过回调实时推给上层；
//! 2. **tool_calls 累加器**：流式协议把一个工具调用拆成很多帧，
//!    每帧只带 `index` 和一小段 arguments 字符串——按 index 归堆、
//!    顺序拼接，流结束时才得到完整 JSON。这段逻辑"只写一次"，附带测试。
//!
//! # 核心概念
//! `async fn` + `.await` ≈ C# 的 async/await：函数体被编译成状态机，
//! 到 IO 边界就让出线程。Rust 的区别是 async 函数返回的是一个"冷"的
//! Future（≈ Task 但不会自动开始），必须有 runtime（tokio）驱动它。

use crate::config::Config;
use crate::tools::ToolDef;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};

/// 一次完整的模型回复（流结束后的累积结果）
#[derive(Debug, Default)]
pub struct Reply {
    /// 正文（拼接后的完整内容）
    pub content: String,
    /// 思考过程（reasoning_content 拼接结果，可能为空）
    pub reasoning: String,
    /// 模型请求的工具调用（通常 0 或 1 个，协议允许并行多个）
    pub tool_calls: Vec<ToolCall>,
    /// 本轮是否被用户取消（Esc）——半截内容保留在此结构里
    pub cancelled: bool,
}

/// 一次工具调用。arguments 是**未解析的 JSON 字符串**（协议原样），
/// 由上层在真正执行前 parse——流式拼接的中间态不是合法 JSON。
#[derive(Debug, Clone, Default)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// 流式增量事件（回调给 agent，再转成 Evt 推给 TUI）
pub enum Delta {
    /// 正文增量
    Text(String),
    /// 思考增量
    Reasoning(String),
    /// 某个 tool_call 的名字**首次**出现（index, name）。
    /// 教学点：流式协议里 name 在第一帧就到，arguments 才是一点点拼的。
    /// 一拿到名字就宣告 ≈ C# 里 IO 一开始就显示进度条，而不是等完成——
    /// 长回复（如 write 大文件）期间用户能看到"正在调用谁"。
    ToolBegin(usize, String),
}

/// HTTPS 冒烟（CI 用）：GET 一次返回状态码。
/// 用途：TLS 栈按平台分叉（Schannel/Security.framework/OpenSSL/rustls）后，
/// CI 对每个已发布二进制跑一次，验证该平台 TLS 握手真的通。
pub async fn check_net(url: &str) -> Result<String, String> {
    let resp = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("HTTP {} {}", resp.status().as_u16(), url))
}

/// 把工具定义转成 API 请求里的 tools 数组
fn tools_json(defs: &[ToolDef]) -> Value {
    Value::Array(
        defs.iter()
            .map(|d| {
                json!({
                    "type": "function",
                    "function": {
                        "name": d.name,
                        "description": d.description,
                        "parameters": d.parameters,
                    }
                })
            })
            .collect(),
    )
}

/// 发起一轮流式对话。
/// `on_delta` 是回调（FnMut ≈ C# 的 Action<Delta> 委托），
/// 每个增量到达时同步调用——借它把流实时转成 TUI 事件。
/// `cancel` 是取消标志：每个 SSE chunk 处理前检查一次，
/// 置位则带着半截累积结果提前返回（cancelled = true）。
pub async fn chat(
    cfg: &Config,
    messages: &[Value],
    defs: &[ToolDef],
    cancel: &AtomicBool,
    mut on_delta: impl FnMut(Delta),
) -> Result<Reply, String> {
    let client = reqwest::Client::new();
    let body = serde_json::to_string(&json!({
        "model": cfg.model,
        "messages": messages,
        "tools": tools_json(defs),
        "stream": true,
    }))
    .map_err(|e| e.to_string())?;
    let resp = client
        .post(format!("{}/chat/completions", cfg.url.trim_end_matches('/')))
        .bearer_auth(&cfg.key) // Authorization: Bearer <key>
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body) // 手动序列化，不启用 reqwest 的 json feature（体积考虑）
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {status}: {body}"));
    }

    // bytes_stream() 拿到字节流，Eventsource 包一层解 SSE 帧。
    // `Stream` ≈ C# 的 IAsyncEnumerable<T>：异步拉取的一串值。
    let mut stream = resp.bytes_stream().eventsource();
    let mut acc = Accumulator::default();

    // `while let Some(x) = ... .await`：异步迭代 ≈ await foreach
    while let Some(event) = stream.next().await {
        // 取消检查点①：每个 chunk 处理前看标志。Relaxed 足够——
        // 纯标志位，没有需要顺带同步的其他数据（≈ C# volatile 读）
        if cancel.load(Ordering::Relaxed) {
            acc.reply.cancelled = true;
            break;
        }
        let event = event.map_err(|e| e.to_string())?;
        if event.data == "[DONE]" {
            break;
        }
        // 单帧可能是碎片/多帧合并——serde 只认完整 JSON，SSE 层已保证一帧一条 data
        let chunk: Value = match serde_json::from_str(&event.data) {
            Ok(v) => v,
            Err(_) => continue, // 容错：跳过坏帧
        };
        for choice in chunk["choices"].as_array().into_iter().flatten() {
            let delta = &choice["delta"];
            if let Some(r) = delta["reasoning_content"].as_str() {
                acc.reply.reasoning.push_str(r);
                on_delta(Delta::Reasoning(r.to_string()));
            }
            if let Some(c) = delta["content"].as_str() {
                acc.reply.content.push_str(c);
                on_delta(Delta::Text(c.to_string()));
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                for tc in calls {
                    // 累加器返回 Some((index, name)) 表示该 index 第一次拿到
                    // 名字——立刻宣告，让用户在流式中段就看到工具调用
                    if let Some((idx, name)) = acc.push_tool_call(tc) {
                        on_delta(Delta::ToolBegin(idx, name));
                    }
                }
            }
        }
    }
    Ok(acc.reply)
}

/// tool_calls 增量累加器：按 `index` 归堆，顺序拼接 arguments。
/// 单独抽成结构体是为了可测试（碎片帧、交错 index 都是真实场景）。
#[derive(Default)]
struct Accumulator {
    reply: Reply,
}

impl Accumulator {
    /// 喂入一帧 tool_call 增量。返回值：该 index **首次**拿到名字时为
    /// `Some((index, name))`（供上层提前宣告工具调用），其余为 None。
    fn push_tool_call(&mut self, tc: &Value) -> Option<(usize, String)> {
        // index 缺失时按 0 处理（个别实现的怪癖）
        let idx = tc["index"].as_u64().unwrap_or(0) as usize;
        // 归堆：index 指向的槽位不够就补默认实例
        while self.reply.tool_calls.len() <= idx {
            self.reply.tool_calls.push(ToolCall::default());
        }
        let slot = &mut self.reply.tool_calls[idx];
        // id / name 只在首帧带全量，后续帧为空——非空才覆盖
        if let Some(id) = tc["id"].as_str() {
            slot.id.push_str(id);
        }
        let mut first_named = None;
        if let Some(name) = tc["function"]["name"].as_str() {
            // 槽位原本没名字 → 这是该调用的首次宣告
            if slot.name.is_empty() {
                first_named = Some((idx, name.to_string()));
            }
            slot.name.push_str(name);
        }
        // arguments 是真正的增量片段，无脑拼接
        if let Some(args) = tc["function"]["arguments"].as_str() {
            slot.arguments.push_str(args);
        }
        first_named
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 把一帧帧 delta JSON 喂给累加器（模拟 SSE 流）
    fn feed(frames: &[Value]) -> Reply {
        let mut acc = Accumulator::default();
        for f in frames {
            for tc in f["tool_calls"].as_array().into_iter().flatten() {
                let _ = acc.push_tool_call(tc);
            }
        }
        acc.reply
    }

    #[test]
    fn first_name_announced_exactly_once() {
        // name 只在首帧出现：第一次推入返回 Some，后续碎片帧返回 None
        let mut acc = Accumulator::default();
        let first = acc.push_tool_call(
            &json!({"index":0,"id":"c1","function":{"name":"read","arguments":"{\"pa"}}),
        );
        assert_eq!(first, Some((0, "read".to_string())));
        let second = acc.push_tool_call(
            &json!({"index":0,"function":{"arguments":"th\":\"a.rs\"}"}}),
        );
        assert_eq!(second, None);
        // 交错 index 1 的首次出现也要宣告
        let third = acc.push_tool_call(
            &json!({"index":1,"id":"c2","function":{"name":"ls","arguments":"{}"}}),
        );
        assert_eq!(third, Some((1, "ls".to_string())));
    }

    #[test]
    fn accumulates_argument_fragments() {
        // arguments 被拆成三帧：{"pa + th":"a + .rs"}
        let reply = feed(&[
            json!({"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":"{\"pa"}}]}),
            json!({"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a"}}]}),
            json!({"tool_calls":[{"index":0,"function":{"arguments":".rs\"}"}}]}),
        ]);
        assert_eq!(reply.tool_calls.len(), 1);
        assert_eq!(reply.tool_calls[0].id, "c1");
        assert_eq!(reply.tool_calls[0].name, "read");
        assert_eq!(reply.tool_calls[0].arguments, "{\"path\":\"a.rs\"}");
        // 拼完必须是合法 JSON
        assert!(serde_json::from_str::<Value>(&reply.tool_calls[0].arguments).is_ok());
    }

    #[test]
    fn interleaved_indices_kept_separate() {
        // 两个并行调用交错到达：index 0/1 各归各的堆
        let reply = feed(&[
            json!({"tool_calls":[
                {"index":0,"id":"a","function":{"name":"read","arguments":"{\"pa"}},
                {"index":1,"id":"b","function":{"name":"ls","arguments":"{\"pa"}}
            ]}),
            json!({"tool_calls":[
                {"index":1,"function":{"arguments":"th\":\".\"}"}},
                {"index":0,"function":{"arguments":"th\":\"x\"}"}}
            ]}),
        ]);
        assert_eq!(reply.tool_calls.len(), 2);
        assert_eq!(reply.tool_calls[0].arguments, "{\"path\":\"x\"}");
        assert_eq!(reply.tool_calls[1].arguments, "{\"path\":\".\"}");
        assert_eq!(reply.tool_calls[1].name, "ls");
    }
}
