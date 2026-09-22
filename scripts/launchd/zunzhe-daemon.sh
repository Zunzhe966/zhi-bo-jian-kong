#!/bin/bash
# 启动 zunzhe-core Rust 守护进程（24×30 常驻）。
# 安装位置：~/Library/LaunchAgents/zunzhe-daemon.sh。
#
# macOS 系统策略会拦截 launchd 直接加载/读取 ~/Desktop 下的未签名二进制，
# 即使把二进制复制到 ~/Library/Application Support 也不行，因为脚本还要
# 读取桌面目录里的 release 二进制、项目配置和 pid。24×30 长期稳定必须把
# 整个项目放在非 TCC 保护目录（推荐 ~/Projects/尊者监控直播短视频）。
# 脚本启动时会探测项目位置并给出一行可读错误，不会反复把 launchd 拖入
# 失败循环。
set -euo pipefail

# 项目可能从桌面迁到 ~/Projects 等非保护目录。必须先探测显式候选路径，
# 不能先调用 pwd：launchd 若继承了已删除或不可访问的 WorkingDirectory，
# 在 set -e 下 `pwd` 会直接终止脚本，根本走不到后面的候选探测。
# 安装时固化的项目根优先。不能用候选路径猜：项目目录曾经常被放进
# ~/Projects，而该路径可能只是指向真实仓库的符号链接；一旦猜中链接，
# 旧 release 与当前配置就会各写一份 DB/日志。
ROOT="${ZUNZHE_PROJECT_ROOT:-}"

canonical_root() {
  local candidate="$1"
  if command -v python3 >/dev/null 2>&1; then
    python3 - "$candidate" <<'PY'
import os
import sys

print(os.path.realpath(sys.argv[1]))
PY
    return 0
  fi
  (cd "$candidate" 2>/dev/null && pwd -P) || printf '%s' "$candidate"
}

if [ -z "$ROOT" ]; then
  current="$(pwd 2>/dev/null || true)"
  if [ -n "$current" ] && { [ -f "$current/config/config.ini.example" ] || [ -f "$current/config/config.ini" ]; }; then
    ROOT="$current"
  fi
fi

if [ -n "$ROOT" ]; then
  ROOT="$(canonical_root "$ROOT")"
fi

case "$ROOT" in
  /*) ;;
  *)
    echo "[rust-daemon] ZUNZHE_PROJECT_ROOT 必须是绝对路径: $ROOT" >&2
    exit 1
    ;;
esac

if [ -z "$ROOT" ]; then
  # 兼容升级前未把 ZUNZHE_PROJECT_ROOT 写进 plist 的安装。只作为最后退路，
  # 且优先 Documents/Desktop，避免再次默认踩中 ~/Projects 链接。
  for candidate in "$HOME/Documents/尊者监控直播短视频" "$HOME/Desktop/尊者监控直播短视频"; do
    if [ -f "$candidate/config/config.ini.example" ] || [ -f "$candidate/config/config.ini" ]; then
      ROOT="$candidate"
      break
    fi
  done
fi

if [ ! -f "$ROOT/config/config.ini.example" ] && [ ! -f "$ROOT/config/config.ini" ]; then
  echo "[rust-daemon] 找不到项目根（需要 config/config.ini(.example)）" >&2
  exit 1
fi

case "$ROOT" in
  "$HOME/Desktop/"*)
    echo "[rust-daemon] 项目在 $ROOT：macOS TCC 会阻止 launchd 稳定运行。请把项目迁移到 ~/Projects 或 ~/Documents 后再注册 launchd。" >&2
    exit 1
    ;;
esac

current="$(pwd 2>/dev/null || true)"
if [ -z "$current" ] || [ "$ROOT" != "$current" ]; then
  display_current="$current"
  if [ -z "$display_current" ]; then
    display_current="不可访问"
  fi
  echo "[rust-daemon] 项目根定位为 ${ROOT}（当前目录 ${display_current}）" >&2
fi

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:$PATH"
# plist 的 StandardOutPath 指向配置日志目录；渲染脚本会先创建它。
# 这里不再创建 ROOT/logs，避免默认布局下继续污染源码目录。
echo "[rust-daemon] start $(date '+%Y-%m-%d %H:%M:%S %z') project=$ROOT" >&2
export RUST_LOG="${RUST_LOG:-info}"
cd "$ROOT"
exec "$ROOT/zunzhe-core/target/release/zunzhe-core" run-daemon "$@"
