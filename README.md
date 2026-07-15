# ◆ OmniCLI

[![CI](https://github.com/hdispejioppd-gif/OmniCli/actions/workflows/ci.yml/badge.svg)](https://github.com/hdispejioppd-gif/OmniCli/actions/workflows/ci.yml) [![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE) [![Rust 1.94](https://img.shields.io/badge/rust-1.94%20%C2%B7%20edition%202024-orange.svg)](rust-toolchain.toml)

**Open-source, provider-neutral agentic CLI runtime** — chat, typed tools, DAG workflows, parallel agents, and managed Git worktrees, all in a fast terminal UI.

`Rust 1.94 · edition 2024 · Apache-2.0 · ratatui TUI · SQLite-backed`

---

## Why omni

| | omni | pi & other AI CLIs |
| --- | --- | --- |
| Framed, line-numbered, syntax-highlighted code in the transcript | ✅ | rare |
| Live `/` command palette + `F1` key overlay | ✅ | rare |
| Unlimited named OpenAI-compatible providers in TOML | ✅ | varies |
| Full-access mode with a visible badge and audit trail | ✅ | often silent |
| Workflow DAGs, supervisors, plugins, MCP — journaled in SQLite | ✅ | varies |

- **Provider-neutral by design.** One runtime, many models: `fake` (deterministic, offline), OpenAI, Anthropic, Ollama — plus LM Studio, llama.cpp, and unlimited named OpenAI-compatible endpoints (OpenRouter, DeepSeek, Groq, vLLM, …) via `[custom_providers.*]`. Switch models mid-session without losing state.
- **Beautiful, fast TUI.** A cohesive dark truecolor theme, rounded panels, animated status spinner, speaker-labelled transcript, syntax-highlighted markdown and code, live workflow DAG dashboard — redrawn at 30 fps.
- **Security first, not bolted on.** Every capability (write, shell, MCP, plugins, worktrees) is denied by default and granted per run or per call. Path escapes are hard denials that never reach a prompt.
- **Everything is journaled.** Sessions, messages, workflow runs, and events live in SQLite with a versioned NDJSON event stream — resumable, inspectable, machine-readable with `--json`.
- **Parallel by default.** Independent workflow steps run concurrently; the supervisor fans out whole agents across isolated Git worktrees.

## Quick start

```text
omni           # just `omni` — opens the interactive TUI
omni init      # write a starter omni.toml into the workspace
omni doctor    # inspect provider, API keys, database, and plugins
```

```powershell
cargo build
cargo test

cargo run -- doctor            # environment check
cargo run -- tui               # interactive TUI
cargo run -- run "hello"       # one-shot agent run
cargo run -- --json run "hello"
```

The built-in `fake` provider is deterministic and intentionally small. Prompts beginning with `read`, `write`, `patch`, `status`, `diff`, `checks`, `shell`, or `plugin` exercise the complete provider-to-tool execution loop without network credentials.

### Real providers

Set credentials through the environment only — omni never stores keys:

```powershell
$env:OPENAI_API_KEY = "..."
cargo run -- --provider openai --base-url "https://api.openai.com/v1/" --model "gpt-4.1-mini" run "hello"

$env:ANTHROPIC_API_KEY = "..."
cargo run -- --provider anthropic --model "claude-sonnet-4-20250514" run "hello"

# Ollama — local, no API key required
cargo run -- --provider ollama --model "llama3.1" --base-url "http://localhost:11434" run "hello"
```

The `openai` provider supports compatible `/chat/completions` APIs, true SSE streaming, fragmented tool calls, cancellation, and resumable tool history.

## Terminal UI

```powershell
omni tui
omni --provider openai --model gpt-4.1-mini tui
omni tui --session 019f...
omni tui --allow-mcp-start
```

The TUI streams model output, preserves and continues SQLite sessions, shows live tool activity with status glyphs (◌ requested · ◔ awaiting · ◐ running · ● done · ✖ failed), and asks for one-time `y/n` approval before valid write, shell, validation, or MCP calls that were not preauthorized. Invalid and escaping paths remain hard denials and never reach the modal.

### Keybindings

| Key | Action |
| --- | --- |
| `Enter` / `Ctrl+S` | Submit a task while idle |
| `Alt+Enter` | Insert a new line |
| `Esc` | Cancel the active run, clear input, or exit |
| `Ctrl+Q` | Cancel and exit |
| `y` / `n` | Resolve a permission prompt |
| `Ctrl+P` | Model picker |
| `Ctrl+L` | Session picker |
| `Ctrl+N` | New session |
| `Ctrl+W` | Workflow dashboard |
| `Ctrl+T` | Supervisor dashboard |
| `↑` / `↓` | Prompt history |
| `F1` | Keyboard help overlay |
| `Ctrl+A` / `Ctrl+E` | Jump to line start / end |
| `Ctrl+U` | Clear the prompt |
| `Ctrl+K` | Delete to end of line |
| `PageUp` / `PageDown` | Scroll the transcript |
| `End` | Follow the latest output |

### Slash commands

```text
/workflows
/workflow run examples/inspect.yml
/workflow resume 019f...
/supervisors
/supervisor run tasks.yml
/models
/model fake
/model openai/gpt-4.1-mini
/help
/export
/clear
/yolo
/retry
```

Type `/` in the prompt to see the live command palette; `/export` writes the current transcript to `omni-session-<id>.md` in the working directory.

You can keep typing while omni is working — press ↵ to queue the next message and it is sent automatically when the current turn finishes. `/retry` re-runs the last prompt.

The workflow dashboard polls the SQLite journal while a local workflow is active and shows step status, tool, attempts, and dependency edges. `Tab` or `Right` focuses steps; the selected step displays declared/captured artifacts, complete SHA-256 values, errors, and bounded stdout/stderr. Use `r` to resume the selected run, `n` to prepare a new workflow path, `c` to cancel the workflow owned by this TUI, and `Ctrl+R` to refresh persisted state.

Model switching is allowed only while idle and keeps the current SQLite session, transcript, tools, and workflow runtime. The picker exposes only configured selectors; it never accepts API keys or arbitrary endpoint URLs. A failed provider construction leaves the active model unchanged.

TUI mode requires interactive stdin and stdout. `--json tui` and redirected terminals are rejected before raw mode begins.

## Configuration

omni remembers the last model you picked in the TUI (stored as `last_model` in the data dir) and restores it on the next launch, unless you explicitly pass `--provider`, `--model`, or `--profile`.

Create `omni.toml` in the workspace or pass `--config <path>`:

```toml
data_dir = ".omni"
workspace = "."
max_turns = 8
max_tool_output_bytes = 65536
max_file_bytes = 8388608
shell_timeout_seconds = 30
provider = "fake"

[openai]
base_url = "https://api.openai.com/v1/"
model = "gpt-4.1-mini"
timeout_seconds = 120

[anthropic]
base_url = "https://api.anthropic.com/v1/"
model = "claude-sonnet-4-20250514"
timeout_seconds = 120
api_version = "2023-06-01"

[ollama]
base_url = "http://localhost:11434"
model = "llama3.1"
timeout_seconds = 120

[profiles.fast]
max_turns = 3
max_tool_output_bytes = 16384
shell_timeout_seconds = 15
```

Built-in profiles: `default`, `offline`, `ci`, `secure`, `fast`. Override them in the config or select at runtime:

```powershell
cargo run -- --profile offline doctor
cargo run -- --profile ci --json run "status"
```

CLI flags override path settings.

## Permissions & safety model

> **Full access mode.** `omni tui --full-access` / `omni run --full-access` (alias `--yolo`), or toggle live from inside the TUI with the `/yolo` slash command grants every capability (write, shell, MCP, plugins, worktrees) for the session and auto-approves all remaining permission prompts. The header shows a `FULL ACCESS` badge and every auto-approval is logged to the transcript. Use it only in workspaces you fully trust.

Writes and shell execution are denied unless explicitly enabled for the run:

```powershell
cargo run -- run --allow-write "write notes.txt::hello"
cargo run -- run --allow-shell "shell git status"
cargo run -- run --allow-plugins "plugin plugin__datetime__now::{}"
```

- `read_file` returns the complete file SHA-256 in tool metadata.
- Existing files can only be changed through `apply_patch`, which requires that hash and one unique exact text match.
- New files use `create_file`, which refuses to overwrite an existing path.
- `git_status` and `git_diff` are fixed read-only operations and do not require shell permission.
- `--verify` grants only the fixed `run_checks` capability. It detects root Rust, Node, and Python projects and runs bounded checks with direct argv execution. After a successful workspace edit, the agent validates automatically; failed output is returned to the model for another repair turn. Automated repairs still require `--allow-write`, and arbitrary shell remains separately gated by `--allow-shell`.

### Read-only modes

`ask`, `plan`, and `review` run with a system prompt and a restrictive policy. Writes, shell, MCP, and worktree mutations are denied by default; `--verify` is optional for `review`.

```powershell
cargo run -- ask "explain the agent loop"
cargo run -- plan "add OAuth login"
cargo run -- review --verify
```

These modes are still journaled in SQLite and visible in `sessions list`.

## Context engine

Index the workspace and query it without network calls:

```powershell
cargo run -- context index
cargo run -- context status
cargo run -- context query "agent loop"
cargo run -- --json context query "agent loop" --limit 5
```

The engine classifies languages, detects package managers, respects `.gitignore` and `.omniignore`, skips binaries and ignored directories, and returns ranked file matches with explanations.

Agents can also call `search_files` directly:

```powershell
cargo run -- run "search authentication"
```

## Workflows

Workflows are restricted YAML DAGs of literal tool calls. They have no templating, environment expansion, hidden shell, or implicit permissions.

```yaml
version: 1

steps:
  - id: status
    tool: git_status
    arguments: {}

  - id: read_manifest
    tool: read_file
    arguments:
      path: Cargo.toml

  - id: checks
    tool: run_checks
    arguments: {}
    needs: [status, read_manifest]
    retry:
      max_attempts: 3
      delay_ms: 1000
```

```powershell
omni workflow run workflow.yml
omni --json workflow run workflow.yml --concurrency 8 --verify
omni workflow run remote.yml --allow-mcp-start --allow-mcp-call
omni workflow resume 019f... --verify
```

Independent steps run concurrently up to the configured limit. Failed steps skip only their descendants; unrelated branches continue. Reports remain in declaration order and `--json` emits one machine-readable document. YAML files are limited to 1 MiB and 256 steps; cycles, unknown dependencies, non-object arguments, tags, aliases, merge keys, deep nesting, and unknown fields are rejected before execution.

Workflow execution is not transactional: successful side effects remain if another branch fails. Express write ordering explicitly through `needs`.

Every run receives a UUIDv7 and is journaled incrementally in SQLite. `workflow resume <RUN_ID>` restores successful steps and reruns failed, skipped, cancelled, or interrupted work. Resume recalculates the logical workflow fingerprint and rejects semantic changes; formatting, comments, mapping order, and dependency-list order do not invalidate it. Permissions are never persisted and must be granted again on the resume command.

Retry policies are fixed and bounded to 10 attempts with a maximum delay of 60 seconds. Only tool execution failures are retried automatically; invalid arguments and permission denials fail immediately. Incomplete side-effecting steps use at-least-once semantics after a process crash.

### Artifacts (workflow v2)

Workflow version 2 can require file artifacts from successful steps:

```yaml
version: 2
steps:
  - id: package
    tool: read_file
    arguments:
      path: target/release/omni.exe
    artifacts:
      - target/release/omni.exe
```

Artifact paths are workspace-relative, bounded, and opened through a capability directory. Symbolic links and escaping paths are rejected. Reports persist only path, byte count, and SHA-256 metadata. A missing or invalid declared artifact fails the step even when its tool succeeded.

## MCP

OmniCLI can import tools from MCP stdio processes. Process startup and remote calls are separate permissions:

```toml
[mcp]
max_message_bytes = 1048576

[mcp.servers.local]
command = "C:/absolute/path/to/mcp-server.exe"
args = []
env = ["MCP_TOKEN"]
startup_timeout_seconds = 10
call_timeout_seconds = 30
```

Only environment variable names are stored; values are read at process startup. MCP children receive a cleared environment plus explicitly listed variables.

```powershell
omni run --allow-mcp-start --allow-mcp-call "use the imported tools"
omni mcp serve
omni mcp serve --allow-write --verify
```

Imported names use `mcp__<server>__<tool>`. `--allow-mcp-start` authorizes configured executables, while `--allow-mcp-call` separately authorizes model-generated arguments leaving through those tools. Server mode exports built-in tools and reapplies its own local write, shell, and validation policy.

## Plugins

OmniCLI can load external subprocess plugins that expose additional tools through a JSON-RPC stdio protocol. Plugins are opt-in per command via `--allow-plugins`.

Create an `omni-plugin.toml` and an entrypoint:

```toml
[plugin]
name = "datetime"
version = "0.1.0"
description = "Provides current date and time tools"
entrypoint = "datetime_plugin.py"

[permissions]
read = true
```

```python
# datetime_plugin.py
import json, sys

def send(v): print(json.dumps(v), flush=True)

def main():
    for line in sys.stdin:
        req = json.loads(line)
        mid = req.get("id")
        method = req.get("method")
        if method == "initialize":
            send({"jsonrpc":"2.0","id":mid,"result":{"name":"datetime","version":"0.1.0"}})
        elif method == "tools/list":
            send({"jsonrpc":"2.0","id":mid,"result":[{"name":"now","description":"Current UTC time","input_schema":{"type":"object"}}]})
        elif method == "tools/call":
            send({"jsonrpc":"2.0","id":mid,"result":{"success":True,"stdout":"...","stderr":"","truncated":False,"metadata":{}}})

if __name__ == "__main__":
    main()
```

Configure the plugin in `omni.toml`:

```toml
[plugins.datetime]
manifest = "C:/absolute/path/to/datetime/omni-plugin.toml"
args = []
env = []
startup_timeout_seconds = 10
call_timeout_seconds = 30
max_output_bytes = 65536
```

Commands:

```powershell
omni plugins list
omni plugins show datetime
omni run --allow-plugins "use plugin__datetime__now"
```

Plugin tool names are namespaced as `plugin__<plugin>__<tool>`. The fake provider supports `plugin <tool>::<json-arguments>` for testing.

## Managed worktrees

OmniCLI creates linked Git worktrees under an external deterministic data directory and generates branches as `omni/<name>`:

```powershell
omni worktree create task-17
omni worktree list
omni worktree inspect task-17
omni --worktree task-17 run "inspect and repair this branch"
omni worktree remove task-17
```

Names are lowercase ASCII with digits and hyphens. Checkout references are restricted to `HEAD`, full refs, or full object IDs. Worktree tools are also available to agents and workflows; mutations require `--allow-worktree` or a one-call TUI approval.

The manager stores ownership metadata outside the checkout, disables hooks, rejects configured smudge/process filters, clears inherited Git repository-selection variables, and never uses force, stash, reset, or recursive deletion. Removal refuses staged, modified, untracked, ignored, conflicted, or dirty-submodule states. The generated branch is preserved after clean removal.

The configured `data_dir` must be outside the repository and Git common directory for worktree operations. `HEAD` must resolve to an existing commit. Unknown or unmanaged paths cannot be selected through `--worktree`.

## Parallel agent supervisor

The supervisor runs independent agents concurrently in different managed worktrees:

```yaml
version: 1
tasks:
  - id: backend
    worktree: backend
    prompt: Fix the backend tests with the smallest safe patch.
    verify: true
  - id: frontend
    worktree: frontend
    prompt: Fix the frontend tests with the smallest safe patch.
    verify: true
```

```powershell
omni supervisor run tasks.yml --concurrency 2 --allow-write --verify
omni --json supervisor run tasks.yml
```

Each task receives its own Agent, ToolContext, cancellation token, and SQLite session. Duplicate or dirty worktrees are rejected before execution. Child agents cannot access worktree-management or MCP proxy tools. A failed task does not cancel successful siblings, and the final report remains in manifest order.

Inside the TUI, use `Ctrl+T`, `/supervisors`, or `/supervisor run tasks.yml` to view persisted batches, live task states, worktree names, errors, and session IDs.

## Introspection

```powershell
cargo run -- models
cargo run -- tools
cargo run -- --json tools
```

`models` lists configured selectors. `tools` lists every tool available to the active agent with its JSON schema.

## Architecture

- `agent` — provider-neutral orchestration and verification loop
- `provider` — model contract with fake, OpenAI, Anthropic and Ollama adapters
- `tools` — bounded process execution, atomic file tools, safe Git inspection, shell, and plugin tools
- `plugin` — subprocess plugin host with JSON-RPC stdio protocol
- `permission` — centralized capability policy
- `store` — SQLite sessions, messages, and event journal
- `events` — versioned execution protocol
- `config` — typed configuration and project defaults
- `theme` — truecolor design system for the TUI (palette, panels, styles)
- `tui` / `tui_markdown` / `diffview` — terminal UI, markdown renderer, interactive diff review

See `docs/` for architecture notes, the full command reference, provider details, and decision journal.

## Roadmap

Upcoming vertical slices add LM Studio, llama.cpp and generic OpenAI-compatible adapters, workflow artifact views, additional plugin SDKs, and structured-output adapters.

## License

Apache-2.0
