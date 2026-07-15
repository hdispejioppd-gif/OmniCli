//! Startup welcome banner shown while the transcript is still empty.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme;

const LOGO: &[&str] = &[
    " ██████╗ ███╗   ███╗███╗   ██╗██╗",
    "██╔═══██╗████╗ ████║████╗  ██║██║",
    "██║   ██║██╔████╔██║██╔██╗ ██║██║",
    "██║   ██║██║╚██╔╝██║██║╚██╗██║██║",
    "╚██████╔╝██║ ╚═╝ ██║██║ ╚████║██║",
    " ╚═════╝ ╚═╝     ╚═╝╚═╝  ╚═══╝╚═╝",
];

/// Paint a single logo row with the brand gradient, sampled left-to-right and
/// nudged over time so the empty-transcript logo softly shimmers.
fn gradient_row(row: &str, y: usize, phase: f32) -> Line<'static> {
    let total = row.chars().count().max(1) as f32;
    let spans = row
        .chars()
        .enumerate()
        .map(|(x, ch)| {
            let t = (x as f32 / total + y as f32 * 0.06 + phase).rem_euclid(1.0);
            Span::styled(
                ch.to_string(),
                Style::default()
                    .fg(theme::gradient(t))
                    .add_modifier(Modifier::BOLD),
            )
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// Logo + quick tips rendered at the top of an empty transcript.
pub fn welcome_lines(width: u16) -> Vec<Line<'static>> {
    welcome_lines_at(width, 0.0)
}

/// Animated variant: `phase` (0.0..=1.0) scrolls the gradient across the logo.
pub fn welcome_lines_at(width: u16, phase: f32) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::default());
    if width as usize >= LOGO[0].chars().count() + 4 {
        for (y, row) in LOGO.iter().enumerate() {
            let mut spans = vec![Span::raw("  ")];
            spans.extend(gradient_row(row, y, phase).spans);
            lines.push(Line::from(spans));
        }
    } else {
        lines.push(Line::from(vec![
            Span::styled("  ◆ ", Style::default().fg(theme::gradient(phase))),
            Span::styled(
                "omni",
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("  v", theme::dim()),
        Span::styled(env!("CARGO_PKG_VERSION"), theme::muted()),
        Span::styled("  — a provider-neutral agentic runtime", theme::dim()),
    ]));
    lines.push(Line::default());
    for (key, tip) in [
        ("/", "browse commands as you type"),
        ("F1", "keyboard reference"),
        ("Ctrl+P", "switch models"),
        ("Ctrl+L", "resume a saved session"),
    ] {
        lines.push(Line::from(vec![
            Span::styled(format!("  {key:>6}  "), theme::kbd()),
            Span::styled(tip, theme::muted()),
        ]));
    }
    lines.push(Line::default());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_terminal_gets_logo() {
        let lines = welcome_lines(120);
        assert!(lines.len() > LOGO.len());
    }

    #[test]
    fn narrow_terminal_gets_compact_banner() {
        let lines = welcome_lines(20);
        assert!(!lines.is_empty());
        assert!(lines.len() < 15);
    }
}
