//! # `textDocument/definition` — where the name at the cursor is declared
//!
//! The port template for a **pull** handler: resolve the position, ask the analysis session, turn `Known` into
//! `RequestOutcome`.
//!
//! ```text
//! uri + position ──> path + offset ──> session.view(path) ──> session.definitions(&view, offset)
//!                          │                                        │
//!                          │                                        └─ Known<ProjectDefinitions> { found, conditional }
//!                          └─ util::offset_at_position                    │
//!                                                                        └─ Yes → one location, or **all of them**
//!                                                                           No / Unknown → Missing (null)
//! ```
//!
//! **`Known` is why this handler does not guess.** `No` means "there is no such declaration" and `Unknown` means
//! "the index cannot say yet" — both answer `null`, because a client that jumps to a wrong location is worse off
//! than one that gets no location at all.
//!
//! # Why several locations, and when
//!
//! One name can cover several declarations, and the protocol has a shape for that: `Location[]` is what a client
//! shows as a peek list. `find` in `std::basic_string` is one **overload set** (seventeen declarations in MSVC's
//! library), `std::cin` is written twice in `<iostream>`, and neither is an ambiguity a reader wants to be told
//! about — they are the answer. The analysis side says which lists are worth sending
//! ([`cpp_code_analysis::ProjectIndex::definitions`]): one namespace collapses to one entry, because a namespace
//! is one entity however many files reopen it, and `std` alone has fifty-eight declarations in the index.
//!
//! A single declaration is still sent as `Scalar`, which is the shape every client has handled since before lists
//! existed — and the shape this handler sent before it could answer with more than one.
//!
//! # The range is the name, in the *declaring* file
//!
//! [`DeclFact::name_range`] is the fact's own offsets, so the range is computed by reading the declaring file
//! through the session (the buffer when it is open — a declaration in an unsaved buffer is at the offsets the user
//! can see) and mapping them onto a line and column. When that file cannot be read at all, the answer is still the
//! file, with a zero-width range at its start: a jump that lands at the top of the right file is worth having, and
//! the position of a name the analysis cannot see is not something this layer can invent.
//!
//! # The fourth answer: an `#include` points at a file
//!
//! `#include <vector>` **declares nothing** — no scope declares it, the index has no fact by that name, and the
//! macro table has no entry for it — so every question above answers "nothing found" for a line whose whole purpose
//! is to name a file. It is asked first ([`cpp_code_analysis::Session::header_at`]) because the shape at the cursor
//! decides which question applies, and there is no position at which both can be true.

use cpp_code_analysis::{DiskFiles, Known, Session};
use lsp_types::{
    ClientCapabilities, GotoDefinitionParams, GotoDefinitionResponse, Location, OneOf, Range,
    ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{
    file_offset_at_position, offset_at_position, path_to_uri, position_in_file, uri_to_file_path,
};

pub async fn on_goto_definition_handler(
    context: ServerContextSnapshot,
    params: GotoDefinitionParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<GotoDefinitionResponse> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    // The file is read in first, under the write lock, so that the query below can be a read: a file the client has
    // open is already held by `didOpen`, and this covers the request that arrives about one that is not.
    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;

        // **The module interface units this file imports.** A name that arrives through `import std;` is declared in
        // a file the project never scanned — `<VC>/Tools/MSVC/<version>/modules/std.ixx` is outside it — so a
        // go-to-definition on `std::string` in a file that says `import std;` has nothing to point at until that file
        // is read. `false`: this handler has no edit to catch up on, so the read does it itself.
        crate::handlers::read_the_modules(&context, &path, false).await;
        // **And then the query, which no longer waits for the project's index to drain.**
        //
        // The defect is real and worth keeping on the record: `prepare` reads *this* file, while the declaration
        // being jumped to is usually in a header whose facts exist only once the pump has cooked the include closure
        // — and measured on a live server for `std::endl`, the first three queries after an open answered **nothing
        // at all** while the fourth answered correctly. The wait added for it was `AnalysisState::settle`, which
        // asked whether the *whole project's* indexing queue was empty.
        //
        // The question that matters is about this file's own closure, and `read_the_modules` above is what answers it:
        // it catches this file's summary up and reads the files it reaches that nobody has read. See
        // `docs/status.md` §5.2.
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        // **The cursor is read from the file's own text, and the question is asked of the same reading.**
        //
        // The note below says why for the `#include` case, and the reason does not stop there: a rendering has the
        // directives *resolved*, its line breaks **deleted**, and everything a macro expanded written out in place —
        // so a line and a column from a client name a different place in it, and the reader at that place is not the
        // reader the client is looking at. Measured on a live server: a plain identifier inside a function was read as
        // a qualified name in scope `std`, because with the whole file on one line the recovery glued the `std::` of
        // `std::string name;` to the name being typed.
        //
        // The offset and the view are taken **together**, from one reading. Resolving the position through one and
        // querying the other is the mistake this replaces — and it is not hypothetical: a completion fix that did
        // exactly that resolved the same client position to offset 53 before an edit and 26 after it.
        let view = session.view_of_the_file(&path)?;
        let offset = offset_at_position(&view, position)?;
        // **The file's own offset for the one query that lives in the file's text**: a header name is a
        // directive's argument, and `Session::header_at` parses the written text to find it. Everything below
        // works in the rendering, which is why both offsets are held and each is named.
        let in_the_file = file_offset_at_position(&view, position)?;

        // **A header name before a declaration**, because the two cannot both be here and only one of them can
        // answer: `#include <vector>` declares nothing, so the declaration question below would answer "nothing
        // found" for a name that has a perfectly good file behind it. Asked first rather than as a fallback for
        // the same reason a member query is asked before a name query — the *shape* at the cursor decides which
        // question applies, and the shape here is a directive.
        //
        // **Asked of the file's own text, not of the rendering.** A rendering has the directives *resolved*: the
        // `#include` line is gone from it, because what it stood for is written out in its place. So the one
        // position a reader most obviously wants to jump from — the header they wrote — is not in the reading the
        // rest of this handler works in, and asking there finds nothing.
        if let Known::Yes(header) = session.header_at(&view, in_the_file) {
            // A header this session cannot hold — the file is on disk but the VFS refused it — answers
            // `None` rather than a location with no range, which is the same "nothing to go to" the reader
            // had before.
            return location_of_a_file(session, &header).map(GotoDefinitionResponse::Scalar);
        }

        let Known::Yes(found) = session.definitions(&view, offset) else {
            return None;
        };
        // **One class's member, not every class's.** `definitions` answers by *name*, and three classes in a real
        // project declare `_diagnosticStyle` — so a cursor inside one of them was offered all three. The cursor's own
        // class decides which is meant; see `references::narrowed_to_the_cursors_class`, which is the same rule Find
        // References applies and the reason the two answer consistently.
        let found =
            crate::handlers::references::narrowed_to_the_cursors_class(&view, offset.get(), found);

        // **A declaration is not a definition, and this request is the one for definitions.** `class vector<bool,
        // _Alloc>;` forward-declares the specialization `<vector>` then defines it, and both are candidates for the
        // name — measured on MSVC's `<vector>`, a jump to `vector` offered three places, one of them the forward
        // declaration. C++ has no rule that hides it, but a reader asking *where this is defined* means the body,
        // and a list whose entries are "the definition, a promise that one exists, another definition" makes the
        // promise look like a place. What is kept per group — one file, one scope, one name — is the definitions when
        // a group has any, and everything when it has none (a declaration is a better answer than no answer).
        let found = definitions_where_there_are_bodies(session, found);

        // **The overload a call means comes first.** The protocol allows a list and a client shows it in order, so the
        // first entry is what a reader who does not pick from the popup gets — and for a call that is the declaration
        // its arguments select. The same two queries the hover marks from, so the two handlers cannot disagree.
        let chosen = chosen_definition(session, &view, offset, &found);

        let mut ordered: Vec<(bool, Location)> = Vec::new();
        for (at, declaration) in found.found.iter().enumerate() {
            let Some(uri) = path_to_uri(&declaration.file) else {
                continue;
            };
            let range =
                name_range(session, &declaration.file, &declaration.fact).unwrap_or_default();
            ordered.push((Some(at) == chosen, Location { uri, range }));
        }
        // Stable, so everything that is not the chosen one keeps the order the index answered in.
        ordered.sort_by_key(|(is_the_one, _)| !*is_the_one);
        let mut locations: Vec<Location> =
            ordered.into_iter().map(|(_, location)| location).collect();

        match locations.len() {
            0 => None,
            1 => Some(GotoDefinitionResponse::Scalar(locations.remove(0))),
            _ => Some(GotoDefinitionResponse::Array(locations)),
        }
    })
    .await
}

/// **A jump to a file rather than to a name in it** — what `#include <vector>` points at.
///
/// The range is the **whole first line** rather than a point at the top of the file, and the difference is what a
/// client does with it: an editor that receives a scalar location *selects* the range, and a zero-width range at
/// offset zero selects nothing and scrolls to a column that may not be where anything is. The file itself is the
/// answer — a header has nothing to highlight — so the range says "here, from the beginning" and no more.
///
/// `None` when the resolved path cannot be spelled as a URI or is not in the VFS, which leaves the client with the
/// same "no definition" it had before rather than with a URI it cannot open.
fn location_of_a_file(
    session: &Session<DiskFiles>,
    header: &cpp_code_analysis::HeaderTarget,
) -> Option<Location> {
    let uri = path_to_uri(&header.resolved)?;
    let file = session.files().held(&header.resolved)?;

    let end_of_the_first_line = file
        .text
        .find('\n')
        .map_or_else(|| file.text.len(), |newline| newline + 1);
    let end = position_in_file(file, end_of_the_first_line)?;
    Some(Location {
        uri,
        range: Range::new(lsp_types::Position::new(0, 0), end),
    })
}

/// The declaration's name, as a range in the file that declares it.
fn name_range(
    session: &cpp_code_analysis::Session<cpp_code_analysis::DiskFiles>,
    file: &std::path::Path,
    fact: &cpp_code_analysis::DeclFact,
) -> Option<Range> {
    // The declaring file, from the VFS: its text and **its line index**, which is the pair the entry was made with.
    // No parse — a name's line and column do not need a tree — and no second read of a header this session is
    // already holding.
    let declaring = session.files().held(file)?;

    Some(Range::new(
        position_in_file(declaring, fact.name_range.start_offset)?,
        position_in_file(declaring, fact.name_range.end_offset())?,
    ))
}

pub struct DefinitionCapabilities;

/// **Keep the candidates that are written as definitions, per group — and everything when a group has none.**
///
/// A group is *one file, one scope, one name*: the same entity declared twice. `class vector<bool, _Alloc>;` and the
/// specialization that follows it are one such group, and C++ says they are two declarations of one entity — the
/// first a promise, the second the body. A reader who asked for the definition means the body, and a list that holds
/// both makes the promise look like a place to go.
///
/// # How a fact says "definition" without a field for it
///
/// A [`DeclFact`](cpp_code_analysis::DeclFact) records where a declaration is written, so the file's own text answers
/// the question: the first `{`, `:` or `;` after the declaration's range is a **body**, a **base clause** (or a
/// constructor's initializer list) — both of which only a definition has — or the semicolon that ends a declaration.
/// Measured on the four spellings that matter:
///
/// ```text
/// struct A;                range "struct A"        → `;`      a declaration
/// struct A { int x; };     range "struct A { … }"  → `;` after a `{` *inside* the range
/// struct A : public B {…}  range "struct A : public B" → `:`   a definition
/// void f(); void f() {…}   range "f()"             → `;` / `{`
/// ```
///
/// The braces are looked for **inside** the range as well, because a class's range already contains its body while a
/// function's does not; and nothing is claimed when the file's text is not held — a filter that guessed about a file
/// it cannot read would be removing answers it knows nothing about.
fn definitions_where_there_are_bodies<F: cpp_code_analysis::FileProvider + Clone>(
    session: &Session<F>,
    found: cpp_code_analysis::ProjectDefinitions,
) -> cpp_code_analysis::ProjectDefinitions {
    if found.found.len() < 2 {
        return found;
    }

    let written_as_a_definition: Vec<bool> = found
        .found
        .iter()
        .map(|declaration| {
            is_written_as_a_definition(session, &declaration.file, &declaration.fact)
        })
        .collect();

    // The groups that have one, and the ones that do not are left whole.
    let groups_with_a_body: std::collections::HashSet<(
        std::path::PathBuf,
        Option<String>,
        String,
    )> = found
        .found
        .iter()
        .zip(&written_as_a_definition)
        .filter(|(_, definition)| **definition)
        .map(|(declaration, _)| {
            (
                declaration.file.clone(),
                declaration.fact.scope.clone(),
                declaration.fact.name.clone(),
            )
        })
        .collect();
    if groups_with_a_body.is_empty() {
        return found;
    }

    let kept: Vec<cpp_code_analysis::ProjectDefinition> = found
        .found
        .into_iter()
        .zip(written_as_a_definition)
        .filter(|(declaration, definition)| {
            let group = (
                declaration.file.clone(),
                declaration.fact.scope.clone(),
                declaration.fact.name.clone(),
            );
            !groups_with_a_body.contains(&group) || *definition
        })
        .map(|(declaration, _)| declaration)
        .collect();

    cpp_code_analysis::ProjectDefinitions {
        found: kept,
        ..found
    }
}

/// **Is this declaration written with a body?** — see [`definitions_where_there_are_bodies`].
fn is_written_as_a_definition<F: cpp_code_analysis::FileProvider + Clone>(
    session: &Session<F>,
    file: &std::path::Path,
    fact: &cpp_code_analysis::DeclFact,
) -> bool {
    let Some(text) = session.text(file) else {
        return false;
    };
    let start = fact.range.start_offset.min(text.len());
    let end = fact.range.end_offset().min(text.len());
    let Some(written) = text.get(start..end) else {
        return false;
    };

    // **A range that ends its own declaration is a declaration** — and the semicolon is *inside* it: measured,
    // `class Box;` has the range `class Box;`, so looking past the end would find the next declaration's body and call
    // a promise a definition.
    if written.trim_end().ends_with(';') {
        return false;
    }

    // A class's range already holds its body: `struct A { int x; }` is one range, while a function's range is its
    // declarator — `f()` — with the body written after it.
    if written.contains('{') {
        return true;
    }

    for character in text.get(end..).unwrap_or_default().chars() {
        match character {
            '{' | ':' => return true,
            ';' => return false,
            _ => {}
        }
    }

    false
}

impl RegisterCapabilities for DefinitionCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.definition_provider = Some(OneOf::Left(true));
    }
}

/// **Which declaration a call at this offset means**, as an index into `found.found`.
///
/// `None` for a name with one declaration, for a cursor that is not on a call, and for a call whose arguments select
/// nothing — the three cases in which there is no first location to move forward.
fn chosen_definition(
    session: &Session<DiskFiles>,
    view: &cpp_code_analysis::FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
    found: &cpp_code_analysis::ProjectDefinitions,
) -> Option<usize> {
    if found.found.len() < 2 {
        return None;
    }
    let parameter_lists: Vec<Option<String>> = found
        .found
        .iter()
        .map(|declaration| declaration.fact.parameter_list.clone())
        .collect();
    // The template names are the caller's, because they are in the declaring file's text and a fact does not carry
    // them — `docs/overloads.md` §21.
    let template_parameters: Vec<Vec<String>> = found
        .found
        .iter()
        .map(|declaration| {
            session
                .text(&declaration.file)
                .map(|text| {
                    cpp_code_analysis::sema::deduction::template_parameters_before(
                        &text,
                        declaration.fact.name_range.start_offset,
                    )
                })
                .unwrap_or_default()
        })
        .collect();
    session.the_declaration_a_call_means(view, offset, &parameter_lists, &template_parameters)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    /// **A forward declaration is not a place a definition lives.**
    ///
    /// The two-definitions shape is MSVC's `<vector>`: `class vector<bool, _Alloc>;` forward-declares the
    /// specialization, and the definition with its base clause follows a few hundred lines later — so a jump to
    /// `vector` offered three places, one of them a promise. The filter keeps the two definitions and drops the
    /// promise, and a name that is *only* declared keeps its declaration (a better answer than none).
    #[test]
    fn a_forward_declaration_is_not_offered_beside_its_definition() {
        let header = "\
class Box;
class Box { int size; };
class Box<bool>;
class Box<bool> : public Box { };
class Promised;
";
        let source = "#include \"box.h\"\nBox b;\nPromised* promised;\n";
        let mut memory = MemoryFiles::new()
            .with_file("/p/box.h", header)
            .with_file("/p/main.cpp", source);
        memory = memory.with_file("/p/other.cpp", "int main() { return 0; }\n");
        let providers = SessionFiles::new(OpenDocuments::new(), memory);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files([
            std::path::PathBuf::from("/p/box.h"),
            std::path::PathBuf::from("/p/main.cpp"),
        ]);
        session.index_everything();

        let found = session
            .index()
            .definitions("Box", std::path::Path::new("/p/main.cpp"));
        let Known::Yes(found) = found else {
            panic!("the name is declared: {found:?}");
        };
        assert_eq!(found.found.len(), 4, "four declarations before the filter");

        let kept = definitions_where_there_are_bodies(&session, found);
        let written: Vec<String> = kept
            .found
            .iter()
            .map(|declaration| {
                let text = session.text(&declaration.file).expect("held");
                text[declaration.fact.range.start_offset..declaration.fact.range.end_offset()]
                    .to_string()
            })
            .collect();
        assert_eq!(
            written.len(),
            2,
            "a forward declaration is not a definition: {written:?}"
        );
        assert!(
            written.iter().all(|declaration| declaration.contains('{')),
            "what survived is what has a body: {written:?}"
        );

        // **A name nothing defines keeps its declaration.** `Promised` is only ever forward-declared here, and a
        // declaration is a better answer than "not found" — the filter removes places that cannot be the definition,
        // it does not require one to exist.
        let known = session
            .index()
            .definitions("Promised", std::path::Path::new("/p/main.cpp"));
        let Known::Yes(known) = known else {
            panic!("the name is declared: {known:?}");
        };
        assert_eq!(
            definitions_where_there_are_bodies(&session, known)
                .found
                .len(),
            1,
            "a declaration with nothing to prefer stays"
        );
    }
}
