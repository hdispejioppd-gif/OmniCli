# ⌨ Keybindings & Commands

Press **F1** inside the TUI at any time to see this reference as an overlay.

## Prompt

| Key | Action |
| --- | --- |
| `Enter` | Send prompt / confirm |
| `Alt+Enter` | Insert newline |
| `Up` / `Down` | Browse prompt history |
| `Ctrl+Left` / `Ctrl+Right` | Move cursor by word |
| `Ctrl+Backspace` | Delete previous word |
| `Home` / `End` | Jump to line start / end |
| `Tab` | Insert tab |

## Navigation & views

| Key | Action |
| --- | --- |
| `PgUp` / `PgDn` | Scroll transcript |
| Mouse wheel | Scroll transcript |
| `Ctrl+L` | Browse saved sessions |
| `Ctrl+N` | New session |
| `Ctrl+P` | Model picker |
| `Ctrl+W` | Workflow dashboard |
| `Ctrl+T` | Supervisor dashboard |
| `F1` | Toggle keyboard help |

## Control

| Key | Action |
| --- | --- |
| `Ctrl+S` | Send prompt |
| `Esc` | Cancel run / clear input / quit |
| `Ctrl+Q` / `Ctrl+C` | Quit |

## Slash commands

Type `/` in the prompt to open the live command palette.

| Command | Action |
| --- | --- |
| `/models` | Open the model picker |
| `/model <selector>` | Switch the active model |
| `/workflows` | Open the workflow dashboard |
| `/workflow run <path>` | Start a workflow |
| `/workflow resume <run-id>` | Resume a paused workflow |
| `/supervisors` | Open the supervisor dashboard |
| `/supervisor run <path>` | Run a supervisor task file |

## Добавлено в v6

| Клавиша | Действие |
| --- | --- |
| `Ctrl+A` / `Ctrl+E` | В начало / конец строки |
| `Ctrl+U` | Очистить промпт |
| `Ctrl+K` | Удалить до конца строки |

Новые команды: `/help` (`/keys`) — справка по клавишам, `/export` — экспорт сессии в markdown.
