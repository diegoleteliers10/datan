#!/usr/bin/env bash
# Arma Datan.app, el .dmg y el .tar.gz de macOS.
#
# Uso:
#   EXTENSIONS_DIR=dist/extensions packaging/macos/bundle.sh <target-triple> <version>
#
# Variables opcionales:
#   MACOS_SIGNING_IDENTITY   "Developer ID Application: ..." (sin esto firma ad-hoc)
#   MACOS_NOTARY_PROFILE     perfil de notarytool ya guardado en el keychain
#   MACOS_NOTARY_APPLE_ID    alternativa al perfil: Apple ID
#   MACOS_NOTARY_TEAM_ID     alternativa al perfil: Team ID
#   MACOS_NOTARY_PASSWORD    alternativa al perfil: app-specific password
#
# Salidas:
#   dist/Datan.app
#   dist/datan-<target>.tar.gz
#   dist/datan-<target>.dmg
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "uso: EXTENSIONS_DIR=<dir> $0 <target-triple> <version>" >&2
  exit 2
fi

TARGET="$1"
VERSION="$2"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIST="$ROOT/dist"
APP="$DIST/Datan.app"
BIN="$ROOT/target/$TARGET/release/datan-app"
EXTENSIONS_DIR="${EXTENSIONS_DIR:-}"

[[ -f "$BIN" ]] || { echo "ERROR: no existe $BIN. Corre cargo build --release --target $TARGET -p datan-app --bin datan-app." >&2; exit 1; }
[[ -n "$EXTENSIONS_DIR" && -d "$EXTENSIONS_DIR" ]] || {
  echo "ERROR: EXTENSIONS_DIR debe apuntar al arbol de extensiones de DuckDB (v<version>/<platform>/...)." >&2
  echo "       Genera uno con: scripts/fetch-duckdb-extension.sh osx_arm64 dist/extensions" >&2
  exit 1
}

echo "==> Armando $APP (version $VERSION)"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/Datan"
chmod 755 "$APP/Contents/MacOS/Datan"
cp "$ROOT/assets/datan.icns" "$APP/Contents/Resources/Datan.icns"
sed "s/__VERSION__/${VERSION}/g" "$ROOT/packaging/macos/Info.plist" > "$APP/Contents/Info.plist"

# La app busca las extensiones en Contents/Resources/extensions. DuckDB agrega
# v<version>/<platform>/ debajo de esa raiz, asi que se copia el arbol completo.
mkdir -p "$APP/Contents/Resources/extensions"
cp -R "$EXTENSIONS_DIR"/. "$APP/Contents/Resources/extensions/"
if ! find "$APP/Contents/Resources/extensions" -name '*.duckdb_extension' -print -quit | grep -q .; then
  echo "ERROR: no se encontro ningun .duckdb_extension en $EXTENSIONS_DIR." >&2
  exit 1
fi

echo "==> Firmando"
if [[ -n "${MACOS_SIGNING_IDENTITY:-}" ]]; then
  codesign --force --options runtime --timestamp --sign "$MACOS_SIGNING_IDENTITY" "$APP"
else
  echo "    Sin MACOS_SIGNING_IDENTITY: firma ad-hoc (Gatekeeper pedira aceptar la app)."
  codesign --force --sign - "$APP"
fi
codesign --verify --strict --verbose=2 "$APP"

echo "==> Empaquetando tar.gz"
tar -czf "$DIST/datan-$TARGET.tar.gz" -C "$DIST" Datan.app

echo "==> Creando dmg"
STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT
cp -R "$APP" "$STAGING/"
ln -s /Applications "$STAGING/Applications"
DMG="$DIST/datan-$TARGET.dmg"
rm -f "$DMG"
hdiutil create -volname "Datan" -srcfolder "$STAGING" -ov -format UDZO "$DMG" >/dev/null

if [[ -n "${MACOS_SIGNING_IDENTITY:-}" ]]; then
  codesign --force --sign "$MACOS_SIGNING_IDENTITY" "$DMG"
fi

PROFILE="${MACOS_NOTARY_PROFILE:-}"
if [[ -z "$PROFILE" && -n "${MACOS_NOTARY_APPLE_ID:-}" && -n "${MACOS_NOTARY_TEAM_ID:-}" && -n "${MACOS_NOTARY_PASSWORD:-}" ]]; then
  PROFILE="datan-notary"
  echo "==> Guardando credenciales de notarizacion en el keychain"
  xcrun notarytool store-credentials "$PROFILE" \
    --apple-id "$MACOS_NOTARY_APPLE_ID" \
    --team-id "$MACOS_NOTARY_TEAM_ID" \
    --password "$MACOS_NOTARY_PASSWORD"
fi

if [[ -n "$PROFILE" ]]; then
  if [[ -z "${MACOS_SIGNING_IDENTITY:-}" ]]; then
    echo "ERROR: la notarizacion necesita una identidad Developer ID, no una firma ad-hoc." >&2
    exit 1
  fi
  echo "==> Notarizando $DMG (puede tardar varios minutos)"
  xcrun notarytool submit "$DMG" --keychain-profile "$PROFILE" --wait
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
  spctl -a -vvv -t open --context context:primary-signature "$DMG"
fi

echo "OK: $APP"
echo "OK: $DIST/datan-$TARGET.tar.gz"
echo "OK: $DMG"
