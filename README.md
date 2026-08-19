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

**English** | [中文](README.zh-CN.md) · 📖 [User Guide](docs/USER_GUIDE.md) | [使用文档](docs/USER_GUIDE.zh-CN.md)

</div>

---

## Philosophy

> **AI proposes, you dispose. Give programming back to programmers.**

Most AI dev tools keep adding: they take over your shell, your git, your whole workflow.
DoAgent subtracts — a fixed set of 7 tools, no free shell, no git, nothing "clever" behind your back.
It helps you write code. **Control stays with you.**

- **Featherweight**: a single static binary, ~2–3 MB, millisecond cold start
- **Runs anywhere**: servers, Docker, edge devices — one curl line and it's installed
- **Truly portable**: the program and its config live in one folder; uninstall = delete the folder, zero residue

## Install

Linux / macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/mydouleur/DoAgent/main/install.sh | sh
```

The script detects your OS/arch (and on Linux picks the musl static build unless your system has OpenSSL 3), downloads the latest release, and installs to `/usr/local/bin` or `~/.local/bin`.

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/mydouleur/DoAgent/main/install.ps1 | iex
```

Installs to `%LOCALAPPDATA%\Programs\do` and adds it to your user PATH (no admin needed). Or download `do-windows-x86_64.exe` from [Releases](https://github.com/mydouleur/DoAgent/releases) manually.

## Quick start

```bash
cd your-project
do
```

```
/setting -g url https://your-openai-compatible-endpoint
/setting -g key sk-xxxxxxxx
/setting -g model your-model
/addcmd build cargo build      ← this project's build command (approve in the review page)
```

Then just talk: "check the error in src/main.rs".

## 7 fixed tools — nothing more

| Tool | What it does |
|---|---|
| `read` / `write` / `edit` | read, write, and patch files |
| `ls` / `grep` | list directories, search content |
| `runcmd` | list and run **approved** fixed commands |
| `addcmd` | AI proposes a new fixed command — nothing runs until you approve it |

No bash. The AI's only execution channel is the whitelist **you** approved:

- AI proposes via `addcmd` → you review with `/allowcmd` (list view, Enter to approve)
- Self-register with `/addcmd <name> <command>` (add `-g` for all projects)
- Revoke anytime with `/deletecmd`
- Approvals live in `.do/commands.json` — a directory the AI cannot even see

## Safety: the workspace is the boundary

- The launch directory is the whole world; no tool can escape it (realpath-level checks — symlinks can't get out either)
- The config dir `.do/` is **completely invisible** to the AI — reads report "file not found", `ls` omits it, `grep` skips it
- Global config sits next to the binary, physically outside the workspace, out of the AI's reach

## Audit: every action, on record

Everything is appended to `do.audit.jsonl` next to the binary — your inputs, each AI reply, every tool call (name, args, duration, result tail).
It lives **outside the workspace**, so the AI can neither forge nor erase its own trail. Plain JSONL: open it in any editor.

## Commands & keys

| Input | Action |
|---|---|
| `/setting [-g] <url\|key\|model> <value>` | change config (`-g` writes the global layer); bare `/setting` opens the settings page |
| `/lang [zh\|en]` | switch UI language (bare toggles) |
| `/new` | new conversation (AI re-reads HANDOFF.md itself) |
| `/addcmd <name> <command>` | self-register a whitelisted command |
| `/allowcmd` / `/deletecmd [name]` | approve AI proposals / revoke |
| `Esc` | cancel the current turn |
| `Ctrl+E` | expand/collapse thinking & tool calls |
| `↑↓` / `PageUp / PageDown` | scroll (fine / coarse) |
| `Ctrl+C` or `/quit` | exit |

## Config: layers, each with one job

| Layer | Location | Holds |
|---|---|---|
| Workspace (wins) | `project/.do/config.json` + `.do/commands.json` | per-project overrides, project commands |
| Global (portable) | `do.config.json` + `do.commands.json` next to the binary | `url` / `key` / `model` / `lang`, cross-project commands |

## Context: /new is the compaction

No black-box auto-summaries. The AI maintains a `HANDOFF.md` (goal / progress / decisions / next step).
Watch the token estimate in the status bar; when you decide it's time, `/new` —
history clears, and the AI re-reads the handoff doc on its own. **You decide when to compact.**

## Platform builds & TLS

| Asset | TLS |
|---|---|
| `do-windows-x86_64.exe` | Schannel (system) |
| `do-macos-aarch64` / `do-macos-x86_64` | Security.framework (system) |
| `do-linux-x86_64` | system OpenSSL 3 |
| `do-linux-x86_64-musl` | bundled rustls — **the universal build, runs on any Linux** |

macOS first run: `xattr -d com.apple.quarantine do` (the install script does this for you).

## Uninstall

Linux / macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/mydouleur/DoAgent/main/uninstall.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/mydouleur/DoAgent/main/uninstall.ps1 | iex
```

Both delete the binary plus its sidecar files (config / whitelist / audit log) and, on Windows, remove the PATH entry. Nothing else exists — no registry, no services, no surprises in `%APPDATA%`.

## Build it yourself

```bash
cargo build --release   # requires the Rust toolchain
```

Output: `target/release/do` (`do.exe` on Windows).

---

<div align="center">
A tool should be like a good wrench: pick it up, use it, put it down.
</div>
