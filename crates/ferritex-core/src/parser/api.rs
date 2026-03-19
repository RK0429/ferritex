use std::collections::VecDeque;

use thiserror::Error;

use super::{CatCode, MacroDef, MacroEngine, Token, TokenKind, Tokenizer};

const MAX_CONSECUTIVE_MACRO_EXPANSIONS: usize = 1_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedDocument {
    pub document_class: String,
    pub package_count: usize,
    pub body: String,
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

#[derive(Debug)]
struct ParserDriver<'a> {
    tokenizer: Tokenizer<'a>,
    macro_engine: MacroEngine,
    pending_tokens: VecDeque<QueuedToken>,
    runtime_group_stack: Vec<u32>,
    document_class: Option<String>,
    document_class_error: Option<ParseError>,
    package_count: usize,
    body: String,
    begin_found: bool,
    end_found: bool,
    first_end_before_begin_line: Option<u32>,
    eof_line: u32,
    current_token_from_expansion: bool,
    consecutive_macro_expansions: usize,
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
            pending_tokens: VecDeque::new(),
            runtime_group_stack: Vec::new(),
            document_class: None,
            document_class_error: None,
            package_count: 0,
            body: String::new(),
            begin_found: false,
            end_found: false,
            first_end_before_begin_line: None,
            eof_line: eof_line(source),
            current_token_from_expansion: false,
            consecutive_macro_expansions: 0,
        }
    }

    fn run(mut self) -> Result<ParsedDocument, ParseError> {
        let mut phase = Phase::Preamble;

        while let Some(token) = self.next_raw_token() {
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

        let document_class = self
            .document_class
            .expect("document class presence checked above");

        Ok(ParsedDocument {
            document_class,
            package_count: self.package_count,
            body: self.body.trim().to_string(),
        })
    }

    fn process_preamble_token(&mut self, token: Token) -> Result<bool, ParseError> {
        if self.handle_runtime_group_token(&token)? {
            return Ok(false);
        }

        let Some(name) = control_sequence_name(&token) else {
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
            "def" => self.parse_def(false)?,
            "gdef" => self.parse_def(true)?,
            "newcommand" | "renewcommand" => self.parse_newcommand()?,
            "catcode" => self.parse_catcode()?,
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
            _ => {}
        }

        Ok(false)
    }

    fn process_body_token(&mut self, token: Token) -> Result<bool, ParseError> {
        if self.handle_runtime_group_token(&token)? {
            return Ok(false);
        }

        match &token.kind {
            TokenKind::ControlWord(name) if name == "par" => {
                self.body.push_str("\n\n");
                Ok(false)
            }
            TokenKind::ControlWord(_) | TokenKind::ControlSymbol(_) => {
                let name = control_sequence_name(&token).expect("control sequence token");
                match name.as_str() {
                    "def" => self.parse_def(false)?,
                    "gdef" => self.parse_def(true)?,
                    "newcommand" | "renewcommand" => self.parse_newcommand()?,
                    "catcode" => self.parse_catcode()?,
                    "end" if self.read_environment_name()?.as_deref() == Some("document") => {
                        self.end_found = true;
                        return Ok(true);
                    }
                    _ => {
                        if let Some(definition) = self.macro_engine.lookup(&name).cloned() {
                            self.record_macro_expansion(token.line)?;
                            let args = self.collect_macro_arguments(definition.parameter_count)?;
                            let expansion = self.macro_engine.expand(&name, &args);
                            self.push_front_tokens(expansion);
                        } else {
                            self.body.push_str(&render_token(&token));
                        }
                    }
                }

                Ok(false)
            }
            _ => {
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
                self.runtime_group_stack.push(token.line);
                self.macro_engine.push_group();
                Ok(true)
            }
            TokenKind::CharToken {
                cat: CatCode::EndGroup,
                ..
            } => {
                if self.runtime_group_stack.pop().is_none() {
                    return Err(ParseError::UnexpectedClosingBrace { line: token.line });
                }
                self.macro_engine.pop_group();
                self.sync_tokenizer_catcodes();
                Ok(true)
            }
            _ => Ok(false),
        }
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
        let Some(name_token) = self.next_significant_token() else {
            return Ok(());
        };
        let Some(name) = control_sequence_name(&name_token) else {
            return Ok(());
        };

        let mut parameter_count = 0usize;
        loop {
            let Some(token) = self.next_significant_token() else {
                return Ok(());
            };
            match token.kind {
                TokenKind::Parameter(index) => {
                    parameter_count = parameter_count.max(index as usize);
                }
                TokenKind::CharToken {
                    cat: CatCode::BeginGroup,
                    ..
                } => {
                    if parameter_count > 2 {
                        return Ok(());
                    }
                    let body = self.read_group_contents(token.line)?;
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
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }
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
        if parameter_count > 2 {
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
        self.pending_tokens.push_front(QueuedToken {
            token,
            from_expansion: false,
        });
    }

    fn push_front_tokens(&mut self, tokens: Vec<Token>) {
        for token in tokens.into_iter().rev() {
            self.pending_tokens.push_front(QueuedToken {
                token,
                from_expansion: true,
            });
        }
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

fn eof_line(source: &str) -> u32 {
    1 + source.bytes().filter(|byte| *byte == b'\n').count() as u32
}

#[cfg(test)]
mod tests {
    use super::{MinimalLatexParser, ParseError, ParsedDocument, Parser};

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
    fn rejects_recursive_macro_expansion() {
        let error = MinimalLatexParser
            .parse(
                "\\documentclass{article}\n\\def\\loop{\\loop}\n\\begin{document}\n\\loop\n\\end{document}\n",
            )
            .expect_err("parse should fail");

        assert_eq!(error, ParseError::MacroExpansionLimit { line: 2 });
    }
}
