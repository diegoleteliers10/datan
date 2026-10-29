#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="${1:-$repo_root/target/debug/datan-app}"
app="${2:-$repo_root/target/macos/Datan.app}"

if [[ ! -x "$binary" ]]; then
	printf 'No executable found at %s\n' "$binary" >&2
	exit 1
fi

mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
install -m 755 "$binary" "$app/Contents/MacOS/datan-app"
install -m 644 \
	"$repo_root/branding/datan-identity/digital/macos/Datan.icns" \
	"$app/Contents/Resources/Datan.icns"
install -m 644 \
	"$repo_root/packaging/macos/Info.plist" \
	"$app/Contents/Info.plist"

printf 'Created %s\n' "$app"
