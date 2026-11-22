#!/usr/bin/env bash
# Instala el binario y la unidad de usuario. No habilita el servicio solo.
set -euo pipefail
cd "$(dirname "$0")"
PREFIX="${PREFIX:-$HOME/.local}"
install -Dm755 census "$PREFIX/bin/census"
install -Dm644 census.service "$HOME/.config/systemd/user/census.service"
echo "instalado: $PREFIX/bin/census"
echo "siguiente:"
echo "  census index"
echo "  census serve --port 8787"
echo "  systemctl --user daemon-reload && systemctl --user enable --now census.service"
