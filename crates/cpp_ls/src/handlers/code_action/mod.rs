//! # `textDocument/codeAction` — the fixes this server knows how to make
//!
//! A **framework of one provider per kind of fix**, rather than a match on the diagnostics a client
//! happened to send. The actions worth having are **request-driven** — a question about the cursor,
//! answered with an edit — and a diagnostic is one input to that question rather than its owner.
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
    // **The names the client is asking about**, from the diagnostics it sent with the request. A `missing-include`
    // finding underlines the whole declaration while its name is one token inside it, so a lift-arrow pressed anywhere
    // in that range has to be told which name the finding meant.
    let wanted: Vec<String> = params
        .context
        .diagnostics
        .iter()
        .filter_map(named_by_a_diagnostic)
        .collect();
    // **Where the milliseconds go**, when asked.
    let trace = cpp_code_analysis::probes::code_action();
    let started = std::time::Instant::now();

    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }
    let prepared = started.elapsed();

    let answer = snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let written = session.view_of_the_file(&path)?;
        let after_the_file = std::time::Instant::now();
        let in_the_file = crate::util::file_offset_at_position(&written, position)?;
        let view = session.view(&path)?;
        let after_the_rendering = std::time::Instant::now();
        let offset = view.reading_offset_of(
            cpp_code_analysis::file::coordinates::FileOffset::new(in_the_file.get()),
        )?;

        let fixes = fixes_of_named(session, &view, offset, &wanted);
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

    /// The position just after the file's **last top-level `import`** — or after its module declaration when it has no
    /// imports, or the top of the file when it has neither.
    ///
    /// A module import is not an `#include`: [module.unit] puts the imports after the module declaration, and inserting
    /// one into the include block (which is what [`FixContext::after_the_includes`] answers) produces a file that does
    /// not compile.
    pub fn after_the_module_imports(&self) -> Position {
        let mut last: Option<usize> = None;
        for (number, line) in self.view.source.lines().enumerate() {
            let trimmed = line.trim_start();
            let imports = trimmed.starts_with("import ") && trimmed.ends_with(';');
            let declares_a_module = (trimmed.starts_with("module ")
                || trimmed.starts_with("export module "))
                && trimmed.ends_with(';');
            if imports || declares_a_module {
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
///
/// Only the tests call this now — the server asks [`fixes_of_named`] with the names a diagnostic named — so
/// it is compiled for them and not for the binary.
#[cfg(test)]
pub fn fixes_of<F: FileProvider + Clone>(
    session: &Session<F>,
    view: &FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
) -> Vec<CodeAction> {
    fixes_of_named(session, view, offset, &[])
}

///
/// Two sources decide "which name is this about", and neither is enough alone: a cursor in the middle of a declaration
/// is not on the name, and a client may send no diagnostics at all (a lightbulb with no squiggle in view). A name from
/// either is a name to offer an include for.
pub fn fixes_of_named<F: FileProvider + Clone>(
    session: &Session<F>,
    view: &FileView,
    offset: cpp_code_analysis::file::coordinates::ReadingOffset,
    wanted: &[String],
) -> Vec<CodeAction> {
    let context = FixContext {
        session,
        view,
        offset,
    };
    let mut fixes = Vec::new();
    fixes.extend(include_fix(&context, wanted));
    // The next provider is one line: `fixes.extend(expand_macro_fix(&context));`
    fixes
}

/// **The name a diagnostic is about**, from the `data` this server attached to it.
///
/// One rule owns this shape at the moment — `missing-include`, whose finding underlines a whole declaration and names
/// one type inside it. A diagnostic with no data, or data that is not `{"name": …}`, contributes nothing.
fn named_by_a_diagnostic(diagnostic: &lsp_types::Diagnostic) -> Option<String> {
    let data = diagnostic.data.as_ref()?;
    data.get("name")
        .and_then(|name| name.as_str())
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string())
}

/// **`source.include`: the header or module that declares a name this file cannot see** — the fix the
/// `missing-include` finding exists for.
///
/// Offered when the name does not resolve here, which includes the cursor case and the diagnostic case: `wanted` carries
/// the names a `missing-include` finding named (see [`named_by_a_diagnostic`]), and the cursor supplies one when the
/// client sent no diagnostics.
///
/// # The spelling is the one a compiler will find
///
/// A header under one of this file's include directories is spelled **relative to that directory** —
/// `#include "widgets/widget.h"`, not the absolute path the first version wrote, which compiles only on the machine
/// that produced it and only because the path happens to exist. A header under no include directory falls back to a
/// path relative to the including file, which is what a quoted `#include` searches first.
///
/// # A module is not a header
///
/// When the candidate file is a module interface (`.cppm`, or the interface unit of a module), the fix is
/// `import <name>;` — placed after the module's own declaration and imports rather than in the include block.
fn include_fix<F: FileProvider + Clone>(
    context: &FixContext<'_, F>,
    wanted: &[String],
) -> Vec<CodeAction> {
    let mut names: Vec<String> = Vec::new();
    if !context.resolves()
        && let Some(name) = context.name_at_the_cursor()
    {
        names.push(name.to_string());
    }
    for name in wanted {
        if !names.contains(name)
            && !matches!(
                context.session.index().definition(name, &context.view.path),
                Known::Yes(_)
            )
        {
            names.push(name.clone());
        }
    }
    if names.is_empty() {
        return Vec::new();
    }

    // **The unfiltered lookup, deliberately.** `files_declaring` is the *visible* set, and a header that is not yet
    // included is exactly the one this fix exists to offer — it would answer nothing for the only case that matters.
    // `every_fact_named` gives the files that declare the spelling, and the reader picks the header they mean.
    //
    // A bounded list: a name declared in fifty headers would be a menu of fifty, which is a different problem from the
    // one this solves. The first few are what a reader picks from.
    const MOST_OFFERED: usize = 5;
    let include_paths = context.session.config_for(&context.view.path).include_paths;
    let mut offered: Vec<std::path::PathBuf> = Vec::new();
    let mut fixes = Vec::new();
    for name in &names {
        for (file, _) in context.session.index().every_fact_named(name) {
            if file == context.view.path || offered.contains(&file) {
                continue;
            }
            offered.push(file.clone());
            if offered.len() > MOST_OFFERED {
                break;
            }
            // **A module interface is imported, not included.** The name it declares is exported, and adding an
            // `#include` for a `.cppm` would not even compile.
            if let Some(module) = the_module_of(context, &file) {
                let at = context.after_the_module_imports();
                if let Some(fix) = context.insert_at(
                    format!("add `import {module};` for `{name}`"),
                    CodeActionKind::QUICKFIX,
                    at,
                    format!("import {module};\n"),
                ) {
                    fixes.push(fix);
                }
                continue;
            }
            let spelling = the_include_spelling(&file, &context.view.path, &include_paths);
            if let Some(fix) = context.insert_at(
                format!("add `#include \"{spelling}\"` for `{name}`"),
                CodeActionKind::QUICKFIX,
                context.after_the_includes(),
                format!("#include \"{spelling}\"\n"),
            ) {
                fixes.push(fix);
            }
        }
    }
    fixes
}

/// **The module a file declares**, when it declares one — `report` for `export module report;`.
///
/// An implementation unit (`module report;`) counts: importing the module is what brings the declarations the file's
/// interface exported, and the implementation unit is the file the facts came from in exactly that case.
fn the_module_of<F: FileProvider + Clone>(
    context: &FixContext<'_, F>,
    file: &std::path::Path,
) -> Option<String> {
    let summary = context.session.index().summary(file)?;
    let module = summary.modules.module.as_deref()?;
    // A file that declares a module but no interface for it — a partition or an implementation unit whose interface
    // the index does not hold — is still importable by name; the analysis is not in a position to be surer than the
    // text.
    (!module.is_empty()).then(|| module.to_string())
}

/// **How to spell an `#include` for `header` from `asking`**, given this file's include directories.
///
/// Relative to an include directory first (that is what `-I` means, and what survives a move of the project), then
/// relative to the including file, then — only when neither applies — the absolute path, which is a spelling that
/// compiles on this machine and nowhere else.
pub fn the_include_spelling(
    header: &std::path::Path,
    asking: &std::path::Path,
    include_paths: &[cpp_code_analysis::include::config::IncludePath],
) -> String {
    let header_normal = header.to_string_lossy().replace('\\', "/");
    for path in include_paths {
        let directory = path.directory.to_string_lossy().replace('\\', "/");
        let directory = directory.trim_end_matches('/');
        if directory.is_empty() {
            continue;
        }
        let with_slash = format!("{directory}/");
        if let Some(relative) = header_normal.strip_prefix(&with_slash) {
            return relative.to_string();
        }
    }
    // **Relative to the file**, which is what a quoted include searches first: `../include/widget.h`.
    if let (Some(from), Some(to)) = (asking.parent(), header.parent()) {
        let from: Vec<String> = from
            .components()
            .map(|part| part.as_os_str().to_string_lossy().to_string())
            .collect();
        let to: Vec<String> = to
            .components()
            .map(|part| part.as_os_str().to_string_lossy().to_string())
            .collect();
        let shared = from
            .iter()
            .zip(&to)
            .take_while(|(left, right)| left.eq_ignore_ascii_case(right))
            .count();
        if shared > 0 {
            let mut relative: Vec<String> = vec!["..".to_string(); from.len() - shared];
            relative.extend(to[shared..].iter().cloned());
            if let Some(file) = header.file_name() {
                relative.push(file.to_string_lossy().to_string());
            }
            let spelled = relative.join("/");
            if !spelled.is_empty() {
                return spelled;
            }
        }
    }
    header_normal
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
    /// **The spelling is one a compiler will find** — relative to an include directory, not an absolute path.
    ///
    /// The first version wrote the header's absolute path, which compiles on the machine that produced it and only
    /// because the file happens to exist there. A project's `-I` directory is what a quoted include is resolved against,
    /// so the fix spells `widgets/widget.h` for a header under `include/`.
    #[test]
    fn an_include_is_spelled_relative_to_the_include_directory() {
        let spelling = the_include_spelling(
            std::path::Path::new("/p/include/widgets/widget.h"),
            std::path::Path::new("/p/src/use.cpp"),
            &[cpp_code_analysis::include::config::IncludePath::user(
                "/p/include",
            )],
        );
        assert_eq!(spelling, "widgets/widget.h");

        // **With no include directory**, relative to the file — `../include/widgets/widget.h` — which a quoted include
        // searches before anything else.
        let relative = the_include_spelling(
            std::path::Path::new("/p/include/widgets/widget.h"),
            std::path::Path::new("/p/src/use.cpp"),
            &[],
        );
        assert_eq!(relative, "../include/widgets/widget.h");
    }

    /// **A module interface is imported, not included** — `import report;`, placed after the module declaration.
    ///
    /// The candidate file is a `.cppm`, and `#include "report.cppm"` would not compile. The import goes after the
    /// module's own line rather than into the include block, which is where [module.unit] puts it.
    #[test]
    fn a_module_interface_offers_an_import_and_places_it_after_the_declaration() {
        let use_cpp = "export module app;\n\nexport void f(Report& report);\n";
        let mut memory = MemoryFiles::new();
        memory = memory
            .with_file(
                "/p/report.cppm",
                "export module report;\nexport struct Report {};\n",
            )
            .with_file("/p/use.cpp", use_cpp);
        let session = Session::with_config(
            "/p",
            SessionFiles::new(OpenDocuments::new(), memory),
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        let mut session = session;
        session.add_project_files([PathBuf::from("/p/report.cppm"), PathBuf::from("/p/use.cpp")]);
        session.index_everything();
        let path = PathBuf::from("/p/use.cpp");
        let held = session.files().held(&path).expect("the file is held");
        let view =
            cpp_code_analysis::FileView::parse_with(held, &cpp_parser::MacroEnvironment::default());
        let at = use_cpp.find("Report").expect("the fixture names it");

        let fixes = fixes_of_named(
            &session,
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
            &["Report".to_string()],
        );
        let import = fixes
            .iter()
            .find(|fix| fix.title.contains("import report"))
            .unwrap_or_else(|| panic!("an `import report;` is offered: {fixes:?}"));
        let edit = import
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
            insert.range.start.line, 1,
            "the import goes after `export module app;`, not into an include block: {:?}",
            insert.range
        );
        assert!(insert.new_text.contains("import report;"), "{insert:?}");
    }
}
