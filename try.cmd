@echo off
rem One-click trial: build do, then launch it inside test\ as the workspace
rem (not the repo root). cargo run can't set the child process's cwd, hence this script.
cargo build -p do || exit /b 1
if not exist test mkdir test
cd test
..\target\debug\do.exe
cd ..
