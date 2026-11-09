#!/usr/bin/env bash
# Compatibilidad: el empaquetado real (icono, extensiones, firma, DMG) vive en
# packaging/macos/bundle.sh. Este wrapper existe para no dejar dos rutas que se
# desincronicen.
#
# Uso: EXTENSIONS_DIR=dist/extensions scripts/package-macos-app.sh [target] [version]
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target="${1:-aarch64-apple-darwin}"
version="${2:-0.1.0}"

echo "package-macos-app.sh delega en packaging/macos/bundle.sh" >&2
exec "$repo_root/packaging/macos/bundle.sh" "$target" "$version"
