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
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_user")]
    pub username: String,
    #[serde(default = "default_sslmode")]
    pub sslmode: String,
    #[serde(default)]
    pub mode: ConnectionMode,
    #[serde(default)]
    pub table: String,
    #[serde(default)]
    pub source_query: Option<String>,
}

impl Connection {
    /// Reconstruye los datos editables de una clave de origen. La contraseña siempre se pide de nuevo.
    pub fn from_document_key(name: String, source_key: &str) -> Option<Self> {
        let (database, host, port, username) =
            if let Some(encoded) = source_key.strip_prefix("source-v1--") {
                let mut parts = encoded.split("--");
                let database = decode_source_key_part(parts.next()?)?;
                let host = decode_source_key_part(parts.next()?)?;
                let port = parts.next()?.parse().ok()?;
                let username = decode_source_key_part(parts.next()?)?;
                if parts.next().is_some()
                    || database.trim().is_empty()
                    || host.trim().is_empty()
                    || username.trim().is_empty()
                    || port == 0
                {
                    return None;
                }
                (database, host, port, username)
            } else {
                (
                    source_key.to_string(),
                    default_host(),
                    default_port(),
                    default_user(),
                )
            };

        Some(Self {
            name,
            database,
            host,
            port,
            username,
            sslmode: default_sslmode(),
            mode: ConnectionMode::DirectQuery,
            table: String::new(),
            source_query: None,
        })
    }
}

fn decode_source_key_part(encoded: &str) -> Option<String> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = u8::from_str_radix(
                std::str::from_utf8(bytes.get(index + 1..index + 2)?).ok()?,
                16,
            )
            .ok()?;
            let low = u8::from_str_radix(
                std::str::from_utf8(bytes.get(index + 2..index + 3)?).ok()?,
                16,
            )
            .ok()?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionMode {
    Import,
    #[default]
    DirectQuery,
}

fn default_host() -> String {
    "localhost".to_string()
}
fn default_port() -> u16 {
    5432
}
fn default_user() -> String {
    "postgres".to_string()
}
fn default_sslmode() -> String {
    "prefer".to_string()
}

/// Datos de conexión en memoria. El archivo de configuración nunca guarda la contraseña.
#[derive(Clone, PartialEq, Eq)]
pub struct PostgresConnection {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: String,
    pub sslmode: String,
    pub mode: ConnectionMode,
    pub table: String,
    pub source_query: Option<String>,
}

impl PostgresConnection {
    /// Identificador estable para los documentos locales de este origen de datos.
    pub fn document_key(&self) -> String {
        if matches!(
            self.host.to_ascii_lowercase().as_str(),
            "localhost" | "127.0.0.1" | "::1"
        ) && self.port == 5432
            && self.username == "postgres"
        {
            return self.database.clone();
        }

        let encode = |value: &str| {
            value
                .bytes()
                .map(|byte| match byte {
                    b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => char::from(byte).to_string(),
                    _ => format!("%{byte:02X}"),
                })
                .collect::<String>()
        };
        format!(
            "source-v1--{}--{}--{}--{}",
            encode(&self.database),
            encode(&self.host.to_ascii_lowercase()),
            self.port,
            encode(&self.username),
        )
    }

    pub fn conninfo(&self) -> String {
        let quote = |value: &str| format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"));
        format!(
            "host={} port='{}' user={} password={} dbname={} sslmode={}",
            quote(&self.host),
            self.port,
            quote(&self.username),
            quote(&self.password),
            quote(&self.database),
            quote(&self.sslmode),
        )
    }
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
        let saved = Self::load_saved()?;
        Ok(if saved.list.is_empty() {
            Self::seed()
        } else {
            saved
        })
    }

    /// Lee solo perfiles guardados por el usuario. No incluye las conexiones de ejemplo.
    pub fn load_saved() -> Result<Self, ConfigError> {
        Self::load_saved_from(&Self::path())
    }

    fn load_saved_from(path: &std::path::Path) -> Result<Self, ConfigError> {
        let path = path.to_path_buf();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(ConfigError::Read {
                    path,
                    source: error,
                });
            }
        };

        // El TOML no acepta un array en el raiz: van dentro de una tabla.
        let file: File =
            toml::from_str(&text).map_err(|source| ConfigError::Parse { path, source })?;

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
                    host: default_host(),
                    port: default_port(),
                    username: default_user(),
                    sslmode: default_sslmode(),
                    mode: ConnectionMode::DirectQuery,
                    table: String::new(),
                    source_query: None,
                },
                Connection {
                    name: "postgres (esquema real)".to_string(),
                    database: "postgres".to_string(),
                    host: default_host(),
                    port: default_port(),
                    username: default_user(),
                    sslmode: default_sslmode(),
                    mode: ConnectionMode::DirectQuery,
                    table: String::new(),
                    source_query: None,
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

    /// Anade una conexion, o actualiza el mismo servidor, puerto y base, y la sube arriba.
    ///
    /// La lista ordena por USO, no por nombre: la base de ayer es la que se va a abrir hoy.
    /// Ordenar alfabeticamente haria que la conexion de todos los dias quedara en medio de
    /// la lista para siempre.
    pub fn upsert(&mut self, connection: Connection) {
        self.list.retain(|existing| {
            existing.host != connection.host
                || existing.port != connection.port
                || existing.database != connection.database
                || existing.username != connection.username
        });
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
            host: default_host(),
            port: default_port(),
            username: default_user(),
            sslmode: default_sslmode(),
            mode: ConnectionMode::DirectQuery,
            table: String::new(),
            source_query: None,
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
    fn load_saved_no_muestra_perfiles_de_ejemplo_si_no_hay_archivo() {
        let path = std::env::temp_dir().join(format!(
            "datan-connections-missing-{}.toml",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        assert!(Connections::load_saved_from(&path).unwrap().list.is_empty());
        std::fs::write(&path, "connection = []").unwrap();
        assert!(Connections::load_saved_from(&path).unwrap().list.is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn upsert_actualiza_en_vez_de_duplicar() {
        let mut connections = Connections::seed();
        connections.upsert(connection("datan_bench", "otra_base"));
        assert_eq!(connections.list.len(), 3);
        // La actualizada sube arriba: orden por uso, no por nombre.
        assert_eq!(connections.list[0].database, "otra_base");

        connections.upsert(connection("produccion", "prod"));
        assert_eq!(connections.list.len(), 4);
        assert_eq!(connections.list[0].database, "prod");
    }

    #[test]
    fn una_clave_de_origen_reconstruye_el_perfil_sin_contrasena() {
        let source = PostgresConnection {
            name: "Producción".to_string(),
            host: "db.example.com".to_string(),
            port: 5544,
            username: "ana@equipo".to_string(),
            password: "secreto".to_string(),
            database: "ventas norte".to_string(),
            sslmode: "require".to_string(),
            mode: ConnectionMode::Import,
            table: "ventas".to_string(),
            source_query: None,
        };

        let restored = Connection::from_document_key("Informe".to_string(), &source.document_key())
            .expect("reconstruye la clave");

        assert_eq!(restored.name, "Informe");
        assert_eq!(restored.host, source.host);
        assert_eq!(restored.port, source.port);
        assert_eq!(restored.username, source.username);
        assert_eq!(restored.database, source.database);
        assert_eq!(restored.mode, ConnectionMode::DirectQuery);
    }

    #[test]
    fn una_clave_de_origen_rechaza_campos_requeridos_vacios_y_puerto_cero() {
        for key in [
            "source-v1----host--5432--user",
            "source-v1--db----5432--user",
            "source-v1--db--host--0--user",
            "source-v1--db--host--5432--",
        ] {
            assert!(
                Connection::from_document_key("Informe".to_string(), key).is_none(),
                "la clave no debe crear un perfil: {key}"
            );
        }
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
        let preferences: Preferences = toml::from_str("tema = \"oscuro\"").expect("parsea");
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
        let preferences: Preferences = toml::from_str("tema = \"sistema\"").expect("parsea");
        assert_eq!(preferences.tema, Tema::Sistema);
        let text = toml::to_string_pretty(&preferences).expect("serializa");
        assert!(text.contains("tema = \"sistema\""), "{text}");
    }

    #[test]
    fn las_preferencias_se_escriben_sin_tablas() {
        let text = toml::to_string_pretty(&Preferences { tema: Tema::Oscuro }).expect("serializa");
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
