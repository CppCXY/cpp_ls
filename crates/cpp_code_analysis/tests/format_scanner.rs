//! **The format-string scanner**, asked of literals rather than of files.
//!
//! Every case here is a **literal token's text** — quotes and prefix included — and what comes back is the runs a
//! client would paint. Nothing in this file builds a session, an index or a tree: the scanner is a pure function over
//! a `&str`, and that is the whole point of putting it in its own module (see `docs/format-strings.md` §3.1).
//!
//! ```text
//! cargo test -p cpp_code_analysis --test format_scanner
//! ```
//!
//! The table is written as `(literal, expected)` where an expected piece is `(kind, text)` — the *text* rather than
//! a byte range, because a range is what the code computes and a reader cannot check it by eye. The ranges are
//! asserted on top of that where the arithmetic is the thing under test (`a_pieces_range_is_inside_the_literal`).

use cpp_code_analysis::format::{FormatKind, PieceKind, pieces_in};

/// The pieces of one literal, as `(kind, the bytes it covers)`.
fn pieces(literal: &str, kind: FormatKind) -> Vec<(&'static str, &str)> {
    let mut found = Vec::new();
    pieces_in(literal, kind, &mut found);
    found
        .iter()
        .map(|piece| {
            let name = match piece.kind {
                PieceKind::Specifier => "specifier",
                PieceKind::Argument => "argument",
                PieceKind::Conversion => "conversion",
                PieceKind::Escape => "escape",
                PieceKind::DynamicWidth => "dynamic",
            };
            (name, &literal[piece.start..piece.end])
        })
        .collect()
}

/// **`std::format`'s grammar**, which is Python's: `{}` and `{:spec}`, `{{` and `}}`.
#[test]
fn a_std_field_is_its_own_pieces() {
    for (literal, expected) in [
        // The ordinary field, and the two braces are one run because they are one kind and they touch.
        (r#""{}""#, vec![("specifier", "{}")]),
        (r#""hello {}""#, vec![("specifier", "{}")]),
        // An argument reference by index, by name, and the field around it.
        (
            r#""{0}""#,
            vec![("specifier", "{"), ("argument", "0"), ("specifier", "}")],
        ),
        (
            r#""{name}""#,
            vec![("specifier", "{"), ("argument", "name"), ("specifier", "}")],
        ),
        // A specifier: the colon, the precision, the type. **The colon and the precision are one run** — they are
        // the same kind and they touch, and a client is sent one token rather than two for the same colour.
        (
            r#""{:.3f}""#,
            vec![
                ("specifier", "{:.3"),
                ("conversion", "f"),
                ("specifier", "}"),
            ],
        ),
        // Alignment and width.
        (r#""{:>10}""#, vec![("specifier", "{:>10}")]),
        (r#""{:*<8}""#, vec![("specifier", "{:*<8}")]),
        // A sign, a zero pad and a precision in the standard's order.
        (
            r#""{:+08.2f}""#,
            vec![
                ("specifier", "{:+08.2"),
                ("conversion", "f"),
                ("specifier", "}"),
            ],
        ),
        // The debug and locale flags of C++23.
        (
            r#""{:?}""#,
            vec![("specifier", "{:"), ("conversion", "?"), ("specifier", "}")],
        ),
        (r#""{:L}""#, vec![("specifier", "{:L}")]),
        // **A width taken from an argument** — the one field that uses two of them.
        (
            r#""{:{}}""#,
            vec![("specifier", "{:"), ("dynamic", "{}"), ("specifier", "}")],
        ),
        (
            r#""{0:{1}.{2}f}""#,
            vec![
                ("specifier", "{"),
                ("argument", "0"),
                ("specifier", ":"),
                ("dynamic", "{1}"),
                ("specifier", "."),
                ("dynamic", "{2}"),
                ("conversion", "f"),
                ("specifier", "}"),
            ],
        ),
        // An escape consumes no argument, and it is not a field. The four braces are one run: same kind, touching.
        (r#""{{}}""#, vec![("escape", "{{}}")]),
        (r#""{{x}}""#, vec![("escape", "{{"), ("escape", "}}")]),
        // The real mixture: an escape, a field with a specifier, an argument, and text between them.
        (
            r#""值 = {:.3f}，宽度 {:{}}  {{ok}}""#,
            vec![
                ("specifier", "{:.3"),
                ("conversion", "f"),
                ("specifier", "}"),
                ("specifier", "{:"),
                ("dynamic", "{}"),
                ("specifier", "}"),
                ("escape", "{{"),
                ("escape", "}}"),
            ],
        ),
    ] {
        assert_eq!(pieces(literal, FormatKind::Std), expected, "{literal}");
    }
}

/// **A malformed field is not a panic**, which matters because a malformed field is what a keystroke leaves behind.
#[test]
fn an_unclosed_field_stops_at_the_end_of_the_literal() {
    for (literal, expected) in [
        (r#""{""#, vec![("specifier", "{")]),
        (r#""{:""#, vec![("specifier", "{:")]),
        (r#""{0""#, vec![("specifier", "{"), ("argument", "0")]),
        (r#""{:.2""#, vec![("specifier", "{:.2")]),
        // A `{` that is not a field's start at all: the rest of the braces are painted as the field's own syntax
        // rather than guessed at, and the literal still ends where it ends.
        (
            r#""{0x}""#,
            vec![("specifier", "{"), ("argument", "0"), ("specifier", "x}")],
        ),
        // **A field still being typed.** `{b` is a name as far as the grammar is concerned — it is what `{name}`
        // looks like after two keystrokes — so the `b` is the argument reference and the run simply ends.
        (r#""a{b""#, vec![("specifier", "{"), ("argument", "b")]),
    ] {
        assert_eq!(pieces(literal, FormatKind::Std), expected, "{literal}");
    }
}

/// **`printf`'s grammar**: flags, width, precision, length, conversion — and the two things that take an argument
/// without being a conversion.
#[test]
fn a_printf_conversion_is_its_own_pieces() {
    for (literal, expected) in [
        (r#""%d""#, vec![("specifier", "%"), ("conversion", "d")]),
        (r#""%.2f""#, vec![("specifier", "%.2"), ("conversion", "f")]),
        (
            r#""%-8.2f""#,
            vec![("specifier", "%-8.2"), ("conversion", "f")],
        ),
        (
            r#""%+05d""#,
            vec![("specifier", "%+05"), ("conversion", "d")],
        ),
        (r#""%#x""#, vec![("specifier", "%#"), ("conversion", "x")]),
        // A **positional reference** — `%1$s` takes its value from the first argument, wherever it is written.
        (
            r#""%1$s""#,
            vec![("specifier", "%"), ("argument", "1$"), ("conversion", "s")],
        ),
        // A **width from an argument** — the other reason a printf's argument count is not its conversions' count.
        (
            r#""%*d""#,
            vec![("specifier", "%"), ("argument", "*"), ("conversion", "d")],
        ),
        (
            r#""%.*f""#,
            vec![("specifier", "%."), ("argument", "*"), ("conversion", "f")],
        ),
        // The length modifiers, which must not be read as a conversion of their own: `%zu` is not `%z` then `u`.
        (r#""%zu""#, vec![("specifier", "%z"), ("conversion", "u")]),
        (r#""%lld""#, vec![("specifier", "%ll"), ("conversion", "d")]),
        (r#""%Lf""#, vec![("specifier", "%L"), ("conversion", "f")]),
        (r#""%n""#, vec![("specifier", "%"), ("conversion", "n")]),
        // glibc's `%m`, which takes no argument and prints `strerror(errno)`.
        (r#""%m""#, vec![("specifier", "%"), ("conversion", "m")]),
        (r#""%%""#, vec![("escape", "%%")]),
        // **`%%` before anything else.** `printf("%%d")` prints `%d`, so a scanner that read the second `%` as a
        // conversion's start would paint an argument reference that does not exist.
        (r#""%%d""#, vec![("escape", "%%")]),
        // The whole line from the doc: every shape at once.
        (
            r#""%-8.2f|%*d|%1$s|%%""#,
            vec![
                ("specifier", "%-8.2"),
                ("conversion", "f"),
                ("specifier", "%"),
                ("argument", "*"),
                ("conversion", "d"),
                ("specifier", "%"),
                ("argument", "1$"),
                ("conversion", "s"),
                ("escape", "%%"),
            ],
        ),
    ] {
        assert_eq!(pieces(literal, FormatKind::Printf), expected, "{literal}");
    }
}

/// **A `%` that is not a conversion is not painted**, and a trailing one is not a panic.
#[test]
fn a_broken_printf_conversion_is_left_alone() {
    for (literal, expected) in [
        (r#""%""#, vec![]),
        (r#""100%""#, vec![]),
        (r#""%5""#, vec![]),
        (r#""%.2""#, vec![]),
        // A `%` in front of a letter no conversion takes — and the `%` itself is left alone too, because a lone `%`
        // is not a format spec: the reader's own string colour is the honest answer.
        (r#""%y""#, vec![]),
        // …and a real conversion after the broken one is still found.
        (r#""%y %d""#, vec![("specifier", "%"), ("conversion", "d")]),
    ] {
        assert_eq!(pieces(literal, FormatKind::Printf), expected, "{literal}");
    }
}

/// **`scanf`'s grammar**: a suppressed assignment, a width, a length, a scanset — and no flags and no precision.
#[test]
fn a_scanf_conversion_is_its_own_pieces() {
    for (literal, expected) in [
        (r#""%d""#, vec![("specifier", "%"), ("conversion", "d")]),
        (r#""%3d""#, vec![("specifier", "%3"), ("conversion", "d")]),
        (r#""%ld""#, vec![("specifier", "%l"), ("conversion", "d")]),
        // **Suppression**: `%*d` reads and discards, so the `*` is a reference to nothing at all — painted as the
        // argument it is not, which is what the doc's §4.3 asks for.
        (
            r#""%*d""#,
            vec![("specifier", "%"), ("argument", "*"), ("conversion", "d")],
        ),
        // **The scanset**, brackets included, because the brackets are half of what a reader recognises.
        (
            r#""%[a-z]""#,
            vec![("specifier", "%"), ("conversion", "[a-z]")],
        ),
        (
            r#""%[^,]\n""#,
            vec![("specifier", "%"), ("conversion", "[^,]")],
        ),
        // A `]` as the set's first character is **content**, not its end — the one rule that makes a naive scan
        // stop one byte early.
        (r#""%[]]""#, vec![("specifier", "%"), ("conversion", "[]]")]),
        // A set that is never closed runs to the end of the literal rather than past it.
        (
            r#""%[abc""#,
            vec![("specifier", "%"), ("conversion", "[abc")],
        ),
        // `%n` consumes no input; `%p` is not in C89 and every implementation has it.
        (r#""%n""#, vec![("specifier", "%"), ("conversion", "n")]),
        (r#""%p""#, vec![("specifier", "%"), ("conversion", "p")]),
        // **No flags and no precision in this grammar**: `%-d` is not a conversion, and the scanner must not read
        // `-` as one — `scanf("%-d")` is a broken format string and the `%` is left alone.
        (r#""%-d""#, vec![]),
        (r#""%.2f""#, vec![]),
        (
            r#""%3d %[a-z] %n""#,
            vec![
                ("specifier", "%3"),
                ("conversion", "d"),
                ("specifier", "%"),
                ("conversion", "[a-z]"),
                ("specifier", "%"),
                ("conversion", "n"),
            ],
        ),
    ] {
        assert_eq!(pieces(literal, FormatKind::Scanf), expected, "{literal}");
    }
}

/// **The C++ literal itself is escaped before the format string is read**, and a raw string is not.
///
/// `"\n%d"` is a newline and a conversion; `"\\{d}"` is one backslash and no field; `R"(\{d})"` is a backslash and
/// **a field**, because a raw string has no escapes and the brace is really there.
#[test]
fn backslashes_are_read_the_way_the_language_reads_them() {
    assert_eq!(
        pieces(r#""\n%d""#, FormatKind::Printf),
        vec![("specifier", "%"), ("conversion", "d")],
        "a newline is not a conversion's start"
    );
    assert_eq!(
        pieces(r#""\{""#, FormatKind::Std),
        vec![],
        "`\\{{` is a brace the format string never sees"
    );
    assert_eq!(
        pieces(r#""%%\n""#, FormatKind::Printf),
        vec![("escape", "%%")],
        "the escape is painted and the newline after it is not part of it"
    );

    // **A raw string**: `R"(…)"` has no escapes at all, so the backslash is a character.
    assert_eq!(
        pieces(r#"R"(\{d})""#, FormatKind::Std),
        vec![("specifier", "{"), ("argument", "d"), ("specifier", "}")],
        "in a raw string the brace is real"
    );
    assert_eq!(
        pieces(r#"R"xy({{)xy""#, FormatKind::Std),
        vec![("escape", "{{")],
        "a raw string's delimiter is not content"
    );
    assert_eq!(
        pieces(r#"R"(%d)""#, FormatKind::Printf),
        vec![("specifier", "%"), ("conversion", "d")],
        "and a conversion is still a conversion"
    );
}

/// **A prefix is not content**: `u8"…"`, `L"…"`, `u"…"`, `U"…"` and their raw forms.
#[test]
fn a_literal_prefix_is_skipped() {
    for prefix in ["", "u8", "L", "u", "U"] {
        let literal = format!(r#"{prefix}"%d""#);
        assert_eq!(
            pieces(&literal, FormatKind::Printf),
            vec![("specifier", "%"), ("conversion", "d")],
            "{literal}"
        );
        let literal = format!(r#"{prefix}R"(%d)""#);
        assert_eq!(
            pieces(&literal, FormatKind::Printf),
            vec![("specifier", "%"), ("conversion", "d")],
            "{literal}"
        );
    }
}

/// **Every piece is inside the literal, in order, and they do not overlap** — the invariant the client's encoding
/// cannot recover from if it is broken, asserted over every case above at once.
#[test]
fn every_piece_is_in_order_and_inside_the_literal() {
    let literals = [
        (r#""值 = {:.3f}，宽度 {:{}}  {{ok}}""#, FormatKind::Std),
        (r#""{0:{1}.{2}f}""#, FormatKind::Std),
        (r#""{""#, FormatKind::Std),
        (r#""{0x}""#, FormatKind::Std),
        (r#""%-8.2f|%*d|%1$s|%%""#, FormatKind::Printf),
        (r#""%y %d""#, FormatKind::Printf),
        (r#""%3d %[a-z] %n""#, FormatKind::Scanf),
        (r#""%[]]""#, FormatKind::Scanf),
    ];

    for (literal, kind) in literals {
        let mut found = Vec::new();
        pieces_in(literal, kind, &mut found);
        let mut previous_end = 0usize;
        for piece in &found {
            assert!(
                piece.start >= previous_end,
                "{literal}: {:?} starts at {} before the previous piece ends at {previous_end}",
                piece.kind,
                piece.start
            );
            assert!(
                piece.end <= literal.len(),
                "{literal}: {:?} ends at {} past the literal's {} bytes",
                piece.kind,
                piece.end,
                literal.len()
            );
            assert!(piece.start < piece.end, "{literal}: an empty piece");
            assert!(
                literal.is_char_boundary(piece.start) && literal.is_char_boundary(piece.end),
                "{literal}: a piece that splits a character"
            );
            previous_end = piece.end;
        }
    }
}

/// **A piece carries the bytes it covers**, written out as `(start, end, kind)` — the offsets the colour is painted
/// with, and the numbers a client's encoding turns into a line and a column.
///
/// `format!("{}:{}", …)` above would still pass if two neighbouring pieces were merged and split the other way; this
/// would not, and these are the two literals the doc's §1 is written about.
#[test]
fn a_piece_carries_the_bytes_it_covers() {
    let mut found = Vec::new();
    pieces_in(r#""%-8.2f|%*d""#, FormatKind::Printf, &mut found);
    let ranges: Vec<(usize, usize, &str)> = found
        .iter()
        .map(|piece| {
            (
                piece.start,
                piece.end,
                match piece.kind {
                    PieceKind::Specifier => "specifier",
                    PieceKind::Argument => "argument",
                    PieceKind::Conversion => "conversion",
                    PieceKind::Escape => "escape",
                    PieceKind::DynamicWidth => "dynamic",
                },
            )
        })
        .collect();
    assert_eq!(
        ranges,
        vec![
            (1, 6, "specifier"),    // `%-8.2`, after the opening quote
            (6, 7, "conversion"),   // `f`
            (8, 9, "specifier"), // the second conversion's `%` — the `|` at 7 is not part of any piece
            (9, 10, "argument"), // `*`
            (10, 11, "conversion"), // `d`
        ]
    );

    let mut found = Vec::new();
    pieces_in(r#""{:{}}""#, FormatKind::Std, &mut found);
    let ranges: Vec<(usize, usize)> = found.iter().map(|p| (p.start, p.end)).collect();
    assert_eq!(
        ranges,
        vec![(1, 3), (3, 5), (5, 6)],
        "`{{:` then the nested `{{}}` then the closing brace"
    );
}

/// **A token that is not a literal at all** is answered with nothing rather than a panic — the scanner is called for
/// every token whose kind says "string", and a lexer's recovery can produce one that does not begin with a quote.
#[test]
fn a_token_that_is_not_a_literal_is_ignored() {
    for text in ["", "abc", "\"", "\"unterminated", "R\"", "R\"(", "u8", "%%"] {
        let mut found = Vec::new();
        pieces_in(text, FormatKind::Std, &mut found);
        pieces_in(text, FormatKind::Printf, &mut found);
        pieces_in(text, FormatKind::Scanf, &mut found);
        assert!(
            found.iter().all(|piece| piece.end <= text.len()),
            "{text:?}: a piece past the end of the token"
        );
    }
}

/// **What a piece means**, which is what the hover prints — the same module answers both, so the explanation of
/// `%-8.2` cannot disagree with the colouring of `%-8.2`.
///
/// The pairs are `(label, value)`: the labels are the grammar's words, the values the reading.
#[test]
fn a_piece_explains_itself() {
    use cpp_code_analysis::format::describe;

    /// The explanation of the piece covering `needle`, as `label: value` strings.
    fn of(literal: &str, needle: &str, kind: FormatKind) -> Vec<String> {
        let mut found = Vec::new();
        pieces_in(literal, kind, &mut found);
        let at = literal.find(needle).expect("the fixture writes it");
        let piece = found
            .iter()
            .find(|piece| piece.start <= at && at < piece.end)
            .unwrap_or_else(|| panic!("{needle:?} is inside a piece of {literal}"));
        describe(literal, kind, piece)
            .into_iter()
            .map(|(label, value)| format!("{label}: {value}"))
            .collect()
    }

    // A printf conversion, letter by letter.
    assert_eq!(
        of(r#""%.2f""#, "f", FormatKind::Printf),
        vec!["type: fixed-point decimal — a `double` written as `-.--`"],
        "the conversion the reader looks up"
    );
    assert_eq!(
        of(r#""%zu""#, "u", FormatKind::Printf),
        vec!["type: unsigned decimal"],
        "…and what the letter after the length modifier means"
    );
    // The flags, width, precision and length, each read separately.
    assert_eq!(
        of(r#""%-8.2f""#, "-8.2", FormatKind::Printf),
        vec!["flags: left-aligned", "width: 8", "precision: 2"]
    );
    assert_eq!(
        of(r#""%#llx""#, "#ll", FormatKind::Printf),
        vec![
            "flags: an alternative form (`0x`, a leading `0`)",
            "length: a `long long`"
        ]
    );
    // A `%` with nothing else says what it is rather than nothing at all.
    assert_eq!(
        of(r#""%d""#, "%", FormatKind::Printf),
        vec!["conversion: introduced by `%`; the letter after any flags and width says which"]
    );
    // The two things that take an argument without being a conversion.
    assert_eq!(
        of(r#""%*d""#, "*", FormatKind::Printf),
        vec!["argument: the width or precision comes from an argument"]
    );
    assert_eq!(
        of(r#""%1$s""#, "1$", FormatKind::Printf),
        vec!["argument: number 1, counted from 1 — and this conversion does not advance the count"]
    );
    // scanf's scanset, and its suppression.
    assert_eq!(
        of(r#""%[a-z]""#, "[a-z]", FormatKind::Scanf),
        vec!["set: one or more characters from `a-z`"]
    );
    assert_eq!(
        of(r#""%[^,]\n""#, "[^,]", FormatKind::Scanf),
        vec!["set: one or more characters **not** in `,`"]
    );
    assert_eq!(
        of(r#""%*d""#, "*", FormatKind::Scanf),
        vec!["argument: assigns nothing: the conversion is read and discarded"]
    );
    // std::format: the fields and its own presentation types.
    assert_eq!(
        of(r#""{:>10}""#, ">10", FormatKind::Std),
        vec!["align: right", "width: 10"]
    );
    assert_eq!(
        of(r#""{:*<8}""#, "*<8", FormatKind::Std),
        vec!["fill: `*`", "align: left", "width: 8"]
    );
    assert_eq!(
        of(r#""{:.3f}""#, "f", FormatKind::Std),
        vec!["type: fixed-point decimal"]
    );
    assert_eq!(
        of(r#""{:#x}""#, "#", FormatKind::Std),
        vec!["flags: an alternative form (`0x`, `0b`, a leading `0`)"],
        "a std flag, whose alternative form is not printf's"
    );
    assert_eq!(
        of(r#""{}""#, "{}", FormatKind::Std),
        vec!["field: formatted the way the argument's type defaults to"]
    );
    // **A field being typed still explains itself**: `{` alone is a field whose specification has not been written,
    // and saying "the argument's default formatting" is more useful than an empty popup a keystroke later.
    assert_eq!(
        of(r#""{""#, "{", FormatKind::Std),
        vec!["field: formatted the way the argument's type defaults to"]
    );
}

/// **What may be typed at a position** — the completion list, and the positions it refuses.
#[test]
fn a_conversion_is_offered_where_one_can_go() {
    use cpp_code_analysis::format::completions_at;

    /// The labels offered at the byte of `|` in `literal`.
    fn at(literal: &str, kind: FormatKind) -> Vec<String> {
        let at = literal.find('|').expect("the fixture marks the cursor");
        let without = literal.replace('|', "");
        completions_at(&without, kind, at)
            .into_iter()
            .map(|item| item.label)
            .collect()
    }

    // Right after the `%`, and after the flags/width/precision — the two places a conversion goes.
    assert!(at(r#""%|""#, FormatKind::Printf).contains(&"d".to_string()));
    assert!(at(r#""%-8.2|""#, FormatKind::Printf).contains(&"f".to_string()));
    assert!(at(r#""%5.|""#, FormatKind::Printf).contains(&"x".to_string()));
    assert!(at(r#""%ll|""#, FormatKind::Printf).contains(&"u".to_string()));
    assert!(
        !at(r#""%*|""#, FormatKind::Scanf).is_empty(),
        "after a suppression a conversion goes"
    );
    assert!(
        at(r#""%[a-z]|""#, FormatKind::Scanf).is_empty(),
        "after a whole scanset there is nothing left to say"
    );
    assert!(at(r#""%|""#, FormatKind::Scanf).contains(&"s".to_string()));
    assert!(at(r#""{:|}""#, FormatKind::Std).contains(&"d".to_string()));
    assert!(
        at(r#""{:>10|""#, FormatKind::Std).contains(&"f".to_string()),
        "after a width the type still goes"
    );

    // **And the positions it refuses**, each because the grammar says nothing may go there.
    assert!(
        at(r#""%%|""#, FormatKind::Printf).is_empty(),
        "after an escape"
    );
    assert!(
        at(r#""%d|""#, FormatKind::Printf).is_empty(),
        "after a whole conversion"
    );
    assert!(
        at(r#""%-|8d""#, FormatKind::Printf).is_empty(),
        "in the middle of the flags"
    );
    assert_eq!(
        at(r#""{|}""#, FormatKind::Std),
        vec![":", "}"],
        "right after `{{` the field continues with a specification or ends — not with a type, and an argument \\
         index is not something this layer can know"
    );
    assert!(
        at(r#""hello |world""#, FormatKind::Printf).is_empty(),
        "prose"
    );
}

/// **A completion replaces the character it is offered on**, so that completing `x` over `%d` gives `%x`.
#[test]
fn a_completion_replaces_the_spelling_under_the_cursor() {
    use cpp_code_analysis::format::completions_at;

    let literal = r#""%d""#;
    let at = literal.find('d').expect("the fixture writes it");
    let items = completions_at(literal, FormatKind::Printf, at);
    let x = items
        .iter()
        .find(|item| item.label == "x")
        .expect("`x` is a conversion");
    assert_eq!(
        (&literal[x.start..x.end], x.end - x.start),
        ("d", 1),
        "the edit covers the `d`, so the result is `%x` and not `%dx`"
    );

    // At the position a conversion goes — the empty one after `%` — the edit is empty, so nothing is deleted.
    let literal = r#""%""#;
    let at = literal.find('"').expect("the closing quote");
    let items = completions_at(literal, FormatKind::Printf, at);
    assert!(
        items.iter().all(|item| item.start == item.end),
        "an insertion, not a replacement"
    );
}
