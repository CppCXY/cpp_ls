//! # `textDocument/switchSourceHeader` — the source/header pair of the file in hand
//!
//! clangd's own extension method, and the one editor command that has no protocol equivalent: given `foo.cpp` answer
//! `foo.h`, and given `foo.h` answer `foo.cpp`. The client binds it to a key (the VS Code extension here uses `Alt+O`,
//! which is clangd's own binding) and opens the answer.
//!
//! ```text
//! foo.cpp  →  foo.h        the same stem, a header extension
//! foo.h    →  foo.cpp      and the reverse
//! ```
//!
//! # How the pair is found, in order
//!
//! 1. **The same directory, the same stem**, with the counterpart's extensions — the shape a project uses almost everywhere.
//! 2. **An include**: a header's source is a file that `#include`s it, and a source's header is one it includes — which is
//!    what finds a pair whose names are not twins (`detail.cpp` includng `all.h`).
//! 3. **Anything the project lists**, so a pair in another directory is still reachable.
//!
//! The answer is the **first** candidate in that order, which is what clangd does: a client that wanted a list would have no
//! way to show it (the protocol's result is one URI or none), and the common case has exactly one answer.
//!
//! # What is not answered
//!
//! A file with no counterpart answers `null`, and so does one whose extension is neither a source's nor a header's —
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

/// The extensions a **source** file has, and the ones a **header** file has.
const SOURCE_EXTENSIONS: &[&str] = &["cpp", "cc", "cxx", "c++", "c", "m", "mm"];
const HEADER_EXTENSIONS: &[&str] = &["h", "hh", "hpp", "hxx", "h++", "inl", "ipp", "tcc"];

/// **The file that pairs with this one**, in the order [`the_counterpart_of`] documents.
fn the_counterpart_of<F>(
    session: &cpp_code_analysis::Session<F>,
    path: &std::path::Path,
) -> Option<std::path::PathBuf>
where
    F: cpp_code_analysis::FileProvider + Clone,
{
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let (counterparts, looking_for_a_source) = if SOURCE_EXTENSIONS.contains(&extension.as_str()) {
        (HEADER_EXTENSIONS, false)
    } else if HEADER_EXTENSIONS.contains(&extension.as_str()) {
        (SOURCE_EXTENSIONS, true)
    } else {
        return None;
    };

    let directory = path.parent()?;
    let stem = path.file_stem()?.to_str()?;

    // **1. The same directory, the same stem.**
    for extension in counterparts {
        let candidate = directory.join(format!("{stem}.{extension}"));
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // **2. An include.** A file that includes this one, or one this one includes, whose name is *not* the twin's — and only
    // among the project's own files, because the include graph is what the index holds.
    let listed = session.project_files();
    for candidate in listed {
        let Some(candidate_extension) = candidate
            .extension()
            .and_then(|extension| extension.to_str())
        else {
            continue;
        };
        let candidate_extension = candidate_extension.to_ascii_lowercase();
        if looking_for_a_source && !SOURCE_EXTENSIONS.contains(&candidate_extension.as_str()) {
            continue;
        }
        if !looking_for_a_source && !HEADER_EXTENSIONS.contains(&candidate_extension.as_str()) {
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
        let wanted = if looking_for_a_source {
            SOURCE_EXTENSIONS.contains(&candidate_extension.as_str())
        } else {
            HEADER_EXTENSIONS.contains(&candidate_extension.as_str())
        };
        if wanted {
            return Some(candidate.clone());
        }
    }

    None
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

    /// **A file with no pair answers nothing** — the honest answer rather than a neighbour.
    #[test]
    fn a_file_with_no_pair_answers_nothing() {
        let session = session_with(&[("/p/lonely.cpp", "int main() { return 0; }\n")]);
        assert_eq!(
            the_counterpart_of(&session, std::path::Path::new("/p/lonely.cpp")),
            None
        );
        // And a file that is neither a source nor a header, whatever sits beside it.
        let session = session_with(&[("/p/notes.txt", "notes\n"), ("/p/notes.h", "int f();\n")]);
        assert_eq!(
            the_counterpart_of(&session, std::path::Path::new("/p/notes.txt")),
            None
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
