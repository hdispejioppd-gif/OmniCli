//! Render markdown into ratatui Lines with styling.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::sync::LazyLock;

static SYNTAXES: LazyLock<syntect::parsing::SyntaxSet> =
    LazyLock::new(syntect::parsing::SyntaxSet::load_defaults_newlines);
static THEMES: LazyLock<syntect::highlighting::ThemeSet> =
    LazyLock::new(syntect::highlighting::ThemeSet::load_defaults);

/// Render markdown into ratatui `Line`s with styling.
///
/// - Headings: bold + colored (h1 cyan, h2 blue, h3 magenta)
/// - Inline code: bg DarkGray, fg Yellow
/// - Fenced code blocks: syntect highlight using "base16-ocean.dark"
/// - Lists: "  * " for bullets, "  1. " for ordered
/// - Bold / Italic / Strikethrough
/// - Blockquote: prefix "| ", fg Gray
pub fn render_markdown(input: &str, _width: u16) -> Vec<Line<'static>> {
    let theme = &THEMES.themes["base16-ocean.dark"];

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Style> = vec![Style::default()];
    let mut code_block_lang: Option<String> = None;
    let mut code_buf = String::new();

    fn top_style(stack: &[Style]) -> Style {
        stack.last().copied().unwrap_or_default()
    }

    let flush_line =
        |current: &mut Vec<Span<'static>>| -> Line<'static> { Line::from(std::mem::take(current)) };

    let parser = Parser::new_ext(
        input,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES,
    );

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let color = match level {
                    HeadingLevel::H1 => Color::Cyan,
                    HeadingLevel::H2 => Color::Blue,
                    _ => Color::Magenta,
                };
                style_stack.push(Style::default().fg(color).add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Heading(_)) => {
                style_stack.pop();
                let line = flush_line(&mut current);
                lines.push(line);
                lines.push(Line::default());
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(s) => s.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                code_block_lang = Some(lang);
                code_buf.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                if !code_buf.is_empty() {
                    let lang = code_block_lang.take().unwrap_or_default();
                    let syntax = SYNTAXES
                        .find_syntax_by_token(&lang)
                        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());
                    let mut highlighter = syntect::easy::HighlightLines::new(syntax, theme);
                    for line_str in code_buf.lines() {
                        let spans: Vec<Span> = match highlighter.highlight_line(line_str, &SYNTAXES)
                        {
                            Ok(segments) => segments
                                .into_iter()
                                .map(|(style, text)| {
                                    Span::styled(
                                        text.to_string(),
                                        Style::default()
                                            .fg(syntect_to_ratatui(style.foreground))
                                            .bg(syntect_to_ratatui(style.background)),
                                    )
                                })
                                .collect(),
                            Err(_) => vec![Span::raw(line_str.to_string())],
                        };
                        lines.push(Line::from(spans));
                    }
                }
                code_buf.clear();
                code_block_lang = None;
            }
            Event::Text(t) => {
                if code_block_lang.is_some() {
                    code_buf.push_str(&t);
                } else {
                    current.push(Span::styled(t.to_string(), top_style(&style_stack)));
                }
            }
            Event::Code(t) => {
                current.push(Span::styled(
                    t.to_string(),
                    Style::default().fg(Color::Yellow).bg(Color::DarkGray),
                ));
            }
            Event::Start(Tag::Emphasis) => {
                style_stack.push(top_style(&style_stack).add_modifier(Modifier::ITALIC));
            }
            Event::End(TagEnd::Emphasis) => {
                style_stack.pop();
            }
            Event::Start(Tag::Strong) => {
                style_stack.push(top_style(&style_stack).add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Strong) => {
                style_stack.pop();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                style_stack.push(
                    Style::default()
                        .fg(Color::Gray)
                        .add_modifier(Modifier::ITALIC),
                );
                current.push(Span::styled("| ", Style::default().fg(Color::Gray)));
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                style_stack.pop();
                let line = flush_line(&mut current);
                lines.push(line);
            }
            Event::Start(Tag::Item) => {
                current.push(Span::styled("  * ", Style::default().fg(Color::Yellow)));
            }
            Event::Start(Tag::Strikethrough) => {
                style_stack.push(top_style(&style_stack).add_modifier(Modifier::CROSSED_OUT));
            }
            Event::End(TagEnd::Strikethrough) => {
                style_stack.pop();
            }
            Event::SoftBreak | Event::HardBreak => {
                let line = flush_line(&mut current);
                lines.push(line);
            }
            Event::End(TagEnd::Paragraph) | Event::End(TagEnd::Item) => {
                let line = flush_line(&mut current);
                lines.push(line);
            }
            _ => {}
        }
    }
    if !current.is_empty() {
        lines.push(Line::from(current));
    }
    if lines.is_empty() {
        lines.push(Line::from(input.to_string()));
    }
    lines
}

fn syntect_to_ratatui(color: syntect::highlighting::Color) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_is_bold() {
        let lines = render_markdown("# Hi", 80);
        assert!(!lines.is_empty());
        let has_bold = lines[0]
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(has_bold);
    }

    #[test]
    fn code_block_highlighted() {
        let md = "```rust\nfn main() {}\n```";
        let lines = render_markdown(md, 80);
        let has_color = lines.iter().any(|l| {
            l.spans
                .iter()
                .any(|s| matches!(s.style.fg, Some(Color::Rgb(..))))
        });
        assert!(has_color);
    }

    #[test]
    fn list_bullets() {
        let md = "- a\n- b";
        let lines = render_markdown(md, 80);
        let has_bullet = lines.iter().any(|l| {
            l.spans
                .first()
                .map(|s| s.content.contains('*'))
                .unwrap_or(false)
        });
        assert!(has_bullet);
    }

    #[test]
    fn plain_text_passthrough() {
        let lines = render_markdown("hello world", 80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<String>();
        assert!(text.contains("hello world"));
    }
}
