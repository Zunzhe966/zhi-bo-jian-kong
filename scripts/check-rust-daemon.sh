#!/bin/bash
# 尊者 Rust 核心健康看门狗：每分钟由 launchd/任务计划调用。
# 每次记录 daemon 是否存活、心跳是否新鲜；异常时调用 ensure-services 拉起。
# 同时顺带执行完全访问守卫，避免 Codex 权限配置被误改后影响自动维护。
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

CONFIG_FILE="$ROOT/config/config.ini"
[ -f "$CONFIG_FILE" ] || CONFIG_FILE="$ROOT/config/config.ini.example"
MEDIA_ROOT=""
LOG_DIR=""
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
  FUPAN_DIR="$(awk -F= '
    /^[[:space:]]*\[/ { section=$0; gsub(/^[[:space:]]*\[|[[:space:]]*\].*$/, "", section) }
    section == "路径设置" && $1 ~ /^[[:space:]]*复盘输出目录[[:space:]]*$/ {
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
if [ -z "$FUPAN_DIR" ]; then
  FUPAN_DIR="$MEDIA_ROOT/fupan_output"
else
  case "$FUPAN_DIR" in
    \~) FUPAN_DIR="$HOME" ;;
    \~/*) FUPAN_DIR="$HOME/${FUPAN_DIR#\~/}" ;;
    /*) ;;
    *) FUPAN_DIR="$ROOT/$FUPAN_DIR" ;;
  esac
fi
mkdir -p "$LOG_DIR" 2>/dev/null || true

if [ -x "$ROOT/scripts/ensure-full-access.sh" ]; then
  "$ROOT/scripts/ensure-full-access.sh" >> "$LOG_DIR/zunzhe-core.watchdog.log" 2>/dev/null || true
fi

RUST_BIN="$ROOT/zunzhe-core/target/release/zunzhe-core"
if [ ! -x "$RUST_BIN" ]; then
  RUST_BIN="$ROOT/zunzhe-core/target/debug/zunzhe-core"
fi

LOG="$LOG_DIR/zunzhe-core.watchdog.log"

AUDIT_LOG="$LOG_DIR/zunzhe-core.audit.log"

# 桌面端 watchdog 每分钟追加一行；按大小轮转并保留最近 5 份，避免
# 24×30 期间只靠桌面端运行时日志无限增长。
rotate_if_large() {
  local file="$1"
  local max_bytes="${2:-5242880}"
  local size
  if [ -f "$file" ]; then
    size="$(stat -f%z "$file" 2>/dev/null || stat -c%s "$file" 2>/dev/null || echo 0)"
  else
    size=0
  fi
  if [ "$size" -ge "$max_bytes" ]; then
    # 保留最近 5 份（.1~.5）：先丢弃最老的 .5，再整体后移一位，
    # 最后把当前文件挪成 .1。旧实现 `for i in 5 4 3 2 1` 会把 .5 挪成 .6
    # （多留一份且永不复用），且末尾还有一段重复移动 .1 的死代码。
    rm -f "$file.5" 2>/dev/null || true
    for i in 4 3 2 1; do
      if [ -f "$file.$i" ]; then
        mv -f "$file.$i" "$file.$((i+1))" 2>/dev/null || true
      fi
    done
    mv -f "$file" "$file.1" 2>/dev/null || true
  fi
}
rotate_if_large "$LOG"
rotate_if_large "$AUDIT_LOG"

now="$(date '+%Y-%m-%d %H:%M:%S %z')"
# 运行态目录跟随「复盘输出目录/run」，与 Rust paths::run_dir 同口径。
# 旧写法硬编码 $ROOT/fupan_output/run；产物迁到外置盘后这里会永远读不到 pid，
# 看门狗日志就会长期显示 pid=none，误导排查。
pid="$(cat "$FUPAN_DIR/run/zunzhe-core.pid" 2>/dev/null || true)"

status="$("$RUST_BIN" doctor 2>/dev/null || true)"
if printf '%s' "$status" | grep -q '"rust_daemon":true'; then
  daemon_ok=true
else
  daemon_ok=false
fi
if printf '%s' "$status" | grep -q '"heartbeat_ok":true'; then
  heartbeat_ok=true
else
  heartbeat_ok=false
fi

printf '%s pid=%s rust_daemon=%s heartbeat_ok=%s\n' \
  "$now" "${pid:-none}" "$daemon_ok" "$heartbeat_ok" >> "$LOG"

# 与 audit-longrun.sh / check-rust-daemon.ps1 对齐：把 doctor 的
# source_health / notify_health 字段写入审计日志，让桌面端看门狗和 Windows
# 看门狗有相同可审计指标；解析失败只记一行异常，不阻塞健康判定。
printf '%s' "$status" | python3 -c 'import json,sys
try:
    d = json.load(sys.stdin)
except Exception:
    print("audit_unparseable")
    sys.exit(0)
h = d.get("health_audit") or {}
sh = d.get("source_health") or {}
nh = d.get("notify_health") or {}
print(json.dumps({
    "health": bool(d.get("health", h.get("ok", False))),
    "daemon_ok": bool(d.get("daemon_ok", d.get("rust_daemon", h.get("daemon_ok", False)))),
    "heartbeat_ok": bool(d.get("heartbeat_ok", h.get("heartbeat_ok", False))),
    "pid": h.get("pid") or d.get("rust_pid"),
    "rss_kib": h.get("rss_kib"),
    "uptime_secs": h.get("uptime_secs", d.get("uptime_secs", 0)),
    "heartbeat_age_secs": h.get("last_heartbeat_age_secs"),
    "heartbeat_lines": h.get("heartbeat_lines", 0),
    "consecutive_heartbeats": h.get("consecutive_heartbeats", 0),
    "log_gap_secs": h.get("log_gap_secs"),
    "source_total": sh.get("total", 0),
    "source_failing": sh.get("failing", 0),
    "source_cooling": sh.get("cooling", 0),
    "source_ok": bool(sh.get("ok", True)),
    "notify_pending": nh.get("pending", 0),
    "notify_failed": nh.get("failed", 0),
    "notify_latest_failed_channel": nh.get("latest_failed_channel"),
    "notify_hint": nh.get("hint"),
    "notify_ok": bool(nh.get("ok", True)),
    "disk_free_mb": d.get("disk_free_mb"),
}, ensure_ascii=False))' 2>/dev/null | sed "s/^/$now /" >> "$AUDIT_LOG" 2>/dev/null || echo "$now audit_unparseable" >> "$AUDIT_LOG"

if [ "$daemon_ok" = "false" ] || [ "$heartbeat_ok" = "false" ]; then
  echo "$now daemon unhealthy, calling ensure-services" >> "$LOG"
  # 不能在这里无限等：macOS 桌面端看门狗由 launchd 每分钟触发；Windows
  # 由任务计划触发。ensure-services 自身有有限等待（launchd 接管确认最多
  # 5 秒 + spawn 健康轮询最多 5 秒），整体最坏约 10 秒。
  "$RUST_BIN" ensure-services >/dev/null 2>&1 || true
fi
