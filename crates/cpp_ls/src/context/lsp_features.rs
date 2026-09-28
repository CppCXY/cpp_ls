use lsp_types::ClientCapabilities;

#[derive(Debug)]
pub struct LspFeatures {
    client_capabilities: ClientCapabilities,
}

#[allow(unused)]
impl LspFeatures {
    pub fn new(client_capabilities: ClientCapabilities) -> Self {
        Self {
            client_capabilities,
        }
    }

    pub fn supports_multiline_tokens(&self) -> bool {
        self.client_capabilities
            .text_document
            .as_ref()
            .and_then(|text_document| text_document.semantic_tokens.as_ref())
            .and_then(|semantic_tokens| semantic_tokens.multiline_token_support)
            .unwrap_or_default()
    }

    /// **The token types the client says it can draw** — `None` when it does not do semantic tokens at all.
    ///
    /// The server's legend is fixed (the numbers in an answer are indices into it), so this is a *filter* rather
    /// than a legend: a kind the client cannot draw is left out of the answer instead of being sent as a number it
    /// maps to nothing. An empty list is treated as "said nothing" by the caller, because a client that asks for
    /// tokens without naming any type is one this server cannot filter for.
    pub fn semantic_token_types(&self) -> Option<Vec<lsp_types::SemanticTokenType>> {
        self.client_capabilities
            .text_document
            .as_ref()
            .and_then(|text_document| text_document.semantic_tokens.as_ref())
            .map(|semantic_tokens| semantic_tokens.token_types.clone())
    }

    pub fn supports_config_request(&self) -> bool {
        self.client_capabilities
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.configuration)
            .unwrap_or_default()
    }

    pub fn supports_work_done_progress(&self) -> bool {
        self.client_capabilities
            .window
            .as_ref()
            .and_then(|window| window.work_done_progress)
            .unwrap_or_default()
    }

    /// **Can the client interpolate a snippet body?**
    ///
    /// The one capability a completion has to branch on, because the cost of guessing wrong is asymmetric: a client
    /// that supports snippets and is not sent any loses a convenience, while a client that does not and is sent one
    /// writes `${1:condition}` into the file as literal text.
    ///
    /// `snippetSupport` is the protocol's flag for exactly this question, and its absence is a **no**: the field's
    /// own note says a client that does not set it is assumed to take plain text only. That default matters here
    /// more than usual, because the failure is not a missing feature — it is placeholder syntax written into the
    /// user's file, which they then have to delete.
    pub fn supports_snippets(&self) -> bool {
        self.client_capabilities
            .text_document
            .as_ref()
            .and_then(|text_document| text_document.completion.as_ref())
            .and_then(|completion| completion.completion_item.as_ref())
            .and_then(|item| item.snippet_support)
            .unwrap_or(false)
    }

    pub fn supports_pull_diagnostic(&self) -> bool {
        if let Some(text_document) = &self.client_capabilities.text_document {
            return text_document.diagnostic.is_some();
        }
        false
    }

    pub fn supports_workspace_diagnostic(&self) -> bool {
        self.supports_pull_diagnostic()
    }

    pub fn supports_refresh_diagnostic(&self) -> bool {
        self.client_capabilities
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.diagnostics.as_ref())
            .and_then(|diagnostic| diagnostic.refresh_support)
            .unwrap_or_default()
    }

    pub fn supports_dynamic_watched_files_registration(&self) -> bool {
        self.client_capabilities
            .workspace
            .as_ref()
            .and_then(|ws| ws.did_change_watched_files.as_ref())
            .and_then(|watch| watch.dynamic_registration)
            .unwrap_or_default()
    }
}
