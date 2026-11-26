#!/usr/bin/env bash
# Compila CensusIndex para macOS. Por defecto compila la arquitectura de esta Mac;
# con --both compila Intel y Apple silicon; con --universal deja universal2.
set -euo pipefail
cd "$(dirname "$0")/.."

MODE="${1:-host}"
case "$MODE" in
  host)
    cargo build --release
    OUT="target/release/censusindex"
    ;;
  --both|both)
    rustup target add x86_64-apple-darwin aarch64-apple-darwin
    cargo build --release --target x86_64-apple-darwin
    cargo build --release --target aarch64-apple-darwin
    mkdir -p dist
    cp target/x86_64-apple-darwin/release/censusindex dist/censusindex-x86_64
    cp target/aarch64-apple-darwin/release/censusindex dist/censusindex-aarch64
    OUT="dist/censusindex-x86_64 y dist/censusindex-aarch64"
    ;;
  --universal|universal)
    ./scripts/make-universal.sh
    OUT="dist/censusindex-universal"
    ;;
  *)
    echo "uso: $0 [host|--both|--universal]" >&2
    exit 2
    ;;
esac

echo "listo: $OUT"
file "$OUT" 2>/dev/null || true
