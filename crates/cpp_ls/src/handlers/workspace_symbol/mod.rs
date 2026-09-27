//! # `workspace/symbol` — the project's declarations, by name
//!
//! The first query here that is **not** about one file: [`ProjectIndex::symbols_matching`] searches every
//! declaration the index holds, in both readings, without the visibility walk — a symbol search is about the
//! project, so a class in a header nothing includes is still a symbol.
//!
//! # The match, which is the whole feature
//!
//! A **bare word** is matched against a declaration's *name* (exact, then prefix, then anywhere in it), and a
//! **qualified query** against the *tail* of its qualified name (`Widget::si` finds `ns::Widget::size`, and
//! `ns::widget` does not drag that member in). The index owns the rule and documents it; this layer passes the
//! query through and turns what comes back into the protocol's shape.
//!
//! # Why this refuses while the index has work, and `references` too
//!
//! Both answer a question about the **whole project**, and the revision of the protocol this crate builds against
//! gives neither response a way to say "and there may be more" — no `isIncomplete`, which a completion list has and
//! these do not. An answer assembled from a partially read index is therefore not a smaller truth but a false one:
//! "no symbol by that name" is a conclusion the user acts on.
//!
//! # The cap is *not* a refusal
//!
//! A search may return fewer symbols than match, and that is the ordinary shape of a symbol box: the user narrows
//! the query, and every editor's search truncates. The two are different claims — "these are all the uses of this
//! name" is a statement about the project, "here are 500 matches" is an answer to a query — and this module is
//! where the difference is decided.

use cpp_code_analysis::Session;
use lsp_types::{
    ClientCapabilities, Location, OneOf, ServerCapabilities, WorkspaceSymbol, WorkspaceSymbolParams,
    WorkspaceSymbolResponse,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{path_to_uri, position_in_file, symbol_kind};

/// How many symbols one search answers with.
///
/// Far past what a client shows at once and far short of what a match-everything query would produce: the cap
/// exists so that a search for two letters cannot walk a ten-thousand-file index into one response.
const SYMBOL_LIMIT: usize = 512;

pub async fn on_workspace_symbol(
    context: ServerContextSnapshot,
    params: WorkspaceSymbolParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<WorkspaceSymbolResponse> {
    let query = params.query;

    // No `prepare`: the session index is what the search reads, and a file nobody has read is not in it — reading
    // one file into the VFS would not change the answer.
    snapshot_query(context.analysis(), cancel_token, move |session| {
        symbols(session, &query).map(WorkspaceSymbolResponse::Nested)
    })
    .await
}

/// The symbols matching `query`, or `None` when the index is not in a state to answer at all.
///
/// A plain function, so that a test can search without a client and a `params` value — and so that the *guard* is
/// testable where it is decided rather than through a timeout.
pub fn symbols<F: cpp_code_analysis::FileProvider + Clone>(
    session: &Session<F>,
    query: &str,
) -> Option<Vec<WorkspaceSymbol>> {
    if session.pending() > 0 {
        log::debug!(
            "no symbol search yet: {} file(s) are queued and their declarations are not in the index",
            session.pending()
        );
        return None;
    }

    // One more than the cap, so that "the answer was truncated" is knowable rather than inferred from a length that
    // happens to equal it.
    let mut found = session.index().symbols_matching(query, SYMBOL_LIMIT + 1);
    if found.len() > SYMBOL_LIMIT {
        found.truncate(SYMBOL_LIMIT);
        log::info!("`{query}` matches more than {SYMBOL_LIMIT} symbols; the client was given the first ones");
    }

    let mut symbols = Vec::new();

    for symbol in found {
        // The declaring file's own line index, the same pair `definition` answers with: a symbol in a header nobody
        // opened is a symbol in a header this session holds.
        let Some(held) = session.files().held(&symbol.file) else {
            log::warn!(
                "no symbol search: {} is not held, so its symbols cannot be placed",
                symbol.file.display()
            );
            return None;
        };
        let (Some(uri), Some(start), Some(end)) = (
            path_to_uri(&symbol.file),
            position_in_file(held, symbol.fact.name_range.start_offset),
            position_in_file(held, symbol.fact.name_range.end_offset()),
        ) else {
            return None;
        };

        symbols.push(WorkspaceSymbol {
            name: symbol.fact.name.clone(),
            kind: symbol_kind(symbol.fact.kind),
            tags: None,
            // What a client shows as the qualifier: the scope the declaration was written in.
            container_name: symbol.fact.scope.clone(),
            location: OneOf::Left(Location {
                uri,
                range: lsp_types::Range::new(start, end),
            }),
            data: None,
        });
    }

    Some(symbols)
}

pub struct WorkspaceSymbolCapabilities;

impl RegisterCapabilities for WorkspaceSymbolCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.workspace_symbol_provider = Some(OneOf::Left(true));
    }
}

#[cfg(test)]
mod tests {
    use super::symbols;
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };
    use std::path::PathBuf;

    const WIDGET_H: &str = "namespace ns {\nstruct Widget { int size; };\n}\n";

    /// **A search is refused while the index has work**, for the reason the module documents: the protocol's
    /// response has no "and there may be more", so a partial search is a false answer rather than a smaller one.
    ///
    /// The state is built rather than waited for — a project list with the file queued and nothing read — and the
    /// second half is the same call once the queue is empty, which is what makes the first half about timing rather
    /// than about a search that never works.
    #[test]
    fn a_symbol_search_is_refused_while_the_index_has_work() {
        let files = MemoryFiles::new().with_file("/p/widget.h", WIDGET_H);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );

        session.add_project_files([PathBuf::from("/p/widget.h")]);
        assert!(session.pending() > 0, "the fixture is the refused state");
        assert_eq!(symbols(&session, "Widget"), None, "an answer now would be missing symbols");

        session.index_everything();
        let found = symbols(&session, "widget").expect("the index is complete now");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].name, "Widget");
        assert_eq!(
            found[0].container_name.as_deref(),
            Some("ns"),
            "the qualification travels, because a client shows it"
        );
    }
}
