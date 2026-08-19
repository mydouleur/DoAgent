# AGENTS.md — 给协作者（人类或 AI）的项目约定

## 这是什么

DoAgent：极简 TUI AI 助手。理念——**AI 只提方案，程序员做决定。把程序员的还给程序员。**不写代码以外的任何事，不碰 shell。

本文档是唯一权威规格（task.md 已完成历史使命，归档于此并删除）。

## 构建与验证

```bash
cargo build --release     # 产物 target/release/do.exe
cargo test                # 必须全绿
cargo clippy --all-targets  # 必须零警告
```

体积基准：Windows 1.95 MB（native-tls 分叉后）。不允许明显上涨。

## 结构

- `crates/core`（lib）：workspace（工作区守卫）/ tools（7 工具）/ api（SSE）/ agent（loop）/ config（双层配置）/ commands（命令白名单）/ audit（审计日志）
- `crates/do`（bin）：main + tui/（mod 主循环与对话流 / pages 页面渲染 / forms 页面交互 / lang i18n / md Markdown 渲染），只通过 core 的最小公共 API 交互

## 硬性约定

1. **`.do/` 对 AI 隐形**：read/edit/ls 返回与"不存在"逐字相同的错误；write 返回 os error 5；ls/grep 跳过。不得暴露其存在。`config.json` 与命令白名单 `commands.json` 同属 `.do/` 隐形范围
2. **工作区 = 启动 cwd**：所有工具路径必须过 `workspace.resolve`（词法归一 + 大小写归一 + realpath 最深祖先），禁止直接拼路径
3. **依赖克制**：加依赖前先问"std 能不能做"
4. **tools 与 system prompt 冻结**：工具列表固定 7 个（read/write/edit/ls/grep/addcmd/runcmd），批准的白名单命令一律走 `runcmd` 发现式调用，**不得**把动态内容注入 tools 数组或 system prompt——prompt 缓存按 system+tools+messages 前缀匹配，前缀一动全量 miss
5. **注释是教材**：读者是"会 C# 的 Rust 小白"——模块导读 + 语法点对比 C# 概念，讲"为什么这么写"，不逐行翻译
6. 改动涉及规格（工具行为、配置层、沙盒语义）时，同步更新本文档

## 核心规格速查

### 工具（固定 7 个）

read（≤400 行）/ write / edit（精确替换，多处未 all 则报错）/ ls（≤200 条）/ grep（regex-lite，≤100 匹配）/ addcmd（AI 提案固定命令：name 限 `^[a-zA-Z0-9_-]+$`、command、description、mode=once|daemon）/ runcmd（无参列白名单、带名执行；once 等结束取尾 20 KB，daemon 后台即返）

### 命令白名单

- AI `addcmd` 提案 → 人类 `/allowcmd` 列表页审批（Enter 批 / x 拒 / e 编辑描述）→ 落盘 `.do/commands.json`
- 人类自助注册 `/addcmd [-g] <name> <命令>`（仍过审批页确认；`-g` 写全局层 `do.commands.json`）
- **AI 提案恒落工作区层**——AI 不能获得跨项目生效的命令
- `/deletecmd [name]` 撤销；重名工作区层赢，列表带来源标注
- 执行统一 shell 包装（Windows `cmd /c` / Unix `sh -c`），审批常量零拼接无注入面，cwd 固定工作区根

### 配置（双层，覆盖合并）

工作区 `.do/config.json`（优先）> exe 旁 `do.config.json` > 内置默认。全局层存 url/key/model/lang（"人"的身份）。`/setting [-g] <url|key|model> <值>`；裸 `/setting` 开设置页（显示合并生效值 + 来源标注，key 掩码）。`/lang [zh|en]` 切换语言（lang 只由 /lang 写入，/setting 拒绝该字段）。

### 上下文

- system prompt < 200 token（含维护 HANDOFF.md 义务）
- 工具结果源头截断；`/new` 只清历史——AI 按 prompt 要求新对话开始自行 read HANDOFF.md 续接（不注入，省 token）
- 状态栏 token 估算（chars/4），何时压缩人说了算
- Esc 取消当前轮（Arc<AtomicBool> 检查点：SSE 每 chunk + 每次工具执行前）

### 审计

`do.audit.jsonl`（exe 旁，工作区外 = AI 无法伪造/擦除）：input / reply / tool 三类事件。写失败静默降级。选 JSONL 不选 SQLite 是体积账（+0 KB vs +1.5 MB）。

### 沙盒

- `.do/` 隐形（见约定 1）；write 例外返回 os error 5（写入不存在路径本应成功，报"不存在"会自相矛盾）
- realpath + 大小写归一后比较（Windows：`.DO`、`.do `、`.do.` 都是 `.do`）；symlink 逃逸被 realpath 天然挡住
- 固有残余（接受）：命令执行时读取的项目文件是 AI 可写的——"让 AI 写代码"的固有属性，不归沙盒管

### TLS 与平台产物

win（Schannel）/ mac（Security.framework）/ linux-gnu（系统 OpenSSL 3）用 native-tls；linux-musl 保留 rustls（通用兜底，任何 Linux 可跑，体积最大是合理的）。`do --check-net <url>` 供 CI 冒烟。

## 演进记录（仅存档）

- **v0.1**：初始实现——workspace 拆分、6 工具、`.do` 隐形、SSE 累加器、双层配置、splash、/setting 页
- **v0.2 批次**：Markdown 全量渲染（pulldown-cmark，仅 do crate）；思考流式可见/正文开始自动折叠；工具调用函数样式 `read("src/main.rs")`；Esc 取消；参数校验失败回填模型自愈
- **命令白名单**：addcmd 提案 + /allowcmd 审批 + /deletecmd 撤销 + 人类 /addcmd 自助注册；发现式注入（tools 冻结，批准零缓存代价）；start 独立工具弃用，后彻底删除（config 字段、隐式条目、设置页行全清，构建命令走 `/addcmd build ...` 平权注册）
- **体验批次**：/setting 合并视图修复；splash 仅按键进入；工具状态字 doing（黄，点号动画）/done（绿）/已取消（红）；ToolStart 提前到流式中段（name 首现即宣告）；TLS 平台分叉（Windows 2.78→1.95 MB）；macOS 入 CI 矩阵；tui.rs 拆 mod/pages/forms
- **全局命令层 + JSONL 审计**：见上文规格
- **i18n + 紧凑布局 + /lang**：lang.rs 静态表（编译器强制双语齐全）；md 渲染三层策略——段落/列表/引用零空行，空行只在标题/代码块/分隔线边界且幂等去重；修复标题样式栈泄漏
- **实验否决记录**：主流 agent（Kimi Code/OpenCode）流式中段不显示任何工具信息——DoAgent 的提前宣告已领先，空括号宽容提取不做

## 技术选型存档（仅留档）

- Zig 实验：~0.7 MB 可期但 0.x std 每周变 API，维护成本过高——弃
- .NET 实验：AOT hello 0.91 MB，+HttpClient/JSON 后 4.4 MB；Terminal.Gui +16 MB 且不可裁剪——弃
