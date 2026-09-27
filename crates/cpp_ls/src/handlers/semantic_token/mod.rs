//! # `textDocument/semanticTokens/full` — what each name *is*, drawn as colour
//!
//! ```text
//! struct Widget { int size; };        `Widget` a class, `size` a field, both drawn from what declares them
//! #define LIMIT 8                     `LIMIT` a macro — the preprocessor's name, not a C++ declaration at all
//! int f(Widget& w) { return w.size; }  the use of `size` is the *declaration's* kind, because that is what it is
//! ```
//!
//! The classification comes from the analysis ([`Session::classified_names`]) and this layer does three things
//! with it, and nothing else:
//!
//! ```text
//! 1. the legend      which of the protocol's token types this server uses, fixed and advertised once
//! 2. the filter      a kind the client says it cannot draw is not sent — a number it would not understand
//! 3. the encoding    absolute offsets and lengths  →  the protocol's delta walk
//! ```
//!
//! # Why the numbers are a *walk* rather than a list
//!
//! The protocol sends five numbers per token, and the first two are deltas: how many lines since the previous
//! token, and how many characters since it (or from the line's start, on a new line). Nothing in the answer may be
//! out of order, overlap, or have zero length, and the client reconstructs absolute positions from the deltas
//! alone — so an off-by-one does not shift one token, it shifts **every token after it**. That is why the encoding
//! is a function of its own here rather than a loop inside the handler, and why it is tested against a file whose
//! tokens straddle lines.
//!
//! # What is not sent
//!
//! A name the analysis cannot place (see the `semantic` module: a name nothing declares). The client draws those
//! identifiers with its own default colour, which is the honest drawing of "this analysis has nothing to say
//! about this name" — and it is also why this server sends **no** token for `int`, `return`, or a string literal:
//! a client that already lexes C++ draws keywords and literals itself, and two answers for one span is a
//! disagreement the user would see as flicker.

use cpp_code_analysis::semantic::{Name, NameKind};
use cpp_code_analysis::VfsFile;
use lsp_types::{
    ClientCapabilities, SemanticToken, SemanticTokens, SemanticTokensFullOptions,
    SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams, SemanticTokensResult,
    SemanticTokensServerCapabilities, SemanticTokenType, ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{position_in_file, uri_to_file_path};

/// The token types this server uses, in the order the encoding's numbers refer to.
///
/// Fixed, and advertised once in `initialize`: the numbers in the answer are indices into this list, so a legend
/// that changed per request would make every number mean something different.
const TOKEN_TYPES: &[(NameKind, SemanticTokenType)] = &[
    (NameKind::Namespace, SemanticTokenType::NAMESPACE),
    (NameKind::Type, SemanticTokenType::TYPE),
    (NameKind::TypeParameter, SemanticTokenType::TYPE_PARAMETER),
    (NameKind::EnumMember, SemanticTokenType::ENUM_MEMBER),
    (NameKind::Function, SemanticTokenType::FUNCTION),
    (NameKind::Method, SemanticTokenType::METHOD),
    (NameKind::Variable, SemanticTokenType::VARIABLE),
    (NameKind::Parameter, SemanticTokenType::PARAMETER),
    (NameKind::Macro, SemanticTokenType::MACRO),
];

/// The one modifier this server can honestly set: "this is where the name is declared".
///
/// The protocol has a dozen more — `readonly`, `static`, `deprecated`, `defaultLibrary` — and every one of them is
/// a fact this layer does not have: a summary records a kind, a scope and whether a declaration is conditional,
/// and none of those is "is this object const". A modifier that was wrong would be a visible claim about the
/// code, so only the one that is known is sent.
const TOKEN_MODIFIERS: &[lsp_types::SemanticTokenModifier] =
    &[lsp_types::SemanticTokenModifier::DECLARATION];

/// The legend, as advertised in `initialize` and as the encoding indexes it.
fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TOKEN_TYPES.iter().map(|(_, kind)| kind.clone()).collect(),
        token_modifiers: TOKEN_MODIFIERS.to_vec(),
    }
}

pub async fn on_semantic_tokens(
    context: ServerContextSnapshot,
    params: SemanticTokensParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<SemanticTokensResult> {
    let uri = params.text_document.uri;
    // **Which kinds the client can draw**, read before the query so that the closure below is a plain function of
    // the session: the client's capabilities are not part of the analysis, and the legend they filter is fixed.
    let drawable = drawable_types(context.lsp_features().semantic_token_types());

    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let held = session.files().held(&view.path)?;

        // **No answer while the index is still reading.** A name a header declares is classified through the
        // index, and before it has been read that name has no classification — so an answer now would be a
        // *shorter* list, which a client caches until the next edit. The protocol has a word for "ask again
        // later", and this is it: `null`.
        if session.pending() > 0 {
            log::debug!(
                "no semantic tokens yet: {} file(s) are queued, and a name in one of them would be uncoloured",
                session.pending()
            );
            return None;
        }

        let names = session.classified_names(&view);
        Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: encode(held, &names, &drawable),
        }))
    })
    .await
}

/// Which of this server's kinds the client says it can draw.
///
/// `None` — the client said nothing about token types — means **all of them**: a client that asks for semantic
/// tokens and lists no types is one this server cannot filter for, and refusing to answer would be worse than
/// answering with a legend the client may partly degrade (the protocol's own note says a client may render fewer
/// types than the server sends).
fn drawable_types(supported: Option<Vec<SemanticTokenType>>) -> Option<Vec<SemanticTokenType>> {
    match supported {
        Some(types) if !types.is_empty() => Some(types),
        _ => None,
    }
}

/// The tokens, as the protocol's delta walk — see the module documentation for why nothing may be skipped or
/// reordered here.
///
/// `drawable` is the client's list of supported types, or `None` for "it said nothing", in which case everything
/// this server can say is sent.
pub fn encode(
    file: &VfsFile,
    names: &[Name],
    drawable: &Option<Vec<SemanticTokenType>>,
) -> Vec<SemanticToken> {
    let mut tokens: Vec<SemanticToken> = Vec::new();
    let mut previous: Option<(u32, u32)> = None;

    for name in names {
        let Some(index) = type_index(name.kind) else {
            continue;
        };

        // The kind is one this server knows and the client cannot draw: sending its number would be sending a
        // number the client maps to nothing.
        if let Some(types) = drawable
            && !types.contains(&TOKEN_TYPES[index as usize].1)
        {
            continue;
        }

        let Some(position) = position_in_file(file, name.range.start_offset) else {
            // A range the file cannot place — an offset past its end, which a stale view could produce. Dropping
            // one token is the only option that keeps the walk valid: a guessed position would move every token
            // after it.
            continue;
        };

        let (delta_line, delta_start) = match previous {
            Some((line, character)) if line == position.line => {
                (0, position.character.saturating_sub(character))
            }
            Some((line, _)) => (position.line - line, position.character),
            None => (position.line, position.character),
        };
        previous = Some((position.line, position.character));

        // **UTF-16 code units**, which is what the protocol counts — the identifier's own text, not its byte
        // length: a name with a character outside the basic plane is longer in bytes than in what the client
        // measures the span with.
        let text = &file.text[name.range.start_offset..name.range.end_offset()];
        let length = text.encode_utf16().count() as u32;

        tokens.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: index,
            token_modifiers_bitset: if name.declaration { 1 } else { 0 },
        });
    }

    tokens
}

/// Which number means this kind — the position in [`TOKEN_TYPES`], which is the advertised legend.
fn type_index(kind: NameKind) -> Option<u32> {
    TOKEN_TYPES
        .iter()
        .position(|(known, _)| *known == kind)
        .map(|at| at as u32)
}

pub struct SemanticTokenCapabilities;

impl RegisterCapabilities for SemanticTokenCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, client: &ClientCapabilities) {
        // **Only if the client asks for the whole file, in the format this server speaks.** The protocol has a
        // second shape (a range at a time, and a `relative`/`delta` encoding) and this server implements neither:
        // advertising a provider for a request nobody will send, or in a format the client cannot read, is a
        // capability that lies.
        let semantic_tokens = client
            .text_document
            .as_ref()
            .and_then(|text_document| text_document.semantic_tokens.as_ref());

        let Some(semantic_tokens) = semantic_tokens else {
            return;
        };
        if semantic_tokens.requests.full.is_none() {
            return;
        }

        server_capabilities.semantic_tokens_provider =
            Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
                SemanticTokensOptions {
                    work_done_progress_options: Default::default(),
                    legend: legend(),
                    // No `range` provider: a range request would be the same walk over a slice, and the clients
                    // that use it are also the ones that ask for the whole file.
                    range: Some(false),
                    full: Some(SemanticTokensFullOptions::Bool(true)),
                },
            ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };
    use cpp_parser::SourceRange;

    /// A file held by a session, so that its line index is the one the VFS built — the same one every position in
    /// this server comes from.
    fn a_held_file(text: &str) -> (Session<MemoryFiles>, std::path::PathBuf) {
        let files = MemoryFiles::new().with_file("/p/a.cpp", text);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = cpp_code_analysis::Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.load("/p/a.cpp");
        (session, std::path::PathBuf::from("/p/a.cpp"))
    }

    fn name(start: usize, length: usize, kind: NameKind, declaration: bool) -> Name {
        Name {
            range: SourceRange::new(start, length),
            kind,
            declaration,
        }
    }

    /// **The walk, on tokens that straddle lines.** The deltas are relative — to the previous token on the same
    /// line, and to the line's start on a new one — so a client reconstructing positions from them is the only
    /// check that matters, and it is done here by rebuilding the absolute positions.
    #[test]
    fn the_encoding_is_a_walk_a_client_can_rebuild() {
        let text = "int alpha;\nint beta;\n    int gamma;\n";
        let (session, path) = a_held_file(text);
        let file = session.files().held(&path).expect("the file is held");

        // The offsets come from the text rather than from arithmetic: a hand-counted offset is a test that fails
        // when the fixture changes by one space, and the failure says nothing about the encoding.
        let at = |spelling: &str| {
            let start = text.find(spelling).expect("the fixture writes it");
            name(start, spelling.len(), NameKind::Variable, true)
        };
        let names = vec![at("alpha"), at("beta"), at("gamma")];
        let tokens = encode(file, &names, &None);

        let mut line = 0u32;
        let mut character = 0u32;
        let rebuilt: Vec<(u32, u32, u32)> = tokens
            .iter()
            .map(|token| {
                if token.delta_line == 0 {
                    character += token.delta_start;
                } else {
                    line += token.delta_line;
                    character = token.delta_start;
                }
                (line, character, token.length)
            })
            .collect();

        assert_eq!(
            rebuilt,
            vec![(0, 4, 5), (1, 4, 4), (2, 8, 5)],
            "the client's own reconstruction: {tokens:?}"
        );
        assert!(
            tokens.iter().all(|token| token.token_type == type_index(NameKind::Variable).unwrap()),
            "every one is a variable"
        );
        assert!(
            tokens.iter().all(|token| token.token_modifiers_bitset == 1),
            "and a declaration"
        );
    }

    /// A kind the client cannot draw is left out — and leaving it out must not disturb the walk for the tokens
    /// around it, which is the trap: the skipped token's position is what the next delta would have been measured
    /// against.
    #[test]
    fn a_kind_the_client_cannot_draw_is_skipped_without_moving_the_others() {
        let text = "int alpha;\nnamespace ns { }\nint beta;\n";
        let (session, path) = a_held_file(text);
        let file = session.files().held(&path).expect("the file is held");

        let variables = || {
            ["alpha", "beta"].map(|spelling| {
                let start = text.find(spelling).expect("the fixture writes it");
                name(start, spelling.len(), NameKind::Variable, true)
            })
        };
        let namespace = {
            let start = text.find("namespace").expect("the fixture writes it");
            name(start, "namespace".len(), NameKind::Namespace, true)
        };

        let mut names = variables().to_vec();
        names.insert(1, namespace);

        // With everything drawable: three tokens.
        assert_eq!(encode(file, &names, &None).len(), 3);

        // With namespaces unsupported: two, and the second variable's delta is measured from the *first variable*
        // — the skipped token is not a step in the walk.
        let drawable = Some(vec![SemanticTokenType::VARIABLE]);
        let tokens = encode(file, &names, &drawable);
        assert_eq!(tokens.len(), 2);
        assert_eq!((tokens[0].delta_line, tokens[0].delta_start), (0, 4));
        assert_eq!(
            (tokens[1].delta_line, tokens[1].delta_start),
            (2, 4),
            "the namespace's token is not a step: {tokens:?}"
        );
    }

    /// A token whose position the file cannot place is dropped rather than guessed — and the walk stays valid for
    /// everything after it.
    #[test]
    fn a_token_past_the_end_of_the_file_is_dropped() {
        let text = "int alpha;\n";
        let (session, path) = a_held_file(text);
        let file = session.files().held(&path).expect("the file is held");

        let names = vec![
            name(4, 5, NameKind::Variable, true),
            name(text.len() + 40, 4, NameKind::Variable, true),
        ];

        assert_eq!(encode(file, &names, &None).len(), 1);
    }

    /// The advertised legend and the numbers in the answer are the same list: a token's type index has to point at
    /// the type the client was told about, or every colour is off by whatever the two lists disagree about.
    #[test]
    fn the_advertised_legend_is_the_list_the_numbers_index() {
        let advertised = legend();
        assert_eq!(advertised.token_types.len(), TOKEN_TYPES.len());
        assert_eq!(advertised.token_modifiers, TOKEN_MODIFIERS.to_vec());

        for (index, (_, kind)) in TOKEN_TYPES.iter().enumerate() {
            assert_eq!(
                advertised.token_types[index],
                *kind,
                "the number {index} means this type"
            );
        }
    }

    /// A client that asks for semantic tokens but names no type this server uses gets an empty list rather than a
    /// legend it cannot read.
    #[test]
    fn a_client_that_supports_none_of_our_types_gets_nothing_to_draw() {
        let text = "int alpha;\n";
        let (session, path) = a_held_file(text);
        let file = session.files().held(&path).expect("the file is held");

        let names = vec![name(4, 5, NameKind::Variable, true)];
        let drawable = Some(vec![SemanticTokenType::COMMENT, SemanticTokenType::STRING]);

        assert!(encode(file, &names, &drawable).is_empty());
    }
}
