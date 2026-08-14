#!/bin/bash
# 在迁移副本中执行：构建 Rust release、安装桌面端依赖并注册 launchd。
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:$PATH"
command -v cargo >/dev/null 2>&1 || { echo "未找到 Rust 工具链" >&2; exit 1; }
(cd zunzhe-core && cargo build --release)
if [ ! -d desktop/node_modules ]; then
  (cd desktop && npm install --no-audit --no-fund)
fi
(cd desktop && npm run build)
bash scripts/setup-macos.sh
