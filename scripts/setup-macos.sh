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
# launchd 直接加载/读取 ~/Desktop 下的未签名二进制会被系统策略拦截，
# 24×30 稳定运行要求项目不在桌面/文稿保护目录。桌面端自带看门狗仍作为
# 第二重兜底，但不会在 launchd 反复失败时误报成功。
LAUNCH_AGENTS="$HOME/Library/LaunchAgents"
mkdir -p "$LAUNCH_AGENTS"
# 启动/健康脚本复制到 LaunchAgents：launchd 直接执行 LaunchAgents 下的脚本，
# 脚本用 plist WorkingDirectory 解析项目根。
cp scripts/launchd/zunzhe-daemon.sh "$LAUNCH_AGENTS/zunzhe-daemon.sh"
cp scripts/launchd/zunzhe-healthcheck.sh "$LAUNCH_AGENTS/zunzhe-healthcheck.sh"
cp scripts/ensure-full-access.sh "$LAUNCH_AGENTS/ensure-full-access.sh"
chmod +x "$LAUNCH_AGENTS/zunzhe-daemon.sh" "$LAUNCH_AGENTS/zunzhe-healthcheck.sh"
chmod +x "$LAUNCH_AGENTS/ensure-full-access.sh"
PROJECT_DIR="$(pwd)"
sed "s|__PROJECT_DIR__|$PROJECT_DIR|g" scripts/launchd/com.zunzhe.monitor.plist > "$LAUNCH_AGENTS/com.zunzhe.monitor.plist"
cp scripts/launchd/com.zunzhe.healthcheck.plist "$LAUNCH_AGENTS/com.zunzhe.healthcheck.plist"
case "$(pwd)" in
  "$HOME/Desktop/"*)
    echo "[setup] 当前项目在 $HOME/Desktop：macOS TCC 会阻止 launchd 稳定运行。"
    echo "[setup] 请把项目迁移到 ~/Projects/尊者监控直播短视频 或 ~/Documents 后再运行本脚本。"
    echo "[setup] launchd 将保持未加载；桌面端自带看门狗仍可手动启动 daemon。"
    ;;
  *)
    launchctl unload "$LAUNCH_AGENTS/com.zunzhe.monitor.plist" 2>/dev/null || true
    launchctl unload "$LAUNCH_AGENTS/com.zunzhe.healthcheck.plist" 2>/dev/null || true
    launchctl load "$LAUNCH_AGENTS/com.zunzhe.monitor.plist"
    launchctl load "$LAUNCH_AGENTS/com.zunzhe.healthcheck.plist"
    ;;
esac

echo "[setup] 完成"
