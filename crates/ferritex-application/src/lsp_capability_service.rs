#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextDocumentSyncKind {
    None = 0,
    Full = 1,
    Incremental = 2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionOptions {
    pub trigger_characters: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCapabilities {
    pub text_document_sync: TextDocumentSyncKind,
    pub completion_provider: CompletionOptions,
    pub code_action_provider: bool,
    pub definition_provider: bool,
    pub hover_provider: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspCapabilityService {
    definition_provider_enabled: bool,
    hover_provider_enabled: bool,
}

impl LspCapabilityService {
    pub const fn new(definition_provider_enabled: bool, hover_provider_enabled: bool) -> Self {
        Self {
            definition_provider_enabled,
            hover_provider_enabled,
        }
    }

    pub fn capabilities(&self) -> ServerCapabilities {
        ServerCapabilities {
            text_document_sync: TextDocumentSyncKind::Full,
            completion_provider: CompletionOptions {
                trigger_characters: vec!["\\".to_string(), "{".to_string()],
            },
            code_action_provider: true,
            definition_provider: self.definition_provider_enabled,
            hover_provider: self.hover_provider_enabled,
        }
    }
}

impl Default for LspCapabilityService {
    fn default() -> Self {
        Self::new(true, true)
    }
}

#[cfg(test)]
mod tests {
    use super::{LspCapabilityService, TextDocumentSyncKind};

    #[test]
    fn advertises_required_capabilities() {
        let capabilities = LspCapabilityService::default().capabilities();

        assert_eq!(capabilities.text_document_sync, TextDocumentSyncKind::Full);
        assert!(capabilities.code_action_provider);
        assert!(capabilities.definition_provider);
        assert!(capabilities.hover_provider);
        assert_eq!(
            capabilities.completion_provider.trigger_characters,
            vec!["\\".to_string(), "{".to_string()]
        );
    }

    #[test]
    fn optional_providers_can_be_disabled() {
        let capabilities = LspCapabilityService::new(false, false).capabilities();

        assert!(!capabilities.definition_provider);
        assert!(!capabilities.hover_provider);
    }
}
