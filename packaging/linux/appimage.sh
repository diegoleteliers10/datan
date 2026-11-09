#!/usr/bin/env bash
# Arma el AppImage de Datan para Linux x86_64.
#
# Uso:
#   EXTENSIONS_DIR=dist/extensions packaging/linux/appimage.sh <target-triple> <version>
#
# Salida: dist/Datan_<version>_amd64.AppImage
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "uso: EXTENSIONS_DIR=<dir> $0 <target-triple> <version>" >&2
  exit 2
fi

TARGET="$1"
VERSION="$2"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIST="$ROOT/dist"
APPDIR="$DIST/AppDir"
BIN="$ROOT/target/$TARGET/release/datan-app"
EXTENSIONS_DIR="${EXTENSIONS_DIR:-}"

[[ -f "$BIN" ]] || { echo "ERROR: no existe $BIN." >&2; exit 1; }
[[ -n "$EXTENSIONS_DIR" && -d "$EXTENSIONS_DIR" ]] || {
  echo "ERROR: EXTENSIONS_DIR debe apuntar al arbol de extensiones de DuckDB." >&2
  exit 1
}

rm -rf "$APPDIR"
mkdir -p \
  "$APPDIR/usr/bin" \
  "$APPDIR/usr/lib/datan" \
  "$APPDIR/usr/share/applications" \
  "$APPDIR/usr/share/icons/hicolor/512x512/apps"

cp "$BIN" "$APPDIR/usr/bin/datan"
chmod 755 "$APPDIR/usr/bin/datan"
cp -R "$EXTENSIONS_DIR"/. "$APPDIR/usr/lib/datan/extensions/"
cp "$ROOT/packaging/linux/datan.desktop" "$APPDIR/usr/share/applications/datan.desktop"
cp "$ROOT/assets/datan-512.png" "$APPDIR/usr/share/icons/hicolor/512x512/apps/datan.png"

if ! find "$APPDIR/usr/lib/datan/extensions" -name '*.duckdb_extension' -print -quit | grep -q .; then
  echo "ERROR: no se encontro ningun .duckdb_extension en $EXTENSIONS_DIR." >&2
  exit 1
fi

TOOLS="$DIST/appimage-tools"
mkdir -p "$TOOLS"
if [[ ! -x "$TOOLS/linuxdeploy" ]]; then
  echo "==> Descargando linuxdeploy"
  curl -fsSL -o "$TOOLS/linuxdeploy" \
    "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage"
  curl -fsSL -o "$TOOLS/linuxdeploy-plugin-appimage" \
    "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/continuous/linuxdeploy-plugin-appimage-x86_64.AppImage"
  chmod +x "$TOOLS/linuxdeploy" "$TOOLS/linuxdeploy-plugin-appimage"
fi

export APPIMAGE_EXTRACT_AND_RUN=1
export PATH="$TOOLS:$PATH"

cd "$DIST"
rm -f Datan*.AppImage
"$TOOLS/linuxdeploy" \
  --appdir "$APPDIR" \
  --executable "$APPDIR/usr/bin/datan" \
  --desktop-file "$APPDIR/usr/share/applications/datan.desktop" \
  --icon-file "$APPDIR/usr/share/icons/hicolor/512x512/apps/datan.png" \
  --icon-filename datan \
  --output appimage

if [[ ! -x "$APPDIR/usr/bin/datan" ]]; then
  echo "ERROR: linuxdeploy no dejo usr/bin/datan en el AppDir." >&2
  exit 1
fi

shopt -s nullglob
built=(Datan*.AppImage)
shopt -u nullglob
if [[ ${#built[@]} -eq 0 ]]; then
  echo "ERROR: linuxdeploy no genero ningun AppImage." >&2
  exit 1
fi

out="$DIST/Datan_${VERSION}_amd64.AppImage"
mv -f "${built[0]}" "$out"
echo "OK: $out"
