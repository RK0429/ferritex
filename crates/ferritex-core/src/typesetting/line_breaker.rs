use crate::kernel::api::DimensionValue;

use super::{
    api::{HListItem, TextLineLink, PENALTY_FORBIDDEN, PENALTY_FORCED},
    knuth_plass::{self, BreakParams},
};

pub fn break_paragraph(hlist: &[HListItem], params: &BreakParams) -> Vec<String> {
    break_paragraph_with_links(hlist, params)
        .into_iter()
        .map(|line| line.text)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrokenLine {
    pub text: String,
    pub links: Vec<TextLineLink>,
}

pub fn break_paragraph_with_links(hlist: &[HListItem], params: &BreakParams) -> Vec<BrokenLine> {
    if hlist.is_empty() {
        return Vec::new();
    }

    let breakpoints = knuth_plass::find_breakpoints(hlist, params);
    let mut lines = Vec::new();
    let mut line_start = skip_line_start(hlist, 0);

    for break_index in breakpoints {
        let line_end = visible_break_end(hlist, line_start, break_index);
        push_segment_lines(
            &mut lines,
            &hlist[line_start..line_end],
            params.line_width,
            is_discretionary_hyphen_break(hlist, break_index),
        );
        line_start = skip_line_start(hlist, break_index + 1);
    }

    let tail_end = trim_visible_end(hlist, line_start, hlist.len());
    if line_start < tail_end {
        push_segment_lines(
            &mut lines,
            &hlist[line_start..tail_end],
            params.line_width,
            false,
        );
    }

    lines
}

fn is_discretionary_hyphen_break(hlist: &[HListItem], break_index: usize) -> bool {
    matches!(
        hlist.get(break_index),
        Some(HListItem::Penalty { value }) if *value > 0 && *value < PENALTY_FORBIDDEN
    )
}

fn visible_break_end(hlist: &[HListItem], line_start: usize, break_index: usize) -> usize {
    let raw_end = match hlist.get(break_index) {
        Some(HListItem::Penalty { .. }) => break_index,
        _ => break_index.saturating_add(1),
    };

    trim_visible_end(hlist, line_start, raw_end)
}

fn trim_visible_end(hlist: &[HListItem], line_start: usize, mut line_end: usize) -> usize {
    while line_end > line_start {
        match hlist[line_end - 1] {
            HListItem::Glue { .. } => line_end -= 1,
            HListItem::Penalty { value } if value > PENALTY_FORCED => line_end -= 1,
            _ => break,
        }
    }

    line_end
}

fn skip_line_start(hlist: &[HListItem], mut index: usize) -> usize {
    while let Some(item) = hlist.get(index) {
        match item {
            HListItem::Glue { .. } => index += 1,
            HListItem::Penalty { value } if *value > PENALTY_FORCED => index += 1,
            _ => break,
        }
    }

    index
}

fn push_segment_lines(
    lines: &mut Vec<BrokenLine>,
    segment: &[HListItem],
    line_width: DimensionValue,
    append_hyphen: bool,
) {
    let initial_len = lines.len();

    if segment.is_empty() {
        lines.push(BrokenLine::default());
        return;
    }

    if segment
        .iter()
        .any(|item| matches!(item, HListItem::Glue { .. }))
        || segment_width(segment) <= line_width
    {
        lines.push(render_line(segment));
    } else {
        push_unbreakable_lines(lines, segment, line_width);
    }

    if append_hyphen && lines.len() > initial_len {
        let last_index = lines.len() - 1;
        if let Some(line) = lines.get_mut(last_index) {
            if !line.text.is_empty() {
                let start_char = line.text.chars().count();
                line.text.push('-');
                if let Some(url) = line
                    .links
                    .last()
                    .and_then(|link| (link.end_char == start_char).then(|| link.url.clone()))
                {
                    push_link_range(&mut line.links, &url, start_char, start_char + 1);
                }
            }
        }
    }
}

fn segment_width(segment: &[HListItem]) -> DimensionValue {
    segment.iter().fold(DimensionValue::zero(), |width, item| {
        width + item_width(item)
    })
}

fn item_width(item: &HListItem) -> DimensionValue {
    match item {
        HListItem::Char { width, .. } | HListItem::Kern { width } => *width,
        HListItem::Glue { width, .. } => *width,
        HListItem::Penalty { .. } => DimensionValue::zero(),
    }
}

fn render_line(segment: &[HListItem]) -> BrokenLine {
    let mut text = String::new();
    let mut links = Vec::new();
    let mut pending_space = false;
    let mut pending_space_link = None;

    for item in segment {
        match item {
            HListItem::Char {
                codepoint, link, ..
            } => {
                if pending_space && !text.is_empty() {
                    let start_char = text.chars().count();
                    text.push(' ');
                    if let Some(url) = pending_space_link.as_deref() {
                        push_link_range(&mut links, url, start_char, start_char + 1);
                    }
                }
                let start_char = text.chars().count();
                text.push(*codepoint);
                if let Some(url) = link.as_deref() {
                    push_link_range(&mut links, url, start_char, start_char + 1);
                }
                pending_space = false;
                pending_space_link = None;
            }
            HListItem::Glue { link, .. } => {
                if !text.is_empty() {
                    pending_space = true;
                    pending_space_link = link.clone();
                }
            }
            HListItem::Kern { .. } | HListItem::Penalty { .. } => {}
        }
    }

    BrokenLine { text, links }
}

fn push_unbreakable_lines(
    lines: &mut Vec<BrokenLine>,
    segment: &[HListItem],
    line_width: DimensionValue,
) {
    let mut current_line = BrokenLine::default();
    let mut current_width = DimensionValue::zero();

    for item in segment {
        let width = item_width(item);
        if !current_line.text.is_empty() && current_width + width > line_width {
            lines.push(std::mem::take(&mut current_line));
            current_width = DimensionValue::zero();
        }

        if let HListItem::Char {
            codepoint, link, ..
        } = item
        {
            let start_char = current_line.text.chars().count();
            current_line.text.push(*codepoint);
            if let Some(url) = link.as_deref() {
                push_link_range(&mut current_line.links, url, start_char, start_char + 1);
            }
        }
        current_width = current_width + width;

        if current_width > line_width {
            lines.push(std::mem::take(&mut current_line));
            current_width = DimensionValue::zero();
        }
    }

    if !current_line.text.is_empty() {
        lines.push(current_line);
    }
}

fn push_link_range(links: &mut Vec<TextLineLink>, url: &str, start_char: usize, end_char: usize) {
    if start_char >= end_char {
        return;
    }

    if let Some(last) = links.last_mut() {
        if last.url == url && last.end_char == start_char {
            last.end_char = end_char;
            return;
        }
    }

    links.push(TextLineLink {
        url: url.to_string(),
        start_char,
        end_char,
    });
}

#[cfg(test)]
mod tests {
    use super::break_paragraph;
    use crate::kernel::api::DimensionValue;
    use crate::typesetting::{
        api::{CharWidthProvider, FixedWidthProvider, GlueComponent, HListItem, PENALTY_FORCED},
        knuth_plass::BreakParams,
    };

    #[derive(Debug, Clone, Copy)]
    enum TestPart<'a> {
        Word(&'a str),
        Glue,
        ForcedBreak,
    }

    fn dim(value: i64) -> DimensionValue {
        DimensionValue(value)
    }

    fn params(line_width: i64) -> BreakParams {
        BreakParams {
            line_width: dim(line_width),
            ..BreakParams::default()
        }
    }

    fn provider(char_width: i64, space_width: i64) -> FixedWidthProvider {
        FixedWidthProvider {
            char_width: dim(char_width),
            space_width: dim(space_width),
        }
    }

    fn build_hlist(
        provider: FixedWidthProvider,
        stretch: DimensionValue,
        shrink: DimensionValue,
        parts: &[TestPart<'_>],
    ) -> Vec<HListItem> {
        let mut hlist = Vec::new();

        for part in parts {
            match part {
                TestPart::Word(word) => {
                    for codepoint in word.chars() {
                        hlist.push(HListItem::Char {
                            codepoint,
                            width: provider.char_width(codepoint),
                            link: None,
                        });
                    }
                }
                TestPart::Glue => hlist.push(HListItem::Glue {
                    width: provider.space_width(),
                    stretch: GlueComponent::normal(stretch),
                    shrink: GlueComponent::normal(shrink),
                    link: None,
                }),
                TestPart::ForcedBreak => hlist.push(HListItem::Penalty {
                    value: PENALTY_FORCED,
                }),
            }
        }

        hlist
    }

    #[test]
    fn renders_lines_from_knuth_plass_breakpoints() {
        let hlist = build_hlist(
            provider(10, 1),
            dim(10),
            dim(1),
            &[
                TestPart::Word("a"),
                TestPart::Glue,
                TestPart::Word("b"),
                TestPart::Glue,
                TestPart::Word("c"),
                TestPart::Glue,
                TestPart::Word("d"),
                TestPart::Glue,
                TestPart::Word("e"),
                TestPart::Glue,
                TestPart::Word("f"),
            ],
        );

        assert_eq!(
            break_paragraph(&hlist, &params(22)),
            vec!["a b", "c d", "e f"]
        );
    }

    #[test]
    fn double_forced_break_yields_blank_line() {
        let hlist = build_hlist(
            provider(10, 1),
            dim(10),
            dim(1),
            &[
                TestPart::Word("a"),
                TestPart::ForcedBreak,
                TestPart::ForcedBreak,
                TestPart::Word("b"),
            ],
        );

        assert_eq!(break_paragraph(&hlist, &params(100)), vec!["a", "", "b"]);
    }

    #[test]
    fn discretionary_penalty_appends_visible_hyphen() {
        let hlist = vec![
            HListItem::Char {
                codepoint: 'b',
                width: dim(10),
                link: None,
            },
            HListItem::Char {
                codepoint: 'a',
                width: dim(10),
                link: None,
            },
            HListItem::Char {
                codepoint: 's',
                width: dim(10),
                link: None,
            },
            HListItem::Penalty { value: 50 },
            HListItem::Char {
                codepoint: 'k',
                width: dim(10),
                link: None,
            },
            HListItem::Char {
                codepoint: 'e',
                width: dim(10),
                link: None,
            },
            HListItem::Char {
                codepoint: 't',
                width: dim(10),
                link: None,
            },
        ];

        assert_eq!(break_paragraph(&hlist, &params(35)), vec!["bas-", "ket"]);
    }

    #[test]
    fn overfull_unbreakable_segments_fall_back_to_character_splitting() {
        let hlist = vec![
            HListItem::Char {
                codepoint: 'A',
                width: dim(1),
                link: None,
            },
            HListItem::Kern { width: dim(1) },
            HListItem::Char {
                codepoint: 'B',
                width: dim(1),
                link: None,
            },
        ];

        assert_eq!(break_paragraph(&hlist, &params(1)), vec!["A", "B"]);
    }
}
