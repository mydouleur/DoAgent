#!/bin/sh
# DoAgent 卸载：删除二进制与全部伴生文件（配置/白名单/审计日志）。
# 用法: curl -fsSL https://raw.githubusercontent.com/mydouleur/DoAgent/main/uninstall.sh | sh
#
# 安装从不修改 PATH/shell 配置，所以卸载 = 纯删文件，无环境残留。
set -e

FOUND=0
for dir in /usr/local/bin "$HOME/.local/bin"; do
  if [ -f "$dir/do" ]; then
    echo "删除 $dir 下的 do 及伴生文件…"
    rm -f "$dir/do" "$dir/do.config.json" "$dir/do.commands.json" "$dir/do.audit.jsonl"
    FOUND=1
  fi
done

if [ "$FOUND" = "0" ]; then
  echo "未在 /usr/local/bin 或 ~/.local/bin 找到 do" >&2
  exit 1
fi
echo "卸载完成（各项目目录下的 .do/ 随项目自行处置）"
