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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{AssetBundleError, AssetBundleLoader, AssetBundleManifest};

    fn fixture_root() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before unix epoch")
            .as_nanos();

        std::env::temp_dir().join(format!("ferritex-asset-bundle-loader-{unique}"))
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
}
