# Design system

The omni TUI follows a single design system defined in `src/theme.rs`. Every panel, label, and status glyph in the interface draws from this file — never hardcode raw colors in widgets.

## Palette (“Aurora” — premium truecolor)

A deep space-blue canvas with a violet→cyan brand gradient, warm amber
highlights, and carefully stepped text tones.

| Token | Role |
| --- | --- |
| `BG` | App background (near-black indigo) |
| `SURFACE` / `SURFACE_ALT` | Raised surfaces (inline code background), deeper fills |
| `BORDER` / `BORDER_FOCUS` | Panel borders, resting vs. focused (violet) |
| `TEXT` / `TEXT_MUTED` / `TEXT_DIM` | Content hierarchy: primary, secondary, tertiary |
| `ACCENT` (violet) | Brand, primary highlights, omni speaker label |
| `ACCENT_ALT` (cyan) | Secondary highlights, user speaker label, provider name |
| `SUCCESS` (mint) | Completed states, diff additions |
| `WARNING` (amber) | Running states, pending badges, permission borders |
| `ERROR` (rose) | Failures, diff removals |
| `INFO` (sky blue) | Tertiary headings, informational text |

### Brand gradient

`theme::gradient(t)` samples a violet → purple → magenta → cyan ramp for
`t` in `0.0..=1.0`, and `theme::gradient_stops()` exposes the raw anchors. It
powers the shimmering welcome logo (`tui_banner::welcome_lines_at`) and the
gradient-cycled running spinner (`spinner_color()` in `src/tui.rs`), both driven
by a wall-clock `banner_phase()` so they animate with zero extra state.

## Components

- **Panels** — `theme::panel(title)` for resting panels (rounded, muted border), `theme::panel_accent(title)` for interactive/primary panels (Prompt, pickers, dashboards), `theme::panel_warning(title)` for the permission modal (thick amber border).
- **Status glyphs** — tools: ◌ requested · ◔ awaiting · animated spinner running · ● succeeded · ✖ failed · ◒ interrupted. Header: spinner running · ● complete · ✖ error · ○ idle.
- **Speaker labels** — `transcript_label(name, color)` renders `▸ name` in bold; omni uses `ACCENT`, the user uses `ACCENT_ALT`.
- **Key hints** — `key_hints(&[(key, label)])` renders the footer shortcut bar with accent keys and dim labels separated by `·`.

## Motion

All animation is wall-clock based (no extra state) and driven by the 33 ms redraw interval:

- `spinner_glyph()` — 10-frame braille spinner at 80 ms per frame; used by the header status and running tools.
- `blink_on()` — 530 ms cursor blink phase for the streaming caret `▍`.
- Elapsed run time (`⏱ mm:ss`) is computed from `App.run_started` each frame.

## Markdown rendering

`src/tui_markdown.rs` renders model output: H1 in `ACCENT`, H2 in `ACCENT_ALT`, deeper headings in `INFO`; inline code on `SURFACE` with `WARNING` foreground; syntect-highlighted code fences; `▌` blockquotes; `•` bullets and true ordered-list numbering with nesting indents, both in `ACCENT`.

## Diff rendering

`DiffView::render_lines()` in `src/diffview.rs` produces the styled diff: bold file header with `◆`, accent hunk headers with state badges (● pending / ✓ accepted / ✗ rejected), `▶` cursor on the focused hunk, `SUCCESS` additions, `ERROR` removals, dim context, and right-aligned old/new line numbers.

## Rules of thumb

1. New UI must take colors and blocks from `theme.rs` — add a token if none fits.
2. Prefer glyphs over words for state (● ✓ ✖), and keep words lowercase and dim.
3. Empty states are `TEXT_DIM`, never bright.
4. One accent per line: don't mix `ACCENT` and `ACCENT_ALT` for equal-weight elements.
