# OmniCLI — Полное описание проекта для gab.ai

> Сгенерировано: 2026-07-14
> Статус: активная разработка, 99 тестов проходят

---

## 1. Краткая сводка

**OmniCLI** — open-source агентная CLI runtime на Rust. Provider-neutral платформа для исследования проектов, написания и изменения кода, запуска команд, управления агентами, автоматизации workflow, подключения внешних инструментов и моделей.

- **Язык:** Rust 2024 edition
- **Лицензия:** Apache-2.0
- **Пакет:** `omnicli` v0.1.0
- **Бинарник:** `omni`
- **База данных:** SQLite (`sessions.db`)
- **Протокол событий:** versioned NDJSON
- **Плагины:** subprocess JSON-RPC over stdio
- **MCP:** stdio/SSE, client + server

---

## 2. Статус реализации

| Этап | Статус |
|------|--------|
| Walking skeleton | Готово |
| Coding agent | Готово |
| Product UX (TUI) | Готово |
| Providers: fake, openai, anthropic, ollama, openai-compatible, lm-studio, llama.cpp | Готово |
| MCP client/server | Готово |
| Plugins | Готово |
| Workflows (YAML DAG, retry, artifacts, resume) | Готово |
| Supervisor / multi-agent in worktrees | Готово |
| Managed worktrees | Готово |
| Structured-output adapters | Не начато |
| Workflow artifact views | Не начато |
| Benchmarks / fuzzing | Не начато |
| Cross-platform packaging / signed releases | Не начато |

**Тесты:** `cargo test` — 99 passed, 0 failed (65 unit + 34 E2E).

**Линт:** `cargo clippy -- -D warnings` — чисто.

**Формат:** `cargo fmt -- --check` — чисто.

---

## 3. Структура репозитория

```
OmniCLI/
├── .github/workflows/ci.yml
├── .gitignore
├── Cargo.lock              (69.6 KB)
├── Cargo.toml              (1.4 KB)
├── LICENSE                 (9.9 KB, Apache-2.0)
├── README.md               (14.3 KB)
├── PROJECT_SUMMARY.md      (этот файл)
├── rust-toolchain.toml     (0.1 KB)
├── docs/                   (Obsidian-ваулт + документация)
│   ├── README.md
│   ├── architecture.md
│   ├── commands.md
│   ├── context.md
│   ├── decisions.md
│   ├── journal.md
│   ├── providers.md
│   └── todo.md
├── examples/
│   ├── artifacts.yml
│   ├── inspect.yml
│   ├── supervisor.yml
│   └── plugins/datetime/
│       ├── datetime_plugin.py
│       └── omni-plugin.toml
├── src/                    (основной код, ~470 KB)
│   ├── agent.rs            (24.6 KB)
│   ├── config.rs           (22.3 KB)
│   ├── context.rs          (15.8 KB)
│   ├── events.rs           (2.2 KB)
│   ├── lib.rs              (1.7 KB)
│   ├── main.rs             (47.5 KB)
│   ├── mcp.rs              (28.3 KB)
│   ├── permission.rs       (11.3 KB)
│   ├── plugin.rs           (20.4 KB)
│   ├── protocol.rs         (2.2 KB)
│   ├── provider.rs         (61.8 KB)
│   ├── store.rs            (36.5 KB)
│   ├── supervisor.rs       (24.0 KB)
│   ├── tools.rs            (41.5 KB)
│   ├── tui.rs              (82.7 KB)
│   ├── workflow.rs         (60.6 KB)
│   └── worktree.rs         (32.3 KB)
└── tests/
    └── cli_e2e.rs          (30.8 KB)
```

---

## 4. Архитектура

```
CLI / TUI / Workflow / Supervisor
            │
      Agent (оркестрация, верификация, цикл)
            │
    Provider (fake, openai, anthropic, ollama, lm-studio, llama-cpp, openai-compatible)
            │
    Tools (файлы, shell, git, плагины, MCP)
            │
Permission + Store (SQLite) + Events (NDJSON)
```

| Модуль | Назначение |
|--------|-----------|
| `agent` | Provider-neutral цикл оркестрации и верификации |
| `provider` | Абстракция модели + адаптеры |
| `tools` | Ограниченное выполнение процессов, атомарные файловые операции, Git, shell, плагины |
| `plugin` | Хост подпроцессных плагинов через JSON-RPC over stdio |
| `mcp` | MCP client/server layer |
| `permission` | Централизованная политика capability |
| `store` | SQLite-сессии, сообщения, журнал событий |
| `events` | Версионированный протокол исполнения |
| `config` | Типизированная конфигурация и дефолты |
| `context` | Индексация workspace, поиск файлов |
| `workflow` | YAML DAG workflow engine |
| `supervisor` | Параллельные агенты в worktrees |
| `worktree` | Управление Git worktrees |
| `tui` | Ratatui-based интерактивный интерфейс |

---

## 5. Провайдеры моделей

### `fake`
Детерминированный. Команды: `read`, `write`, `patch`, `status`, `diff`, `checks`, `shell`, `plugin`, `search`, `mcp`.

### `openai`
```powershell
$env:OPENAI_API_KEY = "..."
cargo run -- --provider openai --model "gpt-4.1-mini" run "hello"
```

### `anthropic`
```powershell
$env:ANTHROPIC_API_KEY = "..."
cargo run -- --provider anthropic --model "claude-sonnet-4-20250514" run "hello"
```

### `ollama`
```powershell
cargo run -- --provider ollama --model "llama3.1" --base-url "http://localhost:11434" run "hello"
```

### `lm-studio`
```powershell
cargo run -- --provider lm-studio --model "qwen2.5-7b" --base-url "http://localhost:1234/v1" run "hello"
```

### `llama-cpp`
```powershell
cargo run -- --provider llama-cpp --model "qwen2.5-7b" --base-url "http://localhost:8080" run "hello"
```

### `openai-compatible`
```toml
provider = "openai-compatible"

[openai_compatible]
base_url = "https://generic.example.invalid/v1/"
model = "custom-model"
api_key_env = "CUSTOM_API_KEY"
timeout_seconds = 30
```

---

## 6. Команды CLI

```powershell
omni doctor
omni run "исправь падающие тесты"
omni ask "объясни архитектуру"
omni plan "добавь OAuth"
omni review --verify
omni models
omni tools
omni sessions list
omni context index
omni context query "agent loop"
omni workflow run workflow.yml
omni workflow resume <RUN_ID>
omni supervisor run tasks.yml --concurrency 2 --allow-write --verify
omni worktree create task-17
omni tui
omni completions bash|zsh|fish|powershell
```

---

## 7. Пример конфигурации `omni.toml`

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

[lm_studio]
base_url = "http://localhost:1234/v1/"
model = "default"
timeout_seconds = 120

[llama_cpp]
base_url = "http://localhost:8080"
model = "local"
timeout_seconds = 120
temperature = 0.7
n_predict = -1

[mcp]
max_message_bytes = 1048576

[mcp.servers.local]
command = "C:/absolute/path/to/mcp-server.exe"
args = []
env = ["MCP_TOKEN"]
startup_timeout_seconds = 10
call_timeout_seconds = 30

[plugins.datetime]
manifest = "C:/absolute/path/to/datetime/omni-plugin.toml"
args = []
env = []
startup_timeout_seconds = 10
call_timeout_seconds = 30
max_output_bytes = 65536
```

---

## 8. Workflow пример

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

---

## 9. Plugin пример

```toml
[plugin]
name = "datetime"
version = "0.1.0"
description = "Provides current date and time tools"
entrypoint = "datetime_plugin.py"

[permissions]
read = true
```

---

## 10. Последние изменения (сессия 2026-07-13)

### Исправлен падающий тест OpenAI
- Файл: `src/provider.rs`
- `ToolCall` теперь эмитируется при `finish_reason="tool_calls"`, а не только на `[DONE]`.

### Добавлен LM Studio provider
- `ProviderKind::LmStudio`, `LmStudioConfig`, CLI `--provider lm-studio`.
- Работает через `OpenAiProvider` с пустым API-ключом.

### Добавлен llama.cpp provider
- `ProviderKind::LlamaCpp`, `LlamaCppConfig`, CLI `--provider llama-cpp`.
- Собственный `LlamaCppProvider` с нативным endpoint `/completion`.
- ChatML-промпт, streaming SSE/NDJSON, параметры `temperature`/`n_predict`.

### Обновлена документация
- `docs/providers.md`, `docs/commands.md`, `docs/decisions.md`, `docs/journal.md`, `docs/todo.md`.

---

## 11. Backlog

- Structured-output adapters
- Workflow artifact views
- Дополнительные plugin SDK
- Бенчмарки и fuzzing
- Cross-platform packaging
- Signed releases
- SBOM
- Дополнительные provider adapters (Google, Bedrock, Azure OpenAI и др.)

---

## 12. Как собрать и протестировать

```powershell
cargo build
cargo test
cargo run -- doctor
cargo run -- run "hello"
cargo fmt
cargo clippy -- -D warnings
```

---

## 13. Основные зависимости

- `tokio` — async runtime
- `clap` — CLI
- `ratatui` + `crossterm` — TUI
- `rusqlite` — SQLite
- `reqwest` — HTTP client
- `serde` + `serde_json` + `toml`
- `async-trait`, `futures-util`
- `uuid` — UUIDv7
- `sha2` — хеширование патчей
- `eventsource-stream` — SSE streaming
- `thiserror`
- `cap-std` — sandboxing

---

## 14. Безопасность

- API-ключи только через env.
- Явные разрешения на опасные операции.
- Path traversal / symlink escape защита.
- SHA-256 + exact-match для патчей.
- MCP и плагины изолированы в подпроцессах.
- Worktree операции безопасны.
- Нет скрытой телеметрии.

---

*Конец сводки. Файл можно скопировать целиком и отправить gab.ai.*
