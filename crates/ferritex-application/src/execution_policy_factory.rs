use std::path::{Component, Path, PathBuf};

use ferritex_core::policy::{ExecutionPolicy, PreviewPublicationPolicy};

use crate::runtime_options::{RuntimeOptions, ShellEscapeMode};

pub struct ExecutionPolicyFactory;

impl ExecutionPolicyFactory {
    pub fn create(options: &RuntimeOptions) -> ExecutionPolicy {
        let project_root = project_root_for_input(&options.input_file);
        let mut allowed_read_paths = vec![project_root];
        if let Some(bundle_path) = &options.asset_bundle {
            allowed_read_paths.push(bundle_path.clone());
        }

        ExecutionPolicy {
            shell_escape_allowed: matches!(options.shell_escape, ShellEscapeMode::Enabled),
            allowed_read_paths,
            allowed_write_paths: vec![options.output_dir.clone()],
            output_dir: options.output_dir.clone(),
            jobname: options.jobname.clone(),
            preview_publication: Some(PreviewPublicationPolicy {
                loopback_only: true,
                active_job_only: true,
            }),
        }
    }
}

fn project_root_for_input(input_file: &Path) -> PathBuf {
    let absolute_input = absolute_input_path(input_file);
    let fallback = absolute_input
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    if let Some(project_root) = fallback
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
    {
        return project_root.to_path_buf();
    }

    if let Ok(cwd) = std::env::current_dir() {
        let cwd = normalize_path(&cwd);
        if fallback.starts_with(&cwd) {
            return cwd;
        }
    }

    fallback
}

fn absolute_input_path(input_file: &Path) -> PathBuf {
    if input_file.is_absolute() {
        normalize_path(input_file)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        normalize_path(&cwd.join(input_file))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
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
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::ExecutionPolicyFactory;
    use crate::runtime_options::{InteractionMode, RuntimeOptions, ShellEscapeMode};

    fn runtime_options(shell_escape: ShellEscapeMode) -> RuntimeOptions {
        RuntimeOptions {
            input_file: PathBuf::from("src/main.tex"),
            output_dir: PathBuf::from("build"),
            jobname: "main".to_string(),
            parallelism: 1,
            no_cache: false,
            asset_bundle: None,
            interaction_mode: InteractionMode::Nonstopmode,
            synctex: false,
            trace_font_tasks: false,
            shell_escape,
        }
    }

    #[test]
    fn disables_shell_escape_by_default() {
        let policy = ExecutionPolicyFactory::create(&runtime_options(ShellEscapeMode::Disabled));
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let expected_read_root = manifest_dir
            .ancestors()
            .find(|ancestor| ancestor.join(".git").exists())
            .map(PathBuf::from)
            .expect("workspace root with git marker");

        assert!(!policy.shell_escape_allowed);
        assert_eq!(policy.allowed_read_paths, vec![expected_read_root]);
        assert_eq!(policy.allowed_write_paths, vec![PathBuf::from("build")]);
        assert_eq!(policy.output_dir, PathBuf::from("build"));
        assert_eq!(policy.jobname, "main");
    }

    #[test]
    fn enables_shell_escape_only_for_enabled_mode() {
        let policy = ExecutionPolicyFactory::create(&runtime_options(ShellEscapeMode::Enabled));

        assert!(policy.shell_escape_allowed);
    }

    #[test]
    fn restricted_mode_does_not_allow_shell_escape() {
        let policy = ExecutionPolicyFactory::create(&runtime_options(ShellEscapeMode::Restricted));

        assert!(!policy.shell_escape_allowed);
    }

    #[test]
    fn resolves_project_root_from_git_marker_for_nested_input() {
        let dir = tempdir().expect("create tempdir");
        let project_root = dir.path().join("project");
        fs::create_dir_all(project_root.join(".git")).expect("create git marker");

        let options = RuntimeOptions {
            input_file: project_root.join("src/chapters/main.tex"),
            output_dir: project_root.join("build"),
            jobname: "main".to_string(),
            parallelism: 1,
            no_cache: false,
            asset_bundle: None,
            interaction_mode: InteractionMode::Nonstopmode,
            synctex: false,
            trace_font_tasks: false,
            shell_escape: ShellEscapeMode::Disabled,
        };

        let policy = ExecutionPolicyFactory::create(&options);

        assert_eq!(policy.allowed_read_paths, vec![project_root]);
    }

    #[test]
    fn falls_back_to_input_parent_when_input_is_outside_known_project_root() {
        let dir = tempdir().expect("create tempdir");
        let input_dir = dir.path().join("src");
        let options = RuntimeOptions {
            input_file: input_dir.join("main.tex"),
            output_dir: dir.path().join("build"),
            jobname: "main".to_string(),
            parallelism: 1,
            no_cache: false,
            asset_bundle: None,
            interaction_mode: InteractionMode::Nonstopmode,
            synctex: false,
            trace_font_tasks: false,
            shell_escape: ShellEscapeMode::Disabled,
        };

        let policy = ExecutionPolicyFactory::create(&options);

        assert_eq!(policy.allowed_read_paths, vec![input_dir]);
    }

    #[test]
    fn includes_explicit_asset_bundle_in_allowed_read_paths() {
        let dir = tempdir().expect("create tempdir");
        let project_root = dir.path().join("project");
        let bundle_root = dir.path().join("bundle");
        fs::create_dir_all(project_root.join(".git")).expect("create git marker");

        let options = RuntimeOptions {
            input_file: project_root.join("src/main.tex"),
            output_dir: project_root.join("build"),
            jobname: "main".to_string(),
            parallelism: 1,
            no_cache: false,
            asset_bundle: Some(bundle_root.clone()),
            interaction_mode: InteractionMode::Nonstopmode,
            synctex: false,
            trace_font_tasks: false,
            shell_escape: ShellEscapeMode::Disabled,
        };

        let policy = ExecutionPolicyFactory::create(&options);

        assert_eq!(policy.allowed_read_paths, vec![project_root, bundle_root]);
    }
}
