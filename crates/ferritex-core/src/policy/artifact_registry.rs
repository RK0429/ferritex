use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::normalize_path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactKind {
    Auxiliary,
    Bibliography,
    Pdf,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputArtifactRecord {
    pub normalized_path: PathBuf,
    pub produced_path: PathBuf,
    pub primary_input: PathBuf,
    pub kind: ArtifactKind,
    pub jobname: String,
    pub produced_pass: u32,
}

impl OutputArtifactRecord {
    pub fn new(
        produced_path: impl AsRef<Path>,
        primary_input: impl AsRef<Path>,
        jobname: impl Into<String>,
        kind: ArtifactKind,
        produced_pass: u32,
    ) -> Self {
        let produced_path = produced_path.as_ref().to_path_buf();
        let primary_input = primary_input.as_ref().to_path_buf();

        Self {
            normalized_path: normalize_path(&produced_path),
            produced_path,
            primary_input,
            kind,
            jobname: jobname.into(),
            produced_pass,
        }
    }
}

/// 出力アーティファクトの in-memory レジストリ
#[derive(Debug, Clone)]
pub struct OutputArtifactRegistry {
    active: bool,
    records: HashMap<PathBuf, OutputArtifactRecord>,
}

impl OutputArtifactRegistry {
    pub fn new() -> Self {
        Self {
            active: true,
            records: HashMap::new(),
        }
    }

    pub fn record(&mut self, mut record: OutputArtifactRecord) {
        record.normalized_path = normalize_path(&record.produced_path);
        record.primary_input = normalize_path(&record.primary_input);
        self.active = true;
        self.records.insert(record.normalized_path.clone(), record);
    }

    pub fn allow_readback(
        &self,
        path: impl AsRef<Path>,
        primary_input: impl AsRef<Path>,
        jobname: &str,
        _artifact_root: impl AsRef<Path>,
    ) -> bool {
        if !self.active {
            return false;
        }

        let normalized_path = normalize_path(path.as_ref());
        let normalized_primary_input = normalize_path(primary_input.as_ref());

        self.records.get(&normalized_path).is_some_and(|record| {
            record.primary_input == normalized_primary_input && record.jobname == jobname
        })
    }

    pub fn invalidate(&mut self) {
        self.active = false;
        self.records.clear();
    }
}

impl Default for OutputArtifactRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{ArtifactKind, OutputArtifactRecord, OutputArtifactRegistry};

    fn fixture_root() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before unix epoch")
            .as_nanos();

        std::env::temp_dir().join(format!("ferritex-artifacts-{unique}"))
    }

    fn sample_paths() -> (PathBuf, PathBuf, PathBuf) {
        let root = fixture_root();
        let primary_input = root.join("src/main.tex");
        let artifact_root = root.join("out");
        let produced = artifact_root.join("main.aux");

        (primary_input, artifact_root, produced)
    }

    #[test]
    fn recorded_artifacts_allow_same_job_readback() {
        let (primary_input, artifact_root, produced) = sample_paths();
        let mut registry = OutputArtifactRegistry::new();
        registry.record(OutputArtifactRecord::new(
            &produced,
            &primary_input,
            "main",
            ArtifactKind::Auxiliary,
            1,
        ));

        assert!(registry.allow_readback(&produced, &primary_input, "main", &artifact_root));
    }

    #[test]
    fn different_jobname_is_rejected() {
        let (primary_input, artifact_root, produced) = sample_paths();
        let mut registry = OutputArtifactRegistry::new();
        registry.record(OutputArtifactRecord::new(
            &produced,
            &primary_input,
            "main",
            ArtifactKind::Auxiliary,
            3,
        ));

        assert!(!registry.allow_readback(&produced, &primary_input, "other", &artifact_root));
    }

    #[test]
    fn different_primary_input_is_rejected() {
        let (primary_input, artifact_root, produced) = sample_paths();
        let mut registry = OutputArtifactRegistry::new();
        registry.record(OutputArtifactRecord::new(
            &produced,
            &primary_input,
            "main",
            ArtifactKind::Auxiliary,
            2,
        ));
        let other_primary_input = primary_input.with_file_name("appendix.tex");

        assert!(!registry.allow_readback(&produced, &other_primary_input, "main", &artifact_root));
    }

    #[test]
    fn invalidate_revokes_existing_authority() {
        let (primary_input, artifact_root, produced) = sample_paths();
        let mut registry = OutputArtifactRegistry::new();
        registry.record(OutputArtifactRecord::new(
            &produced,
            &primary_input,
            "main",
            ArtifactKind::Auxiliary,
            1,
        ));

        registry.invalidate();

        assert!(!registry.allow_readback(&produced, &primary_input, "main", &artifact_root));
    }

    #[test]
    fn pre_generated_bbl_is_rejected_without_record() {
        let (primary_input, artifact_root, _) = sample_paths();
        let registry = OutputArtifactRegistry::new();
        let bbl = artifact_root.join("main.bbl");

        assert!(!registry.allow_readback(&bbl, &primary_input, "main", &artifact_root));
    }

    #[test]
    fn pre_generated_bbl_metadata_is_rejected_without_record() {
        let (primary_input, artifact_root, _) = sample_paths();
        let registry = OutputArtifactRegistry::new();
        let bbl_metadata = artifact_root.join("main.bbl.ferritex.json");

        assert!(!registry.allow_readback(&bbl_metadata, &primary_input, "main", &artifact_root));
    }

    #[test]
    fn matching_bbl_name_outside_artifact_root_is_rejected() {
        let (primary_input, artifact_root, _) = sample_paths();
        let registry = OutputArtifactRegistry::new();
        let escaped = artifact_root.join("../main.bbl");

        assert!(!registry.allow_readback(&escaped, &primary_input, "main", &artifact_root));
    }
}
