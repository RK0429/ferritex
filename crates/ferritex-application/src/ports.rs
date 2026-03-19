use std::path::{Path, PathBuf};

pub trait AssetBundleLoaderPort: Send + Sync {
    fn validate(&self, bundle_path: &Path) -> Result<(), String>;
    fn resolve_tex_input(&self, bundle_path: &Path, relative_path: &str) -> Option<PathBuf>;
}

pub trait PreviewTransportPort: Send + Sync {
    fn publish_pdf(&self, session_id: &str, pdf_bytes: &[u8]) -> Result<(), String>;
    fn session_url(&self, session_id: &str) -> String;
    fn document_url(&self, session_id: &str) -> String;
    fn events_url(&self, session_id: &str) -> String;
}
