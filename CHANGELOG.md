# Changelog

Los cambios notables de cada release. La seccion mas nueva es la que se publica
en el GitHub Release y la que la app muestra en el dialogo de "novedades".

## 0.1.0 - 2026-11-09

### Empaquetado

- Primer release multiplataforma: `.dmg` y `.tar.gz` para macOS Apple Silicon e Intel, `.deb`,
  AppImage y `.tar.gz` para Linux x86_64, `.msi` y `.zip` para Windows x64.
- La app empaqueta la extension `postgres_scanner` de DuckDB 1.5.6 y la carga desde el bundle:
  el primer uso de PostgreSQL no depende de la red.
- CI en macOS, Linux y Windows, y release automatico al mergear un PR que bumpea la version.

### Runtime

- La raiz de extensiones se resuelve desde el bundle (`Contents/Resources/extensions` en macOS,
  `usr/lib/datan/extensions` en Linux, `extensions\` junto al `.exe` en Windows) con fallback a
  `~/.duckdb/extensions` en desarrollo. `DATAN_EXTENSION_DIR` permite forzar otra ruta.
