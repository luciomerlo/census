#!/usr/bin/env bash
# Firma Developer ID y notariza Census.app. Requiere:
#   MACOS_SIGN_IDENTITY, APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD
set -euo pipefail
cd "$(dirname "$0")/.."

for v in MACOS_SIGN_IDENTITY APPLE_ID APPLE_TEAM_ID APPLE_APP_PASSWORD; do
  [ -n "${!v:-}" ] || { echo "falta $v" >&2; exit 1; }
done

./scripts/package.sh "${1:-universal}"

APP="dist/Census.app"
ZIP="dist/Census-notarize.zip"
ditto -c -k --keepParent "$APP" "$ZIP"
xcrun notarytool submit "$ZIP" \
  --apple-id "$APPLE_ID" \
  --team-id "$APPLE_TEAM_ID" \
  --password "$APPLE_APP_PASSWORD" \
  --wait
xcrun stapler staple "$APP"
spctl --assess --type execute --verbose=4 "$APP"
echo "listo y notarizado: $APP"
