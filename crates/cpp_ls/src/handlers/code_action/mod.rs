//! # `textDocument/codeAction` — the fixes this server knows how to make
//!
//! A **framework of one provider per kind of fix**, rather than a match on the diagnostics a client happened to send.
//! The reason is in what the diagnostics are worth on real code: the checks fire rarely and deliberately (see
//! `docs/cold-start-cpu.md`'s census), so an action that exists only for a fired diagnostic would almost never be
//! offered. The actions worth having are **request-driven** — a question about the cursor, answered with an edit — and
//! a diagnostic is one input to that question rather than its owner.
//!
//! ```text
//! FixContext    the cursor: the session, the view, the offset, and the client's range
//!   name_at_the_cursor   the identifier there, by the lexer's own reading
//!   resolves             does the name resolve here? (the index's answer, not a guess)
//!   after_the_includes   where a new `#include` goes
//!   insert_at            the one edit most fixes are: insert text at a position
//! providers     one function each, offered in the order below
//!   include_fix          `source.include` — the header that declares an unresolved name
//! ```
//!
//! Adding a provider is one function and one line in [`fixes_of`]; nothing about the protocol, the capability or the
//! dispatch changes. That is the whole point of the shape.

use cpp_code_analysis::{FileProvider, FileView, Known, Session};
use lsp_types::{
    ClientCapabilities, CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, Position, Range, ServerCapabilities, TextEdit, Uri,
    WorkspaceEdit,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::uri_to_file_path;

pub async fn on_code_action(
    context: ServerContextSnapshot,
    params: CodeActionParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<CodeActionOrCommand>> {
    let uri = params.text_document.uri;
    let position = params.range.start;
    // **Where the milliseconds go**, when asked. This request is sent on every caret move and `docs/lsp-status.md`
    // measured **57 ms for zero actions**, so the first question is which step owns it: waiting for the analysis,
    // parsing the file, building the rendering, or the fix provider's own queries.
    let trace = std::env::var_os("CPPLS_TRACE_CODE_ACTION").is_some();
    let started = std::time::Instant::now();

    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }
    let prepared = started.elapsed();

    let answer = snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        // **The cursor is read from the file's own text, then mapped into the rendering** — see hover for the
        // measurement: a rendering has the directives resolved and its lines merged, so converting a client's
        // position *through* it names a different place (docs/lsp-status.md, P1 ③).
        let written = session.view_of_the_file(&path)?;
        let after_the_file = std::time::Instant::now();
        let in_the_file = crate::util::file_offset_at_position(&written, position)?;
        let view = session.view(&path)?;
        let after_the_rendering = std::time::Instant::now();
        let offset = view.reading_offset_of(
            cpp_code_analysis::file::coordinates::FileOffset::new(in_the_file.get()),
        )?;

        let fixes = fixes_of(session, &view, offset);
        if trace {
            eprintln!(
                "[codeAction] file {:?}, rendering {:?}, fixes {:?}, {} action(s)",
                after_the_file - started,
                after_the_rendering - after_the_file,
                after_the_rendering.elapsed(),
                fixes.len()
            );
        }
        Some(
            fixes
                .into_iter()
                .map(CodeActionOrCommand::CodeAction)
                .collect(),
        )
    })
    .await;
    if trace {
        eprintln!(
            "[codeAction] prepare {:?}, total {:?}",
            prepared,
            started.elapsed()
        );
    }
    answer
}

/// What a fix provider is given: the cursor, and the few questions every provider asks about it.
pub struct FixContext<'a, F: FileProvider + Clone> {
    pub session: &'a Session<F>,
    pub view: &'a FileView,
    pub offset: cpp_code_analysis::file::coordinates::ReadingOffset,
}

impl<F: FileProvider + Clone> FixContext<'_, F> {
    /// The identifier at the cursor, read off the text around it — `None` on punctuation and whitespace.
    ///
    /// From the text rather than from a token because the cursor may sit **between** two characters: a client sends the
    /// position of the caret, which at the end of a name is just past its last character. Walking out from the offset in
    /// both directions answers that without deciding which side the caret "belongs" to.
    pub fn name_at_the_cursor(&self) -> Option<&str> {
        let source: &str = &self.view.source;
        let bytes = source.as_bytes();
        let offset = self.offset.get();
        if offset > bytes.len() {
            return None;
        }
        let is_identifier_byte = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
        let mut start = offset;
        while start > 0 && is_identifier_byte(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = offset;
        while end < bytes.len() && is_identifier_byte(bytes[end]) {
            end += 1;
        }
        if start == end || bytes[start].is_ascii_digit() {
            return None;
        }
        source.get(start..end)
    }

    /// Does the name at the cursor resolve here? A name that does needs no include.
    pub fn resolves(&self) -> bool {
        matches!(
            self.session.definitions(self.view, self.offset),
            Known::Yes(_)
        )
    }

    /// The position just after the file's **last** `#include`, or the top of the file when there is none.
    ///
    /// After the last one rather than alphabetically among them: this is a line the user will read, and a new include
    /// appended to the block is the shape every C++ editor produces. A file with no includes gets it at line 0.
    pub fn after_the_includes(&self) -> Position {
        let mut last = None;
        for (number, line) in self.view.source.lines().enumerate() {
            if line.trim_start().starts_with("#include") {
                last = Some(number);
            }
        }
        match last {
            Some(number) => Position {
                line: (number + 1) as u32,
                character: 0,
            },
            None => Position {
                line: 0,
                character: 0,
            },
        }
    }

    /// One action whose whole edit is **inserting `text` at a position** — the shape most fixes take.
    pub fn insert_at(
        &self,
        title: String,
        kind: CodeActionKind,
        at: Position,
        text: String,
    ) -> Option<CodeAction> {
        let uri: Uri = crate::util::path_to_uri(&self.view.path)?;
        Some(CodeAction {
            title,
            kind: Some(kind),
            diagnostics: None,
            edit: Some(WorkspaceEdit {
                changes: None,
                document_changes: Some(lsp_types::DocumentChanges::Edits(vec![
                    lsp_types::TextDocumentEdit {
                        text_document: lsp_types::OptionalVersionedTextDocumentIdentifier {
                            uri: uri.clone(),
                            version: None,
                        },
                        edits: vec![lsp_types::OneOf3::Left(TextEdit {
                            range: Range { start: at, end: at },
                            new_text: text,
                        })],
                    },
                ])),
                change_annotations: None,
            }),
            command: None,
            is_preferred: Some(false),
            disabled: None,
            data: None,
        })
    }
}

/// **Every fix this server offers at a cursor**, in the order a client may show them.
pub fn fixes_of<F: FileProvider + Clone>(
    session: &Session<F>,
    view: &FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
) -> Vec<CodeAction> {
    let context = FixContext {
        session,
        view,
        offset,
    };
    let mut fixes = Vec::new();
    fixes.extend(include_fix(&context));
    // The next provider is one line: `fixes.extend(expand_macro_fix(&context));`
    fixes
}

/// **`source.include`: the header that declares the name under the cursor** — offered only when it does not resolve.
///
/// This is the action a C++ reader uses more than any other, and it is the reason the framework exists: it needs no
/// diagnostic to fire, only the index's answer to "which headers declare this spelling", which
/// [`cpp_code_analysis::ProjectIndex::files_declaring`] already gives with the include-visibility rule.
fn include_fix<F: FileProvider + Clone>(context: &FixContext<'_, F>) -> Vec<CodeAction> {
    if context.resolves() {
        return Vec::new();
    }
    let Some(name) = context.name_at_the_cursor() else {
        return Vec::new();
    };
    let name = name.to_string();

    // **The unfiltered lookup, deliberately.** `files_declaring` is the *visible* set, and a header that is not yet
    // included is exactly the one this fix exists to offer — it would answer nothing for the only case that matters.
    // `every_fact_named` gives the files that declare the spelling, and the reader picks the header they mean.
    //
    // A bounded list: a name declared in fifty headers would be a menu of fifty, which is a different problem from the
    // one this solves. The first few are what a reader picks from.
    const MOST_OFFERED: usize = 5;
    let at = context.after_the_includes();
    let mut offered: Vec<std::path::PathBuf> = Vec::new();
    let mut fixes = Vec::new();
    for (file, _) in context.session.index().every_fact_named(&name) {
        if file == context.view.path || offered.contains(&file) {
            continue;
        }
        offered.push(file.clone());
        if offered.len() > MOST_OFFERED {
            break;
        }
        let header = file.to_string_lossy().replace('\\', "/");
        if let Some(fix) = context.insert_at(
            format!("add `#include` for `{name}` from {header}"),
            CodeActionKind::QUICKFIX,
            at,
            format!("#include \"{header}\"\n"),
        ) {
            fixes.push(fix);
        }
    }
    fixes
}

pub struct CodeActionCapabilities;

impl RegisterCapabilities for CodeActionCapabilities {
    fn register_capabilities(server: &mut ServerCapabilities, _client: &ClientCapabilities) {
        server.code_action_provider = Some(CodeActionProviderCapability::Simple(true));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    use super::*;

    fn a_project(use_cpp: &str) -> Session<MemoryFiles> {
        let files = MemoryFiles::new()
            .with_file("/p/base.h", "struct Widget {\n    int size;\n};\n")
            .with_file("/p/use.cpp", use_cpp);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files([PathBuf::from("/p/base.h"), PathBuf::from("/p/use.cpp")]);
        session.index_everything();
        session
    }

    /// **The fix that exists because the name does not resolve**: the header that declares it.
    ///
    /// The lookup is deliberately the **unfiltered** one — a header that is not yet included is invisible to a
    /// visibility-filtered query, which is exactly the case this action exists for.
    #[test]
    fn an_unresolved_name_offers_the_header_that_declares_it() {
        let source = "void f(Widget w) {\n    (void)w;\n}\n";
        let session = a_project(source);
        let path = PathBuf::from("/p/use.cpp");
        let held = session.files().held(&path).expect("the file is held");
        let view =
            cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());
        let at = source.find("Widget").expect("the fixture names it");

        let fixes = fixes_of(
            &session,
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
        );
        assert_eq!(fixes.len(), 1, "one header declares it: {fixes:?}");
        assert!(fixes[0].title.contains("base.h"), "{:?}", fixes[0].title);

        // The edit is an insertion at the top of the file, because the file has no includes to append to.
        let edit = fixes[0]
            .edit
            .as_ref()
            .and_then(|edit| edit.document_changes.as_ref())
            .expect("the action carries an edit");
        let lsp_types::DocumentChanges::Edits(edits) = edit else {
            panic!("expected text edits");
        };
        let lsp_types::OneOf3::Left(insert) = &edits[0].edits[0] else {
            panic!("expected a plain text edit");
        };
        assert_eq!(
            insert.range.start, insert.range.end,
            "an insertion, not a replacement"
        );
        assert!(insert.new_text.contains("base.h"), "{:?}", insert.new_text);
    }

    /// And a name that **does** resolve offers nothing: the action is only a fix while the name is broken.
    #[test]
    fn a_name_that_resolves_offers_nothing() {
        let source = "#include \"base.h\"\nvoid f(Widget w) {\n    (void)w;\n}\n";
        let session = a_project(source);
        let path = PathBuf::from("/p/use.cpp");
        let held = session.files().held(&path).expect("the file is held");
        let view =
            cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());
        let at = source.find("Widget").expect("the fixture names it");

        assert!(
            fixes_of(
                &session,
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(at)
            )
            .is_empty(),
            "a name that resolves is not a missing include"
        );
    }
}
