#!/bin/bash
# 尊者 Rust 核心健康看门狗：每分钟由 launchd/任务计划调用。
# 每次记录 daemon 是否存活、心跳是否新鲜；异常时调用 ensure-services 拉起。
# 同时顺带执行完全访问守卫，避免 Codex 权限配置被误改后影响自动维护。
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [ -x "$ROOT/scripts/ensure-full-access.sh" ]; then
  "$ROOT/scripts/ensure-full-access.sh" >> "$ROOT/logs/zunzhe-core.watchdog.log" 2>/dev/null || true
fi

RUST_BIN="$ROOT/zunzhe-core/target/release/zunzhe-core"
if [ ! -x "$RUST_BIN" ]; then
  RUST_BIN="$ROOT/zunzhe-core/target/debug/zunzhe-core"
fi

LOG="$ROOT/logs/zunzhe-core.watchdog.log"
mkdir -p "$(dirname "$LOG")"

# 桌面端 watchdog 每分钟追加一行；按大小轮转并保留最近 5 份，避免
# 24×30 期间只靠桌面端运行时日志无限增长。
rotate_if_large() {
  local file="$1"
  local max_bytes="${2:-5242880}"
  if [ -f "$file" ] && [ "$(stat -f%z "$file" 2>/dev/null || echo 0)" -ge "$max_bytes" ]; then
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

now="$(date '+%Y-%m-%d %H:%M:%S %z')"
pid="$(cat "$ROOT/fupan_output/run/zunzhe-core.pid" 2>/dev/null || true)"

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

if [ "$daemon_ok" = "false" ] || [ "$heartbeat_ok" = "false" ]; then
  echo "$now daemon unhealthy, calling ensure-services" >> "$LOG"
  # 不能在这里无限等：macOS 桌面端看门狗由 launchd 每分钟触发；Windows
  # 由任务计划触发。ensure-services 自身有有限等待（launchd 接管确认最多
  # 5 秒 + spawn 健康轮询最多 5 秒），整体最坏约 10 秒。
  "$RUST_BIN" ensure-services >/dev/null 2>&1 || true
fi
