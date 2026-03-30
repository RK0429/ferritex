use super::{CatCode, Token, TokenKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipOutcome {
    Resumed,
    EndOfInput,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConditionalState {
    stack: Vec<ConditionalFrame>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConditionalFrame {
    line: u32,
    executing: bool,
    kind: ConditionalKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConditionalKind {
    Simple {
        else_seen: bool,
    },
    Case {
        target_case: i32,
        current_case: i32,
        matched: bool,
        else_seen: bool,
    },
}

impl ConditionalState {
    pub fn is_skipping(&self) -> bool {
        self.stack.last().is_some_and(|frame| !frame.executing)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    pub fn current_open_line(&self) -> Option<u32> {
        self.stack.last().map(|frame| frame.line)
    }

    pub fn top_is_ifcase(&self) -> bool {
        self.stack
            .last()
            .is_some_and(|frame| matches!(frame.kind, ConditionalKind::Case { .. }))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn process_if(&mut self, condition: bool) {
        self.process_if_at(condition, 0);
    }

    pub fn process_if_at(&mut self, condition: bool, line: u32) {
        self.stack.push(ConditionalFrame {
            line,
            executing: condition,
            kind: ConditionalKind::Simple { else_seen: false },
        });
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn process_ifcase(&mut self, value: i32) {
        self.process_ifcase_at(value, 0);
    }

    pub fn process_ifcase_at(&mut self, value: i32, line: u32) {
        self.stack.push(ConditionalFrame {
            line,
            executing: value == 0,
            kind: ConditionalKind::Case {
                target_case: value,
                current_case: 0,
                matched: value == 0,
                else_seen: false,
            },
        });
    }

    pub fn process_else(&mut self) -> bool {
        let Some(frame) = self.stack.last_mut() else {
            return false;
        };

        match &mut frame.kind {
            ConditionalKind::Simple { else_seen } => {
                if *else_seen {
                    frame.executing = false;
                } else {
                    frame.executing = !frame.executing;
                    *else_seen = true;
                }
            }
            ConditionalKind::Case {
                matched, else_seen, ..
            } => {
                if *else_seen {
                    frame.executing = false;
                } else {
                    frame.executing = !*matched;
                    *else_seen = true;
                }
            }
        }

        true
    }

    pub fn process_or(&mut self) -> bool {
        let Some(frame) = self.stack.last_mut() else {
            return false;
        };

        let ConditionalKind::Case {
            target_case,
            current_case,
            matched,
            else_seen,
        } = &mut frame.kind
        else {
            return false;
        };

        if *else_seen {
            frame.executing = false;
            return true;
        }

        *current_case += 1;
        if *matched {
            frame.executing = false;
        } else if *target_case == *current_case {
            *matched = true;
            frame.executing = true;
        } else {
            frame.executing = false;
        }

        true
    }

    pub fn process_fi(&mut self) -> bool {
        self.stack.pop().is_some()
    }

    pub fn skip_false_branch<F>(&mut self, mut token_source: F) -> SkipOutcome
    where
        F: FnMut() -> Option<Token>,
    {
        let mut nested_depth = 0usize;

        loop {
            let Some(token) = token_source() else {
                return SkipOutcome::EndOfInput;
            };

            if control_sequence_name(&token) == Some("unless") {
                if consume_unless_conditional_start(&mut token_source) {
                    nested_depth += 1;
                }
                continue;
            }

            if is_conditional_start(&token) {
                nested_depth += 1;
                continue;
            }

            let Some(name) = control_sequence_name(&token) else {
                continue;
            };

            match name {
                "fi" => {
                    if nested_depth == 0 {
                        let _ = self.process_fi();
                        return SkipOutcome::Resumed;
                    }
                    nested_depth -= 1;
                }
                "else" if nested_depth == 0 => {
                    let _ = self.process_else();
                    if !self.is_skipping() {
                        return SkipOutcome::Resumed;
                    }
                }
                "or" if nested_depth == 0 && self.top_is_ifcase() => {
                    let _ = self.process_or();
                    if !self.is_skipping() {
                        return SkipOutcome::Resumed;
                    }
                }
                _ => {}
            }
        }
    }
}

pub fn evaluate_ifnum(left: i32, relation: char, right: i32) -> bool {
    match relation {
        '<' => left < right,
        '=' => left == right,
        '>' => left > right,
        _ => false,
    }
}

pub fn tokens_equal(left: &Token, right: &Token) -> bool {
    left.kind == right.kind
}

fn is_conditional_start(token: &Token) -> bool {
    control_sequence_name(token).is_some_and(|name| {
        matches!(
            name,
            "unless"
                | "iftrue"
                | "iffalse"
                | "ifnum"
                | "ifx"
                | "ifcase"
                | "ifdim"
                | "ifodd"
                | "ifvoid"
                | "ifhbox"
                | "ifvbox"
                | "ifeof"
                | "ifhmode"
                | "ifvmode"
                | "ifmmode"
                | "ifinner"
                | "ifcat"
                | "if"
                | "ifdefined"
                | "ifcsname"
        )
    })
}

fn consume_unless_conditional_start<F>(token_source: &mut F) -> bool
where
    F: FnMut() -> Option<Token>,
{
    loop {
        let Some(token) = token_source() else {
            return false;
        };

        match token.kind {
            TokenKind::CharToken {
                cat: CatCode::Space,
                ..
            } => continue,
            TokenKind::ControlWord(ref name) if name == "par" => continue,
            _ => return is_conditional_start(&token),
        }
    }
}

fn control_sequence_name(token: &Token) -> Option<&str> {
    match &token.kind {
        TokenKind::ControlWord(name) => Some(name.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::{
        evaluate_ifnum, is_conditional_start, tokens_equal, ConditionalState, SkipOutcome,
    };
    use crate::parser::{CatCode, Token, TokenKind};

    #[test]
    fn iftrue_and_iffalse_update_skip_state() {
        let mut state = ConditionalState::default();

        state.process_if(true);
        assert!(!state.is_skipping());
        assert!(state.process_fi());

        state.process_if(false);
        assert!(state.is_skipping());
    }

    #[test]
    fn nested_conditionals_follow_top_frame() {
        let mut state = ConditionalState::default();

        state.process_if(true);
        state.process_if(false);
        assert!(state.is_skipping());

        assert!(state.process_fi());
        assert!(!state.is_skipping());
        assert!(state.process_fi());
        assert!(state.is_empty());
    }

    #[test]
    fn evaluates_ifnum_relations() {
        assert!(evaluate_ifnum(1, '<', 2));
        assert!(evaluate_ifnum(2, '=', 2));
        assert!(evaluate_ifnum(3, '>', 2));
        assert!(!evaluate_ifnum(2, '<', 1));
    }

    #[test]
    fn compares_tokens_for_ifx() {
        assert!(tokens_equal(&char_token('a'), &char_token('a')));
        assert!(!tokens_equal(&char_token('a'), &char_token('b')));
        assert!(tokens_equal(&control_word("foo"), &control_word("foo")));
    }

    #[test]
    fn ifcase_activates_matching_branch() {
        let mut state = ConditionalState::default();
        state.process_ifcase(1);

        assert!(state.is_skipping());
        assert!(state.process_or());
        assert!(!state.is_skipping());
    }

    #[test]
    fn conditional_start_uses_primitive_whitelist() {
        assert!(is_conditional_start(&control_word("iftrue")));
        assert!(is_conditional_start(&control_word("unless")));
        assert!(is_conditional_start(&control_word("ifdefined")));
        assert!(is_conditional_start(&control_word("ifcsname")));
        assert!(!is_conditional_start(&control_word("ifmycondition")));
    }

    #[test]
    fn else_and_fi_update_stack() {
        let mut state = ConditionalState::default();
        state.process_if(true);

        assert!(state.process_else());
        assert!(state.is_skipping());
        assert!(state.process_fi());
        assert!(state.is_empty());
    }

    #[test]
    fn skip_false_branch_respects_nested_conditionals() {
        let mut state = ConditionalState::default();
        state.process_if(false);

        let mut tokens = VecDeque::from(vec![
            char_token('a'),
            control_word("iftrue"),
            char_token('b'),
            control_word("fi"),
            control_word("else"),
            char_token('c'),
            control_word("fi"),
        ]);

        let outcome = state.skip_false_branch(|| tokens.pop_front());

        assert_eq!(outcome, SkipOutcome::Resumed);
        assert!(!state.is_skipping());
        assert_eq!(tokens.pop_front(), Some(char_token('c')));
    }

    #[test]
    fn skip_false_branch_counts_unless_prefixed_conditionals_once() {
        let mut state = ConditionalState::default();
        state.process_if(false);

        let mut tokens = VecDeque::from(vec![
            control_word("unless"),
            control_word("iftrue"),
            char_token('a'),
            control_word("fi"),
            control_word("else"),
            char_token('b'),
            control_word("fi"),
        ]);

        let outcome = state.skip_false_branch(|| tokens.pop_front());

        assert_eq!(outcome, SkipOutcome::Resumed);
        assert!(!state.is_skipping());
        assert_eq!(tokens.pop_front(), Some(char_token('b')));
    }

    fn control_word(name: &str) -> Token {
        Token {
            kind: TokenKind::ControlWord(name.to_string()),
            line: 1,
            column: 1,
        }
    }

    fn char_token(char: char) -> Token {
        Token {
            kind: TokenKind::CharToken {
                char,
                cat: CatCode::Other,
            },
            line: 1,
            column: 1,
        }
    }
}
