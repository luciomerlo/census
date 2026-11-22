#!/usr/bin/env bash
# Empaqueta Census.app. Si existe una identidad de firma, firma con Developer ID;
# si no, firma ad-hoc para probar en la Mac local.
set -euo pipefail
cd "$(dirname "$0")/.."

ARCH="${1:-universal}"
BIN="dist/census-universal"
[ "$ARCH" = "x86_64" ] && BIN="dist/census-x86_64"
[ "$ARCH" = "aarch64" ] && BIN="dist/census-aarch64"
[ -x "$BIN" ] || { echo "falta $BIN; corré scripts/make-universal.sh o `--both`" >&2; exit 1; }

APP="dist/Census.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp packaging/Info.plist "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/census"
chmod +x "$APP/Contents/MacOS/census"
if [ -f assets/icon.icns ]; then
  cp assets/icon.icns "$APP/Contents/Resources/icon.icns"
fi

if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
  codesign --force --options runtime --timestamp \
    --sign "$MACOS_SIGN_IDENTITY" \
    --entitlements packaging/entitlements.plist \
    "$APP"
else
  echo "aviso: sin MACOS_SIGN_IDENTITY; firma ad-hoc (no notarizable)"
  codesign --force --deep --sign - "$APP"
fi
codesign --verify --deep --strict --verbose=2 "$APP"
echo "listo: $APP"
