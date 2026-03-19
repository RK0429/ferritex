use std::ffi::OsString;
use std::path::{Path, PathBuf};

use ferritex_core::policy::{
    ExecutionPolicy, FileAccessError, FileAccessGate, OutputArtifactRegistry, PathAccessDecision,
    PathAccessPolicy,
};

pub struct FsFileAccessGate {
    execution_policy: ExecutionPolicy,
    policy: PathAccessPolicy,
    artifact_registry: OutputArtifactRegistry,
}

impl FsFileAccessGate {
    pub fn from_policy(execution_policy: ExecutionPolicy) -> Self {
        let policy = execution_policy.to_path_access_policy();

        Self {
            execution_policy,
            policy: PathAccessPolicy {
                allowed_read_dirs: canonicalize_roots(policy.allowed_read_dirs),
                allowed_write_dirs: canonicalize_roots(policy.allowed_write_dirs),
            },
            artifact_registry: OutputArtifactRegistry::new(),
        }
    }

    pub fn with_artifact_registry(
        execution_policy: ExecutionPolicy,
        artifact_registry: OutputArtifactRegistry,
    ) -> Self {
        let policy = execution_policy.to_path_access_policy();

        Self {
            execution_policy,
            policy: PathAccessPolicy {
                allowed_read_dirs: canonicalize_roots(policy.allowed_read_dirs),
                allowed_write_dirs: canonicalize_roots(policy.allowed_write_dirs),
            },
            artifact_registry,
        }
    }

    fn evaluate_path_policy(&self, path: &Path, allowed_dirs: &[PathBuf]) -> PathAccessDecision {
        let Ok(resolved_path) = canonicalize_with_missing(path) else {
            return PathAccessDecision::Denied;
        };

        if allowed_dirs
            .iter()
            .any(|dir| resolved_path.starts_with(dir))
        {
            PathAccessDecision::Allowed
        } else {
            PathAccessDecision::Denied
        }
    }

    fn validate_readback_target(
        &self,
        path: &Path,
        primary_input: &Path,
        jobname: &str,
    ) -> Result<PathBuf, FileAccessError> {
        if !self.artifact_registry.allow_readback(
            path,
            primary_input,
            jobname,
            &self.execution_policy.output_dir,
        ) {
            return Err(FileAccessError::AccessDenied {
                path: path.to_path_buf(),
            });
        }

        let resolved_path = std::fs::canonicalize(path)?;
        let resolved_output_root = canonicalize_with_missing(&self.execution_policy.output_dir)?;
        if !resolved_path.starts_with(&resolved_output_root) {
            return Err(FileAccessError::AccessDenied {
                path: path.to_path_buf(),
            });
        }

        Ok(resolved_path)
    }
}

impl FileAccessGate for FsFileAccessGate {
    fn ensure_directory(&self, path: &Path) -> Result<(), FileAccessError> {
        if self.check_write(path) == PathAccessDecision::Denied {
            return Err(FileAccessError::AccessDenied {
                path: path.to_path_buf(),
            });
        }

        std::fs::create_dir_all(path)?;
        Ok(())
    }

    fn check_read(&self, path: &Path) -> PathAccessDecision {
        self.evaluate_path_policy(path, &self.policy.allowed_read_dirs)
    }

    fn check_write(&self, path: &Path) -> PathAccessDecision {
        self.evaluate_path_policy(path, &self.policy.allowed_write_dirs)
    }

    fn check_readback(
        &self,
        path: &Path,
        primary_input: &Path,
        jobname: &str,
    ) -> PathAccessDecision {
        if self
            .validate_readback_target(path, primary_input, jobname)
            .is_ok()
        {
            PathAccessDecision::Allowed
        } else {
            PathAccessDecision::Denied
        }
    }

    fn read_file(&self, path: &Path) -> Result<Vec<u8>, FileAccessError> {
        if self.check_read(path) == PathAccessDecision::Denied {
            return Err(FileAccessError::AccessDenied {
                path: path.to_path_buf(),
            });
        }

        Ok(std::fs::read(path)?)
    }

    fn write_file(&self, path: &Path, content: &[u8]) -> Result<(), FileAccessError> {
        if self.check_write(path) == PathAccessDecision::Denied {
            return Err(FileAccessError::AccessDenied {
                path: path.to_path_buf(),
            });
        }

        std::fs::write(path, content)?;
        Ok(())
    }

    fn read_readback(
        &self,
        path: &Path,
        primary_input: &Path,
        jobname: &str,
    ) -> Result<Vec<u8>, FileAccessError> {
        let resolved_path = self.validate_readback_target(path, primary_input, jobname)?;

        Ok(std::fs::read(resolved_path)?)
    }
}

fn canonicalize_roots(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .map(|path| canonicalize_with_missing(&path).unwrap_or(path))
        .collect()
}

fn canonicalize_with_missing(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    let mut missing_suffix = Vec::<OsString>::new();
    let mut cursor = absolute.as_path();

    loop {
        if cursor.exists() {
            let mut resolved = std::fs::canonicalize(cursor)?;
            for segment in missing_suffix.iter().rev() {
                resolved.push(segment);
            }
            return Ok(resolved);
        }

        if let Some(name) = cursor.file_name() {
            missing_suffix.push(name.to_os_string());
        }

        cursor = cursor.parent().unwrap_or_else(|| Path::new("."));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ferritex_core::policy::{
        ExecutionPolicy, FileAccessError, FileAccessGate, OutputArtifactRecord,
        OutputArtifactRegistry, PathAccessDecision, PreviewPublicationPolicy,
    };
    use tempfile::tempdir;

    use super::FsFileAccessGate;

    fn execution_policy(read_root: PathBuf, write_root: PathBuf) -> ExecutionPolicy {
        ExecutionPolicy {
            shell_escape_allowed: false,
            allowed_read_paths: vec![read_root],
            allowed_write_paths: vec![write_root.clone()],
            output_dir: write_root,
            jobname: "main".to_string(),
            preview_publication: Some(PreviewPublicationPolicy {
                loopback_only: true,
                active_job_only: true,
            }),
        }
    }

    fn sample_gate() -> (FsFileAccessGate, PathBuf, PathBuf, PathBuf) {
        let root = tempdir().expect("create tempdir").keep();
        let read_root = root.join("project/src");
        let write_root = root.join("project/out");
        let outside_root = root.join("outside");

        std::fs::create_dir_all(read_root.join("nested")).expect("create read root");
        std::fs::create_dir_all(&write_root).expect("create write root");
        std::fs::create_dir_all(&outside_root).expect("create outside root");
        std::fs::write(read_root.join("main.tex"), "\\input{chapter}").expect("write input");
        std::fs::write(outside_root.join("outside.txt"), "forbidden").expect("write outside file");

        let gate =
            FsFileAccessGate::from_policy(execution_policy(read_root.clone(), write_root.clone()));

        (gate, read_root, write_root, outside_root)
    }

    #[test]
    fn denies_relative_escape_on_read() {
        let (gate, read_root, _, _) = sample_gate();
        let escaped = read_root.join("../../outside/outside.txt");

        assert_eq!(gate.check_read(&escaped), PathAccessDecision::Denied);
    }

    #[test]
    fn denies_absolute_reads_outside_allowed_roots() {
        let (gate, _, _, outside_root) = sample_gate();
        let outside_file = outside_root.join("outside.txt");

        assert_eq!(gate.check_read(&outside_file), PathAccessDecision::Denied);
        assert!(matches!(
            gate.read_file(&outside_file),
            Err(FileAccessError::AccessDenied { .. })
        ));
    }

    #[test]
    fn denies_writes_to_read_only_area() {
        let (gate, read_root, _, _) = sample_gate();
        let read_only_target = read_root.join("notes.log");

        assert_eq!(
            gate.check_write(&read_only_target),
            PathAccessDecision::Denied
        );
        assert!(matches!(
            gate.write_file(&read_only_target, b"denied"),
            Err(FileAccessError::AccessDenied { .. })
        ));
    }

    #[test]
    fn denies_nonexistent_file_outside_allowed_roots() {
        let (gate, _, _, outside_root) = sample_gate();
        let missing = outside_root.join("missing.tex");

        assert_eq!(gate.check_read(&missing), PathAccessDecision::Denied);
    }

    #[test]
    fn allows_writes_inside_allowed_root_even_before_file_exists() {
        let (gate, _, write_root, _) = sample_gate();
        let target = write_root.join("build/main.aux");
        std::fs::create_dir_all(target.parent().expect("parent directory"))
            .expect("create output parent");

        assert_eq!(gate.check_write(&target), PathAccessDecision::Allowed);
        gate.write_file(&target, b"ok").expect("write allowed file");
        assert_eq!(std::fs::read(&target).expect("read written file"), b"ok");
    }

    #[test]
    fn allows_readback_for_recorded_same_job_artifacts() {
        let root = tempdir().expect("create tempdir").keep();
        let read_root = root.join("project/src");
        let write_root = root.join("project/out");
        std::fs::create_dir_all(&read_root).expect("create read root");
        std::fs::create_dir_all(&write_root).expect("create write root");

        let primary_input = read_root.join("main.tex");
        let readback = write_root.join("main.aux");
        std::fs::write(&readback, "trusted").expect("write readback file");

        let mut registry = OutputArtifactRegistry::new();
        registry.record(OutputArtifactRecord::new(
            &readback,
            &primary_input,
            "main",
            ferritex_core::policy::ArtifactKind::Auxiliary,
            1,
        ));

        let gate = FsFileAccessGate::with_artifact_registry(
            execution_policy(read_root, write_root),
            registry,
        );

        assert_eq!(
            gate.check_readback(&readback, &primary_input, "main"),
            PathAccessDecision::Allowed
        );
        assert_eq!(
            gate.read_readback(&readback, &primary_input, "main")
                .expect("read trusted artifact"),
            b"trusted"
        );
    }

    #[cfg(unix)]
    #[test]
    fn denies_symlink_readback_that_escapes_output_root() {
        use std::os::unix::fs::symlink;

        let root = tempdir().expect("create tempdir").keep();
        let read_root = root.join("project/src");
        let write_root = root.join("project/out");
        let outside_root = root.join("outside");
        let primary_input = read_root.join("main.tex");
        let outside_file = outside_root.join("secret.txt");
        let symlink_path = write_root.join("main.bbl");

        std::fs::create_dir_all(&read_root).expect("create read root");
        std::fs::create_dir_all(&write_root).expect("create write root");
        std::fs::create_dir_all(&outside_root).expect("create outside root");
        std::fs::write(&primary_input, "\\begin{document}").expect("write input");
        std::fs::write(&outside_file, "secret").expect("write outside file");
        symlink(&outside_file, &symlink_path).expect("create symlink");

        let gate =
            FsFileAccessGate::from_policy(execution_policy(read_root.clone(), write_root.clone()));

        assert_eq!(
            gate.check_readback(&symlink_path, &primary_input, "main"),
            PathAccessDecision::Denied
        );
        assert!(matches!(
            gate.read_readback(&symlink_path, &primary_input, "main"),
            Err(FileAccessError::AccessDenied { .. })
        ));
    }
}
