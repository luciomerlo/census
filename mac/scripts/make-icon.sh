#!/usr/bin/env bash
# Genera dist/icon.icns a partir del PNG fuente. Sólo macOS (sips + iconutil).
set -euo pipefail
cd "$(dirname "$0")/.."

SRC="${1:-../assets/icon-256.png}"
OUT="${2:-dist/icon.icns}"

[ "$(uname -s)" = "Darwin" ] || {
  echo "make-icon: se ejecuta en macOS" >&2
  exit 1
}
[ -f "$SRC" ] || {
  echo "make-icon: falta $SRC" >&2
  exit 1
}

ICONSET="dist/icon.iconset"
rm -rf "$ICONSET"
mkdir -p "$ICONSET" "$(dirname "$OUT")"

# Los nombres son los que exige iconutil. Se escala desde el PNG fuente.
while read -r size name; do
  sips -z "$size" "$size" "$SRC" --out "$ICONSET/$name" >/dev/null
done <<'EOF'
16 icon_16x16.png
32 icon_16x16@2x.png
32 icon_32x32.png
64 icon_32x32@2x.png
128 icon_128x128.png
256 icon_128x128@2x.png
256 icon_256x256.png
512 icon_256x256@2x.png
512 icon_512x512.png
1024 icon_512x512@2x.png
EOF

iconutil -c icns "$ICONSET" -o "$OUT"
rm -rf "$ICONSET"
echo "listo: $OUT"
