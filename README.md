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

**3 MB. Any SSH session. Edits anything, runs nothing.**
**You review, you execute, you decide.**

An AI coding tool built on subtraction — zero dependencies, drop in and go, delete and it's gone

**English** | [中文](README.zh-CN.md)

</div>

---

## Philosophy

> **AI proposes, you dispose. Give programming back to programmers.**

Most AI dev tools keep adding: they take over your shell, your git, your whole workflow.
DoAgent subtracts — it has only 6 tools, never touches shell, never touches git, and does nothing "clever" behind your back.
It helps you write code. **Control stays with you.**

- **Featherweight**: a single static binary, ~3 MB, millisecond cold start
- **Runs anywhere**: servers, Docker, edge devices — `scp` it over and it works
- **Truly portable**: the program and its config live in one folder; uninstall = delete the folder, zero residue

## Only 6 built-in tools

| Tool | What it does |
|---|---|
| `read` / `write` / `edit` | read, write, and patch files |
| `ls` / `grep` | list directories, search content |
| `addcmd` / `runcmd` | propose / list & run whitelisted commands (see below) |

No free-form shell.

**Command whitelist**: the AI can *propose* fixed commands with `addcmd` (e.g. `npm run dev`), but nothing executes until you approve them with `/addcmd`. Approved commands live in `.do/commands.json`; the AI discovers and invokes them through the `runcmd` tool (`runcmd()` lists, `runcmd("name")` runs) — the approved string is all that ever runs, no parameters, always in the workspace root. Your configured `start` command shows up as an implicit whitelist entry. `/deletecmd` revokes.

## Safety: the workspace is the boundary

- The launch directory is the whole world; no tool can escape it (realpath-level checks — symlinks can't get out either)
- The config dir `.do/` is **completely invisible** to the AI — reads report "file not found", `ls` omits it, `grep` skips it
- Global config sits next to the binary, physically outside the workspace, out of the AI's reach

## Quick start

```bash
# 1. Drop do into a fixed directory (e.g. C:\tools or /usr/local/bin) and add it to PATH
# 2. Launch inside any project directory
do
# macOS Gatekeeper (first run): xattr -d com.apple.quarantine do
```

**Downloads & TLS**: Windows uses Schannel, macOS uses Security.framework, and regular Linux (gnu) uses the system OpenSSL — pick `do-linux-x86_64-musl` as the universal Linux build (statically linked rustls; runs on Alpine/distroless/old distros with no system TLS at all).

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
| `/setting [-g] <url\|key\|model\|start\|lang> <value>` | change config (`-g` writes the global layer; `lang` = `zh`\|`en`) |
| `/new` | new conversation (AI re-reads HANDOFF.md itself) |
| `/addcmd <name 命令>` | self-register a whitelisted command (confirms in the approval page) |
| `/allowcmd` / `/deletecmd [name]` | approve AI proposals / revoke whitelisted commands |
| `/lang [zh\|en]` | switch UI language (bare = toggle; saved to the global layer) |
| `/quit` or `Ctrl+C` | exit |
| `Ctrl+E` | expand/collapse thinking & tool calls |
| `PageUp / PageDown` | scroll history |

## Config: two layers, two jobs

| Layer | Location | Holds |
|---|---|---|
| Workspace (wins) | `project/.do/config.json` | `start` + per-project overrides |
| Global (portable) | `do.config.json` next to the binary | `url` / `key` / `model` |

The command whitelist is two-layered the same way: `do.commands.json` next to the binary (global, `/addcmd -g`) plus `project/.do/commands.json` (workspace wins on name clashes). `runcmd` lists both with a source tag. AI proposals always land in the workspace layer — the AI can never grant itself a cross-project command.

## Audit

Every session appends to `do.audit.jsonl` next to the binary — one JSON record per line: user inputs, tool calls (name, args, duration, result tail), and per-round token estimates. It lives **outside** the workspace on purpose: the AI's tools are locked to the workspace root, so it cannot forge or erase its own track record. If the file isn't writable the audit silently disables itself (with a one-line notice at startup).

Set your identity once globally; each project only needs its `start` command.
Need a different key for one project? `/setting key ...` (no `-g`) overrides it there.

## Context: /new is the compaction

No black-box auto-summaries. The AI is instructed to maintain a `HANDOFF.md` (goal / progress / decisions / next step).
Watch the token estimate in the status bar; when you decide it's time, `/new` —
history clears, and the AI re-reads the handoff doc on its own. **You decide when to compact.**

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
