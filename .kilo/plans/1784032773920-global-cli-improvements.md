# Global CLI Improvements Plan

## Context

OmniCLI is a 27-module Rust agent CLI with 31 tools, 7 providers, TUI, workflows, and MCP/plugin support. The codebase has accumulated technical debt: `main.rs` is 1633 lines with heavy duplication, web tools abuse `ToolError::Git` for HTTP errors, no provider retry logic, macOS dropped from CI, and several UX gaps. This plan addresses all 6 improvement categories the user selected.

## Phase 1 — Tool Correctness (no dependencies, quick wins)

### 1.1 Add `Http` variant to `ToolError`
- **File:** `src/tools.rs`
- Add `#[error("HTTP request failed: {0}")] Http(String)` to `ToolError` enum
- Replace all 9 `ToolError::Git(e.to_string())` in `WebFetch`, `WebSearch`, `WebDownload` with `ToolError::Http(e.to_string())`
- This makes error messages accurate instead of saying "git operation failed" for HTTP errors

### 1.2 Improve `strip_html`
- **File:** `src/tools.rs`, function `strip_html`
- Skip content inside `<script>...</script>`, `<style>...</style>`, `<noscript>`, `<svg>` tags entirely
- Decode numeric HTML entities (`&#123;` → char)
- Decode `&apos;` → `'`
- Collapse runs of whitespace into single spaces, preserve paragraph breaks

### 1.3 Add missing fake provider prompts
- **File:** `src/provider.rs`, `FakeProvider::respond`
- Add `download <url> <path>` → `tool_events("web_download", json!({"url": ..., "path": ...}))`
- Add `list <path>` → `tool_events("list_directory", json!({"path": ...}))`
- Add `move <from> <to>` → `tool_events("move_file", ...)`
- Add `copy <from> <to>` → `tool_events("copy_file", ...)`
- Add `delete <path>` → `tool_events("delete_file", ...)`
- Add `mkdir <path>` → `tool_events("create_directory", ...)`
- Add `rmdir <path>` → `tool_events("delete_directory", ...)`
- Add `replace <find>::<replace>` → `tool_events("replace_in_files", ...)`
- Add `plan <step>` → `tool_events("update_plan", ...)`
- Add `commit <message>` → `tool_events("git_commit", ...)`

### 1.4 Remove dead code in fake provider
- **File:** `src/provider.rs` line 284
- Second `strip_prefix("search ")` at line 284 is dead code (already matched at line 271)
- Delete the duplicate

### 1.5 Fix LSP permission clarity
- **File:** `src/tools.rs`, LSP tools (`GotoDefinition`, `Hover`, `FindReferences`, `WorkspaceSymbols`, `RenameSymbol`)
- These use `PermissionRequest::Shell { command: "lsp".into() }` which is confusing
- Add `PermissionRequest::LanguageServer { server: String }` to `src/permission.rs`
- Update `Policy::decide` to auto-allow LSP read operations when `--allow-shell` is set
- `rename_symbol` should require `--allow-write` (it modifies files)

## Phase 2 — Provider Reliability

### 2.1 Retry with exponential backoff
- **File:** `src/provider.rs`
- Add `RetryConfig` struct: `max_retries: u32 (default 3)`, `initial_delay: Duration (1s)`, `max_delay: Duration (30s)`
- Wrap `OpenAiProvider::respond` and `AnthropicProvider::respond` HTTP calls with retry:
  - Retry on: HTTP 429, 500, 502, 503, 504, and `reqwest::Error` where `is_timeout() || is_connect()`
  - Do NOT retry on: 400, 401, 403, 404, stream errors mid-response
  - Backoff: 1s, 2s, 4s (exponential, capped at `max_delay`)
  - Respect `Retry-After` header if present (override backoff)
  - Emit `ModelEvent::TextDelta` with `[retry: attempt N after status 429]` so user sees what's happening
- Add `retry` config section to `AppConfig`:
  ```toml
  [retry]
  max_retries = 3
  initial_delay_seconds = 1
  max_delay_seconds = 30
  ```

### 2.2 Fetch API key at call time for custom providers
- **File:** `src/provider.rs`, `ProviderFactory::build` for `ModelSpec::Custom`
- Currently: API key fetched in `build()`, stored in `OpenAiProvider` at construction
- Problem: if env var changes mid-session, provider doesn't pick it up
- Fix: store `api_key_env` in `OpenAiProvider`, fetch `std::env::var` in `respond()` each call
- This allows `/model aiand/...` to pick up a newly-set env var without restart

### 2.3 Better HTTP error messages
- **File:** `src/provider.rs`, `ProviderError`
- Enhance `HttpStatus` variant to include response body excerpt:
  ```rust
  HttpStatus { status: StatusCode, body: String }
  ```
- In `OpenAiProvider::respond`, read response body before erroring:
  ```rust
  let body = response.text().await.unwrap_or_default();
  return Err(ProviderError::HttpStatus { status, body: body.chars().take(500).collect() });
  ```
- This shows actual API error messages (e.g., "model not found", "invalid API key")

## Phase 3 — Config & UX

### 3.1 `.env` file support
- **File:** `Cargo.toml` — add `dotenvy = "0.15"`
- **File:** `src/main.rs`, top of `run()` — call `dotenvy::dotenv().ok()` before config loading
- Document in README that `.env` files are auto-loaded

### 3.2 `omni config` subcommand
- **File:** `src/main.rs`
- Add `Config { command: ConfigCommand }` to `Command` enum
- `ConfigCommand::Show` — print effective merged config as TOML
- `ConfigCommand::Edit` — open `data_dir/omni.toml` in `$EDITOR` (or notepad on Windows)
- `ConfigCommand::Path` — print path to global config file

### 3.3 Auto-detect shell for completions
- **File:** `src/main.rs`, `Command::Completions`
- Make `shell` argument optional (`Option<String>`)
- If not provided: detect from `$SHELL` (Unix) or `$PSModulePath`/registry (Windows)
- On Windows: default to PowerShell, fall back to cmd
- On Unix: parse `$SHELL` basename (bash, zsh, fish, etc.)

### 3.4 Default value bumps
- **File:** `src/config.rs`, `AppConfig::default()`
- `max_turns: 8` → `25` (complex tasks need more turns)
- `shell_timeout_seconds: 30` → `120` (builds take longer)
- `max_tool_output_bytes: 64 * 1024` → `128 * 1024` (modern output is larger)

### 3.5 Windows error message normalization
- **File:** `src/tools.rs`
- In `existing_regular_file` and similar functions, replace `.to_string()` on `io::Error` with custom formatting:
  ```rust
  fn io_error_message(error: &std::io::Error) -> String {
      match error.kind() {
          std::io::ErrorKind::NotFound => "file not found".into(),
          std::io::ErrorKind::PermissionDenied => "permission denied".into(),
          _ => error.to_string(),
      }
  }
  ```
- Use this in `ToolError::Io` and `ToolError::Path` construction
- This avoids Russian OS locale messages like "Не удается найти указанный файл"

## Phase 4 — main.rs Refactor

### 4.1 Split main.rs into command modules
- Create `src/commands/` directory with:
  - `mod.rs` — shared types (`RunOptions`, `build_policy`, `build_provider`)
  - `run.rs` — `Command::Run`, `Ask`, `Plan`, `Review`, `Fix` (all use `execute_run`)
  - `tui_cmd.rs` — `Command::Tui` setup
  - `context_cmd.rs` — `Command::Context` (index, query, status, map)
  - `mcp_cmd.rs` — `Command::Mcp` (list, serve)
  - `workflow_cmd.rs` — `Command::Workflow` (run, resume)
  - `supervisor_cmd.rs` — `Command::Supervisor`
  - `worktree_cmd.rs` — `Command::Worktree`
  - `session_cmd.rs` — `Command::Sessions`
  - `plugin_cmd.rs` — `Command::Plugins`
- `main.rs` becomes ~200 lines: CLI parsing, config loading, dispatch

### 4.2 `RunOptions` struct
- Replace 8 bool params in `execute_run` with:
  ```rust
  struct RunOptions {
      allow_write: bool,
      allow_shell: bool,
      verify: bool,
      allow_worktree: bool,
      allow_mcp_start: bool,
      allow_mcp_call: bool,
      allow_plugins: bool,
  }
  impl RunOptions {
      fn with_full_access() -> Self { all true }
      fn read_only() -> Self { all false }
  }
  ```

### 4.3 Shared helpers (deduplicate)
- `build_policy(config, &RunOptions) -> Policy` — single function used by all commands
- `build_provider(config) -> Arc<dyn ModelProvider>` — extract from `execute_run`
- `current_model_spec(config) -> ModelSpec` — move to `config.rs` as method on `AppConfig`
- `setup_tools_and_plugins(config, &RunOptions) -> (ToolRegistry, PluginRegistry)` — single function

### 4.4 `omni doctor` — compute tools dynamically
- **File:** `src/main.rs`, `Command::Doctor`
- Replace hardcoded `"tools": [...]` array with `configured_tools(&config).specs()` output
- This shows ALL tools including custom ones, not just 12 hardcoded names

## Phase 5 — TUI Polish

### 5.1 Enter in palette fills selected command
- **File:** `src/tui.rs`, `KeyCode::Enter if !app.running`
- When `app.palette_visible()` is true and Enter is pressed:
  - Get the selected command from `tui_palette::filter_commands()`
  - Replace editor text with the selected command name (e.g., `/yolo`)
  - Don't submit — let user add arguments if needed
- When palette is NOT visible: current behavior (submit prompt)

### 5.2 Theme documentation sync
- **File:** `docs/design.md`
- Current docs describe "Aurora truecolor" palette but `theme.rs` is now monochrome
- Update docs to describe the actual monochrome palette
- Or: ask user if they want to revert to Aurora theme (out of scope for this plan — docs follow code)

### 5.3 Transcript search
- **File:** `src/tui.rs`
- Add `/find <query>` command or `Ctrl+F` keybinding
- Store search query in `App`
- Highlight matching lines in transcript with `theme::warning()` background
- `Esc` clears search

### 5.4 Word wrap toggle
- **File:** `src/tui.rs`
- Currently `Wrap { trim: false }` is hardcoded
- Add `app.word_wrap: bool` (default true)
- `Ctrl+R` toggles word wrap
- Show `WRAP` or `NOWRAP` indicator in footer

## Phase 6 — CI & Releases

### 6.1 Re-add macOS to test matrix
- **File:** `.github/workflows/ci.yml`
- Add `macos-latest` back to matrix
- Add macOS temp canonicalization step:
  ```yaml
  - name: Canonical temp (macOS)
    if: runner.os == 'macOS'
    run: |
      mkdir -p /private/tmp/omni-t
      echo "TMPDIR=/private/tmp/omni-t/" >> "$GITHUB_ENV"
  ```
- The dunce fix from earlier should handle the path resolution

### 6.2 Add clippy `-D warnings` to CI
- **File:** `.github/workflows/ci.yml`
- Change `cargo clippy --all-targets` to `cargo clippy --all-targets -- -D warnings`
- This catches new lint issues in PRs

### 6.3 Release workflow
- **File:** `.github/workflows/release.yml` (new)
- Trigger: push tags `v*`
- Jobs: build for `ubuntu-latest`, `windows-latest`, `macos-latest`
- Steps: `cargo build --release`, strip binary, upload as artifact
- Create GitHub release with binary download links
- Use `softprops/action-gh-release` action

## Validation Plan

After each phase:
1. `cargo fmt --all -- --check` — clean
2. `cargo clippy --all-targets -- -D warnings` — clean
3. `cargo test` — all tests pass (currently 117 unit + 34 e2e = 151)
4. `cargo install --path . --force` — binary installs without error
5. Manual smoke test: `omni doctor`, `omni run "hello"`, `omni tui`

After Phase 6:
6. CI green on ubuntu + windows + macOS
7. Release workflow produces downloadable binaries

## Risks

- **main.rs refactor (Phase 4)** is the riskiest — 1633 lines with many branches. Must be done incrementally, testing after each extraction. Risk of breaking TUI/session/agent initialization.
- **Provider retry (Phase 2)** could cause double-billing if not careful. Must only retry on transport errors and 5xx, never on 200 with partial content.
- **macOS re-add (Phase 6.1)** may still fail if dunce alone doesn't fix the path issue. Fallback: keep macOS temp workaround permanently.
- **`.env` loading (Phase 3.1)** changes env var precedence — could surprise users who set vars in both `.env` and shell. Mitigate: shell env wins (dotenvy doesn't override existing vars).

## Open Questions

1. **Theme:** docs say "Aurora truecolor" but code is monochrome. Keep monochrome and update docs? (Recommended: yes, user explicitly wanted black/white)
2. **`max_turns` default:** 8 is too low, 25 is aggressive. 15-20 might be safer for cost. (Recommended: 25, user can override in config)
3. **Retry billing:** Should we emit a warning event before retrying so the user knows a retry is happening? (Recommended: yes, as `ModelEvent::TextDelta`)
