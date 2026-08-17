//! TUI：启动画面 + 对话流 + 输入 + slash 命令 + 设置页 + 状态栏
//!
//! # 模块导读
//! 单事件循环驱动一切：crossterm 的键盘事件由一个专用线程转发进
//! tokio channel，与 agent actor 的事件、splash 定时器在
//! `tokio::select!` 里汇合——全部非阻塞等待，单线程 runtime 够用。
//!
//! # 三种界面模式（[`Mode`]）
//! - Splash：启动画面，logo + 版本 + 配置状态；任意键或 ~1.2s 自动进入对话
//! - Chat：对话主界面（流式正文/思考/工具调用、slash 命令、Esc 取消）
//! - Settings：/setting 独立设置页（↑↓ 选字段、Enter 编辑、Esc 返回）
//!
//! # 交互约定（Chat 模式）
//! - Enter 发送；Ctrl+C 退出；Esc 取消当前轮（仅 agent 工作时响应）；
//!   Ctrl+E 全部展开/折叠；↑↓ 或 PageUp/PageDown 滚动。
//! - 配色：思考灰、工具青、正文默认色。折叠块只显示一行摘要。
//! - 底部两行：快捷键提示行（上下文感知）+ 状态栏 `~N tok | model | 工作区`。
//!
//! # 核心概念
//! - RAII / Drop：[`TerminalGuard`] 离开作用域时自动恢复终端——
//!   ≈ C# 的 `using` + IDisposable，但由编译器保证调用，不靠 using 块。
//! - 生命周期标注 `'static`：ratatui 的 `Line<'a>` 借用文本；
//!   我们全部用 `String`（拥有所有权），所以是 `Line<'static>`，
//!   不与缓冲区抢借用，渲染代码因此简单很多。

use agent_core::config::Config;
use agent_core::{AgentHandle, ApprovedCommand, Cmd, Evt};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use std::io;
use std::path::Path;
use unicode_width::UnicodeWidthChar;

/// task.md 末尾的 ASCII logo（逐字内嵌，含行尾空格）
const LOGO: &str = "\
░████                        ░███████              ░████ 
░██   ░██                    ░██   ░██               ░██ 
░██    ░██                   ░██    ░██  ░███████    ░██ 
░██     ░██                  ░██    ░██ ░██    ░██   ░██ 
░██    ░██                   ░██    ░██ ░██    ░██   ░██ 
░██   ░██                    ░██   ░██  ░██    ░██   ░██ 
░██           ░██████████    ░███████    ░███████    ░██ 
░██                                                  ░██ 
░████                                              ░████ 
                                                         ";

/// 五种界面模式
#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// 启动画面：任意键或定时器自动跳过，不强制等待
    Splash,
    /// 对话主界面
    Chat,
    /// /setting 独立设置页
    Settings,
    /// /addcmd 命令提案审批页（command 只读，name/description 可改名再批）
    Approve,
    /// /deletecmd 已批准命令删除页
    Delete,
}

/// 对话流里的一条记录
enum Item {
    /// 用户输入（含注入的 HANDOFF.md）
    User(String),
    /// 思考过程。流式期间 open=true 实时可见（等待时能看到进展）；
    /// 正文一开始输出就自动折叠为一行摘要
    Reasoning { text: String, open: bool },
    /// 一次工具调用：args 是**原始 JSON 字符串**，展示层决定怎么渲染
    Tool { name: String, args: String, result: String },
    /// 正文回复（流式增长，经 Markdown 渲染）
    Assistant(String),
    /// 本地提示（slash 反馈、启动信息等）
    Info(String),
    /// 错误
    Error(String),
}

/// 设置页字段表：字段名 + 是否全局层（false = 工作区层）
const SETTINGS_FIELDS: &[(&str, bool)] = &[
    ("url", true),
    ("key", true),
    ("model", true),
    ("start", false),
];

/// TUI 全部可变状态（单所有者，单线程 runtime 下无需锁）
struct Ui {
    items: Vec<Item>,
    input: String,
    /// 距底部的滚动行数：0 = 跟随最新消息
    scroll: usize,
    /// Ctrl+E：全部展开 true / 全部折叠 false
    expand_all: bool,
    tokens: usize,
    model: String,
    has_key: bool,
    workspace: String,
    /// true 时退出主循环
    quit: bool,
    mode: Mode,
    /// agent 正在工作：此时 Esc = 取消本轮；平时 Esc 不响应
    busy: bool,
    /// 已发出取消、等待 agent 收尾（Done 到达时补一行"（已取消）"）
    cancel_pending: bool,
    /// 设置页：当前选中字段下标
    set_sel: usize,
    /// 设置页：编辑态缓冲（None = 非编辑态）
    set_editing: Option<String>,
    /// 设置页：进入时载入的各字段当前值（key 存真值，显示时才掩码）
    set_values: Vec<String>,
    /// 待审批的命令提案队列（AI 通过 addcmd 提交，内存态不落盘）
    pending: Vec<ApprovedCommand>,
    /// 审批页：当前编辑字段（0=name 1=description）与表单值
    appr_sel: usize,
    appr_form: [String; 2],
    /// 删除页：进入时载入的已批准命令列表与选中下标
    del_list: Vec<ApprovedCommand>,
    del_sel: usize,
}

/// RAII 守卫：构造时进 alternate screen + raw mode，Drop 时恢复。
/// 无论主循环正常返回还是 `?` 提前返回，Drop 都会被调用——
/// C# 里要 try/finally 包住的事，Rust 由类型系统兜底。
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<TerminalGuard> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// 启动 TUI 主循环。`root` 即工作区（= 启动 cwd）。
pub async fn run(root: &Path) -> io::Result<()> {
    // 生效配置 = 工作区层 > 全局便携层（exe 旁 do.config.json）> 内置默认。
    // current_exe 失败时 exe_dir 为 None：优雅降级为只用工作区层 + 默认值。
    let exe_dir = agent_core::config::exe_dir();
    let cfg = Config::load_merged(root, exe_dir.as_deref());
    let mut agent = AgentHandle::start(root)?;
    let _guard = TerminalGuard::enter();

    // 键盘事件转发线程：crossterm 的 read 是阻塞调用，
    // 放到专用 std 线程里，通过 tokio channel 送事件——
    // ≈ C# 里用 Channel 把同步 IO 桥接进 async 世界。
    let (key_tx, mut key_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        // while let：通道关闭或读失败时线程自然结束
        while let Ok(ev) = crossterm::event::read() {
            if key_tx.send(ev).is_err() {
                break; // 主循环已退出
            }
        }
    });

    let mut ui = Ui {
        items: Vec::new(),
        input: String::new(),
        scroll: 0,
        expand_all: false,
        tokens: 0,
        model: if cfg.model.is_empty() { "未设置".into() } else { cfg.model.clone() },
        has_key: !cfg.key.is_empty(),
        workspace: root.display().to_string(),
        quit: false,
        mode: Mode::Splash,
        busy: false,
        cancel_pending: false,
        set_sel: 0,
        set_editing: None,
        set_values: Vec::new(),
        pending: Vec::new(),
        appr_sel: 0,
        appr_form: [String::new(), String::new()],
        del_list: Vec::new(),
        del_sel: 0,
    };
    // 启动信息（splash 下面的对话流里保留这几行供回看）
    ui.items.push(Item::Info(format!("工作区: {}", ui.workspace)));
    if exe_dir.is_none() {
        // 降级提示：全局层不可用但不影响使用，绝不 panic
        ui.items.push(Item::Info(
            "无法定位 do.exe 目录，全局配置层不可用（仅用工作区配置 + 默认值）".into(),
        ));
    }
    if !ui.has_key {
        ui.items.push(Item::Info("未设置 API key，请用 /setting -g key <你的key>".into()));
    }

    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend)?;

    // splash 自动跳过时刻：约 1.2 秒
    let splash_until = tokio::time::Instant::now() + std::time::Duration::from_millis(1200);

    while !ui.quit {
        draw(&mut term, &ui)?;
        // select! ≈ C# 的 Task.WhenAny：哪个事件先到处理哪个
        tokio::select! {
            ev = key_rx.recv() => {
                let Some(ev) = ev else { break };
                handle_key(&mut ui, ev, &mut agent, root);
            }
            ev = agent.next() => {
                let Some(ev) = ev else { break };
                handle_agent(&mut ui, ev);
            }
            // `if` 条件分支：只在 splash 期间启用定时器
            // ≈ C# 里 Task.WhenAny(keys, Task.Delay(1200)) 的条件版
            _ = tokio::time::sleep_until(splash_until), if ui.mode == Mode::Splash => {
                ui.mode = Mode::Chat;
            }
        }
    }
    Ok(())
}

/// 键盘事件总入口：先按模式分流，各模式自己的处理器再细分按键
fn handle_key(ui: &mut Ui, ev: Event, agent: &mut AgentHandle, root: &Path) {
    let Event::Key(KeyEvent { code, modifiers, kind, .. }) = ev else { return };
    // crossterm 经典坑：Windows 上一次按键会同时发出 Press 和 Release
    // 两个事件（长按还有 Repeat），不过滤就会每个键处理两次。
    // ≈ C# WinForms 的 KeyDown/KeyUp 是两个事件，这里等价于只响应 KeyDown。
    // 所有键盘入口都走这一个函数，过滤一次即全覆盖。
    if kind != KeyEventKind::Press {
        return;
    }
    // Ctrl+C 在任何界面都是退出
    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        ui.quit = true;
        return;
    }
    match ui.mode {
        // splash：任意键跳过（定时器也会自动跳过）
        Mode::Splash => ui.mode = Mode::Chat,
        Mode::Settings => settings_key(ui, code, root),
        Mode::Approve => approve_key(ui, code, agent, root),
        Mode::Delete => delete_key(ui, code, root),
        Mode::Chat => chat_key(ui, code, modifiers, agent, root),
    }
}

/// Chat 模式的按键：输入框 / 滚动 / 快捷键 / Esc 取消
fn chat_key(ui: &mut Ui, code: KeyCode, modifiers: KeyModifiers, agent: &mut AgentHandle, root: &Path) {
    if modifiers.contains(KeyModifiers::CONTROL) {
        if code == KeyCode::Char('e') {
            ui.expand_all = !ui.expand_all;
        }
        return;
    }
    match code {
        KeyCode::Enter => submit(ui, agent, root),
        // Tab：slash 命令补全为第一个候选（见 slash_candidates）
        KeyCode::Tab => tab_complete(ui),
        // Esc 取消：只在 agent 工作时响应（置位共享标志），平时不响应
        KeyCode::Esc => {
            if ui.busy {
                agent.send(Cmd::Cancel);
                ui.cancel_pending = true;
            }
        }
        // 输入框字符：c 是 Unicode 标量，CJK 直接进字符串
        KeyCode::Char(c) => ui.input.push(c),
        KeyCode::Backspace => {
            // pop 按 char 弹（≈ C# 里按字符删，不会因 UTF-8 多字节切碎）
            ui.input.pop();
        }
        KeyCode::PageUp => ui.scroll = ui.scroll.saturating_add(10),
        KeyCode::PageDown => ui.scroll = ui.scroll.saturating_sub(10),
        // 上下键与 PgUp/PgDn 行为重合，但步长更细（3 行 vs 10 行），适合精读
        KeyCode::Up => ui.scroll = ui.scroll.saturating_add(3),
        KeyCode::Down => ui.scroll = ui.scroll.saturating_sub(3),
        _ => {}
    }
}

/// Settings 模式的按键：选择 / 编辑 / 退出
fn settings_key(ui: &mut Ui, code: KeyCode, root: &Path) {
    // 编辑态：Enter 保存、Esc 放弃、字符进缓冲
    if ui.set_editing.is_some() {
        match code {
            KeyCode::Enter => {
                let value = ui.set_editing.take().unwrap_or_default();
                settings_save(ui, root, value);
            }
            KeyCode::Esc => ui.set_editing = None,
            KeyCode::Char(c) => {
                if let Some(buf) = &mut ui.set_editing {
                    buf.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(buf) = &mut ui.set_editing {
                    buf.pop();
                }
            }
            _ => {}
        }
        return;
    }
    // 选择态
    match code {
        KeyCode::Up => ui.set_sel = ui.set_sel.saturating_sub(1),
        KeyCode::Down => ui.set_sel = (ui.set_sel + 1).min(SETTINGS_FIELDS.len() - 1),
        // 进入编辑态：缓冲预填当前真值（key 给的是未掩码的真值）
        KeyCode::Enter => ui.set_editing = Some(ui.set_values[ui.set_sel].clone()),
        KeyCode::Esc => ui.mode = Mode::Chat,
        _ => {}
    }
}

/// Approve 模式的按键：表单式编辑（选中字段直接敲字），Enter 批准、Esc 拒绝
fn approve_key(ui: &mut Ui, code: KeyCode, agent: &mut AgentHandle, root: &Path) {
    match code {
        KeyCode::Enter => approve_current(ui, agent, root),
        KeyCode::Esc => reject_current(ui),
        // 两个可编辑字段间切换（0=name 1=description；command 不可改——
        // 审批的字符串 = 永远执行的全部内容，这是安全红线）
        KeyCode::Up => ui.appr_sel = 0,
        KeyCode::Down => ui.appr_sel = 1,
        KeyCode::Tab => ui.appr_sel = 1 - ui.appr_sel,
        KeyCode::Char(c) => ui.appr_form[ui.appr_sel].push(c),
        KeyCode::Backspace => {
            ui.appr_form[ui.appr_sel].pop();
        }
        _ => {}
    }
}

/// 批准当前提案：改名后的表单值 + 只读 command 原样写入 .do/commands.json
fn approve_current(ui: &mut Ui, agent: &mut AgentHandle, root: &Path) {
    let Some(p) = ui.pending.first() else {
        ui.mode = Mode::Chat;
        return;
    };
    let name = ui.appr_form[0].trim().to_string();
    let description = ui.appr_form[1].trim().to_string();
    // 批准前再验一次 name（人类改名也要守同一规则）
    if !agent_core::commands::valid_name(&name) {
        ui.items.push(Item::Info("name 只能包含字母/数字/_/-".into()));
        return;
    }
    // 与内建工具或已批准命令重名 = 拒绝（覆盖已有工具太危险）。
    // start 是隐式保留名（白名单视图会合并 config.start），一并保护
    const BUILTIN: &[&str] = &["read", "write", "edit", "ls", "grep", "addcmd", "runcmd", "start"];
    let mut cmds = agent_core::commands::load(root);
    if BUILTIN.contains(&name.as_str()) || cmds.iter().any(|c| c.name == name) {
        ui.items.push(Item::Info(format!("name 冲突：{name} 已被占用")));
        return;
    }
    cmds.push(ApprovedCommand {
        name: name.clone(),
        command: p.command.clone(), // command 不可改：审批什么就执行什么
        description,
        mode: p.mode.clone(),
    });
    match agent_core::commands::save(root, &cmds) {
        Ok(()) => {
            ui.items.push(Item::Info(format!("已批准并注册: {name}")));
            ui.pending.remove(0);
            // 批准后通知模型：以 user 角色注入历史（不触发 API）。
            // tools 数组是冻结的，批准不改变 prompt 前缀——零缓存代价
            if ui.has_key {
                agent.send(Cmd::Notify(format!(
                    "（系统提示：命令 {name} 已获批准，可用 runcmd 调用）"
                )));
            }
            next_pending_or_chat(ui);
        }
        Err(e) => ui.items.push(Item::Info(e.to_string())),
    }
}

/// 拒绝当前提案：丢弃，不入盘
fn reject_current(ui: &mut Ui) {
    if let Some(p) = ui.pending.first() {
        ui.items.push(Item::Info(format!("已拒绝提案: {}", p.name)));
        ui.pending.remove(0);
    }
    next_pending_or_chat(ui);
}

/// 审批完一条：还有待批就装填下一条，否则回对话
fn next_pending_or_chat(ui: &mut Ui) {
    match ui.pending.first() {
        Some(p) => {
            ui.appr_form = [p.name.clone(), p.description.clone()];
            ui.appr_sel = 0;
        }
        None => ui.mode = Mode::Chat,
    }
}

/// Delete 模式的按键：↑↓ 选择、Enter 删除、Esc 返回
fn delete_key(ui: &mut Ui, code: KeyCode, root: &Path) {
    match code {
        KeyCode::Up => ui.del_sel = ui.del_sel.saturating_sub(1),
        KeyCode::Down => {
            if !ui.del_list.is_empty() {
                ui.del_sel = (ui.del_sel + 1).min(ui.del_list.len() - 1);
            }
        }
        KeyCode::Enter => {
            if ui.del_sel < ui.del_list.len() {
                let gone = ui.del_list.remove(ui.del_sel);
                match agent_core::commands::save(root, &ui.del_list) {
                    Ok(()) => ui.items.push(Item::Info(format!("已撤销: {}", gone.name))),
                    Err(e) => ui.items.push(Item::Info(e.to_string())),
                }
                ui.del_sel = ui.del_sel.min(ui.del_list.len().saturating_sub(1));
                if ui.del_list.is_empty() {
                    ui.mode = Mode::Chat;
                }
            }
        }
        KeyCode::Esc => ui.mode = Mode::Chat,
        _ => {}
    }
}

/// 设置页保存一个字段：写哪层只读写哪层（与 /setting 命令同一纪律）
fn settings_save(ui: &mut Ui, root: &Path, value: String) {    let (field, global) = SETTINGS_FIELDS[ui.set_sel];
    let result = if global {
        match agent_core::config::exe_dir() {
            Some(dir) => {
                let mut cfg = Config::load_global(&dir);
                cfg.set(field, &value)
                    .and_then(|()| cfg.save_global(&dir).map_err(|e| e.to_string()))
            }
            None => Err("无法定位 do.exe 目录，全局配置层不可用".to_string()),
        }
    } else {
        let mut cfg = Config::load_workspace(root);
        cfg.set(field, &value)
            .and_then(|()| cfg.save(root).map_err(|e| e.to_string()))
    };
    match result {
        Ok(()) => {
            ui.set_values[ui.set_sel] = value.clone();
            // 两层都影响状态栏，保存后刷新
            if field == "model" {
                ui.model = if value.is_empty() { "未设置".into() } else { value.clone() };
            }
            if field == "key" {
                ui.has_key = !value.is_empty();
            }
            ui.items.push(Item::Info(format!(
                "已更新{} {field}",
                if global { " 全局" } else { "" }
            )));
        }
        Err(e) => ui.items.push(Item::Info(e)),
    }
}

/// agent 事件处理：流式增量拼到对话流最后一条同类记录上
fn handle_agent(ui: &mut Ui, ev: Evt) {
    match ev {
        Evt::Proposal(p) => {
            // 命令提案：入待批队列，对话流里给一条提示
            ui.items.push(Item::Info(format!(
                "命令提案: {} = `{}`（{}），/addcmd 审批",
                p.name, p.command, p.mode
            )));
            ui.pending.push(p);
        }
        Evt::Text(t) => {
            // 正文开始输出 → 展开中的思考块全部自动折叠。
            // 流式期间它们是实时可见的（open=true），正文一来就让位
            for item in &mut ui.items {
                if let Item::Reasoning { open, .. } = item {
                    *open = false;
                }
            }
            match ui.items.last_mut() {
                Some(Item::Assistant(s)) => s.push_str(&t),
                _ => ui.items.push(Item::Assistant(t)),
            }
        }
        Evt::Reasoning(r) => match ui.items.last_mut() {
            Some(Item::Reasoning { text, .. }) => text.push_str(&r),
            _ => ui.items.push(Item::Reasoning { text: r, open: true }),
        },
        Evt::Tool { name, args, result } => {
            ui.items.push(Item::Tool { name, args, result });
        }
        Evt::Error(e) => ui.items.push(Item::Error(e)),
        Evt::Tokens(n) => ui.tokens = n,
        Evt::Done => {
            ui.busy = false;
            if ui.cancel_pending {
                // 取消的收尾：半截内容已保留，补一行说明
                ui.cancel_pending = false;
                ui.items.push(Item::Info("（已取消）".into()));
            }
        }
    }
}

/// Enter：slash 命令本地处理，其余发给 agent
fn submit(ui: &mut Ui, agent: &mut AgentHandle, root: &Path) {
    let text = std::mem::take(&mut ui.input); // take 取出并清空（移动语义的便利方法）
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    if let Some(rest) = text.strip_prefix('/') {
        slash(ui, agent, root, rest);
        return;
    }
    if !ui.has_key {
        ui.items.push(Item::Info("未设置 API key，请用 /setting -g key <你的key>".into()));
        return;
    }
    ui.items.push(Item::User(text.clone()));
    ui.busy = true; // 进入工作态：Esc 变为可取消
    agent.send(Cmd::Chat(text));
}

/// slash 命令：/setting /new /quit
fn slash(ui: &mut Ui, agent: &mut AgentHandle, root: &Path, rest: &str) {
    // splitn(2, ...)：命令名与参数一刀两断，参数里允许含空格（start 需要）
    let mut parts = rest.trim().splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();
    match cmd {
        "quit" => ui.quit = true,
        // /addcmd：有待批提案则进入审批页（表单预填提案值，可改名再批）
        "addcmd" => match ui.pending.first() {
            Some(p) => {
                ui.appr_form = [p.name.clone(), p.description.clone()];
                ui.appr_sel = 0;
                ui.mode = Mode::Approve;
            }
            None => ui.items.push(Item::Info("无待审批提案".into())),
        },
        // /deletecmd：带名字直接删；不带则进入删除页
        "deletecmd" => {
            if arg.is_empty() {
                let cmds = agent_core::commands::load(root);
                if cmds.is_empty() {
                    ui.items.push(Item::Info("无已批准命令".into()));
                } else {
                    ui.del_list = cmds;
                    ui.del_sel = 0;
                    ui.mode = Mode::Delete;
                }
            } else {
                let mut cmds = agent_core::commands::load(root);
                let before = cmds.len();
                cmds.retain(|c| c.name != arg);
                if cmds.len() == before {
                    ui.items.push(Item::Info(format!("未找到已批准命令: {arg}")));
                } else {
                    match agent_core::commands::save(root, &cmds) {
                        Ok(()) => ui.items.push(Item::Info(format!("已撤销: {arg}"))),
                        Err(e) => ui.items.push(Item::Info(e.to_string())),
                    }
                }
            }
        }
        "new" => {
            // /new 即压缩：清历史，把 HANDOFF.md 作为第一条用户消息注入
            agent.send(Cmd::Reset);
            ui.items.clear();
            ui.tokens = 0;
            match std::fs::read_to_string(root.join("HANDOFF.md")) {
                Ok(h) if !h.trim().is_empty() => {
                    let msg = format!("交接文档 HANDOFF.md 内容如下：\n{h}");
                    ui.items.push(Item::User(msg.clone()));
                    if ui.has_key {
                        ui.busy = true;
                        agent.send(Cmd::Chat(msg));
                    }
                }
                _ => ui.items.push(Item::Info("已清空上下文（无 HANDOFF.md 可注入）".into())),
            }
        }
        // 裸 /setting（无参数）→ 进入独立设置页
        "setting" if arg.is_empty() => enter_settings(ui, root),
        "setting" => {
            // `-g` 前缀写全局便携层（exe 旁 do.config.json），否则写工作区层。
            // strip_prefix 命中时返回去掉前缀后的剩余切片（Option 语义）
            let (global, rest) = match arg.strip_prefix("-g") {
                Some(r) => (true, r.trim()),
                None => (false, arg),
            };
            let mut kv = rest.splitn(2, char::is_whitespace);
            let field = kv.next().unwrap_or("");
            let value = kv.next().unwrap_or("").trim();
            if field.is_empty() || value.is_empty() {
                ui.items.push(Item::Info(
                    "用法: /setting [-g] <url|key|model|start> <值>（-g 写全局层）".into(),
                ));
                return;
            }
            // 关键：写哪层就只读写哪层——绝不能把合并结果存回去，
            // 否则全局层的值会被"烤进"工作区层，覆盖关系就失效了
            let result = if global {
                match agent_core::config::exe_dir() {
                    Some(dir) => {
                        let mut cfg = Config::load_global(&dir);
                        cfg.set_global(field, value)
                            .and_then(|()| cfg.save_global(&dir).map_err(|e| e.to_string()))
                            .map(|()| format!("已更新 全局 {field}"))
                    }
                    None => Err("无法定位 do.exe 目录，全局配置层不可用".to_string()),
                }
            } else {
                let mut cfg = Config::load_workspace(root);
                cfg.set(field, value)
                    .and_then(|()| cfg.save(root).map_err(|e| e.to_string()))
                    .map(|()| format!("已更新 {field}"))
            };
            match result {
                Ok(msg) => {
                    if field == "model" {
                        ui.model = value.to_string();
                    }
                    if field == "key" {
                        ui.has_key = true;
                    }
                    ui.items.push(Item::Info(msg));
                }
                Err(e) => ui.items.push(Item::Info(e)),
            }
        }
        // 旧名 /addcmd /deletecmd 不做别名，直接落入未知命令提示——
        // 提示里列出新名，即一行迁移指引
        _ => ui.items.push(Item::Info(
            "未知命令（/setting /new /addcmd /deletecmd /quit）".into(),
        )),
    }
}

/// 进入设置页：载入两层当前值
fn enter_settings(ui: &mut Ui, root: &Path) {
    let global = agent_core::config::exe_dir().map(|d| Config::load_global(&d));
    let ws = Config::load_workspace(root);
    ui.set_values = SETTINGS_FIELDS
        .iter()
        .map(|(field, is_global)| {
            let cfg = if *is_global { global.as_ref() } else { Some(&ws) };
            cfg.map(|c| cfg_field(c, field)).unwrap_or_default()
        })
        .collect();
    ui.set_sel = 0;
    ui.set_editing = None;
    ui.mode = Mode::Settings;
}

/// 按字段名取配置值（设置页载入用）
fn cfg_field(cfg: &Config, field: &str) -> String {
    match field {
        "url" => cfg.url.clone(),
        "key" => cfg.key.clone(),
        "model" => cfg.model.clone(),
        "start" => cfg.start.clone(),
        _ => String::new(),
    }
}

/// key 掩码：保留前 3 后 4 位（如 sk-****xxxx）；太短的整串打码
fn mask_key(key: &str) -> String {
    let n = key.chars().count();
    if n > 7 {
        let head: String = key.chars().take(3).collect();
        let tail: String = key.chars().skip(n - 4).collect();
        format!("{head}****{tail}")
    } else {
        "****".to_string()
    }
}

/// slash 命令候选表：命令名 + 用法提示（渲染在状态栏上方的提示行）
/// 数组 + 切片 ≈ C# 的静态只读表；零分配、零组件，够用就好。
const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/setting", "/setting [-g] <url|key|model|start> <值>"),
    ("/new", "/new"),
    ("/addcmd", "/addcmd 审批命令提案"),
    ("/deletecmd", "/deletecmd [name] 撤销已批准命令"),
    ("/quit", "/quit"),
];

/// 按当前输入的前缀过滤候选（如 `/s` 只剩 /setting）。
/// 返回命中的用法提示文本；输入不以 `/` 开头或无命中时为空。
fn slash_hint(input: &str) -> String {
    if !input.starts_with('/') {
        return String::new();
    }
    SLASH_COMMANDS
        .iter()
        // filter + map 链 ≈ C# LINQ 的 Where + Select。
        // 双向前缀：`/s` 命中 /setting；`/setting m` 也已选定命令，继续显示其用法
        .filter(|(name, _)| name.starts_with(input) || input.starts_with(name))
        .map(|(_, usage)| *usage)
        .collect::<Vec<_>>()
        .join("  ")
}

/// Tab 补全：把输入替换为第一个候选的命令名
fn tab_complete(ui: &mut Ui) {
    if let Some((name, _)) = SLASH_COMMANDS
        .iter()
        .find(|(name, _)| name.starts_with(ui.input.as_str()) && ui.input.starts_with('/'))
    {
        ui.input = name.to_string();
    }
}

/// 工具调用的折叠态渲染：函数调用样式，更接近程序员直觉。
/// 取各工具主参数：read/write/edit/ls 取 path，grep 取 pattern+path，
/// start 无参；JSON 解析失败或主参数缺失时兜底 `name()`。
fn format_call(name: &str, args_json: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args_json) else {
        return format!("{name}()");
    };
    // {:?} 调试格式给字符串加引号并转义，正好就是调用语法里的字符串字面量
    let arg = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|s| format!("{s:?}"));
    match name {
        "read" | "write" | "edit" => match arg("path") {
            Some(p) => format!("{name}({p})"),
            None => format!("{name}()"),
        },
        "grep" => match (arg("pattern"), arg("path")) {
            (Some(p), Some(d)) => format!("grep({p}, {d})"),
            (Some(p), None) => format!("grep({p})"),
            _ => "grep()".to_string(),
        },
        "ls" => match arg("path") {
            Some(p) => format!("ls({p})"),
            None => "ls()".to_string(),
        },
        // 命令白名单工具：主参数是 name，如 runcmd("deploy")、addcmd("dev")
        "addcmd" | "runcmd" => match arg("name") {
            Some(n) => format!("{name}({n})"),
            None => format!("{name}()"),
        },
        _ => format!("{name}()"),
    }
}

/// 状态栏上方的上下文提示行：slash 候选优先，其次按模式给快捷键提示
fn hint_text(ui: &Ui) -> String {
    if ui.mode == Mode::Chat {
        let slash = slash_hint(&ui.input);
        if !slash.is_empty() {
            return slash; // slash 输入时，候选提示保持优先
        }
    }
    match ui.mode {
        Mode::Splash => "按任意键继续".to_string(),
        Mode::Settings => if ui.set_editing.is_some() {
            "Enter 保存 · Esc 放弃".to_string()
        } else {
            "↑↓ 选择 · Enter 编辑 · Esc 返回".to_string()
        },
        Mode::Approve => "↑↓/Tab 切换字段 · 直接输入编辑 · Enter 批准 · Esc 拒绝".to_string(),
        Mode::Delete => "↑↓ 选择 · Enter 删除 · Esc 返回".to_string(),
        Mode::Chat => if ui.busy {
            "^E 展开/折叠 · Esc 取消 · ↑↓/PgUp/Dn 滚动 · ^C 退出".to_string()
        } else {
            // agent 不忙时省略 Esc 项（此时 Esc 不响应）
            "^E 展开/折叠 · ↑↓/PgUp/Dn 滚动 · ^C 退出".to_string()
        },
    }
}

/// 渲染一帧
fn draw(term: &mut Terminal<CrosstermBackend<io::Stdout>>, ui: &Ui) -> io::Result<()> {
    term.draw(|frame| {
        let area = frame.area();
        if ui.mode == Mode::Splash {
            draw_splash(frame, area, ui);
            return;
        }
        // 纵向四段：对话流（吃满剩余）/ 提示行 / 输入行 / 状态栏
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        // 主区域：Chat = 对话流；Settings/Approve/Delete = 各自页面
        let width = chunks[0].width as usize;
        let lines = match ui.mode {
            Mode::Settings => settings_lines(ui),
            Mode::Approve => approve_lines(ui, width),
            Mode::Delete => delete_lines(ui),
            _ => build_lines(ui, width),
        };
        let height = chunks[0].height as usize;
        let total = lines.len();
        // 页面类不滚动（内容一屏内），对话流正常滚动
        let scroll = if ui.mode == Mode::Chat { ui.scroll } else { 0 };
        let bottom_skip = scroll.min(total.saturating_sub(height));
        let start = total.saturating_sub(height + bottom_skip);
        let visible: Vec<Line> = lines
            .into_iter()
            .skip(start)
            .take(height)
            .collect();
        frame.render_widget(Paragraph::new(visible), chunks[0]);

        // 提示行（上下文感知，slash 候选优先）
        frame.render_widget(
            Paragraph::new(hint_text(ui)).style(Style::default().fg(Color::DarkGray)),
            chunks[1],
        );

        // 输入行：Chat = 输入框；Settings 编辑态 = 字段编辑框；审批/删除页无输入行
        match ui.mode {
            Mode::Approve | Mode::Delete => {
                frame.render_widget(Paragraph::new(""), chunks[2]);
            }
            Mode::Settings => {
                if let Some(buf) = &ui.set_editing {
                    let (field, _) = SETTINGS_FIELDS[ui.set_sel];
                    let prompt = format!("{field} = {buf}");
                    frame.render_widget(Paragraph::new(prompt), chunks[2]);
                    let cx = field.len() + 3 + unicode_width::UnicodeWidthStr::width(buf.as_str());
                    frame.set_cursor_position((
                        (cx as u16).min(chunks[2].width.saturating_sub(1)),
                        chunks[2].y,
                    ));
                } else {
                    frame.render_widget(Paragraph::new(""), chunks[2]);
                }
            }
            _ => {
                let prompt = format!("> {}", ui.input);
                frame.render_widget(Paragraph::new(prompt), chunks[2]);
                // 光标定位到输入末尾（CJK 按显示宽度算）
                let cx = 2 + unicode_width::UnicodeWidthStr::width(ui.input.as_str());
                frame.set_cursor_position((
                    (cx as u16).min(chunks[2].width.saturating_sub(1)),
                    chunks[2].y,
                ));
            }
        }

        // 状态栏：~N tok | model | 工作区
        let status = format!("~{} tok | {} | {}", ui.tokens, ui.model, ui.workspace);
        frame.render_widget(
            Paragraph::new(status).style(Style::default().fg(Color::DarkGray)),
            chunks[3],
        );
    })?;
    Ok(())
}

/// 启动画面：logo 居中 + 版本号 + 配置状态 + 跳过提示
fn draw_splash(frame: &mut Frame, area: Rect, ui: &Ui) {
    let mut lines: Vec<Line> = LOGO
        .lines()
        .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(Color::Cyan))))
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(format!("DoAgent v{}", env!("CARGO_PKG_VERSION"))));
    lines.push(Line::from(format!("工作区: {}", ui.workspace)));
    lines.push(Line::from(if ui.has_key {
        "key 已设置".to_string()
    } else {
        "key 未设置：/setting -g key <你的key>".to_string()
    }));
    lines.push(Line::from(format!("model: {}", ui.model)));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "按任意键继续",
        Style::default().fg(Color::DarkGray),
    )));
    // 垂直居中：上缘下移 (高度-内容)/2；水平居中交给 Alignment::Center
    let content_h = lines.len() as u16;
    let top = area.height.saturating_sub(content_h) / 2;
    let centered = Rect {
        y: area.y + top,
        height: content_h.min(area.height),
        ..area // 结构体更新语法：其余字段（x/width）沿用 area
    };
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), centered);
}

/// 设置页内容：全局区 + 工作区，选中行青色高亮
fn settings_lines(ui: &Ui) -> Vec<Line<'static>> {
    let section = Style::default().fg(Color::DarkGray);
    let mut out = vec![
        Line::from(""),
        Line::from(Span::styled("全局（exe 旁 do.config.json）", section)),
    ];
    for (i, (field, _)) in SETTINGS_FIELDS.iter().enumerate() {
        if i == 3 {
            out.push(Line::from(""));
            out.push(Line::from(Span::styled("工作区（.do/config.json）", section)));
        }
        // key 显示掩码；空值显示占位
        let shown = if *field == "key" {
            mask_key(&ui.set_values[i])
        } else {
            ui.set_values[i].clone()
        };
        let shown = if shown.is_empty() { "（未设置）".to_string() } else { shown };
        let selected = i == ui.set_sel;
        let style = if selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        out.push(Line::from(Span::styled(
            format!("{} {field:<6} = {shown}", if selected { ">" } else { " " }),
            style,
        )));
    }
    out
}

/// 审批页：command 全文只读展示，name/description 两行可编辑（选中行青色）
fn approve_lines(ui: &Ui, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let section = Style::default().fg(Color::DarkGray);
    let Some(p) = ui.pending.first() else {
        return out;
    };
    out.push(Line::from(""));
    out.push(Line::from(Span::styled(
        "命令提案审批（command 不可修改 —— 审批的字符串 = 永远执行的全部内容）",
        section,
    )));
    out.push(Line::from(""));
    // command 可能很长，按宽度折行完整展示
    for l in wrap_text(&format!("command = {}", p.command), width.max(8)) {
        out.push(Line::from(Span::styled(l, Style::default().fg(Color::Yellow))));
    }
    out.push(Line::from(Span::styled(format!("mode    = {}", p.mode), section)));
    let labels = ["name", "desc"];
    for (i, label) in labels.iter().enumerate() {
        let selected = i == ui.appr_sel;
        let style = if selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        out.push(Line::from(Span::styled(
            format!("{} {label:<6} = {}", if selected { ">" } else { " " }, ui.appr_form[i]),
            style,
        )));
    }
    if ui.pending.len() > 1 {
        out.push(Line::from(""));
        out.push(Line::from(Span::styled(
            format!("（还有 {} 条待批）", ui.pending.len() - 1),
            section,
        )));
    }
    out
}

/// 删除页：已批准命令列表，选中行青色
fn delete_lines(ui: &Ui) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    out.push(Line::from(""));
    out.push(Line::from(Span::styled(
        "已批准命令（Enter 删除即撤销该工具）",
        Style::default().fg(Color::DarkGray),
    )));
    for (i, c) in ui.del_list.iter().enumerate() {
        let selected = i == ui.del_sel;
        let style = if selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        out.push(Line::from(Span::styled(
            format!(
                "{} {:<12} = `{}`（{}）",
                if selected { ">" } else { " " },
                c.name,
                c.command,
                c.mode
            ),
            style,
        )));
    }
    out
}

/// 把对话流摊平成物理行（含折行），每行一个样式
fn build_lines(ui: &Ui, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let w = width.max(8);
    for item in &ui.items {
        match item {
            Item::User(t) => push_styled(&mut out, &format!("» {t}"), w, Style::default()),
            Item::Assistant(t) => {
                // 正文过 Markdown 渲染：每帧对完整文本重新解析（量小无感），
                // 流式中间态的不完整 md 由解析器自然降级（见 md 模块导读）
                for line in crate::md::render(t) {
                    // span 级折行：在 span 边界内按字符宽度切，样式不被切断
                    out.extend(wrap_spans(line, w));
                }
            }
            Item::Reasoning { text, open } => {
                let style = Style::default().fg(Color::DarkGray);
                if ui.expand_all || *open {
                    push_styled(&mut out, &format!("思考: {text}"), w, style);
                } else {
                    push_styled(&mut out, &format!("思考 (+{} 字)", text.chars().count()), w, style);
                }
            }
            Item::Tool { name, args, result } => {
                // 折叠态：函数调用样式，如 read("src/main.rs")
                let style = Style::default().fg(Color::Cyan);
                push_styled(&mut out, &format_call(name, args), w, style);
                if ui.expand_all {
                    // 展开态：完整 args（原始 JSON）+ 截断后的结果
                    let dim = style.add_modifier(Modifier::DIM);
                    push_styled(&mut out, &indent(args), w, dim);
                    push_styled(&mut out, &indent(result), w, dim);
                }
            }
            Item::Info(t) => push_styled(&mut out, t, w, Style::default().fg(Color::DarkGray)),
            Item::Error(t) => push_styled(&mut out, &format!("错误: {t}"), w, Style::default().fg(Color::Red)),
        }
    }
    out
}

/// 文本折行后按统一样式追加为若干物理行
fn push_styled(out: &mut Vec<Line<'static>>, text: &str, width: usize, style: Style) {
    for l in wrap_text(text, width) {
        out.push(Line::from(Span::styled(l, style)));
    }
}

/// 工具结果缩进两格（展开态）
fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// span 感知折行：与 wrap_text 同样的贪心切分，但输入是带样式的
/// Line——逐字符推进、同样式合并，保证样式 span 在宽字符（CJK）处
/// 也不会被从中间切断（切的是字符边界，不是样式边界）。
fn wrap_spans(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut w = 0;
    for span in line.spans {
        let style = span.style;
        for ch in span.content.chars() {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if w + cw > width && !cur.is_empty() {
                out.push(Line::from(std::mem::take(&mut cur)));
                w = 0;
            }
            // 与行尾 span 同样式就追加合并，避免一字符一 span 的碎片
            match cur.last_mut() {
                Some(last) if last.style == style => last.content.to_mut().push(ch),
                _ => cur.push(Span::styled(ch.to_string(), style)),
            }
            w += cw;
        }
    }
    out.push(Line::from(cur));
    out
}

/// CJK 感知的折行：按字符显示宽度贪心切分。
/// unicode-width 给出每个 char 的终端列宽（ASCII=1，CJK=2）。
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let mut cur = String::new();
        let mut w = 0;
        for ch in raw.chars() {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if w + cw > width && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                w = 0;
            }
            cur.push(ch);
            w += cw;
        }
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个最小可用的 Ui（测试夹具）
    fn test_ui() -> Ui {
        Ui {
            items: Vec::new(),
            input: String::new(),
            scroll: 0,
            expand_all: false,
            tokens: 0,
            model: String::new(),
            has_key: false,
            workspace: String::new(),
            quit: false,
            mode: Mode::Chat,
            busy: false,
            cancel_pending: false,
            set_sel: 0,
            set_editing: None,
            set_values: Vec::new(),
            pending: Vec::new(),
            appr_sel: 0,
            appr_form: [String::new(), String::new()],
            del_list: Vec::new(),
            del_sel: 0,
        }
    }

    #[test]
    fn slash_hint_filters_by_prefix() {
        assert!(slash_hint("").is_empty()); // 非 / 开头无提示
        assert!(slash_hint("abc").is_empty());
        assert!(slash_hint("/x").is_empty()); // 无命中
        // `/s` 只剩 /setting
        assert_eq!(slash_hint("/s"), "/setting [-g] <url|key|model|start> <值>");
        // `/` 显示全部候选
        let all = slash_hint("/");
        assert!(all.contains("/setting") && all.contains("/new") && all.contains("/quit"));
        // 已选定命令继续敲参数时仍显示其用法
        assert!(slash_hint("/setting model g").contains("/setting"));
        // 完整命令名命中自身
        assert_eq!(slash_hint("/new"), "/new");
    }

    #[test]
    fn tab_completes_first_candidate() {
        let mut ui = test_ui();
        ui.input = "/s".to_string();
        tab_complete(&mut ui);
        assert_eq!(ui.input, "/setting");
        // 非 slash 输入不补全
        ui.input = "abc".to_string();
        tab_complete(&mut ui);
        assert_eq!(ui.input, "abc");
    }

    #[test]
    fn reasoning_auto_collapses_when_text_starts() {
        let mut ui = test_ui();
        // 流式期间：思考块实时可见（open=true）
        handle_agent(&mut ui, Evt::Reasoning("想想".into()));
        match ui.items.last() {
            Some(Item::Reasoning { open, .. }) => assert!(open),
            _ => panic!("应为 Reasoning"),
        }
        // 正文开始输出 → 思考块自动折叠
        handle_agent(&mut ui, Evt::Text("答".into()));
        match &ui.items[0] {
            Item::Reasoning { open, .. } => assert!(!open),
            _ => panic!("应为 Reasoning"),
        }
        // 取消的收尾：Done 到达时补一行"（已取消）"
        ui.busy = true;
        ui.cancel_pending = true;
        handle_agent(&mut ui, Evt::Done);
        assert!(!ui.busy);
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("已取消")));
    }

    #[test]
    fn tool_call_rendered_as_function() {
        assert_eq!(format_call("read", "{\"path\":\"src/main.rs\"}"), "read(\"src/main.rs\")");
        assert_eq!(
            format_call("grep", "{\"pattern\":\"foo\",\"path\":\"src/\"}"),
            "grep(\"foo\", \"src/\")"
        );
        assert_eq!(format_call("grep", "{\"pattern\":\"foo\"}"), "grep(\"foo\")");
        assert_eq!(format_call("ls", "{}"), "ls()");
        assert_eq!(format_call("start", "{}"), "start()");
        // JSON 解析失败 / 主参数缺失 → 兜底 name()
        assert_eq!(format_call("read", "not json"), "read()");
        assert_eq!(format_call("write", "{}"), "write()");
    }

    #[test]
    fn key_masking_keeps_head3_tail4() {
        assert_eq!(mask_key("sk-abcdefghij"), "sk-****ghij");
        assert_eq!(mask_key("short"), "****");
        assert_eq!(mask_key(""), "****");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn addcmd_slash_behaviour() {
        let dir = std::env::temp_dir().join(format!("doagent-tui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut agent = AgentHandle::start(&dir).unwrap();
        let mut ui = test_ui();
        // 无待批提案时 /addcmd 只提示，不进审批页
        slash(&mut ui, &mut agent, &dir, "addcmd");
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("无待审批提案")));
        assert_eq!(ui.mode, Mode::Chat);
        // 旧名 /addcmd /deletecmdmd 不再识别，提示里列出新名（迁移指引）
        slash(&mut ui, &mut agent, &dir, "addc");
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("未知命令") && t.contains("/addcmd")));
        slash(&mut ui, &mut agent, &dir, "deletec");
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("未知命令") && t.contains("/deletecmd")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn proposal_event_queues_pending() {
        // AI 提案事件 → 进待批队列 + 对话流提示
        let mut ui = test_ui();
        handle_agent(
            &mut ui,
            Evt::Proposal(ApprovedCommand {
                name: "dev".into(),
                command: "npm run dev".into(),
                description: "开发服务器".into(),
                mode: "daemon".into(),
            }),
        );
        assert_eq!(ui.pending.len(), 1);
        assert_eq!(ui.pending[0].name, "dev");
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("/addcmd")));
    }

    #[test]
    fn hint_line_is_context_aware() {
        let mut ui = test_ui();
        // 对话空闲：无 Esc 项
        assert!(!hint_text(&ui).contains("Esc 取消"));
        // agent 工作：出现 Esc 取消
        ui.busy = true;
        assert!(hint_text(&ui).contains("Esc 取消"));
        // slash 输入时候选提示优先
        ui.input = "/s".to_string();
        assert!(hint_text(&ui).contains("/setting"));
        // 设置页
        ui.input.clear();
        ui.mode = Mode::Settings;
        assert!(hint_text(&ui).contains("Enter 编辑"));
    }

    #[test]
    fn wrap_spans_preserves_styles() {
        // 8 列宽里塞 4 个默认字符 + 4 个加粗字符：应在第 8 列后折成两行
        let line = Line::from(vec![
            Span::styled("aaaa", Style::default()),
            Span::styled("bbbb", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled("cc", Style::default().add_modifier(Modifier::BOLD)),
        ]);
        let wrapped = wrap_spans(line, 8);
        assert_eq!(wrapped.len(), 2);
        // 第二行的 "cc" 仍是加粗（样式不被折行切断）
        let cc = wrapped[1].spans.iter().find(|s| s.content == "cc").unwrap();
        assert!(cc.style.add_modifier.contains(Modifier::BOLD));
        // 折行点字符不丢
        let all: String = wrapped
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert_eq!(all, "aaaabbbbcc");
    }
}
