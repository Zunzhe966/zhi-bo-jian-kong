#!/bin/bash
# =============================================================================
# 尊者直播监控 —— 发布新版本到「更新源」
#
# 作用：把打包好的 .app 放进 <数据目录>/releases/，用户在界面点「检查更新」
#      就能发现并一键换上。全离线，不需要任何服务器。
#
# 为什么不会丢数据：
#   程序装在 /Applications/尊者直播监控.app
#   数据在   <数据目录>（config/ + zunzhe-core.db + fupan_output/ ...）
#   更新只做「换程序」这一步，数据目录一个字节都不碰。
#
# ---------------------------------------------------------------------------
# 【版本节奏 · 请遵守】一个版本 = 一批改动，不是一处改动。
#   不要改一行就发一版，那样会在更新源里堆出一串几乎一样的包（用户会炸）。
#   正确节奏：把一批要改的改完 → 测完 → 只发一次。
#
# 【更新源永远只有最新一个包】
#   每次发布会把比新版本低的旧包自动移进废纸篓，不需要手动清理。
#   想留旧包做回滚用 --keep-old。
# ---------------------------------------------------------------------------
#
# 用法：
#   bash scripts/publish-release.sh                 # 用 desktop/release 里最新打好的包
#   bash scripts/publish-release.sh --build         # 先打包再发布
#   bash scripts/publish-release.sh --app /path/x.app   # 指定某个 .app
#   bash scripts/publish-release.sh --list          # 只看当前更新源里有什么
#   bash scripts/publish-release.sh --keep-old      # 保留旧包（默认会清掉）
#   bash scripts/publish-release.sh --keep-build    # 保留构建产物里的 .app（默认清掉）
# =============================================================================
set -e

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# 数据目录：优先环境变量，其次 ~/Projects 下的链接（本机指向移动硬盘那份），最后本仓库
resolve_data_dir() {
  if [ -n "$ZUNZHE_DATA_DIR" ] && [ -d "$ZUNZHE_DATA_DIR" ]; then
    echo "$ZUNZHE_DATA_DIR"; return
  fi
  local p="$HOME/Projects/尊者监控直播短视频"
  if [ -d "$p" ]; then echo "$p"; return; fi
  echo "$ROOT"
}

DATA_DIR="$(resolve_data_dir)"
RELEASES="$DATA_DIR/releases"

DO_BUILD=0
APP_SRC=""
DO_LIST=0
KEEP_OLD=0
KEEP_BUILD=0
while [ $# -gt 0 ]; do
  case "$1" in
    --build) DO_BUILD=1 ;;
    --list) DO_LIST=1 ;;
    --keep-old) KEEP_OLD=1 ;;
    --keep-build) KEEP_BUILD=1 ;;
    --app) shift; APP_SRC="$1" ;;
    *) echo "[发布] 忽略未知参数：$1" >&2 ;;
  esac
  shift
done

mkdir -p "$RELEASES"

# 送废纸篓：优先用系统 trash（可恢复），没有才用 rm
dispose() {
  local target="$1"
  if [ -x /usr/bin/trash ]; then
    /usr/bin/trash "$target" 2>/dev/null && return 0
  fi
  rm -rf "$target"
}

# 版本比较：ver_lt A B —— A 比 B 旧时返回 0。
# 按数字段比，不做字典序（否则 1.10.0 会被判成比 1.9.0 旧）。
ver_lt() {
  local ka kb
  ka=$(printf '%s' "$1" | awk -F. '{printf "%05d%05d%05d", $1+0, $2+0, $3+0}')
  kb=$(printf '%s' "$2" | awk -F. '{printf "%05d%05d%05d", $1+0, $2+0, $3+0}')
  [ "$ka" -lt "$kb" ]
}

# 从一个更新源条目里读出它的版本号；读不到就输出空串。
entry_version() {
  local path="$1" base
  base="$(basename "$path")"
  local v
  v=$(printf '%s' "$base" | sed -n 's/.*-\([0-9][0-9.]*\)\.zip$/\1/p')
  if [ -n "$v" ]; then echo "$v"; return; fi
  if [ -d "$path" ] && [ "${path##*.}" = "app" ]; then
    /usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$path/Contents/Info.plist" 2>/dev/null || true
    return
  fi
  if [ -d "$path" ]; then
    local inner
    inner=$(find "$path" -maxdepth 2 -name '*.app' -type d 2>/dev/null | head -1)
    if [ -n "$inner" ] && [ -f "$inner/Contents/Info.plist" ]; then
      /usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$inner/Contents/Info.plist" 2>/dev/null || true
    fi
  fi
}

if [ "$DO_LIST" = "1" ]; then
  echo "[更新源] $RELEASES"
  ls -lh "$RELEASES" 2>/dev/null | grep -vE "^total" || echo "  (空)"
  echo ""
  echo "[占用] $(du -sh "$RELEASES" 2>/dev/null | awk '{print $1}')"
  exit 0
fi

if [ "$DO_BUILD" = "1" ]; then
  echo "[发布] 先打包 ..."
  bash "$ROOT/scripts/build-package.sh" --skip-rust
fi

if [ -z "$APP_SRC" ]; then
  APP_SRC="$ROOT/desktop/release/mac-arm64/尊者直播监控.app"
fi
if [ ! -d "$APP_SRC" ]; then
  echo "[发布] 错误：找不到 .app：$APP_SRC" >&2
  echo "[发布] 先跑 bash scripts/build-package.sh --skip-rust" >&2
  exit 1
fi

VER="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP_SRC/Contents/Info.plist" 2>/dev/null || echo '')"
if [ -z "$VER" ]; then
  echo "[发布] 错误：读不到版本号（Info.plist 缺 CFBundleShortVersionString）" >&2
  exit 1
fi

ZIP="$RELEASES/尊者直播监控-$VER.zip"
echo "[发布] 版本 $VER"
echo "[发布] 压缩 -> $ZIP"
rm -f "$ZIP"
# --keepParent：压缩包顶层就是这个 .app，更新器按这个约定找包
ditto -c -k --sequesterRsrc --keepParent "$APP_SRC" "$ZIP"

# 压缩包自检：必须真的含 Contents/Info.plist，否则更新器认不出这个包
if ! unzip -Z1 "$ZIP" 2>/dev/null | grep -q '\.app/Contents/Info\.plist$'; then
  echo "[发布] 错误：压出来的包不完整（里面找不到 .app/Contents/Info.plist）" >&2
  exit 1
fi
echo "[发布] 包自检通过：$(du -h "$ZIP" | awk '{print $1}')"

# ---- 清掉比新版本低的旧条目（更新源永远只留最新）----
if [ "$KEEP_OLD" != "1" ]; then
  for entry in "$RELEASES"/*; do
    [ -e "$entry" ] || continue
    base="$(basename "$entry")"
    case "$base" in
      说明.txt|*.DS_Store) continue ;;
    esac
    [ "$entry" = "$ZIP" ] && continue
    v="$(entry_version "$entry")"
    # 读不出版本号的杂物不动，避免误删
    [ -z "$v" ] && continue
    if ver_lt "$v" "$VER"; then
      echo "[发布] 清走旧版本 ${v}：${base}"
      dispose "$entry"
    else
      echo "[发布] 保留 ${base}（版本 ${v}，不低于 ${VER}）"
    fi
  done
fi

# 说明文件：用户点开文件夹时能看懂
cat > "$RELEASES/说明.txt" <<'TXT'
这个文件夹是「更新源」。

程序里点「检查更新」，就会来这里找比当前版本更高的包，找到就一键换上。
更新只替换程序本身，数据目录（config、zunzhe-core.db、复盘结果）完全不动。

放什么进来都行：
  尊者直播监控-1.2.2.zip      ← 推荐，体积小
  尊者直播监控.app            ← 直接放目录也可以
  1.2.2/尊者直播监控.app      ← 按版本分文件夹也可以

版本号取包内 Info.plist 的 CFBundleShortVersionString，必须比当前版本高才会被采用。
发布脚本会自动把比新版本低的旧包清走，所以这里正常情况下只有一个包。
TXT

# 顺手把构建产物那份重复的 .app 清掉：它和已安装的那份内容完全一样，
# 留着只会让人以为「怎么有两个软件」。下次要发版先 --build 重新生成即可。
if [ "$KEEP_BUILD" != "1" ] && [ -d "$ROOT/desktop/release/mac-arm64/尊者直播监控.app" ]; then
  echo "[发布] 清掉构建产物里那份重复的 .app（要留着用 --keep-build）"
  dispose "$ROOT/desktop/release/mac-arm64/尊者直播监控.app"
fi

echo ""
echo "============================================================================"
echo "✅ 已发布。更新源内容："
ls -lh "$RELEASES" | grep -vE "^total"
echo ""
echo "   用户在界面点「检查更新」即可换到 $VER"
echo "============================================================================"
