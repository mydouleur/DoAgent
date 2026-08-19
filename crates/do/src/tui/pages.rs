//! 页面渲染：splash / 设置页 / 审批页 / 删除页
//!
//! # 模块导读
//! 纯函数渲染：输入 &Ui，输出 ratatui 行序列，不改任何状态。
//! 逻辑（按键/落盘）在 forms 子模块，状态定义在 tui 模块根——
//! Rust 里子模块可以直接看到父模块的私有项，所以不需要
//! 把 Ui 的字段逐个 pub 出去（≈ C# 的 internal 可见性）。

use super::{wrap_text, Ui, SETTINGS_FIELDS};
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
pub(super) fn draw_splash(frame: &mut Frame, area: Rect, ui: &Ui) {
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
pub(super) fn settings_lines(ui: &Ui) -> Vec<Line<'static>> {
    let section = Style::default().fg(Color::DarkGray);
    let mut out = vec![
        Line::from(""),
        // 值是合并后的生效值；括号标注来源层；分区表示"编辑写往哪一层"
        Line::from(Span::styled("全局（编辑写往 exe 旁 do.config.json；值 = 生效值）", section)),
    ];
    for (i, (field, _)) in SETTINGS_FIELDS.iter().enumerate() {
        if i == 3 {
            out.push(Line::from(""));
            out.push(Line::from(Span::styled("工作区（编辑写往 .do/config.json）", section)));
        }
        // key 显示掩码；空值显示占位
        let shown = if *field == "key" {
            mask_key(&ui.set_values[i])
        } else {
            ui.set_values[i].clone()
        };
        let shown = if shown.is_empty() { "（未设置）".to_string() } else { shown };
        // 来源层标注，如（工作区）/（全局）/（默认）
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
        format!("命令提案审批（{} 条待批）", ui.pending.len()),
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
        let desc = if p.description.is_empty() { "(无)" } else { &p.description };
        out.push(Line::from(Span::styled(
            format!("{} {:<12} {desc}", if selected { ">" } else { " " }, p.name),
            style,
        )));
    }
    // 选中条详情
    if let Some(p) = ui.pending.get(ui.appr_sel) {
        out.push(Line::from(""));
        out.push(Line::from(Span::styled("命令:".to_string(), section)));
        // command 可能很长，按宽度折行完整展示（只读，不可修改——
        // 审批的字符串 = 永远执行的全部内容）
        for l in wrap_text(&p.command, width.max(8)) {
            out.push(Line::from(Span::styled(l, Style::default().fg(Color::Yellow))));
        }
        out.push(Line::from(Span::styled(format!("mode: {}", p.mode), section)));
        let desc = if p.description.is_empty() { "(无)" } else { &p.description };
        let desc_style = if ui.appr_editing {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        out.push(Line::from(Span::styled(format!("描述: {desc}"), desc_style)));
    }
    out
}

/// 删除页：已批准命令列表，选中行青色
pub(super) fn delete_lines(ui: &Ui) -> Vec<Line<'static>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_masking_keeps_head3_tail4() {
        assert_eq!(mask_key("sk-abcdefghij"), "sk-****ghij");
        assert_eq!(mask_key("short"), "****");
        assert_eq!(mask_key(""), "****");
    }

}
