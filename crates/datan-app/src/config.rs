//! Las conexiones que la app recuerda.
//!
//! # Por que una lista y no un campo de texto
//!
//! Escribir un DSN necesita un input con foco, teclado y estado. Todo eso no se puede
//! verificar sin pantalla, y un campo de texto roto no se ve como roto: se ve como un campo
//! que no responde.
//!
//! Elegir de una lista usa los mismos elementos clicables que el grid, que ya estan
//! verificados. Y para un usuario de escritorio es lo que espera: abrir, elegir, ver. Escribir
//! el DSN una vez y que quede guardado.
//!
//! # Formato
//!
//! TOML plano en `~/.datan/connections.toml`. Sin secciones anidadas ni tablas de tablas: son
//! pares nombre-base, y el archivo se lee mas veces de las que se escribe.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// El archivo, con la tabla que el formato TOML exige.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct File {
    connection: Vec<Connection>,
}

/// Una conexion guardada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    /// El nombre que se muestra. No tiene que coincidir con la base.
    pub name: String,
    /// La base de Postgres. Es lo unico que la app necesita para el `ATTACH`.
    pub database: String,
}

/// El archivo de conexiones.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Connections {
    pub list: Vec<Connection>,
}

/// Un error del archivo de conexiones.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no se pudo leer {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("no se pudo escribir {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} no es un TOML valido: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("la conexion `{0}` no esta en la lista")]
    Unknown(String),
}

impl Connections {
    /// El archivo del usuario.
    ///
    /// `~/.datan/connections.toml`. La carpeta se crea al guardar si no existe, no al leer:
    /// crear carpetas al leer convierte un error de lectura en una escritura sorpresa.
    pub fn path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".datan").join("connections.toml")
    }

    /// Carga el archivo. Si no existe, devuelve la lista con la conexion de ejemplo.
    ///
    /// Un archivo corrupto NO se sobreescribe: se devuelve el error y el usuario decide. Un
    /// archivo que se borra solo es peor que una app que no arranca.
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::seed());
            }
            Err(error) => return Err(ConfigError::Read { path, source: error }),
        };

        // El TOML no acepta un array en el raiz: van dentro de una tabla.
        let file: File = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path,
            source,
        })?;

        if file.connection.is_empty() {
            return Ok(Self::seed());
        }
        Ok(Self {
            list: file.connection,
        })
    }

    /// La lista inicial, para la primera ejecucion.
    ///
    /// `datan_bench` es la base que los probes y la suite crean y llenan. Sin ella la app
    /// arranca con una lista vacia y el usuario no ve que hay algo que elegir.
    fn seed() -> Self {
        Self {
            list: vec![Connection {
                name: "datan_bench".to_string(),
                database: "datan_bench".to_string(),
            }],
        }
    }

    /// Guarda la lista.
    pub fn save(&self) -> Result<(), ConfigError> {
        let path = Self::path();
        if let Some(parent) = path.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            return Err(ConfigError::Write {
                path: parent.to_path_buf(),
                source: error,
            });
        }

        let text = toml::to_string_pretty(&File {
            connection: self.list.clone(),
        })
        .map_err(|_| ConfigError::Unknown("no se pudo serializar la lista".to_string()))?;
        std::fs::write(&path, text).map_err(|source| ConfigError::Write { path, source })
    }

    /// Anade una conexion, o actualiza la que ya tiene ese nombre, y la sube arriba.
    ///
    /// La lista ordena por USO, no por nombre: la base de ayer es la que se va a abrir hoy.
    /// Ordenar alfabeticamente haria que la conexion de todos los dias quedara en medio de
    /// la lista para siempre.
    pub fn upsert(&mut self, connection: Connection) {
        self.list
            .retain(|existing| existing.name != connection.name);
        self.list.insert(0, connection);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(name: &str, database: &str) -> Connection {
        Connection {
            name: name.to_string(),
            database: database.to_string(),
        }
    }

    #[test]
    fn una_lista_vacia_se_siembra_con_una_conexion_de_ejemplo() {
        let connections = Connections::seed();
        assert_eq!(connections.list.len(), 1);
        assert_eq!(connections.list[0].database, "datan_bench");
    }

    #[test]
    fn upsert_actualiza_en_vez_de_duplicar() {
        let mut connections = Connections::seed();
        connections.upsert(connection("datan_bench", "otra_base"));
        assert_eq!(connections.list.len(), 1);
        assert_eq!(connections.list[0].database, "otra_base");

        connections.upsert(connection("produccion", "prod"));
        assert_eq!(connections.list.len(), 2);
    }

    #[test]
    fn el_toml_es_un_array_de_tablas_planas() {
        // Sin secciones anidadas: el archivo lo escribe una app y lo leen ojos.
        // El TOML no acepta un array en el raiz, asi que va dentro de una tabla. Sin el
        // envoltorio, `to_string` falla en tiempo de ejecucion y no al compilar.
        let connections = Connections {
            list: vec![connection("datan_bench", "datan_bench")],
        };
        let text = toml::to_string_pretty(&File {
            connection: connections.list.clone(),
        })
        .expect("serializa");

        assert!(text.contains("[[connection]]"));
        assert!(text.contains("database = \"datan_bench\""));

        let file: File = toml::from_str(&text).expect("vuelve a parsear");
        assert_eq!(file.connection, connections.list);
    }

    #[test]
    fn la_ruta_vive_en_la_carpeta_del_usuario() {
        let path = Connections::path();
        assert!(
            path.to_string_lossy().contains(".datan"),
            "la ruta tiene que ser reconocible: {path:?}"
        );
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("toml"));
    }
}