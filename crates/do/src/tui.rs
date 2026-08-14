//! TUI：对话流渲染 + 输入 + slash 命令 + 状态栏
//!
//! # 模块导读
//! 单事件循环驱动一切：crossterm 的键盘事件由一个专用线程转发进
//! tokio channel，与 agent actor 的事件在 `tokio::select!` 里汇合——
//! 两类事件都非阻塞等待，单线程 runtime 完全够用。
//!
//! # 交互约定
//! - Enter 发送；Ctrl+C 或 /quit 退出；Ctrl+E 全部展开/折叠；
//!   PageUp/PageDown 滚动对话流。
//! - 配色：思考灰、工具青、正文默认色。折叠块只显示一行摘要，
//!   如 `read(src/main.rs)`、`思考 (+128 字)`。
//! - 底部状态栏：`~N tok | model | 工作区`（token 是 chars/4 粗估）。
//!
//! # 核心概念
//! - RAII / Drop：[`TerminalGuard`] 离开作用域时自动恢复终端——
//!   ≈ C# 的 `using` + IDisposable，但由编译器保证调用，不靠 using 块。
//! - 生命周期标注 `'static`：ratatui 的 `Line<'a>` 借用文本；
//!   我们全部用 `String`（拥有所有权），所以是 `Line<'static>`，
//!   不与缓冲区抢借用，渲染代码因此简单很多。

use agent_core::config::Config;
use agent_core::{AgentHandle, Cmd, Evt};
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

/// 对话流里的一条记录
enum Item {
    /// 用户输入（含注入的 HANDOFF.md）
    User(String),
    /// 思考过程（reasoning，流式增长）
    Reasoning(String),
    /// 一次工具调用 + 截断后的结果
    Tool { name: String, args: String, result: String },
    /// 正文回复（流式增长）
    Assistant(String),
    /// 本地提示（slash 反馈、启动信息等）
    Info(String),
    /// 错误
    Error(String),
}

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
    };
    // 启动画面：logo 逐字展示
    for line in LOGO.lines() {
        ui.items.push(Item::Info(line.to_string()));
    }
    ui.items.push(Item::Info(format!("工作区: {}", ui.workspace)));
    if exe_dir.is_none() {
        // 降级提示：全局层不可用但不影响使用，绝不 panic
        ui.items.push(Item::Info(
            "无法定位 do.exe 目录，全局配置层不可用（仅用工作区配置 + 默认值）".into(),
        ));
    }
    if !ui.has_key {
        ui.items.push(Item::Info("未设置 API key，请用 /setting key <你的key>".into()));
    }

    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend)?;

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
        }
    }
    Ok(())
}

/// 键盘事件处理（输入框、滚动、快捷键的唯一入口）
fn handle_key(ui: &mut Ui, ev: Event, agent: &mut AgentHandle, root: &Path) {
    let Event::Key(KeyEvent { code, modifiers, kind, .. }) = ev else { return };
    // crossterm 经典坑：Windows 上一次按键会同时发出 Press 和 Release
    // 两个事件（长按还有 Repeat），不过滤就会每个键处理两次。
    // ≈ C# WinForms 的 KeyDown/KeyUp 是两个事件，这里等价于只响应 KeyDown。
    // 所有键盘入口都走这一个函数，过滤一次即全覆盖。
    if kind != KeyEventKind::Press {
        return;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        match code {
            KeyCode::Char('c') => ui.quit = true,
            KeyCode::Char('e') => ui.expand_all = !ui.expand_all,
            _ => {}
        }
        return;
    }
    match code {
        KeyCode::Enter => submit(ui, agent, root),
        // Tab：slash 命令补全为第一个候选（见 slash_candidates）
        KeyCode::Tab => tab_complete(ui),
        // 输入框字符：c 是 Unicode 标量，CJK 直接进字符串
        KeyCode::Char(c) => ui.input.push(c),
        KeyCode::Backspace => {
            // pop 按 char 弹（≈ C# 里按字符删，不会因 UTF-8 多字节切碎）
            ui.input.pop();
        }
        KeyCode::PageUp => ui.scroll = ui.scroll.saturating_add(10),
        KeyCode::PageDown => ui.scroll = ui.scroll.saturating_sub(10),
        _ => {}
    }
}

/// agent 事件处理：流式增量拼到对话流最后一条同类记录上
fn handle_agent(ui: &mut Ui, ev: Evt) {
    match ev {
        Evt::Text(t) => match ui.items.last_mut() {
            Some(Item::Assistant(s)) => s.push_str(&t),
            _ => ui.items.push(Item::Assistant(t)),
        },
        Evt::Reasoning(r) => match ui.items.last_mut() {
            Some(Item::Reasoning(s)) => s.push_str(&r),
            _ => ui.items.push(Item::Reasoning(r)),
        },
        Evt::Tool { name, args, result } => {
            ui.items.push(Item::Tool { name, args, result });
        }
        Evt::Error(e) => ui.items.push(Item::Error(e)),
        Evt::Tokens(n) => ui.tokens = n,
        Evt::Done => {}
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
        ui.items.push(Item::Info("未设置 API key，请用 /setting key <你的key>".into()));
        return;
    }
    ui.items.push(Item::User(text.clone()));
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
                        agent.send(Cmd::Chat(msg));
                    }
                }
                _ => ui.items.push(Item::Info("已清空上下文（无 HANDOFF.md 可注入）".into())),
            }
        }
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
                            .map(|()| format!("已更新全局 {field}"))
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
                    // 两层都影响状态栏，所以无论写哪层都刷新
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
        _ => ui.items.push(Item::Info("未知命令（/setting /new /quit）".into())),
    }
}

/// slash 命令候选表：命令名 + 用法提示（渲染在输入框上方那一行）
/// 数组 + 切片 ≈ C# 的静态只读表；零分配、零组件，够用就好。
const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/setting", "/setting [-g] <url|key|model|start> <值>"),
    ("/new", "/new"),
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

/// 渲染一帧
fn draw(term: &mut Terminal<CrosstermBackend<io::Stdout>>, ui: &Ui) -> io::Result<()> {
    term.draw(|frame| {
        let area = frame.area();
        // slash 提示行只在输入以 `/` 开头且有候选时出现
        let hint = slash_hint(&ui.input);
        let has_hint = !hint.is_empty();
        // 纵向布局：对话流（吃满剩余）/ [slash 提示行] / 输入行 / 状态栏
        let mut constraints = vec![Constraint::Min(1)];
        if has_hint {
            constraints.push(Constraint::Length(1));
        }
        constraints.push(Constraint::Length(1)); // 输入行
        constraints.push(Constraint::Length(1)); // 状态栏
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        // 对话流：先按当前宽度折成物理行，再按滚动位置切片
        let width = chunks[0].width as usize;
        let lines = build_lines(ui, width);
        let height = chunks[0].height as usize;
        let total = lines.len();
        let bottom_skip = ui.scroll.min(total.saturating_sub(height));
        let start = total.saturating_sub(height + bottom_skip);
        let visible: Vec<Line> = lines
            .into_iter()
            .skip(start)
            .take(height)
            .collect();
        frame.render_widget(Paragraph::new(visible), chunks[0]);

        // 输入行与状态栏的位置取决于提示行是否存在
        let (input_idx, status_idx) = if has_hint { (2, 3) } else { (1, 2) };
        if has_hint {
            frame.render_widget(
                Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
                chunks[1],
            );
        }

        // 输入行
        let prompt = format!("> {}", ui.input);
        frame.render_widget(Paragraph::new(prompt), chunks[input_idx]);
        // 光标定位到输入末尾（CJK 按显示宽度算）
        let cx = 2 + unicode_width::UnicodeWidthStr::width(ui.input.as_str());
        frame.set_cursor_position((
            (cx as u16).min(chunks[input_idx].width.saturating_sub(1)),
            chunks[input_idx].y,
        ));

        // 状态栏：~N tok | model | 工作区
        let status = format!("~{} tok | {} | {}", ui.tokens, ui.model, ui.workspace);
        frame.render_widget(
            Paragraph::new(status).style(Style::default().fg(Color::DarkGray)),
            chunks[status_idx],
        );
    })?;
    Ok(())
}

/// 把对话流摊平成物理行（含折行），每行一个样式
fn build_lines(ui: &Ui, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let w = width.max(8);
    for item in &ui.items {
        match item {
            Item::User(t) => push_styled(&mut out, &format!("» {t}"), w, Style::default()),
            Item::Assistant(t) => push_styled(&mut out, t, w, Style::default()),
            Item::Reasoning(t) => {
                let style = Style::default().fg(Color::DarkGray);
                if ui.expand_all {
                    push_styled(&mut out, &format!("思考: {t}"), w, style);
                } else {
                    push_styled(&mut out, &format!("思考 (+{} 字)", t.chars().count()), w, style);
                }
            }
            Item::Tool { name, args, result } => {
                // 折叠态一行摘要，如 read(src/main.rs)
                let style = Style::default().fg(Color::Cyan);
                let head = if args.is_empty() {
                    format!("{name}()")
                } else {
                    format!("{name}({args})")
                };
                push_styled(&mut out, &head, w, style);
                if ui.expand_all {
                    push_styled(&mut out, &indent(result), w, style.add_modifier(Modifier::DIM));
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
        let mut ui = Ui {
            items: Vec::new(),
            input: "/s".to_string(),
            scroll: 0,
            expand_all: false,
            tokens: 0,
            model: String::new(),
            has_key: false,
            workspace: String::new(),
            quit: false,
        };
        tab_complete(&mut ui);
        assert_eq!(ui.input, "/setting");
        // 非 slash 输入不补全
        ui.input = "abc".to_string();
        tab_complete(&mut ui);
        assert_eq!(ui.input, "abc");
    }
}
