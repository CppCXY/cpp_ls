//! **The three format grammars, scanned into runs** — `{…}`, `%…`, `%[…]`.
//!
//! A pure walk over one literal's text. `pieces_in` is the only entry point, and every offset it reports is relative
//! to the literal's own bytes (quotes and prefix included), so a caller adds the token's file offset and has the
//! place to paint.

use super::{FormatKind, Piece, PieceKind};
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
