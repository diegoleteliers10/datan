# Releasing Datan

Un release empieza cuando un pull request se mergea a `main`. Si la version del
workspace ya tiene tag y release, el workflow crea un commit de release que
bumpea el patch, lo taggea y construye los cuatro targets. No se pushean tags a
mano.

## Publicar una version

1. Bumpea la version del workspace en `Cargo.toml` (seccion `[workspace.package]`):

   ```bash
   cargo set-version --workspace 0.2.0
   ```

   Si no bumpeas, el merge igual genera un release: el workflow crea un commit
   de release con el patch siguiente. El bump manual sirve para elegir
   minor/major o para dejar la version explicita en `main`.

2. Agrega la seccion al `CHANGELOG.md` con el formato `## 0.2.0 - YYYY-MM-DD`.
   El cuerpo del release sale de ahi. Sin seccion, el release se publica con un
   texto minimo.

3. Commitea el bump en la rama que va a `main`:

   ```bash
   git commit -m "chore(release): bump version to 0.2.0"
   ```

4. Abre el PR a `main` y mergealo.

## Que hace la automatizacion

1. `.github/workflows/release-on-merge.yml` corre cuando un PR a `main` se
   cierra mergeado.
2. Lee la version de `[workspace.package]` en `Cargo.toml`. Si `vX.Y.Z` no
   existe, taggea el merge. Si ya existe y su release esta publicado, crea un
   commit de release que bumpea el patch (`v0.1.0` -> `v0.1.1`), lo taggea y
   sigue. Ese commit vive solo en el tag, no en `main`.
3. Dispara `.github/workflows/release.yml` con `gh workflow run`. Es necesario:
   un tag pusheado con `GITHUB_TOKEN` no dispara otros workflows.
4. `release.yml` construye:

   | Target | Runner | Artefactos |
   | --- | --- | --- |
   | `aarch64-apple-darwin` | `macos-latest` | `.dmg`, `.tar.gz` |
   | `x86_64-apple-darwin` | `macos-latest` (cross) | `.dmg`, `.tar.gz` |
   | `x86_64-unknown-linux-gnu` | `ubuntu-22.04` | `.deb`, `.AppImage`, `.tar.gz` |
   | `x86_64-pc-windows-msvc` | `windows-2022` | `.msi`, `.zip` |

5. El job `release` junta todo, genera `SHA256SUMS` y publica el release.

## Reglas

- El tag sale de `Cargo.toml`. Si el tag no coincide con la version del
  workspace, `release.yml` falla antes de construir.
- Si el tag ya existe y el release ya se publico, el workflow bumpea el patch en
  un commit de release y usa ese tag. No hace falta bumpear a mano para que
  salga un release.
- No pushees el tag a mano. Si el tag existe y el release no, se despacha el tag
  existente (recuperacion). Un re-run del mismo merge no crea otro release.
- Linux se construye en `ubuntu-22.04` a proposito. Cambiarlo a `ubuntu-latest`
  sube el piso de glibc y deja afuera Ubuntu 22.04/24.04 y Debian 12. Si algun
  dia hay que moverlo, verifica `dpkg-deb -I dist/*.deb` antes de publicar.
- El release es atomico: si un target falla, no se publica ninguno. El CI de PRs
  existe para que eso no pase en `main`.

## Firma

### macOS

Sin secretos, el `.app` se firma ad-hoc y Gatekeeper pide aceptar la app. Para
distribuir en serio hacen falta un certificado **Developer ID Application** y
notarizacion. Configura estos secretos:

| Secreto | Valor |
| --- | --- |
| `MACOS_SIGNING_IDENTITY` | `Developer ID Application: Nombre (TEAMID)` |
| `MACOS_NOTARY_APPLE_ID` | Apple ID |
| `MACOS_NOTARY_TEAM_ID` | Team ID |
| `MACOS_NOTARY_PASSWORD` | app-specific password |

`packaging/macos/bundle.sh` firma con hardened runtime, notariza el `.dmg`,
hace `stapler staple` y verifica con `spctl`. Un certificado self-signed no
sirve para notarizar; solo mantiene estable la identidad entre updates.

### Windows

Sin secreto, SmartScreen muestra "editor desconocido". Para firmar, sube el
`.pfx` en base64 y su password:

| Secreto | Valor |
| --- | --- |
| `WINDOWS_CERTIFICATE_BASE64` | `base64 -i cert.pfx` |
| `WINDOWS_CERTIFICATE_PASSWORD` | password del `.pfx` |

El workflow firma `Datan.exe` antes de armar el `.zip` y el `.msi` despues de
compilarlo.

## Extension de DuckDB

La app necesita `postgres_scanner.duckdb_extension` para la version exacta de
DuckDB que compila `libduckdb-sys` (hoy **1.5.6**, del crate `duckdb 1.10506.x`).
El workflow la descarga con:

```bash
scripts/fetch-duckdb-extension.sh linux_amd64 dist/extensions
```

Layout que espera DuckDB:

```
extensions/
  v1.5.6/
    linux_amd64/
      postgres_scanner.duckdb_extension
```

Si actualizas `duckdb`:

1. Mira la version real en `Cargo.lock` (`duckdb` y `libduckdb-sys`).
2. Actualiza `DUCKDB_VERSION` en `.github/workflows/release.yml` y en
   `scripts/fetch-duckdb-extension.sh`.
3. Verifica que exista la URL `https://extensions.duckdb.org/v<version>/<platform>/postgres_scanner.duckdb_extension.gz`.
4. Corre `cargo run --release -p datan-app --bin probe_offline` en macOS.

## Empaquetar localmente

macOS:

```bash
cargo build --release -p datan-app --bin datan-app
scripts/fetch-duckdb-extension.sh osx_arm64 dist/extensions
EXTENSIONS_DIR=dist/extensions packaging/macos/bundle.sh aarch64-apple-darwin 0.1.0
```

Linux:

```bash
scripts/fetch-duckdb-extension.sh linux_amd64 dist/extensions
cargo deb -p datan-app --no-build --output dist/datan_0.1.0_amd64.deb
EXTENSIONS_DIR=dist/extensions packaging/linux/appimage.sh x86_64-unknown-linux-gnu 0.1.0
```

Windows (PowerShell, desde la raiz):

```powershell
bash scripts/fetch-duckdb-extension.sh windows_amd64 dist/extensions
# El .wxs se compila con candle/light; ver el paso "Build MSI Installer" del workflow.
```

Si el build `x86_64-apple-darwin` falla compilando el C++ de DuckDB en un
runner arm64, cambia ese target a un runner Intel (`macos-15-intel` o el que
exista) en lugar de forzar el cross.

## Recuperar un release

```bash
gh workflow run release.yml --ref v0.1.0
```

Tambien se puede correr desde la pestana Actions con el input `tag`.
