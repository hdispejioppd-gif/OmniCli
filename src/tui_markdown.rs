//! Render markdown into ratatui Lines with styling.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::sync::LazyLock;

use crate::theme;

static SYNTAXES: LazyLock<syntect::parsing::SyntaxSet> =
    LazyLock::new(syntect::parsing::SyntaxSet::load_defaults_newlines);
static THEMES: LazyLock<syntect::highlighting::ThemeSet> =
    LazyLock::new(syntect::highlighting::ThemeSet::load_defaults);

/// Render markdown into ratatui `Line`s with styling.
///
/// - Headings: bold + theme accent colors with `◆ ◇ ›` glyphs
/// - Inline code: theme surface background, amber foreground
/// - Fenced code blocks: syntect highlight using "base16-ocean.dark"
/// - Lists: " • " for bullets, " 1. " for ordered, "☐ / ☑" for task lists
/// - Tables: `│`-separated cells with a bold header row
/// - Horizontal rules: dim `───` divider
/// - Bold / Italic / Strikethrough
/// - Blockquote: prefix "▌ ", muted italic
pub fn render_markdown(input: &str, width: u16) -> Vec<Line<'static>> {
    let theme = &THEMES.themes["base16-ocean.dark"];

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Style> = vec![Style::default()];
    let mut code_block_lang: Option<String> = None;
    let mut code_buf = String::new();
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let rule_width = width.saturating_sub(2).clamp(8, 60) as usize;

    fn top_style(stack: &[Style]) -> Style {
        stack.last().copied().unwrap_or_default()
    }

    let flush_line =
        |current: &mut Vec<Span<'static>>| -> Line<'static> { Line::from(std::mem::take(current)) };

    let parser = Parser::new_ext(
        input,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS,
    );

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let (color, glyph) = match level {
                    HeadingLevel::H1 => (theme::ACCENT, "◆ "),
                    HeadingLevel::H2 => (theme::ACCENT_ALT, "◇ "),
                    _ => (theme::INFO, "› "),
                };
                style_stack.push(Style::default().fg(color).add_modifier(Modifier::BOLD));
                current.push(Span::styled(glyph, Style::default().fg(color)));
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
                    let label = if lang.is_empty() {
                        "code".to_string()
                    } else {
                        lang.clone()
                    };
                    lines.push(Line::from(vec![
                        Span::styled("  ╭─ ", theme::table_border()),
                        Span::styled(
                            label,
                            Style::default()
                                .fg(theme::ACCENT_ALT)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" ─", theme::table_border()),
                    ]));
                    let width = if code_buf.lines().count() >= 100 {
                        3
                    } else {
                        2
                    };
                    for (index, line_str) in code_buf.lines().enumerate() {
                        let spans: Vec<Span<'static>> =
                            match highlighter.highlight_line(line_str, &SYNTAXES) {
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
                        let mut row: Vec<Span<'static>> = vec![Span::styled(
                            format!("  {index:>width$} │ ", index = index + 1, width = width),
                            Style::default().fg(theme::TEXT_DIM),
                        )];
                        row.extend(spans);
                        lines.push(Line::from(row));
                    }
                    lines.push(Line::from(Span::styled("  ╰─", theme::table_border())));
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
                    Style::default().fg(theme::WARNING).bg(theme::SURFACE),
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
                        .fg(theme::TEXT_MUTED)
                        .add_modifier(Modifier::ITALIC),
                );
                current.push(Span::styled("▌ ", Style::default().fg(theme::TEXT_DIM)));
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                style_stack.pop();
                let line = flush_line(&mut current);
                lines.push(line);
            }
            Event::Start(Tag::List(start)) => {
                list_stack.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                list_stack.pop();
            }
            Event::Start(Tag::Item) => {
                let indent = "  ".repeat(list_stack.len().max(1));
                match list_stack.last_mut() {
                    Some(Some(number)) => {
                        current.push(Span::styled(
                            format!("{indent}{number}. "),
                            Style::default().fg(theme::ACCENT),
                        ));
                        *number += 1;
                    }
                    _ => {
                        current.push(Span::styled(
                            format!("{indent}• "),
                            Style::default().fg(theme::ACCENT),
                        ));
                    }
                }
            }
            Event::TaskListMarker(checked) => {
                current.push(Span::styled(
                    if checked { "☑ " } else { "☐ " },
                    Style::default().fg(if checked {
                        theme::SUCCESS
                    } else {
                        theme::TEXT_DIM
                    }),
                ));
            }
            Event::Rule => {
                lines.push(Line::from(Span::styled(
                    "─".repeat(rule_width),
                    theme::table_border(),
                )));
                lines.push(Line::default());
            }
            Event::Start(Tag::Table(_)) => {
                if !current.is_empty() {
                    let line = flush_line(&mut current);
                    lines.push(line);
                }
            }
            Event::End(TagEnd::Table) => {
                lines.push(Line::default());
            }
            Event::Start(Tag::TableHead) => {
                style_stack.push(
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                );
            }
            Event::End(TagEnd::TableHead) => {
                style_stack.pop();
                current.push(Span::styled("│", theme::table_border()));
                let line = flush_line(&mut current);
                lines.push(line);
                lines.push(Line::from(Span::styled(
                    "─".repeat(rule_width),
                    theme::table_border(),
                )));
            }
            Event::End(TagEnd::TableRow) => {
                current.push(Span::styled("│", theme::table_border()));
                let line = flush_line(&mut current);
                lines.push(line);
            }
            Event::Start(Tag::TableCell) => {
                current.push(Span::styled("│ ", theme::table_border()));
            }
            Event::End(TagEnd::TableCell) => {
                current.push(Span::raw(" "));
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
    // Monochrome UI: map syntax colors to grayscale by perceived luminance,
    // keeping token contrast without introducing hue.
    let luma =
        0.2126 * f32::from(color.r) + 0.7152 * f32::from(color.g) + 0.0722 * f32::from(color.b);
    let level = luma.round().clamp(0.0, 255.0) as u8;
    Color::Rgb(level, level, level)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<String>()
    }

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
    fn code_block_has_frame_and_line_numbers() {
        let md = "```rust\nfn main() {}\n```";
        let lines = render_markdown(md, 80);
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(text.iter().any(|l| l.contains("╭─ rust")));
        assert!(text.iter().any(|l| l.contains(" 1 │ ")));
        assert!(text.iter().any(|l| l.contains("╰─")));
    }

    #[test]
    fn list_bullets() {
        let md = "- a\n- b";
        let lines = render_markdown(md, 80);
        let has_bullet = lines.iter().any(|l| {
            l.spans
                .first()
                .map(|s| s.content.contains('•'))
                .unwrap_or(false)
        });
        assert!(has_bullet);
    }

    #[test]
    fn plain_text_passthrough() {
        let lines = render_markdown("hello world", 80);
        assert!(plain_text(&lines).contains("hello world"));
    }

    #[test]
    fn horizontal_rule_renders_divider() {
        let lines = render_markdown("above\n\n---\n\nbelow", 40);
        assert!(plain_text(&lines).contains('─'));
    }

    #[test]
    fn task_list_markers_render() {
        let md = "- [x] done\n- [ ] todo";
        let lines = render_markdown(md, 40);
        let text = plain_text(&lines);
        assert!(text.contains('☑'));
        assert!(text.contains('☐'));
    }

    #[test]
    fn table_renders_cells() {
        let md = "| a | b |\n| --- | --- |\n| 1 | 2 |";
        let lines = render_markdown(md, 40);
        let text = plain_text(&lines);
        assert!(text.contains('│'));
        assert!(text.contains('1'));
        assert!(text.contains('2'));
    }

    #[test]
    fn heading_has_glyph() {
        let lines = render_markdown("# Title", 80);
        assert!(plain_text(&lines).contains('◆'));
    }
}
