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

**3 MB，SSH 即插即用。**
**只改代码，绝不越权执行。**
**你审查，你运行，你掌控。**

一款做减法的 AI 编程工具：
零依赖，开箱即用；一键删除，干干净净。

[English](README.md) | **中文** · 📖 [User Guide](docs/USER_GUIDE.md) | [使用文档](docs/USER_GUIDE.zh-CN.md)

</div>

---

## 理念

> **AI 只提方案，程序员做决定。把程序员的还给程序员。**

市面上 AI 开发工具在做加法：接管 shell、接管 git、接管你的整个工作流。
DoAgent 做减法——固定 7 个工具，没有自由 shell，不碰 git，不做任何"自作聪明"的事。
写代码它帮忙，**掌控权永远在你手里**。

- **超轻量**：单文件二进制，约 2–3 MB，冷启动毫秒级
- **随处可用**：服务器、Docker、边缘设备——`scp` 上去就能跑
- **绿色便携**：程序和配置同住一个文件夹，卸载 = 删文件夹，零残留

## 安装

Linux / macOS：

```bash
curl -fsSL https://raw.githubusercontent.com/mydouleur/DoAgent/main/install.sh | sh
```

脚本自动识别系统/架构（Linux 上会探测 OpenSSL 3，没有就给 musl 静态通用版），拉取最新 Release，装到 `/usr/local/bin` 或 `~/.local/bin`。

Windows：从 [Releases](https://github.com/mydouleur/DoAgent/releases) 下载 `do-windows-x86_64.exe`，放进 PATH 里的任意目录。

## 快速开始

```powershell
cd 你的项目
do
```

```
/setting -g url https://你的OpenAI兼容API地址
/setting -g key sk-xxxxxxxx
/setting -g model 模型名
/setting start cargo build      ← 这个项目的编译命令（工作区层）
```

然后直接说话就行："帮我看看 src/main.rs 的错误"。

## 固定 7 个工具，一个不多

| 工具 | 干什么 |
|---|---|
| `read` / `write` / `edit` | 读写改文件 |
| `ls` / `grep` | 看目录、搜内容 |
| `runcmd` | 列出并执行**你批准的**固定命令 |
| `addcmd` | AI 提案新命令——你批准前它什么都不能跑 |

没有 bash。AI 唯一的执行通道是你亲手批准的白名单：

- AI 用 `addcmd` 提案 → 你 `/allowcmd` 审批（列表视图，Enter 批准）
- 自己注册：`/addcmd <name> <命令>`（加 `-g` 全项目生效）
- 随时撤销：`/deletecmd`
- 批准列表在 `.do/commands.json`——这个目录 AI 连看都看不见

## 安全设计：工作区即边界

- 启动时所在的目录就是全部世界，任何工具无法跳出（realpath 级校验，symlink 也出不去）
- 配置目录 `.do/` 对 AI **完全隐形**——读写报"文件不存在"，ls 不显示，grep 跳过
- 全局配置在 exe 旁边，物理上在工作区之外，AI 够不到

## 审计：一举一动，皆有记录

所有动作追加到 exe 旁的 `do.audit.jsonl`——你的输入、每轮 AI 回复、每次工具调用（名称、参数、耗时、结果尾部）。
它放在**工作区之外**，AI 无法伪造也无法擦除自己的轨迹。纯 JSONL 文本，任何编辑器打开即读。

## 命令与快捷键

| 输入 | 作用 |
|---|---|
| `/setting [-g] <url\|key\|model\|start\|lang> <值>` | 改配置（带 `-g` 写全局）；裸 `/setting` 打开设置页 |
| `/lang [zh\|en]` | 切换界面语言（裸敲轮换） |
| `/new` | 新对话（AI 自行读取 HANDOFF.md 续接） |
| `/addcmd <name> <命令>` | 自助注册白名单命令 |
| `/allowcmd` / `/deletecmd [name]` | 审批 AI 提案 / 撤销 |
| `Esc` | 取消当前轮 |
| `Ctrl+E` | 展开/折叠思考过程与工具调用 |
| `↑↓` / `PageUp / PageDown` | 滚动（细粒度 / 翻页） |
| `Ctrl+C` 或 `/quit` | 退出 |

## 配置：分层，各管各的

| 层 | 位置 | 存什么 |
|---|---|---|
| 工作区（优先） | `项目\.do\config.json` + `.do\commands.json` | `start`、本项目覆盖项、项目命令 |
| 全局（便携） | `do.exe` 旁的 `do.config.json` + `do.commands.json` | `url` / `key` / `model` / `lang`、跨项目命令 |

## 上下文管理：/new 即压缩

没有黑盒的自动总结。AI 被要求随时维护一份 `HANDOFF.md`（目标/进展/决策/下一步），
你看着状态栏的 token 估算，觉得该换上下文了，`/new` 一下——
历史清空，AI 自己会读回交接文档续接。**何时压缩，你说了算。**

## 平台产物与 TLS

| 产物 | TLS |
|---|---|
| `do-windows-x86_64.exe` | Schannel（系统） |
| `do-macos-aarch64` / `do-macos-x86_64` | Security.framework（系统） |
| `do-linux-x86_64` | 系统 OpenSSL 3 |
| `do-linux-x86_64-musl` | 内置 rustls——**通用版，任何 Linux 都能跑** |

macOS 首次运行：`xattr -d com.apple.quarantine do`（install.sh 已自动处理）。

## 卸载

```
删掉放 do.exe 的那个文件夹。
```

就这一件事。没有注册表，没有后台服务，没有藏在 `%APPDATA%` 里的惊喜。

## 自己构建

```bash
cargo build --release   # 需要 Rust 工具链
```

产物：`target/release/do`（Windows 下 `do.exe`）。

---

<div align="center">
工具应该像一把好扳手：拿起就用，放下就走。
</div>
