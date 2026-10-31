<div align="center">

# nah 🐎

**A lean, dependency-free coding-agent harness for local LLMs.**

Text-based tool calling · 4K-ready context management · clean terminal UI · std-only Rust

[![CI](https://github.com/canoheil/not-a-harness/actions/workflows/ci.yml/badge.svg)](https://github.com/canoheil/not-a-harness/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/canoheil/not-a-harness)](https://github.com/canoheil/not-a-harness/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

</div>

---

## Overview

`nah` connects to a local language-model server and turns it into a practical coding agent: it reads and writes files, runs commands, and shows every action for approval — through a simple **text tool protocol** instead of JSON function calls.

It is built for locally-run models on modest hardware. The entire program is a single binary **under 1 MB with zero dependencies** (Rust `std` only — no crates, no runtime, no background services), and it is designed to keep its prompts inside a fixed token budget — including 4K context windows.

## Features

| | |
|---|---|
| 🪶 **Tiny footprint** | One static binary under 1 MB. Runs comfortably on low-end machines. |
| 🗣 **Text tool protocol** | Models emit simple fenced `tool` blocks that are parsed by a line-oriented state machine — far more reliable than JSON for small models. |
| 🎯 **Context management that fits** | Pinned task, rolling summary, and a file working set packed into a fixed budget. Token counts are calibrated against what the server actually reports. |
| 🔒 **Safe by default** | Every write shows a diff and asks first; commands and custom tools show the exact resolved command. Paths are sandboxed to the project root. |
| 🧩 **Custom tools** | Wrap domain-specific commands as plain-text tool files; toggle built-ins per workflow. The model only ever sees the tools that are enabled. |
| ⚡ **Polished terminal UX** | Streaming output, per-turn speed and context meters, first-run setup wizard, and a one-shot mode for scripts and CI. |
| 🖥 **Web GUI included** | `nah --gui` opens a local browser interface: chat, inline tool approvals, permission modes, live context meter. |

## Installation

### Prebuilt binaries (recommended)

Copy one line into your terminal — it downloads the latest release and sets up the `nah` command:

**Windows (PowerShell):**
```powershell
irm https://raw.githubusercontent.com/canoheil/not-a-harness/main/install.ps1 | iex
```

**Linux / macOS:**
```sh
curl -fsSL https://raw.githubusercontent.com/canoheil/not-a-harness/main/install.sh | sh
```

The installers put `nah` on your `PATH` so it can be run from any terminal. Restart the terminal once after the first install. To update later, run the same command again.

| Platform | Release asset |
|---|---|
| Windows (x86_64) | `nah-x86_64-pc-windows-msvc.zip` |
| Linux (x86_64) | `nah-x86_64-unknown-linux-gnu.tar.gz` |
| macOS (Apple Silicon) | `nah-aarch64-apple-darwin.tar.gz` |
| macOS (Intel) | `nah-x86_64-apple-darwin.tar.gz` |

All assets are also attached to every [GitHub release](https://github.com/canoheil/not-a-harness/releases) for manual download.

<details>
<summary><b>Uninstall</b></summary>

Windows (PowerShell):
```powershell
Remove-Item -Recurse $env:LOCALAPPDATA\nah
```
Then remove `%LOCALAPPDATA%\nah\bin` from your user `PATH`.

Linux / macOS:
```sh
rm -f ~/.local/bin/nah
```
Then remove the `nah` line added to your shell profile (`.profile` / `.bashrc` / `.zshrc`).

</details>

### From source

```sh
git clone https://github.com/canoheil/not-a-harness
cd not-a-harness
cargo build --release
```

The binary is written to `target/release/nah`. Requires the Rust stable toolchain; there are no other dependencies.

## Quick Start

**1. Configure** (first run starts the setup wizard automatically, or run it explicitly):
```sh
nah --setup
```
The wizard asks for the provider (`ollama` or any OpenAI-compatible server), the server URL, lists the models the server exposes, and lets you pick a default.

**2. Start** in the project you want to work on:
```sh
cd my-project
nah
```

**3. Ask for something:**
```text
> add a notes/todo.md with the next three steps for the release
```

A session looks like this:

```text
       /\  /\
      /  \/  \___
   ~ | •    •   )   nah!
    ~ \  ▽   __/
     ~  \____/
lean local-LLM agent harness - text tool protocol
type /help for commands, /exit to quit

model: local-model | server: http://localhost:11434 | ctx: 4096 tokens

> add a notes/todo.md with the next three steps for the release

  tool: WRITE_FILE path: notes/todo.md
  | + ## Release checklist
  | + - draft the announcement
  | + - tag v1.0.0
  | + - verify the installers
  ? write 4 lines to notes/todo.md? [y/N] y
  | wrote 4 lines (notes/todo.md)

Done - created notes/todo.md with a three-step checklist.

~27 tok/s - 168 tokens - 6.2s
context: ~312/4096 tokens - 4 messages (/compact summarizes)
```

Raw tool blocks are never dumped into the output — the run reads like a transcript of actions, previews, and answers.

## Graphical Interface

`nah` ships with a built-in web GUI:

```sh
nah --gui
```

This starts a small local server and opens the interface in your browser — a dark workspace with chat streaming, inline tool-approval cards (diffs and commands), a permission-mode switcher, and a live context meter. The engine underneath is exactly the one the terminal uses.

- Served on `http://127.0.0.1:8787` (change the port with `--port <n>`).
- Everything runs locally — no external services, no CDNs.
- A native desktop window (Tauri) wrapping the same interface is on the roadmap.

**Permission modes** (the GUI switcher, or `/mode` in the terminal):

| Mode | Behaviour |
|---|---|
| `read-only` | Only `READ_FILE` and `LIST_DIR` are visible and runnable; anything that can change state is refused. |
| `confirm` | *(default)* every write or command asks for approval. |
| `auto` | Actions run without asking (same as `--yes`). |

## Usage

### Command line

```text
Usage:
  nah [options]           interactive REPL
  nah -p "prompt"         one-shot (non-interactive)
  nah --setup             run the setup wizard

Options:
  --model <name>          model to use (picked in the setup wizard)
  --url <url>             server URL (default: http://localhost:11434)
  --provider <kind>       ollama | openai (default: ollama)
  --root <dir>            project root for file access (default: .)
  --ctx <tokens>          context budget / num_ctx (default: 4096)
  --history <n>           trailing messages always kept (default: 5)
  --steps <n>             max model<->tool round trips per turn (default: 8)
  --temperature <f>       sampling temperature (default: 0.2)
  --yes                   auto-approve file writes and command runs
  --no-color              disable colors
  --no-compact            disable automatic history summarization
  --width <n>             override terminal width for text wrapping
  --gui                 start the web GUI instead of the terminal UI
  --port <n>            port for the web GUI (default: 8787)
  --version               print version
  --help                  show this help
```

### REPL commands

| Command | Description |
|---|---|
| `/help` | list all commands |
| `/status` | configuration and live context usage |
| `/model [name]` | show or switch the model (switching starts a fresh conversation) |
| `/models` | list the models available on the server |
| `/provider [url]` | scan for local servers or switch to another one |
| `/setup` | re-run the setup wizard |
| `/ctx [n]` | show or set the token budget |
| `/history [n]` | show or set the trailing-message window |
| `/files` | files currently held in the working set |
| `/tools [on\|off <name>\|reload]` | manage built-in and custom tools |
| `/mode [name]` | permission mode: `read-only` / `confirm` / `auto` |
| `/root [dir]` | show or change the project root |
| `/compact` | summarize older history now |
| `/reset` | start a fresh conversation |
| `/clear` | clear the screen |
| `/yes` / `/no` | toggle auto-approval for writes and commands |
| `/exit` | quit (`Ctrl+C` also exits cleanly) |

## Tool protocol

Instead of JSON schemas, models write tool calls as plain fenced blocks. Each tool has a fixed, minimal syntax:

````text
```tool
READ_FILE
path: src/main.rs
```

```tool
WRITE_FILE
path: notes/todo.md
content:
the new file contents go here
END_OF_CONTENT
```

```tool
EDIT_FILE
path: src/main.rs
<<<<<<< SEARCH
let timeout = 30;
=======
let timeout = 120;
>>>>>>> REPLACE
```

```tool
RUN_COMMAND
command: cargo test
```
````

**Built-in tools:**

| Tool | Purpose |
|---|---|
| `READ_FILE` | read a file (optional offset/limit, size-capped) |
| `WRITE_FILE` | create or overwrite a file (contents delimited by `END_OF_CONTENT`) |
| `EDIT_FILE` | exact-text replacement (search text must match exactly once) |
| `LIST_DIR` | list a directory |
| `RUN_COMMAND` | run a shell command in the project root (sandboxed, timeout-capped) |
| `DELETE_FILE` | delete a file (with confirmation and preview) |
| `MOVE_FILE` | move or rename a file (never overwrites) |

**Rules the harness enforces:**

- Invalid or incomplete blocks are **never executed** — they fall back to plain text and the model is nudged to retry.
- Results return to the model as short plain-text reports (exit codes, previews, diffs).
- Repeated identical calls are first warned about, then blocked, so a looping model cannot spin forever.
- Every tool result is size-capped (`max_tool_bytes`, default 32 KB).
- The prompt documents **only the enabled tools** — disable what a project does not need and the model stops seeing it.

## Custom tools

Anything domain-specific — wrapping a scanner, a test runner, a deploy script — is added as a plain-text file in `~/.config/nah/tools/*.tool` (Windows: `%USERPROFILE%\.config\nah\tools\`):

```text
name: NMAP_SCAN
description: Scan a host with nmap; returns open ports and services.
params:
  target: host or IP to scan
  ports: port range like 1-1000
command: nmap -sV -p {ports} {target}
timeout: 120
```

- `name` — uppercase letters, digits, underscores; must not collide with built-ins.
- `description` — one short line; the model uses it to decide when to call the tool.
- `params` — indented `name: help` lines. **Every parameter is required**: if the model omits one, the call is refused and the error lists exactly what is missing so the model retries.
- `command` — runs in the project root (`cmd /C` on Windows, `sh -c` elsewhere). `{param}` placeholders are substituted with the model's values. `command.windows:` / `command.unix:` override it per platform.
- `timeout` — seconds, 1–300 (default 30).

Custom tools execute like `RUN_COMMAND`: output is captured and size-capped, and **every call goes through the confirmation dialog showing the fully resolved command** — you approve exactly what will run. Manage them with `/tools off <NAME>` and `/tools reload`.

Broken tool files are skipped with a warning and never break startup. Project-local tool files are intentionally **not** supported: a repository must never be able to plant tools in your setup.

## Context Management

Every request is assembled within a fixed token budget (`--ctx`, default 4096):

1. **System prompt + tool documentation** — compact and always sent.
2. **Rolling summary** — older conversation is folded into a short factual summary when history grows (compaction runs at temperature 0).
3. **Pinned task** — the original request never leaves the context.
4. **Working set** — contents of recently used files, newest first, bounded to ~80% of the budget.
5. **Trailing history** — the last N messages verbatim (`--history`, default 5).

Token accounting is layered: a lightweight static estimator (non-ASCII text is weighted realistically) is continuously **calibrated against the server's own reported counts** (Ollama `prompt_eval_count`, OpenAI-compatible `usage`), so the budget tracks the real tokenizer of the loaded model — and resets when you switch models. The `context: ~N/M tokens` line after every turn shows where you stand.

## Safety

- **Confirmation by default** — file writes and edits show a diff and ask first (`--yes` or `/yes` to auto-approve).
- **Sandboxed paths** — `..`, absolute paths, and symlink escapes outside the project root are rejected.
- **Command transparency** — command runs and custom tools confirm with the exact resolved command; destructive-looking commands are flagged.
- **No silent failures** — invalid tool blocks never execute; errors are reported to the model with retry hints.
- **Loop breaker** — repeated identical tool calls are warned about, then blocked.
- **Size caps** — tool output is truncated at `max_tool_bytes`.

## Configuration

Settings are resolved in this order: command-line flags → environment → `./nah.conf` (per-project) → `~/.config/nah/nah.conf` (user).

Environment variables: `NAH_CONFIG` (config path), `NAH_MODEL`, `NAH_URL`, `NAH_PROVIDER`. The installers additionally honor `NAH_DOWNLOAD_BASE` and `NAH_INSTALL_DIR`.

Example `nah.conf` (`KEY = VALUE`):

```ini
provider = ollama
url = http://localhost:11434
model = my-local-model
ctx_tokens = 4096
history_messages = 5
max_open_files = 3
max_tool_bytes = 32768
max_steps = 8
temperature = 0.2
auto_compact = true
disabled_tools = MOVE_FILE
```

## Supported Backends

| Provider kind | Endpoint | Typical servers |
|---|---|---|
| `ollama` | `/api/chat` (streaming NDJSON) | [Ollama](https://ollama.com) |
| `openai` | `/v1/chat/completions` (SSE) | llama.cpp server, LM Studio, vLLM, and other OpenAI-compatible servers |

On first run (and via `/provider`), `nah` scans common local addresses and offers whatever servers it finds.

## Development

```sh
cargo build --release   # optimized binary in target/release/
cargo test              # unit tests (core + cli)
cargo fmt && cargo clippy -- -D warnings
```

| Path | Contents |
|---|---|
| `crates/core` | engine: tool protocol parser, tool execution and registry, context manager, providers, configuration |
| `crates/cli` | REPL, terminal UI, setup wizard |
| `scripts/mock_ollama.py` | mock server used by the end-to-end tests |
| `install.sh` / `install.ps1` | the one-command installers |

Principles this project is built on:

- **Zero dependencies** — Rust `std` only; contributions must not add crates to `Cargo.toml`.
- **Small-model first** — the prompt, the tool protocol, and the error messages are designed for local models of any size.
- **Prove it in CI** — every change is built and tested on Windows, Linux, and macOS, plus an end-to-end run against a mock server.

## Releases

Pushing a `v*` tag builds release binaries for all supported targets (Windows x86_64, Linux x86_64, macOS x86_64 and arm64) and attaches them to the GitHub release — both versioned archives and the stable file names the installers use.

## Roadmap

- Native desktop window (Tauri) wrapping the web GUI
- Global permission modes — read-only / confirm / auto — with quick switching
- Project-local custom tools behind an explicit opt-in flag

## License

MIT — see [LICENSE](LICENSE).
