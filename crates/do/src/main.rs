//! do 二进制入口：终端初始化 + panic 兜底 + 启动 TUI
//!
//! # 模块导读
//! main 只做三件事：
//! 1. 装 panic hook——release 档开了 `panic="abort"`，panic 时 RAII
//!    析构**不会**执行，终端会卡在 raw mode 里报废，所以必须在进程
//!    终止前手动恢复终端（leave alternate screen + disable raw mode）；
//! 2. 以启动 cwd 为工作区根，拉起 core 的 agent actor；
//! 3. 进入 TUI 主循环。
//!
//! # 核心概念
//! `#[tokio::main(flavor = "current_thread")]`：宏展开后等价于手写
//! "建 runtime → block_on(main 的 async 体)"。current_thread 表示单线程
//! 调度（≈ C# 里所有 await 都回到同一线程的同步上下文）——TUI 是单
//! 事件循环，用不到多线程，省体积也省心。

use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};

mod md;
mod tui;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // panic hook：进程 abort 前最后的机会，把终端还给用户
    std::panic::set_hook(Box::new(|_| {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
    }));

    let root = match std::env::current_dir() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("无法确定工作目录: {e}");
            return;
        }
    };
    if let Err(e) = tui::run(&root).await {
        eprintln!("{e}");
    }
}
