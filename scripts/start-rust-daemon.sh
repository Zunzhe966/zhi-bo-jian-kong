#!/bin/bash
# 启动 zunzhe-core Rust 守护进程（24×30 常驻）。
# 用法：
#   bash scripts/start-rust-daemon.sh [--once] [--interval 900]
# 建议配合 launchd/任务计划使用；日志在 logs/zunzhe-core.log。
set -euo pipefail

# 支持从仓库 scripts/ 或 ~/Library/LaunchAgents/ 两种位置运行：
# 优先用仓库绝对路径，避免 launchd 复制脚本后 dirname 解析到错误目录。
if [ -d "$(dirname "$0")/../zunzhe-core" ]; then
  ROOT="$(cd "$(dirname "$0")/.." && pwd)"
elif [ -d "$HOME/Projects/尊者监控直播短视频/zunzhe-core" ]; then
  ROOT="$HOME/Projects/尊者监控直播短视频"
elif [ -d "$HOME/Documents/尊者监控直播短视频/zunzhe-core" ]; then
  ROOT="$HOME/Documents/尊者监控直播短视频"
else
  ROOT="$HOME/Desktop/尊者监控直播短视频"
fi
cd "$ROOT"

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:$PATH"

if ! command -v cargo >/dev/null 2>&1 && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  echo "未找到 Rust 工具链，请先安装 rustup（https://rustup.rs）" >&2
  exit 1
fi

# 只在二进制缺失时构建，不在每次启动时重建。
#
# 这里曾经是「每次启动都 cargo build --release」。对 24×7 常驻服务来说这是错的：
# 开机/重启后 launchd 会先跑一次 1~2 分钟的编译，这期间监控是断的；构建失败还会
# 直接让 daemon 起不来（构建属于发布步骤，不该是启动步骤）。
# 正常的发布流程由 scripts/setup-macos.sh 负责构建；需要手动重建时跑
# `cd zunzhe-core && cargo build --release` 即可。
BIN="$ROOT/zunzhe-core/target/release/zunzhe-core"
if [ ! -x "$BIN" ]; then
  echo "[rust-daemon] 未找到 $BIN，先构建一次 release..."
  (cd "$ROOT/zunzhe-core" && cargo build --release)
fi
if [ ! -x "$BIN" ]; then
  echo "[rust-daemon] 构建后仍未找到二进制，放弃启动" >&2
  exit 1
fi

export RUST_LOG="${RUST_LOG:-info}"
# 不显式重定向项目日志：launchd 场景下 TCC 会拒绝写 ~/Desktop，脚本会在
# 重定向时直接失败。由 com.zunzhe.monitor.plist 的 StandardOut/Err 把日志
# 接到 ~/Library/Logs/zunzhe/；手动运行时自然落在当前终端。
exec "$BIN" run-daemon "$@"
