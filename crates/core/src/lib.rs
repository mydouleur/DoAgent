//! DoAgent 核心库（core crate 的根模块）
//!
//! # 模块导读
//! 这里聚合全部"后端"能力：
//! - [`workspace`] 工作区守卫：路径归一 + 根内校验 + `.do/` 隐形
//! - [`tools`]     7 个 AI 工具（read/write/edit/ls/grep/addcmd/runcmd，数组冻结）
//! - [`api`]       OpenAI 兼容 API 的 SSE 流式客户端 + tool_calls 累加器
//! - [`config`]    双层配置读写与覆盖合并（工作区 .do/config.json > exe 旁 do.config.json > 内置默认）
//! - [`commands`]  命令白名单（工作区 + 全局双层）
//! - [`audit`]     全局审计日志（do.audit.jsonl，exe 旁）
//! - [`agent`]     agent 对话循环（actor 模式：消息进、事件出）
//!
//! # 对外的最小公共 API
//! TUI（do crate）只需要认识三个类型：`AgentHandle`（启动 + 发消息）、
//! `Cmd`（发给 agent 的命令）、`Evt`（agent 推回来的事件），
//! 以及一个常量 `SYSTEM_PROMPT`。其余模块仅 `pub(crate)` 或 pub 但不依赖。

mod agent;
mod api;
pub mod audit; // pub：TUI 启动时做可用性检查
pub mod commands; // pub：TUI 的 /addcmd /allowcmd /deletecmd 与测试复用
pub mod config; // pub 给集成测试复用
mod tools;
pub mod workspace; // pub 给集成测试复用

// `pub use` 把深层模块里的类型"再导出"到 crate 根部，
// 外部就可以写 `core::AgentHandle` 而不是 `core::agent::AgentHandle`。
pub use agent::{AgentHandle, Cmd, Evt, ToolCall, SYSTEM_PROMPT};
pub use api::check_net; // CI 冒烟入口（do --check-net <url>）
pub use commands::{ApprovedCommand, Layer}; // Layer：命令来源层（TUI 删除页按层落盘）
pub use tools::TOOL_NAMES; // 7 个内建工具的唯一名单（TUI 重名检查复用）
