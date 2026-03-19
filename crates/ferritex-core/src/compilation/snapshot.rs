use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::CompilationSession;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilationSnapshot {
    pub pass_number: u32,
    pub primary_input: PathBuf,
    pub jobname: String,
}

impl CompilationSnapshot {
    pub fn from_session(session: &CompilationSession<'_>) -> Self {
        Self {
            pass_number: session.pass_number,
            primary_input: session.context.job.primary_input.clone(),
            jobname: session.context.job.jobname.clone(),
        }
    }
}
