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

/// Logo + quick tips rendered at the top of an empty transcript.
pub fn welcome_lines(width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::default());
    if width as usize >= LOGO[0].chars().count() + 4 {
        for row in LOGO {
            lines.push(Line::from(Span::styled(
                format!("  {row}"),
                Style::default().fg(theme::ACCENT),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "  ◆ omni",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("  v", theme::dim()),
        Span::styled(env!("CARGO_PKG_VERSION"), theme::muted()),
        Span::styled("  — a provider-neutral coding agent", theme::dim()),
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
