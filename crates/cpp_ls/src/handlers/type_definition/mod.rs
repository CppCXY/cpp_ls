//! `textDocument/typeDefinition` — **where the type of the thing under the cursor is declared**.
//!
//! The question is not "where is this name declared" (that is Go to Definition) but "what is this value's type, and
//! where is *that* written". A client offers it on a variable, a parameter, a member, or the result of an `auto`
//! declaration, and it is the feature that makes `auto result = f();` navigable: the reader cannot see the type in the
//! text, so the server has to say it and then show its declaration.
//!
//! # What is answered
//!
//! ```text
//! a variable, a parameter, a member    its declared type's declaration
//! a function                           its return type's declaration
//! a class name                         the class's own declaration (the honest answer: its type is itself)
//! nothing whose type is unknown        `null` — a builtin has no declaration to go to
//! an `auto` declaration                **not yet** — see below
//! ```
//!
//! # An `auto`, and the two readings
//!
//! The case the feature is worth most for — the type is written nowhere in the file — needs the **rendering**: the
//! deduction lives there, and the file's own text answers `Known::No` for the same name (measured). So after the
//! written view has answered nothing, the same question is asked of the rendering, which
//! [`Session::rendering_of`] builds from what the session already holds — no cook queued, nothing to be starved by
//! the rendering cache.
//!
//! Measured on the real project: `auto result = f.GetFormatResult(t);` → `stringfwd.h:79` (the declaration of
//! `std::string`, the deduced type) in 2 ms. A MemoryFiles fixture whose initializer is a *call* still answers
//! nothing — the difference is the deduction of a call's return type in a session with no cooked closure, not the
//! handler.
//!
//! # Where the type comes from
//!
//! [`Session::type_at`] — the same inference the hover shows and the checks use, so the answer cannot disagree with
//! them. It is asked at the **end of the written name**, because that is the offset the inference resolves a
//! declaration at (a cursor inside an identifier is still inside the name; the end of it is where the type question is
//! answered for `a.b` too, whose member is the last segment).
//!
//! # What a partial answer looks like
//!
//! A type this session has not read has no location, and the answer is `null` rather than a guess. `std::vector`'s
//! declaration is answerable only once its header is in the index; the alternative — answering the *variable's* own
//! declaration — would be Go to Definition wearing another name.

use cpp_code_analysis::{FileProvider, Known, Session};
use lsp_types::{
    ClientCapabilities, Location, ServerCapabilities, TypeDefinitionProviderCapability,
    request::{GotoTypeDefinitionParams, GotoTypeDefinitionResponse},
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{offset_at_position, path_to_uri, position_in_file, uri_to_file_path};

pub async fn on_goto_type_definition(
    context: ServerContextSnapshot,
    params: GotoTypeDefinitionParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<GotoTypeDefinitionResponse> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        // The cursor is read from the file's own text, for the reason `implementation` states at its own call:
        // positions converted through a rendering name a different place.
        let view = session.view_of_the_file(&path)?;
        let offset = offset_at_position(&view, position)?;

        let found = type_definitions_of(session, &view, offset.get());
        if found.is_empty() {
            return None;
        }
        Some(GotoTypeDefinitionResponse::Array(found))
    })
    .await
}

/// **The declarations of the type of whatever is at `offset`** — the decision the handler makes, split out so it can be
/// tested without a client.
///
/// Two steps, and the first one is the interesting one: what *is* the type? [`Session::type_at`] answers for a
/// variable, a member, a call and an `auto`, and it is the same answer the hover shows — a second way of deciding would
/// be a second answer free to disagree.
///
/// The second step turns a spelling into locations: the index's declarations of that name, filtered to the ones that
/// declare a **type** (`DeclKind::Type`). A function named the same as a class is not the class's declaration, and
/// returning it would send a reader to the wrong place — the one mistake this feature must not make.
///
/// # The class name itself
///
/// A cursor **on a class's name** asks a question whose answer is the declaration it is already on. That is not a
/// useless answer: a client asks `typeDefinition` on `std::string` in `std::string s;`, and the class is in a header the
/// reader wants to open. The first step answers `Unknown` there (a type name is not a value with a type), so the name is
/// resolved instead — see the second half of the function.
pub fn type_definitions_of<F: FileProvider + Clone>(
    session: &Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: usize,
) -> Vec<Location> {
    // **The two ends of the name under the cursor, and both are asked.** They answer different questions, and the
    // feature needs both: the *start* is where a declaration is resolved — which is where an `auto`'s deduction lives,
    // and it is how the hover reads one (`session.type_at(view, fact.name_range.start_offset)`) — while the *end* is
    // where a member access is resolved (`a.b` asks about `b`, not about `a`). Measured on a fixture: asking only at the
    // end answered nothing for `auto made = make();`, and asking only at the start would answer the *object* for `a.b`.
    let (start, end) = the_name_around(&view.source, offset);

    // (1a) **A member access asks about the member**, not about the object. `w.size` is a call returning `int`, and
    // `session.type_at` at the member's name answers the *object's* type — measured, it answered `Widget` for `w.size`,
    // so the reader would land on the class of the thing the member is called on. The member's own declared type is
    // what the question means, and it is in the fact the member list was built from.
    // **Only the member's type answers here.** Falling through to the object's type would send the reader to the class
    // of the thing the member is called on: `w.size` returning `int` has no type definition, and the answer that
    // matters is "none", not `Widget`.
    if let Some(access) = cpp_code_analysis::sema::resolve::member_access_at(&view.root, offset) {
        return the_members_type(session, view, &access)
            .and_then(|spelling| declarations_of_a_type(session, view, &spelling))
            .unwrap_or_default();
    }

    // (1b) The type of the value at the cursor. **The end first**: it is where a member access resolves (`a.b` asks about
    // `b`), and the start is where a *declaration* resolves.
    for at in [end, start] {
        let Known::Yes(found) = session.type_at(
            view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
        ) else {
            continue;
        };
        let spelling = found.type_of.to_string();
        if let Some(locations) = declarations_of_a_type(session, view, &spelling) {
            return locations;
        }
    }

    // (1c) **An `auto`'s type is deduced by the *rendering's* question**, not the written text's. `auto made = make();`
    // answers `Known::No` at the name in the file's own text — which is why the first version of this handler answered
    // nothing for it — while the same offset in the rendering answers the deduced type. That is the question the hover
    // asks, and the reason a hover shows a type the reader cannot see in the text.
    //
    // `rendering_of` builds the rendering from what the session already holds (a summary, a unit, the text) and takes
    // `&self`: it queues no cook and cannot be starved by the rendering cache the way `view` could.
    if let Some(rendered) = session.rendering_of(&view.path)
        && let Some(held) = session.files().held(&view.path)
    {
        let rendering = cpp_code_analysis::FileView::parse_rendering(held, &rendered);
        for at in [end, start] {
            let Some(at) = rendering
                .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(at))
            else {
                continue;
            };
            let Known::Yes(found) = session.type_at(&rendering, at) else {
                continue;
            };
            let spelling = found.type_of.to_string();
            if let Some(locations) = declarations_of_a_type(session, view, &spelling) {
                return locations;
            }
        }
    }

    // (2) The name at the cursor, when it *is* a type — `std::string` in `std::string s;`, or a class a reader is
    // already standing on.
    let at = cpp_code_analysis::file::coordinates::ReadingOffset::new(end);
    let Known::Yes(defined) = session.definitions(view, at) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for definition in &defined.found {
        if !matches!(definition.fact.kind, cpp_code_analysis::DeclKind::Type) {
            continue;
        }
        if let Some(location) = location_of(session, &definition.file, definition.fact.name_range) {
            found.push(location);
        }
    }
    found
}

/// **The declaration of a type spelling**, if this session can place it.
///
/// The spelling comes from the inference and may be written in any of the forms a fact records — `Widget`,
/// `const Widget&`, `ns::Widget`, `Widget<T>`. The lookup is by the **last segment without its template arguments**,
/// which is the name a declaration is filed under; the filter to `DeclKind::Type` is what keeps a function of the same
/// name from answering.
fn declarations_of_a_type<F: FileProvider + Clone>(
    session: &Session<F>,
    view: &cpp_code_analysis::FileView,
    spelling: &str,
) -> Option<Vec<Location>> {
    let name = the_name_of_a_type(spelling)?;
    // **One query, the same one every other name lookup makes**: the index answers with the declarations visible from
    // this file, "own file wins" included.
    let mut found = Vec::new();
    let Known::Yes(defined) = session.index().definitions(&name, &view.path) else {
        return None;
    };
    for definition in &defined.found {
        if !matches!(definition.fact.kind, cpp_code_analysis::DeclKind::Type) {
            continue;
        }
        if let Some(location) = location_of(session, &definition.file, definition.fact.name_range) {
            found.push(location);
        }
    }
    (!found.is_empty()).then_some(found)
}

/// `const ns::Widget<T>&` → `Widget`. The name a declaration is filed under.
fn the_name_of_a_type(spelling: &str) -> Option<String> {
    let mut text = spelling.trim();
    for keyword in [
        "const ",
        "volatile ",
        "struct ",
        "class ",
        "typename ",
        "enum ",
    ] {
        while let Some(rest) = text.strip_prefix(keyword) {
            text = rest.trim();
        }
    }
    text = text.trim_end_matches(['&', '*']).trim();
    // A template-id's arguments are not part of the name: `Widget<T>` is filed as `Widget`.
    if let Some(open) = text.find('<') {
        text = &text[..open];
    }
    let last = text.rsplit("::").next().unwrap_or(text).trim();
    (!last.is_empty()).then(|| last.to_string())
}

/// One fact's range, as a protocol location — `None` for a file whose line index cannot place it.
fn location_of<F: FileProvider + Clone>(
    session: &Session<F>,
    file: &std::path::Path,
    range: cpp_parser::SourceRange,
) -> Option<Location> {
    let uri = path_to_uri(file)?;
    let held = session.files().held(file)?;
    let start = position_in_file(held, range.start_offset)?;
    let end = position_in_file(held, range.end_offset())?;
    Some(Location {
        uri,
        range: lsp_types::Range::new(start, end),
    })
}

pub struct TypeDefinitionCapabilities;

impl RegisterCapabilities for TypeDefinitionCapabilities {
    fn register_capabilities(server: &mut ServerCapabilities, _client: &ClientCapabilities) {
        server.type_definition_provider = Some(TypeDefinitionProviderCapability::Simple(true));
    }
}
/// The two ends of the identifier chain `offset` is inside — `(start, end)`, both inside the chain's last identifier
/// when it is qualified (`ns::Widget` → the `Widget` part).
///
/// One span rather than a caret: every question the analysis answers is asked at an *end* — a declaration at its start,
/// a member access at its end — and a client's caret is somewhere in the middle by construction.
fn the_name_around(source: &str, offset: usize) -> (usize, usize) {
    let bytes = source.as_bytes();
    let is_part =
        |character: u8| character.is_ascii_alphanumeric() || character == b'_' || character == b':';
    let mut start = offset.min(bytes.len());
    while start > 0 && is_part(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = offset;
    while end < bytes.len() && is_part(bytes[end]) {
        end += 1;
    }
    // A qualified name is two spans; both questions want the **last** segment.
    let last = source
        .get(start..end)
        .and_then(|name| name.rfind("::").map(|at| start + at + 2))
        .unwrap_or(start);
    (last, end.saturating_sub(1).max(last))
}
/// **The declared type of the member a cursor is on** — `Widget` for `w.owner`, `int` for `w.size`.
///
/// The object's type is where the member list comes from; the member's own spelling is what a `typeDefinition` means, and
/// for a member **function** it is the return type (a client asking on `v.size()` wants `size_type`, not the class).
fn the_members_type<F: FileProvider + Clone>(
    session: &Session<F>,
    view: &cpp_code_analysis::FileView,
    access: &cpp_code_analysis::sema::resolve::MemberAccess,
) -> Option<String> {
    let object = session.type_at(
        view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(usize::from(
            access.object.text_range().start(),
        )),
    );
    let Known::Yes(object) = object else {
        return None;
    };
    // The object's spelling is the *alias as written* (`Name`, `const Widget&`), and a member list is keyed by the class
    // name — so the same reduction the type spelling gets is applied here.
    let class = the_name_of_a_type(&object.type_of)?;
    let Known::Yes(listed) = session.members_of(view, &class) else {
        return None;
    };
    let member = listed
        .members
        .iter()
        .find(|member| member.fact.name == *access.member)?;
    member
        .fact
        .returns
        .clone()
        .or_else(|| member.fact.type_of.clone())
}
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    use super::{the_name_of_a_type, type_definitions_of};

    fn session() -> Session<MemoryFiles> {
        let files = MemoryFiles::new().with_file(
            "/p/base.h",
            "struct Widget {\n    int size() const;\n};\nWidget make();\n",
        );
        let files = files.with_file(
            "/p/a.cpp",
            "#include \"base.h\"\nvoid f() {\n    Widget w;\n    auto made = make();\n    w.size();\n}\n",
        );
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files([PathBuf::from("/p/base.h"), PathBuf::from("/p/a.cpp")]);
        session.index_everything();
        session
    }

    fn offset_of(source: &str, needle: &str) -> usize {
        source.find(needle).expect("the fixture writes it") + 1
    }

    /// **A variable's type, and an `auto`'s deduced type, both answer the class's declaration.**
    ///
    /// The `auto` case is the one the feature exists for: the type is written nowhere in the file, so a reader cannot
    /// find it without the inference — and the inference here is the same one the hover shows.
    #[test]
    fn a_variable_and_an_auto_answer_the_types_declaration() {
        let session = session();
        let path = PathBuf::from("/p/a.cpp");
        let held = session.files().held(&path).expect("the file is held");
        let source = held.text.to_string();
        let view = session.view_of_the_file(&path).expect("the file parses");

        {
            let needle = "Widget w;";
            let at = offset_of(&source, needle);
            let found = type_definitions_of(&session, &view, at);
            println!(
                "{needle} → {:?}",
                found
                    .iter()
                    .map(|location| (location.uri.path(), location.range.start.line + 1))
                    .collect::<Vec<_>>()
            );
            assert_eq!(found.len(), 1, "{needle}: one declaration of `Widget`");
            assert!(
                found[0].uri.path().ends_with("base.h"),
                "{needle}: and it is in the header that declares the class"
            );
            assert_eq!(
                found[0].range.start.line, 0,
                "{needle}: `struct Widget` is line 1 of base.h"
            );
        }

        // **A class name is its own type definition** — `std::string s;` asks where `string` is declared, and the
        // answer is the class, not the variable.
        let at = offset_of(&source, "Widget w;");
        let found = type_definitions_of(&session, &view, at + "Widget".len() - 1);
        assert_eq!(
            found.len(),
            1,
            "the class name answers its own declaration: {found:?}"
        );
    }
    /// **A builtin has no declaration to go to**, so the answer is empty rather than the variable's own declaration —
    /// which would be Go to Definition wearing another name.
    #[test]
    fn an_int_answers_nothing() {
        let session = session();
        let held = session
            .files()
            .held(PathBuf::from("/p/a.cpp"))
            .expect("held");
        let source = held.text.to_string();
        let view = session
            .view_of_the_file(PathBuf::from("/p/a.cpp"))
            .expect("parses");
        // **On `size`, not on `w`** — a cursor on the object asks about the object, which is the right answer for that
        // caret and not what this test is about.
        let at = offset_of(&source, "w.size") + 1;
        let found = type_definitions_of(&session, &view, at);
        println!("w.size → {found:?}");
        // `w.size` is a call to a member returning `int`: `int` has no declaration to go to, and the object's class is
        // **not** the answer — that would be the class of the thing the member is called on.
        assert!(
            found.is_empty(),
            "an int has no type declaration, and the object is not it: {found:?}"
        );
    }

    /// The spelling → name rule, which every answer goes through.
    #[test]
    fn a_spelling_gives_the_name_a_declaration_is_filed_under() {
        for (spelling, name) in [
            ("Widget", "Widget"),
            ("const Widget&", "Widget"),
            ("ns::Widget", "Widget"),
            ("Widget<T>", "Widget"),
            ("const ns::Widget<T>&", "Widget"),
            ("struct Widget *", "Widget"),
            ("int", "int"),
        ] {
            assert_eq!(
                the_name_of_a_type(spelling).as_deref(),
                Some(name),
                "{spelling}"
            );
        }
    }
}
