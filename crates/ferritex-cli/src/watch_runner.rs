use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use ferritex_application::compile_job_service::CompileJobService;
use ferritex_application::compile_job_service::CompileResult;
use ferritex_application::execution_policy_factory::ExecutionPolicyFactory;
use ferritex_application::recompile_scheduler::RecompileScheduler;
use ferritex_application::workspace_job_scheduler::WorkspaceJobScheduler;
use ferritex_core::diagnostics::{Diagnostic, Severity};
#[cfg(test)]
use ferritex_core::policy::{ExecutionPolicy, PreviewPublicationPolicy};
use ferritex_core::policy::{FileAccessGate, PathAccessDecision};
use ferritex_infra::asset_bundle::AssetBundleLoader;
use ferritex_infra::fs::FsFileAccessGate;
use ferritex_infra::shell::ShellCommandGateway;
use ferritex_infra::watcher::PollingFileWatcher;

use crate::{emit_diagnostic, emit_diagnostics, runtime_options_from_command, CompileCommand};

pub fn run_watch(command: &CompileCommand) -> i32 {
    let options = runtime_options_from_command(command);
    let policy = ExecutionPolicyFactory::create(&options);
    let shell_command_gateway = ShellCommandGateway::from_policy(&policy);
    let file_access_gate = FsFileAccessGate::from_policy(policy);
    let asset_bundle_loader = AssetBundleLoader;
    let service = CompileJobService::new(
        &file_access_gate,
        &asset_bundle_loader,
        &shell_command_gateway,
    );
    let scheduler = WorkspaceJobScheduler::default();
    let workspace_root = options
        .input_file
        .parent()
        .unwrap_or_else(|| Path::new("."));

    let initial_result = scheduler.run(workspace_root, || service.compile(&options));
    emit_diagnostics(&initial_result.diagnostics);

    let watched_paths =
        watched_paths_for_result(&initial_result, &options.input_file, &file_access_gate);
    let mut watcher = match PollingFileWatcher::new(watched_paths) {
        Ok(watcher) => watcher,
        Err(error) => {
            emit_diagnostic(&watcher_io_diagnostic(
                &error,
                "failed to start file watcher",
            ));
            service.flush_cache();
            return 2;
        }
    };
    let mut recompile_scheduler =
        RecompileScheduler::with_settle_window(Duration::from_millis(150));

    loop {
        thread::sleep(Duration::from_millis(100));
        let changes = match watcher.poll_changes() {
            Ok(changes) => changes,
            Err(error) => {
                emit_diagnostic(&Diagnostic::new(
                    Severity::Error,
                    format!("failed to poll watched files: {error}"),
                ));
                service.flush_cache();
                return 2;
            }
        };

        if !changes.is_empty() {
            recompile_scheduler.enqueue(changes);
        }

        while let Some(coalesced_changes) = recompile_scheduler.start_next() {
            let hint = coalesced_changes;
            let result = scheduler.run(workspace_root, || {
                service.compile_with_changed_paths(&options, &hint)
            });
            emit_diagnostics(&result.diagnostics);
            recompile_scheduler.finish_current();

            if let Err(error) = watcher.replace_paths(watched_paths_for_result(
                &result,
                &options.input_file,
                &file_access_gate,
            )) {
                emit_diagnostic(&watcher_io_diagnostic(
                    &error,
                    "failed to refresh watched files",
                ));
                service.flush_cache();
                return 2;
            }
        }
    }
}

fn watcher_io_diagnostic(error: &io::Error, fallback_context: &str) -> Diagnostic {
    if error.kind() == io::ErrorKind::NotFound {
        Diagnostic::new(
            Severity::Error,
            "stopped watching: a watched file or its parent directory no longer exists",
        )
        .with_suggestion(
            "restore the missing path (or revert the deletion), then rerun `ferritex watch`",
        )
    } else {
        Diagnostic::new(Severity::Error, format!("{fallback_context}: {error}"))
    }
}

fn watched_paths_for_result(
    result: &CompileResult,
    root_input: &Path,
    file_access_gate: &dyn FileAccessGate,
) -> Vec<PathBuf> {
    result
        .stable_compile_state
        .as_ref()
        .map(|state| {
            state
                .document_state
                .source_files
                .iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        })
        .filter(|paths| !paths.is_empty())
        .unwrap_or_else(|| discover_watched_paths(file_access_gate, root_input))
}

fn discover_watched_paths(
    file_access_gate: &dyn FileAccessGate,
    root_input: &Path,
) -> Vec<PathBuf> {
    let mut discovered = BTreeSet::new();
    let mut pending = vec![normalize_candidate(root_input)];

    while let Some(path) = pending.pop() {
        if !discovered.insert(path.clone()) {
            continue;
        }

        let source = if file_access_gate.check_read(&path) == PathAccessDecision::Denied {
            continue;
        } else {
            match file_access_gate.read_file(&path) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(source) => source,
                    Err(_) => continue,
                },
                Err(_) => continue,
            }
        };

        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        for dependency in find_input_dependencies(&source) {
            let candidate = normalize_candidate(&resolve_tex_path(base_dir, &dependency));
            if !discovered.contains(&candidate) {
                pending.push(candidate);
            }
        }
    }

    discovered.into_iter().collect()
}

#[cfg(test)]
fn allow_all_file_access_gate() -> FsFileAccessGate {
    let cwd = std::env::current_dir().expect("current dir");
    FsFileAccessGate::from_policy(ExecutionPolicy {
        shell_escape_allowed: false,
        allowed_read_paths: vec![cwd.clone(), std::env::temp_dir()],
        allowed_write_paths: vec![cwd.clone(), std::env::temp_dir()],
        output_dir: cwd,
        jobname: "watch-test".to_string(),
        preview_publication: Some(PreviewPublicationPolicy {
            loopback_only: true,
            active_job_only: true,
        }),
    })
}

fn find_input_dependencies(source: &str) -> Vec<String> {
    let mut dependencies = Vec::new();
    for line in source.lines() {
        let visible = strip_line_comment(line);
        for command in ["input", "include"] {
            dependencies.extend(
                find_braced_commands(&visible, command)
                    .into_iter()
                    .map(|(_, value)| value),
            );
        }
    }
    dependencies
}

fn find_braced_commands(line: &str, command: &str) -> Vec<(usize, String)> {
    let needle = format!("\\{command}{{");
    let mut search_offset = 0usize;
    let mut matches = Vec::new();

    while let Some(found) = line[search_offset..].find(&needle) {
        let start = search_offset + found;
        let value_start = start + needle.len();
        let Some(value_end_relative) = line[value_start..].find('}') else {
            break;
        };
        let value_end = value_start + value_end_relative;
        matches.push((start, line[value_start..value_end].to_string()));
        search_offset = value_end + 1;
    }

    matches
}

fn resolve_tex_path(base_dir: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    };

    if candidate.extension().is_some() {
        candidate
    } else {
        candidate.with_extension("tex")
    }
}

fn strip_line_comment(line: &str) -> String {
    let mut visible = String::with_capacity(line.len());
    let mut escaped = false;

    for ch in line.chars() {
        if escaped {
            visible.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' => {
                visible.push(ch);
                escaped = true;
            }
            '%' => break,
            _ => visible.push(ch),
        }
    }

    visible
}

fn normalize_candidate(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::{discover_watched_paths, watcher_io_diagnostic};
    use ferritex_core::diagnostics::Severity;

    #[test]
    fn watcher_io_diagnostic_for_not_found_explains_cause_and_suggests_revert() {
        let error = io::Error::from(io::ErrorKind::NotFound);
        let diagnostic = watcher_io_diagnostic(&error, "failed to refresh watched files");

        assert_eq!(diagnostic.severity, Severity::Error);
        assert!(
            diagnostic.message.contains("no longer exists"),
            "message should explain that the path is gone: {}",
            diagnostic.message,
        );
        let suggestion = diagnostic
            .suggestion
            .as_deref()
            .expect("NotFound diagnostic should include a suggestion");
        assert!(
            suggestion.contains("revert") || suggestion.contains("restore"),
            "suggestion should hint at reverting the deletion: {suggestion}",
        );
        assert!(
            !diagnostic.message.contains("os error"),
            "raw OS error string should not leak into the message: {}",
            diagnostic.message,
        );
    }

    #[test]
    fn watcher_io_diagnostic_falls_back_to_raw_error_for_other_kinds() {
        let error = io::Error::new(io::ErrorKind::PermissionDenied, "denied by policy");
        let diagnostic = watcher_io_diagnostic(&error, "failed to refresh watched files");

        assert_eq!(diagnostic.severity, Severity::Error);
        assert!(diagnostic.message.contains("failed to refresh watched files"));
        assert!(diagnostic.message.contains("denied by policy"));
        assert!(diagnostic.suggestion.is_none());
    }

    #[test]
    fn watches_input_and_nested_dependencies() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let main = dir.path().join("main.tex");
        let chapter = dir.path().join("chap1.tex");
        let appendix_dir = dir.path().join("appendix");
        let appendix = appendix_dir.join("extra.tex");
        std::fs::create_dir_all(&appendix_dir).expect("create appendix dir");
        std::fs::write(&main, "\\input{chap1}\n").expect("write main");
        std::fs::write(&chapter, "\\include{appendix/extra}\n").expect("write chapter");
        std::fs::write(&appendix, "hello").expect("write appendix");

        let gate = super::allow_all_file_access_gate();
        let watched = discover_watched_paths(&gate, &main);
        let main = main.canonicalize().expect("canonical main");
        let chapter = chapter.canonicalize().expect("canonical chapter");
        let appendix = appendix.canonicalize().expect("canonical appendix");

        assert!(watched.contains(&main));
        assert!(watched.contains(&chapter));
        assert!(watched.contains(&appendix));
    }
}
