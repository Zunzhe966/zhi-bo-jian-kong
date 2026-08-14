#!/bin/bash
# 尊者 Rust 核心健康看门狗（launchd 专用）。
# 安装位置：~/Library/LaunchAgents/zunzhe-healthcheck.sh。
# 放在 LaunchAgents 而不是项目 scripts/ 下，避免 macOS TCC 阻止 launchd
# 访问 ~/Desktop 目录；Rust 二进制仍指向项目 release。
set -u
ROOT="$HOME/Desktop/尊者监控直播短视频"
RUST_BIN="$ROOT/zunzhe-core/target/release/zunzhe-core"
if [ ! -x "$RUST_BIN" ]; then
  RUST_BIN="$ROOT/zunzhe-core/target/debug/zunzhe-core"
fi
LOG_DIR="$HOME/Library/Logs/zunzhe"
LOG="$LOG_DIR/zunzhe-healthcheck.log"
mkdir -p "$LOG_DIR"

# 24×30 日志轮转：launchd StandardOut/Err 文件不会被 Rust 内部轮转，
# 由健康脚本按大小轮转并保留最近 5 份，防止长期运行无限膨胀。
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
rotate_if_large "$LOG_DIR/zunzhe-core.log"
rotate_if_large "$LOG_DIR/zunzhe-core.err.log"

now="$(date '+%Y-%m-%d %H:%M:%S %z')"
pid="$(cat "$ROOT/fupan_output/run/zunzhe-core.pid" 2>/dev/null || true)"
status="$(cd "$ROOT" && "$RUST_BIN" doctor 2>/dev/null || true)"
daemon_ok=false
heartbeat_ok=false
printf '%s' "$status" | grep -q '"rust_daemon":true' && daemon_ok=true
printf '%s' "$status" | grep -q '"heartbeat_ok":true' && heartbeat_ok=true
printf '%s pid=%s rust_daemon=%s heartbeat_ok=%s\n' \
  "$now" "${pid:-none}" "$daemon_ok" "$heartbeat_ok" >> "$LOG"
if [ "$daemon_ok" = "false" ] || [ "$heartbeat_ok" = "false" ]; then
  echo "$now daemon unhealthy, calling ensure-services" >> "$LOG"
  "$RUST_BIN" ensure-services >/dev/null 2>&1 || true
fi
