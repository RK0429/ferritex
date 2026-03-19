use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CatCode {
    Escape = 0,
    BeginGroup = 1,
    EndGroup = 2,
    MathShift = 3,
    AlignmentTab = 4,
    EndOfLine = 5,
    Parameter = 6,
    Superscript = 7,
    Subscript = 8,
    Ignored = 9,
    Space = 10,
    Letter = 11,
    Other = 12,
    Active = 13,
    Comment = 14,
    Invalid = 15,
}

pub fn default_catcode_table() -> [CatCode; 256] {
    let mut table = [CatCode::Other; 256];

    table[0] = CatCode::Ignored;
    table[10] = CatCode::EndOfLine;
    table[13] = CatCode::EndOfLine;
    table[32] = CatCode::Space;
    table[127] = CatCode::Invalid;
    table[b'\\' as usize] = CatCode::Escape;
    table[b'{' as usize] = CatCode::BeginGroup;
    table[b'}' as usize] = CatCode::EndGroup;
    table[b'$' as usize] = CatCode::MathShift;
    table[b'&' as usize] = CatCode::AlignmentTab;
    table[b'#' as usize] = CatCode::Parameter;
    table[b'^' as usize] = CatCode::Superscript;
    table[b'_' as usize] = CatCode::Subscript;
    table[b'~' as usize] = CatCode::Active;
    table[b'%' as usize] = CatCode::Comment;

    for byte in b'A'..=b'Z' {
        table[byte as usize] = CatCode::Letter;
    }
    for byte in b'a'..=b'z' {
        table[byte as usize] = CatCode::Letter;
    }

    table
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    ControlWord(String),
    ControlSymbol(char),
    CharToken { char: char, cat: CatCode },
    Parameter(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TokenizerDiagnostic {
    #[error("invalid UTF-8 byte 0x{byte:02X} at line {line}, column {column}")]
    InvalidUtf8 { line: u32, column: u32, byte: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineState {
    NewLine,
    SkipSpaces,
    MidLine,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Normal,
    ReadingControlSequence {
        start_line: u32,
        start_column: u32,
        name: String,
    },
    SkippingComment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DecodedChar {
    ch: char,
    line: u32,
    column: u32,
}

#[derive(Debug)]
pub struct Tokenizer<'a> {
    input: &'a [u8],
    index: usize,
    lookahead: Option<DecodedChar>,
    catcodes: [CatCode; 256],
    line: u32,
    column: u32,
    line_state: LineState,
    mode: Mode,
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a [u8]) -> Tokenizer<'a> {
        Tokenizer {
            input,
            index: 0,
            lookahead: None,
            catcodes: default_catcode_table(),
            line: 1,
            column: 1,
            line_state: LineState::NewLine,
            mode: Mode::Normal,
        }
    }

    pub fn next_token(&mut self) -> Option<Result<Token, TokenizerDiagnostic>> {
        loop {
            let outcome = match self.mode {
                Mode::Normal => self.next_normal_token(),
                Mode::ReadingControlSequence { .. } => self.next_control_sequence_token(),
                Mode::SkippingComment => self.skip_comment().map(|()| None),
            };

            match outcome {
                Ok(Some(token)) => return Some(Ok(token)),
                Ok(None) => {
                    if self.is_finished() {
                        return None;
                    }
                }
                Err(diagnostic) => return Some(Err(diagnostic)),
            }
        }
    }

    pub fn set_catcode(&mut self, char_code: u8, cat: CatCode) {
        self.catcodes[char_code as usize] = cat;
    }

    pub fn reset_catcodes(&mut self) {
        self.catcodes = default_catcode_table();
    }

    fn is_finished(&self) -> bool {
        self.lookahead.is_none()
            && self.index >= self.input.len()
            && matches!(self.mode, Mode::Normal)
    }

    fn next_normal_token(&mut self) -> Result<Option<Token>, TokenizerDiagnostic> {
        loop {
            match self.line_state {
                LineState::NewLine => {
                    let Some(current) = self.peek_char()? else {
                        return Ok(None);
                    };
                    let cat = self.catcode_for_char(current.ch);
                    match cat {
                        CatCode::Ignored | CatCode::Space => {
                            let _ = self.take_char()?;
                        }
                        CatCode::EndOfLine => {
                            let _ = self.take_char()?;
                            return Ok(Some(Token {
                                kind: TokenKind::ControlWord("par".to_string()),
                                line: current.line,
                                column: current.column,
                            }));
                        }
                        _ => return self.consume_current_char(current, true),
                    }
                }
                LineState::SkipSpaces => {
                    let Some(current) = self.peek_char()? else {
                        return Ok(None);
                    };
                    match self.catcode_for_char(current.ch) {
                        CatCode::Ignored | CatCode::Space => {
                            let _ = self.take_char()?;
                        }
                        CatCode::EndOfLine => {
                            let _ = self.take_char()?;
                            self.line_state = LineState::NewLine;
                        }
                        _ => {
                            self.line_state = LineState::MidLine;
                        }
                    }
                }
                LineState::MidLine => {
                    let Some(current) = self.peek_char()? else {
                        return Ok(None);
                    };
                    return self.consume_current_char(current, false);
                }
            }
        }
    }

    fn consume_current_char(
        &mut self,
        current: DecodedChar,
        at_line_start: bool,
    ) -> Result<Option<Token>, TokenizerDiagnostic> {
        let cat = self.catcode_for_char(current.ch);
        match cat {
            CatCode::Ignored => {
                let _ = self.take_char()?;
                Ok(None)
            }
            CatCode::Escape => {
                let _ = self.take_char()?;
                self.line_state = LineState::MidLine;
                self.mode = Mode::ReadingControlSequence {
                    start_line: current.line,
                    start_column: current.column,
                    name: String::new(),
                };
                Ok(None)
            }
            CatCode::EndOfLine => {
                let _ = self.take_char()?;
                self.line_state = LineState::NewLine;

                if at_line_start || matches!(self.peek_next_raw_byte(), Some(b'\n' | b'\r')) {
                    Ok(None)
                } else {
                    Ok(Some(Token {
                        kind: TokenKind::CharToken {
                            char: ' ',
                            cat: CatCode::Space,
                        },
                        line: current.line,
                        column: current.column,
                    }))
                }
            }
            CatCode::Space => {
                let taken = self.take_char()?.expect("peeked space must still exist");
                if at_line_start {
                    Ok(None)
                } else {
                    self.line_state = LineState::SkipSpaces;
                    Ok(Some(Token {
                        kind: TokenKind::CharToken {
                            char: taken.ch,
                            cat: CatCode::Space,
                        },
                        line: taken.line,
                        column: taken.column,
                    }))
                }
            }
            CatCode::Comment => {
                let _ = self.take_char()?;
                self.mode = Mode::SkippingComment;
                Ok(None)
            }
            CatCode::Parameter if matches!(self.peek_next_raw_byte(), Some(b'1'..=b'9')) => {
                let _ = self.take_char()?;
                let digit = self
                    .take_char()?
                    .expect("parameter digit must follow immediately");
                self.line_state = LineState::MidLine;
                Ok(Some(Token {
                    kind: TokenKind::Parameter((digit.ch as u8) - b'0'),
                    line: current.line,
                    column: current.column,
                }))
            }
            _ => {
                let taken = self
                    .take_char()?
                    .expect("peeked character must still exist");
                self.line_state = LineState::MidLine;
                Ok(Some(Token {
                    kind: TokenKind::CharToken {
                        char: taken.ch,
                        cat,
                    },
                    line: taken.line,
                    column: taken.column,
                }))
            }
        }
    }

    fn next_control_sequence_token(&mut self) -> Result<Option<Token>, TokenizerDiagnostic> {
        loop {
            let Some(next) = self.peek_char()? else {
                let (line, column, name) = match std::mem::replace(&mut self.mode, Mode::Normal) {
                    Mode::ReadingControlSequence {
                        start_line,
                        start_column,
                        name,
                    } => (start_line, start_column, name),
                    _ => unreachable!("control sequence state must match mode"),
                };
                self.line_state = LineState::SkipSpaces;
                return Ok(Some(Token {
                    kind: TokenKind::ControlWord(name),
                    line,
                    column,
                }));
            };

            if self.catcode_for_char(next.ch) == CatCode::Letter {
                let taken = self.take_char()?.expect("peeked letter must still exist");
                if let Mode::ReadingControlSequence { name, .. } = &mut self.mode {
                    name.push(taken.ch);
                }
                continue;
            }

            let name_is_empty = match &self.mode {
                Mode::ReadingControlSequence { name, .. } => name.is_empty(),
                _ => unreachable!("control sequence state must match mode"),
            };

            if name_is_empty {
                let symbol = self.take_char()?.expect("peeked symbol must still exist");
                let (line, column) = match std::mem::replace(&mut self.mode, Mode::Normal) {
                    Mode::ReadingControlSequence {
                        start_line,
                        start_column,
                        ..
                    } => (start_line, start_column),
                    _ => unreachable!("control sequence state must match mode"),
                };
                self.line_state = LineState::MidLine;
                return Ok(Some(Token {
                    kind: TokenKind::ControlSymbol(symbol.ch),
                    line,
                    column,
                }));
            }

            let (line, column, name) = match std::mem::replace(&mut self.mode, Mode::Normal) {
                Mode::ReadingControlSequence {
                    start_line,
                    start_column,
                    name,
                } => (start_line, start_column, name),
                _ => unreachable!("control sequence state must match mode"),
            };
            self.line_state = LineState::SkipSpaces;
            return Ok(Some(Token {
                kind: TokenKind::ControlWord(name),
                line,
                column,
            }));
        }
    }

    fn skip_comment(&mut self) -> Result<(), TokenizerDiagnostic> {
        loop {
            let Some(next) = self.peek_char()? else {
                self.mode = Mode::Normal;
                self.line_state = LineState::NewLine;
                return Ok(());
            };

            let taken = self
                .take_char()?
                .expect("peeked comment character must still exist");
            if matches!(taken.ch, '\n' | '\r') {
                self.mode = Mode::Normal;
                self.line_state = LineState::NewLine;
                return Ok(());
            }

            if taken.line != next.line || taken.column != next.column {
                unreachable!("take_char must consume the previously peeked character");
            }
        }
    }

    fn peek_char(&mut self) -> Result<Option<DecodedChar>, TokenizerDiagnostic> {
        if self.lookahead.is_none() {
            match self.decode_next_char() {
                Some((decoded, Some(diagnostic))) => {
                    self.lookahead = Some(decoded);
                    return Err(diagnostic);
                }
                Some((decoded, None)) => {
                    self.lookahead = Some(decoded);
                }
                None => {}
            }
        }

        Ok(self.lookahead)
    }

    fn take_char(&mut self) -> Result<Option<DecodedChar>, TokenizerDiagnostic> {
        let _ = self.peek_char()?;
        Ok(self.lookahead.take())
    }

    fn decode_next_char(&mut self) -> Option<(DecodedChar, Option<TokenizerDiagnostic>)> {
        if self.index >= self.input.len() {
            return None;
        }

        let start_line = self.line;
        let start_column = self.column;
        let first = self.input[self.index];

        // Normalize CRLF and lone CR to a single TeX end-of-line event.
        if matches!(first, b'\r' | b'\n') {
            self.index += 1;
            if first == b'\r' && self.input.get(self.index) == Some(&b'\n') {
                self.index += 1;
            }
            self.advance_position('\n');
            return Some((
                DecodedChar {
                    ch: '\n',
                    line: start_line,
                    column: start_column,
                },
                None,
            ));
        }

        if first.is_ascii() {
            self.index += 1;
            let ch = first as char;
            self.advance_position(ch);
            return Some((
                DecodedChar {
                    ch,
                    line: start_line,
                    column: start_column,
                },
                None,
            ));
        }

        let Some(width) = utf8_sequence_width(first) else {
            return Some(self.decode_invalid_byte(start_line, start_column, first));
        };

        if self.index + width > self.input.len() {
            return Some(self.decode_invalid_byte(start_line, start_column, first));
        }

        let bytes = &self.input[self.index..self.index + width];
        if !bytes[1..]
            .iter()
            .all(|byte| is_utf8_continuation_byte(*byte))
        {
            return Some(self.decode_invalid_byte(start_line, start_column, first));
        }

        match std::str::from_utf8(bytes) {
            Ok(text) => {
                let ch = text
                    .chars()
                    .next()
                    .expect("validated UTF-8 must contain a char");
                self.index += width;
                self.advance_position(ch);
                Some((
                    DecodedChar {
                        ch,
                        line: start_line,
                        column: start_column,
                    },
                    None,
                ))
            }
            Err(_) => Some(self.decode_invalid_byte(start_line, start_column, first)),
        }
    }

    fn decode_invalid_byte(
        &mut self,
        line: u32,
        column: u32,
        byte: u8,
    ) -> (DecodedChar, Option<TokenizerDiagnostic>) {
        self.index += 1;
        self.advance_position('\u{FFFD}');
        (
            DecodedChar {
                ch: '\u{FFFD}',
                line,
                column,
            },
            Some(TokenizerDiagnostic::InvalidUtf8 { line, column, byte }),
        )
    }

    fn advance_position(&mut self, ch: char) {
        if matches!(ch, '\n' | '\r') {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
    }

    fn catcode_for_char(&self, ch: char) -> CatCode {
        u8::try_from(ch)
            .ok()
            .map(|byte| self.catcodes[byte as usize])
            .unwrap_or(CatCode::Other)
    }

    fn peek_next_raw_byte(&self) -> Option<u8> {
        self.input.get(self.index).copied()
    }
}

fn utf8_sequence_width(first: u8) -> Option<usize> {
    match first {
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        _ => None,
    }
}

fn is_utf8_continuation_byte(byte: u8) -> bool {
    matches!(byte, 0x80..=0xBF)
}

impl<'a> Iterator for Tokenizer<'a> {
    type Item = Result<Token, TokenizerDiagnostic>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_token()
    }
}

#[cfg(test)]
mod tests {
    use super::{CatCode, Token, TokenKind, Tokenizer, TokenizerDiagnostic};

    #[test]
    fn tokenizes_control_word() {
        let tokens = collect_tokens(br"\hello world");

        assert_eq!(
            tokens,
            vec![
                control_word("hello", 1, 1),
                char_token('w', CatCode::Letter, 1, 8),
                char_token('o', CatCode::Letter, 1, 9),
                char_token('r', CatCode::Letter, 1, 10),
                char_token('l', CatCode::Letter, 1, 11),
                char_token('d', CatCode::Letter, 1, 12),
            ]
        );
    }

    #[test]
    fn tokenizes_control_symbol() {
        let tokens = collect_tokens(br"\#");

        assert_eq!(tokens, vec![control_symbol('#', 1, 1)]);
    }

    #[test]
    fn tokenizes_braces() {
        let tokens = collect_tokens(b"{a}");

        assert_eq!(
            tokens,
            vec![
                char_token('{', CatCode::BeginGroup, 1, 1),
                char_token('a', CatCode::Letter, 1, 2),
                char_token('}', CatCode::EndGroup, 1, 3),
            ]
        );
    }

    #[test]
    fn skips_spaces_after_control_word() {
        let tokens = collect_tokens(br"\cmd   x");

        assert_eq!(
            tokens,
            vec![
                control_word("cmd", 1, 1),
                char_token('x', CatCode::Letter, 1, 8),
            ]
        );
    }

    #[test]
    fn handles_comment() {
        let tokens = collect_tokens(b"a%comment\nbc");

        assert_eq!(
            tokens,
            vec![
                char_token('a', CatCode::Letter, 1, 1),
                char_token('b', CatCode::Letter, 2, 1),
                char_token('c', CatCode::Letter, 2, 2),
            ]
        );
    }

    #[test]
    fn newline_in_mid_line_becomes_space() {
        let tokens = collect_tokens(b"a\nb");

        assert_eq!(
            tokens,
            vec![
                char_token('a', CatCode::Letter, 1, 1),
                char_token(' ', CatCode::Space, 1, 2),
                char_token('b', CatCode::Letter, 2, 1),
            ]
        );
    }

    #[test]
    fn double_newline_becomes_par() {
        let tokens = collect_tokens(b"a\n\nb");

        assert_eq!(
            tokens,
            vec![
                char_token('a', CatCode::Letter, 1, 1),
                control_word("par", 2, 1),
                char_token('b', CatCode::Letter, 3, 1),
            ]
        );
    }

    #[test]
    fn normalizes_crlf_sequences() {
        let tokens = collect_tokens(b"a\r\n\r\nb");

        assert_eq!(
            tokens,
            vec![
                char_token('a', CatCode::Letter, 1, 1),
                control_word("par", 2, 1),
                char_token('b', CatCode::Letter, 3, 1),
            ]
        );
    }

    #[test]
    fn catcode_change_takes_effect() {
        let mut tokenizer = Tokenizer::new(br"\make@cmd");
        tokenizer.set_catcode(b'@', CatCode::Letter);

        let tokens = collect_from(&mut tokenizer);
        assert_eq!(tokens, vec![control_word("make@cmd", 1, 1)]);
    }

    #[test]
    fn invalid_utf8_recovery() {
        let results = collect_results(b"a\xffb");

        assert_eq!(
            results,
            vec![
                Ok(char_token('a', CatCode::Letter, 1, 1)),
                Err(TokenizerDiagnostic::InvalidUtf8 {
                    line: 1,
                    column: 2,
                    byte: 0xFF,
                }),
                Ok(char_token('\u{FFFD}', CatCode::Other, 1, 2)),
                Ok(char_token('b', CatCode::Letter, 1, 3)),
            ]
        );
    }

    #[test]
    fn line_column_tracking() {
        let tokens = collect_tokens(
            br"\foo
{a}

#1",
        );

        assert_eq!(
            tokens,
            vec![
                control_word("foo", 1, 1),
                char_token('{', CatCode::BeginGroup, 2, 1),
                char_token('a', CatCode::Letter, 2, 2),
                char_token('}', CatCode::EndGroup, 2, 3),
                control_word("par", 3, 1),
                parameter_ref(1, 4, 1),
            ]
        );
    }

    #[test]
    fn parameter_token() {
        let tokens = collect_tokens(b"#1");

        assert_eq!(tokens, vec![parameter_ref(1, 1, 1)]);
    }

    fn collect_tokens(input: &[u8]) -> Vec<Token> {
        collect_from(&mut Tokenizer::new(input))
    }

    fn collect_from(tokenizer: &mut Tokenizer<'_>) -> Vec<Token> {
        collect_results_from(tokenizer)
            .into_iter()
            .map(|result| result.expect("tokenization should not produce diagnostics"))
            .collect()
    }

    fn collect_results(input: &[u8]) -> Vec<Result<Token, TokenizerDiagnostic>> {
        collect_results_from(&mut Tokenizer::new(input))
    }

    fn collect_results_from(
        tokenizer: &mut Tokenizer<'_>,
    ) -> Vec<Result<Token, TokenizerDiagnostic>> {
        let mut items = Vec::new();
        while let Some(item) = tokenizer.next_token() {
            items.push(item);
        }
        items
    }

    fn control_word(name: &str, line: u32, column: u32) -> Token {
        Token {
            kind: TokenKind::ControlWord(name.to_string()),
            line,
            column,
        }
    }

    fn control_symbol(symbol: char, line: u32, column: u32) -> Token {
        Token {
            kind: TokenKind::ControlSymbol(symbol),
            line,
            column,
        }
    }

    fn char_token(char: char, cat: CatCode, line: u32, column: u32) -> Token {
        Token {
            kind: TokenKind::CharToken { char, cat },
            line,
            column,
        }
    }

    fn parameter_ref(index: u8, line: u32, column: u32) -> Token {
        Token {
            kind: TokenKind::Parameter(index),
            line,
            column,
        }
    }
}
