use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use super::{CompilationSession, DocumentState};
use crate::parser::{CompatIntRegister, RegisterStore, Token};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RegisterBankView {
    pub counts: BTreeMap<u16, i32>,
    pub dimens: BTreeMap<u16, i32>,
    pub skips: BTreeMap<u16, i32>,
    pub muskips: BTreeMap<u16, i32>,
    pub toks: BTreeMap<u16, Vec<Token>>,
    pub compat_ints: BTreeMap<CompatIntRegister, i32>,
}

impl RegisterBankView {
    pub fn from_register_store(registers: &RegisterStore) -> Self {
        Self {
            counts: registers.counts_snapshot().into_iter().collect(),
            dimens: registers.dimens_snapshot().into_iter().collect(),
            skips: registers.skips_snapshot().into_iter().collect(),
            muskips: registers.muskips_snapshot().into_iter().collect(),
            toks: registers.toks_snapshot().into_iter().collect(),
            compat_ints: registers.compat_ints_snapshot().into_iter().collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CommandRegistryView;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EnvironmentRegistryView;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DocumentStateView {
    pub revision: u64,
    pub bibliography_dirty: bool,
    pub source_files: Vec<String>,
    pub labels: std::collections::BTreeMap<String, super::SymbolLocation>,
    pub citations: std::collections::BTreeMap<String, super::SymbolLocation>,
    pub bibliography_state: crate::bibliography::api::BibliographyState,
    pub navigation: super::NavigationState,
    pub index_state: super::IndexState,
}

impl DocumentStateView {
    pub fn from_document_state(state: &DocumentState) -> Self {
        Self {
            revision: state.revision,
            bibliography_dirty: state.bibliography_dirty,
            source_files: state.source_files.clone(),
            labels: state.labels.clone(),
            citations: state.citations.clone(),
            bibliography_state: state.bibliography_state.clone(),
            navigation: state.navigation.clone(),
            index_state: state.index_state.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilationSnapshot {
    pub pass_number: u32,
    pub primary_input: PathBuf,
    pub jobname: String,
    pub confirmed_registers: RegisterBankView,
    pub confirmed_commands: CommandRegistryView,
    pub confirmed_environments: EnvironmentRegistryView,
    pub confirmed_document_state: DocumentStateView,
}

impl CompilationSnapshot {
    pub fn from_session(session: &CompilationSession<'_>) -> Self {
        Self::derive_snapshot(
            session,
            &RegisterStore::default(),
            &session.context.job.document_state,
        )
    }

    pub fn derive_snapshot(
        session: &CompilationSession<'_>,
        registers: &RegisterStore,
        document_state: &DocumentState,
    ) -> Self {
        Self {
            pass_number: session.pass_number,
            primary_input: session.context.job.primary_input.clone(),
            jobname: session.context.job.jobname.clone(),
            confirmed_registers: RegisterBankView::from_register_store(registers),
            confirmed_commands: CommandRegistryView,
            confirmed_environments: EnvironmentRegistryView,
            confirmed_document_state: DocumentStateView::from_document_state(document_state),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{CompilationSnapshot, DocumentStateView, RegisterBankView};
    use crate::compilation::CompilationJob;
    use crate::compilation::DocumentState;
    use crate::parser::{CatCode, CompatIntRegister, RegisterStore, Token, TokenKind};
    use crate::policy::{ExecutionPolicy, OutputArtifactRegistry, PreviewPublicationPolicy};

    #[test]
    fn derives_frozen_snapshot_from_registers_and_document_state() {
        let job = test_job();
        let session = job.begin_pass(2);
        let mut registers = RegisterStore::default();
        registers.set_count(3, 41, false);
        registers.set_dimen(4, 12, false);
        registers.set_skip(5, 18, false);
        registers.set_muskip(6, 24, false);
        registers.set_toks(7, vec![char_token('x')], false);
        registers.set_compat_int(CompatIntRegister::PdfOutput, 0, false);

        let document_state = DocumentState {
            revision: 9,
            bibliography_dirty: true,
            source_files: vec!["main.tex".to_string()],
            labels: [(
                "sec:intro".to_string(),
                crate::compilation::SymbolLocation {
                    file: "main.tex".to_string(),
                    line: 3,
                    column: 1,
                },
            )]
            .into_iter()
            .collect(),
            citations: [(
                "knuth84".to_string(),
                crate::compilation::SymbolLocation {
                    file: "refs.bib".to_string(),
                    line: 12,
                    column: 2,
                },
            )]
            .into_iter()
            .collect(),
            ..DocumentState::default()
        };

        let snapshot = CompilationSnapshot::derive_snapshot(&session, &registers, &document_state);

        assert_eq!(snapshot.pass_number, 2);
        assert_eq!(snapshot.primary_input, PathBuf::from("src/main.tex"));
        assert_eq!(snapshot.jobname, "main");
        assert_eq!(
            snapshot.confirmed_registers,
            RegisterBankView {
                counts: [(3, 41)].into_iter().collect(),
                dimens: [(4, 12)].into_iter().collect(),
                skips: [(5, 18)].into_iter().collect(),
                muskips: [(6, 24)].into_iter().collect(),
                toks: [(7, vec![char_token('x')])].into_iter().collect(),
                compat_ints: [(CompatIntRegister::PdfOutput, 0)].into_iter().collect(),
            }
        );
        assert_eq!(
            snapshot.confirmed_document_state,
            DocumentStateView::from_document_state(&document_state)
        );
    }

    #[test]
    fn snapshot_is_isolated_from_later_mutations() {
        let job = test_job();
        let session = job.begin_pass(1);
        let mut registers = RegisterStore::default();
        let mut document_state = DocumentState::default();
        registers.set_count(1, 7, false);
        document_state.source_files.push("main.tex".to_string());

        let snapshot = CompilationSnapshot::derive_snapshot(&session, &registers, &document_state);

        registers.set_count(1, 99, false);
        document_state.source_files.push("appendix.tex".to_string());

        assert_eq!(snapshot.confirmed_registers.counts.get(&1), Some(&7));
        assert_eq!(
            snapshot.confirmed_document_state.source_files,
            vec!["main.tex".to_string()]
        );
    }

    fn test_job() -> CompilationJob {
        CompilationJob {
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
        }
    }

    fn char_token(value: char) -> Token {
        Token {
            kind: TokenKind::CharToken {
                char: value,
                cat: CatCode::Other,
            },
            line: 1,
            column: 1,
        }
    }
}
