use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;

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
    if let Some(platform_catalog) = build_platform_host_font_catalog(roots) {
        return platform_catalog;
    }

    build_scanned_host_font_catalog(roots, file_access_gate)
}

fn build_platform_host_font_catalog(roots: &[PathBuf]) -> Option<HostFontCatalog> {
    let entries = discover_platform_font_entries()?
        .into_iter()
        .filter(|entry| path_is_within_roots(&entry.path, roots))
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return None;
    }

    Some(build_catalog_from_entries(entries))
}

fn build_scanned_host_font_catalog(
    roots: &[PathBuf],
    file_access_gate: &dyn FileAccessGate,
) -> HostFontCatalog {
    let mut visited = BTreeSet::new();
    let mut candidates = Vec::new();
    for root in roots {
        collect_opentype_candidates_in_dir(root, &mut visited, &mut candidates);
    }

    let mut entries = Vec::new();
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
        if !names.is_empty() {
            entries.push(CatalogEntry {
                path: candidate,
                names,
            });
        }
    }

    build_catalog_from_entries(entries)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CatalogEntry {
    path: PathBuf,
    names: BTreeSet<String>,
}

fn build_catalog_from_entries(entries: Vec<CatalogEntry>) -> HostFontCatalog {
    let mut catalog = BTreeMap::new();
    for entry in entries {
        for name in entry.names {
            catalog
                .entry(name)
                .or_insert_with(Vec::new)
                .push(entry.path.clone());
        }
    }

    for paths in catalog.values_mut() {
        paths.sort();
        paths.dedup();
    }

    HostFontCatalog { entries: catalog }
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

fn path_is_within_roots(path: &Path, roots: &[PathBuf]) -> bool {
    let normalized_path = normalize_existing_path(path);
    roots.iter().any(|root| normalized_path.starts_with(root))
}

fn discover_platform_font_entries() -> Option<Vec<CatalogEntry>> {
    #[cfg(target_os = "macos")]
    {
        discover_macos_font_entries().or_else(discover_fontconfig_entries)
    }

    #[cfg(target_os = "linux")]
    {
        discover_fontconfig_entries()
    }

    #[cfg(target_os = "windows")]
    {
        discover_windows_font_entries()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn discover_macos_font_entries() -> Option<Vec<CatalogEntry>> {
    let output = Command::new("system_profiler")
        .args(["SPFontsDataType", "-json", "-detailLevel", "mini"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    parse_system_profiler_entries(&output.stdout)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn discover_fontconfig_entries() -> Option<Vec<CatalogEntry>> {
    let output = Command::new("fc-list")
        .args(["--format", "%{family}\t%{fullname}\t%{file}\n"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    parse_fontconfig_entries(&output.stdout)
}

#[cfg(target_os = "windows")]
fn discover_windows_font_entries() -> Option<Vec<CatalogEntry>> {
    let mut entries = Vec::new();
    let windir = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| windir.join("Fonts"));

    let sources = [
        (
            r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts",
            windir.join("Fonts"),
        ),
        (
            r"HKCU\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts",
            local_app_data.join("Microsoft/Windows/Fonts"),
        ),
    ];

    for (registry_path, font_dir) in sources {
        let Some(output) = Command::new("reg")
            .args(["query", registry_path])
            .output()
            .ok()
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        entries.extend(parse_windows_registry_entries(&output.stdout, &font_dir));
    }

    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

#[derive(Debug, Deserialize)]
struct SystemProfilerRoot {
    #[serde(rename = "SPFontsDataType", default)]
    fonts: Vec<SystemProfilerFont>,
}

#[derive(Debug, Deserialize)]
struct SystemProfilerFont {
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    enabled: Option<String>,
    #[serde(default)]
    valid: Option<String>,
    #[serde(rename = "_name", default)]
    name: Option<String>,
    #[serde(default)]
    typefaces: Vec<SystemProfilerTypeface>,
}

#[derive(Debug, Deserialize)]
struct SystemProfilerTypeface {
    #[serde(default)]
    family: Option<String>,
    #[serde(default)]
    fullname: Option<String>,
    #[serde(rename = "_name", default)]
    name: Option<String>,
    #[serde(default)]
    enabled: Option<String>,
    #[serde(default)]
    valid: Option<String>,
}

fn parse_system_profiler_entries(bytes: &[u8]) -> Option<Vec<CatalogEntry>> {
    let root: SystemProfilerRoot = serde_json::from_slice(bytes).ok()?;
    let mut entries = Vec::new();

    for font in root.fonts {
        if !flag_allows_entry(font.enabled.as_deref()) || !flag_allows_entry(font.valid.as_deref())
        {
            continue;
        }
        let Some(path) = font.path else {
            continue;
        };
        if !is_opentype_path(&path) {
            continue;
        }

        let mut names = BTreeSet::new();
        collect_catalog_name(&mut names, font.name.as_deref());
        collect_path_stem_name(&mut names, &path);
        for face in font.typefaces {
            if !flag_allows_entry(face.enabled.as_deref())
                || !flag_allows_entry(face.valid.as_deref())
            {
                continue;
            }
            collect_catalog_name(&mut names, face.family.as_deref());
            collect_catalog_name(&mut names, face.fullname.as_deref());
            collect_catalog_name(&mut names, face.name.as_deref());
        }

        if !names.is_empty() {
            entries.push(CatalogEntry { path, names });
        }
    }

    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

fn parse_fontconfig_entries(bytes: &[u8]) -> Option<Vec<CatalogEntry>> {
    let stdout = String::from_utf8(bytes.to_vec()).ok()?;
    let mut entries = Vec::new();

    for line in stdout.lines() {
        let mut columns = line.splitn(3, '\t');
        let family = columns.next().unwrap_or_default();
        let fullname = columns.next().unwrap_or_default();
        let file = columns.next().unwrap_or_default().trim();
        if file.is_empty() {
            continue;
        }

        let path = PathBuf::from(file);
        if !is_opentype_path(&path) {
            continue;
        }

        let mut names = BTreeSet::new();
        collect_catalog_name_list(&mut names, family);
        collect_catalog_name_list(&mut names, fullname);
        collect_path_stem_name(&mut names, &path);
        if !names.is_empty() {
            entries.push(CatalogEntry { path, names });
        }
    }

    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

#[cfg(target_os = "windows")]
fn parse_windows_registry_entries(bytes: &[u8], font_dir: &Path) -> Vec<CatalogEntry> {
    let stdout = String::from_utf8_lossy(bytes);
    let mut entries = Vec::new();

    for line in stdout.lines() {
        let columns = line
            .split("    ")
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        if columns.len() < 3 {
            continue;
        }
        let path_value = columns[2];
        let path = PathBuf::from(path_value);
        let path = if path.is_absolute() {
            path
        } else {
            font_dir.join(path)
        };
        if !is_opentype_path(&path) {
            continue;
        }

        let mut names = BTreeSet::new();
        collect_catalog_name(&mut names, Some(strip_windows_font_suffix(columns[0])));
        collect_path_stem_name(&mut names, &path);
        if !names.is_empty() {
            entries.push(CatalogEntry { path, names });
        }
    }

    entries
}

#[cfg(target_os = "windows")]
fn strip_windows_font_suffix(name: &str) -> &str {
    name.split(" (").next().unwrap_or(name)
}

fn flag_allows_entry(flag: Option<&str>) -> bool {
    !matches!(flag, Some(value) if value.eq_ignore_ascii_case("no"))
}

fn collect_catalog_name_list(names: &mut BTreeSet<String>, value: &str) {
    for candidate in value.split(',') {
        collect_catalog_name(names, Some(candidate));
    }
}

fn collect_catalog_name(names: &mut BTreeSet<String>, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };
    let normalized = normalize_font_name(value.trim());
    if !normalized.is_empty() {
        names.insert(normalized);
    }
}

fn collect_path_stem_name(names: &mut BTreeSet<String>, path: &Path) {
    if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
        let normalized = normalize_font_name(stem);
        if !normalized.is_empty() {
            names.insert(normalized);
        }
    }
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

    use super::{parse_fontconfig_entries, parse_system_profiler_entries, resolve_host_font};
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

    #[test]
    fn parses_system_profiler_catalog_into_aliases() {
        let entries = parse_system_profiler_entries(
            br#"{
  "SPFontsDataType": [
    {
      "_name": "OpaqueFont-Regular.otf",
      "enabled": "yes",
      "valid": "yes",
      "path": "/Library/Fonts/OpaqueFont-Regular.otf",
      "typefaces": [
        {
          "_name": "OpaqueFont-Regular",
          "enabled": "yes",
          "valid": "yes",
          "family": "Opaque Font",
          "fullname": "Opaque Font Regular"
        }
      ]
    }
  ]
}"#,
        )
        .expect("parse system profiler catalog");

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].path,
            PathBuf::from("/Library/Fonts/OpaqueFont-Regular.otf")
        );
        assert!(entries[0].names.contains("opaquefont"));
        assert!(entries[0].names.contains("opaquefontregular"));
    }

    #[test]
    fn parses_fontconfig_catalog_into_aliases() {
        let entries = parse_fontconfig_entries(
            b"Opaque Font,Opaque Font UI\tOpaque Font Regular\t/usr/share/fonts/opentype/OpaqueFont-Regular.otf\n",
        )
        .expect("parse fontconfig catalog");

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].path,
            PathBuf::from("/usr/share/fonts/opentype/OpaqueFont-Regular.otf")
        );
        assert!(entries[0].names.contains("opaquefont"));
        assert!(entries[0].names.contains("opaquefontui"));
        assert!(entries[0].names.contains("opaquefontregular"));
    }
}
