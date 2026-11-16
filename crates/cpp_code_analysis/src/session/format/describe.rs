//! **What a piece means** — the grammar's own reading, as `(label, value)` pairs.
//!
//! Grammar knowledge lives here rather than in the server: the labels are the grammar's words and the values are the
//! reading, so a tooltip does not have to know that `%-8.2` is flags-then-width-then-precision.

use super::{FormatKind, Piece, PieceKind};
// ---------------------------------------------------------------------------------------------------------------
// What a piece means — the grammar's own reading, for a tooltip.
// ---------------------------------------------------------------------------------------------------------------

/// **What one piece says**, as `(label, value)` pairs a tooltip can print as a list.
///
/// The split is deliberate: **the labels are grammar words and the values are the reading**, so a consumer does not
/// have to know that `%-8.2` is "flags, width, precision" — it gets `("flags", "left-aligned")`,
/// `("width", "8")`, `("precision", "2")` and prints them. The grammar knowledge stays in this file (it is the same
/// knowledge the scanner uses, and a second copy in the server would drift), and the *prose* stays in the server
/// (which is the only layer that knows what a hover is).
///
/// Empty for a piece this file cannot read, which is the honest answer for `%y` in a broken format string.
///
/// ```text
///   %-8.2f    ("flags", "left-aligned") ("width", "8") ("precision", "2") ("type", "fixed-point decimal — a double")
///   {:.3f}    ("precision", "3") ("type", "fixed-point decimal — a double")
///   %1$s      ("argument", "the first argument, by position")
///   %*d       ("argument", "the width comes from an argument")
///   %[a-z]    ("set", "one or more characters from `a-z`")
/// ```
pub fn describe(literal: &str, kind: FormatKind, piece: &Piece) -> Vec<(&'static str, String)> {
    let text = match literal.get(piece.start..piece.end) {
        Some(text) => text,
        None => return Vec::new(),
    };

    match piece.kind {
        PieceKind::Conversion => describe_a_conversion(text, kind),
        PieceKind::Specifier => match kind {
            FormatKind::Std => describe_a_std_specifier(text),
            FormatKind::Printf | FormatKind::Scanf => describe_a_c_specifier(text, kind),
        },
        PieceKind::Argument => describe_an_argument(text, kind),
        PieceKind::DynamicWidth => vec![(
            "width",
            "taken from an argument, which is why this field uses two".to_string(),
        )],
        PieceKind::Escape => vec![(
            "escape",
            match text {
                "{{" => "a literal `{`".to_string(),
                "}}" => "a literal `}`".to_string(),
                _ => "a literal `%`".to_string(),
            },
        )],
    }
}

/// **What a conversion character means** — the one part of a format string a reader most often has to look up.
///
/// `None` for a character this grammar does not take, which is a broken format string rather than an unknown one.
fn describe_a_conversion(text: &str, kind: FormatKind) -> Vec<(&'static str, String)> {
    if kind == FormatKind::Scanf && text.starts_with('[') {
        let body = text.trim_start_matches('[').trim_end_matches(']');
        let negated = body.starts_with('^');
        let set = body.trim_start_matches('^');
        return vec![(
            "set",
            match negated {
                true => format!("one or more characters **not** in `{set}`"),
                false => format!("one or more characters from `{set}`"),
            },
        )];
    }

    let Some(character) = text.chars().next() else {
        return Vec::new();
    };
    let meaning = match (kind, character) {
        // **The three families share four letters and mean different things by them**, which is the reason this
        // table is keyed by the grammar and not by the letter.
        (FormatKind::Printf, 'd') | (FormatKind::Scanf, 'd') => "signed decimal",
        (FormatKind::Printf, 'i') | (FormatKind::Scanf, 'i') => {
            "signed decimal (the base is read from the text)"
        }
        (FormatKind::Printf, 'u') | (FormatKind::Scanf, 'u') => "unsigned decimal",
        (FormatKind::Printf, 'o') | (FormatKind::Scanf, 'o') => "unsigned octal",
        (FormatKind::Printf, 'x') | (FormatKind::Scanf, 'x') => "unsigned hexadecimal, lowercase",
        (FormatKind::Printf, 'X') | (FormatKind::Scanf, 'X') => "unsigned hexadecimal, uppercase",
        (FormatKind::Printf, 'e') => "scientific, lowercase — `1.5e+00`",
        (FormatKind::Printf, 'E') => "scientific, uppercase",
        (FormatKind::Printf, 'f') => "fixed-point decimal — a `double` written as `-.--`",
        (FormatKind::Printf, 'F') => "fixed-point decimal, `INF`/`NAN` in capitals",
        (FormatKind::Printf, 'g') | (FormatKind::Printf, 'G') => {
            "whichever of fixed-point and scientific is shorter"
        }
        (FormatKind::Printf, 'a') | (FormatKind::Printf, 'A') => "hexadecimal floating point",
        (FormatKind::Printf, 'c') => "one character",
        (FormatKind::Printf, 's') => "a string, up to the first `\\0` (or the precision)",
        (FormatKind::Printf, 'p') => "a pointer, printed as an address",
        (FormatKind::Printf, 'n') => {
            "**writes** the number of characters written so far through a pointer, and prints nothing"
        }
        (FormatKind::Printf, 'm') => "`strerror(errno)` — takes no argument (glibc)",
        (FormatKind::Scanf, 'e')
        | (FormatKind::Scanf, 'E')
        | (FormatKind::Scanf, 'f')
        | (FormatKind::Scanf, 'F')
        | (FormatKind::Scanf, 'g')
        | (FormatKind::Scanf, 'G')
        | (FormatKind::Scanf, 'a')
        | (FormatKind::Scanf, 'A') => "a floating-point number",
        (FormatKind::Scanf, 'c') => "one character — **no** terminating `\\0` is written",
        (FormatKind::Scanf, 's') => "a word, and an argument of `char*` gets a terminating `\\0`",
        (FormatKind::Scanf, 'p') => "a pointer, read as an address",
        (FormatKind::Scanf, 'n') => {
            "**stores** the number of characters read so far — it reads no input and takes no width"
        }
        (FormatKind::Std, 'd') => "decimal",
        (FormatKind::Std, 'b') | (FormatKind::Std, 'B') => "binary",
        (FormatKind::Std, 'o') => "octal",
        (FormatKind::Std, 'x') => "hexadecimal, lowercase",
        (FormatKind::Std, 'X') => "hexadecimal, uppercase",
        (FormatKind::Std, 'c') => "one character",
        (FormatKind::Std, 's') => "a string",
        (FormatKind::Std, 'e') | (FormatKind::Std, 'E') => "scientific",
        (FormatKind::Std, 'f') | (FormatKind::Std, 'F') => "fixed-point decimal",
        (FormatKind::Std, 'g') | (FormatKind::Std, 'G') => {
            "whichever of fixed-point and scientific is shorter"
        }
        (FormatKind::Std, 'a') | (FormatKind::Std, 'A') => "hexadecimal floating point",
        (FormatKind::Std, 'p') | (FormatKind::Std, 'P') => "a pointer",
        (FormatKind::Std, '?') => {
            "the **debug** form — the type's own, for a reader and not for a parser"
        }
        _ => return Vec::new(),
    };
    vec![("type", meaning.to_string())]
}

/// The `%…` of a `printf` or `scanf`: what its flags, width, precision and length say.
fn describe_a_c_specifier(text: &str, kind: FormatKind) -> Vec<(&'static str, String)> {
    let mut parts = Vec::new();
    let bytes = text.as_bytes();
    let mut at = usize::from(bytes.first() == Some(&b'%'));

    if kind == FormatKind::Printf {
        let flags_from = at;
        while matches!(bytes.get(at), Some(b'-' | b'+' | b' ' | b'#' | b'0')) {
            at += 1;
        }
        if at > flags_from {
            let flags: Vec<&str> = bytes[flags_from..at]
                .iter()
                .map(|flag| match flag {
                    b'-' => "left-aligned",
                    b'+' => "always signed",
                    b' ' => "a space for a positive value",
                    b'#' => "an alternative form (`0x`, a leading `0`)",
                    _ => "zero-padded",
                })
                .collect();
            parts.push(("flags", flags.join(", ")));
        }
    }

    let width_from = at;
    while bytes.get(at).is_some_and(|byte| byte.is_ascii_digit()) {
        at += 1;
    }
    if at > width_from {
        parts.push(("width", text[width_from..at].to_string()));
    }
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        let digits_from = at;
        while bytes.get(at).is_some_and(|byte| byte.is_ascii_digit()) {
            at += 1;
        }
        if at > digits_from {
            parts.push(("precision", text[digits_from..at].to_string()));
        }
    }

    let length_from = at;
    at += match bytes.get(at) {
        Some(b'h' | b'l') if bytes.get(at + 1) == bytes.get(at) => 2,
        Some(b'h' | b'l' | b'j' | b'z' | b't' | b'L') => 1,
        _ => 0,
    };
    if at > length_from {
        let meaning = match &text[length_from..at] {
            "hh" => "the argument is a `signed char`/`unsigned char`",
            "h" => "a `short`",
            "l" => "a `long`",
            "ll" => "a `long long`",
            "j" => "an `intmax_t`/`uintmax_t`",
            "z" => "a `size_t` (or its signed counterpart)",
            "t" => "a `ptrdiff_t`",
            _ => "a `long double`",
        };
        parts.push(("length", meaning.to_string()));
    }

    // **`%` on its own**, which is what a conversion with no flags, width or length leaves: the letter after it is
    // the conversion, and it has its own piece (and its own tooltip).
    if parts.is_empty() {
        parts.push((
            "conversion",
            "introduced by `%`; the letter after any flags and width says which".to_string(),
        ));
    }
    parts
}

/// The `{…}` of a `std::format` field: alignment, sign, width, precision — the parts that are not the type.
fn describe_a_std_specifier(text: &str) -> Vec<(&'static str, String)> {
    let mut parts = Vec::new();
    let body = text.trim_start_matches('{').trim_start_matches(':');
    let body = body.trim_end_matches('}');
    let characters: Vec<char> = body.chars().collect();
    let mut at = 0usize;

    // `[fill] align`, where the fill is **one character** and may be multi-byte.
    if at < characters.len() {
        let align = matches!(characters[at], '<' | '>' | '^');
        if align {
            parts.push(("align", reading_of_alignment(characters[at])));
            at += 1;
        } else if matches!(characters.get(at + 1), Some('<' | '>' | '^')) {
            parts.push(("fill", format!("`{}`", characters[at])));
            parts.push(("align", reading_of_alignment(characters[at + 1])));
            at += 2;
        }
    }

    let sign_from = at;
    while at < characters.len() && matches!(characters[at], '+' | '-' | ' ' | '#' | '0') {
        at += 1;
    }
    if at > sign_from {
        let flags: Vec<String> = characters[sign_from..at]
            .iter()
            .map(|character| match character {
                '+' => "always signed".to_string(),
                '-' => "a `-` only for a negative value (the default)".to_string(),
                ' ' => "a space for a positive value".to_string(),
                '#' => "an alternative form (`0x`, `0b`, a leading `0`)".to_string(),
                _ => "zero-padded".to_string(),
            })
            .collect();
        parts.push(("flags", flags.join(", ")));
    }

    let width_from = at;
    while at < characters.len() && characters[at].is_ascii_digit() {
        at += 1;
    }
    if at > width_from {
        let width: String = characters[width_from..at].iter().collect();
        parts.push(("width", width));
    }
    if at < characters.len() && characters[at] == '.' {
        at += 1;
        let digits_from = at;
        while at < characters.len() && characters[at].is_ascii_digit() {
            at += 1;
        }
        if at > digits_from {
            let precision: String = characters[digits_from..at].iter().collect();
            parts.push(("precision", precision));
        }
    }
    if at < characters.len() && characters[at] == 'L' {
        parts.push(("locale", "the locale-specific form".to_string()));
    }

    // **`{}` and `{:}`** — a field with nothing said about how, which is the commonest field there is.
    if parts.is_empty() {
        parts.push((
            "field",
            "formatted the way the argument's type defaults to".to_string(),
        ));
    }

    parts
}

fn reading_of_alignment(align: char) -> String {
    match align {
        '<' => "left".to_string(),
        '>' => "right".to_string(),
        _ => "centred".to_string(),
    }
}

/// The argument reference a field or a conversion names, and what it means for the call.
fn describe_an_argument(text: &str, kind: FormatKind) -> Vec<(&'static str, String)> {
    // **`%1$s`** — a position, counted from one, and the argument after it is *not* the next one.
    if let Some(digits) = text.strip_suffix('$')
        && let Ok(position) = digits.parse::<usize>()
    {
        return vec![(
            "argument",
            format!(
                "number {position}, counted from 1 — and this conversion does not advance the count"
            ),
        )];
    }
    // **`*`** — the width or the precision is itself an argument.
    if text == "*" {
        return vec![(
            "argument",
            match kind {
                FormatKind::Scanf => {
                    "assigns nothing: the conversion is read and discarded".to_string()
                }
                _ => "the width or precision comes from an argument".to_string(),
            },
        )];
    }
    // `{0}` or `{name}`: the field's own argument.
    vec![(
        "argument",
        match text.parse::<usize>() {
            Ok(index) => format!("number {index}, counted from 0"),
            Err(_) => format!("the argument named `{text}`"),
        },
    )]
}
