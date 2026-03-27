use std::path::{Path, PathBuf};

pub trait AssetBundleLoaderPort: Send + Sync {
    fn validate(&self, bundle_path: &Path) -> Result<(), String>;
    fn resolve_tex_input(&self, bundle_path: &Path, relative_path: &str) -> Option<PathBuf>;
    fn resolve_package(
        &self,
        _bundle_path: &Path,
        _package_name: &str,
        _project_root: Option<&Path>,
    ) -> Option<PathBuf> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommandOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub trait ShellCommandGatewayPort: Send + Sync {
    fn execute(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
    ) -> Result<ShellCommandOutput, String>;
}

pub trait PreviewTransportPort: Send + Sync {
    fn publish_pdf(&self, session_id: &str, pdf_bytes: &[u8]) -> Result<(), String>;
    fn session_url(&self, session_id: &str) -> String;
    fn document_url(&self, session_id: &str) -> String;
    fn events_url(&self, session_id: &str) -> String;
}
