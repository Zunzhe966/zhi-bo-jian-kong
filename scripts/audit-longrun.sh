#!/bin/bash
# 24×30 长跑体检：从 doctor 的 health_audit 提取可审计指标，按分钟落盘。
# 由 launchd/桌面端每分钟调用一次，配合 zunzhe-core.uptime.log 一起证明
# “持续运行、心跳连续、RSS 不增长、无日志断档”。任何一步失败只记一行
# 异常，不让审计脚本本身阻塞健康看门狗。
set -u

ROOT="${ZUNZHE_PROJECT_ROOT:-}"
if [ -z "$ROOT" ]; then
  for candidate in "$HOME/Projects/尊者监控直播短视频" "$HOME/Documents/尊者监控直播短视频" "$HOME/Desktop/尊者监控直播短视频"; do
    if [ -f "$candidate/config/config.ini.example" ] || [ -f "$candidate/config/config.ini" ]; then
      ROOT="$candidate"
      break
    fi
  done
fi
if [ -z "$ROOT" ]; then
  exit 0
fi

RUST_BIN="$ROOT/zunzhe-core/target/release/zunzhe-core"
if [ ! -x "$RUST_BIN" ]; then
  RUST_BIN="$ROOT/zunzhe-core/target/debug/zunzhe-core"
fi
[ -x "$RUST_BIN" ] || exit 0

LOG_DIR="$ROOT/logs"
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/zunzhe-core.audit.log"

# 与健康脚本同样按 5MB 轮转，保留 5 份，避免 24×30 审计日志无限膨胀。
rotate_if_large() {
  local file="$1"
  if [ -f "$file" ] && [ "$(stat -f%z "$file" 2>/dev/null || echo 0)" -ge 5242880 ]; then
    for i in 5 4 3 2 1; do
      if [ -f "$file.$i" ]; then
        mv -f "$file.$i" "$file.$((i+1))" 2>/dev/null || true
      fi
    done
    if [ -f "$file.1" ]; then
      mv -f "$file.1" "$file.2" 2>/dev/null || true
    fi
    mv -f "$file" "$file.1" 2>/dev/null || true
  fi
}
rotate_if_large "$LOG"

status="$(cd "$ROOT" && "$RUST_BIN" doctor 2>/dev/null || true)"
if [ -z "$status" ]; then
  echo "$(date '+%Y-%m-%d %H:%M:%S %z') doctor_unavailable" >> "$LOG"
  exit 0
fi

# 优先取新增 health_audit 的机器字段；旧二进制缺失时回退到原有字段，
# 保证脚本对老版本也能记录基础指标。
health="$(printf '%s' "$status" | python3 -c 'import json,sys
d=json.load(sys.stdin)
h=d.get("health_audit") or {}
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
}, ensure_ascii=False))' 2>/dev/null || true)"
if [ -z "$health" ]; then
  echo "$(date '+%Y-%m-%d %H:%M:%S %z') audit_unparseable" >> "$LOG"
  exit 0
fi

printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S %z')" "$health" >> "$LOG"
exit 0
