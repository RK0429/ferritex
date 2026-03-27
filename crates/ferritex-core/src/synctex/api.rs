use serde::{Deserialize, Serialize};

use crate::kernel::api::{DimensionValue, SourceLocation, SourceSpan};

const SCALED_POINTS_PER_POINT: i64 = 65_536;
const LEFT_MARGIN_PT: i64 = 72;
const DEFAULT_CHAR_WIDTH_PT: i64 = 6;
const DEFAULT_LINE_TOP_OFFSET_PT: i64 = 10;
const DEFAULT_LINE_DESCENT_PT: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLineTrace {
    pub file: String,
    pub line: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedLineTrace {
    pub text: String,
    pub y: DimensionValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderedPageTrace {
    pub lines: Vec<RenderedLineTrace>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfPosition {
    pub page: u32,
    pub x: DimensionValue,
    pub y: DimensionValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncTraceFragment {
    pub span: SourceSpan,
    pub page: u32,
    pub x_start: DimensionValue,
    pub x_end: DimensionValue,
    pub y_bottom: DimensionValue,
    pub y_top: DimensionValue,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SyncTexData {
    pub files: Vec<String>,
    pub fragments: Vec<SyncTraceFragment>,
}

impl SyncTexData {
    pub fn build_line_based(pages: &[RenderedPageTrace], source_lines: &[SourceLineTrace]) -> Self {
        let source_lines = prepare_source_lines(source_lines);
        if source_lines.is_empty() {
            return Self::default();
        }

        let mut files = Vec::new();
        let mut fragments = Vec::new();
        let mut source_index = 0usize;
        let mut remaining_budget = source_lines[0].budget;

        for (page_index, page) in pages.iter().enumerate() {
            for line in &page.lines {
                if line.text.trim().is_empty() {
                    continue;
                }

                source_index = advance_to_best_source_line(
                    &source_lines,
                    source_index,
                    &line.text,
                    remaining_budget,
                );
                remaining_budget = source_lines[source_index].budget;
                let source_line = &source_lines[source_index];
                let file_id = file_id_for(&mut files, &source_line.file);
                fragments.push(fragment_for_line(
                    page_index as u32 + 1,
                    line,
                    file_id,
                    source_line,
                ));

                let consumed = normalized_text(&line.text).chars().count().max(1);
                remaining_budget = remaining_budget.saturating_sub(consumed);
                if remaining_budget == 0 && source_index + 1 < source_lines.len() {
                    source_index += 1;
                    remaining_budget = source_lines[source_index].budget;
                }
            }
        }

        Self { files, fragments }
    }

    pub fn forward_search(&self, location: SourceLocation) -> Vec<PdfPosition> {
        self.fragments
            .iter()
            .filter(|fragment| contains_location(fragment.span, location))
            .map(|fragment| PdfPosition {
                page: fragment.page,
                x: fragment.x_start,
                y: fragment.y_top,
            })
            .collect()
    }

    pub fn inverse_search(&self, position: PdfPosition) -> Option<SourceSpan> {
        self.fragments
            .iter()
            .find(|fragment| contains_pdf_position(fragment, position))
            .map(|fragment| fragment.span)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedSourceLine {
    file: String,
    line: u32,
    text: String,
    normalized: String,
    budget: usize,
}

fn prepare_source_lines(source_lines: &[SourceLineTrace]) -> Vec<PreparedSourceLine> {
    let mut prepared = source_lines
        .iter()
        .map(|line| {
            let text = visible_text_hint(&line.text);
            let normalized = normalized_text(&text);
            PreparedSourceLine {
                file: line.file.clone(),
                line: line.line,
                text: line.text.clone(),
                budget: normalized.chars().count().max(1),
                normalized,
            }
        })
        .filter(|line| !line.normalized.is_empty())
        .collect::<Vec<_>>();

    if prepared.is_empty() {
        prepared.push(PreparedSourceLine {
            file: "unknown".to_string(),
            line: 1,
            text: String::new(),
            normalized: String::new(),
            budget: 1,
        });
    }

    prepared
}

fn advance_to_best_source_line(
    source_lines: &[PreparedSourceLine],
    current_index: usize,
    output_text: &str,
    remaining_budget: usize,
) -> usize {
    if current_index + 1 >= source_lines.len() {
        return current_index;
    }

    let normalized_output = normalized_text(output_text);
    if normalized_output.is_empty() {
        return current_index;
    }

    let current = &source_lines[current_index];
    let next = &source_lines[current_index + 1];
    let current_matches = normalized_contains(&current.normalized, &normalized_output);
    let next_matches = normalized_contains(&next.normalized, &normalized_output);

    if !current_matches && next_matches {
        current_index + 1
    } else if remaining_budget == 0 {
        current_index + 1
    } else {
        current_index
    }
}

fn normalized_contains(haystack: &str, needle: &str) -> bool {
    !needle.is_empty() && (haystack.contains(needle) || needle.contains(haystack))
}

fn fragment_for_line(
    page: u32,
    line: &RenderedLineTrace,
    file_id: u32,
    source_line: &PreparedSourceLine,
) -> SyncTraceFragment {
    let x_start = points(LEFT_MARGIN_PT);
    let x_end = x_start + points((line.text.chars().count().max(1) as i64) * DEFAULT_CHAR_WIDTH_PT);
    let y_bottom = line.y - points(DEFAULT_LINE_DESCENT_PT);
    let y_top = line.y + points(DEFAULT_LINE_TOP_OFFSET_PT);
    let end_column = source_line.text.chars().count().max(1) as u32 + 1;

    SyncTraceFragment {
        span: SourceSpan {
            start: SourceLocation {
                file_id,
                line: source_line.line,
                column: 1,
            },
            end: SourceLocation {
                file_id,
                line: source_line.line,
                column: end_column,
            },
        },
        page,
        x_start,
        x_end,
        y_bottom,
        y_top,
        text: line.text.clone(),
    }
}

fn file_id_for(files: &mut Vec<String>, file: &str) -> u32 {
    if let Some(index) = files.iter().position(|entry| entry == file) {
        index as u32
    } else {
        files.push(file.to_string());
        (files.len() - 1) as u32
    }
}

fn contains_location(span: SourceSpan, location: SourceLocation) -> bool {
    span.start.file_id == location.file_id
        && span.start.line <= location.line
        && location.line <= span.end.line
        && span.start.column <= location.column
        && location.column <= span.end.column
}

fn contains_pdf_position(fragment: &SyncTraceFragment, position: PdfPosition) -> bool {
    fragment.page == position.page
        && fragment.x_start <= position.x
        && position.x <= fragment.x_end
        && fragment.y_bottom <= position.y
        && position.y <= fragment.y_top
}

fn points(value: i64) -> DimensionValue {
    DimensionValue(value * SCALED_POINTS_PER_POINT)
}

fn normalized_text(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn visible_text_hint(text: &str) -> String {
    let trimmed = text.trim_start();
    if let Some(command) = leading_control_word(trimmed) {
        if matches!(
            command,
            "documentclass"
                | "begin"
                | "end"
                | "usepackage"
                | "RequirePackage"
                | "InputIfFileExists"
                | "input"
                | "include"
                | "bibliography"
                | "tableofcontents"
                | "listoffigures"
                | "listoftables"
                | "makeindex"
                | "printindex"
                | "hypersetup"
        ) {
            return String::new();
        }
    }

    let mut visible = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '%' => break,
            '\\' => {
                while chars.peek().is_some_and(|next| next.is_alphabetic()) {
                    chars.next();
                }
            }
            '{' | '}' | '[' | ']' => {}
            _ if !ch.is_control() => visible.push(ch),
            _ => {}
        }
    }

    visible.trim().to_string()
}

fn leading_control_word(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('\\')?;
    let end = rest
        .find(|ch: char| !ch.is_alphabetic())
        .unwrap_or(rest.len());
    (end > 0).then_some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::{
        points, PdfPosition, RenderedLineTrace, RenderedPageTrace, SourceLineTrace, SyncTexData,
    };
    use crate::kernel::api::SourceLocation;

    fn text_line(text: &str, y_pt: i64) -> RenderedLineTrace {
        RenderedLineTrace {
            text: text.to_string(),
            y: points(y_pt),
        }
    }

    fn simple_document(lines: Vec<RenderedLineTrace>) -> Vec<RenderedPageTrace> {
        vec![RenderedPageTrace { lines }]
    }

    #[test]
    fn build_line_based_supports_forward_and_inverse_search() {
        let document = simple_document(vec![text_line("Hello world", 700)]);
        let data = SyncTexData::build_line_based(
            &document,
            &[SourceLineTrace {
                file: "/tmp/main.tex".to_string(),
                line: 3,
                text: "Hello world".to_string(),
            }],
        );

        assert_eq!(data.files, vec!["/tmp/main.tex".to_string()]);
        let forward = data.forward_search(SourceLocation {
            file_id: 0,
            line: 3,
            column: 1,
        });
        assert_eq!(forward.len(), 1);
        assert_eq!(forward[0].page, 1);

        let inverse = data.inverse_search(PdfPosition {
            page: 1,
            x: points(72),
            y: points(705),
        });
        assert_eq!(inverse.map(|span| span.start.line), Some(3));
    }

    #[test]
    fn build_line_based_preserves_file_order_for_multiple_sources() {
        let document = simple_document(vec![text_line("Intro", 700), text_line("Included", 682)]);
        let data = SyncTexData::build_line_based(
            &document,
            &[
                SourceLineTrace {
                    file: "/tmp/main.tex".to_string(),
                    line: 1,
                    text: "\\section{Intro}".to_string(),
                },
                SourceLineTrace {
                    file: "/tmp/chapter.tex".to_string(),
                    line: 1,
                    text: "Included".to_string(),
                },
            ],
        );

        assert_eq!(
            data.files,
            vec!["/tmp/main.tex".to_string(), "/tmp/chapter.tex".to_string()]
        );
        assert_eq!(data.fragments.len(), 2);
        assert_eq!(data.fragments[0].span.start.file_id, 0);
        assert_eq!(data.fragments[1].span.start.file_id, 1);
    }
}
