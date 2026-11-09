#!/usr/bin/env bash
# Descarga la extension de PostgreSQL de DuckDB para una plataforma y la deja
# en el layout que DuckDB espera: <dest>/v<version>/<platform>/.
#
# Uso:
#   scripts/fetch-duckdb-extension.sh <platform> <dest>
#
# Plataformas: osx_arm64, osx_amd64, linux_amd64, linux_arm64, windows_amd64.
# La version debe coincidir con la de DuckDB que compila `libduckdb-sys`
# (hoy 1.5.6, del crate duckdb 1.10506.x). Si actualizas DuckDB, actualiza
# DUCKDB_VERSION en el workflow y en docs/releasing.md.
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "uso: $0 <platform> <dest>" >&2
  exit 2
fi

PLATFORM="$1"
DEST="$2"
DUCKDB_VERSION="${DUCKDB_VERSION:-1.5.6}"
DIR="$DEST/v$DUCKDB_VERSION/$PLATFORM"
URL="https://extensions.duckdb.org/v$DUCKDB_VERSION/$PLATFORM/postgres_scanner.duckdb_extension.gz"

case "$PLATFORM" in
  osx_arm64|osx_amd64|linux_amd64|linux_arm64|windows_amd64) ;;
  *) echo "ERROR: plataforma desconocida '$PLATFORM'." >&2; exit 2 ;;
esac

mkdir -p "$DIR"
echo "==> Descargando $URL"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
curl -fsSL --retry 3 --retry-delay 2 -o "$tmp" "$URL"
gunzip -c "$tmp" > "$DIR/postgres_scanner.duckdb_extension"

size="$(wc -c < "$DIR/postgres_scanner.duckdb_extension" | tr -d ' ')"
if [[ "$size" -lt 1000000 ]]; then
  echo "ERROR: la extension descargada pesa $size bytes; se esperaba ~30 MB." >&2
  exit 1
fi

echo "OK: $DIR/postgres_scanner.duckdb_extension ($size bytes)"
