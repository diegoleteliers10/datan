#!/usr/bin/env bash
# Regenera los iconos de assets/ desde la identidad oficial en
# branding/datan-identity/.
#
# Requiere macOS (sips + iconutil) y Python con Pillow para el .ico.
# Los archivos generados se commitean: el CI no corre este script.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE="$ROOT/branding/datan-identity/digital/mono/datan-app-icon-1024.png"
ICNS="$ROOT/branding/datan-identity/digital/macos/Datan.icns"
SVG="$ROOT/branding/datan-identity/digital/mono/datan-app-icon.svg"

[[ -f "$SOURCE" ]] || { echo "ERROR: falta $SOURCE" >&2; exit 1; }
[[ -f "$ICNS" ]] || { echo "ERROR: falta $ICNS" >&2; exit 1; }

echo "==> Copiando fuente oficial"
cp "$SOURCE" "$ROOT/assets/icons/datan-1024.png"
cp "$SVG" "$ROOT/assets/icons/datan-icon.svg"
cp "$ICNS" "$ROOT/assets/datan.icns"

echo "==> Generando PNGs"
sips -z 512 512 "$SOURCE" --out "$ROOT/assets/datan-512.png" >/dev/null
sips -z 256 256 "$SOURCE" --out "$ROOT/assets/datan-256.png" >/dev/null

echo "==> Generando .ico"
python3 - "$ROOT" <<'PY'
import sys
from PIL import Image

root = sys.argv[1]
img = Image.open(f"{root}/assets/icons/datan-1024.png")
img.save(
    f"{root}/assets/datan.ico",
    sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
)
PY

echo "OK: assets/datan.icns, assets/datan.ico, assets/datan-512.png, assets/datan-256.png"
