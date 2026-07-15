//! Central visual theme for the omni TUI.
//!
//! Every color and reusable style lives here so the whole interface stays
//! coherent and can be re-skinned by editing a single file.

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders};

// ─── Palette ────────────────────────────────────────────────────────────────
// Monochrome palette — pure black & white with carefully stepped grays.
// High contrast, zero hue: a premium engineering-tool aesthetic.

pub const BG: Color = Color::Rgb(0x0a, 0x0a, 0x0a);
pub const SURFACE: Color = Color::Rgb(0x1a, 0x1a, 0x1a);
pub const BORDER: Color = Color::Rgb(0x30, 0x30, 0x30);
pub const BORDER_FOCUS: Color = Color::Rgb(0x9e, 0x9e, 0x9e);

pub const TEXT: Color = Color::Rgb(0xea, 0xea, 0xea);
pub const TEXT_MUTED: Color = Color::Rgb(0xa0, 0xa0, 0xa0);
pub const TEXT_DIM: Color = Color::Rgb(0x5e, 0x5e, 0x5e);

pub const ACCENT: Color = Color::Rgb(0xff, 0xff, 0xff); // pure white
pub const ACCENT_ALT: Color = Color::Rgb(0xd4, 0xd4, 0xd4); // light gray
pub const SUCCESS: Color = Color::Rgb(0xc4, 0xc4, 0xc4); // light gray
pub const WARNING: Color = Color::Rgb(0xe8, 0xe8, 0xe8); // bright gray
pub const ERROR: Color = Color::Rgb(0xff, 0xff, 0xff); // white — paired with ✖/bold for emphasis
pub const INFO: Color = Color::Rgb(0xb0, 0xb0, 0xb0); // mid gray

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

/// Keyboard keys rendered inside overlays (e.g. the F1 help).
pub fn kbd() -> Style {
    Style::default().fg(INFO).add_modifier(Modifier::BOLD)
}

/// Dim borders for markdown tables and horizontal rules.
pub fn table_border() -> Style {
    Style::default().fg(BORDER)
}
