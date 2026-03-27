use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BibliographyState {
    pub citations: CitationTable,
    pub bbl: Option<BblSnapshot>,
}

impl BibliographyState {
    pub fn from_snapshot(snapshot: BblSnapshot) -> Self {
        let citations = snapshot
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let label = entry
                    .optional_label
                    .clone()
                    .unwrap_or_else(|| (index + 1).to_string());
                let key = entry.key.clone();
                (
                    key.clone(),
                    CitationInfo {
                        key,
                        label: label.clone(),
                        formatted_text: label,
                    },
                )
            })
            .collect();

        Self {
            citations: CitationTable { entries: citations },
            bbl: Some(snapshot),
        }
    }

    pub fn resolve_citation(&self, key: &str) -> Option<&CitationInfo> {
        self.citations.entries.get(key)
    }

    pub fn has_citations(&self) -> bool {
        !self.citations.entries.is_empty()
    }

    pub fn upsert_entry(&mut self, key: String, display_text: String) -> BibliographyEntry {
        let label = self
            .resolve_citation(&key)
            .map(|info| info.label.clone())
            .unwrap_or_else(|| self.next_label());
        let entry = BibliographyEntry {
            key: key.clone(),
            optional_label: None,
            rendered_block: render_bibliography_block(&label, &display_text),
        };

        self.citations.entries.insert(
            key.clone(),
            CitationInfo {
                key: key.clone(),
                label: label.clone(),
                formatted_text: label,
            },
        );

        let snapshot = self.bbl.get_or_insert_with(BblSnapshot::default);
        if let Some(existing) = snapshot
            .entries
            .iter_mut()
            .find(|existing| existing.key == key)
        {
            *existing = entry.clone();
        } else {
            snapshot.entries.push(entry.clone());
        }

        entry
    }

    fn next_label(&self) -> String {
        self.bbl
            .as_ref()
            .map(|snapshot| snapshot.entries.len() + 1)
            .unwrap_or_else(|| self.citations.entries.len() + 1)
            .to_string()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationTable {
    pub entries: BTreeMap<String, CitationInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationInfo {
    pub key: String,
    pub label: String,
    pub formatted_text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BblSnapshot {
    pub entries: Vec<BibliographyEntry>,
    pub input_fingerprint: Option<BibliographyInputFingerprint>,
    pub toolchain: Option<BibliographyToolchain>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BibliographyEntry {
    pub key: String,
    pub optional_label: Option<String>,
    pub rendered_block: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BibliographyInputFingerprint {
    pub hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BibliographyToolchain {
    Bibtex,
    Biber,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BibliographyDiagnostic {
    MissingBbl,
    StaleBbl { reason: String },
    UnresolvedCitation { key: String },
}

pub fn parse_bbl(input: &str) -> BblSnapshot {
    let body = extract_thebibliography_body(input).unwrap_or(input);
    let mut entries = Vec::new();
    let mut cursor = 0usize;

    while let Some(relative_start) = body[cursor..].find(r"\bibitem") {
        let command_start = cursor + relative_start;
        let mut index = command_start + r"\bibitem".len();
        skip_whitespace(body, &mut index);

        let optional_label = if body[index..].starts_with('[') {
            let Some((label, optional_end)) = read_bracket_value(body, index) else {
                break;
            };
            index = optional_end;
            skip_whitespace(body, &mut index);
            Some(label)
        } else {
            None
        };

        let Some((key, after_key)) = read_braced_value(body, index) else {
            if index >= body.len() {
                break;
            }
            cursor = index + 1;
            continue;
        };

        let next_entry = body[after_key..]
            .find(r"\bibitem")
            .map(|next| after_key + next);
        let environment_end = body[after_key..]
            .find(r"\end{thebibliography}")
            .map(|next| after_key + next);
        let entry_end = match (next_entry, environment_end) {
            (Some(next_entry), Some(environment_end)) => next_entry.min(environment_end),
            (Some(next_entry), None) => next_entry,
            (None, Some(environment_end)) => environment_end,
            (None, None) => body.len(),
        };

        let display_text = normalize_bibliography_text(&body[after_key..entry_end]);
        let label = optional_label
            .clone()
            .unwrap_or_else(|| (entries.len() + 1).to_string());
        entries.push(BibliographyEntry {
            key,
            optional_label,
            rendered_block: render_bibliography_block(&label, &display_text),
        });
        cursor = entry_end;
    }

    BblSnapshot {
        entries,
        input_fingerprint: None,
        toolchain: Some(BibliographyToolchain::Bibtex),
    }
}

fn render_bibliography_block(label: &str, display_text: &str) -> String {
    if display_text.is_empty() {
        format!("[{label}]")
    } else {
        format!("[{label}] {display_text}")
    }
}

fn extract_thebibliography_body(input: &str) -> Option<&str> {
    let begin = input.find(r"\begin{thebibliography}")?;
    let mut begin_argument_start = begin + r"\begin{thebibliography}".len();
    skip_whitespace(input, &mut begin_argument_start);
    let body_start = find_matching_delimiter(input, begin_argument_start, '{', '}')?;
    let body = &input[body_start..];
    let body_end = body
        .find(r"\end{thebibliography}")
        .map(|offset| body_start + offset)
        .unwrap_or_else(|| input.len());
    Some(&input[body_start..body_end])
}

fn find_matching_delimiter(input: &str, start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut escaped = false;

    for (offset, ch) in input[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        if ch == '\\' {
            escaped = true;
            continue;
        }

        if ch == open {
            depth += 1;
        } else if ch == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(start + offset + ch.len_utf8());
            }
        }
    }

    None
}

fn read_braced_value(input: &str, mut index: usize) -> Option<(String, usize)> {
    skip_whitespace(input, &mut index);
    if !input[index..].starts_with('{') {
        return None;
    }

    let end = find_matching_delimiter(input, index, '{', '}')?;
    let value = input[index + 1..end - 1].trim().to_string();
    (!value.is_empty()).then_some((value, end))
}

fn read_bracket_value(input: &str, mut index: usize) -> Option<(String, usize)> {
    skip_whitespace(input, &mut index);
    if !input[index..].starts_with('[') {
        return None;
    }

    let end = find_matching_delimiter(input, index, '[', ']')?;
    let value = input[index + 1..end - 1].trim().to_string();
    (!value.is_empty()).then_some((value, end))
}

fn normalize_bibliography_text(text: &str) -> String {
    text.replace(r"\newblock", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn skip_whitespace(input: &str, index: &mut usize) {
    while let Some(ch) = input[*index..].chars().next() {
        if !ch.is_whitespace() {
            break;
        }
        *index += ch.len_utf8();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_bbl, BblSnapshot, BibliographyDiagnostic, BibliographyEntry, BibliographyState,
        BibliographyToolchain,
    };

    #[test]
    fn parse_bbl_basic() {
        let snapshot = parse_bbl(
            "\\begin{thebibliography}{99}\n\\bibitem{knuth} Donald Knuth\n\\bibitem{lamport} Leslie Lamport\n\\end{thebibliography}\n",
        );

        assert_eq!(
            snapshot,
            BblSnapshot {
                entries: vec![
                    BibliographyEntry {
                        key: "knuth".to_string(),
                        optional_label: None,
                        rendered_block: "[1] Donald Knuth".to_string(),
                    },
                    BibliographyEntry {
                        key: "lamport".to_string(),
                        optional_label: None,
                        rendered_block: "[2] Leslie Lamport".to_string(),
                    },
                ],
                input_fingerprint: None,
                toolchain: Some(BibliographyToolchain::Bibtex),
            }
        );
    }

    #[test]
    fn parse_bbl_empty() {
        let snapshot = parse_bbl("\\begin{thebibliography}{99}\\end{thebibliography}");

        assert!(snapshot.entries.is_empty());
        assert_eq!(snapshot.toolchain, Some(BibliographyToolchain::Bibtex));
    }

    #[test]
    fn parse_bbl_with_explicit_labels() {
        let snapshot = parse_bbl(
            "\\begin{thebibliography}{99}\n\\bibitem[Knu84]{knuth} Donald Knuth\n\\bibitem{lamport} Leslie Lamport\n\\end{thebibliography}\n",
        );

        assert_eq!(snapshot.entries.len(), 2);
        assert_eq!(
            snapshot.entries[0].optional_label,
            Some("Knu84".to_string())
        );
        assert_eq!(snapshot.entries[0].rendered_block, "[Knu84] Donald Knuth");
        assert_eq!(snapshot.entries[1].optional_label, None);
        assert_eq!(snapshot.entries[1].rendered_block, "[2] Leslie Lamport");
    }

    #[test]
    fn parse_bbl_malformed() {
        let snapshot =
            parse_bbl("\\begin{thebibliography}{99}\n\\bibitem missing\n\\bibitem{ok} Fine\n");

        assert_eq!(
            snapshot.entries,
            vec![BibliographyEntry {
                key: "ok".to_string(),
                optional_label: None,
                rendered_block: "[1] Fine".to_string(),
            }]
        );
    }

    #[test]
    fn resolve_citation_found() {
        let state = BibliographyState::from_snapshot(parse_bbl(
            "\\begin{thebibliography}{99}\\bibitem{key} Reference\\end{thebibliography}",
        ));

        let citation = state.resolve_citation("key").expect("citation");
        assert_eq!(citation.key, "key");
        assert_eq!(citation.label, "1");
        assert_eq!(citation.formatted_text, "1");
    }

    #[test]
    fn from_snapshot_preserves_explicit_labels() {
        let snapshot = parse_bbl(
            "\\begin{thebibliography}{99}\\bibitem[Knu84]{knuth} Donald Knuth\\end{thebibliography}",
        );
        let state = BibliographyState::from_snapshot(snapshot);

        let citation = state.resolve_citation("knuth").expect("citation");
        assert_eq!(citation.label, "Knu84");
        assert_eq!(citation.formatted_text, "Knu84");
    }

    #[test]
    fn resolve_citation_missing() {
        let state = BibliographyState::from_snapshot(parse_bbl(
            "\\begin{thebibliography}{99}\\bibitem{key} Reference\\end{thebibliography}",
        ));

        assert_eq!(state.resolve_citation("missing"), None);
    }

    #[test]
    fn bibliography_diagnostic_variants_are_constructible() {
        assert_eq!(
            BibliographyDiagnostic::MissingBbl,
            BibliographyDiagnostic::MissingBbl
        );
    }
}
