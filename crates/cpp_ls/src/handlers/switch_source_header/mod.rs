//! # `textDocument/switchSourceHeader` — the source/header pair of the file in hand
//!
//! clangd's own extension method, and the one editor command that has no protocol equivalent: given `foo.cpp` answer
//! `foo.h`, and given `foo.h` answer `foo.cpp`. The client binds it to a key (the VS Code extension here uses `Alt+O`,
//! which is clangd's own binding) and opens the answer.
//!
//! ```text
//! foo.cpp  →  foo.h        the same stem, a header extension
//! foo.h    →  foo.cpp      and the reverse — `foo.c` in a project that reads the header as C
//! foo.cppm →  foo.cpp      a module interface and its implementation unit
//! foo.cpp  →  foo.cppm     and the reverse, when the file has no header
//! ```
//!
//! # What a file is, for this question
//!
//! ```text
//!   source            .cpp .cc .cxx .c++ .c .m .mm         compiles things
//!   header            .h .hh .hpp .hxx .h++ .inl .ipp .tcc  is included
//!   module interface  .cppm .ixx .mpp .ccm .cxxm            declares a C++20 module, and is compiled itself
//! ```
//!
//! # How the pair is found, in order
//!
//! 1. **The same directory, the same stem**, with the kind's counterpart extensions — the shape a project uses almost
//!    everywhere. A **header** prefers the source extension of its own language family (a `.h` read as C answers `.c` before
//!    `.cpp`), and a module interface prefers C++ sources, because a module interface is C++ by definition.
//! 2. **An include**: a header's source is a file that `#include`s it, and a source's header is one it includes — which is
//!    what finds a pair whose names are not twins (`detail.cpp` including `all.h`). A module interface is reached the same
//!    way: the implementation unit writes `import foo;`, which the directive summary records.
//! 3. **Anything the project lists**, so a pair in another directory is still reachable.
//! 4. **The file a header's implementation would have**, when nothing exists yet: `<stem>.c` for a header read as C and
//!    `<stem>.cpp` otherwise, and `<stem>.cpp` for a module interface. Jumping from a header to its implementation is the
//!    point of the command, and a project that has not written it yet is exactly when a reader wants the empty file.
//!
//! The answer is the **first** candidate in that order, which is what clangd does: a client that wanted a list would have no
//! way to show it (the protocol's result is one URI or none), and the common case has exactly one answer.
//!
//! # What is not answered
//!
//! A file whose extension is none of the three kinds answers `null`, and so does a **source** with no counterpart anywhere —
//! printing a wrong file would move the reader somewhere the language does not name.

use lsp_types::{ClientCapabilities, ServerCapabilities, TextDocumentIdentifier, Uri};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{path_to_uri, uri_to_file_path};

/// **The extension method itself** — clangd's `textDocument/switchSourceHeader`, with clangd's params and result.
///
/// `lsp_types` has no type for it: it is not part of the protocol, so the request is declared here and the dispatch table
/// takes it like any other row. `Option<Url>` is what clangd answers — one URI, or `null` when there is no counterpart.
pub enum SwitchSourceHeader {}

/// **The params, in either shape a client sends.**
///
/// LSP's [`TextDocumentIdentifier`] is `{ "uri": … }`, and that is what clangd's own method takes. The VS Code client
/// sends the LSP-shaped `{ "textDocument": { "uri": … } }` — the shape every other text-document request uses — and the first
/// version of this handler answered `missing field 'uri'`, measured in the extension's log. A method that is an extension
/// has no schema to appeal to, so both are accepted: the field name is not worth telling a client it is too old.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchSourceHeaderParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text_document: Option<TextDocumentIdentifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uri: Option<Uri>,
}

impl SwitchSourceHeaderParams {
    /// The document the request names, whichever field carried it.
    pub fn the_uri(&self) -> Option<&Uri> {
        self.text_document
            .as_ref()
            .map(|document| &document.uri)
            .or(self.uri.as_ref())
    }
}

impl lsp_types::request::Request for SwitchSourceHeader {
    type Params = SwitchSourceHeaderParams;
    type Result = Option<Uri>;
    const METHOD: &'static str = "textDocument/switchSourceHeader";
}

pub async fn on_switch_source_header(
    context: ServerContextSnapshot,
    params: SwitchSourceHeaderParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Option<Uri>> {
    let Some(uri) = params.the_uri().cloned() else {
        // **A request that names no document** is answered `null` rather than an error: the client asked a question the
        // server cannot answer, and a null says so without failing the session.
        return RequestOutcome::Ready(None);
    };
    if let Some(path) = uri_to_file_path(&uri) {
        // The file has to be read for its includes to be known; a header nothing has opened is exactly the case where the
        // answer comes from another file's `#include`.
        context.analysis().prepare(&path).await;
        context.analysis().catch_up(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        // **`Some(None)` is the answer "no counterpart"**, which is `Ready(None)` on the wire — a file with no pair
        // is a real answer, not a missing one.
        let path = uri_to_file_path(&uri)?;
        Some(match the_counterpart_of(session, &path) {
            Some(counterpart) => path_to_uri(&counterpart),
            None => None,
        })
    })
    .await
}

/// **The declarations this extension contributes** — none.
///
/// clangd's method needs no server capability: a client sends it and the server either answers or says `MethodNotFound`.
/// This row exists so the handler takes part in the same registration table as every other, and so the reason it contributes
/// nothing is written down rather than left as a hole.
pub struct SwitchSourceHeaderCapabilities;

impl RegisterCapabilities for SwitchSourceHeaderCapabilities {
    fn register_capabilities(_: &mut ServerCapabilities, _: &ClientCapabilities) {}
}

/// The extensions of each kind of file — see the module documentation for what the three kinds mean here.
const SOURCE_EXTENSIONS: &[&str] = &["cpp", "cc", "cxx", "c++", "c", "m", "mm"];
const HEADER_EXTENSIONS: &[&str] = &["h", "hh", "hpp", "hxx", "h++", "inl", "ipp", "tcc"];
const MODULE_INTERFACE_EXTENSIONS: &[&str] = &["cppm", "ixx", "mpp", "ccm", "cxxm"];

/// **What a file is**, in the three kinds this command pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Source,
    Header,
    /// A C++20 module interface unit: `.cppm`, `.ixx`, `.mpp`. It declares what a module exports and is compiled itself, so
    /// it pairs with the **implementation unit** — the `.cpp` that `import`s it.
    ModuleInterface,
}

impl Kind {
    fn of(extension: &str) -> Option<Self> {
        if SOURCE_EXTENSIONS.contains(&extension) {
            Some(Kind::Source)
        } else if HEADER_EXTENSIONS.contains(&extension) {
            Some(Kind::Header)
        } else if MODULE_INTERFACE_EXTENSIONS.contains(&extension) {
            Some(Kind::ModuleInterface)
        } else {
            None
        }
    }

    /// **The extensions to try for the counterpart of a file of this kind**, most likely first.
    ///
    /// `family` is the language the file is read as, which decides the order between `.c` and `.cpp`: a header a C project
    /// includes answers `.c` before `.cpp`, and one a C++ project includes does the reverse. A module interface is C++ by
    /// definition, so its own order is the C++ one.
    fn counterpart_extensions(
        self,
        family: Option<cpp_parser::features::Family>,
    ) -> Vec<&'static str> {
        use cpp_parser::features::Family;
        let sources: Vec<&'static str> = match family {
            Some(Family::C) => vec!["c", "m", "mm", "cpp", "cc", "cxx", "c++"],
            _ => vec!["cpp", "cc", "cxx", "c++", "c", "m", "mm"],
        };
        match self {
            // A source's counterpart is its header — clangd's answer — and a module interface only when there is no header:
            // a `.cpp` beside both a `.h` and a `.cppm` is the header's implementation, and the interface is reachable from
            // itself.
            Kind::Source => HEADER_EXTENSIONS
                .iter()
                .chain(MODULE_INTERFACE_EXTENSIONS)
                .copied()
                .collect(),
            // A header's counterpart is the source that implements it, or the module interface that declares it.
            Kind::Header => sources
                .into_iter()
                .chain(MODULE_INTERFACE_EXTENSIONS.iter().copied())
                .collect(),
            // A module interface's counterpart is its implementation unit, and nothing else.
            Kind::ModuleInterface => sources,
        }
    }

    /// **The file a counterpart would have**, when this file's kind names one and no candidate exists yet.
    ///
    /// Only a header and a module interface answer one: both declare something a source implements, and a reader standing in
    /// one is usually on their way to writing that source. A source with no header anywhere has no "would-be" pair — the
    /// answer would be a name the project never chose.
    fn expected_counterpart(
        self,
        family: Option<cpp_parser::features::Family>,
    ) -> Option<&'static str> {
        use cpp_parser::features::Family;
        match self {
            Kind::Header if family == Some(Family::C) => Some("c"),
            Kind::Header => Some("cpp"),
            // A module interface is C++, and its implementation unit is a `.cpp`.
            Kind::ModuleInterface => Some("cpp"),
            Kind::Source => None,
        }
    }
}

/// **The file that pairs with this one**, in the order the module documentation gives.
pub(crate) fn the_counterpart_of<F>(
    session: &cpp_code_analysis::Session<F>,
    path: &std::path::Path,
) -> Option<std::path::PathBuf>
where
    F: cpp_code_analysis::FileProvider + Clone,
{
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let kind = Kind::of(&extension)?;
    let family = session.config_for(path).language_for(path).family();
    let counterparts = kind.counterpart_extensions(family);

    let directory = path.parent()?;
    let stem = path.file_stem()?.to_str()?;

    // **1. The same directory, the same stem.**
    for extension in &counterparts {
        let candidate = directory.join(format!("{stem}.{extension}"));
        if exists(session, &candidate) {
            return Some(candidate);
        }
    }

    // **2. An include.** A file that includes this one, or one this one includes, whose name is *not* the twin's — and only
    // among the project's own files, because the include graph is what the index holds. A module interface arrives here the
    // same way a header does: the implementation unit writes `import foo;`, which the directive summary records.
    let listed = session.project_files();
    for candidate in listed {
        let Some(candidate_extension) = candidate
            .extension()
            .and_then(|extension| extension.to_str())
        else {
            continue;
        };
        let candidate_extension = candidate_extension.to_ascii_lowercase();
        if !counterparts.contains(&candidate_extension.as_str()) {
            continue;
        }
        if includes(session, candidate, path) || includes(session, path, candidate) {
            return Some(candidate.clone());
        }
    }

    // **3. Anywhere the project lists**, by the same stem.
    for candidate in listed {
        let Some(candidate_stem) = candidate.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if candidate_stem != stem {
            continue;
        }
        let Some(candidate_extension) = candidate
            .extension()
            .and_then(|extension| extension.to_str())
        else {
            continue;
        };
        let candidate_extension = candidate_extension.to_ascii_lowercase();
        if counterparts.contains(&candidate_extension.as_str()) {
            return Some(candidate.clone());
        }
    }

    // **4. The file this one's counterpart would have**, when nothing exists yet.
    let expected = kind.expected_counterpart(family)?;
    Some(directory.join(format!("{stem}.{expected}")))
}

/// **Does this path exist** — on disk, or among the files the session lists?
///
/// The second half is what makes the twin-by-name step work for a file a project knows and the disk does not show (a file
/// held in memory, a fixture, a virtual file system): the session's own list is the answer the index uses, so asking it here
/// is the same question.
fn exists<F>(session: &cpp_code_analysis::Session<F>, path: &std::path::Path) -> bool
where
    F: cpp_code_analysis::FileProvider + Clone,
{
    if path.exists() {
        return true;
    }
    let wanted = normalized(path);
    session
        .project_files()
        .iter()
        .any(|listed| normalized(listed) == wanted)
}

/// **Does `file` include `target`?** — asked of the index's resolved includes, which is the same answer the compiler's
/// search gives.
fn includes<F>(
    session: &cpp_code_analysis::Session<F>,
    file: &std::path::Path,
    target: &std::path::Path,
) -> bool
where
    F: cpp_code_analysis::FileProvider + Clone,
{
    let Some(summary) = session.directive_summary(file) else {
        return false;
    };
    let target = normalized(target);
    summary.includes.iter().any(|include| {
        include
            .resolved
            .as_deref()
            .is_some_and(|resolved| normalized(resolved) == target)
    })
}

/// A path's spelling with one form of separators, for comparing two answers that came from different places.
fn normalized(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::the_counterpart_of;
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    fn session_with(files: &[(&str, &str)]) -> Session<MemoryFiles> {
        let mut memory = MemoryFiles::new();
        for (path, source) in files {
            memory = memory.with_file(*path, *source);
        }
        let providers = SessionFiles::new(OpenDocuments::new(), memory);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files(
            files
                .iter()
                .map(|(path, _)| std::path::PathBuf::from(*path)),
        );
        session
    }

    /// **The twin by name is the first answer**, in both directions.
    #[test]
    fn the_twin_is_found_in_both_directions() {
        let session = session_with(&[
            ("/p/foo.cpp", "#include \"foo.h\"\n"),
            ("/p/foo.h", "int f();\n"),
        ]);
        assert_eq!(
            the_counterpart_of(&session, std::path::Path::new("/p/foo.cpp")),
            Some(std::path::PathBuf::from("/p/foo.h")),
            "a source's header"
        );
        assert_eq!(
            the_counterpart_of(&session, std::path::Path::new("/p/foo.h")),
            Some(std::path::PathBuf::from("/p/foo.cpp")),
            "a header's source"
        );
    }

    /// **A pair whose names are not twins is found by the include.**
    #[test]
    fn an_include_finds_a_pair_that_is_not_named_alike() {
        let session = session_with(&[
            ("/p/detail.cpp", "#include \"all.h\"\n"),
            ("/p/all.h", "int f();\n"),
        ]);
        assert_eq!(
            the_counterpart_of(&session, std::path::Path::new("/p/detail.cpp")),
            Some(std::path::PathBuf::from("/p/all.h"))
        );
    }

    /// **A file with no pair answers nothing**, for the two kinds whose pair cannot be named without one.
    #[test]
    fn a_file_with_no_pair_answers_nothing() {
        let session = session_with(&[("/p/lonely.cpp", "int main() { return 0; }\n")]);
        assert_eq!(
            the_counterpart_of(&session, std::path::Path::new("/p/lonely.cpp")),
            None
        );
        // And a file that is none of the three kinds, whatever sits beside it.
        let session = session_with(&[("/p/notes.txt", "notes\n"), ("/p/notes.h", "int f();\n")]);
        assert_eq!(
            the_counterpart_of(&session, std::path::Path::new("/p/notes.txt")),
            None
        );
    }

    /// **A header with no implementation anywhere answers the file it would have** — `.cpp` for C++, and `.c` when the `.c`
    /// is there.
    ///
    /// The point of the command is to move the reader to the other half of a pair, and a project that has not written that
    /// half yet is exactly when they want the empty file. A **source** with no header answers nothing: the name it would
    /// have is one the project never chose.
    #[test]
    fn a_header_answers_the_source_it_would_have() {
        let session = session_with(&[("/p/api.h", "int f();\n")]);
        assert_eq!(
            the_counterpart_of(&session, std::path::Path::new("/p/api.h")),
            Some(std::path::PathBuf::from("/p/api.cpp"))
        );

        // The `.c` beside the header is the implementation — the extension list is tried in order, and no `.cpp` exists.
        let c_implementation = session_with(&[
            ("/p/api.h", "int f(void);\n"),
            ("/p/api.c", "int f(void);\n"),
        ]);
        assert_eq!(
            the_counterpart_of(&c_implementation, std::path::Path::new("/p/api.h")),
            Some(std::path::PathBuf::from("/p/api.c"))
        );
    }

    /// **A module interface and its implementation unit pair in both directions.**
    ///
    /// C++20's interface unit is `.cppm` and the implementation unit is a `.cpp` that `import`s it; neither is a header, so
    /// the header extensions must not be consulted, and a `.cpp` beside an interface (and no header) answers the interface.
    #[test]
    fn a_module_interface_pairs_with_its_implementation() {
        let session = session_with(&[
            ("/p/widget.cppm", "export module widget;\n"),
            ("/p/widget.cpp", "import widget;\n"),
        ]);
        assert_eq!(
            the_counterpart_of(&session, std::path::Path::new("/p/widget.cppm")),
            Some(std::path::PathBuf::from("/p/widget.cpp")),
            "an interface's implementation unit"
        );
        assert_eq!(
            the_counterpart_of(&session, std::path::Path::new("/p/widget.cpp")),
            Some(std::path::PathBuf::from("/p/widget.cppm")),
            "and the implementation unit's interface, when there is no header"
        );

        // An interface with nothing beside it answers the implementation it would have.
        let alone = session_with(&[("/p/widget.cppm", "export module widget;\n")]);
        assert_eq!(
            the_counterpart_of(&alone, std::path::Path::new("/p/widget.cppm")),
            Some(std::path::PathBuf::from("/p/widget.cpp"))
        );
    }

    /// **A module interface is not a header**: a source with both a header and an interface answers the header, which is
    /// clangd's answer and the implementation's own declaration.
    #[test]
    fn a_source_with_both_answers_the_header() {
        let session = session_with(&[
            ("/p/widget.h", "int f();\n"),
            ("/p/widget.cppm", "export module widget;\n"),
            ("/p/widget.cpp", "#include \"widget.h\"\n"),
        ]);
        assert_eq!(
            the_counterpart_of(&session, std::path::Path::new("/p/widget.cpp")),
            Some(std::path::PathBuf::from("/p/widget.h"))
        );
    }
}

#[cfg(test)]
mod params_tests {
    use super::SwitchSourceHeaderParams;

    /// **Both shapes on the wire**, because the extension sends the second and clangd's method documents the first.
    #[test]
    fn both_param_shapes_name_the_document() {
        let bare: SwitchSourceHeaderParams = serde_json::from_str(r#"{"uri":"file:///p/foo.cpp"}"#)
            .expect("a bare `uri` is a `TextDocumentIdentifier`");
        assert_eq!(
            bare.the_uri().map(|uri| uri.as_str()),
            Some("file:///p/foo.cpp")
        );

        let nested: SwitchSourceHeaderParams =
            serde_json::from_str(r#"{"textDocument":{"uri":"file:///p/foo.h"}}"#)
                .expect("the LSP-shaped `textDocument` is what a VS Code client sends");
        assert_eq!(
            nested.the_uri().map(|uri| uri.as_str()),
            Some("file:///p/foo.h")
        );

        // And neither answers nothing rather than failing.
        let empty: SwitchSourceHeaderParams = serde_json::from_str("{}").expect("an empty object");
        assert_eq!(empty.the_uri(), None);
    }
}
