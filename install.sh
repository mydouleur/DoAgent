#!/bin/sh
# DoAgent 一键安装：识别 OS/架构，拉取最新 Release 对应产物。
# 用法: curl -fsSL https://raw.githubusercontent.com/mydouleur/DoAgent/main/install.sh | sh
#
# 设计要点：
# - GitHub 的 releases/latest/download/<asset> 永远指向最新发布，无需维护版本号
# - Linux 下探测 libssl.so.3：有则给 gnu 版（更小），没有（musl 系/老 OpenSSL）给 musl 静态版
# - macOS 下载后去除 quarantine 属性，免 Gatekeeper 拦截
set -e

REPO="mydouleur/DoAgent"
BASE="https://github.com/$REPO/releases/latest/download"

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)
    case "$ARCH" in
      x86_64)
        # ldconfig 不存在（如 Alpine/musl 系）时 grep 直接失败 → 落到 musl
        if ldconfig -p 2>/dev/null | grep -q 'libssl\.so\.3'; then
          ASSET="do-linux-x86_64"
        else
          ASSET="do-linux-x86_64-musl"
        fi
        ;;
      *) echo "暂不支持的 Linux 架构: $ARCH（欢迎提 issue）" >&2; exit 1 ;;
    esac
    ;;
  Darwin)
    case "$ARCH" in
      arm64)  ASSET="do-macos-aarch64" ;;
      x86_64) ASSET="do-macos-x86_64" ;;
      *) echo "暂不支持的 macOS 架构: $ARCH" >&2; exit 1 ;;
    esac
    ;;
  *)
    echo "不支持的操作系统: $OS（Windows 请从 Releases 页手动下载 do-windows-x86_64.exe）" >&2
    exit 1
    ;;
esac

# 安装目标：/usr/local/bin 优先，无权限退 ~/.local/bin
if [ -w /usr/local/bin ]; then
  DEST="/usr/local/bin/do"
elif [ -d "$HOME/.local/bin" ] || mkdir -p "$HOME/.local/bin"; then
  DEST="$HOME/.local/bin/do"
fi

echo "下载 $ASSET …"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT
curl -fsSL "$BASE/$ASSET" -o "$TMP"
chmod +x "$TMP"

if [ -w "$(dirname "$DEST")" ]; then
  mv "$TMP" "$DEST"
else
  sudo mv "$TMP" "$DEST"
fi

# macOS Gatekeeper：未签名二进制需要去隔离属性
[ "$OS" = "Darwin" ] && xattr -d com.apple.quarantine "$DEST" 2>/dev/null || true

echo "已安装到 $DEST"
echo "下一步：do 启动后执行  /setting -g url <API地址> 、/setting -g key <你的key> 、/setting -g model <模型名>"
