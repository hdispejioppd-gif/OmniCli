# Контекст для KiloCode / AI

## О проекте

OmniCLI — open-source агентная CLI runtime на Rust. Provider-neutral, с явной permission-моделью, SQLite-журналированием и versioned NDJSON events.

## Как я должен работать с этим проектом

1. **Проверяй `docs/` перед большими изменениями** — здесь фиксируются решения и контекст.
2. **Соблюдай существующий стиль кода** — Rust 2024 edition, минимальные изменения.
3. **Тестируй изменения**: `cargo test`.
4. **Не коммить без явного запроса** — пользователь должен попросить `git commit`/`push` отдельно.
5. **Фиксируй важные решения** — добавляй в `decisions.md` значимые архитектурные выборы.

## Ключевые ограничения

- Запись в файлы только через `--allow-write` или TUI-аппрув.
- Shell только через `--allow-shell`.
- MCP, плагины, worktree — отдельные capability.
- Патчи через `apply_patch` требуют SHA-256 и exact-match.

## Где что лежит

- Код: `src/`
- Тесты: `tests/`
- Примеры workflow: `examples/`
- Документация: `docs/` (этот Obsidian-ваулт)
- Конфигурация: `Cargo.toml`, `omni.toml` (если создан)

## Команды для быстрой проверки

```powershell
cargo build
cargo test
cargo run -- doctor
```
