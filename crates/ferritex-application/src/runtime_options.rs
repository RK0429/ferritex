use std::path::{Path, PathBuf};

/// CLI から受け取る compile サブコマンド引数
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileArgs {
    pub input_file: PathBuf,
    pub output_dir: Option<PathBuf>,
    pub jobname: Option<String>,
    pub jobs: Option<usize>,
    pub no_cache: bool,
    pub asset_bundle: Option<PathBuf>,
    pub interaction: Option<CompileInteraction>,
    pub synctex: bool,
    pub trace_font_tasks: bool,
    pub shell_escape: bool,
    pub no_shell_escape: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileInteraction {
    Nonstopmode,
    Batchmode,
    Scrollmode,
    Errorstopmode,
}

/// CLI フラグから正規化されたランタイムオプション
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOptions {
    pub input_file: PathBuf,
    pub output_dir: PathBuf,
    pub jobname: String,
    pub parallelism: usize,
    pub no_cache: bool,
    pub asset_bundle: Option<PathBuf>,
    pub interaction_mode: InteractionMode,
    pub synctex: bool,
    pub trace_font_tasks: bool,
    pub shell_escape: ShellEscapeMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionMode {
    Nonstopmode,
    Batchmode,
    Scrollmode,
    Errorstopmode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellEscapeMode {
    Disabled,
    Restricted,
    Enabled,
}

impl RuntimeOptions {
    pub fn from_compile_args(args: &CompileArgs) -> Self {
        Self {
            input_file: args.input_file.clone(),
            output_dir: args
                .output_dir
                .clone()
                .unwrap_or_else(|| default_output_dir(&args.input_file)),
            jobname: args
                .jobname
                .clone()
                .unwrap_or_else(|| derive_jobname(&args.input_file)),
            parallelism: args.jobs.unwrap_or_else(default_parallelism).max(1),
            no_cache: args.no_cache,
            asset_bundle: args.asset_bundle.clone(),
            interaction_mode: match args.interaction.unwrap_or(CompileInteraction::Nonstopmode) {
                CompileInteraction::Nonstopmode => InteractionMode::Nonstopmode,
                CompileInteraction::Batchmode => InteractionMode::Batchmode,
                CompileInteraction::Scrollmode => InteractionMode::Scrollmode,
                CompileInteraction::Errorstopmode => InteractionMode::Errorstopmode,
            },
            synctex: args.synctex,
            trace_font_tasks: args.trace_font_tasks,
            shell_escape: normalize_shell_escape(args.shell_escape, args.no_shell_escape),
        }
    }

    pub fn for_lsp(
        input_file: PathBuf,
        output_dir: Option<PathBuf>,
        jobname: Option<String>,
    ) -> Self {
        Self {
            output_dir: output_dir.unwrap_or_else(|| default_output_dir(&input_file)),
            jobname: jobname.unwrap_or_else(|| derive_jobname(&input_file)),
            input_file,
            parallelism: default_parallelism(),
            no_cache: false,
            asset_bundle: None,
            interaction_mode: InteractionMode::Nonstopmode,
            synctex: false,
            trace_font_tasks: false,
            shell_escape: ShellEscapeMode::Disabled,
        }
    }
}

fn default_output_dir(input_file: &Path) -> PathBuf {
    input_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn derive_jobname(input_file: &Path) -> String {
    input_file
        .file_stem()
        .or_else(|| input_file.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "texput".to_string())
}

fn normalize_shell_escape(shell_escape: bool, no_shell_escape: bool) -> ShellEscapeMode {
    if no_shell_escape {
        ShellEscapeMode::Disabled
    } else if shell_escape {
        ShellEscapeMode::Enabled
    } else {
        ShellEscapeMode::Restricted
    }
}

fn default_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .max(1)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        CompileArgs, CompileInteraction, InteractionMode, RuntimeOptions, ShellEscapeMode,
    };

    fn compile_args(input_file: impl Into<PathBuf>) -> CompileArgs {
        CompileArgs {
            input_file: input_file.into(),
            output_dir: None,
            jobname: None,
            jobs: None,
            no_cache: false,
            asset_bundle: None,
            interaction: None,
            synctex: false,
            trace_font_tasks: false,
            shell_escape: false,
            no_shell_escape: false,
        }
    }

    #[test]
    fn derives_jobname_and_output_dir_defaults_from_input_file() {
        let args = compile_args(PathBuf::from("chapters/main.tex"));

        let options = RuntimeOptions::from_compile_args(&args);

        assert_eq!(options.jobname, "main");
        assert_eq!(options.output_dir, PathBuf::from("chapters"));
        assert_eq!(options.parallelism, super::default_parallelism());
        assert_eq!(options.interaction_mode, InteractionMode::Nonstopmode);
        assert_eq!(options.shell_escape, ShellEscapeMode::Restricted);
    }

    #[test]
    fn preserves_explicit_flags_when_normalizing() {
        let mut args = compile_args(PathBuf::from("input.tex"));
        args.output_dir = Some(PathBuf::from("build"));
        args.jobname = Some("custom-job".to_string());
        args.jobs = Some(4);
        args.no_cache = true;
        args.asset_bundle = Some(PathBuf::from("bundle"));
        args.interaction = Some(CompileInteraction::Batchmode);
        args.synctex = true;
        args.trace_font_tasks = true;
        args.shell_escape = true;

        let options = RuntimeOptions::from_compile_args(&args);

        assert_eq!(options.output_dir, PathBuf::from("build"));
        assert_eq!(options.jobname, "custom-job");
        assert_eq!(options.parallelism, 4);
        assert!(options.no_cache);
        assert_eq!(options.asset_bundle, Some(PathBuf::from("bundle")));
        assert_eq!(options.interaction_mode, InteractionMode::Batchmode);
        assert!(options.synctex);
        assert!(options.trace_font_tasks);
        assert_eq!(options.shell_escape, ShellEscapeMode::Enabled);
    }

    #[test]
    fn maps_all_interaction_modes() {
        let expectations = [
            (
                CompileInteraction::Nonstopmode,
                InteractionMode::Nonstopmode,
            ),
            (CompileInteraction::Batchmode, InteractionMode::Batchmode),
            (CompileInteraction::Scrollmode, InteractionMode::Scrollmode),
            (
                CompileInteraction::Errorstopmode,
                InteractionMode::Errorstopmode,
            ),
        ];

        for (input_mode, expected_mode) in expectations {
            let mut args = compile_args(PathBuf::from("input.tex"));
            args.interaction = Some(input_mode);

            let options = RuntimeOptions::from_compile_args(&args);

            assert_eq!(options.interaction_mode, expected_mode);
        }
    }

    #[test]
    fn no_shell_escape_flag_overrides_shell_escape() {
        let mut args = compile_args(PathBuf::from("input.tex"));
        args.shell_escape = true;
        args.no_shell_escape = true;

        let options = RuntimeOptions::from_compile_args(&args);

        assert_eq!(options.shell_escape, ShellEscapeMode::Disabled);
    }

    #[test]
    fn lsp_defaults_follow_requirement_profile() {
        let options = RuntimeOptions::for_lsp(PathBuf::from("chapters/main.tex"), None, None);

        assert_eq!(options.input_file, PathBuf::from("chapters/main.tex"));
        assert_eq!(options.output_dir, PathBuf::from("chapters"));
        assert_eq!(options.jobname, "main");
        assert_eq!(options.parallelism, super::default_parallelism());
        assert!(!options.no_cache);
        assert_eq!(options.asset_bundle, None);
        assert_eq!(options.interaction_mode, InteractionMode::Nonstopmode);
        assert!(!options.synctex);
        assert!(!options.trace_font_tasks);
        assert_eq!(options.shell_escape, ShellEscapeMode::Disabled);
    }
}
