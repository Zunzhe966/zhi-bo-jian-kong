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
# watchdog 日志独立成文件，避免与 daemon 心跳日志混写；pid 读取失败只
# 影响诊断显示，不影响健康判定（doctor 以进程/心跳为准）。
pid="$(cat "$ROOT/fupan_output/run/zunzhe-core.pid" 2>/dev/null || true)"
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
  # 有限等待（launchd 最多 5 秒）+ spawn 健康轮询（最多 5 秒），整体
  # 最坏约 10 秒，不会拖垮下一分钟。
  "$RUST_BIN" ensure-services >/dev/null 2>&1 || true
fi
