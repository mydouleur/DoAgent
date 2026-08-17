# DoAgent — 极简 TUI AI 助手

理念：AI 只提方案，程序员做决定。把程序员的还给程序员。不写代码以外的任何事，不碰 shell。

## 技术栈

- **Rust**，单文件静态二进制；上一版已实测 **2.56 MB**（tokio+reqwest 栈），本版沿用同一栈，体积以此为基准
- 结构：cargo workspace 两 crate——`core`（lib：workspace/tools/api/agent/config）+ `do`（bin：main/tui）。`lto=fat` + `codegen-units=1` 下 crate 边界不影响体积
- 依赖 10 个：`tokio`(rt/macros/process)、`reqwest`(default-features=false, stream+rustls-tls/ring)、`serde`、`serde_json`、`eventsource-stream`、`futures-util`、`ratatui`、`crossterm`、`regex-lite`、`ignore`；另允许 `unicode-width`（CJK 折行）
- release profile：`opt-level="z"`, `lto="fat"`, `codegen-units=1`, `strip=true`, `panic="abort"`（需 panic hook 恢复终端）
- API 调用不走 SDK：reqwest 直连 OpenAI 兼容 API + SSE 流式；tool_calls 增量累加器（按 index 归堆拼接 arguments）自己写，写一次
- **注释教学要求**：读者是"会 C# 的 Rust 小白"。每个文件顶部写模块导读；Rust 特有语法点（所有权/借用、match、Option/Result、`?`、impl/trait、闭包、生命周期、迭代器、Arc/mpsc、RAII 等）出现时用简短注释解释，**尽量对比 C# 等价概念**（如 "`?` ≈ C# 里 catch 后 return null 的结构化写法"）。密度控制：讲语法点，不逐行翻译代码

## 工具（仅 6 个）

| 工具 | 说明 |
|---|---|
| read | 读文件 |
| write | 写文件（整文件） |
| edit | 精确替换 |
| ls | 列目录 |
| grep | 内容搜索 |
| start | 执行配置的那一条命令（构建 / 类型检查 / 启动），返回输出。命令存于 `.do/`，AI 不可见 |

无 bash。AI 拿编译反馈的唯一途径是 start。

## 配置与 slash 命令

- **两层配置，覆盖合并**：工作区 `.do/config.json`（项目级，字段优先）> exe 旁 `do.config.json`（全局便携层，跟着 do.exe 走，删文件夹即完全卸载）> 内置默认值
- 全局层只存三项：`url`、`key`、`model`（"人"的身份）；工作区层额外存 `start`（项目级，不进全局层）
- `/setting <字段> <值>` 写工作区层（如 `/setting model gpt-4o`）；`/setting -g <字段> <值>` 写全局层（如 `/setting -g key sk-xxx`）；`-g start` 拒绝并提示 start 是项目级
- 未设置 start 时，start 工具返回提示"请让使用者用 /setting start <命令> 设置"
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
                                                         

## 待思考调整（记录）

1. ~~**TUI 的 Markdown 渲染**~~ ✅ 已完成：全量解析器 pulldown-cmark（仅 crates/do），只渲染 AI 回复主体，思考内容保持纯文本；流式中间态自然降级；+194 KB（2.56 → 2.75 MB）
2. ~~**思考内容（reasoning）不可见**~~ ✅ 已完成：流式期间思考块实时展开（灰色），正文开始输出即自动折叠为 `思考 (+N 字)`；Ctrl+E 全局展开/折叠不变
3. ~~**logo 位置/展示方式**~~ ✅ 已完成：改为全屏 splash——logo 居中 + 版本号 + 配置状态（key/model/工作区），任意键或 ~1.2s 自动进入对话，不强制等待
4. ~~**/setting 独立界面**~~ ✅ 已完成：裸 `/setting` 进入设置页（全局 url/key/model + 工作区 start 两区，↑↓ 选择、Enter 编辑、Esc 返回/放弃），key 掩码 `sk-****xxxx`；带参数的单行命令形式保留
5. ~~**工具调用显示样式**~~ ✅ 已完成：折叠态改函数调用样式 `read("src/main.rs")`、`grep("foo", "src/")`（解析失败兜底 `name()`）；展开态显示完整 args + 结果
6. ~~**手动取消机制**~~ ✅ 已完成：Esc → `Cmd::Cancel` 置位 `Arc<AtomicBool>`（未引 tokio-util），SSE 每 chunk 与每次工具执行前检查；半截内容保留，Done 后补"（已取消）"；平时 Esc 不响应
7. ~~**内建工具参数校验**~~ ✅ 已完成：`agent.rs` 派发前手写 required/type 检查（`check_call`），缺参/类型错/非法 JSON 不执行工具、错误文案作为 tool 结果回填模型自愈，静默降级 `{}` 已移除
8. **固定命令白名单 `/addc` `/deletec`（替代/扩展 start，触及"不碰 shell"理念，需先拍板）**：AI 可提案注册**固定命令**（如 `npm run dev`，零参数、不开参数面），人类审批后才生效；批准列表落盘 `.do/commands.json`（对 AI 隐形规则同 `.do/` 其余文件），即为 AI 可执行 shell 能力的完整边界。设计要点：①审批覆盖的字符串 = 永远执行的全部内容，零注入面；②长跑命令（dev server 类）走 start 的后台语义，add 时需声明一次性/常驻二态；③提案须含合法工具名（`^[a-zA-Z0-9_-]+$`）和 description，审批界面允许人类改名再批；④持久批准、`/deletec` 撤销，不做逐次审批（防审批疲劳）；⑤固定 cwd 为工作区根。前置依赖：先做 6、7，再给 AI 开放命令提案
