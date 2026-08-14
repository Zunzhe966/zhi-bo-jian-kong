#!/bin/bash
# Install the Zig 0.16 cross toolchain adapters into ~/.local/bin so Cargo can
# find them. The adapters in this directory stay in the repository as the
# reproducible source of truth.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
ZIG_BIN="${ZIG_BIN:-$HOME/.local/opt/zig-aarch64-macos-0.16.0/zig}"
if [[ ! -x "$ZIG_BIN" ]]; then
  echo "zig 0.16 not found at $ZIG_BIN (set ZIG_BIN to override)" >&2
  exit 1
fi

mkdir -p "$HOME/.local/bin"
for name in zig-cc-linux zig-cc-windows zig-ar-linux zig-ar-windows \
            zig-dlltool-windows zig-ld-linux zig-ld-windows; do
  cp "$ROOT/$name" "$HOME/.local/bin/$name"
  chmod +x "$HOME/.local/bin/$name"
done

echo "installed zig cross adapters to $HOME/.local/bin (zig: $ZIG_BIN)"
