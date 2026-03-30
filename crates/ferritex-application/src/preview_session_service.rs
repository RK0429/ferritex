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

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewViewState {
    pub page_number: usize,
    pub zoom: f64,
    pub viewport_offset_y: f64,
}

impl Default for PreviewViewState {
    fn default() -> Self {
        Self {
            page_number: 1,
            zoom: 1.0,
            viewport_offset_y: 0.0,
        }
    }
}

impl PreviewViewState {
    pub fn clamp_to_page_count(&self, page_count: usize) -> Self {
        let clamped_page = if page_count == 0 {
            1
        } else {
            self.page_number.min(page_count).max(1)
        };
        Self {
            page_number: clamped_page,
            zoom: self.zoom,
            viewport_offset_y: self.viewport_offset_y,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewRevision {
    pub target: PreviewTarget,
    pub revision: u64,
    pub page_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionErrorKind {
    PreviewDisabled,
    LoopbackRequired,
    SessionExpired,
    SessionUnknown,
    TargetMismatch,
}

impl fmt::Display for SessionErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PreviewDisabled => f.write_str("preview_disabled"),
            Self::LoopbackRequired => f.write_str("loopback_required"),
            Self::SessionExpired => f.write_str("session_expired"),
            Self::SessionUnknown => f.write_str("session_unknown"),
            Self::TargetMismatch => f.write_str("target_mismatch"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreviewSession {
    pub session_id: SessionId,
    pub target: PreviewTarget,
    pub document_url: String,
    pub events_url: String,
    pub view_state: PreviewViewState,
    pub revision: u64,
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
    pub error_kind: SessionErrorKind,
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
    target_sessions: HashMap<PreviewTarget, SessionId>,
    next_session_id: u64,
}

impl PreviewSessionService {
    pub fn new(transport: Arc<dyn PreviewTransportPort>) -> Self {
        Self {
            transport,
            sessions: HashMap::new(),
            target_sessions: HashMap::new(),
            next_session_id: 0,
        }
    }

    pub fn create_session(
        &mut self,
        target: &PreviewTarget,
        policy: &ExecutionPolicy,
    ) -> Result<SessionBootstrapPayload, SessionErrorResponse> {
        if policy.preview_publication.is_none() {
            let session_id = self.allocate_session_id();
            tracing::warn!(
                session_id = %session_id,
                input = %target.input_file.display(),
                jobname = %target.jobname,
                "preview session rejected because preview publication is disabled"
            );

            return Err(SessionErrorResponse {
                error_kind: SessionErrorKind::PreviewDisabled,
                session_id,
                recovery_instruction: "rerun with preview publication enabled".to_string(),
            });
        }

        if let Some(existing_id) = self.target_sessions.get(target) {
            if let Some(session) = self.sessions.get(existing_id) {
                if session.active {
                    tracing::info!(
                        session_id = %existing_id,
                        input = %target.input_file.display(),
                        jobname = %target.jobname,
                        "reusing existing active preview session"
                    );
                    return Ok(SessionBootstrapPayload {
                        session_id: session.session_id.clone(),
                        document_url: session.document_url.clone(),
                        events_url: session.events_url.clone(),
                    });
                }
            }
        }

        let session_id = self.allocate_session_id();
        let document_url = self.transport.document_url(session_id.as_str());
        let events_url = self.transport.events_url(session_id.as_str());
        let session = PreviewSession {
            session_id: session_id.clone(),
            target: target.clone(),
            document_url: document_url.clone(),
            events_url: events_url.clone(),
            view_state: PreviewViewState::default(),
            revision: 0,
            active: true,
        };

        self.sessions.insert(session_id.clone(), session);
        self.target_sessions
            .insert(target.clone(), session_id.clone());

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

    pub fn find_active_session(&self, target: &PreviewTarget) -> Option<&PreviewSession> {
        let session_id = self.target_sessions.get(target)?;
        let session = self.sessions.get(session_id)?;
        if session.active {
            Some(session)
        } else {
            None
        }
    }

    pub fn update_view_state(&mut self, session_id: &SessionId, view_state: PreviewViewState) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.view_state = view_state;
            tracing::debug!(session_id = %session_id, "preview view state updated");
        }
    }

    pub fn apply_page_fallback(
        &mut self,
        session_id: &SessionId,
        new_page_count: usize,
    ) -> Option<PreviewViewState> {
        let session = self.sessions.get_mut(session_id)?;
        let clamped = session.view_state.clamp_to_page_count(new_page_count);
        session.view_state = clamped.clone();
        Some(clamped)
    }

    /// Advance the revision counter for a session and return a PreviewRevision.
    /// Returns None if session doesn't exist.
    pub fn advance_revision(
        &mut self,
        session_id: &SessionId,
        page_count: usize,
    ) -> Option<PreviewRevision> {
        let session = self.sessions.get_mut(session_id)?;
        session.revision += 1;
        Some(PreviewRevision {
            target: session.target.clone(),
            revision: session.revision,
            page_count,
        })
    }

    /// Check whether the events path is available for a session.
    /// Distinguishes expired (invalidated) from unknown sessions.
    pub fn check_events_session(&self, session_id: &SessionId) -> Result<(), SessionErrorKind> {
        match self.sessions.get(session_id) {
            Some(session) if session.active => Ok(()),
            Some(_) => Err(SessionErrorKind::SessionExpired),
            None => Err(SessionErrorKind::SessionUnknown),
        }
    }

    pub fn check_publish(
        &self,
        session_id: &SessionId,
        compile_target: &PreviewTarget,
        policy: &ExecutionPolicy,
    ) -> PublishDecision {
        let Some(preview_policy) = &policy.preview_publication else {
            return PublishDecision::Denied(Self::session_error(
                SessionErrorKind::PreviewDisabled,
                session_id.clone(),
                "rerun with preview publication enabled",
            ));
        };

        if !preview_policy.loopback_only {
            return PublishDecision::Denied(Self::session_error(
                SessionErrorKind::LoopbackRequired,
                session_id.clone(),
                "connect through the loopback preview transport",
            ));
        }

        let Some(session) = self.sessions.get(session_id) else {
            return PublishDecision::Denied(Self::session_error(
                SessionErrorKind::SessionUnknown,
                session_id.clone(),
                "bootstrap a new preview session via POST /preview/session",
            ));
        };

        if !session.active {
            return PublishDecision::Denied(Self::session_error(
                SessionErrorKind::SessionExpired,
                session_id.clone(),
                "bootstrap a new preview session via POST /preview/session",
            ));
        }

        if preview_policy.active_job_only && session.target != *compile_target {
            return PublishDecision::Denied(Self::session_error(
                SessionErrorKind::TargetMismatch,
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
            self.target_sessions.remove(&session.target);
            self.transport
                .notify_session_invalidated(session_id.as_str());
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
        error_kind: SessionErrorKind,
        session_id: SessionId,
        recovery_instruction: &str,
    ) -> SessionErrorResponse {
        tracing::warn!(
            session_id = %session_id,
            error_kind = %error_kind,
            recovery_instruction,
            "preview publish denied"
        );

        SessionErrorResponse {
            error_kind,
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

    use super::{
        PreviewSessionService, PreviewTarget, PreviewViewState, PublishDecision, SessionErrorKind,
    };
    use crate::ports::{
        EventsSessionError, PreviewTransportPort, TransportRevisionEvent, TransportViewStateUpdate,
    };

    struct DummyPreviewTransport;

    impl PreviewTransportPort for DummyPreviewTransport {
        fn publish_pdf(&self, _session_id: &str, _pdf_bytes: &[u8]) -> Result<(), String> {
            Ok(())
        }

        fn publish_revision_event(&self, _event: &TransportRevisionEvent) -> Result<(), String> {
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

        fn check_events_session(&self, session_id: &str) -> Result<(), EventsSessionError> {
            Err(EventsSessionError::Unknown {
                session_id: session_id.to_string(),
            })
        }

        fn submit_view_update(
            &self,
            session_id: &str,
            _update: &TransportViewStateUpdate,
        ) -> Result<(), EventsSessionError> {
            Err(EventsSessionError::Unknown {
                session_id: session_id.to_string(),
            })
        }

        fn take_pending_events(
            &self,
            session_id: &str,
        ) -> Result<Vec<TransportRevisionEvent>, EventsSessionError> {
            Err(EventsSessionError::Unknown {
                session_id: session_id.to_string(),
            })
        }

        fn take_pending_view_updates(
            &self,
            session_id: &str,
        ) -> Result<Vec<TransportViewStateUpdate>, EventsSessionError> {
            Err(EventsSessionError::Unknown {
                session_id: session_id.to_string(),
            })
        }

        fn notify_session_invalidated(&self, _session_id: &str) {}
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
        assert_eq!(session.view_state.page_number, 1);
        assert!((session.view_state.zoom - 1.0).abs() < f64::EPSILON);
        assert!((session.view_state.viewport_offset_y - 0.0).abs() < f64::EPSILON);
        assert_eq!(session.revision, 0);
        assert!(session.active);
    }

    #[test]
    fn advance_revision_produces_monotonic_revisions() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");
        let payload = service
            .create_session(&target, &execution_policy(true))
            .expect("create session");

        let first = service
            .advance_revision(&payload.session_id, 3)
            .expect("first revision");
        let second = service
            .advance_revision(&payload.session_id, 5)
            .expect("second revision");
        let third = service
            .advance_revision(&payload.session_id, 8)
            .expect("third revision");

        assert_eq!(first.revision, 1);
        assert_eq!(first.page_count, 3);
        assert_eq!(first.target, target);
        assert_eq!(second.revision, 2);
        assert_eq!(second.page_count, 5);
        assert_eq!(second.target, target);
        assert_eq!(third.revision, 3);
        assert_eq!(third.page_count, 8);
        assert_eq!(third.target, target);
    }

    #[test]
    fn advance_revision_returns_none_for_unknown_session() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let unknown_id = super::SessionId::new("missing-session");

        assert_eq!(service.advance_revision(&unknown_id, 4), None);
    }

    #[test]
    fn check_events_session_active_returns_ok() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");
        let payload = service
            .create_session(&target, &execution_policy(true))
            .expect("create session");

        assert_eq!(service.check_events_session(&payload.session_id), Ok(()));
    }

    #[test]
    fn check_events_session_invalidated_returns_expired() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");
        let payload = service
            .create_session(&target, &execution_policy(true))
            .expect("create session");
        service.invalidate_session(&payload.session_id);

        assert_eq!(
            service.check_events_session(&payload.session_id),
            Err(SessionErrorKind::SessionExpired)
        );
    }

    #[test]
    fn check_events_session_unknown_returns_unknown() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let service = PreviewSessionService::new(transport);
        let unknown_id = super::SessionId::new("missing-session");

        assert_eq!(
            service.check_events_session(&unknown_id),
            Err(SessionErrorKind::SessionUnknown)
        );
    }

    #[test]
    fn create_session_reuses_active_session_for_same_target() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");

        let first = service
            .create_session(&target, &execution_policy(true))
            .expect("first session");
        let second = service
            .create_session(&target, &execution_policy(true))
            .expect("second session");

        assert_eq!(first.session_id, second.session_id);
        assert_eq!(first.document_url, second.document_url);
        assert_eq!(first.events_url, second.events_url);
    }

    #[test]
    fn create_session_allocates_new_after_invalidation() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");

        let first = service
            .create_session(&target, &execution_policy(true))
            .expect("first session");
        service.invalidate_session(&first.session_id);
        let second = service
            .create_session(&target, &execution_policy(true))
            .expect("second session");

        assert_ne!(first.session_id, second.session_id);
    }

    #[test]
    fn create_session_different_targets_get_different_sessions() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);

        let first = service
            .create_session(
                &preview_target("chapter.tex", "chapter"),
                &execution_policy(true),
            )
            .expect("first session");
        let second = service
            .create_session(
                &preview_target("appendix.tex", "appendix"),
                &execution_policy(true),
            )
            .expect("second session");

        assert_ne!(first.session_id, second.session_id);
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

        assert_eq!(error.error_kind, SessionErrorKind::PreviewDisabled);
        assert_eq!(
            error.recovery_instruction,
            "rerun with preview publication enabled"
        );
        assert!(service.get_session(&error.session_id).is_none());
    }

    #[test]
    fn find_active_session_returns_active() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");

        let payload = service
            .create_session(&target, &execution_policy(true))
            .expect("create session");

        let found = service.find_active_session(&target).expect("found session");
        assert_eq!(found.session_id, payload.session_id);
    }

    #[test]
    fn find_active_session_returns_none_after_invalidation() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");

        let payload = service
            .create_session(&target, &execution_policy(true))
            .expect("create session");
        service.invalidate_session(&payload.session_id);

        assert!(service.find_active_session(&target).is_none());
    }

    #[test]
    fn view_state_persists_across_updates() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");

        let payload = service
            .create_session(&target, &execution_policy(true))
            .expect("create session");

        let custom_state = PreviewViewState {
            page_number: 5,
            zoom: 1.5,
            viewport_offset_y: 120.0,
        };
        service.update_view_state(&payload.session_id, custom_state.clone());

        let session = service
            .get_session(&payload.session_id)
            .expect("stored session");
        assert_eq!(session.view_state, custom_state);
    }

    #[test]
    fn page_fallback_snaps_to_nearest_valid_page() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");

        let payload = service
            .create_session(&target, &execution_policy(true))
            .expect("create session");

        service.update_view_state(
            &payload.session_id,
            PreviewViewState {
                page_number: 20,
                zoom: 2.0,
                viewport_offset_y: 50.0,
            },
        );

        let clamped = service
            .apply_page_fallback(&payload.session_id, 15)
            .expect("fallback result");

        assert_eq!(clamped.page_number, 15);
        assert!((clamped.zoom - 2.0).abs() < f64::EPSILON);
        assert!((clamped.viewport_offset_y - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn page_fallback_preserves_when_within_range() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");

        let payload = service
            .create_session(&target, &execution_policy(true))
            .expect("create session");

        service.update_view_state(
            &payload.session_id,
            PreviewViewState {
                page_number: 5,
                zoom: 1.25,
                viewport_offset_y: 30.0,
            },
        );

        let clamped = service
            .apply_page_fallback(&payload.session_id, 20)
            .expect("fallback result");

        assert_eq!(clamped.page_number, 5);
        assert!((clamped.zoom - 1.25).abs() < f64::EPSILON);
        assert!((clamped.viewport_offset_y - 30.0).abs() < f64::EPSILON);
    }

    #[test]
    fn clamp_to_page_count_zero_pages_returns_page_one() {
        let state = PreviewViewState {
            page_number: 10,
            zoom: 1.0,
            viewport_offset_y: 0.0,
        };
        let clamped = state.clamp_to_page_count(0);
        assert_eq!(clamped.page_number, 1);
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
                error_kind: SessionErrorKind::TargetMismatch,
                session_id: payload.session_id,
                recovery_instruction: "request a new session for the active preview target"
                    .to_string(),
            })
        );
    }

    #[test]
    fn check_publish_denies_invalidated_session_with_expired_kind() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let mut service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");
        let payload = service
            .create_session(&target, &execution_policy(true))
            .expect("create session");
        service.invalidate_session(&payload.session_id);

        let decision = service.check_publish(&payload.session_id, &target, &execution_policy(true));

        match decision {
            PublishDecision::Denied(err) => {
                assert_eq!(err.error_kind, SessionErrorKind::SessionExpired);
                assert!(err.recovery_instruction.contains("POST /preview/session"));
            }
            _ => panic!("expected denied"),
        }
    }

    #[test]
    fn check_publish_denies_unknown_session() {
        let transport: Arc<dyn PreviewTransportPort> = Arc::new(DummyPreviewTransport);
        let service = PreviewSessionService::new(transport);
        let target = preview_target("chapter.tex", "chapter");
        let unknown_id = super::SessionId::new("nonexistent");

        let decision = service.check_publish(&unknown_id, &target, &execution_policy(true));

        match decision {
            PublishDecision::Denied(err) => {
                assert_eq!(err.error_kind, SessionErrorKind::SessionUnknown);
                assert!(err.recovery_instruction.contains("POST /preview/session"));
            }
            _ => panic!("expected denied"),
        }
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
                error_kind: SessionErrorKind::PreviewDisabled,
                session_id: payload.session_id,
                recovery_instruction: "rerun with preview publication enabled".to_string(),
            })
        );
    }
}
