//! # `textDocument/implementation` — the classes and functions that implement a declaration
//!
//! The protocol's "go to implementations" is a question about **overrides**: given a class, the classes deriving from
//! it; given a member function, the functions in those classes that redeclare it. C++ spells both with the same two
//! facts the analysis already stores — [`DeclFact::bases`] on the deriving class, and the member's own `scope` — so
//! neither needs a new index query.
//!
//! # What is answered, precisely
//!
//! ```text
//! a class          every class whose `bases` names it          → those classes' declarations
//! a member         every member of the same name whose scope
//!                  is one of those derived classes             → those members' declarations
//! ```
//!
//! A base is matched by its **last segment**: a declaration writes `Base` in one file and `ns::Base` in another, and
//! both name the same class. `scope` is matched the same way, for the same reason.
//!
//! # What is not answered, and why that is deliberate
//!
//! * **Nothing** for a name with no class or member behind it — a local, a namespace, a macro. "Implementations of a
//!   local" is not a question with a wrong answer, it is a question with no answer, and `null` says so.
//! * **Nothing** from classes the index has not read yet: the scan is over the summaries the session holds, and a
//!   partial index gives a partial list rather than a guess. `docs/usability.md` records why a partial list is the
//!   honest answer for a read.

use cpp_code_analysis::{FileProvider, Known, Session};
use lsp_types::{
    ClientCapabilities, ImplementationProviderCapability, Location, Range, ServerCapabilities,
    request::{GotoImplementationParams, GotoImplementationResponse},
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{offset_at_position, path_to_uri, position_in_file, uri_to_file_path};

pub async fn on_goto_implementation(
    context: ServerContextSnapshot,
    params: GotoImplementationParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<GotoImplementationResponse> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let offset = offset_at_position(&view, position)?;

        // **The name comes from the declaration the cursor is on**, not from the text: the analysis already answered
        // "what does this name declare", and reading the identifier would be a second answer free to disagree.
        let Known::Yes(defined) = session.definitions(&view, offset) else {
            return None;
        };
        let name = defined.found.first()?.fact.name.clone();

        let found = implementations_of(session, &name);
        if found.is_empty() {
            return None;
        }
        Some(GotoImplementationResponse::Array(found))
    })
    .await
}

/// Every declaration that implements `name` — the derived classes, and their members of that name.
///
/// A free function so that a test can ask it without a client, and generic over the provider for the same reason
/// [`crate::handlers::references::locations`] is: the fixture that reproduces the question is a session, not a server.
pub fn implementations_of<F: FileProvider + Clone>(
    session: &Session<F>,
    name: &str,
) -> Vec<Location> {
    let summaries = session.index().all_summaries();

    // **The classes the name belongs to.** A class name belongs to no class (it *is* one), a member to its scope —
    // and the distinction is what makes one function answer both questions: derive from `name` itself for a class,
    // from the class that declares `name` for a member.
    let mut owners: Vec<String> = Vec::new();
    for (_, summary) in &summaries {
        for fact in &summary.declarations {
            if fact.name == name {
                if let Some(scope) = fact.scope.as_deref() {
                    owners.push(last_segment(scope).to_string());
                }
            }
        }
    }

    let mut found: Vec<Location> = Vec::new();
    let mut derived: Vec<String> = Vec::new();
    for (path, summary) in &summaries {
        for fact in &summary.declarations {
            let derives_from_the_name = fact.bases.iter().any(|base| {
                let segment = last_segment(base);
                segment == name || owners.iter().any(|owner| owner == segment)
            });
            if derives_from_the_name {
                derived.push(fact.name.clone());
                push_location(session, path, fact.name_range, &mut found);
            }
        }
    }

    if derived.is_empty() {
        return found;
    }

    // …and the members of that name written in those classes: the overrides themselves.
    for (path, summary) in &summaries {
        for fact in &summary.declarations {
            if fact.name != name {
                continue;
            }
            let Some(scope) = fact.scope.as_deref() else {
                continue;
            };
            if derived.iter().any(|class| class == last_segment(scope)) {
                push_location(session, path, fact.name_range, &mut found);
            }
        }
    }

    found
}

/// The last `::`-separated segment of a spelling — `std::vector` -> `vector`.
fn last_segment(spelling: &str) -> &str {
    spelling.rsplit("::").next().unwrap_or(spelling).trim()
}

/// The declaration's position in its own file, or the top of that file when it cannot be read.
fn push_location<F: FileProvider + Clone>(
    session: &Session<F>,
    path: &std::path::Path,
    range: cpp_parser::SourceRange,
    found: &mut Vec<Location>,
) {
    let Some(uri) = path_to_uri(path) else {
        return;
    };
    let at_the_top = Location {
        uri: uri.clone(),
        range: Range::default(),
    };
    let position = session.files().held(path).and_then(|file| {
        let start = position_in_file(file, range.start_offset)?;
        let end = position_in_file(file, range.end_offset())?;
        Some(Range { start, end })
    });
    let location = match position {
        Some(range) => Location { uri, range },
        None => at_the_top,
    };
    if !found
        .iter()
        .any(|held| held.uri == location.uri && held.range == location.range)
    {
        found.push(location);
    }
}

pub struct ImplementationCapabilities;

impl RegisterCapabilities for ImplementationCapabilities {
    fn register_capabilities(server: &mut ServerCapabilities, _client: &ClientCapabilities) {
        server.implementation_provider = Some(ImplementationProviderCapability::Simple(true));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    use super::*;

    fn an_indexed_project() -> Session<MemoryFiles> {
        let files = MemoryFiles::new()
            .with_file("/p/base.h", "struct Base {\n    virtual void run();\n};\n")
            .with_file(
                "/p/derived.h",
                "struct Derived : public Base {\n    void run() override;\n};\n",
            );
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files([PathBuf::from("/p/base.h"), PathBuf::from("/p/derived.h")]);
        session.index_everything();
        session
    }

    /// **A class is implemented by the classes deriving from it, and a member by the members that override it.**
    ///
    /// The two questions share one function because they share one relation — the base of a class — but they find it
    /// from different ends: the class from its own name, the member from the name of the class that declares it. A
    /// version that looked for "classes deriving from `run`" would answer the first question only, and would answer it
    /// with nothing.
    #[test]
    fn a_derived_class_and_its_override_are_the_implementations() {
        let session = an_indexed_project();

        let classes = implementations_of(&session, "Base");
        assert_eq!(classes.len(), 1, "one class derives from Base: {classes:?}");
        assert!(
            classes[0].uri.path().ends_with("derived.h"),
            "{:?}",
            classes[0]
        );

        let overrides = implementations_of(&session, "run");
        assert!(
            !overrides.is_empty(),
            "the member `run` of the derived class implements the base's"
        );
        assert!(
            overrides
                .iter()
                .any(|found| found.uri.path().ends_with("derived.h")),
            "the implementation is in the derived class: {overrides:?}"
        );
    }
}
