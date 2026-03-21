use std::{
    collections::{BTreeMap, VecDeque},
    ops::{Deref, DerefMut},
};

use thiserror::Error;

use crate::kernel::api::DimensionValue;

use super::{
    conditionals::{evaluate_ifnum, tokens_equal, ConditionalState, SkipOutcome},
    registers::{RegisterStore, MAX_REGISTER_INDEX},
    CatCode, EnvironmentDef, MacroDef, MacroEngine, Token, TokenKind, Tokenizer,
};

const MAX_CONSECUTIVE_MACRO_EXPANSIONS: usize = 1_000;
const BODY_PAGE_BREAK_MARKER: char = '\u{E000}';
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
const BODY_EQUATION_ENV_START: char = '\u{E00E}';
const BODY_EQUATION_ENV_END: char = '\u{E00F}';
const BODY_PAGEREF_START: char = '\u{E010}';
const BODY_PAGEREF_END: char = '\u{E011}';
const BODY_INCLUDEGRAPHICS_START: char = '\u{E012}';
const BODY_INCLUDEGRAPHICS_END: char = '\u{E013}';
const BODY_INCLUDEGRAPHICS_PATH_END: char = '\u{E014}';
const BODY_INCLUDEGRAPHICS_FIELD_SEPARATOR: char = '\u{E015}';
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentLabels {
    entries: BTreeMap<String, String>,
    pub citations: Vec<String>,
    pub bibliography: BTreeMap<String, String>,
    pub section_entries: Vec<SectionEntry>,
    pub page_label_anchors: BTreeMap<String, String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub has_unresolved_toc: bool,
}

impl DocumentLabels {
    fn with_metadata(
        entries: BTreeMap<String, String>,
        citations: Vec<String>,
        bibliography: BTreeMap<String, String>,
        section_entries: Vec<SectionEntry>,
        page_label_anchors: BTreeMap<String, String>,
        title: Option<String>,
        author: Option<String>,
        has_unresolved_toc: bool,
    ) -> Self {
        Self {
            entries,
            citations,
            bibliography,
            section_entries,
            page_label_anchors,
            title,
            author,
            has_unresolved_toc,
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
    pub package_count: usize,
    pub body: String,
    pub labels: DocumentLabels,
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
    Superscript(Box<MathNode>),
    Subscript(Box<MathNode>),
    Frac {
        numer: Vec<MathNode>,
        denom: Vec<MathNode>,
    },
    Group(Vec<MathNode>),
    Text(String),
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

#[derive(Debug, Clone, PartialEq)]
pub enum DocumentNode {
    Text(String),
    Link {
        url: String,
        children: Vec<DocumentNode>,
    },
    ParBreak,
    PageBreak,
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
            | Self::MacroExpansionLimit { line } => Some(*line),
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
            BTreeMap::new(),
            BTreeMap::new(),
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

    pub fn parse_with_state(
        &self,
        source: &str,
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_page_labels: BTreeMap<String, u32>,
    ) -> Result<ParsedDocument, ParseError> {
        parse_minimal_latex_with_state(
            source,
            initial_labels,
            initial_section_entries,
            initial_page_labels,
        )
    }

    pub fn parse_with_context(
        &self,
        source: &str,
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_bibliography: BTreeMap<String, String>,
        initial_page_labels: BTreeMap<String, u32>,
    ) -> Result<ParsedDocument, ParseError> {
        parse_minimal_latex_with_context(
            source,
            initial_labels,
            initial_section_entries,
            initial_bibliography,
            initial_page_labels,
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
            BTreeMap::new(),
            initial_page_labels,
        )
    }

    pub fn parse_recovering_with_state(
        &self,
        source: &str,
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_page_labels: BTreeMap<String, u32>,
    ) -> ParseOutput {
        self.parse_recovering_with_context(
            source,
            initial_labels,
            initial_section_entries,
            BTreeMap::new(),
            initial_page_labels,
        )
    }

    pub fn parse_recovering_with_context(
        &self,
        source: &str,
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_bibliography: BTreeMap<String, String>,
        initial_page_labels: BTreeMap<String, u32>,
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
            initial_bibliography,
            initial_page_labels,
        )
        .run_recovering()
    }
}

fn parse_minimal_latex(source: &str) -> Result<ParsedDocument, ParseError> {
    parse_minimal_latex_with_context(
        source,
        BTreeMap::new(),
        Vec::new(),
        BTreeMap::new(),
        BTreeMap::new(),
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
        BTreeMap::new(),
        initial_page_labels,
    )
}

fn parse_minimal_latex_with_state(
    source: &str,
    initial_labels: BTreeMap<String, String>,
    initial_section_entries: Vec<SectionEntry>,
    initial_page_labels: BTreeMap<String, u32>,
) -> Result<ParsedDocument, ParseError> {
    parse_minimal_latex_with_context(
        source,
        initial_labels,
        initial_section_entries,
        BTreeMap::new(),
        initial_page_labels,
    )
}

fn parse_minimal_latex_with_context(
    source: &str,
    initial_labels: BTreeMap<String, String>,
    initial_section_entries: Vec<SectionEntry>,
    initial_bibliography: BTreeMap<String, String>,
    initial_page_labels: BTreeMap<String, u32>,
) -> Result<ParsedDocument, ParseError> {
    if source.trim().is_empty() {
        return Err(ParseError::EmptyInput);
    }

    ParserDriver::new_with_context(
        source,
        initial_labels,
        initial_section_entries,
        initial_bibliography,
        initial_page_labels,
    )
    .run()
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArithmeticOperation {
    Advance,
    Multiply,
    Divide,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParserState {
    section: u32,
    subsection: u32,
    subsubsection: u32,
    current_section_number: Option<String>,
    current_label_anchor: Option<String>,
    equation_counter: u32,
    labels: BTreeMap<String, String>,
    page_labels: BTreeMap<String, u32>,
    page_label_anchors: BTreeMap<String, String>,
    citations: Vec<String>,
    bibliography: BTreeMap<String, String>,
    section_entries: Vec<SectionEntry>,
    initial_section_entries: Vec<SectionEntry>,
    has_unresolved_refs: bool,
    has_unresolved_toc: bool,
}

impl Default for ParserState {
    fn default() -> Self {
        Self::new(
            BTreeMap::new(),
            Vec::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        )
    }
}

impl ParserState {
    fn new(
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_bibliography: BTreeMap<String, String>,
        initial_page_labels: BTreeMap<String, u32>,
    ) -> Self {
        Self {
            section: 0,
            subsection: 0,
            subsubsection: 0,
            current_section_number: None,
            current_label_anchor: None,
            equation_counter: 0,
            labels: initial_labels,
            page_labels: initial_page_labels,
            page_label_anchors: BTreeMap::new(),
            citations: Vec::new(),
            bibliography: initial_bibliography,
            section_entries: Vec::new(),
            initial_section_entries,
            has_unresolved_refs: false,
            has_unresolved_toc: false,
        }
    }

    fn next_section_number(&mut self, level: u8) -> String {
        let number = match level {
            1 => {
                self.section += 1;
                self.subsection = 0;
                self.subsubsection = 0;
                self.section.to_string()
            }
            2 => {
                self.subsection += 1;
                self.subsubsection = 0;
                format!("{}.{}", self.section, self.subsection)
            }
            3 => {
                self.subsubsection += 1;
                format!(
                    "{}.{}.{}",
                    self.section, self.subsection, self.subsubsection
                )
            }
            _ => unreachable!("section levels are constrained by the caller"),
        };
        self.current_section_number = Some(number.clone());
        number
    }

    fn citation_number(&mut self, key: &str) -> Option<usize> {
        if !self.bibliography.contains_key(key) {
            return None;
        }

        if let Some(index) = self.citations.iter().position(|citation| citation == key) {
            Some(index + 1)
        } else {
            self.citations.push(key.to_string());
            Some(self.citations.len())
        }
    }

    fn register_bibliography_entry(&mut self, key: String, display_text: String) -> usize {
        self.bibliography.insert(key.clone(), display_text);
        self.citation_number(&key).unwrap_or_else(|| {
            self.citations.push(key);
            self.citations.len()
        })
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

#[derive(Debug)]
struct ParserDriver<'a> {
    tokenizer: Tokenizer<'a>,
    macro_engine: MacroEngine,
    state: ParserState,
    registers: RegisterStore,
    conditionals: ConditionalState,
    pending_tokens: VecDeque<QueuedToken>,
    environment_stack: Vec<OpenEnvironment>,
    runtime_group_stack: Vec<u32>,
    semisimple_group_stack: Vec<u32>,
    document_class: Option<String>,
    document_class_error: Option<ParseError>,
    errors: Vec<ParseError>,
    package_count: usize,
    title: Option<String>,
    author: Option<String>,
    body: String,
    begin_found: bool,
    end_found: bool,
    first_end_before_begin_line: Option<u32>,
    eof_line: u32,
    current_token_from_expansion: bool,
    consecutive_macro_expansions: usize,
    global_prefix: bool,
    alloc_count: u32,
}

#[derive(Debug)]
struct QueuedToken {
    token: Token,
    from_expansion: bool,
}

impl<'a> ParserDriver<'a> {
    fn new_with_context(
        source: &'a str,
        initial_labels: BTreeMap<String, String>,
        initial_section_entries: Vec<SectionEntry>,
        initial_bibliography: BTreeMap<String, String>,
        initial_page_labels: BTreeMap<String, u32>,
    ) -> Self {
        Self {
            tokenizer: Tokenizer::new(source.as_bytes()),
            macro_engine: MacroEngine::default(),
            state: ParserState::new(
                initial_labels,
                initial_section_entries,
                initial_bibliography,
                initial_page_labels,
            ),
            registers: RegisterStore::default(),
            conditionals: ConditionalState::default(),
            pending_tokens: VecDeque::new(),
            environment_stack: Vec::new(),
            runtime_group_stack: Vec::new(),
            semisimple_group_stack: Vec::new(),
            document_class: None,
            document_class_error: None,
            errors: Vec::new(),
            package_count: 0,
            title: None,
            author: None,
            body: String::new(),
            begin_found: false,
            end_found: false,
            first_end_before_begin_line: None,
            eof_line: eof_line(source),
            current_token_from_expansion: false,
            consecutive_macro_expansions: 0,
            global_prefix: false,
            alloc_count: 10,
        }
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

        if self.document_class.is_none() {
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

        if self.document_class.is_none() {
            self.record_error(ParseError::MissingDocumentClass);
        }

        let trailing_valid = self.validate_trailing_content();
        let has_trailing_error = trailing_valid.is_err();
        if let Err(error) = trailing_valid {
            self.record_error(error);
        }

        let document = if self.document_class.is_some()
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
        ParsedDocument {
            document_class: self
                .document_class
                .clone()
                .expect("document class presence checked above"),
            package_count: self.package_count,
            body: self.body.trim().to_string(),
            labels: DocumentLabels::with_metadata(
                self.state.labels.clone(),
                self.state.citations.clone(),
                self.state.bibliography.clone(),
                self.state.section_entries.clone(),
                self.state.page_label_anchors.clone(),
                self.title.clone(),
                self.author.clone(),
                self.state.has_unresolved_toc,
            ),
            has_unresolved_refs: self.state.has_unresolved_refs,
        }
    }

    fn process_preamble_token(&mut self, token: Token) -> Result<bool, ParseError> {
        if self.handle_runtime_group_token(&token)? {
            return Ok(false);
        }

        let Some(name) = control_sequence_name(&token) else {
            let _ = self.take_global_prefix();
            return Ok(false);
        };

        match name.as_str() {
            "documentclass" => {
                if self.document_class.is_none() && self.document_class_error.is_none() {
                    match self.parse_document_class() {
                        Ok(Some(class_name)) => self.document_class = Some(class_name),
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
            "usepackage" => {
                self.package_count += 1;
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
                self.body.push_str("\n\n");
                Ok(false)
            }
            TokenKind::ControlWord(_) | TokenKind::ControlSymbol(_) => {
                let name = control_sequence_name(&token).expect("control sequence token");

                if self.handle_common_primitive(&token, &name)? {
                    return Ok(false);
                }

                match name.as_str() {
                    "pagebreak" | "newpage" | "clearpage" => {
                        let _ = self.take_global_prefix();
                        self.body.push(BODY_PAGE_BREAK_MARKER);
                    }
                    "cite" => {
                        let _ = self.take_global_prefix();
                        self.parse_cite_command()?;
                    }
                    "href" => {
                        let _ = self.take_global_prefix();
                        self.parse_href_command()?;
                    }
                    "url" => {
                        let _ = self.take_global_prefix();
                        self.parse_url_command()?;
                    }
                    "includegraphics" => {
                        let _ = self.take_global_prefix();
                        self.parse_includegraphics_command()?;
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
                            self.body.push_str(&page_number.to_string());
                        } else {
                            self.body.push(BODY_PAGEREF_START);
                            self.body.push_str(&key);
                            self.body.push(BODY_PAGEREF_END);
                            self.state.has_unresolved_refs = true;
                        }
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
                if self.should_skip_insignificant_space(&token) {
                    return Ok(false);
                }
                self.body.push_str(&render_token(&token));
                Ok(false)
            }
        }
    }

    fn handle_runtime_group_token(&mut self, token: &Token) -> Result<bool, ParseError> {
        match token.kind {
            TokenKind::CharToken {
                cat: CatCode::BeginGroup,
                ..
            } => {
                let _ = self.take_global_prefix();
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
            "catcode" => {
                let _ = self.take_global_prefix();
                self.parse_catcode()?;
                Ok(true)
            }
            "global" => {
                self.global_prefix = true;
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
            "countdef" => {
                let is_global = self.take_global_prefix();
                self.parse_countdef(is_global, token.line)?;
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

        if matches!(name.as_str(), "equation" | "equation*" | "align" | "align*") {
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
                    OpenEnvironmentKind::UserDefined { .. } => None,
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
            self.body.push_str(&entry.number);
            if !entry.title.is_empty() {
                self.body.push_str("  ");
                self.body.push_str(&entry.title);
            }
            self.body.push('\n');
        }
        self.body.push('\n');
    }

    fn parse_section_command(&mut self, level: u8) -> Result<(), ParseError> {
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };

        let title = tokens_to_text(&tokens).trim().to_string();
        let number = self.state.next_section_number(level);
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
                    |number| number.to_string(),
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
        let number = self
            .state
            .register_bibliography_entry(key, display_text.clone());
        if display_text.is_empty() {
            entries.push(format!("[{number}]"));
        } else {
            entries.push(format!("[{number}] {display_text}"));
        }
    }

    fn parse_label_command(&mut self) -> Result<(), ParseError> {
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(());
        };
        let key = tokens_to_text(&tokens).trim().to_string();
        if key.is_empty() {
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
            self.body.push_str(&number);
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
        {
            return;
        }

        if self.body.ends_with('\n') {
            self.body.push('\n');
        } else {
            self.body.push_str("\n\n");
        }
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
            || self.body.ends_with(BODY_PAGE_BREAK_MARKER))
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
        }

        Ok(())
    }

    fn expand_the(&mut self, line: u32) -> Result<(), ParseError> {
        let Some((kind, index)) = self.parse_register_target(line)? else {
            return Ok(());
        };

        let rendered = match kind {
            RegisterKind::Count => self.registers.get_count(index).to_string(),
            RegisterKind::Dimen => format_dimen(self.registers.get_dimen(index)),
            RegisterKind::Skip => format_dimen(self.registers.get_skip(index)),
            RegisterKind::Muskip => format_dimen(self.registers.get_muskip(index)),
        };
        self.push_front_tokens(tokens_from_text(&rendered, line));
        Ok(())
    }

    fn apply_arithmetic(
        &mut self,
        operation: ArithmeticOperation,
        line: u32,
    ) -> Result<(), ParseError> {
        let Some((kind, index)) = self.parse_register_target(line)? else {
            return Ok(());
        };
        let _ = self.read_keyword("by");

        let global = self.take_global_prefix();
        match kind {
            RegisterKind::Count => {
                let current = self.registers.get_count(index);
                let operand = self.parse_integer_value()?.unwrap_or(0);
                let value = apply_integer_arithmetic(current, operand, operation, line)?;
                self.registers.set_count(index, value, global);
            }
            RegisterKind::Dimen => {
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

    fn parse_register_target(
        &mut self,
        line: u32,
    ) -> Result<Option<(RegisterKind, u16)>, ParseError> {
        loop {
            let Some(token) = self.next_significant_token() else {
                return Ok(None);
            };

            match control_sequence_name(&token).as_deref() {
                Some("count") => {
                    return Ok(Some((
                        RegisterKind::Count,
                        self.parse_register_index(line)?,
                    )));
                }
                Some("dimen") => {
                    return Ok(Some((
                        RegisterKind::Dimen,
                        self.parse_register_index(line)?,
                    )));
                }
                Some("skip") => {
                    return Ok(Some((RegisterKind::Skip, self.parse_register_index(line)?)));
                }
                Some("muskip") => {
                    return Ok(Some((
                        RegisterKind::Muskip,
                        self.parse_register_index(line)?,
                    )));
                }
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
                TokenKind::ControlWord(ref name) if name == "count" => {
                    let index = self.parse_register_index(value_token.line)?;
                    return Ok(Some(sign * self.registers.get_count(index)));
                }
                TokenKind::ControlWord(ref name) if name == "dimen" => {
                    let index = self.parse_register_index(value_token.line)?;
                    return Ok(Some(sign * self.registers.get_dimen(index)));
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
                TokenKind::ControlWord(_) | TokenKind::ControlSymbol(_) => {
                    if let Some(expansion) =
                        self.expand_defined_control_sequence_token(&value_token)?
                    {
                        self.push_front_tokens(expansion);
                        self.push_front_plain_tokens(consumed);
                        continue;
                    }
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
                TokenKind::ControlWord(ref name) if name == "dimen" => {
                    let index = self.parse_register_index(value_token.line)?;
                    return Ok(Some(sign * self.registers.get_dimen(index)));
                }
                TokenKind::ControlWord(ref name) if name == "count" => {
                    let index = self.parse_register_index(value_token.line)?;
                    return Ok(Some(sign * self.registers.get_count(index)));
                }
                TokenKind::ControlWord(ref name) if name == "skip" => {
                    let index = self.parse_register_index(value_token.line)?;
                    return Ok(Some(sign * self.registers.get_skip(index)));
                }
                TokenKind::ControlWord(ref name) if name == "muskip" => {
                    let index = self.parse_register_index(value_token.line)?;
                    return Ok(Some(sign * self.registers.get_muskip(index)));
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
                TokenKind::ControlWord(_) | TokenKind::ControlSymbol(_) => {
                    if let Some(expansion) =
                        self.expand_defined_control_sequence_token(&value_token)?
                    {
                        self.push_front_tokens(expansion);
                        self.push_front_plain_tokens(consumed);
                        continue;
                    }
                }
                _ => {}
            }

            consumed.push(value_token);
            self.push_front_plain_tokens(consumed);
            return Ok(None);
        }
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

    fn parse_document_class(&mut self) -> Result<Option<String>, ParseError> {
        let _ = self.read_optional_bracket_tokens()?;
        let Some(tokens) = self.read_required_braced_tokens()? else {
            return Ok(None);
        };
        let class_name = tokens_to_text(&tokens).trim().to_string();
        if class_name.is_empty() || !is_valid_document_class(&class_name) {
            return Ok(None);
        }
        Ok(Some(class_name))
    }

    fn parse_def(&mut self, is_global: bool) -> Result<(), ParseError> {
        let Some((name, parameter_count, open_line)) = self.read_macro_definition_head()? else {
            return Ok(());
        };
        let body = self.read_group_contents(open_line)?;
        self.store_macro_definition(name, parameter_count, body, is_global);
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
                let source_def = self.macro_engine.lookup(&source_name).cloned();
                self.macro_engine.let_assign(target, source_def, is_global);
            }
            TokenKind::CharToken { .. } => {
                self.store_macro_definition(target, 0, vec![rhs_token], is_global);
            }
            _ => {}
        }

        Ok(())
    }

    fn parse_edef(&mut self, is_global: bool) -> Result<(), ParseError> {
        let Some((name, parameter_count, open_line)) = self.read_macro_definition_head()? else {
            return Ok(());
        };
        let body = self.expand_edef_body(open_line)?;
        self.store_macro_definition(name, parameter_count, body, is_global);
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
        let mut name = String::new();

        loop {
            let Some(token) = self.next_raw_token() else {
                return Err(ParseError::UnclosedBrace { line });
            };
            match token.kind {
                TokenKind::ControlWord(ref control_word) if control_word == "endcsname" => break,
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
    ) {
        let definition = MacroDef {
            name: name.clone(),
            parameter_count,
            body,
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
        MathNode::Superscript(node) => format!("^{}", render_math_attachment_for_anchor(node)),
        MathNode::Subscript(node) => format!("_{}", render_math_attachment_for_anchor(node)),
        MathNode::Frac { numer, denom } => format!(
            "({})/({})",
            render_math_nodes_for_anchor(numer),
            render_math_nodes_for_anchor(denom)
        ),
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
    let segments = body_with_placeholders
        .split(BODY_PAGE_BREAK_MARKER)
        .collect::<Vec<_>>();
    let mut nodes = Vec::new();

    for (index, segment) in segments.iter().enumerate() {
        nodes.extend(body_text_nodes(segment, &placeholders));
        if index + 1 < segments.len() {
            nodes.push(DocumentNode::PageBreak);
        }
    }

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
            BODY_INCLUDEGRAPHICS_START => {
                let (content, next_index) =
                    extract_single_marker_content(body, index, BODY_INCLUDEGRAPHICS_END);
                let placeholder = next_box_placeholder(placeholders.len());
                placeholders.push(deserialize_includegraphics_marker(content));
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

fn parse_dimension_marker_field(value: &str) -> Option<DimensionValue> {
    let trimmed = value.trim();
    (!trimmed.is_empty())
        .then(|| trimmed.parse::<i64>().ok().map(DimensionValue))
        .flatten()
}

fn parse_scale_marker_field(value: &str) -> Option<f64> {
    let trimmed = value.trim();
    (!trimmed.is_empty())
        .then(|| trimmed.parse::<f64>().ok())
        .flatten()
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
            "pagebreak" | "newpage" | "clearpage" => {
                encoded.push(BODY_PAGE_BREAK_MARKER);
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
        MathNode::Superscript(node) => format!("^{}", render_math_attachment_for_encoding(node)),
        MathNode::Subscript(node) => format!("_{}", render_math_attachment_for_encoding(node)),
        MathNode::Frac { numer, denom } => {
            format!(
                r"\frac{{{}}}{{{}}}",
                render_math_nodes_for_encoding(numer),
                render_math_nodes_for_encoding(denom)
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
            let mut name = String::new();
            while let Some(next) = self.peek_char() {
                if !next.is_ascii_alphabetic() {
                    break;
                }
                name.push(next);
                let _ = self.take_char();
            }

            if name == "frac" {
                let numer = self.parse_required_group();
                let denom = self.parse_required_group();
                return vec![MathNode::Frac { numer, denom }];
            }

            if name == "text" {
                return vec![MathNode::Text(self.parse_required_text_group())];
            }

            return name.chars().map(MathNode::Ordinary).collect();
        }

        let symbol = self.take_char().expect("peek_char ensured a symbol exists");
        vec![MathNode::Ordinary(symbol)]
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
        DocumentLabels, DocumentNode, IncludeGraphicsOptions, LineTag, MathLine, MathNode,
        MinimalLatexParser, ParseError, ParsedDocument, Parser, SectionEntry,
    };
    use crate::kernel::api::DimensionValue;
    use std::collections::BTreeMap;

    fn parsed_document(body: &str) -> ParsedDocument {
        ParsedDocument {
            document_class: "article".to_string(),
            package_count: 0,
            body: body.to_string(),
            labels: DocumentLabels::default(),
            has_unresolved_refs: false,
        }
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
                package_count: 0,
                body: "Hello".to_string(),
                labels: DocumentLabels::default(),
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
                package_count: 0,
                body: "AB".to_string(),
                labels: DocumentLabels::default(),
                has_unresolved_refs: false,
            })
        );
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
    fn clearpage_parsed_to_page_break_node() {
        assert_eq!(
            parse_document("First\n\\clearpage\nSecond").body_nodes(),
            vec![
                DocumentNode::Text("First".to_string()),
                DocumentNode::PageBreak,
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
        assert_eq!(
            nodes.get(1),
            Some(&DocumentNode::Text(" See 1.".to_string()))
        );
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
    fn counts_preamble_packages() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass[11pt]{report}\n\\usepackage{amsmath}\n% \\usepackage{commented}\n\\usepackage{hyperref}\n\\begin{document}\nBody\n\\end{document}",
            )
            .expect("parse document");

        assert_eq!(document.document_class, "report");
        assert_eq!(document.package_count, 2);
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
    fn countdef_creates_named_count_aliases() {
        let document = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\countdef\\foo=12\n\\begin{document}\n\\foo=9\\advance\\foo by 1\\the\\foo/\\the\\count12\n\\end{document}\n",
            )
            .expect("parse document");

        assert_eq!(document.body, "10/10");
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

        assert_eq!(document.body, "1 Intro\n\nSee 1.");
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
                BTreeMap::new(),
                BTreeMap::from([("sec:later".to_string(), 5)]),
            )
            .expect("parse document");

        assert_eq!(document.body, "See page 5.");
        assert!(!document.has_pageref_markers());
        assert!(!document.has_unresolved_refs);
    }

    #[test]
    fn cite_resolves_when_bibitem_is_available() {
        let document = parse_document(
            "\\begin{thebibliography}{99}\\bibitem{key} Reference text\\end{thebibliography}\nSee \\cite{key}.",
        );

        assert!(document.body.contains("See [1]."));
        assert_eq!(document.citations, vec!["key".to_string()]);
        assert_eq!(
            document.bibliography.get("key").map(String::as_str),
            Some("Reference text")
        );
    }

    #[test]
    fn cite_supports_multiple_keys() {
        let document = parse_document(
            "\\begin{thebibliography}{99}\\bibitem{a} Alpha\\bibitem{b} Beta\\end{thebibliography}\nSee \\cite{a,b}.",
        );

        assert!(document.body.contains("See [1, 2]."));
        assert_eq!(document.citations, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn unresolved_cite_emits_question_mark_placeholder() {
        let document = parse_document("See \\cite{missing}.");

        assert_eq!(document.body, "See [?].");
        assert!(document.has_unresolved_refs);
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
    fn cite_forward_reference_resolves_on_second_pass() {
        let source = "\\documentclass{article}\n\\begin{document}\nSee \\cite{key}.\n\\begin{thebibliography}{99}\n\\bibitem{key} Reference text\n\\end{thebibliography}\n\\end{document}\n";
        let first = MinimalLatexParser.parse(source).expect("parse first pass");
        let second = MinimalLatexParser
            .parse_with_context(
                source,
                first.labels.clone().into_inner(),
                first.section_entries.clone(),
                first.bibliography.clone(),
                BTreeMap::new(),
            )
            .expect("parse second pass");

        assert_eq!(
            first.body.lines().next().map(str::trim_end),
            Some("See [?].")
        );
        assert!(first.has_unresolved_refs);
        assert_eq!(
            second.body.lines().next().map(str::trim_end),
            Some("See [1].")
        );
        assert!(!second.has_unresolved_refs);
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
                BTreeMap::new(),
            )
            .expect("parse document");

        assert_eq!(document.body, "1  Intro\n1.1  Scope");
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
    fn tableofcontents_without_seed_entries_requests_second_pass() {
        let document = parse_document("\\tableofcontents\n\\section{Intro}");

        assert_eq!(document.body, "1 Intro");
        assert!(document.has_unresolved_toc);
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
}
