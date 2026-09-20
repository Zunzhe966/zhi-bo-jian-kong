#!/bin/bash
# 恢复/重装 zunzhe launchd 任务。
#
# 什么时候需要它：launchctl bootstrap/bootout 在某些受限终端环境
# （沙箱、TCC、远程会话）会返回 "Bootstrap failed: 5: Input/output error"，
# 而 GUI 内的「终端.app」不受此限制。任务一旦从 domain 里移除，
# 只能由真实终端重新加载。
#
# 用法：在「终端.app」里执行
#   bash "/Volumes/A区/尊者监控直播短视频/scripts/restore-launchd.sh"
set -uo pipefail
UID_NUM="$(id -u)"
AGENTS="$HOME/Library/LaunchAgents"
LABELS="com.zunzhe.monitor com.zunzhe.healthcheck"

echo "== 恢复 zunzhe launchd 任务 =="
for label in $LABELS; do
  plist="$AGENTS/$label.plist"
  if [ ! -f "$plist" ]; then
    echo "  ✗ 缺少 $plist"
    continue
  fi
  # 已加载则先卸掉，保证用的是磁盘上的最新 plist
  if launchctl print "gui/$UID_NUM/$label" >/dev/null 2>&1; then
    launchctl bootout "gui/$UID_NUM/$label" 2>/dev/null && echo "  · 已卸载旧实例 $label"
  fi
  if launchctl bootstrap "gui/$UID_NUM" "$plist" 2>/dev/null; then
    echo "  ✓ 已加载 $label"
  elif launchctl load -w "$plist" 2>/dev/null; then
    echo "  ✓ 已加载 $label（launchctl load 兼容路径）"
  else
    echo "  ✗ 加载失败 $label —— 请确认是在「终端.app」里运行，且已授予终端完全磁盘访问权限"
  fi
done

echo
echo "== 结果 =="
launchctl list | grep zunzhe || echo "  (没有 zunzhe 任务在运行)"
