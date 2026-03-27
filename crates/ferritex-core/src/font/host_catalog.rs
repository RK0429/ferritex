use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::policy::{FileAccessGate, PathAccessDecision};

use super::opentype::extract_font_names;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostFontCatalog {
    entries: BTreeMap<String, Vec<PathBuf>>,
}

impl HostFontCatalog {
    pub fn resolve(&self, name: &str) -> Option<PathBuf> {
        self.entries
            .get(&normalize_font_name(name))
            .and_then(|paths| paths.first())
            .cloned()
    }
}

pub fn resolve_host_font(
    name: &str,
    roots: &[PathBuf],
    file_access_gate: &dyn FileAccessGate,
) -> Option<PathBuf> {
    let catalog_roots = normalized_catalog_roots(roots);
    if catalog_roots.is_empty() {
        return None;
    }

    host_font_catalog(&catalog_roots, file_access_gate).resolve(name)
}

fn host_font_catalog(roots: &[PathBuf], file_access_gate: &dyn FileAccessGate) -> HostFontCatalog {
    static CACHE: OnceLock<Mutex<BTreeMap<Vec<PathBuf>, HostFontCatalog>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));

    if let Some(existing) = cache
        .lock()
        .expect("host font cache lock")
        .get(roots)
        .cloned()
    {
        return existing;
    }

    let built = build_host_font_catalog(roots, file_access_gate);
    cache
        .lock()
        .expect("host font cache lock")
        .insert(roots.to_vec(), built.clone());
    built
}

fn build_host_font_catalog(
    roots: &[PathBuf],
    file_access_gate: &dyn FileAccessGate,
) -> HostFontCatalog {
    let mut visited = BTreeSet::new();
    let mut candidates = Vec::new();
    for root in roots {
        collect_opentype_candidates_in_dir(root, &mut visited, &mut candidates);
    }

    let mut entries = BTreeMap::new();
    for candidate in candidates {
        if file_access_gate.check_read(&candidate) == PathAccessDecision::Denied {
            continue;
        }

        let bytes = match file_access_gate.read_file(&candidate) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let mut names = BTreeSet::new();
        if let Some(stem) = candidate.file_stem().and_then(|stem| stem.to_str()) {
            let normalized = normalize_font_name(stem);
            if !normalized.is_empty() {
                names.insert(normalized);
            }
        }
        if let Ok(font_names) = extract_font_names(&bytes) {
            for font_name in font_names {
                let normalized = normalize_font_name(&font_name);
                if !normalized.is_empty() {
                    names.insert(normalized);
                }
            }
        }

        for name in names {
            entries
                .entry(name)
                .or_insert_with(Vec::new)
                .push(candidate.clone());
        }
    }

    for paths in entries.values_mut() {
        paths.sort();
        paths.dedup();
    }

    HostFontCatalog { entries }
}

fn normalized_catalog_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut normalized = roots
        .iter()
        .map(|path| normalize_existing_path(path))
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn collect_opentype_candidates_in_dir(
    path: &Path,
    visited: &mut BTreeSet<PathBuf>,
    candidates: &mut Vec<PathBuf>,
) {
    let normalized = normalize_existing_path(path);
    if !visited.insert(normalized.clone()) {
        return;
    }

    if normalized.is_file() {
        if is_opentype_path(&normalized) {
            candidates.push(normalized);
        }
        return;
    }
    if !normalized.is_dir() {
        return;
    }

    let Ok(read_dir) = std::fs::read_dir(&normalized) else {
        return;
    };
    let mut entries = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();

    for entry in entries {
        if entry.is_dir() {
            collect_opentype_candidates_in_dir(&entry, visited, candidates);
        } else if is_opentype_path(&entry) {
            candidates.push(entry);
        }
    }
}

fn is_opentype_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            extension.eq_ignore_ascii_case("ttf") || extension.eq_ignore_ascii_case("otf")
        })
        .unwrap_or(false)
}

fn normalize_font_name(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn normalize_existing_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::resolve_host_font;
    use crate::font::opentype::{minimal_test_font_bytes, named_test_font_bytes};
    use crate::policy::{FileAccessError, FileAccessGate, PathAccessDecision};

    struct FsTestFileAccessGate;

    impl FileAccessGate for FsTestFileAccessGate {
        fn ensure_directory(&self, path: &Path) -> Result<(), FileAccessError> {
            fs::create_dir_all(path).map_err(FileAccessError::from)
        }

        fn check_read(&self, _path: &Path) -> PathAccessDecision {
            PathAccessDecision::Allowed
        }

        fn check_write(&self, _path: &Path) -> PathAccessDecision {
            PathAccessDecision::Allowed
        }

        fn check_readback(
            &self,
            _path: &Path,
            _primary_input: &Path,
            _jobname: &str,
        ) -> PathAccessDecision {
            PathAccessDecision::Allowed
        }

        fn read_file(&self, path: &Path) -> Result<Vec<u8>, FileAccessError> {
            fs::read(path).map_err(FileAccessError::from)
        }

        fn write_file(&self, path: &Path, content: &[u8]) -> Result<(), FileAccessError> {
            fs::write(path, content).map_err(FileAccessError::from)
        }

        fn read_readback(
            &self,
            path: &Path,
            _primary_input: &Path,
            _jobname: &str,
        ) -> Result<Vec<u8>, FileAccessError> {
            fs::read(path).map_err(FileAccessError::from)
        }
    }

    struct FixtureDir {
        root: PathBuf,
    }

    impl FixtureDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);

            let temp_root = std::env::temp_dir();
            loop {
                let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
                let root = temp_root.join(format!(
                    "ferritex-host-font-catalog-{}-{unique}",
                    std::process::id()
                ));
                match fs::create_dir(&root) {
                    Ok(()) => return Self { root },
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create fixture root: {error}"),
                }
            }
        }

        fn path(&self) -> &Path {
            &self.root
        }

        fn write_font(&self, relative_path: &str, bytes: &[u8]) -> PathBuf {
            let path = self.root.join(relative_path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create font parent");
            }
            fs::write(&path, bytes).expect("write test font");
            path
        }
    }

    impl Drop for FixtureDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn resolves_host_font_by_name_table_alias() {
        let fixture = FixtureDir::new();
        let expected_path = fixture.write_font(
            "catalog/opaque-file-name.ttf",
            &named_test_font_bytes("Noto Serif", "Noto Serif Regular"),
        );

        let resolved = resolve_host_font(
            "Noto Serif",
            &[fixture.path().join("catalog")],
            &FsTestFileAccessGate,
        );

        assert_eq!(
            resolved,
            Some(
                expected_path
                    .canonicalize()
                    .expect("canonical expected path")
            )
        );
    }

    #[test]
    fn falls_back_to_file_stem_when_name_table_is_missing() {
        let fixture = FixtureDir::new();
        let expected_path = fixture.write_font("catalog/TestFont.ttf", &minimal_test_font_bytes());

        let resolved = resolve_host_font(
            "TestFont",
            &[fixture.path().join("catalog")],
            &FsTestFileAccessGate,
        );

        assert_eq!(
            resolved,
            Some(
                expected_path
                    .canonicalize()
                    .expect("canonical expected path")
            )
        );
    }

    #[test]
    fn returns_none_when_host_catalog_has_no_match() {
        let fixture = FixtureDir::new();
        fixture.write_font("catalog/TestFont.ttf", &minimal_test_font_bytes());

        let resolved = resolve_host_font(
            "MissingFont",
            &[fixture.path().join("catalog")],
            &FsTestFileAccessGate,
        );

        assert_eq!(resolved, None);
    }
}
