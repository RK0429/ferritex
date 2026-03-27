use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionOutlineEntry {
    pub level: u8,
    pub number: String,
    pub title: String,
}

impl SectionOutlineEntry {
    pub fn display_title(&self) -> String {
        match (self.number.is_empty(), self.title.is_empty()) {
            (true, true) => String::new(),
            (false, true) => self.number.clone(),
            (true, false) => self.title.clone(),
            (false, false) => format!("{} {}", self.number, self.title),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PartitionKind {
    Document,
    Chapter,
    Section,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionLocator {
    pub entry_file: PathBuf,
    pub level: u8,
    pub ordinal: usize,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentWorkUnit {
    pub partition_id: String,
    pub kind: PartitionKind,
    pub locator: PartitionLocator,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentPartitionPlan {
    pub fallback_partition_id: String,
    pub work_units: Vec<DocumentWorkUnit>,
}

impl DocumentPartitionPlan {
    pub fn fallback_partition_id_for(primary_input: &Path) -> String {
        let stem = primary_input
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .map(slugify_partition_title)
            .unwrap_or_else(|| "root".to_string());
        format!("document:0000:{stem}")
    }
}

impl Default for DocumentPartitionPlan {
    fn default() -> Self {
        Self {
            fallback_partition_id: "document:0000:root".to_string(),
            work_units: Vec::new(),
        }
    }
}

pub fn slugify_partition_title(value: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "untitled".to_string()
    } else {
        slug.to_string()
    }
}
