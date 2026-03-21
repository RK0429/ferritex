use super::{
    hyphenation::{Hyphenator, TexPatternHyphenator},
    knuth_plass::BreakParams,
    line_breaker,
};

use crate::font::api::TfmMetrics;
use crate::kernel::api::DimensionValue;
use crate::parser::api::{DocumentNode, MathNode, ParsedDocument};

const SCALED_POINTS_PER_POINT: i64 = 65_536;
const PAGE_WIDTH_PT: i64 = 612;
const PAGE_HEIGHT_PT: i64 = 792;
const TOP_MARGIN_PT: i64 = 72;
const BOTTOM_MARGIN_PT: i64 = 72;
const LINE_HEIGHT_PT: i64 = 18;
const MAX_LINE_CHARS: usize = 70;
const LINE_WIDTH_SAMPLE: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
#[cfg(test)]
const MAX_LINE_WIDTH: DimensionValue =
    DimensionValue(MAX_LINE_CHARS as i64 * SCALED_POINTS_PER_POINT);

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
pub struct TeXBox {
    pub width: DimensionValue,
    pub height: DimensionValue,
    pub depth: DimensionValue,
}

impl TeXBox {
    pub const fn zero() -> Self {
        Self::new(
            DimensionValue::zero(),
            DimensionValue::zero(),
            DimensionValue::zero(),
        )
    }

    pub const fn with_height(height: DimensionValue) -> Self {
        Self::new(DimensionValue::zero(), height, DimensionValue::zero())
    }

    pub const fn new(width: DimensionValue, height: DimensionValue, depth: DimensionValue) -> Self {
        Self {
            width,
            height,
            depth,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GlueOrder {
    #[default]
    Normal,
    Fil,
    Fill,
    Filll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlueComponent {
    pub value: DimensionValue,
    pub order: GlueOrder,
}

impl GlueComponent {
    pub fn normal(value: DimensionValue) -> Self {
        Self {
            value,
            order: GlueOrder::Normal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HListItem {
    Char {
        codepoint: char,
        width: DimensionValue,
    },
    Glue {
        width: DimensionValue,
        stretch: GlueComponent,
        shrink: GlueComponent,
    },
    Kern {
        width: DimensionValue,
    },
    Penalty {
        value: i32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VListItem {
    Box { tex_box: TeXBox, content: String },
    Glue { height: DimensionValue },
    Penalty { value: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HBox {
    pub tex_box: TeXBox,
    pub content: Vec<HListItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VBox {
    pub tex_box: TeXBox,
    pub content: Vec<VListItem>,
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
        let provider = default_fixed_width_provider();
        self.typeset_with_provider(document, &provider)
    }

    pub fn typeset_with_provider(
        &self,
        document: &ParsedDocument,
        provider: &dyn CharWidthProvider,
    ) -> TypesetDocument {
        let page_box = page_box_for_class(&document.document_class);
        let nodes = document.body_nodes();
        let params = break_params_for_provider(provider);
        let hyphenator = TexPatternHyphenator::english();
        let vlist =
            document_nodes_to_vlist_with_config(&nodes, provider, Some(&hyphenator), &params);
        let pages = paginate_vlist(&vlist, &page_box);

        TypesetDocument { pages }
    }
}

fn break_params_for_provider(provider: &dyn CharWidthProvider) -> BreakParams {
    let sample_count = LINE_WIDTH_SAMPLE.chars().count() as i64;
    let sample_width_sum = LINE_WIDTH_SAMPLE
        .chars()
        .map(|codepoint| provider.char_width(codepoint).0)
        .sum::<i64>();
    let average_char_width = (sample_width_sum / sample_count)
        .max(provider.space_width().0)
        .max(SCALED_POINTS_PER_POINT);

    BreakParams {
        line_width: DimensionValue(average_char_width * MAX_LINE_CHARS as i64),
        hyphen_penalty: BreakParams::default().hyphen_penalty,
        ..BreakParams::default()
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
    document_nodes_to_hlist_with_config(
        nodes,
        provider,
        None,
        BreakParams::default().hyphen_penalty,
    )
}

pub fn document_nodes_to_hlist_with_hyphenation(
    nodes: &[DocumentNode],
    provider: &dyn CharWidthProvider,
    hyphenator: &dyn Hyphenator,
) -> Vec<HListItem> {
    document_nodes_to_hlist_with_config(
        nodes,
        provider,
        Some(hyphenator),
        BreakParams::default().hyphen_penalty,
    )
}

fn document_nodes_to_hlist_with_config(
    nodes: &[DocumentNode],
    provider: &dyn CharWidthProvider,
    hyphenator: Option<&dyn Hyphenator>,
    hyphen_penalty: i32,
) -> Vec<HListItem> {
    let space_width = provider.space_width();
    let stretch = GlueComponent::normal(DimensionValue(space_width.0 / 2));
    let shrink = GlueComponent::normal(DimensionValue(space_width.0 / 3));
    let mut hlist = Vec::new();
    let mut current_word = String::new();
    let mut current_word_items = Vec::new();

    for node in nodes {
        match node {
            DocumentNode::Text(text) => {
                for codepoint in text.chars() {
                    match codepoint {
                        '\n' => {
                            flush_word(
                                &mut hlist,
                                &mut current_word,
                                &mut current_word_items,
                                hyphenator,
                                hyphen_penalty,
                            );
                            hlist.push(HListItem::Penalty {
                                value: PENALTY_FORCED,
                            });
                        }
                        codepoint if codepoint.is_whitespace() => {
                            flush_word(
                                &mut hlist,
                                &mut current_word,
                                &mut current_word_items,
                                hyphenator,
                                hyphen_penalty,
                            );
                            hlist.push(HListItem::Glue {
                                width: space_width,
                                stretch,
                                shrink,
                            });
                        }
                        codepoint => {
                            current_word.push(codepoint);
                            current_word_items.push((codepoint, provider.char_width(codepoint)));
                        }
                    }
                }
            }
            DocumentNode::HBox(children) | DocumentNode::VBox(children) => {
                flush_word(
                    &mut hlist,
                    &mut current_word,
                    &mut current_word_items,
                    hyphenator,
                    hyphen_penalty,
                );
                hlist.extend(document_nodes_to_hlist_with_config(
                    children,
                    provider,
                    hyphenator,
                    hyphen_penalty,
                ));
            }
            DocumentNode::InlineMath(nodes) => {
                flush_word(
                    &mut hlist,
                    &mut current_word,
                    &mut current_word_items,
                    hyphenator,
                    hyphen_penalty,
                );
                append_literal_text_to_hlist(
                    &mut hlist,
                    &render_math_nodes(nodes),
                    provider,
                    space_width,
                    stretch,
                    shrink,
                );
            }
            DocumentNode::DisplayMath(nodes) => {
                flush_word(
                    &mut hlist,
                    &mut current_word,
                    &mut current_word_items,
                    hyphenator,
                    hyphen_penalty,
                );
                push_forced_break_if_needed(&mut hlist);
                append_literal_text_to_hlist(
                    &mut hlist,
                    &render_math_nodes(nodes),
                    provider,
                    space_width,
                    stretch,
                    shrink,
                );
                push_forced_break_if_needed(&mut hlist);
            }
            DocumentNode::ParBreak => {
                flush_word(
                    &mut hlist,
                    &mut current_word,
                    &mut current_word_items,
                    hyphenator,
                    hyphen_penalty,
                );
                hlist.push(HListItem::Penalty {
                    value: PENALTY_FORCED,
                });
                hlist.push(HListItem::Penalty {
                    value: PENALTY_FORCED,
                });
            }
            DocumentNode::PageBreak => {
                flush_word(
                    &mut hlist,
                    &mut current_word,
                    &mut current_word_items,
                    hyphenator,
                    hyphen_penalty,
                );
                hlist.push(HListItem::Penalty {
                    value: PENALTY_FORCED,
                });
            }
        }
    }

    flush_word(
        &mut hlist,
        &mut current_word,
        &mut current_word_items,
        hyphenator,
        hyphen_penalty,
    );

    hlist
}

fn document_nodes_to_vlist_with_config(
    nodes: &[DocumentNode],
    provider: &dyn CharWidthProvider,
    hyphenator: Option<&dyn Hyphenator>,
    params: &BreakParams,
) -> Vec<VListItem> {
    let mut vlist = Vec::new();
    let mut segment_start = 0;

    for (index, node) in nodes.iter().enumerate() {
        if matches!(node, DocumentNode::PageBreak) {
            append_nodes_segment_to_vlist(
                &mut vlist,
                &nodes[segment_start..index],
                provider,
                hyphenator,
                params,
            );
            vlist.push(VListItem::Penalty {
                value: PENALTY_FORCED,
            });
            segment_start = index + 1;
        }
    }

    append_nodes_segment_to_vlist(
        &mut vlist,
        &nodes[segment_start..],
        provider,
        hyphenator,
        params,
    );

    vlist
}

fn append_nodes_segment_to_vlist(
    vlist: &mut Vec<VListItem>,
    nodes: &[DocumentNode],
    provider: &dyn CharWidthProvider,
    hyphenator: Option<&dyn Hyphenator>,
    params: &BreakParams,
) {
    if nodes.is_empty() {
        return;
    }

    let hlist =
        document_nodes_to_hlist_with_config(nodes, provider, hyphenator, params.hyphen_penalty);
    if hlist.is_empty() {
        return;
    }

    let wrapped_lines = line_breaker::break_paragraph(&hlist, params);
    vlist.extend(lines_to_vlist(&wrapped_lines));
}

fn render_math_nodes(nodes: &[MathNode]) -> String {
    nodes
        .iter()
        .map(render_math_node)
        .collect::<Vec<_>>()
        .join("")
}

fn render_math_node(node: &MathNode) -> String {
    match node {
        MathNode::Ordinary(ch) => ch.to_string(),
        MathNode::Superscript(node) => format!("^{}", render_math_attachment(node)),
        MathNode::Subscript(node) => format!("_{}", render_math_attachment(node)),
        MathNode::Frac { numer, denom } => {
            format!(
                "({})/({})",
                render_math_nodes(numer),
                render_math_nodes(denom)
            )
        }
        MathNode::Group(nodes) => render_math_nodes(nodes),
    }
}

fn render_math_attachment(node: &MathNode) -> String {
    match node {
        MathNode::Group(nodes) if nodes.len() > 1 => format!("({})", render_math_nodes(nodes)),
        _ => render_math_node(node),
    }
}

fn append_literal_text_to_hlist(
    hlist: &mut Vec<HListItem>,
    text: &str,
    provider: &dyn CharWidthProvider,
    space_width: DimensionValue,
    stretch: GlueComponent,
    shrink: GlueComponent,
) {
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

fn push_forced_break_if_needed(hlist: &mut Vec<HListItem>) {
    if matches!(
        hlist.last(),
        Some(HListItem::Penalty { value }) if *value <= PENALTY_FORCED
    ) {
        return;
    }

    hlist.push(HListItem::Penalty {
        value: PENALTY_FORCED,
    });
}

fn flush_word(
    hlist: &mut Vec<HListItem>,
    word: &mut String,
    word_items: &mut Vec<(char, DimensionValue)>,
    hyphenator: Option<&dyn Hyphenator>,
    hyphen_penalty: i32,
) {
    if word_items.is_empty() {
        return;
    }

    let hyphen_points = hyphenator
        .map(|hyphenator| hyphenator.hyphenate(word))
        .unwrap_or_default();
    let mut next_hyphen = hyphen_points.iter().copied().peekable();
    let mut byte_offset = 0;

    for (index, (codepoint, width)) in word_items.iter().copied().enumerate() {
        hlist.push(HListItem::Char { codepoint, width });
        byte_offset += codepoint.len_utf8();

        if index + 1 == word_items.len() {
            continue;
        }

        if next_hyphen.peek().copied() == Some(byte_offset) {
            hlist.push(HListItem::Penalty {
                value: hyphen_penalty,
            });
            next_hyphen.next();
        }
    }

    word.clear();
    word_items.clear();
}

fn lines_to_vlist(lines: &[String]) -> Vec<VListItem> {
    lines
        .iter()
        .map(|line| VListItem::Box {
            tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
            content: line.clone(),
        })
        .collect()
}

fn paginate_vlist(vlist: &[VListItem], page_box: &PageBox) -> Vec<TypesetPage> {
    let content_height = page_box.height - points(TOP_MARGIN_PT) - points(BOTTOM_MARGIN_PT);

    if vlist.is_empty() {
        return vec![TypesetPage {
            lines: Vec::new(),
            page_box: page_box.clone(),
        }];
    }

    let mut pages = Vec::new();
    let mut current_page = Vec::new();
    let mut current_height = DimensionValue::zero();
    let mut best_break_candidate: Option<VListBreakCandidate> = None;

    for item in vlist {
        if matches!(
            item,
            VListItem::Penalty { value } if *value <= PENALTY_FORCED
        ) {
            pages.push(typeset_page_from_vlist(&current_page, page_box));
            current_page.clear();
            current_height = DimensionValue::zero();
            best_break_candidate = None;
            continue;
        }

        let item_height = vlist_item_height(item);
        if !current_page.is_empty() && current_height + item_height > content_height {
            if let Some(candidate) = best_break_candidate {
                let trailing_items = current_page.split_off(candidate.split_after);
                pages.push(typeset_page_from_vlist(&current_page, page_box));
                current_page = trailing_items;
                current_height = vlist_total_height(&current_page);
                best_break_candidate = find_best_break_candidate(&current_page);
            } else {
                pages.push(typeset_page_from_vlist(&current_page, page_box));
                current_page.clear();
                current_height = DimensionValue::zero();
            }

            if !current_page.is_empty() && current_height + item_height > content_height {
                pages.push(typeset_page_from_vlist(&current_page, page_box));
                current_page.clear();
                current_height = DimensionValue::zero();
                best_break_candidate = None;
            }
        }

        current_page.push(item.clone());
        current_height = current_height + item_height;
        if let VListItem::Penalty { value } = item {
            maybe_record_break_candidate(&mut best_break_candidate, current_page.len(), *value);
        }
    }

    if current_page.is_empty() && !pages.is_empty() {
        return pages;
    }

    pages.push(typeset_page_from_vlist(&current_page, page_box));
    pages
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VListBreakCandidate {
    split_after: usize,
    penalty: i32,
}

fn maybe_record_break_candidate(
    candidate: &mut Option<VListBreakCandidate>,
    split_after: usize,
    penalty: i32,
) {
    if penalty <= PENALTY_FORCED || penalty >= PENALTY_FORBIDDEN {
        return;
    }

    let next_candidate = VListBreakCandidate {
        split_after,
        penalty,
    };

    if candidate.is_none_or(|current| {
        penalty < current.penalty
            || (penalty == current.penalty && split_after > current.split_after)
    }) {
        *candidate = Some(next_candidate);
    }
}

fn find_best_break_candidate(items: &[VListItem]) -> Option<VListBreakCandidate> {
    let mut candidate = None;
    for (index, item) in items.iter().enumerate() {
        if let VListItem::Penalty { value } = item {
            maybe_record_break_candidate(&mut candidate, index + 1, *value);
        }
    }
    candidate
}

fn vlist_total_height(items: &[VListItem]) -> DimensionValue {
    items.iter().fold(DimensionValue::zero(), |height, item| {
        height + vlist_item_height(item)
    })
}

fn vlist_item_height(item: &VListItem) -> DimensionValue {
    match item {
        VListItem::Box { tex_box, .. } => tex_box.height + tex_box.depth,
        VListItem::Glue { height } => *height,
        VListItem::Penalty { .. } => DimensionValue::zero(),
    }
}

fn typeset_page_from_vlist(items: &[VListItem], page_box: &PageBox) -> TypesetPage {
    let mut lines = Vec::new();
    let mut consumed_height = DimensionValue::zero();

    for item in items {
        match item {
            VListItem::Box { tex_box, content } => {
                lines.push(TextLine {
                    text: content.clone(),
                    y: page_box.height - points(TOP_MARGIN_PT) - consumed_height,
                });
                consumed_height = consumed_height + tex_box.height + tex_box.depth;
            }
            VListItem::Glue { height } => {
                consumed_height = consumed_height + *height;
            }
            VListItem::Penalty { .. } => {}
        }
    }

    TypesetPage {
        lines,
        page_box: page_box.clone(),
    }
}

#[cfg(test)]
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
                current_word.push(WordSegment::Char {
                    codepoint: *codepoint,
                    width: *width,
                });
                last_item_forced_break = false;
            }
            HListItem::Kern { width } => {
                current_word.push(WordSegment::Kern { width: *width });
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordSegment {
    Char {
        codepoint: char,
        width: DimensionValue,
    },
    Kern {
        width: DimensionValue,
    },
}

#[cfg(test)]
impl WordSegment {
    fn width(self) -> DimensionValue {
        match self {
            Self::Char { width, .. } | Self::Kern { width } => width,
        }
    }
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

#[cfg(test)]
fn append_word_to_line(
    lines: &mut Vec<String>,
    current_line: &mut String,
    current_line_width: &mut DimensionValue,
    current_word: &mut Vec<WordSegment>,
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

#[cfg(test)]
fn split_long_word_to_lines(
    lines: &mut Vec<String>,
    current_line: &mut String,
    current_line_width: &mut DimensionValue,
    word: &[WordSegment],
    max_line_width: DimensionValue,
) {
    let mut chunk = String::new();
    let mut chunk_width = DimensionValue::zero();

    for segment in word {
        let width = segment.width();
        if !chunk.is_empty() && chunk_width + width > max_line_width {
            lines.push(std::mem::take(&mut chunk));
            chunk_width = DimensionValue::zero();
        }

        if let WordSegment::Char { codepoint, .. } = segment {
            chunk.push(*codepoint);
        }
        chunk_width = chunk_width + width;

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

#[cfg(test)]
fn word_width(word: &[WordSegment]) -> DimensionValue {
    word.iter().fold(DimensionValue::zero(), |width, segment| {
        width + segment.width()
    })
}

#[cfg(test)]
fn word_to_string(word: &[WordSegment]) -> String {
    word.iter()
        .filter_map(|segment| match segment {
            WordSegment::Char { codepoint, .. } => Some(*codepoint),
            WordSegment::Kern { .. } => None,
        })
        .collect()
}

fn points(value: i64) -> DimensionValue {
    DimensionValue(value * SCALED_POINTS_PER_POINT)
}

#[cfg(test)]
mod tests {
    use super::{
        default_fixed_width_provider, document_nodes_to_hlist,
        document_nodes_to_hlist_with_hyphenation, document_nodes_to_vlist_with_config,
        page_box_for_class, paginate_vlist, points, vlist_item_height, wrap_body, wrap_hlist,
        CharWidthProvider, GlueComponent, GlueOrder, HBox, HListItem, MinimalTypesetter, TeXBox,
        TextLine, TfmWidthProvider, VBox, VListItem, LINE_HEIGHT_PT, MAX_LINE_CHARS,
        MAX_LINE_WIDTH, PAGE_HEIGHT_PT, PENALTY_FORBIDDEN, PENALTY_FORCED, TOP_MARGIN_PT,
    };
    use crate::font::api::TfmMetrics;
    use crate::kernel::api::DimensionValue;
    use crate::parser::api::{DocumentNode, MathNode, MinimalLatexParser, ParsedDocument, Parser};
    use crate::typesetting::{
        hyphenation::TexPatternHyphenator, knuth_plass::BreakParams, line_breaker,
    };

    fn parsed_document(body: &str) -> ParsedDocument {
        ParsedDocument {
            document_class: "article".to_string(),
            package_count: 0,
            body: body.to_string(),
            labels: Default::default(),
            has_unresolved_refs: false,
        }
    }

    fn parsed_latex_document(body: &str) -> ParsedDocument {
        MinimalLatexParser
            .parse(&format!(
                "\\documentclass{{article}}\n\\begin{{document}}\n{body}\n\\end{{document}}\n"
            ))
            .expect("parse document")
    }

    struct SkewedWidthProvider;

    impl CharWidthProvider for SkewedWidthProvider {
        fn char_width(&self, codepoint: char) -> DimensionValue {
            match codepoint {
                'A' => points(200),
                _ => points(1),
            }
        }

        fn space_width(&self) -> DimensionValue {
            points(1)
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
    fn paginate_vlist_empty_produces_single_page() {
        let pages = paginate_vlist(&[], &page_box_for_class("article"));

        assert_eq!(pages.len(), 1);
        assert!(pages[0].lines.is_empty());
    }

    #[test]
    fn height_based_pagination_respects_content_area() {
        let mut vlist = (1..=35)
            .map(|index| VListItem::Box {
                tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
                content: format!("Line {index}"),
            })
            .collect::<Vec<_>>();
        vlist.push(VListItem::Glue {
            height: points(LINE_HEIGHT_PT),
        });
        vlist.push(VListItem::Box {
            tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
            content: "Overflow".to_string(),
        });

        let pages = paginate_vlist(&vlist, &page_box_for_class("article"));

        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].lines.len(), 35);
        assert_eq!(pages[1].lines.len(), 1);
        assert_eq!(pages[1].lines[0].text, "Overflow");
    }

    #[test]
    fn mixed_height_lines_break_by_accumulated_height() {
        let mut vlist = (1..=16)
            .map(|index| VListItem::Box {
                tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT * 2)),
                content: format!("Tall {index}"),
            })
            .collect::<Vec<_>>();
        vlist.extend((1..=5).map(|index| VListItem::Box {
            tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
            content: format!("Short {index}"),
        }));

        let pages = paginate_vlist(&vlist, &page_box_for_class("article"));

        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].lines.len(), 20);
        assert_eq!(pages[1].lines.len(), 1);
        assert_eq!(
            pages[0].lines[1].y,
            points(PAGE_HEIGHT_PT - TOP_MARGIN_PT - (LINE_HEIGHT_PT * 2))
        );
        assert_eq!(pages[1].lines[0].text, "Short 5");
        assert_eq!(pages[1].lines[0].y, points(PAGE_HEIGHT_PT - TOP_MARGIN_PT));
    }

    #[test]
    fn vlist_penalty_forced_forces_page_break() {
        let vlist = vec![
            VListItem::Box {
                tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
                content: "First".to_string(),
            },
            VListItem::Penalty {
                value: PENALTY_FORCED,
            },
            VListItem::Box {
                tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
                content: "Second".to_string(),
            },
        ];

        let pages = paginate_vlist(&vlist, &page_box_for_class("article"));

        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].lines[0].text, "First");
        assert_eq!(pages[1].lines[0].text, "Second");
    }

    #[test]
    fn penalty_candidate_is_used_when_page_overflows() {
        let mut vlist = (1..=34)
            .map(|index| VListItem::Box {
                tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
                content: format!("Line {index}"),
            })
            .collect::<Vec<_>>();
        vlist.push(VListItem::Penalty { value: 50 });
        vlist.extend((35..=37).map(|index| VListItem::Box {
            tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
            content: format!("Line {index}"),
        }));

        let pages = paginate_vlist(&vlist, &page_box_for_class("article"));

        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].lines.len(), 34);
        assert_eq!(pages[0].lines[33].text, "Line 34");
        assert_eq!(
            pages[1]
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["Line 35", "Line 36", "Line 37"]
        );
    }

    #[test]
    fn forbidden_penalty_is_not_used_as_page_break_candidate() {
        let mut vlist = (1..=34)
            .map(|index| VListItem::Box {
                tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
                content: format!("Line {index}"),
            })
            .collect::<Vec<_>>();
        vlist.push(VListItem::Penalty {
            value: PENALTY_FORBIDDEN,
        });
        vlist.extend((35..=37).map(|index| VListItem::Box {
            tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
            content: format!("Line {index}"),
        }));

        let pages = paginate_vlist(&vlist, &page_box_for_class("article"));

        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].lines.len(), 36);
        assert_eq!(pages[1].lines.len(), 1);
        assert_eq!(pages[1].lines[0].text, "Line 37");
    }

    #[test]
    fn tex_box_zero_has_all_zero_dimensions() {
        assert_eq!(
            TeXBox::zero(),
            TeXBox::new(
                DimensionValue::zero(),
                DimensionValue::zero(),
                DimensionValue::zero()
            )
        );
    }

    #[test]
    fn tex_box_with_height_sets_only_height() {
        assert_eq!(
            TeXBox::with_height(points(LINE_HEIGHT_PT)),
            TeXBox::new(
                DimensionValue::zero(),
                points(LINE_HEIGHT_PT),
                DimensionValue::zero()
            )
        );
    }

    #[test]
    fn tex_box_new_sets_all_dimensions() {
        assert_eq!(
            TeXBox::new(points(10), points(11), points(12)),
            TeXBox {
                width: points(10),
                height: points(11),
                depth: points(12),
            }
        );
    }

    #[test]
    fn hbox_stores_dimensions_and_content() {
        let content = vec![HListItem::Char {
            codepoint: 'A',
            width: points(1),
        }];
        let hbox = HBox {
            tex_box: TeXBox::new(points(10), points(11), points(12)),
            content: content.clone(),
        };

        assert_eq!(hbox.tex_box.width, points(10));
        assert_eq!(hbox.content, content);
    }

    #[test]
    fn vbox_stores_dimensions_and_content() {
        let content = vec![VListItem::Box {
            tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
            content: "Line".to_string(),
        }];
        let vbox = VBox {
            tex_box: TeXBox::new(points(10), points(11), points(12)),
            content: content.clone(),
        };

        assert_eq!(vbox.tex_box.depth, points(12));
        assert_eq!(vbox.content, content);
    }

    #[test]
    fn vlist_box_item_uses_tex_box_dimensions() {
        let item = VListItem::Box {
            tex_box: TeXBox::new(points(10), points(11), points(12)),
            content: "Line".to_string(),
        };

        assert_eq!(vlist_item_height(&item), points(23));
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
    fn typeset_with_provider_uses_custom_character_widths() {
        let document =
            MinimalTypesetter.typeset_with_provider(&parsed_document("AA"), &SkewedWidthProvider);

        assert_eq!(
            document.pages[0]
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "A"]
        );
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
                    stretch: GlueComponent::normal(DimensionValue(points(1).0 / 2)),
                    shrink: GlueComponent::normal(DimensionValue(points(1).0 / 3)),
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
    fn document_nodes_to_hlist_renders_inline_math_readably() {
        let hlist = document_nodes_to_hlist(
            &[
                DocumentNode::Text("Area ".to_string()),
                DocumentNode::InlineMath(vec![
                    MathNode::Ordinary('x'),
                    MathNode::Superscript(Box::new(MathNode::Ordinary('2'))),
                ]),
            ],
            &default_fixed_width_provider(),
        );

        assert_eq!(
            wrap_hlist(&hlist, MAX_LINE_WIDTH),
            vec!["Area x^2".to_string()]
        );
    }

    #[test]
    fn document_nodes_to_hlist_puts_display_math_on_separate_lines() {
        let hlist = document_nodes_to_hlist(
            &[
                DocumentNode::Text("Before".to_string()),
                DocumentNode::DisplayMath(vec![
                    MathNode::Ordinary('a'),
                    MathNode::Subscript(Box::new(MathNode::Ordinary('1'))),
                ]),
                DocumentNode::Text("After".to_string()),
            ],
            &default_fixed_width_provider(),
        );

        assert_eq!(
            wrap_hlist(&hlist, MAX_LINE_WIDTH),
            vec!["Before".to_string(), "a_1".to_string(), "After".to_string()]
        );
    }

    #[test]
    fn minimal_typesetter_renders_inline_and_display_math_lines() {
        let document = MinimalTypesetter.typeset(&parsed_latex_document(
            "Inline $x^2$.\n\\[\\frac{a}{b}\\]\nAfter",
        ));

        assert_eq!(
            document.pages[0]
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["Inline x^2.", "(a)/(b)", "After"]
        );
    }

    #[test]
    fn document_nodes_to_hlist_with_hyphenation_inserts_penalties_inside_words() {
        let hyphenator = TexPatternHyphenator::english();
        let hlist = document_nodes_to_hlist_with_hyphenation(
            &[DocumentNode::Text("basket".to_string())],
            &default_fixed_width_provider(),
            &hyphenator,
        );

        assert_eq!(
            hlist,
            vec![
                HListItem::Char {
                    codepoint: 'b',
                    width: points(1),
                },
                HListItem::Char {
                    codepoint: 'a',
                    width: points(1),
                },
                HListItem::Char {
                    codepoint: 's',
                    width: points(1),
                },
                HListItem::Penalty { value: 50 },
                HListItem::Char {
                    codepoint: 'k',
                    width: points(1),
                },
                HListItem::Char {
                    codepoint: 'e',
                    width: points(1),
                },
                HListItem::Char {
                    codepoint: 't',
                    width: points(1),
                },
            ]
        );
    }

    #[test]
    fn line_breaker_matches_char_counting_for_explicit_breaks() {
        let body = format!(
            "{}\n{}",
            "a".repeat(71),
            "Ferritex wraps this line using the same output as before"
        );
        let nodes = parsed_document(&body).body_nodes();
        let hlist = document_nodes_to_hlist(&nodes, &default_fixed_width_provider());
        let params = BreakParams {
            line_width: MAX_LINE_WIDTH,
            ..BreakParams::default()
        };

        assert_eq!(
            line_breaker::break_paragraph(&hlist, &params),
            wrap_body(&body)
        );
    }

    #[test]
    fn explicit_par_break_produces_blank_output_line() {
        let document =
            MinimalTypesetter.typeset(&parsed_document(r"First paragraph\par Second paragraph"));

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
    fn hyphenation_changes_line_break_output_for_long_word() {
        let nodes = parsed_document("basket").body_nodes();
        let provider = default_fixed_width_provider();
        let hyphenator = TexPatternHyphenator::english();
        let params = BreakParams {
            line_width: points(3),
            ..BreakParams::default()
        };
        let plain_hlist = document_nodes_to_hlist(&nodes, &provider);
        let hyphenated_hlist =
            document_nodes_to_hlist_with_hyphenation(&nodes, &provider, &hyphenator);

        assert_eq!(
            line_breaker::break_paragraph(&plain_hlist, &params),
            vec!["bas".to_string(), "ket".to_string()]
        );
        assert_eq!(
            line_breaker::break_paragraph(&hyphenated_hlist, &params),
            vec!["bas-".to_string(), "ket".to_string()]
        );
    }

    #[test]
    fn short_word_wrapping_is_unchanged_with_hyphenation_enabled() {
        let nodes = parsed_document("ship yard").body_nodes();
        let provider = default_fixed_width_provider();
        let hyphenator = TexPatternHyphenator::english();
        let params = BreakParams {
            line_width: points(9),
            ..BreakParams::default()
        };
        let plain_hlist = document_nodes_to_hlist(&nodes, &provider);
        let hyphenated_hlist =
            document_nodes_to_hlist_with_hyphenation(&nodes, &provider, &hyphenator);

        assert_eq!(hyphenated_hlist, plain_hlist);
        assert_eq!(
            line_breaker::break_paragraph(&hyphenated_hlist, &params),
            line_breaker::break_paragraph(&plain_hlist, &params)
        );
    }

    #[test]
    fn minimal_typesetter_uses_tex_pattern_hyphenation_by_default() {
        let body = "basket".repeat(12);
        let parsed = parsed_document(&body);
        let document = MinimalTypesetter.typeset(&parsed);
        let provider = default_fixed_width_provider();
        let params = super::break_params_for_provider(&provider);
        let nodes = parsed.body_nodes();
        let plain_vlist = document_nodes_to_vlist_with_config(&nodes, &provider, None, &params);
        let plain_document = super::TypesetDocument {
            pages: paginate_vlist(&plain_vlist, &page_box_for_class(&parsed.document_class)),
        };
        let hyphenated_lines = document.pages[0]
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>();
        let plain_lines = plain_document.pages[0]
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>();

        assert_ne!(hyphenated_lines, plain_lines);
        assert!(hyphenated_lines.iter().any(|line| line.ends_with('-')));
        assert!(plain_lines.iter().all(|line| !line.ends_with('-')));
    }

    #[test]
    fn line_breaker_uses_knuth_plass_breaks_for_mixed_overfull_paragraphs() {
        let body = format!(
            "intro {} tail words\n\nprefix short {} end",
            "a".repeat(71),
            "b".repeat(72)
        );
        let nodes = parsed_document(&body).body_nodes();
        let hlist = document_nodes_to_hlist(&nodes, &default_fixed_width_provider());
        let params = BreakParams {
            line_width: MAX_LINE_WIDTH,
            ..BreakParams::default()
        };

        assert_eq!(
            line_breaker::break_paragraph(&hlist, &params),
            vec![
                format!("intro {} tail words", "a".repeat(71)),
                String::new(),
                format!("prefix short {} end", "b".repeat(72)),
            ]
        );
    }

    #[test]
    fn kp_produces_different_split_from_greedy() {
        let hlist = vec![
            HListItem::Char {
                codepoint: 'a',
                width: points(10),
            },
            HListItem::Glue {
                width: points(1),
                stretch: GlueComponent::normal(points(60)),
                shrink: GlueComponent::normal(points(1)),
            },
            HListItem::Char {
                codepoint: 'b',
                width: points(10),
            },
            HListItem::Glue {
                width: points(1),
                stretch: GlueComponent::normal(points(60)),
                shrink: GlueComponent::normal(points(1)),
            },
            HListItem::Char {
                codepoint: 'c',
                width: points(10),
            },
            HListItem::Penalty { value: 100 },
            HListItem::Glue {
                width: points(1),
                stretch: GlueComponent::normal(points(60)),
                shrink: GlueComponent::normal(points(1)),
            },
            HListItem::Char {
                codepoint: 'd',
                width: points(10),
            },
            HListItem::Glue {
                width: points(1),
                stretch: GlueComponent::normal(points(60)),
                shrink: GlueComponent::normal(points(1)),
            },
            HListItem::Char {
                codepoint: 'e',
                width: points(10),
            },
        ];
        let params = BreakParams {
            line_width: points(32),
            ..BreakParams::default()
        };

        assert_eq!(
            wrap_hlist(&hlist, points(32)),
            vec!["a b c".to_string(), "d e".to_string()]
        );
        assert_eq!(
            line_breaker::break_paragraph(&hlist, &params),
            vec!["a b".to_string(), "c d e".to_string()]
        );
    }

    #[test]
    fn kp_par_break_produces_blank_line() {
        let nodes = parsed_document(r"First paragraph\par Second paragraph").body_nodes();
        let hlist = document_nodes_to_hlist(&nodes, &default_fixed_width_provider());
        let params = BreakParams {
            line_width: MAX_LINE_WIDTH,
            ..BreakParams::default()
        };

        assert_eq!(
            line_breaker::break_paragraph(&hlist, &params),
            vec![
                "First paragraph".to_string(),
                String::new(),
                "Second paragraph".to_string(),
            ]
        );
    }

    #[test]
    fn pagebreak_command_produces_page_break() {
        let document =
            MinimalTypesetter.typeset(&parsed_latex_document("First\n\\pagebreak\nSecond"));

        assert_eq!(document.pages.len(), 2);
        assert_eq!(document.pages[0].lines[0].text, "First");
        assert_eq!(document.pages[1].lines[0].text, "Second");
    }

    #[test]
    fn newpage_command_produces_page_break() {
        let document =
            MinimalTypesetter.typeset(&parsed_latex_document("First\n\\newpage\nSecond"));

        assert_eq!(document.pages.len(), 2);
        assert_eq!(document.pages[0].lines[0].text, "First");
        assert_eq!(document.pages[1].lines[0].text, "Second");
    }

    #[test]
    fn clearpage_command_produces_page_break() {
        let document =
            MinimalTypesetter.typeset(&parsed_latex_document("First\n\\clearpage\nSecond"));

        assert_eq!(document.pages.len(), 2);
        assert_eq!(document.pages[0].lines[0].text, "First");
        assert_eq!(document.pages[1].lines[0].text, "Second");
    }

    #[test]
    fn hbox_content_appears_in_typeset_output() {
        let document = MinimalTypesetter.typeset(&parsed_latex_document(r"\hbox{hello}"));

        assert_eq!(document.pages.len(), 1);
        assert_eq!(
            document.pages[0]
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["hello"]
        );
    }

    #[test]
    fn vbox_content_appears_in_typeset_output() {
        let document = MinimalTypesetter.typeset(&parsed_latex_document(r"\vbox{content}"));

        assert_eq!(document.pages.len(), 1);
        assert_eq!(
            document.pages[0]
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["content"]
        );
    }

    #[test]
    fn hbox_in_paragraph_flows_inline() {
        let document =
            MinimalTypesetter.typeset(&parsed_latex_document(r"Before \hbox{middle} after"));

        assert_eq!(document.pages.len(), 1);
        assert_eq!(document.pages[0].lines[0].text, "Before middle after");
    }

    #[test]
    fn pagebreak_in_middle_of_content_splits_pages() {
        let before = (1..=10)
            .map(|index| format!("Before {index}"))
            .collect::<Vec<_>>()
            .join(r"\par ");
        let after = (1..=10)
            .map(|index| format!("After {index}"))
            .collect::<Vec<_>>()
            .join(r"\par ");
        let document = MinimalTypesetter.typeset(&parsed_latex_document(&format!(
            "{before}\n\\pagebreak\n{after}"
        )));

        assert_eq!(document.pages.len(), 2);
        assert!(document.pages[0].lines.len() > 1);
        assert!(document.pages[1].lines.len() > 1);
        assert_eq!(document.pages[0].lines[0].text, "Before 1");
        assert_eq!(document.pages[1].lines[0].text, "After 1");
    }

    #[test]
    fn kp_long_paragraph_paginates_correctly() {
        let word = "a".repeat(MAX_LINE_CHARS);
        let body = (0..60).map(|_| word.as_str()).collect::<Vec<_>>().join(" ");

        let document = MinimalTypesetter.typeset(&parsed_document(&body));
        let total_lines = document
            .pages
            .iter()
            .map(|page| page.lines.len())
            .sum::<usize>();

        assert_eq!(document.pages.len(), 2);
        assert_eq!(document.pages[0].lines.len(), 36);
        assert_eq!(document.pages[1].lines.len(), 24);
        assert!(total_lines >= 50);
        assert!(document
            .pages
            .iter()
            .all(|page| page.lines.iter().all(|line| line.text == word)));
    }

    #[test]
    fn glue_component_normal_helper() {
        assert_eq!(
            GlueComponent::normal(DimensionValue(100)),
            GlueComponent {
                value: DimensionValue(100),
                order: GlueOrder::Normal,
            }
        );
    }

    #[test]
    fn glue_order_default_is_normal() {
        assert_eq!(GlueOrder::default(), GlueOrder::Normal);
    }

    #[test]
    fn kern_in_hlist_does_not_break_line() {
        let hlist = vec![
            HListItem::Char {
                codepoint: 'A',
                width: points(1),
            },
            HListItem::Kern { width: points(1) },
            HListItem::Char {
                codepoint: 'B',
                width: points(1),
            },
            HListItem::Glue {
                width: points(1),
                stretch: GlueComponent::normal(points(0)),
                shrink: GlueComponent::normal(points(0)),
            },
            HListItem::Char {
                codepoint: 'C',
                width: points(1),
            },
        ];

        assert_eq!(
            wrap_hlist(&hlist, points(3)),
            vec!["AB".to_string(), "C".to_string()]
        );
    }

    #[test]
    fn kern_adds_width_to_line() {
        let hlist = vec![
            HListItem::Char {
                codepoint: 'A',
                width: points(1),
            },
            HListItem::Kern { width: points(1) },
            HListItem::Char {
                codepoint: 'B',
                width: points(1),
            },
        ];

        assert_eq!(
            wrap_hlist(&hlist, points(1)),
            vec!["A".to_string(), "B".to_string()]
        );
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
