# ◆ Why OmniCLI

What makes OmniCLI stand out against other terminal AI agents (Claude Code, aider, pi and friends).

## Design first

- **Coherent visual language.** One theme file (`src/theme.rs`, Tokyo Night inspired) drives every panel, border, hint and status color — the UI never looks stitched together.
- **Real TUI, not a scrolling log.** Built on ratatui: rounded panels, scrollbar, live spinner, elapsed-run timer, blinking caret, mouse-wheel scrolling, dashboards.
- **Readable output.** Markdown rendering with syntax-highlighted code blocks, tables, task lists (☐/☑), heading glyphs (◆ ◇ ›) and dim dividers — straight in the terminal.
- **Discoverable.** F1 keyboard overlay and a live slash-command palette: you never have to memorize anything to be productive.

## Engineering depth

- **Single static Rust binary.** No Node/Python runtime, instant startup, tiny footprint (`lto = "thin"`, `strip = true`).
- **Sessions in SQLite.** Every conversation is stored locally and resumable; a session browser is one keystroke away (`Ctrl+L`).
- **Workflows & supervisors.** Declarative YAML workflows with resume support, plus a supervisor runtime for long multi-step tasks — with dedicated dashboards.
- **Permission system.** Policy-driven tool authorization with interactive approval prompts; nothing touches your machine silently.
- **Extensible.** MCP servers, plugins, LSP integration, repo map and full-text search (tantivy) built in.
- **Multi-model.** Switch providers/models mid-session with `/model` — no restart.

## Quality bar

- 130+ unit & e2e tests, `cargo clippy -D warnings`, rustfmt-clean, CI on Linux and Windows.
- Cross-platform terminal handling with panic-safe restore — your shell is never left broken.
