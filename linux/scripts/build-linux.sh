#!/usr/bin/env bash
# Construye Census para Intel/AMD y ARM64 en Linux (binarios estaticos musl).
# En Windows/macOS usa rust-lld; en Linux usa el linker nativo.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
HOST="$(rustc -vV | sed -n 's/^host: //p')"
if [[ "$(uname -s)" != "Linux" ]]; then
  SYSLIB="$(rustc --print sysroot)/lib/rustlib/${HOST}/bin"
  export PATH="${SYSLIB}:${PATH}"
  export RUSTFLAGS="${RUSTFLAGS:-} -C linker=rust-lld"
fi

TARGETS=("$@")
if [[ ${#TARGETS[@]} -eq 0 ]]; then
  TARGETS=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl)
fi

rm -rf dist
mkdir -p dist
for target in "${TARGETS[@]}"; do
  rustup target add "$target" >/dev/null 2>&1 || true
  echo "==> $target"
  cargo build --release --target "$target"
  case "$target" in
    x86_64-unknown-linux-*) arch=x86_64 ;;
    aarch64-unknown-linux-*) arch=aarch64 ;;
    *) arch="${target%%-*}" ;;
  esac
  stage="$(mktemp -d)"
  cp "target/$target/release/census" "$stage/census"
  cp README.md "$stage/README.md"
  cp packaging/census.service "$stage/census.service"
  cp packaging/install.sh "$stage/install.sh"
  tar -C "$stage" -czf "dist/census-${VERSION}-linux-${arch}.tar.gz" .
  rm -rf "$stage"
  echo "    dist/census-${VERSION}-linux-${arch}.tar.gz"
done
( cd dist && sha256sum ./*.tar.gz > SHA256SUMS.txt )
cat dist/SHA256SUMS.txt
