#!/bin/bash
# Cross-compile and link every test target with the Zig 0.16 toolchain.
#
# Requirements:
#   Zig 0.16 at "$HOME/.local/opt/zig-aarch64-macos-0.16.0/zig"
#   Rust target x86_64-unknown-linux-gnu and x86_64-pc-windows-gnu installed
#   scripts/zig-cross/setup-zig-cross.sh run once
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

export PATH="$HOME/.cargo/bin:$PATH"
ZIG_BIN="${ZIG_BIN:-$HOME/.local/opt/zig-aarch64-macos-0.16.0/zig}"
if [[ ! -x "$ZIG_BIN" ]]; then
  echo "zig 0.16 not found at $ZIG_BIN (set ZIG_BIN to override)" >&2
  exit 1
fi
export ZIG_CC="$ZIG_BIN"

export CC_x86_64_unknown_linux_gnu="$HOME/.local/bin/zig-cc-linux"
export AR_x86_64_unknown_linux_gnu="$HOME/.local/bin/zig-ar-linux"
export RANLIB_x86_64_unknown_linux_gnu="$HOME/.local/bin/zig-ar-linux"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="$HOME/.local/bin/zig-ld-linux"
export CC_x86_64_pc_windows_gnu="$HOME/.local/bin/zig-cc-windows"
export AR_x86_64_pc_windows_gnu="$HOME/.local/bin/zig-ar-windows"
export RANLIB_x86_64_pc_windows_gnu="$HOME/.local/bin/zig-ar-windows"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="$ROOT/scripts/zig-cross/zig-ld-windows"
export RUSTFLAGS="-C dlltool=$HOME/.local/bin/zig-dlltool-windows"

echo "== linux cross link =="
cargo test --all-targets --no-run --target x86_64-unknown-linux-gnu

echo "== windows cross link =="
cargo test --all-targets --no-run --target x86_64-pc-windows-gnu
