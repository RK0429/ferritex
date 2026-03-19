use thiserror::Error;

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
            | Self::UnclosedBrace { line } => Some(*line),
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

    let uncommented = strip_comments(source);
    validate_braces(&uncommented)?;

    let begin_index =
        uncommented
            .find("\\begin{document}")
            .ok_or(ParseError::MissingBeginDocument {
                line: line_for_offset(&uncommented, uncommented.len()),
            })?;
    if let Some(end_before_begin) = uncommented[..begin_index].find("\\end{document}") {
        return Err(ParseError::UnexpectedEndDocument {
            line: line_for_offset(&uncommented, end_before_begin),
        });
    }
    let end_index = uncommented[begin_index..]
        .find("\\end{document}")
        .map(|offset| begin_index + offset)
        .ok_or(ParseError::MissingEndDocument {
            line: line_for_offset(&uncommented, uncommented.len()),
        })?;

    let documentclass_index = uncommented[..begin_index]
        .find("\\documentclass")
        .ok_or(ParseError::MissingDocumentClass)?;
    let documentclass_line = line_for_offset(&uncommented, documentclass_index);
    let document_class = extract_document_class(&uncommented, documentclass_index).ok_or(
        ParseError::InvalidDocumentClass {
            line: documentclass_line,
        },
    )?;

    let body_start = begin_index + "\\begin{document}".len();
    let document_end = end_index + "\\end{document}".len();
    if let Some((offset, _)) = uncommented[document_end..]
        .char_indices()
        .find(|(_, ch)| !ch.is_whitespace())
    {
        return Err(ParseError::TrailingContentAfterEndDocument {
            line: line_for_offset(&uncommented, document_end + offset),
        });
    }
    let body = uncommented[body_start..end_index].trim().to_string();
    let package_count = uncommented[..begin_index]
        .match_indices("\\usepackage")
        .count();

    Ok(ParsedDocument {
        document_class,
        package_count,
        body,
    })
}

fn strip_comments(source: &str) -> String {
    let mut stripped = String::with_capacity(source.len());
    let mut escaped = false;
    let mut in_comment = false;

    for ch in source.chars() {
        if in_comment {
            if ch == '\n' {
                in_comment = false;
                stripped.push('\n');
            }
            continue;
        }

        if escaped {
            stripped.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' => {
                stripped.push(ch);
                escaped = true;
            }
            '%' => in_comment = true,
            _ => stripped.push(ch),
        }
    }

    stripped
}

fn extract_document_class(source: &str, command_index: usize) -> Option<String> {
    let tail = &source[command_index + "\\documentclass".len()..];
    let tail = tail.trim_start();
    let tail = if tail.starts_with('[') {
        let closing = tail.find(']')?;
        &tail[closing + 1..]
    } else {
        tail
    };

    let tail = tail.trim_start();
    let closing = tail.find('}')?;
    tail.strip_prefix('{')
        .map(|stripped| stripped[..closing - 1].trim().to_string())
        .filter(|name| is_valid_document_class(name))
        .filter(|name| !name.is_empty())
}

fn is_valid_document_class(name: &str) -> bool {
    !name.chars().any(|ch| ch.is_control() || ch.is_whitespace())
}

fn validate_braces(source: &str) -> Result<(), ParseError> {
    let mut line = 1u32;
    let mut escaped = false;
    let mut in_comment = false;
    let mut open_braces = Vec::<u32>::new();

    for ch in source.chars() {
        if in_comment {
            if ch == '\n' {
                in_comment = false;
                line += 1;
            }
            continue;
        }

        if escaped {
            escaped = false;
            if ch == '\n' {
                line += 1;
            }
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '%' => in_comment = true,
            '{' => open_braces.push(line),
            '}' => {
                if open_braces.pop().is_none() {
                    return Err(ParseError::UnexpectedClosingBrace { line });
                }
            }
            '\n' => line += 1,
            _ => {}
        }
    }

    if open_braces.is_empty() {
        Ok(())
    } else {
        Err(ParseError::UnclosedBrace {
            line: *open_braces.last().expect("open brace line"),
        })
    }
}

fn line_for_offset(source: &str, offset: usize) -> u32 {
    let offset = offset.min(source.len());
    1 + source[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as u32
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
}
