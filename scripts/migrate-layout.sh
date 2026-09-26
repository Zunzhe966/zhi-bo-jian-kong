#!/bin/bash
# =============================================================================
# 尊者直播监控 —— 产物目录「布局修复」脚本
#
# 布局约定（唯一正确形态）：
#   <产物根>/<主播名>/直播/<日期 场次>/…
#   <产物根>/<博主名>/短视频/<标题>/…
#   <产物根>/<博主名>/批量下载/<标题>/…
#   即：**主播名在最上层，分类（直播/短视频/批量下载）在其下一级**。
#   一个主播/博主的东西都在他自己的文件夹里——这是用户明确的诉求。
#
# 本脚本做两件事（都可单独关闭）：
#   1.（可选，仅当显式传 --rename-root）把产物根目录改名为「尊者直播监控」，
#      并同步 config.ini 的 [路径设置] 产物根目录。
#   2.（默认执行）修复布局：盘上若残留「分类在最上层」的历史目录
#      （<产物根>/直播/<主播>、/<产物根>/短视频/<主播>、/<产物根>/批量下载/<主播>），
#      一律搬回「<主播>/<分类>」的正确形态。
#
# 默认只打印将要执行的操作（dry-run）。确认无误后加 --go 才真正执行。
# 所有移动都是同卷 rename（mv），可逆。
#
# 用法：
#   bash scripts/migrate-layout.sh                      # 只预览
#   bash scripts/migrate-layout.sh --go                 # 修复布局
#   bash scripts/migrate-layout.sh --go --rename-root   # 同时改名根目录
#   bash scripts/migrate-layout.sh --go --root /自定义/产物根
#
# 前置：请先退出「尊者直播监控」App（让 daemon 停掉），避免迁移过程中 daemon
#       往半迁移的目录里写新文件。
# =============================================================================
set -e

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="$ROOT/config/config.ini"
DRY=1
RENAME_ROOT=0
ROOT_DIR=""

for a in "$@"; do
  case "$a" in
    --go) DRY=0 ;;
    --rename-root) RENAME_ROOT=1 ;;
    --root) shift; ROOT_DIR="$1" ;;
  esac
done

# ---- 解析当前产物根目录（config.ini 的 产物根目录；拿不到则用默认） ----
if [ -z "$ROOT_DIR" ] && [ -f "$CONFIG" ]; then
  ROOT_DIR="$(grep -E '^[[:space:]]*产物根目录[[:space:]]*=' "$CONFIG" | head -1 | sed -E 's/^[^=]*=[[:space:]]*//' | sed -E 's/[[:space:]]*$//')"
fi
if [ -z "$ROOT_DIR" ]; then
  ROOT_DIR="/Volumes/A区/尊者产物"
fi
ROOT_DIR="$(echo "$ROOT_DIR" | sed -E 's/^["'\'']//; s/["'\'']$//')"

echo "============================================================================"
echo "产物根目录 : $ROOT_DIR"
echo "配置文件   : $CONFIG"
echo "模式       : $( [ "$DRY" = 0 ] && echo '执行(--go)' || echo '预览(dry-run)' )"
echo "根目录改名 : $( [ "$RENAME_ROOT" = 1 ] && echo '是' || echo '否' )"
echo "============================================================================"

if [ ! -d "$ROOT_DIR" ]; then
  echo "[错误] 产物根目录不存在：$ROOT_DIR" >&2
  exit 1
fi

# ---- 1. （可选）根目录改名 尊者产物 -> 尊者直播监控 ----
if [ "$RENAME_ROOT" = 1 ]; then
  NEW_ROOT="$(dirname "$ROOT_DIR")/尊者直播监控"
  if [ "$ROOT_DIR" = "$NEW_ROOT" ]; then
    echo "[1/2] 根目录已是「尊者直播监控」，跳过改名"
  elif [ -e "$NEW_ROOT" ]; then
    echo "[错误] 目标路径已存在：$NEW_ROOT（请先确认/清理）" >&2
    exit 1
  else
    if [ "$DRY" = 0 ]; then
      echo "[1/2] 改名：$ROOT_DIR -> $NEW_ROOT"
      mv "$ROOT_DIR" "$NEW_ROOT"
      if [ -f "$CONFIG" ]; then
        python3 - "$CONFIG" "$NEW_ROOT" <<'PY'
import sys
p, new = sys.argv[1], sys.argv[2]
s = open(p, encoding='utf-8').read()
lines, changed = [], False
for ln in s.splitlines(keepends=True):
    if ln.lstrip().startswith('产物根目录'):
        lines.append('产物根目录 = %s\n' % new); changed = True
    else:
        lines.append(ln)
open(p, 'w', encoding='utf-8').write(''.join(lines))
print('       已更新 config.ini' if changed else '       未找到可替换行')
PY
      fi
      ROOT_DIR="$NEW_ROOT"
    else
      echo "[1/2] 预览改名：$ROOT_DIR -> $NEW_ROOT"
    fi
  fi
else
  echo "[1/2] 根目录改名：未启用（加 --rename-root 才会做）"
fi

# ---- 2. 修复布局：分类在最上层 -> 搬回 主播/分类 ----
echo "[2/2] 修复布局：把「分类/主播」搬回「主播/分类」"

# 已知系统目录，不当成主播目录处理
SYS_DIRS="database fupan_output logs covers run node_modules .git downloads"

is_sys() {
  local base="$1"
  for s in $SYS_DIRS; do [ "$base" = "$s" ] && return 0; done
  return 1
}

# 把 $src 下每个条目移到 $dst/（避免「目标已存在时 mv 嵌套」），移空后删除 $src
move_children() {
  local src="$1" dst="$2"
  [ -d "$src" ] || return
  if [ "$DRY" = 0 ]; then
    mkdir -p "$dst"
    for item in "$src"/*; do
      [ -e "$item" ] || continue
      mv "$item" "$dst"/
    done
    rmdir "$src" 2>/dev/null || true
  else
    echo "       $src/*  ->  $dst/"
  fi
}

for cat in 直播 短视频 批量下载; do
  cat_dir="$ROOT_DIR/$cat"
  [ -d "$cat_dir" ] || continue
  for d in "$cat_dir"/*; do
    [ -d "$d" ] || continue
    base="$(basename "$d")"
    is_sys "$base" && continue
    # <根>/<分类>/<主播>  ->  <根>/<主播>/<分类>
    move_children "$d" "$ROOT_DIR/$base/$cat"
  done
  if [ "$DRY" = 0 ]; then
    rmdir "$cat_dir" 2>/dev/null || true
  fi
done

if [ "$DRY" = 0 ]; then
  echo ""
  echo "✅ 布局修复完成。建议重启「尊者直播监控」App 让 daemon 重新读取配置。"
else
  echo ""
  echo "（以上是预览，未改动任何文件。确认无误后执行：bash scripts/migrate-layout.sh --go）"
fi
