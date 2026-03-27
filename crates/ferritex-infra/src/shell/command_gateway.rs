use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ferritex_application::ports::{ShellCommandGatewayPort, ShellCommandOutput};
use ferritex_core::policy::ExecutionPolicy;

pub struct ShellCommandGateway {
    allowed: bool,
    timeout_secs: u64,
    max_processes: usize,
    max_output_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum ShellCommandError {
    #[error("shell escape is not allowed")]
    NotAllowed,
    #[error("command timed out after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },
    #[error("output exceeded {max_bytes} bytes")]
    OutputTooLarge { max_bytes: usize },
    #[error("I/O error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}

impl Default for ShellCommandGateway {
    fn default() -> Self {
        Self {
            allowed: false,
            timeout_secs: 30,
            max_processes: 1,
            max_output_bytes: 4 * 1024 * 1024,
        }
    }
}

impl ShellCommandGateway {
    pub fn new(allowed: bool) -> Self {
        Self {
            allowed,
            ..Self::default()
        }
    }

    pub fn with_limits(
        allowed: bool,
        timeout_secs: u64,
        max_processes: usize,
        max_output_bytes: usize,
    ) -> Self {
        Self {
            allowed,
            timeout_secs,
            max_processes,
            max_output_bytes,
        }
    }

    pub fn from_policy(policy: &ExecutionPolicy) -> Self {
        Self::new(policy.shell_escape_allowed)
    }

    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    pub fn max_processes(&self) -> usize {
        self.max_processes
    }

    pub fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    pub fn execute(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
    ) -> Result<CommandResult, ShellCommandError> {
        if !self.allowed {
            return Err(ShellCommandError::NotAllowed);
        }

        let mut child = Command::new(program)
            .args(args)
            .current_dir(working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("stdout pipe"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("stderr pipe"))?;

        let total_output = Arc::new(AtomicUsize::new(0));
        let output_exceeded = Arc::new(AtomicBool::new(false));
        let captured_stdout = Arc::new(Mutex::new(Vec::new()));
        let captured_stderr = Arc::new(Mutex::new(Vec::new()));
        let stdout_reader = spawn_reader(
            stdout,
            Arc::clone(&captured_stdout),
            Arc::clone(&total_output),
            Arc::clone(&output_exceeded),
            self.max_output_bytes,
        );
        let stderr_reader = spawn_reader(
            stderr,
            Arc::clone(&captured_stderr),
            Arc::clone(&total_output),
            Arc::clone(&output_exceeded),
            self.max_output_bytes,
        );

        let started_at = Instant::now();
        let status = loop {
            if output_exceeded.load(Ordering::SeqCst) {
                let _ = child.kill();
                break Err(ShellCommandError::OutputTooLarge {
                    max_bytes: self.max_output_bytes,
                });
            }

            if started_at.elapsed() >= Duration::from_secs(self.timeout_secs) {
                let _ = child.kill();
                break Err(ShellCommandError::Timeout {
                    timeout_secs: self.timeout_secs,
                });
            }

            if let Some(status) = child.try_wait()? {
                break Ok(status);
            }

            thread::sleep(Duration::from_millis(10));
        };

        let stdout_read = stdout_reader
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("stdout reader thread panicked")));
        let stderr_read = stderr_reader
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("stderr reader thread panicked")));
        stdout_read?;
        stderr_read?;

        let stdout = Arc::try_unwrap(captured_stdout)
            .map_err(|_| std::io::Error::other("stdout buffer still shared"))?
            .into_inner()
            .map_err(|_| std::io::Error::other("stdout buffer poisoned"))?;
        let stderr = Arc::try_unwrap(captured_stderr)
            .map_err(|_| std::io::Error::other("stderr buffer still shared"))?
            .into_inner()
            .map_err(|_| std::io::Error::other("stderr buffer poisoned"))?;

        let status = status?;
        Ok(CommandResult {
            exit_code: status.code().unwrap_or(1),
            stdout,
            stderr,
        })
    }
}

impl ShellCommandGatewayPort for ShellCommandGateway {
    fn execute(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
    ) -> Result<ShellCommandOutput, String> {
        ShellCommandGateway::execute(self, program, args, working_dir)
            .map(|result| ShellCommandOutput {
                exit_code: result.exit_code,
                stdout: result.stdout,
                stderr: result.stderr,
            })
            .map_err(|error| error.to_string())
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    target: Arc<Mutex<Vec<u8>>>,
    total_output: Arc<AtomicUsize>,
    output_exceeded: Arc<AtomicBool>,
    limit: usize,
) -> thread::JoinHandle<std::io::Result<()>> {
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            let read = reader.read(&mut chunk)?;
            if read == 0 {
                return Ok(());
            }

            let previous = total_output.fetch_add(read, Ordering::SeqCst);
            let remaining = limit.saturating_sub(previous);
            let stored = remaining.min(read);
            if stored < read {
                output_exceeded.store(true, Ordering::SeqCst);
            }

            if stored > 0 {
                target
                    .lock()
                    .map_err(|_| std::io::Error::other("output buffer poisoned"))?
                    .extend_from_slice(&chunk[..stored]);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use ferritex_core::policy::{ExecutionPolicy, PreviewPublicationPolicy};

    use super::{ShellCommandError, ShellCommandGateway};

    #[test]
    fn rejects_execution_when_shell_escape_is_not_allowed() {
        let gateway = ShellCommandGateway::new(false);

        let error = gateway
            .execute("echo", &["ok"], Path::new("."))
            .expect_err("execution should be denied");

        assert!(matches!(error, ShellCommandError::NotAllowed));
    }

    #[test]
    fn default_limits_match_policy_defaults() {
        let gateway = ShellCommandGateway::default();

        assert_eq!(gateway.timeout_secs(), 30);
        assert_eq!(gateway.max_processes(), 1);
        assert_eq!(gateway.max_output_bytes(), 4 * 1024 * 1024);
    }

    #[test]
    fn from_policy_copies_shell_escape_flag() {
        let policy = ExecutionPolicy {
            shell_escape_allowed: true,
            allowed_read_paths: Vec::new(),
            allowed_write_paths: Vec::new(),
            output_dir: "out".into(),
            jobname: "main".to_string(),
            preview_publication: Some(PreviewPublicationPolicy {
                loopback_only: true,
                active_job_only: true,
            }),
        };

        let gateway = ShellCommandGateway::from_policy(&policy);

        assert!(gateway.allowed);
    }

    #[cfg(unix)]
    #[test]
    fn executes_commands_and_captures_output() {
        let gateway = ShellCommandGateway::new(true);
        let result = gateway
            .execute(
                "/bin/sh",
                &["-c", "printf stdout && printf stderr >&2"],
                Path::new("."),
            )
            .expect("command should run");

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, b"stdout");
        assert_eq!(result.stderr, b"stderr");
    }

    #[cfg(unix)]
    #[test]
    fn kills_commands_that_exceed_timeout() {
        let gateway = ShellCommandGateway::with_limits(true, 0, 1, 4 * 1024 * 1024);
        let error = gateway
            .execute("/bin/sh", &["-c", "sleep 1"], Path::new("."))
            .expect_err("command should time out");

        assert!(matches!(
            error,
            ShellCommandError::Timeout { timeout_secs: 0 }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_commands_that_exceed_output_limit() {
        let gateway = ShellCommandGateway::with_limits(true, 30, 1, 4);
        let error = gateway
            .execute("/bin/sh", &["-c", "printf 123456"], Path::new("."))
            .expect_err("command should exceed output limit");

        assert!(matches!(
            error,
            ShellCommandError::OutputTooLarge { max_bytes: 4 }
        ));
    }
}
