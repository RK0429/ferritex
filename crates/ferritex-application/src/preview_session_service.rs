use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use ferritex_core::policy::ExecutionPolicy;

use crate::ports::PreviewTransportPort;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PreviewTarget {
    pub input_file: PathBuf,
    pub jobname: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewViewPosition {
    pub page_number: usize,
}

impl Default for PreviewViewPosition {
    fn default() -> Self {
        Self { page_number: 1 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewSession {
    pub session_id: SessionId,
    pub target: PreviewTarget,
    pub document_url: String,
    pub events_url: String,
    pub view_position: PreviewViewPosition,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBootstrapPayload {
    pub session_id: SessionId,
    pub document_url: String,
    pub events_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionErrorResponse {
    pub error_kind: String,
    pub session_id: SessionId,
    pub recovery_instruction: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishDecision {
    Allowed,
    Denied(SessionErrorResponse),
}

pub struct PreviewSessionService {
    transport: Arc<dyn PreviewTransportPort>,
    sessions: HashMap<SessionId, PreviewSession>,
    next_session_id: u64,
}

impl PreviewSessionService {
    pub fn new(transport: Arc<dyn PreviewTransportPort>) -> Self {
        Self {
            transport,
            sessions: HashMap::new(),
            next_session_id: 0,
        }
    }

    pub fn create_session(
        &mut self,
        target: &PreviewTarget,
        policy: &ExecutionPolicy,
    ) -> Result<SessionBootstrapPayload, SessionErrorResponse> {
        let session_id = self.allocate_session_id();

        if policy.preview_publication.is_none() {
            tracing::warn!(
                session_id = %session_id,
                input = %target.input_file.display(),
                jobname = %target.jobname,
                "preview session rejected because preview publication is disabled"
            );

            return Err(SessionErrorResponse {
                error_kind: "preview_disabled".to_string(),
                session_id,
                recovery_instruction: "rerun with preview publication enabled".to_string(),
            });
        }

        let document_url = self.transport.document_url(session_id.as_str());
        let events_url = self.transport.events_url(session_id.as_str());
        let session = PreviewSession {
            session_id: session_id.clone(),
            target: target.clone(),
            document_url: document_url.clone(),
            events_url: events_url.clone(),
            view_position: PreviewViewPosition::default(),
            active: true,
        };

        self.sessions.insert(session_id.clone(), session);

        tracing::info!(
            session_id = %session_id,
            input = %target.input_file.display(),
            jobname = %target.jobname,
            "preview session created"
        );

        Ok(SessionBootstrapPayload {
            session_id,
            document_url,
            events_url,
        })
    }

    pub fn check_publish(
        &self,
        session_id: &SessionId,
        compile_target: &PreviewTarget,
        policy: &ExecutionPolicy,
    ) -> PublishDecision {
        let Some(preview_policy) = &policy.preview_publication else {
            return PublishDecision::Denied(Self::session_error(
                "preview_disabled",
                session_id.clone(),
                "rerun with preview publication enabled",
            ));
        };

        if !preview_policy.loopback_only {
            return PublishDecision::Denied(Self::session_error(
                "loopback_required",
                session_id.clone(),
                "connect through the loopback preview transport",
            ));
        }

        let Some(session) = self.sessions.get(session_id) else {
            return PublishDecision::Denied(Self::session_error(
                "session_invalid",
                session_id.clone(),
                "bootstrap a new preview session",
            ));
        };

        if !session.active {
            return PublishDecision::Denied(Self::session_error(
                "session_invalid",
                session_id.clone(),
                "bootstrap a new preview session",
            ));
        }

        if preview_policy.active_job_only && session.target != *compile_target {
            return PublishDecision::Denied(Self::session_error(
                "target_mismatch",
                session_id.clone(),
                "request a new session for the active preview target",
            ));
        }

        tracing::info!(
            session_id = %session_id,
            input = %compile_target.input_file.display(),
            jobname = %compile_target.jobname,
            "preview publish allowed"
        );

        PublishDecision::Allowed
    }

    pub fn invalidate_session(&mut self, session_id: &SessionId) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.active = false;
            tracing::info!(session_id = %session_id, "preview session invalidated");
        }
    }

    pub fn get_session(&self, session_id: &SessionId) -> Option<&PreviewSession> {
        self.sessions.get(session_id)
    }

    fn allocate_session_id(&mut self) -> SessionId {
        self.next_session_id += 1;
        SessionId::new(format!("preview-session-{}", self.next_session_id))
    }

    fn session_error(
        error_kind: &str,
        session_id: SessionId,
        recovery_instruction: &str,
    ) -> SessionErrorResponse {
        tracing::warn!(
            session_id = %session_id,
            error_kind,
            recovery_instruction,
            "preview publish denied"
        );

        SessionErrorResponse {
            error_kind: error_kind.to_string(),
            session_id,
            recovery_instruction: recovery_instruction.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use ferritex_core::policy::{ExecutionPolicy, PreviewPublicationPolicy};

    use super::{PreviewSessionService, PreviewTarget, PublishDecision};
    use crate::ports::PreviewTransportPort;

    struct DummyPreviewTransport;

    impl PreviewTransportPort for DummyPreviewTransport {
        fn publish_pdf(&self, _session_id: &str, _pdf_bytes: &[u8]) -> Result<(), String> {
            Ok(())
        }

        fn session_url(&self, session_id: &str) -> String {
            format!("http://127.0.0.1/preview/{session_id}")
        }

        fn document_url(&self, session_id: &str) -> String {
            format!("{}/document", self.session_url(session_id))
        }

        fn events_url(&self, session_id: &str) -> String {
            format!("ws://127.0.0.1/preview/{session_id}/events")
        }
    }

    fn preview_target(file: &str, jobname: &str) -> PreviewTarget {
        PreviewTarget {
            input_file: PathBuf::from(file),
            jobname: jobname.to_string(),
        }
    }

    fn execution_policy(preview_enabled: bool) -> ExecutionPolicy {
        ExecutionPolicy {
            shell_escape_allowed: false,
            allowed_read_paths: vec![PathBuf::from(".")],
            allowed_write_paths: vec![PathBuf::from(".")],
            output_dir: PathBuf::from("."),
            jobname: "main".to_string(),
            preview_publication: preview_enabled.then_some(PreviewPublicationPolicy {
                loopback_only: true,
                active_job_only: true,
            }),
        }
    }

    #[test]
    fn create_session_returns_bootstrap_payload() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");

        let payload = service
            .create_session(&target, &execution_policy(true))
            .expect("create session");

        assert_eq!(payload.session_id.as_str(), "preview-session-1");
        assert_eq!(
            payload.document_url,
            "http://127.0.0.1/preview/preview-session-1/document"
        );
        assert_eq!(
            payload.events_url,
            "ws://127.0.0.1/preview/preview-session-1/events"
        );

        let session = service
            .get_session(&payload.session_id)
            .expect("stored session");
        assert_eq!(session.target, target);
        assert_eq!(session.view_position.page_number, 1);
        assert!(session.active);
    }

    #[test]
    fn create_session_rejects_when_no_preview_policy() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);

        let error = service
            .create_session(
                &preview_target("chapter.tex", "chapter"),
                &execution_policy(false),
            )
            .expect_err("preview disabled");

        assert_eq!(error.error_kind, "preview_disabled");
        assert_eq!(
            error.recovery_instruction,
            "rerun with preview publication enabled"
        );
        assert!(service.get_session(&error.session_id).is_none());
    }

    #[test]
    fn check_publish_allows_matching_target() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");
        let payload = service
            .create_session(&target, &execution_policy(true))
            .expect("create session");

        let decision = service.check_publish(&payload.session_id, &target, &execution_policy(true));

        assert_eq!(decision, PublishDecision::Allowed);
    }

    #[test]
    fn check_publish_denies_mismatched_target() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let payload = service
            .create_session(
                &preview_target("chapter.tex", "chapter"),
                &execution_policy(true),
            )
            .expect("create session");

        let decision = service.check_publish(
            &payload.session_id,
            &preview_target("appendix.tex", "appendix"),
            &execution_policy(true),
        );

        assert_eq!(
            decision,
            PublishDecision::Denied(super::SessionErrorResponse {
                error_kind: "target_mismatch".to_string(),
                session_id: payload.session_id,
                recovery_instruction: "request a new session for the active preview target"
                    .to_string(),
            })
        );
    }

    #[test]
    fn check_publish_denies_invalidated_session() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");
        let payload = service
            .create_session(&target, &execution_policy(true))
            .expect("create session");
        service.invalidate_session(&payload.session_id);

        let decision = service.check_publish(&payload.session_id, &target, &execution_policy(true));

        assert_eq!(
            decision,
            PublishDecision::Denied(super::SessionErrorResponse {
                error_kind: "session_invalid".to_string(),
                session_id: payload.session_id,
                recovery_instruction: "bootstrap a new preview session".to_string(),
            })
        );
    }

    #[test]
    fn check_publish_denies_when_no_preview_policy() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");
        let payload = service
            .create_session(&target, &execution_policy(true))
            .expect("create session");

        let decision =
            service.check_publish(&payload.session_id, &target, &execution_policy(false));

        assert_eq!(
            decision,
            PublishDecision::Denied(super::SessionErrorResponse {
                error_kind: "preview_disabled".to_string(),
                session_id: payload.session_id,
                recovery_instruction: "rerun with preview publication enabled".to_string(),
            })
        );
    }
}
