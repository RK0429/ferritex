use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::normalize_path;

/// パスアクセスの判定結果
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathAccessDecision {
    Allowed,
    Denied,
}

/// パスアクセスポリシー (Value Object)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathAccessPolicy {
    pub allowed_read_dirs: Vec<PathBuf>,
    pub allowed_write_dirs: Vec<PathBuf>,
}

impl PathAccessPolicy {
    pub fn check_read(&self, path: impl AsRef<Path>) -> PathAccessDecision {
        self.check(path.as_ref(), &self.allowed_read_dirs)
    }

    pub fn check_write(&self, path: impl AsRef<Path>) -> PathAccessDecision {
        self.check(path.as_ref(), &self.allowed_write_dirs)
    }

    fn check(&self, path: &Path, allowed_dirs: &[PathBuf]) -> PathAccessDecision {
        let normalized_path = normalize_path(path);

        if allowed_dirs
            .iter()
            .map(|dir| normalize_path(dir))
            .any(|dir| normalized_path.starts_with(&dir))
        {
            PathAccessDecision::Allowed
        } else {
            PathAccessDecision::Denied
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{PathAccessDecision, PathAccessPolicy};

    fn fixture_root() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before unix epoch")
            .as_nanos();

        std::env::temp_dir().join(format!("ferritex-path-policy-{unique}"))
    }

    fn sample_policy() -> PathAccessPolicy {
        let root = fixture_root();

        PathAccessPolicy {
            allowed_read_dirs: vec![root.join("project"), root.join("cache")],
            allowed_write_dirs: vec![root.join("project/out")],
        }
    }

    #[test]
    fn allows_paths_inside_authorized_directories() {
        let policy = sample_policy();
        let allowed_read = policy.allowed_read_dirs[0].join("chapters/main.tex");
        let allowed_write = policy.allowed_write_dirs[0].join("main.aux");

        assert_eq!(policy.check_read(allowed_read), PathAccessDecision::Allowed);
        assert_eq!(
            policy.check_write(allowed_write),
            PathAccessDecision::Allowed
        );
    }

    #[test]
    fn denies_relative_directory_escape_on_read() {
        let policy = sample_policy();

        assert_eq!(
            policy.check_read(Path::new("../../outside.txt")),
            PathAccessDecision::Denied
        );
    }

    #[test]
    fn denies_absolute_path_outside_read_roots() {
        let policy = sample_policy();
        let outside = fixture_root().join("elsewhere/file.tex");

        assert_eq!(policy.check_read(outside), PathAccessDecision::Denied);
    }

    #[test]
    fn denies_normalized_escape_from_allowed_read_root() {
        let policy = sample_policy();
        let escaped = policy.allowed_read_dirs[0].join("../secret.tex");

        assert_eq!(policy.check_read(escaped), PathAccessDecision::Denied);
    }

    #[test]
    fn denies_writes_to_read_only_area() {
        let policy = sample_policy();
        let read_only_target = policy.allowed_read_dirs[0].join("notes.log");

        assert_eq!(
            policy.check_write(read_only_target),
            PathAccessDecision::Denied
        );
    }

    #[test]
    fn denies_writes_outside_write_root() {
        let policy = sample_policy();
        let outside = policy.allowed_read_dirs[1].join("cache.idx");

        assert_eq!(policy.check_write(outside), PathAccessDecision::Denied);
    }

    #[test]
    fn denies_normalized_escape_from_write_root() {
        let policy = sample_policy();
        let escaped = policy.allowed_write_dirs[0].join("../../outside.aux");

        assert_eq!(policy.check_write(escaped), PathAccessDecision::Denied);
    }
}
