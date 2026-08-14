#!/bin/bash
# 启动 zunzhe-core Rust 守护进程（24×30 常驻）。
# 安装位置：~/Library/LaunchAgents/zunzhe-daemon.sh。
# 放在 LaunchAgents 而不是项目 scripts/ 下，避免 macOS TCC 阻止 launchd
# 访问 ~/Desktop 目录；Rust 二进制仍指向项目 release。
set -euo pipefail
ROOT="$HOME/Desktop/尊者监控直播短视频"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:$PATH"
# 确保日志目录存在；launchd 的 StandardOut/Err 需要目录可写。
mkdir -p "$HOME/Library/Logs/zunzhe"
echo "[rust-daemon] start $(date '+%Y-%m-%d %H:%M:%S %z')" >&2
export RUST_LOG="${RUST_LOG:-info}"
# 不显式重定向项目日志：launchd 场景下 TCC 会拒绝写 ~/Desktop，脚本会在
# 重定向时直接失败。由 com.zunzhe.monitor.plist 的 StandardOut/Err 把日志
# 接到 ~/Library/Logs/zunzhe/；手动运行时自然落在当前终端。
exec "$ROOT/zunzhe-core/target/release/zunzhe-core" run-daemon "$@"
