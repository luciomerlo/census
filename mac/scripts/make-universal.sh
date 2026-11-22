#!/usr/bin/env bash
# Crea un binario universal2 (Intel + Apple silicon) con lipo.
set -euo pipefail
cd "$(dirname "$0")/.."

rustup target add x86_64-apple-darwin aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
mkdir -p dist
lipo -create \
  target/x86_64-apple-darwin/release/census \
  target/aarch64-apple-darwin/release/census \
  -output dist/census-universal
chmod +x dist/census-universal
lipo -info dist/census-universal
