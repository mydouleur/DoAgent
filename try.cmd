@echo off
rem 一键试玩：构建 do 后在 test\ 沙盒目录里启动它（工作区=test\，而不是仓库根）。
rem 为什么不用 cargo run：cargo 只能在 Cargo.toml 所在目录运行，没法替子进程换工作目录。
cargo build -p do || exit /b 1
if not exist test mkdir test
cd test
..\target\debug\do.exe
cd ..
