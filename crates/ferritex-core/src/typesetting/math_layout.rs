use super::api::{
    push_forced_break_if_needed, CharWidthProvider, GlueComponent, GlueOrder, HListItem,
};
use crate::kernel::api::DimensionValue;
use crate::parser::api::{MathLine, MathNode, OverUnderKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathAtomKind {
    Ord,
    Op,
    Bin,
    Rel,
    Open,
    Close,
    Punct,
    Inner,
}

impl MathAtomKind {
    const fn index(self) -> usize {
        match self {
            Self::Ord => 0,
            Self::Op => 1,
            Self::Bin => 2,
            Self::Rel => 3,
            Self::Open => 4,
            Self::Close => 5,
            Self::Punct => 6,
            Self::Inner => 7,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathStyle {
    Display,
    Text,
    Script,
    ScriptScript,
}

impl MathStyle {
    pub const fn script_style(self) -> Self {
        match self {
            Self::Display | Self::Text => Self::Script,
            Self::Script | Self::ScriptScript => Self::ScriptScript,
        }
    }

    pub const fn frac_numer_style(self) -> Self {
        match self {
            Self::Display => Self::Text,
            Self::Text => Self::Script,
            Self::Script | Self::ScriptScript => Self::ScriptScript,
        }
    }

    pub const fn frac_denom_style(self) -> Self {
        self.frac_numer_style()
    }

    pub const fn scale_factor(self) -> f64 {
        match self {
            Self::Display | Self::Text => 1.0,
            Self::Script => 0.7,
            Self::ScriptScript => 0.5,
        }
    }
}

pub const SPACING_TABLE: [[u8; 8]; 8] = [
    [0, 1, 2, 3, 0, 0, 0, 1],
    [1, 1, 0, 3, 0, 0, 0, 1],
    [2, 2, 0, 0, 2, 0, 0, 2],
    [3, 3, 0, 0, 3, 0, 0, 3],
    [0, 0, 0, 0, 0, 0, 0, 0],
    [0, 1, 2, 3, 0, 0, 0, 1],
    [1, 1, 0, 1, 1, 1, 1, 1],
    [1, 1, 2, 3, 1, 0, 1, 1],
];

pub const SPACING_BRACKETED: [[bool; 8]; 8] = [
    [false, false, true, true, false, false, false, true],
    [false, false, false, true, false, false, false, true],
    [true, true, false, false, true, false, false, true],
    [true, true, false, false, true, false, false, true],
    [false, false, false, false, false, false, false, false],
    [false, false, true, true, false, false, false, true],
    [true, true, false, true, true, true, true, true],
    [true, false, true, true, true, false, true, true],
];

pub fn classify_char(ch: char) -> MathAtomKind {
    match ch {
        '+' | '-' | '*' => MathAtomKind::Bin,
        '=' | '<' | '>' => MathAtomKind::Rel,
        '(' | '[' => MathAtomKind::Open,
        ')' | ']' => MathAtomKind::Close,
        ',' | ';' => MathAtomKind::Punct,
        _ => MathAtomKind::Ord,
    }
}

pub fn classify_symbol(symbol: &str) -> MathAtomKind {
    if matches!(symbol, "∑" | "∫" | "∏" | "lim") {
        MathAtomKind::Op
    } else if matches!(symbol, "≤" | "≥" | "→" | "←" | "↔" | "∈" | "⊂" | "≠") {
        MathAtomKind::Rel
    } else {
        MathAtomKind::Ord
    }
}

pub fn classify_math_node(node: &MathNode) -> MathAtomKind {
    match node {
        MathNode::Ordinary(ch) => classify_char(*ch),
        MathNode::Symbol(symbol) => classify_symbol(symbol),
        MathNode::Frac { .. } | MathNode::Sqrt { .. } | MathNode::LeftRight { .. } => {
            MathAtomKind::Inner
        }
        MathNode::OverUnder { .. } | MathNode::Text(_) => MathAtomKind::Ord,
        MathNode::MathFont { body, .. } | MathNode::Group(body) => last_visible_atom_kind(body),
        MathNode::Superscript(node) | MathNode::Subscript(node) => classify_math_node(node),
    }
}

pub fn mu_to_sp(mu: f64, em_width: DimensionValue) -> DimensionValue {
    DimensionValue(((em_width.0 as f64 / 18.0) * mu).round() as i64)
}

pub fn inter_atom_space(
    left: MathAtomKind,
    right: MathAtomKind,
    style: MathStyle,
    em_width: DimensionValue,
) -> DimensionValue {
    let mut spacing = SPACING_TABLE[left.index()][right.index()];
    if matches!(style, MathStyle::Script | MathStyle::ScriptScript)
        && SPACING_BRACKETED[left.index()][right.index()]
    {
        spacing = 0;
    }

    match spacing {
        1 => mu_to_sp(3.0, em_width),
        2 => mu_to_sp(4.0, em_width),
        3 => mu_to_sp(5.0, em_width),
        _ => DimensionValue::zero(),
    }
}

pub fn math_nodes_to_hlist(
    nodes: &[MathNode],
    provider: &dyn CharWidthProvider,
    style: MathStyle,
) -> Vec<HListItem> {
    let mut items = Vec::new();
    let mut prev_kind = None;
    extend_math_nodes(&mut items, nodes, provider, style, &mut prev_kind);
    items
}

pub fn hlist_to_string(items: &[HListItem]) -> String {
    let mut rendered = String::new();
    for item in items {
        match item {
            HListItem::Char { codepoint, .. } => rendered.push(*codepoint),
            HListItem::Glue { .. } => rendered.push(' '),
            HListItem::InlineBox { content, .. } => rendered.push_str(content),
            HListItem::Kern { .. } | HListItem::Penalty { .. } => {}
        }
    }
    rendered
}

pub fn hlist_total_width(items: &[HListItem]) -> DimensionValue {
    items.iter().fold(DimensionValue::zero(), |width, item| {
        width
            + match item {
                HListItem::Char { width, .. }
                | HListItem::Glue { width, .. }
                | HListItem::Kern { width }
                | HListItem::InlineBox { width, .. } => *width,
                HListItem::Penalty { .. } => DimensionValue::zero(),
            }
    })
}

pub fn typeset_equation_env(
    lines: &[MathLine],
    provider: &dyn CharWidthProvider,
    line_width: DimensionValue,
) -> Vec<HListItem> {
    if lines.is_empty() {
        return Vec::new();
    }

    let column_gap = DimensionValue(provider.space_width().0 * 4);
    let column_count = lines
        .iter()
        .map(|line| line.segments.len())
        .max()
        .unwrap_or(0);
    let mut max_column_widths = vec![DimensionValue::zero(); column_count];
    let mut prepared_lines = Vec::with_capacity(lines.len());

    for line in lines {
        let mut segments = Vec::with_capacity(line.segments.len());
        for (column_index, segment) in line.segments.iter().enumerate() {
            let items = math_nodes_to_hlist(segment, provider, MathStyle::Display);
            let width = hlist_total_width(&items);
            if width > max_column_widths[column_index] {
                max_column_widths[column_index] = width;
            }
            segments.push((items, width));
        }

        let tag_items = line
            .display_tag
            .as_deref()
            .map(|tag| text_to_hlist(&format!("({tag})"), provider));
        prepared_lines.push((segments, tag_items));
    }

    let block_width = max_column_widths
        .iter()
        .fold(DimensionValue::zero(), |width, column_width| {
            width + *column_width
        })
        + DimensionValue(column_gap.0 * column_count.saturating_sub(1) as i64);

    let mut hlist = Vec::new();
    for (line_index, (segments, tag_items)) in prepared_lines.into_iter().enumerate() {
        if line_index > 0 {
            push_forced_break_if_needed(&mut hlist);
        }

        let tag_width = tag_items
            .as_ref()
            .map(|items| hlist_total_width(items))
            .unwrap_or_else(DimensionValue::zero);
        let remaining = (line_width - tag_width).0 - block_width.0;
        let left_padding = DimensionValue(remaining.max(0) / 2);
        if left_padding.0 > 0 {
            hlist.push(HListItem::Kern {
                width: left_padding,
            });
        }

        for (column_index, max_column_width) in
            max_column_widths.iter().enumerate().take(column_count)
        {
            let (segment_items, segment_width) = segments
                .get(column_index)
                .cloned()
                .unwrap_or_else(|| (Vec::new(), DimensionValue::zero()));
            hlist.extend(segment_items);

            let pad_width = *max_column_width - segment_width;
            if pad_width.0 > 0 {
                hlist.push(HListItem::Kern { width: pad_width });
            }

            if column_index + 1 < column_count {
                hlist.push(HListItem::Kern { width: column_gap });
            }
        }

        if let Some(tag_items) = tag_items {
            hlist.push(HListItem::Glue {
                width: DimensionValue::zero(),
                stretch: GlueComponent {
                    value: DimensionValue(1),
                    order: GlueOrder::Fill,
                },
                shrink: GlueComponent::normal(DimensionValue::zero()),
                link: None,
                font_index: 0,
            });
            hlist.extend(tag_items);
        }

        push_forced_break_if_needed(&mut hlist);
    }

    hlist
}

fn extend_math_nodes(
    items: &mut Vec<HListItem>,
    nodes: &[MathNode],
    provider: &dyn CharWidthProvider,
    style: MathStyle,
    prev_kind: &mut Option<MathAtomKind>,
) {
    let em_width = scaled_dimension(provider.char_width('M'), style.scale_factor());

    for node in nodes {
        match node {
            MathNode::Ordinary(ch) => {
                let kind = classify_char(*ch);
                push_spacing(items, *prev_kind, kind, style, em_width);
                items.push(HListItem::Char {
                    codepoint: *ch,
                    width: scaled_char_width(provider, *ch, style),
                    link: None,
                    font_index: 0,
                });
                *prev_kind = Some(kind);
            }
            MathNode::Symbol(symbol) => {
                if symbol.is_empty() {
                    continue;
                }
                let kind = classify_symbol(symbol);
                push_spacing(items, *prev_kind, kind, style, em_width);
                for ch in symbol.chars() {
                    items.push(HListItem::Char {
                        codepoint: ch,
                        width: scaled_char_width(provider, ch, style),
                        link: None,
                        font_index: 0,
                    });
                }
                *prev_kind = Some(kind);
            }
            MathNode::Superscript(node) | MathNode::Subscript(node) => {
                let script_items = math_nodes_to_hlist(
                    std::slice::from_ref(node.as_ref()),
                    provider,
                    style.script_style(),
                );
                if !script_items.is_empty() {
                    items.push(inline_box_from_items(script_items));
                }
            }
            MathNode::Frac { numer, denom } => {
                push_spacing(items, *prev_kind, MathAtomKind::Inner, style, em_width);
                let numer_items = math_nodes_to_hlist(numer, provider, style.frac_numer_style());
                let denom_items = math_nodes_to_hlist(denom, provider, style.frac_denom_style());
                let numer_width = hlist_total_width(&numer_items);
                let denom_width = hlist_total_width(&denom_items);
                items.push(HListItem::InlineBox {
                    width: max_dimension(numer_width, denom_width),
                    height: DimensionValue::zero(),
                    depth: DimensionValue::zero(),
                    content: format!(
                        "{}/{}",
                        hlist_to_string(&numer_items),
                        hlist_to_string(&denom_items)
                    ),
                });
                *prev_kind = Some(MathAtomKind::Inner);
            }
            MathNode::Sqrt { radicand, index } => {
                push_spacing(items, *prev_kind, MathAtomKind::Inner, style, em_width);
                let radicand_items = math_nodes_to_hlist(radicand, provider, style);
                let radicand_text = hlist_to_string(&radicand_items);
                let mut total_width =
                    scaled_char_width(provider, '√', style) + hlist_total_width(&radicand_items);
                let mut content = String::from("√");
                if let Some(index) = index.as_ref() {
                    let index_items = math_nodes_to_hlist(index, provider, style.script_style());
                    total_width = total_width + hlist_total_width(&index_items);
                    content.push_str(&hlist_to_string(&index_items));
                }
                content.push_str(&radicand_text);
                items.push(HListItem::InlineBox {
                    width: total_width,
                    height: DimensionValue::zero(),
                    depth: DimensionValue::zero(),
                    content,
                });
                *prev_kind = Some(MathAtomKind::Inner);
            }
            MathNode::LeftRight { left, right, body } => {
                push_spacing(items, *prev_kind, MathAtomKind::Inner, style, em_width);
                let left_visible = visible_delimiter(left);
                let right_visible = visible_delimiter(right);

                for ch in left_visible.chars() {
                    items.push(HListItem::Char {
                        codepoint: ch,
                        width: scaled_char_width(provider, ch, style),
                        link: None,
                        font_index: 0,
                    });
                }

                let mut inner_prev = left_visible.chars().last().map(classify_char);
                extend_math_nodes(items, body, provider, style, &mut inner_prev);

                if let Some(first) = right_visible.chars().next() {
                    push_spacing(items, inner_prev, classify_char(first), style, em_width);
                }
                for ch in right_visible.chars() {
                    items.push(HListItem::Char {
                        codepoint: ch,
                        width: scaled_char_width(provider, ch, style),
                        link: None,
                        font_index: 0,
                    });
                }
                *prev_kind = Some(MathAtomKind::Inner);
            }
            MathNode::OverUnder {
                kind,
                base,
                annotation,
            } => {
                push_spacing(items, *prev_kind, MathAtomKind::Ord, style, em_width);
                let base_items = math_nodes_to_hlist(base, provider, style);
                let annotation_items =
                    math_nodes_to_hlist(annotation, provider, style.script_style());
                let base_text = hlist_to_string(&base_items);
                let annotation_text = hlist_to_string(&annotation_items);
                let content = match kind {
                    OverUnderKind::Over => format!("{base_text}^{annotation_text}"),
                    OverUnderKind::Under => format!("{base_text}_{annotation_text}"),
                };
                items.push(HListItem::InlineBox {
                    width: max_dimension(
                        hlist_total_width(&base_items),
                        hlist_total_width(&annotation_items),
                    ),
                    height: DimensionValue::zero(),
                    depth: DimensionValue::zero(),
                    content,
                });
                *prev_kind = Some(MathAtomKind::Ord);
            }
            MathNode::MathFont { body, .. } | MathNode::Group(body) => {
                extend_math_nodes(items, body, provider, style, prev_kind);
            }
            MathNode::Text(text) => {
                if text.is_empty() {
                    continue;
                }
                push_spacing(items, *prev_kind, MathAtomKind::Ord, style, em_width);
                for ch in text.chars() {
                    items.push(HListItem::Char {
                        codepoint: ch,
                        width: provider.char_width(ch),
                        link: None,
                        font_index: 0,
                    });
                }
                *prev_kind = Some(MathAtomKind::Ord);
            }
        }
    }
}

fn push_spacing(
    items: &mut Vec<HListItem>,
    left: Option<MathAtomKind>,
    right: MathAtomKind,
    style: MathStyle,
    em_width: DimensionValue,
) {
    let Some(left) = left else {
        return;
    };

    let width = inter_atom_space(left, right, style, em_width);
    if width.0 > 0 {
        items.push(HListItem::Kern { width });
    }
}

fn inline_box_from_items(items: Vec<HListItem>) -> HListItem {
    HListItem::InlineBox {
        width: hlist_total_width(&items),
        height: DimensionValue::zero(),
        depth: DimensionValue::zero(),
        content: hlist_to_string(&items),
    }
}

fn text_to_hlist(text: &str, provider: &dyn CharWidthProvider) -> Vec<HListItem> {
    let mut items = Vec::new();
    for ch in text.chars() {
        match ch {
            ' ' => items.push(HListItem::Glue {
                width: provider.space_width(),
                stretch: GlueComponent::normal(DimensionValue::zero()),
                shrink: GlueComponent::normal(DimensionValue::zero()),
                link: None,
                font_index: 0,
            }),
            _ => items.push(HListItem::Char {
                codepoint: ch,
                width: provider.char_width(ch),
                link: None,
                font_index: 0,
            }),
        }
    }
    items
}

fn visible_delimiter(delimiter: &str) -> &str {
    if delimiter == "." {
        ""
    } else {
        delimiter
    }
}

fn last_visible_atom_kind(nodes: &[MathNode]) -> MathAtomKind {
    for node in nodes.iter().rev() {
        match node {
            MathNode::Superscript(_) | MathNode::Subscript(_) => continue,
            MathNode::MathFont { body, .. } | MathNode::Group(body) if body.is_empty() => continue,
            MathNode::Text(text) if text.is_empty() => continue,
            MathNode::Symbol(symbol) if symbol.is_empty() => continue,
            MathNode::MathFont { body, .. } | MathNode::Group(body) => {
                return last_visible_atom_kind(body);
            }
            _ => return classify_math_node(node),
        }
    }

    MathAtomKind::Ord
}

fn max_dimension(left: DimensionValue, right: DimensionValue) -> DimensionValue {
    if left >= right {
        left
    } else {
        right
    }
}

fn scaled_char_width(
    provider: &dyn CharWidthProvider,
    codepoint: char,
    style: MathStyle,
) -> DimensionValue {
    scaled_dimension(provider.char_width(codepoint), style.scale_factor())
}

fn scaled_dimension(width: DimensionValue, factor: f64) -> DimensionValue {
    DimensionValue((width.0 as f64 * factor).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::{
        classify_char, classify_symbol, hlist_to_string, hlist_total_width, inter_atom_space,
        math_nodes_to_hlist, mu_to_sp, MathAtomKind, MathStyle,
    };
    use crate::kernel::api::DimensionValue;
    use crate::parser::api::MathNode;
    use crate::typesetting::api::{CharWidthProvider, HListItem};

    struct TestWidthProvider;

    impl CharWidthProvider for TestWidthProvider {
        fn char_width(&self, _codepoint: char) -> DimensionValue {
            DimensionValue(10)
        }

        fn space_width(&self) -> DimensionValue {
            DimensionValue(10)
        }
    }

    #[test]
    fn classifies_char_atoms() {
        assert_eq!(classify_char('+'), MathAtomKind::Bin);
        assert_eq!(classify_char('='), MathAtomKind::Rel);
        assert_eq!(classify_char('x'), MathAtomKind::Ord);
        assert_eq!(classify_char('('), MathAtomKind::Open);
        assert_eq!(classify_char(')'), MathAtomKind::Close);
        assert_eq!(classify_char(','), MathAtomKind::Punct);
    }

    #[test]
    fn classifies_symbol_atoms() {
        assert_eq!(classify_symbol("∑"), MathAtomKind::Op);
        assert_eq!(classify_symbol("α"), MathAtomKind::Ord);
        assert_eq!(classify_symbol("≤"), MathAtomKind::Rel);
    }

    #[test]
    fn computes_inter_atom_spacing() {
        let em_width = DimensionValue(10);
        assert_eq!(
            inter_atom_space(
                MathAtomKind::Ord,
                MathAtomKind::Bin,
                MathStyle::Text,
                em_width
            ),
            mu_to_sp(4.0, em_width)
        );
        assert_eq!(
            inter_atom_space(
                MathAtomKind::Ord,
                MathAtomKind::Rel,
                MathStyle::Text,
                em_width
            ),
            mu_to_sp(5.0, em_width)
        );
        assert_eq!(
            inter_atom_space(
                MathAtomKind::Ord,
                MathAtomKind::Ord,
                MathStyle::Text,
                em_width
            ),
            DimensionValue::zero()
        );
        assert_eq!(
            inter_atom_space(
                MathAtomKind::Open,
                MathAtomKind::Ord,
                MathStyle::Text,
                em_width
            ),
            DimensionValue::zero()
        );
        assert_eq!(
            inter_atom_space(
                MathAtomKind::Ord,
                MathAtomKind::Bin,
                MathStyle::Script,
                em_width,
            ),
            DimensionValue::zero()
        );
    }

    #[test]
    fn converts_simple_math_to_hlist() {
        let items = math_nodes_to_hlist(
            &[
                MathNode::Ordinary('x'),
                MathNode::Ordinary('+'),
                MathNode::Ordinary('y'),
            ],
            &TestWidthProvider,
            MathStyle::Text,
        );

        assert_eq!(items.len(), 5);
        assert!(matches!(items[0], HListItem::Char { codepoint: 'x', .. }));
        assert!(matches!(
            items[1],
            HListItem::Kern {
                width: DimensionValue(2)
            }
        ));
        assert!(matches!(items[2], HListItem::Char { codepoint: '+', .. }));
        assert!(matches!(
            items[3],
            HListItem::Kern {
                width: DimensionValue(2)
            }
        ));
        assert!(matches!(items[4], HListItem::Char { codepoint: 'y', .. }));
    }

    #[test]
    fn fraction_produces_inline_box() {
        let items = math_nodes_to_hlist(
            &[MathNode::Frac {
                numer: vec![MathNode::Ordinary('a')],
                denom: vec![MathNode::Ordinary('b')],
            }],
            &TestWidthProvider,
            MathStyle::Text,
        );

        assert_eq!(items.len(), 1);
        assert!(matches!(
            &items[0],
            HListItem::InlineBox { width, content, .. }
                if *width == DimensionValue(7) && content == "a/b"
        ));
    }

    #[test]
    fn nested_fraction_shrinks_style() {
        let items = math_nodes_to_hlist(
            &[MathNode::Frac {
                numer: vec![MathNode::Frac {
                    numer: vec![MathNode::Ordinary('a')],
                    denom: vec![MathNode::Ordinary('b')],
                }],
                denom: vec![MathNode::Ordinary('c')],
            }],
            &TestWidthProvider,
            MathStyle::Text,
        );

        assert!(matches!(
            &items[0],
            HListItem::InlineBox { width, content, .. }
                if *width == DimensionValue(7) && content == "a/b/c"
        ));
    }

    #[test]
    fn superscript_produces_scaled_inline_box() {
        let items = math_nodes_to_hlist(
            &[
                MathNode::Ordinary('x'),
                MathNode::Superscript(Box::new(MathNode::Ordinary('2'))),
            ],
            &TestWidthProvider,
            MathStyle::Text,
        );

        assert_eq!(items.len(), 2);
        assert!(matches!(items[0], HListItem::Char { codepoint: 'x', .. }));
        assert!(matches!(
            &items[1],
            HListItem::InlineBox { width, content, .. }
                if *width == DimensionValue(7) && content == "2"
        ));
    }

    #[test]
    fn hlist_to_string_round_trips_visible_content() {
        let items = vec![
            HListItem::Char {
                codepoint: 'x',
                width: DimensionValue(10),
                link: None,
                font_index: 0,
            },
            HListItem::Glue {
                width: DimensionValue(10),
                stretch: crate::typesetting::api::GlueComponent::normal(DimensionValue::zero()),
                shrink: crate::typesetting::api::GlueComponent::normal(DimensionValue::zero()),
                link: None,
                font_index: 0,
            },
            HListItem::InlineBox {
                width: DimensionValue(10),
                height: DimensionValue::zero(),
                depth: DimensionValue::zero(),
                content: String::from("y/z"),
            },
            HListItem::Kern {
                width: DimensionValue(5),
            },
        ];

        assert_eq!(hlist_to_string(&items), "x y/z");
    }

    #[test]
    fn hlist_total_width_sums_visible_and_invisible_widths() {
        let items = vec![
            HListItem::Char {
                codepoint: 'x',
                width: DimensionValue(10),
                link: None,
                font_index: 0,
            },
            HListItem::Kern {
                width: DimensionValue(2),
            },
            HListItem::InlineBox {
                width: DimensionValue(7),
                height: DimensionValue::zero(),
                depth: DimensionValue::zero(),
                content: String::from("2"),
            },
            HListItem::Penalty { value: 50 },
        ];

        assert_eq!(hlist_total_width(&items), DimensionValue(19));
    }

    #[test]
    fn thin_space_suppressed_in_script_style() {
        let em = DimensionValue(180); // 10 per mu
        assert_eq!(
            inter_atom_space(MathAtomKind::Ord, MathAtomKind::Op, MathStyle::Script, em),
            DimensionValue(30)
        );
        assert_eq!(
            inter_atom_space(MathAtomKind::Ord, MathAtomKind::Bin, MathStyle::Script, em),
            DimensionValue::zero()
        );
        assert_eq!(
            inter_atom_space(MathAtomKind::Ord, MathAtomKind::Rel, MathStyle::Script, em),
            DimensionValue::zero()
        );
        assert_eq!(
            inter_atom_space(MathAtomKind::Op, MathAtomKind::Op, MathStyle::Script, em),
            DimensionValue(30)
        );
        assert_eq!(
            inter_atom_space(MathAtomKind::Inner, MathAtomKind::Op, MathStyle::Script, em),
            DimensionValue(30)
        );
        assert_eq!(
            inter_atom_space(
                MathAtomKind::Punct,
                MathAtomKind::Ord,
                MathStyle::Script,
                em
            ),
            DimensionValue::zero()
        );
        assert_eq!(
            inter_atom_space(MathAtomKind::Ord, MathAtomKind::Op, MathStyle::Text, em),
            DimensionValue(30)
        );
    }

    #[test]
    fn sqrt_produces_inline_box() {
        let items = math_nodes_to_hlist(
            &[MathNode::Sqrt {
                radicand: vec![MathNode::Ordinary('x')],
                index: None,
            }],
            &TestWidthProvider,
            MathStyle::Text,
        );
        assert_eq!(items.len(), 1);
        assert!(matches!(&items[0], HListItem::InlineBox { content, .. } if content.contains('√')));
    }

    #[test]
    fn sqrt_with_index_includes_index_width() {
        let items_no_idx = math_nodes_to_hlist(
            &[MathNode::Sqrt {
                radicand: vec![MathNode::Ordinary('x')],
                index: None,
            }],
            &TestWidthProvider,
            MathStyle::Text,
        );
        let items_with_idx = math_nodes_to_hlist(
            &[MathNode::Sqrt {
                radicand: vec![MathNode::Ordinary('x')],
                index: Some(vec![MathNode::Ordinary('3')]),
            }],
            &TestWidthProvider,
            MathStyle::Text,
        );
        let w1 = match &items_no_idx[0] {
            HListItem::InlineBox { width, .. } => *width,
            _ => panic!("expected InlineBox"),
        };
        let w2 = match &items_with_idx[0] {
            HListItem::InlineBox { width, .. } => *width,
            _ => panic!("expected InlineBox"),
        };
        assert!(w2 > w1); // index adds width
    }

    #[test]
    fn left_right_emits_delimiter_chars() {
        let items = math_nodes_to_hlist(
            &[MathNode::LeftRight {
                left: "(".to_string(),
                right: ")".to_string(),
                body: vec![MathNode::Ordinary('x')],
            }],
            &TestWidthProvider,
            MathStyle::Text,
        );
        let text = hlist_to_string(&items);
        assert!(text.starts_with('('));
        assert!(text.ends_with(')'));
        assert!(text.contains('x'));
    }

    #[test]
    fn over_under_produces_inline_box() {
        let items = math_nodes_to_hlist(
            &[MathNode::OverUnder {
                kind: crate::parser::api::OverUnderKind::Over,
                base: vec![MathNode::Ordinary('X')],
                annotation: vec![MathNode::Symbol("*".to_string())],
            }],
            &TestWidthProvider,
            MathStyle::Text,
        );
        assert!(items
            .iter()
            .any(|item| matches!(item, HListItem::InlineBox { .. })));
    }

    #[test]
    fn typeset_equation_env_produces_correct_structure() {
        use crate::parser::api::{LineTag, MathLine};
        let lines = vec![MathLine {
            segments: vec![vec![
                MathNode::Ordinary('a'),
                MathNode::Ordinary('='),
                MathNode::Ordinary('b'),
            ]],
            tag: LineTag::Auto,
            display_tag: Some("1".to_string()),
        }];
        let line_width = DimensionValue(720);
        let hlist = super::typeset_equation_env(&lines, &TestWidthProvider, line_width);
        let text = hlist_to_string(&hlist);
        assert!(text.contains('a'));
        assert!(text.contains('b'));
        assert!(text.contains('1')); // tag
    }

    #[test]
    fn typeset_equation_env_multi_column_alignment() {
        use crate::parser::api::{LineTag, MathLine};

        let lines = vec![
            MathLine {
                segments: vec![vec![MathNode::Ordinary('a')], vec![MathNode::Ordinary('b')]],
                tag: LineTag::Auto,
                display_tag: None,
            },
            MathLine {
                segments: vec![
                    vec![MathNode::Ordinary('c'), MathNode::Ordinary('c')],
                    vec![
                        MathNode::Ordinary('d'),
                        MathNode::Ordinary('d'),
                        MathNode::Ordinary('d'),
                    ],
                ],
                tag: LineTag::Auto,
                display_tag: None,
            },
        ];
        let hlist = super::typeset_equation_env(&lines, &TestWidthProvider, DimensionValue(200));

        let mut kerns_by_line = Vec::new();
        let mut current_line = Vec::new();
        for item in hlist {
            match item {
                HListItem::Kern { width } => current_line.push(width),
                HListItem::Penalty { .. } => {
                    if !current_line.is_empty() {
                        kerns_by_line.push(std::mem::take(&mut current_line));
                    }
                }
                _ => {}
            }
        }

        assert_eq!(
            kerns_by_line,
            vec![
                vec![
                    DimensionValue(55),
                    DimensionValue(10),
                    DimensionValue(40),
                    DimensionValue(20),
                ],
                vec![DimensionValue(55), DimensionValue(40)],
            ]
        );
    }
}
