#!/usr/bin/env bash
# Compila Census universal2 desde Linux/WSL, sin Mac.
# Requiere: python -m pip install --user cargo-zigbuild ziglang
#           rustup target add x86_64-apple-darwin aarch64-apple-darwin
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo-zigbuild >/dev/null; then
  user_scripts="$(python -c "import sysconfig; print(sysconfig.get_path('scripts', scheme='posix_user'))" 2>/dev/null || true)"
  [ -n "$user_scripts" ] && [ -d "$user_scripts" ] && PATH="$user_scripts:$PATH"
fi
command -v cargo-zigbuild >/dev/null || {
  echo "falta cargo-zigbuild; instalá: python -m pip install --user cargo-zigbuild ziglang" >&2
  exit 1
}

if ! command -v zig >/dev/null && [ -z "${CARGO_ZIGBUILD_ZIG_PATH:-}" ]; then
  zig_path="$(python -c 'import pathlib, ziglang; print(pathlib.Path(ziglang.__file__).parent / "zig")')"
  export CARGO_ZIGBUILD_ZIG_PATH="$zig_path"
fi

rustup target add x86_64-apple-darwin aarch64-apple-darwin
cargo zigbuild --release --target universal2-apple-darwin

bin="target/universal2-apple-darwin/release/census"
ls -l "$bin"
if command -v llvm-objdump >/dev/null; then
  llvm-objdump --macho --universal-headers "$bin"
elif command -v file >/dev/null; then
  file "$bin"
fi
