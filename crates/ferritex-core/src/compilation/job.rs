use std::path::PathBuf;
use std::time::SystemTime;

use super::{CompilationSession, DocumentState, JobContext};
use crate::policy::{ExecutionPolicy, OutputArtifactRegistry};

#[derive(Debug, Clone)]
pub struct CompilationJob {
    pub primary_input: PathBuf,
    pub jobname: String,
    pub policy: ExecutionPolicy,
    pub document_state: DocumentState,
    pub output_artifacts: OutputArtifactRegistry,
}

impl CompilationJob {
    pub fn begin_pass(&self, pass_number: u32) -> CompilationSession<'_> {
        CompilationSession {
            pass_number,
            started_at: SystemTime::now(),
            context: JobContext {
                job: self,
                policy: &self.policy,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{CompilationJob, DocumentState};
    use crate::policy::{ExecutionPolicy, OutputArtifactRegistry, PreviewPublicationPolicy};

    #[test]
    fn begin_pass_creates_a_session_with_job_context() {
        let job = CompilationJob {
            primary_input: PathBuf::from("src/main.tex"),
            jobname: "main".to_string(),
            policy: ExecutionPolicy {
                shell_escape_allowed: false,
                allowed_read_paths: vec![PathBuf::from("src")],
                allowed_write_paths: vec![PathBuf::from("out")],
                output_dir: PathBuf::from("out"),
                jobname: "main".to_string(),
                preview_publication: Some(PreviewPublicationPolicy {
                    loopback_only: true,
                    active_job_only: true,
                }),
            },
            document_state: DocumentState::default(),
            output_artifacts: OutputArtifactRegistry::new(),
        };

        let session = job.begin_pass(2);

        assert_eq!(session.pass_number, 2);
        assert_eq!(session.context.job.jobname, "main");
        assert_eq!(session.context.policy.jobname, "main");
    }
}
