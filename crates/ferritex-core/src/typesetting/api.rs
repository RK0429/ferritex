use crate::font::api::TfmMetrics;
use crate::kernel::api::DimensionValue;
use crate::parser::api::{DocumentNode, ParsedDocument};

const SCALED_POINTS_PER_POINT: i64 = 65_536;
const PAGE_WIDTH_PT: i64 = 612;
const PAGE_HEIGHT_PT: i64 = 792;
const TOP_MARGIN_PT: i64 = 72;
const LINE_HEIGHT_PT: i64 = 18;
const MAX_LINE_CHARS: usize = 70;
const MAX_LINE_WIDTH: DimensionValue =
    DimensionValue(MAX_LINE_CHARS as i64 * SCALED_POINTS_PER_POINT);
const LINES_PER_PAGE: usize = 36;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageBox {
    pub width: DimensionValue,
    pub height: DimensionValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextLine {
    pub text: String,
    pub y: DimensionValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypesetPage {
    pub lines: Vec<TextLine>,
    pub page_box: PageBox,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypesetDocument {
    pub pages: Vec<TypesetPage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HListItem {
    Char {
        codepoint: char,
        width: DimensionValue,
    },
    Glue {
        width: DimensionValue,
        stretch: DimensionValue,
        shrink: DimensionValue,
    },
    Penalty {
        value: i32,
    },
}

pub const PENALTY_FORBIDDEN: i32 = 10_000;
pub const PENALTY_FORCED: i32 = -10_000;

pub trait CharWidthProvider {
    fn char_width(&self, codepoint: char) -> DimensionValue;
    fn space_width(&self) -> DimensionValue;
}

#[derive(Debug, Clone, Copy)]
pub struct FixedWidthProvider {
    pub char_width: DimensionValue,
    pub space_width: DimensionValue,
}

impl CharWidthProvider for FixedWidthProvider {
    fn char_width(&self, _codepoint: char) -> DimensionValue {
        self.char_width
    }

    fn space_width(&self) -> DimensionValue {
        self.space_width
    }
}

pub struct TfmWidthProvider<'a> {
    pub metrics: &'a TfmMetrics,
    pub fallback_width: DimensionValue,
}

impl CharWidthProvider for TfmWidthProvider<'_> {
    fn char_width(&self, codepoint: char) -> DimensionValue {
        let code = codepoint as u32;
        if code <= u16::MAX as u32 {
            self.metrics
                .width(code as u16)
                .unwrap_or(self.fallback_width)
        } else {
            self.fallback_width
        }
    }

    fn space_width(&self) -> DimensionValue {
        self.metrics.width(32).unwrap_or(self.fallback_width)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MinimalTypesetter;

impl MinimalTypesetter {
    pub fn typeset(&self, document: &ParsedDocument) -> TypesetDocument {
        let page_box = page_box_for_class(&document.document_class);
        let nodes = document.body_nodes();
        let provider = default_fixed_width_provider();
        let hlist = document_nodes_to_hlist(&nodes, &provider);
        let wrapped_lines = wrap_hlist(&hlist, MAX_LINE_WIDTH);
        let pages = paginate_lines(&wrapped_lines, &page_box);

        TypesetDocument { pages }
    }
}

fn page_box_for_class(_document_class: &str) -> PageBox {
    PageBox {
        width: points(PAGE_WIDTH_PT),
        height: points(PAGE_HEIGHT_PT),
    }
}

fn default_fixed_width_provider() -> FixedWidthProvider {
    FixedWidthProvider {
        char_width: points(1),
        space_width: points(1),
    }
}

pub fn document_nodes_to_hlist(
    nodes: &[DocumentNode],
    provider: &dyn CharWidthProvider,
) -> Vec<HListItem> {
    let space_width = provider.space_width();
    let stretch = DimensionValue(space_width.0 / 2);
    let shrink = DimensionValue(space_width.0 / 3);
    let mut hlist = Vec::new();

    for node in nodes {
        match node {
            DocumentNode::Text(text) => {
                for codepoint in text.chars() {
                    match codepoint {
                        '\n' => hlist.push(HListItem::Penalty {
                            value: PENALTY_FORCED,
                        }),
                        codepoint if codepoint.is_whitespace() => hlist.push(HListItem::Glue {
                            width: space_width,
                            stretch,
                            shrink,
                        }),
                        codepoint => hlist.push(HListItem::Char {
                            codepoint,
                            width: provider.char_width(codepoint),
                        }),
                    }
                }
            }
            DocumentNode::ParBreak => {
                hlist.push(HListItem::Penalty {
                    value: PENALTY_FORCED,
                });
                hlist.push(HListItem::Penalty {
                    value: PENALTY_FORCED,
                });
            }
        }
    }

    hlist
}

fn paginate_lines(lines: &[String], page_box: &PageBox) -> Vec<TypesetPage> {
    if lines.is_empty() {
        return vec![TypesetPage {
            lines: Vec::new(),
            page_box: page_box.clone(),
        }];
    }

    lines
        .chunks(LINES_PER_PAGE)
        .map(|page_lines| TypesetPage {
            lines: page_lines
                .iter()
                .enumerate()
                .map(|(line_index, text)| TextLine {
                    text: text.clone(),
                    y: points(
                        PAGE_HEIGHT_PT - TOP_MARGIN_PT - (line_index as i64 * LINE_HEIGHT_PT),
                    ),
                })
                .collect(),
            page_box: page_box.clone(),
        })
        .collect()
}

fn wrap_hlist(hlist: &[HListItem], max_line_width: DimensionValue) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut current_line_width = DimensionValue::zero();
    let mut current_word = Vec::new();
    let mut pending_glue_width = None;
    let mut last_item_forced_break = false;

    for item in hlist {
        match item {
            HListItem::Char { codepoint, width } => {
                current_word.push((*codepoint, *width));
                last_item_forced_break = false;
            }
            HListItem::Glue { width, .. } => {
                last_item_forced_break = false;
                if !current_word.is_empty() {
                    append_word_to_line(
                        &mut lines,
                        &mut current_line,
                        &mut current_line_width,
                        &mut current_word,
                        &mut pending_glue_width,
                        max_line_width,
                    );
                }
                if !current_line.is_empty() {
                    pending_glue_width.get_or_insert(*width);
                }
            }
            HListItem::Penalty { value } if *value <= PENALTY_FORCED => {
                if !current_word.is_empty() {
                    append_word_to_line(
                        &mut lines,
                        &mut current_line,
                        &mut current_line_width,
                        &mut current_word,
                        &mut pending_glue_width,
                        max_line_width,
                    );
                }
                if !current_line.is_empty() {
                    lines.push(std::mem::take(&mut current_line));
                } else if last_item_forced_break {
                    lines.push(String::new());
                }
                current_line_width = DimensionValue::zero();
                pending_glue_width = None;
                last_item_forced_break = true;
            }
            HListItem::Penalty { .. } => {}
        }
    }

    if !current_word.is_empty() {
        append_word_to_line(
            &mut lines,
            &mut current_line,
            &mut current_line_width,
            &mut current_word,
            &mut pending_glue_width,
            max_line_width,
        );
    }
    if !current_line.is_empty() {
        lines.push(current_line);
    }

    lines
}

#[cfg(test)]
fn wrap_body(body: &str) -> Vec<String> {
    if body.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    for source_line in body.lines() {
        wrap_source_line(source_line.trim(), &mut lines);
    }
    lines
}

#[cfg(test)]
fn wrap_source_line(source_line: &str, lines: &mut Vec<String>) {
    if source_line.is_empty() {
        lines.push(String::new());
        return;
    }

    let mut current = String::new();
    for word in source_line.split_whitespace() {
        if word.chars().count() > MAX_LINE_CHARS {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            wrap_long_word(word, lines);
            continue;
        }

        let next_len = if current.is_empty() {
            word.chars().count()
        } else {
            current.chars().count() + 1 + word.chars().count()
        };

        if next_len > MAX_LINE_CHARS && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }

        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }

    if !current.is_empty() {
        lines.push(current);
    }
}

#[cfg(test)]
fn wrap_long_word(word: &str, lines: &mut Vec<String>) {
    let mut chunk = String::new();
    for ch in word.chars() {
        if chunk.chars().count() == MAX_LINE_CHARS {
            lines.push(std::mem::take(&mut chunk));
        }
        chunk.push(ch);
    }

    if !chunk.is_empty() {
        lines.push(chunk);
    }
}

fn append_word_to_line(
    lines: &mut Vec<String>,
    current_line: &mut String,
    current_line_width: &mut DimensionValue,
    current_word: &mut Vec<(char, DimensionValue)>,
    pending_glue_width: &mut Option<DimensionValue>,
    max_line_width: DimensionValue,
) {
    let word = std::mem::take(current_word);
    let word_width = word_width(&word);

    if word_width > max_line_width {
        if !current_line.is_empty() {
            lines.push(std::mem::take(current_line));
            *current_line_width = DimensionValue::zero();
        }
        *pending_glue_width = None;
        split_long_word_to_lines(
            lines,
            current_line,
            current_line_width,
            &word,
            max_line_width,
        );
        return;
    }

    let separator_width = if current_line.is_empty() {
        DimensionValue::zero()
    } else {
        pending_glue_width.take().unwrap_or(DimensionValue::zero())
    };

    if !current_line.is_empty()
        && *current_line_width + separator_width + word_width > max_line_width
    {
        lines.push(std::mem::take(current_line));
        *current_line_width = DimensionValue::zero();
    }

    if !current_line.is_empty() {
        current_line.push(' ');
        *current_line_width = *current_line_width + separator_width;
    }
    current_line.push_str(&word_to_string(&word));
    *current_line_width = *current_line_width + word_width;
}

fn split_long_word_to_lines(
    lines: &mut Vec<String>,
    current_line: &mut String,
    current_line_width: &mut DimensionValue,
    word: &[(char, DimensionValue)],
    max_line_width: DimensionValue,
) {
    let mut chunk = String::new();
    let mut chunk_width = DimensionValue::zero();

    for (codepoint, width) in word {
        if !chunk.is_empty() && chunk_width + *width > max_line_width {
            lines.push(std::mem::take(&mut chunk));
            chunk_width = DimensionValue::zero();
        }

        chunk.push(*codepoint);
        chunk_width = chunk_width + *width;

        if chunk_width > max_line_width {
            lines.push(std::mem::take(&mut chunk));
            chunk_width = DimensionValue::zero();
        }
    }

    if !chunk.is_empty() {
        lines.push(chunk);
    }

    *current_line = String::new();
    *current_line_width = DimensionValue::zero();
}

fn word_width(word: &[(char, DimensionValue)]) -> DimensionValue {
    word.iter()
        .fold(DimensionValue::zero(), |width, (_, char_width)| {
            width + *char_width
        })
}

fn word_to_string(word: &[(char, DimensionValue)]) -> String {
    word.iter().map(|(codepoint, _)| *codepoint).collect()
}

fn points(value: i64) -> DimensionValue {
    DimensionValue(value * SCALED_POINTS_PER_POINT)
}

#[cfg(test)]
mod tests {
    use super::{
        default_fixed_width_provider, document_nodes_to_hlist, points, wrap_body, wrap_hlist,
        CharWidthProvider, HListItem, MinimalTypesetter, TextLine, TfmWidthProvider,
        LINE_HEIGHT_PT, MAX_LINE_WIDTH, PAGE_HEIGHT_PT, PENALTY_FORCED, TOP_MARGIN_PT,
    };
    use crate::font::api::TfmMetrics;
    use crate::kernel::api::DimensionValue;
    use crate::parser::api::{DocumentNode, ParsedDocument};

    fn parsed_document(body: &str) -> ParsedDocument {
        ParsedDocument {
            document_class: "article".to_string(),
            package_count: 0,
            body: body.to_string(),
        }
    }

    #[test]
    fn empty_body_yields_single_empty_page() {
        let document = MinimalTypesetter.typeset(&parsed_document(""));

        assert_eq!(document.pages.len(), 1);
        assert!(document.pages[0].lines.is_empty());
    }

    #[test]
    fn short_body_stays_on_single_page() {
        let document = MinimalTypesetter.typeset(&parsed_document("Hello\nFerritex"));

        assert_eq!(document.pages.len(), 1);
        assert_eq!(
            document.pages[0].lines,
            vec![
                TextLine {
                    text: "Hello".to_string(),
                    y: points(PAGE_HEIGHT_PT - TOP_MARGIN_PT),
                },
                TextLine {
                    text: "Ferritex".to_string(),
                    y: points(PAGE_HEIGHT_PT - TOP_MARGIN_PT - LINE_HEIGHT_PT),
                },
            ]
        );
    }

    #[test]
    fn long_body_flows_to_multiple_pages() {
        let body = (1..=37)
            .map(|index| format!("Line {index}"))
            .collect::<Vec<_>>()
            .join("\n");

        let document = MinimalTypesetter.typeset(&parsed_document(&body));

        assert_eq!(document.pages.len(), 2);
        assert_eq!(document.pages[0].lines.len(), 36);
        assert_eq!(document.pages[1].lines.len(), 1);
        assert_eq!(document.pages[1].lines[0].text, "Line 37");
    }

    #[test]
    fn wraps_long_lines_at_fixed_width() {
        let body = "a".repeat(71);

        let document = MinimalTypesetter.typeset(&parsed_document(&body));

        assert_eq!(document.pages.len(), 1);
        assert_eq!(document.pages[0].lines.len(), 2);
        assert_eq!(document.pages[0].lines[0].text.chars().count(), 70);
        assert_eq!(document.pages[0].lines[1].text, "a");
    }

    #[test]
    fn document_nodes_to_hlist_produces_chars_and_glue() {
        let hlist = document_nodes_to_hlist(
            &[DocumentNode::Text("A B".to_string())],
            &default_fixed_width_provider(),
        );

        assert_eq!(
            hlist,
            vec![
                HListItem::Char {
                    codepoint: 'A',
                    width: points(1),
                },
                HListItem::Glue {
                    width: points(1),
                    stretch: DimensionValue(points(1).0 / 2),
                    shrink: DimensionValue(points(1).0 / 3),
                },
                HListItem::Char {
                    codepoint: 'B',
                    width: points(1),
                },
            ]
        );
    }

    #[test]
    fn document_nodes_to_hlist_par_break_produces_blank_line_break() {
        let hlist =
            document_nodes_to_hlist(&[DocumentNode::ParBreak], &default_fixed_width_provider());

        assert_eq!(
            hlist,
            vec![
                HListItem::Penalty {
                    value: PENALTY_FORCED
                },
                HListItem::Penalty {
                    value: PENALTY_FORCED
                },
            ]
        );
    }

    #[test]
    fn wrap_hlist_matches_char_counting() {
        let body = format!(
            "{}\n{}",
            "a".repeat(71),
            "Ferritex wraps this line using the same output as before"
        );
        let nodes = parsed_document(&body).body_nodes();
        let hlist = document_nodes_to_hlist(&nodes, &default_fixed_width_provider());

        assert_eq!(wrap_hlist(&hlist, MAX_LINE_WIDTH), wrap_body(&body));
    }

    #[test]
    fn explicit_par_break_produces_blank_output_line() {
        let document = MinimalTypesetter.typeset(&parsed_document(
            r"First paragraph\par Second paragraph",
        ));

        assert_eq!(
            document.pages[0]
                .lines
                .iter()
                .map(|line| line.text.clone())
                .collect::<Vec<_>>(),
            vec![
                "First paragraph".to_string(),
                String::new(),
                "Second paragraph".to_string(),
            ]
        );
    }

    #[test]
    fn wrap_hlist_matches_char_counting_with_paragraph_breaks_and_mixed_words() {
        let body = format!(
            "intro {} tail words\n\nprefix short {} end",
            "a".repeat(71),
            "b".repeat(72)
        );
        let nodes = parsed_document(&body).body_nodes();
        let hlist = document_nodes_to_hlist(&nodes, &default_fixed_width_provider());

        assert_eq!(wrap_hlist(&hlist, MAX_LINE_WIDTH), wrap_body(&body));
    }

    #[test]
    fn tfm_width_provider_reads_character_widths() {
        let metrics =
            TfmMetrics::parse(&single_char_tfm(65, 10_485_760, 524_288)).expect("parse TFM");
        let provider = TfmWidthProvider {
            metrics: &metrics,
            fallback_width: points(1),
        };

        assert_eq!(provider.char_width('A'), DimensionValue(327_680));
        assert_eq!(provider.char_width('😀'), points(1));
        assert_eq!(provider.space_width(), points(1));
    }

    fn single_char_tfm(char_code: u16, design_size_fixword: i32, width_fixword: i32) -> Vec<u8> {
        let lf = 14u16;
        let lh = 2u16;
        let mut data = Vec::with_capacity(usize::from(lf) * 4);

        for value in [lf, lh, char_code, char_code, 2, 1, 1, 1, 0, 0, 0, 0] {
            data.extend_from_slice(&value.to_be_bytes());
        }
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&design_size_fixword.to_be_bytes());
        data.extend_from_slice(&[1, 0, 0, 0]);
        data.extend_from_slice(&0i32.to_be_bytes());
        data.extend_from_slice(&width_fixword.to_be_bytes());
        data.extend_from_slice(&0i32.to_be_bytes());
        data.extend_from_slice(&0i32.to_be_bytes());
        data.extend_from_slice(&0i32.to_be_bytes());

        data
    }
}
