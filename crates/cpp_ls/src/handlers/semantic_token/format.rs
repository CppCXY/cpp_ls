//! **The format strings** — a small language inside a literal, drawn where the client drew one colour.
//!
//! ```cpp
//! std::println("值 = {:.3f}，宽度 {:{}}", x, w);     `{:…}` a field, `f` its conversion, `{}` an argument
//! printf("%-8.2f|%*d|%%", y, w);                  `%-8.2` a specifier, `*` an argument, `%%` an escape
//! scanf("%3d %[a-z] %n", &n, name, &used);        and the scanset
//! ```
//!
//! # What this layer is, and what it is not
//!
//! Nothing here decides **which** literal is a format string or **what** its parts are: that is
//! [`cpp_code_analysis::sema::format`], a pure walk with no LSP in it, and this file is the three lines that turn
//! its answer into claims. The split is the one the module's other passes already make — [`super::push_names`] gets a
//! list of names and paints them — and it is what lets the same walk serve a check later ("this format string wants
//! three arguments and the call passes two") without a second implementation of either grammar.
//!
//! # The fallbacks, and why a reader sees colour without configuring anything
//!
//! The four types are **custom**: the protocol's vocabulary has no word for "the specification of a format field",
//! and a client that was not told about a type cannot draw it ([`Builder::push_range`] refuses the claim). So each
//! piece has a **standard** type behind it:
//!
//! ```text
//!   a field      formatSpecifier   →  regexp      the pattern language of a string — which is what it is
//!   an argument  formatArgument    →  parameter   a reference to something the call passes
//!   a conversion formatConversion  →  keyword     one letter that changes what a value means
//!   an escape    formatEscape      →  (nothing)   the client's own string colour already draws it
//! ```
//!
//! The fallbacks are *not* a worse approximation of the same picture; they are the three standard types whose theme
//! rules are closest to the four custom ones, so an editor whose theme has never heard of `formatSpecifier` still
//! shows the field, the argument and the conversion apart from the text around them. A client that *does* advertise
//! the custom types — or a theme that colours them — gets them instead, because [`Builder::push_range`] is asked for
//! the custom one first.
//!
//! An escape has **no fallback on purpose**: `string` is the colour it already has, and sending a token to repaint a
//! literal as a literal is one token of answer for no difference on screen.

use cpp_code_analysis::HighlightSection;
use cpp_code_analysis::sema::format::{FormatConfig, FormatLiteral, PieceKind};

use super::{Builder, Modifier, TypeKind};

/// **What a piece is drawn as**, custom type first and the standard one it falls back to.
///
/// `None` for the fallback means "there is nothing to fall back to" — the escape, whose colour is the literal's own.
fn kinds_of(piece: PieceKind) -> (TypeKind, Option<TypeKind>) {
    match piece {
        PieceKind::Specifier | PieceKind::DynamicWidth => {
            (TypeKind::FormatSpecifier, Some(TypeKind::Regexp))
        }
        PieceKind::Argument => (TypeKind::FormatArgument, Some(TypeKind::Parameter)),
        PieceKind::Conversion => (TypeKind::FormatConversion, Some(TypeKind::Keyword)),
        PieceKind::Escape => (TypeKind::FormatEscape, None),
    }
}

/// **Paint every piece of every format string of a file.**
///
/// The offsets are the file's, because the walk that produced them read the file's own text (see
/// [`cpp_code_analysis::Session::format_literals`]) — there is no mapping to do here, which is the reason that
/// query insists on a written view rather than a rendering.
pub fn draw(builder: &mut Builder<'_>, literals: &[FormatLiteral]) {
    for literal in literals {
        for piece in &literal.pieces {
            let start = literal.range.start_offset + piece.start;
            let end = literal.range.start_offset + piece.end;
            let (drawn, fallback) = kinds_of(piece.kind);

            // **First the custom type, then the standard one.** `push_range` answers `false` when the client did not
            // advertise the type — a number it could not map — and only then is the fallback asked for, which is
            // what makes the four types an addition to an editor's colouring rather than a condition of it.
            if builder.push_range(start, end, drawn, Modifier::none()) {
                continue;
            }
            if let Some(fallback) = fallback {
                builder.push_range(start, end, fallback, Modifier::none());
            }
        }
    }
}

/// **The project's `[highlight]` section, as the analysis's own configuration.**
///
/// The defaults are decided here rather than in the section so that `.cppls.toml` stays *data* — see the note on
/// [`cpp_code_analysis::ProjectConfig`] — and `None` therefore means "on": a project that has written no
/// configuration gets the colouring, because whether a string is a format string is not a matter of taste.
pub fn config_from(section: &HighlightSection) -> FormatConfig {
    FormatConfig {
        std_format: section.format_strings.unwrap_or(true),
        printf: section.printf.unwrap_or(true),
        scanf: section.scanf.unwrap_or(true),
        extra: section.extra.clone(),
        extra_format: section.extra_format.clone(),
    }
}
