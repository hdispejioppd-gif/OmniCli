# Журнал сессий

Автоматически пополняемый журнал заметок, которые KiloCode добавляет в ваулт.

## 2026-07-13

- Создан Obsidian-ваулт в `docs/`.
- Ваулт зарегистрирован в Obsidian: открой приложение — `OmniCLI Docs` появится в списке хранилищ.
- Базовая структура заметок готова (architecture, commands, providers, decisions, todo, context).
- Демонстрация: эта заметка добавлена напрямую файловой записью, без запуска Obsidian.

## 2026-07-13 (вечер, Dartik)

- Проанализирован текущий репозиторий OmniCLI.
- Состояние: walking skeleton, coding agent, product UX (TUI) и большая часть Stage 4 (providers, MCP, plugins, workflows, supervisor, worktree) уже реализованы.
- Найден и исправлен падающий unit-тест: `provider::tests::openai_stream_assembles_text_and_fragmented_tool_call`.
  - Причина: тестовые данные не содержали закрывающую фигурную скобку `}` в финальном фрагменте аргументов; реализация также не эмитировала `ToolCall` по `finish_reason="tool_calls"`, а ждала `[DONE]`.
  - Исправление: ToolCall теперь эмитируется при `finish_reason="tool_calls"`; на `[DONE]` остаётся только `Finished`.
- Реализован LM Studio provider.
  - Новый `ProviderKind::LmStudio`, конфиг `lm_studio`, CLI-флаг `--provider lm-studio`.
  - Работает через `OpenAiProvider` с пустым API-ключом; Authorization-заголовок не отправляется.
  - Добавлены unit- и E2E-тесты.
- Реализован llama.cpp provider.
  - Новый `ProviderKind::LlamaCpp`, конфиг `llama_cpp`, CLI-флаг `--provider llama-cpp`.
  - Собственный `LlamaCppProvider` с нативным endpoint `/completion` и ChatML-промптом.
  - Поддерживает текстовый streaming; tool calls не реализованы (по дизайну нативного API).
  - Добавлены unit- и E2E-тесты.
- Все тесты проходят (99 passed), clippy и fmt чистые.
- Создан `PROJECT_SUMMARY.md` — сводка проекта для отправки AI-чатам (gab.ai и др.).
- Следующий вертикальный срез: workflow artifact views или structured-output адаптеры.
