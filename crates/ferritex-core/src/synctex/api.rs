use serde::{Deserialize, Serialize};

use crate::kernel::api::{DimensionValue, SourceLocation, SourceSpan};

const SCALED_POINTS_PER_POINT: i64 = 65_536;
const LEFT_MARGIN_PT: i64 = 72;
const DEFAULT_CHAR_WIDTH_PT: i64 = 6;
const DEFAULT_LINE_TOP_OFFSET_PT: i64 = 10;
const DEFAULT_LINE_DESCENT_PT: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedTextNode {
    pub text: String,
    pub source_span: SourceSpan,
    pub page: u32,
    pub x_start: DimensionValue,
    pub x_end: DimensionValue,
    pub y_bottom: DimensionValue,
    pub y_top: DimensionValue,
}

impl PlacedTextNode {
    pub fn from_text_line(
        text: String,
        source_span: SourceSpan,
        page: u32,
        y: DimensionValue,
    ) -> Self {
        let (x_start, x_end, y_bottom, y_top) = placed_text_node_bounds(text.chars().count(), y);
        Self {
            text,
            source_span,
            page,
            x_start,
            x_end,
            y_bottom,
            y_top,
        }
    }
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
        let mut cursor = SourceCursor::default();

        for (page_index, page) in pages.iter().enumerate() {
            for line in &page.lines {
                if line.text.trim().is_empty() {
                    continue;
                }

                for rendered_fragment in rendered_fragments(&line.text) {
                    if let Some(source_fragment) =
                        match_rendered_fragment(&source_lines, &mut cursor, &rendered_fragment)
                    {
                        let file_id = file_id_for(&mut files, &source_fragment.file);
                        fragments.push(fragment_for_rendered_fragment(
                            page_index as u32 + 1,
                            line,
                            &rendered_fragment,
                            file_id,
                            source_fragment,
                        ));
                    }
                }
            }
        }

        Self { files, fragments }
    }

    pub fn build_from_placed_nodes(nodes: Vec<PlacedTextNode>) -> Self {
        if nodes.is_empty() {
            return Self::default();
        }

        let max_file_id = nodes
            .iter()
            .map(|node| {
                node.source_span
                    .start
                    .file_id
                    .max(node.source_span.end.file_id)
            })
            .max()
            .unwrap_or(0);
        let files = (0..=max_file_id).map(|_| String::new()).collect();
        let fragments = nodes
            .into_iter()
            .map(fragment_for_placed_text_node)
            .collect();

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
    visible_chars: Vec<VisibleSourceChar>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisibleSourceChar {
    ch: char,
    column: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SourceCursor {
    line_index: usize,
    visible_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderedTextFragment {
    text: String,
    start_char: usize,
    end_char: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MatchedSourceFragment {
    file: String,
    line: u32,
    start_column: u32,
    end_column: u32,
}

fn prepare_source_lines(source_lines: &[SourceLineTrace]) -> Vec<PreparedSourceLine> {
    let mut prepared = source_lines
        .iter()
        .map(|line| {
            let visible_chars = visible_source_chars(&line.text);
            PreparedSourceLine {
                file: line.file.clone(),
                line: line.line,
                text: line.text.clone(),
                visible_chars,
            }
        })
        .filter(|line| !line.visible_chars.is_empty())
        .collect::<Vec<_>>();

    if prepared.is_empty() {
        prepared.push(PreparedSourceLine {
            file: "unknown".to_string(),
            line: 1,
            text: String::new(),
            visible_chars: Vec::new(),
        });
    }

    prepared
}

fn rendered_fragments(text: &str) -> Vec<RenderedTextFragment> {
    let mut fragments = Vec::new();
    let mut current_start = None;
    let mut current_text = String::new();

    for (char_index, ch) in text.chars().enumerate() {
        if ch.is_whitespace() {
            if let Some(start_char) = current_start.take() {
                fragments.push(RenderedTextFragment {
                    text: std::mem::take(&mut current_text),
                    start_char,
                    end_char: char_index,
                });
            }
            continue;
        }

        current_start.get_or_insert(char_index);
        current_text.push(ch);
    }

    if let Some(start_char) = current_start {
        fragments.push(RenderedTextFragment {
            text: current_text,
            start_char,
            end_char: text.chars().count(),
        });
    }

    fragments
}

fn match_rendered_fragment(
    source_lines: &[PreparedSourceLine],
    cursor: &mut SourceCursor,
    fragment: &RenderedTextFragment,
) -> Option<MatchedSourceFragment> {
    let mut fragment_chars = normalized_chars(&fragment.text);
    if fragment_chars.is_empty() {
        return None;
    }

    if let Some(matched) = match_fragment_chars(source_lines, cursor, &fragment_chars) {
        return Some(matched);
    }

    if fragment_chars.last() == Some(&'-') {
        fragment_chars.pop();
        if let Some(matched) = match_fragment_chars(source_lines, cursor, &fragment_chars) {
            return Some(matched);
        }
    }

    fallback_source_fragment(source_lines, cursor)
}

fn match_fragment_chars(
    source_lines: &[PreparedSourceLine],
    cursor: &mut SourceCursor,
    fragment_chars: &[char],
) -> Option<MatchedSourceFragment> {
    for (line_index, line) in source_lines.iter().enumerate().skip(cursor.line_index) {
        let search_start = if line_index == cursor.line_index {
            cursor.visible_index.min(line.visible_chars.len())
        } else {
            0
        };
        if let Some((start_index, end_index)) =
            find_fragment_in_line(line, search_start, fragment_chars)
        {
            let start_column = line.visible_chars[start_index].column;
            let end_column = line.visible_chars[end_index - 1].column + 1;
            cursor.line_index = line_index;
            cursor.visible_index = end_index;
            return Some(MatchedSourceFragment {
                file: line.file.clone(),
                line: line.line,
                start_column,
                end_column,
            });
        }
    }

    None
}

fn find_fragment_in_line(
    line: &PreparedSourceLine,
    search_start: usize,
    fragment_chars: &[char],
) -> Option<(usize, usize)> {
    if fragment_chars.is_empty() {
        return None;
    }

    let max_start = line.visible_chars.len().checked_sub(fragment_chars.len())?;
    for start in search_start..=max_start {
        let matches = fragment_chars
            .iter()
            .enumerate()
            .all(|(offset, expected)| line.visible_chars[start + offset].ch == *expected);
        if matches {
            return Some((start, start + fragment_chars.len()));
        }
    }

    None
}

fn fallback_source_fragment(
    source_lines: &[PreparedSourceLine],
    cursor: &mut SourceCursor,
) -> Option<MatchedSourceFragment> {
    for (line_index, line) in source_lines.iter().enumerate().skip(cursor.line_index) {
        if line.visible_chars.is_empty() {
            continue;
        }
        cursor.line_index = line_index;
        cursor.visible_index = line.visible_chars.len();
        return Some(MatchedSourceFragment {
            file: line.file.clone(),
            line: line.line,
            start_column: line.visible_chars.first()?.column,
            end_column: line.visible_chars.last()?.column + 1,
        });
    }

    None
}

fn fragment_for_rendered_fragment(
    page: u32,
    line: &RenderedLineTrace,
    rendered_fragment: &RenderedTextFragment,
    file_id: u32,
    source_fragment: MatchedSourceFragment,
) -> SyncTraceFragment {
    let x_start =
        points(LEFT_MARGIN_PT + rendered_fragment.start_char as i64 * DEFAULT_CHAR_WIDTH_PT);
    let x_end = points(LEFT_MARGIN_PT + rendered_fragment.end_char as i64 * DEFAULT_CHAR_WIDTH_PT);
    let y_bottom = line.y - points(DEFAULT_LINE_DESCENT_PT);
    let y_top = line.y + points(DEFAULT_LINE_TOP_OFFSET_PT);

    SyncTraceFragment {
        span: SourceSpan {
            start: SourceLocation {
                file_id,
                line: source_fragment.line,
                column: source_fragment.start_column,
            },
            end: SourceLocation {
                file_id,
                line: source_fragment.line,
                column: source_fragment.end_column,
            },
        },
        page,
        x_start,
        x_end,
        y_bottom,
        y_top,
        text: rendered_fragment.text.clone(),
    }
}

fn fragment_for_placed_text_node(node: PlacedTextNode) -> SyncTraceFragment {
    SyncTraceFragment {
        span: node.source_span,
        page: node.page,
        x_start: node.x_start,
        x_end: node.x_end,
        y_bottom: node.y_bottom,
        y_top: node.y_top,
        text: node.text,
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
        && fragment.y_bottom < position.y
        && position.y <= fragment.y_top
}

fn points(value: i64) -> DimensionValue {
    DimensionValue(value * SCALED_POINTS_PER_POINT)
}

fn placed_text_node_bounds(
    char_count: usize,
    y: DimensionValue,
) -> (
    DimensionValue,
    DimensionValue,
    DimensionValue,
    DimensionValue,
) {
    let x_start = points(LEFT_MARGIN_PT);
    let x_end = points(LEFT_MARGIN_PT + char_count as i64 * DEFAULT_CHAR_WIDTH_PT);
    let y_bottom = y - points(DEFAULT_LINE_DESCENT_PT);
    let y_top = y + points(DEFAULT_LINE_TOP_OFFSET_PT);
    (x_start, x_end, y_bottom, y_top)
}

fn normalized_chars(text: &str) -> Vec<char> {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn visible_source_chars(text: &str) -> Vec<VisibleSourceChar> {
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
            return Vec::new();
        }
    }

    let mut visible = Vec::new();
    let mut chars = text.chars().peekable();
    let mut column = 1u32;

    while let Some(ch) = chars.next() {
        match ch {
            '%' => break,
            '\\' => {
                column += 1;
                while chars.peek().is_some_and(|next| next.is_alphabetic()) {
                    chars.next();
                    column += 1;
                }
            }
            '{' | '}' | '[' | ']' => {
                column += 1;
            }
            _ if !ch.is_control() => {
                if !ch.is_whitespace() {
                    visible.push(VisibleSourceChar { ch, column });
                }
                column += 1;
            }
            _ => {
                column += 1;
            }
        }
    }

    visible
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
        points, PdfPosition, PlacedTextNode, RenderedLineTrace, RenderedPageTrace, SourceLineTrace,
        SyncTexData,
    };
    use crate::kernel::api::{SourceLocation, SourceSpan};

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

    #[test]
    fn build_line_based_emits_column_precise_fragments() {
        let document = simple_document(vec![text_line("Hello world", 700)]);
        let data = SyncTexData::build_line_based(
            &document,
            &[SourceLineTrace {
                file: "/tmp/main.tex".to_string(),
                line: 3,
                text: "Hello world".to_string(),
            }],
        );

        assert_eq!(data.fragments.len(), 2);
        assert_eq!(data.fragments[0].text, "Hello");
        assert_eq!(data.fragments[0].span.start.column, 1);
        assert_eq!(data.fragments[0].span.end.column, 6);
        assert_eq!(data.fragments[1].text, "world");
        assert_eq!(data.fragments[1].span.start.column, 7);
        assert_eq!(data.fragments[1].span.end.column, 12);

        let positions = data.forward_search(SourceLocation {
            file_id: 0,
            line: 3,
            column: 8,
        });
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].x, points(108));
    }

    #[test]
    fn build_line_based_keeps_wrapped_fragments_on_same_source_line() {
        let document = simple_document(vec![text_line("Hello", 700), text_line("world", 682)]);
        let data = SyncTexData::build_line_based(
            &document,
            &[SourceLineTrace {
                file: "/tmp/main.tex".to_string(),
                line: 3,
                text: "Hello world".to_string(),
            }],
        );

        assert_eq!(data.fragments.len(), 2);
        assert_eq!(data.fragments[0].span.start.line, 3);
        assert_eq!(data.fragments[0].span.start.column, 1);
        assert_eq!(data.fragments[0].span.end.column, 6);
        assert_eq!(data.fragments[1].span.start.line, 3);
        assert_eq!(data.fragments[1].span.start.column, 7);
        assert_eq!(data.fragments[1].span.end.column, 12);
        assert_eq!(data.fragments[1].y_top, points(692));
    }

    #[test]
    fn build_from_placed_nodes_builds_fragments_from_explicit_spans() {
        let span = SourceSpan {
            start: SourceLocation {
                file_id: 0,
                line: 3,
                column: 1,
            },
            end: SourceLocation {
                file_id: 0,
                line: 3,
                column: 12,
            },
        };
        let data = SyncTexData::build_from_placed_nodes(vec![PlacedTextNode::from_text_line(
            "Hello world".to_string(),
            span,
            2,
            points(700),
        )]);

        assert_eq!(data.files, vec![String::new()]);
        assert_eq!(data.fragments.len(), 1);
        assert_eq!(data.fragments[0].span, span);
        assert_eq!(data.fragments[0].page, 2);
        assert_eq!(data.fragments[0].x_start, points(72));
        assert_eq!(data.fragments[0].x_end, points(138));
    }

    #[test]
    fn build_from_placed_nodes_keeps_wrapped_fragments_on_same_source_span() {
        let span = SourceSpan {
            start: SourceLocation {
                file_id: 0,
                line: 3,
                column: 1,
            },
            end: SourceLocation {
                file_id: 0,
                line: 3,
                column: 12,
            },
        };
        let data = SyncTexData::build_from_placed_nodes(vec![
            PlacedTextNode::from_text_line("Hello".to_string(), span, 1, points(700)),
            PlacedTextNode::from_text_line("world".to_string(), span, 1, points(682)),
        ]);

        let forward = data.forward_search(SourceLocation {
            file_id: 0,
            line: 3,
            column: 8,
        });
        assert_eq!(forward.len(), 2);
        assert_eq!(forward[0].page, 1);
        assert_eq!(forward[1].page, 1);

        let inverse = data.inverse_search(PdfPosition {
            page: 1,
            x: points(72),
            y: points(687),
        });
        assert_eq!(inverse, Some(span));
    }

    #[test]
    fn build_from_placed_nodes_preserves_source_span_for_split_text() {
        let span = SourceSpan {
            start: SourceLocation {
                file_id: 1,
                line: 12,
                column: 5,
            },
            end: SourceLocation {
                file_id: 1,
                line: 12,
                column: 23,
            },
        };
        let data = SyncTexData::build_from_placed_nodes(vec![
            PlacedTextNode::from_text_line("Split".to_string(), span, 2, points(700)),
            PlacedTextNode::from_text_line("text".to_string(), span, 2, points(682)),
        ]);

        assert_eq!(data.fragments.len(), 2);
        assert!(data.fragments.iter().all(|fragment| fragment.span == span));
        assert_eq!(
            data.forward_search(SourceLocation {
                file_id: 1,
                line: 12,
                column: 10,
            })
            .len(),
            2
        );
    }
}
