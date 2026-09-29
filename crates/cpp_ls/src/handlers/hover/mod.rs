//! # `textDocument/hover` — what the name under the cursor is
//!
//! Three questions, in the order the preprocessor asks them:
//!
//! ```text
//! 1. is this a macro?        a name that a #define settles IS that macro at this point in the file
//! 2. otherwise, what does it name?  the index's declaration, rendered from what the fact records
//! 3. and if it names nothing, what is it?  the type of the expression the cursor is in — see below
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
//! type or return type, the documentation comment above it, and the caveats the fact itself records: a declaration
//! inside a conditional block, or one the parser recovered around. That is the point of a fact-only hover:
//! everything shown is something the analysis *knows*, and nothing is a guess dressed as a signature.
//!
//! The third question is the one a cursor on `this`, `*p` or `f(1)` needs, and it is asked **only** when the
//! second one names nothing: a name is what a reader is asking about, and the type of the expression around it is
//! a fallback rather than a second answer competing with the first.
//!
//! No `range` is set on the answer: an LSP hover may carry one, and computing it would mean deciding where the
//! name under the cursor starts and ends — a second implementation of "what is a name" beside the one the analysis
//! already has (`sema::resolve::name_at`), which is free to disagree with it. The client positions the popup
//! itself when the answer carries none, which is what happens today.

use cpp_code_analysis::{
    DeclFact, DeclKind, DiskFiles, FactGuard, FileView, Known, ProjectDefinition, ProjectMacro,
    Session, UnknownReason,
};
use cpp_parser::CppDocComment;
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

/// How long an expression quoted in a hover may be before it is cut.
///
/// An expression is quoted *inside a line of markdown*, so it is collapsed to one line first; the cut is what
/// keeps a long call from filling the popup with code the reader is already looking at.
const MAX_INLINE_CHARS: usize = 80;

pub async fn on_hover(
    context: ServerContextSnapshot,
    params: HoverParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Hover> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    // Read in first, under the write lock, so the query itself can be a read (`AnalysisState::prepare` records why).
    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }

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
    // A project can turn the popup off (`hover.enable = false` in `.cppls.toml`), which is a preference about the
    // editor rather than about the analysis — so it is checked here, at the edge, and the queries below do not know
    // it exists.
    if session.project_config().config.hover.enable == Some(false) {
        return None;
    }

    // **A header name before a declaration**, for the reason the jump asks it first: `#include <vector>` declares
    // nothing, so every question below answers "nothing to say" for a name whose answer is a file on disk. The
    // popup is the resolved path, which is the one thing a reader cannot see from the line they wrote — the header
    // is found by a search, and *which* of the four thousand files on this machine's search path it found is the
    // answer to the question the line asks.
    if let Known::Yes(header) = session.header_at(view, offset) {
        return Some(markdown(header_markdown(&header)));
    }

    // The macro question first: it is about the text, and it is answered without the scope tree.
    if let Known::Yes(found) = session.macro_definition(view, offset) {
        return Some(markdown(macro_markdown(session, view, &found)));
    }

    match session.definition(view, offset) {
        Known::Yes(found) => Some(markdown(declaration_markdown(session, view, &found))),
        // **A name with several declarations is a popup with several signatures**, not none. `std::format` is four
        // overloads, so the name question has always answered `Ambiguous` — and `Ambiguous` used to fall through to
        // the *expression* question, which has nothing to say about a name that is not in an expression: hovering
        // `std::format` showed an empty popup on the most ordinary call in modern C++. The list is what the reader
        // is asking for ("what can I call here"), and the index can now show each one's parameters.
        Known::Unknown(UnknownReason::Ambiguous(_)) => match session.definitions(view, offset) {
            Known::Yes(found) if found.found.len() > 1 => {
                Some(markdown(overloads_markdown(session, &found)))
            }
            _ => expression_markdown(session, view, offset).map(markdown),
        },
        // `No` and `Unknown` both leave the name question unanswered: the first says the analysis looked and there
        // is no such declaration, the second that it cannot say yet (the file's includes are still being read).
        // Either way the cursor may still be *in* something the analysis can type — `this`, `*p`, `f(1)` — which is
        // a different question about the same position, and the one a reader on a keyword is asking.
        Known::No | Known::Unknown(_) => expression_markdown(session, view, offset).map(markdown),
    }
}

/// **Every declaration of one name**, as one popup — what a hover says about an overloaded function.
///
/// # Why a signature per declaration rather than each declaration's text
///
/// Because the question is a choice: a reader hovering `std::format` wants to know what they can call, and a code
/// block holding four whole definitions (bodies and all) answers a different question at four times the size. The
/// line is built from the fact — the return type and the parameter list, both of which the index records — so this
/// costs no parsing, whoever's header it is.
///
/// A declaration whose parameters were never read (a fact written before the field existed) still gets a line, with
/// the `(…)` the old detail line used: less than the truth, and not more.
fn overloads_markdown(
    session: &Session<DiskFiles>,
    found: &cpp_code_analysis::ProjectDefinitions,
) -> String {    let mut lines = String::new();
    for declaration in &found.found {
        lines.push_str(&signature_line(&declaration.fact));
        lines.push('\n');
    }

    let first = &found.found[0];
    let qualified = first.fact.qualified_name();
    let mut out = code_block(lines.trim_end());

    out.push_str(&format!(
        "\n`{qualified}` — {} declarations. Which one a call means is decided by the arguments, so they are all \
         here rather than one of them being guessed at.\n",
        found.found.len()
    ));

    if found.conditional > 0 {
        out.push_str(&format!(
            "\n{} more declaration(s) of this name are reachable only through a conditional `#include`, so whether \
             they are in scope depends on the compilation.\n",
            found.conditional
        ));
    }

    let files: std::collections::BTreeSet<&std::path::Path> = found
        .found
        .iter()
        .map(|declaration| declaration.file.as_path())
        .collect();
    out.push_str(&format!(
        "\nDeclared in {}",
        where_clause(session, first.file.as_path(), first.fact.name_range.start_offset)
    ));
    if files.len() > 1 {
        out.push_str(&format!(" and {} other file(s)", files.len() - 1));
    }

    out
}

/// One declaration, as the line a hover shows: `string format(const _Fmt, _Args...)`.
fn signature_line(fact: &DeclFact) -> String {
    match (&fact.returns, &fact.parameter_list) {
        (Some(returns), Some(parameters)) => format!("{returns} {}{parameters}", fact.name),
        (Some(returns), None) => format!("{returns} {}(…)", fact.name),
        (None, Some(parameters)) => format!("{}{parameters}", fact.name),
        (None, None) => format!("{} {}", kind_words(fact), fact.qualified_name()),
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

/// **A header, as the file it resolved to** — what a hover on `#include <vector>` says.
///
/// One line and a path, because that is the whole answer: the reader wrote a name, a search turned it into a file,
/// and the file is the thing they cannot see. The **delimiters are the form the directive used**, put back on the
/// name so that the line reads as the reader wrote it — the two spellings search differently, and seeing which one
/// was applied is how a reader tells "found beside the file" from "found on the search path".
fn header_markdown(header: &cpp_code_analysis::HeaderTarget) -> String {
    let (open, close) = match header.form {
        cpp_code_analysis::IncludeForm::Quote => ('"', '"'),
        // A macro include never reaches here: its target is a macro, and the question about it is the macro
        // question, which is asked above. Both remaining forms are the angle one as far as a reader is concerned.
        cpp_code_analysis::IncludeForm::Angle | cpp_code_analysis::IncludeForm::Macro => ('<', '>'),
    };

    format!(
        "```cpp\n#include {open}{}{close}\n```\n\n`{}`\n",
        header.spelling,
        header.resolved.display()
    )
}

/// A macro, as the definition writes it.
fn macro_markdown(session: &Session<DiskFiles>, view: &FileView, found: &ProjectMacro) -> String {
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

    if let Some(documentation) =
        documentation_markdown(session, view, &found.file, fact.range.start_offset)
    {
        out.push_str(&format!("\n{documentation}\n"));
    }

    out
}

/// A declaration, rendered from the fact the index holds.
fn declaration_markdown(
    session: &Session<DiskFiles>,
    view: &FileView,
    found: &ProjectDefinition,
) -> String {
    let fact = &found.fact;
    let mut out = String::new();

    let declaration = declaration_text(session, &found.file, fact);
    out.push_str(&code_block(&declaration));

    out.push_str(&format!(
        "\n`{}` — {}\n",
        fact.qualified_name(),
        kind_words(fact)
    ));

    // The comment the file writes above the declaration, in the place clangd and the C++ extension put it: after
    // the signature, before where it was declared from.
    if let Some(documentation) =
        documentation_markdown(session, view, &found.file, fact.range.start_offset)
    {
        out.push_str(&format!("\n{documentation}\n"));
    }

    out.push_str(&format!(
        "\nDeclared in {}",
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

/// The comment a declaration is documented by, as markdown — `None` when the file writes none.
///
/// # Why only a comment written *as* documentation
///
/// The parser answers "which comment documents this declaration" for any comment above it, and it also answers
/// whether that comment is written as documentation ([`cpp_parser::CppDocComment::is_documentation`]): `///`, `//!`, `/**`,
/// `/*!`. This asks the second question as well, because of what a plain `//` comment above a declaration usually
/// is in a real codebase — `// TODO`, `// NOLINT`, a banner of slashes — and a popup that showed every one of them
/// would dress a note to the reader as an interface description. A codebase that documents with plain `//` gets no
/// hover text: a deliberate miss rather than a silent wrong answer.
///
/// # Why the comment is shown as it was written
///
/// Doxygen's `@brief` and `@param` are left as the file spells them rather than turned into a markdown list. That
/// is the same choice clangd and the C++ extension make, and the alternative is a *rendering policy* the file did
/// not write: the structure is available (`CppDocComment::get_commands`) for the feature that needs it, and
/// signature help — which has to line a parameter up with its documentation — is where that will be.
fn documentation_markdown(
    session: &Session<DiskFiles>,
    view: &FileView,
    file: &std::path::Path,
    offset: usize,
) -> Option<String> {
    documentation_text(&session.documentation(view, file, offset)?)
}

/// The comment as markdown — `None` when it is not written as documentation, or says nothing.
///
/// Shared with signature help, which shows the same comment beside the parameter being typed: one rendering, so
/// that two popups about one declaration cannot disagree about what it says.
pub(crate) fn documentation_text(comment: &CppDocComment) -> Option<String> {
    if !comment.is_documentation() {
        return None;
    }

    // Delimiters, line markers and the blank lines around them are the parser's reading; an empty comment — `///`
    // with nothing after it — has nothing to show.
    let text = comment.get_comment_text();
    let text = text.trim();

    (!text.is_empty()).then(|| text.to_string())
}

/// What the expression at the cursor is, when no declaration is at it.
///
/// The fallback for `this`, `*p`, `f(1)`, `w.size`: positions where there is no *name* to look up and the analysis
/// still knows the type. `None` when it does not — which is the ordinary answer, because most positions are in no
/// expression at all and the ones that are may be a call through a function pointer or a template parameter that
/// nothing here instantiates.
fn expression_markdown(
    session: &Session<DiskFiles>,
    view: &FileView,
    offset: usize,
) -> Option<String> {
    let Known::Yes(found) = session.type_at(view, offset) else {
        return None;
    };

    // `this` is the one expression whose type is not what the word means: the analysis answers with the enclosing
    // *class*, and calling that the type of `this` would be wrong about a pointer.
    if found.expression == "this" {
        return Some(format!(
            "`this` — a pointer to the enclosing class `{}`",
            found.type_of
        ));
    }

    Some(format!(
        "`{}` — has type `{}`",
        inline(&found.expression),
        found.type_of
    ))
}

/// An expression's text for a line of markdown: one line, and short enough to read.
fn inline(text: &str) -> String {
    let mut collapsed = String::new();
    let mut after_space = false;

    for character in text.chars() {
        if character.is_whitespace() {
            after_space = true;
            continue;
        }

        if after_space && !collapsed.is_empty() {
            collapsed.push(' ');
        }
        after_space = false;
        collapsed.push(character);
    }

    if collapsed.chars().count() <= MAX_INLINE_CHARS {
        return collapsed;
    }

    let cut: String = collapsed.chars().take(MAX_INLINE_CHARS - 1).collect();
    format!("{cut}…")
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
    // From the VFS, which holds the file and its lines: a hover that names a declaration in a header nobody has
    // opened reads that header **once per session**, not once per hover.
    match session.files().held(file) {
        Some(declaring) => {
            slice_lines(&declaring.text, fact.range.start_offset, fact.range.end_offset())
        }
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
    let Some(defining) = session.files().held(file) else {
        return RenderedText {
            text: format!("<the macro is defined in an unreadable file: {}>", file.display()),
        };
    };
    let text: &str = &defining.text;

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
        .files()
        .held(file)
        .and_then(|declaring| declaring.position_at(offset))
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

    /// An expression quoted in a line of markdown: one line, and bounded.
    ///
    /// Both halves matter in a popup: a newline inside a single pair of backticks ends the markdown *span*, so the
    /// rest of the answer would be rendered as prose, and an unbounded quote would push the declaration itself out
    /// of the popup the reader opened.
    #[test]
    fn an_expression_is_collapsed_to_one_bounded_line() {
        assert_eq!(inline("scale(1,\n     2)"), "scale(1, 2)");
        assert_eq!(inline("  spaced   out  "), "spaced out");

        let long = "a".repeat(MAX_INLINE_CHARS + 20);
        let shown = inline(&long);
        assert_eq!(shown.chars().count(), MAX_INLINE_CHARS);
        assert!(shown.ends_with('…'), "the cut is visible: {shown}");

        // Exactly at the bound nothing is cut: an elision of nothing would be a lie.
        let exact = "b".repeat(MAX_INLINE_CHARS);
        assert_eq!(inline(&exact), exact);
    }

    #[test]
    fn nothing_is_shown_for_an_empty_declaration() {
        assert_eq!(code_block(""), "");
    }
}



