use crate::kernel::api::DimensionValue;
use crate::parser::api::ParsedDocument;

const SCALED_POINTS_PER_POINT: i64 = 65_536;
const PAGE_WIDTH_PT: i64 = 612;
const PAGE_HEIGHT_PT: i64 = 792;
const TOP_MARGIN_PT: i64 = 72;
const LINE_HEIGHT_PT: i64 = 18;
const MAX_LINE_CHARS: usize = 70;
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MinimalTypesetter;

impl MinimalTypesetter {
    pub fn typeset(&self, document: &ParsedDocument) -> TypesetDocument {
        let page_box = page_box_for_class(&document.document_class);
        let wrapped_lines = wrap_body(&document.body);
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

fn points(value: i64) -> DimensionValue {
    DimensionValue(value * SCALED_POINTS_PER_POINT)
}

#[cfg(test)]
mod tests {
    use super::{
        points, MinimalTypesetter, TextLine, LINE_HEIGHT_PT, PAGE_HEIGHT_PT, TOP_MARGIN_PT,
    };
    use crate::parser::api::ParsedDocument;

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
}
