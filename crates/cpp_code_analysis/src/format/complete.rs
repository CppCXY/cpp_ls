//! **What can go here** — the completions inside a format string.
//!
//! ```cpp
//! printf("%|", x)              → d i o u x X e E f F g G a A c s p n m   (with what each one means)
//! printf("%5.|", x)            → the same list, at the precision's end
//! std::format("{:|}", x)       → d b o x X c s e f g a p ?   and `L`, and `{` for a dynamic width
//! scanf("%|", &n)              → d i o u x X e E f F g G a A c s p n   and `[` for a scanset
//! ```
//!
//! # Why this is the useful place to complete, and the only one
//!
//! The vocabulary of a format string is **closed and short** — two dozen letters in `printf`, a dozen in
//! `std::format` — and it is not the vocabulary of the language the file is written in, so an editor's own
//! completion has nothing to offer: inside `"%5."` there is no name, no keyword and no type, and the only thing that
//! can say what may follow is a reader of the grammar. That is this module.
//!
//! It is deliberately **narrow**: it completes where a conversion or a presentation type goes, and nowhere else. A
//! completion list offered inside `{width}` or in the middle of prose would be a list of conversions where the reader
//! is typing a variable's name, which is worse than no list at all.
//!
//! # What it does not know
//!
//! Nothing about the *arguments*: `{0}` could be completed with the argument indices of the call in hand, and that is
//! a different feature (it needs the call's expressions, which is the locator's business, and it is the one place a
//! wrong index is silently accepted). Not here, and not yet.

use super::{FormatKind, Piece, PieceKind, pieces_in};

/// **One thing that can be typed at the cursor**, with what it means.
///
/// `start` and `end` are offsets **in the literal's text** — the range the completion replaces — so a caller adds the
/// literal's file offset and has a protocol range. `detail` is one line, and is the same sentence the hover shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// What is inserted — one character for every conversion.
    pub label: String,
    pub detail: String,
    pub start: usize,
    pub end: usize,
}

/// **The completions at `at` bytes into the literal**, given the grammar it is read as.
///
/// Empty is the ordinary answer — most positions in a literal are prose — and it is the answer for every position
/// this module cannot be sure about, because a completion list is a claim that something *can* go here.
pub fn completions_at(literal: &str, kind: FormatKind, at: usize) -> Vec<Completion> {
    let Some((body_from, body_to)) = body_range(literal) else {
        return Vec::new();
    };
    if at < body_from || at > body_to {
        return Vec::new();
    }

    let mut pieces = Vec::new();
    pieces_in(literal, kind, &mut pieces);

    match kind {
        FormatKind::Std => complete_a_std_field(literal, at, &pieces),
        FormatKind::Printf | FormatKind::Scanf => complete_a_c_conversion(literal, kind, at),
    }
}

/// The bytes between a literal's quotes, or `None` for a token that is not a literal.
///
/// The same rule the scanner uses, and the two must agree: a completion offered outside the body would be offered on
/// the quotes.
fn body_range(literal: &str) -> Option<(usize, usize)> {
    let bytes = literal.as_bytes();
    let mut at = 0usize;
    while at < bytes.len() && bytes[at] != b'"' {
        at += 1;
    }
    if at >= bytes.len() {
        return None;
    }
    at += 1;
    let end = literal
        .rfind('"')
        .filter(|end| *end >= at)
        .unwrap_or(literal.len());
    Some((at, end))
}

// ---------------------------------------------------------------------------------------------------------------
// `printf` / `scanf` — after the `%`, the flags, the width and the length, the conversion goes.
// ---------------------------------------------------------------------------------------------------------------

/// **The conversions**, offered where a conversion character would go.
///
/// The run the cursor is in is found through the **pieces**, not by looking for the nearest `%`: `%%` is an escape and
/// offers nothing, `%1$` is an argument reference, `%5.` is a specifier — and only the *end* of a run is a place a
/// conversion can appear. The scanner already decided all of that, so this asks it rather than re-deriving it.
fn complete_a_c_conversion(literal: &str, kind: FormatKind, at: usize) -> Vec<Completion> {
    // **The run's `%`, found by walking back over prefix bytes.** The pieces cannot answer this: a conversion with no
    // letter yet (`"%` at the end of a literal, which is where a reader is when they ask for the list) produces *no
    // piece at all* — the scanner refuses to paint a `%` that is not a conversion. So the run is read from the text,
    // which is also what makes "is the cursor at the end of the run" a question about the cursor rather than about a
    // piece's bounds.
    let bytes = literal.as_bytes();
    let mut run = at;
    while run > 0 && is_a_prefix_byte(bytes[run - 1], kind) {
        run -= 1;
    }
    // The `%` is the byte just before that run — the cursor is *after* the introducer, which is the position a
    // conversion goes in.
    if run == 0 || bytes[run - 1] != b'%' {
        return Vec::new();
    }
    let percent = run - 1;
    // **`%%` is an escape, and its second `%` introduces nothing.** The parity of the run before this one is what
    // tells the two apart: one `%` before it means this one is the escape's second half, two mean this one starts a
    // conversion.
    let mut before_the_run = percent;
    let mut percents = 0usize;
    while before_the_run > 0 && bytes[before_the_run - 1] == b'%' {
        percents += 1;
        before_the_run -= 1;
    }
    if percents % 2 == 1 {
        return Vec::new();
    }
    let before = &literal[percent..at];
    if !is_prefix_of_a_conversion(before, kind) {
        // A complete conversion, or a `%%`, or something that is not one at all.
        return Vec::new();
    }
    // **And nothing *after* the cursor that belongs to the run**: `%|8d` is a reader typing a width, not one asking
    // for a conversion. `%|d` is not — the `d` is a conversion, which is what the replacement below is for.
    if bytes
        .get(at)
        .is_some_and(|byte| is_a_prefix_byte(*byte, kind))
    {
        return Vec::new();
    }

    let (start, end) = replacement_at(literal, at, kind);
    conversions_of(kind)
        .into_iter()
        .map(|(label, detail)| Completion {
            label: label.to_string(),
            detail: detail.to_string(),
            start,
            end,
        })
        .collect()
}

/// **A byte that can be written between the `%` and the conversion** in this grammar: a flag, a digit, a `.`, a `*`,
/// a positional `$`, or a length modifier.
///
/// The set is used to find the run's start (walking back over it) and to refuse a completion where the run continues
/// to the right of the cursor — one set, so the two answers cannot disagree.
fn is_a_prefix_byte(byte: u8, kind: FormatKind) -> bool {
    if byte.is_ascii_digit() {
        return true;
    }
    match kind {
        FormatKind::Printf => {
            matches!(byte, b'-' | b'+' | b' ' | b'#' | b'0' | b'.' | b'*' | b'$')
                || matches!(byte, b'h' | b'l' | b'j' | b'z' | b't' | b'L')
        }
        FormatKind::Scanf => {
            matches!(byte, b'*') || matches!(byte, b'h' | b'l' | b'j' | b'z' | b't' | b'L')
        }
        FormatKind::Std => false,
    }
}

/// Is `text` — a `%…` run up to the cursor — something a conversion may follow?
///
/// A small re-read of the grammar, and it has to be a re-read: the piece only says "this run is a specifier", and the
/// question here is whether the cursor is at the *end* of it rather than inside the flags.
fn is_prefix_of_a_conversion(text: &str, kind: FormatKind) -> bool {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'%') {
        return false;
    }
    let mut at = 1usize;

    // `printf` only: a positional reference and the flags.
    if kind == FormatKind::Printf {
        let digits_from = at;
        while bytes.get(at).is_some_and(|byte| byte.is_ascii_digit()) {
            at += 1;
        }
        if at > digits_from && bytes.get(at) == Some(&b'$') {
            at += 1;
        } else {
            at = digits_from;
        }
        while matches!(bytes.get(at), Some(b'-' | b'+' | b' ' | b'#' | b'0')) {
            at += 1;
        }
    }

    // The width, and `printf`'s precision.
    if bytes.get(at) == Some(&b'*') {
        at += 1;
    } else {
        while bytes.get(at).is_some_and(|byte| byte.is_ascii_digit()) {
            at += 1;
        }
    }
    if kind == FormatKind::Printf && bytes.get(at) == Some(&b'.') {
        at += 1;
        if bytes.get(at) == Some(&b'*') {
            at += 1;
        } else {
            while bytes.get(at).is_some_and(|byte| byte.is_ascii_digit()) {
                at += 1;
            }
        }
    }

    // The length modifier, which is the last thing before the conversion.
    match bytes.get(at) {
        Some(b'h' | b'l') if bytes.get(at + 1) == bytes.get(at) => at += 2,
        Some(b'h' | b'l' | b'j' | b'z' | b't' | b'L') => at += 1,
        _ => {}
    }

    // **And nothing after it**: a character already sitting where the conversion goes means the run is complete, in
    // which case the list would be offered on top of a reading conversion.
    at == bytes.len()
}

// ---------------------------------------------------------------------------------------------------------------
// `std::format` — after the `:`, a presentation type goes (and the flags, which are one character each)
// ---------------------------------------------------------------------------------------------------------------

/// **The presentation types**, offered where one would go.
///
/// The spec is `[fill]align [sign] [#] [0] [width] [.precision] [L] [type]`, and the type is what a reader asks about,
/// so this offers the types always and the flags only at the very start of the spec (where `<`, `>`, `^`, `+`, `#`,
/// `0`, `.` are all still possible). A cursor after a fill or an alignment is a place for a type, not for another
/// fill.
fn complete_a_std_field(literal: &str, at: usize, pieces: &[Piece]) -> Vec<Completion> {
    let Some((index, piece)) = piece_at(pieces, at) else {
        return Vec::new();
    };
    if piece.kind == PieceKind::Escape {
        return Vec::new();
    }

    // **The `{` that opens the field this specifier belongs to.** The specifier piece may be the whole `{:>10}`
    // (merged) or just its start, so the field is found by walking back to a piece that begins with `{`.
    let mut field_from = piece.start;
    let mut first = index;
    while first > 0 && pieces[first - 1].end == pieces[first].start {
        first -= 1;
        field_from = pieces[first].start;
        if literal.as_bytes().get(field_from) == Some(&b'{') {
            break;
        }
    }
    if literal.as_bytes().get(field_from) != Some(&b'{') {
        return Vec::new();
    }

    let before = &literal[field_from..at.min(piece.end)];
    let Some(colon) = before.find(':') else {
        // **Before the `:`**, which is the position right after `{` or `{0`. A presentation type cannot go here — the
        // grammar requires the colon — and an argument index is not something this module can know (it would need the
        // call's expressions). What *can* go here is the two things a field continues with, so those are offered:
        // without them, a reader who has just typed `{` gets an empty list at the one moment they are most likely to
        // ask.
        let (start, end) = replacement_at(literal, at, FormatKind::Std);
        return vec![
            Completion {
                label: ":".to_string(),
                detail: "a specification follows: fill, align, width, precision and the type"
                    .to_string(),
                start,
                end,
            },
            Completion {
                label: "}".to_string(),
                detail: "an empty field — the argument formatted the way its type defaults to"
                    .to_string(),
                start,
                end,
            },
        ];
    };

    let (start, end) = replacement_at(literal, at, FormatKind::Std);
    let mut items: Vec<Completion> = Vec::new();

    // **The flags and the alignments, but only at the start of the spec** — after a width or a precision the grammar
    // does not allow them, and offering them would be offering a mistake.
    let at_the_start = before[colon + 1..]
        .chars()
        .all(|ch| matches!(ch, '<' | '>' | '^'));
    if at_the_start {
        for (label, detail) in [
            ("<", "align left"),
            (">", "align right"),
            ("^", "centre"),
            ("+", "always signed"),
            ("#", "an alternative form (`0x`, `0b`, a leading `0`)"),
            ("0", "zero-padded"),
            (".", "a precision follows"),
            ("{", "a width taken from an argument"),
            ("L", "the locale-specific form"),
        ] {
            items.push(Completion {
                label: label.to_string(),
                detail: detail.to_string(),
                start,
                end,
            });
        }
    }

    for (label, detail) in conversions_of(FormatKind::Std) {
        items.push(Completion {
            label: label.to_string(),
            detail: detail.to_string(),
            start,
            end,
        });
    }
    items
}

// ---------------------------------------------------------------------------------------------------------------
// Shared.
// ---------------------------------------------------------------------------------------------------------------

/// The piece a cursor at `at` is in — **both ends inclusive**, because a cursor sits *between* two bytes and the
/// position after a `%` is exactly where the conversion goes.
fn piece_at(pieces: &[Piece], at: usize) -> Option<(usize, &Piece)> {
    pieces
        .iter()
        .enumerate()
        .find(|(_, piece)| piece.start <= at && at <= piece.end)
}

/// **Every conversion a grammar has**, with the one-line meaning.
///
/// The sentences are the ones [`super::describe`] uses, and they are written twice on purpose: a hover describes what
/// is **already written** (and may add parts from the neighbouring pieces), a completion offers what is **not**, and
/// coupling them would mean a function that has to be told which of the two it is answering.
fn conversions_of(kind: FormatKind) -> Vec<(char, &'static str)> {
    match kind {
        FormatKind::Printf => vec![
            ('d', "signed decimal"),
            ('i', "signed decimal, the base read from the text"),
            ('u', "unsigned decimal"),
            ('o', "unsigned octal"),
            ('x', "unsigned hexadecimal, lowercase"),
            ('X', "unsigned hexadecimal, uppercase"),
            ('e', "scientific, lowercase"),
            ('E', "scientific, uppercase"),
            ('f', "fixed-point decimal"),
            ('F', "fixed-point decimal, `INF`/`NAN` in capitals"),
            ('g', "whichever of fixed-point and scientific is shorter"),
            ('G', "the same, `INF`/`NAN` in capitals"),
            ('a', "hexadecimal floating point"),
            ('A', "hexadecimal floating point, uppercase"),
            ('c', "one character"),
            ('s', "a string"),
            ('p', "a pointer, printed as an address"),
            ('n', "writes the number of characters written so far"),
            ('m', "`strerror(errno)` — takes no argument (glibc)"),
        ],
        FormatKind::Scanf => vec![
            ('d', "signed decimal"),
            ('i', "signed decimal, the base read from the text"),
            ('u', "unsigned decimal"),
            ('o', "unsigned octal"),
            ('x', "unsigned hexadecimal"),
            ('X', "unsigned hexadecimal"),
            ('e', "a floating-point number"),
            ('f', "a floating-point number"),
            ('g', "a floating-point number"),
            ('a', "a floating-point number"),
            ('c', "one character, with no terminating `\\0`"),
            ('s', "a word, and a `char*` gets a terminating `\\0`"),
            ('p', "a pointer"),
            ('n', "stores the number of characters read so far"),
        ],
        FormatKind::Std => vec![
            ('d', "decimal"),
            ('b', "binary"),
            ('B', "binary, uppercase"),
            ('o', "octal"),
            ('x', "hexadecimal, lowercase"),
            ('X', "hexadecimal, uppercase"),
            ('c', "one character"),
            ('s', "a string"),
            ('e', "scientific"),
            ('E', "scientific, uppercase"),
            ('f', "fixed-point decimal"),
            ('F', "fixed-point decimal, uppercase"),
            ('g', "whichever of fixed-point and scientific is shorter"),
            ('G', "the same, uppercase"),
            ('a', "hexadecimal floating point"),
            ('A', "hexadecimal floating point, uppercase"),
            ('p', "a pointer"),
            ('?', "the debug form — the type's own, for a reader"),
        ],
    }
}

/// **What a completion at `at` replaces** — the character under the cursor when it could be the thing being
/// completed, and nothing otherwise.
///
/// The point is that completing over a spelling replaces it: `printf("%d", …)` with the cursor on the `d` offers `x`,
/// and picking it has to give `%x`. Inserting at the cursor instead would give `%dx`, which is a different string and
/// a silent mistake.
fn replacement_at(literal: &str, at: usize, kind: FormatKind) -> (usize, usize) {
    let bytes = literal.as_bytes();
    let could_be_a_conversion = |byte: u8| match kind {
        FormatKind::Std => {
            (byte.is_ascii_alphanumeric() && byte != b'L') || matches!(byte, b'?' | b'#' | b'0')
        }
        _ => byte.is_ascii_alphanumeric(),
    };
    match bytes.get(at) {
        Some(byte) if could_be_a_conversion(*byte) => (at, at + 1),
        _ => (at, at),
    }
}
