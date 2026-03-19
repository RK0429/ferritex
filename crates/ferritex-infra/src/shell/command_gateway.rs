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
        _program: &str,
        _args: &[&str],
    ) -> Result<CommandResult, ShellCommandError> {
        if !self.allowed {
            return Err(ShellCommandError::NotAllowed);
        }

        Ok(CommandResult {
            exit_code: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use ferritex_core::policy::{ExecutionPolicy, PreviewPublicationPolicy};

    use super::{ShellCommandError, ShellCommandGateway};

    #[test]
    fn rejects_execution_when_shell_escape_is_not_allowed() {
        let gateway = ShellCommandGateway::new(false);

        let error = gateway
            .execute("echo", &["ok"])
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
}
