#!/bin/bash
# 尊者 Rust 核心健康看门狗（launchd 专用）。
# 安装位置：~/Library/LaunchAgents/zunzhe-healthcheck.sh。
# 与 com.zunzhe.monitor 一样，只支持项目位于非 TCC 保护目录（推荐
# ~/Projects/尊者监控直播短视频）。桌面目录由 launchd 看门狗无法稳定读取。
set -u

# 不依赖 launchd 的 WorkingDirectory：TCC 场景下 pwd 可能返回损坏字节，
# 也会被 set -u 当成未定义变量引用。安装时固化的项目根优先；候选路径只
# 作为旧 plist 升级兼容的退路，且优先 Documents/Desktop，避免踩中
# ~/Projects 符号链接导致旧 release 与当前配置分叉。
ROOT="${ZUNZHE_PROJECT_ROOT:-}"
if [ -z "$ROOT" ]; then
  for candidate in "$HOME/Documents/尊者监控直播短视频" "$HOME/Desktop/尊者监控直播短视频"; do
    if [ -f "$candidate/config/config.ini.example" ] || [ -f "$candidate/config/config.ini" ]; then
      ROOT="$candidate"
      break
    fi
  done
fi

if [ -z "$ROOT" ] || { [ ! -f "$ROOT/config/config.ini.example" ] && [ ! -f "$ROOT/config/config.ini" ]; }; then
  echo "找不到项目根（需要 config/config.ini(.example)）" >&2
  exit 1
fi

case "$ROOT" in
  /*) ;;
  *)
    echo "ZUNZHE_PROJECT_ROOT 必须是绝对路径: $ROOT" >&2
    exit 1
    ;;
esac

# 日志目录解析（与 render-launchd-plists.sh / zunzhe-core paths.rs 同一套语义）：
# 优先读 [路径设置] 日志目录；留空 = <产物根目录>/logs。
#
# 历史坑（2026-09-20 修复）：launchd 下项目路径若是 ~/Projects/xxx 这类
# 指向外置盘的符号链接，则在项目目录内 mkdir/重定向会被 TCC 拒绝
# （Operation not permitted），看门狗每分钟往 StandardErrorPath 吐 1KB
# 错误且永远修不好自己。而 StandardOut/Err 的 fd 是 launchd 启动时打开的，
# 跟项目目录权限无关 —— 所以只要把「脚本自己写的日志」指向同一个
# StandardOut 目标路径，就永远不会写失败。
LOG_DIR=""
MEDIA_ROOT=""
CONFIG_FILE="$ROOT/config/config.ini"
[ -f "$CONFIG_FILE" ] || CONFIG_FILE="$ROOT/config/config.ini.example"
if [ -f "$CONFIG_FILE" ]; then
  MEDIA_ROOT="$(awk -F= '
    /^[[:space:]]*\[/ { section=$0; gsub(/^[[:space:]]*\[|[[:space:]]*\].*$/, "", section) }
    section == "路径设置" && $1 ~ /^[[:space:]]*产物根目录[[:space:]]*$/ {
      sub(/^[^=]*=/, ""); gsub(/^[[:space:]]+|[[:space:]]+$/, ""); print; exit
    }
  ' "$CONFIG_FILE")"
  LOG_DIR="$(awk -F= '
    /^[[:space:]]*\[/ { section=$0; gsub(/^[[:space:]]*\[|[[:space:]]*\].*$/, "", section) }
    section == "路径设置" && $1 ~ /^[[:space:]]*日志目录[[:space:]]*$/ {
      sub(/^[^=]*=/, ""); gsub(/^[[:space:]]+|[[:space:]]+$/, ""); print; exit
    }
  ' "$CONFIG_FILE")"
fi
if [ -z "$MEDIA_ROOT" ]; then
  MEDIA_ROOT="$HOME/Documents/尊者产物"
else
  case "$MEDIA_ROOT" in
    \~) MEDIA_ROOT="$HOME" ;;
    \~/*) MEDIA_ROOT="$HOME/${MEDIA_ROOT#\~/}" ;;
    /*) ;;
    *) MEDIA_ROOT="$ROOT/$MEDIA_ROOT" ;;
  esac
fi
if [ -z "$LOG_DIR" ]; then
  LOG_DIR="$MEDIA_ROOT/logs"
else
  case "$LOG_DIR" in
    \~) LOG_DIR="$HOME" ;;
    \~/*) LOG_DIR="$HOME/${LOG_DIR#\~/}" ;;
    /*) ;;
    *) LOG_DIR="$ROOT/$LOG_DIR" ;;
  esac
fi

# mkdir 失败不再致命：TCC 拒绝时回退到真实可写目录，绝不让看门狗
# 因为「日志写不进去」而整体失效。
#
# 注意：探测本身不能产生 stderr。`... > "$dir/probe" 2>/dev/null` 这种写法
# 在 TCC 拒绝时，**重定向阶段**就已经报错，2>/dev/null 只在命令执行后生效，
# 拦不住它 —— 结果每次探测都往 launchd 的 StandardErrorPath 吐一行，等于
# 把刚修好的日志噪音又加回来。所以先 mkdir 拿到退出码，再用 [ -w ] 判断，
# 全程没有会被拒绝的写操作。
LOG_DIR_USABLE=true
if ! mkdir -p "$LOG_DIR" 2>/dev/null || [ ! -w "$LOG_DIR" ]; then
  LOG_DIR_USABLE=false
  FALLBACK="$HOME/Library/Logs/zunzhe"
  mkdir -p "$FALLBACK" 2>/dev/null || true
  LOG_DIR="$FALLBACK"
fi
readonly LOG_DIR_USABLE
export LOG_DIR_USABLE

# 后续 guard/audit/doctor 的输出统一落到 LOG_DIR。不要把日志预创建到源码根，
# 否则即使不再写入，也会继续制造“两份日志目录”的假象。

# 先执行完全访问守卫：即使 launchd 因 TCC 无法读项目，也能自愈 Codex 的
# 权限配置。优先使用随脚本一起安装到 LaunchAgents 的独立副本，避免 launchd
# 连 ~/Desktop 下的项目脚本都读不到时守卫也被一起挡掉；迁移后项目内副本
# 更新，setup 会重新覆盖这份副本。
GUARD="$HOME/Library/LaunchAgents/ensure-full-access.sh"
if [ ! -x "$GUARD" ]; then
  GUARD="$ROOT/scripts/ensure-full-access.sh"
fi
if [ -x "$GUARD" ]; then
  ZUNZHE_PROJECT_ROOT="$ROOT" "$GUARD" >> "$LOG_DIR/zunzhe-healthcheck.log" 2>/dev/null || true
fi

# 24×30 体检：把 doctor 的 health_audit 指标按分钟写入 logs/zunzhe-core.audit.log，
# 供长期验收读取 RSS/心跳/断档；失败不阻塞健康看门狗。
AUDIT="$ROOT/scripts/audit-longrun.sh"
if [ -x "$AUDIT" ]; then
  ZUNZHE_PROJECT_ROOT="$ROOT" "$AUDIT" >> "$LOG_DIR/zunzhe-healthcheck.log" 2>/dev/null || true
fi

case "$ROOT" in
  "$HOME/Desktop/"*)
    echo "项目在 $ROOT：macOS TCC 会阻止 launchd 稳定运行。请把项目迁移到 ~/Projects 或 ~/Documents 后再注册 launchd。" >&2
    exit 1
    ;;
esac

RUST_BIN="$ROOT/zunzhe-core/target/release/zunzhe-core"
if [ ! -x "$RUST_BIN" ]; then
  RUST_BIN="$ROOT/zunzhe-core/target/debug/zunzhe-core"
fi
LOG="$LOG_DIR/zunzhe-healthcheck.log"
# LOG_DIR 上面已解析并验证可写（含 TCC 兜底），这里不再重复探测，
# 只保证目录存在，避免覆盖掉已经选好的可用路径。
mkdir -p "$LOG_DIR" 2>/dev/null || true

# 24×30 日志轮转：launchd StandardOut/Err 文件不会被 Rust 内部轮转，
# 由健康脚本按大小轮转并保留最近 5 份，防止长期运行无限膨胀。
# 24×30 日志轮转。
#
# 关键坑（2026-09-17 实测复现）：launchd 的 StandardOutPath / StandardErrorPath
# 是把 fd 绑在 inode 上的。`mv` 只改目录项、不会让 launchd 重新 open，所以
# daemon 会继续往改名后的文件写 —— 结果是 $file 消失、日志全部堆到 $file.1，
# 而且下一轮因为 `[ -f "$file" ]` 为假直接跳过，永远不再轮转。
# 实测后果：zunzhe-core.log 不存在，daemon 7 天的日志全在 zunzhe-core.log.1
# （14,858,001 字节且持续增长，无上限）。
#
# 因此：live 文件只能「复制 + 原地截断」（保留 inode，launchd 的 fd 继续有效），
# 只有归档档（.1/.2/...，没有进程持有 fd）才可以用 mv。
rotate_if_large() {
  local file="$1"
  local max_bytes="${2:-5242880}"
  local size
  if [ -f "$file" ]; then
    size="$(stat -f%z "$file" 2>/dev/null || stat -c%s "$file" 2>/dev/null || echo 0)"
  else
    size=0
  fi
  if [ "$size" -lt "$max_bytes" ]; then
    return 0
  fi
  # 归档档后移：这些文件没有进程持有 fd，mv 是安全的。保留 .1~.5 共 5 份。
  for i in 4 3 2 1; do
    if [ -f "$file.$i" ]; then
      mv -f "$file.$i" "$file.$((i+1))" 2>/dev/null || true
    fi
  done
  # live 文件：先快照成 .1，再原地截断。绝不能 mv，否则 daemon 的 fd 会跟着
  # 改名走，日志从此落到 .1 且永不轮转。
  cp -f "$file" "$file.1" 2>/dev/null || true
  : > "$file" 2>/dev/null || true
}
rotate_if_large "$LOG_DIR/zunzhe-core.log"
rotate_if_large "$LOG_DIR/zunzhe-core.err.log"
rotate_if_large "$LOG_DIR/zunzhe-healthcheck.log"
rotate_if_large "$LOG_DIR/zunzhe-healthcheck.err.log"

# 兼容升级前安装在 ~/Library/Logs/zunzhe 的历史日志：继续按大小轮转，
# 但不让新日志再写入那里，确保 tail_logs/doctor 只需看项目 logs。
LEGACY_LOG_DIR="$HOME/Library/Logs/zunzhe"
rotate_if_large "$LEGACY_LOG_DIR/zunzhe-core.log"
rotate_if_large "$LEGACY_LOG_DIR/zunzhe-core.err.log"
rotate_if_large "$LEGACY_LOG_DIR/zunzhe-healthcheck.log"
rotate_if_large "$LEGACY_LOG_DIR/zunzhe-healthcheck.err.log"

now="$(date '+%Y-%m-%d %H:%M:%S %z')"
# watchdog 日志独立成文件，避免与 daemon 心跳日志混写；pid 读取失败只
# 影响诊断显示，不影响健康判定（doctor 以进程/心跳为准）。
pid="$("$RUST_BIN" doctor 2>/dev/null | python3 -c 'import json,sys
try: print((json.load(sys.stdin).get("health_audit") or {}).get("pid") or "")
except Exception: pass' 2>/dev/null || true)"
# doctor 本身有超时；这里不用 GNU timeout（macOS 默认没有），避免看门狗
# 因为命令不存在而永远误判。
status="$(cd "$ROOT" && "$RUST_BIN" doctor 2>/dev/null || true)"
daemon_ok=false
heartbeat_ok=false
printf '%s' "$status" | grep -q '"rust_daemon":true' && daemon_ok=true
printf '%s' "$status" | grep -q '"heartbeat_ok":true' && heartbeat_ok=true
printf '%s pid=%s rust_daemon=%s heartbeat_ok=%s\n' \
  "$now" "${pid:-none}" "$daemon_ok" "$heartbeat_ok" >> "$LOG"
if [ "$daemon_ok" = "false" ] || [ "$heartbeat_ok" = "false" ]; then
  echo "$now daemon unhealthy, calling ensure-services" >> "$LOG"
  # 不能在这里无限等：launchd 的 StartInterval 不会重叠运行同任务，若
  # ensure-services 卡住会停掉后续健康轮。目前 ensure-services 自身有
  # 有限等待（launchd 接管确认最多 5 秒 + spawn 健康轮询最多 5 秒），
  # 整体最坏约 10 秒，不会拖垮下一分钟。
  "$RUST_BIN" ensure-services >/dev/null 2>&1 || true
fi
