#!/bin/bash
# 把项目从 ~/Desktop 迁到非 TCC 保护目录，恢复 launchd 24×30 自愈能力。
#
# 用法（在项目根目录执行）：
#   bash scripts/migrate-from-desktop.sh
#
# 默认迁到 ~/Projects/尊者监控直播短视频；可通过环境变量覆盖：
#   DEST="$HOME/Documents/尊者监控直播短视频" bash scripts/migrate-from-desktop.sh
#
# 脚本会：
#   1. 停止并确认 Rust daemon 已退出（避免复制运行中数据库）
#   2. 整目录复制（保留 .git、配置、下载、复盘产物）
#   3. 重写迁移副本里的数据库绝对路径（封面缓存等）
#   4. 生成可执行的 setup-macos 脚本并打印后续步骤
#   5. 只在用户显式调用 `--commit` 时删除桌面原目录
set -euo pipefail

if [ "$(uname)" != "Darwin" ]; then
  echo "此脚本仅用于 macOS 桌面 TCC 迁移" >&2
  exit 1
fi

SRC="$(cd "$(dirname "$0")/.." && pwd)"
case "$SRC" in
  "$HOME/Desktop/"*) ;;
  *)
    echo "当前项目不在 ~/Desktop（$SRC），无需迁移" >&2
    exit 1
    ;;
esac

DEST="${DEST:-$HOME/Projects/尊者监控直播短视频}"
if [ "$DEST" = "$SRC" ]; then
  echo "目标目录不能等于源目录" >&2
  exit 1
fi
case "$DEST" in
  "$HOME/Desktop/"*)
    echo "目标目录不能仍在 ~/Desktop：$DEST" >&2
    exit 1
    ;;
esac

echo "[migrate] 源目录：$SRC"
echo "[migrate] 目标目录：$DEST"

RUST_BIN="$SRC/zunzhe-core/target/release/zunzhe-core"
if [ ! -x "$RUST_BIN" ]; then
  RUST_BIN="$SRC/zunzhe-core/target/debug/zunzhe-core"
fi
if [ -x "$RUST_BIN" ]; then
  echo "[migrate] 停止 Rust daemon..."
  "$RUST_BIN" stop >/dev/null 2>&1 || true
  sleep 2
  if "$RUST_BIN" doctor 2>/dev/null | grep -q '"rust_daemon":true'; then
    echo "[migrate] daemon 仍在运行，请先手动停止后再迁移" >&2
    exit 1
  fi
fi

mkdir -p "$(dirname "$DEST")"
if [ -e "$DEST" ] && [ ! -d "$DEST" ]; then
  echo "[migrate] 目标路径存在但不是目录：$DEST" >&2
  exit 1
fi

# rsync 排除可重建的大目录；target 可达 18GB，重建成本远低于复制。
echo "[migrate] 复制项目（排除 target、node_modules、.venv、临时文件）..."
RSYNC_ARGS=(-a --delete
  --exclude='zunzhe-core/target/'
  --exclude='desktop/node_modules/'
  --exclude='.venv/'
  --exclude='*.pyc' --exclude='__pycache__/'
  --exclude='.DS_Store' --exclude='logs/zunzhe-core.uptime.log.*')
rsync "${RSYNC_ARGS[@]}" "$SRC/" "$DEST/"

DB="$DEST/zunzhe-core.db"
if [ -f "$DB" ] && command -v sqlite3 >/dev/null 2>&1; then
  echo "[migrate] 修正数据库中的旧桌面绝对路径..."
  # 只替换仍指向旧根目录的封面缓存路径；网络 URL 不动。
  sqlite3 "$DB" "UPDATE videos SET cover_cache = replace(cover_cache, '$SRC', '$DEST') WHERE cover_cache LIKE '$SRC/%';"
  sqlite3 "$DB" "PRAGMA wal_checkpoint(TRUNCATE);" >/dev/null 2>&1 || true
fi

cp scripts/migrate-after-copy.sh "$DEST/scripts/migrate-after-copy.sh"
chmod +x "$DEST/scripts/migrate-after-copy.sh"

echo
echo "[migrate] 复制完成。请在新目录执行："
echo "  cd \"$DEST\""
echo "  bash scripts/migrate-after-copy.sh"
echo
echo "确认新目录 daemon 正常后，再用 --commit 删除桌面旧目录："
echo "  bash scripts/migrate-from-desktop.sh --commit"

if [ "${1:-}" = "--commit" ]; then
  echo
  echo "[migrate] 校验目标目录存在项目配置后删除桌面旧目录..."
  test -f "$DEST/config/config.ini" || { echo "目标目录缺少 config/config.ini，拒绝删除" >&2; exit 1; }
  NEW_BIN="$DEST/zunzhe-core/target/release/zunzhe-core"
  if [ ! -x "$NEW_BIN" ] || ! "$NEW_BIN" doctor 2>/dev/null | grep -q '"rust_daemon":true'; then
    echo "[migrate] 新目录 daemon 未运行/未健康，拒绝删除旧目录" >&2
    echo "[migrate] 请先在新目录执行 bash scripts/migrate-after-copy.sh 并确认 doctor 正常" >&2
    exit 1
  fi
  rm -rf "$SRC"
  echo "[migrate] 已删除 $SRC"
fi
