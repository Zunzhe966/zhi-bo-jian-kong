#!/bin/bash
# 首次运行：创建虚拟环境、安装 Python 依赖、安装桌面端依赖并构建。
set -e
cd "$(dirname "$0")/.."

if [ ! -d .venv ]; then
  echo "[setup] 创建 Python 虚拟环境 .venv"
  python3 -m venv .venv
fi

echo "[setup] 安装 Python 依赖"
.venv/bin/pip install --upgrade pip
.venv/bin/pip install -r requirements.txt -r requirements-fupan.txt

if [ ! -f config/config.ini ]; then
  cp config/config.ini.example config/config.ini
  echo "[setup] 已生成 config/config.ini，请填入自己的邮箱/Cookie/API 配置"
fi
if [ ! -f config/URL_config.ini ]; then
  cp config/URL_config.ini.example config/URL_config.ini
  echo "[setup] 已生成 config/URL_config.ini"
fi

if [ ! -d desktop/node_modules ]; then
  echo "[setup] 安装桌面端依赖"
  (cd desktop && npm install --no-audit --no-fund)
fi

echo "[setup] 构建桌面端界面"
(cd desktop && npm run build)

echo "[setup] 构建 Rust 核心 release 版本"
if ! command -v cargo >/dev/null 2>&1 && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  echo "[setup] 未找到 Rust 工具链，请先安装 rustup（https://rustup.rs）" >&2
  exit 1
fi
export PATH="$HOME/.cargo/bin:$PATH"
(cd zunzhe-core && cargo build --release)

# 安装 launchd 守护与健康看门狗（macOS 开机自启 + 崩溃自动拉起）。
# 项目在 ~/Desktop 下时受 macOS TCC 保护，首次加载可能需要在
# 系统设置-隐私与安全性-完全磁盘访问 中授权；桌面端自带看门狗作为兜底。
LAUNCH_AGENTS="$HOME/Library/LaunchAgents"
mkdir -p "$LAUNCH_AGENTS"
# 启动/健康脚本复制到 LaunchAgents：launchd 访问 ~/Desktop 会被 TCC 拒绝，
# 必须让 launchd 直接执行 LaunchAgents 下的脚本，再由脚本访问项目 release。
cp scripts/launchd/zunzhe-daemon.sh "$LAUNCH_AGENTS/zunzhe-daemon.sh"
cp scripts/launchd/zunzhe-healthcheck.sh "$LAUNCH_AGENTS/zunzhe-healthcheck.sh"
chmod +x "$LAUNCH_AGENTS/zunzhe-daemon.sh" "$LAUNCH_AGENTS/zunzhe-healthcheck.sh"
cp scripts/launchd/com.zunzhe.monitor.plist "$LAUNCH_AGENTS/"
cp scripts/launchd/com.zunzhe.healthcheck.plist "$LAUNCH_AGENTS/"
launchctl unload "$LAUNCH_AGENTS/com.zunzhe.monitor.plist" 2>/dev/null || true
launchctl unload "$LAUNCH_AGENTS/com.zunzhe.healthcheck.plist" 2>/dev/null || true
launchctl load "$LAUNCH_AGENTS/com.zunzhe.monitor.plist"
launchctl load "$LAUNCH_AGENTS/com.zunzhe.healthcheck.plist"

echo "[setup] 完成"
