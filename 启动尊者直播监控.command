#!/bin/bash
# macOS 启动入口：首次运行请先执行 scripts/setup-macos.sh
cd "$(dirname "$0")"
if [ ! -x .venv/bin/python ] || [ ! -d desktop/node_modules ]; then
  echo "首次运行，正在自动配置环境..."
  bash scripts/setup-macos.sh
fi
cd desktop
exec npx electron .
