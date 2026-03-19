use std::path::{Path, PathBuf};

use ferritex_core::policy::{ExecutionPolicy, PreviewPublicationPolicy};

use crate::runtime_options::{RuntimeOptions, ShellEscapeMode};

pub struct ExecutionPolicyFactory;

impl ExecutionPolicyFactory {
    pub fn create(options: &RuntimeOptions) -> ExecutionPolicy {
        let input_dir = options
            .input_file
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        ExecutionPolicy {
            shell_escape_allowed: matches!(options.shell_escape, ShellEscapeMode::Enabled),
            allowed_read_paths: vec![input_dir],
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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

        assert!(!policy.shell_escape_allowed);
        assert_eq!(policy.allowed_read_paths, vec![PathBuf::from("src")]);
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
}
