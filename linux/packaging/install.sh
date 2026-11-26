#!/usr/bin/env bash
# Instala el binario y la unidad de usuario. No habilita el servicio solo.
set -euo pipefail
cd "$(dirname "$0")"
PREFIX="${PREFIX:-$HOME/.local}"
install -Dm755 censusindex "$PREFIX/bin/censusindex"
install -Dm644 censusindex.service "$HOME/.config/systemd/user/censusindex.service"
echo "instalado: $PREFIX/bin/censusindex"
echo "siguiente:"
echo "  censusindex index"
echo "  censusindex serve --port 8787"
echo "  systemctl --user daemon-reload && systemctl --user enable --now censusindex.service"
