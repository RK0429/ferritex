pub trait ShellEscapeHandler: Send + Sync {
    fn execute_write18(&self, command: &str, line: u32) -> ShellEscapeResult;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEscapeResult {
    Denied,
    Success { exit_code: i32 },
    Error(String),
}

pub trait FileOperationHandler: Send + Sync {
    fn check_open_read(&self, path: &str, line: u32) -> FileOperationResult;
    fn check_open_write(&self, path: &str, line: u32) -> FileOperationResult;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOperationResult {
    Allowed,
    Denied { path: String, reason: String },
}
