//! 页面渲染：splash / 设置页 / 审批页 / 删除页
//!
//! # 模块导读
//! 纯函数渲染：输入 &Ui，输出 ratatui 行序列，不改任何状态。
//! 逻辑（按键/落盘）在 forms 子模块，状态定义在 tui 模块根——
//! Rust 里子模块可以直接看到父模块的私有项，所以不需要
//! 把 Ui 的字段逐个 pub 出去（≈ C# 的 internal 可见性）。

use super::{wrap_text, Ui, SETTINGS_FIELDS};
use crate::lang::Key;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

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
// logo 配色：实心方块 █ = #ff006e（品红），点状方块 ░ = #ccff00（荧光绿）
const SOLID: Color = Color::Rgb(0xcc, 0xff, 0x00);
const DOTTED: Color = Color::Rgb(0xff, 0x00, 0x6e);

/// 把一行 logo 文本变成逐段着色的 Line。
/// ratatui 的 Line = Span 序列 ≈ C# 里 RichTextBox 的一串 Run：
/// 每段文字各带样式。连续同色字符合并成一个 Span，少分配。
fn logo_line(l: &str) -> Line<'static> {
    let color_of = |c: char| match c {
        '█' => Some(SOLID),
        '░' => Some(DOTTED),
        _ => None, // 空格等：默认色
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cur = String::new();
    let mut cur_color: Option<Color> = None;
    for c in l.chars() {
        if color_of(c) != cur_color {
            if !cur.is_empty() {
                spans.push(match cur_color {
                    Some(col) => Span::styled(std::mem::take(&mut cur), Style::default().fg(col)),
                    None => Span::raw(std::mem::take(&mut cur)),
                });
            }
            cur_color = color_of(c);
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        spans.push(match cur_color {
            Some(col) => Span::styled(cur, Style::default().fg(col)),
            None => Span::raw(cur),
        });
    }
    Line::from(spans)
}

pub(super) fn draw_splash(frame: &mut Frame, area: Rect, ui: &Ui) {
    let mut lines: Vec<Line> = LOGO.lines().map(logo_line).collect();
    lines.push(Line::from(""));
    lines.push(Line::from(format!("DoAgent v{}", env!("CARGO_PKG_VERSION"))));
    lines.push(Line::from(format!(
        "{}: {}",
        ui.lang.t(Key::WorkspaceLabel),
        ui.workspace
    )));
    lines.push(Line::from(if ui.has_key {
        ui.lang.t(Key::ApiKeySet).to_string()
    } else {
        ui.lang.t(Key::ApiKeyMissing).to_string()
    }));
    let model = if ui.model.is_empty() {
        ui.lang.t(Key::ModelUnset)
    } else {
        &ui.model
    };
    lines.push(Line::from(format!("model: {model}")));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        ui.lang.t(Key::SplashContinue),
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
pub(super) fn settings_lines(ui: &Ui) -> Vec<Line<'static>> {
    let section = Style::default().fg(Color::DarkGray);
    let mut out = vec![
        Line::from(""),
        // 值是合并后的生效值；括号标注来源层；分区表示"编辑写往哪一层"
        Line::from(Span::styled(ui.lang.t(Key::SettingsHeaderGlobal), section)),
    ];
    for (i, (field, _)) in SETTINGS_FIELDS.iter().enumerate() {
        if *field == "start" {
            out.push(Line::from(""));
            out.push(Line::from(Span::styled(ui.lang.t(Key::SettingsHeaderWs), section)));
        }
        // key 显示掩码；空值显示占位
        let shown = if *field == "key" {
            mask_key(&ui.set_values[i])
        } else {
            ui.set_values[i].clone()
        };
        let shown = if shown.is_empty() { ui.lang.t(Key::Unset).to_string() } else { shown };
        // 来源层标注（加载时已按语言写入 set_sources）
        let src = ui.set_sources.get(i).copied().unwrap_or("");
        let suffix = if src.is_empty() { String::new() } else { format!("（{src}）") };
        let selected = i == ui.set_sel;
        let style = if selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        out.push(Line::from(Span::styled(
            format!("{} {field:<6} = {shown}{suffix}", if selected { ">" } else { " " }),
            style,
        )));
    }
    out
}

/// 审批页列表视图：上部是待批提案列表（选中行青色 `>` 高亮，跟随设置页
/// 的视觉语言），下部是选中条详情（command 黄色只读折行全文 + mode + 描述）。
/// desc 编辑态时详情行变青提示正在编辑。
pub(super) fn approve_lines(ui: &Ui, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let section = Style::default().fg(Color::DarkGray);
    if ui.pending.is_empty() {
        return out;
    }
    out.push(Line::from(""));
    out.push(Line::from(Span::styled(
        ui.lang
            .t(Key::ApproveTitle)
            .replace("{}", &ui.pending.len().to_string()),
        section,
    )));
    // 提案列表：name + 描述摘要
    for (i, p) in ui.pending.iter().enumerate() {
        let selected = i == ui.appr_sel;
        let style = if selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        let desc = if p.description.is_empty() { ui.lang.t(Key::NoDesc) } else { &p.description };
        out.push(Line::from(Span::styled(
            format!("{} {:<12} {desc}", if selected { ">" } else { " " }, p.name),
            style,
        )));
    }
    // 选中条详情
    if let Some(p) = ui.pending.get(ui.appr_sel) {
        out.push(Line::from(""));
        out.push(Line::from(Span::styled(ui.lang.t(Key::CmdLabel).to_string(), section)));
        // command 可能很长，按宽度折行完整展示（只读，不可修改——
        // 审批的字符串 = 永远执行的全部内容）
        for l in wrap_text(&p.command, width.max(8)) {
            out.push(Line::from(Span::styled(l, Style::default().fg(Color::Yellow))));
        }
        // 目标层：AI 提案恒为工作区层；人类 /addcmd -g 注册的显示全局层
        let target = if p.global { ui.lang.t(Key::TargetGlobal) } else { "" };
        out.push(Line::from(Span::styled(format!("mode: {}{target}", p.mode), section)));
        let desc = if p.description.is_empty() { ui.lang.t(Key::NoDesc) } else { &p.description };
        let desc_style = if ui.appr_editing {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        out.push(Line::from(Span::styled(
            ui.lang.t(Key::DescLabel).replace("{}", desc),
            desc_style,
        )));
    }
    out
}

/// 删除页：已批准命令列表，选中行青色
pub(super) fn delete_lines(ui: &Ui) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    out.push(Line::from(""));
    out.push(Line::from(Span::styled(
        ui.lang.t(Key::DeleteHeader),
        Style::default().fg(Color::DarkGray),
    )));
    for (i, (c, src)) in ui.del_list.iter().enumerate() {
        let selected = i == ui.del_sel;
        let style = if selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        // src 来自 core 的合并视图（中文标签），展示时按界面语言映射
        let src_disp = if *src == "全局" {
            ui.lang.t(Key::SrcGlobal)
        } else {
            ui.lang.t(Key::SrcWorkspace)
        };
        out.push(Line::from(Span::styled(
            format!(
                "{} {:<12} = `{}`（{}·{}）",
                if selected { ">" } else { " " },
                c.name,
                c.command,
                c.mode,
                src_disp
            ),
            style,
        )));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_masking_keeps_head3_tail4() {
        assert_eq!(mask_key("sk-abcdefghij"), "sk-****ghij");
        assert_eq!(mask_key("short"), "****");
        assert_eq!(mask_key(""), "****");
    }

    #[test]
    fn logo_colors_solid_and_dotted() {
        // 实心方块 #ff006e、点状方块 #ccff00、空格默认色
        let line = logo_line("░█ ░");
        let spans: Vec<_> = line.spans.iter().collect();
        assert_eq!(spans[0].content.as_ref(), "░");
        assert_eq!(spans[0].style.fg, Some(DOTTED));
        assert_eq!(spans[1].content.as_ref(), "█");
        assert_eq!(spans[1].style.fg, Some(SOLID));
        assert_eq!(spans[2].content.as_ref(), " ");
        assert_eq!(spans[2].style.fg, None);
        // 连续同名字符合并：尾部 ░ 与开头 ░ 不同段（中间隔着 █ 和空格）
        assert_eq!(spans[3].content.as_ref(), "░");
        assert_eq!(spans[3].style.fg, Some(DOTTED));
    }

}
