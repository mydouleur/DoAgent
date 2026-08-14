<div align="center">

```
░████                        ░███████              ░████ 
░██   ░██                    ░██   ░██               ░██ 
░██    ░██                   ░██    ░██  ░███████    ░██ 
░██     ░██                  ░██    ░██ ░██    ░██   ░██ 
░██    ░██                   ░██    ░██ ░██    ░██   ░██ 
░██   ░██                    ░██   ░██  ░██    ░██   ░██ 
░██           ░██████████    ░███████    ░███████    ░██ 
░██                                                  ░██ 
░████                                              ░████ 
```

**An AI coding copilot built on subtraction**

2.5 MB single file · zero dependencies · drop in and go · delete and it's gone

**English** | [中文](README.zh-CN.md)

</div>

---

## Philosophy

> **AI is the copilot. Give programming back to programmers.**

Most AI dev tools keep adding: they take over your shell, your git, your whole workflow.
DoAgent subtracts — it has only 6 tools, never touches shell, never touches git, and does nothing "clever" behind your back.
It helps you write code. **Control stays with you.**

- **Featherweight**: a single static binary, ~2.5 MB, millisecond cold start
- **Runs anywhere**: servers, Docker, edge devices — `scp` it over and it works
- **Truly portable**: the program and its config live in one folder; uninstall = delete the folder, zero residue

## Only 6 tools

| Tool | What it does |
|---|---|
| `read` / `write` / `edit` | read, write, and patch files |
| `ls` / `grep` | list directories, search content |
| `start` | runs the **one command you configured** (build / typecheck) and feeds the output back to the AI |

No bash. The AI's only channel for compiler feedback is `start` — you pick the command, it can only press the button.

## Safety: the workspace is the boundary

- The launch directory is the whole world; no tool can escape it (realpath-level checks — symlinks can't get out either)
- The config dir `.do/` is **completely invisible** to the AI — reads report "file not found", `ls` omits it, `grep` skips it
- Global config sits next to the binary, physically outside the workspace, out of the AI's reach

## Quick start

```bash
# 1. Drop do into a fixed directory (e.g. C:\tools or /usr/local/bin) and add it to PATH
# 2. Launch inside any project directory
do
```

```
/setting -g url https://your-openai-compatible-endpoint
/setting -g key sk-xxxxxxxx
/setting -g model your-model
/setting start cargo build      ← tell it how this project compiles
```

Then just talk: "check the error in src/main.rs".

## Commands & keys

| Input | Action |
|---|---|
| `/setting [-g] <url\|key\|model\|start> <value>` | change config (`-g` writes the global layer) |
| `/new` | new conversation (auto-resumes from HANDOFF.md) |
| `/quit` or `Ctrl+C` | exit |
| `Ctrl+E` | expand/collapse thinking & tool calls |
| `PageUp / PageDown` | scroll history |

## Config: two layers, two jobs

| Layer | Location | Holds |
|---|---|---|
| Workspace (wins) | `project/.do/config.json` | `start` + per-project overrides |
| Global (portable) | `do.config.json` next to the binary | `url` / `key` / `model` |

Set your identity once globally; each project only needs its `start` command.
Need a different key for one project? `/setting key ...` (no `-g`) overrides it there.

## Context: /new is the compaction

No black-box auto-summaries. The AI is instructed to maintain a `HANDOFF.md` (goal / progress / decisions / next step).
Watch the token estimate in the status bar; when you decide it's time, `/new` —
history clears, and the handoff doc becomes the first message of the new conversation. **You decide when to compact.**

## Uninstall

```
Delete the folder that holds do.
```

That's it. No registry, no background services, no surprises hidden in `%APPDATA%`.

## Build it yourself

```bash
cargo build --release   # requires the Rust toolchain
```

Output: `target/release/do` (`do.exe` on Windows).

---

<div align="center">
A tool should be like a good wrench: pick it up, use it, put it down.
</div>
