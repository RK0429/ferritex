use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::PathAccessPolicy;

/// コンパイルジョブの実行ポリシー (Value Object)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    pub shell_escape_allowed: bool,
    pub allowed_read_paths: Vec<PathBuf>,
    pub allowed_write_paths: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub jobname: String,
    pub preview_publication: Option<PreviewPublicationPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewPublicationPolicy {
    pub loopback_only: bool,
    pub active_job_only: bool,
}

impl ExecutionPolicy {
    pub fn to_path_access_policy(&self) -> PathAccessPolicy {
        PathAccessPolicy {
            allowed_read_dirs: self.allowed_read_paths.clone(),
            allowed_write_dirs: self.allowed_write_paths.clone(),
        }
    }
}
