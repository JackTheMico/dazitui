#!/usr/bin/env bash
set -e

REPO="JackTheMico/dazitui"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$ARCH" in
    x86_64|amd64)
        ARCH_TAG="x86_64"
        ;;
    aarch64|arm64)
        ARCH_TAG="aarch64"
        ;;
    *)
        echo "不支持的 CPU 架构: $ARCH" >&2
        exit 1
        ;;
esac

case "$OS" in
    linux)
        TARGET="${ARCH_TAG}-unknown-linux-gnu"
        ;;
    darwin)
        TARGET="${ARCH_TAG}-apple-darwin"
        ;;
    *)
        echo "当前一键脚本仅支持 Linux 与 macOS，Windows 请在 Releases 页面下载 .zip 包" >&2
        exit 1
        ;;
esac

echo "🔍 正在获取 dazitui 最新版本..."
LATEST_TAG=$(curl -s "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/')
if [ -z "$LATEST_TAG" ]; then
    LATEST_TAG="v1.0.0"
fi

FILENAME="dazitui-${LATEST_TAG}-${TARGET}.tar.gz"
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${LATEST_TAG}/${FILENAME}"

echo "📦 正在下载 ${FILENAME} (${LATEST_TAG})..."
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

curl -fsSL "$DOWNLOAD_URL" -o "$TMP_DIR/$FILENAME"
tar -xzf "$TMP_DIR/$FILENAME" -C "$TMP_DIR"

BIN_SRC=$(find "$TMP_DIR" -type f -name "dazitui" | head -n 1)
if [ -z "$BIN_SRC" ]; then
    echo "❌ 解压后未找到 dazitui 可执行文件" >&2
    exit 1
fi
chmod +x "$BIN_SRC"

INSTALL_DIR="/usr/local/bin"
if [ ! -w "$INSTALL_DIR" ]; then
    INSTALL_DIR="$HOME/.local/bin"
    mkdir -p "$INSTALL_DIR"
fi

echo "🚀 正在安装到 ${INSTALL_DIR}/dazitui ..."
if [ -w "$INSTALL_DIR" ]; then
    cp "$BIN_SRC" "$INSTALL_DIR/dazitui"
else
    sudo cp "$BIN_SRC" "$INSTALL_DIR/dazitui"
fi

echo "✅ dazitui ${LATEST_TAG} 安装成功！直接在终端输入 dazitui 即可启动。"
