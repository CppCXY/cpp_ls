//! **A format string's parts, explained** — the popup on `%zu` or on `{:.3f}`.
//!
//! ```text
//! printf("%-8.2f", x)     hovering `%-8.2` says: left-aligned, width 8, precision 2
//!                         hovering `f`     says: fixed-point decimal — a `double` written as `-.--`
//! std::format("{:>10}", s)  hovering `{:>10}` says: right-aligned, width 10
//! scanf("%[a-z]", name)   hovering `[a-z]` says: one or more characters from `a-z`
//! ```
//!
//! # Why this is worth a popup at all
//!
//! A format string is the one part of C++ where the language is **a string**, and the vocabulary is not the
//! language's: `%zu` is a `size_t` in decimal, `{:#x}` is hex with a prefix, `%n` writes instead of printing. Every
//! reader looks these up, and the one place they are already looking is the line of code. So the popup is a
//! reference page for the four characters under the cursor.
//!
//! # What produces the words
//!
//! [`cpp_code_analysis::sema::format::describe`] — the **same module that decides which bytes are a piece**, so the
//! explanation of `%-8.2` cannot disagree with the colouring of `%-8.2`. This file decides only what a popup looks
//! like: the title, the list, and which grammar the string was read as.
//!
//! # What it deliberately does not do
//!
//! It does not say anything about the *argument* a conversion takes beyond its type's name, and it does not check
//! the call: a popup that said "this argument is a `std::string` and `%d` wants an `int`" would be a diagnostic
//! wearing a tooltip's clothes, and it would be wrong wherever the analysis cannot see a template's instantiation.
//! The check is Phase 3's other half and it is not here.

use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::sema::format::{self, FormatKind, FormatLiteral, PieceKind};
use cpp_code_analysis::{DiskFiles, FileView, Session};
use lsp_types::Hover;

use super::markdown;

/// **The popup for the format piece at `in_the_file`**, or `None` when the cursor is not inside one.
///
/// `view` must be a view of the file's **own text** — the offsets this layer paints in are the file's, and a
/// rendering's are its own (see [`Session::format_literals`], whose first act is to refuse one).
pub fn format_string_hover(
    session: &Session<DiskFiles>,
    view: &FileView,
    in_the_file: FileOffset,
) -> Option<Hover> {
    if view.written_text().is_some() {
        return None;
    }

    let config = crate::handlers::semantic_token::format::config_from(
        &session.project_config().config.highlight,
    );
    // **A project that switched the colouring off gets no popup either**: the two are one feature, and a popup
    // explaining bytes the editor is not drawing would be answering a question the reader did not ask.
    if config.is_off() {
        return None;
    }

    let literals = session.format_literals(view, &config);
    let source = view.source.as_ref();
    for literal in &literals {
        if !literal.range.contain(in_the_file.get()) {
            continue;
        }
        let text = source.get(literal.range.start_offset..literal.range.end_offset())?;
        let at = in_the_file.get() - literal.range.start_offset;
        let piece = literal
            .pieces
            .iter()
            .find(|piece| piece.start <= at && at < piece.end)?;
        return Some(markdown(the_popup(text, literal, piece)?));
    }
    None
}

/// The markdown of one piece: what it is, and what the grammar says about it.
///
/// The parts come from the analysis, the sentence around them from here — see the module documentation. A piece with
/// nothing to say (a broken format string) answers `None` rather than a title with no body, which is what keeps a
/// popup on `"%y"` from claiming to know something.
fn the_popup(literal_text: &str, literal: &FormatLiteral, piece: &format::Piece) -> Option<String> {
    let shown = literal_text.get(piece.start..piece.end)?;
    let parts = format::describe(literal_text, literal.kind, piece);
    if parts.is_empty() {
        return None;
    }

    let mut text = format!(
        "**`{shown}`** — {}\n\n",
        what_it_is(piece.kind, literal.kind)
    );
    for (label, value) in parts {
        text.push_str(&format!("- **{label}**: {value}\n"));
    }
    Some(text)
}

/// The name of the thing, which is what the title says — and it is the *grammar's* name, because `%` means a
/// conversion in both C grammars and a `{` means a field in only one of them.
fn what_it_is(kind: PieceKind, grammar: FormatKind) -> &'static str {
    match (kind, grammar) {
        (PieceKind::Conversion, FormatKind::Std) => "a presentation type",
        (PieceKind::Conversion, FormatKind::Printf) => "a `printf` conversion",
        (PieceKind::Conversion, FormatKind::Scanf) => "a `scanf` conversion",
        (PieceKind::Specifier | PieceKind::DynamicWidth, FormatKind::Std) => {
            "a field's specification"
        }
        (PieceKind::Specifier | PieceKind::DynamicWidth, FormatKind::Printf) => {
            "a `printf` conversion's flags, width and precision"
        }
        (PieceKind::Specifier | PieceKind::DynamicWidth, FormatKind::Scanf) => {
            "a `scanf` conversion's width"
        }
        (PieceKind::Argument, _) => "an argument reference",
        (PieceKind::Escape, FormatKind::Std) => "an escape — a literal brace",
        (PieceKind::Escape, _) => "an escape — a literal `%`",
    }
}
