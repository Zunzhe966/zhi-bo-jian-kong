#!/bin/bash
# 只读审计 launchd 日志落点：配置目录、磁盘 plist、当前已加载任务必须一致。
#
# 当前项目曾出现三种状态同时存在：config.ini 指向外置产物 logs，磁盘 plist
# 仍指项目 logs，而已加载 launchd 仍写 ~/Library/Logs/zunzhe。本脚本不重启、
# 不 reload、不修改任何文件，只把漂移如实报告出来，供 doctor/运维定位。
set -euo pipefail

ROOT="${ZUNZHE_PROJECT_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
CONFIG_FILE="$ROOT/config/config.ini"
[ -f "$CONFIG_FILE" ] || CONFIG_FILE="$ROOT/config/config.ini.example"
LAUNCH_AGENTS="${ZUNZHE_LAUNCH_AGENTS:-$HOME/Library/LaunchAgents}"
PLIST="$LAUNCH_AGENTS/com.zunzhe.monitor.plist"
LABEL="${ZUNZHE_LAUNCHD_LABEL:-com.zunzhe.monitor}"

if [ ! -f "$CONFIG_FILE" ]; then
  printf 'FAIL: 找不到 %s\n' "$CONFIG_FILE" >&2
  exit 2
fi

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

expand_path() {
  local value="$1"
  local base="$2"
  case "$value" in
    "~") printf '%s' "$HOME" ;;
    "~/"*) printf '%s/%s' "$HOME" "${value#\~/}" ;;
    /*) printf '%s' "$value" ;;
    *) printf '%s/%s' "$base" "$value" ;;
  esac
}

if [ -z "$MEDIA_ROOT" ]; then
  MEDIA_ROOT="$HOME/Documents/尊者产物"
else
  MEDIA_ROOT="$(expand_path "$MEDIA_ROOT" "$ROOT")"
fi
if [ -z "$LOG_DIR" ]; then
  CONFIG_LOG_DIR="$MEDIA_ROOT/logs"
else
  CONFIG_LOG_DIR="$(expand_path "$LOG_DIR" "$ROOT")"
fi

if [ ! -f "$PLIST" ]; then
  printf 'FAIL: 未安装 launchd plist: %s\n' "$PLIST" >&2
  exit 2
fi
PLIST_OUT="$(plutil -extract StandardOutPath raw "$PLIST" 2>/dev/null || true)"
PLIST_ERR="$(plutil -extract StandardErrorPath raw "$PLIST" 2>/dev/null || true)"
PLIST_LOG_DIR="$(dirname "$PLIST_OUT")"

UID_VALUE="$(id -u)"
LOADED_OUT="$(launchctl print "gui/$UID_VALUE/$LABEL" 2>/dev/null | awk -F' = ' '/stdout path =/ {sub(/^[^=]*= /, ""); print; exit}')"
LOADED_ERR="$(launchctl print "gui/$UID_VALUE/$LABEL" 2>/dev/null | awk -F' = ' '/stderr path =/ {sub(/^[^=]*= /, ""); print; exit}')"
LOADED_LOG_DIR="$(dirname "$LOADED_OUT")"

printf 'configured_log_dir=%s\n' "$CONFIG_LOG_DIR"
printf 'plist_log_dir=%s\n' "$PLIST_LOG_DIR"
printf 'loaded_log_dir=%s\n' "$LOADED_LOG_DIR"
printf 'configured_stdout=%s\n' "$CONFIG_LOG_DIR/zunzhe-core.log"
printf 'configured_stderr=%s\n' "$CONFIG_LOG_DIR/zunzhe-core.err.log"
printf 'plist_stdout=%s\n' "$PLIST_OUT"
printf 'plist_stderr=%s\n' "$PLIST_ERR"
printf 'loaded_stdout=%s\n' "$LOADED_OUT"
printf 'loaded_stderr=%s\n' "$LOADED_ERR"

if [ "$PLIST_LOG_DIR" != "$CONFIG_LOG_DIR" ]; then
  printf 'FAIL: 磁盘 plist 日志目录与配置不一致，重新运行 scripts/setup-macos.sh 渲染并重载。\n' >&2
  exit 1
fi
if [ "$LOADED_LOG_DIR" != "$CONFIG_LOG_DIR" ]; then
  printf 'FAIL: 当前已加载 launchd 仍指向旧日志目录；需要重载 com.zunzhe.monitor。\n' >&2
  exit 1
fi

printf 'PASS: 配置、磁盘 plist、已加载 launchd 的日志目录一致。\n'
