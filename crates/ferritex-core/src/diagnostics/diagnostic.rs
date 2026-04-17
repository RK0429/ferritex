use std::fmt;

use serde::{Deserialize, Serialize};

/// 診断の重大度
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// REQ-NF-010 準拠の構造化診断
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub message: String,
    pub context: Option<String>,
    pub suggestion: Option<String>,
}

impl Diagnostic {
    pub fn new(severity: Severity, message: impl Into<String>) -> Self {
        Self {
            severity,
            file: None,
            line: None,
            column: None,
            message: message.into(),
            context: None,
            suggestion: None,
        }
    }

    pub fn with_file(mut self, file: impl Into<String>) -> Self {
        self.file = Some(file.into());
        self
    }

    pub fn with_line(mut self, line: u32) -> Self {
        self.line = Some(line);
        self
    }

    pub fn with_column(mut self, column: u32) -> Self {
        self.column = Some(column);
        self
    }

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.severity)?;

        match (&self.file, self.line, self.column) {
            (Some(file), Some(line), Some(column)) => write!(f, " {file}:{line}:{column}:")?,
            (Some(file), Some(line), None) => write!(f, " {file}:{line}:")?,
            (Some(file), None, _) => write!(f, " {file}:")?,
            (None, Some(line), Some(column)) => write!(f, " line {line}:{column}:")?,
            (None, Some(line), None) => write!(f, " line {line}:")?,
            (None, None, _) => {}
        }

        write!(f, " {}", self.message)?;

        if let Some(context) = &self.context {
            write!(f, "\n  context: {context}")?;
        }

        if let Some(suggestion) = &self.suggestion {
            write!(f, "\n  help: {suggestion}")?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Diagnostic, Severity};

    #[test]
    fn new_populates_required_fields() {
        let diagnostic = Diagnostic::new(Severity::Error, "unexpected token");

        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.message, "unexpected token");
        assert_eq!(diagnostic.file, None);
        assert_eq!(diagnostic.line, None);
        assert_eq!(diagnostic.column, None);
        assert_eq!(diagnostic.context, None);
        assert_eq!(diagnostic.suggestion, None);
    }

    #[test]
    fn display_includes_optional_details() {
        let diagnostic = Diagnostic::new(Severity::Warning, "deprecated command")
            .with_file("main.tex")
            .with_line(12)
            .with_context("inside bibliography block")
            .with_suggestion("replace with the newer form");

        let rendered = diagnostic.to_string();

        assert!(rendered.contains("warning: main.tex:12: deprecated command"));
        assert!(rendered.contains("context: inside bibliography block"));
        assert!(rendered.contains("help: replace with the newer form"));
    }

    #[test]
    fn display_includes_column_when_present_and_preserves_legacy_format_without_it() {
        let with_column = Diagnostic::new(Severity::Error, "undefined control sequence")
            .with_file("E3.tex")
            .with_line(3)
            .with_column(14)
            .to_string();
        let without_column = Diagnostic::new(Severity::Error, "undefined control sequence")
            .with_file("E3.tex")
            .with_line(3)
            .to_string();

        assert!(with_column.contains("error: E3.tex:3:14: undefined control sequence"));
        assert!(without_column.contains("error: E3.tex:3: undefined control sequence"));
        assert!(!without_column.contains("E3.tex:3:14:"));
    }
}
