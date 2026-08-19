# DoAgent 使用文档

[English](USER_GUIDE.md) | [中文](USER_GUIDE.zh-CN.md)

README 没讲透的都在这里：完整命令参考、配置文件格式、命令白名单工作流、审计日志、沙盒模型。

---

## 1. 一分钟概念

- **工作区** = 启动 `do` 时所在的目录。所有文件工具都被关在里面。
- **固定 7 工具** = AI 的全部能力：`read`、`write`、`edit`、`ls`、`grep`、`runcmd`、`addcmd`。
- **命令白名单** = 你批准过的固定命令字符串，经 `runcmd` 执行。没有自由 shell。
- **双层配置** = 工作区（`.do/`）覆盖全局（exe 旁）。

## 2. 初始配置

```bash
do
/setting -g url https://你的OpenAI兼容API地址   # 任何 OpenAI 兼容 API 均可
/setting -g key sk-xxxxxxxx
/setting -g model 模型名
```

每个项目告诉它怎么拿编译反馈：

```
/setting start cargo build        # 或 npm run build、go build ./...、make check …
```

`start` 会自动出现在白名单里（见第 4 节）。

裸 `/setting` 打开交互式设置页：↑↓ 选择、Enter 编辑、Esc 返回。key 显示掩码（`sk-****xxxx`），每个值标注来自哪一层。

## 3. slash 命令全表

| 命令 | 说明 |
|---|---|
| `/setting [-g] <字段> <值>` | 设置 `url`/`key`/`model`/`start`/`lang`；`-g` 写全局层；裸命令打开设置页 |
| `/lang [zh\|en]` | 切换界面语言，裸敲轮换；持久化到全局层 |
| `/new` | 清空对话历史；AI 会自行读取 `HANDOFF.md` 续接 |
| `/addcmd [-g] <name> <命令...>` | 自助注册固定命令（仍会过审批页确认）；`-g` 全项目生效 |
| `/allowcmd` | 打开 AI 提案的审批页 |
| `/deletecmd [name]` | 撤销已批准命令（裸敲进列表页） |
| `/quit` | 退出（或 `Ctrl+C`） |

## 4. 命令白名单工作流

### AI 提案

AI 调用内建工具 `addcmd`，参数为 `name`（须匹配 `^[a-zA-Z0-9_-]+$`）、`command`、`description`、`mode`（`once` 一次性 | `daemon` 常驻）。**此时什么都不执行**，你会收到待批提示。

### 你审批

`/allowcmd` 打开待批列表：

```
↑↓ 选择 · Enter 批准 · x 拒绝 · e 编辑描述 · Esc 返回
```

命令全文只读——**你看到的字符串就是永远会执行的全部内容**。批准后写入 `.do/commands.json`（工作区层）。AI 提案只能进工作区层，永远进不了全局层。

### AI 使用

批准的命令**不会**注入工具列表（那会击穿 prompt 缓存）。AI 用无参 `runcmd` 列出已批准命令，再 `runcmd("<名字>")` 执行。`once` 等待结束并返回尾部 20 KB 输出；`daemon`（dev server 类）后台启动立即返回。

### 文件

| 文件 | 层 | 内容 |
|---|---|---|
| `.do/commands.json` | 工作区 | 本项目批准的命令 |
| `do.commands.json`（exe 旁） | 全局 | 所有项目可用的命令 |

重名时工作区层赢；列表里每条标注来源。

## 5. 配置文件格式

`.do/config.json`（工作区）与 `do.config.json`（全局）同构：

```json
{
  "url": "https://api.deepseek.com/v1",
  "key": "sk-...",
  "model": "deepseek-chat",
  "start": "cargo build",
  "lang": "zh"
}
```

生效值 = 工作区非空 → 全局非空 → 内置默认。`start` 只属于工作区层（它是项目属性）；`lang` 接受 `zh` / `en`。

`.do/commands.json`：

```json
[
  { "name": "build", "command": "cargo build", "description": "编译项目", "mode": "once" },
  { "name": "dev", "command": "npm run dev", "description": "开发服务器", "mode": "daemon" }
]
```

## 6. 审计日志

`do.audit.jsonl`（exe 旁），每行一条 JSON：

```json
{"ts":1787000000,"ws":"D:\\proj","kind":"input","text":"帮我修一下编译错误"}
{"ts":1787000004,"ws":"D:\\proj","kind":"tool","name":"read","args":"{\"path\":\"src/main.rs\"}","duration_ms":3,"result":"…尾部…"}
{"ts":1787000012,"ws":"D:\\proj","kind":"reply","tokens":4821}
```

为什么放工作区外？**让 AI 无法伪造或擦除自己的轨迹**——它的工具被锁在工作区根内。文件不可写时审计静默降级（启动时给一条提示）。

## 7. 沙盒模型

所有工具路径过工作区守卫：词法归一（`.`/`..`）→ 大小写不敏感的根内判断 → realpath 最深已存在祖先（解析 symlink）→ 再判根内。Windows 下 `.DO`、`.do `、`.do.` 都识别为 `.do`。

`.do/` 是**隐形**而非拒绝：

- `read`/`edit` → 与文件不存在逐字相同的错误
- `ls` / `grep` → 静默跳过
- `write` → 普通 `Permission denied`（写入不存在的路径本应成功，报"不存在"反而会暴露）

## 8. 上下文管理

- system prompt 不到 200 token；工具结果源头截断（read ≤400 行、ls ≤200 条、grep ≤100 匹配、命令 ≤20 KB 尾部）
- AI 被要求在工作区根维护 `HANDOFF.md`（目标/进展/决策/下一步）
- 状态栏实时显示 token 估算（chars/4）。你觉得该换了就 `/new`——历史清空，AI 自行读回 HANDOFF.md 续接。**何时压缩，你说了算**
- `Esc` 取消当前轮；半截内容保留在对话流

## 9. 快捷键

| 按键 | 作用 |
|---|---|
| `Ctrl+E` | 展开/折叠全部思考与工具块 |
| `↑` `↓` | 滚动 3 行 |
| `PageUp` `PageDown` | 滚动 10 行 |
| `Tab` | 补全 slash 命令 |
| `Esc` | 取消当前轮（忙时）/ 返回（页面中） |
| `Ctrl+C` | 退出 |

## 10. 卸载

删掉放 `do` 的文件夹（里面有 `do.config.json`、`do.commands.json`、`do.audit.jsonl`），移除 PATH 条目。各项目的 `.do/` 随项目同生共死。完。
