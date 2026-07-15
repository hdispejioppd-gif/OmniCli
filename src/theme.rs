//! Central visual theme for the omni TUI.
//!
//! Every color and reusable style lives here so the whole interface stays
//! coherent and can be re-skinned by editing a single file.

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, BorderType, Borders};

// ─── Palette ────────────────────────────────────────────────────────────────
// "Aurora" — a premium truecolor theme: deep space-blue canvas with a violet →
// cyan brand gradient, warm amber highlights, and carefully tuned text steps.
// Every widget draws from these tokens so the whole surface stays coherent and
// can be re-skinned by editing this single file.

pub const BG: Color = Color::Rgb(0x0b, 0x0d, 0x17); // near-black indigo canvas
pub const SURFACE: Color = Color::Rgb(0x16, 0x1a, 0x2b); // raised panels / inline code
pub const SURFACE_ALT: Color = Color::Rgb(0x1f, 0x24, 0x3b); // deeper raised fills
pub const BORDER: Color = Color::Rgb(0x2b, 0x31, 0x4d); // resting borders
pub const BORDER_FOCUS: Color = Color::Rgb(0x8b, 0x7c, 0xff); // focused violet border

pub const TEXT: Color = Color::Rgb(0xed, 0xf0, 0xff); // primary content
pub const TEXT_MUTED: Color = Color::Rgb(0xa5, 0xad, 0xcf); // secondary content
pub const TEXT_DIM: Color = Color::Rgb(0x5b, 0x62, 0x86); // tertiary / empty states

pub const ACCENT: Color = Color::Rgb(0x8b, 0x7c, 0xff); // violet — brand / omni
pub const ACCENT_ALT: Color = Color::Rgb(0x4d, 0xd8, 0xf0); // cyan — user / secondary
pub const SUCCESS: Color = Color::Rgb(0x54, 0xe6, 0xa6); // mint green
pub const WARNING: Color = Color::Rgb(0xff, 0xc4, 0x66); // warm amber
pub const ERROR: Color = Color::Rgb(0xff, 0x6b, 0x8b); // rose red
pub const INFO: Color = Color::Rgb(0x6f, 0xb4, 0xff); // sky blue

// ─── Brand gradient ─────────────────────────────────────────────────────────
// The signature violet → magenta → cyan ramp used by the logo, the streaming
// spinner, and other "alive" accents. `gradient(t)` samples the ramp for t in
// 0.0..=1.0; `gradient_stops()` exposes the raw anchors.

const GRADIENT_STOPS: [(u8, u8, u8); 4] = [
    (0x8b, 0x7c, 0xff), // violet
    (0xb4, 0x6b, 0xff), // purple
    (0xff, 0x6b, 0xc4), // magenta
    (0x4d, 0xd8, 0xf0), // cyan
];

pub fn gradient_stops() -> [(u8, u8, u8); 4] {
    GRADIENT_STOPS
}

/// Sample the brand gradient at position `t` (clamped to 0.0..=1.0).
pub fn gradient(t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let segments = (GRADIENT_STOPS.len() - 1) as f32;
    let scaled = t * segments;
    let index = (scaled.floor() as usize).min(GRADIENT_STOPS.len() - 2);
    let local = scaled - index as f32;
    let (r0, g0, b0) = GRADIENT_STOPS[index];
    let (r1, g1, b1) = GRADIENT_STOPS[index + 1];
    let lerp = |a: u8, b: u8| -> u8 {
        (a as f32 + (b as f32 - a as f32) * local)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::Rgb(lerp(r0, r1), lerp(g0, g1), lerp(b0, b1))
}

/// Linearly blend two colors. Used by fade-in animations. When either color is
/// not an RGB truecolor value, the target color is returned unchanged.
pub fn lerp(from: Color, to: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    match (from, to) {
        (Color::Rgb(r0, g0, b0), Color::Rgb(r1, g1, b1)) => {
            let mix = |a: u8, b: u8| -> u8 {
                (a as f32 + (b as f32 - a as f32) * t)
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            Color::Rgb(mix(r0, r1), mix(g0, g1), mix(b0, b1))
        }
        _ => to,
    }
}

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

/// Panel variant with a caller-provided border color, used for "breathing"
/// animated accents on active surfaces (e.g. the running tools panel).
pub fn panel_border(title: &str, border: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(format!(" {title} "))
        .title_style(Style::default().fg(border).add_modifier(Modifier::BOLD))
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
