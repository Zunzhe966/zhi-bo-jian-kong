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
echo "[setup] 完成"
