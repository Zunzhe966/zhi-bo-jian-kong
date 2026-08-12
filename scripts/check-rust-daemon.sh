#!/bin/bash
# 尊者 Rust 核心健康看门狗：每分钟由 launchd/任务计划调用。
# 每次记录 daemon 是否存活、心跳是否新鲜；异常时调用 ensure-services 拉起。
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

RUST_BIN="$ROOT/zunzhe-core/target/release/zunzhe-core"
if [ ! -x "$RUST_BIN" ]; then
  RUST_BIN="$ROOT/zunzhe-core/target/debug/zunzhe-core"
fi

LOG="$ROOT/logs/zunzhe-core.uptime.log"
mkdir -p "$(dirname "$LOG")"

now="$(date '+%Y-%m-%d %H:%M:%S %z')"
pid="$(cat "$ROOT/fupan_output/run/zunzhe-core.pid" 2>/dev/null || true)"

status="$("$RUST_BIN" doctor 2>/dev/null || true)"
daemon_ok="$(printf '%s' "$status" | python3 -c 'import json,sys
try: print("true" if json.load(sys.stdin).get("rust_daemon") else "false")
except Exception: print("false")' 2>/dev/null || echo false)"
heartbeat_ok="$(printf '%s' "$status" | python3 -c 'import json,sys
try: print("true" if json.load(sys.stdin).get("heartbeat_ok") else "false")
except Exception: print("false")' 2>/dev/null || echo false)"

printf '%s pid=%s rust_daemon=%s heartbeat_ok=%s\n' \
  "$now" "${pid:-none}" "$daemon_ok" "$heartbeat_ok" >> "$LOG"

if [ "$daemon_ok" = "false" ] || [ "$heartbeat_ok" = "false" ]; then
  echo "$now daemon unhealthy, calling ensure-services" >> "$LOG"
  "$RUST_BIN" ensure-services >/dev/null 2>&1 || true
fi
