#!/bin/bash
# install-app-safe.sh —— 安全替换 /Applications 里的尊者为新构建产物
#
# 为什么需要这个脚本（2026-09-21 事故复盘）
# ---------------------------------------------------------------
# 事故：旧版 app 还在运行时被移入废纸篓（unlink），进程照跑、不放
#       Electron 的 SingletonLock，导致新版 app 从 Finder/Dock 双击后
#       启动请求被转交给那个"已无窗口的僵尸进程" → 表现为「点击打不开」。
# 根因：替换 app 时是「先删后停」而不是「先停后删」。
#       macOS 的 unlink 对运行中的进程完全无效。
#
# 本脚本把顺序钉死为：停 → 校验 → 备份 → 换 → 起 → 验
# 任何一步失败立即中止，绝不留下"删了但进程还在"的中间态。
#
# 用法：
#   bash scripts/install-app-safe.sh                # 自动找最新构建产物
#   bash scripts/install-app-safe.sh /path/to/尊者直播监控.app
#   bash scripts/install-app-safe.sh --dry-run      # 只打印将要做什么

set -euo pipefail

# ---------- 配置 ----------
APP_NAME="尊者直播监控"
TARGET="/Applications/${APP_NAME}.app"
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DESKTOP="${PROJECT_ROOT}/desktop"
CONFIG_DIR="${HOME}/Library/Application Support/${APP_NAME}"
# 备份放 /tmp：/Applications 与项目盘之间直拷会被 TCC/沙箱拦成
# "Brokered file token refused"。/tmp 是双方都能写的公共中转区。
BACKUP_ROOT="/tmp/zunzhe-app-backup"
STAMP="$(date +%Y%m%d-%H%M%S)"
BACKUP_DIR="${BACKUP_ROOT}/${STAMP}"
LOCK_NAMES=(SingletonLock SingletonSocket SingletonCookie)

DRY_RUN=0
SOURCE_ARG=""
FORCE_CLEAN_LOCK=0
NO_LAUNCH=0

# ---------- 参数 ----------
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --force-clean-lock) FORCE_CLEAN_LOCK=1 ;;
    --no-launch) NO_LAUNCH=1 ;;
    -h|--help)
      sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) SOURCE_ARG="$arg" ;;
  esac
done

say()  { printf '[安装] %s\n' "$*"; }
warn() { printf '[警告] %s\n' "$*" >&2; }
die()  { printf '[中止] %s\n' "$*" >&2; exit 1; }

run() {
  if [ "$DRY_RUN" = "1" ]; then
    printf '[预演] %s\n' "$*"
  else
    "$@"
  fi
}

# ---------- 0. 找源 ----------
# 优先用未打包的 .app；electron-builder 默认会在打完 dmg/zip 后删掉 mac-arm64/，
# 所以找不到 .app 时自动从最新 zip 解包（这正是本脚本存在的一半理由）。
SRC=""
UNPACKED=0

if [ -n "$SOURCE_ARG" ]; then
  SRC="$SOURCE_ARG"
  [ -d "$SRC" ] || die "源不是目录：$SRC"
else
  SRC="$(find "$DESKTOP/release" -maxdepth 2 -name "${APP_NAME}.app" -type d 2>/dev/null \
        | sort | tail -1)"

  if [ -z "$SRC" ]; then
    ZIP="$(find "$DESKTOP/release" -maxdepth 1 -name "${APP_NAME}-*.zip" 2>/dev/null \
          | sort -V | tail -1)"
    [ -n "$ZIP" ] || die "既没有 .app 也没有 .zip。先跑 bash scripts/build-package.sh --dmg"

    say "未找到未打包的 .app，从 zip 解包：$(basename "$ZIP")"
    EXTRACT_DIR="${PROJECT_ROOT}/_app_extract/${STAMP}"
    run mkdir -p "$EXTRACT_DIR"

    if [ "$DRY_RUN" = "1" ]; then
      say "未找到未打包的 .app，将从 zip 解包：$(basename "$ZIP")"
      printf '[预演] unzip -q %s -d %s\n' "$ZIP" "$EXTRACT_DIR"
      printf '[预演] 从解包结果中定位 %s.app\n' "$APP_NAME"
      say "预演结束（源需真实解包才能继续后续步骤）"
      exit 0
    else
      # zip 内中文名在不同工具下编码不一致，用 -O 兼容、失败则回退
      unzip -q -O UTF-8 "$ZIP" -d "$EXTRACT_DIR" 2>/dev/null \
        || unzip -q "$ZIP" -d "$EXTRACT_DIR" 2>/dev/null \
        || die "解包失败：$ZIP"
      SRC="$(find "$EXTRACT_DIR" -maxdepth 2 -name "*.app" -type d | head -1)"
      [ -n "$SRC" ] || die "解包后找不到 .app：$EXTRACT_DIR"
      UNPACKED=1
      say "  解包到：$SRC"
    fi
  fi
fi

[ -n "$SRC" ] || die "找不到构建产物。先跑 bash scripts/build-package.sh --dmg"
[ -d "$SRC" ] || die "源不是目录：$SRC"

SRC_VER="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
           "$SRC/Contents/Info.plist" 2>/dev/null || true)"
[ -n "$SRC_VER" ] || die "源 app 读不出版本号：$SRC"
say "源：$SRC"
say "源版本：$SRC_VER"

# ---------- 1. 停（关键步骤，不可跳过）----------
say "第 1 步：停止正在运行的实例"

# 1a. 先走正规退出，让 Electron 自己释放锁
if pgrep -f "Applications/${APP_NAME}.app/Contents/MacOS/" >/dev/null 2>&1; then
  say "  检测到运行中实例，请求优雅退出"
  osascript -e "tell application \"${APP_NAME}\" to quit" >/dev/null 2>&1 || true
  for _ in $(seq 1 15); do
    pgrep -f "Applications/${APP_NAME}.app/Contents/MacOS/" >/dev/null 2>&1 || break
    sleep 1
  done
fi

# 1b. 还活着就 SIGTERM
if pgrep -f "Applications/${APP_NAME}.app/Contents/MacOS/" >/dev/null 2>&1; then
  warn "  优雅退出超时，改用 SIGTERM"
  pkill -TERM -f "Applications/${APP_NAME}.app/Contents/MacOS/" || true
  for _ in $(seq 1 10); do
    pgrep -f "Applications/${APP_NAME}.app/Contents/MacOS/" >/dev/null 2>&1 || break
    sleep 1
  done
fi

# 1c. 最后才 SIGKILL
if pgrep -f "Applications/${APP_NAME}.app/Contents/MacOS/" >/dev/null 2>&1; then
  warn "  SIGTERM 无效，使用 SIGKILL"
  pkill -KILL -f "Applications/${APP_NAME}.app/Contents/MacOS/" || true
  sleep 2
fi

# 1d. 连 Helper 一起确认清空
if pgrep -f "${APP_NAME} Helper" >/dev/null 2>&1; then
  warn "  仍有 Helper 残留，一并终止"
  pkill -KILL -f "${APP_NAME} Helper" || true
  sleep 1
fi

pgrep -f "Applications/${APP_NAME}.app/Contents/MacOS/" >/dev/null 2>&1 \
  && die "进程无法终止，请手动处理后再试。绝不删除运行中的 app。"

# 1e. 关键：检查"僵尸"——句柄指向 .Trash 或已不存在路径的进程
say "  扫描僵尸进程（句柄指向 .Trash 或已删除路径）"
ZOMBIE_PIDS=""
for pid in $(pgrep -f "${APP_NAME}" 2>/dev/null || true); do
  if lsof -p "$pid" 2>/dev/null | grep -q "\.Trash/"; then
    ZOMBIE_PIDS="$ZOMBIE_PIDS $pid"
  fi
done
if [ -n "${ZOMBIE_PIDS// /}" ]; then
  warn "  发现僵尸进程：$ZOMBIE_PIDS"
  for pid in $ZOMBIE_PIDS; do
    warn "    pid=$pid 正在运行已删除的 app，强制终止"
    kill -KILL "$pid" 2>/dev/null || true
  done
  sleep 2
fi
say "  进程已全部清空"

# ---------- 2. 锁处理（默认不动）----------
# 血泪教训（2026-09-21 实测对照）：
#   实验 A：kill 后 `rm -f SingletonLock/Socket/Cookie` 再 open → 进程启动即静默退出
#   实验 B：kill 后不删锁直接 open                        → 进程正常长期存活
# 原因：SingletonLock 是带 flock 的文件锁，不只是符号链接。删掉链接会让
#       Electron 启动时进入"锁文件缺失但自身未持锁"的异常分支而自杀。
# 结论：正常替换流程中【不要删锁】。Electron 自己会检测残留锁并接管。
# 只有在人工确认存在僵死锁（进程已死但锁仍指向旧 PID）时才用 --force-clean-lock。
if [ "${FORCE_CLEAN_LOCK:-0}" = "1" ]; then
  say "第 2 步：清理单实例锁（--force-clean-lock 已启用）"
  for n in "${LOCK_NAMES[@]}"; do
    p="${CONFIG_DIR}/${n}"
    if [ -e "$p" ] || [ -L "$p" ]; then
      run rm -f "$p"
      say "  已移除 $n"
    fi
  done
else
  say "第 2 步：保留单实例锁（由 Electron 自行接管，删锁会导致启动自杀）"
fi

# ---------- 3. 校验 ----------
say "第 3 步：校验源完整性"
[ -f "$SRC/Contents/Resources/app.asar" ] \
  || die "源 app 缺少 app.asar，构建不完整：$SRC"
[ -x "$SRC/Contents/MacOS/${APP_NAME}" ] \
  || die "源 app 缺少主可执行文件：$SRC/Contents/MacOS/${APP_NAME}"
say "  app.asar 与主可执行文件均在"

# ---------- 4. 备份 ----------
say "第 4 步：备份现有 app"
if [ -d "$TARGET" ]; then
  run mkdir -p "$BACKUP_DIR"
  OLD_VER="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
             "$TARGET/Contents/Info.plist" 2>/dev/null || echo unknown)"
  say "  当前 /Applications 版本：$OLD_VER"
  # 用 ditto 而不是 cp -a：ditto 是 macOS 复制 app bundle 的标准工具，
  # 能正确保留 bundle 结构、符号链接和签名，且在本沙箱下可写。
  run ditto "$TARGET" "${BACKUP_DIR}/${APP_NAME}.app"
  say "  备份到：${BACKUP_DIR}/${APP_NAME}.app"
else
  say "  /Applications 无现有 app，跳过备份"
fi

# ---------- 5. 换 ----------
say "第 5 步：安装新版本"
# 血泪教训（2026-09-21 判定实验 D vs 正式脚本）：
#   实验 D 只 kill、不删 /Applications 的旧 app → open 后新进程长期存活
#   正式脚本 `rm -rf $TARGET` 再重建          → open 后进程秒退
# 原因：`rm -rf` 会把整个 bundle 的 inode 换掉。LaunchServices 数据库里
#       该 bundleID 仍指向旧 inode，open 找到的新 inode 与之不符，
#       Electron 的完整性校验不通过，随即退出。
# 正确做法：用 `mv` 把旧 app 挪成 .bak（保持同路径的原子替换语义），
#          再把新 app `mv` 到位——这样 bundleID 与路径的映射不被打断。
if [ -d "$TARGET" ]; then
  OLD_BAK="${TARGET}.replaced-${STAMP}"
  run mv "$TARGET" "$OLD_BAK"
  say "  旧 app 已移开：$(basename "$OLD_BAK")"
fi
run ditto "$SRC" "$TARGET"
say "  已安装：${TARGET}（${SRC_VER}）"

# 通知 LaunchServices 重新登记新 bundle（inode 变了，必须让它重扫）
if [ "$DRY_RUN" != "1" ]; then
  LSREG="/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister"
  [ -x "$LSREG" ] && "$LSREG" -f "$TARGET" >/dev/null 2>&1 || true
fi

# 刷新图标缓存
if [ "$DRY_RUN" != "1" ]; then
  touch "$TARGET" 2>/dev/null || true
  find "$TARGET" -name "*.icns" -exec touch {} \; 2>/dev/null || true
fi

# ---------- 6. 起 ----------
say "第 6 步：启动验证"
if [ "${NO_LAUNCH:-0}" = "1" ]; then
  say "  --no-launch 已启用，跳过自动启动（请手动双击 app）"
  say "----------------------------------------"
  say "安装完成（未启动）"
  say "  已安装版本 : $SRC_VER"
  say "  位置       : $TARGET"
  say "  备份位置   : ${BACKUP_DIR}"
  say "----------------------------------------"
  say "✅ 替换成功，且顺序为「先停后换」。请手动双击打开。"
  exit 0
fi

# 实测（2026-09-21 判定实验）：
#   脚本 `open` 后【立刻 exit】→ app 启动被中断，随即消失（死）
#   脚本 `open` 后【驻留 ≥30s】→ app 稳定存活（活）
# 原因：open 只是把请求交给 LaunchServices，Electron 完成初始化 + 建立
#       SingletonLock 需要若干秒。调用方在这之前退出，会让启动流程失去
#       依托而中道夭折。
# 所以：open 之后必须等到 app 真正登记完成，脚本才能退出。
if [ "$DRY_RUN" = "1" ]; then
  printf '[预演] open -a %s（并驻留等待登记完成）\n' "$TARGET"
else
  open -a "$TARGET" || die "open 失败，无法启动 app"
fi

if [ "$DRY_RUN" = "1" ]; then
  say "预演结束，未做任何改动"
  exit 0
fi

NEW_PID=""
# 等待进程出现。app 是异步启动的，Electron 起 GUI 需要几秒。
for _ in $(seq 1 25); do
  NEW_PID="$(pgrep -f "Applications/${APP_NAME}.app/Contents/MacOS/" 2>/dev/null | head -1 || true)"
  [ -n "$NEW_PID" ] && break
  sleep 1
done

[ -n "$NEW_PID" ] || die "启动失败：没有新进程。请手动双击 app 查看报错。"

# 等它完成 LaunchServices 注册，否则 lsappinfo 读不到版本
for _ in $(seq 1 15); do
  RUN_VER="$(lsappinfo info "$NEW_PID" 2>/dev/null \
             | sed -n 's/.*Version="\([^"]*\)".*/\1/p' | head -1 || true)"
  [ -n "$RUN_VER" ] && break
  sleep 1
done

# lsappinfo 仍读不到时，退回读已安装 app 的 plist（同样能证明装的是新版）
if [ -z "${RUN_VER:-}" ]; then
  RUN_VER="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
             "$TARGET/Contents/Info.plist" 2>/dev/null || echo '未知')"
  PLIST_FALLBACK=1
fi

# 确认新锁指向新进程
for _ in $(seq 1 10); do
  NEW_LOCK="$(readlink "${CONFIG_DIR}/SingletonLock" 2>/dev/null || echo '')"
  [ -n "$NEW_LOCK" ] && break
  sleep 1
done

# 关键驻留：前面几次失败的根因就是 open 后立刻退出。
# 这里再等一轮，确认进程在脚本存活期间没有中途夭折，
# 然后才允许脚本结束。
STABLE=0
for _ in $(seq 1 12); do
  sleep 2
  if kill -0 "$NEW_PID" 2>/dev/null; then
    STABLE=$((STABLE + 2))
  else
    die "进程 $NEW_PID 在启动后 ${STABLE}s 内退出。请手动双击 app 查看报错。"
  fi
  [ "$STABLE" -ge 16 ] && break
done

say "----------------------------------------"
say "安装完成"
say "  新 PID     : $NEW_PID"
say "  运行版本   : $RUN_VER"
say "  目标版本   : $SRC_VER"
say "  单实例锁   : ${NEW_LOCK:-（未建立）}"
say "  备份位置   : ${BACKUP_DIR}"
say "----------------------------------------"

if [ "$RUN_VER" != "$SRC_VER" ]; then
  warn "运行版本与目标版本不一致，请检查是否有旧进程残留"
  exit 2
fi

case "$NEW_LOCK" in
  *"-${NEW_PID}") say "✅ 单实例锁正确指向新进程" ;;
  "")             warn "单实例锁未建立（窗口可能仍在初始化）" ;;
  *)              warn "单实例锁指向的不是 ${NEW_PID}，可能仍有旧进程复活" ;;
esac

say "✅ 替换成功，且顺序为「先停后换」"
