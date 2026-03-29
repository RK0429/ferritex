mod artifact_registry;
mod execution_policy;
mod file_access_gate;
mod operation_handlers;
mod path_access;

use std::path::{Component, Path, PathBuf};

pub use artifact_registry::{ArtifactKind, OutputArtifactRecord, OutputArtifactRegistry};
pub use execution_policy::{ExecutionPolicy, PreviewPublicationPolicy};
pub use file_access_gate::{FileAccessError, FileAccessGate};
pub use operation_handlers::{
    FileOperationHandler, FileOperationResult, ShellEscapeHandler, ShellEscapeResult,
};
pub use path_access::{PathAccessDecision, PathAccessPolicy};

pub(crate) fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let can_pop = normalized
                    .components()
                    .next_back()
                    .is_some_and(|entry| matches!(entry, Component::Normal(_)));

                if can_pop {
                    normalized.pop();
                } else if !path.is_absolute() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    if normalized.as_os_str().is_empty() {
        if path.is_absolute() {
            PathBuf::from(std::path::MAIN_SEPARATOR.to_string())
        } else {
            PathBuf::from(".")
        }
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_path;
    use std::path::Path;

    #[test]
    fn current_dir_is_preserved() {
        assert_eq!(normalize_path(Path::new(".")), Path::new("."));
    }

    #[test]
    fn removes_dot_components() {
        assert_eq!(normalize_path(Path::new("a/./b")), Path::new("a/b"));
    }

    #[test]
    fn resolves_parent_components() {
        assert_eq!(normalize_path(Path::new("a/b/../c")), Path::new("a/c"));
    }

    #[test]
    fn absolute_double_parent_resolves_to_root() {
        assert_eq!(normalize_path(Path::new("/a/b/../../")), Path::new("/"));
    }

    #[test]
    fn relative_double_parent_beyond_base() {
        assert_eq!(normalize_path(Path::new("a/../..")), Path::new(".."));
    }

    #[test]
    fn empty_relative_path_becomes_dot() {
        assert_eq!(normalize_path(Path::new("")), Path::new("."));
    }
}
