use std::path::{Component, Path, PathBuf};

use ferritex_core::policy::{ExecutionPolicy, PreviewPublicationPolicy};

use crate::runtime_options::{RuntimeOptions, ShellEscapeMode};

pub struct ExecutionPolicyFactory;

impl ExecutionPolicyFactory {
    pub fn create(options: &RuntimeOptions) -> ExecutionPolicy {
        let project_root = project_root_for_input(&options.input_file);
        let mut allowed_read_paths = vec![project_root];
        for overlay_root in &options.overlay_roots {
            push_read_path_if_needed(&mut allowed_read_paths, overlay_root.clone());
        }
        if let Some(bundle_path) = &options.asset_bundle {
            push_read_path_if_needed(&mut allowed_read_paths, bundle_path.clone());
        }
        if options.host_font_fallback {
            for host_root in &options.host_font_roots {
                push_read_path_if_needed(&mut allowed_read_paths, host_root.clone());
            }
        }
        push_read_path_if_needed(&mut allowed_read_paths, options.output_dir.clone());

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

fn push_read_path_if_needed(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    let normalized_candidate = absolute_normalized_path(&candidate);
    if paths
        .iter()
        .map(|path| absolute_normalized_path(path))
        .any(|existing| normalized_candidate.starts_with(&existing))
    {
        return;
    }
    paths.push(candidate);
}

fn project_root_for_input(input_file: &Path) -> PathBuf {
    // The project root defines the base of the file-access boundary. It is
    // intentionally restricted to the directory that contains the input file:
    // widening it via ancestor scans (e.g. searching for `.git`) or current
    // working directory would let absolute-path `\input{...}` references escape
    // the advertised workspace/overlay/bundle boundary (GH-37). Callers that
    // need additional read roots must declare them explicitly via
    // `--overlay-roots` / `--asset-bundle`.
    let absolute_input = absolute_normalized_path(input_file);
    absolute_input
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn absolute_normalized_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_path(path)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        normalize_path(&cwd.join(path))
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
            overlay_roots: Vec::new(),
            no_cache: false,
            asset_bundle: None,
            host_font_fallback: false,
            host_font_roots: Vec::new(),
            interaction_mode: InteractionMode::Nonstopmode,
            synctex: false,
            trace_font_tasks: false,
            shell_escape,
        }
    }

    #[test]
    fn disables_shell_escape_by_default() {
        let policy = ExecutionPolicyFactory::create(&runtime_options(ShellEscapeMode::Disabled));
        let expected_read_root = super::absolute_normalized_path(std::path::Path::new("src"));

        assert!(!policy.shell_escape_allowed);
        assert_eq!(
            policy.allowed_read_paths,
            vec![expected_read_root, PathBuf::from("build")]
        );
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
    fn ignores_ancestor_git_marker_when_deriving_project_root() {
        // GH-37 regression: a `.git` marker in an ancestor directory must not
        // be treated as the project root, since that would widen the policy
        // boundary past the input's own directory.
        let dir = tempdir().expect("create tempdir");
        let project_root = dir.path().join("project");
        let chapters_dir = project_root.join("src/chapters");
        fs::create_dir_all(project_root.join(".git")).expect("create git marker");
        fs::create_dir_all(&chapters_dir).expect("create source tree");

        let options = RuntimeOptions {
            input_file: project_root.join("src/chapters/main.tex"),
            output_dir: project_root.join("build"),
            jobname: "main".to_string(),
            parallelism: 1,
            overlay_roots: Vec::new(),
            no_cache: false,
            asset_bundle: None,
            host_font_fallback: false,
            host_font_roots: Vec::new(),
            interaction_mode: InteractionMode::Nonstopmode,
            synctex: false,
            trace_font_tasks: false,
            shell_escape: ShellEscapeMode::Disabled,
        };

        let policy = ExecutionPolicyFactory::create(&options);

        assert_eq!(
            policy.allowed_read_paths,
            vec![chapters_dir, project_root.join("build")]
        );
    }

    #[test]
    fn falls_back_to_input_parent_when_input_is_outside_known_project_root() {
        let dir = tempdir().expect("create tempdir");
        let input_dir = dir.path().join("src");
        let output_dir = dir.path().join("build");
        let options = RuntimeOptions {
            input_file: input_dir.join("main.tex"),
            output_dir: output_dir.clone(),
            jobname: "main".to_string(),
            parallelism: 1,
            overlay_roots: Vec::new(),
            no_cache: false,
            asset_bundle: None,
            host_font_fallback: false,
            host_font_roots: Vec::new(),
            interaction_mode: InteractionMode::Nonstopmode,
            synctex: false,
            trace_font_tasks: false,
            shell_escape: ShellEscapeMode::Disabled,
        };

        let policy = ExecutionPolicyFactory::create(&options);

        assert_eq!(policy.allowed_read_paths, vec![input_dir, output_dir]);
    }

    #[test]
    fn includes_explicit_asset_bundle_in_allowed_read_paths() {
        let dir = tempdir().expect("create tempdir");
        let project_root = dir.path().join("project");
        let src_dir = project_root.join("src");
        let bundle_root = dir.path().join("bundle");
        fs::create_dir_all(&src_dir).expect("create source tree");

        let options = RuntimeOptions {
            input_file: src_dir.join("main.tex"),
            output_dir: project_root.join("build"),
            jobname: "main".to_string(),
            parallelism: 1,
            overlay_roots: Vec::new(),
            no_cache: false,
            asset_bundle: Some(bundle_root.clone()),
            host_font_fallback: false,
            host_font_roots: Vec::new(),
            interaction_mode: InteractionMode::Nonstopmode,
            synctex: false,
            trace_font_tasks: false,
            shell_escape: ShellEscapeMode::Disabled,
        };

        let policy = ExecutionPolicyFactory::create(&options);

        assert_eq!(
            policy.allowed_read_paths,
            vec![src_dir, bundle_root, project_root.join("build")]
        );
    }

    #[test]
    fn includes_overlay_roots_between_project_and_bundle() {
        let dir = tempdir().expect("create tempdir");
        let project_root = dir.path().join("project");
        let src_dir = project_root.join("src");
        let overlay_root = dir.path().join("overlay");
        let bundle_root = dir.path().join("bundle");
        fs::create_dir_all(&src_dir).expect("create source tree");

        let options = RuntimeOptions {
            input_file: src_dir.join("main.tex"),
            output_dir: project_root.join("build"),
            jobname: "main".to_string(),
            parallelism: 1,
            overlay_roots: vec![overlay_root.clone()],
            no_cache: false,
            asset_bundle: Some(bundle_root.clone()),
            host_font_fallback: false,
            host_font_roots: Vec::new(),
            interaction_mode: InteractionMode::Nonstopmode,
            synctex: false,
            trace_font_tasks: false,
            shell_escape: ShellEscapeMode::Disabled,
        };

        let policy = ExecutionPolicyFactory::create(&options);

        assert_eq!(
            policy.allowed_read_paths,
            vec![
                src_dir,
                overlay_root,
                bundle_root,
                project_root.join("build")
            ]
        );
    }

    #[test]
    fn includes_host_font_roots_after_bundle_when_enabled() {
        let dir = tempdir().expect("create tempdir");
        let project_root = dir.path().join("project");
        let src_dir = project_root.join("src");
        let bundle_root = dir.path().join("bundle");
        let host_root = dir.path().join("host-fonts");
        fs::create_dir_all(&src_dir).expect("create source tree");

        let options = RuntimeOptions {
            input_file: src_dir.join("main.tex"),
            output_dir: project_root.join("build"),
            jobname: "main".to_string(),
            parallelism: 1,
            overlay_roots: Vec::new(),
            no_cache: false,
            asset_bundle: Some(bundle_root.clone()),
            host_font_fallback: true,
            host_font_roots: vec![host_root.clone()],
            interaction_mode: InteractionMode::Nonstopmode,
            synctex: false,
            trace_font_tasks: false,
            shell_escape: ShellEscapeMode::Disabled,
        };

        let policy = ExecutionPolicyFactory::create(&options);

        assert_eq!(
            policy.allowed_read_paths,
            vec![src_dir, bundle_root, host_root, project_root.join("build")]
        );
    }
}
