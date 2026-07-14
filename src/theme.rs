//! Central visual theme for the omni TUI.
//!
//! Every color and reusable style lives here so the whole interface stays
//! coherent and can be re-skinned by editing a single file.

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders};

// ─── Palette ────────────────────────────────────────────────────────────────
// Deep, modern dark palette (Tokyo Night inspired). Works on both dark
// terminals and truecolor-capable emulators.

pub const BG: Color = Color::Rgb(0x16, 0x16, 0x1e);
pub const SURFACE: Color = Color::Rgb(0x2a, 0x2f, 0x45);
pub const BORDER: Color = Color::Rgb(0x3b, 0x42, 0x61);
pub const BORDER_FOCUS: Color = Color::Rgb(0x7a, 0xa2, 0xf7);

pub const TEXT: Color = Color::Rgb(0xc0, 0xca, 0xf5);
pub const TEXT_MUTED: Color = Color::Rgb(0x9a, 0xa5, 0xce);
pub const TEXT_DIM: Color = Color::Rgb(0x56, 0x5f, 0x89);

pub const ACCENT: Color = Color::Rgb(0x7a, 0xa2, 0xf7); // blue
pub const ACCENT_ALT: Color = Color::Rgb(0xbb, 0x9a, 0xf7); // purple
pub const SUCCESS: Color = Color::Rgb(0x9e, 0xce, 0x6a); // green
pub const WARNING: Color = Color::Rgb(0xe0, 0xaf, 0x68); // amber
pub const ERROR: Color = Color::Rgb(0xf7, 0x76, 0x8e); // red
pub const INFO: Color = Color::Rgb(0x7d, 0xcf, 0xff); // cyan

// ─── Reusable styles ────────────────────────────────────────────────────────

pub fn text() -> Style {
    Style::default().fg(TEXT)
}

pub fn muted() -> Style {
    Style::default().fg(TEXT_MUTED)
}

pub fn dim() -> Style {
    Style::default().fg(TEXT_DIM)
}

pub fn error() -> Style {
    Style::default().fg(ERROR)
}

pub fn success() -> Style {
    Style::default().fg(SUCCESS)
}

pub fn warning() -> Style {
    Style::default().fg(WARNING)
}

pub fn accent() -> Style {
    Style::default().fg(ACCENT)
}

pub fn accent_bold() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// Highlighted row in pickers and dashboards.
pub fn selection() -> Style {
    Style::default()
        .fg(BG)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD)
}

/// Key-hint bar entries: the key itself.
pub fn hint_key() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// Key-hint bar entries: the label after the key.
pub fn hint_label() -> Style {
    Style::default().fg(TEXT_DIM)
}

/// Standard rounded panel with a muted border and a bold title.
pub fn panel(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .title(format!(" {title} "))
        .title_style(Style::default().fg(TEXT_MUTED).add_modifier(Modifier::BOLD))
}

/// Panel variant with an accent border, for focused or modal surfaces.
pub fn panel_accent(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER_FOCUS))
        .title(format!(" {title} "))
        .title_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
}

/// Panel variant for warnings such as permission prompts.
pub fn panel_warning(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(WARNING))
        .title(format!(" {title} "))
        .title_style(Style::default().fg(WARNING).add_modifier(Modifier::BOLD))
}
