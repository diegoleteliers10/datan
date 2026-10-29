use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Project,
    Report,
}

pub fn document_kind(path: &Path) -> Result<DocumentKind, ProjectDocError> {
    let text = std::fs::read_to_string(path).map_err(|source| ProjectDocError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let document: toml::Table = toml::from_str(&text).map_err(|source| ProjectDocError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    match (
        document.contains_key("format_version"),
        document.contains_key("database"),
    ) {
        (true, false) => Ok(DocumentKind::Project),
        (false, true) => Ok(DocumentKind::Report),
        _ => Err(ProjectDocError::InvalidFormat {
            path: path.to_path_buf(),
        }),
    }
}

pub fn report_path(path: &Path) -> Result<PathBuf, ProjectDocError> {
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dtx"))
    {
        return Ok(path.to_path_buf());
    }
    let destination = path.with_extension("dtx");
    if destination.exists() {
        return Err(ProjectDocError::Occupied { path: destination });
    }
    Ok(destination)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatanxDocument {
    pub format_version: u32,
    pub title: String,
    pub pages: Vec<datan_report::saved::SavedPage>,
    pub active_page: usize,
    #[serde(default)]
    pub panels: datan_report::saved::SavedPanels,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loaded_queries: Vec<datan_report::saved::SavedSourceTable>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub model_positions:
        std::collections::BTreeMap<String, datan_report::saved::SavedModelPosition>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectDocError {
    #[error("{path} no tiene un formato de informe válido")]
    InvalidFormat { path: PathBuf },
    #[error("ya existe {path}. Elige otro nombre para guardar el informe")]
    Occupied { path: PathBuf },
    #[error("no se pudo leer {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} no es un archivo Datan válido: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("la versión de {path} no es compatible: {version}")]
    UnsupportedVersion { path: PathBuf, version: u32 },
    #[error("no se pudo guardar {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl DatanxDocument {
    pub fn new(
        title: String,
        pages: Vec<datan_report::saved::SavedPage>,
        active_page: usize,
    ) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            title,
            pages,
            active_page,
            panels: datan_report::saved::SavedPanels::default(),
            saved_at: None,
            data_scope: None,
            source_key: None,
            loaded_queries: Vec::new(),
            model_positions: std::collections::BTreeMap::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Self, ProjectDocError> {
        let text = std::fs::read_to_string(path).map_err(|source| ProjectDocError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let document: Self = toml::from_str(&text).map_err(|source| ProjectDocError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        if document.format_version != FORMAT_VERSION {
            return Err(ProjectDocError::UnsupportedVersion {
                path: path.to_path_buf(),
                version: document.format_version,
            });
        }
        Ok(document)
    }

    pub fn save(&self, path: &Path) -> Result<(), ProjectDocError> {
        let Some(parent) = path.parent() else {
            return Err(ProjectDocError::Write {
                path: path.to_path_buf(),
                source: std::io::Error::other("la ruta no tiene carpeta"),
            });
        };
        std::fs::create_dir_all(parent).map_err(|source| ProjectDocError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
        let text = toml::to_string_pretty(self).map_err(|error| ProjectDocError::Write {
            path: path.to_path_buf(),
            source: std::io::Error::other(error.to_string()),
        })?;
        let temporary = path.with_extension("dtx.tmp");
        std::fs::write(&temporary, text).map_err(|source| ProjectDocError::Write {
            path: temporary.clone(),
            source,
        })?;
        std::fs::rename(&temporary, path).map_err(|source| ProjectDocError::Write {
            path: path.to_path_buf(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guarda_y_abre_un_documento_vacio() {
        let path = std::env::temp_dir().join(format!("{}.dtx", uuid_for_test()));
        let document = DatanxDocument::new("Informe nuevo".to_string(), Vec::new(), 0);
        document.save(&path).expect("guarda documento");

        let loaded = DatanxDocument::load(&path).expect("abre documento");
        assert_eq!(loaded, document);
        std::fs::remove_file(path).expect("quita archivo temporal");
    }

    #[test]
    fn model_positions_roundtrip_in_project_document() {
        let path = std::env::temp_dir().join(format!("{}.dtx", uuid_for_test()));
        let mut document = DatanxDocument::new("Report".into(), Vec::new(), 0);
        document.model_positions.insert(
            "Ventas".into(),
            datan_report::saved::SavedModelPosition { x: 320, y: 180 },
        );

        document.save(&path).expect("save project");
        assert_eq!(DatanxDocument::load(&path).expect("read project"), document);
        std::fs::remove_file(path).expect("remove temporary file");
    }

    #[test]
    fn project_panel_visibility_roundtrips_all_combinations() {
        let path = std::env::temp_dir().join(format!("{}.dtx", uuid_for_test()));
        for datos in [false, true] {
            for filtros in [false, true] {
                for visualizaciones in [false, true] {
                    let mut document = DatanxDocument::new("Report".into(), Vec::new(), 0);
                    document.panels = datan_report::saved::SavedPanels {
                        datos,
                        filtros,
                        visualizaciones,
                    };
                    document.save(&path).expect("save project");
                    assert_eq!(DatanxDocument::load(&path).expect("read project"), document);
                }
            }
        }
        std::fs::remove_file(path).expect("remove temporary file");
    }

    #[test]
    fn old_project_keeps_all_panels_visible() {
        let document: DatanxDocument =
            toml::from_str("format_version = 1\ntitle = 'Report'\npages = []\nactive_page = 0")
                .expect("read old project");
        assert_eq!(document.panels, datan_report::saved::SavedPanels::default());
        assert!(document.model_positions.is_empty());
    }

    #[test]
    fn dispatch_uses_content_for_current_and_legacy_extensions() {
        for extension in ["dtx", "datanx", "datanr"] {
            let path = std::env::temp_dir().join(format!("{}.{extension}", uuid_for_test()));
            DatanxDocument::new("Informe".into(), Vec::new(), 0)
                .save(&path)
                .expect("save project");
            assert_eq!(document_kind(&path).unwrap(), DocumentKind::Project);
            std::fs::write(
                &path,
                "name = 'Report'\ndatabase = 'pg'\ndimensions = []\nmeasures = []\n",
            )
            .expect("save connected report");
            assert_eq!(document_kind(&path).unwrap(), DocumentKind::Report);
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn dispatch_rejects_invalid_and_ambiguous_documents() {
        let path = std::env::temp_dir().join(format!("{}.dtx", uuid_for_test()));
        for text in [
            "[",
            "title = 'Other'",
            "format_version = 1\ndatabase = 'pg'",
        ] {
            std::fs::write(&path, text).unwrap();
            assert!(document_kind(&path).is_err());
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn dispatch_keeps_version_validation_in_project_loader() {
        let path = std::env::temp_dir().join(format!("{}.dtx", uuid_for_test()));
        let mut project = DatanxDocument::new("Report".into(), Vec::new(), 0);
        project.format_version = FORMAT_VERSION + 1;
        project.save(&path).unwrap();
        assert_eq!(document_kind(&path).unwrap(), DocumentKind::Project);
        assert!(matches!(
            DatanxDocument::load(&path),
            Err(ProjectDocError::UnsupportedVersion { .. })
        ));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn save_path_uses_dtx_and_preserves_the_legacy_file() {
        let legacy = std::env::temp_dir().join(format!("{}.datanx", uuid_for_test()));
        std::fs::write(&legacy, "legacy").unwrap();
        let destination = report_path(&legacy).unwrap();
        assert_eq!(destination.extension().unwrap(), "dtx");
        assert_eq!(report_path(&destination).unwrap(), destination);
        assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "legacy");
        let no_extension = legacy.with_extension("");
        assert_eq!(report_path(&no_extension).unwrap(), destination);
        std::fs::remove_file(legacy).unwrap();
    }

    #[test]
    fn save_path_does_not_overwrite_a_sibling_dtx_file() {
        let legacy = std::env::temp_dir().join(format!("{}.datanr", uuid_for_test()));
        let destination = legacy.with_extension("dtx");
        std::fs::write(&legacy, "original").unwrap();
        std::fs::write(&destination, "existing report").unwrap();
        assert!(matches!(
            report_path(&legacy),
            Err(ProjectDocError::Occupied { .. })
        ));
        assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "original");
        assert_eq!(
            std::fs::read_to_string(&destination).unwrap(),
            "existing report"
        );
        std::fs::remove_file(legacy).unwrap();
        std::fs::remove_file(destination).unwrap();
    }

    #[test]
    fn save_path_keeps_an_existing_uppercase_dtx_path() {
        let path = std::env::temp_dir().join(format!("{}.DTX", uuid_for_test()));
        std::fs::write(&path, "existing report").unwrap();
        assert_eq!(report_path(&path).unwrap(), path);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn repeated_save_replaces_the_project_at_the_same_path() {
        let path = std::env::temp_dir().join(format!("{}.dtx", uuid_for_test()));
        let mut project = DatanxDocument::new("First title".into(), Vec::new(), 0);
        project.save(&path).unwrap();
        project.title = "Second title".into();
        project.save(&path).unwrap();
        assert_eq!(DatanxDocument::load(&path).unwrap(), project);
        std::fs::remove_file(path).unwrap();
    }

    fn uuid_for_test() -> String {
        format!(
            "dtx-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        )
    }
}
