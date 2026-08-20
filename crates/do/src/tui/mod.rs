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

use crate::lang::{Key, Lang};
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
    /// 启动画面：停留到按任意键才进入对话（tick 心跳刻意不跳过）
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
    /// 用户输入（原样入流，不注入任何附加内容）
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

/// 设置页字段表：字段名 + 是否全局层（false = 工作区层）。
/// 当前三项全是全局身份项（bool 恒 true，工作区 Section 为空则不显示）；
/// bool 保留为结构占位：settings_save 的双层分支已就位，将来若要
/// 加工作区级字段只需在表里补一行
const SETTINGS_FIELDS: &[(&str, bool)] = &[
    ("url", true),
    ("key", true),
    ("model", true),
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
    /// 界面语言（/lang zh|en 切换；默认 en）
    lang: Lang,
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
    /// 删除页：进入时载入的合并视图（命令 + 来源层）与选中下标
    del_list: Vec<(ApprovedCommand, agent_core::Layer)>,
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
        // EnterAlternateScreen 失败时先手动关 raw mode 再返回——
        // 此刻守卫尚未构造，Drop 不会兜底，直接 `?` 会把终端烂在 raw mode
        if let Err(e) = execute!(io::stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(e);
        }
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
    // poll(100ms) 超时即发 Tick：主循环借此驱动工具状态字的 doing 动画帧，
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
        lang: Lang::parse(&cfg.lang),
        model: cfg.model.clone(),
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
    ui.items.push(Item::Info(format!(
        "{}: {}",
        ui.lang.t(Key::WorkspaceLabel),
        ui.workspace
    )));
    if exe_dir.is_none() {
        // 降级提示：全局层不可用但不影响使用，绝不 panic
        ui.items.push(Item::Info(ui.lang.t(Key::GlobalLayerUnavailable).into()));
    }
    if !ui.has_key {
        ui.items.push(Item::Info(ui.lang.t(Key::ApiKeyMissing).into()));
    }
    // 审计可用性检查：不可写只是降级，但用户应当知道
    if !agent_core::audit::Audit::new(root).enabled() {
        ui.items.push(Item::Info(ui.lang.t(Key::AuditDisabled).into()));
    }

    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend)?;

    // dirty 标志：只在显示内容可能变化后才重渲染。
    // 此前每个 100ms tick 都全量 build_lines（含全部 Assistant 文本的
    // Markdown 重解析），idle 时每秒白跑 10 次；现在 tick 仅在
    // "存在进行中工具块"（doing 点号动画要推进）时才置脏。
    // 按键/agent 事件一律置脏（保守策略：个别无效按键多画一帧无感，
    // 漏置脏导致画面不更新才是 bug）；resize 走按键通道同样被覆盖。
    let mut dirty = true; // 首帧必画
    while !ui.quit {
        if dirty {
            draw(&mut term, &ui)?;
            dirty = false;
        }
        // select! ≈ C# 的 Task.WhenAny：哪个事件先到处理哪个
        tokio::select! {
            ev = key_rx.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    TermEvent::Key(e) => {
                        handle_key(&mut ui, e, &mut agent, root);
                        dirty = true;
                    }
                    TermEvent::Tick => {
                        tick(&mut ui);
                        if needs_anim_frame(&ui) {
                            dirty = true;
                        }
                    }
                }
            }
            ev = agent.next() => {
                let Some(ev) = ev else { break };
                handle_agent(&mut ui, ev);
                dirty = true;
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

/// tick 后是否需要重绘：只有"存在进行中工具块"（doing 点号动画依赖
/// tick 驱动换帧）才需要；无变化时主循环跳过整帧重建（见 run 的 dirty 注释）。
/// 抽成纯函数供单测直接断言。
fn needs_anim_frame(ui: &Ui) -> bool {
    ui.items
        .iter()
        .any(|i| matches!(i, Item::Tool { result: None, .. }))
}

/// 进行中状态字 + 点号动画帧：每 5 个 tick（≈500ms）换一个点号数量，
/// 1→2→3 循环。状态字走 lang 表（i18n），点号是中性符号不翻译。
/// 纯函数 ≈ C# 的 static 方法：同样输入恒定输出，单测直接断言。
fn ellipsis_frame(lang: Lang, tick_count: u64) -> String {
    let dots = match (tick_count / 5) % 3 {
        0 => ".",
        1 => "..",
        _ => "...",
    };
    format!("{}{dots}", lang.t(Key::Doing))
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
        // splash：任意键进入对话（tick 刻意不跳过，见 tick()）
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

/// agent 事件处理：流式增量拼到对话流最后一条同类记录上；
/// 工具结果按 FIFO 回填第一个进行中块（详见 Tool 分支注释）
fn handle_agent(ui: &mut Ui, ev: Evt) {
    match ev {
        Evt::Proposal(p) => {
            // 命令提案：入待批队列，对话流里给一条提示
            let msg = ui
                .lang
                .t(Key::ProposalArrived)
                .replacen("{}", &p.name, 1)
                .replacen("{}", &p.command, 1)
                .replacen("{}", &p.mode, 1);
            ui.items.push(Item::Info(msg));
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
            // 更新第一个未完成块：agent 顺序执行工具，结果按派发顺序回来，
            // FIFO 匹配（position）才不会在并行调用（连续多个 doing 块）
            // 时把先完成的结果错填到最后一个块上（rposition 的串位 bug）；
            // 找不到（如取消前未发 ToolStart 的）则补一个完成态块。
            // name/args 也要覆盖：提前宣告时 args 是空串，完成后补上真实参数
            let idx = ui
                .items
                .iter()
                .position(|i| matches!(i, Item::Tool { result: None, .. }));
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
        ui.items.push(Item::Info(ui.lang.t(Key::ApiKeyMissing).into()));
        return;
    }
    ui.items.push(Item::User(text.clone()));
    ui.busy = true; // 进入工作态：Esc 变为可取消
    agent.send(Cmd::Chat(text));
}

/// slash 命令本地分发：/setting /new /addcmd /allowcmd /deletecmd /lang /quit
fn slash(ui: &mut Ui, agent: &mut AgentHandle, root: &Path, rest: &str) {
    // splitn(2, ...)：命令名与参数一刀两断，参数里允许含空格（如 /addcmd 的命令全文）
    let mut parts = rest.trim().splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();
    match cmd {
        "quit" => ui.quit = true,
        // /addcmd：仅人类自助注册（name 与命令全文必填）。
        // 注册后仍开审批页确认——保持"落盘前必有确认"的不变量
        "addcmd" => {
            // `-g` 前缀注册到全局层（exe 旁 do.commands.json），与 /setting -g 同构。
            // `-g` 必须是独立词：按空白切分后判断首 token，
            // `-gfoo` 不是开关，整体落入 name 校验
            let mut head = arg.splitn(2, char::is_whitespace);
            let (global, rest) = if head.next() == Some("-g") {
                (true, head.next().unwrap_or("").trim())
            } else {
                (false, arg)
            };
            let mut kv = rest.splitn(2, char::is_whitespace);
            let name = kv.next().unwrap_or("");
            let command = kv.next().unwrap_or("").trim();
            if name.is_empty() || command.is_empty() {
                ui.items.push(Item::Info(ui.lang.t(Key::UsageAddcmd).into()));
                return;
            }
            if !agent_core::commands::valid_name(name) {
                ui.items.push(Item::Info(ui.lang.t(Key::BadName).into()));
                return;
            }
            if global && agent_core::config::exe_dir().is_none() {
                ui.items.push(Item::Info(ui.lang.t(Key::NoExeDir).into()));
                return;
            }
            ui.pending.push(ApprovedCommand {
                name: name.to_string(),
                command: command.to_string(),
                description: String::new(), // desc 默认空，审批页可按 e 补
                mode: "once".into(),        // 自助注册默认一次性
                global,
            });
            // 选中刚注册的这条（队尾）
            ui.appr_sel = ui.pending.len() - 1;
            ui.appr_editing = false;
            ui.mode = Mode::Approve;
        }
        // /allowcmd：仅打开审批页处理 AI 待批提案
        "allowcmd" => {
            if ui.pending.is_empty() {
                ui.items.push(Item::Info(ui.lang.t(Key::NoPending).into()));
            } else {
                ui.appr_sel = 0;
                ui.appr_editing = false;
                ui.mode = Mode::Approve;
            }
        }
        // /deletecmd：带名字直接删（先查工作区层再查全局层）；不带则进入删除页
        "deletecmd" => {
            if arg.is_empty() {
                let cmds = agent_core::commands::merged(
                    root,
                    agent_core::config::exe_dir().as_deref(),
                );
                if cmds.is_empty() {
                    ui.items.push(Item::Info(ui.lang.t(Key::NoApproved).into()));
                } else {
                    ui.del_list = cmds;
                    ui.del_sel = 0;
                    ui.mode = Mode::Delete;
                }
            } else {
                // 先查工作区层
                let mut ws_cmds = agent_core::commands::load(root);
                let before = ws_cmds.len();
                ws_cmds.retain(|c| c.name != arg);
                if ws_cmds.len() != before {
                    match agent_core::commands::save(root, &ws_cmds) {
                        Ok(()) => {
                            // 两层同名：删工作区层，提示全局层还有一条
                            let g_has = agent_core::config::exe_dir()
                                .map(|d| agent_core::commands::load_global(&d))
                                .unwrap_or_default()
                                .iter()
                                .any(|c| c.name == arg);
                            let extra =
                                if g_has { ui.lang.t(Key::AlsoInGlobal) } else { "" };
                            ui.items.push(Item::Info(format!(
                                "{}{extra}",
                                ui.lang.t(Key::Revoked).replace("{}", arg)
                            )));
                        }
                        Err(e) => ui.items.push(Item::Info(e.to_string())),
                    }
                    return;
                }
                // 再查全局层
                let Some(dir) = agent_core::config::exe_dir() else {
                    ui.items.push(Item::Info(ui.lang.t(Key::CmdNotFound).replace("{}", arg)));
                    return;
                };
                let mut g_cmds = agent_core::commands::load_global(&dir);
                let before = g_cmds.len();
                g_cmds.retain(|c| c.name != arg);
                if g_cmds.len() == before {
                    ui.items.push(Item::Info(ui.lang.t(Key::CmdNotFound).replace("{}", arg)));
                } else {
                    match agent_core::commands::save_global(&dir, &g_cmds) {
                        Ok(()) => ui
                            .items
                            .push(Item::Info(ui.lang.t(Key::RevokedGlobal).replace("{}", arg))),
                        Err(e) => ui.items.push(Item::Info(e.to_string())),
                    }
                }
            }
        }
        "new" => {
            // /new 即压缩：只清历史。不做 HANDOFF.md 注入——system prompt
            // 已要求 AI 新对话开始时自行 read 续接（它在工作区里、有工具可达），
            // 注入只是替它读一遍，白费 token
            agent.send(Cmd::Reset);
            ui.items.clear();
            ui.tokens = 0;
            ui.items.push(Item::Info(ui.lang.t(Key::NewChatDone).into()));
        }
        // 裸 /setting（无参数）→ 进入独立设置页
        "setting" if arg.is_empty() => enter_settings(ui, root),
        "setting" => {
            // `-g` 前缀写全局便携层（exe 旁 do.config.json），否则写工作区层。
            // `-g` 必须是独立词（同 /addcmd）：`-gfoo` 不当开关
            let mut head = arg.splitn(2, char::is_whitespace);
            let (global, rest) = if head.next() == Some("-g") {
                (true, head.next().unwrap_or("").trim())
            } else {
                (false, arg)
            };
            let mut kv = rest.splitn(2, char::is_whitespace);
            let field = kv.next().unwrap_or("");
            let value = kv.next().unwrap_or("").trim();
            if field.is_empty() || value.is_empty() {
                ui.items.push(Item::Info(ui.lang.t(Key::UsageSetting).into()));
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
                            .map(|()| ui_lang_fmt(ui, true, field))
                    }
                    None => Err(ui.lang.t(Key::NoExeDir).to_string()),
                }
            } else {
                let mut cfg = Config::load_workspace(root);
                cfg.set(field, value)
                    .and_then(|()| cfg.save(root).map_err(|e| e.to_string()))
                    .map(|()| ui_lang_fmt(ui, false, field))
            };
            match result {
                Ok(msg) => {
                    // 状态栏显示的是**生效值**：保存后从合并配置重算，
                    // 不能直接用刚写入的值——写全局层时可能被工作区层
                    // 同名值覆盖；清空全局 key 而工作区层有 key 时，
                    // has_key 必须仍为 true，否则会错误拦截提交
                    let merged =
                        Config::load_merged(root, agent_core::config::exe_dir().as_deref());
                    ui.model = merged.model;
                    ui.has_key = !merged.key.is_empty();
                    ui.items.push(Item::Info(msg));
                }
                Err(e) => ui.items.push(Item::Info(e)),
            }
        }
        // /lang：显式切换或裸命令轮换；语言是个人偏好，持久化到全局层
        "lang" => {
            let next = match arg {
                "" => match ui.lang {
                    // 裸 /lang：两种语言间轮换
                    Lang::Zh => Lang::En,
                    Lang::En => Lang::Zh,
                },
                "zh" => Lang::Zh,
                "en" => Lang::En,
                _ => {
                    ui.items.push(Item::Info(ui.lang.t(Key::UsageLang).into()));
                    return;
                }
            };
            ui.lang = next;
            let value = match next {
                Lang::Zh => "zh",
                Lang::En => "en",
            };
            // 落盘失败不阻断切换（session 内仍生效），但提示一声
            if let Err(e) = persist_lang(ui.lang, value) {
                ui.items.push(Item::Info(e));
            }
            // 反馈用**新**语言
            let msg = ui.lang.t(Key::LangSet).replace("{}", ui.lang.t(Key::LangName));
            ui.items.push(Item::Info(msg));
        }
        // 旧名 /addc /deletec /allowc 不做别名，直接落入未知命令提示——
        // 提示里列出新名，即一行迁移指引
        _ => ui.items.push(Item::Info(ui.lang.t(Key::UnknownCmd).into())),
    }
}

/// "已更新 {field}" 反馈（按当前界面语言）
fn ui_lang_fmt(ui: &Ui, global: bool, field: &str) -> String {
    let key = if global { Key::UpdatedGlobalField } else { Key::UpdatedField };
    ui.lang.t(key).replace("{}", field)
}

/// /lang 持久化：语言是个人偏好，写全局层（exe 旁 do.config.json）。
/// 拆出 exe 目录参数是为了可测（测试喂临时目录）
fn persist_lang(lang: Lang, value: &str) -> Result<(), String> {
    let dir = agent_core::config::exe_dir()
        .ok_or_else(|| lang.t(Key::NoExeDir).to_string())?;
    persist_lang_to(&dir, value)
}

/// 实际写入：读全局层 → 改 lang → 写回。
/// 直接写字段而非 Config::set——/lang 是 lang 的唯一写入入口，set 不管它
fn persist_lang_to(dir: &Path, value: &str) -> Result<(), String> {
    let mut cfg = Config::load_global(dir);
    cfg.lang = value.to_string();
    cfg.save_global(dir).map_err(|e| e.to_string())
}

/// slash 命令候选表：命令名 + 用法文案 key（渲染时按当前语言取）。
/// 命令名是英文标识符，不翻译
const SLASH_COMMANDS: &[(&str, Key)] = &[
    ("/setting", Key::UsageCmdSetting),
    ("/new", Key::UsageCmdNew),
    ("/addcmd", Key::UsageCmdAddcmd),
    ("/allowcmd", Key::UsageCmdAllowcmd),
    ("/deletecmd", Key::UsageCmdDeletecmd),
    ("/lang", Key::UsageCmdLang),
    ("/quit", Key::UsageCmdQuit),
];

/// 按当前输入的前缀过滤候选（如 `/s` 只剩 /setting）。
/// 返回命中的用法提示文本；输入不以 `/` 开头或无命中时为空。
fn slash_hint(input: &str, lang: Lang) -> String {
    if !input.starts_with('/') {
        return String::new();
    }
    SLASH_COMMANDS
        .iter()
        // filter + map 链 ≈ C# LINQ 的 Where + Select。
        // 正向：输入是命令名前缀（`/s` 命中 /setting）；
        // 反向：输入已选定命令，但要求精确命中或命中后紧跟空白——
        // `/newx` 这类非法输入不该蹭到 /new 的用法提示
        .filter(|(name, _)| {
            name.starts_with(input)
                || input
                    .strip_prefix(name)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        })
        .map(|(_, usage)| lang.t(*usage))
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
/// addcmd/runcmd 取 name；JSON 解析失败或主参数缺失时兜底 `name()`。
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
    let t = |k: Key| ui.lang.t(k);
    if ui.mode == Mode::Chat {
        let slash = slash_hint(&ui.input, ui.lang);
        if !slash.is_empty() {
            return slash; // slash 输入时，候选提示保持优先
        }
    }
    match ui.mode {
        Mode::Splash => t(Key::SplashContinue).to_string(),
        Mode::Settings => if ui.set_editing.is_some() {
            t(Key::HintSettingsEdit).to_string()
        } else {
            t(Key::HintSettings).to_string()
        },
        Mode::Approve => if ui.appr_editing {
            t(Key::HintSettingsEdit).to_string()
        } else {
            t(Key::HintApprove).to_string()
        },
        Mode::Delete => t(Key::HintDelete).to_string(),
        Mode::Chat => if ui.busy {
            t(Key::HintChatBusy).to_string()
        } else {
            // agent 不忙时省略 Esc 项（此时 Esc 不响应）
            t(Key::HintChatIdle).to_string()
        },
    }
}

/// 页面类（Settings/Approve/Delete）的渲染起点：默认从尾部截取（详情区在尾部，
/// 一屏内时与旧行为一致），但选中行移出可视窗时跟随——
/// 在上沿之上则把起点提到选中行，在下沿之下则下压到刚好露出。
/// 瘦身版滚动：不维护滚动状态，只保证"选中的永远可见"
fn page_view_start(total: usize, height: usize, sel: Option<usize>) -> usize {
    let mut start = total.saturating_sub(height);
    if let Some(row) = sel {
        if row < start {
            start = row;
        } else if row >= start + height {
            start = row + 1 - height;
        }
    }
    start
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
        // 对话流正常滚动；页面类（Settings/Approve/Delete）渲染起点跟随
        // 选中项，保证选中行始终可见（超屏时不再钉死尾部盲操作）
        let start = if ui.mode == Mode::Chat {
            let bottom_skip = ui.scroll.min(total.saturating_sub(height));
            total.saturating_sub(height + bottom_skip)
        } else {
            // 页面结构约定：前 2 行是空行 + 标题，列表第 i 项落在逻辑行 2 + i
            let sel = match ui.mode {
                Mode::Approve if !ui.pending.is_empty() => Some(2 + ui.appr_sel),
                Mode::Delete if !ui.del_list.is_empty() => Some(2 + ui.del_sel),
                _ => None,
            };
            page_view_start(total, height, sel)
        };
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
        let model = if ui.model.is_empty() {
            ui.lang.t(Key::ModelUnset)
        } else {
            &ui.model
        };
        let status = format!("~{} tok | {} | {}", ui.tokens, model, ui.workspace);
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
                    let line = ui.lang.t(Key::ThinkingOpen).replace("{}", text);
                    push_styled(&mut out, &line, w, style);
                } else {
                    let line = ui
                        .lang
                        .t(Key::ThinkingFolded)
                        .replace("{}", &text.chars().count().to_string());
                    push_styled(&mut out, &line, w, style);
                }
            }
            Item::Tool { name, args, result } => {
                // 折叠态：函数调用样式（青色）+ 状态字（分色）：
                // doing 黄（动画帧随 tick 推进）/ done 绿 / 已取消 红
                let style = Style::default().fg(Color::Cyan);
                let (word, word_style) = match result {
                    None => (
                        ellipsis_frame(ui.lang, ui.tick_count),
                        Style::default().fg(Color::Yellow),
                    ),
                    // CANCEL_MARK 是逻辑比较用的内部常量，展示按语言取
                    Some(r) if r == CANCEL_MARK => (
                        ui.lang.t(Key::Cancelled).to_string(),
                        Style::default().fg(Color::Red),
                    ),
                    Some(_) => (
                        ui.lang.t(Key::Done).to_string(),
                        Style::default().fg(Color::Green),
                    ),
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
            Item::Error(t) => push_styled(
                &mut out,
                &ui.lang.t(Key::ErrorPrefix).replace("{}", t),
                w,
                Style::default().fg(Color::Red),
            ),
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
        lang: Lang::Zh,
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
        assert!(slash_hint("", Lang::Zh).is_empty()); // 非 / 开头无提示
        assert!(slash_hint("abc", Lang::Zh).is_empty());
        assert!(slash_hint("/x", Lang::Zh).is_empty()); // 无命中
        // `/s` 只剩 /setting
        assert_eq!(slash_hint("/s", Lang::Zh), "/setting [-g] <url|key|model> <值>");
        // `/` 显示全部候选
        let all = slash_hint("/", Lang::Zh);
        assert!(all.contains("/setting") && all.contains("/new") && all.contains("/quit"));
        // 已选定命令继续敲参数时仍显示其用法
        assert!(slash_hint("/setting model g", Lang::Zh).contains("/setting"));
        // 完整命令名命中自身
        assert_eq!(slash_hint("/new", Lang::Zh), "/new");
        // 非法输入不蹭提示：/newx 不是 /new（词边界）
        assert!(slash_hint("/newx", Lang::Zh).is_empty());
        assert!(slash_hint("/quitters", Lang::Zh).is_empty());
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
        assert_eq!(format_call("runcmd", "{\"name\":\"deploy\"}"), "runcmd(\"deploy\")");
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
        // 未知命令提示列出全部可用命令（含 /lang）
        slash(&mut ui, &mut agent, &dir, "nosuch");
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("/lang") && t.contains("/setting")));
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
        // 每 5 tick 换一帧：点号 1→2→3 循环；状态字走 lang 表（双语）
        assert_eq!(ellipsis_frame(Lang::En, 0), "doing.");
        assert_eq!(ellipsis_frame(Lang::En, 4), "doing.");
        assert_eq!(ellipsis_frame(Lang::En, 5), "doing..");
        assert_eq!(ellipsis_frame(Lang::En, 10), "doing...");
        assert_eq!(ellipsis_frame(Lang::En, 15), "doing."); // 回绕
        assert_eq!(ellipsis_frame(Lang::Zh, 0), "执行中."); // 中文界面
    }

    #[test]
    fn tick_redraw_only_when_animating() {
        // dirty 标志的 tick 侧判定：无进行中工具块 → tick 不触发重绘
        // （idle 时不再每秒全量重建 10 次）；有进行中块 → 动画要推进，重绘
        let mut ui = test_ui();
        ui.items.push(Item::Assistant("答完了".into()));
        assert!(!needs_anim_frame(&ui));
        // 已完成/已取消的工具块同样不需要动画帧
        ui.items.push(Item::Tool { name: "ls".into(), args: "{}".into(), result: Some("ok".into()) });
        ui.items.push(Item::Tool { name: "grep".into(), args: "{}".into(), result: Some(CANCEL_MARK.into()) });
        assert!(!needs_anim_frame(&ui));
        // 出现进行中块 → 需要 tick 驱动重绘
        ui.items.push(Item::Tool { name: "read".into(), args: "{}".into(), result: None });
        assert!(needs_anim_frame(&ui));
    }

    #[test]
    fn parallel_tool_results_backfill_fifo() {
        // 并行调用：模型一次发多个 tool_calls，ToolBegin 连续建两个 doing 块。
        // 第一个完成的结果必须落回第一个块（FIFO），不能串到最后一块
        let mut ui = test_ui();
        handle_agent(&mut ui, Evt::ToolStart { name: "read".into(), args: String::new() });
        handle_agent(&mut ui, Evt::ToolStart { name: "ls".into(), args: String::new() });
        handle_agent(&mut ui, Evt::Tool {
            name: "read".into(),
            args: "{\"path\":\"a.rs\"}".into(),
            result: "r1".into(),
        });
        match &ui.items[0] {
            Item::Tool { name, result, .. } => {
                assert_eq!(name, "read");
                assert_eq!(result.as_deref(), Some("r1"));
            }
            _ => panic!("应为 Tool 块"),
        }
        // 第二个块仍是进行中，且名字没被覆盖
        assert!(
            matches!(&ui.items[1], Item::Tool { name, result: None, .. } if name == "ls")
        );
        // 第二个结果回填第二个块
        handle_agent(&mut ui, Evt::Tool { name: "ls".into(), args: "{}".into(), result: "r2".into() });
        assert!(matches!(&ui.items[1], Item::Tool { result: Some(r), .. } if r == "r2"));
        assert_eq!(ui.items.len(), 2); // 不新增块
    }

    #[tokio::test(flavor = "current_thread")]
    async fn setting_save_refreshes_status_from_merged() {
        // 保存后状态栏按合并生效值重算，而非直接用刚写入的值
        let dir = std::env::temp_dir().join(format!("doagent-setm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut agent = AgentHandle::start(&dir).unwrap();
        let mut ui = test_ui();
        // 写工作区层 model：工作区层优先级最高，生效值必为刚写入的值
        slash(&mut ui, &mut agent, &dir, "setting model ws-model");
        assert_eq!(ui.model, "ws-model");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn page_view_start_follows_selection() {
        // 一屏内：起点 0，全部可见
        assert_eq!(page_view_start(5, 10, Some(2)), 0);
        // 超屏、选中在尾部视窗内：保持尾部（详情区在尾部可见）
        assert_eq!(page_view_start(20, 10, Some(15)), 10);
        assert_eq!(page_view_start(20, 10, None), 10);
        // 选中在上沿之上：起点提到选中行（向上跟随）
        assert_eq!(page_view_start(20, 10, Some(3)), 3);
        assert_eq!(page_view_start(20, 10, Some(0)), 0);
        // 选中在下沿之下：下压到刚好露出（防御分支，正常选中不会越界）
        assert_eq!(page_view_start(20, 10, Some(25)), 16);
    }

    #[test]
    fn tool_status_word_colors() {
        // 三态分色：执行中 黄 / 完成 绿 / 已取消 红；工具名保持青色
        // （夹具语言为 Zh，状态字断言中文案）
        let mut ui = test_ui();
        ui.items.push(Item::Tool { name: "read".into(), args: "{\"path\":\"a.rs\"}".into(), result: None });
        ui.items.push(Item::Tool { name: "ls".into(), args: "{}".into(), result: Some("x".into()) });
        ui.items.push(Item::Tool { name: "grep".into(), args: "{}".into(), result: Some(CANCEL_MARK.into()) });
        let lines = build_lines(&ui, 80);
        // 每条工具块一行两 span：[青色函数调用, 分色状态字]
        let status = |i: usize| (lines[i].spans[1].content.to_string(), lines[i].spans[1].style.fg);
        assert_eq!(status(0), (" 执行中.".to_string(), Some(Color::Yellow)));
        assert_eq!(status(1), (" 完成".to_string(), Some(Color::Green)));
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
                global: false,
            }),
        );
        assert_eq!(ui.pending.len(), 1);
        assert_eq!(ui.pending[0].name, "dev");
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("/allowcmd")));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lang_slash_rotate_set_and_persist() {
        let dir = std::env::temp_dir().join(format!("doagent-lang-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut agent = AgentHandle::start(&dir).unwrap();
        let mut ui = test_ui(); // 夹具默认 Zh
        // slash 路径会写真实 exe 旁的 do.config.json（persist_lang 走 exe_dir）。
        // 先备份原文件，结束还原：不能毁掉用户真实配置——
        // 旧版直接整文件删除，跑个测试就把全局配置抹了
        let backup = agent_core::config::exe_dir().and_then(|d| {
            let f = d.join("do.config.json");
            std::fs::read(&f).ok().map(|bytes| (f, bytes))
        });
        // 显式设置
        slash(&mut ui, &mut agent, &dir, "lang en");
        assert_eq!(ui.lang, Lang::En);
        // 反馈用新语言
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("English")));
        // 裸 /lang 轮换
        slash(&mut ui, &mut agent, &dir, "lang");
        assert_eq!(ui.lang, Lang::Zh);
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("中文")));
        // 未知参数给用法提示
        slash(&mut ui, &mut agent, &dir, "lang fr");
        assert_eq!(ui.lang, Lang::Zh); // 不变
        assert!(matches!(ui.items.last(), Some(Item::Info(t)) if t.contains("/lang [zh|en]")));
        // 还原 exe 旁配置：原本有就写回原内容，原本没有才删掉测试产物
        if let Some(d) = agent_core::config::exe_dir() {
            let f = d.join("do.config.json");
            match &backup {
                Some((_, bytes)) => {
                    let _ = std::fs::write(&f, bytes);
                }
                None => {
                    let _ = std::fs::remove_file(&f);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_lang_writes_global_layer() {
        // 持久化目标层：写进给定目录的 do.config.json（全局层）
        let dir = std::env::temp_dir().join(format!("doagent-lang2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        persist_lang_to(&dir, "zh").unwrap();
        let cfg = Config::load_global(&dir);
        assert_eq!(cfg.lang, "zh");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lang_switch_changes_ui_text() {
        // 切换 lang 后 hint 与 slash 候选提示语言变化
        let mut ui = test_ui(); // 夹具默认 Zh
        assert!(hint_text(&ui).contains("展开/折叠"));
        ui.lang = Lang::En;
        assert!(hint_text(&ui).contains("expand/collapse"));
        assert_eq!(slash_hint("/s", Lang::En), "/setting [-g] <url|key|model> <value>");
        assert!(slash_hint("/s", Lang::Zh).contains("<值>"));
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
