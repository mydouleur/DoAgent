//! Markdown → ratatui 样式行：AI 正文回复的渲染器
//!
//! # 模块导读
//! 把 `Item::Assistant` 的 Markdown 文本解析成带样式的 [`Line`]（span 级样式），
//! 再由 tui 的 span 感知折行摊平成物理行。渲染范围刻意收窄：
//! 标题（加粗+亮色）、代码围栏块（整行底色）、行内代码、粗体/斜体、
//! 列表缩进、引用块；表格不开扩展，按普通段落原样显示。
//! reasoning（思考）不走这里，保持纯文本灰色。
//!
//! # 核心概念
//! pulldown-cmark 是**事件流式解析器**：`Parser` 是一个 `Iterator<Item = Event>`，
//! 逐个吐出 Start/Text/End 等事件，我们自己维护样式栈——
//! ≈ C# 的 `XmlReader`（SAX 式逐事件前向读取），而不是一次性建树 DOM。
//! 好处是零中间表示、内存恒定；代价是"当前该用什么样式"要自己算。
//!
//! # 流式兼容
//! 调用方每帧对**完整文本**重新解析（文本量小，性能无感），不做增量解析。
//! 流式中间态可能是不完整 md（如半个代码围栏）：pulldown-cmark 对未闭合
//! 结构会自然降级（围栏视为延伸到 EOF），不会 panic——这正是选事件式
//! 解析器而非手写逐行判断的原因。

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// 标题：加粗 + 亮绿
const STYLE_HEADING: Style = Style::new().fg(Color::Green).add_modifier(Modifier::BOLD);
/// 代码围栏块：整行深灰底（与正文明显区分）
const STYLE_CODE_BLOCK: Style = Style::new().bg(Color::DarkGray);
/// 行内代码：黄色
const STYLE_INLINE_CODE: Style = Style::new().fg(Color::Yellow);
/// 粗体 / 斜体
const STYLE_STRONG: Style = Style::new().add_modifier(Modifier::BOLD);
const STYLE_EMPHASIS: Style = Style::new().add_modifier(Modifier::ITALIC);
/// 引用块：灰色
const STYLE_QUOTE: Style = Style::new().fg(Color::DarkGray);

/// 把 Markdown 文本渲染成逻辑行（此处还不折行，折行由调用方做）。
/// `Line<'static>`：Span 持有 String 所有权，不与输入文本抢借用。
pub fn render(text: &str) -> Vec<Line<'static>> {
    let mut r = Renderer::default();
    for event in Parser::new(text) {
        r.on_event(event);
    }
    r.finish()
}

/// 渲染器状态：样式栈 + 当前行缓冲 + 已完成的逻辑行
#[derive(Default)]
struct Renderer {
    /// 样式栈：Start 压入、End 弹出；当前样式 = 栈全部叠加。
    /// Vec 当栈用 ≈ C# 的 Stack<T>（push/pop 都在尾部）
    styles: Vec<Style>,
    /// 当前逻辑行的 span 序列
    cur: Vec<Span<'static>>,
    /// 已完成的逻辑行
    lines: Vec<Line<'static>>,
    /// 是否在代码围栏块内（块内换行是真实换行，且不再套行内样式）
    in_code_block: bool,
    /// 列表嵌套深度（决定缩进）
    list_depth: usize,
    /// 引用块嵌套深度（每行前缀 "│ " 的层数）
    quote_depth: usize,
    /// 当前行是否已写入内容（引用/列表前缀只在行首插一次）
    line_started: bool,
}

impl Renderer {
    /// 当前生效样式：栈内全部样式依序叠加。
    /// Style::patch ≈ C# 里"后者覆盖前者非默认值"的合并
    fn style(&self) -> Style {
        self.styles
            .iter()
            .fold(Style::default(), |acc, s| acc.patch(*s))
    }

    fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => self.on_start(tag),
            Event::End(tag) => self.on_end(tag),
            Event::Text(t) => self.on_text(&t),
            // 行内代码：独立事件（不等同 Text），套用行内代码样式
            Event::Code(c) => self.push_span(&c, self.style().patch(STYLE_INLINE_CODE)),
            Event::SoftBreak | Event::HardBreak => self.break_line(),
            Event::Rule => {
                self.break_line();
                self.push_span("──────────", self.style());
                self.break_line();
            }
            _ => {} // html/脚注/任务列表等忽略
        }
    }

    fn on_start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => self.break_line(),
            Tag::Heading { .. } => {
                self.break_line();
                self.styles.push(STYLE_HEADING);
            }
            Tag::CodeBlock(_) => {
                self.break_line();
                self.in_code_block = true;
                self.styles.push(STYLE_CODE_BLOCK);
            }
            Tag::BlockQuote(_) => {
                self.break_line();
                self.quote_depth += 1;
                self.styles.push(STYLE_QUOTE);
            }
            Tag::List(_) => {
                self.break_line();
                self.list_depth += 1;
            }
            Tag::Item => {
                self.break_line();
                // 列表项行首：按嵌套深度缩进 + "- " 标记
                let indent = "  ".repeat(self.list_depth.saturating_sub(1));
                self.push_span(&format!("{indent}- "), self.style());
            }
            Tag::Emphasis => self.styles.push(STYLE_EMPHASIS),
            Tag::Strong => self.styles.push(STYLE_STRONG),
            _ => {}
        }
    }

    fn on_end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.break_line(),
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                self.styles.pop();
                self.break_line();
            }
            TagEnd::BlockQuote(_) => {
                self.quote_depth -= 1;
                self.styles.pop();
                self.break_line();
            }
            TagEnd::List(_) => {
                self.list_depth -= 1;
                self.break_line();
            }
            TagEnd::Item => self.break_line(),
            TagEnd::Emphasis | TagEnd::Strong => {
                self.styles.pop();
            }
            _ => {}
        }
    }

    fn on_text(&mut self, t: &str) {
        if self.in_code_block {
            // 代码块文本内含真实换行：拆开逐行输出，整行套底色
            let style = self.style();
            for (i, part) in t.split('\n').enumerate() {
                if i > 0 {
                    self.break_line();
                }
                if !part.is_empty() {
                    self.push_span(part, style);
                }
            }
        } else {
            // 行首才补引用前缀（│ 层级标记）
            if !self.line_started && self.quote_depth > 0 {
                let prefix = "│ ".repeat(self.quote_depth);
                self.push_span(&prefix, STYLE_QUOTE);
            }
            let style = self.style();
            self.push_span(t, style);
        }
    }

    fn push_span(&mut self, text: &str, style: Style) {
        self.line_started = true;
        self.cur.push(Span::styled(text.to_string(), style));
    }

    /// 结束当前逻辑行（空行也保留，维持段落间距）
    fn break_line(&mut self) {
        // mem::take ≈ C# 里"取出引用并置 null"：所有权移出，原地留默认空 Vec
        self.lines.push(Line::from(std::mem::take(&mut self.cur)));
        self.line_started = false;
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.break_line();
        // 去掉解析器事件边界产生的首尾空行
        while self.lines.first().is_some_and(|l| l.spans.is_empty()) {
            self.lines.remove(0);
        }
        while self.lines.last().is_some_and(|l| l.spans.is_empty()) {
            self.lines.pop();
        }
        if self.lines.is_empty() {
            self.lines.push(Line::default());
        }
        self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把渲染结果摊平成 (文本, 样式) 便于断言
    fn flatten(lines: &[Line<'static>]) -> Vec<(String, Style)> {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| (s.content.to_string(), s.style)))
            .collect()
    }

    #[test]
    fn heading_is_bold_bright() {
        let flat = flatten(&render("# 标题"));
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].0, "标题");
        assert!(flat[0].1.add_modifier.contains(Modifier::BOLD));
        assert_eq!(flat[0].1.fg, Some(Color::Green));
    }

    #[test]
    fn inline_code_styled() {
        let flat = flatten(&render("用 `cargo build` 编译"));
        let code = flat.iter().find(|(t, _)| t == "cargo build").unwrap();
        assert_eq!(code.1.fg, Some(Color::Yellow));
        // 其余正文保持默认样式
        let plain = flat.iter().find(|(t, _)| t.contains("编译")).unwrap();
        assert_eq!(plain.1.fg, None);
    }

    #[test]
    fn strong_is_bold() {
        let flat = flatten(&render("这**很重要**啊"));
        let strong = flat.iter().find(|(t, _)| t == "很重要").unwrap();
        assert!(strong.1.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn code_block_has_background() {
        let flat = flatten(&render("```\nlet x = 1;\nlet y = 2;\n```"));
        let code: Vec<_> = flat.iter().filter(|(_, s)| s.bg == Some(Color::DarkGray)).collect();
        assert_eq!(code.len(), 2); // 两行代码都带底色
        assert_eq!(code[0].0, "let x = 1;");
    }

    #[test]
    fn unclosed_fence_degrades_gracefully() {
        // 流式中间态：围栏未闭合，不许 panic，已收到部分按代码块渲染
        let flat = flatten(&render("```\npartial code"));
        let code = flat.iter().find(|(t, _)| t == "partial code").unwrap();
        assert_eq!(code.1.bg, Some(Color::DarkGray));
    }

    #[test]
    fn list_items_indented() {
        let lines = render("- 甲\n- 乙");
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains("- 甲") && text.contains("- 乙"));
    }

    #[test]
    fn quote_has_marker() {
        let flat = flatten(&render("> 引用一句"));
        let marker = flat.iter().find(|(t, _)| t.contains('│')).unwrap();
        assert_eq!(marker.1.fg, Some(Color::DarkGray));
    }
}
