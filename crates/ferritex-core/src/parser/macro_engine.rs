use std::collections::HashMap;

use super::{CatCode, Token, TokenKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroDef {
    pub name: String,
    pub parameter_count: usize,
    pub body: Vec<Token>,
    pub protected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentDef {
    pub name: String,
    pub begin_tokens: Vec<Token>,
    pub end_tokens: Vec<Token>,
    pub parameter_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroEngine {
    scope_stack: Vec<HashMap<String, MacroDef>>,
    environment_scope_stack: Vec<HashMap<String, EnvironmentDef>>,
    catcode_stack: Vec<HashMap<u8, CatCode>>,
}

impl Default for MacroEngine {
    fn default() -> Self {
        Self {
            scope_stack: vec![HashMap::new()],
            environment_scope_stack: vec![HashMap::new()],
            catcode_stack: vec![HashMap::new()],
        }
    }
}

impl MacroEngine {
    pub fn push_group(&mut self) {
        let next_scope = self.scope_stack.last().cloned().unwrap_or_default();
        let next_environment_scope = self
            .environment_scope_stack
            .last()
            .cloned()
            .unwrap_or_default();
        let next_catcodes = self.catcode_stack.last().cloned().unwrap_or_default();
        self.scope_stack.push(next_scope);
        self.environment_scope_stack.push(next_environment_scope);
        self.catcode_stack.push(next_catcodes);
    }

    pub fn pop_group(&mut self) {
        if self.scope_stack.len() > 1 {
            let _ = self.scope_stack.pop();
        }
        if self.environment_scope_stack.len() > 1 {
            let _ = self.environment_scope_stack.pop();
        }
        if self.catcode_stack.len() > 1 {
            let _ = self.catcode_stack.pop();
        }
    }

    pub fn define_local(&mut self, name: String, def: MacroDef) {
        self.scope_stack
            .last_mut()
            .expect("macro engine always has at least one scope")
            .insert(name, def);
    }

    pub fn define_global(&mut self, name: String, def: MacroDef) {
        for scope in &mut self.scope_stack {
            scope.insert(name.clone(), def.clone());
        }
    }

    pub fn let_assign(&mut self, target: String, source_def: Option<MacroDef>, global: bool) {
        match source_def {
            Some(mut definition) => {
                definition.name = target.clone();
                if global {
                    self.define_global(target, definition);
                } else {
                    self.define_local(target, definition);
                }
            }
            None if global => {
                for scope in &mut self.scope_stack {
                    let _ = scope.remove(&target);
                }
            }
            None => {
                let _ = self
                    .scope_stack
                    .last_mut()
                    .expect("macro engine always has at least one scope")
                    .remove(&target);
            }
        }
    }

    pub fn lookup(&self, name: &str) -> Option<&MacroDef> {
        self.scope_stack.last().and_then(|scope| scope.get(name))
    }

    pub fn define_environment(&mut self, name: String, def: EnvironmentDef) {
        self.environment_scope_stack
            .last_mut()
            .expect("macro engine always has at least one environment scope")
            .insert(name, def);
    }

    pub fn define_global_environment(&mut self, name: String, def: EnvironmentDef) {
        for scope in &mut self.environment_scope_stack {
            scope.insert(name.clone(), def.clone());
        }
    }

    pub fn lookup_environment(&self, name: &str) -> Option<&EnvironmentDef> {
        self.environment_scope_stack
            .last()
            .and_then(|scope| scope.get(name))
    }

    pub fn expand(&self, name: &str, args: &[Vec<Token>]) -> Vec<Token> {
        let Some(definition) = self.lookup(name) else {
            return Vec::new();
        };

        let mut expanded = Vec::with_capacity(definition.body.len());
        for token in &definition.body {
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

    pub fn set_catcode(&mut self, char_code: u8, cat: CatCode) {
        self.catcode_stack
            .last_mut()
            .expect("macro engine always has at least one catcode scope")
            .insert(char_code, cat);
    }

    pub fn get_catcode_overrides(&self) -> Vec<(u8, CatCode)> {
        let mut overrides = self
            .catcode_stack
            .last()
            .expect("macro engine always has at least one catcode scope")
            .iter()
            .map(|(char_code, cat)| (*char_code, *cat))
            .collect::<Vec<_>>();
        overrides.sort_by_key(|(char_code, _)| *char_code);
        overrides
    }
}

#[cfg(test)]
mod tests {
    use super::{CatCode, EnvironmentDef, MacroDef, MacroEngine, Token, TokenKind};

    #[test]
    fn expands_macros_with_zero_one_and_two_arguments() {
        let mut engine = MacroEngine::default();
        engine.define_local(
            "zero".to_string(),
            macro_def("zero", 0, vec![char_token('a'), char_token('b')]),
        );
        engine.define_local(
            "one".to_string(),
            macro_def(
                "one",
                1,
                vec![char_token('['), parameter(1), char_token(']')],
            ),
        );
        engine.define_local(
            "two".to_string(),
            macro_def("two", 2, vec![parameter(1), char_token('+'), parameter(2)]),
        );

        assert_eq!(
            engine.expand("zero", &[]),
            vec![char_token('a'), char_token('b')]
        );
        assert_eq!(
            engine.expand("one", &[vec![char_token('x')]]),
            vec![char_token('['), char_token('x'), char_token(']')]
        );
        assert_eq!(
            engine.expand("two", &[vec![char_token('x')], vec![char_token('y')]],),
            vec![char_token('x'), char_token('+'), char_token('y')]
        );
    }

    #[test]
    fn group_scope_push_and_pop_rolls_back_local_definitions() {
        let mut engine = MacroEngine::default();
        engine.define_local(
            "name".to_string(),
            macro_def("name", 0, vec![char_token('a')]),
        );

        engine.push_group();
        engine.define_local(
            "name".to_string(),
            macro_def("name", 0, vec![char_token('b')]),
        );
        assert_eq!(engine.expand("name", &[]), vec![char_token('b')]);

        engine.pop_group();
        assert_eq!(engine.expand("name", &[]), vec![char_token('a')]);
    }

    #[test]
    fn gdef_like_global_definition_survives_group_pop() {
        let mut engine = MacroEngine::default();

        engine.push_group();
        engine.define_global(
            "name".to_string(),
            macro_def("name", 0, vec![char_token('g')]),
        );
        engine.pop_group();

        assert_eq!(engine.expand("name", &[]), vec![char_token('g')]);
    }

    #[test]
    fn catcode_storage_is_scoped() {
        let mut engine = MacroEngine::default();
        engine.set_catcode(b'@', CatCode::Letter);
        assert_eq!(
            engine.get_catcode_overrides(),
            vec![(b'@', CatCode::Letter)]
        );

        engine.push_group();
        engine.set_catcode(b'!', CatCode::Active);
        assert_eq!(
            engine.get_catcode_overrides(),
            vec![(b'!', CatCode::Active), (b'@', CatCode::Letter)]
        );

        engine.pop_group();
        assert_eq!(
            engine.get_catcode_overrides(),
            vec![(b'@', CatCode::Letter)]
        );
    }

    #[test]
    fn environment_storage_is_scoped() {
        let mut engine = MacroEngine::default();
        engine.define_environment(
            "outer".to_string(),
            environment_def("outer", 0, vec![char_token('a')], vec![char_token('z')]),
        );

        engine.push_group();
        engine.define_environment(
            "outer".to_string(),
            environment_def("outer", 0, vec![char_token('b')], vec![char_token('y')]),
        );
        assert_eq!(
            engine
                .lookup_environment("outer")
                .map(|definition| definition.begin_tokens.clone()),
            Some(vec![char_token('b')])
        );

        engine.pop_group();
        assert_eq!(
            engine
                .lookup_environment("outer")
                .map(|definition| definition.begin_tokens.clone()),
            Some(vec![char_token('a')])
        );
    }

    #[test]
    fn global_environment_definition_survives_group_pop() {
        let mut engine = MacroEngine::default();

        engine.push_group();
        engine.define_global_environment(
            "global".to_string(),
            environment_def("global", 0, vec![char_token('a')], vec![char_token('z')]),
        );
        engine.pop_group();

        assert!(engine.lookup_environment("global").is_some());
    }

    fn macro_def(name: &str, parameter_count: usize, body: Vec<Token>) -> MacroDef {
        MacroDef {
            name: name.to_string(),
            parameter_count,
            body,
            protected: false,
        }
    }

    fn environment_def(
        name: &str,
        parameter_count: usize,
        begin_tokens: Vec<Token>,
        end_tokens: Vec<Token>,
    ) -> EnvironmentDef {
        EnvironmentDef {
            name: name.to_string(),
            begin_tokens,
            end_tokens,
            parameter_count,
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

    fn parameter(index: u8) -> Token {
        Token {
            kind: TokenKind::Parameter(index),
            line: 1,
            column: 1,
        }
    }
}
