# DoAgent User Guide

[English](USER_GUIDE.md) | [中文](USER_GUIDE.zh-CN.md)

Everything the README doesn't cover: full command reference, config file formats, the command-whitelist workflow, the audit log, and the sandbox model.

---

## 1. Concepts in one minute

- **Workspace** = the directory where you launch `do`. All file tools are confined to it.
- **7 fixed tools** = the AI's entire capability set: `read`, `write`, `edit`, `ls`, `grep`, `runcmd`, `addcmd`.
- **Command whitelist** = fixed command strings approved by you, executed via `runcmd`. There is no free shell.
- **Two config layers** = workspace (`.do/`) overrides global (next to the binary).

## 2. Setup

```bash
do
/setting -g url https://your-openai-compatible-endpoint   # any OpenAI-compatible API
/setting -g key sk-xxxxxxxx
/setting -g model your-model
```

Per project, tell it how to get build feedback:

```
/setting start cargo build        # or: npm run build, go build ./..., make check …
```

The `start` entry appears in the whitelist automatically (see §4).

Bare `/setting` opens an interactive settings page: ↑↓ to select, Enter to edit, Esc to go back. The key is masked (`sk-****xxxx`); each value shows which layer it comes from.

## 3. Slash commands — full reference

| Command | Description |
|---|---|
| `/setting [-g] <field> <value>` | Set `url`/`key`/`model`/`start`/`lang`. `-g` writes the global layer. Bare `/setting` opens the settings page |
| `/lang [zh\|en]` | Switch UI language; bare toggles. Persisted to the global layer |
| `/new` | Clear conversation history. The AI re-reads `HANDOFF.md` itself to continue |
| `/addcmd [-g] <name> <command...>` | Register a fixed command yourself (still confirmed in the approval page). `-g` makes it available in all projects |
| `/allowcmd` | Open the approval page for AI-proposed commands |
| `/deletecmd [name]` | Revoke an approved command (bare = list view) |
| `/quit` | Exit (also `Ctrl+C`) |

## 4. The command whitelist workflow

### AI proposes

The AI calls the built-in `addcmd` tool with `name` (must match `^[a-zA-Z0-9_-]+$`), `command`, `description`, and `mode` (`once` | `daemon`). **Nothing executes.** You get a pending-proposal notice.

### You approve

`/allowcmd` opens the list of pending proposals:

```
↑↓ select · Enter approve · x reject · e edit description · Esc back
```

The command text is read-only — **the string you see is exactly what will ever run**. Approving writes it to `.do/commands.json` (workspace layer). AI proposals can only ever land in the workspace layer; they can never become global.

### The AI uses it

Approved commands are **not** injected into the tool list (that would break prompt caching). The AI calls `runcmd` with no arguments to list what's approved, then `runcmd("<name>")` to execute. `once` commands wait and return the last 20 KB of output; `daemon` commands (dev servers etc.) start in the background and return immediately.

### Files

| File | Layer | Contents |
|---|---|---|
| `.do/commands.json` | workspace | this project's approved commands |
| `do.commands.json` (next to the binary) | global | commands available everywhere |

Name conflicts: the workspace layer wins. The listing shows the source of each entry.

## 5. Config file formats

`.do/config.json` (workspace) and `do.config.json` (global) share one schema:

```json
{
  "url": "https://api.deepseek.com/v1",
  "key": "sk-...",
  "model": "deepseek-chat",
  "start": "cargo build",
  "lang": "zh"
}
```

Effective value = workspace non-empty → global non-empty → built-in default. `start` is workspace-only (it's a project property). `lang` accepts `zh` or `en`.

`.do/commands.json`:

```json
[
  { "name": "build", "command": "cargo build", "description": "编译项目", "mode": "once" },
  { "name": "dev", "command": "npm run dev", "description": "开发服务器", "mode": "daemon" }
]
```

## 6. The audit log

`do.audit.jsonl` (next to the binary), one JSON object per line:

```json
{"ts":1787000000,"ws":"D:\\proj","kind":"input","text":"帮我修一下编译错误"}
{"ts":1787000004,"ws":"D:\\proj","kind":"tool","name":"read","args":"{\"path\":\"src/main.rs\"}","duration_ms":3,"result":"…tail…"}
{"ts":1787000012,"ws":"D:\\proj","kind":"reply","tokens":4821}
```

Why outside the workspace? **So the AI can't forge or erase its own trail** — its tools are jailed inside the workspace root. If the file isn't writable, auditing degrades silently (a notice is shown at startup).

## 7. The sandbox model

Every tool path goes through the workspace guard: lexical normalization (`.`/`..`) → case-insensitive in-root check → realpath of the deepest existing ancestor (symlink resolution) → in-root check again. On Windows, `.DO`, `.do ` and `.do.` are all recognized as `.do`.

`.do/` is invisible, not forbidden:

- `read`/`edit` → the exact "file not found" error, indistinguishable from a truly missing file
- `ls` / `grep` → entries silently omitted
- `write` → plain `Permission denied` (writing to a nonexistent path should succeed, so "not found" would expose the trick)

## 8. Context management

- The system prompt is under 200 tokens. Tool results are truncated at the source (read ≤400 lines, ls ≤200 entries, grep ≤100 matches, commands ≤20 KB tail).
- The AI maintains `HANDOFF.md` in the workspace root (goal / progress / decisions / next step).
- The status bar shows a live token estimate (chars/4). When you judge it's time, `/new` clears history; the AI re-reads `HANDOFF.md` itself. **You decide when to compact.**
- `Esc` cancels the current turn; partial output stays in the conversation.

## 9. Keybindings

| Key | Action |
|---|---|
| `Ctrl+E` | expand/collapse all thinking & tool blocks |
| `↑` `↓` | scroll 3 lines |
| `PageUp` `PageDown` | scroll 10 lines |
| `Tab` | complete slash command |
| `Esc` | cancel current turn (busy) / back (in pages) |
| `Ctrl+C` | quit |

## 10. Uninstall

Delete the folder containing `do` (it holds `do.config.json`, `do.commands.json`, `do.audit.jsonl`). Remove the PATH entry. Per-project `.do/` folders live and die with their projects. Done.
