#!/bin/bash
# 渲染 launchd plist：项目路径与日志目录都来自同一份 [路径设置] 解析。
# 用法：render-launchd-plists.sh <项目目录> <LaunchAgents 目录>
set -euo pipefail

PROJECT_DIR="${1:-$(pwd)}"
LAUNCH_AGENTS="${2:-$HOME/Library/LaunchAgents}"
mkdir -p "$LAUNCH_AGENTS"
PROJECT_DIR="$(printf '%s' "$PROJECT_DIR" | sed 's|//*|/|g')"

# 安装时必须钉住物理项目根。`~/Projects/尊者监控直播短视频` 可能是指向
# 外置盘真实仓库的符号链接；如果 plist 记录链接路径，launchd、旧 release
# 与配置文件解析就可能落到不同根，重新制造 DB/日志分叉。
if command -v python3 >/dev/null 2>&1; then
  PROJECT_DIR="$(python3 - "$PROJECT_DIR" <<'PY'
import os
import sys

print(os.path.realpath(sys.argv[1]))
PY
  )"
else
  PROJECT_DIR="$(cd "$PROJECT_DIR" && pwd -P)"
fi

CONFIG_FILE="$PROJECT_DIR/config/config.ini"
if [ ! -f "$CONFIG_FILE" ] && [ -f "$PROJECT_DIR/config/config.ini.example" ]; then
  CONFIG_FILE="$PROJECT_DIR/config/config.ini.example"
fi
# 与 zunzhe-core paths 的展开语义保持一致：~ 回家目录，绝对路径原样，
# 相对路径相对项目根。渲染前先确保目录存在，因为 launchd 会先打开 fd。
LOG_DIR=""
MEDIA_ROOT=""
if [ -f "$CONFIG_FILE" ]; then
  MEDIA_ROOT="$(awk -F= '
    /^[[:space:]]*\[/ { section=$0; gsub(/^[[:space:]]*\[|[[:space:]]*\].*$/, "", section) }
    section == "路径设置" && $1 ~ /^[[:space:]]*产物根目录[[:space:]]*$/ {
      sub(/^[^=]*=/, ""); gsub(/^[[:space:]]+|[[:space:]]+$/, ""); print; exit
    }
  ' "$CONFIG_FILE")"
  LOG_DIR="$(awk -F= '
    /^[[:space:]]*\[/ { section=$0; gsub(/^[[:space:]]*\[|[[:space:]]*\].*$/, "", section) }
    section == "路径设置" && $1 ~ /^[[:space:]]*日志目录[[:space:]]*$/ {
      sub(/^[^=]*=/, ""); gsub(/^[[:space:]]+|[[:space:]]+$/, ""); print; exit
    }
  ' "$CONFIG_FILE")"
fi
if [ -z "$MEDIA_ROOT" ]; then
  MEDIA_ROOT="$HOME/Documents/尊者产物"
else
  case "$MEDIA_ROOT" in
    \~) MEDIA_ROOT="$HOME" ;;
    \~/*) MEDIA_ROOT="$HOME/${MEDIA_ROOT#\~/}" ;;
    /*) ;;
    *) MEDIA_ROOT="$PROJECT_DIR/$MEDIA_ROOT" ;;
  esac
fi
if [ -z "$LOG_DIR" ]; then
  LOG_DIR="$MEDIA_ROOT/logs"
else
  case "$LOG_DIR" in
    \~) LOG_DIR="$HOME" ;;
    \~/*) LOG_DIR="$HOME/${LOG_DIR#\~/}" ;;
    /*) ;;
    *) LOG_DIR="$PROJECT_DIR/$LOG_DIR" ;;
  esac
fi
mkdir -p "$LOG_DIR"

render_plist() {
  sed -e "s|__PROJECT_DIR__|$PROJECT_DIR|g" \
      -e "s|__LOG_DIR__|$LOG_DIR|g" \
      -e "s|__HOME__|$HOME|g" "$1"
}
render_plist "$PROJECT_DIR/scripts/launchd/com.zunzhe.monitor.plist" > "$LAUNCH_AGENTS/com.zunzhe.monitor.plist"
render_plist "$PROJECT_DIR/scripts/launchd/com.zunzhe.healthcheck.plist" > "$LAUNCH_AGENTS/com.zunzhe.healthcheck.plist"
