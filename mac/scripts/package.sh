#!/usr/bin/env bash
# Empaqueta Census.app. Si existe una identidad de firma, firma con Developer ID;
# si no, firma ad-hoc para probar en la Mac local.
set -euo pipefail
cd "$(dirname "$0")/.."

ARCH="${1:-universal}"
case "$ARCH" in
  universal) BIN="dist/census-universal" ;;
  x86_64)    BIN="dist/census-x86_64" ;;
  aarch64)   BIN="dist/census-aarch64" ;;
  *)
    echo "uso: $0 [universal|x86_64|aarch64]" >&2
    exit 2
    ;;
esac
# El CI puede pasar el binario ya compilado sin copiarlo a mac/dist.
BIN="${CENSUS_BIN:-$BIN}"
[ -x "$BIN" ] || { echo "falta $BIN; corré scripts/make-universal.sh o --both" >&2; exit 1; }

# Nombre distinto por arquitectura para no pisar una app con otra.
APP_NAME="${CENSUS_APP_NAME:-Census}"
APP="dist/$APP_NAME.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp packaging/Info.plist "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/census"
chmod +x "$APP/Contents/MacOS/census"
if [ -f dist/icon.icns ]; then
  cp dist/icon.icns "$APP/Contents/Resources/icon.icns"
elif [ -f assets/icon.icns ]; then
  cp assets/icon.icns "$APP/Contents/Resources/icon.icns"
elif [ -f ../assets/icon.icns ]; then
  cp ../assets/icon.icns "$APP/Contents/Resources/icon.icns"
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
file "$APP/Contents/MacOS/census" 2>/dev/null || true
