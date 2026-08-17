# AGENTS.md — 给协作者（人类或 AI）的项目约定

## 这是什么

DoAgent：极简 TUI AI 副驾驶。完整需求规格见根目录 **task.md**（权威，先读它）。

## 构建与验证

```bash
cargo build --release     # 产物 target/release/do.exe（基准 2.56 MB，不许明显涨）
cargo test                # 必须全绿
cargo clippy --all-targets  # 必须零警告
```

## 结构

- `crates/core`（lib）：workspace（工作区守卫）/ tools（7 工具）/ api（SSE）/ agent（loop）/ config（双层配置）/ commands（命令白名单）
- `crates/do`（bin）：main + tui（ratatui），只通过 core 的最小公共 API 交互

## 硬性约定

1. **`.do/` 对 AI 隐形**：read/edit/ls 返回与"不存在"逐字相同的错误；write 返回 os error 5；ls/grep 跳过。不得暴露其存在。`config.json` 与命令白名单 `commands.json` 同属 `.do/` 隐形范围
2. **工作区 = 启动 cwd**：所有工具路径必须过 `workspace.resolve`（词法归一 + 大小写归一 + realpath 最深祖先），禁止直接拼路径
3. **依赖克制**：加依赖前先问"std 能不能做"；体积基准 2.56 MB
4. **注释是教材**：读者是"会 C# 的 Rust 小白"——模块导读 + 语法点对比 C# 概念，讲"为什么这么写"，不逐行翻译
5. 改动涉及规格（工具行为、配置层、沙盒语义）时，同步更新 task.md
