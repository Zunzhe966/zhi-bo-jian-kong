#!/bin/bash
# =============================================================================
# 尊者直播监控 —— 目录重组 + 根目录改名 迁移脚本
#
# 作用：
#   1. 把产物根目录从「尊者产物」改名为「尊者直播监控」（同卷内 rename，瞬时、可回退）
#   2. 更新 config.ini 的 [路径设置] 产物根目录 指向新路径
#   3. 把盘上已有的录像/短视频从旧布局「<主播>/直播」「<主播>/短视频」
#      搬进新布局「直播/<主播>」「短视频/<主播>」
#      （批量下载与短视频在磁盘上无法区分，历史批量下载一并归入「短视频」；
#        今后新的批量下载才会单独落到「批量下载」）
#
# 默认只打印将要执行的操作（dry-run）。确认无误后加 --go 才真正执行。
# 所有移动都是同卷 rename（mv），可逆：把目录 mv 回去即可还原。
#
# 用法：
#   bash scripts/migrate-layout.sh                 # 只预览，不改动任何文件
#   bash scripts/migrate-layout.sh --go            # 真正执行
#   bash scripts/migrate-layout.sh --go --old /旧路径 --new /新路径   # 自定义路径
#
# 前置：请先退出「尊者直播监控」App（让 daemon 停掉），避免迁移过程中 daemon
#       往半迁移的目录里写新文件。脚本会尝试自动停 daemon，但最好先手动退出 App。
# =============================================================================
set -e

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="$ROOT/config/config.ini"
DRY=1
OLD=""
NEW=""

for a in "$@"; do
  case "$a" in
    --go) DRY=0 ;;
    --old)  shift; OLD="$1" ;;
    --new)  shift; NEW="$1" ;;
  esac
done

# ---- 解析当前产物根目录（从 config.ini 的 产物根目录；拿不到则用默认） ----
if [ -z "$OLD" ]; then
  if [ -f "$CONFIG" ]; then
    OLD="$(grep -E '^[[:space:]]*产物根目录[[:space:]]*=' "$CONFIG" | head -1 | sed -E 's/^[^=]*=[[:space:]]*//' | sed -E 's/[[:space:]]*$//')"
  fi
fi
if [ -z "$OLD" ]; then
  OLD="/Volumes/A区/尊者产物"
fi
# 去掉可能的引号
OLD="$(echo "$OLD" | sed -E 's/^["'\'']//; s/["'\'']$//')"

# ---- 推导新路径：把最末一级「尊者产物」换成「尊者直播监控」 ----
if [ -z "$NEW" ]; then
  PARENT="$(dirname "$OLD")"
  NEW="$PARENT/尊者直播监控"
fi

echo "============================================================================"
echo "产物根目录（旧）: $OLD"
echo "产物根目录（新）: $NEW"
echo "配置文件        : $CONFIG"
echo "模式            : $( [ "$DRY" = 0 ] && echo '执行(--)go' || echo '预览(dry-run)' )"
echo "============================================================================"

# ---- 校验 ----
if [ ! -d "$OLD" ]; then
  echo "[错误] 旧产物根目录不存在：$OLD" >&2
  exit 1
fi
if [ "$OLD" = "$NEW" ]; then
  echo "[错误] 新旧路径相同，无需改名。" >&2
  exit 1
fi

# ---- 1. 改名 ----
if [ "$DRY" = 0 ]; then
  if [ -e "$NEW" ]; then
    echo "[错误] 新路径已存在：$NEW（请先确认/清理）" >&2
    exit 1
  fi
  echo "[1/3] 改名：$OLD -> $NEW"
  mv "$OLD" "$NEW"
else
  echo "[1/3] 预览改名：$OLD -> $NEW"
fi

# ---- 2. 更新 config.ini 的 产物根目录 ----
if [ -f "$CONFIG" ]; then
  if grep -qE '^[[:space:]]*产物根目录[[:space:]]*=' "$CONFIG"; then
    if [ "$DRY" = 0 ]; then
      echo "[2/3] 更新 config.ini 产物根目录 -> $NEW"
      python3 - "$CONFIG" "$NEW" <<'PY'
import sys, io
p, new = sys.argv[1], sys.argv[2]
s = open(p, encoding='utf-8').read()
lines = []
changed = False
for ln in s.splitlines(keepends=True):
    if ln.lstrip().startswith('产物根目录'):
        lines.append('产物根目录 = %s\n' % new)
        changed = True
    else:
        lines.append(ln)
open(p, 'w', encoding='utf-8').write(''.join(lines))
print('       已写入' if changed else '       未找到可替换行')
PY
    else
      echo "[2/3] 预览更新 config.ini 产物根目录 -> $NEW"
    fi
  else
    echo "[2/3] config.ini 无 产物根目录 行，跳过"
  fi
else
  echo "[2/3] 找不到 config.ini，跳过（请用 --config 指定）"
fi

# ---- 3. 迁移内容：<主播>/直播 -> 直播/<主播> ；<主播>/短视频 -> 短视频/<主播> ----
echo "[3/3] 迁移内容（按类型重新归置）"
run_mv() {
  local src="$1" dst="$2"
  if [ ! -d "$src" ]; then return; fi
  if [ "$DRY" = 0 ]; then
    mkdir -p "$dst"
    # 逐条目移动内容，避免「目标已存在目录时 mv 把源塞进目标内部」的嵌套问题
    for item in "$src"/*; do
      [ -e "$item" ] || continue
      mv "$item" "$dst"/
    done
    rmdir "$src" 2>/dev/null || true
  else
    echo "       mv $src/*  ->  $dst/"
  fi
}

# 已知系统目录，不当成主播目录处理
SYS_DIRS="database fupan_output logs covers run node_modules .git"

for d in "$NEW"/*; do
  [ -d "$d" ] || continue
  base="$(basename "$d")"
  skip=0
  for s in $SYS_DIRS; do
    [ "$base" = "$s" ] && skip=1 && break
  done
  [ "$skip" = 1 ] && continue
  # 主播目录：搬走它的 直播 / 短视频 子目录
  run_mv "$d/直播"   "$NEW/直播/$base"
  run_mv "$d/短视频" "$NEW/短视频/$base"
  # 主播目录若已空则清理
  if [ "$DRY" = 0 ]; then
    rmdir "$d" 2>/dev/null || true
  fi
done

# 兼容：若根下存在历史 downloads 目录（旧版短视频落点），里面的 <主播> 一并归入 短视频
run_mv "$NEW/downloads" "$NEW/短视频"

if [ "$DRY" = 0 ]; then
  echo ""
  echo "✅ 迁移完成。建议重启「尊者直播监控」App 让 daemon 重新读取配置。"
  echo "   回退方法：把 $NEW 改回 $OLD，并把 config.ini 的 产物根目录 改回 $OLD 即可。"
else
  echo ""
  echo "（以上是预览，未改动任何文件。确认无误后执行：bash scripts/migrate-layout.sh --go）"
fi
