# OmniCLI

OmniCLI is a provider-neutral foundation for an open-source agentic command-line platform. The first vertical slice includes a deterministic model provider, typed tools, explicit permissions, SQLite-backed sessions, and a versioned NDJSON event stream.

## Build

```powershell
cargo build
cargo test
```

## Use

```powershell
cargo run -- doctor
cargo run -- run "hello"
cargo run -- --json run "hello"
cargo run -- --provider openai --model gpt-4.1-mini run "inspect this project"
cargo run -- run "read README.md"
cargo run -- run --allow-write "write notes.txt::hello"
cargo run -- run "status"
cargo run -- run "diff"
cargo run -- run --verify "checks"
cargo run -- ask "explain the agent loop"
cargo run -- plan "add OAuth login"
cargo run -- review --verify
cargo run -- run --allow-shell "shell git status"
cargo run -- run --allow-plugins "plugin plugin__datetime__now::{}"
cargo run -- plugins list
cargo run -- sessions list
```

The built-in `fake` provider is deterministic and intentionally small. Prompts beginning with `read`, `write`, `patch`, `status`, `diff`, `checks`, `shell`, or `plugin` exercise the complete provider-to-tool execution loop without network credentials.
 The `openai` provider supports compatible `/chat/completions` APIs, true SSE streaming, fragmented tool calls, cancellation, and resumable tool history.

Set credentials through the environment only:

```powershell
$env:OPENAI_API_KEY = "..."
cargo run -- --provider openai --base-url "https://api.openai.com/v1/" --model "gpt-4.1-mini" run "hello"

$env:ANTHROPIC_API_KEY = "..."
cargo run -- --provider anthropic --model "claude-sonnet-4-20250514" run "hello"

# Ollama — local, no API key required
cargo run -- --provider ollama --model "llama3.1" --base-url "http://localhost:11434" run "hello"
```

## Configuration

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

CLI flags override path settings. Writes and shell execution are denied unless explicitly enabled for the run.

`read_file` returns the complete file SHA-256 in tool metadata. Existing files can only be changed through `apply_patch`, which requires that hash and one unique exact text match. New files use `create_file`, which refuses to overwrite an existing path. `git_status` and `git_diff` are fixed read-only operations and do not require shell permission.

`--verify` grants only the fixed `run_checks` capability. It detects root Rust, Node, and Python projects and runs bounded checks with direct argv execution. After a successful workspace edit, the agent validates automatically; failed output is returned to the model for another repair turn. Automated repairs still require `--allow-write`, and arbitrary shell remains separately gated by `--allow-shell`.

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

## Read-only modes

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

## Introspection

```powershell
cargo run -- models
cargo run -- tools
cargo run -- --json tools
```

`models` lists configured selectors. `tools` lists every tool available to the active agent with its JSON schema.

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

## Terminal UI

Launch the interactive TUI with the configured provider:

```powershell
omni tui
omni --provider openai --model gpt-4.1-mini tui
omni tui --session 019f...
omni tui --allow-mcp-start
```

The TUI streams model output, preserves and continues SQLite sessions, shows tool activity, and asks for one-time `y/n` approval before valid write, shell, validation, or MCP calls that were not preauthorized. Invalid and escaping paths remain hard denials and never reach the modal.

Keybindings:

- `Enter` / `Ctrl+S`: submit a task while idle
- `Alt+Enter`: insert a new line
- `Esc`: cancel the active run, clear input, or exit
- `Ctrl+Q`: cancel and exit
- `y` / `n`: resolve a permission prompt
- `Ctrl+L`: open the SQLite session picker
- `Ctrl+P`: open the configured model picker
- `Ctrl+N`: start a new session
- `Ctrl+W`: open the persisted workflow dashboard
- `PageUp` / `PageDown`: scroll the transcript
- `End`: follow the latest output

Workflow commands can run inside the TUI without leaving the terminal:

```text
/workflows
/workflow run examples/inspect.yml
/workflow resume 019f...
/models
/model fake
/model openai/gpt-4.1-mini
```

The dashboard polls the SQLite journal while a local workflow is active and shows step status, tool, attempts, and dependency edges. `Tab` or `Right` focuses steps; the selected step displays declared/captured artifacts, complete SHA-256 values, errors, and bounded stdout/stderr. Use `r` to resume the selected run, `n` to prepare a new workflow path, `c` to cancel the workflow owned by this TUI, and `Ctrl+R` to refresh persisted state.

Model switching is allowed only while idle and keeps the current SQLite session, transcript, tools, and workflow runtime. The picker exposes only configured selectors; it never accepts API keys or arbitrary endpoint URLs. A failed provider construction leaves the active model unchanged.

## Managed Worktrees

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

## Parallel Agent Supervisor

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

TUI mode requires interactive stdin and stdout. `--json tui` and redirected terminals are rejected before raw mode begins.

## Architecture

- `agent`: provider-neutral orchestration and verification loop
- `provider`: model contract with fake, OpenAI, Anthropic and Ollama adapters
- `tools`: bounded process execution, atomic file tools, safe Git inspection, shell, and plugin tools
- `plugin`: subprocess plugin host with JSON-RPC stdio protocol
- `permission`: centralized capability policy
- `store`: SQLite sessions, messages, and event journal
- `events`: versioned execution protocol
- `config`: typed configuration and project defaults

Upcoming vertical slices add LM Studio, llama.cpp and generic OpenAI-compatible adapters, workflow artifact views, additional plugin SDKs, and structured-output adapters.
