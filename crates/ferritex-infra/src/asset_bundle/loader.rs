use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use ferritex_application::ports::AssetBundleLoaderPort;
use memmap2::MmapOptions;
use serde::{Deserialize, Serialize};

const CURRENT_FERRITEX_VERSION: &str = "0.1.0";
const CURRENT_BUNDLE_FORMAT_VERSION: u32 = 1;
const DEFAULT_ASSET_INDEX_PATH: &str = "asset-index.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetBundleManifest {
    pub name: String,
    pub version: String,
    pub min_ferritex_version: String,
    #[serde(default = "default_bundle_format_version")]
    pub format_version: u32,
    #[serde(default = "default_asset_index_path")]
    pub asset_index_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AssetBundleIndex {
    #[serde(default)]
    pub tex_inputs: BTreeMap<String, String>,
    #[serde(default)]
    pub packages: BTreeMap<String, String>,
    #[serde(default)]
    pub opentype_fonts: BTreeMap<String, String>,
    #[serde(default)]
    pub tfm_fonts: BTreeMap<String, String>,
    #[serde(default)]
    pub default_opentype_fonts: Vec<String>,
}

struct LoadedAssetBundle {
    manifest: AssetBundleManifest,
    index: AssetBundleIndex,
}

pub struct AssetBundleLoader;

impl AssetBundleLoaderPort for AssetBundleLoader {
    fn validate(&self, bundle_path: &Path) -> Result<(), String> {
        Self::load_indexed_bundle(bundle_path)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn resolve_tex_input(&self, bundle_path: &Path, relative_path: &str) -> Option<PathBuf> {
        let bundle = Self::load_indexed_bundle(bundle_path).ok()?;
        lookup_bundle_path(
            &bundle.index.tex_inputs,
            &tex_lookup_keys(relative_path),
            bundle_path,
        )
    }

    fn resolve_package(
        &self,
        bundle_path: &Path,
        package_name: &str,
        project_root: Option<&Path>,
    ) -> Option<PathBuf> {
        let package_relative = package_relative_candidate(package_name);

        if let Some(project_root) = project_root {
            let project_candidate = project_root.join(&package_relative);
            if let Some(resolved) = resolve_guarded(project_root, &project_candidate) {
                return Some(resolved);
            }
        }

        let bundle = Self::load_indexed_bundle(bundle_path).ok()?;
        lookup_bundle_path(
            &bundle.index.packages,
            &package_lookup_keys(package_name),
            bundle_path,
        )
    }

    fn resolve_opentype_font(&self, bundle_path: &Path, font_name: &str) -> Option<PathBuf> {
        let bundle = Self::load_indexed_bundle(bundle_path).ok()?;
        let key = normalize_font_key(font_name);
        bundle
            .index
            .opentype_fonts
            .get(&key)
            .and_then(|relative| resolve_bundle_relative(bundle_path, relative))
    }

    fn resolve_default_opentype_font(&self, bundle_path: &Path) -> Option<PathBuf> {
        let bundle = Self::load_indexed_bundle(bundle_path).ok()?;
        bundle
            .index
            .default_opentype_fonts
            .iter()
            .find_map(|relative| resolve_bundle_relative(bundle_path, relative))
    }

    fn resolve_tfm_font(&self, bundle_path: &Path, font_name: &str) -> Option<PathBuf> {
        let bundle = Self::load_indexed_bundle(bundle_path).ok()?;
        let key = normalize_filename_key(font_name);
        bundle
            .index
            .tfm_fonts
            .get(&key)
            .and_then(|relative| resolve_bundle_relative(bundle_path, relative))
    }
}

impl AssetBundleLoader {
    pub fn load(bundle_path: &Path) -> Result<AssetBundleManifest, AssetBundleError> {
        Self::load_indexed_bundle(bundle_path).map(|bundle| bundle.manifest)
    }

    fn load_indexed_bundle(bundle_path: &Path) -> Result<LoadedAssetBundle, AssetBundleError> {
        if !bundle_path.exists() {
            return Err(AssetBundleError::NotFound {
                path: bundle_path.to_path_buf(),
            });
        }

        let manifest = load_manifest(bundle_path)?;
        let index = load_index(bundle_path, &manifest)?;
        Ok(LoadedAssetBundle { manifest, index })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AssetBundleError {
    #[error("bundle not found at {path}")]
    NotFound { path: PathBuf },
    #[error("manifest not found in bundle")]
    ManifestNotFound,
    #[error("asset index not found at {path}")]
    AssetIndexNotFound { path: PathBuf },
    #[error("invalid manifest: {reason}")]
    InvalidManifest { reason: String },
    #[error("invalid asset index: {reason}")]
    InvalidAssetIndex { reason: String },
    #[error("unsupported bundle format version {actual}; supported version is {supported}")]
    UnsupportedFormatVersion { actual: u32, supported: u32 },
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

fn load_manifest(bundle_path: &Path) -> Result<AssetBundleManifest, AssetBundleError> {
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

fn load_index(
    bundle_path: &Path,
    manifest: &AssetBundleManifest,
) -> Result<AssetBundleIndex, AssetBundleError> {
    let index_path = bundle_path.join(&manifest.asset_index_path);
    if !index_path.is_file() {
        return Err(AssetBundleError::AssetIndexNotFound { path: index_path });
    }

    let file = std::fs::File::open(&index_path)?;
    let mmap = unsafe {
        // SAFETY: The file is opened read-only and the mapped bytes are only used
        // for immutable JSON parsing during this function call.
        MmapOptions::new().map(&file)
    }?;
    let index: AssetBundleIndex =
        serde_json::from_slice(&mmap).map_err(|source| AssetBundleError::InvalidAssetIndex {
            reason: source.to_string(),
        })?;
    validate_asset_index(&index)?;
    Ok(index)
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

    if manifest.asset_index_path.trim().is_empty() {
        return Err(AssetBundleError::InvalidManifest {
            reason: "asset_index_path must not be empty".to_string(),
        });
    }

    if manifest.format_version > CURRENT_BUNDLE_FORMAT_VERSION {
        return Err(AssetBundleError::UnsupportedFormatVersion {
            actual: manifest.format_version,
            supported: CURRENT_BUNDLE_FORMAT_VERSION,
        });
    }

    validate_relative_asset_path(&manifest.asset_index_path)
        .map_err(|reason| AssetBundleError::InvalidManifest { reason })?;
    parse_version(&manifest.version)
        .map_err(|reason| AssetBundleError::InvalidManifest { reason })?;
    parse_version(&manifest.min_ferritex_version)
        .map_err(|reason| AssetBundleError::InvalidManifest { reason })?;

    Ok(())
}

fn validate_asset_index(index: &AssetBundleIndex) -> Result<(), AssetBundleError> {
    for relative in index
        .tex_inputs
        .values()
        .chain(index.packages.values())
        .chain(index.opentype_fonts.values())
        .chain(index.tfm_fonts.values())
        .chain(index.default_opentype_fonts.iter())
    {
        validate_relative_asset_path(relative)
            .map_err(|reason| AssetBundleError::InvalidAssetIndex { reason })?;
    }

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

fn default_bundle_format_version() -> u32 {
    CURRENT_BUNDLE_FORMAT_VERSION
}

fn default_asset_index_path() -> String {
    DEFAULT_ASSET_INDEX_PATH.to_string()
}

fn tex_lookup_keys(relative_path: &str) -> Vec<String> {
    let normalized = normalize_relative_key(relative_path);
    let path = Path::new(&normalized);
    let mut keys = Vec::new();
    push_unique_key(&mut keys, normalized.clone());
    if path.extension().is_none() {
        push_unique_key(
            &mut keys,
            path.with_extension("tex")
                .to_string_lossy()
                .replace('\\', "/"),
        );
    } else if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("tex"))
    {
        push_unique_key(
            &mut keys,
            path.with_extension("").to_string_lossy().replace('\\', "/"),
        );
    }
    keys
}

fn package_lookup_keys(package_name: &str) -> Vec<String> {
    let normalized = normalize_relative_key(package_name);
    let path = Path::new(&normalized);
    let mut keys = Vec::new();
    push_unique_key(&mut keys, normalized.clone());
    push_unique_key(&mut keys, normalized.to_ascii_lowercase());
    if path.extension().is_none() {
        let with_extension = path
            .with_extension("sty")
            .to_string_lossy()
            .replace('\\', "/");
        push_unique_key(&mut keys, with_extension.clone());
        push_unique_key(&mut keys, with_extension.to_ascii_lowercase());
    }
    keys
}

fn lookup_bundle_path(
    entries: &BTreeMap<String, String>,
    keys: &[String],
    bundle_path: &Path,
) -> Option<PathBuf> {
    keys.iter()
        .find_map(|key| entries.get(key))
        .and_then(|relative| resolve_bundle_relative(bundle_path, relative))
}

fn resolve_bundle_relative(bundle_path: &Path, relative_path: &str) -> Option<PathBuf> {
    resolve_guarded(bundle_path, &bundle_path.join(relative_path))
}

fn validate_relative_asset_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("asset paths must not be empty".to_string());
    }

    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err(format!("asset path `{path}` must be relative"));
    }

    if candidate.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!(
            "asset path `{path}` must not escape the bundle root"
        ));
    }

    Ok(())
}

fn normalize_relative_key(value: &str) -> String {
    value
        .trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

fn normalize_font_key(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn normalize_filename_key(value: &str) -> String {
    Path::new(value)
        .file_stem()
        .or_else(|| Path::new(value).file_name())
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn push_unique_key(keys: &mut Vec<String>, key: String) {
    if !key.is_empty() && !keys.contains(&key) {
        keys.push(key);
    }
}

fn package_relative_candidate(package_name: &str) -> PathBuf {
    PathBuf::from(package_name).with_extension("sty")
}

fn resolve_guarded(root: &Path, candidate: &Path) -> Option<PathBuf> {
    let resolved = candidate.canonicalize().ok()?;
    let root_resolved = root.canonicalize().ok()?;
    resolved.starts_with(&root_resolved).then_some(resolved)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use ferritex_application::ports::AssetBundleLoaderPort;
    use tempfile::tempdir;

    use super::{AssetBundleError, AssetBundleIndex, AssetBundleLoader, AssetBundleManifest};

    fn fixture_root() -> PathBuf {
        tempdir().expect("create tempdir").keep()
    }

    fn manifest(
        min_ferritex_version: &str,
        format_version: u32,
        asset_index_path: &str,
    ) -> AssetBundleManifest {
        AssetBundleManifest {
            name: "default".to_string(),
            version: "2026.03.18".to_string(),
            min_ferritex_version: min_ferritex_version.to_string(),
            format_version,
            asset_index_path: asset_index_path.to_string(),
        }
    }

    fn write_bundle(
        bundle_root: &Path,
        manifest: &AssetBundleManifest,
        index: &AssetBundleIndex,
    ) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(bundle_root)?;
        std::fs::write(
            bundle_root.join("manifest.json"),
            serde_json::to_vec(manifest).expect("serialize manifest"),
        )?;
        std::fs::write(
            bundle_root.join(&manifest.asset_index_path),
            serde_json::to_vec(index).expect("serialize index"),
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
    fn returns_error_when_asset_index_is_missing() {
        let bundle_root = fixture_root();
        std::fs::create_dir_all(&bundle_root).expect("create bundle directory");
        std::fs::write(
            bundle_root.join("manifest.json"),
            serde_json::to_vec(&manifest("0.1.0", 1, "asset-index.json"))
                .expect("serialize manifest"),
        )
        .expect("write manifest");

        let error = AssetBundleLoader::load(&bundle_root).expect_err("bundle should be rejected");

        assert!(matches!(error, AssetBundleError::AssetIndexNotFound { .. }));
    }

    #[test]
    fn returns_error_when_bundle_version_is_incompatible() {
        let bundle_root = fixture_root();
        write_bundle(
            &bundle_root,
            &manifest("0.2.0", 1, "asset-index.json"),
            &AssetBundleIndex::default(),
        )
        .expect("write bundle");

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
    fn returns_error_when_bundle_format_is_unsupported() {
        let bundle_root = fixture_root();
        write_bundle(
            &bundle_root,
            &manifest("0.1.0", 2, "asset-index.json"),
            &AssetBundleIndex::default(),
        )
        .expect("write bundle");

        let error = AssetBundleLoader::load(&bundle_root).expect_err("bundle should be rejected");

        assert!(matches!(
            error,
            AssetBundleError::UnsupportedFormatVersion {
                actual: 2,
                supported: 1
            }
        ));
    }

    #[test]
    fn loads_valid_manifest() {
        let bundle_root = fixture_root();
        let expected = manifest("0.1.0", 1, "asset-index.json");
        write_bundle(&bundle_root, &expected, &AssetBundleIndex::default()).expect("write bundle");

        let manifest = AssetBundleLoader::load(&bundle_root).expect("bundle should load");

        assert_eq!(manifest, expected);
    }

    #[test]
    fn resolves_tex_input_from_bundle_texmf_directory() {
        let bundle_root = fixture_root();
        let index = AssetBundleIndex {
            tex_inputs: BTreeMap::from([(
                "shared/macros.tex".to_string(),
                "texmf/shared/macros.tex".to_string(),
            )]),
            ..AssetBundleIndex::default()
        };
        write_bundle(
            &bundle_root,
            &manifest("0.1.0", 1, "asset-index.json"),
            &index,
        )
        .expect("write bundle");
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
        write_bundle(
            &bundle_root,
            &manifest("0.1.0", 1, "asset-index.json"),
            &AssetBundleIndex::default(),
        )
        .expect("write bundle");

        let resolved = AssetBundleLoader.resolve_tex_input(&bundle_root, "missing");

        assert_eq!(resolved, None);
    }

    #[test]
    fn rejects_path_traversal_in_tex_input_resolution() {
        let bundle_root = fixture_root();
        let index = AssetBundleIndex {
            tex_inputs: BTreeMap::from([("secret.tex".to_string(), "../secret.tex".to_string())]),
            ..AssetBundleIndex::default()
        };
        write_bundle(
            &bundle_root,
            &manifest("0.1.0", 1, "asset-index.json"),
            &index,
        )
        .expect("write bundle");

        let error = AssetBundleLoader::load(&bundle_root).expect_err("bundle should be rejected");

        assert!(matches!(error, AssetBundleError::InvalidAssetIndex { .. }));
    }

    #[test]
    fn resolves_package_from_bundle_texmf_directory() {
        let bundle_root = fixture_root();
        let index = AssetBundleIndex {
            packages: BTreeMap::from([("mypkg.sty".to_string(), "texmf/mypkg.sty".to_string())]),
            ..AssetBundleIndex::default()
        };
        write_bundle(
            &bundle_root,
            &manifest("0.1.0", 1, "asset-index.json"),
            &index,
        )
        .expect("write bundle");
        std::fs::create_dir_all(bundle_root.join("texmf")).expect("create texmf");
        std::fs::write(bundle_root.join("texmf/mypkg.sty"), "% bundle package\n")
            .expect("write bundled package");

        let resolved = AssetBundleLoader.resolve_package(&bundle_root, "mypkg", None);

        assert_eq!(
            resolved,
            Some(
                bundle_root
                    .join("texmf/mypkg.sty")
                    .canonicalize()
                    .expect("canonicalize bundled package"),
            )
        );
    }

    #[test]
    fn resolve_package_prefers_project_root_over_bundle() {
        let bundle_root = fixture_root();
        let project_root = fixture_root();
        let index = AssetBundleIndex {
            packages: BTreeMap::from([("mypkg.sty".to_string(), "texmf/mypkg.sty".to_string())]),
            ..AssetBundleIndex::default()
        };
        write_bundle(
            &bundle_root,
            &manifest("0.1.0", 1, "asset-index.json"),
            &index,
        )
        .expect("write bundle");
        std::fs::create_dir_all(bundle_root.join("texmf")).expect("create texmf");
        std::fs::write(bundle_root.join("texmf/mypkg.sty"), "% bundle package\n")
            .expect("write bundled package");
        std::fs::write(project_root.join("mypkg.sty"), "% project package\n")
            .expect("write project package");

        let resolved =
            AssetBundleLoader.resolve_package(&bundle_root, "mypkg", Some(&project_root));

        assert_eq!(
            resolved,
            Some(
                project_root
                    .join("mypkg.sty")
                    .canonicalize()
                    .expect("canonicalize project package"),
            )
        );
    }

    #[test]
    fn rejects_path_traversal_in_package_resolution() {
        let bundle_root = fixture_root();
        let project_root = fixture_root();
        let index = AssetBundleIndex {
            packages: BTreeMap::from([("secret.sty".to_string(), "../secret.sty".to_string())]),
            ..AssetBundleIndex::default()
        };
        write_bundle(
            &bundle_root,
            &manifest("0.1.0", 1, "asset-index.json"),
            &index,
        )
        .expect("write bundle");
        let error = AssetBundleLoader::load(&bundle_root).expect_err("bundle should be rejected");
        assert!(matches!(error, AssetBundleError::InvalidAssetIndex { .. }));
        std::fs::write(project_root.join("secret.sty"), "% project secret\n")
            .expect("write project secret");

        let project_escape =
            AssetBundleLoader.resolve_package(&bundle_root, "../../secret", Some(&project_root));

        assert_eq!(project_escape, None);
    }

    #[test]
    fn resolves_named_opentype_font_from_asset_index() {
        let bundle_root = fixture_root();
        let index = AssetBundleIndex {
            opentype_fonts: BTreeMap::from([(
                "testsans".to_string(),
                "texmf/fonts/truetype/TestSans.ttf".to_string(),
            )]),
            default_opentype_fonts: vec!["texmf/fonts/truetype/TestSans.ttf".to_string()],
            ..AssetBundleIndex::default()
        };
        write_bundle(
            &bundle_root,
            &manifest("0.1.0", 1, "asset-index.json"),
            &index,
        )
        .expect("write bundle");
        let font_path = bundle_root.join("texmf/fonts/truetype/TestSans.ttf");
        std::fs::create_dir_all(font_path.parent().expect("font parent"))
            .expect("create font directory");
        std::fs::write(&font_path, b"dummy").expect("write font file");

        let resolved = AssetBundleLoader
            .resolve_opentype_font(&bundle_root, "Test Sans")
            .expect("resolve bundle font");

        assert_eq!(
            resolved,
            font_path.canonicalize().expect("canonicalize font path")
        );
        assert_eq!(
            AssetBundleLoader
                .resolve_default_opentype_font(&bundle_root)
                .expect("resolve default font"),
            font_path.canonicalize().expect("canonicalize font path")
        );
    }

    #[test]
    fn resolves_tfm_font_from_asset_index() {
        let bundle_root = fixture_root();
        let index = AssetBundleIndex {
            tfm_fonts: BTreeMap::from([(
                "cmr10".to_string(),
                "texmf/fonts/tfm/public/cm/cmr10.tfm".to_string(),
            )]),
            ..AssetBundleIndex::default()
        };
        write_bundle(
            &bundle_root,
            &manifest("0.1.0", 1, "asset-index.json"),
            &index,
        )
        .expect("write bundle");
        let tfm_path = bundle_root.join("texmf/fonts/tfm/public/cm/cmr10.tfm");
        std::fs::create_dir_all(tfm_path.parent().expect("tfm parent"))
            .expect("create tfm directory");
        std::fs::write(&tfm_path, b"dummy").expect("write tfm file");

        let resolved = AssetBundleLoader
            .resolve_tfm_font(&bundle_root, "cmr10")
            .expect("resolve tfm font");

        assert_eq!(
            resolved,
            tfm_path.canonicalize().expect("canonicalize tfm path")
        );
    }
}
