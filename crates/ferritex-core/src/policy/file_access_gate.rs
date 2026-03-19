use std::path::{Path, PathBuf};

use thiserror::Error;

use super::PathAccessDecision;

/// ファイルアクセスゲート (Port trait — infra が実装する)
pub trait FileAccessGate: Send + Sync {
    fn ensure_directory(&self, path: &Path) -> Result<(), FileAccessError>;
    fn check_read(&self, path: &Path) -> PathAccessDecision;
    fn check_write(&self, path: &Path) -> PathAccessDecision;
    fn check_readback(
        &self,
        path: &Path,
        primary_input: &Path,
        jobname: &str,
    ) -> PathAccessDecision;
    fn read_file(&self, path: &Path) -> Result<Vec<u8>, FileAccessError>;
    fn write_file(&self, path: &Path, content: &[u8]) -> Result<(), FileAccessError>;
    fn read_readback(
        &self,
        path: &Path,
        primary_input: &Path,
        jobname: &str,
    ) -> Result<Vec<u8>, FileAccessError>;
}

#[derive(Debug, Error)]
pub enum FileAccessError {
    #[error("access denied: {path}")]
    AccessDenied { path: PathBuf },
    #[error("I/O error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },
}
