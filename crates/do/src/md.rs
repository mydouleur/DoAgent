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
    /// 各层有序列表的当前序号（None = 无序层）；与 list_depth 平行入栈
    list_nums: Vec<Option<u64>>,
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
                // 分隔线是重结构：前后各锚一个空行
                self.break_line();
                self.ensure_blank();
                self.push_span("──────────", self.style());
                self.break_line();
                self.ensure_blank();
            }
            _ => {} // html/脚注/任务列表等忽略
        }
    }

    fn on_start(&mut self, tag: Tag) {
        match tag {
            // 段落/列表/引用：换行即分段，不制造空行（紧凑布局）
            Tag::Paragraph => self.break_line(),
            Tag::Heading { .. } => {
                // 标题是重结构：前后各一个空行当视觉锚点
                self.break_line();
                self.ensure_blank();
                self.styles.push(STYLE_HEADING);
            }
            Tag::CodeBlock(_) => {
                self.break_line();
                self.ensure_blank();
                self.in_code_block = true;
                self.styles.push(STYLE_CODE_BLOCK);
            }
            Tag::BlockQuote(_) => {
                self.break_line();
                self.quote_depth += 1;
                self.styles.push(STYLE_QUOTE);
            }
            Tag::List(start) => {
                self.break_line();
                self.list_depth += 1;
                // 有序列表记下起始序号（pulldown-cmark 给 Option<u64>，None = 无序）
                self.list_nums.push(start);
            }
            Tag::Item => {
                self.break_line();
                // 列表项行首：按嵌套深度缩进 + 标记。
                // 有序层输出当前序号并自增（`{n}. `）；无序层保持 "- "
                let indent = "  ".repeat(self.list_depth.saturating_sub(1));
                let marker = match self.list_nums.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{indent}{n}. ");
                        *n += 1;
                        m
                    }
                    _ => format!("{indent}- "),
                };
                self.push_span(&marker, self.style());
            }
            Tag::Emphasis => self.styles.push(STYLE_EMPHASIS),
            Tag::Strong => self.styles.push(STYLE_STRONG),
            _ => {}
        }
    }

    fn on_end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.break_line(),
            TagEnd::Heading(_) => {
                self.styles.pop(); // 修复：标题样式出栈（旧版漏 pop，会漏给后文）
                self.break_line();
                self.ensure_blank();
            }
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                self.styles.pop();
                self.break_line();
                self.ensure_blank();
            }
            TagEnd::BlockQuote(_) => {
                self.quote_depth -= 1;
                self.styles.pop();
                self.break_line();
            }
            TagEnd::List(_) => {
                self.list_depth -= 1;
                self.list_nums.pop();
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
            let style = self.style();
            self.push_span(t, style);
        }
    }

    fn push_span(&mut self, text: &str, style: Style) {
        // 引用前缀统一在这里补齐：不只 Text，行内代码（Code）、列表项标记等
        // 独立事件也走 push_span——若只在 on_text 补，行首是行内代码的引用行
        // 会丢 "│ " 前缀。代码块内不补（块内行首是代码原文，非引用排版）
        if !self.line_started && !self.in_code_block && self.quote_depth > 0 {
            let prefix = "│ ".repeat(self.quote_depth);
            self.cur.push(Span::styled(prefix, STYLE_QUOTE));
        }
        self.line_started = true;
        self.cur.push(Span::styled(text.to_string(), style));
    }

    /// 原语一：断行。只交出已积累的内容；cur 为空时什么都不产生——
    /// 断行本身永远不制造空行（紧凑布局的第一层）
    fn break_line(&mut self) {
        // mem::take ≈ C# 里"取出引用并置 null"：所有权移出，原地留默认空 Vec
        if !self.cur.is_empty() {
            self.lines.push(Line::from(std::mem::take(&mut self.cur)));
        }
        self.line_started = false;
    }

    /// 原语二：确保空行（重结构边界的视觉锚点）。
    /// 幂等：上一条输出已是空行、或还没有任何输出（文档开头）时跳过——
    /// 连续结构事件不会叠出多个空行（紧凑布局的第二、三层）
    fn ensure_blank(&mut self) {
        if self.lines.last().is_some_and(|l| !l.spans.is_empty()) {
            self.lines.push(Line::default());
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.break_line();
        // 去掉可能残留的首尾空行（文档开头/结尾不锚空行）。
        // 首部一次性 drain：remove(0) 循环每次都要把后续元素整体搬移
        let head = self.lines.iter().take_while(|l| l.spans.is_empty()).count();
        self.lines.drain(..head);
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

    /// 行级文本视图（空行为 ""）
    fn texts(md: &str) -> Vec<String> {
        render(md)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect()
    }

    #[test]
    fn paragraphs_have_no_blank_between() {
        // 紧凑布局：段落间零空行
        assert_eq!(texts("甲\n\n乙\n\n丙"), ["甲", "乙", "丙"]);
    }

    #[test]
    fn code_block_has_exactly_one_blank_around() {
        // 代码块前后恰一空行；文档开头侧不锚空行
        assert_eq!(texts("```\nx\n```\n尾"), ["x", "", "尾"]);
        assert_eq!(texts("头\n```\nx\n```"), ["头", "", "x"]);
        assert_eq!(texts("头\n```\nx\n```\n尾"), ["头", "", "x", "", "尾"]);
    }

    #[test]
    fn consecutive_blanks_collapse() {
        // 标题接代码块：两个重结构相邻，空行不叠加
        assert_eq!(texts("# 题\n```\nx\n```"), ["题", "", "x"]);
    }

    #[test]
    fn list_items_have_no_blank_between() {
        let t = texts("- 甲\n- 乙");
        assert_eq!(t.len(), 2);
        assert!(t[0].contains("- 甲") && t[1].contains("- 乙"));
    }

    #[test]
    fn heading_has_one_blank_before_and_after() {
        assert_eq!(texts("头\n# 题\n尾"), ["头", "", "题", "", "尾"]);
        // 文档开头的标题：前面不锚空行
        assert_eq!(texts("# 题\n尾"), ["题", "", "尾"]);
    }

    #[test]
    fn ordered_list_keeps_numbers() {
        // 有序列表按起始序号渲染并自增，不再全部退化成 "- "
        assert_eq!(texts("1. 甲\n2. 乙\n3. 丙"), ["1. 甲", "2. 乙", "3. 丙"]);
        // 非 1 起始也尊重
        assert_eq!(texts("5. 甲\n6. 乙"), ["5. 甲", "6. 乙"]);
        // 无序列表保持 "- "
        assert_eq!(texts("- 甲\n- 乙"), ["- 甲", "- 乙"]);
    }

    #[test]
    fn quote_line_starting_with_inline_code_keeps_prefix() {
        // 行首是行内代码的引用行也要补 "│ " 前缀（Code 事件同样走 push_span）
        let lines = render("> `code` 后文");
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with("│ "), "{text}");
        assert!(text.contains("code"));
        // 代码 span 仍是行内代码样式（前缀没有吞掉样式）
        let code = lines[0].spans.iter().find(|s| s.content == "code").unwrap();
        assert_eq!(code.style.fg, Some(Color::Yellow));
    }
}
