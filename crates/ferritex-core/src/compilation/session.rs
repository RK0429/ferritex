use std::time::SystemTime;

use super::CompilationJob;
use crate::policy::ExecutionPolicy;

#[derive(Debug, Clone, Copy)]
pub struct JobContext<'a> {
    pub job: &'a CompilationJob,
    pub policy: &'a ExecutionPolicy,
}

#[derive(Debug, Clone)]
pub struct CompilationSession<'a> {
    pub pass_number: u32,
    pub started_at: SystemTime,
    pub context: JobContext<'a>,
}
