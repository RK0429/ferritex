use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SymbolLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DocumentState {
    pub revision: u64,
    pub bibliography_dirty: bool,
    pub source_files: Vec<String>,
    pub labels: BTreeMap<String, SymbolLocation>,
    pub citations: BTreeMap<String, SymbolLocation>,
}
