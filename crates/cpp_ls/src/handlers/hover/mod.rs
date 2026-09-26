//! # `textDocument/hover` — what the name under the cursor is
//!
//! Two questions, in the order the preprocessor asks them:
//!
//! ```text
//! 1. is this a macro?        a name that a #define settles IS that macro at this point in the file
//! 2. otherwise, what does it name?  the index's declaration, rendered from what the fact records
//! ```
//!
//! Macros first because that is what the text means: `#define WIDGET_MAX 8` makes `WIDGET_MAX` eight, and a hover
//! that instead described some declaration of the same spelling would describe a name the compiler never saw. The
//! analysis answers that question the same way — `macro_across_files` walks the `#define`/`#undef` history before
//! the cursor's offset, and `#undef` counts, so a name that *was* a macro answers "not a macro here" rather than
//! pointing at the definition it no longer has.
//!
//! # What is shown, and what is deliberately not
//!
//! The declaration **as the file writes it** (the fact carries the declaration's range, and the text is read
//! through the session — the buffer for an open file), the qualified name, what kind of thing it is, its written
//! type or return type, and the caveats the fact itself records: a declaration inside a conditional block, or one
//! the parser recovered around. That last part is the point of a fact-only hover: everything shown is something the
//! analysis *knows*, and nothing is a guess dressed as a signature.
//!
//! No `range` is set on the answer: an LSP hover may carry one, and computing it would mean deciding where the
//! name under the cursor starts and ends — a second implementation of "what is a name" beside the one the analysis
//! already has (`sema::resolve::name_at`), which is free to disagree with it. The client positions the popup
//! itself when the answer carries none, which is what happens today.

use cpp_code_analysis::{
    DeclFact, DeclKind, DiskFiles, FactGuard, FileView, Known, ProjectDefinition, ProjectMacro,
    Session,
};
use lsp_types::{
    ClientCapabilities, Hover, HoverContents, HoverParams, HoverProviderCapability, MarkupContent,
    MarkupKind, ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{offset_at_position, uri_to_file_path};

/// How much of a declaration's text is shown before it is elided.
///
/// A hover is a popup, and a declaration can be a whole class body or a 40-line function. The cut is at a line
/// boundary so that what is shown is still code, and the count says how much was left out — an elision a reader
/// can see, rather than a `…` that might be the file's own text.
const MAX_HOVER_LINES: usize = 12;

pub async fn on_hover(
    context: ServerContextSnapshot,
    params: HoverParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Hover> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view(&path)?;
        let offset = offset_at_position(&view, position)?;

        hover(session, &view, offset)
    })
    .await
}

/// What the name at `offset` is — `None` when it does not name anything the analysis can answer about.
///
/// `None` is the answer for punctuation, for whitespace, and for a name the analysis cannot place: a client shows
/// nothing, and "there is nothing to say here" is true, where an empty popup would not be.
pub fn hover(session: &Session<DiskFiles>, view: &FileView, offset: usize) -> Option<Hover> {
    // The macro question first: it is about the text, and it is answered without the scope tree.
    if let Known::Yes(found) = session.macro_definition(view, offset) {
        return Some(markdown(macro_markdown(session, &found)));
    }

    match session.definition(view, offset) {
        Known::Yes(found) => Some(markdown(declaration_markdown(session, &found))),
        // `No` and `Unknown` are both "nothing to show": the first says the analysis looked and there is no such
        // declaration, the second that it cannot say yet (the file's includes are still being read). A hover that
        // guessed at either would be showing the user something the analysis does not know.
        Known::No | Known::Unknown(_) => None,
    }
}

fn markdown(value: String) -> Hover {
    Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    }
}

/// A macro, as the definition writes it.
fn macro_markdown(session: &Session<DiskFiles>, found: &ProjectMacro) -> String {
    let fact = &found.fact;

    if !fact.kind.is_definition() {
        return format!(
            "`#undef {}`{}\n\n`{}` is not a macro at this point in the file.",
            fact.name,
            where_clause(session, &found.file, fact.range.start_offset),
            fact.name
        );
    }

    let mut out = String::new();
    let definition = definition_line(session, &found.file, fact.range.start_offset, fact.body_range);
    out.push_str(&code_block(&definition.text));

    out.push_str(&format!(
        "\n`{}` — a {}{}, defined in {}\n",
        fact.name,
        if fact.function_like {
            "function-like macro"
        } else {
            "macro"
        },
        match &fact.value {
            Some(value) => format!(" whose value is `{value}`"),
            None => String::new(),
        },
        where_clause(session, &found.file, fact.range.start_offset)
    ));

    out
}

/// A declaration, rendered from the fact the index holds.
fn declaration_markdown(session: &Session<DiskFiles>, found: &ProjectDefinition) -> String {
    let fact = &found.fact;
    let mut out = String::new();

    let declaration = declaration_text(session, &found.file, fact);
    out.push_str(&code_block(&declaration));

    out.push_str(&format!(
        "\n`{}` — {}\n\nDeclared in {}",
        fact.qualified_name(),
        kind_words(fact),
        where_clause(session, &found.file, fact.name_range.start_offset)
    ));

    if let FactGuard::Region(_) = fact.guard {
        out.push_str(
            "\n\nThis declaration is inside a conditional block, so whether it exists depends on the compilation \
             — the analysis is reporting what the file says, not what a compiler concluded.",
        );
    }

    if !fact.clean {
        out.push_str(
            "\n\nThe parser recovered from an error inside this declaration, so its text may not mean what it \
             looks like.",
        );
    }

    out
}

/// What kind of declaration this is, as a sentence fragment.
fn kind_words(fact: &DeclFact) -> String {
    let kind = match fact.kind {
        DeclKind::Type => "a type",
        DeclKind::Function => "a function",
        DeclKind::Variable => "a variable",
        DeclKind::Namespace => "a namespace",
        DeclKind::MacroLike => "a macro-like declaration",
        DeclKind::Other => "a declaration",
    };

    let mut words = kind.to_string();
    match (&fact.type_of, &fact.returns) {
        (Some(written), _) => words.push_str(&format!(" of type `{written}`")),
        (None, Some(written)) => words.push_str(&format!(" returning `{written}`")),
        (None, None) => {}
    }

    if !fact.bases.is_empty() {
        words.push_str(&format!(" derived from `{}`", fact.bases.join("`, `")));
    }

    if fact.local {
        words.push_str(", declared in a function body");
    }

    if let Some(scope) = &fact.scope {
        words.push_str(&format!(", in `{scope}`"));
    }

    words
}

/// The declaration's own text: the file's bytes between the fact's range.
fn declaration_text(
    session: &Session<DiskFiles>,
    file: &std::path::Path,
    fact: &DeclFact,
) -> String {
    match session.text(file) {
        Some(text) => slice_lines(&text, fact.range.start_offset, fact.range.end_offset()),
        // The file cannot be read (deleted since it was indexed, or a buffer that was closed unsaved): the fact is
        // still true, and what it says is shown without the text.
        None => format!("{} {}", kind_words(fact), fact.qualified_name()),
    }
}

/// The `#define` line a macro fact points at, body included.
fn definition_line(
    session: &Session<DiskFiles>,
    file: &std::path::Path,
    name_offset: usize,
    body_range: Option<cpp_parser::SourceRange>,
) -> RenderedText {
    let Some(text) = session.text(file) else {
        return RenderedText {
            text: format!("<the macro is defined in an unreadable file: {}>", file.display()),
        };
    };

    // The name's range is the name; the body's range (when there is one) is everything after the parameters, and
    // it is stored precisely so that a consumer does not have to search for the end of the directive.
    let end = body_range.map_or(name_offset, |body| body.end_offset());
    let line_end = text[name_offset..]
        .find('\n')
        .map_or(text.len(), |offset| name_offset + offset);
    let end = end.max(line_end.min(text.len()));

    // The directive's own `#define` keyword is before the name, and the fact does not record it: taking the line's
    // start is honest — it is the directive — where inventing the keyword would not be.
    let line_start = text[..name_offset].rfind('\n').map_or(0, |offset| offset + 1);

    RenderedText {
        text: text[line_start..end].trim_end().to_string(),
    }
}

/// A slice of a file, cut at a line boundary when it is long.
fn slice_lines(text: &str, start: usize, end: usize) -> String {
    if start >= text.len() {
        return String::new();
    }

    let end = end.min(text.len()).max(start);
    let slice = &text[start..end];

    let lines: Vec<&str> = slice.lines().collect();
    if lines.len() <= MAX_HOVER_LINES {
        return slice.trim_end().to_string();
    }

    format!(
        "{}\n// … {} more lines",
        lines[..MAX_HOVER_LINES].join("\n"),
        lines.len() - MAX_HOVER_LINES
    )
}

/// `file.h:12` — where something is, as a client can follow.
fn where_clause(session: &Session<DiskFiles>, file: &std::path::Path, offset: usize) -> String {
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.display().to_string());

    match session
        .text(file)
        .and_then(|text| cpp_parser::LineIndex::parse(&text).position_of(offset, &text))
    {
        Some((line, column)) => format!("`{name}:{}:{}`", line + 1, column + 1),
        None => format!("`{name}`"),
    }
}

fn code_block(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    // A fence longer than any run inside the declaration: a C++ file can hold a raw string containing three
    // backticks, and a hover that ended early would show the rest of the answer as prose.
    let fence = "`".repeat(3.max(longest_backtick_run(text) + 1));
    format!("{fence}cpp\n{text}\n{fence}\n")
}

/// The longest run of backticks in `text`, which is what a fence has to be longer than.
fn longest_backtick_run(text: &str) -> usize {
    text.split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0)
}

struct RenderedText {
    text: String,
}

pub struct HoverCapabilities;

impl RegisterCapabilities for HoverCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.hover_provider = Some(HoverProviderCapability::Simple(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_declaration_is_cut_at_a_line_and_says_how_much_is_missing() {
        let text = (0..20)
            .map(|line| format!("int field_{line};"))
            .collect::<Vec<_>>()
            .join("\n");
        let shown = slice_lines(&text, 0, text.len());

        assert_eq!(shown.lines().count(), MAX_HOVER_LINES + 1);
        assert!(shown.ends_with("// … 8 more lines"), "{shown}");
    }

    #[test]
    fn a_short_declaration_is_shown_whole() {
        let text = "struct Widget { int size; };\nint other;";
        assert_eq!(
            slice_lines(text, 0, "struct Widget { int size; };".len()),
            "struct Widget { int size; };"
        );
    }

    #[test]
    fn a_fence_cannot_be_closed_by_the_declaration_it_quotes() {
        // A declaration holding a raw string with three backticks in it: the block has to be fenced with more.
        let block = code_block("const char* s = R\"(```)\";");
        assert!(block.starts_with("````cpp\n"));
        assert!(block.ends_with("````\n"));
    }

    #[test]
    fn nothing_is_shown_for_an_empty_declaration() {
        assert_eq!(code_block(""), "");
    }
}
