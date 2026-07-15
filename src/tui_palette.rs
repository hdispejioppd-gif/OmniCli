//! Command palette, slash-command hints, and the F1 help overlay.
//!
//! Self-contained UI helpers for the TUI: pure logic (filtering, layout math)
//! is kept separate from rendering so it can be unit-tested without a
//! terminal.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::theme;

/// Slash commands understood by the TUI prompt, with human descriptions.
/// Keep in sync with `parse_tui_command` in `tui.rs`.
pub const COMMANDS: &[(&str, &str)] = &[
    ("/model <selector>", "switch the active model"),
    ("/models", "open the model picker"),
    ("/workflows", "open the workflow dashboard"),
    ("/workflow run <path>", "start a workflow"),
    ("/workflow resume <run-id>", "resume a paused workflow"),
    ("/supervisors", "open the supervisor dashboard"),
    ("/supervisor run <path>", "run a supervisor task file"),
    ("/help", "toggle the keyboard help overlay"),
    ("/export", "save this session transcript to markdown"),
    ("/clear", "start a fresh session (also /new)"),
    ("/yolo", "toggle full access — auto-approve every tool"),
    ("/retry", "run the last prompt again"),
];

/// Keyboard reference shown in the F1 overlay.
pub const HELP: &[(&str, &str)] = &[
    ("Enter", "send prompt / confirm"),
    ("Alt+Enter", "insert newline"),
    ("Up / Down", "browse prompt history"),
    ("Ctrl+Left / Right", "move by word"),
    ("Ctrl+Backspace", "delete previous word"),
    ("Home / End", "jump to line start / end"),
    ("Ctrl+A / Ctrl+E", "jump to line start / end"),
    ("Ctrl+U", "clear the prompt"),
    ("Ctrl+K", "delete to end of line"),
    ("PgUp / PgDn", "scroll transcript"),
    ("Mouse wheel", "scroll transcript"),
    ("Ctrl+S", "send prompt"),
    ("Ctrl+N", "new session"),
    ("Ctrl+L", "browse saved sessions"),
    ("Ctrl+P", "model picker"),
    ("Ctrl+W", "workflow dashboard"),
    ("Ctrl+T", "supervisor dashboard"),
    ("Esc", "cancel run / clear input / quit"),
    ("Ctrl+Q / Ctrl+C", "quit"),
    ("F1", "toggle this help"),
];

/// Filter the slash-command list by the current prompt prefix.
///
/// Returns an empty list unless the (left-trimmed) input starts with `/`.
pub fn filter_commands(input: &str) -> Vec<(&'static str, &'static str)> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with('/') {
        return Vec::new();
    }
    let needle = trimmed.split_whitespace().next().unwrap_or(trimmed);
    COMMANDS
        .iter()
        .copied()
        .filter(|(command, _)| command.starts_with(needle))
        .collect()
}

/// A rectangle of the given size, centered inside `area` and clamped to it.
pub fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

/// Draw the F1 keyboard-help overlay centered in `area`.
pub fn render_help(frame: &mut Frame, area: Rect) {
    let key_width = HELP.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
    let height = (HELP.len() as u16).saturating_add(3);
    let popup = centered_rect(area, 58, height);
    frame.render_widget(Clear, popup);
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(HELP.len() + 1);
    lines.push(Line::default());
    for (key, label) in HELP {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{key:<key_width$}"), theme::kbd()),
            Span::raw("   "),
            Span::styled((*label).to_string(), theme::text()),
        ]));
    }
    let paragraph = Paragraph::new(lines).block(theme::panel_accent("Keyboard — F1 closes"));
    frame.render_widget(paragraph, popup);
}

/// Draw slash-command hints in a floating panel above the prompt.
///
/// Call this only while the user is typing a `/`-prefixed prompt. Does
/// nothing when no command matches.
pub fn render_palette(frame: &mut Frame, prompt_area: Rect, input: &str) {
    let matches = filter_commands(input);
    if matches.is_empty() {
        return;
    }
    let height = (matches.len() as u16).saturating_add(2).min(9);
    let width = prompt_area.width.min(64);
    let y = prompt_area.y.saturating_sub(height);
    let popup = Rect::new(prompt_area.x, y, width, height);
    frame.render_widget(Clear, popup);
    let lines: Vec<Line<'static>> = matches
        .iter()
        .map(|(command, description)| {
            Line::from(vec![
                Span::raw(" "),
                Span::styled((*command).to_string(), theme::accent_bold()),
                Span::raw("  "),
                Span::styled((*description).to_string(), theme::dim()),
            ])
        })
        .collect();
    let paragraph = Paragraph::new(lines).block(theme::panel("Commands"));
    frame.render_widget(paragraph, popup);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_slash_input_has_no_hints() {
        assert!(filter_commands("hello").is_empty());
        assert!(filter_commands("").is_empty());
    }

    #[test]
    fn slash_prefix_filters_commands() {
        let matches = filter_commands("/mo");
        assert!(matches.len() >= 2);
        assert!(
            matches
                .iter()
                .all(|(command, _)| command.starts_with("/mo"))
        );
    }

    #[test]
    fn full_command_with_arguments_still_matches() {
        let matches = filter_commands("/workflow run examples/inspect.yml");
        assert!(
            matches
                .iter()
                .any(|(command, _)| command.starts_with("/workflow run"))
        );
    }

    #[test]
    fn bare_slash_lists_everything() {
        assert_eq!(filter_commands("/").len(), COMMANDS.len());
    }

    #[test]
    fn centered_rect_stays_inside_area() {
        let area = Rect::new(0, 0, 100, 40);
        let popup = centered_rect(area, 58, 20);
        assert!(popup.x + popup.width <= area.width);
        assert!(popup.y + popup.height <= area.height);
        let oversized = centered_rect(area, 200, 200);
        assert_eq!(oversized.width, 100);
        assert_eq!(oversized.height, 40);
    }

    #[test]
    fn help_table_is_consistent() {
        assert!(!HELP.is_empty());
        assert!(
            HELP.iter()
                .all(|(key, label)| !key.is_empty() && !label.is_empty())
        );
    }
}
