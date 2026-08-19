//! TUI：启动画面 + 对话流 + 输入 + slash 命令 + 状态栏（模块根）
//!
//! # 模块导读
//! 单事件循环驱动一切：crossterm 的键盘事件由一个专用线程以
//! poll(100ms) 心跳转发进 tokio channel，与 agent actor 的事件在
//! `tokio::select!` 里汇合——全部非阻塞等待，单线程 runtime 够用。
//!
//! # 子模块拆分（纯结构搬迁，行为不变）
//! - `pages`：各全屏页面的渲染（splash / 设置页 / 审批页 / 删除页）
//! - `forms`：各页面的按键处理与落盘动作（设置保存、批准/拒绝/删除）
//! - 本文件：界面模式与状态、主循环、对话流渲染、slash 命令
//!
//! # 界面模式（[`Mode`]）
//! - Splash：启动画面，logo + 版本 + 配置状态；按任意键进入对话
//! - Chat：对话主界面（流式正文/思考/工具调用、slash 命令、Esc 取消）
//! - Settings / Approve / Delete：/setting、/allowcmd、/deletecmd 三个页面
//!
//! # 交互约定（Chat 模式）
//! - Enter 发送；Ctrl+C 退出；Esc 取消当前轮（仅 agent 工作时响应）；
//!   Ctrl+E 全部展开/折叠；PageUp/PageDown 滚动。
//! - 配色：思考灰、工具青、正文默认色。折叠块只显示一行摘要。
//! - 底部两行：快捷键提示行（上下文感知）+ 状态栏 `~N tok | model | 工作区`。
//!
//! # 核心概念
//! - RAII / Drop：[`TerminalGuard`] 离开作用域时自动恢复终端——
//!   ≈ C# 的 `using` + IDisposable，但由编译器保证调用，不靠 using 块。
//! - 生命周期标注 `'static`：ratatui 的 `Line<'a>` 借用文本；
//!   我们全部用 `String`（拥有所有权），所以是 `Line<'static>`，
//!   不与缓冲区抢借用，渲染代码因此简单很多。
//! - 子模块可见性：`pub(super)` ≈ C# 的 internal——只对父模块 tui 可见，
//!   子模块可以直接看到父模块的私有项（Rust 的可见性向下渗透）。

use agent_core::config::Config;
use agent_core::{AgentHandle, ApprovedCommand, Cmd, Evt};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;
use std::io;
use std::path::Path;
use unicode_width::UnicodeWidthChar;

mod forms;
mod pages;

use forms::{approve_key, delete_key, enter_settings, settings_key};
use pages::{approve_lines, delete_lines, draw_splash, settings_lines};

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// 启动画面：任意键或定时器自动跳过，不强制等待
    Splash,
    /// 对话主界面
    Chat,
    /// /setting 独立设置页
    Settings,
    /// /allowcmd 命令提案审批页（command 只读全文，name 不可改，desc 可按 e 编辑）
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
    /// 一次工具调用：args 是**原始 JSON 字符串**，展示层决定怎么渲染。
    /// result = None 表示进行中（ToolStart 已发、结果未回），
    /// 渲染为 doing 动画；取消时遗留的进行中块会被标成"（已取消）"。
    Tool { name: String, args: String, result: Option<String> },
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

/// 取消标记：agent 取消时遗留进行中块的结果文本（渲染层据此显红色"已取消"）
const CANCEL_MARK: &str = "（已取消）";

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
    /// 设置页：进入时载入的各字段**生效值**（合并视图；key 存真值，显示时才掩码）
    set_values: Vec<String>,
    /// 设置页：各字段来源层标注（"工作区"/"全局"/"默认"/""）
    set_sources: Vec<&'static str>,
    /// 待审批的命令提案队列（AI addcmd 提案或人类 /addcmd 自助注册，内存态不落盘）
    pending: Vec<ApprovedCommand>,
    /// 审批页：列表选中下标、desc 编辑态标志与编辑前备份（Esc 放弃时还原）
    appr_sel: usize,
    appr_editing: bool,
    appr_desc_backup: String,
    /// 删除页：进入时载入的已批准命令列表与选中下标
    del_list: Vec<ApprovedCommand>,
    del_sel: usize,
    /// Tick 心跳计数（100ms 一次）：驱动工具调用 doing 动画帧
    tick_count: u64,
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
    // poll(100ms) 超时即发 Tick：主循环借此做定时逻辑（splash 自动跳过），
    // 这是 TUI 的经典心跳模式，比在 select! 里挂定时器分支行为更确定。
    let (key_tx, mut key_rx) = tokio::sync::mpsc::unbounded_channel::<TermEvent>();
    std::thread::spawn(move || {
        loop {
            let ev = if crossterm::event::poll(std::time::Duration::from_millis(100)).unwrap_or(false)
            {
                match crossterm::event::read() {
                    Ok(e) => TermEvent::Key(e),
                    Err(_) => break,
                }
            } else {
                TermEvent::Tick
            };
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
        set_sources: Vec::new(),
        pending: Vec::new(),
        appr_sel: 0,
        appr_editing: false,
        appr_desc_backup: String::new(),
        del_list: Vec::new(),
        del_sel: 0,
        tick_count: 0,
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

    while !ui.quit {
        draw(&mut term, &ui)?;
        // select! ≈ C# 的 Task.WhenAny：哪个事件先到处理哪个
        tokio::select! {
            ev = key_rx.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    TermEvent::Key(e) => handle_key(&mut ui, e, &mut agent, root),
                    TermEvent::Tick => tick(&mut ui),
                }
            }
            ev = agent.next() => {
                let Some(ev) = ev else { break };
                handle_agent(&mut ui, ev);
            }
        }
    }
    Ok(())
}

/// 终端事件：真实按键 / 心跳（100ms 一次，驱动 doing 动画帧）
enum TermEvent {
    Key(Event),
    Tick,
}

/// 心跳处理：只推进动画帧计数。
/// splash 不自动跳过（拍板：停留到按任意键）——tick 到达不改模式。
/// 抽成独立函数保持可测（帧推进是纯状态判断）。
fn tick(ui: &mut Ui) {
    ui.tick_count = ui.tick_count.wrapping_add(1);
}

/// doing 动画帧：每 5 个 tick（≈500ms）换一个点号数量，1→2→3 循环。
/// 纯函数 ≈ C# 的 static 方法：同样输入恒定输出，单测直接断言。
fn ellipsis_frame(tick_count: u64) -> &'static str {
    match (tick_count / 5) % 3 {
        0 => "doing.",
        1 => "doing..",
        _ => "doing...",
    }
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
fn handle_agent(ui: &mut Ui, ev: Evt) {
    match ev {
        Evt::Proposal(p) => {
            // 命令提案：入待批队列，对话流里给一条提示
            ui.items.push(Item::Info(format!(
                "命令提案: {} = `{}`（{}），/allowcmd 审批",
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
        Evt::ToolStart { name, args } => {
            // 派发即插入进行中块：用户立刻看到工具名，不用等执行结束
            ui.items.push(Item::Tool { name, args, result: None });
        }
        Evt::Tool { name, args, result } => {
            // 更新最后一个未完成块（多工具顺序执行时按序匹配）；
            // 找不到（如取消前未发 ToolStart 的）则补一个完成态块。
            // name/args 也要覆盖：提前宣告时 args 是空串，完成后补上真实参数
            let idx = ui
                .items
                .iter()
                .rposition(|i| matches!(i, Item::Tool { result: None, .. }));
            match idx {
                Some(i) => {
                    if let Item::Tool { name: n, args: a, result: r, .. } = &mut ui.items[i] {
                        *n = name;
                        *a = args;
                        *r = Some(result);
                    }
                }
                None => ui.items.push(Item::Tool {
                    name,
                    args,
                    result: Some(result),
                }),
            }
        }
        Evt::Error(e) => ui.items.push(Item::Error(e)),
        Evt::Tokens(n) => ui.tokens = n,
        Evt::Done => {
            ui.busy = false;
            if ui.cancel_pending {
                // 取消的收尾：半截内容已保留，补一行说明；
                // 遗留的进行中工具块标"已取消"，不永远挂 doing 动画
                ui.cancel_pending = false;
                for item in &mut ui.items {
                    if let Item::Tool { result: r @ None, .. } = item {
                        *r = Some(CANCEL_MARK.to_string());
                    }
                }
                ui.items.push(Item::Info(CANCEL_MARK.into()));
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
        // /addcmd：仅人类自助注册（name 与命令全文必填）。
        // 注册后仍开审批页确认——保持"落盘前必有确认"的不变量
        "addcmd" => {
            let mut kv = arg.splitn(2, char::is_whitespace);
            let name = kv.next().unwrap_or("");
            let command = kv.next().unwrap_or("").trim();
            if name.is_empty() || command.is_empty() {
                ui.items.push(Item::Info("用法: /addcmd <name> <命令全文>".into()));
                return;
            }
            if !agent_core::commands::valid_name(name) {
                ui.items.push(Item::Info("name 只能包含字母/数字/_/-".into()));
                return;
            }
            ui.pending.push(ApprovedCommand {
                name: name.to_string(),
                command: command.to_string(),
                description: String::new(), // desc 默认空，审批页可按 e 补
                mode: "once".into(),        // 自助注册默认一次性
            });
            // 选中刚注册的这条（队尾）
            ui.appr_sel = ui.pending.len() - 1;
            ui.appr_editing = false;
            ui.mode = Mode::Approve;
        }
        // /allowcmd：仅打开审批页处理 AI 待批提案
        "allowcmd" => {
            if ui.pending.is_empty() {
                ui.items.push(Item::Info("无待审批提案".into()));
            } else {
                ui.appr_sel = 0;
                ui.appr_editing = false;
                ui.mode = Mode::Approve;
            }
        }
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
        // 旧名 /addc /deletec /allowc 不做别名，直接落入未知命令提示——
        // 提示里列出新名，即一行迁移指引
        _ => ui.items.push(Item::Info(
            "未知命令（/setting /new /addcmd /allowcmd /deletecmd /quit）".into(),
        )),
    }
}

/// 进入设置页：载入各字段的**生效值**（合并视图）与来源层。
/// 修复历史 bug：旧版按"字段归属层"各读单层——若值写在工作区层
/// （如 `/setting key` 不带 -g），全局字段行就会错误地显示"未设置"。
/// 显示用合并值；保存纪律不变（写哪层只写哪层，见 settings_save）。
const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/setting", "/setting [-g] <url|key|model|start> <值>"),
    ("/new", "/new"),
    ("/addcmd", "/addcmd <name> <命令全文>"),
    ("/allowcmd", "/allowcmd 审批命令提案"),
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
        Mode::Approve => if ui.appr_editing {
            "Enter 保存 · Esc 放弃".to_string()
        } else {
            "↑↓ 选择 · Enter 批准 · x 拒绝 · e 编辑描述 · Esc 返回".to_string()
        },
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
                // 折叠态：函数调用样式（青色）+ 状态字（分色）：
                // doing 黄（动画帧随 tick 推进）/ done 绿 / 已取消 红
                let style = Style::default().fg(Color::Cyan);
                let (word, word_style) = match result {
                    None => (ellipsis_frame(ui.tick_count), Style::default().fg(Color::Yellow)),
                    Some(r) if r == CANCEL_MARK => {
                        (CANCEL_MARK, Style::default().fg(Color::Red))
                    }
                    Some(_) => ("done", Style::default().fg(Color::Green)),
                };
                let line = Line::from(vec![
                    Span::styled(format_call(name, args), style),
                    Span::styled(format!(" {word}"), word_style),
                ]);
                out.extend(wrap_spans(line, w));
                if ui.expand_all {
                    // 展开态：完整 args（原始 JSON）+ 截断后的结果
                    let dim = style.add_modifier(Modifier::DIM);
                    push_styled(&mut out, &indent(args), w, dim);
                    if let Some(r) = result {
                        push_styled(&mut out, &indent(r), w, dim);
                    }
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
fn test_ui() -> Ui {
    // 测试夹具：最小可用的 Ui
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
        set_sources: Vec::new(),
        pending: Vec::new(),
        appr_sel: 0,
        appr_editing: false,
        appr_desc_backup: String::new(),
        del_list: Vec::new(),
        del_sel: 0,
        tick_count: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test(flavor = "current_thread")]
    async fn addcmd_allowcmd_slash_behaviour() {
        let dir = std::env::temp_dir().join(format!("doagent-tui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut agent = AgentHandle::start(&dir).unwrap();
        let mut ui = test_ui();
        // 裸 /addcmd：不再开审批页，给用法提示（职责单一：仅自助注册）
        slash(&mut ui, &mut agent, &dir, "addcmd");
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("用法")));
        assert_eq!(ui.mode, Mode::Chat);
        // /allowcmd：无待批提案时只提示，不进审批页
        slash(&mut ui, &mut agent, &dir, "allowcmd");
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("无待审批提案")));
        assert_eq!(ui.mode, Mode::Chat);
        // 旧名 /addc /deletec /allowc 不再识别，提示里列出新名（迁移指引）
        slash(&mut ui, &mut agent, &dir, "addc");
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("未知命令") && t.contains("/addcmd")));
        slash(&mut ui, &mut agent, &dir, "deletec");
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("未知命令") && t.contains("/deletecmd")));
        slash(&mut ui, &mut agent, &dir, "allowc");
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("未知命令") && t.contains("/allowcmd")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_start_then_result_updates_same_block() {
        let mut ui = test_ui();
        // ToolStart 插入进行中块
        handle_agent(&mut ui, Evt::ToolStart { name: "read".into(), args: "{\"path\":\"a.rs\"}".into() });
        assert!(matches!(ui.items.last(), Some(Item::Tool { result: None, .. })));
        // Tool 结果到来 → 更新同一块而不是新增
        handle_agent(&mut ui, Evt::Tool { name: "read".into(), args: "{}".into(), result: "ok".into() });
        assert_eq!(ui.items.iter().filter(|i| matches!(i, Item::Tool { .. })).count(), 1);
        assert!(matches!(ui.items.last(), Some(Item::Tool { result: Some(r), .. }) if r == "ok"));
        // 再来一对：顺序匹配更新
        handle_agent(&mut ui, Evt::ToolStart { name: "ls".into(), args: "{}".into() });
        handle_agent(&mut ui, Evt::Tool { name: "ls".into(), args: "{}".into(), result: "ls-ok".into() });
        assert_eq!(ui.items.len(), 2);
        // 取消收尾：遗留进行中块标"已取消"
        handle_agent(&mut ui, Evt::ToolStart { name: "grep".into(), args: "{}".into() });
        ui.busy = true;
        ui.cancel_pending = true;
        handle_agent(&mut ui, Evt::Done);
        assert!(matches!(&ui.items[2], Item::Tool { result: Some(r), .. } if r == "（已取消）"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn splash_stays_until_keypress() {
        let dir = std::env::temp_dir().join(format!("doagent-splash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut agent = AgentHandle::start(&dir).unwrap();
        let mut ui = test_ui();
        ui.mode = Mode::Splash;
        // tick 到达不迁移模式（只推进动画帧）
        for _ in 0..20 {
            tick(&mut ui);
        }
        assert_eq!(ui.mode, Mode::Splash);
        assert_eq!(ui.tick_count, 20);
        // 任意键（Press）进入对话
        handle_key(
            &mut ui,
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            &mut agent,
            &dir,
        );
        assert_eq!(ui.mode, Mode::Chat);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ellipsis_frame_cycles() {
        // 每 5 tick 换一帧：doing. → doing.. → doing... → 循环
        assert_eq!(ellipsis_frame(0), "doing.");
        assert_eq!(ellipsis_frame(4), "doing.");
        assert_eq!(ellipsis_frame(5), "doing..");
        assert_eq!(ellipsis_frame(10), "doing...");
        assert_eq!(ellipsis_frame(15), "doing."); // 回绕
    }

    #[test]
    fn tool_status_word_colors() {
        // 三态分色：doing 黄 / done 绿 / 已取消 红；工具名保持青色
        let mut ui = test_ui();
        ui.items.push(Item::Tool { name: "read".into(), args: "{\"path\":\"a.rs\"}".into(), result: None });
        ui.items.push(Item::Tool { name: "ls".into(), args: "{}".into(), result: Some("x".into()) });
        ui.items.push(Item::Tool { name: "grep".into(), args: "{}".into(), result: Some(CANCEL_MARK.into()) });
        let lines = build_lines(&ui, 80);
        // 每条工具块一行两 span：[青色函数调用, 分色状态字]
        let status = |i: usize| (lines[i].spans[1].content.to_string(), lines[i].spans[1].style.fg);
        assert_eq!(status(0), (" doing.".to_string(), Some(Color::Yellow)));
        assert_eq!(status(1), (" done".to_string(), Some(Color::Green)));
        assert_eq!(status(2), (" （已取消）".to_string(), Some(Color::Red)));
        for line in &lines {
            assert_eq!(line.spans[0].style.fg, Some(Color::Cyan));
        }
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
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("/allowcmd")));
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
