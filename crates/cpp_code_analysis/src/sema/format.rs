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
//! [`§1 of docs/format-strings.md`]: ../../../docs/format-strings.md

use cpp_parser::{CppTokenData, CppTokenKind, SourceRange};

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
/// The scanner below does not know this type exists; the locator ([`literals_in`]) does. Defaults are **on**: a
/// project that says nothing gets the feature, and `false` is how it is turned off. See `[highlight]` in
/// `.cppls.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatConfig {
    /// Read `std::format` / `print` / `println` / `format_to` … and `fmt::`'s.
    pub std_format: bool,
    /// Read the C library's `printf` family.
    pub printf: bool,
    /// Read the C library's `scanf` family.
    pub scanf: bool,
    /// **The project's own wrappers**, in `printf`'s syntax, with the format string first: `mylib::logf`.
    ///
    /// Matched against the fully qualified name the analysis resolved, so a local `logf` is not read as one.
    pub extra: Vec<String>,
    /// The project's own wrappers in `std::format`'s syntax: `mylib::log`.
    pub extra_format: Vec<String>,
}

impl Default for FormatConfig {
    fn default() -> Self {
        FormatConfig {
            std_format: true,
            printf: true,
            scanf: true,
            extra: Vec::new(),
            extra_format: Vec::new(),
        }
    }
}

impl FormatConfig {
    /// Is every family on and nothing extra — the state in which the whole pass can be skipped?
    pub fn is_off(&self) -> bool {
        !self.std_format
            && !self.printf
            && !self.scanf
            && self.extra.is_empty()
            && self.extra_format.is_empty()
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

// ---------------------------------------------------------------------------------------------------------------
// The scanner.
// ---------------------------------------------------------------------------------------------------------------

/// **Read one literal's pieces.** The whole public surface of the scanner.
///
/// `literal` is the **token's text**, quotes and prefix included (`R"(…)`, `u8"…"`, `L"…"`), and every offset
/// reported is an offset into it. Nothing is appended for a literal this module cannot read (a token that does not
/// begin with a quote), and nothing is appended for a piece that is not certain.
pub fn pieces_in(literal: &str, kind: FormatKind, out: &mut Vec<Piece>) {
    let Some(body) = body_of(literal) else {
        return;
    };

    match kind {
        FormatKind::Std => scan_the_std_form(literal, body, out),
        FormatKind::Printf => scan_the_c_form(literal, body, out, FormatKind::Printf),
        FormatKind::Scanf => scan_the_c_form(literal, body, out, FormatKind::Scanf),
    }
}

/// The bytes of a literal **between its quotes**: what a format string actually is.
///
/// The prefix and the quotes are not part of it, and neither is a raw string's delimiter — `R"xy(…)xy"` says `xy`
/// twice and neither is content. A **raw string has no backslash escapes either**, which is the other thing this
/// answers: `R"(\n)"` is a backslash and an `n`, where `"\n"` is one character.
#[derive(Debug, Clone, Copy)]
struct Body {
    start: usize,
    end: usize,
    raw: bool,
}

fn body_of(literal: &str) -> Option<Body> {
    let bytes = literal.as_bytes();

    // **The prefix**, which is one of ten spellings and ends where the quote begins: `u8R"`, `LR"`, `uR"`, `UR"`,
    // `R"`, `u8"`, `L"`, `u"`, `U"`, `"`. Written as a list rather than as "skip the letters", because `u8` has a
    // digit in it and a loop over `is_ascii_alphabetic` stops on that digit and calls the token unreadable.
    const PREFIXES: &[&str] = &["u8R", "LR", "uR", "UR", "R", "u8", "L", "u", "U", ""];
    let at = PREFIXES.iter().find_map(|prefix| {
        let length = prefix.len();
        (bytes.len() > length && bytes.starts_with(prefix.as_bytes()) && bytes[length] == b'"')
            .then_some(length)
    })?;
    let raw = literal.get(..at).is_some_and(|prefix| prefix.contains('R'));

    let at = at + 1;

    if !raw {
        // **The closing quote is the last byte.** A literal token ends with it by construction, and a token that
        // does not (the lexer's recovery, a file being typed) is read as if it ended at the token's end — which is
        // what "stop at the last byte" means and is never past the text.
        let end = if bytes.len() > at && bytes[bytes.len() - 1] == b'"' {
            bytes.len() - 1
        } else {
            bytes.len()
        };
        return Some(Body {
            start: at,
            end,
            raw,
        });
    }

    // **A raw string's body is between `(delimiter` and `)delimiter"`**, and the delimiter is whatever is written
    // there — up to sixteen characters, and nothing at all in the ordinary case.
    let open = literal.get(at..)?.find('(')? + at;
    let delimiter = literal.get(at..open)?;
    let closing = format!("){delimiter}\"");
    let end = if let Some(body) = literal.get(open + 1..) {
        // The token must end with the closing sequence, or the string is unterminated and the body runs to the end.
        if body.ends_with(&closing) {
            literal.len() - closing.len()
        } else {
            literal.len()
        }
    } else {
        literal.len()
    };
    Some(Body {
        start: open + 1,
        end: end.max(open + 1),
        raw,
    })
}

/// The byte at `at`, or `None` past the end.
fn byte_at(text: &str, at: usize) -> Option<u8> {
    text.as_bytes().get(at).copied()
}

/// The length in bytes of the character at `at` — 1 for anything the text does not hold.
fn step(text: &str, at: usize) -> usize {
    text.get(at..)
        .and_then(|rest| rest.chars().next())
        .map_or(1, char::len_utf8)
}

// `printf`'s conversion characters, `scanf`'s, and theirs with `%n`/`%m`. One list each, because the two grammars
// take different letters: `scanf` has no `%p` in C89 but every implementation has it, and neither has the other's
// `%[`.
const PRINTF_CONVERSIONS: &[u8] = b"diouxXeEfFgGaAcspnm";
const SCANF_CONVERSIONS: &[u8] = b"diouxXeEfFgGaAcspn";

/// `std::format`'s presentation types — the character that says *how* a value is written.
const STD_CONVERSIONS: &[u8] = b"aAbBcdeEfFgGopPsxX?";

// ---------------------------------------------------------------------------------------------------------------
// `std::format` / `fmt` — `{…}`, `{{`, `}}`.
// ---------------------------------------------------------------------------------------------------------------

fn scan_the_std_form(text: &str, body: Body, out: &mut Vec<Piece>) {
    let mut at = body.start;
    while at < body.end {
        match byte_at(text, at) {
            // **An escape, and it must be asked before a field.** `{{` is one literal brace; reading it as a field
            // would take the next `}` as its end and paint the pair as a field that holds nothing.
            Some(b'{') if byte_at(text, at + 1) == Some(b'{') => {
                push(out, PieceKind::Escape, at, at + 2);
                at += 2;
            }
            Some(b'}') if byte_at(text, at + 1) == Some(b'}') => {
                push(out, PieceKind::Escape, at, at + 2);
                at += 2;
            }
            Some(b'{') => at = scan_a_std_field(text, at, body.end, out),
            // **A C++ escape inside the literal**, which the runtime string does not contain: `"\{"` is a brace and
            // `"\n%d"` is not a conversion. Two bytes go by, so the second can never be read as a `{` or a `%`.
            // A raw string has none of these — `R"(\n)"` really is a backslash and an `n` — which the scanner must
            // not confuse with an escape either: skipping two bytes there would skip a real `%`.
            Some(b'\\') if !body.raw => at = (at + 2).min(body.end),
            _ => at += step(text, at),
        }
    }
}

/// One `{…}` field, from its `{`; answers where the field ends (past its `}`, or at the literal's end).
///
/// ```text
/// {                       a field that holds nothing
/// {}                      the same, and the two braces merge into one piece
/// {0}   {name}            an argument reference — the index or the name
/// {:.3f}                  a specifier: the colon, then alignment/sign/width/precision, then the type
/// {:{}}                   a width taken from an argument: the inner braces are their own piece
/// ```
fn scan_a_std_field(text: &str, open: usize, end: usize, out: &mut Vec<Piece>) -> usize {
    push(out, PieceKind::Specifier, open, open + 1);
    let mut at = open + 1;
    if at >= end {
        return end;
    }

    // **The argument**: digits (`{0}`) or a name (`{w}`) or nothing at all (`{:d}`). fmt allows a name, and so will
    // `std::format` for named arguments — which is why a letter is accepted here and not only a digit.
    let argument_from = at;
    if byte_at(text, at).is_some_and(|byte| byte.is_ascii_digit()) {
        while byte_at(text, at).is_some_and(|byte| byte.is_ascii_digit()) {
            at += 1;
        }
    } else if byte_at(text, at).is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic()) {
        while byte_at(text, at).is_some_and(|byte| byte == b'_' || byte.is_ascii_alphanumeric()) {
            at += 1;
        }
    }
    if at > argument_from {
        push(out, PieceKind::Argument, argument_from, at);
    }
    if at >= end {
        return end;
    }

    match byte_at(text, at) {
        // The field ends here: `{}`, `{0}`.
        Some(b'}') => {
            push(out, PieceKind::Specifier, at, at + 1);
            at + 1
        }
        Some(b':') => {
            push(out, PieceKind::Specifier, at, at + 1);
            let after_the_spec = scan_a_std_spec(text, at + 1, end, out);
            if after_the_spec < end && byte_at(text, after_the_spec) == Some(b'}') {
                push(
                    out,
                    PieceKind::Specifier,
                    after_the_spec,
                    after_the_spec + 1,
                );
                after_the_spec + 1
            } else {
                after_the_spec
            }
        }
        // **Not a field this grammar knows.** `{0x}` is a typo or a runtime string; the rest of the braces are the
        // field's own syntax and are painted as one run rather than guessed at.
        _ => {
            let close = find(text, at, end, b'}').unwrap_or(end);
            push(out, PieceKind::Specifier, at, close);
            if close < end {
                push(out, PieceKind::Specifier, close, close + 1);
                close + 1
            } else {
                close
            }
        }
    }
}

/// The specifier of a field, from just past its `:`; answers the index of the `}` that ends the field, or `end`.
///
/// The order of the parts is the standard's, and it matters: an `0` after the sign is the **zero-padding flag**
/// while an `0` in the width is a **width**, and a `<`/`>`/`^` is alignment unless a character precedes it, in which
/// case that character is the fill.
fn scan_a_std_spec(text: &str, from: usize, end: usize, out: &mut Vec<Piece>) -> usize {
    let mut at = from;

    // `[fill] align` — the one place a **multi-byte character** is part of the grammar (`{:宽>8}` centres with a
    // wide character), which is why this counts bytes with `step` and not with `+= 1`.
    let align_from = at;
    if !matches!(byte_at(text, at), Some(b'<') | Some(b'>') | Some(b'^')) {
        let second = at + step(text, at);
        if matches!(byte_at(text, second), Some(b'<') | Some(b'>') | Some(b'^')) {
            at = second;
        }
    }
    if matches!(at, _ if byte_at(text, at).is_some_and(|b| matches!(b, b'<' | b'>' | b'^'))) {
        at += 1;
        push(out, PieceKind::Specifier, align_from, at);
    }

    // `sign`, `#`, `0` — one run of flags, and the pieces are contiguous so they merge.
    while let Some(byte) = byte_at(text, at) {
        if !matches!(byte, b'+' | b'-' | b' ' | b'#' | b'0') {
            break;
        }
        push(out, PieceKind::Specifier, at, at + 1);
        at += 1;
    }

    // `width` — digits, or an argument's `{…}`.
    if byte_at(text, at) == Some(b'{') {
        at = scan_a_dynamic_width(text, at, end, out);
    } else {
        let width_from = at;
        while byte_at(text, at).is_some_and(|byte| byte.is_ascii_digit()) {
            at += 1;
        }
        if at > width_from {
            push(out, PieceKind::Specifier, width_from, at);
        }
    }

    // `.` precision — the same two shapes.
    if byte_at(text, at) == Some(b'.') {
        push(out, PieceKind::Specifier, at, at + 1);
        at += 1;
        if byte_at(text, at) == Some(b'{') {
            at = scan_a_dynamic_width(text, at, end, out);
        } else {
            let digits_from = at;
            while byte_at(text, at).is_some_and(|byte| byte.is_ascii_digit()) {
                at += 1;
            }
            if at > digits_from {
                push(out, PieceKind::Specifier, digits_from, at);
            }
        }
    }

    // `L` — the locale flag.
    if byte_at(text, at) == Some(b'L') {
        push(out, PieceKind::Specifier, at, at + 1);
        at += 1;
    }

    // **The presentation type**, which is the one piece a reader scans for: it is the `f` of `{:.3f}`.
    if let Some(byte) = byte_at(text, at)
        && STD_CONVERSIONS.contains(&byte)
    {
        push(out, PieceKind::Conversion, at, at + 1);
        at += 1;
    }

    at
}

/// The `{…}` that takes a width or a precision from an argument: `{:{}}`, `{0:{width}.{}}`.
///
/// **One piece**, because the braces are the reference's own syntax and the colour has nothing finer to say: the
/// whole `{}` is *this refers to an argument*. What is inside is read only to find the matching `}` — a nested
/// field cannot contain another (the standard allows one level), so a `{` inside is skipped to the `}`.
fn scan_a_dynamic_width(text: &str, open: usize, end: usize, out: &mut Vec<Piece>) -> usize {
    let mut at = open + 1;
    while at < end && byte_at(text, at) != Some(b'}') {
        at += step(text, at);
    }
    let close = if at < end { at + 1 } else { end };
    push(out, PieceKind::DynamicWidth, open, close);
    close
}

// ---------------------------------------------------------------------------------------------------------------
// `printf` and `scanf` — `%…`, `%%`.
// ---------------------------------------------------------------------------------------------------------------

/// The C library's two grammars, which differ in three places: `printf` has flags and a precision, `scanf` has a
/// suppression `*` and a scanset `%[…]`, and neither has the other's.
fn scan_the_c_form(text: &str, body: Body, out: &mut Vec<Piece>, kind: FormatKind) {
    let mut at = body.start;
    while at < body.end {
        match byte_at(text, at) {
            Some(b'\\') if !body.raw => at = (at + 2).min(body.end),
            Some(b'%') => {
                // **`%%` is an escape before it is anything else** — `printf("%%d")` prints `%d` and takes no
                // argument, so reading the second `%` as a conversion's start would paint an argument reference
                // that is not there.
                if byte_at(text, at + 1) == Some(b'%') {
                    push(out, PieceKind::Escape, at, at + 2);
                    at += 2;
                    continue;
                }
                match scan_a_c_conversion(text, at, body.end, out, kind) {
                    Some(after) => at = after,
                    // **Not a conversion.** A lone `%` at the end of the string, or `%` followed by something no
                    // grammar takes, is not painted at all: the reader's own string colour is the honest answer for
                    // "this is not a format spec" — the doc's §4.2 says so for the trailing `%`, and one rule
                    // covers both.
                    None => at += 1,
                }
            }
            _ => at += step(text, at),
        }
    }
}

/// One `%…` conversion, from its `%`; answers where it ends, or `None` when the text is not one.
///
/// The pieces are collected **in ascending order and nothing after the conversion's end**, so a `None` cannot leave
/// a half-painted conversion behind: what it pushed is taken back out of `out` before it answers.
fn scan_a_c_conversion(
    text: &str,
    from: usize,
    end: usize,
    out: &mut Vec<Piece>,
    kind: FormatKind,
) -> Option<usize> {
    // Where the conversion began, and what ended the run before it — so that taking the pieces back out does not
    // leave the previous run's piece shortened by a merge it should not have made.
    let mark = out.len();
    let tail = out.last().map(|piece| (piece.kind, piece.end));

    let conversion = scan_a_c_conversion_inner(text, from, end, out, kind);

    if conversion.is_none() {
        out.truncate(mark);
        if let Some((kind, end)) = tail
            && let Some(last) = out.last_mut()
            && last.kind == kind
        {
            last.end = end;
        }
    }
    conversion
}

fn scan_a_c_conversion_inner(
    text: &str,
    from: usize,
    end: usize,
    out: &mut Vec<Piece>,
    kind: FormatKind,
) -> Option<usize> {
    let mut at = from;
    push(out, PieceKind::Specifier, at, at + 1);
    at += 1;
    if at >= end {
        return None;
    }

    // **`printf` only: a positional reference.** `%2$d` takes its value from the third argument, and the `2$` is
    // the reference. `scanf` has none — a `%2$d` there is `%2d` and then a literal `$` — so the digit has to be
    // looked at with the `$` before it means anything.
    if kind == FormatKind::Printf {
        let digits_from = at;
        let mut after_digits = at;
        while byte_at(text, after_digits).is_some_and(|byte| byte.is_ascii_digit()) {
            after_digits += 1;
        }
        if after_digits > digits_from && byte_at(text, after_digits) == Some(b'$') {
            push(out, PieceKind::Argument, digits_from, after_digits + 1);
            at = after_digits + 1;
        }
    }

    // **`printf` only: the flags.** `-`, `+`, a space, `#`, `0`, and the POSIX `'` grouping flag. `scanf` reads all
    // of these as nothing at all — `%-d` is not a conversion there.
    if kind == FormatKind::Printf {
        let flags_from = at;
        while byte_at(text, at).is_some_and(|byte| matches!(byte, b'-' | b'+' | b' ' | b'#' | b'0'))
        {
            at += 1;
        }
        if at > flags_from {
            push(out, PieceKind::Specifier, flags_from, at);
        }
    }

    // **A width, or the argument it comes from.** `%*d` reads its width from an argument, which is what makes a
    // `printf`'s argument count not equal to its conversions' count.
    if byte_at(text, at) == Some(b'*') {
        push(out, PieceKind::Argument, at, at + 1);
        at += 1;
    } else {
        let width_from = at;
        while byte_at(text, at).is_some_and(|byte| byte.is_ascii_digit()) {
            at += 1;
        }
        if at > width_from {
            push(out, PieceKind::Specifier, width_from, at);
        }
    }

    // **`printf` only: a precision**, which is a `.` and then a count or an argument.
    if kind == FormatKind::Printf && byte_at(text, at) == Some(b'.') {
        push(out, PieceKind::Specifier, at, at + 1);
        at += 1;
        if byte_at(text, at) == Some(b'*') {
            push(out, PieceKind::Argument, at, at + 1);
            at += 1;
        } else {
            let digits_from = at;
            while byte_at(text, at).is_some_and(|byte| byte.is_ascii_digit()) {
                at += 1;
            }
            if at > digits_from {
                push(out, PieceKind::Specifier, digits_from, at);
            }
        }
    }

    // **The length modifier** — `hh`, `h`, `ll`, `l`, `j`, `z`, `t`, `L`. Two letters before one, because `ll` must
    // not be read as `l` and then a conversion `l`.
    let length_from = at;
    match byte_at(text, at) {
        Some(b'h') | Some(b'l') => {
            let doubled = byte_at(text, at + 1) == byte_at(text, at);
            at += if doubled { 2 } else { 1 };
        }
        Some(b'j') | Some(b'z') | Some(b't') | Some(b'L') => at += 1,
        _ => {}
    }
    if at > length_from {
        push(out, PieceKind::Specifier, length_from, at);
    }

    // **`scanf`'s scanset**: `%[a-z]`, `%[^,]`, `%[]]`. The `]` is content when it is the first character of the set,
    // which is the one rule that makes a naive scan end the set one byte early, and a `\` in the *source* is the
    // C++ escape of the character it stands for — so it cannot be the terminator either.
    if kind == FormatKind::Scanf && byte_at(text, at) == Some(b'[') {
        let set_from = at;
        at += 1;
        if byte_at(text, at) == Some(b'^') {
            at += 1;
        }
        if byte_at(text, at) == Some(b']') {
            at += 1;
        }
        while at < end && byte_at(text, at) != Some(b']') {
            at += if byte_at(text, at) == Some(b'\\') {
                2
            } else {
                1
            };
        }
        let close = if at < end { at + 1 } else { end };
        push(out, PieceKind::Conversion, set_from, close);
        return Some(close);
    }

    // **The conversion itself**, which is what makes the whole run a conversion rather than a stretch of `%` and
    // digits: without one, this is not a format spec and nothing is painted.
    let byte = byte_at(text, at)?;
    let known = match kind {
        FormatKind::Printf => PRINTF_CONVERSIONS.contains(&byte),
        FormatKind::Scanf => SCANF_CONVERSIONS.contains(&byte),
        FormatKind::Std => false,
    };
    if !known {
        return None;
    }
    push(out, PieceKind::Conversion, at, at + 1);
    Some(at + 1)
}

// ---------------------------------------------------------------------------------------------------------------
// The locator — which literal is a format string, and in which language.
// ---------------------------------------------------------------------------------------------------------------

/// **Every format string in a file**, read from its own token stream.
///
/// The question this answers is "is the literal at this token the format string of a call I recognise", and neither
/// half of it is a question about the *parse*:
///
/// ```text
/// std::format("…", x)     a CallExpr                     the tree has it
/// std::println("…", x)    a Declaration, not a call      the tree does NOT have it
/// printf("…", x)          a CallExpr
/// ```
///
/// A **statement that begins with a qualified name and a `(`** is the C++ grammar's most-vexing-parse shape, and
/// this parser reads `std::println("…", x);` as a *declaration* of nothing in particular — which is a defect of its
/// own (`std::sort(v.begin(), v.end());` is read the same way, and that costs signature help and the argument checks
/// too), but it is not this module's to fix. What matters here is that the same source text has **two readings** and
/// the format string is there either way: so the walk is over `(`/`,`/`)` in the **token stream**, which is one
/// reading, and the answer does not move when the parser's changes.
///
/// `qualified_name` is asked for a callee's **resolved** name — `scope::name` — at the callee's own offset, and only
/// when the written spelling cannot answer: `format("…")` is `std::format` after a `using`, and it is a project's own
/// function otherwise. It answers `None` when the analysis cannot say, which is a refusal and not a "no".
pub fn literals_in(
    source: &str,
    tokens: &[CppTokenData],
    config: &FormatConfig,
    qualified_name: &mut dyn FnMut(usize) -> Option<String>,
) -> Vec<FormatLiteral> {
    if config.is_off() {
        return Vec::new();
    }

    let mut found = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.kind != CppTokenKind::StringLiteral {
            continue;
        }
        let Some(call) = the_call_a_literal_is_in(source, tokens, index) else {
            continue;
        };
        let Some(kind) = the_format_kind_of(&call, config, qualified_name) else {
            continue;
        };
        let Some(text) = source.get(token.range.start_offset..token.range.end_offset()) else {
            continue;
        };
        let mut pieces = Vec::new();
        pieces_in(text, kind, &mut pieces);
        if pieces.is_empty() {
            continue;
        }
        found.push(FormatLiteral {
            range: token.range,
            kind,
            pieces,
        });
    }
    found
}

/// **A literal's place in a call**, as the token stream spells it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    /// The callee as written, `::` and all: `std::format`, `::printf`, `logf`.
    pub callee: String,
    /// Was the callee written with a **scope** — `std::format` rather than `format`?
    pub qualified: bool,
    /// Was it a **member** call — `stream.printf(…)`?
    pub member: bool,
    /// Which argument the literal is, counted from zero at the call's `(`.
    pub argument: usize,
    /// Is the call's **first** argument a string literal? What tells `std::print("…")` from `std::print(stream, "…")`.
    pub first_argument_is_a_literal: bool,
    /// Where the callee's first token is — the offset a resolution is asked at.
    pub callee_offset: usize,
}

/// **Is this token the whole of an argument of a call, and which one?**
///
/// The walk is a single pass backwards from the literal for its call's `(` and a single pass forwards over the
/// arguments before it, and both are over *significant* tokens only — a format string is often the only thing on its
/// line, and the whitespace between is not part of the grammar.
fn the_call_a_literal_is_in(
    source: &str,
    tokens: &[CppTokenData],
    index: usize,
) -> Option<CallSite> {
    // **The literal has to be the whole argument.** `f(a + "…")` passes a value a format string is not, and
    // `f("a" ` `"b")` is two tokens of one argument (see the module note on what is not read). Both are answered by
    // the tokens immediately around it: a literal that begins its argument has `(` or `,` before it, and one that
    // ends its argument has `,` or `)` after it.
    let before = previous_significant(tokens, index)?;
    if !matches!(
        tokens[before].kind,
        CppTokenKind::LeftParen | CppTokenKind::Comma
    ) {
        return None;
    }
    let after = next_significant(tokens, index)?;
    if !matches!(
        tokens[after].kind,
        CppTokenKind::RightParen | CppTokenKind::Comma
    ) {
        return None;
    }

    // **The enclosing `(`.** Backwards, counting brackets, so a literal inside `f(g("…"))` finds `g`'s parenthesis
    // and not `f`'s: the depth is what says which call this literal is an argument of. A `;`, a `{` or a `}` at
    // depth zero ends the search — a literal in a statement with no call before it on the same level is not an
    // argument of anything.
    let mut open = None;
    let mut depth = 0usize;
    let mut at = index;
    while at > 0 {
        at -= 1;
        match tokens[at].kind {
            CppTokenKind::RightParen | CppTokenKind::RightBracket | CppTokenKind::RightBrace => {
                depth += 1
            }
            CppTokenKind::LeftBracket | CppTokenKind::LeftBrace if depth == 0 => break,
            CppTokenKind::Semicolon if depth == 0 => break,
            CppTokenKind::LeftParen => {
                if depth == 0 {
                    open = Some(at);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    let open = open?;

    // **The callee**, which is the qualified name in front of the `(`: `format`, `std::format`, `::std::format`,
    // `obj.printf`. Anything else — `f()(…)`, `arr[0](…)`, `T{…}` — is not a name, and a name is the only thing the
    // table can answer for.
    let mut name_end = previous_significant(tokens, open)?;
    if tokens[name_end].kind != CppTokenKind::Identifier {
        return None;
    }
    let callee_end = tokens[name_end].range.end_offset();
    let mut first = name_end;
    let mut qualified = false;
    let mut member = false;
    loop {
        let Some(before_this) = previous_significant(tokens, first) else {
            break;
        };
        match tokens[before_this].kind {
            CppTokenKind::Scope => {
                // A leading `::` — `::printf`, the global scope. It is part of the spelling but not a qualifier:
                // there is one `printf`, so the name is the table's to answer for.
                let Some(qualifier) = previous_significant(tokens, before_this) else {
                    first = before_this;
                    break;
                };
                if tokens[qualifier].kind != CppTokenKind::Identifier {
                    first = before_this;
                    break;
                }
                qualified = true;
                first = qualifier;
                name_end = qualifier;
            }
            // `stream.printf(…)`: the name is the method's, and the object before the `.` is not part of it.
            CppTokenKind::Dot | CppTokenKind::Arrow => {
                member = true;
                break;
            }
            _ => break,
        }
    }
    let callee = source
        .get(tokens[first].range.start_offset..callee_end)?
        .to_string();
    let _ = name_end;

    // **Which argument**, and whether the first one is a literal. One forward pass: the commas at this level, and
    // the first argument's own first token.
    let mut argument = 0usize;
    let mut depth = 0usize;
    let mut first_argument_is_a_literal = false;
    let mut seen_anything = false;
    for token in &tokens[open + 1..index] {
        if is_layout(token.kind) {
            continue;
        }
        if !seen_anything {
            seen_anything = true;
            first_argument_is_a_literal = token.kind == CppTokenKind::StringLiteral;
        }
        match token.kind {
            CppTokenKind::LeftParen | CppTokenKind::LeftBracket | CppTokenKind::LeftBrace => {
                depth += 1
            }
            CppTokenKind::RightParen | CppTokenKind::RightBracket | CppTokenKind::RightBrace => {
                depth = depth.saturating_sub(1);
            }
            CppTokenKind::Comma if depth == 0 => argument += 1,
            _ => {}
        }
    }

    Some(CallSite {
        callee,
        qualified,
        member,
        argument,
        first_argument_is_a_literal,
        callee_offset: tokens[first].range.start_offset,
    })
}

/// **Where in a call the format string is.** The one thing a table of names per function has to carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// The argument at this index, and no other.
    At(usize),
    /// **`print` and `println`**, which have two overloads: `print(fmt, …)` and `print(stream, fmt, …)`.
    ///
    /// The literal is the format string when it is the first argument, or the second one **when the first is not a
    /// string literal**. That second half is what keeps `std::print("{}", "text")` from reading the *argument* as a
    /// format string — the string a reader expects to be painted is the first one, and the second is data.
    AtTheFirstArgumentUnlessItIsAStream,
}

/// **The functions this server reads, by name.** `docs/format-strings.md` §3.2 is the rule this implements.
///
/// The list is deliberately short and every entry is a name the standard (or a library everyone has) declares: a
/// name this table guessed would paint a string a project meant as data. A project's own wrappers come in through
/// `[highlight]`'s `extra` / `extra_format`, which is where a *person* says what a checker cannot know.
///
/// Two names are **missing on purpose**: `swprintf` and `_snwprintf` take the format at argument 2 in C and at
/// argument 1 in MSVC's library, so one table cannot be right for both and a wrong answer paints an argument.
const THE_TABLE: &[(&str, FormatKind, Position)] = &[
    // `std::format` and the `fmt` library, whose specifier grammar is the same one.
    ("format", FormatKind::Std, Position::At(0)),
    ("format_to", FormatKind::Std, Position::At(1)),
    ("format_to_n", FormatKind::Std, Position::At(2)),
    ("formatted_size", FormatKind::Std, Position::At(0)),
    ("vformat", FormatKind::Std, Position::At(0)),
    ("vformat_to", FormatKind::Std, Position::At(1)),
    (
        "print",
        FormatKind::Std,
        Position::AtTheFirstArgumentUnlessItIsAStream,
    ),
    (
        "println",
        FormatKind::Std,
        Position::AtTheFirstArgumentUnlessItIsAStream,
    ),
    (
        "vprint",
        FormatKind::Std,
        Position::AtTheFirstArgumentUnlessItIsAStream,
    ),
    (
        "vprintln",
        FormatKind::Std,
        Position::AtTheFirstArgumentUnlessItIsAStream,
    ),
    // `printf`, in the order of its own arguments: the output first where there is one.
    ("printf", FormatKind::Printf, Position::At(0)),
    ("fprintf", FormatKind::Printf, Position::At(1)),
    ("sprintf", FormatKind::Printf, Position::At(1)),
    ("snprintf", FormatKind::Printf, Position::At(2)),
    ("vprintf", FormatKind::Printf, Position::At(0)),
    ("vfprintf", FormatKind::Printf, Position::At(1)),
    ("vsprintf", FormatKind::Printf, Position::At(1)),
    ("vsnprintf", FormatKind::Printf, Position::At(2)),
    ("dprintf", FormatKind::Printf, Position::At(1)),
    ("vdprintf", FormatKind::Printf, Position::At(1)),
    ("asprintf", FormatKind::Printf, Position::At(1)),
    ("vasprintf", FormatKind::Printf, Position::At(1)),
    ("syslog", FormatKind::Printf, Position::At(1)),
    ("vsyslog", FormatKind::Printf, Position::At(2)),
    // The `_s` family of C11's Annex K, whose extra argument is the buffer's size.
    ("printf_s", FormatKind::Printf, Position::At(0)),
    ("fprintf_s", FormatKind::Printf, Position::At(1)),
    ("sprintf_s", FormatKind::Printf, Position::At(2)),
    ("snprintf_s", FormatKind::Printf, Position::At(2)),
    ("vsprintf_s", FormatKind::Printf, Position::At(2)),
    ("vsnprintf_s", FormatKind::Printf, Position::At(2)),
    // The wide-character spellings, which are the same grammar over `wchar_t`.
    ("wprintf", FormatKind::Printf, Position::At(0)),
    ("fwprintf", FormatKind::Printf, Position::At(1)),
    ("vwprintf", FormatKind::Printf, Position::At(0)),
    ("vfwprintf", FormatKind::Printf, Position::At(1)),
    // `scanf`, which has no flags and one extra shape — the scanset.
    ("scanf", FormatKind::Scanf, Position::At(0)),
    ("fscanf", FormatKind::Scanf, Position::At(1)),
    ("sscanf", FormatKind::Scanf, Position::At(1)),
    ("vscanf", FormatKind::Scanf, Position::At(0)),
    ("vfscanf", FormatKind::Scanf, Position::At(1)),
    ("vsscanf", FormatKind::Scanf, Position::At(1)),
    ("scanf_s", FormatKind::Scanf, Position::At(0)),
    ("fscanf_s", FormatKind::Scanf, Position::At(1)),
    ("sscanf_s", FormatKind::Scanf, Position::At(1)),
    ("wscanf", FormatKind::Scanf, Position::At(0)),
    ("fwscanf", FormatKind::Scanf, Position::At(1)),
    ("swscanf", FormatKind::Scanf, Position::At(1)),
];

/// **Which language this call's literal is written in**, or `None` for "not one this server reads".
///
/// Three questions, in this order, and each one is a refusal rather than a guess when it cannot answer:
///
/// ```text
/// 1. the name       is `printf` in the table? is `format`?
/// 2. the scope      `std::` and `fmt::` are the libraries';  any other scope is a project's own, and only
///                   `[highlight]` can say one is a wrapper — the same name in `app::` is not `std::`
/// 3. the spelling   an UNQUALIFIED `format("…")` could be either, so it is resolved through the index — a
///                   `using std::format;` answers `std::format`, a project's own answers its own scope, and an
///                   analysis that cannot say answers nothing (no colour, never a wrong one)
/// ```
///
/// The third question is not asked for `printf` and `scanf`: those names are the C library's by convention, they are
/// not in a namespace to collide with, and a project that declares its own `printf` with a different syntax is not a
/// case worth costing every ordinary call a lookup.
fn the_format_kind_of(
    call: &CallSite,
    config: &FormatConfig,
    qualified_name: &mut dyn FnMut(usize) -> Option<String>,
) -> Option<FormatKind> {
    // `::printf` and `::std::format` are the same functions as the unqualified spellings: the leading `::` says
    // *the global scope*, which for these names is where they are anyway.
    let written = call.callee.strip_prefix("::").unwrap_or(&call.callee);
    let (scope, bare) = match written.rfind("::") {
        Some(at) => (&written[..at], &written[at + 2..]),
        None => ("", written),
    };

    // **A resolved name is asked for at most once**, and only where the written spelling cannot decide.
    let mut resolved: Option<Option<String>> = None;
    let mut resolved_name = |ask: &mut dyn FnMut(usize) -> Option<String>| -> Option<String> {
        if resolved.is_none() {
            resolved = Some(ask(call.callee_offset));
        }
        resolved.clone().flatten()
    };

    // **The project's own wrappers**, which are the one thing a person can say that the analysis cannot. A qualified
    // spelling is matched as written; a bare one against what it resolves to, so `using mylib::logf;` works and a
    // local `logf` that is something else does not.
    let in_the_list = |entry: &str, what: &mut dyn FnMut() -> Option<String>| -> bool {
        if entry == written {
            return true;
        }
        !call.qualified && what().is_some_and(|name| name == entry)
    };
    if config
        .extra_format
        .iter()
        .any(|entry| in_the_list(entry, &mut || resolved_name(qualified_name)))
        && config.std_format
        && position_matches(Position::At(0), call)
    {
        return Some(FormatKind::Std);
    }
    if config
        .extra
        .iter()
        .any(|entry| in_the_list(entry, &mut || resolved_name(qualified_name)))
        && position_matches(Position::At(0), call)
    {
        return Some(FormatKind::Printf);
    }

    let Some(&(_, kind, position)) = THE_TABLE.iter().find(|(name, _, _)| *name == bare) else {
        return None;
    };

    // **Which scope may claim a name.** `std` for everything; `fmt` (and its inline `fmt::v11` version namespace)
    // for the names `fmt` shares with `std`; nothing else, unless `[highlight]` said so above.
    match kind {
        FormatKind::Std => {
            if !scope.is_empty() && scope != "std" && !is_fmt(scope) {
                return None;
            }
            // An unqualified `format(…)` is only this one if the analysis can say it is `std::format` — or `fmt`'s.
            if scope.is_empty()
                && !resolved_name(qualified_name).is_some_and(|name| {
                    the_scope_of(&name).is_some_and(|s| s == "std" || is_fmt(s))
                })
            {
                return None;
            }
            if !config.std_format {
                return None;
            }
        }
        FormatKind::Printf => {
            if !scope.is_empty() && scope != "std" && !is_fmt(scope) {
                return None;
            }
            if !config.printf {
                return None;
            }
        }
        FormatKind::Scanf => {
            if !scope.is_empty() && scope != "std" && !is_fmt(scope) {
                return None;
            }
            if !config.scanf {
                return None;
            }
        }
    }

    position_matches(position, call).then_some(kind)
}

/// Is this scope the `fmt` library's — including the versioned namespace it inlines (`fmt::v11::`)?
fn is_fmt(scope: &str) -> bool {
    scope == "fmt"
        || scope.strip_prefix("fmt::v").is_some_and(|version| {
            !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit())
        })
}

/// The scope of a resolved qualified name: `std` for `std::format`, `None` for a file-scope `printf`.
fn the_scope_of(qualified: &str) -> Option<&str> {
    qualified.rfind("::").map(|at| &qualified[..at])
}

fn position_matches(position: Position, call: &CallSite) -> bool {
    match position {
        Position::At(at) => call.argument == at,
        Position::AtTheFirstArgumentUnlessItIsAStream => {
            call.argument == 0 || (call.argument == 1 && !call.first_argument_is_a_literal)
        }
    }
}

/// **A token that is layout rather than code** — whitespace, a newline, a comment, a line splice.
///
/// The walk here is over `(`/`,`/`)`, and a format string is often the only thing on its line, so the whitespace
/// between the two is skipped rather than counted: `printf (\n    "%d",\n    x)` is one call for every purpose.
fn is_layout(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Whitespace
            | CppTokenKind::Newline
            | CppTokenKind::LineContinuation
            | CppTokenKind::LineComment
            | CppTokenKind::BlockComment
            | CppTokenKind::DocTrivia
            | CppTokenKind::Eof
            | CppTokenKind::None
    )
}

/// The index of the last non-layout token before `index`.
fn previous_significant(tokens: &[CppTokenData], index: usize) -> Option<usize> {
    let mut at = index;
    while at > 0 {
        at -= 1;
        if !is_layout(tokens[at].kind) {
            return Some(at);
        }
    }
    None
}

/// The index of the first non-layout token after `index`.
fn next_significant(tokens: &[CppTokenData], index: usize) -> Option<usize> {
    let mut at = index + 1;
    while at < tokens.len() {
        if !is_layout(tokens[at].kind) {
            return Some(at);
        }
        at += 1;
    }
    None
}

/// The first `needle` in `text[from..to]`, or `None`.
fn find(text: &str, from: usize, to: usize, needle: u8) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = from;
    while at < to {
        if bytes.get(at).copied() == Some(needle) {
            return Some(at);
        }
        at += 1;
    }
    None
}

/// Append a run, **merging it with the one before** when they are the same kind and touch.
///
/// The merge is what makes `{}` one piece instead of two and `%-8.2` one instead of five; without it a client is sent
/// a token per character, which is the same picture drawn a hundred times.
fn push(out: &mut Vec<Piece>, kind: PieceKind, start: usize, end: usize) {
    if end <= start {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.kind == kind
        && last.end == start
    {
        last.end = end;
        return;
    }
    out.push(Piece { kind, start, end });
}
