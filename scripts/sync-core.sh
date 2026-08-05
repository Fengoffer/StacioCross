#!/bin/bash
# 从 Mac 仓同步 stacio_core 快照到 vendor/stacio_core。
#
# 背景：公共仓（Fengoffer/Stacio main）不含 StacioCore/Vendor（vendored ironrdp），
# 无法用 git 依赖消费，故 StacioCross 以 vendor/ 快照自包含。
# 本脚本把 Mac 仓最新 StacioCore 同步进来（读 Mac 仓，不写入）。
#
# 用法: ./scripts/sync-core.sh [源路径，默认 /Users/mac/Documents/Stacio/StacioCore]
set -euo pipefail

SRC="${1:-/Users/mac/Documents/Stacio/StacioCore}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/vendor/stacio_core"

if [ ! -f "$SRC/Cargo.toml" ]; then
  echo "错误: 源不是有效的 stacio_core（$SRC/Cargo.toml 不存在）" >&2
  exit 1
fi

echo "同步 $SRC → $DEST"
rsync -a --delete \
  --exclude target --exclude .git --exclude .DS_Store \
  "$SRC"/ "$DEST"/

echo "完成。同步后检查："
echo "  1. cargo check --workspace（patch / 依赖路径是否仍对得上）"
echo "  2. cargo test -p stacio-core-bridge"
