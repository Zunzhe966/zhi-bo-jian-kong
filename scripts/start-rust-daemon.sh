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
else
  ROOT="$HOME/Desktop/尊者监控直播短视频"
fi
cd "$ROOT"

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:$PATH"

if ! command -v cargo >/dev/null 2>&1 && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  echo "未找到 Rust 工具链，请先安装 rustup（https://rustup.rs）" >&2
  exit 1
fi

# 每次启动都确保 release 二进制与最新代码一致；构建失败立即报错，不自欺。
echo "[rust-daemon] 构建 release 版本..."
(cd "$ROOT/zunzhe-core" && cargo build --release)

mkdir -p "$ROOT/logs"
export RUST_LOG="${RUST_LOG:-info}"
# 不显式重定向项目日志：launchd 场景下 TCC 会拒绝写 ~/Desktop，脚本会在
# 重定向时直接失败。由 com.zunzhe.monitor.plist 的 StandardOut/Err 把日志
# 接到 ~/Library/Logs/zunzhe/；手动运行时自然落在当前终端。
exec "$ROOT/zunzhe-core/target/release/zunzhe-core" run-daemon "$@"
