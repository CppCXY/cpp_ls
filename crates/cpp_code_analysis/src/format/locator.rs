//! **Which literal is a format string, and in which grammar** — the locator, and the table of known functions.
//!
//! Walks a file's **token stream**, not its syntax tree: `std::println("…", x);` is a *declaration* to this parser
//! (the most-vexing-parse reading), so the `CallExpr` a tree walk would look for does not exist for the most ordinary
//! C++23 print there is. The format string is there either way.

use cpp_parser::{CppTokenData, CppTokenKind};

use super::{FormatConfig, FormatKind, FormatLiteral, pieces_in};
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
        // **A literal with no pieces is still a format string**, which is not obvious and was a defect: `"%5."` — a
        // conversion whose letter has not been typed yet — produces no piece, because the scanner refuses to paint a
        // `%` that is not a conversion. Dropping the literal here made the *completion* at that exact position answer
        // nothing at all: it could not find a literal to be inside. The colouring pushes no token for an empty piece
        // list and the popup has nothing to describe, so keeping the literal costs those two nothing and is what
        // makes "complete the conversion I am typing" possible.
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
    while let Some(before_this) = previous_significant(tokens, first) {
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
/// name this table guessed would paint a string a project meant as data. A project's own functions come in through
/// `[format.functions]`, which is where a *person* says what a checker cannot know — and which can say where the
/// format string is, which this table's `Position` also does.
///
/// **The `fmt` library is here too, and it has two syntaxes.** `fmt::format` and `fmt::print` take `{…}`, the same
/// grammar as `std::format` — that is what `fmt` became the standard from — and `fmt::printf` / `fmt::fprintf` /
/// `fmt::sprintf` take `%…`, the C library's. The table does not carry the scope: a bare name is looked up once and
/// the scope check that follows says which libraries may claim it (see [`is_fmt`]), which is why `fmt::printf` needs
/// no entry of its own. `fmt::runtime(…)` is a format string the *program* computes, so it is not read: there is no
/// literal to read.
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

    // **The project's own functions**, which are the one thing a person can say that the analysis cannot: the
    // grammar they take, and which argument the format string is. A qualified spelling is matched as written; a bare
    // one against what it resolves to, so `using mylib::logf;` works and a local `logf` that is something else does
    // not. The entry's grammar decides the scanner, and its index decides which argument — which is the half a list
    // of names could never express (`mylib::trace(level, fmt, …)`).
    //
    // **Asked before the table**, because a project that lists a name saying so is more specific than a guess by
    // spelling: a wrapper named `printf` with a different grammar is read as the project says.
    let mine = config.functions.iter().find(|entry| {
        entry.name == written
            || (!call.qualified
                && resolved_name(qualified_name).is_some_and(|name| name == entry.name))
    });
    if let Some(entry) = mine
        && position_matches(Position::At(entry.argument), call)
    {
        return Some(entry.syntax);
    }

    let &(_, kind, position) = THE_TABLE.iter().find(|(name, _, _)| *name == bare)?;

    // **One name whose position depends on the library that declares it.** `sprintf` is the C library's
    // `sprintf(buffer, format, …)` — the format string is the second argument — and `fmt::sprintf(format, …)` is
    // `fmt`'s, which takes it first. The table cannot hold both under one spelling, so the scope decides; every
    // other `fmt::` name either agrees with its C counterpart (`fmt::printf` is `%` at argument 0, `fmt::fprintf`
    // takes the file first) or is `fmt`'s alone.
    let position = match (is_fmt(scope), bare) {
        (true, "sprintf") => Position::At(0),
        _ => position,
    };

    // **Which scope may claim a name.** `std` for everything; `fmt` (and its inline `fmt::v11` version namespace)
    // for the names `fmt` shares with `std`; nothing else — a project's own function is read only when
    // `[format.functions]` names it, above.
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
