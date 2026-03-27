use std::collections::{BTreeMap, BTreeSet};

use ferritex_core::compilation::{DocumentState, SymbolLocation};
use ferritex_core::diagnostics::Diagnostic;
use ferritex_core::diagnostics::Severity;
use ferritex_core::parser::{MinimalLatexParser, ParseError, Parser};

use crate::open_document_store::OpenDocumentBuffer;
use crate::stable_compile_state::StableCompileState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextPosition {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRange {
    pub start: TextPosition,
    pub end: TextPosition,
}

impl TextRange {
    pub const fn new(
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
    ) -> Self {
        Self {
            start: TextPosition {
                line: start_line,
                character: start_character,
            },
            end: TextPosition {
                line: end_line,
                character: end_character,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisDiagnostic {
    pub range: TextRange,
    pub severity: Severity,
    pub message: String,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Command,
    Environment,
    Label,
    Citation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionCandidate {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionLocation {
    pub uri: String,
    pub range: TextRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverInfo {
    pub range: TextRange,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub range: TextRange,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeActionSuggestion {
    pub title: String,
    pub edit: TextEdit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAnalysisSnapshot {
    pub uri: String,
    diagnostics: Vec<AnalysisDiagnostic>,
    labels: BTreeMap<String, DefinitionLocation>,
    citations: BTreeMap<String, DefinitionLocation>,
    command_definitions: BTreeMap<String, TextRange>,
    code_actions: Vec<CodeActionSuggestion>,
    text: String,
}

impl LiveAnalysisSnapshot {
    pub fn diagnostics(&self) -> &[AnalysisDiagnostic] {
        &self.diagnostics
    }

    pub fn code_actions(&self) -> &[CodeActionSuggestion] {
        &self.code_actions
    }

    pub fn completions(&self, position: TextPosition) -> Vec<CompletionCandidate> {
        let Some(line_text) = nth_line(&self.text, position.line as usize) else {
            return Vec::new();
        };
        let prefix = line_prefix(line_text, position.character as usize);

        if let Some(partial) = completion_context(&prefix, "\\begin{") {
            return filter_named_items(
                environment_candidates(),
                partial,
                CompletionKind::Environment,
            );
        }

        if let Some(partial) = completion_context(&prefix, "\\ref{") {
            return filter_named_items(
                self.labels.keys().map(String::as_str).collect(),
                partial,
                CompletionKind::Label,
            );
        }

        if let Some(partial) = completion_context(&prefix, "\\cite{") {
            return filter_named_items(
                self.citations.keys().map(String::as_str).collect(),
                partial,
                CompletionKind::Citation,
            );
        }

        if let Some(partial) = command_completion_prefix(&prefix) {
            let mut commands = static_command_candidates();
            commands.extend(self.command_definitions.keys().cloned());
            let commands = dedup_strings(commands);
            return filter_named_items(
                commands.iter().map(String::as_str).collect(),
                partial,
                CompletionKind::Command,
            );
        }

        Vec::new()
    }

    pub fn definition(&self, position: TextPosition) -> Option<DefinitionLocation> {
        let line_text = nth_line(&self.text, position.line as usize)?;
        if let Some((label, _)) =
            braced_argument_at(line_text, position.character as usize, "\\ref{")
        {
            return self.labels.get(&label).cloned();
        }

        if let Some((key, _)) =
            braced_argument_at(line_text, position.character as usize, "\\cite{")
        {
            return self.citations.get(&key).cloned();
        }

        let (command, range) = command_token_at(line_text, position.character as usize)?;
        self.command_definitions
            .get(&command)
            .copied()
            .map(|definition| DefinitionLocation {
                uri: self.uri.clone(),
                range: definition,
            })
            .or_else(|| {
                static_hover_doc(&command).map(|_| DefinitionLocation {
                    uri: self.uri.clone(),
                    range: to_document_range(position.line, range),
                })
            })
    }

    pub fn hover(&self, position: TextPosition) -> Option<HoverInfo> {
        let line_text = nth_line(&self.text, position.line as usize)?;
        let (command, range) = command_token_at(line_text, position.character as usize)?;
        let markdown = static_hover_doc(&command)?;

        Some(HoverInfo {
            range: to_document_range(position.line, range),
            markdown: markdown.to_string(),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct LiveAnalysisSnapshotFactory {
    parser: MinimalLatexParser,
}

impl LiveAnalysisSnapshotFactory {
    pub fn build(
        &self,
        buffer: &OpenDocumentBuffer,
        compile_state: Option<&StableCompileState>,
    ) -> LiveAnalysisSnapshot {
        let mut labels = compile_state_symbol_locations(compile_state, |state| &state.labels);
        labels.extend(collect_symbol_locations(&buffer.uri, &buffer.text, "label"));
        let mut citations = compile_state_symbol_locations(compile_state, |state| &state.citations);
        citations.extend(collect_symbol_locations(
            &buffer.uri,
            &buffer.text,
            "bibitem",
        ));
        let command_definitions = collect_command_definitions(&buffer.text);
        let mut diagnostics = compile_state
            .filter(|state| !state.success)
            .map(|state| compile_diagnostics_to_analysis(&buffer.text, &state.diagnostics))
            .unwrap_or_default();
        diagnostics.extend(collect_buffer_diagnostics(
            &self.parser,
            &buffer.text,
            compile_state.is_none(),
        ));
        let code_actions = collect_code_actions(&buffer.text);

        LiveAnalysisSnapshot {
            uri: buffer.uri.clone(),
            diagnostics,
            labels,
            citations,
            command_definitions,
            code_actions,
            text: buffer.text.clone(),
        }
    }
}

fn collect_buffer_diagnostics(
    parser: &MinimalLatexParser,
    text: &str,
    include_parse_diagnostics: bool,
) -> Vec<AnalysisDiagnostic> {
    let mut diagnostics = Vec::new();
    if include_parse_diagnostics {
        if let Err(error) = parser.parse(text) {
            diagnostics.push(parse_error_to_diagnostic(text, error));
        }
    }
    diagnostics.extend(environment_diagnostics(text));
    diagnostics
}

fn compile_diagnostics_to_analysis(
    text: &str,
    diagnostics: &[Diagnostic],
) -> Vec<AnalysisDiagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| compile_diagnostic_to_analysis(text, diagnostic))
        .collect()
}

fn compile_diagnostic_to_analysis(text: &str, diagnostic: &Diagnostic) -> AnalysisDiagnostic {
    AnalysisDiagnostic {
        range: diagnostic
            .line
            .map(|line| range_for_line(text, line.saturating_sub(1)))
            .unwrap_or_else(|| range_for_line(text, 0)),
        severity: diagnostic.severity,
        message: diagnostic.message.clone(),
        suggestion: diagnostic.suggestion.clone(),
    }
}

fn collect_code_actions(text: &str) -> Vec<CodeActionSuggestion> {
    unclosed_environments(text)
        .into_iter()
        .map(|(environment, _)| {
            let end_position = end_of_document(text);
            CodeActionSuggestion {
                title: format!("Insert \\end{{{environment}}}"),
                edit: TextEdit {
                    range: TextRange::new(
                        end_position.line,
                        end_position.character,
                        end_position.line,
                        end_position.character,
                    ),
                    new_text: format!("\n\\end{{{environment}}}\n"),
                },
            }
        })
        .collect()
}

fn parse_error_to_diagnostic(text: &str, error: ParseError) -> AnalysisDiagnostic {
    let line = error.line().unwrap_or(1).saturating_sub(1);
    AnalysisDiagnostic {
        range: range_for_line(text, line),
        severity: Severity::Error,
        message: error.to_string(),
        suggestion: match error {
            ParseError::MissingDocumentClass => {
                Some("add \\documentclass{article} at the top of the file".to_string())
            }
            ParseError::MissingBeginDocument { .. } => {
                Some("add \\begin{document} before the document body".to_string())
            }
            ParseError::MissingEndDocument { .. } => {
                Some("add \\end{document} at the end of the file".to_string())
            }
            ParseError::UnclosedBrace { .. } => {
                Some("close the outstanding { ... } group".to_string())
            }
            ParseError::InvalidRegisterIndex { .. } => {
                Some("use a count or dimen register between 0 and 32767".to_string())
            }
            ParseError::UnclosedConditional { .. } => {
                Some("add the missing \\fi for the open conditional".to_string())
            }
            ParseError::UnclosedEnvironment { name, .. } => {
                Some(format!("add the matching \\end{{{name}}}"))
            }
            ParseError::UnexpectedElse { .. } => {
                Some("remove the stray \\else or add the matching \\if...".to_string())
            }
            ParseError::UnexpectedFi { .. } => {
                Some("remove the stray \\fi or add the matching \\if...".to_string())
            }
            ParseError::DivisionByZero { .. } => {
                Some("change the divisor to a non-zero integer".to_string())
            }
            ParseError::MacroExpansionLimit { .. } => {
                Some("check for recursive macro definitions and reduce expansion depth".to_string())
            }
            _ => None,
        },
    }
}

fn environment_diagnostics(text: &str) -> Vec<AnalysisDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut stack = Vec::<(String, TextRange)>::new();

    for (line_index, line_text) in text.lines().enumerate() {
        let visible = strip_line_comment(line_text);
        let mut events = Vec::<(usize, bool, String)>::new();
        events.extend(
            find_braced_commands(&visible, "begin")
                .into_iter()
                .map(|(start, name)| (start, true, name)),
        );
        events.extend(
            find_braced_commands(&visible, "end")
                .into_iter()
                .map(|(start, name)| (start, false, name)),
        );
        events.sort_by_key(|(start, _, _)| *start);

        for (start, is_begin, environment) in events {
            let start_character = byte_to_char_index(&visible, start) as u32;
            let command_length = if is_begin {
                format!("\\begin{{{environment}}}").len()
            } else {
                format!("\\end{{{environment}}}").len()
            };
            let end_character = byte_to_char_index(&visible, start + command_length)
                .min(visible.chars().count()) as u32;
            let range = TextRange::new(
                line_index as u32,
                start_character,
                line_index as u32,
                end_character,
            );

            if is_begin {
                stack.push((environment, range));
                continue;
            }

            match stack.last() {
                Some((open_environment, _)) if open_environment == &environment => {
                    stack.pop();
                }
                Some((open_environment, _)) => diagnostics.push(AnalysisDiagnostic {
                    range,
                    severity: Severity::Error,
                    message: format!(
                        "unexpected \\end{{{environment}}} while \\begin{{{open_environment}}} is still open"
                    ),
                    suggestion: Some(format!(
                        "close \\begin{{{open_environment}}} before ending {environment}"
                    )),
                }),
                None => diagnostics.push(AnalysisDiagnostic {
                    range,
                    severity: Severity::Error,
                    message: format!("unexpected \\end{{{environment}}}"),
                    suggestion: Some(format!("remove \\end{{{environment}}} or add the matching \\begin")),
                }),
            }
        }
    }

    diagnostics.extend(
        unclosed_environments(text)
            .into_iter()
            .map(|(environment, range)| AnalysisDiagnostic {
                range,
                severity: Severity::Error,
                message: format!("unclosed environment `{environment}`"),
                suggestion: Some(format!(
                    "insert \\end{{{environment}}} before the document ends"
                )),
            }),
    );

    diagnostics
}

fn unclosed_environments(text: &str) -> Vec<(String, TextRange)> {
    let mut stack = Vec::<(String, TextRange)>::new();

    for (line_index, line_text) in text.lines().enumerate() {
        let visible = strip_line_comment(line_text);
        let mut events = Vec::<(usize, bool, String)>::new();
        events.extend(
            find_braced_commands(&visible, "begin")
                .into_iter()
                .map(|(start, name)| (start, true, name)),
        );
        events.extend(
            find_braced_commands(&visible, "end")
                .into_iter()
                .map(|(start, name)| (start, false, name)),
        );
        events.sort_by_key(|(start, _, _)| *start);

        for (start, is_begin, environment) in events {
            if is_begin {
                let start_character = byte_to_char_index(&visible, start) as u32;
                let end_character =
                    byte_to_char_index(&visible, start + format!("\\begin{{{environment}}}").len())
                        as u32;
                stack.push((
                    environment,
                    TextRange::new(
                        line_index as u32,
                        start_character,
                        line_index as u32,
                        end_character,
                    ),
                ));
            } else if matches!(stack.last(), Some((open_environment, _)) if open_environment == &environment)
            {
                stack.pop();
            }
        }
    }

    stack
}

fn collect_symbol_locations(
    uri: &str,
    text: &str,
    command: &str,
) -> BTreeMap<String, DefinitionLocation> {
    let mut ranges = BTreeMap::new();

    for (line_index, line_text) in text.lines().enumerate() {
        let visible = strip_line_comment(line_text);
        for (start, value) in find_braced_commands(&visible, command) {
            let range = range_for_match(
                line_index as u32,
                &visible,
                start,
                start + format!("\\{command}{{{value}}}").len(),
            );
            ranges.entry(value).or_insert(DefinitionLocation {
                uri: uri.to_string(),
                range,
            });
        }
    }

    ranges
}

fn compile_state_symbol_locations(
    compile_state: Option<&StableCompileState>,
    select: impl Fn(&DocumentState) -> &BTreeMap<String, SymbolLocation>,
) -> BTreeMap<String, DefinitionLocation> {
    compile_state
        .map(|state| {
            select(&state.document_state)
                .iter()
                .map(|(name, location)| {
                    (
                        name.clone(),
                        DefinitionLocation {
                            uri: path_to_file_uri(&location.file),
                            range: TextRange::new(
                                location.line.saturating_sub(1),
                                location.column,
                                location.line.saturating_sub(1),
                                location.column,
                            ),
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn path_to_file_uri(path: &str) -> String {
    if path.starts_with("file://") {
        path.to_string()
    } else {
        format!("file://{path}")
    }
}

fn collect_command_definitions(text: &str) -> BTreeMap<String, TextRange> {
    let mut definitions = BTreeMap::new();

    for (line_index, line_text) in text.lines().enumerate() {
        let visible = strip_line_comment(line_text);

        for command in ["newcommand", "renewcommand"] {
            for (start, value) in find_braced_commands(&visible, command) {
                if let Some(name) = value.strip_prefix('\\') {
                    let range = range_for_match(
                        line_index as u32,
                        &visible,
                        start,
                        start + format!("\\{command}{{{value}}}").len(),
                    );
                    definitions.entry(name.to_string()).or_insert(range);
                }
            }
        }

        for (command, start, end) in find_simple_command_definitions(&visible) {
            let range = range_for_match(line_index as u32, &visible, start, end);
            definitions.entry(command).or_insert(range);
        }
    }

    definitions
}

fn find_simple_command_definitions(line: &str) -> Vec<(String, usize, usize)> {
    let mut definitions = Vec::new();
    for keyword in ["\\def\\", "\\gdef\\"] {
        let mut search_offset = 0usize;
        while let Some(found) = line[search_offset..].find(keyword) {
            let start = search_offset + found;
            let name_start = start + keyword.len();
            let name_length = line[name_start..]
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '@')
                .map(char::len_utf8)
                .sum::<usize>();
            if name_length > 0 {
                let end = name_start + name_length;
                definitions.push((line[name_start..end].to_string(), start, end));
            }
            search_offset = name_start.saturating_add(name_length.max(1));
        }
    }
    definitions
}

fn find_braced_commands(line: &str, command: &str) -> Vec<(usize, String)> {
    let needle = format!("\\{command}{{");
    let mut search_offset = 0usize;
    let mut matches = Vec::new();

    while let Some(found) = line[search_offset..].find(&needle) {
        let start = search_offset + found;
        let value_start = start + needle.len();
        let Some(value_end_relative) = line[value_start..].find('}') else {
            break;
        };
        let value_end = value_start + value_end_relative;
        matches.push((start, line[value_start..value_end].to_string()));
        search_offset = value_end + 1;
    }

    matches
}

fn completion_context<'a>(prefix: &'a str, trigger: &str) -> Option<&'a str> {
    let start = prefix.rfind(trigger)?;
    let partial = &prefix[start + trigger.len()..];
    if partial.contains('}') {
        None
    } else {
        Some(partial)
    }
}

fn command_completion_prefix(prefix: &str) -> Option<&str> {
    let start = prefix.rfind('\\')?;
    let partial = &prefix[start + 1..];
    if partial
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '@')
    {
        Some(partial)
    } else {
        None
    }
}

fn filter_named_items(
    candidates: Vec<&str>,
    partial: &str,
    kind: CompletionKind,
) -> Vec<CompletionCandidate> {
    candidates
        .into_iter()
        .filter(|candidate| candidate.starts_with(partial))
        .map(|candidate| CompletionCandidate {
            label: candidate.to_string(),
            kind,
            detail: None,
        })
        .collect()
}

fn dedup_strings(values: Vec<String>) -> Vec<String> {
    let mut ordered = BTreeSet::new();
    ordered.extend(values);
    ordered.into_iter().collect()
}

fn static_command_candidates() -> Vec<String> {
    vec![
        "begin",
        "cite",
        "documentclass",
        "end",
        "frac",
        "includegraphics",
        "label",
        "ref",
        "section",
        "subsection",
        "usepackage",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn environment_candidates() -> Vec<&'static str> {
    vec![
        "align",
        "aligned",
        "document",
        "enumerate",
        "equation",
        "figure",
        "itemize",
        "table",
    ]
}

fn static_hover_doc(command: &str) -> Option<&'static str> {
    match command {
        "frac" => {
            Some("`\\frac{numerator}{denominator}` inserts a fraction.\n\nExample: `\\frac{a}{b}`.")
        }
        "includegraphics" => {
            Some("`\\includegraphics[options]{file}` inserts an image from `graphicx`.")
        }
        "section" => Some("`\\section{title}` starts a numbered section heading."),
        "label" => Some("`\\label{key}` defines a cross-reference anchor."),
        "ref" => Some("`\\ref{key}` resolves a previously defined label."),
        "cite" => Some("`\\cite{key}` inserts a bibliography citation."),
        "begin" => Some("`\\begin{environment}` starts an environment."),
        "end" => Some("`\\end{environment}` closes an environment."),
        _ => None,
    }
}

fn braced_argument_at(line: &str, character: usize, trigger: &str) -> Option<(String, TextRange)> {
    let cursor = char_to_byte_index(line, character);
    let mut search_offset = 0usize;
    while let Some(found) = line[search_offset..].find(trigger) {
        let start = search_offset + found;
        let value_start = start + trigger.len();
        let Some(value_end_relative) = line[value_start..].find('}') else {
            break;
        };
        let value_end = value_start + value_end_relative;
        if cursor >= value_start && cursor <= value_end {
            let range = range_for_match(0, line, value_start, value_end);
            return Some((line[value_start..value_end].to_string(), range));
        }
        search_offset = value_end + 1;
    }
    None
}

fn command_token_at(line: &str, character: usize) -> Option<(String, TextRange)> {
    let chars = line.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return None;
    }

    let mut cursor = character.min(chars.len().saturating_sub(1));
    while cursor > 0 && !matches!(chars[cursor], '\\') && is_command_char(chars[cursor]) {
        cursor -= 1;
    }

    let start = if chars[cursor] == '\\' {
        cursor
    } else if chars[0] == '\\' && cursor == 0 {
        0
    } else {
        return None;
    };

    let mut end = start + 1;
    while end < chars.len() && is_command_char(chars[end]) {
        end += 1;
    }

    if character < start || character > end {
        return None;
    }

    let command = chars[start + 1..end].iter().collect::<String>();
    if command.is_empty() {
        return None;
    }

    Some((command, TextRange::new(0, start as u32, 0, end as u32)))
}

fn is_command_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '@'
}

fn strip_line_comment(line: &str) -> String {
    let mut visible = String::with_capacity(line.len());
    let mut escaped = false;

    for ch in line.chars() {
        if escaped {
            visible.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' => {
                visible.push(ch);
                escaped = true;
            }
            '%' => break,
            _ => visible.push(ch),
        }
    }

    visible
}

fn line_prefix(line: &str, character: usize) -> String {
    line.chars().take(character).collect()
}

fn nth_line(text: &str, line: usize) -> Option<&str> {
    text.lines().nth(line)
}

fn range_for_line(text: &str, line: u32) -> TextRange {
    let line_index = line as usize;
    let line_text = nth_line(text, line_index).unwrap_or("");
    TextRange::new(line, 0, line, line_text.chars().count() as u32)
}

fn range_for_match(line: u32, visible_line: &str, start: usize, end: usize) -> TextRange {
    TextRange::new(
        line,
        byte_to_char_index(visible_line, start) as u32,
        line,
        byte_to_char_index(visible_line, end) as u32,
    )
}

fn to_document_range(line: u32, range: TextRange) -> TextRange {
    TextRange::new(line, range.start.character, line, range.end.character)
}

fn end_of_document(text: &str) -> TextPosition {
    let line_count = text.lines().count();
    if line_count == 0 {
        return TextPosition {
            line: 0,
            character: 0,
        };
    }

    let last_line_index = line_count.saturating_sub(1);
    let last_line = nth_line(text, last_line_index).unwrap_or("");
    TextPosition {
        line: last_line_index as u32,
        character: last_line.chars().count() as u32,
    }
}

fn char_to_byte_index(line: &str, character: usize) -> usize {
    line.char_indices()
        .nth(character)
        .map(|(index, _)| index)
        .unwrap_or(line.len())
}

fn byte_to_char_index(line: &str, byte_index: usize) -> usize {
    line[..byte_index.min(line.len())].chars().count()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::{CompletionKind, LiveAnalysisSnapshotFactory, TextPosition};
    use crate::open_document_store::OpenDocumentBuffer;
    use crate::stable_compile_state::StableCompileState;
    use ferritex_core::compilation::{CompilationSnapshot, DocumentState, SymbolLocation};
    use ferritex_core::diagnostics::{Diagnostic, Severity};

    fn buffer(text: &str) -> OpenDocumentBuffer {
        OpenDocumentBuffer {
            uri: "file:///main.tex".to_string(),
            language_id: "latex".to_string(),
            version: 1,
            text: text.to_string(),
        }
    }

    fn compile_state(
        success: bool,
        diagnostics: Vec<Diagnostic>,
        document_state: DocumentState,
    ) -> StableCompileState {
        StableCompileState {
            snapshot: CompilationSnapshot {
                pass_number: 1,
                primary_input: PathBuf::from("/tmp/main.tex"),
                jobname: "main".to_string(),
            },
            document_state,
            cross_reference_seed: Default::default(),
            page_count: 0,
            success,
            diagnostics,
        }
    }

    #[test]
    fn reports_unclosed_environment_and_quick_fix() {
        let snapshot = LiveAnalysisSnapshotFactory::default().build(
            &buffer(
            "\\documentclass{article}\n\\begin{document}\n\\begin{equation}\na=b\n\\end{document}\n",
            ),
            None,
        );

        assert!(snapshot.diagnostics().iter().any(|diagnostic| diagnostic
            .message
            .contains("unclosed environment `equation`")));
        assert!(snapshot
            .code_actions()
            .iter()
            .any(|action| action.title.contains("\\end{equation}")));
    }

    #[test]
    fn completion_includes_unsaved_labels() {
        let snapshot = LiveAnalysisSnapshotFactory::default().build(
            &buffer(
            "\\documentclass{article}\n\\begin{document}\n\\label{fig:overview}\n\\ref{fig:\n\\end{document}\n",
            ),
            None,
        );

        let completions = snapshot.completions(TextPosition {
            line: 3,
            character: 9,
        });

        assert!(completions.iter().any(|candidate| {
            candidate.label == "fig:overview" && candidate.kind == CompletionKind::Label
        }));
    }

    #[test]
    fn resolves_macro_definitions() {
        let snapshot = LiveAnalysisSnapshotFactory::default().build(
            &buffer(
            "\\documentclass{article}\n\\newcommand{\\foo}{bar}\n\\begin{document}\n\\foo\n\\end{document}\n",
            ),
            None,
        );

        let definition = snapshot
            .definition(TextPosition {
                line: 3,
                character: 1,
            })
            .expect("macro definition");

        assert_eq!(definition.range.start.line, 1);
    }

    #[test]
    fn hover_returns_static_command_docs() {
        let snapshot = LiveAnalysisSnapshotFactory::default().build(
            &buffer("\\documentclass{article}\n\\begin{document}\n\\frac{a}{b}\n\\end{document}\n"),
            None,
        );

        let hover = snapshot
            .hover(TextPosition {
                line: 2,
                character: 2,
            })
            .expect("hover information");

        assert!(hover.markdown.contains("\\frac"));
    }

    #[test]
    fn compile_state_none_falls_back_to_buffer_only() {
        let snapshot = LiveAnalysisSnapshotFactory::default().build(
            &buffer("\\documentclass{article}\n\\begin{document}\nHello\n"),
            None,
        );

        assert!(snapshot
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.message.contains("missing \\end{document}")));
    }

    #[test]
    fn compile_state_success_adds_no_extra_diagnostics() {
        let snapshot = LiveAnalysisSnapshotFactory::default().build(
            &buffer(
                "\\documentclass{article}\n\\begin{document}\n\\begin{equation}\na=b\n\\end{document}\n",
            ),
            Some(&compile_state(true, Vec::new(), DocumentState::default())),
        );

        let messages = snapshot
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();

        assert!(!messages
            .iter()
            .any(|message| message.contains("missing \\end{document}")));
        assert!(messages
            .iter()
            .any(|message| message.contains("unclosed environment `equation`")));
    }

    #[test]
    fn compile_state_failure_merges_compile_diagnostics() {
        let snapshot = LiveAnalysisSnapshotFactory::default().build(
            &buffer("\\documentclass{article}\n\\begin{document}\n\\begin{equation}\na=b\n"),
            Some(&compile_state(
                false,
                vec![Diagnostic::new(Severity::Error, "missing \\end{document}")
                    .with_line(4)
                    .with_suggestion("add \\end{document} at the end of the file")],
                DocumentState::default(),
            )),
        );

        let messages = snapshot
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();

        assert_eq!(messages[0], "missing \\end{document}");
        assert!(messages
            .iter()
            .skip(1)
            .any(|message| message.contains("unclosed environment `equation`")));
    }

    #[test]
    fn completion_merges_compile_state_symbols() {
        let mut labels = BTreeMap::new();
        labels.insert(
            "fig:external".to_string(),
            SymbolLocation {
                file: "/tmp/chapters/figures.tex".to_string(),
                line: 12,
                column: 4,
            },
        );
        let snapshot = LiveAnalysisSnapshotFactory::default().build(
            &buffer("\\documentclass{article}\n\\begin{document}\n\\ref{fig:\n\\end{document}\n"),
            Some(&compile_state(
                true,
                Vec::new(),
                DocumentState {
                    labels,
                    ..DocumentState::default()
                },
            )),
        );

        let completions = snapshot.completions(TextPosition {
            line: 2,
            character: 9,
        });

        assert!(completions.iter().any(|candidate| {
            candidate.label == "fig:external" && candidate.kind == CompletionKind::Label
        }));
    }

    #[test]
    fn definition_can_jump_to_compile_state_symbol_in_another_file() {
        let mut labels = BTreeMap::new();
        labels.insert(
            "fig:external".to_string(),
            SymbolLocation {
                file: "/tmp/chapters/figures.tex".to_string(),
                line: 12,
                column: 4,
            },
        );
        let snapshot = LiveAnalysisSnapshotFactory::default().build(
            &buffer(
                "\\documentclass{article}\n\\begin{document}\nSee \\ref{fig:external}.\n\\end{document}\n",
            ),
            Some(&compile_state(
                true,
                Vec::new(),
                DocumentState {
                    labels,
                    ..DocumentState::default()
                },
            )),
        );

        let definition = snapshot
            .definition(TextPosition {
                line: 2,
                character: 14,
            })
            .expect("definition");

        assert_eq!(definition.uri, "file:///tmp/chapters/figures.tex");
        assert_eq!(definition.range.start.line, 11);
        assert_eq!(definition.range.start.character, 4);
    }
}
