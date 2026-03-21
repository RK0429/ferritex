use std::collections::VecDeque;

use thiserror::Error;

use super::{
    conditionals::{evaluate_ifnum, tokens_equal, ConditionalState, SkipOutcome},
    registers::{RegisterStore, MAX_REGISTER_INDEX},
    CatCode, MacroDef, MacroEngine, Token, TokenKind, Tokenizer,
};

const MAX_CONSECUTIVE_MACRO_EXPANSIONS: usize = 1_000;
const BODY_PAGE_BREAK_MARKER: char = '\u{E000}';

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedDocument {
    pub document_class: String,
    pub package_count: usize,
    pub body: String,
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
pub enum DocumentNode {
    Text(String),
    ParBreak,
    PageBreak,
}

impl ParsedDocument {
    pub fn body_nodes(&self) -> Vec<DocumentNode> {
        if self.body.trim().is_empty() {
            return Vec::new();
        }

        let normalized_body = normalize_body_par_breaks(&self.body);
        let segments = normalized_body
            .split(BODY_PAGE_BREAK_MARKER)
            .collect::<Vec<_>>();
        let mut nodes = Vec::new();

        for (index, segment) in segments.iter().enumerate() {
            nodes.extend(body_text_nodes(segment));
            if index + 1 < segments.len() {
                nodes.push(DocumentNode::PageBreak);
            }
        }

        nodes
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
        if source.trim().is_empty() {
            return ParseOutput {
                document: None,
                errors: vec![ParseError::EmptyInput],
            };
        }

        ParserDriver::new(source).run_recovering()
    }
}

fn parse_minimal_latex(source: &str) -> Result<ParsedDocument, ParseError> {
    if source.trim().is_empty() {
        return Err(ParseError::EmptyInput);
    }

    ParserDriver::new(source).run()
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

#[derive(Debug)]
struct ParserDriver<'a> {
    tokenizer: Tokenizer<'a>,
    macro_engine: MacroEngine,
    registers: RegisterStore,
    conditionals: ConditionalState,
    pending_tokens: VecDeque<QueuedToken>,
    runtime_group_stack: Vec<u32>,
    semisimple_group_stack: Vec<u32>,
    document_class: Option<String>,
    document_class_error: Option<ParseError>,
    errors: Vec<ParseError>,
    package_count: usize,
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
    fn new(source: &'a str) -> Self {
        Self {
            tokenizer: Tokenizer::new(source.as_bytes()),
            macro_engine: MacroEngine::default(),
            registers: RegisterStore::default(),
            conditionals: ConditionalState::default(),
            pending_tokens: VecDeque::new(),
            runtime_group_stack: Vec::new(),
            semisimple_group_stack: Vec::new(),
            document_class: None,
            document_class_error: None,
            errors: Vec::new(),
            package_count: 0,
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
                    "end" if self.read_environment_name()?.as_deref() == Some("document") => {
                        self.end_found = true;
                        return Ok(true);
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
            .map(|tokens| tokens_to_text(&tokens)))
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

fn body_text_nodes(body: &str) -> Vec<DocumentNode> {
    if body.trim().is_empty() {
        return Vec::new();
    }

    let mut nodes = Vec::new();
    let mut current_text = String::new();
    let mut in_break = false;

    for line in body.split('\n') {
        if line.trim().is_empty() {
            push_body_text_node(&mut nodes, &mut current_text);
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

    push_body_text_node(&mut nodes, &mut current_text);
    if matches!(nodes.last(), Some(DocumentNode::ParBreak)) {
        let _ = nodes.pop();
    }

    nodes
}

fn push_body_text_node(nodes: &mut Vec<DocumentNode>, current_text: &mut String) {
    if current_text.is_empty() {
        return;
    }

    let text = current_text.trim().to_string();
    current_text.clear();
    if !text.is_empty() {
        nodes.push(DocumentNode::Text(text));
    }
}

fn eof_line(source: &str) -> u32 {
    1 + source.bytes().filter(|byte| *byte == b'\n').count() as u32
}

#[cfg(test)]
mod tests {
    use super::{DocumentNode, MinimalLatexParser, ParseError, ParsedDocument, Parser};

    fn parsed_document(body: &str) -> ParsedDocument {
        ParsedDocument {
            document_class: "article".to_string(),
            package_count: 0,
            body: body.to_string(),
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
        assert!(
            output
                .errors
                .iter()
                .any(|e| matches!(e, ParseError::UnclosedBrace { .. }))
        );
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
}
