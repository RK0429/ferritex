use std::path::{Path, PathBuf};

use ferritex_application::ports::AssetBundleLoaderPort;
use serde::{Deserialize, Serialize};

const CURRENT_FERRITEX_VERSION: &str = "0.1.0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetBundleManifest {
    pub name: String,
    pub version: String,
    pub min_ferritex_version: String,
}

pub struct AssetBundleLoader;

impl AssetBundleLoaderPort for AssetBundleLoader {
    fn validate(&self, bundle_path: &Path) -> Result<(), String> {
        Self::load(bundle_path)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn resolve_tex_input(&self, bundle_path: &Path, relative_path: &str) -> Option<PathBuf> {
        let bundle_relative = tex_relative_candidate(Path::new(relative_path));
        let texmf_root = bundle_path.join("texmf");
        let tex_input_path = texmf_root.join(bundle_relative);

        let resolved = tex_input_path.canonicalize().ok()?;
        let texmf_resolved = texmf_root.canonicalize().ok()?;
        if !resolved.starts_with(&texmf_resolved) {
            return None;
        }

        Some(resolved)
    }
}

impl AssetBundleLoader {
    pub fn load(bundle_path: &Path) -> Result<AssetBundleManifest, AssetBundleError> {
        if !bundle_path.exists() {
            return Err(AssetBundleError::NotFound {
                path: bundle_path.to_path_buf(),
            });
        }

        let manifest_path = bundle_path.join("manifest.json");
        if !manifest_path.is_file() {
            return Err(AssetBundleError::ManifestNotFound);
        }

        let content = std::fs::read_to_string(&manifest_path)?;
        let manifest: AssetBundleManifest =
            serde_json::from_str(&content).map_err(|source| AssetBundleError::InvalidManifest {
                reason: source.to_string(),
            })?;

        validate_manifest(&manifest)?;
        ensure_version_compatible(&manifest)?;

        Ok(manifest)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AssetBundleError {
    #[error("bundle not found at {path}")]
    NotFound { path: PathBuf },
    #[error("manifest not found in bundle")]
    ManifestNotFound,
    #[error("invalid manifest: {reason}")]
    InvalidManifest { reason: String },
    #[error("version incompatible: bundle {bundle_version}, required {required_version}")]
    VersionIncompatible {
        bundle_version: String,
        required_version: String,
    },
    #[error("I/O error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}

fn validate_manifest(manifest: &AssetBundleManifest) -> Result<(), AssetBundleError> {
    if manifest.name.trim().is_empty() {
        return Err(AssetBundleError::InvalidManifest {
            reason: "name must not be empty".to_string(),
        });
    }

    if manifest.version.trim().is_empty() {
        return Err(AssetBundleError::InvalidManifest {
            reason: "version must not be empty".to_string(),
        });
    }

    if manifest.min_ferritex_version.trim().is_empty() {
        return Err(AssetBundleError::InvalidManifest {
            reason: "min_ferritex_version must not be empty".to_string(),
        });
    }

    parse_version(&manifest.version)
        .map_err(|reason| AssetBundleError::InvalidManifest { reason })?;
    parse_version(&manifest.min_ferritex_version)
        .map_err(|reason| AssetBundleError::InvalidManifest { reason })?;

    Ok(())
}

fn ensure_version_compatible(manifest: &AssetBundleManifest) -> Result<(), AssetBundleError> {
    let current = parse_version(CURRENT_FERRITEX_VERSION).map_err(|reason| {
        AssetBundleError::InvalidManifest {
            reason: format!("invalid ferritex version: {reason}"),
        }
    })?;
    let required = parse_version(&manifest.min_ferritex_version)
        .map_err(|reason| AssetBundleError::InvalidManifest { reason })?;

    if current < required {
        return Err(AssetBundleError::VersionIncompatible {
            bundle_version: manifest.version.clone(),
            required_version: manifest.min_ferritex_version.clone(),
        });
    }

    Ok(())
}

fn parse_version(input: &str) -> Result<Vec<u64>, String> {
    let segments = input
        .split('.')
        .map(|segment| {
            segment
                .parse::<u64>()
                .map_err(|_| format!("invalid version segment `{segment}`"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    if segments.len() < 3 {
        return Err("version must contain at least major.minor.patch".to_string());
    }

    Ok(segments)
}

fn tex_relative_candidate(relative_path: &Path) -> PathBuf {
    if relative_path.extension().is_some() {
        relative_path.to_path_buf()
    } else {
        relative_path.with_extension("tex")
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use ferritex_application::ports::AssetBundleLoaderPort;
    use tempfile::tempdir;

    use super::{AssetBundleError, AssetBundleLoader, AssetBundleManifest};

    fn fixture_root() -> PathBuf {
        tempdir().expect("create tempdir").keep()
    }

    fn write_manifest(
        bundle_root: &Path,
        manifest: &AssetBundleManifest,
    ) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(bundle_root)?;
        std::fs::write(
            bundle_root.join("manifest.json"),
            serde_json::to_vec(manifest).expect("serialize manifest"),
        )
    }

    #[test]
    fn returns_error_when_manifest_is_missing() {
        let bundle_root = fixture_root();
        std::fs::create_dir_all(&bundle_root).expect("create bundle directory");

        let error = AssetBundleLoader::load(&bundle_root).expect_err("bundle should be rejected");

        assert!(matches!(error, AssetBundleError::ManifestNotFound));
    }

    #[test]
    fn returns_error_when_bundle_version_is_incompatible() {
        let bundle_root = fixture_root();
        write_manifest(
            &bundle_root,
            &AssetBundleManifest {
                name: "default".to_string(),
                version: "2026.03.18".to_string(),
                min_ferritex_version: "0.2.0".to_string(),
            },
        )
        .expect("write manifest");

        let error = AssetBundleLoader::load(&bundle_root).expect_err("bundle should be rejected");

        assert!(matches!(
            error,
            AssetBundleError::VersionIncompatible {
                bundle_version,
                required_version
            } if bundle_version == "2026.03.18" && required_version == "0.2.0"
        ));
    }

    #[test]
    fn loads_valid_manifest() {
        let bundle_root = fixture_root();
        let expected = AssetBundleManifest {
            name: "default".to_string(),
            version: "2026.03.18".to_string(),
            min_ferritex_version: "0.1.0".to_string(),
        };
        write_manifest(&bundle_root, &expected).expect("write manifest");

        let manifest = AssetBundleLoader::load(&bundle_root).expect("bundle should load");

        assert_eq!(manifest, expected);
    }

    #[test]
    fn resolves_tex_input_from_bundle_texmf_directory() {
        let bundle_root = fixture_root();
        write_manifest(
            &bundle_root,
            &AssetBundleManifest {
                name: "default".to_string(),
                version: "2026.03.18".to_string(),
                min_ferritex_version: "0.1.0".to_string(),
            },
        )
        .expect("write manifest");
        std::fs::create_dir_all(bundle_root.join("texmf/shared")).expect("create texmf");
        std::fs::write(
            bundle_root.join("texmf/shared/macros.tex"),
            "Bundled macros.\n",
        )
        .expect("write bundled tex input");

        let resolved = AssetBundleLoader.resolve_tex_input(&bundle_root, "shared/macros");

        assert_eq!(
            resolved,
            Some(
                bundle_root
                    .join("texmf/shared/macros.tex")
                    .canonicalize()
                    .expect("canonicalize bundled tex input"),
            )
        );
    }

    #[test]
    fn returns_none_when_bundle_tex_input_is_missing() {
        let bundle_root = fixture_root();
        write_manifest(
            &bundle_root,
            &AssetBundleManifest {
                name: "default".to_string(),
                version: "2026.03.18".to_string(),
                min_ferritex_version: "0.1.0".to_string(),
            },
        )
        .expect("write manifest");

        let resolved = AssetBundleLoader.resolve_tex_input(&bundle_root, "missing");

        assert_eq!(resolved, None);
    }

    #[test]
    fn rejects_path_traversal_in_tex_input_resolution() {
        let bundle_root = fixture_root();
        write_manifest(
            &bundle_root,
            &AssetBundleManifest {
                name: "default".to_string(),
                version: "2026.03.18".to_string(),
                min_ferritex_version: "0.1.0".to_string(),
            },
        )
        .expect("write manifest");
        std::fs::create_dir_all(bundle_root.join("texmf")).expect("create texmf");
        std::fs::write(bundle_root.join("secret.tex"), "SECRET").expect("write secret");

        let resolved = AssetBundleLoader.resolve_tex_input(&bundle_root, "../secret");

        assert_eq!(resolved, None);
    }
}
