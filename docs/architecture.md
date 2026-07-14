# Архитектура OmniCLI

## Обзор

OmniCLI — provider-neutral runtime для агентных CLI-приложений. Основные слои:

```
CLI / TUI / Workflow / Supervisor
            │
      Agent (оркестрация, верификация, цикл)
            │
    Provider (fake, openai, anthropic, ollama)
            │
    Tools (файлы, shell, git, плагины, MCP)
            │
Permission + Store (SQLite) + Events (NDJSON)
```

## Модули

| Модуль | Назначение |
|--------|-----------|
| `agent` | Provider-neutral цикл оркестрации и верификации |
| `provider` | Абстракция модели + адаптеры fake/OpenAI/Anthropic/Ollama |
| `tools` | Ограниченное выполнение процессов, атомарные файловые операции, Git, shell, плагины |
| `plugin` | Хост подпроцессных плагинов через JSON-RPC over stdio |
| `permission` | Централизованная политика capability |
| `store` | SQLite-сессии, сообщения, журнал событий |
| `events` | Версионированный протокол исполнения |
| `config` | Типизированная конфигурация и дефолты проекта |

## Ключевые ограничения безопасности

- Запись и shell требуют явных флагов (`--allow-write`, `--allow-shell`).
- `apply_patch` требует SHA-256 файла и exact-match текста.
- `create_file` не перезаписывает существующие файлы.
- MCP, плагины, worktree — отдельные capability-флаги.

## Поток событий

Каждый запрос получает UUIDv7 и журналируется в SQLite инкрементально. NDJSON-поток версионируется.

## Что планируется

- LM Studio, llama.cpp, generic OpenAI-compatible адаптеры
- Workflow artifact views
- Дополнительные plugin SDK
- Structured-output адаптеры
