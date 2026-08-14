#!/bin/bash
# 容器内启动 zunzhe-core Rust daemon（Linux 云服务器/镜像默认入口）。
# 只做路径准备和 exec，不把 daemon 放进后台；容器退出即服务退出，
# 由 Docker `restart: always` 负责自动拉起。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

mkdir -p "$ROOT/logs" "$ROOT/fupan_output/run" "$ROOT/downloads"

export PATH="$PATH:/usr/local/bin"
export RUST_LOG="${RUST_LOG:-info}"

exec zunzhe-core run-daemon "$@"
