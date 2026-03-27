use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::host_catalog::resolve_host_font;
use super::opentype::{extract_font_names, OpenTypeFont};
use crate::policy::FileAccessGate;
use crate::policy::PathAccessDecision;

pub const OPENTYPE_FONT_SEARCH_ROOTS: [&str; 4] = [
    "texmf/fonts/truetype",
    "fonts/truetype",
    "texmf/fonts/opentype",
    "fonts/opentype",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFont {
    pub path: PathBuf,
    pub font: OpenTypeFont,
    pub base_font_name: String,
}

pub fn resolve_named_font(
    name: &str,
    input_dir: &Path,
    project_root: &Path,
    overlay_roots: &[PathBuf],
    asset_bundle_path: Option<&Path>,
    host_font_roots: &[PathBuf],
    file_access_gate: &dyn FileAccessGate,
) -> Option<ResolvedFont> {
    let requested_name = name.trim();
    if requested_name.is_empty() {
        return None;
    }

    for candidate in collect_flat_opentype_candidates(input_dir) {
        if let Some(font) = try_load_named_candidate(&candidate, requested_name, file_access_gate) {
            return Some(font);
        }
    }

    for candidate in collect_flat_opentype_candidates(&project_root.join("fonts")) {
        if let Some(font) = try_load_named_candidate(&candidate, requested_name, file_access_gate) {
            return Some(font);
        }
    }

    let mut visited = BTreeSet::new();
    let mut overlay_candidates = Vec::new();
    for overlay_root in overlay_roots {
        collect_opentype_candidates_in_dir(overlay_root, &mut visited, &mut overlay_candidates);
    }

    for candidate in overlay_candidates {
        if let Some(font) = try_load_named_candidate(&candidate, requested_name, file_access_gate) {
            return Some(font);
        }
    }

    if let Some(bundle_path) = asset_bundle_path {
        let mut visited = BTreeSet::new();
        let mut candidates = Vec::new();

        for root in OPENTYPE_FONT_SEARCH_ROOTS {
            collect_opentype_candidates_in_dir(
                &bundle_path.join(root),
                &mut visited,
                &mut candidates,
            );
        }
        collect_opentype_candidates_in_dir(bundle_path, &mut visited, &mut candidates);

        for candidate in candidates {
            if let Some(font) =
                try_load_named_candidate(&candidate, requested_name, file_access_gate)
            {
                return Some(font);
            }
        }
    }

    if let Some(host_font_path) =
        resolve_host_font(requested_name, host_font_roots, file_access_gate)
    {
        if let Some(font) =
            try_load_named_candidate(&host_font_path, requested_name, file_access_gate)
        {
            return Some(font);
        }
    }

    None
}

fn try_load_named_candidate(
    candidate: &Path,
    requested_name: &str,
    file_access_gate: &dyn FileAccessGate,
) -> Option<ResolvedFont> {
    if file_access_gate.check_read(candidate) == PathAccessDecision::Denied {
        return None;
    }

    let bytes = file_access_gate.read_file(candidate).ok()?;
    if !matches_requested_font_name(candidate, requested_name, &bytes) {
        return None;
    }
    let font = OpenTypeFont::parse(&bytes).ok()?;
    let stem = candidate
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("FerritexOpenType");

    Some(ResolvedFont {
        path: candidate.to_path_buf(),
        font,
        base_font_name: stem.to_string(),
    })
}

fn collect_flat_opentype_candidates(dir: &Path) -> Vec<PathBuf> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut entries = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_opentype_path(path))
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

fn collect_opentype_candidates_in_dir(
    path: &Path,
    visited: &mut BTreeSet<PathBuf>,
    candidates: &mut Vec<PathBuf>,
) {
    if !visited.insert(normalize_existing_path(path)) {
        return;
    }

    if path.is_file() {
        if is_opentype_path(path) {
            candidates.push(path.to_path_buf());
        }
        return;
    }

    if !path.is_dir() {
        return;
    }

    let Ok(read_dir) = std::fs::read_dir(path) else {
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

fn matches_requested_font_name(path: &Path, requested_name: &str, bytes: &[u8]) -> bool {
    if path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| normalize_font_name(stem) == normalize_font_name(requested_name))
        .unwrap_or(false)
    {
        return true;
    }

    extract_font_names(bytes)
        .ok()
        .into_iter()
        .flatten()
        .any(|name| normalize_font_name(&name) == normalize_font_name(requested_name))
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

    use super::resolve_named_font;
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
                    "ferritex-font-resolver-{}-{unique}",
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
    }

    impl Drop for FixtureDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn write_font(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create font parent directory");
        }
        fs::write(path, minimal_test_font_bytes()).expect("write font bytes");
    }

    #[test]
    fn resolve_named_font_finds_project_local_font() {
        let fixture = FixtureDir::new();
        let input_dir = fixture.path().join("input");
        let project_root = fixture.path().join("project");
        let font_path = input_dir.join("TestFont.ttf");
        write_font(&font_path);

        let resolved = resolve_named_font(
            "TestFont",
            &input_dir,
            &project_root,
            &[],
            None,
            &[],
            &FsTestFileAccessGate,
        )
        .expect("resolve project-local font");

        assert_eq!(resolved.path, font_path);
        assert_eq!(resolved.base_font_name, "TestFont".to_string());
        assert_eq!(resolved.font.glyph_id(65), Some(1));
    }

    #[test]
    fn resolve_named_font_finds_asset_bundle_font() {
        let fixture = FixtureDir::new();
        let input_dir = fixture.path().join("input");
        let project_root = fixture.path().join("project");
        let bundle_path = fixture.path().join("bundle");
        let font_path = bundle_path.join("texmf/fonts/truetype/TestFont.ttf");
        write_font(&font_path);

        let resolved = resolve_named_font(
            "TestFont",
            &input_dir,
            &project_root,
            &[],
            Some(&bundle_path),
            &[],
            &FsTestFileAccessGate,
        )
        .expect("resolve asset bundle font");

        assert_eq!(resolved.path, font_path);
    }

    #[test]
    fn resolve_named_font_project_local_takes_priority() {
        let fixture = FixtureDir::new();
        let input_dir = fixture.path().join("input");
        let project_root = fixture.path().join("project");
        let bundle_path = fixture.path().join("bundle");
        let local_font_path = input_dir.join("TestFont.ttf");
        let bundle_font_path = bundle_path.join("texmf/fonts/truetype/TestFont.ttf");
        write_font(&local_font_path);
        write_font(&bundle_font_path);

        let resolved = resolve_named_font(
            "TestFont",
            &input_dir,
            &project_root,
            &[],
            Some(&bundle_path),
            &[],
            &FsTestFileAccessGate,
        )
        .expect("resolve preferred project-local font");

        assert_eq!(resolved.path, local_font_path);
    }

    #[test]
    fn resolve_named_font_case_insensitive() {
        let fixture = FixtureDir::new();
        let input_dir = fixture.path().join("input");
        let project_root = fixture.path().join("project");
        let font_path = input_dir.join("TestFont.ttf");
        write_font(&font_path);

        let resolved = resolve_named_font(
            "testfont",
            &input_dir,
            &project_root,
            &[],
            None,
            &[],
            &FsTestFileAccessGate,
        )
        .expect("resolve case-insensitive font name");

        assert_eq!(resolved.path, font_path);
    }

    #[test]
    fn resolve_named_font_returns_none_when_not_found() {
        let fixture = FixtureDir::new();
        let input_dir = fixture.path().join("input");
        let project_root = fixture.path().join("project");
        let bundle_path = fixture.path().join("bundle");

        assert!(resolve_named_font(
            "Nonexistent",
            &input_dir,
            &project_root,
            &[],
            Some(&bundle_path),
            &[],
            &FsTestFileAccessGate,
        )
        .is_none());
    }

    #[test]
    fn resolve_named_font_finds_overlay_root_font() {
        let fixture = FixtureDir::new();
        let input_dir = fixture.path().join("input");
        let project_root = fixture.path().join("project");
        let overlay_root = fixture.path().join("overlay");
        let font_path = overlay_root.join("fonts/OverlayFace.ttf");
        write_font(&font_path);

        let resolved = resolve_named_font(
            "OverlayFace",
            &input_dir,
            &project_root,
            &[overlay_root],
            None,
            &[],
            &FsTestFileAccessGate,
        )
        .expect("resolve overlay-root font");

        assert_eq!(resolved.path, font_path);
        assert_eq!(resolved.base_font_name, "OverlayFace".to_string());
    }

    #[test]
    fn resolve_named_font_uses_host_catalog_after_bundle_miss() {
        let fixture = FixtureDir::new();
        let input_dir = fixture.path().join("input");
        let project_root = fixture.path().join("project");
        let host_root = fixture.path().join("host");
        let font_path = host_root.join("opaque-file-name.ttf");
        if let Some(parent) = font_path.parent() {
            fs::create_dir_all(parent).expect("create host font parent");
        }
        fs::write(
            &font_path,
            named_test_font_bytes("Noto Serif", "Noto Serif Regular"),
        )
        .expect("write host font");

        let resolved = resolve_named_font(
            "Noto Serif",
            &input_dir,
            &project_root,
            &[],
            None,
            &[host_root],
            &FsTestFileAccessGate,
        )
        .expect("resolve host catalog font");

        assert_eq!(
            resolved.path,
            font_path.canonicalize().expect("canonical host font path")
        );
    }
}
