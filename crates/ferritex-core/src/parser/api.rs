use std::{
    cmp::Ordering,
    collections::{BTreeMap, VecDeque},
    ops::{Deref, DerefMut},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::bibliography::api::{parse_bbl, BibliographyState};
use crate::compilation::{IndexEntry, SectionOutlineEntry};
use crate::graphics::api::{
    compile_graphics_scene, parse_tikzpicture, GraphicsBox, TikzDiagnostic, TikzParseResult,
};
use crate::kernel::api::DimensionValue;
use crate::policy::{
    FileOperationHandler, FileOperationResult, ShellEscapeHandler, ShellEscapeResult,
};

use super::{
    conditionals::{evaluate_ifnum, tokens_equal, ConditionalState, SkipOutcome},
    package_loading::{
        load_document_class, load_package, ClassRegistry, OptionRegistry, PackageInfo,
        PackageRegistry, StyPackageResolver,
    },
    registers::{CompatIntRegister, RegisterStore, MAX_REGISTER_INDEX},
    CatCode, EnvironmentDef, MacroDef, MacroEngine, Token, TokenKind, Tokenizer,
};

const MAX_CONSECUTIVE_MACRO_EXPANSIONS: usize = 1_000;
const BODY_PAGE_BREAK_MARKER: char = '\u{E000}';
const BODY_CLEAR_PAGE_MARKER: char = '\u{E01B}';
const BODY_HBOX_START: char = '\u{E001}';
const BODY_HBOX_END: char = '\u{E002}';
const BODY_VBOX_START: char = '\u{E003}';
const BODY_VBOX_END: char = '\u{E004}';
const BODY_INLINE_MATH_START: char = '\u{E005}';
const BODY_INLINE_MATH_END: char = '\u{E006}';
const BODY_DISPLAY_MATH_START: char = '\u{E007}';
const BODY_DISPLAY_MATH_END: char = '\u{E008}';
const BODY_HREF_START: char = '\u{E009}';
const BODY_HREF_URL_END: char = '\u{E00A}';
const BODY_HREF_END: char = '\u{E00B}';
const BODY_URL_START: char = '\u{E00C}';
const BODY_URL_END: char = '\u{E00D}';
const BODY_HYPERREF_START: char = '\u{E020}';
const BODY_HYPERREF_TARGET_END: char = '\u{E021}';
const BODY_HYPERREF_END: char = '\u{E022}';
const BODY_SANS_FAMILY_START: char = '\u{E023}';
const BODY_SANS_FAMILY_END: char = '\u{E024}';
const BODY_MONO_FAMILY_START: char = '\u{E025}';
const BODY_MONO_FAMILY_END: char = '\u{E026}';
const BODY_EQUATION_ENV_START: char = '\u{E00E}';
const BODY_EQUATION_ENV_END: char = '\u{E00F}';
const BODY_PAGEREF_START: char = '\u{E010}';
const BODY_PAGEREF_END: char = '\u{E011}';
const BODY_INCLUDEGRAPHICS_START: char = '\u{E012}';
const BODY_INCLUDEGRAPHICS_END: char = '\u{E013}';
const BODY_INCLUDEGRAPHICS_PATH_END: char = '\u{E014}';
const BODY_INCLUDEGRAPHICS_FIELD_SEPARATOR: char = '\u{E015}';
const BODY_TIKZPICTURE_START: char = '\u{E027}';
const BODY_TIKZPICTURE_END: char = '\u{E028}';
const BODY_FLOAT_START: char = '\u{E016}';
const BODY_FLOAT_END: char = '\u{E017}';
const BODY_FLOAT_CAPTION_SEP: char = '\u{E018}';
const BODY_FLOAT_LABEL_SEP: char = '\u{E019}';
const BODY_FLOAT_TYPE_SEP: char = '\u{E01A}';
const BODY_FLOAT_SPECIFIER_SEP: char = '\u{E01C}';
const BODY_INDEX_ENTRY_START: char = '\u{E01D}';
const BODY_INDEX_ENTRY_END: char = '\u{E01E}';
const BODY_INDEX_ENTRY_FIELD_SEP: char = '\u{E01F}';
const BODY_BOX_PLACEHOLDER_BASE: u32 = 0xE100;
const EQUATION_ENV_ROW_SEPARATOR: char = '\u{001E}';
const EQUATION_ENV_FIELD_SEPARATOR: char = '\u{001F}';
const EQUATION_ENV_SEGMENT_SEPARATOR: char = '\u{001D}';

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SectionEntry {
    pub level: u8,
    pub number: String,
    pub title: String,
}

impl SectionEntry {
    pub fn display_title(&self) -> String {
        match (self.number.is_empty(), self.title.is_empty()) {
            (true, true) => String::new(),
            (false, true) => self.number.clone(),
            (true, false) => self.title.clone(),
            (false, false) => format!("{} {}", self.number, self.title),
        }
    }
}

impl From<&SectionEntry> for SectionOutlineEntry {
    fn from(value: &SectionEntry) -> Self {
        Self {
            level: value.level,
            number: value.number.clone(),
            title: value.title.clone(),
        }
    }
}

fn bibliography_destination_name(key: &str) -> String {
    format!("bib:{key}")
}

fn section_destination_name(display_title: &str) -> String {
    if display_title.trim().is_empty() {
        String::new()
    } else {
        format!("section:{display_title}")
    }
}

fn internal_link_marker(target: &str, display_text: &str) -> String {
    if target.trim().is_empty() {
        return display_text.to_string();
    }

    let mut rendered = String::with_capacity(target.len() + display_text.len() + 3);
    rendered.push(BODY_HYPERREF_START);
    rendered.push_str(target);
    rendered.push(BODY_HYPERREF_TARGET_END);
    rendered.push_str(display_text);
    rendered.push(BODY_HYPERREF_END);
    rendered
}

fn float_anchor_text(float_type: FloatType, number: &str, caption: &str) -> String {
    let prefix = match float_type {
        FloatType::Figure => "Figure",
        FloatType::Table => "Table",
    };
    format!("{prefix} {number}: {caption}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionEntry {
    pub kind: FloatType,
    pub number: String,
    pub caption: String,
}

impl CaptionEntry {
    pub fn display_title(&self) -> String {
        let prefix = match self.kind {
            FloatType::Figure => "Figure",
            FloatType::Table => "Table",
        };

        if self.caption.is_empty() {
            format!("{prefix} {}", self.number)
        } else {
            format!("{prefix} {}: {}", self.number, self.caption)
        }
    }
}

impl Default for CaptionEntry {
    fn default() -> Self {
        Self {
            kind: FloatType::Figure,
            number: String::new(),
            caption: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexRawEntry {
    pub sort_key: String,
    pub display: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentLabels {
    entries: BTreeMap<String, String>,
    pub citations: Vec<String>,
    pub bibliography: BTreeMap<String, String>,
    pub section_entries: Vec<SectionEntry>,
    pub figure_entries: Vec<CaptionEntry>,
    pub table_entries: Vec<CaptionEntry>,
    pub page_label_anchors: BTreeMap<String, String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub pdf_title: Option<String>,
    pub pdf_author: Option<String>,
    pub color_links: Option<bool>,
    pub link_color: Option<String>,
    pub index_enabled: bool,
    pub index_entries: Vec<IndexRawEntry>,
    pub has_unresolved_toc: bool,
    pub has_unresolved_lof: bool,
    pub has_unresolved_lot: bool,
    pub has_unresolved_index: bool,
}

impl DocumentLabels {
    #[allow(clippy::too_many_arguments)]
    fn with_metadata(
        entries: BTreeMap<String, String>,
        citations: Vec<String>,
        bibliography: BTreeMap<String, String>,
        section_entries: Vec<SectionEntry>,
        figure_entries: Vec<CaptionEntry>,
        table_entries: Vec<CaptionEntry>,
        page_label_anchors: BTreeMap<String, String>,
        title: Option<String>,
        author: Option<String>,
        pdf_title: Option<String>,
        pdf_author: Option<String>,
        color_links: Option<bool>,
        link_color: Option<String>,
        index_enabled: bool,
        index_entries: Vec<IndexRawEntry>,
        has_unresolved_toc: bool,
        has_unresolved_lof: bool,
        has_unresolved_lot: bool,
        has_unresolved_index: bool,
    ) -> Self {
        Self {
            entries,
            citations,
            bibliography,
            section_entries,
            figure_entries,
            table_entries,
            page_label_anchors,
            title,
            author,
            pdf_title,
            pdf_author,
            color_links,
            link_color,
            index_enabled,
            index_entries,
            has_unresolved_toc,
            has_unresolved_lof,
            has_unresolved_lot,
            has_unresolved_index,
        }
    }

    pub fn into_inner(self) -> BTreeMap<String, String> {
        self.entries
    }
}

impl From<BTreeMap<String, String>> for DocumentLabels {
    fn from(entries: BTreeMap<String, String>) -> Self {
        Self {
            entries,
            ..Self::default()
        }
    }
}

impl Deref for DocumentLabels {
    type Target = BTreeMap<String, String>;

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl DerefMut for DocumentLabels {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entries
    }
}

impl PartialEq<BTreeMap<String, String>> for DocumentLabels {
    fn eq(&self, other: &BTreeMap<String, String>) -> bool {
        self.entries == *other
    }
}

impl PartialEq<DocumentLabels> for BTreeMap<String, String> {
    fn eq(&self, other: &DocumentLabels) -> bool {
        *self == other.entries
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedDocument {
    pub document_class: String,
    pub class_options: Vec<String>,
    pub loaded_packages: Vec<PackageInfo>,
    pub package_count: usize,
    pub main_font_name: Option<String>,
    pub sans_font_name: Option<String>,
    pub mono_font_name: Option<String>,
    pub body: String,
    pub labels: DocumentLabels,
    pub bibliography_state: BibliographyState,
    pub has_unresolved_refs: bool,
}

impl Deref for ParsedDocument {
    type Target = DocumentLabels;

    fn deref(&self) -> &Self::Target {
        &self.labels
    }
}

impl DerefMut for ParsedDocument {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.labels
    }
}

/// Result of a parse-with-recovery attempt.
/// Contains a best-effort document (if structural requirements were met)
/// and all accumulated parse errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOutput {
    pub document: Option<ParsedDocument>,
    pub errors: Vec<ParseError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MathNode {
    Ordinary(char),
    Symbol(String),
    Superscript(Box<MathNode>),
    Subscript(Box<MathNode>),
    Frac {
        numer: Vec<MathNode>,
        denom: Vec<MathNode>,
    },
    Sqrt {
        radicand: Vec<MathNode>,
        index: Option<Vec<MathNode>>,
    },
    MathFont {
        cmd: String,
        body: Vec<MathNode>,
    },
    LeftRight {
        left: String,
        right: String,
        body: Vec<MathNode>,
    },
    OverUnder {
        kind: OverUnderKind,
        base: Vec<MathNode>,
        annotation: Vec<MathNode>,
    },
    Group(Vec<MathNode>),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverUnderKind {
    Over,
    Under,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineTag {
    Auto,
    Notag,
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MathLine {
    pub segments: Vec<Vec<MathNode>>,
    pub tag: LineTag,
    pub display_tag: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct IncludeGraphicsOptions {
    pub width: Option<DimensionValue>,
    pub height: Option<DimensionValue>,
    pub scale: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatType {
    Figure,
    Table,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontFamilyRole {
    Main,
    Sans,
    Mono,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DocumentNode {
    Text(String),
    FontFamily {
        role: FontFamilyRole,
        children: Vec<DocumentNode>,
    },
    Link {
        url: String,
        children: Vec<DocumentNode>,
    },
    IndexMarker(IndexRawEntry),
    ParBreak,
    PageBreak,
    ClearPage,
    HBox(Vec<DocumentNode>),
    VBox(Vec<DocumentNode>),
    InlineMath(Vec<MathNode>),
    DisplayMath(Vec<MathNode>),
    EquationEnv {
        lines: Vec<MathLine>,
        numbered: bool,
        aligned: bool,
    },
    IncludeGraphics {
        path: String,
        options: IncludeGraphicsOptions,
    },
    TikzPicture {
        graphics_box: GraphicsBox,
    },
    Float {
        float_type: FloatType,
        specifier: Option<String>,
        content: Vec<DocumentNode>,
        caption: Option<String>,
        label: Option<String>,
    },
}

impl ParsedDocument {
    pub fn body_nodes(&self) -> Vec<DocumentNode> {
        body_nodes_from_text(&self.body)
    }

    pub fn has_pageref_markers(&self) -> bool {
        body_contains_pageref_markers(&self.body)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    #[error("input is empty")]
    EmptyInput,
    #[error("missing \\documentclass declaration")]
    MissingDocumentClass,
    #[error("invalid \\documentclass declaration")]
    InvalidDocumentClass { line: u32 },
    #[error("missing \\begin{{document}}")]
    MissingBeginDocument { line: u32 },
    #[error("missing \\end{{document}}")]
    MissingEndDocument { line: u32 },
    #[error("unexpected \\end{{document}} before \\begin{{document}}")]
    UnexpectedEndDocument { line: u32 },
    #[error("unexpected content after \\end{{document}}")]
    TrailingContentAfterEndDocument { line: u32 },
    #[error("unexpected closing brace")]
    UnexpectedClosingBrace { line: u32 },
    #[error("unclosed brace")]
    UnclosedBrace { line: u32 },
    #[error("invalid register index")]
    InvalidRegisterIndex { line: u32 },
    #[error("unclosed conditional")]
    UnclosedConditional { line: u32 },
    #[error("unclosed environment `{name}`")]
    UnclosedEnvironment { line: u32, name: String },
    #[error("unexpected \\else")]
    UnexpectedElse { line: u32 },
    #[error("unexpected \\fi")]
    UnexpectedFi { line: u32 },
    #[error("division by zero")]
    DivisionByZero { line: u32 },
    #[error("macro expansion limit exceeded")]
    MacroExpansionLimit { line: u32 },
    #[error("{message}")]
    TikzDiagnostic { line: u32, message: String },
    #[error("\\setmainfont, \\setsansfont, and \\setmonofont require \\usepackage{{fontspec}}")]
    FontspecNotLoaded { line: u32 },
    #[error("\\setmainfont, \\setsansfont, and \\setmonofont in document body are not supported; use them in the preamble")]
    SetmainfontInBody { line: u32 },
    #[error("shell escape is not allowed: \\write18{{{command}}}")]
    ShellEscapeNotAllowed { line: u32, command: String },
    #[error("{message}")]
    ShellEscapeError { line: u32, message: String },
    #[error("file access denied: \\{operation} {path}")]
    FileOperationDenied {
        line: u32,
        operation: FileOperationKind,
        path: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOperationKind {
    OpenIn,
    OpenOut,
}

impl FileOperationKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::OpenIn => "openin",
            Self::OpenOut => "openout",
        }
    }
}

impl std::fmt::Display for FileOperationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl ParseError {
    pub const fn line(&self) -> Option<u32> {
        match self {
            Self::EmptyInput | Self::MissingDocumentClass => None,
            Self::InvalidDocumentClass { line }
            | Self::MissingBeginDocument { line }
            | Self::MissingEndDocument { line }
            | Self::UnexpectedEndDocument { line }
            | Self::TrailingContentAfterEndDocument { line }
            | Self::UnexpectedClosingBrace { line }
            | Self::UnclosedBrace { line }
            | Self::InvalidRegisterIndex { line }
            | Self::UnclosedConditional { line }
            | Self::UnclosedEnvironment { line, .. }
            | Self::UnexpectedElse { line }
            | Self::UnexpectedFi { line }
            | Self::DivisionByZero { line }
            | Self::MacroExpansionLimit { line }
            | Self::TikzDiagnostic { line, .. }
            | Self::FontspecNotLoaded { line }
            | Self::SetmainfontInBody { line }
            | Self::ShellEscapeNotAllowed { line, .. }
            | Self::ShellEscapeError { line, .. }
            | Self::FileOperationDenied { line, .. } => Some(*line),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MinimalLatexParser;

pub trait Parser {
    type Error;

    fn parse(&self, source: &str) -> Result<ParsedDocument, Self::Error>;
}

impl Parser for MinimalLatexParser {
    type Error = ParseError;

    fn parse(&self, source: &str) -> Result<ParsedDocument, Self::Error> {
        parse_minimal_latex(source)
    }
}

impl MinimalLatexParser {
    pub fn parse_recovering(&self, source: &str) -> ParseOutput {
        self.parse_recovering_with_context(
            source,
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            BTreeMap::new(),
            None,
            BTreeMap::new(),
            Vec::new(),
        )
    }

    pub fn parse_with_labels(
        &self,
        source: &str,
        initial_labels: BTreeMap<String, String>,
        initial_page_labels: BTreeMap<String, u32>,
    ) -> Result<ParsedDocument, ParseError> {
        parse_minimal_latex_with_labels(source, initial_labels, initial_page_labels)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn parse_with_state(
        &self,
        source: &str,
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_figure_entries: Vec<CaptionEntry>,
        initial_table_entries: Vec<CaptionEntry>,
        initial_page_labels: BTreeMap<String, u32>,
        initial_index_entries: Vec<IndexEntry>,
    ) -> Result<ParsedDocument, ParseError> {
        parse_minimal_latex_with_state(
            source,
            initial_labels,
            initial_section_entries,
            initial_figure_entries,
            initial_table_entries,
            initial_page_labels,
            initial_index_entries,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn parse_with_context(
        &self,
        source: &str,
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_figure_entries: Vec<CaptionEntry>,
        initial_table_entries: Vec<CaptionEntry>,
        initial_bibliography: BTreeMap<String, String>,
        initial_bibliography_state: Option<BibliographyState>,
        initial_page_labels: BTreeMap<String, u32>,
        initial_index_entries: Vec<IndexEntry>,
    ) -> Result<ParsedDocument, ParseError> {
        parse_minimal_latex_with_context(
            source,
            initial_labels,
            initial_section_entries,
            initial_figure_entries,
            initial_table_entries,
            initial_bibliography,
            initial_bibliography_state,
            initial_page_labels,
            initial_index_entries,
        )
    }

    pub fn parse_recovering_with_labels(
        &self,
        source: &str,
        initial_labels: BTreeMap<String, String>,
        initial_page_labels: BTreeMap<String, u32>,
    ) -> ParseOutput {
        self.parse_recovering_with_context(
            source,
            initial_labels,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            BTreeMap::new(),
            None,
            initial_page_labels,
            Vec::new(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn parse_recovering_with_state(
        &self,
        source: &str,
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_figure_entries: Vec<CaptionEntry>,
        initial_table_entries: Vec<CaptionEntry>,
        initial_page_labels: BTreeMap<String, u32>,
        initial_index_entries: Vec<IndexEntry>,
    ) -> ParseOutput {
        self.parse_recovering_with_context(
            source,
            initial_labels,
            initial_section_entries,
            initial_figure_entries,
            initial_table_entries,
            BTreeMap::new(),
            None,
            initial_page_labels,
            initial_index_entries,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn parse_recovering_with_context(
        &self,
        source: &str,
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_figure_entries: Vec<CaptionEntry>,
        initial_table_entries: Vec<CaptionEntry>,
        initial_bibliography: BTreeMap<String, String>,
        initial_bibliography_state: Option<BibliographyState>,
        initial_page_labels: BTreeMap<String, u32>,
        initial_index_entries: Vec<IndexEntry>,
    ) -> ParseOutput {
        if source.trim().is_empty() {
            return ParseOutput {
                document: None,
                errors: vec![ParseError::EmptyInput],
            };
        }

        ParserDriver::new_with_context(
            source,
            initial_labels,
            initial_section_entries,
            initial_figure_entries,
            initial_table_entries,
            initial_bibliography,
            initial_bibliography_state,
            initial_page_labels,
            initial_index_entries,
            None,
        )
        .run_recovering()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn parse_recovering_with_context_and_package_resolver(
        &self,
        source: &str,
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_figure_entries: Vec<CaptionEntry>,
        initial_table_entries: Vec<CaptionEntry>,
        initial_bibliography: BTreeMap<String, String>,
        initial_bibliography_state: Option<BibliographyState>,
        initial_page_labels: BTreeMap<String, u32>,
        initial_index_entries: Vec<IndexEntry>,
        sty_resolver: Option<&StyPackageResolver<'_>>,
    ) -> ParseOutput {
        self.parse_recovering_with_context_and_handlers(
            source,
            initial_labels,
            initial_section_entries,
            initial_figure_entries,
            initial_table_entries,
            initial_bibliography,
            initial_bibliography_state,
            initial_page_labels,
            initial_index_entries,
            sty_resolver,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn parse_recovering_with_context_and_handlers(
        &self,
        source: &str,
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_figure_entries: Vec<CaptionEntry>,
        initial_table_entries: Vec<CaptionEntry>,
        initial_bibliography: BTreeMap<String, String>,
        initial_bibliography_state: Option<BibliographyState>,
        initial_page_labels: BTreeMap<String, u32>,
        initial_index_entries: Vec<IndexEntry>,
        sty_resolver: Option<&StyPackageResolver<'_>>,
        shell_escape_handler: Option<&dyn ShellEscapeHandler>,
        file_operation_handler: Option<&dyn FileOperationHandler>,
    ) -> ParseOutput {
        if source.trim().is_empty() {
            return ParseOutput {
                document: None,
                errors: vec![ParseError::EmptyInput],
            };
        }

        ParserDriver::new_with_context_and_handlers(
            source,
            initial_labels,
            initial_section_entries,
            initial_figure_entries,
            initial_table_entries,
            initial_bibliography,
            initial_bibliography_state,
            initial_page_labels,
            initial_index_entries,
            sty_resolver,
            shell_escape_handler,
            file_operation_handler,
        )
        .run_recovering()
    }
}

pub fn parse_bbl_input(input: &str) -> BibliographyState {
    BibliographyState::from_snapshot(parse_bbl(input))
}

fn parse_minimal_latex(source: &str) -> Result<ParsedDocument, ParseError> {
    parse_minimal_latex_with_context(
        source,
        BTreeMap::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        BTreeMap::new(),
        None,
        BTreeMap::new(),
        Vec::new(),
    )
}

fn parse_minimal_latex_with_labels(
    source: &str,
    initial_labels: BTreeMap<String, String>,
    initial_page_labels: BTreeMap<String, u32>,
) -> Result<ParsedDocument, ParseError> {
    parse_minimal_latex_with_context(
        source,
        initial_labels,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        BTreeMap::new(),
        None,
        initial_page_labels,
        Vec::new(),
    )
}

fn parse_minimal_latex_with_state(
    source: &str,
    initial_labels: BTreeMap<String, String>,
    initial_section_entries: Vec<SectionEntry>,
    initial_figure_entries: Vec<CaptionEntry>,
    initial_table_entries: Vec<CaptionEntry>,
    initial_page_labels: BTreeMap<String, u32>,
    initial_index_entries: Vec<IndexEntry>,
) -> Result<ParsedDocument, ParseError> {
    parse_minimal_latex_with_context(
        source,
        initial_labels,
        initial_section_entries,
        initial_figure_entries,
        initial_table_entries,
        BTreeMap::new(),
        None,
        initial_page_labels,
        initial_index_entries,
    )
}

#[allow(clippy::too_many_arguments)]
fn parse_minimal_latex_with_context(
    source: &str,
    initial_labels: BTreeMap<String, String>,
    initial_section_entries: Vec<SectionEntry>,
    initial_figure_entries: Vec<CaptionEntry>,
    initial_table_entries: Vec<CaptionEntry>,
    initial_bibliography: BTreeMap<String, String>,
    initial_bibliography_state: Option<BibliographyState>,
    initial_page_labels: BTreeMap<String, u32>,
    initial_index_entries: Vec<IndexEntry>,
) -> Result<ParsedDocument, ParseError> {
    if source.trim().is_empty() {
        return Err(ParseError::EmptyInput);
    }

    ParserDriver::new_with_context(
        source,
        initial_labels,
        initial_section_entries,
        initial_figure_entries,
        initial_table_entries,
        initial_bibliography,
        initial_bibliography_state,
        initial_page_labels,
        initial_index_entries,
        None,
    )
    .run()
}

pub(crate) fn interpret_sty_package_source(
    source: &str,
    current_package_options: &[String],
    registry: &mut PackageRegistry,
    engine: &mut MacroEngine,
    sty_resolver: Option<&StyPackageResolver<'_>>,
) -> Result<(), String> {
    if source.trim().is_empty() {
        return Ok(());
    }

    let driver = ParserDriver::new_sty_interpreter(
        source,
        engine.clone(),
        registry.clone(),
        current_package_options.to_vec(),
        sty_resolver,
    );
    let (next_engine, next_registry) = driver
        .run_sty_interpreter()
        .map_err(|error| error.to_string())?;
    *engine = next_engine;
    *registry = next_registry;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Preamble,
    Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegisterKind {
    Count,
    Dimen,
    Skip,
    Muskip,
    CompatInt(CompatIntRegister),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArithmeticOperation {
    Advance,
    Multiply,
    Divide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsedDimensionValue {
    Unitless(i32),
    Scaled(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParserState {
    chapter: u32,
    section: u32,
    subsection: u32,
    subsubsection: u32,
    current_section_number: Option<String>,
    current_label_anchor: Option<String>,
    equation_counter: u32,
    figure_counter: u32,
    table_counter: u32,
    labels: BTreeMap<String, String>,
    page_labels: BTreeMap<String, u32>,
    page_label_anchors: BTreeMap<String, String>,
    citations: Vec<String>,
    bibliography: BTreeMap<String, String>,
    bibliography_state: BibliographyState,
    index_enabled: bool,
    index_entries: Vec<IndexRawEntry>,
    section_entries: Vec<SectionEntry>,
    figure_entries: Vec<CaptionEntry>,
    table_entries: Vec<CaptionEntry>,
    initial_section_entries: Vec<SectionEntry>,
    initial_figure_entries: Vec<CaptionEntry>,
    initial_table_entries: Vec<CaptionEntry>,
    initial_index_entries: Vec<IndexEntry>,
    has_unresolved_refs: bool,
    has_unresolved_toc: bool,
    has_unresolved_lof: bool,
    has_unresolved_lot: bool,
    has_unresolved_index: bool,
}

impl Default for ParserState {
    fn default() -> Self {
        Self::new(
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            BTreeMap::new(),
            None,
            BTreeMap::new(),
            Vec::new(),
        )
    }
}

impl ParserState {
    #[allow(clippy::too_many_arguments)]
    fn new(
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_figure_entries: Vec<CaptionEntry>,
        initial_table_entries: Vec<CaptionEntry>,
        initial_bibliography: BTreeMap<String, String>,
        initial_bibliography_state: Option<BibliographyState>,
        initial_page_labels: BTreeMap<String, u32>,
        initial_index_entries: Vec<IndexEntry>,
    ) -> Self {
        Self {
            chapter: 0,
            section: 0,
            subsection: 0,
            subsubsection: 0,
            current_section_number: None,
            current_label_anchor: None,
            equation_counter: 0,
            figure_counter: 0,
            table_counter: 0,
            labels: initial_labels,
            page_labels: initial_page_labels,
            page_label_anchors: BTreeMap::new(),
            citations: Vec::new(),
            bibliography: initial_bibliography,
            bibliography_state: initial_bibliography_state.unwrap_or_default(),
            index_enabled: false,
            index_entries: Vec::new(),
            section_entries: Vec::new(),
            figure_entries: Vec::new(),
            table_entries: Vec::new(),
            initial_section_entries,
            initial_figure_entries,
            initial_table_entries,
            initial_index_entries,
            has_unresolved_refs: false,
            has_unresolved_toc: false,
            has_unresolved_lof: false,
            has_unresolved_lot: false,
            has_unresolved_index: false,
        }
    }

    fn next_chapter_number(&mut self) -> String {
        self.chapter += 1;
        self.section = 0;
        self.subsection = 0;
        self.subsubsection = 0;
        let number = self.chapter.to_string();
        self.current_section_number = Some(number.clone());
        number
    }

    fn next_section_number(&mut self, level: u8, include_chapter: bool) -> String {
        let number = match level {
            1 => {
                self.section += 1;
                self.subsection = 0;
                self.subsubsection = 0;
                if include_chapter {
                    format!("{}.{}", self.chapter, self.section)
                } else {
                    self.section.to_string()
                }
            }
            2 => {
                self.subsection += 1;
                self.subsubsection = 0;
                if include_chapter {
                    format!("{}.{}.{}", self.chapter, self.section, self.subsection)
                } else {
                    format!("{}.{}", self.section, self.subsection)
                }
            }
            3 => {
                self.subsubsection += 1;
                if include_chapter {
                    format!(
                        "{}.{}.{}.{}",
                        self.chapter, self.section, self.subsection, self.subsubsection
                    )
                } else {
                    format!(
                        "{}.{}.{}",
                        self.section, self.subsection, self.subsubsection
                    )
                }
            }
            _ => unreachable!("section levels are constrained by the caller"),
        };
        self.current_section_number = Some(number.clone());
        number
    }

    fn citation_number(&mut self, key: &str) -> Option<String> {
        if self.bibliography_state.has_citations() {
            let citation = self.bibliography_state.resolve_citation(key)?;
            if !self
                .citations
                .iter()
                .any(|citation_key| citation_key == key)
            {
                self.citations.push(key.to_string());
            }
            return Some(citation.formatted_text.clone());
        }

        if !self.bibliography.contains_key(key) {
            return None;
        }

        if let Some(index) = self.citations.iter().position(|citation| citation == key) {
            Some((index + 1).to_string())
        } else {
            self.citations.push(key.to_string());
            Some(self.citations.len().to_string())
        }
    }

    fn register_bibliography_entry(&mut self, key: String, display_text: String) -> String {
        self.bibliography.insert(key.clone(), display_text.clone());
        let rendered_block = self
            .bibliography_state
            .upsert_entry(key.clone(), display_text)
            .rendered_block;
        let _ = self.citation_number(&key);
        rendered_block
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenEnvironment {
    name: String,
    line: u32,
    kind: OpenEnvironmentKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OpenEnvironmentKind {
    UserDefined { end_tokens: Vec<Token> },
    List(ListEnvironmentState),
    Float(FloatEnvironmentState),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ListEnvironmentState {
    kind: ListEnvironmentKind,
    item_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListEnvironmentKind {
    Itemize,
    Enumerate,
    Description,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FloatEnvironmentState {
    float_type: FloatType,
    caption: Option<String>,
    label: Option<String>,
}

struct ParserDriver<'a, 'resolver> {
    tokenizer: Tokenizer<'a>,
    macro_engine: MacroEngine,
    state: ParserState,
    package_registry: PackageRegistry,
    class_registry: ClassRegistry,
    option_registry: OptionRegistry,
    registers: RegisterStore,
    conditionals: ConditionalState,
    pending_tokens: VecDeque<QueuedToken>,
    environment_stack: Vec<OpenEnvironment>,
    runtime_group_stack: Vec<u32>,
    semisimple_group_stack: Vec<u32>,
    document_class_error: Option<ParseError>,
    errors: Vec<ParseError>,
    title: Option<String>,
    author: Option<String>,
    pdf_title: Option<String>,
    pdf_author: Option<String>,
    color_links: Option<bool>,
    link_color: Option<String>,
    main_font_name: Option<String>,
    sans_font_name: Option<String>,
    mono_font_name: Option<String>,
    body: String,
    begin_found: bool,
    end_found: bool,
    first_end_before_begin_line: Option<u32>,
    eof_line: u32,
    current_token_from_expansion: bool,
    consecutive_macro_expansions: usize,
    global_prefix: bool,
    protected_prefix: bool,
    current_package_options: Vec<String>,
    alloc_count: u32,
    alloc_toks: u32,
    sty_resolver: Option<&'resolver StyPackageResolver<'resolver>>,
    shell_escape_handler: Option<&'a dyn ShellEscapeHandler>,
    file_operation_handler: Option<&'a dyn FileOperationHandler>,
    sty_interpreter_mode: bool,
}

#[derive(Debug)]
struct QueuedToken {
    token: Token,
    from_expansion: bool,
}

impl<'a, 'resolver> ParserDriver<'a, 'resolver> {
    #[allow(clippy::too_many_arguments)]
    fn new_with_context(
        source: &'a str,
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_figure_entries: Vec<CaptionEntry>,
        initial_table_entries: Vec<CaptionEntry>,
        initial_bibliography: BTreeMap<String, String>,
        initial_bibliography_state: Option<BibliographyState>,
        initial_page_labels: BTreeMap<String, u32>,
        initial_index_entries: Vec<IndexEntry>,
        sty_resolver: Option<&'resolver StyPackageResolver<'resolver>>,
    ) -> Self {
        Self::new_with_context_and_handlers(
            source,
            initial_labels,
            initial_section_entries,
            initial_figure_entries,
            initial_table_entries,
            initial_bibliography,
            initial_bibliography_state,
            initial_page_labels,
            initial_index_entries,
            sty_resolver,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_context_and_handlers(
        source: &'a str,
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_figure_entries: Vec<CaptionEntry>,
        initial_table_entries: Vec<CaptionEntry>,
        initial_bibliography: BTreeMap<String, String>,
        initial_bibliography_state: Option<BibliographyState>,
        initial_page_labels: BTreeMap<String, u32>,
        initial_index_entries: Vec<IndexEntry>,
        sty_resolver: Option<&'resolver StyPackageResolver<'resolver>>,
        shell_escape_handler: Option<&'a dyn ShellEscapeHandler>,
        file_operation_handler: Option<&'a dyn FileOperationHandler>,
    ) -> Self {
        Self {
            tokenizer: Tokenizer::new(source.as_bytes()),
            macro_engine: MacroEngine::default(),
            state: ParserState::new(
                initial_labels,
                initial_section_entries,
                initial_figure_entries,
                initial_table_entries,
                initial_bibliography,
                initial_bibliography_state,
                initial_page_labels,
                initial_index_entries,
            ),
            package_registry: PackageRegistry::default(),
            class_registry: ClassRegistry::default(),
            option_registry: OptionRegistry::default(),
            registers: RegisterStore::default(),
            conditionals: ConditionalState::default(),
            pending_tokens: VecDeque::new(),
            environment_stack: Vec::new(),
            runtime_group_stack: Vec::new(),
            semisimple_group_stack: Vec::new(),
            document_class_error: None,
            errors: Vec::new(),
            title: None,
            author: None,
            pdf_title: None,
            pdf_author: None,
            color_links: None,
            link_color: None,
            main_font_name: None,
            sans_font_name: None,
            mono_font_name: None,
            body: String::new(),
            begin_found: false,
            end_found: false,
            first_end_before_begin_line: None,
            eof_line: eof_line(source),
            current_token_from_expansion: false,
            consecutive_macro_expansions: 0,
            global_prefix: false,
            protected_prefix: false,
            current_package_options: Vec::new(),
            alloc_count: 10,
            alloc_toks: 10,
            sty_resolver,
            shell_escape_handler,
            file_operation_handler,
            sty_interpreter_mode: false,
        }
    }

    fn new_sty_interpreter(
        source: &'a str,
        macro_engine: MacroEngine,
        package_registry: PackageRegistry,
        current_package_options: Vec<String>,
        sty_resolver: Option<&'resolver StyPackageResolver<'resolver>>,
    ) -> Self {
        let mut driver = Self::new_with_context(
            source,
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            BTreeMap::new(),
            None,
            BTreeMap::new(),
            Vec::new(),
            sty_resolver,
        );
        driver.macro_engine = macro_engine;
        driver.package_registry = package_registry;
        driver.current_package_options = current_package_options;
        driver.sty_interpreter_mode = true;
        driver.macro_engine.set_catcode(b'@', CatCode::Letter);
        driver.sync_tokenizer_catcodes();
        driver
    }

    fn record_error(&mut self, error: ParseError) {
        self.errors.push(error);
    }

    fn run(mut self) -> Result<ParsedDocument, ParseError> {
        let mut phase = Phase::Preamble;

        while let Some(token) = self.next_raw_token() {
            if self.conditionals.is_skipping() {
                self.push_front_queued_token(token, self.current_token_from_expansion);
                self.skip_current_false_branch();
                continue;
            }

            match phase {
                Phase::Preamble => {
                    if self.process_preamble_token(token)? {
                        phase = Phase::Body;
                    }
                }
                Phase::Body => {
                    if self.process_body_token(token)? {
                        break;
                    }
                }
            }
        }

        if let Some(line) = self.runtime_group_stack.last().copied() {
            return Err(ParseError::UnclosedBrace { line });
        }

        if let Some(line) = self.semisimple_group_stack.last().copied() {
            return Err(ParseError::UnclosedBrace { line });
        }

        if let Some(line) = self.conditionals.current_open_line() {
            return Err(ParseError::UnclosedConditional { line });
        }

        if let Some(open_environment) = self.environment_stack.last() {
            return Err(ParseError::UnclosedEnvironment {
                line: open_environment.line,
                name: open_environment.name.clone(),
            });
        }

        if !self.begin_found {
            return Err(ParseError::MissingBeginDocument {
                line: self.eof_line,
            });
        }

        if let Some(line) = self.first_end_before_begin_line {
            return Err(ParseError::UnexpectedEndDocument { line });
        }

        if !self.end_found {
            return Err(ParseError::MissingEndDocument {
                line: self.eof_line,
            });
        }

        if let Some(error) = self.document_class_error {
            return Err(error);
        }

        if self.class_registry.active_class().is_none() {
            return Err(ParseError::MissingDocumentClass);
        }

        self.validate_trailing_content()?;

        Ok(self.build_parsed_document())
    }

    fn run_recovering(mut self) -> ParseOutput {
        let mut phase = Phase::Preamble;

        while let Some(token) = self.next_raw_token() {
            if self.conditionals.is_skipping() {
                self.push_front_queued_token(token, self.current_token_from_expansion);
                self.skip_current_false_branch();
                continue;
            }

            let result = match phase {
                Phase::Preamble => match self.process_preamble_token(token) {
                    Ok(true) => {
                        phase = Phase::Body;
                        Ok(false)
                    }
                    Ok(false) => Ok(false),
                    Err(error) => Err(error),
                },
                Phase::Body => self.process_body_token(token),
            };

            match result {
                Ok(true) => break,
                Ok(false) => {}
                Err(error) => {
                    let recoverable = Self::is_recoverable_main_loop_error(&error);
                    self.record_error(error);
                    if !recoverable {
                        break;
                    }
                }
            }
        }

        if let Some(line) = self.runtime_group_stack.last().copied() {
            self.record_error(ParseError::UnclosedBrace { line });
        }

        if let Some(line) = self.semisimple_group_stack.last().copied() {
            self.record_error(ParseError::UnclosedBrace { line });
        }

        if let Some(line) = self.conditionals.current_open_line() {
            self.record_error(ParseError::UnclosedConditional { line });
        }

        for open_environment in self.environment_stack.clone() {
            self.record_error(ParseError::UnclosedEnvironment {
                line: open_environment.line,
                name: open_environment.name,
            });
        }

        if !self.begin_found {
            self.record_error(ParseError::MissingBeginDocument {
                line: self.eof_line,
            });
        }

        if let Some(line) = self.first_end_before_begin_line {
            self.record_error(ParseError::UnexpectedEndDocument { line });
        }

        if !self.end_found {
            self.record_error(ParseError::MissingEndDocument {
                line: self.eof_line,
            });
        }

        if let Some(error) = self.document_class_error.take() {
            self.record_error(error);
        }

        if self.class_registry.active_class().is_none() {
            self.record_error(ParseError::MissingDocumentClass);
        }

        let trailing_valid = self.validate_trailing_content();
        let has_trailing_error = trailing_valid.is_err();
        if let Err(error) = trailing_valid {
            self.record_error(error);
        }

        let document = if self.class_registry.active_class().is_some()
            && self.begin_found
            && self.end_found
            && !has_trailing_error
        {
            Some(self.build_parsed_document())
        } else {
            None
        };

        ParseOutput {
            document,
            errors: self.errors,
        }
    }

    fn run_sty_interpreter(mut self) -> Result<(MacroEngine, PackageRegistry), ParseError> {
        while let Some(token) = self.next_raw_token() {
            if self.conditionals.is_skipping() {
                self.push_front_queued_token(token, self.current_token_from_expansion);
                self.skip_current_false_branch();
                continue;
            }

            self.process_sty_token(token)?;
        }

        if let Some(line) = self.runtime_group_stack.last().copied() {
            return Err(ParseError::UnclosedBrace { line });
        }

        if let Some(line) = self.semisimple_group_stack.last().copied() {
            return Err(ParseError::UnclosedBrace { line });
        }

        if let Some(line) = self.conditionals.current_open_line() {
            return Err(ParseError::UnclosedConditional { line });
        }

        Ok((self.macro_engine, self.package_registry))
    }

    fn is_recoverable_main_loop_error(error: &ParseError) -> bool {
        matches!(
            error,
            ParseError::UnexpectedClosingBrace { .. }
                | ParseError::InvalidDocumentClass { .. }
                | ParseError::UnclosedEnvironment { .. }
                | ParseError::UnexpectedElse { .. }
                | ParseError::UnexpectedFi { .. }
        )
    }

    fn build_parsed_document(&self) -> ParsedDocument {
        let active_class = self
            .class_registry
            .active_class()
            .expect("document class presence checked above");

        ParsedDocument {
            document_class: active_class.name.clone(),
            class_options: active_class.options.clone(),
            loaded_packages: self.package_registry.loaded_packages().to_vec(),
            package_count: self.package_registry.loaded_packages().len(),
            main_font_name: self.main_font_name.clone(),
            sans_font_name: self.sans_font_name.clone(),
            mono_font_name: self.mono_font_name.clone(),
            body: self.body.trim().to_string(),
            labels: DocumentLabels::with_metadata(
                self.state.labels.clone(),
                self.state.citations.clone(),
                self.state.bibliography.clone(),
                self.state.section_entries.clone(),
                self.state.figure_entries.clone(),
                self.state.table_entries.clone(),
                self.state.page_label_anchors.clone(),
                self.title.clone(),
                self.author.clone(),
                self.pdf_title.clone(),
                self.pdf_author.clone(),
                self.color_links,
                self.link_color.clone(),
                self.state.index_enabled,
                self.state.index_entries.clone(),
                self.state.has_unresolved_toc,
                self.state.has_unresolved_lof,
                self.state.has_unresolved_lot,
                self.state.has_unresolved_index,
            ),
            bibliography_state: self.state.bibliography_state.clone(),
            has_unresolved_refs: self.state.has_unresolved_refs,
        }
    }

    fn process_preamble_token(&mut self, token: Token) -> Result<bool, ParseError> {
        if self.handle_runtime_group_token(&token)? {
            return Ok(false);
        }

        let Some(name) = control_sequence_name(&token) else {
            let _ = self.take_global_prefix();
            let _ = self.take_protected_prefix();
            return Ok(false);
        };

        if !preserves_protected_prefix(&name) {
            let _ = self.take_protected_prefix();
        }

        match name.as_str() {
            "documentclass" => {
                if self.class_registry.active_class().is_none()
                    && self.document_class_error.is_none()
                {
                    match self.parse_document_class_declaration() {
                        Ok(Some((class_name, options))) => {
                            if load_document_class(
                                &class_name,
                                &options,
                                &mut self.class_registry,
                                &mut self.macro_engine,
                            )
                            .is_err()
                            {
                                self.document_class_error =
                                    Some(ParseError::InvalidDocumentClass { line: token.line });
                            }
                        }
                        Ok(None) => {
                            self.document_class_error =
                                Some(ParseError::InvalidDocumentClass { line: token.line });
                        }
                        Err(ParseError::UnexpectedClosingBrace { line })
                        | Err(ParseError::UnclosedBrace { line }) => {
                            return Err(ParseError::InvalidDocumentClass { line });
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            "usepackage" | "RequirePackage" => {
                self.parse_package_directive(token.line)?;
            }
            "title" => {
                self.title = self.read_required_braced_tokens()?.map(|tokens| {
                    let text = tokens_to_text(&tokens);
                    text.trim().to_string()
                });
            }
            "author" => {
                self.author = self.read_required_braced_tokens()?.map(|tokens| {
                    let text = tokens_to_text(&tokens);
                    text.trim().to_string()
                });
            }
            "hypersetup" => {
                self.parse_hypersetup_command()?;
            }
            "makeindex" => {
                self.state.index_enabled = true;
            }
            "setmainfont" => self.parse_set_font_command(token.line, FontFamilyRole::Main)?,
            "setsansfont" => self.parse_set_font_command(token.line, FontFamilyRole::Sans)?,
            "setmonofont" => self.parse_set_font_command(token.line, FontFamilyRole::Mono)?,
            "begin" => {
                if self.read_environment_name()?.as_deref() == Some("document") {
                    self.begin_found = true;
                    return Ok(true);
                }
            }
            "end" => {
                if self.read_environment_name()?.as_deref() == Some("document") {
                    self.first_end_before_begin_line.get_or_insert(token.line);
                }
            }
            _ => {
                if self.handle_common_primitive(&token, &name)? {
                    return Ok(false);
                }
            }
        }

        Ok(false)
    }

    fn process_sty_token(&mut self, token: Token) -> Result<(), ParseError> {
        if self.handle_runtime_group_token(&token)? {
            return Ok(());
        }

        let Some(name) = control_sequence_name(&token) else {
            let _ = self.take_global_prefix();
            let _ = self.take_protected_prefix();
            return Ok(());
        };

        if !preserves_protected_prefix(&name) {
            let _ = self.take_protected_prefix();
        }

        match name.as_str() {
            "usepackage" | "RequirePackage" => {
                self.parse_package_directive(token.line)?;
            }
            "NeedsTeXFormat" => {
                let _ = self.take_global_prefix();
                let _ = self.read_required_braced_tokens()?;
                let _ = self.read_optional_bracket_tokens()?;
            }
            "ProvidesPackage" => {
                let _ = self.take_global_prefix();
                let _ = self.read_required_braced_tokens()?;
                let _ = self.read_optional_bracket_tokens()?;
            }
            "makeatletter" => {
                let _ = self.take_global_prefix();
                self.macro_engine.set_catcode(b'@', CatCode::Letter);
                self.sync_tokenizer_catcodes();
            }
            "makeatother" => {
                let _ = self.take_global_prefix();
                self.macro_engine.set_catcode(b'@', CatCode::Other);
                self.sync_tokenizer_catcodes();
            }
            "@ifpackageloaded" => {
                let _ = self.take_global_prefix();
                self.parse_ifpackageloaded()?;
            }
            "@namedef" => {
                let is_global = self.take_global_prefix();
                self.parse_at_namedef(is_global)?;
            }
            "@ifundefined" => {
                let _ = self.take_global_prefix();
                self.parse_ifundefined()?;
            }
            "newif" => {
                let is_global = self.take_global_prefix();
                self.parse_newif(token.line, is_global)?;
            }
            "input" => {
                let _ = self.take_global_prefix();
                let _ = self.read_required_braced_tokens()?;
            }
            _ => {
                if self.handle_common_primitive(&token, &name)? {
                    return Ok(());
                }

                let _ = self.take_global_prefix();
                if let Some(expansion) = self.expand_defined_control_sequence_token(&token)? {
                    self.push_front_tokens(expansion);
                }
            }
        }

        Ok(())
    }

    fn process_body_token(&mut self, token: Token) -> Result<bool, ParseError> {
        if self.handle_runtime_group_token(&token)? {
            return Ok(false);
        }

        match &token.kind {
            TokenKind::CharToken {
                cat: CatCode::MathShift,
                char: '$',
            } => {
                let _ = self.take_global_prefix();
                self.parse_inline_math()?;
                Ok(false)
            }
            TokenKind::ControlWord(name) if name == "par" => {
                let _ = self.take_global_prefix();
                let _ = self.take_protected_prefix();
                self.body.push_str("\n\n");
                Ok(false)
            }
            TokenKind::ControlWord(_) | TokenKind::ControlSymbol(_) => {
                let name = control_sequence_name(&token).expect("control sequence token");

                if !preserves_protected_prefix(&name) {
                    let _ = self.take_protected_prefix();
                }

                if self.handle_common_primitive(&token, &name)? {
                    return Ok(false);
                }

                match name.as_str() {
                    "pagebreak" | "newpage" => {
                        let _ = self.take_global_prefix();
                        self.body.push(BODY_PAGE_BREAK_MARKER);
                    }
                    "clearpage" => {
                        let _ = self.take_global_prefix();
                        self.body.push(BODY_CLEAR_PAGE_MARKER);
                    }
                    "cite" => {
                        let _ = self.take_global_prefix();
                        self.parse_cite_command()?;
                    }
                    "bibliography" => {
                        let _ = self.take_global_prefix();
                        self.parse_bibliography_command()?;
                    }
                    "bibliographystyle" => {
                        let _ = self.take_global_prefix();
                        let _ = self.read_required_braced_tokens()?;
                    }
                    "printbibliography" => {
                        let _ = self.take_global_prefix();
                        self.parse_printbibliography_command()?;
                    }
                    "href" => {
                        let _ = self.take_global_prefix();
                        self.parse_href_command()?;
                    }
                    "hyperref" => {
                        let _ = self.take_global_prefix();
                        self.parse_hyperref_command()?;
                    }
                    "url" => {
                        let _ = self.take_global_prefix();
                        self.parse_url_command()?;
                    }
                    "hypersetup" => {
                        let _ = self.take_global_prefix();
                        self.parse_hypersetup_command()?;
                    }
                    "includegraphics" => {
                        let _ = self.take_global_prefix();
                        self.parse_includegraphics_command()?;
                    }
                    "color" if self.package_registry.is_loaded("xcolor") => {
                        let _ = self.take_global_prefix();
                        self.parse_color_command()?;
                    }
                    "textcolor" if self.package_registry.is_loaded("xcolor") => {
                        let _ = self.take_global_prefix();
                        self.parse_textcolor_command()?;
                    }
                    "definecolor" if self.package_registry.is_loaded("xcolor") => {
                        let _ = self.take_global_prefix();
                        self.parse_definecolor_command()?;
                    }
                    "geometry" if self.package_registry.is_loaded("geometry") => {
                        let _ = self.take_global_prefix();
                        self.parse_geometry_command()?;
                    }
                    "chapter" if self.class_supports_chapters() => {
                        let _ = self.take_global_prefix();
                        self.parse_chapter_command()?;
                    }
                    "section" => {
                        let _ = self.take_global_prefix();
                        self.parse_section_command(1)?;
                    }
                    "subsection" => {
                        let _ = self.take_global_prefix();
                        self.parse_section_command(2)?;
                    }
                    "subsubsection" => {
                        let _ = self.take_global_prefix();
                        self.parse_section_command(3)?;
                    }
                    "label" => {
                        let _ = self.take_global_prefix();
                        self.parse_label_command()?;
                    }
                    "caption" => {
                        let _ = self.take_global_prefix();
                        self.parse_caption_command()?;
                    }
                    "ref" => {
                        let _ = self.take_global_prefix();
                        self.parse_ref_command()?;
                    }
                    "pageref" => {
                        let _ = self.take_global_prefix();
                        let Some(tokens) = self.read_required_braced_tokens()? else {
                            return Ok(false);
                        };
                        let key = tokens_to_text(&tokens).trim().to_string();
                        if key.is_empty() {
                            return Ok(false);
                        }

                        if let Some(page_number) = self.state.page_labels.get(&key).copied() {
                            self.emit_internal_link(&key, &page_number.to_string());
                        } else {
                            self.body.push(BODY_PAGEREF_START);
                            self.body.push_str(&key);
                            self.body.push(BODY_PAGEREF_END);
                            self.state.has_unresolved_refs = true;
                        }
                    }
                    "index" => {
                        let _ = self.take_global_prefix();
                        self.parse_index_command()?;
                    }
                    "printindex" => {
                        let _ = self.take_global_prefix();
                        self.parse_printindex()?;
                    }
                    "[" => {
                        let _ = self.take_global_prefix();
                        self.parse_display_math()?;
                    }
                    "hbox" | "vbox" => {
                        let _ = self.take_global_prefix();
                        if let Some(tokens) = self.read_required_braced_tokens()? {
                            let content = encode_body_markers_in_text(&tokens_to_text(&tokens));
                            let (start_marker, end_marker) = if name == "hbox" {
                                (BODY_HBOX_START, BODY_HBOX_END)
                            } else {
                                (BODY_VBOX_START, BODY_VBOX_END)
                            };
                            self.body.push(start_marker);
                            self.body.push_str(&content);
                            self.body.push(end_marker);
                        }
                    }
                    "textsf" => {
                        let _ = self.take_global_prefix();
                        self.parse_font_family_body_command(FontFamilyRole::Sans)?;
                    }
                    "texttt" => {
                        let _ = self.take_global_prefix();
                        self.parse_font_family_body_command(FontFamilyRole::Mono)?;
                    }
                    "begin" => {
                        let _ = self.take_global_prefix();
                        self.parse_begin_environment(token.line)?;
                    }
                    "end" => {
                        let _ = self.take_global_prefix();
                        if self.parse_end_environment()? {
                            return Ok(true);
                        }
                    }
                    "item" => {
                        let _ = self.take_global_prefix();
                        self.parse_item_command()?;
                    }
                    "tableofcontents" => {
                        let _ = self.take_global_prefix();
                        self.parse_table_of_contents();
                    }
                    "listoffigures" => {
                        let _ = self.take_global_prefix();
                        self.parse_list_of_figures();
                    }
                    "listoftables" => {
                        let _ = self.take_global_prefix();
                        self.parse_list_of_tables();
                    }
                    "setmainfont" | "setsansfont" | "setmonofont" => {
                        let _ = self.take_global_prefix();
                        self.record_error(ParseError::SetmainfontInBody { line: token.line });
                        let _ = self.read_required_braced_tokens()?;
                    }
                    _ => {
                        let _ = self.take_global_prefix();
                        if let Some(expansion) =
                            self.expand_defined_control_sequence_token(&token)?
                        {
                            self.push_front_tokens(expansion);
                        } else {
                            self.body.push_str(&render_token(&token));
                        }
                    }
                }

                Ok(false)
            }
            _ => {
                let _ = self.take_global_prefix();
                let _ = self.take_protected_prefix();
                if self.should_skip_insignificant_space(&token) {
                    return Ok(false);
                }
                self.body.push_str(&render_token(&token));
                Ok(false)
            }
        }
    }

    fn parse_set_font_command(
        &mut self,
        line: u32,
        role: FontFamilyRole,
    ) -> Result<(), ParseError> {
        let font_tokens = self.read_required_braced_tokens()?;
        if self.package_registry.is_loaded("fontspec") {
            if let Some(tokens) = font_tokens {
                let font_name = tokens_to_text(&tokens).trim().to_string();
                if !font_name.is_empty() {
                    *self.font_name_slot_mut(role) = Some(font_name);
                }
            }
        } else {
            self.record_error(ParseError::FontspecNotLoaded { line });
        }
        Ok(())
    }

    fn parse_font_family_body_command(&mut self, role: FontFamilyRole) -> Result<(), ParseError> {
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let content = encode_body_markers_in_text(&tokens_to_text(&tokens));
        let (start_marker, end_marker) = font_family_markers(role);
        self.body.push(start_marker);
        self.body.push_str(&content);
        self.body.push(end_marker);
        Ok(())
    }

    fn font_name_slot_mut(&mut self, role: FontFamilyRole) -> &mut Option<String> {
        match role {
            FontFamilyRole::Main => &mut self.main_font_name,
            FontFamilyRole::Sans => &mut self.sans_font_name,
            FontFamilyRole::Mono => &mut self.mono_font_name,
        }
    }

    fn handle_write18_command(&mut self, line: u32) -> Result<(), ParseError> {
        let command = self
            .read_required_braced_tokens()?
            .map(|tokens| tokens_to_text(&tokens).trim().to_string())
            .unwrap_or_default();
        let Some(handler) = self.shell_escape_handler else {
            self.record_error(ParseError::ShellEscapeNotAllowed { line, command });
            return Ok(());
        };

        match handler.execute_write18(&command, line) {
            ShellEscapeResult::Denied => {
                self.record_error(ParseError::ShellEscapeNotAllowed { line, command });
            }
            ShellEscapeResult::Success { .. } => {}
            ShellEscapeResult::Error(message) => {
                self.record_error(ParseError::ShellEscapeError { line, message });
            }
        }
        Ok(())
    }

    fn try_handle_write18(&mut self, line: u32) -> Result<bool, ParseError> {
        let Some((mut leading, first)) = self.next_non_space_token_with_leading() else {
            return Ok(false);
        };
        let TokenKind::CharToken { char, .. } = first.kind else {
            self.push_back_with_leading(leading, first);
            return Ok(false);
        };
        if !char.is_ascii_digit() {
            self.push_back_with_leading(leading, first);
            return Ok(false);
        }

        let mut digits = String::new();
        digits.push(char);
        let mut consumed = vec![first];
        while let Some(token) = self.next_raw_token() {
            match token.kind {
                TokenKind::CharToken { char, .. } if char.is_ascii_digit() => {
                    digits.push(char);
                    consumed.push(token);
                }
                _ => {
                    self.push_front_token(token);
                    break;
                }
            }
        }

        if digits == "18" {
            self.handle_write18_command(line)?;
            Ok(true)
        } else {
            leading.extend(consumed);
            self.push_front_plain_tokens(leading);
            Ok(false)
        }
    }

    fn handle_file_operation(
        &mut self,
        line: u32,
        operation: FileOperationKind,
    ) -> Result<(), ParseError> {
        let _ = self.parse_unsigned_integer()?;
        let _ = self.consume_optional_equals();
        let path = self.read_file_operation_path()?;
        // A missing handler means the parser is running without an application-layer
        // FileOperationAdapter. The parser itself never performs I/O, so in this mode
        // file operations are parsed without diagnostics; real gating happens where the
        // adapter is always supplied.
        let Some(handler) = self.file_operation_handler else {
            return Ok(());
        };

        let result = match operation {
            FileOperationKind::OpenIn => handler.check_open_read(&path, line),
            FileOperationKind::OpenOut => handler.check_open_write(&path, line),
        };
        if let FileOperationResult::Denied { path, reason } = result {
            self.record_error(ParseError::FileOperationDenied {
                line,
                operation,
                path,
                reason,
            });
        }
        Ok(())
    }

    fn read_file_operation_path(&mut self) -> Result<String, ParseError> {
        let Some(first) = self.next_significant_token() else {
            return Ok(String::new());
        };
        let mut tokens = match first.kind {
            TokenKind::CharToken {
                cat: CatCode::BeginGroup,
                ..
            } => {
                return Ok(tokens_to_text(&self.read_group_contents(first.line)?)
                    .trim()
                    .to_string())
            }
            TokenKind::CharToken {
                cat: CatCode::EndGroup,
                ..
            } => return Err(ParseError::UnexpectedClosingBrace { line: first.line }),
            _ => vec![first],
        };
        while let Some(token) = self.next_raw_token() {
            match token.kind {
                TokenKind::CharToken {
                    cat: CatCode::Space,
                    ..
                } => break,
                TokenKind::ControlWord(ref name) if name == "par" => break,
                _ => tokens.push(token),
            }
        }

        Ok(tokens_to_text(&tokens).trim().to_string())
    }

    fn handle_runtime_group_token(&mut self, token: &Token) -> Result<bool, ParseError> {
        match token.kind {
            TokenKind::CharToken {
                cat: CatCode::BeginGroup,
                ..
            } => {
                let _ = self.take_global_prefix();
                let _ = self.take_protected_prefix();
                self.runtime_group_stack.push(token.line);
                self.macro_engine.push_group();
                self.registers.push_group();
                Ok(true)
            }
            TokenKind::CharToken {
                cat: CatCode::EndGroup,
                ..
            } => {
                let _ = self.take_global_prefix();
                let _ = self.take_protected_prefix();
                if self.runtime_group_stack.pop().is_none() {
                    return Err(ParseError::UnexpectedClosingBrace { line: token.line });
                }
                self.macro_engine.pop_group();
                self.registers.pop_group();
                self.sync_tokenizer_catcodes();
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn handle_common_primitive(&mut self, token: &Token, name: &str) -> Result<bool, ParseError> {
        match name {
            "relax" => {
                let _ = self.take_global_prefix();
                Ok(true)
            }
            "protected" => {
                self.protected_prefix = true;
                Ok(true)
            }
            "def" => {
                let is_global = self.take_global_prefix();
                self.parse_def(is_global)?;
                Ok(true)
            }
            "let" => {
                let is_global = self.take_global_prefix();
                self.parse_let(is_global)?;
                Ok(true)
            }
            "edef" => {
                let is_global = self.take_global_prefix();
                self.parse_edef(is_global)?;
                Ok(true)
            }
            "gdef" => {
                let _ = self.take_global_prefix();
                self.parse_def(true)?;
                Ok(true)
            }
            "newcommand" | "renewcommand" => {
                let _ = self.take_global_prefix();
                self.parse_newcommand()?;
                Ok(true)
            }
            "newenvironment" | "renewenvironment" => {
                let _ = self.take_global_prefix();
                self.parse_newenvironment()?;
                Ok(true)
            }
            "DeclareOption" => {
                let _ = self.take_global_prefix();
                self.parse_declare_option()?;
                Ok(true)
            }
            "ProcessOptions" => {
                let _ = self.take_global_prefix();
                self.parse_process_options()?;
                Ok(true)
            }
            "ExecuteOptions" => {
                let _ = self.take_global_prefix();
                self.parse_execute_options()?;
                Ok(true)
            }
            "catcode" => {
                let _ = self.take_global_prefix();
                self.parse_catcode()?;
                Ok(true)
            }
            "global" => {
                self.global_prefix = true;
                Ok(true)
            }
            "immediate" => {
                let _ = self.take_global_prefix();
                if let Some(next) = self.next_significant_token() {
                    match control_sequence_name(&next).as_deref() {
                        Some("write18") => self.handle_write18_command(next.line)?,
                        Some("write") => {
                            if !self.try_handle_write18(next.line)? {
                                self.push_front_token(next);
                            }
                        }
                        _ => self.push_front_token(next),
                    }
                }
                Ok(true)
            }
            "write18" => {
                let _ = self.take_global_prefix();
                self.handle_write18_command(token.line)?;
                Ok(true)
            }
            "write" => {
                let _ = self.take_global_prefix();
                self.try_handle_write18(token.line)
            }
            "openin" => {
                let _ = self.take_global_prefix();
                self.handle_file_operation(token.line, FileOperationKind::OpenIn)?;
                Ok(true)
            }
            "openout" => {
                let _ = self.take_global_prefix();
                self.handle_file_operation(token.line, FileOperationKind::OpenOut)?;
                Ok(true)
            }
            "noexpand" => {
                let _ = self.take_global_prefix();
                if let Some(next) = self.next_raw_token() {
                    self.push_front_token(next);
                }
                Ok(true)
            }
            "expandafter" => {
                let _ = self.take_global_prefix();
                self.parse_expandafter()?;
                Ok(true)
            }
            "begingroup" => {
                let _ = self.take_global_prefix();
                self.semisimple_group_stack.push(token.line);
                self.macro_engine.push_group();
                self.registers.push_group();
                Ok(true)
            }
            "endgroup" => {
                let _ = self.take_global_prefix();
                if self.semisimple_group_stack.pop().is_none() {
                    return Err(ParseError::UnexpectedClosingBrace { line: token.line });
                }
                self.macro_engine.pop_group();
                self.registers.pop_group();
                self.sync_tokenizer_catcodes();
                Ok(true)
            }
            "csname" => {
                let _ = self.take_global_prefix();
                self.parse_csname(token.line)?;
                Ok(true)
            }
            "pdfoutput" => {
                self.parse_compat_int_primitive(CompatIntRegister::PdfOutput, token.line)?;
                Ok(true)
            }
            "pdftexversion" => {
                self.parse_compat_int_primitive(CompatIntRegister::PdfTexVersion, token.line)?;
                Ok(true)
            }
            "pdfstrcmp" => {
                let _ = self.take_global_prefix();
                let value = self.parse_pdfstrcmp_value(token.line)?;
                self.push_front_tokens(tokens_from_text(&value.to_string(), token.line));
                Ok(true)
            }
            "pdffilesize" => {
                let _ = self.take_global_prefix();
                let value = self.parse_pdffilesize_value()?;
                self.push_front_tokens(tokens_from_text(&value.to_string(), token.line));
                Ok(true)
            }
            "pdftexbanner" => {
                let _ = self.take_global_prefix();
                self.push_front_tokens(tokens_from_text("This is ferritex", token.line));
                Ok(true)
            }
            "numexpr" => {
                let _ = self.take_global_prefix();
                let value = self.parse_numexpr_value(token.line)?;
                self.push_front_tokens(tokens_from_text(&value.to_string(), token.line));
                Ok(true)
            }
            "dimexpr" => {
                let _ = self.take_global_prefix();
                let value = self.parse_dimexpr_value(token.line)?;
                self.push_front_tokens(tokens_from_text(&format_dimen(value), token.line));
                Ok(true)
            }
            "detokenize" => {
                let _ = self.take_global_prefix();
                let Some(tokens) = self.read_required_braced_tokens()? else {
                    return Ok(true);
                };
                self.push_front_plain_tokens(detokenized_tokens(&tokens, token.line));
                Ok(true)
            }
            "unexpanded" => {
                let _ = self.take_global_prefix();
                let Some(tokens) = self.read_required_braced_tokens()? else {
                    return Ok(true);
                };
                self.push_front_plain_tokens(tokens);
                Ok(true)
            }
            "endcsname" => {
                let _ = self.take_global_prefix();
                Ok(true)
            }
            "count" => {
                self.parse_register_assignment(RegisterKind::Count, token.line)?;
                Ok(true)
            }
            "dimen" => {
                self.parse_register_assignment(RegisterKind::Dimen, token.line)?;
                Ok(true)
            }
            "skip" => {
                self.parse_register_assignment(RegisterKind::Skip, token.line)?;
                Ok(true)
            }
            "muskip" => {
                self.parse_register_assignment(RegisterKind::Muskip, token.line)?;
                Ok(true)
            }
            "toks" => {
                self.parse_toks_assignment(token.line)?;
                Ok(true)
            }
            "newcount" => {
                let is_global = self.take_global_prefix();
                self.parse_newcount(is_global, token.line)?;
                Ok(true)
            }
            "newtoks" => {
                let is_global = self.take_global_prefix();
                self.parse_newtoks(is_global, token.line)?;
                Ok(true)
            }
            "countdef" => {
                let is_global = self.take_global_prefix();
                self.parse_countdef(is_global, token.line)?;
                Ok(true)
            }
            "toksdef" => {
                let is_global = self.take_global_prefix();
                self.parse_toksdef(is_global, token.line)?;
                Ok(true)
            }
            "the" => {
                let _ = self.take_global_prefix();
                self.expand_the(token.line)?;
                Ok(true)
            }
            "advance" => {
                self.apply_arithmetic(ArithmeticOperation::Advance, token.line)?;
                Ok(true)
            }
            "multiply" => {
                self.apply_arithmetic(ArithmeticOperation::Multiply, token.line)?;
                Ok(true)
            }
            "divide" => {
                self.apply_arithmetic(ArithmeticOperation::Divide, token.line)?;
                Ok(true)
            }
            "iftrue" => {
                let _ = self.take_global_prefix();
                self.conditionals.process_if_at(true, token.line);
                Ok(true)
            }
            "iffalse" => {
                let _ = self.take_global_prefix();
                self.conditionals.process_if_at(false, token.line);
                self.skip_current_false_branch();
                Ok(true)
            }
            "ifnum" => {
                let _ = self.take_global_prefix();
                let left = self.parse_integer_value()?.unwrap_or(0);
                let relation = self.parse_relation_token().unwrap_or('=');
                let right = self.parse_integer_value()?.unwrap_or(0);
                self.conditionals
                    .process_if_at(evaluate_ifnum(left, relation, right), token.line);
                if self.conditionals.is_skipping() {
                    self.skip_current_false_branch();
                }
                Ok(true)
            }
            "if" => {
                let _ = self.take_global_prefix();
                let left = self.next_significant_token();
                let right = self.next_significant_token();
                let condition = match (left.as_ref(), right.as_ref()) {
                    (Some(left), Some(right)) => {
                        matches!(
                            (char_code_of(left), char_code_of(right)),
                            (Some(left_code), Some(right_code)) if left_code == right_code
                        )
                    }
                    _ => false,
                };
                self.conditionals.process_if_at(condition, token.line);
                if self.conditionals.is_skipping() {
                    self.skip_current_false_branch();
                }
                Ok(true)
            }
            "ifcat" => {
                let _ = self.take_global_prefix();
                let left = self.next_significant_token();
                let right = self.next_significant_token();
                let condition = match (left.as_ref(), right.as_ref()) {
                    (Some(left), Some(right)) => {
                        matches!(
                            (cat_code_of(left), cat_code_of(right)),
                            (Some(left_cat), Some(right_cat)) if left_cat == right_cat
                        )
                    }
                    _ => false,
                };
                self.conditionals.process_if_at(condition, token.line);
                if self.conditionals.is_skipping() {
                    self.skip_current_false_branch();
                }
                Ok(true)
            }
            "ifdim" => {
                let _ = self.take_global_prefix();
                let left = self.parse_dimension_value()?.unwrap_or(0);
                let relation = self.parse_relation_token().unwrap_or('=');
                let right = self.parse_dimension_value()?.unwrap_or(0);
                self.conditionals
                    .process_if_at(evaluate_ifnum(left, relation, right), token.line);
                if self.conditionals.is_skipping() {
                    self.skip_current_false_branch();
                }
                Ok(true)
            }
            "ifx" => {
                let _ = self.take_global_prefix();
                let left = self.next_significant_token();
                let right = self.next_significant_token();
                let condition = match (left.as_ref(), right.as_ref()) {
                    (Some(left), Some(right)) => self.tokens_match_for_ifx(left, right),
                    _ => false,
                };
                self.conditionals.process_if_at(condition, token.line);
                if self.conditionals.is_skipping() {
                    self.skip_current_false_branch();
                }
                Ok(true)
            }
            "ifdefined" => {
                let _ = self.take_global_prefix();
                let condition = self
                    .next_significant_token()
                    .and_then(|name_token| control_sequence_name(&name_token))
                    .map(|name| self.macro_engine.lookup(&name).is_some())
                    .unwrap_or(false);
                self.conditionals.process_if_at(condition, token.line);
                if self.conditionals.is_skipping() {
                    self.skip_current_false_branch();
                }
                Ok(true)
            }
            "ifcsname" => {
                let _ = self.take_global_prefix();
                let name = self.read_csname_name(token.line)?;
                let condition = !name.is_empty() && self.macro_engine.lookup(&name).is_some();
                self.conditionals.process_if_at(condition, token.line);
                if self.conditionals.is_skipping() {
                    self.skip_current_false_branch();
                }
                Ok(true)
            }
            "ifcase" => {
                let _ = self.take_global_prefix();
                let value = self.parse_integer_value()?.unwrap_or(0);
                self.conditionals.process_ifcase_at(value, token.line);
                if self.conditionals.is_skipping() {
                    self.skip_current_false_branch();
                }
                Ok(true)
            }
            "else" => {
                let _ = self.take_global_prefix();
                if !self.conditionals.process_else() {
                    return Err(ParseError::UnexpectedElse { line: token.line });
                }
                if self.conditionals.is_skipping() {
                    self.skip_current_false_branch();
                }
                Ok(true)
            }
            "or" => {
                let _ = self.take_global_prefix();
                if self.conditionals.is_empty() {
                    return Ok(false);
                }

                if self.conditionals.top_is_ifcase() {
                    let _ = self.conditionals.process_or();
                    if self.conditionals.is_skipping() {
                        self.skip_current_false_branch();
                    }
                }

                Ok(true)
            }
            "fi" => {
                let _ = self.take_global_prefix();
                if !self.conditionals.process_fi() {
                    return Err(ParseError::UnexpectedFi { line: token.line });
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn parse_begin_environment(&mut self, line: u32) -> Result<(), ParseError> {
        let Some(name) = self.read_environment_name()? else {
            self.body.push_str("begin");
            return Ok(());
        };

        if name == "document" {
            self.body.push_str("document");
            return Ok(());
        }

        if name == "thebibliography" {
            self.parse_thebibliography_environment()?;
            return Ok(());
        }

        if is_math_environment(&name) {
            self.parse_math_environment(&name, line)?;
            return Ok(());
        }

        if let Some(kind) = list_environment_kind(&name) {
            self.emit_paragraph_break_before_block();
            self.environment_stack.push(OpenEnvironment {
                name,
                line,
                kind: OpenEnvironmentKind::List(ListEnvironmentState {
                    kind,
                    item_count: 0,
                }),
            });
            return Ok(());
        }

        if name == "figure" || name == "table" {
            let specifier = self
                .read_optional_bracket_tokens()?
                .map(|tokens| tokens_to_text(&tokens))
                .filter(|value| !value.is_empty());
            let float_type = if name == "figure" {
                FloatType::Figure
            } else {
                FloatType::Table
            };

            self.emit_paragraph_break_before_block();
            self.body.push(BODY_FLOAT_START);
            self.body.push(float_type_marker(float_type));
            self.body.push(BODY_FLOAT_TYPE_SEP);
            self.body.push_str(specifier.as_deref().unwrap_or_default());
            self.body.push(BODY_FLOAT_SPECIFIER_SEP);
            self.environment_stack.push(OpenEnvironment {
                name,
                line,
                kind: OpenEnvironmentKind::Float(FloatEnvironmentState {
                    float_type,
                    caption: None,
                    label: None,
                }),
            });
            return Ok(());
        }

        if name == "tikzpicture" {
            let _ = self.read_optional_bracket_tokens()?;
            let content = self.read_raw_environment_body(&name, line)?;
            let parse_result = parse_tikzpicture(&content);
            let TikzParseResult { scene, diagnostics } = parse_result;
            for diag in diagnostics {
                let message = match diag {
                    TikzDiagnostic::UnsupportedCommand { command } => {
                        format!("tikz: unsupported command `{command}`")
                    }
                    TikzDiagnostic::ParseError { message } => {
                        format!("tikz: {message}")
                    }
                };
                self.record_error(ParseError::TikzDiagnostic { line, message });
            }
            let graphics_box = compile_graphics_scene(scene);
            self.emit_paragraph_break_before_block();
            self.body.push(BODY_TIKZPICTURE_START);
            self.body
                .push_str(&serialize_tikzpicture_marker(&graphics_box));
            self.body.push(BODY_TIKZPICTURE_END);
            self.emit_paragraph_break_before_block();
            return Ok(());
        }

        let Some(definition) = self.macro_engine.lookup_environment(&name).cloned() else {
            self.body.push_str(&name);
            return Ok(());
        };

        let arguments = self.collect_macro_arguments(definition.parameter_count)?;
        let begin_tokens = expand_parameter_tokens(&definition.begin_tokens, &arguments);
        let end_tokens = expand_parameter_tokens(&definition.end_tokens, &arguments);
        self.macro_engine.push_group();
        self.registers.push_group();
        self.environment_stack.push(OpenEnvironment {
            name,
            line,
            kind: OpenEnvironmentKind::UserDefined { end_tokens },
        });
        self.push_front_tokens(begin_tokens);
        Ok(())
    }

    fn parse_end_environment(&mut self) -> Result<bool, ParseError> {
        let Some(name) = self.read_environment_name()? else {
            self.body.push_str("end");
            return Ok(false);
        };

        if name == "document" {
            self.end_found = true;
            return Ok(true);
        }

        if matches!(
            self.environment_stack.last(),
            Some(OpenEnvironment {
                name: open_name,
                kind: OpenEnvironmentKind::Float(_),
                ..
            }) if open_name == &name
        ) {
            let Some(open_environment) = self.environment_stack.pop() else {
                return Ok(false);
            };
            let OpenEnvironmentKind::Float(float_state) = open_environment.kind else {
                unreachable!("checked float environment before pop");
            };

            let number = match float_state.float_type {
                FloatType::Figure => {
                    self.state.figure_counter += 1;
                    self.state.figure_counter.to_string()
                }
                FloatType::Table => {
                    self.state.table_counter += 1;
                    self.state.table_counter.to_string()
                }
            };

            self.state.current_section_number = Some(number.clone());
            self.state.current_label_anchor = None;
            if let Some(label) = float_state.label.as_ref() {
                self.state.labels.insert(label.clone(), number.clone());
                if let Some(caption) = float_state.caption.as_deref() {
                    self.state.page_label_anchors.insert(
                        label.clone(),
                        float_anchor_text(float_state.float_type, &number, caption),
                    );
                }
            }

            if float_state.caption.is_some() {
                let caption_entry = CaptionEntry {
                    kind: float_state.float_type,
                    number,
                    caption: float_state.caption.clone().unwrap_or_default(),
                };
                match float_state.float_type {
                    FloatType::Figure => self.state.figure_entries.push(caption_entry),
                    FloatType::Table => self.state.table_entries.push(caption_entry),
                }
            }

            self.body.push(BODY_FLOAT_CAPTION_SEP);
            self.body
                .push_str(float_state.caption.as_deref().unwrap_or_default());
            self.body.push(BODY_FLOAT_LABEL_SEP);
            self.body
                .push_str(float_state.label.as_deref().unwrap_or_default());
            self.body.push(BODY_FLOAT_END);
            self.emit_paragraph_break_before_block();
            return Ok(false);
        }

        if list_environment_kind(&name).is_some() {
            if matches!(
                self.environment_stack.last(),
                Some(OpenEnvironment {
                    name: open_name,
                    kind: OpenEnvironmentKind::List(_),
                    ..
                }) if open_name == &name
            ) {
                let _ = self.environment_stack.pop();
                self.emit_paragraph_break_before_block();
            } else {
                self.body.push_str(&name);
            }
            return Ok(false);
        }

        let Some(open_environment) = self.environment_stack.last().cloned() else {
            self.body.push_str(&name);
            return Ok(false);
        };

        if open_environment.name != name {
            self.body.push_str(&name);
            return Ok(false);
        }

        match open_environment.kind {
            OpenEnvironmentKind::UserDefined { end_tokens } => {
                let _ = self.environment_stack.pop();
                self.macro_engine.pop_group();
                self.registers.pop_group();
                self.sync_tokenizer_catcodes();
                self.push_front_tokens(end_tokens);
            }
            OpenEnvironmentKind::List(_) => {
                self.body.push_str(&name);
            }
            OpenEnvironmentKind::Float(_) => {
                self.body.push_str(&name);
            }
        }

        Ok(false)
    }

    fn parse_item_command(&mut self) -> Result<(), ParseError> {
        enum Marker {
            Bullet,
            Numbered(usize),
            Description,
        }

        let Some(marker) = ({
            self.environment_stack
                .iter_mut()
                .rev()
                .find_map(|environment| match &mut environment.kind {
                    OpenEnvironmentKind::List(list_state) => Some(match list_state.kind {
                        ListEnvironmentKind::Itemize => Marker::Bullet,
                        ListEnvironmentKind::Enumerate => {
                            list_state.item_count += 1;
                            Marker::Numbered(list_state.item_count)
                        }
                        ListEnvironmentKind::Description => Marker::Description,
                    }),
                    OpenEnvironmentKind::UserDefined { .. } | OpenEnvironmentKind::Float(_) => None,
                })
        }) else {
            self.body.push_str("item");
            return Ok(());
        };

        self.emit_paragraph_break_before_block();
        match marker {
            Marker::Bullet => self.body.push_str("• "),
            Marker::Numbered(number) => self.body.push_str(&format!("{number}. ")),
            Marker::Description => {
                if let Some(term_tokens) = self.read_optional_bracket_tokens()? {
                    let term = tokens_to_text(&term_tokens).trim().to_string();
                    if !term.is_empty() {
                        self.body.push_str(&term);
                        self.body.push_str(": ");
                        if let Some(next) = self.next_raw_token() {
                            if !matches!(
                                next.kind,
                                TokenKind::CharToken {
                                    cat: CatCode::Space,
                                    ..
                                }
                            ) {
                                self.push_front_token(next);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn parse_table_of_contents(&mut self) {
        let entries = if self.state.initial_section_entries.is_empty() {
            self.state.has_unresolved_toc = true;
            self.state.section_entries.clone()
        } else {
            self.state.initial_section_entries.clone()
        };

        if entries.is_empty() {
            return;
        }

        self.emit_paragraph_break_before_block();
        for entry in entries {
            let mut line = entry.number.clone();
            if !entry.title.is_empty() {
                line.push_str("  ");
                line.push_str(&entry.title);
            }
            let destination_name = section_destination_name(&entry.display_title());
            if destination_name.is_empty() {
                self.body.push_str(&line);
            } else {
                self.emit_internal_link(&destination_name, &line);
            }
            self.body.push('\n');
        }
        self.body.push('\n');
    }

    fn parse_list_of_figures(&mut self) {
        let entries = if self.state.initial_figure_entries.is_empty() {
            self.state.has_unresolved_lof = true;
            self.state.figure_entries.clone()
        } else {
            self.state.initial_figure_entries.clone()
        };

        if entries.is_empty() {
            return;
        }

        self.emit_paragraph_break_before_block();
        for entry in entries {
            self.body.push_str(&entry.display_title());
            self.body.push('\n');
        }
        self.body.push('\n');
    }

    fn parse_list_of_tables(&mut self) {
        let entries = if self.state.initial_table_entries.is_empty() {
            self.state.has_unresolved_lot = true;
            self.state.table_entries.clone()
        } else {
            self.state.initial_table_entries.clone()
        };

        if entries.is_empty() {
            return;
        }

        self.emit_paragraph_break_before_block();
        for entry in entries {
            self.body.push_str(&entry.display_title());
            self.body.push('\n');
        }
        self.body.push('\n');
    }

    fn parse_index_command(&mut self) -> Result<(), ParseError> {
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let raw = tokens_to_text(&tokens);
        let Some(entry) = parse_index_entry_argument(&raw) else {
            return Ok(());
        };

        if !self.state.index_enabled {
            return Ok(());
        }

        self.state.index_entries.push(entry.clone());
        self.body.push(BODY_INDEX_ENTRY_START);
        self.body.push_str(&serialize_index_marker(&entry));
        self.body.push(BODY_INDEX_ENTRY_END);
        Ok(())
    }

    fn parse_printindex(&mut self) -> Result<(), ParseError> {
        let _ = self.read_optional_bracket_tokens()?;

        if self.state.initial_index_entries.is_empty() {
            self.state.has_unresolved_index = self.state.index_enabled;
            return Ok(());
        }

        let rendered = format_index_entries(&self.state.initial_index_entries);
        if rendered.is_empty() {
            return Ok(());
        }

        self.emit_paragraph_break_before_block();
        self.body.push_str(&rendered);
        self.body.push('\n');
        Ok(())
    }

    fn class_supports_chapters(&self) -> bool {
        matches!(
            self.class_registry
                .active_class()
                .map(|info| info.name.as_str()),
            Some("book" | "report")
        )
    }

    fn parse_chapter_command(&mut self) -> Result<(), ParseError> {
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };

        let title = tokens_to_text(&tokens).trim().to_string();
        let number = self.state.next_chapter_number();
        self.state.current_label_anchor = Some(section_anchor_text(&number, &title));
        self.state.section_entries.push(SectionEntry {
            level: 0,
            number: number.clone(),
            title: title.clone(),
        });
        self.emit_paragraph_break_before_block();
        self.body.push_str(&number);
        if !title.is_empty() {
            self.body.push(' ');
            self.body.push_str(&title);
        }
        self.body.push_str("\n\n");
        Ok(())
    }

    fn parse_section_command(&mut self, level: u8) -> Result<(), ParseError> {
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };

        let title = tokens_to_text(&tokens).trim().to_string();
        let number = self
            .state
            .next_section_number(level, self.class_supports_chapters());
        self.state.current_label_anchor = Some(section_anchor_text(&number, &title));
        self.state.section_entries.push(SectionEntry {
            level,
            number: number.clone(),
            title: title.clone(),
        });
        self.emit_paragraph_break_before_block();
        self.body.push_str(&number);
        if !title.is_empty() {
            self.body.push(' ');
            self.body.push_str(&title);
        }
        self.body.push_str("\n\n");
        Ok(())
    }

    fn parse_color_command(&mut self) -> Result<(), ParseError> {
        let _ = self.read_required_braced_tokens()?;
        Ok(())
    }

    fn parse_textcolor_command(&mut self) -> Result<(), ParseError> {
        let _ = self.read_required_braced_tokens()?;
        let Some(text_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        self.body
            .push_str(&encode_body_markers_in_text(&tokens_to_text(&text_tokens)));
        Ok(())
    }

    fn parse_definecolor_command(&mut self) -> Result<(), ParseError> {
        let _ = self.read_required_braced_tokens()?;
        let _ = self.read_required_braced_tokens()?;
        let _ = self.read_required_braced_tokens()?;
        Ok(())
    }

    fn parse_geometry_command(&mut self) -> Result<(), ParseError> {
        let _ = self.read_required_braced_tokens()?;
        Ok(())
    }

    fn parse_bibliography_command(&mut self) -> Result<(), ParseError> {
        let _ = self.read_required_braced_tokens()?;
        self.emit_bibliography_list();
        Ok(())
    }

    fn parse_printbibliography_command(&mut self) -> Result<(), ParseError> {
        let _ = self.read_optional_bracket_tokens()?;
        self.emit_bibliography_list();
        Ok(())
    }

    fn emit_bibliography_list(&mut self) {
        let Some(entries) = self
            .state
            .bibliography_state
            .bbl
            .as_ref()
            .map(|snapshot| snapshot.entries.clone())
        else {
            return;
        };
        if entries.is_empty() {
            return;
        }

        self.emit_paragraph_break_before_block();
        for entry in &entries {
            self.body.push_str(&entry.rendered_block);
            self.body.push('\n');
        }
        self.body.push('\n');
    }

    fn parse_cite_command(&mut self) -> Result<(), ParseError> {
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };

        let rendered = tokens_to_text(&tokens)
            .split(',')
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(|key| {
                self.state.citation_number(key).map_or_else(
                    || {
                        self.state.has_unresolved_refs = true;
                        "?".to_string()
                    },
                    |number| internal_link_marker(&bibliography_destination_name(key), &number),
                )
            })
            .collect::<Vec<_>>();
        if rendered.is_empty() {
            return Ok(());
        }

        self.body.push('[');
        self.body.push_str(&rendered.join(", "));
        self.body.push(']');
        Ok(())
    }

    fn parse_hyperref_command(&mut self) -> Result<(), ParseError> {
        let target_tokens = self.read_optional_bracket_tokens()?;
        let Some(display_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };

        let display_text = encode_body_markers_in_text(&tokens_to_text(&display_tokens));
        let Some(target_tokens) = target_tokens else {
            self.body.push_str(&display_text);
            return Ok(());
        };

        let target = tokens_to_text(&target_tokens).trim().to_string();
        if target.is_empty() {
            self.body.push_str(&display_text);
            return Ok(());
        }

        self.emit_internal_link(&target, &display_text);
        Ok(())
    }

    fn parse_href_command(&mut self) -> Result<(), ParseError> {
        let Some(url_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let Some(display_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };

        let url = tokens_to_text(&url_tokens).trim().to_string();
        let display_text = encode_body_markers_in_text(&tokens_to_text(&display_tokens));
        if url.is_empty() {
            self.body.push_str(&display_text);
            return Ok(());
        }

        self.body.push(BODY_HREF_START);
        self.body.push_str(&url);
        self.body.push(BODY_HREF_URL_END);
        self.body.push_str(&display_text);
        self.body.push(BODY_HREF_END);
        Ok(())
    }

    fn parse_url_command(&mut self) -> Result<(), ParseError> {
        let Some(url_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };

        let url = tokens_to_text(&url_tokens).trim().to_string();
        if url.is_empty() {
            return Ok(());
        }

        self.body.push(BODY_URL_START);
        self.body.push_str(&url);
        self.body.push(BODY_URL_END);
        Ok(())
    }

    fn parse_hypersetup_command(&mut self) -> Result<(), ParseError> {
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };

        for (key, value) in parse_hypersetup_options(&tokens_to_text(&tokens)) {
            match key.as_str() {
                "pdftitle" => {
                    self.pdf_title = (!value.is_empty()).then_some(value);
                }
                "pdfauthor" => {
                    self.pdf_author = (!value.is_empty()).then_some(value);
                }
                "colorlinks" => {
                    if let Some(color_links) = parse_hypersetup_bool(&value) {
                        self.color_links = Some(color_links);
                    }
                }
                "linkcolor" => {
                    self.link_color = (!value.is_empty()).then_some(value);
                }
                _ => {}
            }
        }

        Ok(())
    }

    fn parse_includegraphics_command(&mut self) -> Result<(), ParseError> {
        let option_tokens = self.read_optional_bracket_tokens()?;
        let Some(path_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };

        let path = tokens_to_text(&path_tokens).trim().to_string();
        if path.is_empty() {
            return Ok(());
        }

        let options = option_tokens
            .as_deref()
            .map(parse_includegraphics_options)
            .unwrap_or_default();

        self.body.push(BODY_INCLUDEGRAPHICS_START);
        self.body
            .push_str(&serialize_includegraphics_marker(&path, &options));
        self.body.push(BODY_INCLUDEGRAPHICS_END);
        Ok(())
    }

    fn parse_caption_command(&mut self) -> Result<(), ParseError> {
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let caption = tokens_to_text(&tokens).trim().to_string();

        if let Some(float_state) = self.current_float_environment_mut() {
            float_state.caption = (!caption.is_empty()).then_some(caption);
        }
        Ok(())
    }

    fn parse_thebibliography_environment(&mut self) -> Result<(), ParseError> {
        let _ = self.read_required_braced_tokens()?;
        let mut entries = Vec::new();
        let mut current_key = None;
        let mut current_tokens = Vec::new();

        while let Some(token) = self.next_raw_token() {
            match control_sequence_name(&token).as_deref() {
                Some("bibitem") => {
                    if let Some(key) = current_key.take() {
                        self.finalize_bibliography_entry(key, &mut current_tokens, &mut entries);
                    }
                    let _ = self.read_optional_bracket_tokens()?;
                    current_key = self
                        .read_required_braced_tokens()?
                        .map(|tokens| tokens_to_text(&tokens).trim().to_string())
                        .filter(|key| !key.is_empty());
                }
                Some("end") => match self.read_environment_name()?.as_deref() {
                    Some("thebibliography") => {
                        if let Some(key) = current_key.take() {
                            self.finalize_bibliography_entry(
                                key,
                                &mut current_tokens,
                                &mut entries,
                            );
                        }
                        break;
                    }
                    Some(other) => {
                        current_tokens
                            .extend(tokens_from_text(&format!(r"\end{{{other}}}"), token.line));
                    }
                    None => current_tokens.push(token),
                },
                _ => current_tokens.push(token),
            }
        }

        if entries.is_empty() {
            return Ok(());
        }

        self.emit_paragraph_break_before_block();
        self.body.push_str(&entries.join("\n\n"));
        if !self.body.ends_with("\n\n") {
            self.body.push_str("\n\n");
        }
        Ok(())
    }

    fn parse_math_environment(&mut self, name: &str, line: u32) -> Result<(), ParseError> {
        let content = self.collect_environment_body(name, line)?;
        let numbered = !name.ends_with('*');
        let aligned = name.starts_with("align");
        let lines = self.parse_math_environment_lines(&content, numbered, aligned);

        self.body.push(BODY_EQUATION_ENV_START);
        self.body
            .push_str(&serialize_equation_env(numbered, aligned, &lines));
        self.body.push(BODY_EQUATION_ENV_END);
        Ok(())
    }

    fn collect_environment_body(&mut self, name: &str, line: u32) -> Result<String, ParseError> {
        // NOTE: Tokens are collected raw (without macro expansion) for simplicity.
        // User-defined macros (\newcommand etc.) are not expanded inside math environments.
        // Future waves may introduce expanded token collection here.
        let mut content = String::new();

        while let Some(token) = self.next_raw_token() {
            match control_sequence_name(&token).as_deref() {
                Some("end") => match self.read_environment_name()?.as_deref() {
                    Some(end_name) if end_name == name => return Ok(content),
                    Some("document") => {
                        self.push_front_plain_tokens(end_environment_tokens(
                            "document", token.line,
                        ));
                        return Err(ParseError::UnclosedEnvironment {
                            line,
                            name: name.to_string(),
                        });
                    }
                    Some(other) => content.push_str(&format!(r"\end{{{other}}}")),
                    None => content.push_str(&render_token(&token)),
                },
                _ => content.push_str(&render_token(&token)),
            }
        }

        Err(ParseError::UnclosedEnvironment {
            line,
            name: name.to_string(),
        })
    }

    fn parse_math_environment_lines(
        &mut self,
        content: &str,
        numbered: bool,
        aligned: bool,
    ) -> Vec<MathLine> {
        split_math_environment_lines(content)
            .into_iter()
            .map(|line| self.parse_math_environment_line(&line, numbered, aligned))
            .collect()
    }

    fn parse_math_environment_line(
        &mut self,
        line: &str,
        numbered: bool,
        aligned: bool,
    ) -> MathLine {
        let (without_labels, labels) = strip_math_line_labels(line);
        let (without_tag, tag) = strip_math_line_tag(&without_labels);
        let segments = split_math_line_segments(&without_tag)
            .into_iter()
            .map(|segment| parse_math_content(segment.trim()))
            .collect::<Vec<_>>();
        let display_tag = match &tag {
            LineTag::Auto if numbered => {
                self.state.equation_counter += 1;
                Some(self.state.equation_counter.to_string())
            }
            LineTag::Custom(tag) => Some(tag.clone()),
            LineTag::Auto | LineTag::Notag => None,
        };

        if let Some(display_tag) = display_tag.as_ref() {
            let anchor_text = render_math_line_for_anchor(&segments, display_tag, aligned);
            for label in labels {
                self.state.labels.insert(label.clone(), display_tag.clone());
                self.state
                    .page_label_anchors
                    .insert(label, anchor_text.clone());
            }
        }

        MathLine {
            segments,
            tag,
            display_tag,
        }
    }

    fn finalize_bibliography_entry(
        &mut self,
        key: String,
        current_tokens: &mut Vec<Token>,
        entries: &mut Vec<String>,
    ) {
        let raw_text = tokens_to_text(current_tokens);
        current_tokens.clear();
        let display_text = encode_body_markers_in_text(raw_text.trim());
        entries.push(self.state.register_bibliography_entry(key, display_text));
    }

    fn parse_label_command(&mut self) -> Result<(), ParseError> {
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let key = tokens_to_text(&tokens).trim().to_string();
        if key.is_empty() {
            return Ok(());
        }

        if let Some(float_state) = self.current_float_environment_mut() {
            float_state.label = Some(key);
            return Ok(());
        }

        if let Some(number) = self.state.current_section_number.clone() {
            self.state.labels.insert(key.clone(), number);
        }
        if let Some(anchor_text) = self.state.current_label_anchor.clone() {
            self.state.page_label_anchors.insert(key, anchor_text);
        }
        Ok(())
    }

    fn current_float_environment_mut(&mut self) -> Option<&mut FloatEnvironmentState> {
        self.environment_stack
            .last_mut()
            .and_then(|environment| match &mut environment.kind {
                OpenEnvironmentKind::Float(state) => Some(state),
                OpenEnvironmentKind::UserDefined { .. } | OpenEnvironmentKind::List(_) => None,
            })
    }

    fn parse_ref_command(&mut self) -> Result<(), ParseError> {
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let key = tokens_to_text(&tokens).trim().to_string();
        if key.is_empty() {
            return Ok(());
        }

        if let Some(number) = self
            .state
            .labels
            .get(&key)
            .filter(|number| !number.is_empty())
            .cloned()
        {
            self.emit_internal_link(&key, &number);
        } else {
            self.body.push_str("??");
            self.state.has_unresolved_refs = true;
        }
        Ok(())
    }

    fn emit_paragraph_break_before_block(&mut self) {
        if self.body.is_empty()
            || self.body.ends_with("\n\n")
            || self.body.ends_with(BODY_PAGE_BREAK_MARKER)
            || self.body.ends_with(BODY_CLEAR_PAGE_MARKER)
        {
            return;
        }

        if self.body.ends_with('\n') {
            self.body.push('\n');
        } else {
            self.body.push_str("\n\n");
        }
    }

    fn emit_internal_link(&mut self, target: &str, display_text: &str) {
        self.body
            .push_str(&internal_link_marker(target, display_text));
    }

    fn should_skip_insignificant_space(&self, token: &Token) -> bool {
        matches!(
            token.kind,
            TokenKind::CharToken {
                cat: CatCode::Space,
                ..
            }
        ) && (self.body.is_empty()
            || self.body.ends_with("\n\n")
            || self.body.ends_with(BODY_PAGE_BREAK_MARKER)
            || self.body.ends_with(BODY_CLEAR_PAGE_MARKER))
    }

    fn parse_inline_math(&mut self) -> Result<(), ParseError> {
        let Some(next) = self.next_raw_token() else {
            self.body.push('$');
            return Ok(());
        };

        if is_inline_math_end_token(&next) {
            self.body.push('$');
            self.body.push('$');
            return Ok(());
        }

        self.push_front_token(next);
        let (content, matched) = self.collect_math_body(is_inline_math_end_token)?;
        if matched {
            self.body.push(BODY_INLINE_MATH_START);
            self.body.push_str(&content);
            self.body.push(BODY_INLINE_MATH_END);
        } else {
            self.body.push('$');
            self.body.push_str(&content);
        }
        Ok(())
    }

    fn parse_display_math(&mut self) -> Result<(), ParseError> {
        let (content, matched) = self.collect_math_body(is_display_math_end_token)?;
        if matched {
            self.body.push(BODY_DISPLAY_MATH_START);
            self.body.push_str(&content);
            self.body.push(BODY_DISPLAY_MATH_END);
        } else {
            self.body.push_str(r"\[");
            self.body.push_str(&content);
        }
        Ok(())
    }

    fn collect_math_body<F>(&mut self, is_end: F) -> Result<(String, bool), ParseError>
    where
        F: Fn(&Token) -> bool,
    {
        let mut content = String::new();

        while let Some(token) = self.next_raw_token() {
            if is_end(&token) {
                return Ok((content, true));
            }
            content.push_str(&render_token(&token));
        }

        Ok((content, false))
    }

    fn parse_register_assignment(
        &mut self,
        kind: RegisterKind,
        line: u32,
    ) -> Result<(), ParseError> {
        let index = self.parse_register_index(line)?;
        if let Some(token) = self.next_significant_token() {
            if !matches!(token.kind, TokenKind::CharToken { char: '=', .. }) {
                self.push_front_token(token);
            }
        }

        let global = self.take_global_prefix();
        match kind {
            RegisterKind::Count => {
                let value = self.parse_integer_value()?.unwrap_or(0);
                self.registers.set_count(index, value, global);
            }
            RegisterKind::Dimen => {
                let value = self.parse_dimension_value()?.unwrap_or(0);
                self.registers.set_dimen(index, value, global);
            }
            RegisterKind::Skip => {
                let value = self.parse_dimension_value()?.unwrap_or(0);
                self.registers.set_skip(index, value, global);
            }
            RegisterKind::Muskip => {
                let value = self.parse_dimension_value()?.unwrap_or(0);
                self.registers.set_muskip(index, value, global);
            }
            RegisterKind::CompatInt(_) => {
                unreachable!("compat integer registers do not use \\count-style indices")
            }
        }

        Ok(())
    }

    fn expand_the(&mut self, line: u32) -> Result<(), ParseError> {
        loop {
            let Some(token) = self.next_significant_token() else {
                return Ok(());
            };

            match control_sequence_name(&token).as_deref() {
                Some("toks") => {
                    let index = self.parse_register_index(line)?;
                    self.push_front_tokens(self.registers.get_toks(index));
                    return Ok(());
                }
                Some("pdfoutput") => {
                    let value = self.registers.get_compat_int(CompatIntRegister::PdfOutput);
                    self.push_front_tokens(tokens_from_text(&value.to_string(), line));
                    return Ok(());
                }
                Some("pdftexversion") => {
                    let value = self
                        .registers
                        .get_compat_int(CompatIntRegister::PdfTexVersion);
                    self.push_front_tokens(tokens_from_text(&value.to_string(), line));
                    return Ok(());
                }
                _ => {
                    if let Some(expansion) = self.expand_defined_control_sequence_token(&token)? {
                        self.push_front_tokens(expansion);
                        continue;
                    }

                    self.push_front_token(token);
                    break;
                }
            }
        }

        let Some(kind) = self.parse_register_target(line)? else {
            return Ok(());
        };

        let rendered = match kind {
            RegisterKind::Count => {
                let index = self.parse_register_index(line)?;
                self.registers.get_count(index).to_string()
            }
            RegisterKind::CompatInt(register) => {
                self.registers.get_compat_int(register).to_string()
            }
            RegisterKind::Dimen => {
                let index = self.parse_register_index(line)?;
                format_dimen(self.registers.get_dimen(index))
            }
            RegisterKind::Skip => {
                let index = self.parse_register_index(line)?;
                format_dimen(self.registers.get_skip(index))
            }
            RegisterKind::Muskip => {
                let index = self.parse_register_index(line)?;
                format_dimen(self.registers.get_muskip(index))
            }
        };
        self.push_front_tokens(tokens_from_text(&rendered, line));
        Ok(())
    }

    fn apply_arithmetic(
        &mut self,
        operation: ArithmeticOperation,
        line: u32,
    ) -> Result<(), ParseError> {
        let Some(kind) = self.parse_register_target(line)? else {
            return Ok(());
        };
        let index = match kind {
            RegisterKind::Count
            | RegisterKind::Dimen
            | RegisterKind::Skip
            | RegisterKind::Muskip => Some(self.parse_register_index(line)?),
            RegisterKind::CompatInt(_) => None,
        };
        let _ = self.read_keyword("by");

        let global = self.take_global_prefix();
        match kind {
            RegisterKind::Count => {
                let index = index.expect("indexed register kind");
                let current = self.registers.get_count(index);
                let operand = self.parse_integer_value()?.unwrap_or(0);
                let value = apply_integer_arithmetic(current, operand, operation, line)?;
                self.registers.set_count(index, value, global);
            }
            RegisterKind::CompatInt(register) => {
                let current = self.registers.get_compat_int(register);
                let operand = self.parse_integer_value()?.unwrap_or(0);
                let value = apply_integer_arithmetic(current, operand, operation, line)?;
                self.registers.set_compat_int(register, value, global);
            }
            RegisterKind::Dimen => {
                let index = index.expect("indexed register kind");
                let current = self.registers.get_dimen(index);
                let operand = match operation {
                    ArithmeticOperation::Advance => self.parse_dimension_value()?.unwrap_or(0),
                    ArithmeticOperation::Multiply | ArithmeticOperation::Divide => {
                        self.parse_integer_value()?.unwrap_or(0)
                    }
                };
                let value = apply_integer_arithmetic(current, operand, operation, line)?;
                self.registers.set_dimen(index, value, global);
            }
            RegisterKind::Skip => {
                let index = index.expect("indexed register kind");
                let current = self.registers.get_skip(index);
                let operand = match operation {
                    ArithmeticOperation::Advance => self.parse_dimension_value()?.unwrap_or(0),
                    ArithmeticOperation::Multiply | ArithmeticOperation::Divide => {
                        self.parse_integer_value()?.unwrap_or(0)
                    }
                };
                let value = apply_integer_arithmetic(current, operand, operation, line)?;
                self.registers.set_skip(index, value, global);
            }
            RegisterKind::Muskip => {
                let index = index.expect("indexed register kind");
                let current = self.registers.get_muskip(index);
                let operand = match operation {
                    ArithmeticOperation::Advance => self.parse_dimension_value()?.unwrap_or(0),
                    ArithmeticOperation::Multiply | ArithmeticOperation::Divide => {
                        self.parse_integer_value()?.unwrap_or(0)
                    }
                };
                let value = apply_integer_arithmetic(current, operand, operation, line)?;
                self.registers.set_muskip(index, value, global);
            }
        }

        Ok(())
    }

    fn parse_register_target(&mut self, _line: u32) -> Result<Option<RegisterKind>, ParseError> {
        loop {
            let Some(token) = self.next_significant_token() else {
                return Ok(None);
            };

            match control_sequence_name(&token).as_deref() {
                Some("count") => return Ok(Some(RegisterKind::Count)),
                Some("pdfoutput") => {
                    return Ok(Some(RegisterKind::CompatInt(CompatIntRegister::PdfOutput)));
                }
                Some("pdftexversion") => {
                    return Ok(Some(RegisterKind::CompatInt(
                        CompatIntRegister::PdfTexVersion,
                    )));
                }
                Some("dimen") => return Ok(Some(RegisterKind::Dimen)),
                Some("skip") => return Ok(Some(RegisterKind::Skip)),
                Some("muskip") => return Ok(Some(RegisterKind::Muskip)),
                _ => {
                    if let Some(expansion) = self.expand_defined_control_sequence_token(&token)? {
                        self.push_front_tokens(expansion);
                        continue;
                    }

                    self.push_front_token(token);
                    return Ok(None);
                }
            }
        }
    }

    fn parse_register_index(&mut self, line: u32) -> Result<u16, ParseError> {
        let value = self
            .parse_unsigned_integer()?
            .ok_or(ParseError::InvalidRegisterIndex { line })?;
        if value > i32::from(MAX_REGISTER_INDEX) {
            return Err(ParseError::InvalidRegisterIndex { line });
        }
        Ok(value as u16)
    }

    fn parse_unsigned_integer(&mut self) -> Result<Option<i32>, ParseError> {
        let Some(first) = self.next_significant_token() else {
            return Ok(None);
        };
        let TokenKind::CharToken { char, .. } = first.kind else {
            self.push_front_token(first);
            return Ok(None);
        };
        if !char.is_ascii_digit() {
            self.push_front_token(first);
            return Ok(None);
        }

        let mut digits = String::new();
        digits.push(char);
        while let Some(token) = self.next_raw_token() {
            match token.kind {
                TokenKind::CharToken { char, .. } if char.is_ascii_digit() => digits.push(char),
                _ => {
                    self.push_front_token(token);
                    break;
                }
            }
        }

        Ok(digits.parse::<i32>().ok())
    }

    fn parse_integer_value(&mut self) -> Result<Option<i32>, ParseError> {
        loop {
            let Some((sign, mut consumed, value_token)) = self.read_signed_value_token()? else {
                return Ok(None);
            };

            match value_token.kind {
                TokenKind::ControlWord(_) | TokenKind::ControlSymbol(_) => {
                    if let Some(value) =
                        self.parse_integer_control_sequence_value(&value_token, sign)?
                    {
                        return Ok(Some(value));
                    }
                    if let Some(expansion) =
                        self.expand_defined_control_sequence_token(&value_token)?
                    {
                        self.push_front_tokens(expansion);
                        self.push_front_plain_tokens(consumed);
                        continue;
                    }
                }
                TokenKind::CharToken { char, .. } if char.is_ascii_digit() => {
                    let mut digits = String::new();
                    digits.push(char);
                    while let Some(token) = self.next_raw_token() {
                        match token.kind {
                            TokenKind::CharToken { char, .. } if char.is_ascii_digit() => {
                                digits.push(char);
                            }
                            _ => {
                                self.push_front_token(token);
                                break;
                            }
                        }
                    }

                    return Ok(digits.parse::<i32>().ok().map(|value| sign * value));
                }
                _ => {}
            }

            consumed.push(value_token);
            self.push_front_plain_tokens(consumed);
            return Ok(None);
        }
    }

    fn parse_dimension_value(&mut self) -> Result<Option<i32>, ParseError> {
        loop {
            let Some((sign, mut consumed, value_token)) = self.read_signed_value_token()? else {
                return Ok(None);
            };

            match value_token.kind {
                TokenKind::ControlWord(_) | TokenKind::ControlSymbol(_) => {
                    if let Some(value) =
                        self.parse_dimension_control_sequence_value(&value_token, sign)?
                    {
                        return Ok(Some(match value {
                            ParsedDimensionValue::Unitless(value) => {
                                if self.read_keyword("pt") {
                                    scale_points_to_sp(value)
                                } else {
                                    value
                                }
                            }
                            ParsedDimensionValue::Scaled(value) => value,
                        }));
                    }
                    if let Some(expansion) =
                        self.expand_defined_control_sequence_token(&value_token)?
                    {
                        self.push_front_tokens(expansion);
                        self.push_front_plain_tokens(consumed);
                        continue;
                    }
                }
                TokenKind::CharToken { char, .. } if char.is_ascii_digit() => {
                    let mut digits = String::new();
                    digits.push(char);
                    while let Some(token) = self.next_raw_token() {
                        match token.kind {
                            TokenKind::CharToken { char, .. } if char.is_ascii_digit() => {
                                digits.push(char);
                            }
                            _ => {
                                self.push_front_token(token);
                                break;
                            }
                        }
                    }

                    let Some(mut value) = digits.parse::<i32>().ok() else {
                        return Ok(None);
                    };
                    if self.read_keyword("pt") {
                        value = scale_points_to_sp(value);
                    }
                    return Ok(Some(sign * value));
                }
                _ => {}
            }

            consumed.push(value_token);
            self.push_front_plain_tokens(consumed);
            return Ok(None);
        }
    }

    fn parse_integer_control_sequence_value(
        &mut self,
        token: &Token,
        sign: i32,
    ) -> Result<Option<i32>, ParseError> {
        match control_sequence_name(token).as_deref() {
            Some("count") => {
                let index = self.parse_register_index(token.line)?;
                Ok(Some(sign * self.registers.get_count(index)))
            }
            Some("dimen") => {
                let index = self.parse_register_index(token.line)?;
                Ok(Some(sign * self.registers.get_dimen(index)))
            }
            Some("pdfoutput") => Ok(Some(
                sign * self.registers.get_compat_int(CompatIntRegister::PdfOutput),
            )),
            Some("pdftexversion") => Ok(Some(
                sign * self
                    .registers
                    .get_compat_int(CompatIntRegister::PdfTexVersion),
            )),
            Some("pdfstrcmp") => Ok(Some(sign * self.parse_pdfstrcmp_value(token.line)?)),
            Some("pdffilesize") => Ok(Some(sign * self.parse_pdffilesize_value()?)),
            Some("numexpr") => Ok(Some(sign * self.parse_numexpr_value(token.line)?)),
            _ => Ok(None),
        }
    }

    fn parse_dimension_control_sequence_value(
        &mut self,
        token: &Token,
        sign: i32,
    ) -> Result<Option<ParsedDimensionValue>, ParseError> {
        match control_sequence_name(token).as_deref() {
            Some("dimen") => {
                let index = self.parse_register_index(token.line)?;
                Ok(Some(ParsedDimensionValue::Scaled(
                    sign * self.registers.get_dimen(index),
                )))
            }
            Some("count") => {
                let index = self.parse_register_index(token.line)?;
                Ok(Some(ParsedDimensionValue::Unitless(
                    sign * self.registers.get_count(index),
                )))
            }
            Some("skip") => {
                let index = self.parse_register_index(token.line)?;
                Ok(Some(ParsedDimensionValue::Scaled(
                    sign * self.registers.get_skip(index),
                )))
            }
            Some("muskip") => {
                let index = self.parse_register_index(token.line)?;
                Ok(Some(ParsedDimensionValue::Scaled(
                    sign * self.registers.get_muskip(index),
                )))
            }
            Some("dimexpr") => Ok(Some(ParsedDimensionValue::Scaled(
                sign * self.parse_dimexpr_value(token.line)?,
            ))),
            Some("numexpr") => Ok(Some(ParsedDimensionValue::Unitless(
                sign * self.parse_numexpr_value(token.line)?,
            ))),
            _ => Ok(None),
        }
    }

    fn parse_dimexpr_value(&mut self, line: u32) -> Result<i32, ParseError> {
        let value = self.parse_dimexpr_sum(line)?;
        if let Some((leading, token)) = self.next_non_space_token_with_leading() {
            if !matches!(control_sequence_name(&token).as_deref(), Some("relax")) {
                self.push_back_with_leading(leading, token);
            }
        }
        Ok(value)
    }

    fn parse_dimexpr_sum(&mut self, line: u32) -> Result<i32, ParseError> {
        let mut value = self.parse_dimexpr_product(line)?;

        loop {
            let Some((leading, token)) = self.next_non_space_token_with_leading() else {
                return Ok(value);
            };

            match token.kind {
                TokenKind::CharToken { char: '+', .. } => {
                    value = clamp_i64_to_i32(
                        i64::from(value) + i64::from(self.parse_dimexpr_product(line)?),
                    );
                }
                TokenKind::CharToken { char: '-', .. } => {
                    value = clamp_i64_to_i32(
                        i64::from(value) - i64::from(self.parse_dimexpr_product(line)?),
                    );
                }
                _ if matches!(control_sequence_name(&token).as_deref(), Some("relax")) => {
                    return Ok(value);
                }
                _ => {
                    self.push_back_with_leading(leading, token);
                    return Ok(value);
                }
            }
        }
    }

    fn parse_dimexpr_product(&mut self, line: u32) -> Result<i32, ParseError> {
        let mut value = self.parse_dimexpr_factor(line)?;

        loop {
            let Some((leading, token)) = self.next_non_space_token_with_leading() else {
                return Ok(value);
            };

            match token.kind {
                TokenKind::CharToken { char: '*', .. } => {
                    let factor = self.parse_integer_value()?.unwrap_or(0);
                    value = apply_integer_arithmetic(
                        value,
                        factor,
                        ArithmeticOperation::Multiply,
                        line,
                    )?;
                }
                TokenKind::CharToken { char: '/', .. } => {
                    let divisor = self.parse_integer_value()?.unwrap_or(0);
                    value = apply_integer_arithmetic(
                        value,
                        divisor,
                        ArithmeticOperation::Divide,
                        line,
                    )?;
                }
                _ if matches!(control_sequence_name(&token).as_deref(), Some("relax")) => {
                    return Ok(value);
                }
                _ => {
                    self.push_back_with_leading(leading, token);
                    return Ok(value);
                }
            }
        }
    }

    fn parse_dimexpr_factor(&mut self, line: u32) -> Result<i32, ParseError> {
        let Some((leading, token)) = self.next_non_space_token_with_leading() else {
            return Ok(0);
        };

        match token.kind {
            TokenKind::CharToken { char: '+', .. } => self.parse_dimexpr_factor(line),
            TokenKind::CharToken { char: '-', .. } => Ok(-self.parse_dimexpr_factor(line)?),
            TokenKind::CharToken { char: '(', .. } => {
                let value = self.parse_dimexpr_sum(line)?;
                if let Some((inner_leading, closer)) = self.next_non_space_token_with_leading() {
                    if !matches!(closer.kind, TokenKind::CharToken { char: ')', .. }) {
                        self.push_back_with_leading(inner_leading, closer);
                    }
                }
                Ok(value)
            }
            _ => {
                self.push_back_with_leading(leading, token);
                Ok(self.parse_dimension_value()?.unwrap_or(0))
            }
        }
    }

    fn parse_numexpr_value(&mut self, line: u32) -> Result<i32, ParseError> {
        let value = self.parse_numexpr_sum(line)?;
        if let Some((leading, token)) = self.next_non_space_token_with_leading() {
            if !matches!(control_sequence_name(&token).as_deref(), Some("relax")) {
                self.push_back_with_leading(leading, token);
            }
        }
        Ok(value)
    }

    fn parse_numexpr_sum(&mut self, line: u32) -> Result<i32, ParseError> {
        let mut value = self.parse_numexpr_product(line)?;

        loop {
            let Some((leading, token)) = self.next_non_space_token_with_leading() else {
                return Ok(value);
            };

            match token.kind {
                TokenKind::CharToken { char: '+', .. } => {
                    value = clamp_i64_to_i32(
                        i64::from(value) + i64::from(self.parse_numexpr_product(line)?),
                    );
                }
                TokenKind::CharToken { char: '-', .. } => {
                    value = clamp_i64_to_i32(
                        i64::from(value) - i64::from(self.parse_numexpr_product(line)?),
                    );
                }
                _ if matches!(control_sequence_name(&token).as_deref(), Some("relax")) => {
                    return Ok(value);
                }
                _ => {
                    self.push_back_with_leading(leading, token);
                    return Ok(value);
                }
            }
        }
    }

    fn parse_numexpr_product(&mut self, line: u32) -> Result<i32, ParseError> {
        let mut value = self.parse_numexpr_factor(line)?;

        loop {
            let Some((leading, token)) = self.next_non_space_token_with_leading() else {
                return Ok(value);
            };

            match token.kind {
                TokenKind::CharToken { char: '*', .. } => {
                    value = clamp_i64_to_i32(
                        i64::from(value) * i64::from(self.parse_numexpr_factor(line)?),
                    );
                }
                TokenKind::CharToken { char: '/', .. } => {
                    let divisor = self.parse_numexpr_factor(line)?;
                    value = apply_integer_arithmetic(
                        value,
                        divisor,
                        ArithmeticOperation::Divide,
                        line,
                    )?;
                }
                _ if matches!(control_sequence_name(&token).as_deref(), Some("relax")) => {
                    return Ok(value);
                }
                _ => {
                    self.push_back_with_leading(leading, token);
                    return Ok(value);
                }
            }
        }
    }

    fn parse_numexpr_factor(&mut self, line: u32) -> Result<i32, ParseError> {
        let Some((leading, token)) = self.next_non_space_token_with_leading() else {
            return Ok(0);
        };

        match token.kind {
            TokenKind::CharToken { char: '+', .. } => self.parse_numexpr_factor(line),
            TokenKind::CharToken { char: '-', .. } => Ok(-self.parse_numexpr_factor(line)?),
            TokenKind::CharToken { char: '(', .. } => {
                let value = self.parse_numexpr_sum(line)?;
                if let Some((inner_leading, closer)) = self.next_non_space_token_with_leading() {
                    if !matches!(closer.kind, TokenKind::CharToken { char: ')', .. }) {
                        self.push_back_with_leading(inner_leading, closer);
                    }
                }
                Ok(value)
            }
            TokenKind::CharToken { char, .. } if char.is_ascii_digit() => {
                let mut digits = String::new();
                digits.push(char);
                while let Some(token) = self.next_raw_token() {
                    match token.kind {
                        TokenKind::CharToken { char, .. } if char.is_ascii_digit() => {
                            digits.push(char);
                        }
                        _ => {
                            self.push_front_token(token);
                            break;
                        }
                    }
                }
                Ok(digits.parse::<i32>().unwrap_or(0))
            }
            TokenKind::ControlWord(_) | TokenKind::ControlSymbol(_) => {
                if let Some(value) = self.parse_integer_control_sequence_value(&token, 1)? {
                    Ok(value)
                } else {
                    self.push_back_with_leading(leading, token);
                    Ok(0)
                }
            }
            _ => {
                self.push_back_with_leading(leading, token);
                Ok(0)
            }
        }
    }

    fn next_non_space_token_with_leading(&mut self) -> Option<(Vec<Token>, Token)> {
        let mut leading = Vec::new();

        loop {
            let token = self.next_raw_token()?;
            match token.kind {
                TokenKind::CharToken {
                    cat: CatCode::Space,
                    ..
                } => {
                    leading.push(token);
                }
                TokenKind::ControlWord(ref name) if name == "par" => {
                    leading.push(token);
                }
                _ => return Some((leading, token)),
            }
        }
    }

    fn push_back_with_leading(&mut self, mut leading: Vec<Token>, token: Token) {
        leading.push(token);
        self.push_front_plain_tokens(leading);
    }

    fn parse_relation_token(&mut self) -> Option<char> {
        let token = self.next_significant_token()?;
        match token.kind {
            TokenKind::CharToken { char, .. } if matches!(char, '<' | '=' | '>') => Some(char),
            _ => {
                self.push_front_token(token);
                None
            }
        }
    }

    fn read_signed_value_token(&mut self) -> Result<Option<(i32, Vec<Token>, Token)>, ParseError> {
        let mut sign = 1;
        let mut consumed = Vec::new();

        loop {
            let Some(token) = self.next_significant_token() else {
                if !consumed.is_empty() {
                    self.push_front_plain_tokens(consumed);
                }
                return Ok(None);
            };

            match token.kind {
                TokenKind::CharToken { char: '+', .. } => consumed.push(token),
                TokenKind::CharToken { char: '-', .. } => {
                    sign = -sign;
                    consumed.push(token);
                }
                _ => return Ok(Some((sign, consumed, token))),
            }
        }
    }

    fn read_keyword(&mut self, keyword: &str) -> bool {
        let mut consumed = Vec::new();
        let Some(first) = self.next_significant_token() else {
            return false;
        };
        consumed.push(first);

        let mut chars = keyword.chars();
        let Some(expected) = chars.next() else {
            return true;
        };
        if token_as_char(&consumed[0]) != Some(expected) {
            self.push_front_plain_tokens(consumed);
            return false;
        }

        for expected in chars {
            let Some(token) = self.next_raw_token() else {
                self.push_front_plain_tokens(consumed);
                return false;
            };
            if token_as_char(&token) != Some(expected) {
                consumed.push(token);
                self.push_front_plain_tokens(consumed);
                return false;
            }
            consumed.push(token);
        }

        true
    }

    fn tokens_match_for_ifx(&self, left: &Token, right: &Token) -> bool {
        if tokens_equal(left, right) {
            return true;
        }

        let Some(left_name) = control_sequence_name(left) else {
            return false;
        };
        let Some(right_name) = control_sequence_name(right) else {
            return false;
        };

        match (
            self.macro_engine.lookup(&left_name),
            self.macro_engine.lookup(&right_name),
        ) {
            (Some(left_def), Some(right_def)) => {
                left_def.parameter_count == right_def.parameter_count
                    && left_def.body.len() == right_def.body.len()
                    && left_def
                        .body
                        .iter()
                        .zip(&right_def.body)
                        .all(|(left, right)| left.kind == right.kind)
            }
            _ => false,
        }
    }

    fn take_global_prefix(&mut self) -> bool {
        let global = self.global_prefix;
        self.global_prefix = false;
        global
    }

    fn take_protected_prefix(&mut self) -> bool {
        let protected = self.protected_prefix;
        self.protected_prefix = false;
        protected
    }

    fn parse_document_class_declaration(
        &mut self,
    ) -> Result<Option<(String, Vec<String>)>, ParseError> {
        let options = self
            .read_optional_bracket_tokens()?
            .map(|tokens| split_comma_separated_values(&tokens_to_text(&tokens)))
            .unwrap_or_default();
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(None);
        };
        let class_name = tokens_to_text(&tokens).trim().to_string();
        if class_name.is_empty() || !is_valid_document_class(&class_name) {
            return Ok(None);
        }
        Ok(Some((class_name, options)))
    }

    fn parse_package_directive(&mut self, line: u32) -> Result<(), ParseError> {
        let options = self
            .read_optional_bracket_tokens()?
            .map(|tokens| split_comma_separated_values(&tokens_to_text(&tokens)))
            .unwrap_or_default();
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };

        for package_name in split_comma_separated_values(&tokens_to_text(&tokens)) {
            if !self.sty_interpreter_mode {
                self.current_package_options = options.clone();
                self.option_registry.clear();
            }
            let _ = load_package(
                &package_name,
                &options,
                &mut self.package_registry,
                &mut self.macro_engine,
                self.sty_resolver,
            )
            .map_err(|_| ParseError::InvalidDocumentClass { line })?;
        }

        Ok(())
    }

    fn parse_declare_option(&mut self) -> Result<(), ParseError> {
        if self.consume_optional_star().is_some() {
            let Some(code) = self.read_required_braced_tokens()? else {
                return Ok(());
            };
            self.option_registry.declare_default(code);
            return Ok(());
        }

        let Some(name_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let Some(code) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let name = tokens_to_text(&name_tokens).trim().to_string();
        if name.is_empty() {
            return Ok(());
        }
        if name == "*" {
            self.option_registry.declare_default(code);
        } else {
            self.option_registry.declare_option(name, code);
        }
        Ok(())
    }

    fn parse_process_options(&mut self) -> Result<(), ParseError> {
        let _ = self.consume_optional_star();
        let tokens = self
            .option_registry
            .process_options(&self.current_package_options);
        self.push_front_plain_tokens(tokens);
        Ok(())
    }

    fn parse_execute_options(&mut self) -> Result<(), ParseError> {
        let Some(option_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let options = split_comma_separated_values(&tokens_to_text(&option_tokens));
        let tokens = self.option_registry.execute_options(&options);
        self.push_front_plain_tokens(tokens);
        Ok(())
    }

    fn parse_ifpackageloaded(&mut self) -> Result<(), ParseError> {
        let Some(package_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let Some(true_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let Some(false_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };

        let package_name = tokens_to_text(&package_tokens)
            .trim()
            .trim_end_matches(".sty")
            .to_string();
        let selected = if !package_name.is_empty() && self.package_registry.is_loaded(&package_name)
        {
            true_tokens
        } else {
            false_tokens
        };
        self.push_front_plain_tokens(selected);
        Ok(())
    }

    fn parse_at_namedef(&mut self, is_global: bool) -> Result<(), ParseError> {
        let Some(name_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let Some(body) = self.read_required_braced_tokens()? else {
            return Ok(());
        };

        let name = tokens_to_text(&name_tokens).trim().to_string();
        if name.is_empty() {
            return Ok(());
        }

        let definition = MacroDef {
            name: name.clone(),
            parameter_count: 0,
            body,
            protected: false,
        };
        if is_global {
            self.macro_engine.define_global(name, definition);
        } else {
            self.macro_engine.define_local(name, definition);
        }
        Ok(())
    }

    fn parse_ifundefined(&mut self) -> Result<(), ParseError> {
        let Some(name_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let Some(true_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let Some(false_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };

        let name = tokens_to_text(&name_tokens)
            .trim()
            .trim_start_matches('\\')
            .to_string();
        let selected = if name.is_empty() || self.macro_engine.lookup(&name).is_none() {
            true_tokens
        } else {
            false_tokens
        };
        self.push_front_plain_tokens(selected);
        Ok(())
    }

    fn parse_newif(&mut self, line: u32, is_global: bool) -> Result<(), ParseError> {
        let Some(name_token) = self.next_significant_token() else {
            return Ok(());
        };
        let Some(if_name) = control_sequence_name(&name_token) else {
            return Ok(());
        };
        let Some(base_name) = if_name.strip_prefix("if") else {
            return Ok(());
        };
        if base_name.is_empty() {
            return Ok(());
        }

        let conditional = MacroDef {
            name: if_name.clone(),
            parameter_count: 0,
            body: vec![control_word_token("iffalse", line)],
            protected: false,
        };
        let true_setter_name = format!("{base_name}true");
        let false_setter_name = format!("{base_name}false");
        let true_setter = MacroDef {
            name: true_setter_name.clone(),
            parameter_count: 0,
            body: boolean_setter_tokens(&if_name, true, line),
            protected: false,
        };
        let false_setter = MacroDef {
            name: false_setter_name.clone(),
            parameter_count: 0,
            body: boolean_setter_tokens(&if_name, false, line),
            protected: false,
        };

        if is_global {
            self.macro_engine.define_global(if_name, conditional);
            self.macro_engine
                .define_global(true_setter_name, true_setter);
            self.macro_engine
                .define_global(false_setter_name, false_setter);
        } else {
            self.macro_engine.define_local(if_name, conditional);
            self.macro_engine
                .define_local(true_setter_name, true_setter);
            self.macro_engine
                .define_local(false_setter_name, false_setter);
        }
        Ok(())
    }

    fn parse_def(&mut self, is_global: bool) -> Result<(), ParseError> {
        let protected = self.take_protected_prefix();
        let Some((name, parameter_count, open_line)) = self.read_macro_definition_head()? else {
            return Ok(());
        };
        let body = self.read_group_contents(open_line)?;
        self.store_macro_definition(name, parameter_count, body, is_global, protected);
        Ok(())
    }

    fn parse_let(&mut self, is_global: bool) -> Result<(), ParseError> {
        let Some(target_token) = self.next_significant_token() else {
            return Ok(());
        };
        let Some(target) = control_sequence_name(&target_token) else {
            return Ok(());
        };

        if let Some(token) = self.next_significant_token() {
            if !matches!(token.kind, TokenKind::CharToken { char: '=', .. }) {
                self.push_front_token(token);
            }
        }

        let Some(rhs_token) = self.next_significant_token() else {
            return Ok(());
        };
        match rhs_token.kind {
            TokenKind::ControlWord(_) | TokenKind::ControlSymbol(_) => {
                let source_name = control_sequence_name(&rhs_token).expect("control sequence rhs");
                let source_def = self
                    .macro_engine
                    .lookup(&source_name)
                    .cloned()
                    .or_else(|| primitive_alias_definition(&source_name, rhs_token.line));
                self.macro_engine.let_assign(target, source_def, is_global);
            }
            TokenKind::CharToken { .. } => {
                self.store_macro_definition(target, 0, vec![rhs_token], is_global, false);
            }
            _ => {}
        }

        Ok(())
    }

    fn parse_edef(&mut self, is_global: bool) -> Result<(), ParseError> {
        let protected = self.take_protected_prefix();
        let Some((name, parameter_count, open_line)) = self.read_macro_definition_head()? else {
            return Ok(());
        };
        let body = self.expand_edef_body(open_line)?;
        self.store_macro_definition(name, parameter_count, body, is_global, protected);
        Ok(())
    }

    fn parse_newcommand(&mut self) -> Result<(), ParseError> {
        let Some(name_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let Some(name) = macro_name_from_tokens(&name_tokens) else {
            return Ok(());
        };

        let parameter_count = self
            .read_optional_bracket_tokens()?
            .and_then(|tokens| tokens_to_text(&tokens).trim().parse::<usize>().ok())
            .unwrap_or(0);
        if parameter_count > 9 {
            return Ok(());
        }

        let Some(body) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        self.macro_engine.define_local(
            name.clone(),
            MacroDef {
                name,
                parameter_count,
                body,
                protected: false,
            },
        );
        Ok(())
    }

    fn parse_newenvironment(&mut self) -> Result<(), ParseError> {
        let Some(name_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let name = tokens_to_text(&name_tokens).trim().to_string();
        if name.is_empty() {
            return Ok(());
        }

        let parameter_count = self
            .read_optional_bracket_tokens()?
            .and_then(|tokens| tokens_to_text(&tokens).trim().parse::<usize>().ok())
            .unwrap_or(0);
        if parameter_count > 9 {
            return Ok(());
        }

        let Some(begin_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let Some(end_tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };

        self.macro_engine.define_environment(
            name.clone(),
            EnvironmentDef {
                name,
                begin_tokens,
                end_tokens,
                parameter_count,
            },
        );
        Ok(())
    }

    fn parse_toks_assignment(&mut self, line: u32) -> Result<(), ParseError> {
        let index = self.parse_register_index(line)?;
        if let Some(token) = self.next_significant_token() {
            if !matches!(token.kind, TokenKind::CharToken { char: '=', .. }) {
                self.push_front_token(token);
            }
        }

        let global = self.take_global_prefix();
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        self.registers.set_toks(index, tokens, global);
        Ok(())
    }

    fn parse_compat_int_primitive(
        &mut self,
        register: CompatIntRegister,
        line: u32,
    ) -> Result<(), ParseError> {
        if self.consume_optional_equals() {
            let value = self.parse_integer_value()?.unwrap_or(0);
            let global = self.take_global_prefix();
            self.registers.set_compat_int(register, value, global);
        } else {
            let _ = self.take_global_prefix();
            let value = self.registers.get_compat_int(register);
            self.push_front_tokens(tokens_from_text(&value.to_string(), line));
        }
        Ok(())
    }

    fn parse_pdfstrcmp_value(&mut self, _line: u32) -> Result<i32, ParseError> {
        let left = self
            .read_required_braced_tokens()?
            .map(|tokens| tokens_to_text(&tokens))
            .unwrap_or_default();
        let right = self
            .read_required_braced_tokens()?
            .map(|tokens| tokens_to_text(&tokens))
            .unwrap_or_default();

        Ok(match left.cmp(&right) {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        })
    }

    fn parse_pdffilesize_value(&mut self) -> Result<i32, ParseError> {
        let _ = self.read_required_braced_tokens()?;
        Ok(0)
    }

    fn consume_optional_equals(&mut self) -> bool {
        let mut buffered = Vec::new();

        while let Some(token) = self.next_raw_token() {
            match token.kind {
                TokenKind::CharToken {
                    cat: CatCode::Space,
                    ..
                } => {
                    buffered.push(token);
                }
                TokenKind::ControlWord(ref name) if name == "par" => {
                    buffered.push(token);
                }
                TokenKind::CharToken { char: '=', .. } => return true,
                _ => {
                    buffered.push(token);
                    self.push_front_plain_tokens(buffered);
                    return false;
                }
            }
        }

        self.push_front_plain_tokens(buffered);
        false
    }

    fn consume_optional_star(&mut self) -> Option<Token> {
        let token = self.next_significant_token()?;
        if matches!(token.kind, TokenKind::CharToken { char: '*', .. }) {
            Some(token)
        } else {
            self.push_front_token(token);
            None
        }
    }

    fn parse_newcount(&mut self, is_global: bool, line: u32) -> Result<(), ParseError> {
        let Some(name_token) = self.next_significant_token() else {
            return Ok(());
        };
        let Some(name) = control_sequence_name(&name_token) else {
            return Ok(());
        };

        let index = self.next_allocated_count(line)?;
        self.define_register_alias(name, "count", index, is_global, line);
        Ok(())
    }

    fn parse_newtoks(&mut self, is_global: bool, line: u32) -> Result<(), ParseError> {
        let Some(name_token) = self.next_significant_token() else {
            return Ok(());
        };
        let Some(name) = control_sequence_name(&name_token) else {
            return Ok(());
        };

        let index = self.next_allocated_toks(line)?;
        self.define_register_alias(name, "toks", index, is_global, line);
        Ok(())
    }

    fn parse_countdef(&mut self, is_global: bool, line: u32) -> Result<(), ParseError> {
        let Some(name_token) = self.next_significant_token() else {
            return Ok(());
        };
        let Some(name) = control_sequence_name(&name_token) else {
            return Ok(());
        };

        if let Some(token) = self.next_significant_token() {
            if !matches!(token.kind, TokenKind::CharToken { char: '=', .. }) {
                self.push_front_token(token);
            }
        }

        let index = self.parse_register_index(line)?;
        self.define_register_alias(name, "count", index, is_global, line);
        Ok(())
    }

    fn parse_toksdef(&mut self, is_global: bool, line: u32) -> Result<(), ParseError> {
        let Some(name_token) = self.next_significant_token() else {
            return Ok(());
        };
        let Some(name) = control_sequence_name(&name_token) else {
            return Ok(());
        };

        if let Some(token) = self.next_significant_token() {
            if !matches!(token.kind, TokenKind::CharToken { char: '=', .. }) {
                self.push_front_token(token);
            }
        }

        let index = self.parse_register_index(line)?;
        self.define_register_alias(name, "toks", index, is_global, line);
        Ok(())
    }

    fn parse_catcode(&mut self) -> Result<(), ParseError> {
        let Some(backtick) = self.next_significant_token() else {
            return Ok(());
        };
        if !matches!(backtick.kind, TokenKind::CharToken { char: '`', .. }) {
            return Ok(());
        }

        let Some(target) = self.next_raw_token() else {
            return Ok(());
        };
        let Some(char_code) = catcode_target_byte(&target) else {
            return Ok(());
        };

        let Some(equals) = self.next_significant_token() else {
            return Ok(());
        };
        if !matches!(equals.kind, TokenKind::CharToken { char: '=', .. }) {
            return Ok(());
        }

        let mut value = String::new();
        loop {
            let Some(token) = self.next_significant_token() else {
                break;
            };
            match token.kind {
                TokenKind::CharToken { char, .. } if char.is_ascii_digit() => value.push(char),
                _ => {
                    self.push_front_token(token);
                    break;
                }
            }
        }

        let Ok(number) = value.parse::<u8>() else {
            return Ok(());
        };
        let Some(catcode) = catcode_from_u8(number) else {
            return Ok(());
        };

        self.macro_engine.set_catcode(char_code, catcode);
        self.tokenizer.set_catcode(char_code, catcode);
        Ok(())
    }

    fn parse_expandafter(&mut self) -> Result<(), ParseError> {
        let Some(first) = self.next_raw_token() else {
            return Ok(());
        };
        let Some(second) = self.next_raw_token() else {
            self.push_front_token(first);
            return Ok(());
        };

        match control_sequence_name(&second).as_deref() {
            Some("csname") => {
                self.parse_csname(second.line)?;
            }
            Some("the") => {
                self.expand_the(second.line)?;
            }
            _ => {
                if let Some(expansion) = self.expand_defined_control_sequence_token(&second)? {
                    self.push_front_tokens(expansion);
                } else {
                    self.push_front_token(second);
                }
            }
        }
        self.push_front_queued_token(first, false);
        Ok(())
    }

    fn parse_csname(&mut self, line: u32) -> Result<(), ParseError> {
        let name = self.read_csname_name(line)?;

        self.push_front_queued_token(
            Token {
                kind: TokenKind::ControlWord(name),
                line,
                column: 1,
            },
            true,
        );
        Ok(())
    }

    fn read_csname_name(&mut self, line: u32) -> Result<String, ParseError> {
        let mut name = String::new();

        loop {
            let Some(token) = self.next_raw_token() else {
                return Err(ParseError::UnclosedBrace { line });
            };
            match token.kind {
                TokenKind::ControlWord(ref control_word) if control_word == "endcsname" => {
                    return Ok(name);
                }
                TokenKind::CharToken { char, .. } => name.push(char),
                TokenKind::ControlWord(ref control_word) if control_word == "the" => {
                    self.expand_the(token.line)?;
                }
                TokenKind::ControlWord(ref control_word) if control_word == "csname" => {
                    self.parse_csname(token.line)?;
                }
                TokenKind::ControlWord(ref control_word) if control_word == "expandafter" => {
                    self.parse_expandafter()?;
                }
                TokenKind::ControlWord(_) | TokenKind::ControlSymbol(_) => {
                    if let Some(expansion) = self.expand_defined_control_sequence_token(&token)? {
                        self.push_front_tokens(expansion);
                    }
                }
                _ => {}
            }
        }
    }

    fn collect_macro_arguments(
        &mut self,
        parameter_count: usize,
    ) -> Result<Vec<Vec<Token>>, ParseError> {
        let mut arguments = Vec::with_capacity(parameter_count);
        for _ in 0..parameter_count {
            let Some(token) = self.next_significant_token() else {
                arguments.push(Vec::new());
                continue;
            };
            match token.kind {
                TokenKind::CharToken {
                    cat: CatCode::BeginGroup,
                    ..
                } => arguments.push(self.read_group_contents(token.line)?),
                _ => arguments.push(vec![token]),
            }
        }
        Ok(arguments)
    }

    fn read_macro_definition_head(&mut self) -> Result<Option<(String, usize, u32)>, ParseError> {
        let Some(name_token) = self.next_significant_token() else {
            return Ok(None);
        };
        let Some(name) = control_sequence_name(&name_token) else {
            return Ok(None);
        };

        let mut parameter_count = 0usize;
        loop {
            let Some(token) = self.next_significant_token() else {
                return Ok(None);
            };
            match token.kind {
                TokenKind::Parameter(index) => {
                    parameter_count = parameter_count.max(index as usize);
                }
                TokenKind::CharToken {
                    cat: CatCode::BeginGroup,
                    ..
                } => {
                    if parameter_count > 9 {
                        return Ok(None);
                    }
                    return Ok(Some((name, parameter_count, token.line)));
                }
                _ => return Ok(None),
            }
        }
    }

    fn expand_edef_body(&mut self, open_line: u32) -> Result<Vec<Token>, ParseError> {
        let mut depth = 1usize;
        let mut body = Vec::new();

        while let Some(token) = self.next_raw_token() {
            match token.kind {
                TokenKind::CharToken {
                    cat: CatCode::BeginGroup,
                    ..
                } => {
                    depth += 1;
                    body.push(token);
                }
                TokenKind::CharToken {
                    cat: CatCode::EndGroup,
                    ..
                } => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(body);
                    }
                    body.push(token);
                }
                TokenKind::ControlWord(ref name) if name == "noexpand" => {
                    if let Some(next) = self.next_raw_token() {
                        body.push(next);
                    }
                }
                TokenKind::ControlWord(ref name) if name == "unexpanded" => {
                    if let Some(tokens) = self.read_required_braced_tokens()? {
                        body.extend(tokens);
                    }
                }
                TokenKind::ControlWord(ref name) if name == "detokenize" => {
                    if let Some(tokens) = self.read_required_braced_tokens()? {
                        body.extend(detokenized_tokens(&tokens, token.line));
                    }
                }
                TokenKind::ControlWord(ref name) if name == "pdfstrcmp" => {
                    let value = self.parse_pdfstrcmp_value(token.line)?;
                    body.extend(tokens_from_text(&value.to_string(), token.line));
                }
                TokenKind::ControlWord(ref name) if name == "pdfoutput" => {
                    let value = self.registers.get_compat_int(CompatIntRegister::PdfOutput);
                    body.extend(tokens_from_text(&value.to_string(), token.line));
                }
                TokenKind::ControlWord(ref name) if name == "pdftexversion" => {
                    let value = self
                        .registers
                        .get_compat_int(CompatIntRegister::PdfTexVersion);
                    body.extend(tokens_from_text(&value.to_string(), token.line));
                }
                TokenKind::ControlWord(ref name) if name == "pdffilesize" => {
                    let value = self.parse_pdffilesize_value()?;
                    body.extend(tokens_from_text(&value.to_string(), token.line));
                }
                TokenKind::ControlWord(ref name) if name == "pdftexbanner" => {
                    body.extend(tokens_from_text("This is ferritex", token.line));
                }
                TokenKind::ControlWord(ref name) if name == "numexpr" => {
                    let value = self.parse_numexpr_value(token.line)?;
                    body.extend(tokens_from_text(&value.to_string(), token.line));
                }
                TokenKind::ControlWord(ref name) if name == "dimexpr" => {
                    let value = self.parse_dimexpr_value(token.line)?;
                    body.extend(tokens_from_text(&format_dimen(value), token.line));
                }
                TokenKind::ControlWord(ref name) if name == "the" => {
                    self.expand_the(token.line)?;
                }
                TokenKind::ControlWord(ref name) if name == "csname" => {
                    self.parse_csname(token.line)?;
                }
                TokenKind::ControlWord(_) | TokenKind::ControlSymbol(_) => {
                    if let Some(expansion) = self.expand_defined_control_sequence_token(&token)? {
                        self.push_front_tokens(expansion);
                    } else {
                        body.push(token);
                    }
                }
                _ => body.push(token),
            }
        }

        Err(ParseError::UnclosedBrace { line: open_line })
    }

    fn read_environment_name(&mut self) -> Result<Option<String>, ParseError> {
        Ok(self
            .read_required_braced_tokens()?
            .map(|tokens| tokens_to_text(&tokens).trim().to_string())
            .filter(|name| !name.is_empty()))
    }

    fn read_raw_environment_body(&mut self, name: &str, line: u32) -> Result<String, ParseError> {
        let mut depth = 1usize;
        let mut body_tokens = Vec::new();

        while let Some(token) = self.next_raw_token() {
            match &token.kind {
                TokenKind::ControlWord(command) if command == "begin" => {
                    let Some(environment_name) = self.read_environment_name()? else {
                        body_tokens.push(token);
                        continue;
                    };
                    if environment_name == name {
                        depth += 1;
                    }
                    body_tokens.extend(begin_environment_tokens(&environment_name, token.line));
                }
                TokenKind::ControlWord(command) if command == "end" => {
                    let Some(environment_name) = self.read_environment_name()? else {
                        body_tokens.push(token);
                        continue;
                    };
                    if environment_name == name {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(tokens_to_text(&body_tokens));
                        }
                    }
                    body_tokens.extend(end_environment_tokens(&environment_name, token.line));
                }
                _ => body_tokens.push(token),
            }
        }

        Err(ParseError::UnclosedEnvironment {
            line,
            name: name.to_string(),
        })
    }

    fn read_required_braced_tokens(&mut self) -> Result<Option<Vec<Token>>, ParseError> {
        let Some(token) = self.next_significant_token() else {
            return Ok(None);
        };
        match token.kind {
            TokenKind::CharToken {
                cat: CatCode::BeginGroup,
                ..
            } => Ok(Some(self.read_group_contents(token.line)?)),
            TokenKind::CharToken {
                cat: CatCode::EndGroup,
                ..
            } => Err(ParseError::UnexpectedClosingBrace { line: token.line }),
            _ => Ok(None),
        }
    }

    fn read_group_contents(&mut self, open_line: u32) -> Result<Vec<Token>, ParseError> {
        let mut depth = 1usize;
        let mut contents = Vec::new();

        while let Some(token) = self.next_raw_token() {
            match token.kind {
                TokenKind::CharToken {
                    cat: CatCode::BeginGroup,
                    ..
                } => {
                    depth += 1;
                    contents.push(token);
                }
                TokenKind::CharToken {
                    cat: CatCode::EndGroup,
                    ..
                } => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(contents);
                    }
                    contents.push(token);
                }
                _ => contents.push(token),
            }
        }

        Err(ParseError::UnclosedBrace { line: open_line })
    }

    fn store_macro_definition(
        &mut self,
        name: String,
        parameter_count: usize,
        body: Vec<Token>,
        is_global: bool,
        protected: bool,
    ) {
        let definition = MacroDef {
            name: name.clone(),
            parameter_count,
            body,
            protected,
        };
        if is_global {
            self.macro_engine.define_global(name, definition);
        } else {
            self.macro_engine.define_local(name, definition);
        }
    }

    fn define_register_alias(
        &mut self,
        name: String,
        primitive: &str,
        index: u16,
        is_global: bool,
        line: u32,
    ) {
        self.store_macro_definition(
            name,
            0,
            register_alias_tokens(primitive, index, line),
            is_global,
            false,
        );
    }

    fn next_allocated_count(&mut self, line: u32) -> Result<u16, ParseError> {
        if self.alloc_count > u32::from(MAX_REGISTER_INDEX) {
            return Err(ParseError::InvalidRegisterIndex { line });
        }

        let index = self.alloc_count as u16;
        self.alloc_count += 1;
        Ok(index)
    }

    fn next_allocated_toks(&mut self, line: u32) -> Result<u16, ParseError> {
        if self.alloc_toks > u32::from(MAX_REGISTER_INDEX) {
            return Err(ParseError::InvalidRegisterIndex { line });
        }

        let index = self.alloc_toks as u16;
        self.alloc_toks += 1;
        Ok(index)
    }

    fn expand_defined_control_sequence_token(
        &mut self,
        token: &Token,
    ) -> Result<Option<Vec<Token>>, ParseError> {
        let Some(name) = control_sequence_name(token) else {
            return Ok(None);
        };
        let Some(definition) = self.macro_engine.lookup(&name).cloned() else {
            return Ok(None);
        };

        self.record_macro_expansion(token.line)?;
        let args = self.collect_macro_arguments(definition.parameter_count)?;
        Ok(Some(self.macro_engine.expand(&name, &args)))
    }

    fn read_optional_bracket_tokens(&mut self) -> Result<Option<Vec<Token>>, ParseError> {
        let Some(token) = self.next_significant_token() else {
            return Ok(None);
        };
        if !matches!(token.kind, TokenKind::CharToken { char: '[', .. }) {
            self.push_front_token(token);
            return Ok(None);
        }

        let mut contents = Vec::new();
        while let Some(next) = self.next_raw_token() {
            match next.kind {
                TokenKind::CharToken { char: ']', .. } => return Ok(Some(contents)),
                _ => contents.push(next),
            }
        }

        Ok(None)
    }

    fn validate_trailing_content(&mut self) -> Result<(), ParseError> {
        let mut first_non_whitespace_line = None;
        let mut group_stack = Vec::new();

        while let Some(token) = self.next_raw_token() {
            match token.kind {
                TokenKind::CharToken {
                    cat: CatCode::BeginGroup,
                    ..
                } => {
                    group_stack.push(token.line);
                    first_non_whitespace_line.get_or_insert(token.line);
                }
                TokenKind::CharToken {
                    cat: CatCode::EndGroup,
                    ..
                } => {
                    if group_stack.pop().is_none() {
                        return Err(ParseError::UnexpectedClosingBrace { line: token.line });
                    }
                    first_non_whitespace_line.get_or_insert(token.line);
                }
                TokenKind::CharToken {
                    cat: CatCode::Space,
                    ..
                } => {}
                TokenKind::ControlWord(ref name) if name == "par" => {}
                _ => {
                    first_non_whitespace_line.get_or_insert(token.line);
                }
            }
        }

        if let Some(line) = group_stack.last().copied() {
            return Err(ParseError::UnclosedBrace { line });
        }

        if let Some(line) = first_non_whitespace_line {
            return Err(ParseError::TrailingContentAfterEndDocument { line });
        }

        Ok(())
    }

    fn skip_current_false_branch(&mut self) {
        let outcome = {
            let conditionals = &mut self.conditionals;
            let pending_tokens = &mut self.pending_tokens;
            let tokenizer = &mut self.tokenizer;

            conditionals.skip_false_branch(|| {
                if let Some(queued) = pending_tokens.pop_front() {
                    return Some(queued.token);
                }

                tokenizer.next().map(|result| {
                    result.expect("tokenizing a UTF-8 string should not produce diagnostics")
                })
            })
        };

        if matches!(outcome, SkipOutcome::EndOfInput) {
            self.current_token_from_expansion = false;
            self.consecutive_macro_expansions = 0;
        }
    }

    fn next_raw_token(&mut self) -> Option<Token> {
        if let Some(queued) = self.pending_tokens.pop_front() {
            self.current_token_from_expansion = queued.from_expansion;
            return Some(queued.token);
        }

        self.current_token_from_expansion = false;
        self.consecutive_macro_expansions = 0;
        self.tokenizer
            .next()
            .map(|result| result.expect("tokenizing a UTF-8 string should not produce diagnostics"))
    }

    fn next_significant_token(&mut self) -> Option<Token> {
        loop {
            let token = self.next_raw_token()?;
            match token.kind {
                TokenKind::CharToken {
                    cat: CatCode::Space,
                    ..
                } => continue,
                TokenKind::ControlWord(ref name) if name == "par" => continue,
                _ => return Some(token),
            }
        }
    }

    fn push_front_token(&mut self, token: Token) {
        self.push_front_queued_token(token, false);
    }

    fn push_front_tokens(&mut self, tokens: Vec<Token>) {
        for token in tokens.into_iter().rev() {
            self.push_front_queued_token(token, true);
        }
    }

    fn push_front_plain_tokens(&mut self, tokens: Vec<Token>) {
        for token in tokens.into_iter().rev() {
            self.push_front_queued_token(token, false);
        }
    }

    fn push_front_queued_token(&mut self, token: Token, from_expansion: bool) {
        self.pending_tokens.push_front(QueuedToken {
            token,
            from_expansion,
        });
    }

    fn sync_tokenizer_catcodes(&mut self) {
        self.tokenizer.reset_catcodes();
        for (char_code, catcode) in self.macro_engine.get_catcode_overrides() {
            self.tokenizer.set_catcode(char_code, catcode);
        }
    }

    fn record_macro_expansion(&mut self, line: u32) -> Result<(), ParseError> {
        if self.current_token_from_expansion {
            self.consecutive_macro_expansions += 1;
        } else {
            self.consecutive_macro_expansions = 1;
        }

        if self.consecutive_macro_expansions > MAX_CONSECUTIVE_MACRO_EXPANSIONS {
            return Err(ParseError::MacroExpansionLimit { line });
        }

        Ok(())
    }
}

fn control_sequence_name(token: &Token) -> Option<String> {
    match &token.kind {
        TokenKind::ControlWord(name) => Some(name.clone()),
        TokenKind::ControlSymbol(symbol) => Some(symbol.to_string()),
        _ => None,
    }
}

fn control_word_token(name: &str, line: u32) -> Token {
    Token {
        kind: TokenKind::ControlWord(name.to_string()),
        line,
        column: 1,
    }
}

fn boolean_setter_tokens(name: &str, value: bool, line: u32) -> Vec<Token> {
    vec![
        control_word_token("global", line),
        control_word_token("let", line),
        control_word_token(name, line),
        control_word_token(if value { "iftrue" } else { "iffalse" }, line),
    ]
}

fn primitive_alias_definition(name: &str, line: u32) -> Option<MacroDef> {
    match name {
        "iftrue" | "iffalse" => Some(MacroDef {
            name: name.to_string(),
            parameter_count: 0,
            body: vec![control_word_token(name, line)],
            protected: false,
        }),
        _ => None,
    }
}

fn macro_name_from_tokens(tokens: &[Token]) -> Option<String> {
    let filtered = tokens
        .iter()
        .filter(|token| {
            !matches!(
                token.kind,
                TokenKind::CharToken {
                    cat: CatCode::Space,
                    ..
                }
            )
        })
        .collect::<Vec<_>>();
    if filtered.len() != 1 {
        return None;
    }
    control_sequence_name(filtered[0])
}

fn list_environment_kind(name: &str) -> Option<ListEnvironmentKind> {
    match name {
        "itemize" => Some(ListEnvironmentKind::Itemize),
        "enumerate" => Some(ListEnvironmentKind::Enumerate),
        "description" => Some(ListEnvironmentKind::Description),
        _ => None,
    }
}

fn is_math_environment(name: &str) -> bool {
    matches!(
        name,
        "align"
            | "align*"
            | "equation"
            | "equation*"
            | "gather"
            | "gather*"
            | "multline"
            | "multline*"
    )
}

fn expand_parameter_tokens(tokens: &[Token], args: &[Vec<Token>]) -> Vec<Token> {
    let mut expanded = Vec::with_capacity(tokens.len());
    for token in tokens {
        match token.kind {
            TokenKind::Parameter(index) => {
                let argument_index = usize::from(index.saturating_sub(1));
                if let Some(argument) = args.get(argument_index) {
                    expanded.extend(argument.clone());
                }
            }
            _ => expanded.push(token.clone()),
        }
    }
    expanded
}

fn tokens_to_text(tokens: &[Token]) -> String {
    let mut text = String::new();
    for token in tokens {
        text.push_str(&render_token(token));
    }
    text
}

fn render_token(token: &Token) -> String {
    match &token.kind {
        TokenKind::ControlWord(name) => format!(r"\{name}"),
        TokenKind::ControlSymbol(symbol) => format!(r"\{symbol}"),
        TokenKind::CharToken { char, .. } => char.to_string(),
        TokenKind::Parameter(index) => format!("#{index}"),
    }
}

fn apply_integer_arithmetic(
    current: i32,
    operand: i32,
    operation: ArithmeticOperation,
    line: u32,
) -> Result<i32, ParseError> {
    match operation {
        ArithmeticOperation::Advance => {
            Ok(clamp_i64_to_i32(i64::from(current) + i64::from(operand)))
        }
        ArithmeticOperation::Multiply => {
            Ok(clamp_i64_to_i32(i64::from(current) * i64::from(operand)))
        }
        ArithmeticOperation::Divide => {
            if operand == 0 {
                return Err(ParseError::DivisionByZero { line });
            }
            Ok(current / operand)
        }
    }
}

fn clamp_i64_to_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn scale_points_to_sp(points: i32) -> i32 {
    clamp_i64_to_i32(i64::from(points) * 65_536)
}

fn format_dimen(value: i32) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let absolute = i64::from(value).abs();
    let mut whole = absolute / 65_536;
    let remainder = absolute % 65_536;
    if remainder == 0 {
        return format!("{sign}{whole}.0pt");
    }

    let mut fractional = ((remainder * 100_000) + 32_768) / 65_536;
    if fractional == 100_000 {
        whole += 1;
        fractional = 0;
    }

    let mut fraction = format!("{fractional:05}");
    while fraction.len() > 1 && fraction.ends_with('0') {
        let _ = fraction.pop();
    }
    format!("{sign}{whole}.{fraction}pt")
}

fn tokens_from_text(text: &str, line: u32) -> Vec<Token> {
    text.chars()
        .enumerate()
        .map(|(offset, char)| Token {
            kind: TokenKind::CharToken {
                char,
                cat: catcode_for_expanded_char(char),
            },
            line,
            column: (offset + 1) as u32,
        })
        .collect()
}

fn detokenized_tokens(tokens: &[Token], line: u32) -> Vec<Token> {
    tokens_to_text(tokens)
        .chars()
        .enumerate()
        .map(|(offset, char)| Token {
            kind: TokenKind::CharToken {
                char,
                cat: CatCode::Other,
            },
            line,
            column: (offset + 1) as u32,
        })
        .collect()
}

fn end_environment_tokens(name: &str, line: u32) -> Vec<Token> {
    let mut tokens = Vec::with_capacity(3 + name.chars().count());
    tokens.push(Token {
        kind: TokenKind::ControlWord("end".to_string()),
        line,
        column: 1,
    });
    tokens.push(Token {
        kind: TokenKind::CharToken {
            char: '{',
            cat: CatCode::BeginGroup,
        },
        line,
        column: 2,
    });
    for (offset, ch) in name.chars().enumerate() {
        tokens.push(Token {
            kind: TokenKind::CharToken {
                char: ch,
                cat: catcode_for_expanded_char(ch),
            },
            line,
            column: (offset + 3) as u32,
        });
    }
    tokens.push(Token {
        kind: TokenKind::CharToken {
            char: '}',
            cat: CatCode::EndGroup,
        },
        line,
        column: (name.chars().count() + 3) as u32,
    });
    tokens
}

fn begin_environment_tokens(name: &str, line: u32) -> Vec<Token> {
    let mut tokens = Vec::with_capacity(3 + name.chars().count());
    tokens.push(Token {
        kind: TokenKind::ControlWord("begin".to_string()),
        line,
        column: 1,
    });
    tokens.push(Token {
        kind: TokenKind::CharToken {
            char: '{',
            cat: CatCode::BeginGroup,
        },
        line,
        column: 2,
    });
    for (offset, ch) in name.chars().enumerate() {
        tokens.push(Token {
            kind: TokenKind::CharToken {
                char: ch,
                cat: catcode_for_expanded_char(ch),
            },
            line,
            column: (offset + 3) as u32,
        });
    }
    tokens.push(Token {
        kind: TokenKind::CharToken {
            char: '}',
            cat: CatCode::EndGroup,
        },
        line,
        column: (name.chars().count() + 3) as u32,
    });
    tokens
}

fn register_alias_tokens(primitive: &str, index: u16, line: u32) -> Vec<Token> {
    let mut tokens = Vec::with_capacity(1 + index.to_string().len());
    tokens.push(Token {
        kind: TokenKind::ControlWord(primitive.to_string()),
        line,
        column: 1,
    });
    for (offset, char) in index.to_string().chars().enumerate() {
        tokens.push(Token {
            kind: TokenKind::CharToken {
                char,
                cat: CatCode::Other,
            },
            line,
            column: (offset + 2) as u32,
        });
    }
    tokens
}

fn catcode_for_expanded_char(char: char) -> CatCode {
    if char == ' ' {
        CatCode::Space
    } else if char.is_ascii_alphabetic() {
        CatCode::Letter
    } else {
        CatCode::Other
    }
}

fn token_as_char(token: &Token) -> Option<char> {
    match token.kind {
        TokenKind::CharToken { char, .. } => Some(char),
        _ => None,
    }
}

fn char_code_of(token: &Token) -> Option<u32> {
    match &token.kind {
        TokenKind::CharToken { char, .. } => Some(*char as u32),
        _ => None,
    }
}

fn cat_code_of(token: &Token) -> Option<CatCode> {
    match &token.kind {
        TokenKind::CharToken { cat, .. } => Some(*cat),
        TokenKind::ControlWord(_) | TokenKind::ControlSymbol(_) => None,
        _ => None,
    }
}

fn is_inline_math_end_token(token: &Token) -> bool {
    matches!(
        token.kind,
        TokenKind::CharToken {
            char: '$',
            cat: CatCode::MathShift,
        }
    )
}

fn is_display_math_end_token(token: &Token) -> bool {
    matches!(token.kind, TokenKind::ControlSymbol(']'))
}

fn catcode_target_byte(token: &Token) -> Option<u8> {
    match token.kind {
        TokenKind::CharToken { char, .. } => u8::try_from(char).ok(),
        TokenKind::ControlSymbol(symbol) => u8::try_from(symbol).ok(),
        TokenKind::ControlWord(ref name) if name.chars().count() == 1 => {
            u8::try_from(name.chars().next().expect("single-char name")).ok()
        }
        _ => None,
    }
}

fn catcode_from_u8(value: u8) -> Option<CatCode> {
    match value {
        0 => Some(CatCode::Escape),
        1 => Some(CatCode::BeginGroup),
        2 => Some(CatCode::EndGroup),
        3 => Some(CatCode::MathShift),
        4 => Some(CatCode::AlignmentTab),
        5 => Some(CatCode::EndOfLine),
        6 => Some(CatCode::Parameter),
        7 => Some(CatCode::Superscript),
        8 => Some(CatCode::Subscript),
        9 => Some(CatCode::Ignored),
        10 => Some(CatCode::Space),
        11 => Some(CatCode::Letter),
        12 => Some(CatCode::Other),
        13 => Some(CatCode::Active),
        14 => Some(CatCode::Comment),
        15 => Some(CatCode::Invalid),
        _ => None,
    }
}

fn is_valid_document_class(name: &str) -> bool {
    !name.chars().any(|ch| ch.is_control() || ch.is_whitespace())
}

fn preserves_protected_prefix(name: &str) -> bool {
    matches!(name, "protected" | "global" | "def" | "edef" | "gdef")
}

fn split_comma_separated_values(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn normalize_body_par_breaks(body: &str) -> String {
    let normalized = body.replace("\r\n", "\n").replace('\r', "\n");
    let mut output = String::with_capacity(normalized.len());
    let mut chars = normalized.char_indices().peekable();

    while let Some((index, ch)) = chars.next() {
        if ch == '\\' && normalized[index..].starts_with(r"\par") {
            let next_char = normalized[index + 4..].chars().next();
            if !matches!(next_char, Some(next) if next.is_ascii_alphabetic()) {
                output.push('\n');
                output.push('\n');
                let _ = chars.next();
                let _ = chars.next();
                let _ = chars.next();
                continue;
            }
        }

        output.push(ch);
    }

    output
}

fn body_contains_pageref_markers(body: &str) -> bool {
    body.contains(BODY_PAGEREF_START)
}

fn section_anchor_text(number: &str, title: &str) -> String {
    match (number.is_empty(), title.is_empty()) {
        (true, true) => String::new(),
        (false, true) => number.to_string(),
        (true, false) => title.to_string(),
        (false, false) => format!("{number} {title}"),
    }
}

fn math_symbol_lookup(name: &str) -> Option<&'static str> {
    match name {
        "alpha" => Some("α"),
        "beta" => Some("β"),
        "gamma" => Some("γ"),
        "delta" => Some("δ"),
        "epsilon" => Some("ε"),
        "zeta" => Some("ζ"),
        "eta" => Some("η"),
        "theta" => Some("θ"),
        "iota" => Some("ι"),
        "kappa" => Some("κ"),
        "lambda" => Some("λ"),
        "mu" => Some("μ"),
        "nu" => Some("ν"),
        "xi" => Some("ξ"),
        "pi" => Some("π"),
        "rho" => Some("ρ"),
        "sigma" => Some("σ"),
        "tau" => Some("τ"),
        "upsilon" => Some("υ"),
        "phi" => Some("φ"),
        "chi" => Some("χ"),
        "psi" => Some("ψ"),
        "omega" => Some("ω"),
        "varepsilon" => Some("ϵ"),
        "vartheta" => Some("ϑ"),
        "varphi" => Some("ϕ"),
        "varrho" => Some("ϱ"),
        "varsigma" => Some("ς"),
        "varpi" => Some("ϖ"),
        "Gamma" => Some("Γ"),
        "Delta" => Some("Δ"),
        "Theta" => Some("Θ"),
        "Lambda" => Some("Λ"),
        "Xi" => Some("Ξ"),
        "Pi" => Some("Π"),
        "Sigma" => Some("Σ"),
        "Upsilon" => Some("Υ"),
        "Phi" => Some("Φ"),
        "Psi" => Some("Ψ"),
        "Omega" => Some("Ω"),
        "pm" => Some("±"),
        "mp" => Some("∓"),
        "times" => Some("×"),
        "div" => Some("÷"),
        "cdot" => Some("·"),
        "star" => Some("⋆"),
        "circ" => Some("∘"),
        "bullet" => Some("•"),
        "cap" => Some("∩"),
        "cup" => Some("∪"),
        "vee" => Some("∨"),
        "wedge" => Some("∧"),
        "oplus" => Some("⊕"),
        "otimes" => Some("⊗"),
        "odot" => Some("⊙"),
        "leq" | "le" => Some("≤"),
        "geq" | "ge" => Some("≥"),
        "neq" | "ne" => Some("≠"),
        "equiv" => Some("≡"),
        "sim" => Some("∼"),
        "simeq" => Some("≃"),
        "approx" => Some("≈"),
        "cong" => Some("≅"),
        "subset" => Some("⊂"),
        "supset" => Some("⊃"),
        "subseteq" => Some("⊆"),
        "supseteq" => Some("⊇"),
        "in" => Some("∈"),
        "notin" => Some("∉"),
        "ni" => Some("∋"),
        "propto" => Some("∝"),
        "perp" => Some("⊥"),
        "parallel" => Some("∥"),
        "leftarrow" => Some("←"),
        "rightarrow" | "to" => Some("→"),
        "leftrightarrow" => Some("↔"),
        "Leftarrow" => Some("⇐"),
        "Rightarrow" => Some("⇒"),
        "Leftrightarrow" => Some("⇔"),
        "mapsto" => Some("↦"),
        "implies" => Some("⟹"),
        "infty" => Some("∞"),
        "partial" => Some("∂"),
        "nabla" => Some("∇"),
        "forall" => Some("∀"),
        "exists" => Some("∃"),
        "neg" | "lnot" => Some("¬"),
        "emptyset" => Some("∅"),
        "sum" => Some("∑"),
        "prod" => Some("∏"),
        "int" => Some("∫"),
        "oint" => Some("∮"),
        "dots" | "ldots" => Some("…"),
        "cdots" => Some("⋯"),
        "vdots" => Some("⋮"),
        "ddots" => Some("⋱"),
        "ell" => Some("ℓ"),
        "hbar" => Some("ℏ"),
        "Re" => Some("ℜ"),
        "Im" => Some("ℑ"),
        "aleph" => Some("ℵ"),
        "langle" => Some("⟨"),
        "rangle" => Some("⟩"),
        "lfloor" => Some("⌊"),
        "rfloor" => Some("⌋"),
        "lceil" => Some("⌈"),
        "rceil" => Some("⌉"),
        "lvert" | "rvert" | "vert" => Some("|"),
        "lVert" | "rVert" | "Vert" => Some("‖"),
        "lbrace" => Some("{"),
        "rbrace" => Some("}"),
        "quad" => Some("\u{2003}"),
        "qquad" => Some("\u{2003}\u{2003}"),
        "comma" => Some("\u{2009}"),
        _ => None,
    }
}

fn is_math_font_command(name: &str) -> bool {
    matches!(
        name,
        "mathrm" | "mathbf" | "mathit" | "mathsf" | "mathtt" | "mathcal" | "mathbb" | "mathfrak"
    )
}

fn visible_math_delimiter(delimiter: &str) -> &str {
    if delimiter == "." {
        ""
    } else {
        delimiter
    }
}

fn encode_math_delimiter(delimiter: &str, is_left: bool) -> String {
    match delimiter {
        "." => ".".to_string(),
        "{" => r"\{".to_string(),
        "}" => r"\}".to_string(),
        "⟨" | "⟩" => {
            if is_left {
                r"\langle".to_string()
            } else {
                r"\rangle".to_string()
            }
        }
        "⌊" | "⌋" => {
            if is_left {
                r"\lfloor".to_string()
            } else {
                r"\rfloor".to_string()
            }
        }
        "⌈" | "⌉" => {
            if is_left {
                r"\lceil".to_string()
            } else {
                r"\rceil".to_string()
            }
        }
        "|" => {
            if is_left {
                r"\lvert".to_string()
            } else {
                r"\rvert".to_string()
            }
        }
        "‖" => {
            if is_left {
                r"\lVert".to_string()
            } else {
                r"\rVert".to_string()
            }
        }
        _ => delimiter.to_string(),
    }
}

fn render_math_annotation_for_anchor(nodes: &[MathNode]) -> String {
    if nodes.len() > 1 {
        format!("({})", render_math_nodes_for_anchor(nodes))
    } else {
        render_math_nodes_for_anchor(nodes)
    }
}

fn render_math_line_for_anchor(
    segments: &[Vec<MathNode>],
    display_tag: &str,
    aligned: bool,
) -> String {
    let separator = if aligned { " " } else { "" };
    let mut rendered = segments
        .iter()
        .map(|segment| render_math_nodes_for_anchor(segment))
        .collect::<Vec<_>>()
        .join(separator);

    if !display_tag.is_empty() {
        if !rendered.is_empty() {
            rendered.push(' ');
        }
        rendered.push('(');
        rendered.push_str(display_tag);
        rendered.push(')');
    }

    rendered
}

fn render_math_nodes_for_anchor(nodes: &[MathNode]) -> String {
    nodes
        .iter()
        .map(render_math_node_for_anchor)
        .collect::<Vec<_>>()
        .join("")
}

fn render_math_node_for_anchor(node: &MathNode) -> String {
    match node {
        MathNode::Ordinary(ch) => ch.to_string(),
        MathNode::Symbol(symbol) => symbol.clone(),
        MathNode::Superscript(node) => format!("^{}", render_math_attachment_for_anchor(node)),
        MathNode::Subscript(node) => format!("_{}", render_math_attachment_for_anchor(node)),
        MathNode::Frac { numer, denom } => format!(
            "({})/({})",
            render_math_nodes_for_anchor(numer),
            render_math_nodes_for_anchor(denom)
        ),
        MathNode::Sqrt { radicand, index } => {
            let body = render_math_nodes_for_anchor(radicand);
            match index {
                Some(index) => format!("√[{}]({body})", render_math_nodes_for_anchor(index)),
                None => format!("√({body})"),
            }
        }
        MathNode::MathFont { body, .. } => render_math_nodes_for_anchor(body),
        MathNode::LeftRight { left, right, body } => format!(
            "{}{}{}",
            visible_math_delimiter(left),
            render_math_nodes_for_anchor(body),
            visible_math_delimiter(right)
        ),
        MathNode::OverUnder {
            kind,
            base,
            annotation,
        } => {
            let base = render_math_nodes_for_anchor(base);
            let annotation = render_math_annotation_for_anchor(annotation);
            match kind {
                OverUnderKind::Over => format!("{base}^{annotation}"),
                OverUnderKind::Under => format!("{base}_{annotation}"),
            }
        }
        MathNode::Group(nodes) => render_math_nodes_for_anchor(nodes),
        MathNode::Text(text) => text.clone(),
    }
}

fn render_math_attachment_for_anchor(node: &MathNode) -> String {
    match node {
        MathNode::Group(nodes) if nodes.len() > 1 => {
            format!("({})", render_math_nodes_for_anchor(nodes))
        }
        _ => render_math_node_for_anchor(node),
    }
}

fn body_nodes_from_text(body: &str) -> Vec<DocumentNode> {
    if body.trim().is_empty() {
        return Vec::new();
    }

    let normalized_body = normalize_body_par_breaks(body);
    let (body_with_placeholders, placeholders) =
        replace_body_markers_with_placeholders(&normalized_body);
    let mut nodes = Vec::new();
    let mut segment_start = 0;

    for (index, marker) in body_with_placeholders
        .char_indices()
        .filter(|(_, ch)| *ch == BODY_PAGE_BREAK_MARKER || *ch == BODY_CLEAR_PAGE_MARKER)
    {
        nodes.extend(body_text_nodes(
            &body_with_placeholders[segment_start..index],
            &placeholders,
        ));
        nodes.push(if marker == BODY_PAGE_BREAK_MARKER {
            DocumentNode::PageBreak
        } else {
            DocumentNode::ClearPage
        });
        segment_start = index + marker.len_utf8();
    }

    nodes.extend(body_text_nodes(
        &body_with_placeholders[segment_start..],
        &placeholders,
    ));
    nodes
}

fn body_text_nodes(body: &str, placeholders: &[DocumentNode]) -> Vec<DocumentNode> {
    if body.trim().is_empty() {
        return Vec::new();
    }

    let mut nodes = Vec::new();
    let mut current_text = String::new();
    let mut in_break = false;

    for line in body.split('\n') {
        if line.trim().is_empty() {
            push_body_text_node(&mut nodes, &mut current_text, placeholders);
            if !nodes.is_empty() && !in_break {
                nodes.push(DocumentNode::ParBreak);
                in_break = true;
            }
            continue;
        }

        if !current_text.is_empty() {
            current_text.push('\n');
        }
        current_text.push_str(line);
        in_break = false;
    }

    push_body_text_node(&mut nodes, &mut current_text, placeholders);
    if matches!(nodes.last(), Some(DocumentNode::ParBreak)) {
        let _ = nodes.pop();
    }

    nodes
}

fn replace_body_markers_with_placeholders(body: &str) -> (String, Vec<DocumentNode>) {
    let mut text = String::with_capacity(body.len());
    let mut placeholders = Vec::new();
    let mut index = 0;

    while index < body.len() {
        let ch = body[index..]
            .chars()
            .next()
            .expect("valid UTF-8 slice should yield a char");

        match ch {
            BODY_HBOX_START | BODY_VBOX_START => {
                let (content, next_index) = extract_box_marker_content(body, index);
                let placeholder = next_box_placeholder(placeholders.len());
                let children = body_nodes_from_text(content);
                let node = if ch == BODY_HBOX_START {
                    DocumentNode::HBox(children)
                } else {
                    DocumentNode::VBox(children)
                };
                placeholders.push(node);
                text.push(placeholder);
                index = next_index;
            }
            BODY_SANS_FAMILY_START | BODY_MONO_FAMILY_START => {
                let (content, next_index) = extract_font_family_marker_content(body, index);
                let placeholder = next_box_placeholder(placeholders.len());
                let role = if ch == BODY_SANS_FAMILY_START {
                    FontFamilyRole::Sans
                } else {
                    FontFamilyRole::Mono
                };
                placeholders.push(DocumentNode::FontFamily {
                    role,
                    children: body_nodes_from_text(content),
                });
                text.push(placeholder);
                index = next_index;
            }
            BODY_HREF_START => {
                let (url, display_text, next_index) = extract_href_marker_content(body, index);
                let placeholder = next_box_placeholder(placeholders.len());
                placeholders.push(DocumentNode::Link {
                    url: url.to_string(),
                    children: body_nodes_from_text(display_text),
                });
                text.push(placeholder);
                index = next_index;
            }
            BODY_HYPERREF_START => {
                let (target, display_text, next_index) =
                    extract_hyperref_marker_content(body, index);
                let placeholder = next_box_placeholder(placeholders.len());
                placeholders.push(DocumentNode::Link {
                    url: format!("#{target}"),
                    children: body_nodes_from_text(display_text),
                });
                text.push(placeholder);
                index = next_index;
            }
            BODY_URL_START => {
                let (url, next_index) = extract_single_marker_content(body, index, BODY_URL_END);
                let placeholder = next_box_placeholder(placeholders.len());
                placeholders.push(DocumentNode::Link {
                    url: url.to_string(),
                    children: vec![DocumentNode::Text(url.to_string())],
                });
                text.push(placeholder);
                index = next_index;
            }
            BODY_EQUATION_ENV_START => {
                let (content, next_index) =
                    extract_single_marker_content(body, index, BODY_EQUATION_ENV_END);
                let placeholder = next_box_placeholder(placeholders.len());
                placeholders.push(deserialize_equation_env(content));
                text.push(placeholder);
                index = next_index;
            }
            BODY_PAGEREF_START => {
                let (_, next_index) = extract_single_marker_content(body, index, BODY_PAGEREF_END);
                let placeholder = next_box_placeholder(placeholders.len());
                placeholders.push(DocumentNode::Text("??".to_string()));
                text.push(placeholder);
                index = next_index;
            }
            BODY_INDEX_ENTRY_START => {
                let (content, next_index) =
                    extract_single_marker_content(body, index, BODY_INDEX_ENTRY_END);
                let placeholder = next_box_placeholder(placeholders.len());
                placeholders.push(deserialize_index_marker(content));
                text.push(placeholder);
                index = next_index;
            }
            BODY_FLOAT_START => {
                let (content, next_index) =
                    extract_single_marker_content(body, index, BODY_FLOAT_END);
                let placeholder = next_box_placeholder(placeholders.len());
                placeholders.push(deserialize_float_marker(content));
                text.push(placeholder);
                index = next_index;
            }
            BODY_INCLUDEGRAPHICS_START => {
                let (content, next_index) =
                    extract_single_marker_content(body, index, BODY_INCLUDEGRAPHICS_END);
                let placeholder = next_box_placeholder(placeholders.len());
                placeholders.push(deserialize_includegraphics_marker(content));
                text.push(placeholder);
                index = next_index;
            }
            BODY_TIKZPICTURE_START => {
                let (content, next_index) =
                    extract_single_marker_content(body, index, BODY_TIKZPICTURE_END);
                let placeholder = next_box_placeholder(placeholders.len());
                placeholders.push(deserialize_tikzpicture_marker(content));
                text.push(placeholder);
                index = next_index;
            }
            BODY_INLINE_MATH_START | BODY_DISPLAY_MATH_START => {
                let (content, next_index) = extract_math_marker_content(body, index);
                let placeholder = next_box_placeholder(placeholders.len());
                let math = parse_math_content(content);
                let node = if ch == BODY_INLINE_MATH_START {
                    DocumentNode::InlineMath(math)
                } else {
                    DocumentNode::DisplayMath(math)
                };
                placeholders.push(node);
                text.push(placeholder);
                index = next_index;
            }
            _ => {
                text.push(ch);
                index += ch.len_utf8();
            }
        }
    }

    (text, placeholders)
}

fn extract_box_marker_content(body: &str, start_index: usize) -> (&str, usize) {
    let start_char = body[start_index..]
        .chars()
        .next()
        .expect("box marker should exist at start index");
    let content_start = start_index + start_char.len_utf8();
    let mut index = content_start;
    let mut depth = 1usize;

    while index < body.len() {
        let ch = body[index..]
            .chars()
            .next()
            .expect("valid UTF-8 slice should yield a char");

        match ch {
            BODY_HBOX_START | BODY_VBOX_START => depth += 1,
            BODY_HBOX_END | BODY_VBOX_END => {
                depth -= 1;
                if depth == 0 {
                    return (&body[content_start..index], index + ch.len_utf8());
                }
            }
            _ => {}
        }

        index += ch.len_utf8();
    }

    (&body[content_start..], body.len())
}

fn extract_font_family_marker_content(body: &str, start_index: usize) -> (&str, usize) {
    let start_char = body[start_index..]
        .chars()
        .next()
        .expect("font family marker should exist at start index");
    let content_start = start_index + start_char.len_utf8();
    let end_marker = if start_char == BODY_SANS_FAMILY_START {
        BODY_SANS_FAMILY_END
    } else {
        BODY_MONO_FAMILY_END
    };
    let mut index = content_start;
    let mut depth = 1usize;

    while index < body.len() {
        let ch = body[index..]
            .chars()
            .next()
            .expect("valid UTF-8 slice should yield a char");

        if ch == start_char {
            depth += 1;
        } else if ch == end_marker {
            depth -= 1;
            if depth == 0 {
                return (&body[content_start..index], index + ch.len_utf8());
            }
        }

        index += ch.len_utf8();
    }

    (&body[content_start..], body.len())
}

fn extract_math_marker_content(body: &str, start_index: usize) -> (&str, usize) {
    let start_char = body[start_index..]
        .chars()
        .next()
        .expect("math marker should exist at start index");
    let content_start = start_index + start_char.len_utf8();
    let end_marker = if start_char == BODY_INLINE_MATH_START {
        BODY_INLINE_MATH_END
    } else {
        BODY_DISPLAY_MATH_END
    };
    let mut index = content_start;

    while index < body.len() {
        let ch = body[index..]
            .chars()
            .next()
            .expect("valid UTF-8 slice should yield a char");
        if ch == end_marker {
            return (&body[content_start..index], index + ch.len_utf8());
        }
        index += ch.len_utf8();
    }

    (&body[content_start..], body.len())
}

fn extract_href_marker_content(body: &str, start_index: usize) -> (&str, &str, usize) {
    let start_char = body[start_index..]
        .chars()
        .next()
        .expect("href marker should exist at start index");
    let url_start = start_index + start_char.len_utf8();
    let mut index = url_start;

    while index < body.len() {
        let ch = body[index..]
            .chars()
            .next()
            .expect("valid UTF-8 slice should yield a char");
        if ch == BODY_HREF_URL_END {
            let display_start = index + ch.len_utf8();
            let mut display_end = display_start;

            while display_end < body.len() {
                let next = body[display_end..]
                    .chars()
                    .next()
                    .expect("valid UTF-8 slice should yield a char");
                if next == BODY_HREF_END {
                    return (
                        &body[url_start..index],
                        &body[display_start..display_end],
                        display_end + next.len_utf8(),
                    );
                }
                display_end += next.len_utf8();
            }

            return (&body[url_start..index], &body[display_start..], body.len());
        }
        index += ch.len_utf8();
    }

    (&body[url_start..], "", body.len())
}

fn extract_hyperref_marker_content(body: &str, start_index: usize) -> (&str, &str, usize) {
    let start_char = body[start_index..]
        .chars()
        .next()
        .expect("hyperref marker should exist at start index");
    let target_start = start_index + start_char.len_utf8();
    let mut index = target_start;

    while index < body.len() {
        let ch = body[index..]
            .chars()
            .next()
            .expect("valid UTF-8 slice should yield a char");
        if ch == BODY_HYPERREF_TARGET_END {
            let display_start = index + ch.len_utf8();
            let mut display_end = display_start;

            while display_end < body.len() {
                let next = body[display_end..]
                    .chars()
                    .next()
                    .expect("valid UTF-8 slice should yield a char");
                if next == BODY_HYPERREF_END {
                    return (
                        &body[target_start..index],
                        &body[display_start..display_end],
                        display_end + next.len_utf8(),
                    );
                }
                display_end += next.len_utf8();
            }

            return (
                &body[target_start..index],
                &body[display_start..],
                body.len(),
            );
        }
        index += ch.len_utf8();
    }

    (&body[target_start..], "", body.len())
}

fn extract_single_marker_content(
    body: &str,
    start_index: usize,
    end_marker: char,
) -> (&str, usize) {
    let start_char = body[start_index..]
        .chars()
        .next()
        .expect("marker should exist at start index");
    let content_start = start_index + start_char.len_utf8();
    let mut index = content_start;

    while index < body.len() {
        let ch = body[index..]
            .chars()
            .next()
            .expect("valid UTF-8 slice should yield a char");
        if ch == end_marker {
            return (&body[content_start..index], index + ch.len_utf8());
        }
        index += ch.len_utf8();
    }

    (&body[content_start..], body.len())
}

fn serialize_index_marker(entry: &IndexRawEntry) -> String {
    format!(
        "{}{}{}",
        entry.sort_key, BODY_INDEX_ENTRY_FIELD_SEP, entry.display
    )
}

fn deserialize_index_marker(content: &str) -> DocumentNode {
    let (sort_key, display) = content
        .split_once(BODY_INDEX_ENTRY_FIELD_SEP)
        .unwrap_or((content, content));
    DocumentNode::IndexMarker(IndexRawEntry {
        sort_key: sort_key.to_string(),
        display: display.to_string(),
    })
}

fn serialize_includegraphics_marker(path: &str, options: &IncludeGraphicsOptions) -> String {
    let width = options
        .width
        .map(|value| value.0.to_string())
        .unwrap_or_default();
    let height = options
        .height
        .map(|value| value.0.to_string())
        .unwrap_or_default();
    let scale = options
        .scale
        .map(|value| value.to_string())
        .unwrap_or_default();

    format!(
        "{path}{BODY_INCLUDEGRAPHICS_PATH_END}{width}{BODY_INCLUDEGRAPHICS_FIELD_SEPARATOR}{height}{BODY_INCLUDEGRAPHICS_FIELD_SEPARATOR}{scale}"
    )
}

fn deserialize_includegraphics_marker(content: &str) -> DocumentNode {
    let (path, rest) = content
        .split_once(BODY_INCLUDEGRAPHICS_PATH_END)
        .unwrap_or((content, ""));
    let mut fields = rest.split(BODY_INCLUDEGRAPHICS_FIELD_SEPARATOR);
    let options = IncludeGraphicsOptions {
        width: fields.next().and_then(parse_dimension_marker_field),
        height: fields.next().and_then(parse_dimension_marker_field),
        scale: fields.next().and_then(parse_scale_marker_field),
    };

    DocumentNode::IncludeGraphics {
        path: path.to_string(),
        options,
    }
}

fn serialize_tikzpicture_marker(graphics_box: &GraphicsBox) -> String {
    serde_json::to_string(graphics_box).expect("graphics box should serialize")
}

fn deserialize_tikzpicture_marker(content: &str) -> DocumentNode {
    let graphics_box =
        serde_json::from_str(content).expect("tikz graphics marker should deserialize");
    DocumentNode::TikzPicture { graphics_box }
}

fn float_type_marker(float_type: FloatType) -> char {
    match float_type {
        FloatType::Figure => 'f',
        FloatType::Table => 't',
    }
}

fn parse_float_type_marker(value: &str) -> FloatType {
    match value.trim() {
        "t" => FloatType::Table,
        _ => FloatType::Figure,
    }
}

fn deserialize_float_marker(content: &str) -> DocumentNode {
    let (float_type_field, rest) = content
        .split_once(BODY_FLOAT_TYPE_SEP)
        .unwrap_or(("f", content));
    let (specifier_field, rest) = rest
        .split_once(BODY_FLOAT_SPECIFIER_SEP)
        .unwrap_or(("", rest));
    let (body_content, metadata) = rest
        .split_once(BODY_FLOAT_CAPTION_SEP)
        .unwrap_or((rest, ""));
    let (caption_field, label_field) = metadata
        .split_once(BODY_FLOAT_LABEL_SEP)
        .unwrap_or((metadata, ""));

    DocumentNode::Float {
        float_type: parse_float_type_marker(float_type_field),
        specifier: marker_optional_raw_string(specifier_field),
        content: body_nodes_from_text(body_content),
        caption: marker_optional_string(caption_field),
        label: marker_optional_string(label_field),
    }
}

fn parse_dimension_marker_field(value: &str) -> Option<DimensionValue> {
    let trimmed = value.trim();
    (!trimmed.is_empty())
        .then(|| trimmed.parse::<i64>().ok().map(DimensionValue))
        .flatten()
}

fn marker_optional_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn marker_optional_raw_string(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn parse_scale_marker_field(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    (!trimmed.is_empty())
        .then(|| trimmed.parse::<f64>().ok())
        .flatten()
}

fn parse_index_entry_argument(raw: &str) -> Option<IndexRawEntry> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (sort_key, display) = match trimmed.split_once('@') {
        Some((sort_key, display)) => (sort_key.trim(), display.trim()),
        None => (trimmed, trimmed),
    };
    if sort_key.is_empty() || display.is_empty() {
        return None;
    }

    Some(IndexRawEntry {
        sort_key: sort_key.to_string(),
        display: display.to_string(),
    })
}

fn format_index_entries(entries: &[IndexEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }

    let mut entries = entries.to_vec();
    entries.sort_by(|left, right| {
        left.sort_key
            .to_lowercase()
            .cmp(&right.sort_key.to_lowercase())
            .then_with(|| left.sort_key.cmp(&right.sort_key))
            .then_with(|| left.display.cmp(&right.display))
            .then_with(|| left.page.cmp(&right.page))
    });

    // Merge page numbers for entries with the same (sort_key, display)
    let mut merged: Vec<(String, String, Vec<String>)> = Vec::new();
    for entry in &entries {
        let page_str = entry
            .page
            .map(|p| p.to_string())
            .unwrap_or_else(|| "?".to_string());
        if let Some(last) = merged.last_mut() {
            if last.0 == entry.sort_key && last.1 == entry.display {
                if !last.2.contains(&page_str) {
                    last.2.push(page_str);
                }
                continue;
            }
        }
        merged.push((
            entry.sort_key.clone(),
            entry.display.clone(),
            vec![page_str],
        ));
    }

    let mut rendered = String::new();
    let mut current_heading = None::<String>;
    for (sort_key, display, pages) in &merged {
        let heading = sort_key
            .chars()
            .next()
            .map(|ch| ch.to_uppercase().collect::<String>())
            .unwrap_or_else(|| "#".to_string());
        if current_heading.as_deref() != Some(heading.as_str()) {
            if !rendered.is_empty() {
                rendered.push('\n');
            }
            rendered.push_str(&heading);
            rendered.push('\n');
            current_heading = Some(heading);
        }

        rendered.push_str(display);
        rendered.push_str(" . . . . ");
        rendered.push_str(&pages.join(", "));
        rendered.push('\n');
    }

    rendered.trim_end().to_string()
}

fn parse_hypersetup_options(input: &str) -> Vec<(String, String)> {
    split_top_level_delimited(input, ',')
        .into_iter()
        .filter_map(|entry| {
            let (key, value) = split_top_level_key_value(entry)?;
            Some((
                key.trim().to_ascii_lowercase(),
                normalize_hypersetup_value(value),
            ))
        })
        .collect()
}

fn split_top_level_key_value(input: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;

    for (index, ch) in input.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            '=' if depth == 0 => return Some((&input[..index], &input[index + 1..])),
            _ => {}
        }
    }

    None
}

fn split_top_level_delimited(input: &str, delimiter: char) -> Vec<&str> {
    let mut entries = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;

    for (index, ch) in input.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ if ch == delimiter && depth == 0 => {
                let entry = input[start..index].trim();
                if !entry.is_empty() {
                    entries.push(entry);
                }
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }

    let entry = input[start..].trim();
    if !entry.is_empty() {
        entries.push(entry);
    }

    entries
}

fn normalize_hypersetup_value(value: &str) -> String {
    let trimmed = value.trim();
    strip_wrapping_braces(trimmed)
        .map(str::trim)
        .unwrap_or(trimmed)
        .to_string()
}

fn strip_wrapping_braces(value: &str) -> Option<&str> {
    if !value.starts_with('{') || !value.ends_with('}') {
        return None;
    }

    let mut depth = 0usize;
    for (index, ch) in value.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && index != value.len() - ch.len_utf8() {
                    return None;
                }
            }
            _ => {}
        }
    }

    value.strip_prefix('{')?.strip_suffix('}')
}

fn parse_hypersetup_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn parse_includegraphics_options(tokens: &[Token]) -> IncludeGraphicsOptions {
    let mut options = IncludeGraphicsOptions::default();
    let text = tokens_to_text(tokens);

    for entry in text
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        match key {
            "width" => options.width = parse_dimension_option(value),
            "height" => options.height = parse_dimension_option(value),
            "scale" => options.scale = value.parse::<f64>().ok(),
            _ => {}
        }
    }

    options
}

fn parse_dimension_option(value: &str) -> Option<DimensionValue> {
    let trimmed = value.trim();
    let number = trimmed
        .strip_suffix("pt")
        .or_else(|| trimmed.strip_suffix("PT"))?
        .trim()
        .parse::<f64>()
        .ok()?;

    Some(DimensionValue((number * 65_536.0).round() as i64))
}

fn push_body_text_node(
    nodes: &mut Vec<DocumentNode>,
    current_text: &mut String,
    placeholders: &[DocumentNode],
) {
    if current_text.is_empty() {
        return;
    }

    let text = current_text.trim().to_string();
    current_text.clear();

    if text.is_empty() {
        return;
    }

    let mut plain_text = String::new();
    for ch in text.chars() {
        if let Some(index) = box_placeholder_index(ch, placeholders.len()) {
            if !plain_text.is_empty() {
                nodes.push(DocumentNode::Text(std::mem::take(&mut plain_text)));
            }
            nodes.push(placeholders[index].clone());
        } else {
            plain_text.push(ch);
        }
    }

    if !plain_text.is_empty() {
        nodes.push(DocumentNode::Text(plain_text));
    }
}

fn next_box_placeholder(index: usize) -> char {
    char::from_u32(BODY_BOX_PLACEHOLDER_BASE + index as u32)
        .expect("private-use placeholder codepoint should be valid")
}

fn font_family_markers(role: FontFamilyRole) -> (char, char) {
    match role {
        FontFamilyRole::Main => unreachable!("main font body markers are not used"),
        FontFamilyRole::Sans => (BODY_SANS_FAMILY_START, BODY_SANS_FAMILY_END),
        FontFamilyRole::Mono => (BODY_MONO_FAMILY_START, BODY_MONO_FAMILY_END),
    }
}

fn box_placeholder_index(ch: char, placeholder_count: usize) -> Option<usize> {
    let codepoint = ch as u32;
    if codepoint < BODY_BOX_PLACEHOLDER_BASE {
        return None;
    }

    let index = (codepoint - BODY_BOX_PLACEHOLDER_BASE) as usize;
    (index < placeholder_count).then_some(index)
}

fn encode_body_markers_in_text(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    let mut index = 0;

    while index < text.len() {
        let ch = text[index..]
            .chars()
            .next()
            .expect("valid UTF-8 slice should yield a char");

        if ch == '$' {
            let delimiter_end = index + ch.len_utf8();
            let Some(close_index) = find_inline_math_end(text, delimiter_end) else {
                encoded.push(ch);
                index = delimiter_end;
                continue;
            };
            let content = &text[delimiter_end..close_index];
            encoded.push(BODY_INLINE_MATH_START);
            encoded.push_str(content);
            encoded.push(BODY_INLINE_MATH_END);
            index = close_index + ch.len_utf8();
            continue;
        }

        if ch != '\\' {
            encoded.push(ch);
            index += ch.len_utf8();
            continue;
        }

        let command_start = index + ch.len_utf8();
        let Some(next_char) = text[command_start..].chars().next() else {
            encoded.push(ch);
            break;
        };

        if next_char == '[' {
            let command_end = command_start + next_char.len_utf8();
            if let Some(close_index) = find_display_math_end(text, command_end) {
                let content = &text[command_end..close_index];
                encoded.push(BODY_DISPLAY_MATH_START);
                encoded.push_str(content);
                encoded.push(BODY_DISPLAY_MATH_END);
                index = close_index + r"\]".len();
                continue;
            }

            encoded.push(ch);
            encoded.push(next_char);
            index = command_end;
            continue;
        }

        if !next_char.is_ascii_alphabetic() {
            encoded.push(ch);
            encoded.push(next_char);
            index = command_start + next_char.len_utf8();
            continue;
        }

        let mut command_end = command_start + next_char.len_utf8();
        while command_end < text.len() {
            let next = text[command_end..]
                .chars()
                .next()
                .expect("valid UTF-8 slice should yield a char");
            if !next.is_ascii_alphabetic() {
                break;
            }
            command_end += next.len_utf8();
        }

        let command = &text[command_start..command_end];
        match command {
            "hbox" | "vbox" => {
                let brace_start = skip_optional_command_whitespace(text, command_end);
                if let Some((content, next_index)) = extract_braced_text(text, brace_start) {
                    let encoded_content = encode_body_markers_in_text(content);
                    let (start_marker, end_marker) = if command == "hbox" {
                        (BODY_HBOX_START, BODY_HBOX_END)
                    } else {
                        (BODY_VBOX_START, BODY_VBOX_END)
                    };
                    encoded.push(start_marker);
                    encoded.push_str(&encoded_content);
                    encoded.push(end_marker);
                    index = next_index;
                    continue;
                }

                encoded.push(ch);
                encoded.push_str(command);
                index = command_end;
            }
            "textsf" | "texttt" => {
                let brace_start = skip_optional_command_whitespace(text, command_end);
                if let Some((content, next_index)) = extract_braced_text(text, brace_start) {
                    let encoded_content = encode_body_markers_in_text(content);
                    let (start_marker, end_marker) = if command == "textsf" {
                        font_family_markers(FontFamilyRole::Sans)
                    } else {
                        font_family_markers(FontFamilyRole::Mono)
                    };
                    encoded.push(start_marker);
                    encoded.push_str(&encoded_content);
                    encoded.push(end_marker);
                    index = next_index;
                    continue;
                }

                encoded.push(ch);
                encoded.push_str(command);
                index = command_end;
            }
            "pagebreak" | "newpage" => {
                encoded.push(BODY_PAGE_BREAK_MARKER);
                index = command_end;
            }
            "clearpage" => {
                encoded.push(BODY_CLEAR_PAGE_MARKER);
                index = command_end;
            }
            "href" => {
                let url_start = skip_optional_command_whitespace(text, command_end);
                if let Some((url, after_url)) = extract_braced_text(text, url_start) {
                    let display_start = skip_optional_command_whitespace(text, after_url);
                    if let Some((display_text, next_index)) =
                        extract_braced_text(text, display_start)
                    {
                        encoded.push(BODY_HREF_START);
                        encoded.push_str(url);
                        encoded.push(BODY_HREF_URL_END);
                        encoded.push_str(&encode_body_markers_in_text(display_text));
                        encoded.push(BODY_HREF_END);
                        index = next_index;
                        continue;
                    }
                }

                encoded.push(ch);
                encoded.push_str(command);
                index = command_end;
            }
            "hyperref" => {
                let target_start = skip_optional_command_whitespace(text, command_end);
                if let Some((target, after_target)) = extract_bracket_text(text, target_start) {
                    let display_start = skip_optional_command_whitespace(text, after_target);
                    if let Some((display_text, next_index)) =
                        extract_braced_text(text, display_start)
                    {
                        encoded.push_str(&internal_link_marker(
                            target,
                            &encode_body_markers_in_text(display_text),
                        ));
                        index = next_index;
                        continue;
                    }
                }

                encoded.push(ch);
                encoded.push_str(command);
                index = command_end;
            }
            "url" => {
                let url_start = skip_optional_command_whitespace(text, command_end);
                if let Some((url, next_index)) = extract_braced_text(text, url_start) {
                    encoded.push(BODY_URL_START);
                    encoded.push_str(url);
                    encoded.push(BODY_URL_END);
                    index = next_index;
                    continue;
                }

                encoded.push(ch);
                encoded.push_str(command);
                index = command_end;
            }
            _ => {
                encoded.push(ch);
                encoded.push_str(command);
                index = command_end;
            }
        }
    }

    encoded
}

fn find_inline_math_end(text: &str, mut index: usize) -> Option<usize> {
    while index < text.len() {
        let ch = text[index..].chars().next()?;
        if ch == '$' {
            return Some(index);
        }
        index += ch.len_utf8();
    }
    None
}

fn find_display_math_end(text: &str, mut index: usize) -> Option<usize> {
    while index < text.len() {
        let ch = text[index..].chars().next()?;
        if ch == '\\' && text[index..].starts_with(r"\]") {
            return Some(index);
        }
        index += ch.len_utf8();
    }
    None
}

fn skip_optional_command_whitespace(text: &str, mut index: usize) -> usize {
    while index < text.len() {
        let ch = text[index..]
            .chars()
            .next()
            .expect("valid UTF-8 slice should yield a char");
        if !ch.is_whitespace() {
            break;
        }
        index += ch.len_utf8();
    }
    index
}

fn extract_braced_text(text: &str, open_index: usize) -> Option<(&str, usize)> {
    if open_index >= text.len() || !text[open_index..].starts_with('{') {
        return None;
    }

    let mut depth = 0usize;
    let mut index = open_index;
    let content_start = open_index + 1;

    while index < text.len() {
        let ch = text[index..]
            .chars()
            .next()
            .expect("valid UTF-8 slice should yield a char");

        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&text[content_start..index], index + ch.len_utf8()));
                }
            }
            _ => {}
        }

        index += ch.len_utf8();
    }

    None
}

fn extract_bracket_text(text: &str, open_index: usize) -> Option<(&str, usize)> {
    if open_index >= text.len() || !text[open_index..].starts_with('[') {
        return None;
    }

    let mut depth = 0usize;
    let mut index = open_index;
    let content_start = open_index + 1;

    while index < text.len() {
        let ch = text[index..]
            .chars()
            .next()
            .expect("valid UTF-8 slice should yield a char");

        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&text[content_start..index], index + ch.len_utf8()));
                }
            }
            _ => {}
        }

        index += ch.len_utf8();
    }

    None
}

fn parse_math_content(content: &str) -> Vec<MathNode> {
    MathParser::new(content).parse_all()
}

fn split_math_environment_lines(content: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut chars = content.chars().peekable();
    let mut brace_depth = 0usize;

    while let Some(ch) = chars.next() {
        match ch {
            '{' => {
                brace_depth += 1;
                current.push(ch);
            }
            '}' => {
                brace_depth = brace_depth.saturating_sub(1);
                current.push(ch);
            }
            '\\' if brace_depth == 0 && matches!(chars.peek(), Some('\\')) => {
                let _ = chars.next();
                lines.push(current.trim().to_string());
                current.clear();
                while matches!(chars.peek(), Some(next) if next.is_whitespace()) {
                    let _ = chars.next();
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.trim().is_empty() || lines.is_empty() {
        lines.push(current.trim().to_string());
    }

    lines
}

fn strip_math_line_labels(line: &str) -> (String, Vec<String>) {
    let mut output = String::new();
    let mut labels = Vec::new();
    let mut index = 0;

    while index < line.len() {
        if matches_control_word_at(line, index, "label") {
            let brace_start = skip_optional_command_whitespace(line, index + r"\label".len());
            if let Some((content, next_index)) = extract_braced_text(line, brace_start) {
                let key = content.trim();
                if !key.is_empty() {
                    labels.push(key.to_string());
                }
                index = next_index;
                continue;
            }
        }

        let ch = line[index..]
            .chars()
            .next()
            .expect("valid UTF-8 slice should yield a char");
        output.push(ch);
        index += ch.len_utf8();
    }

    (output, labels)
}

fn strip_math_line_tag(line: &str) -> (String, LineTag) {
    let mut current = line.trim_end().to_string();
    let mut saw_notag = false;
    let mut custom_tag = None;

    loop {
        if let Some((prefix, tag)) = strip_trailing_tag_command(&current) {
            current = prefix;
            custom_tag = Some(tag);
            continue;
        }

        if let Some(prefix) = strip_trailing_notag_command(&current) {
            current = prefix;
            saw_notag = true;
            continue;
        }

        break;
    }

    let tag = if let Some(tag) = custom_tag {
        LineTag::Custom(tag)
    } else if saw_notag {
        LineTag::Notag
    } else {
        LineTag::Auto
    };

    (current.trim_end().to_string(), tag)
}

fn strip_trailing_tag_command(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_end();
    let start = trimmed.rfind(r"\tag")?;
    if !matches_control_word_at(trimmed, start, "tag") {
        return None;
    }
    let brace_start = skip_optional_command_whitespace(trimmed, start + r"\tag".len());
    let (content, next_index) = extract_braced_text(trimmed, brace_start)?;
    if !trimmed[next_index..].trim().is_empty() {
        return None;
    }

    Some((
        trimmed[..start].trim_end().to_string(),
        content.trim().to_string(),
    ))
}

fn strip_trailing_notag_command(line: &str) -> Option<String> {
    let trimmed = line.trim_end();
    let start = trimmed.rfind(r"\notag")?;
    if !matches_control_word_at(trimmed, start, "notag") {
        return None;
    }
    let end = start + r"\notag".len();
    if !trimmed[end..].trim().is_empty() {
        return None;
    }

    Some(trimmed[..start].trim_end().to_string())
}

fn split_math_line_segments(line: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut brace_depth = 0usize;

    while let Some(ch) = chars.next() {
        match ch {
            '{' => {
                brace_depth += 1;
                current.push(ch);
            }
            '}' => {
                brace_depth = brace_depth.saturating_sub(1);
                current.push(ch);
            }
            '\\' => {
                current.push(ch);
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '&' if brace_depth == 0 => {
                segments.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }

    segments.push(current);
    segments
}

fn matches_control_word_at(text: &str, index: usize, name: &str) -> bool {
    let command = format!(r"\{name}");
    if !text[index..].starts_with(&command) {
        return false;
    }

    let next_index = index + command.len();
    !matches!(
        text[next_index..].chars().next(),
        Some(ch) if ch.is_ascii_alphabetic()
    )
}

fn serialize_equation_env(numbered: bool, aligned: bool, lines: &[MathLine]) -> String {
    let mut encoded = String::new();
    encoded.push(if numbered { 'n' } else { 'u' });
    encoded.push(if aligned { 'a' } else { 'e' });

    for line in lines {
        encoded.push(EQUATION_ENV_ROW_SEPARATOR);
        let line_tag = match &line.tag {
            LineTag::Auto => "a".to_string(),
            LineTag::Notag => "n".to_string(),
            LineTag::Custom(tag) => format!("c{tag}"),
        };
        encoded.push_str(&line_tag);
        encoded.push(EQUATION_ENV_FIELD_SEPARATOR);
        encoded.push_str(line.display_tag.as_deref().unwrap_or_default());
        encoded.push(EQUATION_ENV_FIELD_SEPARATOR);
        let segment_text = line
            .segments
            .iter()
            .map(|segment| render_math_nodes_for_encoding(segment))
            .collect::<Vec<_>>()
            .join(&EQUATION_ENV_SEGMENT_SEPARATOR.to_string());
        encoded.push_str(&segment_text);
    }

    encoded
}

fn deserialize_equation_env(content: &str) -> DocumentNode {
    let mut rows = content.split(EQUATION_ENV_ROW_SEPARATOR);
    let header = rows.next().unwrap_or_default();
    let numbered = header.starts_with('n');
    let aligned = header.chars().nth(1) == Some('a');
    let mut lines = Vec::new();

    for row in rows {
        let mut fields = row.splitn(3, EQUATION_ENV_FIELD_SEPARATOR);
        let tag_field = fields.next().unwrap_or_default();
        let display_tag = fields.next().unwrap_or_default();
        let segments_field = fields.next().unwrap_or_default();
        let tag = match tag_field.chars().next() {
            Some('n') => LineTag::Notag,
            Some('c') => LineTag::Custom(tag_field[1..].to_string()),
            _ => LineTag::Auto,
        };
        let segments = if segments_field.is_empty() {
            vec![Vec::new()]
        } else {
            segments_field
                .split(EQUATION_ENV_SEGMENT_SEPARATOR)
                .map(parse_math_content)
                .collect::<Vec<_>>()
        };

        lines.push(MathLine {
            segments,
            tag,
            display_tag: (!display_tag.is_empty()).then(|| display_tag.to_string()),
        });
    }

    DocumentNode::EquationEnv {
        lines,
        numbered,
        aligned,
    }
}

fn render_math_nodes_for_encoding(nodes: &[MathNode]) -> String {
    nodes
        .iter()
        .map(render_math_node_for_encoding)
        .collect::<Vec<_>>()
        .join("")
}

fn render_math_node_for_encoding(node: &MathNode) -> String {
    match node {
        MathNode::Ordinary(ch) => ch.to_string(),
        MathNode::Symbol(symbol) => symbol.clone(),
        MathNode::Superscript(node) => format!("^{}", render_math_attachment_for_encoding(node)),
        MathNode::Subscript(node) => format!("_{}", render_math_attachment_for_encoding(node)),
        MathNode::Frac { numer, denom } => {
            format!(
                r"\frac{{{}}}{{{}}}",
                render_math_nodes_for_encoding(numer),
                render_math_nodes_for_encoding(denom)
            )
        }
        MathNode::Sqrt { radicand, index } => match index {
            Some(index) => format!(
                r"\sqrt[{}]{{{}}}",
                render_math_nodes_for_encoding(index),
                render_math_nodes_for_encoding(radicand)
            ),
            None => format!(r"\sqrt{{{}}}", render_math_nodes_for_encoding(radicand)),
        },
        MathNode::MathFont { cmd, body } => {
            format!(r"\{cmd}{{{}}}", render_math_nodes_for_encoding(body))
        }
        MathNode::LeftRight { left, right, body } => format!(
            r"\left{}{}\right{}",
            encode_math_delimiter(left, true),
            render_math_nodes_for_encoding(body),
            encode_math_delimiter(right, false)
        ),
        MathNode::OverUnder {
            kind,
            base,
            annotation,
        } => {
            let cmd = match kind {
                OverUnderKind::Over => "overset",
                OverUnderKind::Under => "underset",
            };
            format!(
                r"\{cmd}{{{}}}{{{}}}",
                render_math_nodes_for_encoding(annotation),
                render_math_nodes_for_encoding(base)
            )
        }
        MathNode::Group(nodes) => format!("{{{}}}", render_math_nodes_for_encoding(nodes)),
        MathNode::Text(text) => format!(r"\text{{{text}}}"),
    }
}

fn render_math_attachment_for_encoding(node: &MathNode) -> String {
    match node {
        MathNode::Group(nodes) => format!("{{{}}}", render_math_nodes_for_encoding(nodes)),
        _ => render_math_node_for_encoding(node),
    }
}

struct MathParser<'a> {
    input: &'a str,
    index: usize,
}

impl<'a> MathParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, index: 0 }
    }

    fn parse_all(&mut self) -> Vec<MathNode> {
        self.parse_until(None)
    }

    fn parse_until(&mut self, end: Option<char>) -> Vec<MathNode> {
        let mut nodes = Vec::new();

        while let Some(ch) = self.peek_char() {
            if Some(ch) == end {
                let _ = self.take_char();
                break;
            }

            if ch.is_whitespace() {
                let _ = self.take_char();
                continue;
            }

            match ch {
                '^' => {
                    let _ = self.take_char();
                    if let Some(target) = self.parse_attachment() {
                        nodes.push(MathNode::Superscript(Box::new(target)));
                    }
                }
                '_' => {
                    let _ = self.take_char();
                    if let Some(target) = self.parse_attachment() {
                        nodes.push(MathNode::Subscript(Box::new(target)));
                    }
                }
                '{' => {
                    let _ = self.take_char();
                    nodes.push(MathNode::Group(self.parse_until(Some('}'))));
                }
                '\\' => {
                    let _ = self.take_char();
                    nodes.extend(self.parse_control_sequence());
                }
                _ => {
                    let _ = self.take_char();
                    nodes.push(MathNode::Ordinary(ch));
                }
            }
        }

        nodes
    }

    fn parse_attachment(&mut self) -> Option<MathNode> {
        while matches!(self.peek_char(), Some(ch) if ch.is_whitespace()) {
            let _ = self.take_char();
        }

        match self.peek_char()? {
            '{' => {
                let _ = self.take_char();
                Some(MathNode::Group(self.parse_until(Some('}'))))
            }
            '\\' => {
                let _ = self.take_char();
                let mut nodes = self.parse_control_sequence();
                if nodes.len() == 1 {
                    Some(nodes.remove(0))
                } else {
                    Some(MathNode::Group(nodes))
                }
            }
            ch => {
                let _ = self.take_char();
                Some(MathNode::Ordinary(ch))
            }
        }
    }

    fn parse_control_sequence(&mut self) -> Vec<MathNode> {
        let Some(ch) = self.peek_char() else {
            return vec![MathNode::Ordinary('\\')];
        };

        if ch.is_ascii_alphabetic() {
            let name = self.take_control_word();
            return self.parse_named_control_sequence(&name);
        }

        self.parse_control_symbol()
    }

    fn parse_required_group(&mut self) -> Vec<MathNode> {
        while matches!(self.peek_char(), Some(ch) if ch.is_whitespace()) {
            let _ = self.take_char();
        }

        if self.peek_char() == Some('{') {
            let _ = self.take_char();
            self.parse_until(Some('}'))
        } else {
            self.parse_attachment().into_iter().collect()
        }
    }

    fn parse_required_text_group(&mut self) -> String {
        while matches!(self.peek_char(), Some(ch) if ch.is_whitespace()) {
            let _ = self.take_char();
        }

        if self.peek_char() != Some('{') {
            return String::new();
        }

        let _ = self.take_char();
        let mut text = String::new();
        let mut depth = 1usize;

        while let Some(ch) = self.take_char() {
            match ch {
                '{' => {
                    depth += 1;
                    text.push(ch);
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    text.push(ch);
                }
                _ => text.push(ch),
            }
        }

        text
    }

    fn parse_optional_group(&mut self, open: char, close: char) -> Option<Vec<MathNode>> {
        while matches!(self.peek_char(), Some(ch) if ch.is_whitespace()) {
            let _ = self.take_char();
        }

        if self.peek_char() != Some(open) {
            return None;
        }

        let _ = self.take_char();
        Some(self.parse_until(Some(close)))
    }

    fn take_control_word(&mut self) -> String {
        let mut name = String::new();
        while let Some(next) = self.peek_char() {
            if !next.is_ascii_alphabetic() {
                break;
            }
            name.push(next);
            let _ = self.take_char();
        }
        name
    }

    fn parse_control_symbol(&mut self) -> Vec<MathNode> {
        let symbol = self.take_char().expect("peek_char ensured a symbol exists");
        match symbol {
            ',' => vec![MathNode::Symbol("\u{2009}".to_string())],
            _ => vec![MathNode::Ordinary(symbol)],
        }
    }

    fn parse_named_control_sequence(&mut self, name: &str) -> Vec<MathNode> {
        if name == "frac" {
            let numer = self.parse_required_group();
            let denom = self.parse_required_group();
            return vec![MathNode::Frac { numer, denom }];
        }

        if name == "text" {
            return vec![MathNode::Text(self.parse_required_text_group())];
        }

        if name == "sqrt" {
            let index = self.parse_optional_group('[', ']');
            let radicand = self.parse_required_group();
            return vec![MathNode::Sqrt { radicand, index }];
        }

        if is_math_font_command(name) {
            return vec![MathNode::MathFont {
                cmd: name.to_string(),
                body: self.parse_required_group(),
            }];
        }

        if name == "left" {
            let left = self.parse_math_delimiter();
            let (body, right) = self.parse_until_right();
            return vec![MathNode::LeftRight { left, right, body }];
        }

        if name == "overset" || name == "underset" {
            let annotation = self.parse_required_group();
            let base = self.parse_required_group();
            return vec![MathNode::OverUnder {
                kind: if name == "overset" {
                    OverUnderKind::Over
                } else {
                    OverUnderKind::Under
                },
                base,
                annotation,
            }];
        }

        if let Some(symbol) = math_symbol_lookup(name) {
            return vec![MathNode::Symbol(symbol.to_string())];
        }

        name.chars().map(MathNode::Ordinary).collect()
    }

    fn parse_math_delimiter(&mut self) -> String {
        while matches!(self.peek_char(), Some(ch) if ch.is_whitespace()) {
            let _ = self.take_char();
        }

        match self.peek_char() {
            Some('\\') => {
                let _ = self.take_char();
                if matches!(self.peek_char(), Some(ch) if ch.is_ascii_alphabetic()) {
                    let name = self.take_control_word();
                    if let Some(symbol) = math_symbol_lookup(&name) {
                        symbol.to_string()
                    } else {
                        name
                    }
                } else {
                    self.take_char()
                        .map(|ch| ch.to_string())
                        .unwrap_or_else(|| ".".to_string())
                }
            }
            Some(ch) => {
                let _ = self.take_char();
                ch.to_string()
            }
            None => ".".to_string(),
        }
    }

    fn parse_until_right(&mut self) -> (Vec<MathNode>, String) {
        let mut nodes = Vec::new();

        while let Some(ch) = self.peek_char() {
            if ch.is_whitespace() {
                let _ = self.take_char();
                continue;
            }

            match ch {
                '^' => {
                    let _ = self.take_char();
                    if let Some(target) = self.parse_attachment() {
                        nodes.push(MathNode::Superscript(Box::new(target)));
                    }
                }
                '_' => {
                    let _ = self.take_char();
                    if let Some(target) = self.parse_attachment() {
                        nodes.push(MathNode::Subscript(Box::new(target)));
                    }
                }
                '{' => {
                    let _ = self.take_char();
                    nodes.push(MathNode::Group(self.parse_until(Some('}'))));
                }
                '\\' => {
                    let _ = self.take_char();
                    match self.peek_char() {
                        Some(next) if next.is_ascii_alphabetic() => {
                            let name = self.take_control_word();
                            if name == "right" {
                                return (nodes, self.parse_math_delimiter());
                            }
                            nodes.extend(self.parse_named_control_sequence(&name));
                        }
                        Some(_) => nodes.extend(self.parse_control_symbol()),
                        None => {
                            nodes.push(MathNode::Ordinary('\\'));
                            break;
                        }
                    }
                }
                _ => {
                    let _ = self.take_char();
                    nodes.push(MathNode::Ordinary(ch));
                }
            }
        }

        (nodes, ".".to_string())
    }

    fn peek_char(&self) -> Option<char> {
        self.input[self.index..].chars().next()
    }

    fn take_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.index += ch.len_utf8();
        Some(ch)
    }
}

fn eof_line(source: &str) -> u32 {
    1 + source.bytes().filter(|byte| *byte == b'\n').count() as u32
}

#[cfg(test)]
mod tests {
    use super::{
        parse_bbl_input, render_math_nodes_for_anchor, render_math_nodes_for_encoding,
        CaptionEntry, DocumentLabels, DocumentNode, FloatType, FontFamilyRole,
        IncludeGraphicsOptions, IndexRawEntry, LineTag, MathLine, MathNode, MinimalLatexParser,
        OverUnderKind, PackageInfo, ParseError, ParsedDocument, Parser, ParserDriver, SectionEntry,
    };
    use crate::bibliography::api::BibliographyState;
    use crate::compilation::IndexEntry;
    use crate::graphics::api::{GraphicNode, PathSegment, Point};
    use crate::kernel::api::DimensionValue;
    use crate::policy::{
        FileOperationHandler, FileOperationResult, ShellEscapeHandler, ShellEscapeResult,
    };
    use std::collections::BTreeMap;

    struct MockShellEscapeHandler {
        result: ShellEscapeResult,
    }

    impl ShellEscapeHandler for MockShellEscapeHandler {
        fn execute_write18(&self, _command: &str, _line: u32) -> ShellEscapeResult {
            self.result.clone()
        }
    }

    struct MockFileOperationHandler {
        read_result: FileOperationResult,
        write_result: FileOperationResult,
    }

    impl FileOperationHandler for MockFileOperationHandler {
        fn check_open_read(&self, _path: &str, _line: u32) -> FileOperationResult {
            self.read_result.clone()
        }

        fn check_open_write(&self, _path: &str, _line: u32) -> FileOperationResult {
            self.write_result.clone()
        }
    }

    fn parsed_document(body: &str) -> ParsedDocument {
        ParsedDocument {
            document_class: "article".to_string(),
            class_options: Vec::new(),
            loaded_packages: Vec::new(),
            package_count: 0,
            main_font_name: None,
            sans_font_name: None,
            mono_font_name: None,
            body: body.to_string(),
            labels: DocumentLabels::default(),
            bibliography_state: BibliographyState::default(),
            has_unresolved_refs: false,
        }
    }

    fn parse_recovering_with_handlers(
        source_body: &str,
        shell_escape_handler: Option<&dyn ShellEscapeHandler>,
        file_operation_handler: Option<&dyn FileOperationHandler>,
    ) -> super::ParseOutput {
        MinimalLatexParser.parse_recovering_with_context_and_handlers(
            &format!(
                "\\documentclass{{article}}\n\\begin{{document}}\n{source_body}\n\\end{{document}}\n"
            ),
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            BTreeMap::new(),
            None,
            BTreeMap::new(),
            Vec::new(),
            None,
            shell_escape_handler,
            file_operation_handler,
        )
    }

    #[test]
    fn parses_minimal_latex_document() {
        let document = MinimalLatexParser
            .parse("\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\n")
            .expect("parse document");

        assert_eq!(
            document,
            ParsedDocument {
                document_class: "article".to_string(),
                class_options: Vec::new(),
                loaded_packages: Vec::new(),
                package_count: 0,
                main_font_name: None,
                sans_font_name: None,
                mono_font_name: None,
                body: "Hello".to_string(),
                labels: DocumentLabels::default(),
                bibliography_state: BibliographyState::default(),
                has_unresolved_refs: false,
            }
        );
    }

    #[test]
    fn parse_recovering_succeeds_for_valid_document() {
        let output = MinimalLatexParser.parse_recovering(
            "\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\n",
        );

        assert!(output.errors.is_empty());
        assert!(output.document.is_some());
        assert_eq!(output.document.expect("parsed document").body, "Hello");
    }

    #[test]
    fn parse_recovering_reports_multiple_structural_errors() {
        let output = MinimalLatexParser.parse_recovering("some text with no document structure");

        assert_eq!(
            output.errors,
            vec![
                ParseError::MissingBeginDocument { line: 1 },
                ParseError::MissingEndDocument { line: 1 },
                ParseError::MissingDocumentClass,
            ]
        );
        assert!(output.document.is_none());
    }

    #[test]
    fn parse_recovering_recovers_from_unexpected_closing_brace() {
        let output = MinimalLatexParser.parse_recovering(
            "\\documentclass{article}\n\\begin{document}\nA}B\n\\end{document}\n",
        );

        assert_eq!(
            output.errors,
            vec![ParseError::UnexpectedClosingBrace { line: 3 }]
        );
        assert_eq!(
            output.document,
            Some(ParsedDocument {
                document_class: "article".to_string(),
                class_options: Vec::new(),
                loaded_packages: Vec::new(),
                package_count: 0,
                main_font_name: None,
                sans_font_name: None,
                mono_font_name: None,
                body: "AB".to_string(),
                labels: DocumentLabels::default(),
                bibliography_state: BibliographyState::default(),
                has_unresolved_refs: false,
            })
        );
    }

    #[test]
    fn setmainfont_in_preamble_sets_main_font_name() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{TestFont}\n\\begin{document}\nBody\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.main_font_name, Some("TestFont".to_string()));
    }

    #[test]
    fn setsansfont_in_preamble_sets_sans_font_name() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\usepackage{fontspec}\n\\setsansfont{SansFont}\n\\begin{document}\nBody\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.sans_font_name, Some("SansFont".to_string()));
    }

    #[test]
    fn setmonofont_in_preamble_sets_mono_font_name() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmonofont{MonoFont}\n\\begin{document}\nBody\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.mono_font_name, Some("MonoFont".to_string()));
    }

    #[test]
    fn all_three_font_families_in_preamble() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{MainFont}\n\\setsansfont{SansFont}\n\\setmonofont{MonoFont}\n\\begin{document}\nBody\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.main_font_name, Some("MainFont".to_string()));
        assert_eq!(document.sans_font_name, Some("SansFont".to_string()));
        assert_eq!(document.mono_font_name, Some("MonoFont".to_string()));
    }

    #[test]
    fn setmainfont_without_fontspec_emits_warning() {
        let output = MinimalLatexParser.parse_recovering(
            "\\documentclass{article}\n\\setmainfont{TestFont}\n\\begin{document}\nBody\n\\end{document}\n",
        );

        assert_eq!(
            output
                .document
                .as_ref()
                .and_then(|document| document.main_font_name.clone()),
            None
        );
        assert!(output
            .errors
            .contains(&ParseError::FontspecNotLoaded { line: 2 }));
    }

    #[test]
    fn setsansfont_without_fontspec_emits_warning() {
        let output = MinimalLatexParser.parse_recovering(
            "\\documentclass{article}\n\\setsansfont{SansFont}\n\\begin{document}\nBody\n\\end{document}\n",
        );

        assert_eq!(
            output
                .document
                .as_ref()
                .and_then(|document| document.sans_font_name.clone()),
            None
        );
        assert!(output
            .errors
            .contains(&ParseError::FontspecNotLoaded { line: 2 }));
    }

    #[test]
    fn setmainfont_in_body_emits_warning() {
        let output = MinimalLatexParser.parse_recovering(
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\setmainfont{PreambleFont}\n\\begin{document}\n\\setmainfont{BodyFont}\nBody\n\\end{document}\n",
        );

        assert_eq!(
            output
                .document
                .as_ref()
                .and_then(|document| document.main_font_name.clone()),
            Some("PreambleFont".to_string())
        );
        assert!(output
            .errors
            .contains(&ParseError::SetmainfontInBody { line: 5 }));
    }

    #[test]
    fn setsansfont_in_body_emits_warning() {
        let output = MinimalLatexParser.parse_recovering(
            "\\documentclass{article}\n\\usepackage{fontspec}\n\\setsansfont{PreambleSans}\n\\begin{document}\n\\setsansfont{BodySans}\nBody\n\\end{document}\n",
        );

        assert_eq!(
            output
                .document
                .as_ref()
                .and_then(|document| document.sans_font_name.clone()),
            Some("PreambleSans".to_string())
        );
        assert!(output
            .errors
            .contains(&ParseError::SetmainfontInBody { line: 5 }));
    }

    #[test]
    fn no_fontspec_produces_none_main_font_name() {
        let document = MinimalLatexParser
            .parse("\\documentclass{article}\n\\begin{document}\nBody\n\\end{document}\n")
            .expect("parse document");

        assert_eq!(document.main_font_name, None);
        assert_eq!(document.sans_font_name, None);
        assert_eq!(document.mono_font_name, None);
    }

    #[test]
    fn parse_recovering_rejects_trailing_content_after_end_document() {
        let output = MinimalLatexParser.parse_recovering(
            "\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\nTrailing\n",
        );

        assert!(output.document.is_none());
        assert_eq!(
            output.errors,
            vec![ParseError::TrailingContentAfterEndDocument { line: 5 }]
        );
    }

    #[test]
    fn parse_recovering_rejects_unclosed_brace_in_trailing_content() {
        let output = MinimalLatexParser.parse_recovering(
            "\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\n{stuff\n",
        );

        assert!(output.document.is_none());
        assert!(output
            .errors
            .iter()
            .any(|e| matches!(e, ParseError::UnclosedBrace { .. })));
    }

    #[test]
    fn body_nodes_empty() {
        assert!(parsed_document("").body_nodes().is_empty());
    }

    #[test]
    fn body_nodes_single_paragraph() {
        assert_eq!(
            parsed_document("Hello Ferritex").body_nodes(),
            vec![DocumentNode::Text("Hello Ferritex".to_string())]
        );
    }

    #[test]
    fn body_nodes_with_par_break() {
        assert_eq!(
            parsed_document("First paragraph\n\nSecond paragraph").body_nodes(),
            vec![
                DocumentNode::Text("First paragraph".to_string()),
                DocumentNode::ParBreak,
                DocumentNode::Text("Second paragraph".to_string()),
            ]
        );
    }

    #[test]
    fn body_nodes_with_explicit_par_command() {
        assert_eq!(
            parsed_document(r"First paragraph\par Second paragraph").body_nodes(),
            vec![
                DocumentNode::Text("First paragraph".to_string()),
                DocumentNode::ParBreak,
                DocumentNode::Text("Second paragraph".to_string()),
            ]
        );
    }

    fn parse_document(source_body: &str) -> ParsedDocument {
        MinimalLatexParser
            .parse(&format!(
                "\\documentclass{{article}}\n\\begin{{document}}\n{source_body}\n\\end{{document}}\n"
            ))
            .expect("parse document")
    }

    #[test]
    fn pagebreak_parsed_to_page_break_node() {
        assert_eq!(
            parse_document("First\n\\pagebreak\nSecond").body_nodes(),
            vec![
                DocumentNode::Text("First".to_string()),
                DocumentNode::PageBreak,
                DocumentNode::Text("Second".to_string()),
            ]
        );
    }

    #[test]
    fn newpage_parsed_to_page_break_node() {
        assert_eq!(
            parse_document("First\n\\newpage\nSecond").body_nodes(),
            vec![
                DocumentNode::Text("First".to_string()),
                DocumentNode::PageBreak,
                DocumentNode::Text("Second".to_string()),
            ]
        );
    }

    #[test]
    fn clearpage_parsed_to_clear_page_node() {
        assert_eq!(
            parse_document("First\n\\clearpage\nSecond").body_nodes(),
            vec![
                DocumentNode::Text("First".to_string()),
                DocumentNode::ClearPage,
                DocumentNode::Text("Second".to_string()),
            ]
        );
    }

    #[test]
    fn hbox_parsed_to_hbox_node() {
        assert_eq!(
            parse_document(r"\hbox{hello}").body_nodes(),
            vec![DocumentNode::HBox(vec![DocumentNode::Text(
                "hello".to_string()
            )])]
        );
    }

    #[test]
    fn vbox_parsed_to_vbox_node() {
        assert_eq!(
            parse_document(r"\vbox{hello}").body_nodes(),
            vec![DocumentNode::VBox(vec![DocumentNode::Text(
                "hello".to_string()
            )])]
        );
    }

    #[test]
    fn nested_hbox_in_vbox() {
        assert_eq!(
            parse_document(r"\vbox{\hbox{inner}}").body_nodes(),
            vec![DocumentNode::VBox(vec![DocumentNode::HBox(vec![
                DocumentNode::Text("inner".to_string())
            ])])]
        );
    }

    #[test]
    fn hbox_with_empty_content() {
        assert_eq!(
            parse_document(r"\hbox{}").body_nodes(),
            vec![DocumentNode::HBox(vec![])]
        );
    }

    #[test]
    fn textsf_produces_font_family_node() {
        assert_eq!(
            parse_document(r"\textsf{hello}").body_nodes(),
            vec![DocumentNode::FontFamily {
                role: FontFamilyRole::Sans,
                children: vec![DocumentNode::Text("hello".to_string())],
            }]
        );
    }

    #[test]
    fn texttt_produces_font_family_node() {
        assert_eq!(
            parse_document(r"\texttt{code}").body_nodes(),
            vec![DocumentNode::FontFamily {
                role: FontFamilyRole::Mono,
                children: vec![DocumentNode::Text("code".to_string())],
            }]
        );
    }

    #[test]
    fn includegraphics_parsed_to_document_node_with_options() {
        assert_eq!(
            parse_document(r"\includegraphics[width=100pt,height=50pt,scale=1.5]{images/test.png}")
                .body_nodes(),
            vec![DocumentNode::IncludeGraphics {
                path: "images/test.png".to_string(),
                options: IncludeGraphicsOptions {
                    width: Some(DimensionValue(100 * 65_536)),
                    height: Some(DimensionValue(50 * 65_536)),
                    scale: Some(1.5),
                },
            }]
        );
    }

    #[test]
    fn tikzpicture_environment_parses_to_graphics_box() {
        let nodes =
            parse_document(r"\begin{tikzpicture}\draw (0,0) rectangle (2,1);\end{tikzpicture}")
                .body_nodes();

        let [DocumentNode::TikzPicture { graphics_box }] = nodes.as_slice() else {
            panic!("expected single tikzpicture node");
        };
        assert_eq!(
            graphics_box.width,
            DimensionValue((2.0_f64 * 28.3465_f64 * 65_536.0_f64).round() as i64)
        );
        assert_eq!(
            graphics_box.height,
            DimensionValue((28.3465_f64 * 65_536.0_f64).round() as i64)
        );
        assert_eq!(
            graphics_box
                .scene
                .as_ref()
                .map(|scene| scene.nodes.as_slice()),
            Some(
                [GraphicNode::Vector(crate::graphics::api::VectorPrimitive {
                    path: vec![
                        PathSegment::MoveTo(Point { x: 0.0, y: 0.0 }),
                        PathSegment::LineTo(Point {
                            x: 2.0 * 28.3465,
                            y: 0.0,
                        }),
                        PathSegment::LineTo(Point {
                            x: 2.0 * 28.3465,
                            y: 28.3465,
                        }),
                        PathSegment::LineTo(Point { x: 0.0, y: 28.3465 }),
                        PathSegment::ClosePath,
                    ],
                    stroke: Some(crate::graphics::api::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                    }),
                    fill: None,
                    line_width: 0.4,
                    arrows: crate::graphics::api::ArrowSpec::None,
                })]
                .as_slice()
            )
        );
    }

    #[test]
    fn tikzpicture_unsupported_command_surfaces_diagnostic() {
        let output = MinimalLatexParser.parse_recovering(
            "\\documentclass{article}\n\\begin{document}\n\\begin{tikzpicture}\n\\foo (0,0);\n\\end{tikzpicture}\n\\end{document}\n",
        );

        assert!(output.document.is_some());
        assert!(output.errors.contains(&ParseError::TikzDiagnostic {
            line: 3,
            message: "tikz: unsupported command `foo`".to_string(),
        }));
    }

    #[test]
    fn write18_without_handler_emits_warning() {
        let output = MinimalLatexParser.parse_recovering(
            "\\documentclass{article}\n\\begin{document}\n\\write18{echo test}\n\\end{document}\n",
        );

        assert!(output.document.is_some());
        assert!(output.errors.contains(&ParseError::ShellEscapeNotAllowed {
            line: 3,
            command: "echo test".to_string(),
        }));
    }

    #[test]
    fn immediate_write18_without_handler_emits_warning() {
        let output = parse_recovering_with_handlers(r"\immediate\write18{echo test}", None, None);

        assert!(output.document.is_some());
        assert!(output.errors.contains(&ParseError::ShellEscapeNotAllowed {
            line: 3,
            command: "echo test".to_string(),
        }));
    }

    #[test]
    fn write18_with_denied_handler_emits_warning() {
        let handler = MockShellEscapeHandler {
            result: ShellEscapeResult::Denied,
        };

        let output = parse_recovering_with_handlers(r"\write18{echo test}", Some(&handler), None);

        assert!(output.document.is_some());
        assert!(output.errors.contains(&ParseError::ShellEscapeNotAllowed {
            line: 3,
            command: "echo test".to_string(),
        }));
    }

    #[test]
    fn immediate_write18_with_success_handler_emits_no_diagnostic() {
        let handler = MockShellEscapeHandler {
            result: ShellEscapeResult::Success { exit_code: 0 },
        };

        let output =
            parse_recovering_with_handlers(r"\immediate\write18{echo test}", Some(&handler), None);

        assert!(output.document.is_some());
        assert!(!output.errors.iter().any(|error| matches!(
            error,
            ParseError::ShellEscapeNotAllowed { .. } | ParseError::ShellEscapeError { .. }
        )));
    }

    #[test]
    fn write18_with_success_handler_emits_no_diagnostic() {
        let handler = MockShellEscapeHandler {
            result: ShellEscapeResult::Success { exit_code: 0 },
        };

        let output = parse_recovering_with_handlers(r"\write18{echo test}", Some(&handler), None);

        assert!(output.document.is_some());
        assert!(!output.errors.iter().any(|error| matches!(
            error,
            ParseError::ShellEscapeNotAllowed { .. } | ParseError::ShellEscapeError { .. }
        )));
    }

    #[test]
    fn write_space_18_without_handler_emits_warning() {
        let output = parse_recovering_with_handlers(r"\write 18{echo test}", None, None);

        assert!(output.document.is_some());
        assert!(output.errors.contains(&ParseError::ShellEscapeNotAllowed {
            line: 3,
            command: "echo test".to_string(),
        }));
    }

    #[test]
    fn immediate_write_space_18_without_handler_emits_warning() {
        let output =
            parse_recovering_with_handlers(r"\immediate\write 18{echo test}", None, None);

        assert!(output.document.is_some());
        assert!(output.errors.contains(&ParseError::ShellEscapeNotAllowed {
            line: 3,
            command: "echo test".to_string(),
        }));
    }

    #[test]
    fn openin_denied_handler_emits_diagnostic() {
        let handler = MockFileOperationHandler {
            read_result: FileOperationResult::Denied {
                path: "/tmp/secret.tex".to_string(),
                reason: "outside allowed read/write roots".to_string(),
            },
            write_result: FileOperationResult::Allowed,
        };

        let output =
            parse_recovering_with_handlers(r"\openin1=/tmp/secret.tex", None, Some(&handler));

        assert!(output.document.is_some());
        assert!(output.errors.contains(&ParseError::FileOperationDenied {
            line: 3,
            operation: super::FileOperationKind::OpenIn,
            path: "/tmp/secret.tex".to_string(),
            reason: "outside allowed read/write roots".to_string(),
        }));
    }

    #[test]
    fn openout_denied_handler_emits_diagnostic() {
        let handler = MockFileOperationHandler {
            read_result: FileOperationResult::Allowed,
            write_result: FileOperationResult::Denied {
                path: "/tmp/output.tex".to_string(),
                reason: "outside allowed read/write roots".to_string(),
            },
        };

        let output =
            parse_recovering_with_handlers(r"\openout1=/tmp/output.tex", None, Some(&handler));

        assert!(output.document.is_some());
        assert!(output.errors.contains(&ParseError::FileOperationDenied {
            line: 3,
            operation: super::FileOperationKind::OpenOut,
            path: "/tmp/output.tex".to_string(),
            reason: "outside allowed read/write roots".to_string(),
        }));
    }

    #[test]
    fn openin_and_openout_allowed_emit_no_diagnostic() {
        let handler = MockFileOperationHandler {
            read_result: FileOperationResult::Allowed,
            write_result: FileOperationResult::Allowed,
        };

        let output = parse_recovering_with_handlers(
            "\\openin1=allowed.tex\n\\openout1=allowed.out",
            None,
            Some(&handler),
        );

        assert!(output.document.is_some());
        assert!(!output
            .errors
            .iter()
            .any(|error| matches!(error, ParseError::FileOperationDenied { .. })));
    }

    #[test]
    fn parses_inline_math_to_math_nodes() {
        assert_eq!(
            parse_document(r"$x^2$").body_nodes(),
            vec![DocumentNode::InlineMath(vec![
                MathNode::Ordinary('x'),
                MathNode::Superscript(Box::new(MathNode::Ordinary('2'))),
            ])]
        );
    }

    #[test]
    fn parses_display_math_to_math_nodes() {
        assert_eq!(
            parse_document(r"\[a_1\]").body_nodes(),
            vec![DocumentNode::DisplayMath(vec![
                MathNode::Ordinary('a'),
                MathNode::Subscript(Box::new(MathNode::Ordinary('1'))),
            ])]
        );
    }

    #[test]
    fn parses_frac_math_content() {
        assert_eq!(
            parse_document(r"$\frac{a}{b}$").body_nodes(),
            vec![DocumentNode::InlineMath(vec![MathNode::Frac {
                numer: vec![MathNode::Ordinary('a')],
                denom: vec![MathNode::Ordinary('b')],
            }])]
        );
    }

    #[test]
    fn parses_text_command_inside_math() {
        assert_eq!(
            parse_document(r"$x+\text{units}$").body_nodes(),
            vec![DocumentNode::InlineMath(vec![
                MathNode::Ordinary('x'),
                MathNode::Ordinary('+'),
                MathNode::Text("units".to_string()),
            ])]
        );
    }

    #[test]
    fn parses_named_symbol_commands_inside_math() {
        assert_eq!(
            parse_document(r"$\alpha\leq\beta\rightarrow\Gamma\quad\infty$").body_nodes(),
            vec![DocumentNode::InlineMath(vec![
                MathNode::Symbol("α".to_string()),
                MathNode::Symbol("≤".to_string()),
                MathNode::Symbol("β".to_string()),
                MathNode::Symbol("→".to_string()),
                MathNode::Symbol("Γ".to_string()),
                MathNode::Symbol("\u{2003}".to_string()),
                MathNode::Symbol("∞".to_string()),
            ])]
        );
    }

    #[test]
    fn parses_sqrt_with_and_without_index() {
        assert_eq!(
            parse_document(r"$\sqrt{x}+\sqrt[3]{y}$").body_nodes(),
            vec![DocumentNode::InlineMath(vec![
                MathNode::Sqrt {
                    radicand: vec![MathNode::Ordinary('x')],
                    index: None,
                },
                MathNode::Ordinary('+'),
                MathNode::Sqrt {
                    radicand: vec![MathNode::Ordinary('y')],
                    index: Some(vec![MathNode::Ordinary('3')]),
                },
            ])]
        );
    }

    #[test]
    fn parses_math_font_left_right_and_over_under_commands() {
        assert_eq!(
            parse_document(r"$\mathrm{sin}\left(\alpha\right)+\overset{*}{X}+\underset{n}{Y}$")
                .body_nodes(),
            vec![DocumentNode::InlineMath(vec![
                MathNode::MathFont {
                    cmd: "mathrm".to_string(),
                    body: vec![
                        MathNode::Ordinary('s'),
                        MathNode::Ordinary('i'),
                        MathNode::Ordinary('n'),
                    ],
                },
                MathNode::LeftRight {
                    left: "(".to_string(),
                    right: ")".to_string(),
                    body: vec![MathNode::Symbol("α".to_string())],
                },
                MathNode::Ordinary('+'),
                MathNode::OverUnder {
                    kind: OverUnderKind::Over,
                    base: vec![MathNode::Ordinary('X')],
                    annotation: vec![MathNode::Ordinary('*')],
                },
                MathNode::Ordinary('+'),
                MathNode::OverUnder {
                    kind: OverUnderKind::Under,
                    base: vec![MathNode::Ordinary('Y')],
                    annotation: vec![MathNode::Ordinary('n')],
                },
            ])]
        );
    }

    #[test]
    fn parses_unknown_control_sequence_as_literal_characters() {
        assert_eq!(
            parse_document(r"$\mystery$").body_nodes(),
            vec![DocumentNode::InlineMath(vec![
                MathNode::Ordinary('m'),
                MathNode::Ordinary('y'),
                MathNode::Ordinary('s'),
                MathNode::Ordinary('t'),
                MathNode::Ordinary('e'),
                MathNode::Ordinary('r'),
                MathNode::Ordinary('y'),
            ])]
        );
    }

    #[test]
    fn parses_equation_environment_with_auto_number_and_label() {
        let document = parse_document(
            "\\begin{equation}a=b\\label{eq:test}\\end{equation}\nSee \\ref{eq:test}.",
        );

        let nodes = document.body_nodes();
        assert_eq!(
            nodes.first(),
            Some(&DocumentNode::EquationEnv {
                lines: vec![MathLine {
                    segments: vec![vec![
                        MathNode::Ordinary('a'),
                        MathNode::Ordinary('='),
                        MathNode::Ordinary('b'),
                    ]],
                    tag: LineTag::Auto,
                    display_tag: Some("1".to_string()),
                }],
                numbered: true,
                aligned: false,
            })
        );
        assert_eq!(nodes.get(1), Some(&DocumentNode::Text(" See ".to_string())));
        assert_eq!(
            nodes.get(2),
            Some(&DocumentNode::Link {
                url: "#eq:test".to_string(),
                children: vec![DocumentNode::Text("1".to_string())],
            })
        );
        assert_eq!(nodes.get(3), Some(&DocumentNode::Text(".".to_string())));
        assert_eq!(
            document.labels.get("eq:test").map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn parses_align_environment_with_notag_and_custom_tag() {
        let document = parse_document(
            "\\begin{align}a&=&b\\notag\\\\c&=&\\text{done}\\tag{A}\\label{eq:done}\\end{align}",
        );

        assert_eq!(
            document.body_nodes(),
            vec![DocumentNode::EquationEnv {
                lines: vec![
                    MathLine {
                        segments: vec![
                            vec![MathNode::Ordinary('a')],
                            vec![MathNode::Ordinary('=')],
                            vec![MathNode::Ordinary('b')],
                        ],
                        tag: LineTag::Notag,
                        display_tag: None,
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
            }]
        );
        assert_eq!(
            document.labels.get("eq:done").map(String::as_str),
            Some("A")
        );
    }

    #[test]
    fn parses_equation_star_as_unnumbered_environment() {
        assert_eq!(
            parse_document(r"\begin{equation*}x=y\end{equation*}").body_nodes(),
            vec![DocumentNode::EquationEnv {
                lines: vec![MathLine {
                    segments: vec![vec![
                        MathNode::Ordinary('x'),
                        MathNode::Ordinary('='),
                        MathNode::Ordinary('y'),
                    ]],
                    tag: LineTag::Auto,
                    display_tag: None,
                }],
                numbered: false,
                aligned: false,
            }]
        );
    }

    #[test]
    fn renders_extended_math_nodes_for_anchor_and_encoding() {
        let nodes = vec![
            MathNode::LeftRight {
                left: ".".to_string(),
                right: "⟩".to_string(),
                body: vec![
                    MathNode::MathFont {
                        cmd: "mathbf".to_string(),
                        body: vec![MathNode::Symbol("α".to_string())],
                    },
                    MathNode::OverUnder {
                        kind: OverUnderKind::Over,
                        base: vec![MathNode::Ordinary('X')],
                        annotation: vec![MathNode::Ordinary('n')],
                    },
                ],
            },
            MathNode::Sqrt {
                radicand: vec![MathNode::Ordinary('y')],
                index: Some(vec![MathNode::Ordinary('3')]),
            },
        ];

        assert_eq!(render_math_nodes_for_anchor(&nodes), "αX^n⟩√[3](y)");
        assert_eq!(
            render_math_nodes_for_encoding(&nodes),
            r"\left.\mathbf{α}\overset{n}{X}\right\rangle\sqrt[3]{y}"
        );
    }

    #[test]
    fn parses_figure_environment_with_caption_label_and_includegraphics() {
        let document = parse_document(
            "\\begin{figure}[h]\\includegraphics{img.png}\\caption{A figure}\\label{fig:test}\\end{figure}",
        );

        assert_eq!(
            document.body_nodes(),
            vec![DocumentNode::Float {
                float_type: FloatType::Figure,
                specifier: Some("h".to_string()),
                content: vec![DocumentNode::IncludeGraphics {
                    path: "img.png".to_string(),
                    options: IncludeGraphicsOptions::default(),
                }],
                caption: Some("A figure".to_string()),
                label: Some("fig:test".to_string()),
            }]
        );
        assert_eq!(
            document.labels.get("fig:test").map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn parses_table_environment_as_table_float() {
        let document = parse_document("\\begin{table}\\caption{A table}\\end{table}");

        assert_eq!(
            document.body_nodes(),
            vec![DocumentNode::Float {
                float_type: FloatType::Table,
                specifier: None,
                content: Vec::new(),
                caption: Some("A table".to_string()),
                label: None,
            }]
        );
    }

    #[test]
    fn figure_label_resolves_for_following_ref() {
        let document = parse_document(
            "\\begin{figure}\\caption{Fig}\\label{fig:1}\\end{figure}See \\ref{fig:1}.",
        );

        assert_eq!(
            document.body_nodes(),
            vec![
                DocumentNode::Float {
                    float_type: FloatType::Figure,
                    specifier: None,
                    content: Vec::new(),
                    caption: Some("Fig".to_string()),
                    label: Some("fig:1".to_string()),
                },
                DocumentNode::ParBreak,
                DocumentNode::Text("See ".to_string()),
                DocumentNode::Link {
                    url: "#fig:1".to_string(),
                    children: vec![DocumentNode::Text("1".to_string())],
                },
                DocumentNode::Text(".".to_string()),
            ]
        );
        assert_eq!(document.labels.get("fig:1").map(String::as_str), Some("1"));
        assert!(!document.has_unresolved_refs);
    }

    #[test]
    fn specifier_preserved_in_float_node() {
        let document = parse_document("\\begin{figure}[htbp!]Body\\end{figure}");

        assert_eq!(
            document.body_nodes(),
            vec![DocumentNode::Float {
                float_type: FloatType::Figure,
                specifier: Some("htbp!".to_string()),
                content: vec![DocumentNode::Text("Body".to_string())],
                caption: None,
                label: None,
            }]
        );
    }

    #[test]
    fn figure_and_table_counters_increment_independently() {
        let document = parse_document(
            "\\begin{figure}\\label{fig:first}\\end{figure}\n\\begin{table}\\label{tab:first}\\end{table}\n\\begin{figure}\\label{fig:second}\\end{figure}\n\\begin{table}\\label{tab:second}\\end{table}",
        );

        assert_eq!(
            document.labels,
            BTreeMap::from([
                ("fig:first".to_string(), "1".to_string()),
                ("fig:second".to_string(), "2".to_string()),
                ("tab:first".to_string(), "1".to_string()),
                ("tab:second".to_string(), "2".to_string()),
            ])
        );
    }

    #[test]
    fn counts_preamble_packages() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass[11pt]{report}\n\\usepackage{amsmath}\n% \\usepackage{commented}\n\\usepackage{hyperref}\n\\begin{document}\nBody\n\\end{document}",
            )
            .expect("parse document");

        assert_eq!(document.document_class, "report");
        assert_eq!(document.class_options, vec!["11pt".to_string()]);
        assert_eq!(
            document.loaded_packages,
            vec![
                PackageInfo {
                    name: "amsmath".to_string(),
                    options: Vec::new(),
                },
                PackageInfo {
                    name: "hyperref".to_string(),
                    options: Vec::new(),
                },
            ]
        );
        assert_eq!(document.package_count, 2);
    }

    #[test]
    fn duplicate_package_load_is_a_no_op() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\usepackage{graphicx}\n\\usepackage{graphicx}\n\\begin{document}\nBody\n\\end{document}",
            )
            .expect("parse document");

        assert_eq!(
            document.loaded_packages,
            vec![PackageInfo {
                name: "graphicx".to_string(),
                options: Vec::new(),
            }]
        );
        assert_eq!(document.package_count, 1);
    }

    #[test]
    fn usepackage_supports_comma_separated_names_and_requirepackage() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\usepackage[dvipsnames]{xcolor,graphicx}\n\\RequirePackage{geometry}\n\\begin{document}\nBody\n\\end{document}",
            )
            .expect("parse document");

        assert_eq!(
            document.loaded_packages,
            vec![
                PackageInfo {
                    name: "xcolor".to_string(),
                    options: vec!["dvipsnames".to_string()],
                },
                PackageInfo {
                    name: "graphicx".to_string(),
                    options: vec!["dvipsnames".to_string()],
                },
                PackageInfo {
                    name: "geometry".to_string(),
                    options: Vec::new(),
                },
            ]
        );
        assert_eq!(document.package_count, 3);
    }

    #[test]
    fn report_class_enables_chapter_numbering() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{report}\n\\begin{document}\n\\chapter{Intro}\n\\section{Scope}\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "1 Intro\n\n1.1 Scope");
    }

    #[test]
    fn xcolor_stub_preserves_textcolor_body_content() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\usepackage{xcolor}\n\\begin{document}\n\\textcolor{red}{Alert}\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "Alert");
    }

    #[test]
    fn rejects_missing_document_environment() {
        let error = MinimalLatexParser
            .parse("\\documentclass{article}\nHello\n")
            .expect_err("parse should fail");

        assert_eq!(error, ParseError::MissingBeginDocument { line: 3 });
    }

    #[test]
    fn rejects_unbalanced_braces_with_line_information() {
        let error = MinimalLatexParser
            .parse("\\documentclass{article}\n\\begin{document}\n{text\n\\end{document}\n")
            .expect_err("parse should fail");

        assert_eq!(error, ParseError::UnclosedBrace { line: 3 });
    }

    #[test]
    fn ignores_commented_control_sequences_when_validating_structure() {
        let error = MinimalLatexParser
            .parse("% \\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\n")
            .expect_err("parse should fail");

        assert_eq!(error, ParseError::MissingDocumentClass);
    }

    #[test]
    fn rejects_end_document_before_begin_document() {
        let error = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\end{document}\n\\begin{document}\nHello\n\\end{document}\n",
            )
            .expect_err("parse should fail");

        assert_eq!(error, ParseError::UnexpectedEndDocument { line: 2 });
    }

    #[test]
    fn rejects_trailing_content_after_end_document() {
        let error = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\nHello\n\\end{document}\nTrailing\n",
            )
            .expect_err("parse should fail");

        assert_eq!(
            error,
            ParseError::TrailingContentAfterEndDocument { line: 5 }
        );
    }

    #[test]
    fn rejects_document_class_with_control_characters() {
        let error = MinimalLatexParser
            .parse("\\documentclass{arti\ncle}\n\\begin{document}\nHello\n\\end{document}\n")
            .expect_err("parse should fail");

        assert_eq!(error, ParseError::InvalidDocumentClass { line: 1 });
    }

    #[test]
    fn expands_def_macro_in_body() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\hello{Hello, Ferritex!}\n\\begin{document}\n\\hello\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "Hello, Ferritex!");
    }

    #[test]
    fn expands_newcommand_with_argument() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\newcommand{\\hello}[1]{Hello #1}\n\\begin{document}\n\\hello{Ferritex}\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "Hello Ferritex");
    }

    #[test]
    fn expands_def_macro_with_three_arguments() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\triple#1#2#3{#1-#2-#3}\n\\begin{document}\n\\triple{A}{B}{C}\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "A-B-C");
    }

    #[test]
    fn expands_newcommand_with_five_arguments() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\newcommand{\\hello}[5]{#1#2#3#4#5}\n\\begin{document}\n\\hello{A}{B}{C}{D}{E}\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "ABCDE");
    }

    #[test]
    fn expands_def_macro_with_nine_arguments() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\nine#1#2#3#4#5#6#7#8#9{#9#1#5}\n\\begin{document}\n\\nine123456789\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "915");
    }

    #[test]
    fn group_scoped_macro_rolls_back_after_group() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n{\\def\\hello{local }\\hello}\\hello\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "local \\hello");
    }

    #[test]
    fn catcode_changes_affect_tokenization() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\catcode`\\@=11\n\\def\\make@title{Catcode Works}\n\\begin{document}\n\\make@title\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "Catcode Works");
    }

    #[test]
    fn gdef_persists_after_group_close() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n{\\gdef\\hello{global }\\hello}\\hello\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "global global");
    }

    #[test]
    fn let_copies_macro_definition() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\foo{hello}\n\\begin{document}\n\\let\\bar=\\foo\\bar\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "hello");
    }

    #[test]
    fn let_with_char_token() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\let\\star=*\\star\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "*");
    }

    #[test]
    fn let_respects_group_scope() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\foo{hello}\n\\begin{document}\n{\\let\\bar=\\foo}\\bar\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "\\bar");
    }

    #[test]
    fn edef_expands_body_at_definition_time() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\x{world}\n\\edef\\y{hello \\x}\n\\def\\x{changed}\n\\begin{document}\n\\y\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "hello world");
    }

    #[test]
    fn edef_respects_noexpand() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\x{world}\n\\edef\\y{\\noexpand\\x}\n\\def\\x{changed}\n\\begin{document}\n\\y\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "changed");
    }

    #[test]
    fn edef_expands_the_at_definition_time() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\count0=1\n\\edef\\foo{\\the\\count0}\n\\count0=2\n\\begin{document}\n\\foo\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "1");
    }

    #[test]
    fn edef_expands_csname_at_definition_time() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\bar{old}\n\\def\\x{bar}\n\\edef\\y{\\csname \\x\\endcsname}\n\\def\\bar{new}\n\\begin{document}\n\\y\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "old");
    }

    #[test]
    fn rejects_recursive_macro_expansion() {
        let error = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\loop{\\loop}\n\\begin{document}\n\\loop\n\\end{document}\n",
            )
            .expect_err("parse should fail");

        assert_eq!(error, ParseError::MacroExpansionLimit { line: 2 });
    }

    #[test]
    fn rejects_recursive_csname_expansion() {
        let error = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\x{\\csname x\\endcsname}\n\\begin{document}\n\\x\n\\end{document}\n",
            )
            .expect_err("parse should fail");

        assert!(matches!(error, ParseError::MacroExpansionLimit { .. }));
    }

    #[test]
    fn expands_the_for_count_register() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\count0=42 \\the\\count0\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "42");
    }

    #[test]
    fn expands_the_for_dimen_register() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\dimen0=2pt \\the\\dimen0\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "2.0pt");
    }

    #[test]
    fn expands_the_for_toks_register() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\toks0={hello}\\the\\toks0\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "hello");
    }

    #[test]
    fn the_toks_in_edef() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\toks0={alpha}\n\\edef\\foo{\\the\\toks0}\n\\toks0={beta}\n\\begin{document}\n\\foo\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "alpha");
    }

    #[test]
    fn iftrue_keeps_true_branch() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\iftrue visible\\else hidden\\fi\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "visible");
    }

    #[test]
    fn iffalse_keeps_else_branch() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\iffalse hidden\\else visible\\fi\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "visible");
    }

    #[test]
    fn ifnum_compares_register_values() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\count0=1 \\ifnum\\count0>0 positive\\fi\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "positive");
    }

    #[test]
    fn if_compares_character_codes() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\if aa same\\else diff\\fi/\\if ab same\\else diff\\fi\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "same/diff");
    }

    #[test]
    fn ifcat_compares_character_categories() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\ifcat aa same\\else diff\\fi/\\ifcat a1 same\\else diff\\fi\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "same/diff");
    }

    #[test]
    fn ifdim_compares_dimensions() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\ifdim1pt<2pt L\\else X\\fi\\ifdim2pt=2pt E\\else X\\fi\\ifdim3pt>2pt G\\else X\\fi\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "L E G");
    }

    #[test]
    fn ifx_compares_macro_meaning() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\foo{same}\n\\def\\bar{same}\n\\begin{document}\n\\ifx\\foo\\bar yes\\else no\\fi\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "yes");
    }

    #[test]
    fn count_register_rolls_back_after_group() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\count0=5 {\\count0=10}\\the\\count0\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "5");
    }

    #[test]
    fn global_count_assignment_persists_after_group() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n{\\global\\count0=99}\\the\\count0\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "99");
    }

    #[test]
    fn skip_register_supports_scope_global_arithmetic_and_the() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\skip0=1pt{\\skip0=4pt}\\the\\skip0/{\\global\\skip0=2pt}\\the\\skip0/\\advance\\skip0 by 1pt\\multiply\\skip0 by 3\\divide\\skip0 by 2\\the\\skip0\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "1.0pt/2.0pt/4.5pt");
    }

    #[test]
    fn muskip_register_supports_scope_global_arithmetic_and_the() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\muskip0=2pt{\\muskip0=5pt}\\the\\muskip0/{\\global\\muskip0=3pt}\\the\\muskip0/\\advance\\muskip0 by 1pt\\multiply\\muskip0 by 2\\divide\\muskip0 by 4\\the\\muskip0\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "2.0pt/3.0pt/2.0pt");
    }

    #[test]
    fn global_prefix_does_not_leak_past_macro_usage() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\noop{}\n\\begin{document}\n\\count0=1{\\global\\noop\\count0=2}\\the\\count0\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "1");
    }

    #[test]
    fn global_prefix_applies_to_definitions() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n{\\global\\def\\hello{global }\\hello}\\hello\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "global global");
    }

    #[test]
    fn expandafter_expands_second_token() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\name{\\bar}\n\\begin{document}\n\\expandafter\\def\\name{X}\\bar\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "X");
    }

    #[test]
    fn expandafter_def_csname() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\expandafter\\def\\csname foo\\endcsname{X}\n\\begin{document}\n\\foo\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "X");
    }

    #[test]
    fn advance_multiply_and_divide_update_registers() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\count0=1\\advance\\count0 by 3\\multiply\\count0 by 2\\divide\\count0 by 4\\the\\count0\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "2");
    }

    #[test]
    fn newcount_allocates_count_aliases() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\newcount\\foo\n\\newcount\\bar\n\\begin{document}\n\\foo=7\\bar=11\\the\\count10/\\the\\count11/\\the\\foo/\\the\\bar\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "7/11/7/11");
    }

    #[test]
    fn newtoks_allocates_and_aliases() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\newtoks\\foo\n\\newtoks\\bar\n\\begin{document}\n\\foo={left}\\bar={right}\\the\\toks10/\\the\\toks11/\\the\\foo/\\the\\bar\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "left/right/left/right");
    }

    #[test]
    fn countdef_creates_named_count_aliases() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\countdef\\foo=12\n\\begin{document}\n\\foo=9\\advance\\foo by 1\\the\\foo/\\the\\count12\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "10/10");
    }

    #[test]
    fn toksdef_creates_alias() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\toksdef\\foo=12\n\\begin{document}\n\\foo={value}\\the\\foo/\\the\\toks12\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "value/value");
    }

    #[test]
    fn the_toks_empty_register() {
        let document = MinimalLatexParser
            .parse("\\documentclass{article}\n\\begin{document}\nA\\the\\toks0B\n\\end{document}\n")
            .expect("parse document");

        assert_eq!(document.body, "AB");
    }

    #[test]
    fn toks_scope_with_the() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\toks0={outer}{\\toks0={inner}}\\the\\toks0\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "outer");
    }

    #[test]
    fn expands_the_for_newtoks_alias() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\newtoks\\mytoks\n\\begin{document}\n\\mytoks={alias}\\the\\mytoks\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "alias");
    }

    #[test]
    fn count_assignment_accepts_missing_equals() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\count0 42\\the\\count0\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "42");
    }

    #[test]
    fn advance_accepts_missing_by_keyword() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\count0=1\\advance\\count0 4\\the\\count0\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "5");
    }

    #[test]
    fn ifnum_accepts_multiple_signs() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\ifnum--1=1 yes\\else no\\fi\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "yes");
    }

    #[test]
    fn ifcase_selects_case_zero() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\ifcase0 zero\\or one\\or two\\fi\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "zero");
    }

    #[test]
    fn ifcase_selects_case_one() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\ifcase1 zero\\or one\\or two\\fi\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "one");
    }

    #[test]
    fn or_in_simple_conditional_does_not_leak_into_output() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\iftrue left\\or right\\fi\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "leftright");
    }

    #[test]
    fn iffalse_false_branch_ignores_if_prefixed_control_words() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\iffalse \\ifmycondition hidden\\else visible\\fi\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "visible");
    }

    #[test]
    fn nested_conditionals_render_inner_and_outer_content() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\n\\iftrue\\iftrue inner\\fi{} outer\\fi\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "inner outer");
    }

    #[test]
    fn begingroup_endgroup_scope() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\foo{outer }\n\\begin{document}\n\\count0=1\\begingroup\\def\\foo{inner }\\count0=2\\foo\\endgroup\\foo\\the\\count0\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "inner outer 1");
    }

    #[test]
    fn csname_builds_dynamic_control_sequence() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\o{o}\n\\def\\foo{made}\n\\begin{document}\n\\csname fo\\o\\endcsname\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "made");
    }

    #[test]
    fn csname_expands_the_primitive() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\count0=1\n\\expandafter\\def\\csname foo1\\endcsname{made}\n\\begin{document}\n\\csname foo\\the\\count0\\endcsname\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "made");
    }

    #[test]
    fn rejects_unclosed_conditional() {
        let error = MinimalLatexParser
            .parse("\\documentclass{article}\n\\begin{document}\n\\iftrue open\n\\end{document}\n")
            .expect_err("parse should fail");

        assert_eq!(error, ParseError::UnclosedConditional { line: 3 });
    }

    #[test]
    fn section_command_emits_numbered_title() {
        let document = parse_document("\\section{Introduction}");

        assert_eq!(document.body, "1 Introduction");
    }

    #[test]
    fn subsection_command_uses_parent_section_number() {
        let document = parse_document("\\section{Intro}\n\\subsection{Scope}");

        assert_eq!(document.body, "1 Intro\n\n1.1 Scope");
    }

    #[test]
    fn section_counters_reset_for_new_parent_sections() {
        let document =
            parse_document("\\section{One}\n\\subsection{A}\n\\section{Two}\n\\subsection{B}");

        assert_eq!(document.body, "1 One\n\n1.1 A\n\n2 Two\n\n2.1 B");
    }

    #[test]
    fn label_and_ref_resolve_within_same_file() {
        let document = parse_document("\\section{Intro}\\label{sec:intro}\nSee \\ref{sec:intro}.");

        assert_eq!(
            document.body_nodes(),
            vec![
                DocumentNode::Text("1 Intro".to_string()),
                DocumentNode::ParBreak,
                DocumentNode::Text("See ".to_string()),
                DocumentNode::Link {
                    url: "#sec:intro".to_string(),
                    children: vec![DocumentNode::Text("1".to_string())],
                },
                DocumentNode::Text(".".to_string()),
            ]
        );
        assert_eq!(
            document.labels,
            BTreeMap::from([("sec:intro".to_string(), "1".to_string())])
        );
        assert!(!document.has_unresolved_refs);
    }

    #[test]
    fn unresolved_ref_emits_placeholder() {
        let document = parse_document("See \\ref{missing}.");

        assert_eq!(document.body, "See ??.");
        assert!(document.has_unresolved_refs);
    }

    #[test]
    fn unresolved_pageref_emits_body_marker() {
        let document = parse_document("See page \\pageref{sec:later}.");

        assert_eq!(
            document.body,
            format!(
                "See page {}sec:later{}.",
                super::BODY_PAGEREF_START,
                super::BODY_PAGEREF_END
            )
        );
        assert!(document.has_pageref_markers());
        assert!(document.has_unresolved_refs);
    }

    #[test]
    fn pageref_resolves_when_page_labels_are_provided() {
        let document = MinimalLatexParser
            .parse_with_context(
                "\\documentclass{article}\n\\begin{document}\nSee page \\pageref{sec:later}.\n\\end{document}\n",
                BTreeMap::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                BTreeMap::new(),
                None,
                BTreeMap::from([("sec:later".to_string(), 5)]),
                Vec::new(),
            )
            .expect("parse document");

        assert_eq!(
            document.body_nodes(),
            vec![
                DocumentNode::Text("See page ".to_string()),
                DocumentNode::Link {
                    url: "#sec:later".to_string(),
                    children: vec![DocumentNode::Text("5".to_string())],
                },
                DocumentNode::Text(".".to_string()),
            ]
        );
        assert!(!document.has_pageref_markers());
        assert!(!document.has_unresolved_refs);
    }

    #[test]
    fn cite_resolves_when_bibitem_is_available() {
        let document = parse_document(
            "\\begin{thebibliography}{99}\\bibitem{key} Reference text\\end{thebibliography}\nSee \\cite{key}.",
        );

        assert_eq!(document.citations, vec!["key".to_string()]);
        assert_eq!(
            document.bibliography.get("key").map(String::as_str),
            Some("Reference text")
        );
        assert!(document.body_nodes().iter().any(|node| matches!(
            node,
            DocumentNode::Link { url, children }
                if url == "#bib:key"
                    && children == &vec![DocumentNode::Text("1".to_string())]
        )));
    }

    #[test]
    fn cite_supports_multiple_keys() {
        let bibliography_state = parse_bbl_input(
            "\\begin{thebibliography}{99}\\bibitem{a} Alpha\\bibitem{b} Beta\\end{thebibliography}",
        );
        let document = MinimalLatexParser
            .parse_with_context(
                "\\documentclass{article}\n\\begin{document}\nSee \\cite{a,b}.\n\\end{document}\n",
                BTreeMap::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                BTreeMap::new(),
                Some(bibliography_state),
                BTreeMap::new(),
                Vec::new(),
            )
            .expect("parse document");

        assert_eq!(
            document.body_nodes(),
            vec![
                DocumentNode::Text("See [".to_string()),
                DocumentNode::Link {
                    url: "#bib:a".to_string(),
                    children: vec![DocumentNode::Text("1".to_string())],
                },
                DocumentNode::Text(", ".to_string()),
                DocumentNode::Link {
                    url: "#bib:b".to_string(),
                    children: vec![DocumentNode::Text("2".to_string())],
                },
                DocumentNode::Text("].".to_string()),
            ]
        );
        assert_eq!(document.citations, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn bibitem_with_optional_label_resolves_correctly() {
        let document = parse_document(
            "\\begin{thebibliography}{99}\\bibitem[Knu84]{knuth} Donald Knuth\\end{thebibliography}\nSee \\cite{knuth}.",
        );

        assert!(document.body.contains("[1] Donald Knuth"));
        assert!(document
            .body_nodes()
            .iter()
            .any(|node| matches!(node, DocumentNode::Link { url, .. } if url == "#bib:knuth")));
        assert!(!document.has_unresolved_refs);
        assert_eq!(document.citations, vec!["knuth".to_string()]);
        assert_eq!(
            document.bibliography.get("knuth").map(String::as_str),
            Some("Donald Knuth")
        );
    }

    #[test]
    fn unresolved_cite_emits_question_mark_placeholder() {
        let document = parse_document("See \\cite{missing}.");

        assert_eq!(document.body, "See [?].");
        assert!(document.has_unresolved_refs);
    }

    #[test]
    fn cite_resolves_through_bibliography_state() {
        let bibliography_state = parse_bbl_input(
            "\\begin{thebibliography}{99}\\bibitem{knuth} Donald Knuth\\end{thebibliography}",
        );
        let document = MinimalLatexParser
            .parse_with_context(
                "\\documentclass{article}\n\\begin{document}\nSee \\cite{knuth}.\n\\end{document}\n",
                BTreeMap::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                BTreeMap::new(),
                Some(bibliography_state),
                BTreeMap::new(),
                Vec::new(),
            )
            .expect("parse document");

        assert_eq!(
            document.body_nodes(),
            vec![
                DocumentNode::Text("See [".to_string()),
                DocumentNode::Link {
                    url: "#bib:knuth".to_string(),
                    children: vec![DocumentNode::Text("1".to_string())],
                },
                DocumentNode::Text("].".to_string()),
            ]
        );
        assert_eq!(document.citations, vec!["knuth".to_string()]);
    }

    #[test]
    fn bibliography_state_separate_from_labels() {
        let bibliography_state = parse_bbl_input(
            "\\begin{thebibliography}{99}\\bibitem{shared} Shared Reference\\end{thebibliography}",
        );
        let document = MinimalLatexParser
            .parse_with_context(
                "\\documentclass{article}\n\\begin{document}\nRef \\ref{shared}; cite \\cite{shared}.\n\\end{document}\n",
                BTreeMap::from([("shared".to_string(), "42".to_string())]),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                BTreeMap::new(),
                Some(bibliography_state),
                BTreeMap::new(),
                Vec::new(),
            )
            .expect("parse document");

        assert_eq!(
            document.body_nodes(),
            vec![
                DocumentNode::Text("Ref ".to_string()),
                DocumentNode::Link {
                    url: "#shared".to_string(),
                    children: vec![DocumentNode::Text("42".to_string())],
                },
                DocumentNode::Text("; cite [".to_string()),
                DocumentNode::Link {
                    url: "#bib:shared".to_string(),
                    children: vec![DocumentNode::Text("1".to_string())],
                },
                DocumentNode::Text("].".to_string()),
            ]
        );
        assert_eq!(
            document.labels.get("shared").map(String::as_str),
            Some("42")
        );
        assert_eq!(
            document
                .bibliography_state
                .resolve_citation("shared")
                .map(|citation| citation.formatted_text.as_str()),
            Some("1")
        );
    }

    #[test]
    fn thebibliography_collects_multiple_entries() {
        let document = parse_document(
            "\\begin{thebibliography}{99}\\bibitem{a} Alpha\\bibitem{b} Beta\\end{thebibliography}",
        );

        assert_eq!(
            document.bibliography,
            BTreeMap::from([
                ("a".to_string(), "Alpha".to_string()),
                ("b".to_string(), "Beta".to_string()),
            ])
        );
        assert!(document.body.contains("[1] Alpha"));
        assert!(document.body.contains("[2] Beta"));
    }

    #[test]
    fn bibliography_command_renders_preparsed_bbl_entries() {
        let bibliography_state = parse_bbl_input(
            "\\begin{thebibliography}{99}\\bibitem{key} Reference text\\end{thebibliography}",
        );
        let document = MinimalLatexParser
            .parse_with_context(
                "\\documentclass{article}\n\\begin{document}\n\\bibliography{refs}\n\\end{document}\n",
                BTreeMap::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                BTreeMap::new(),
                Some(bibliography_state),
                BTreeMap::new(),
                Vec::new(),
            )
            .expect("parse document");

        assert!(document.body.contains("[1] Reference text"));
    }

    #[test]
    fn printbibliography_renders_preparsed_bbl_entries_with_explicit_label() {
        let bibliography_state = parse_bbl_input(
            "\\begin{thebibliography}{99}\\bibitem[Knu84]{key} Reference text\\end{thebibliography}",
        );
        let document = MinimalLatexParser
            .parse_with_context(
                "\\documentclass{article}\n\\begin{document}\n\\printbibliography[heading=none]\n\\end{document}\n",
                BTreeMap::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                BTreeMap::new(),
                Some(bibliography_state),
                BTreeMap::new(),
                Vec::new(),
            )
            .expect("parse document");

        assert!(document.body.contains("[Knu84] Reference text"));
    }

    #[test]
    fn cite_forward_reference_resolves_on_second_pass() {
        let source = "\\documentclass{article}\n\\begin{document}\nSee \\cite{key}.\n\\begin{thebibliography}{99}\n\\bibitem{key} Reference text\n\\end{thebibliography}\n\\end{document}\n";
        let first = MinimalLatexParser.parse(source).expect("parse first pass");
        let second = MinimalLatexParser
            .parse_with_context(
                source,
                first.labels.clone().into_inner(),
                first.section_entries.clone(),
                first.figure_entries.clone(),
                first.table_entries.clone(),
                first.bibliography.clone(),
                Some(first.bibliography_state.clone()),
                BTreeMap::new(),
                Vec::new(),
            )
            .expect("parse second pass");

        assert_eq!(
            first.body.lines().next().map(str::trim_end),
            Some("See [?].")
        );
        assert!(first.has_unresolved_refs);
        assert_eq!(
            second.body_nodes(),
            vec![
                DocumentNode::Text("See [".to_string()),
                DocumentNode::Link {
                    url: "#bib:key".to_string(),
                    children: vec![DocumentNode::Text("1".to_string())],
                },
                DocumentNode::Text("].".to_string()),
                DocumentNode::ParBreak,
                DocumentNode::Text("[1] Reference text".to_string()),
            ]
        );
        assert!(!second.has_unresolved_refs);
    }

    #[test]
    fn hyperref_body_nodes_preserve_internal_link_structure() {
        assert_eq!(
            parse_document(r"\hyperref[sec:intro]{See intro}").body_nodes(),
            vec![DocumentNode::Link {
                url: "#sec:intro".to_string(),
                children: vec![DocumentNode::Text("See intro".to_string())],
            }]
        );
    }

    #[test]
    fn href_body_nodes_preserve_link_structure() {
        assert_eq!(
            parse_document(r"\href{https://example.com}{Example}").body_nodes(),
            vec![DocumentNode::Link {
                url: "https://example.com".to_string(),
                children: vec![DocumentNode::Text("Example".to_string())],
            }]
        );
    }

    #[test]
    fn url_body_nodes_preserve_link_structure() {
        assert_eq!(
            parse_document(r"\url{https://example.com}").body_nodes(),
            vec![DocumentNode::Link {
                url: "https://example.com".to_string(),
                children: vec![DocumentNode::Text("https://example.com".to_string())],
            }]
        );
    }

    #[test]
    fn hypersetup_sets_pdf_metadata_overrides() {
        let document = parse_document(r"\hypersetup{pdftitle={My Title},pdfauthor={Author Name}}");

        assert_eq!(document.labels.pdf_title.as_deref(), Some("My Title"));
        assert_eq!(document.labels.pdf_author.as_deref(), Some("Author Name"));
    }

    #[test]
    fn hypersetup_sets_colorlinks_flag() {
        let document = parse_document(r"\hypersetup{colorlinks=true}");

        assert_eq!(document.labels.color_links, Some(true));
    }

    #[test]
    fn hypersetup_sets_link_color_and_flag() {
        let document = parse_document(r"\hypersetup{colorlinks=true,linkcolor=red}");

        assert_eq!(document.labels.color_links, Some(true));
        assert_eq!(document.labels.link_color.as_deref(), Some("red"));
    }

    #[test]
    fn newenvironment_definition_and_begin_end_expand_with_arguments() {
        let document = parse_document(
            "\\newenvironment{wrap}[1]{<#1: }{>}\\begin{wrap}{boxed}content\\end{wrap}",
        );

        assert_eq!(document.body, "<boxed: content>");
    }

    #[test]
    fn renewenvironment_overrides_existing_definition() {
        let document = parse_document(
            "\\newenvironment{wrap}{[}{]}\\renewenvironment{wrap}{(}{)}\\begin{wrap}x\\end{wrap}",
        );

        assert_eq!(document.body, "(x)");
    }

    #[test]
    fn itemize_environment_emits_bulleted_items() {
        let document = parse_document("\\begin{itemize}\\item First\\item Second\\end{itemize}");

        assert_eq!(document.body, "• First\n\n• Second");
    }

    #[test]
    fn enumerate_environment_emits_numbered_items() {
        let document =
            parse_document("\\begin{enumerate}\\item First\\item Second\\end{enumerate}");

        assert_eq!(document.body, "1. First\n\n2. Second");
    }

    #[test]
    fn description_environment_emits_term_prefixes() {
        let document =
            parse_document("\\begin{description}\\item[term] definition\\end{description}");

        assert_eq!(document.body, "term: definition");
    }

    #[test]
    fn nested_list_environments_keep_independent_item_counters() {
        let document = parse_document(
            "\\begin{itemize}\\item Outer\\begin{enumerate}\\item Inner\\end{enumerate}\\item Tail\\end{itemize}",
        );

        assert_eq!(document.body, "• Outer\n\n1. Inner\n\n• Tail");
    }

    #[test]
    fn tableofcontents_emits_provided_section_entries() {
        let document = MinimalLatexParser
            .parse_with_state(
                "\\documentclass{article}\n\\begin{document}\n\\tableofcontents\n\\end{document}\n",
                BTreeMap::new(),
                vec![
                    SectionEntry {
                        level: 1,
                        number: "1".to_string(),
                        title: "Intro".to_string(),
                    },
                    SectionEntry {
                        level: 2,
                        number: "1.1".to_string(),
                        title: "Scope".to_string(),
                    },
                ],
                Vec::new(),
                Vec::new(),
                BTreeMap::new(),
                Vec::new(),
            )
            .expect("parse document");

        assert_eq!(
            document.body_nodes(),
            vec![
                DocumentNode::Link {
                    url: "#section:1 Intro".to_string(),
                    children: vec![DocumentNode::Text("1  Intro".to_string())],
                },
                DocumentNode::Text("\n".to_string()),
                DocumentNode::Link {
                    url: "#section:1.1 Scope".to_string(),
                    children: vec![DocumentNode::Text("1.1  Scope".to_string())],
                },
            ]
        );
        assert!(!document.has_unresolved_toc);
    }

    #[test]
    fn sections_are_collected_for_toc_resolution() {
        let document = parse_document("\\section{Intro}\n\\subsection{Scope}");

        assert_eq!(
            document.section_entries,
            vec![
                SectionEntry {
                    level: 1,
                    number: "1".to_string(),
                    title: "Intro".to_string(),
                },
                SectionEntry {
                    level: 2,
                    number: "1.1".to_string(),
                    title: "Scope".to_string(),
                },
            ]
        );
    }

    #[test]
    fn float_captions_are_collected_for_lof_and_lot_resolution() {
        let document = parse_document(
            "\\begin{figure}\\caption{Overview}\\end{figure}\n\\begin{table}\\caption{Metrics}\\end{table}",
        );

        assert_eq!(
            document.figure_entries,
            vec![CaptionEntry {
                kind: FloatType::Figure,
                number: "1".to_string(),
                caption: "Overview".to_string(),
            }]
        );
        assert_eq!(
            document.table_entries,
            vec![CaptionEntry {
                kind: FloatType::Table,
                number: "1".to_string(),
                caption: "Metrics".to_string(),
            }]
        );
    }

    #[test]
    fn listoffigures_emits_provided_figure_entries() {
        let document = MinimalLatexParser
            .parse_with_state(
                "\\documentclass{article}\n\\begin{document}\n\\listoffigures\n\\end{document}\n",
                BTreeMap::new(),
                Vec::new(),
                vec![
                    CaptionEntry {
                        kind: FloatType::Figure,
                        number: "1".to_string(),
                        caption: "Overview".to_string(),
                    },
                    CaptionEntry {
                        kind: FloatType::Figure,
                        number: "2".to_string(),
                        caption: String::new(),
                    },
                ],
                Vec::new(),
                BTreeMap::new(),
                Vec::new(),
            )
            .expect("parse document");

        assert_eq!(document.body, "Figure 1: Overview\nFigure 2");
        assert!(!document.has_unresolved_lof);
    }

    #[test]
    fn listoftables_emits_provided_table_entries() {
        let document = MinimalLatexParser
            .parse_with_state(
                "\\documentclass{article}\n\\begin{document}\n\\listoftables\n\\end{document}\n",
                BTreeMap::new(),
                Vec::new(),
                Vec::new(),
                vec![CaptionEntry {
                    kind: FloatType::Table,
                    number: "1".to_string(),
                    caption: "Metrics".to_string(),
                }],
                BTreeMap::new(),
                Vec::new(),
            )
            .expect("parse document");

        assert_eq!(document.body, "Table 1: Metrics");
        assert!(!document.has_unresolved_lot);
    }

    #[test]
    fn makeindex_enables_index_collection() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\makeindex\n\\begin{document}\nAlpha\\index{Alpha}\n\\end{document}\n",
            )
            .expect("parse document");

        assert!(document.index_enabled);
        assert_eq!(
            document.index_entries,
            vec![IndexRawEntry {
                sort_key: "Alpha".to_string(),
                display: "Alpha".to_string(),
            }]
        );
    }

    #[test]
    fn index_command_parses_sort_key_and_display_text() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\makeindex\n\\begin{document}\nTerm\\index{sortkey@Display Text}\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(
            document.index_entries,
            vec![IndexRawEntry {
                sort_key: "sortkey".to_string(),
                display: "Display Text".to_string(),
            }]
        );
    }

    #[test]
    fn printindex_emits_seeded_entries_in_case_insensitive_order() {
        let document = MinimalLatexParser
            .parse_with_state(
                "\\documentclass{article}\n\\makeindex\n\\begin{document}\n\\printindex\n\\end{document}\n",
                BTreeMap::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                BTreeMap::new(),
                vec![
                    IndexEntry {
                        sort_key: "beta".to_string(),
                        display: "Beta".to_string(),
                        page: Some(2),
                    },
                    IndexEntry {
                        sort_key: "Alpha".to_string(),
                        display: "Alpha".to_string(),
                        page: Some(1),
                    },
                ],
            )
            .expect("parse document");

        assert_eq!(document.body, "A\nAlpha . . . . 1\n\nB\nBeta . . . . 2");
        assert!(!document.has_unresolved_index);
    }

    #[test]
    fn printindex_without_seed_entries_requests_second_pass() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\makeindex\n\\begin{document}\nAlpha\\index{Alpha}\n\\printindex\n\\end{document}\n",
            )
            .expect("parse document");

        assert!(document.has_unresolved_index);
        assert_eq!(
            document.index_entries,
            vec![IndexRawEntry {
                sort_key: "Alpha".to_string(),
                display: "Alpha".to_string(),
            }]
        );
    }

    #[test]
    fn index_without_makeindex_is_ignored() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\begin{document}\nAlpha\\index{Alpha}\n\\end{document}\n",
            )
            .expect("parse document");

        assert!(document.index_entries.is_empty());
        assert!(!document.has_unresolved_index);
        assert!(!document.body.contains('\u{E01D}'));
    }

    #[test]
    fn printindex_merges_page_numbers_for_same_term() {
        let document = MinimalLatexParser
            .parse_with_state(
                "\\documentclass{article}\n\\makeindex\n\\begin{document}\n\\printindex\n\\end{document}\n",
                BTreeMap::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                BTreeMap::new(),
                vec![
                    IndexEntry {
                        sort_key: "Alpha".to_string(),
                        display: "Alpha".to_string(),
                        page: Some(1),
                    },
                    IndexEntry {
                        sort_key: "Alpha".to_string(),
                        display: "Alpha".to_string(),
                        page: Some(3),
                    },
                    IndexEntry {
                        sort_key: "Beta".to_string(),
                        display: "Beta".to_string(),
                        page: Some(2),
                    },
                ],
            )
            .expect("parse document");

        assert!(document.body.contains("Alpha . . . . 1, 3"));
        assert!(document.body.contains("Beta . . . . 2"));
    }

    #[test]
    fn tableofcontents_without_seed_entries_requests_second_pass() {
        let document = parse_document("\\tableofcontents\n\\section{Intro}");

        assert_eq!(document.body, "1 Intro");
        assert!(document.has_unresolved_toc);
    }

    #[test]
    fn list_commands_without_seed_entries_request_second_pass() {
        let document = parse_document(
            "\\listoffigures\n\\listoftables\n\\begin{figure}\\caption{Overview}\\end{figure}\n\\begin{table}\\caption{Metrics}\\end{table}",
        );

        assert!(document.has_unresolved_lof);
        assert!(document.has_unresolved_lot);
        assert_eq!(
            document.figure_entries,
            vec![CaptionEntry {
                kind: FloatType::Figure,
                number: "1".to_string(),
                caption: "Overview".to_string(),
            }]
        );
        assert_eq!(
            document.table_entries,
            vec![CaptionEntry {
                kind: FloatType::Table,
                number: "1".to_string(),
                caption: "Metrics".to_string(),
            }]
        );
    }

    #[test]
    fn uncaptioned_floats_excluded_from_lof_lot_entries() {
        let document = parse_document(
            "\\begin{figure}Body only\\end{figure}\n\\begin{table}\\caption{Metrics}\\end{table}",
        );

        assert!(document.figure_entries.is_empty());
        assert_eq!(
            document.table_entries,
            vec![CaptionEntry {
                kind: FloatType::Table,
                number: "1".to_string(),
                caption: "Metrics".to_string(),
            }]
        );
    }

    #[test]
    fn recovering_reports_unclosed_environment() {
        let output = MinimalLatexParser.parse_recovering(
            "\\documentclass{article}\n\\begin{document}\n\\begin{itemize}\n\\item Open\n\\end{document}\n",
        );

        assert!(output.errors.contains(&ParseError::UnclosedEnvironment {
            line: 3,
            name: "itemize".to_string(),
        }));
        assert!(output.document.is_some());
    }

    #[test]
    fn recovering_reports_unclosed_equation_environment() {
        let source = "\\documentclass{article}\n\\begin{document}\nBefore\n\\begin{equation}\nx = 1\n\\end{document}\n";
        let output = MinimalLatexParser.parse_recovering(source);
        assert!(output.document.is_some());
        let doc = output.document.unwrap();
        assert!(doc.body.contains("Before"));
        assert!(
            output.errors.iter().any(
                |e| matches!(e, ParseError::UnclosedEnvironment { name, .. } if name == "equation")
            ),
            "should report unclosed equation environment"
        );
    }

    #[test]
    fn pdftex_stub_primitives_expand_and_assign() {
        let document = parse_document(
            "\\pdfoutput=0\\pdfoutput/\\pdftexversion/\\pdfstrcmp{a}{b}/\\pdfstrcmp{a}{a}/\\pdfstrcmp{b}{a}/\\pdffilesize{demo.pdf}/\\pdftexbanner",
        );

        assert_eq!(document.body, "0/140/-1/0/1/0/This is ferritex");
    }

    #[test]
    fn etex_primitives_expand_in_body_and_conditionals() {
        let document = parse_document(
            "\\def\\foo{X}\\numexpr(2+3)*4\\relax/\\ifnum\\numexpr(10-4)/3\\relax=2Y\\else N\\fi/\\dimexpr1pt+2pt\\relax/\\detokenize{\\foo bar}/\\unexpanded{\\foo}",
        );

        assert_eq!(document.body, "20/Y/3.0pt/\\foobar/X");
    }

    #[test]
    fn dimexpr_supports_parentheses_multiply_divide_and_registers() {
        let document =
            parse_document("\\dimen0=2pt\\skip0=4pt\\dimexpr(\\dimen0+\\skip0)*3/2\\relax");

        assert_eq!(document.body, "9.0pt");
    }

    #[test]
    fn ifdim_accepts_dimexpr_operands() {
        let document = parse_document("\\ifdim\\dimexpr1pt+2pt\\relax=3pt T\\else F\\fi");

        assert_eq!(document.body, "T");
    }

    #[test]
    fn protected_prefix_marks_definitions_as_protected() {
        let source = "\\documentclass{article}\n\\protected\\def\\foo{X}\n\\protected\\edef\\bar{Y}\n\\begin{document}\n\\end{document}\n";
        let mut driver = ParserDriver::new_with_context(
            source,
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            BTreeMap::new(),
            None,
            BTreeMap::new(),
            Vec::new(),
            None,
        );

        while let Some(token) = driver.next_raw_token() {
            if driver
                .process_preamble_token(token)
                .expect("process preamble token")
            {
                break;
            }
        }

        assert_eq!(
            driver
                .macro_engine
                .lookup("foo")
                .map(|definition| definition.protected),
            Some(true)
        );
        assert_eq!(
            driver
                .macro_engine
                .lookup("bar")
                .map(|definition| definition.protected),
            Some(true)
        );
    }

    #[test]
    fn process_options_uses_declaration_order_and_execute_options_uses_default_handler() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\RequirePackage[alpha,beta]{demo}\n\\def\\trace{}\n\\DeclareOption{beta}{\\edef\\trace{\\trace B}}\n\\DeclareOption{alpha}{\\edef\\trace{\\trace A}}\n\\ProcessOptions\n\\DeclareOption*{\\edef\\trace{\\trace W}}\n\\ExecuteOptions{unknown}\n\\begin{document}\n\\trace\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "BAW");
    }

    #[test]
    fn process_options_star_variant_uses_same_declaration_order_stub() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\RequirePackage[alpha,beta]{demo}\n\\def\\trace{}\n\\DeclareOption{beta}{\\edef\\trace{\\trace B}}\n\\DeclareOption{alpha}{\\edef\\trace{\\trace A}}\n\\ProcessOptions*\n\\begin{document}\n\\trace\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "BA");
    }
}
