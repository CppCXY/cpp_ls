//! **The format string** — which `{…}` and `%…` of a literal is what.
//!
//! ```cpp
//! std::println("值 = {:.3f}，宽度 {:{}}", x, w);      the field, its conversion, the dynamic width
//! printf("%-8.2f|%*d|%1$s|%%", y, w, s);            the conversion, two argument references, the escape
//! scanf("%3d %[a-z] %n", &n, name, &used);          the scanset, and `%n` which reads nothing
//! ```
//!
//! # Why this is a scanner and not a parse
//!
//! A format string is a **language inside a string**, written in the syntax of whatever function receives it —
//! `std::format`'s is Python's, `printf`'s is the C standard's, `scanf`'s is the same with no flags and a scanset.
//! The C++ parser has no business reading it: the literal is one token to the grammar, and turning it into fields
//! would be a second grammar threaded through the first one. So this module is a **pure function over a `&str`**
//! with no session, no index and no LSP in it, and everything it knows about a format string is in this file.
//!
//! Two things ask it: the **colour** (`crates/cpp_ls`'s `semantic_token` pass), and later the *check* — "this format
//! string wants three arguments and the call passes two" — which is the same walk over the same pieces. That is why
//! the pieces are a data structure rather than a list of ranges to paint.
//!
//! # The rule that shapes the output: ascending, non-overlapping, merged
//!
//! The protocol this feeds requires one token per range with **no two ranges overlapping**, and the builder that
//! encodes it keeps the first claim on an offset. A `{…}` containing a `{…}` (the dynamic width `{:{}}`) would
//! naturally produce a piece *inside* a piece, and a `%-8.2f` would naturally be "one specifier, whose last
//! character is a conversion" — both of which overlap. So the walk here emits **runs of one kind**, into a
//! container that has no children:
//!
//! ```text
//!   {:{}}       Specifier "{:"   DynamicWidth "{}"   Specifier "}"
//!   %-8.2f      Specifier "%-8.2"                    Conversion "f"
//!   {}          Specifier "{}"                       (the two merge: one kind, contiguous)
//! ```
//!
//! [§1 of `docs/format-strings.md`] describes the pieces as *containment* ("`formatSpecifier` is the whole field,
//! `formatConversion` is a sub-range of it"); this is that description written as the runs a client can draw. The
//! colour a reader sees is the same — the field's own syntax is one colour and its conversion another — and every
//! client can draw it, including the ones that do not say they can draw overlapping tokens.
//!
//! # What it must never do
//!
//! A malformed format string is *legal C++*: `printf("%")` compiles with a warning, `std::format("{")` throws at
//! runtime, and a file being typed has both for as long as the keystroke lasts. So this walk **never panics, never
//! runs past the literal, and never reports a piece it is not sure of**: an unclosed `{`, a `%` at the end, a `%[`
//! with no `]` all stop at the literal's last byte and keep what was decided before that point.
//!
//! # The files, and why there are four
//!
//! ```text
//! mod.rs        the vocabulary — what a piece is, what a literal is, which functions are read
//! scanner.rs    the three grammars, scanned into runs (a pure walk over one literal's text)
//! locator.rs    which literal of a file is a format string, and in which grammar (the function table)
//! describe.rs   what a piece means, for the popup
//! complete.rs   what may be typed at a position, for the completion list
//! ```
//!
//! The split is the feature's three consumers — colour, popup, completion — meeting one scanner and one locator.
//! They were one file while there were two consumers; the third is what made it worth saying which part answers
//! which question.
//!
//! [`§1 of docs/format-strings.md`]: ../../../docs/format-strings.md

use cpp_parser::SourceRange;

mod complete;
pub mod describe;
pub mod locator;
pub mod scanner;

/// **Which language a format string is written in** — decided by the function that receives it, not by its text.
///
/// The three are not dialects of one thing: `std::format`'s grammar is Python's (`{:>8.2f}`, `{{`), the C library's
/// two share a shape (`%-8.2f`) and differ in what a conversion means. A string is read as exactly one of them, and
/// a caller that cannot tell which does not read it at all — that is the whole reason [`FormatKind`] is an argument
/// rather than something the scanner guesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormatKind {
    /// `std::format`, `std::print`, `std::format_to`, and the `fmt` library's names — `{…}` and `{{`.
    Std,
    /// `printf`, `fprintf`, `snprintf`, … — `%…` with flags, width, precision.
    Printf,
    /// `scanf`, `sscanf`, `%[…]` — `%…` with a width and a scanset, and no flags.
    Scanf,
}

impl FormatKind {
    /// What a person calls it, for a message.
    pub fn name(self) -> &'static str {
        match self {
            FormatKind::Std => "a std::format string",
            FormatKind::Printf => "a printf format string",
            FormatKind::Scanf => "a scanf format string",
        }
    }
}

/// **What one run of bytes inside a literal is.** The vocabulary the colour maps to token types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PieceKind {
    /// The field's or conversion's own syntax: `{`, `}`, `:`, `.3`, `-`, `8`, `*`'s neighbours …
    Specifier,
    /// **A reference to an argument**: `{0}`, `{name}`, `%1$`, and the `*` that takes its width from one.
    Argument,
    /// The conversion character: `f`, `d`, `s`, `x`, `[a-z]`, `?`.
    Conversion,
    /// `{{`, `}}`, `%%` — a literal brace or percent, which consumes no argument.
    Escape,
    /// **A width or precision taken from an argument** — the inner `{}` of `{:{}}`.
    ///
    /// Kept apart from [`PieceKind::Specifier`] although it is drawn as one, because it is the piece that makes the
    /// *number of arguments* a format string uses impossible to count from the conversions alone: `{:{}}` uses two.
    /// The check that will read this needs the distinction; the colour does not.
    DynamicWidth,
}

impl PieceKind {
    /// Is this piece drawn as the field's own syntax?
    pub fn is_a_specifier(self) -> bool {
        matches!(self, PieceKind::Specifier | PieceKind::DynamicWidth)
    }
}

/// One run of a literal: what it is, and **where it is inside the literal token** (not in the file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub kind: PieceKind,
    /// Byte offset from the start of the literal token — the quote and any prefix included, so a caller adds the
    /// token's own offset and has a file offset.
    pub start: usize,
    pub end: usize,
}

impl Piece {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// **Which functions' literals to read, and with what names** — the project's own answer, not the scanner's.
///
/// The scanner does not know this type exists; the locator ([`literals_in`]) does. Defaults are **on**: a project
/// that says nothing gets the feature, and `false` is how it is turned off. See `[format]` in `.cppls.toml` — the
/// file's shape and the reasons for it are in `crate::project::config::FormatSection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatConfig {
    /// Read `{…}`: `std::format` / `print` / `println` / `format_to` … and `fmt::`'s.
    pub std_format: bool,
    /// Read the C library's `printf` family, and `fmt::printf`'s.
    pub printf: bool,
    /// Read the C library's `scanf` family.
    pub scanf: bool,
    /// **The project's own format functions**, each with the grammar it takes and where its format string is.
    ///
    /// Matched against the qualified name the analysis resolved, so `using mylib::logf;` works and a local `logf`
    /// is not read as one.
    pub functions: Vec<ExtraFunction>,
}

/// One of a project's own format functions — see [`FormatConfig::functions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraFunction {
    /// The **qualified** name the analysis resolves the callee to: `mylib::logf`.
    pub name: String,
    pub syntax: FormatKind,
    /// Which argument holds the format string, counted from zero.
    pub argument: usize,
}

impl Default for FormatConfig {
    fn default() -> Self {
        FormatConfig {
            std_format: true,
            printf: true,
            scanf: true,
            functions: Vec::new(),
        }
    }
}

impl FormatConfig {
    /// Is every grammar off and nothing extra — the state in which the whole pass can be skipped?
    pub fn is_off(&self) -> bool {
        !self.std_format && !self.printf && !self.scanf && self.functions.is_empty()
    }
}

/// **One literal that is a format string**, with the pieces already read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatLiteral {
    /// The literal token's range **in the file**, quotes and prefix included.
    pub range: SourceRange,
    pub kind: FormatKind,
    /// **Ascending, non-overlapping, merged** — see the module note.
    pub pieces: Vec<Piece>,
}

pub use complete::{Completion, completions_at};
pub use describe::describe;
pub use locator::{CallSite, literals_in};
pub use scanner::pieces_in;
