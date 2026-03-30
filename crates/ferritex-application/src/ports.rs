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
    fn resolve_opentype_font(&self, _bundle_path: &Path, _font_name: &str) -> Option<PathBuf> {
        None
    }
    fn resolve_default_opentype_font(&self, _bundle_path: &Path) -> Option<PathBuf> {
        None
    }
    fn resolve_tfm_font(&self, _bundle_path: &Path, _font_name: &str) -> Option<PathBuf> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommandOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransportRevisionEvent {
    pub session_id: String,
    pub target_input: String,
    pub target_jobname: String,
    pub revision: u64,
    pub page_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransportViewStateUpdate {
    pub page_number: usize,
    pub zoom: f64,
    pub viewport_offset_y: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventsSessionError {
    Expired { session_id: String },
    Unknown { session_id: String },
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
    /// Publish a revision event to the events channel for the given session.
    fn publish_revision_event(&self, event: &TransportRevisionEvent) -> Result<(), String>;
    fn session_url(&self, session_id: &str) -> String;
    fn document_url(&self, session_id: &str) -> String;
    fn events_url(&self, session_id: &str) -> String;
    /// Check whether the events path is available for the given session.
    /// Returns Ok(()) if session is active, Expired if invalidated, Unknown otherwise.
    fn check_events_session(&self, session_id: &str) -> Result<(), EventsSessionError>;
    /// Submit a view-state update received from a preview client on the events channel.
    fn submit_view_update(
        &self,
        session_id: &str,
        update: &TransportViewStateUpdate,
    ) -> Result<(), EventsSessionError>;
    /// Take (drain) all pending revision events for the given session.
    fn take_pending_events(
        &self,
        session_id: &str,
    ) -> Result<Vec<TransportRevisionEvent>, EventsSessionError>;
    /// Take (drain) all pending view-state updates for the given session.
    fn take_pending_view_updates(
        &self,
        session_id: &str,
    ) -> Result<Vec<TransportViewStateUpdate>, EventsSessionError>;
    fn notify_session_invalidated(&self, session_id: &str);
}
