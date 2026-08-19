# DoAgent — 极简 TUI AI 助手

理念：AI 只提方案，程序员做决定。把程序员的还给程序员。不写代码以外的任何事，不碰 shell。

## 技术栈

- **Rust**，单文件静态二进制；上一版已实测 **2.56 MB**（tokio+reqwest 栈），本版沿用同一栈，体积以此为基准
- 结构：cargo workspace 两 crate——`core`（lib：workspace/tools/api/agent/config）+ `do`（bin：main/tui）。`lto=fat` + `codegen-units=1` 下 crate 边界不影响体积
- 依赖 10 个：`tokio`(rt/macros/process)、`reqwest`(default-features=false, stream+rustls-tls/ring)、`serde`、`serde_json`、`eventsource-stream`、`futures-util`、`ratatui`、`crossterm`、`regex-lite`、`ignore`；另允许 `unicode-width`（CJK 折行）
- release profile：`opt-level="z"`, `lto="fat"`, `codegen-units=1`, `strip=true`, `panic="abort"`（需 panic hook 恢复终端）
- API 调用不走 SDK：reqwest 直连 OpenAI 兼容 API + SSE 流式；tool_calls 增量累加器（按 index 归堆拼接 arguments）自己写，写一次
- **注释教学要求**：读者是"会 C# 的 Rust 小白"。每个文件顶部写模块导读；Rust 特有语法点（所有权/借用、match、Option/Result、`?`、impl/trait、闭包、生命周期、迭代器、Arc/mpsc、RAII 等）出现时用简短注释解释，**尽量对比 C# 等价概念**（如 "`?` ≈ C# 里 catch 后 return null 的结构化写法"）。密度控制：讲语法点，不逐行翻译代码

## 工具（固定 7 个，数组冻结）

| 工具 | 说明 |
|---|---|
| read | 读文件 |
| write | 写文件（整文件） |
| edit | 精确替换 |
| ls | 列目录 |
| grep | 内容搜索 |
| addcmd | 提案固定命令（name/command/description/mode），人类 `/addcmd` 审批后生效 |
| runcmd | 发现式调用：无参列出白名单，带 name 执行。命令存于 `.do/commands.json`，AI 不可见 |

无自由 shell。`/setting start` 配的命令作为隐式条目并入白名单视图（不落盘），AI 用 `runcmd` 拿编译反馈。

tools 数组永远冻结为这 7 个：prompt 缓存按 system + tools + messages 前缀匹配，批准命令不改变前缀（零缓存代价），白名单内容走 messages（本来逐轮增长，无额外 miss）。

## 配置与 slash 命令

- **两层配置，覆盖合并**：工作区 `.do/config.json`（项目级，字段优先）> exe 旁 `do.config.json`（全局便携层，跟着 do.exe 走，删文件夹即完全卸载）> 内置默认值
- 全局层只存三项：`url`、`key`、`model`（"人"的身份）；工作区层额外存 `start`（项目级，不进全局层）
- `/setting <字段> <值>` 写工作区层（如 `/setting model gpt-4o`）；`/setting -g <字段> <值>` 写全局层（如 `/setting -g key sk-xxx`）；`-g start` 拒绝并提示 start 是项目级
- 未设置 start 时，白名单视图无 start 条目，`runcmd` 调 `start` 报"未知命令"并附当前名单
- 全局层对 AI **物理不可达**（在工作区之外）；`.do/` 对 AI **代码级隐形**：read/edit 访问 `.do` 内路径时，返回与"路径不存在"完全相同的反馈（不得暴露"被拒绝"）；ls 不列出它；grep 直接跳过
- **唯一例外是 write**：对不存在的路径写入本应成功，装成"不存在"会自相矛盾。因此对 `.do` 内的写入返回普通的 I/O 失败（`Permission denied`，与操作系统权限拒绝一致）——AI 只能看出"这路径写不进去"，看不出这里藏着配置

## 工作区

- 启动时 cwd 即工作区根，所有工具禁止跳出
- 实现：所有路径先 `realpath` 解析，再校验是否在根内（必须 realpath，防 symlink 逃逸；不要只做字符串前缀判断）

## 上下文

- system prompt < 200 token，一句话说清角色 + 工具纪律 + 维护 `HANDOFF.md` 的义务
- 注意：tool schema 才是 token 大头，控制工具描述的长度
- 上下文管理就两条，不做 LLM 自动总结、不做占位化：
  1. 源头截断：所有工具结果返回时掐断（read 限行、grep 限匹配、start 限字节）
  2. `/new` 即压缩：system prompt 要求 AI 随时在工作区根维护 `HANDOFF.md`（交接文档：当前目标、进展、关键决策、下一步）；`/new` 清空消息历史，并自动把 `HANDOFF.md` 内容作为新对话的第一条用户消息注入 → 上下文天然连带
- 底部状态栏实时显示上下文 token 估算（chars/4 粗估即可）——程序员要对自己的模型有了解，看着数字自己决定何时 /new

## 沙盒

- `.do/` 对 AI **隐形**（不是拒绝）：read/edit 报"文件不存在"，ls 不显示，grep 跳过；**write 是唯一例外**——写入不存在路径本应成功，故返回普通 `Permission denied`，行为像一个碰巧不可写的目录，不暴露 `.do` 的存在
- 校验必须在 realpath 之后、且大小写规范化之后比较（Windows 大小写不敏感：`.DO`、`.do ` 尾部空格都是同一目录）
- 注意 AI 可在工作区建 symlink 指向 `.do/` 再顺着读——realpath 校验天然挡住
- 残余（固有、接受）：命令执行时读取的项目文件是 AI 可写的，AI 理论上能写出恶意源码被编译/运行——这是"让 AI 写代码"本身的固有属性，不归沙盒管

## 技术选型存档（Zig/.NET 实验结论，仅留档）

- Rust 第一版已实测跑通：2.56 MB、16 测试全绿——本版沿用同一依赖栈
- Zig 实验：~0.7 MB 可期但 0.x std 每周变 API，维护成本过高——弃
- .NET 实验：AOT hello 0.91 MB，+HttpClient/JSON 后 4.4 MB；Terminal.Gui +16 MB 且不可裁剪——弃，回 Rust

## 任务清单

1. 项目初始化：cargo workspace（根 Cargo.toml 放 workspace + release profile + workspace.dependencies），`crates/core`（lib）+ `crates/do`（bin）
2. `core/workspace.rs`：根目录锁定 + realpath 校验 + `.do/` 隐形化（与不存在一致的行为，含大小写规范化）
3. `core/tools.rs`：5 个文件工具（read/write/edit/ls/grep），全部过 workspace 校验
4. `core/agent.rs` + `core/api.rs`：agent loop + SSE 流式解析（含 tool_calls 增量累加器）+ 工具结果源头截断；core 只暴露最小公共 API 给 TUI
5. `core/config.rs` + `start` 工具：读 `.do/config.json`，spawn 执行 start 命令，截断返回 stdout/stderr
6. `do` 的 TUI（ratatui）：
   - 启动展示 task.md 末尾的 ASCII logo（逐字内嵌）
   - 对话流；思考过程（reasoning）与工具调用可折叠显示，不同颜色区分（思考灰、工具青、正文默认色），折叠块可展开/收起，布局随终端宽度自适应拉伸
   - `/setting` 修改 url/key/model/start（写 `.do/config.json`）；`/new` 清历史并注入 `HANDOFF.md`
   - 底部状态栏：上下文 token 估算 + 当前模型
7. 压缩 system prompt 到 200 token 内（含维护 HANDOFF.md 的指令）
8. 教学级注释贯穿全代码（见技术栈）；build + test 全绿后实测 do.exe 体积

░████                        ░███████              ░████ 
░██   ░██                    ░██   ░██               ░██ 
░██    ░██                   ░██    ░██  ░███████    ░██ 
░██     ░██                  ░██    ░██ ░██    ░██   ░██ 
░██    ░██                   ░██    ░██ ░██    ░██   ░██ 
░██   ░██                    ░██   ░██  ░██    ░██   ░██ 
░██           ░██████████    ░███████    ░███████    ░██ 
░██                                                  ░██ 
░████                                              ░████ 
                                                         

## 演进记录（已完成的批次，仅存档）

- **v0.2 批次（待思考 1-7）**：Markdown 全量渲染（pulldown-cmark，+194 KB）；思考流式可见/正文开始自动折叠；启动 splash；/setting 独立设置页（key 掩码）；工具调用函数样式 `read("src/main.rs")`；Esc 取消；参数校验失败回填模型自愈
- **待思考 8（命令白名单）**：AI 用 `addcmd` 工具提案、人类 `/allowcmd` 审批（列表视图）/ `/deletecmd` 撤销、人类也可 `/addcmd <name> <命令>` 自助注册；批准落盘 `.do/commands.json`（对 AI 隐形）；**发现式注入**——tools 数组冻结为固定 7 个（read/write/edit/ls/grep/addcmd/runcmd），白名单经 `runcmd` 列出与执行，批准动作零缓存代价；start 独立工具弃用（config.start 并入白名单视图）
- **待办 1-6 批次**：①/setting 页显示 bug 修复——设置页改显示合并生效值并标注来源层（工作区/全局/默认），保存仍按"写哪层只写哪层"；②start 残留清理（tools/lib/config 文档与截断说明统一为 runcmd）；③splash 自动跳过修复——select! 条件定时分支改 tick 心跳（crossterm poll 100ms + 截止时间判断，纯函数可测；后改为 splash 仅按键进入，tick 只驱动 doing 动画帧）；④工具调用即时显示——新增 `Evt::ToolStart`，派发即插入进行中块，结果到来按序更新，取消时遗留块标"（已取消）"；状态字 doing（黄、500ms 点号动画）/ done（绿）/ 已取消（红）分色；⑤TLS 平台分叉——Windows/macOS/gnu 用 native-tls（Schannel/Security.framework/系统 OpenSSL），musl 保留 rustls，新增 `do --check-net <url>` 冒烟入口，CI 每个构建 job 跑 HTTPS 冒烟；Windows 体积 2.78 → 1.95 MB；⑥CI 矩阵加 macOS（aarch64 + x86_64），README 补 Gatekeeper 与各平台 TLS 来源说明，tui.rs 拆为 tui/{mod,pages,forms}（纯结构搬迁）

## 待办（之后处理，现在不动手）

（空）