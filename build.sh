#!/bin/bash
set -e

echo "============================================"
echo "  wallet-flow-bot 编译脚本"
echo "============================================"

# check
echo "[*] 正在运行构建前检查"
cargo check
echo "[✓] 库检查通过"

# build release
echo "[*] 正在为Release构建"
cargo build --release
echo "[✓] 构建完成"

# product info
BIN="target/release/wallet-flow-bot"
if [ -f "$BIN" ]; then
    ls -lh "$BIN"
    cp $BIN ./
    echo ""
else
    echo "[✗] 未找到产物"
    exit 1
fi
echo "[✓] 移动到仓库根目录"
# clean
cargo clean
echo "[✓] 清理完成"
