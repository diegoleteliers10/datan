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
//!
//! # Preferencias
//!
//! `~/.datan/preferences.toml` guarda lo que no es una conexion: hoy, el tema. Se separa del
//! archivo de conexiones porque una cosa no tiene por que perderse con la otra, y un archivo
//! roto no impide arrancar: se pierde la preferencia, no el trabajo.

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
    /// `datan_bench` es la base que los probes y la suite crean y llenan; `postgres` es la
    /// base del esquema real (`boleta` -> `product_boleta` -> `catalogo_productos`). Sin
    /// entradas la app arranca con una lista vacia y el usuario no ve que hay algo que
    /// elegir. La base de pruebas va primero porque es la que tiene datos para mirar.
    fn seed() -> Self {
        Self {
            list: vec![
                Connection {
                    name: "datan_bench".to_string(),
                    database: "datan_bench".to_string(),
                },
                Connection {
                    name: "postgres (esquema real)".to_string(),
                    database: "postgres".to_string(),
                },
            ],
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

/// El tema de la app.
///
/// `Sistema` sigue la apariencia del escritorio y escucha el cambio en caliente: un tema que
/// solo se mira al arrancar deja la app en claro cuando el sistema pasa a oscuro, y el
/// usuario no tiene por que saber que hay que reiniciarla.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tema {
    #[default]
    Claro,
    Oscuro,
    Sistema,
}

/// Las preferencias de la app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    #[serde(default)]
    pub tema: Tema,
}

impl Preferences {
    /// El archivo del usuario.
    pub fn path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".datan").join("preferences.toml")
    }

    /// Carga las preferencias.
    ///
    /// No devuelve error a proposito: un archivo que no parsea deja el tema por defecto y la
    /// app arranca. Perder el tema es molesto; no abrir la app por el tema no tiene defensa.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    /// Carga de una ruta. Es la mitad testeable de `load`.
    pub fn load_from(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Guarda las preferencias.
    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to(&Self::path())
    }

    /// Guarda en una ruta. Es la mitad testeable de `save`.
    pub fn save_to(&self, path: &std::path::Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            return Err(ConfigError::Write {
                path: parent.to_path_buf(),
                source: error,
            });
        }

        let text = toml::to_string_pretty(self)
            .map_err(|_| ConfigError::Unknown("no se pudo serializar el tema".to_string()))?;
        std::fs::write(path, text).map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
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
        assert_eq!(connections.list.len(), 2);
        // La de pruebas va primero: es la que tiene datos para mirar el primer dia.
        assert_eq!(connections.list[0].database, "datan_bench");
        assert_eq!(connections.list[1].database, "postgres");
    }

    #[test]
    fn upsert_actualiza_en_vez_de_duplicar() {
        let mut connections = Connections::seed();
        connections.upsert(connection("datan_bench", "otra_base"));
        assert_eq!(connections.list.len(), 2);
        // La actualizada sube arriba: orden por uso, no por nombre.
        assert_eq!(connections.list[0].database, "otra_base");

        connections.upsert(connection("produccion", "prod"));
        assert_eq!(connections.list.len(), 3);
        assert_eq!(connections.list[0].database, "prod");
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

    fn ruta_de_prueba(nombre: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "datan-preferencias-{}-{nombre}.toml",
            std::process::id()
        ))
    }

    #[test]
    fn el_tema_guardado_vuelve_al_arrancar() {
        let path = ruta_de_prueba("vuelve");
        let preferences = Preferences { tema: Tema::Oscuro };
        preferences.save_to(&path).expect("guarda");
        let leido = Preferences::load_from(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(leido, preferences);
    }

    #[test]
    fn sin_archivo_el_tema_es_claro() {
        let path = ruta_de_prueba("no-existe");
        let _ = std::fs::remove_file(&path);
        assert_eq!(Preferences::load_from(&path).tema, Tema::Claro);
    }

    #[test]
    fn un_archivo_roto_no_impide_arrancar() {
        // La app no puede negarse a abrir porque una preferencia no parsea. El tema vuelve
        // al default y el archivo se reescribe en el proximo cambio.
        let path = ruta_de_prueba("roto");
        std::fs::write(&path, "tema = 3\n").expect("escribe");
        let leido = Preferences::load_from(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(leido.tema, Tema::Claro);
    }

    #[test]
    fn un_archivo_sin_tema_abre_en_claro() {
        // El archivo puede existir por otra preferencia, o venir de una version que no
        // tenia tema. La falta del campo no es un error: es el default.
        let preferences: Preferences = toml::from_str("").expect("parsea");
        assert_eq!(preferences.tema, Tema::Claro);
    }

    #[test]
    fn el_tema_se_lee_en_minusculas() {
        let preferences: Preferences =
            toml::from_str("tema = \"oscuro\"").expect("parsea");
        assert_eq!(preferences.tema, Tema::Oscuro);
    }

    #[test]
    fn un_tema_que_no_existe_no_se_inventa() {
        // Una version futura escribe otro tema; esta version no lo entiende y no puede
        // adivinar. `Preferences::load` cae al default en vez de fallar.
        assert!(toml::from_str::<Preferences>("tema = \"sepia\"").is_err());
    }

    #[test]
    fn el_tema_del_sistema_se_lee_y_se_escribe() {
        let preferences: Preferences =
            toml::from_str("tema = \"sistema\"").expect("parsea");
        assert_eq!(preferences.tema, Tema::Sistema);
        let text = toml::to_string_pretty(&preferences).expect("serializa");
        assert!(text.contains("tema = \"sistema\""), "{text}");
    }

    #[test]
    fn las_preferencias_se_escriben_sin_tablas() {
        let text = toml::to_string_pretty(&Preferences {
            tema: Tema::Oscuro,
        })
        .expect("serializa");
        assert!(text.contains("tema = \"oscuro\""), "{text}");
        let back: Preferences = toml::from_str(&text).expect("vuelve a parsear");
        assert_eq!(back, Preferences { tema: Tema::Oscuro });
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