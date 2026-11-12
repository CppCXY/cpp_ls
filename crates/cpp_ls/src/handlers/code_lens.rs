//! # `textDocument/codeLens` — **how many places use each function in this file**
//!
//! A lens that reads `3 references` above a function, and above its declaration in a header when the reader is there
//! — the same number, because it is the same question about the same entity. Only **functions** get one: a type, a
//! variable or a macro has no body and no callers a reader counts.
//!
//! # What is counted, and what is not
//!
//! ```text
//!   counted      a place that *uses* the function — a point the body's work is invoked from
//!   not counted  the definition itself, and a declaration of it (the `void grow(int);` in the header)
//! ```
//!
//! The distinction is [`ReferenceKind::Declaration`] and [`ReferenceKind::Definition`], which the reference query
//! keeps apart: a list a reader renames from wants the promise as well as the body, while a count is about the places
//! that *call* it. Without it a function nobody calls would read `1 reference` on the line that introduces it.
//!
//! **The count is [`super::references::places_of`]'s list, and the command carries that same list** — see the module
//! of `handlers::references`, which is where the one rule lives. Two queries used to answer "where is this used": Find
//! References asked the index, and this file asked the *rename* rule; measured on `EmmyLuaCodeStyle`, the lens read
//! `0 references` over `LuaParseError`'s constructor while Find References at the same byte listed eight uses, and `7`
//! over `CompleteMarker`'s where Find References listed sixteen.
//!
//! **A constructor's count is its class's uses.** The language gives a constructor no name of its own — nothing can
//! write `Pair::Pair(...)` in a call — so the places that name the class are the places that reach it, and that is
//! exactly what Find References answers at that byte. It is more than the construction sites, and it is what the click
//! shows, which is the property a number needs to have.
//!
//! # What it costs
//!
//! One reference query per function, and the query is the index's: the candidates are the declaring file and
//! everything that can see it, each filtered by the summary's summary-level `use_filter` before it is read and lexed.
//! A function that is a class member also pays **one parse per file that has an occurrence**, because attributing an
//! occurrence to a class needs that file's scopes — the same price, and the same shape, as the rename rule.
//!
//! Measured on `EmmyLuaCodeStyle` with `tests/code_lens_probe.rs` (this file's own `Mark.cpp`, seven lenses, index
//! warm): **35 ms for the file, 5 ms a lens** — against **60 ms / 8.6 ms** for the rename rule it replaced, so the
//! unification was also 40% cheaper (the parse the rename rule needed for *its* own answer is gone). A lens is
//! requested when a file is opened or saved, not on every keystroke.
//!
//! # What the client is told
//!
//! A lens with no command is drawn as nothing by several clients, so each carries `editor.action.showReferences` —
//! the editor's own "show me them" — with the places the count was made of. A client that does not know the command
//! still draws the title.

use cpp_code_analysis::{DeclKind, Known};
use lsp_types::{
    ClientCapabilities, CodeLens, CodeLensParams, Command, Location, Position, Range,
    ServerCapabilities, Uri,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{ClientId, RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{path_to_uri, position_in_file, uri_to_file_path};

pub async fn on_code_lens(
    context: ServerContextSnapshot,
    params: CodeLensParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<CodeLens>> {
    let uri = params.text_document.uri;
    let client = context.lsp_features().client_id();

    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
        context.analysis().catch_up(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        // **A project can turn the lens off** (`code_lens.enable = false` in `.cppls.toml`), which is a preference
        // about the editor rather than about the analysis — so it is answered here, at the edge, before any query is
        // made: the point of the switch is not to pay for a count nobody will read. The capability is still
        // advertised, because a client asks for it once at `initialize` and cannot be told later that the provider
        // went away; an empty list is what "no lenses" looks like on the wire.
        if session.project_config().config.code_lens.enable == Some(false) {
            return Some(Vec::new());
        }
        // **Answer from what is known, and let the refresh correct it.** Withholding the list while the index catches up
        // made every keystroke *clear* the lenses and redraw them seconds later — a redraw is worse than a number that
        // grows: the editor is told the answer changed, not that it is unknown. `workspace/codeLens/refresh` (sent when
        // indexing drains) is what asks again, so the number can be low for a moment and then settle, without ever
        // vanishing.
        let view = session.view_of_the_file(&path)?;
        Some(lenses_of(session, &view, &uri, client))
    })
    .await
}

/// **One lens per function this file declares or defines**, with the number of places that use it.
///
/// The file's own summary is the list: a `DeclKind::Function` that is not local. A local lambda or a nested helper is
/// not what a reader counts callers of.
pub fn lenses_of<F: cpp_code_analysis::FileProvider + Clone>(
    session: &cpp_code_analysis::Session<F>,
    view: &cpp_code_analysis::FileView,
    uri: &Uri,
    client: ClientId,
) -> Vec<CodeLens> {
    let Some(held) = session.files().held(&view.path) else {
        return Vec::new();
    };
    let Some(summary) = session.index().summary(&view.path) else {
        return Vec::new();
    };

    let mut lenses = Vec::new();
    for fact in &summary.declarations {
        if fact.kind != DeclKind::Function || fact.local {
            continue;
        }
        let (Some(start), Some(end)) = (
            position_in_file(held, fact.name_range.start_offset),
            position_in_file(held, fact.name_range.end_offset()),
        ) else {
            continue;
        };

        // **Find References' own query, for the same declaration.** The lens used to ask a *different* query —
        // `references::name_level_offsets`, which is the **rename** rule — and the two disagree wherever a spelling
        // and an entity differ. Measured on `EmmyLuaCodeStyle`: the lens read `0 references` over `LuaParseError`'s
        // constructor while Find References at the same byte listed eight uses of its class, and `7` over
        // `CompleteMarker`'s constructor where Find References listed nineteen. A reader who clicks the number and
        // gets another number has been told two things; `references::places_of` is the one answer both now ask, so
        // the count and the peek are the same list by construction.
        //
        // `include_declaration = false` is the user's rule: the definition and the header's declaration are the
        // places the name is *introduced*, not places it is used, and a function nobody calls must not read
        // `1 reference` on the line that introduces it.
        //
        // **No view, no parse for the answer.** The reference query takes the declaration itself
        // (`symbol_references_of`), whose offsets are the summary's own file offsets — the coordinate system that
        // made a header count `0` while five `.cpp` files called the function.
        let Known::Yes(places) =
            super::references::places_of(session, fact, view.path.clone(), false)
        else {
            // A declaration no query can name — a template parameter, a name from a header nothing read. `Unknown`
            // is not "no uses": the lens then says nothing about a number rather than `0 references`, which would be
            // the same false claim the whole-file report was about.
            continue;
        };
        let locations = locations_of(session, &places);
        let uses = places.len();

        lenses.push(CodeLens {
            range: Range::new(start, end),
            command: the_command(client, uri, start, locations, uses),
            data: None,
        });
    }
    lenses
}

/// The places, as protocol locations — for the files this session holds.
///
/// A file the session cannot hold is skipped rather than guessed at: the count still says how many there are, and a
/// missing entry in a list is better than a location nobody can show.
fn locations_of<F: cpp_code_analysis::FileProvider + Clone>(
    session: &cpp_code_analysis::Session<F>,
    places: &[(std::path::PathBuf, cpp_parser::SourceRange)],
) -> Vec<Location> {
    let mut locations = Vec::new();
    for (file, range) in places {
        let Some(held) = session.files().held(file) else {
            continue;
        };
        let Some(uri) = path_to_uri(file) else {
            continue;
        };
        let (Some(start), Some(end)) = (
            position_in_file(held, range.start_offset),
            position_in_file(held, range.end_offset()),
        ) else {
            continue;
        };
        locations.push(Location {
            uri,
            range: Range::new(start, end),
        });
    }
    locations
}
/// **The command a client can actually run**, or a title-only lens when it has none for this.
///
/// `editor.action.showReferences` is the editor's own "show me them", and **VS Code does not accept it from a language
/// server**: it is an internal command, and a server that names it gets "command not found" when the lens is clicked.
/// The extension registers `cppls.showReferences` and forwards to the built-in, so that is what this client is sent.
/// IntelliJ's LSP client does accept the editor command. Every other client is sent **no command at all** — several
/// draw a lens with no command as nothing, and the count is the answer they can use; a command nobody implements would
/// only be an error message on click.
fn the_command(
    client: ClientId,
    uri: &Uri,
    at: Position,
    places: Vec<Location>,
    uses: usize,
) -> Option<Command> {
    let title = match uses {
        1 => "1 reference".to_string(),
        other => format!("{other} references"),
    };
    let command = if client.is_vscode() {
        "cppls.showReferences"
    } else if client.is_intellij() {
        "editor.action.showReferences"
    } else {
        return Some(Command {
            title,
            command: String::new(),
            arguments: None,
        });
    };
    Some(Command {
        title,
        command: command.to_string(),
        arguments: Some(vec![
            serde_json::Value::String(uri.to_string()),
            serde_json::to_value(at).unwrap_or_default(),
            serde_json::to_value(&places).unwrap_or_default(),
        ]),
    })
}

pub struct CodeLensCapabilities;

/// The lens provider, advertised unconditionally: the answer needs nothing from the client, and a client that does not
/// ask for lenses is never sent one.
impl RegisterCapabilities for CodeLensCapabilities {
    fn register_capabilities(server: &mut ServerCapabilities, _client: &ClientCapabilities) {
        server.code_lens_provider = Some(lsp_types::CodeLensOptions {
            resolve_provider: Some(false),
        });
    }
}

#[cfg(test)]
mod tests {
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    use super::*;

    /// **A function's count is the places that use it** — not its definition, not its declaration.
    ///
    /// The fixture is the shape a project writes every time: the header declares, the `.cpp` defines, two sites call.
    /// Three occurrences of the name exist and **two** are references a reader counts.
    #[test]
    fn a_functions_count_is_its_uses() {
        let header = "int grow(int by);\n";
        let source = "\
#include \"a.h\"
int grow(int by) { return by + 1; }
int f() { return grow(1) + grow(2); }
";
        let memory = MemoryFiles::new()
            .with_file("/p/a.h", header)
            .with_file("/p/a.cpp", source);
        let providers = SessionFiles::new(OpenDocuments::new(), memory);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files([
            std::path::PathBuf::from("/p/a.h"),
            std::path::PathBuf::from("/p/a.cpp"),
        ]);
        session.load("/p/a.cpp");
        session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
        session.index_everything();

        let uri = "file:///p/a.cpp".parse::<Uri>().expect("a uri");
        let view = session
            .view_of_the_file(std::path::Path::new("/p/a.cpp"))
            .expect("a view");
        let titles: Vec<String> = lenses_of(&session, &view, &uri, ClientId::VSCode)
            .into_iter()
            .filter_map(|lens| lens.command.map(|command| command.title))
            .collect();
        assert_eq!(
            titles,
            vec!["2 references".to_string(), "0 references".to_string()],
            "`grow` has two calls; `f` is defined and nobody calls it, so it says none — and neither `grow`'s own \
             definition nor the header's declaration is counted"
        );

        // The declaration, in the header — the same number, because it is the same entity.
        let header_uri = "file:///p/a.h".parse::<Uri>().expect("a uri");
        let header_view = session
            .view_of_the_file(std::path::Path::new("/p/a.h"))
            .expect("a view");
        let titles: Vec<String> = lenses_of(&session, &header_view, &header_uri, ClientId::VSCode)
            .into_iter()
            .filter_map(|lens| lens.command.map(|command| command.title))
            .collect();
        assert_eq!(
            titles,
            vec!["2 references".to_string()],
            "the header's declaration counts the same uses"
        );

        // **The shared query says the same two places** — `places_of` is what the lens and
        // `textDocument/references` both ask, so this is the list a click reveals. The declarations are the name
        // being *introduced* — the header's `int grow(int by);` and the `.cpp` definition — and a count that
        // included them would read `4 references` where two sites call the function.
        let summary = session
            .index()
            .summary(std::path::Path::new("/p/a.h"))
            .expect("a summary");
        let fact = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "grow")
            .expect("the header's declaration")
            .clone();
        let places = crate::handlers::references::places_of(
            &session,
            &fact,
            std::path::PathBuf::from("/p/a.h"),
            false,
        );
        let Known::Yes(places) = places else {
            panic!("the declaration is in the index");
        };
        assert_eq!(
            places.len(),
            2,
            "two calls, and neither declaration: {places:?}"
        );
    }

    /// **A constructor's count is what Find References answers for it** — the class's uses, because a constructor has
    /// no name of its own to write.
    ///
    /// Measured on `EmmyLuaCodeStyle`: the lens over `LuaParseError`'s constructor read `0 references` — no line in
    /// the project writes `LuaParseError::LuaParseError` — while Find References at the same byte listed eight uses of
    /// the class. The answer is the class's: here two constructions, the return type of `make`, and the qualifier of
    /// `Pair::Mine`. That is more than the two constructions, and it is what the click shows — which is the property
    /// that matters, because a count a reader cannot open and verify is a number with no evidence behind it.
    #[test]
    fn a_constructors_count_is_what_find_references_answers() {
        let source = "\
struct Pair {
    Pair(int a, int b);
    int Mine() const;
};
int Pair::Mine() const { return 0; }
Pair make() { return Pair(1, 2); }
int use() { Pair p(3, 4); return p.Mine(); }
";
        let session = session_of(&[("/p/pair.cpp", source)]);
        let uri = "file:///p/pair.cpp".parse::<Uri>().expect("a uri");
        let view = session
            .view_of_the_file(std::path::Path::new("/p/pair.cpp"))
            .expect("a view");
        let titles: Vec<(String, String)> = lenses_of(&session, &view, &uri, ClientId::VSCode)
            .into_iter()
            .filter_map(|lens| {
                let name = lens_name(&view, &lens);
                let command = lens.command?;
                Some((command.title, name))
            })
            .collect();

        assert_eq!(
            titles,
            vec![
                ("4 references".to_string(), "Pair".to_string()),
                ("1 reference".to_string(), "Mine".to_string()),
                ("1 reference".to_string(), "Mine".to_string()),
                ("0 references".to_string(), "make".to_string()),
                ("0 references".to_string(), "use".to_string()),
            ],
            "the constructor's count is the class's uses; `Pair::Mine` is called once and says so on both its lines; \
             `make` and `use` are called by nothing"
        );
    }

    /// **The number and the list are one answer** — the command carries exactly the places the title counts.
    ///
    /// A reader clicks `2 references` and the editor shows the command's third argument; a count made of one list and
    /// a peek made of another is a number that cannot be verified. Both come from the same `places` here, and this
    /// test is what keeps it so.
    #[test]
    fn the_count_and_the_places_it_hands_the_editor_are_one_list() {
        let source = "\
int grow(int by) { return by + 1; }
int f() { return grow(1) + grow(2); }
";
        let session = session_of(&[("/p/a.cpp", source)]);
        let uri = "file:///p/a.cpp".parse::<Uri>().expect("a uri");
        let view = session
            .view_of_the_file(std::path::Path::new("/p/a.cpp"))
            .expect("a view");

        let lens = lenses_of(&session, &view, &uri, ClientId::VSCode)
            .into_iter()
            .next()
            .expect("`grow` has a lens");
        let command = lens.command.expect("a command");
        assert_eq!(command.title, "2 references");
        let places = command.arguments.expect("the places")[2].clone();
        let places = places.as_array().expect("an array of locations");
        assert_eq!(
            places.len(),
            2,
            "two calls, and the peek is those two: {places:?}"
        );
    }

    /// **A member of one class is not a member of another** — the case the rename rule had and the reference query
    /// must keep.
    ///
    /// `Format::SpellCheck` is called by nobody, and `Format::NameStyleCheck` calls `Builder::SpellCheck`: a lens on
    /// `Format::SpellCheck` that says `1 reference` is counting another class's member. Measured on a real project as
    /// "a lens said 1 reference while Find All References showed the other class's function".
    #[test]
    fn a_member_of_another_class_is_not_counted() {
        let source = "\
struct Builder { int SpellCheck(int t) { return t; } };
struct Format {
    int NameStyleCheck(Builder &builder, int style) { return builder.SpellCheck(style); }
    int SpellCheck(int t) { return t; }
};
int use(Format &f, Builder &b) { return f.NameStyleCheck(b, 1); }
";
        let session = session_of(&[("/p/a.cpp", source)]);
        let uri = "file:///p/a.cpp".parse::<Uri>().expect("a uri");
        let view = session
            .view_of_the_file(std::path::Path::new("/p/a.cpp"))
            .expect("a view");
        let titles: Vec<(String, String)> = lenses_of(&session, &view, &uri, ClientId::VSCode)
            .into_iter()
            .filter_map(|lens| {
                let name = lens_name(&view, &lens);
                let command = lens.command?;
                Some((command.title, name))
            })
            .collect();
        assert_eq!(
            titles,
            vec![
                ("1 reference".to_string(), "SpellCheck".to_string()),
                ("1 reference".to_string(), "NameStyleCheck".to_string()),
                ("0 references".to_string(), "SpellCheck".to_string()),
                ("0 references".to_string(), "use".to_string()),
            ],
            "the call in `NameStyleCheck` is `Builder`'s member, so `Format::SpellCheck` has none — while \
             `NameStyleCheck` itself is called once"
        );
    }

    /// The name under a lens, from the file's own text — the fixture's text is what the lens ranges are in.
    fn lens_name(view: &cpp_code_analysis::FileView, lens: &CodeLens) -> String {
        let Some(offset) = crate::util::offset_at_position(view, lens.range.start) else {
            return String::new();
        };
        let text = &view.source[offset.get().min(view.source.len())..];
        text.chars()
            .take_while(|character| character.is_alphanumeric() || *character == '_')
            .collect()
    }

    /// A session over a fixture of `(path, text)` pairs, every file indexed.
    fn session_of(files: &[(&str, &str)]) -> Session<MemoryFiles> {
        let mut memory = MemoryFiles::new();
        let mut paths = Vec::new();
        for (path, text) in files {
            memory = memory.with_file(*path, *text);
            paths.push(std::path::PathBuf::from(path));
        }
        let providers = SessionFiles::new(OpenDocuments::new(), memory);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files(paths.clone());
        for path in &paths {
            session.load(path);
            session.want_cooked_reading(path);
        }
        session.index_everything();
        session
    }

    /// **The command is the one this client can run**, and a client with none gets a title and nothing else.
    ///
    /// `editor.action.showReferences` is the editor's own, and VS Code refuses it from a language server — the
    /// extension registers `cppls.showReferences` and forwards. IntelliJ accepts the editor command; anything else is
    /// sent no command, because a command nobody implements is an error message on click.
    #[test]
    fn the_command_is_the_one_this_client_can_run() {
        let uri: Uri = "file:///p/a.cpp".parse().expect("a uri");
        let at = Position::new(3, 4);
        for (client, expected) in [
            (ClientId::VSCode, "cppls.showReferences"),
            (ClientId::Intellij, "editor.action.showReferences"),
        ] {
            let command = the_command(client, &uri, at, Vec::new(), 2).expect("a command");
            assert_eq!(command.command, expected, "{client:?}");
            assert_eq!(command.title, "2 references");
            assert!(
                command.arguments.is_some(),
                "{client:?} needs the places the count was made of"
            );
        }

        let other = the_command(ClientId::Other, &uri, at, Vec::new(), 1).expect("a lens");
        assert_eq!(other.title, "1 reference");
        assert_eq!(
            other.command, "",
            "a client with no command for this shows the count alone"
        );
        assert!(other.arguments.is_none());
    }
}
