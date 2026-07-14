# Полезные команды

## Сборка и тесты

```powershell
cargo build
cargo test
```

## Базовые run-команды

```powershell
cargo run -- doctor
cargo run -- run "hello"
cargo run -- --json run "hello"
cargo run -- run "read README.md"
cargo run -- run --allow-write "write notes.txt::hello"
cargo run -- run "status"
cargo run -- run "diff"
cargo run -- run --verify "checks"
```

## Провайдеры

```powershell
# OpenAI
$env:OPENAI_API_KEY = "..."
cargo run -- --provider openai --base-url "https://api.openai.com/v1/" --model "gpt-4.1-mini" run "hello"

# Anthropic
$env:ANTHROPIC_API_KEY = "..."
cargo run -- --provider anthropic --model "claude-sonnet-4-20250514" run "hello"

# Ollama (local)
cargo run -- --provider ollama --model "llama3.1" --base-url "http://localhost:11434" run "hello"

# LM Studio (local)
cargo run -- --provider lm-studio --model "qwen2.5-7b" --base-url "http://localhost:1234/v1" run "hello"

# llama.cpp server (native /completion)
cargo run -- --provider llama-cpp --model "qwen2.5-7b" --base-url "http://localhost:8080" run "hello"
```

## Read-only режимы

```powershell
cargo run -- ask "explain the agent loop"
cargo run -- plan "add OAuth login"
cargo run -- review --verify
```

## Контекст

```powershell
cargo run -- context index
cargo run -- context status
cargo run -- context query "agent loop"
```

## Интроспекция

```powershell
cargo run -- models
cargo run -- tools
cargo run -- --json tools
```

## TUI

```powershell
cargo run -- tui
```
