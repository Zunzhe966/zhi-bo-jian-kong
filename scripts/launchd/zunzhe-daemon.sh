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

# 项目可能从桌面迁到 ~/Projects 等非保护目录；优先用 launchd plist 的
# WorkingDirectory 参数（由 setup 写入），其次按当前目录/常见位置探测。
ROOT="$(pwd)"
# 探测顺序：新目录优先（即使 plist 还残留旧桌面 WorkingDirectory），
# 再退回当前目录，最后才是桌面目录（桌面只用于给出明确错误提示）。
for candidate in "$HOME/Projects/尊者监控直播短视频" "$HOME/Documents/尊者监控直播短视频" "$ROOT" "$HOME/Desktop/尊者监控直播短视频"; do
  if [ -f "$candidate/config/config.ini.example" ] || [ -f "$candidate/config/config.ini" ]; then
    ROOT="$candidate"
    break
  fi
done

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

if [ "$ROOT" != "$(pwd)" ]; then
  echo "[rust-daemon] 项目根定位为 $ROOT（当前目录 $(pwd) 不包含项目配置）" >&2
fi

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:$PATH"
mkdir -p "$HOME/Library/Logs/zunzhe"
echo "[rust-daemon] start $(date '+%Y-%m-%d %H:%M:%S %z') project=$ROOT" >&2
export RUST_LOG="${RUST_LOG:-info}"
exec "$ROOT/zunzhe-core/target/release/zunzhe-core" run-daemon "$@"
