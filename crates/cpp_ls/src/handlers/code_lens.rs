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
//! The distinction is [`ReferenceKind::Declaration`], which the reference query now keeps: a list a reader renames
//! from wants the promise as well as the body, while a count is about the places that *call* it. Without it a
//! function nobody calls would read `1 reference` on the line that introduces it.
//!
//! # What it costs
//!
//! One reference query per function, and the query is the index's: the candidates are the declaring file and
//! everything that can see it, each filtered by the summary's identifier Bloom filter before it is read and lexed.
//! Measured on `EmmyLuaCodeStyle` (1 086 files): **0.7–0.9 ms** per query once warm and **9.7 ms** for the first, so a
//! file of forty functions costs tens of milliseconds — and a lens is requested when a file is opened or saved, not on
//! every keystroke. The measurement is `tests/reference_cost_probe.rs`, so it can be run again on another project.
//!
//! # What the client is told
//!
//! A lens with no command is drawn as nothing by several clients, so each carries `editor.action.showReferences` —
//! the editor's own "show me them" — with the places the count was made of. A client that does not know the command
//! still draws the title.

use cpp_code_analysis::{DeclKind, Known, ReferenceBudget, ReferenceKind};
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

        // **The fact, not an offset.** `symbol_references(view, offset)` finds the declaration by looking a *rendering*
        // offset up in the scope tree — and the summary's `name_range` is a **file** offset. The two coincide only in a
        // file whose rendering equals its text, so a header with includes behind it counted nothing: measured on
        // `EmmyLuaCodeStyle`, `GetState` in `DiagnosticBuilder.h` answered `0 references` while the `.cpp` files call
        // it. The declaration is already here, so the symbol is asked for directly and no coordinate is converted.
        let Known::Yes(found) =
            session.symbol_references_of(fact, view.path.clone(), ReferenceBudget::default())
        else {
            continue;
        };
        let uses = found
            .files
            .iter()
            .flat_map(|file| &file.references)
            .filter(|reference| matches!(reference.kind, ReferenceKind::Use { .. }))
            .count();
        let places = places_of(session, &found);

        lenses.push(CodeLens {
            range: Range::new(start, end),
            command: the_command(client, uri, start, places, uses),
            data: None,
        });
    }
    lenses
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

/// The places the count was made of, as the editor's own "show references" takes them.
///
/// A file this session cannot hold is skipped rather than guessed at: the count still says how many there are, and a
/// missing entry in a list is better than a location nobody can show.
fn places_of<F: cpp_code_analysis::FileProvider + Clone>(
    session: &cpp_code_analysis::Session<F>,
    found: &cpp_code_analysis::SymbolReferences,
) -> Vec<Location> {
    let mut places = Vec::new();
    for file in &found.files {
        let Some(held) = session.files().held(&file.file) else {
            continue;
        };
        let Some(uri) = path_to_uri(&file.file) else {
            continue;
        };
        for reference in &file.references {
            if !matches!(reference.kind, ReferenceKind::Use { .. }) {
                continue;
            }
            let (Some(start), Some(end)) = (
                position_in_file(held, reference.range.start_offset),
                position_in_file(held, reference.range.end_offset()),
            ) else {
                continue;
            };
            places.push(Location {
                uri: uri.clone(),
                range: Range::new(start, end),
            });
        }
    }
    places
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
