use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::bibliography::api::BibliographyState;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SymbolLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PdfMetadataDraft {
    pub title: Option<String>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LinkStyle {
    pub color_links: bool,
    pub link_color: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NavigationState {
    pub metadata: PdfMetadataDraft,
    pub outline_entries: Vec<OutlineDraftEntry>,
    #[serde(default)]
    pub named_destinations: BTreeMap<String, DestinationAnchor>,
    pub default_link_style: LinkStyle,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct IndexEntry {
    pub sort_key: String,
    pub display: String,
    pub page: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct IndexState {
    pub enabled: bool,
    pub entries: Vec<IndexEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineDraftEntry {
    pub level: u8,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestinationAnchor {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DocumentState {
    pub revision: u64,
    pub bibliography_dirty: bool,
    pub source_files: Vec<String>,
    pub labels: BTreeMap<String, SymbolLocation>,
    pub citations: BTreeMap<String, SymbolLocation>,
    #[serde(default)]
    pub bibliography_state: BibliographyState,
    #[serde(default)]
    pub navigation: NavigationState,
    #[serde(default)]
    pub index_state: IndexState,
}
