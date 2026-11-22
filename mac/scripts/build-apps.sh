#!/usr/bin/env bash
# Compila y empaqueta Census.app para Intel, Apple Silicon y universal2.
# Sólo macOS: usa lipo, ditto, codesign e iconutil.
set -euo pipefail
cd "$(dirname "$0")/.."

[ "$(uname -s)" = "Darwin" ] || {
  echo "build-apps: se ejecuta en macOS" >&2
  exit 1
}

rustup target add x86_64-apple-darwin aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin

mkdir -p dist
cp target/x86_64-apple-darwin/release/census dist/census-x86_64
cp target/aarch64-apple-darwin/release/census dist/census-aarch64
chmod +x dist/census-x86_64 dist/census-aarch64

# Universal2: un solo ejecutable que corre en ambas arquitecturas.
lipo -create \
  dist/census-x86_64 \
  dist/census-aarch64 \
  -output dist/census-universal
chmod +x dist/census-universal

./scripts/make-icon.sh
CENSUS_APP_NAME=Census-Intel ./scripts/package.sh x86_64
CENSUS_APP_NAME=Census-AppleSilicon ./scripts/package.sh aarch64
CENSUS_APP_NAME=Census-universal ./scripts/package.sh universal

for app in Census-Intel Census-AppleSilicon Census-universal; do
  ditto -c -k --sequesterRsrc --keepParent "dist/$app.app" "dist/$app.zip"
done

(cd dist && shasum -a 256 Census-Intel.zip Census-AppleSilicon.zip Census-universal.zip > SHA256SUMS.txt)
cat dist/SHA256SUMS.txt
