#!/bin/bash
# 启动 zunzhe-core Rust 守护进程（24×30 常驻）。
# 用法：
#   bash scripts/start-rust-daemon.sh [--once] [--interval 900]
# 建议配合 launchd/任务计划使用；日志在 logs/zunzhe-core.log。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:$PATH"

if ! command -v cargo >/dev/null 2>&1; then
  echo "未找到 Rust 工具链，请先安装 rustup（https://rustup.rs）" >&2
  exit 1
fi

# 每次启动都确保 release 二进制与最新代码一致；构建失败立即报错，不自欺。
echo "[rust-daemon] 构建 release 版本..."
(cd "$ROOT/zunzhe-core" && cargo build --release)

mkdir -p "$ROOT/logs"
export RUST_LOG="${RUST_LOG:-info}"
exec "$ROOT/zunzhe-core/target/release/zunzhe-core" run-daemon "$@"
