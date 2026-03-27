use std::collections::BTreeMap;

use super::{
    hyphenation::{Hyphenator, TexPatternHyphenator},
    knuth_plass::BreakParams,
    line_breaker,
};

use crate::compilation::{
    IndexEntry, LinkStyle, NavigationState, OutlineDraftEntry, PdfMetadataDraft,
};
use crate::font::api::TfmMetrics;
use crate::graphics::api::{
    compile_includegraphics, ExternalGraphic, GraphicAssetResolver, GraphicNode, GraphicsBox,
};
use crate::kernel::api::DimensionValue;
use crate::parser::api::{DocumentNode, FloatType, IndexRawEntry, MathNode, ParsedDocument};

const SCALED_POINTS_PER_POINT: i64 = 65_536;
const PAGE_WIDTH_PT: i64 = 612;
const PAGE_HEIGHT_PT: i64 = 792;
const LEFT_MARGIN_PT: i64 = 72;
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
pub struct TextLineLink {
    pub url: String,
    pub start_char: usize,
    pub end_char: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextLine {
    pub text: String,
    pub y: DimensionValue,
    pub links: Vec<TextLineLink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypesetPage {
    pub lines: Vec<TextLine>,
    pub images: Vec<TypesetImage>,
    pub page_box: PageBox,
    pub float_placements: Vec<FloatPlacement>,
    pub index_entries: Vec<IndexRawEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypesetDocument {
    pub pages: Vec<TypesetPage>,
    pub outlines: Vec<TypesetOutline>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub navigation: NavigationState,
    pub index_entries: Vec<IndexEntry>,
    pub has_unresolved_index: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypesetOutline {
    pub level: u8,
    pub title: String,
    pub page_index: usize,
    pub y: DimensionValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypesetImage {
    pub graphic: ExternalGraphic,
    pub x: DimensionValue,
    pub y: DimensionValue,
    pub display_width: DimensionValue,
    pub display_height: DimensionValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatRegion {
    Here,
    Top,
    Bottom,
    Page,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementSpec {
    pub priority_order: Vec<FloatRegion>,
    pub force: bool,
}

impl PlacementSpec {
    pub fn parse(specifier: Option<&str>) -> Self {
        let mut priority_order = Vec::new();
        let mut force = false;

        for ch in specifier.unwrap_or_default().chars() {
            match ch {
                'h' => push_float_region(&mut priority_order, FloatRegion::Here),
                't' => push_float_region(&mut priority_order, FloatRegion::Top),
                'b' => push_float_region(&mut priority_order, FloatRegion::Bottom),
                'p' => push_float_region(&mut priority_order, FloatRegion::Page),
                '!' => force = true,
                _ => {}
            }
        }

        if priority_order.is_empty() {
            priority_order = vec![FloatRegion::Top, FloatRegion::Bottom, FloatRegion::Page];
        }

        Self {
            priority_order,
            force,
        }
    }
}

fn push_float_region(regions: &mut Vec<FloatRegion>, region: FloatRegion) {
    if !regions.contains(&region) {
        regions.push(region);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloatContent {
    pub lines: Vec<TextLine>,
    pub images: Vec<TypesetImage>,
    pub height: DimensionValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloatItem {
    pub spec: PlacementSpec,
    pub content: FloatContent,
    pub defer_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloatPlacement {
    pub region: FloatRegion,
    pub content: FloatContent,
    pub y_position: DimensionValue,
}

#[derive(Debug, Default)]
pub struct FloatQueue {
    pending: Vec<FloatItem>,
}

impl FloatQueue {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn enqueue(&mut self, item: FloatItem) {
        self.pending.push(item);
    }

    pub fn try_place_at(
        &mut self,
        region: FloatRegion,
        available_height: DimensionValue,
    ) -> Option<FloatPlacement> {
        let item = self.pending.first()?;
        if item.defer_count < 10 && !item.spec.priority_order.contains(&region) {
            return None;
        }
        if item.content.height > available_height {
            return None;
        }

        let item = self.pending.remove(0);
        Some(FloatPlacement {
            region,
            content: item.content,
            y_position: DimensionValue::zero(),
        })
    }

    pub fn force_flush(&mut self) -> Vec<FloatPlacement> {
        self.pending
            .drain(..)
            .map(|item| FloatPlacement {
                region: FloatRegion::Page,
                content: item.content,
                y_position: DimensionValue::zero(),
            })
            .collect()
    }

    pub fn increment_defer_counts(&mut self) {
        for item in &mut self.pending {
            item.defer_count += 1;
        }
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
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
        link: Option<String>,
    },
    Glue {
        width: DimensionValue,
        stretch: GlueComponent,
        shrink: GlueComponent,
        link: Option<String>,
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
    Box {
        tex_box: TeXBox,
        content: String,
        links: Vec<TextLineLink>,
    },
    Image {
        graphics_box: GraphicsBox,
    },
    Glue {
        height: DimensionValue,
    },
    Penalty {
        value: i32,
    },
    IndexMarker {
        entry: IndexRawEntry,
    },
    Float {
        spec: PlacementSpec,
        content: FloatContent,
    },
    PlacedFloat {
        region: FloatRegion,
        content: FloatContent,
    },
    ClearPage,
}

#[derive(Debug, Default)]
struct FloatCounters {
    figure: u32,
    table: u32,
}

impl FloatCounters {
    fn next(&mut self, float_type: FloatType) -> u32 {
        match float_type {
            FloatType::Figure => {
                self.figure += 1;
                self.figure
            }
            FloatType::Table => {
                self.table += 1;
                self.table
            }
        }
    }
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
        self.typeset_with_provider_and_graphics_resolver(document, &provider, None)
    }

    pub fn typeset_with_graphics_resolver(
        &self,
        document: &ParsedDocument,
        graphics_resolver: &dyn GraphicAssetResolver,
    ) -> TypesetDocument {
        let provider = default_fixed_width_provider();
        self.typeset_with_provider_and_graphics_resolver(
            document,
            &provider,
            Some(graphics_resolver),
        )
    }

    pub fn typeset_with_provider(
        &self,
        document: &ParsedDocument,
        provider: &dyn CharWidthProvider,
    ) -> TypesetDocument {
        self.typeset_with_provider_and_graphics_resolver(document, provider, None)
    }

    pub fn typeset_with_provider_and_graphics_resolver(
        &self,
        document: &ParsedDocument,
        provider: &dyn CharWidthProvider,
        graphics_resolver: Option<&dyn GraphicAssetResolver>,
    ) -> TypesetDocument {
        let page_box = page_box_for_class(&document.document_class);
        let nodes = document.body_nodes();
        let params = break_params_for_provider(provider);
        let hyphenator = TexPatternHyphenator::english();
        let vlist = document_nodes_to_vlist_with_config(
            &nodes,
            provider,
            Some(&hyphenator),
            &params,
            graphics_resolver,
        );
        let pages = paginate_vlist(&vlist, &page_box);
        let outlines = collect_outlines(document, &pages);
        let index_entries = resolve_index_entries(&pages);

        TypesetDocument {
            pages,
            outlines,
            title: document.title.clone(),
            author: document.author.clone(),
            navigation: build_navigation_state(document),
            index_entries,
            has_unresolved_index: document.has_unresolved_index,
        }
    }
}

fn build_navigation_state(document: &ParsedDocument) -> NavigationState {
    NavigationState {
        metadata: PdfMetadataDraft {
            title: document
                .labels
                .pdf_title
                .clone()
                .or_else(|| document.title.clone()),
            author: document
                .labels
                .pdf_author
                .clone()
                .or_else(|| document.author.clone()),
        },
        outline_entries: document
            .section_entries
            .iter()
            .filter_map(|entry| {
                let title = entry.display_title();
                (!title.is_empty()).then_some(OutlineDraftEntry {
                    level: entry.level,
                    title,
                })
            })
            .collect(),
        named_destinations: BTreeMap::new(),
        default_link_style: LinkStyle {
            color_links: document.labels.color_links.unwrap_or(false),
            link_color: document.labels.link_color.clone(),
        },
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
                                link: None,
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
            DocumentNode::Link { url, children } => {
                flush_word(
                    &mut hlist,
                    &mut current_word,
                    &mut current_word_items,
                    hyphenator,
                    hyphen_penalty,
                );
                let mut link_hlist = document_nodes_to_hlist_with_config(
                    children,
                    provider,
                    hyphenator,
                    hyphen_penalty,
                );
                for item in &mut link_hlist {
                    match item {
                        HListItem::Char { link, .. } | HListItem::Glue { link, .. } => {
                            *link = Some(url.clone());
                        }
                        HListItem::Kern { .. } | HListItem::Penalty { .. } => {}
                    }
                }
                hlist.extend(link_hlist);
            }
            DocumentNode::IndexMarker(_) => {
                flush_word(
                    &mut hlist,
                    &mut current_word,
                    &mut current_word_items,
                    hyphenator,
                    hyphen_penalty,
                );
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
            DocumentNode::EquationEnv { lines, aligned, .. } => {
                flush_word(
                    &mut hlist,
                    &mut current_word,
                    &mut current_word_items,
                    hyphenator,
                    hyphen_penalty,
                );
                if !hlist.is_empty() {
                    push_forced_break_if_needed(&mut hlist);
                }
                for (index, line) in lines.iter().enumerate() {
                    append_literal_text_to_hlist(
                        &mut hlist,
                        &render_math_line(line, *aligned),
                        provider,
                        space_width,
                        stretch,
                        shrink,
                    );
                    if index + 1 < lines.len() {
                        push_forced_break_if_needed(&mut hlist);
                    }
                }
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
            DocumentNode::ClearPage => {
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
            DocumentNode::IncludeGraphics { .. } => {
                flush_word(
                    &mut hlist,
                    &mut current_word,
                    &mut current_word_items,
                    hyphenator,
                    hyphen_penalty,
                );
            }
            DocumentNode::Float { .. } => {
                flush_word(
                    &mut hlist,
                    &mut current_word,
                    &mut current_word_items,
                    hyphenator,
                    hyphen_penalty,
                );
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
    graphics_resolver: Option<&dyn GraphicAssetResolver>,
) -> Vec<VListItem> {
    let mut float_counters = FloatCounters::default();
    document_nodes_to_vlist_with_state(
        nodes,
        provider,
        hyphenator,
        params,
        graphics_resolver,
        &mut float_counters,
    )
}

fn document_nodes_to_vlist_with_state(
    nodes: &[DocumentNode],
    provider: &dyn CharWidthProvider,
    hyphenator: Option<&dyn Hyphenator>,
    params: &BreakParams,
    graphics_resolver: Option<&dyn GraphicAssetResolver>,
    float_counters: &mut FloatCounters,
) -> Vec<VListItem> {
    let mut vlist = Vec::new();
    let mut segment_start = 0;

    for (index, node) in nodes.iter().enumerate() {
        match node {
            DocumentNode::PageBreak => {
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
            DocumentNode::ClearPage => {
                append_nodes_segment_to_vlist(
                    &mut vlist,
                    &nodes[segment_start..index],
                    provider,
                    hyphenator,
                    params,
                );
                vlist.push(VListItem::ClearPage);
                segment_start = index + 1;
            }
            DocumentNode::IncludeGraphics { path, options } => {
                append_nodes_segment_to_vlist(
                    &mut vlist,
                    &nodes[segment_start..index],
                    provider,
                    hyphenator,
                    params,
                );
                if let Some(resolver) = graphics_resolver {
                    if let Some(graphics_box) = compile_includegraphics(path, options, resolver) {
                        vlist.push(VListItem::Image { graphics_box });
                    }
                }
                segment_start = index + 1;
            }
            DocumentNode::IndexMarker(entry) => {
                append_nodes_segment_to_vlist(
                    &mut vlist,
                    &nodes[segment_start..index],
                    provider,
                    hyphenator,
                    params,
                );
                vlist.push(VListItem::IndexMarker {
                    entry: entry.clone(),
                });
                segment_start = index + 1;
            }
            DocumentNode::Float {
                float_type,
                specifier,
                content,
                caption,
                ..
            } => {
                append_nodes_segment_to_vlist(
                    &mut vlist,
                    &nodes[segment_start..index],
                    provider,
                    hyphenator,
                    params,
                );

                let mut float_vlist = document_nodes_to_vlist_with_state(
                    content,
                    provider,
                    hyphenator,
                    params,
                    graphics_resolver,
                    float_counters,
                );

                let number = float_counters.next(*float_type);
                if let Some(caption) = caption {
                    let prefix = match float_type {
                        FloatType::Figure => "Figure",
                        FloatType::Table => "Table",
                    };
                    let caption_line = format!("{prefix} {number}: {caption}");
                    let caption_nodes = [DocumentNode::Text(caption_line)];
                    append_nodes_segment_to_vlist(
                        &mut float_vlist,
                        &caption_nodes,
                        provider,
                        hyphenator,
                        params,
                    );
                }

                vlist.push(VListItem::Float {
                    spec: PlacementSpec::parse(specifier.as_deref()),
                    content: float_content_from_vlist(&float_vlist),
                });

                segment_start = index + 1;
            }
            _ => {}
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

    let wrapped_lines = line_breaker::break_paragraph_with_links(&hlist, params);
    vlist.extend(lines_to_vlist(&wrapped_lines));
}

fn render_math_nodes(nodes: &[MathNode]) -> String {
    nodes
        .iter()
        .map(render_math_node)
        .collect::<Vec<_>>()
        .join("")
}

fn visible_math_delimiter(delimiter: &str) -> &str {
    if delimiter == "." {
        ""
    } else {
        delimiter
    }
}

fn render_math_annotation(nodes: &[MathNode]) -> String {
    if nodes.len() > 1 {
        format!("({})", render_math_nodes(nodes))
    } else {
        render_math_nodes(nodes)
    }
}

fn render_math_node(node: &MathNode) -> String {
    match node {
        MathNode::Ordinary(ch) => ch.to_string(),
        MathNode::Symbol(symbol) => symbol.clone(),
        MathNode::Superscript(node) => format!("^{}", render_math_attachment(node)),
        MathNode::Subscript(node) => format!("_{}", render_math_attachment(node)),
        MathNode::Frac { numer, denom } => {
            format!(
                "({})/({})",
                render_math_nodes(numer),
                render_math_nodes(denom)
            )
        }
        MathNode::Sqrt { radicand, index } => {
            let body = render_math_nodes(radicand);
            match index {
                Some(index) => format!("√[{}]({body})", render_math_nodes(index)),
                None => format!("√({body})"),
            }
        }
        MathNode::MathFont { body, .. } => render_math_nodes(body),
        MathNode::LeftRight { left, right, body } => format!(
            "{}{}{}",
            visible_math_delimiter(left),
            render_math_nodes(body),
            visible_math_delimiter(right)
        ),
        MathNode::OverUnder {
            kind,
            base,
            annotation,
        } => {
            let base = render_math_nodes(base);
            let annotation = render_math_annotation(annotation);
            match kind {
                crate::parser::api::OverUnderKind::Over => format!("{base}^{annotation}"),
                crate::parser::api::OverUnderKind::Under => format!("{base}_{annotation}"),
            }
        }
        MathNode::Group(nodes) => render_math_nodes(nodes),
        MathNode::Text(text) => text.clone(),
    }
}

fn render_math_attachment(node: &MathNode) -> String {
    match node {
        MathNode::Group(nodes) if nodes.len() > 1 => format!("({})", render_math_nodes(nodes)),
        _ => render_math_node(node),
    }
}

fn render_math_line(line: &crate::parser::api::MathLine, aligned: bool) -> String {
    let separator = if aligned { " " } else { "" };
    let mut rendered = line
        .segments
        .iter()
        .map(|segment| render_math_nodes(segment))
        .collect::<Vec<_>>()
        .join(separator);

    if let Some(display_tag) = line.display_tag.as_deref() {
        if !rendered.is_empty() {
            rendered.push(' ');
        }
        rendered.push('(');
        rendered.push_str(display_tag);
        rendered.push(')');
    }

    rendered
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
                link: None,
            }),
            codepoint => hlist.push(HListItem::Char {
                codepoint,
                width: provider.char_width(codepoint),
                link: None,
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
        hlist.push(HListItem::Char {
            codepoint,
            width,
            link: None,
        });
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

fn lines_to_vlist(lines: &[line_breaker::BrokenLine]) -> Vec<VListItem> {
    lines
        .iter()
        .map(|line| VListItem::Box {
            tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
            content: line.text.clone(),
            links: line.links.clone(),
        })
        .collect()
}

fn float_content_from_vlist(items: &[VListItem]) -> FloatContent {
    let mut lines = Vec::new();
    let mut images = Vec::new();
    let mut consumed_height = DimensionValue::zero();

    for item in items {
        match item {
            VListItem::Box {
                tex_box,
                content,
                links,
            } => {
                lines.push(TextLine {
                    text: content.clone(),
                    y: consumed_height,
                    links: links.clone(),
                });
                consumed_height = consumed_height + tex_box.height + tex_box.depth;
            }
            VListItem::Image { graphics_box } => {
                if let Some(graphic) = graphics_box_external(graphics_box) {
                    images.push(TypesetImage {
                        graphic,
                        x: points(LEFT_MARGIN_PT),
                        y: consumed_height,
                        display_width: graphics_box.width,
                        display_height: graphics_box.height,
                    });
                }
                consumed_height = consumed_height + graphics_box.height;
            }
            VListItem::Glue { height } => {
                consumed_height = consumed_height + *height;
            }
            VListItem::Penalty { .. }
            | VListItem::IndexMarker { .. }
            | VListItem::Float { .. }
            | VListItem::PlacedFloat { .. }
            | VListItem::ClearPage => {}
        }
    }

    FloatContent {
        lines,
        images,
        height: consumed_height,
    }
}

fn graphics_box_external(graphics_box: &GraphicsBox) -> Option<ExternalGraphic> {
    let scene = graphics_box.scene.as_ref()?;
    let node = scene.nodes.first()?;
    match node {
        GraphicNode::External(graphic) => Some(graphic.clone()),
    }
}

fn collect_outlines(document: &ParsedDocument, pages: &[TypesetPage]) -> Vec<TypesetOutline> {
    let mut anchors = Vec::new();
    let mut used = Vec::new();

    for (page_index, page) in pages.iter().enumerate() {
        for line in &page.lines {
            anchors.push((page_index, line));
            used.push(false);
        }
    }

    let mut outlines = Vec::new();
    for entry in document.section_entries.iter().rev() {
        let title = entry.display_title();
        if title.is_empty() {
            continue;
        }

        if let Some(anchor_index) = find_outline_anchor(&anchors, &used, &title) {
            used[anchor_index] = true;
            let (page_index, line) = anchors[anchor_index];
            outlines.push(TypesetOutline {
                level: entry.level,
                title,
                page_index,
                y: line.y,
            });
        }
    }

    outlines.reverse();
    outlines
}

pub fn resolve_page_labels(
    document: &ParsedDocument,
    pages: &[TypesetPage],
) -> BTreeMap<String, u32> {
    document
        .page_label_anchors
        .iter()
        .filter_map(|(label, anchor_text)| {
            pages
                .iter()
                .position(|page| {
                    page.lines
                        .iter()
                        .any(|line| line.text.trim() == anchor_text.trim())
                })
                .map(|page_index| (label.clone(), (page_index + 1) as u32))
        })
        .collect()
}

fn resolve_index_entries(pages: &[TypesetPage]) -> Vec<IndexEntry> {
    pages
        .iter()
        .enumerate()
        .flat_map(|(page_index, page)| {
            page.index_entries.iter().map(move |entry| IndexEntry {
                sort_key: entry.sort_key.clone(),
                display: entry.display.clone(),
                page: Some((page_index + 1) as u32),
            })
        })
        .collect()
}

fn find_outline_anchor(
    anchors: &[(usize, &TextLine)],
    used: &[bool],
    title: &str,
) -> Option<usize> {
    anchors
        .iter()
        .enumerate()
        .rev()
        .find(|(index, (_, line))| !used[*index] && line.text.trim() == title)
        .map(|(index, _)| index)
}

fn paginate_vlist(vlist: &[VListItem], page_box: &PageBox) -> Vec<TypesetPage> {
    let content_height = page_content_height(page_box);

    if vlist.is_empty() {
        return vec![TypesetPage {
            lines: Vec::new(),
            images: Vec::new(),
            page_box: page_box.clone(),
            float_placements: Vec::new(),
            index_entries: Vec::new(),
        }];
    }

    let mut pages = Vec::new();
    let mut current_page = Vec::new();
    let mut current_height = DimensionValue::zero();
    let mut best_break_candidate: Option<VListBreakCandidate> = None;
    let mut float_queue = FloatQueue::new();

    for item in vlist {
        match item {
            VListItem::Float { spec, content } => {
                if spec.priority_order.first() == Some(&FloatRegion::Here)
                    && current_height + content.height <= content_height
                {
                    push_placed_float(&mut current_page, FloatRegion::Here, content.clone());
                    current_height = current_height + content.height;
                } else {
                    float_queue.enqueue(FloatItem {
                        spec: spec.clone(),
                        content: content.clone(),
                        defer_count: 0,
                    });
                }
                continue;
            }
            VListItem::ClearPage => {
                place_pending_floats_in_region(
                    &mut current_page,
                    &mut current_height,
                    &mut float_queue,
                    FloatRegion::Bottom,
                    content_height,
                );
                if page_has_renderable_content(&current_page)
                    || (pages.is_empty() && float_queue.is_empty())
                {
                    pages.push(typeset_page_from_vlist(&current_page, page_box));
                }

                current_page.clear();
                current_height = DimensionValue::zero();
                best_break_candidate = None;
                pages.extend(flush_pending_float_pages(&mut float_queue, page_box));
                continue;
            }
            VListItem::Penalty { value } if *value <= PENALTY_FORCED => {
                place_pending_floats_in_region(
                    &mut current_page,
                    &mut current_height,
                    &mut float_queue,
                    FloatRegion::Bottom,
                    content_height,
                );
                if page_has_renderable_content(&current_page) || pages.is_empty() {
                    pages.push(typeset_page_from_vlist(&current_page, page_box));
                }
                if !float_queue.is_empty() {
                    float_queue.increment_defer_counts();
                }

                current_page.clear();
                current_height = DimensionValue::zero();
                best_break_candidate = None;
                place_pending_floats_in_region(
                    &mut current_page,
                    &mut current_height,
                    &mut float_queue,
                    FloatRegion::Top,
                    content_height,
                );
                continue;
            }
            _ => {}
        }

        let item_height = vlist_item_height(item);
        if !current_page.is_empty() && current_height + item_height > content_height {
            if let Some(candidate) = best_break_candidate {
                let trailing_items = current_page.split_off(candidate.split_after);
                current_height = vlist_total_height(&current_page);
                place_pending_floats_in_region(
                    &mut current_page,
                    &mut current_height,
                    &mut float_queue,
                    FloatRegion::Bottom,
                    content_height,
                );
                pages.push(typeset_page_from_vlist(&current_page, page_box));
                if !float_queue.is_empty() {
                    float_queue.increment_defer_counts();
                }

                current_page.clear();
                current_height = DimensionValue::zero();
                place_pending_floats_in_region(
                    &mut current_page,
                    &mut current_height,
                    &mut float_queue,
                    FloatRegion::Top,
                    content_height,
                );
                current_page.extend(trailing_items);
                current_height = vlist_total_height(&current_page);
                best_break_candidate = find_best_break_candidate(&current_page);
            } else {
                place_pending_floats_in_region(
                    &mut current_page,
                    &mut current_height,
                    &mut float_queue,
                    FloatRegion::Bottom,
                    content_height,
                );
                pages.push(typeset_page_from_vlist(&current_page, page_box));
                if !float_queue.is_empty() {
                    float_queue.increment_defer_counts();
                }

                current_page.clear();
                current_height = DimensionValue::zero();
                place_pending_floats_in_region(
                    &mut current_page,
                    &mut current_height,
                    &mut float_queue,
                    FloatRegion::Top,
                    content_height,
                );
            }

            if !current_page.is_empty() && current_height + item_height > content_height {
                place_pending_floats_in_region(
                    &mut current_page,
                    &mut current_height,
                    &mut float_queue,
                    FloatRegion::Bottom,
                    content_height,
                );
                pages.push(typeset_page_from_vlist(&current_page, page_box));
                if !float_queue.is_empty() {
                    float_queue.increment_defer_counts();
                }

                current_page.clear();
                current_height = DimensionValue::zero();
                best_break_candidate = None;
                place_pending_floats_in_region(
                    &mut current_page,
                    &mut current_height,
                    &mut float_queue,
                    FloatRegion::Top,
                    content_height,
                );
            }
        }

        current_page.push(item.clone());
        current_height = current_height + item_height;
        if let VListItem::Penalty { value } = item {
            maybe_record_break_candidate(&mut best_break_candidate, current_page.len(), *value);
        }
    }

    if current_page.is_empty() && !pages.is_empty() && float_queue.is_empty() {
        return pages;
    }

    place_pending_floats_in_region(
        &mut current_page,
        &mut current_height,
        &mut float_queue,
        FloatRegion::Bottom,
        content_height,
    );
    if page_has_renderable_content(&current_page) || (pages.is_empty() && float_queue.is_empty()) {
        pages.push(typeset_page_from_vlist(&current_page, page_box));
    }
    pages.extend(flush_pending_float_pages(&mut float_queue, page_box));
    if pages.is_empty() {
        pages.push(typeset_page_from_vlist(&[], page_box));
    }
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

fn page_content_height(page_box: &PageBox) -> DimensionValue {
    page_box.height - points(TOP_MARGIN_PT) - points(BOTTOM_MARGIN_PT)
}

fn page_content_top(page_box: &PageBox) -> DimensionValue {
    page_box.height - points(TOP_MARGIN_PT)
}

fn page_has_renderable_content(items: &[VListItem]) -> bool {
    items.iter().any(|item| {
        matches!(
            item,
            VListItem::Box { .. } | VListItem::Image { .. } | VListItem::PlacedFloat { .. }
        )
    })
}

fn push_placed_float(items: &mut Vec<VListItem>, region: FloatRegion, content: FloatContent) {
    let height = content.height;
    items.push(VListItem::PlacedFloat { region, content });
    items.push(VListItem::Glue { height });
}

fn place_pending_floats_in_region(
    items: &mut Vec<VListItem>,
    current_height: &mut DimensionValue,
    float_queue: &mut FloatQueue,
    region: FloatRegion,
    content_height: DimensionValue,
) {
    while *current_height < content_height {
        let available_height = content_height - *current_height;
        let Some(placement) = float_queue.try_place_at(region, available_height) else {
            break;
        };
        let height = placement.content.height;
        push_placed_float(items, placement.region, placement.content);
        *current_height = *current_height + height;
    }
}

fn flush_pending_float_pages(float_queue: &mut FloatQueue, page_box: &PageBox) -> Vec<TypesetPage> {
    let mut pages = Vec::new();
    let mut current_page = Vec::new();
    let mut current_height = DimensionValue::zero();
    let content_height = page_content_height(page_box);

    for placement in float_queue.force_flush() {
        let height = placement.content.height;
        if !current_page.is_empty() && current_height + height > content_height {
            pages.push(typeset_page_from_vlist(&current_page, page_box));
            current_page.clear();
            current_height = DimensionValue::zero();
        }

        push_placed_float(&mut current_page, FloatRegion::Page, placement.content);
        current_height = current_height + height;
    }

    if page_has_renderable_content(&current_page) {
        pages.push(typeset_page_from_vlist(&current_page, page_box));
    }

    pages
}

fn vlist_item_height(item: &VListItem) -> DimensionValue {
    match item {
        VListItem::Box { tex_box, .. } => tex_box.height + tex_box.depth,
        VListItem::Image { graphics_box } => graphics_box.height,
        VListItem::Glue { height } => *height,
        VListItem::Penalty { .. }
        | VListItem::IndexMarker { .. }
        | VListItem::Float { .. }
        | VListItem::PlacedFloat { .. }
        | VListItem::ClearPage => DimensionValue::zero(),
    }
}

fn typeset_page_from_vlist(items: &[VListItem], page_box: &PageBox) -> TypesetPage {
    let mut lines = Vec::new();
    let mut images = Vec::new();
    let mut float_placements = Vec::new();
    let mut index_entries = Vec::new();
    let mut consumed_height = DimensionValue::zero();

    for item in items {
        match item {
            VListItem::Box {
                tex_box,
                content,
                links,
            } => {
                lines.push(TextLine {
                    text: content.clone(),
                    y: page_content_top(page_box) - consumed_height,
                    links: links.clone(),
                });
                consumed_height = consumed_height + tex_box.height + tex_box.depth;
            }
            VListItem::Image { graphics_box } => {
                if let Some(graphic) = graphics_box_external(graphics_box) {
                    images.push(TypesetImage {
                        graphic,
                        x: points(LEFT_MARGIN_PT),
                        y: page_content_top(page_box) - consumed_height - graphics_box.height,
                        display_width: graphics_box.width,
                        display_height: graphics_box.height,
                    });
                }
                consumed_height = consumed_height + graphics_box.height;
            }
            VListItem::PlacedFloat { region, content } => {
                let y_position = page_content_top(page_box) - consumed_height;
                float_placements.push(FloatPlacement {
                    region: *region,
                    content: content.clone(),
                    y_position,
                });
                images.extend(content.images.iter().map(|image| TypesetImage {
                    graphic: image.graphic.clone(),
                    x: image.x,
                    y: y_position - image.y - image.display_height,
                    display_width: image.display_width,
                    display_height: image.display_height,
                }));
            }
            VListItem::IndexMarker { entry } => {
                index_entries.push(entry.clone());
            }
            VListItem::Glue { height } => {
                consumed_height = consumed_height + *height;
            }
            VListItem::Penalty { .. } | VListItem::Float { .. } | VListItem::ClearPage => {}
        }
    }

    TypesetPage {
        lines,
        images,
        page_box: page_box.clone(),
        float_placements,
        index_entries,
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
            HListItem::Char {
                codepoint, width, ..
            } => {
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
        page_box_for_class, paginate_vlist, points, resolve_page_labels, vlist_item_height,
        wrap_body, wrap_hlist, CharWidthProvider, FloatContent, FloatItem, FloatQueue, FloatRegion,
        GlueComponent, GlueOrder, HBox, HListItem, MinimalTypesetter, PlacementSpec, TeXBox,
        TextLine, TfmWidthProvider, TypesetPage, VBox, VListItem, LEFT_MARGIN_PT, LINE_HEIGHT_PT,
        MAX_LINE_CHARS, MAX_LINE_WIDTH, PAGE_HEIGHT_PT, PENALTY_FORBIDDEN, PENALTY_FORCED,
        TOP_MARGIN_PT,
    };
    use crate::assets::api::{AssetHandle, LogicalAssetId};
    use crate::bibliography::api::BibliographyState;
    use crate::compilation::IndexEntry;
    use crate::font::api::TfmMetrics;
    use crate::graphics::api::{
        ExternalGraphic, GraphicAssetResolver, ImageColorSpace, ImageMetadata,
    };
    use crate::kernel::api::DimensionValue;
    use crate::kernel::api::StableId;
    use crate::parser::api::{
        DocumentNode, FloatType, IncludeGraphicsOptions, IndexRawEntry, LineTag, MathLine,
        MathNode, MinimalLatexParser, OverUnderKind, ParsedDocument, Parser,
    };
    use crate::typesetting::{
        hyphenation::TexPatternHyphenator, knuth_plass::BreakParams, line_breaker,
    };
    use std::collections::BTreeMap;

    fn parsed_document(body: &str) -> ParsedDocument {
        ParsedDocument {
            document_class: "article".to_string(),
            class_options: Vec::new(),
            loaded_packages: Vec::new(),
            package_count: 0,
            body: body.to_string(),
            labels: Default::default(),
            bibliography_state: BibliographyState::default(),
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

    struct StubGraphicResolver;

    impl GraphicAssetResolver for StubGraphicResolver {
        fn resolve(&self, path: &str) -> Option<ExternalGraphic> {
            Some(ExternalGraphic {
                path: path.to_string(),
                asset_handle: AssetHandle {
                    id: LogicalAssetId(StableId(11)),
                },
                metadata: ImageMetadata {
                    width: 10,
                    height: 20,
                    color_space: ImageColorSpace::DeviceRGB,
                    bits_per_component: 8,
                },
            })
        }
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

    fn sample_float_content(text: &str, height_pt: i64) -> FloatContent {
        FloatContent {
            lines: vec![TextLine {
                text: text.to_string(),
                y: DimensionValue::zero(),
                links: Vec::new(),
            }],
            images: Vec::new(),
            height: points(height_pt),
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
                    links: Vec::new(),
                },
                TextLine {
                    text: "Ferritex".to_string(),
                    y: points(PAGE_HEIGHT_PT - TOP_MARGIN_PT - LINE_HEIGHT_PT),
                    links: Vec::new(),
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
    fn resolve_page_labels_maps_section_anchor_to_one_based_page_number() {
        let mut parsed = parsed_document("");
        parsed
            .page_label_anchors
            .insert("sec:later".to_string(), "1 Later".to_string());
        let page_box = page_box_for_class("article");
        let typeset = vec![
            TypesetPage {
                lines: vec![TextLine {
                    text: "Line 1".to_string(),
                    y: points(PAGE_HEIGHT_PT - TOP_MARGIN_PT),
                    links: Vec::new(),
                }],
                images: Vec::new(),
                page_box: page_box.clone(),
                float_placements: Vec::new(),
                index_entries: Vec::new(),
            },
            TypesetPage {
                lines: vec![TextLine {
                    text: "1 Later".to_string(),
                    y: points(PAGE_HEIGHT_PT - TOP_MARGIN_PT),
                    links: Vec::new(),
                }],
                images: Vec::new(),
                page_box,
                float_placements: Vec::new(),
                index_entries: Vec::new(),
            },
        ];

        assert_eq!(
            resolve_page_labels(&parsed, &typeset),
            BTreeMap::from([("sec:later".to_string(), 2)])
        );
    }

    #[test]
    fn minimal_typesetter_collects_index_entries_with_page_numbers() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\makeindex\n\\begin{document}\nAlpha\\index{Alpha}\n\\newpage\nBeta\\index{beta@Beta}\n\\end{document}\n",
            )
            .expect("parse document");

        let typeset = MinimalTypesetter.typeset(&document);

        assert_eq!(
            typeset.pages[0].index_entries,
            vec![IndexRawEntry {
                sort_key: "Alpha".to_string(),
                display: "Alpha".to_string(),
            }]
        );
        assert_eq!(
            typeset.pages[1].index_entries,
            vec![IndexRawEntry {
                sort_key: "beta".to_string(),
                display: "Beta".to_string(),
            }]
        );
        assert_eq!(
            typeset.index_entries,
            vec![
                IndexEntry {
                    sort_key: "Alpha".to_string(),
                    display: "Alpha".to_string(),
                    page: Some(1),
                },
                IndexEntry {
                    sort_key: "beta".to_string(),
                    display: "Beta".to_string(),
                    page: Some(2),
                },
            ]
        );
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
                links: Vec::new(),
            })
            .collect::<Vec<_>>();
        vlist.push(VListItem::Glue {
            height: points(LINE_HEIGHT_PT),
        });
        vlist.push(VListItem::Box {
            tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
            content: "Overflow".to_string(),
            links: Vec::new(),
        });

        let pages = paginate_vlist(&vlist, &page_box_for_class("article"));

        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].lines.len(), 35);
        assert_eq!(pages[1].lines.len(), 1);
        assert_eq!(pages[1].lines[0].text, "Overflow");
    }

    #[test]
    fn includegraphics_flows_through_typesetting_and_pagination() {
        let provider = default_fixed_width_provider();
        let params = super::break_params_for_provider(&provider);
        let nodes = vec![
            DocumentNode::Text("Before".to_string()),
            DocumentNode::IncludeGraphics {
                path: "figure.png".to_string(),
                options: IncludeGraphicsOptions {
                    width: Some(points(100)),
                    height: None,
                    scale: None,
                },
            },
            DocumentNode::Text("After".to_string()),
        ];

        let vlist = document_nodes_to_vlist_with_config(
            &nodes,
            &provider,
            None,
            &params,
            Some(&StubGraphicResolver),
        );
        let pages = paginate_vlist(&vlist, &page_box_for_class("article"));

        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].lines[0].text, "Before");
        assert_eq!(pages[0].lines[1].text, "After");
        assert_eq!(pages[0].images.len(), 1);
        assert_eq!(pages[0].images[0].display_width, points(100));
        assert_eq!(pages[0].images[0].display_height, points(200));
        assert_eq!(pages[0].images[0].x, points(LEFT_MARGIN_PT));
        assert_eq!(pages[0].images[0].graphic.path, "figure.png".to_string());
    }

    #[test]
    fn float_nodes_append_numbered_caption_text() {
        let provider = default_fixed_width_provider();
        let params = super::break_params_for_provider(&provider);
        let nodes = vec![DocumentNode::Float {
            float_type: FloatType::Figure,
            specifier: Some("h".to_string()),
            content: vec![DocumentNode::Text("Body".to_string())],
            caption: Some("A caption".to_string()),
            label: Some("fig:test".to_string()),
        }];

        let vlist = document_nodes_to_vlist_with_config(&nodes, &provider, None, &params, None);
        assert_eq!(
            vlist,
            vec![VListItem::Float {
                spec: PlacementSpec::parse(Some("h")),
                content: FloatContent {
                    lines: vec![
                        TextLine {
                            text: "Body".to_string(),
                            y: DimensionValue::zero(),
                            links: Vec::new(),
                        },
                        TextLine {
                            text: "Figure 1: A caption".to_string(),
                            y: points(LINE_HEIGHT_PT),
                            links: Vec::new(),
                        },
                    ],
                    images: Vec::new(),
                    height: points(LINE_HEIGHT_PT * 2),
                },
            }]
        );
    }

    #[test]
    fn placement_spec_parse_default() {
        assert_eq!(
            PlacementSpec::parse(None),
            PlacementSpec {
                priority_order: vec![FloatRegion::Top, FloatRegion::Bottom, FloatRegion::Page],
                force: false,
            }
        );
    }

    #[test]
    fn placement_spec_parse_htbp() {
        assert_eq!(
            PlacementSpec::parse(Some("htbp")),
            PlacementSpec {
                priority_order: vec![
                    FloatRegion::Here,
                    FloatRegion::Top,
                    FloatRegion::Bottom,
                    FloatRegion::Page,
                ],
                force: false,
            }
        );
    }

    #[test]
    fn placement_spec_parse_force() {
        assert_eq!(
            PlacementSpec::parse(Some("h!")),
            PlacementSpec {
                priority_order: vec![FloatRegion::Here],
                force: true,
            }
        );
    }

    #[test]
    fn placement_spec_parse_dedup() {
        assert_eq!(
            PlacementSpec::parse(Some("hht")),
            PlacementSpec {
                priority_order: vec![FloatRegion::Here, FloatRegion::Top],
                force: false,
            }
        );
    }

    #[test]
    fn placement_spec_parse_unknown_ignored() {
        assert_eq!(
            PlacementSpec::parse(Some("hxb")),
            PlacementSpec {
                priority_order: vec![FloatRegion::Here, FloatRegion::Bottom],
                force: false,
            }
        );
    }

    #[test]
    fn float_queue_enqueue_and_flush() {
        let mut queue = FloatQueue::new();
        queue.enqueue(FloatItem {
            spec: PlacementSpec::parse(Some("t")),
            content: sample_float_content("First", LINE_HEIGHT_PT),
            defer_count: 0,
        });
        queue.enqueue(FloatItem {
            spec: PlacementSpec::parse(Some("b")),
            content: sample_float_content("Second", LINE_HEIGHT_PT * 2),
            defer_count: 0,
        });

        let placements = queue.force_flush();

        assert_eq!(placements.len(), 2);
        assert!(queue.is_empty());
        assert_eq!(placements[0].region, FloatRegion::Page);
        assert_eq!(placements[1].content.lines[0].text, "Second");
    }

    #[test]
    fn float_queue_try_place_fits() {
        let mut queue = FloatQueue::new();
        queue.enqueue(FloatItem {
            spec: PlacementSpec::parse(Some("t")),
            content: sample_float_content("Fit", LINE_HEIGHT_PT * 2),
            defer_count: 0,
        });

        let placement = queue
            .try_place_at(FloatRegion::Top, points(LINE_HEIGHT_PT * 3))
            .expect("float should fit");

        assert_eq!(placement.region, FloatRegion::Top);
        assert_eq!(placement.content.lines[0].text, "Fit");
        assert!(queue.is_empty());
    }

    #[test]
    fn float_queue_try_place_too_tall() {
        let mut queue = FloatQueue::new();
        queue.enqueue(FloatItem {
            spec: PlacementSpec::parse(Some("t")),
            content: sample_float_content("Tall", LINE_HEIGHT_PT * 3),
            defer_count: 0,
        });

        assert!(queue
            .try_place_at(FloatRegion::Top, points(LINE_HEIGHT_PT * 2))
            .is_none());
        assert_eq!(queue.pending_count(), 1);
    }

    #[test]
    fn float_queue_increment_defer_counts() {
        let mut queue = FloatQueue::new();
        queue.enqueue(FloatItem {
            spec: PlacementSpec::parse(Some("t")),
            content: sample_float_content("Deferred", LINE_HEIGHT_PT),
            defer_count: 0,
        });

        queue.increment_defer_counts();

        let placement = queue
            .try_place_at(FloatRegion::Here, points(LINE_HEIGHT_PT))
            .is_none();
        assert!(placement);
        queue.increment_defer_counts();
        queue.increment_defer_counts();
        queue.increment_defer_counts();
        queue.increment_defer_counts();
        queue.increment_defer_counts();
        queue.increment_defer_counts();
        queue.increment_defer_counts();
        queue.increment_defer_counts();
        queue.increment_defer_counts();
        assert!(queue
            .try_place_at(FloatRegion::Here, points(LINE_HEIGHT_PT))
            .is_some());
    }

    #[test]
    fn float_with_here_spec_placed_inline() {
        let vlist = vec![
            VListItem::Box {
                tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
                content: "Before".to_string(),
                links: Vec::new(),
            },
            VListItem::Float {
                spec: PlacementSpec::parse(Some("h")),
                content: sample_float_content("Float", LINE_HEIGHT_PT * 2),
            },
            VListItem::Box {
                tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
                content: "After".to_string(),
                links: Vec::new(),
            },
        ];

        let pages = paginate_vlist(&vlist, &page_box_for_class("article"));

        assert_eq!(pages.len(), 1);
        assert_eq!(
            pages[0]
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["Before", "After"]
        );
        assert_eq!(pages[0].float_placements.len(), 1);
        assert_eq!(pages[0].float_placements[0].region, FloatRegion::Here);
        assert_eq!(pages[0].float_placements[0].content.lines[0].text, "Float");
        assert_eq!(
            pages[0].lines[1].y,
            points(PAGE_HEIGHT_PT - TOP_MARGIN_PT - (LINE_HEIGHT_PT * 3))
        );
    }

    #[test]
    fn float_deferred_to_next_page_top() {
        let mut vlist = vec![
            VListItem::Box {
                tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
                content: "Line 1".to_string(),
                links: Vec::new(),
            },
            VListItem::Float {
                spec: PlacementSpec::parse(Some("t")),
                content: sample_float_content("Top float", LINE_HEIGHT_PT * 2),
            },
        ];
        vlist.extend((2..=37).map(|index| VListItem::Box {
            tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
            content: format!("Line {index}"),
            links: Vec::new(),
        }));

        let pages = paginate_vlist(&vlist, &page_box_for_class("article"));

        assert_eq!(pages.len(), 2);
        assert!(pages[0].float_placements.is_empty());
        assert_eq!(pages[1].float_placements.len(), 1);
        assert_eq!(pages[1].float_placements[0].region, FloatRegion::Top);
        assert_eq!(
            pages[1].float_placements[0].content.lines[0].text,
            "Top float"
        );
        assert_eq!(pages[1].lines[0].text, "Line 37");
        assert_eq!(
            pages[1].lines[0].y,
            points(PAGE_HEIGHT_PT - TOP_MARGIN_PT - (LINE_HEIGHT_PT * 2))
        );
    }

    #[test]
    fn clearpage_flushes_pending_floats() {
        let vlist = vec![
            VListItem::Box {
                tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
                content: "Before".to_string(),
                links: Vec::new(),
            },
            VListItem::Float {
                spec: PlacementSpec::parse(Some("t")),
                content: sample_float_content("Pending float", LINE_HEIGHT_PT * 2),
            },
            VListItem::ClearPage,
            VListItem::Box {
                tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
                content: "After".to_string(),
                links: Vec::new(),
            },
        ];

        let pages = paginate_vlist(&vlist, &page_box_for_class("article"));

        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0].lines[0].text, "Before");
        assert!(pages[0].float_placements.is_empty());
        assert_eq!(pages[1].float_placements.len(), 1);
        assert_eq!(pages[1].float_placements[0].region, FloatRegion::Page);
        assert_eq!(
            pages[1].float_placements[0].content.lines[0].text,
            "Pending float"
        );
        assert_eq!(pages[2].lines[0].text, "After");
    }

    #[test]
    fn parsed_document_with_multiple_float_specifiers_typesets_across_pages() {
        let body = [
            "Intro",
            "\\begin{figure}[h]Inline body\\caption{Inline}\\end{figure}",
            "Line 2",
            "Line 3",
            "Line 4",
            "Line 5",
            "Line 6",
            "Line 7",
            "Line 8",
            "Line 9",
            "Line 10",
            "Line 11",
            "Line 12",
            "Line 13",
            "Line 14",
            "Line 15",
            "Line 16",
            "Line 17",
            "Line 18",
            "Line 19",
            "Line 20",
            "Line 21",
            "Line 22",
            "Line 23",
            "Line 24",
            "Line 25",
            "Line 26",
            "Line 27",
            "Line 28",
            "Line 29",
            "Line 30",
            "Line 31",
            "Line 32",
            "Line 33",
            "Line 34",
            "Line 35",
            "Line 36",
            "\\begin{figure}[t]Top body\\caption{Top}\\end{figure}",
            "Tail",
            "\\clearpage",
            "Done",
        ]
        .join("\n");

        let document = MinimalTypesetter.typeset(&parsed_latex_document(&body));

        assert_eq!(document.pages.len(), 3);
        assert!(document
            .pages
            .iter()
            .flat_map(|page| page.float_placements.iter())
            .any(|placement| placement.region == FloatRegion::Here
                && placement
                    .content
                    .lines
                    .iter()
                    .any(|line| line.text.contains("Inline body"))));
        assert!(document
            .pages
            .iter()
            .flat_map(|page| page.float_placements.iter())
            .any(|placement| placement.region == FloatRegion::Page
                && placement
                    .content
                    .lines
                    .iter()
                    .any(|line| line.text.contains("Top body"))));
        assert_eq!(document.pages[2].lines[0].text, "Done");
    }

    #[test]
    fn mixed_height_lines_break_by_accumulated_height() {
        let mut vlist = (1..=16)
            .map(|index| VListItem::Box {
                tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT * 2)),
                content: format!("Tall {index}"),
                links: Vec::new(),
            })
            .collect::<Vec<_>>();
        vlist.extend((1..=5).map(|index| VListItem::Box {
            tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
            content: format!("Short {index}"),
            links: Vec::new(),
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
                links: Vec::new(),
            },
            VListItem::Penalty {
                value: PENALTY_FORCED,
            },
            VListItem::Box {
                tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
                content: "Second".to_string(),
                links: Vec::new(),
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
                links: Vec::new(),
            })
            .collect::<Vec<_>>();
        vlist.push(VListItem::Penalty { value: 50 });
        vlist.extend((35..=37).map(|index| VListItem::Box {
            tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
            content: format!("Line {index}"),
            links: Vec::new(),
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
                links: Vec::new(),
            })
            .collect::<Vec<_>>();
        vlist.push(VListItem::Penalty {
            value: PENALTY_FORBIDDEN,
        });
        vlist.extend((35..=37).map(|index| VListItem::Box {
            tex_box: TeXBox::with_height(points(LINE_HEIGHT_PT)),
            content: format!("Line {index}"),
            links: Vec::new(),
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
            link: None,
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
            links: Vec::new(),
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
            links: Vec::new(),
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
                    link: None,
                },
                HListItem::Glue {
                    width: points(1),
                    stretch: GlueComponent::normal(DimensionValue(points(1).0 / 2)),
                    shrink: GlueComponent::normal(DimensionValue(points(1).0 / 3)),
                    link: None,
                },
                HListItem::Char {
                    codepoint: 'B',
                    width: points(1),
                    link: None,
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
    fn document_nodes_to_hlist_renders_extended_math_nodes_readably() {
        let hlist = document_nodes_to_hlist(
            &[
                DocumentNode::Text("f = ".to_string()),
                DocumentNode::InlineMath(vec![
                    MathNode::MathFont {
                        cmd: "mathrm".to_string(),
                        body: vec![
                            MathNode::Ordinary('m'),
                            MathNode::Ordinary('a'),
                            MathNode::Ordinary('x'),
                        ],
                    },
                    MathNode::LeftRight {
                        left: "(".to_string(),
                        right: ")".to_string(),
                        body: vec![MathNode::Symbol("α".to_string())],
                    },
                    MathNode::Ordinary('+'),
                    MathNode::Sqrt {
                        radicand: vec![MathNode::Ordinary('x')],
                        index: Some(vec![MathNode::Ordinary('3')]),
                    },
                    MathNode::Ordinary('+'),
                    MathNode::OverUnder {
                        kind: OverUnderKind::Under,
                        base: vec![MathNode::Ordinary('Y')],
                        annotation: vec![MathNode::Ordinary('n')],
                    },
                ]),
            ],
            &default_fixed_width_provider(),
        );

        assert_eq!(
            wrap_hlist(&hlist, MAX_LINE_WIDTH),
            vec!["f = max(α)+√[3](x)+Y_n".to_string()]
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
    fn document_nodes_to_hlist_puts_equation_environment_on_separate_lines() {
        let hlist = document_nodes_to_hlist(
            &[
                DocumentNode::Text("Before".to_string()),
                DocumentNode::EquationEnv {
                    lines: vec![
                        MathLine {
                            segments: vec![vec![
                                MathNode::Ordinary('a'),
                                MathNode::Ordinary('='),
                                MathNode::Ordinary('b'),
                            ]],
                            tag: LineTag::Auto,
                            display_tag: Some("1".to_string()),
                        },
                        MathLine {
                            segments: vec![
                                vec![MathNode::Ordinary('c')],
                                vec![MathNode::Ordinary('=')],
                                vec![MathNode::Text("done".to_string())],
                            ],
                            tag: LineTag::Custom("A".to_string()),
                            display_tag: Some("A".to_string()),
                        },
                    ],
                    numbered: true,
                    aligned: true,
                },
                DocumentNode::Text("After".to_string()),
            ],
            &default_fixed_width_provider(),
        );

        assert_eq!(
            wrap_hlist(&hlist, MAX_LINE_WIDTH),
            vec![
                "Before".to_string(),
                "a=b (1)".to_string(),
                "c = done (A)".to_string(),
                "After".to_string(),
            ]
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
    fn minimal_typesetter_renders_multiline_math_environments() {
        let document = MinimalTypesetter.typeset(&parsed_latex_document(
            "\\begin{equation}E=mc^2\\label{eq:e}\\end{equation}\n\
             Ref \\ref{eq:e}.\n\
             \\begin{align}\n\
             a&=&b\\notag\\\\\n\
             c&=&\\text{done}\\tag{A}\\label{eq:done}\n\
             \\end{align}\n\
             Also \\ref{eq:done}.",
        ));

        assert_eq!(
            document.pages[0]
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["E=mc^2 (1)", "Ref 1.", "a = b", "c = done (A)", "Also A."]
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
                    link: None,
                },
                HListItem::Char {
                    codepoint: 'a',
                    width: points(1),
                    link: None,
                },
                HListItem::Char {
                    codepoint: 's',
                    width: points(1),
                    link: None,
                },
                HListItem::Penalty { value: 50 },
                HListItem::Char {
                    codepoint: 'k',
                    width: points(1),
                    link: None,
                },
                HListItem::Char {
                    codepoint: 'e',
                    width: points(1),
                    link: None,
                },
                HListItem::Char {
                    codepoint: 't',
                    width: points(1),
                    link: None,
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
        let plain_vlist =
            document_nodes_to_vlist_with_config(&nodes, &provider, None, &params, None);
        let plain_document = super::TypesetDocument {
            pages: paginate_vlist(&plain_vlist, &page_box_for_class(&parsed.document_class)),
            outlines: Vec::new(),
            title: None,
            author: None,
            navigation: Default::default(),
            index_entries: Vec::new(),
            has_unresolved_index: false,
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
    fn minimal_typesetter_populates_navigation_from_hypersetup() {
        let parsed = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\title{Document Title}\n\\author{Document Author}\n\\begin{document}\n\\hypersetup{pdftitle={PDF Title},pdfauthor={PDF Author},colorlinks=true,linkcolor=red}\nBody\n\\end{document}\n",
            )
            .expect("parse document");
        let document = MinimalTypesetter.typeset(&parsed);

        assert_eq!(
            document.navigation.metadata.title.as_deref(),
            Some("PDF Title")
        );
        assert_eq!(
            document.navigation.metadata.author.as_deref(),
            Some("PDF Author")
        );
        assert!(document.navigation.default_link_style.color_links);
        assert_eq!(
            document.navigation.default_link_style.link_color.as_deref(),
            Some("red")
        );
    }

    #[test]
    fn minimal_typesetter_navigation_falls_back_to_document_title() {
        let parsed = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\title{Document Title}\n\\author{Document Author}\n\\begin{document}\nBody\n\\end{document}\n",
            )
            .expect("parse document");
        let document = MinimalTypesetter.typeset(&parsed);

        assert_eq!(
            document.navigation.metadata.title.as_deref(),
            Some("Document Title")
        );
        assert_eq!(
            document.navigation.metadata.author.as_deref(),
            Some("Document Author")
        );
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
                link: None,
            },
            HListItem::Glue {
                width: points(1),
                stretch: GlueComponent::normal(points(60)),
                shrink: GlueComponent::normal(points(1)),
                link: None,
            },
            HListItem::Char {
                codepoint: 'b',
                width: points(10),
                link: None,
            },
            HListItem::Glue {
                width: points(1),
                stretch: GlueComponent::normal(points(60)),
                shrink: GlueComponent::normal(points(1)),
                link: None,
            },
            HListItem::Char {
                codepoint: 'c',
                width: points(10),
                link: None,
            },
            HListItem::Penalty { value: 100 },
            HListItem::Glue {
                width: points(1),
                stretch: GlueComponent::normal(points(60)),
                shrink: GlueComponent::normal(points(1)),
                link: None,
            },
            HListItem::Char {
                codepoint: 'd',
                width: points(10),
                link: None,
            },
            HListItem::Glue {
                width: points(1),
                stretch: GlueComponent::normal(points(60)),
                shrink: GlueComponent::normal(points(1)),
                link: None,
            },
            HListItem::Char {
                codepoint: 'e',
                width: points(10),
                link: None,
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
                link: None,
            },
            HListItem::Kern { width: points(1) },
            HListItem::Char {
                codepoint: 'B',
                width: points(1),
                link: None,
            },
            HListItem::Glue {
                width: points(1),
                stretch: GlueComponent::normal(points(0)),
                shrink: GlueComponent::normal(points(0)),
                link: None,
            },
            HListItem::Char {
                codepoint: 'C',
                width: points(1),
                link: None,
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
                link: None,
            },
            HListItem::Kern { width: points(1) },
            HListItem::Char {
                codepoint: 'B',
                width: points(1),
                link: None,
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
