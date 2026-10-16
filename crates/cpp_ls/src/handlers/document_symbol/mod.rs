//! # `textDocument/documentSymbol` — the file's own structure
//!
//! The one handler whose query is deliberately **not** the cooked reading. Every other feature asks what a compiler
//! makes of the file; an outline asks what the file **is**, and the two answers differ in both directions:
//!
//! ```text
//! #if 0                                in the outline (the reader is editing it), never compiled
//! struct Never { int x; };
//! #endif
//! DECLARE_HANDLE(HWND);                not in the outline: the file writes a call, not a declaration
//! ```
//!
//! So the tree comes from the file's own declarations ([`cpp_code_analysis::FileSummary::outline`]) and the nesting
//! from their scopes — a class's members under the class, a namespace's contents under the namespace.
//!
//! # Two ranges per symbol, and the client needs both
//!
//! `range` is the **whole declaration** and `selection_range` is the **name**: a client folds or highlights with
//! the first and puts the cursor, the breadcrumb and the rename box on the second. The protocol requires the second
//! to be inside the first, which is a property of the facts rather than something this layer arranges — a
//! declaration's name is written inside it — but a fact read out of a *recovered* parse can have a range that does
//! not hold, so the pair is checked here and the name's range is used for both when it does not. A client that
//! rejected an outline because one entry's ranges disagreed would show nothing at all.
//!
//! # Freshness, which for this feature is the whole game
//!
//! An outline is refreshed as the user types, and a file's summary is dropped the moment it is edited. That is why
//! [`cpp_code_analysis::Session::outline`] falls back to the buffer's own parse: the alternative is a panel that
//! blinks empty between a keystroke and the next drain.

use cpp_code_analysis::{FileView, OutlineSymbol};
use lsp_types::{
    ClientCapabilities, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, OneOf,
    Position, Range, ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{symbol_kind, uri_to_file_path};

pub async fn on_document_symbol(
    context: ServerContextSnapshot,
    params: DocumentSymbolParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<DocumentSymbolResponse> {
    let uri = params.text_document.uri;

    // Read in first, under the write lock: an outline of a file nobody has read would be an empty outline, which is
    // a wrong answer rather than a missing one.
    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let outline = session.outline(&view);

        Some(DocumentSymbolResponse::Nested(symbols(&view, &outline)))
    })
    .await
}

/// The outline, as the protocol's nested symbols.
///
/// A plain function rather than a closure in the handler, so that a test can build one from a view without a
/// client, a URI and a `params` value. The session does not appear because it is not needed: the facts have already
/// been read into the outline ([`Session::outline`]), and everything below is a conversion.
pub fn symbols(view: &FileView, outline: &[OutlineSymbol]) -> Vec<DocumentSymbol> {
    outline
        .iter()
        .map(|symbol| document_symbol(view, symbol))
        .collect()
}

fn document_symbol(view: &FileView, symbol: &OutlineSymbol) -> DocumentSymbol {
    let fact = &symbol.fact;
    let whole = range_in_file(view, declaration_range(view, fact));
    let name = range_in_file(view, fact.name_range);

    // **The protocol's one invariant about this pair**: `selection_range` is inside `range`. Asking for the name's
    // range twice when it is not keeps the entry (its kind and its nesting are still right) instead of handing a
    // client a symbol it may reject.
    let (range, selection_range) = match (whole, name) {
        (Some(whole), Some(name)) if name.start >= whole.start && name.end <= whole.end => {
            (whole, name)
        }
        (_, Some(name)) => (name, name),
        (Some(whole), None) => (whole, whole),
        (None, None) => (Range::default(), Range::default()),
    };

    // The schema this crate builds against still **requires** `deprecated`, and `tags` is the modern spelling of the
    // same idea — so the field is written once, empty, and the deprecation is allowed here rather than left as a
    // warning: leaving it out does not compile, and a server that has nothing to deprecate sets neither.
    #[allow(deprecated)]
    DocumentSymbol {
        name: fact.name.clone(),
        // What the label does not say: the scope a member was written in (`ns::Widget`).
        detail: fact.scope.clone(),
        kind: symbol_kind(fact.kind),
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: Some(
            symbol
                .children
                .iter()
                .map(|child| document_symbol(view, child))
                .collect(),
        ),
    }
}

/// **The range a client folds and draws a breadcrumb with** — the whole declaration, not just its declarator.
///
/// A binding records its **declarator**, which is right for the many consumers that ask "what does this name
/// declare" and wrong for this one: the protocol documents `DocumentSymbol.range` as the range *"enclosing this
/// symbol"*, and measured on
///
/// ```text
/// struct Widget {
///     int size;
///     int scaled(int factor) const;
/// };
/// ```
///
/// `size`'s declarator is `"size"` and `scaled`'s is `"int scaled(int factor) const"` — so the first folded nothing
/// at all while the second looked right.
///
/// **Why the widening is here and not in the scope walker.** Five test files across the analysis depend on a
/// binding's range being its declarator: its text is where the operators of a deduced type are read from, and the
/// position it starts at is how a declaration finds the template parameters in scope. Widening it there broke `auto`
/// deduction in two suites — `const auto& r = base` came back as `const int` — which is the measurement that moved
/// this here. Those consumers want the declarator; this one wants the declaration; the difference is a presentation
/// choice and belongs at the layer that presents.
///
/// The **innermost** `Declaration` holding the name is the one used, not the first: a declaration can hold another,
/// and `struct Widget { int size; };` is one `Declaration` whose body holds a second for `size`. An outer one would
/// answer a member's range with the whole class.
///
/// The enclosing `Declaration` is used only when it holds **exactly one** declarator. With two, `int a, b;`, the
/// whole declaration is the wrong answer for either name and the fact's declarator is the best available one.
fn declaration_range(view: &FileView, fact: &cpp_code_analysis::DeclFact) -> cpp_parser::SourceRange {
    let mut node = view.root.clone();
    let at = fact.name_range.start_offset;
    let mut innermost: Option<cpp_parser::SourceRange> = None;

    while let Some(child) = node.children().find(|child| {
        let range = child.text_range();
        usize::from(range.start()) <= at && at < usize::from(range.end())
    }) {        if cpp_parser::CppSyntaxKind::from(child.kind()) == cpp_parser::CppSyntaxKind::Declaration {
            let declarators = child
                .children()
                .filter(|inner| {
                    matches!(
                        cpp_parser::CppSyntaxKind::from(inner.kind()),
                        cpp_parser::CppSyntaxKind::InitDeclarator | cpp_parser::CppSyntaxKind::Declarator
                    )
                })
                .count();
            if declarators == 1 {
                innermost = Some(cpp_parser::source_range(child.text_range()));
            }
        }
        node = child;
    }

    // Wider than the fact and still holding the name — otherwise the declarator is the honest answer.
    match innermost {
        Some(range)
            if range.start_offset <= fact.name_range.start_offset
                && range.end_offset() >= fact.name_range.end_offset() =>
        {
            range
        }
        _ => fact.range,
    }
}

/// A source range as a place in the file, through **the file's own** line index.
///
/// # Why this is not [`crate::util::position_at_offset`], which every other handler uses
///
/// Because an outline's offsets are not in the reading. `position_at_offset` takes a [`ReadingOffset`] and maps it
/// through `FileView::file_offset_of` — the rendering-to-file seam, for a span the parser produced. An outline is
/// built from the file's **own** declarations ([`cpp_code_analysis::FileSummary::outline`], or the buffer's own
/// parse), so its offsets are already file offsets, and putting them through that seam measures them against the
/// wrong document. Measured on the handshake fixture, where the rendering of `main.cpp` collapses two `#include`
/// lines and a false `#if OFF` branch: `Dead`'s range starts at file offset 45, `file_offset_of(45)` answers
/// **100**, and the symbol's range came out empty — a client draws a breadcrumb entry it cannot navigate to.
///
/// [`FileView::written_lines`] paired with [`FileView::written_text`] is the file's own index and text, and they are
/// the same pair `position_at_offset` uses once it has crossed the seam. When the view is of the file itself —
/// `written` is `None` — this is that same call.
fn range_in_file(view: &FileView, range: cpp_parser::SourceRange) -> Option<Range> {
    let text = view.written_text().unwrap_or(&view.source);
    let lines = view.written_lines();
    Some(Range::new(
        file_position(text, lines, range.start_offset)?,
        file_position(text, lines, range.end_offset())?,
    ))
}

/// One byte offset in `text`, as a client's position — **the file's** coordinates, through the file's own index.
///
/// These three lines are `crate::util::position_in`'s body. It is private because every other handler reaches it
/// through a seam that first takes a *reading* offset, and this is the one caller whose offsets are already the
/// file's — see [`range_in_file`].
fn file_position(text: &str, lines: &cpp_parser::LineIndex, offset: usize) -> Option<Position> {
    let (line, column) = lines.position_of(offset, text)?;
    let body = crate::util::line_body(text, line)?;
    Some(Position::new(
        line as u32,
        crate::util::utf16_column(body, column),
    ))
}

pub struct DocumentSymbolCapabilities;

impl RegisterCapabilities for DocumentSymbolCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.document_symbol_provider = Some(OneOf::Left(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The two ranges, on the shapes that used to get them wrong.**
    ///
    /// Reported from an editor: *"our documentSymbol positions are almost always wrong"*. Two defects, and every
    /// symbol in a braced declaration had one or both:
    ///
    /// ```text
    /// namespace ns { … }   the range ran to the END OF THE FILE, because the trailing newline is a child of the
    ///                      body rather than of the declaration and the trim only looked at direct children
    /// int size;            the range was `"size"` — the declarator — with neither `int` nor `;`
    /// void f();            the range was `"f()"`, same reason
    /// ```
    ///
    /// A client folds, highlights and draws a breadcrumb from `range`, and the protocol documents it as *"the range
    /// enclosing this symbol"*. So a range ending at the file's last line swallows everything after the declaration,
    /// and a range that is only the name folds nothing at all.
    ///
    /// The expectations are written as the **text they select**, sliced out of the fixture, which is the only
    /// spelling a reader can check without counting columns — and the one that shows both defects at a glance.
    #[test]
    fn a_symbols_range_is_its_whole_declaration() {
        // CRLF and tabs on purpose: a real file written on Windows, which is where the newline defect showed.
        let text = "namespace ns {\r\n\tstruct Widget {\r\n\t\tint size;\r\n\t\tvoid f();\r\n\t};\r\n}\r\n";
        let (session, path) = a_session(text);
        let view = session
            .view_of_the_file(&path)
            .expect("a view of the file's own text");
        let outline = session.outline(&view);

        let selected = |range: Range| -> String {
            let lines: Vec<&str> = text.split("\r\n").collect();
            let from = lines[range.start.line as usize];
            let to = lines[range.end.line as usize];
            if range.start.line == range.end.line {
                from[range.start.character as usize..range.end.character as usize].to_string()
            } else {
                format!(
                    "{}…{}",
                    &from[range.start.character as usize..],
                    &to[..range.end.character as usize]
                )
            }
        };

        let symbols = symbols(&view, &outline);
        let ns = &symbols[0];
        assert_eq!(ns.name, "ns");
        assert_eq!(
            selected(ns.range),
            "namespace ns {…}",
            "a namespace ends at its own `}}`, not at the end of the file: {:?}",
            ns.range
        );
        assert_eq!(
            selected(ns.selection_range),
            "ns",
            "and the name is what a client selects"
        );

        // **The members**, which are the shapes whose range was only the name. The expected text carries the
        // fixture's own tab, because that is what the range really covers.
        let members = ns.children.as_ref().expect("the class is inside the namespace");
        let widget = &members[0];
        assert_eq!(widget.name, "Widget");
        assert_eq!(
            selected(widget.range),
            "struct Widget {…\t}",
            "a class ends at its `}};`, indentation and all: {:?}",
            widget.range
        );

        let body = widget.children.as_ref().expect("its members");
        assert_eq!(body[0].name, "size");
        assert_eq!(
            selected(body[0].range),
            "int size;",
            "a variable's range is the declaration, type and `;` included: {:?}",
            body[0].range
        );
        assert_eq!(
            selected(body[0].selection_range),
            "size",
            "and its name is inside it, which the protocol requires"
        );

        assert_eq!(body[1].name, "f");
        assert_eq!(
            selected(body[1].range),
            "void f();",
            "and so is a member function's: {:?}",
            body[1].range
        );

        // **The pair's invariant**, on every symbol however deep: `selection_range` inside `range`. A client that
        // rejected an outline because one entry disagreed would show nothing at all.
        fn check(symbols: &[DocumentSymbol]) {
            for symbol in symbols {
                let inside = symbol.selection_range.start >= symbol.range.start
                    && symbol.selection_range.end <= symbol.range.end;
                assert!(
                    inside,
                    "`{}` selects {:?} which is not inside {:?}",
                    symbol.name, symbol.selection_range, symbol.range
                );
                if let Some(children) = &symbol.children {
                    check(children);
                }
            }
        }
        check(&symbols);

        // **A member's range is its own declaration, not the class's.** The walk that finds the enclosing
        // `Declaration` must take the innermost one; taking the first gave `size` the whole `struct Widget { … };`.
        assert_ne!(
            body[0].range, widget.range,
            "a member is not the class it is written in"
        );
        assert!(
            body[0].range.start >= widget.range.start && body[0].range.end <= widget.range.end,
            "and it is written inside it"
        );
    }

    /// **Two declarators keep their own ranges**, which the widening must not swallow.
    ///
    /// `int a = 1, b = 2;` declares two names under one type, so the type belongs to neither alone: attributing
    /// the whole declaration to both would make folding or highlighting one edit the other.
    #[test]
    fn a_shared_declaration_is_not_attributed_to_one_name() {
        let text = "int a = 1, b = 2;\n";
        let (session, path) = a_session(text);
        let view = session
            .view_of_the_file(&path)
            .expect("a view of the file's own text");
        let outline = session.outline(&view);
        let symbols = symbols(&view, &outline);

        let mut found: Vec<(String, Range)> = Vec::new();
        collect(&symbols, &mut found);
        assert_eq!(found.len(), 2, "two names: {found:?}");

        let line = "int a = 1, b = 2;";
        for (name, range) in &found {
            let selected = &line[range.start.character as usize..range.end.character as usize];
            assert!(
                !selected.starts_with("int"),
                "`{name}` must not claim the shared type: {selected:?}"
            );
        }
    }

    fn collect(symbols: &[DocumentSymbol], into: &mut Vec<(String, Range)>) {
        for symbol in symbols {
            into.push((symbol.name.clone(), symbol.range));
            if let Some(children) = &symbol.children {
                collect(children, into);
            }
        }
    }

    /// A workspace holding one file, so the outline comes from the file's own declarations.
    fn a_session(
        text: &str,
    ) -> (
        cpp_code_analysis::Session<cpp_code_analysis::MemoryFiles>,
        std::path::PathBuf,
    ) {
        use cpp_code_analysis::{
            CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
        };
        let files = MemoryFiles::new().with_file("/p/a.cpp", text);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.load("/p/a.cpp");
        (session, std::path::PathBuf::from("/p/a.cpp"))
    }
}
