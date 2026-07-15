# Провайдеры моделей

## `fake`

Детерминированный встроенный провайдер для тестирования без сети. Поддерживает команды:
- `read`, `write`, `patch`, `status`, `diff`, `checks`, `shell`, `plugin`

## `openai`

Совместим с `/chat/completions` API. Поддерживает:
- SSE streaming
- Фрагментированные tool calls
- Отмену
- Возобновляемую историю tool-вызовов

## `anthropic`

Адаптер для Anthropic API.

## `ollama`

Локальный провайдер, не требует API-ключа.

```powershell
cargo run -- --provider ollama --model "llama3.1" --base-url "http://localhost:11434" run "hello"
```

## `lm-studio`

Адаптер для [LM Studio](https://lmstudio.ai/) local inference server. Работает поверх OpenAI-compatible API на `http://localhost:1234/v1/` по умолчанию. API-ключ не требуется — Authorization-заголовок не отправляется, если ключ пуст.

```powershell
cargo run -- --provider lm-studio --model "qwen2.5-7b" run "hello"
```

Конфигурация:

```toml
provider = "lm-studio"

[lm_studio]
base_url = "http://localhost:1234/v1/"
model = "qwen2.5-7b"
timeout_seconds = 120
```

## `llama-cpp`

Адаптер для нативного HTTP API `llama-server` из [llama.cpp](https://github.com/ggerganov/llama.cpp). Использует endpoint `/completion` с streaming SSE/NDJSON.

Особенности:
- Не требует API-ключа.
- Поддерживает текстовые ответы через ChatML-промпт (`<|im_start|>`).
- Tool calls не поддерживаются нативным `/completion`; для tool calls используй `lm-studio` или `openai-compatible` режим llama.cpp server.
- Настраиваемые `temperature` и `n_predict`.

```powershell
cargo run -- --provider llama-cpp --model "qwen2.5-7b" --base-url "http://localhost:8080" run "hello"
```

Конфигурация:

```toml
provider = "llama-cpp"

[llama_cpp]
base_url = "http://localhost:8080"
model = "qwen2.5-7b"
timeout_seconds = 120
temperature = 0.7
n_predict = -1
```

## `openai-compatible`

Generic OpenAI-compatible адаптер. Базовый URL, модель и env-переменная API-ключа настраиваются в конфиге.

## Кастомные провайдеры (`custom_providers`)

Как в Kilo Code: можно добавить сколько угодно OpenAI-compatible эндпоинтов —
OpenRouter, DeepSeek, Groq, Together, vLLM, свой прокси — каждый со своим
именем, URL, моделью и API-ключом.

В `omni.toml`:

```toml
[custom_providers.openrouter]
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-sonnet-4"
timeout_seconds = 120
api_key_env = "OPENROUTER_API_KEY"

[custom_providers.deepseek]
base_url = "https://api.deepseek.com/v1"
model = "deepseek-chat"
api_key_env = "DEEPSEEK_API_KEY"

[custom_providers.vllm]
base_url = "http://localhost:8000/v1"
model = "qwen2.5-coder"
# api_key_env можно не указывать — для локальных серверов ключ не нужен
```

- `api_key_env` — имя переменной окружения, в которой лежит ключ. Если пустое,
  Authorization-заголовок не отправляется (удобно для локальных серверов).
- Каждый провайдер появляется в TUI: `Ctrl+P` / `/models`; переключение —
  `/model openrouter/anthropic/claude-sonnet-4`.
- `omni models` (и `omni models --json`) показывает кастомные провайдеры вместе
  со встроенными.
- Профили могут переопределять весь набор `custom_providers` целиком.

## Быстрый старт

`omni init` создаёт заготовку `omni.toml` в рабочей папке (все строки закомментированы — раскомментируй нужное, `--force` перезапишет). `omni doctor` показывает активный провайдер, наличие ключей и все настроенные кастом-провайдеры.
