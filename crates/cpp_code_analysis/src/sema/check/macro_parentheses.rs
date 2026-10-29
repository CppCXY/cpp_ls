//! `bugprone-macro-parentheses`, from clang-tidy: **a macro's replacement list needs its own parentheses.**
//!
//! ```cpp
//! #define TWICE(x) x * 2        // this check says: ((x) * 2)
//! int n = TWICE(1 + 2);         // …because this is `1 + 2 * 2`, which is 5, and the author meant 6
//! ```
//!
//! The rule exists because a macro is a **text substitution**, not a function: whatever expression the caller passes is
//! spliced in without precedence, and the compiler then applies the operators in the order the text happens to have. Every
//! C and C++ style guide says the same thing, which is how one knows the bug is real rather than a taste.
//!
//! # How it is found
//!
//! Over a `#define` whose replacement list is not already parenthesised and contains an operator that binds looser than a
//! postfix expression. A macro with a single token, a braced initialiser, or a call is not this rule's shape, and a
//! replacement list that starts with `(` is already the answer.

use super::{Check, CheckContext, DiagnosticCode};

pub struct MacroParentheses;

impl Check for MacroParentheses {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::MacroParentheses];

    fn check(context: &mut CheckContext<'_>) {
        // **The macro facts, not a scan of the source.** `context.source` is the *reading* — the text after preprocessing,
        // where a macro use is already expanded and the `#define` is gone (measured: a file that begins with
        // `#define TWICE(x) x * 2` reaches a check as `"int f(int n) { return n + 1*2; }"`). The facts carry the
        // replacement list's own range, and `context.written` is the file as its author wrote it.
        let bodies: Vec<(String, usize, usize)> = context
            .summary
            .macros
            .iter()
            .filter_map(|fact| {
                let range = fact.body_range?;
                // The body is sliced again below, where the borrow of `context` is not held; here it only proves the range
                // is inside the file.
                context
                    .written
                    .get(range.start_offset..range.end_offset())?;
                Some((fact.name.clone(), range.start_offset, range.end_offset()))
            })
            .collect();
        if std::env::var_os("CPPLS_TRACE_MACROS").is_some() {
            eprintln!(
                "macros in {}: written {} chars, {} bodies",
                context.path.display(),
                context.written.len(),
                bodies.len()
            );
            for (name, start, end) in &bodies {
                eprintln!(
                    "  {name} @ {start}..{end} = {:?}",
                    context.written.get(*start..*end)
                );
            }
        }
        for (name, start, end) in bodies {
            let body = context.written.get(start..end).unwrap_or("").trim();
            // A body that is one token, a braced initialiser, a call, or already parenthesised is not this rule's shape.
            if body.is_empty()
                || body.starts_with('(')
                || body.starts_with('{')
                || body.starts_with("do") && !has_a_loose_operator(body)
            {
                continue;
            }
            // **Three shapes that are not an expression at all**, measured on a nine-file C corpus where every one of
            // them was a finding:
            //
            //     #define DECLARE(name, v) static int name = (v)   a declaration: its `=` is a declarator's
            //     #define FRAGMENT(name)   COLOUR_##name = 1,      an X-macro fragment: `##` and a trailing `,`
            //     #define CALL(f, v)       f(v)                    a call, whose arguments are already grouped
            //
            // A caller's expression is spliced into the *parenthesised* place in all three, so there is no precedence to
            // get wrong — and a rule that reports them teaches a reader to ignore it on the one shape that matters.
            if starts_a_declaration(body) || body.contains("##") || is_a_call(body) {
                continue;
            }
            if !has_a_loose_operator(body) {
                continue;
            }
            // **`add_in_the_file`, because a `#define` is not in the rendering.** A directive is consumed before the
            // parse, so the rendering has no offset for it: `add` maps a *rendering* range into the file, and handing it
            // a file range put the squiggle wherever that offset happened to land — measured on a nine-file C corpus,
            // five findings on the *uses* of macros whose definitions were elsewhere, with the `%` of a format string
            // inside the body text deciding some of them. In the small case it did not land at all: every finding was
            // dropped as unplaced, so `#define TWICE(x) x * 2` was never reported.
            context.add_in_the_file(
                DiagnosticCode::MacroParentheses,
                cpp_parser::SourceRange::new(start, end - start),
                name.clone(),
                t!(
                    "`%{name}`'s replacement list is not parenthesised, so a caller's expression is spliced in without its precedence — write `#define %{name}(…) (…)`", name = name
                ),
            );
        }
    }
}

/// Does this replacement list **begin a declaration** rather than an expression?
///
/// A body that starts with a storage class, a qualifier or a type is a declaration — `static int name = (v)` — and its
/// `=` belongs to the declarator, so a caller's expression cannot collide with it. The list is the words that can begin
/// one; a body starting with anything else (a name, a literal, a unary operator) is an expression and stays this rule's
/// question.
fn starts_a_declaration(body: &str) -> bool {
    const DECLARATION_WORDS: &[&str] = &[
        "static",
        "extern",
        "const",
        "volatile",
        "inline",
        "register",
        "typedef",
        "using",
        "struct",
        "class",
        "union",
        "enum",
        "namespace",
        "template",
        "auto",
        "void",
        "char",
        "short",
        "int",
        "long",
        "float",
        "double",
        "signed",
        "unsigned",
        "bool",
        "_Bool",
        "size_t",
    ];
    body.split_whitespace()
        .next()
        .is_some_and(|first| DECLARATION_WORDS.contains(&first))
}

/// Is the whole body **one call** — `f(v)`, `fprintf(stderr, fmt, …)`?
///
/// The parenthesised argument list groups whatever the caller passes, so the precedence question this rule asks does
/// not arise. A call with an operator *after* it (`f(x) + 1`) is not this shape and stays a finding.
fn is_a_call(body: &str) -> bool {
    let Some(open) = body.find('(') else {
        return false;
    };
    // The callee is a name, and nothing else: `a > b(c)` is a comparison whose right side happens to be a call.
    let callee = body[..open].trim();
    if callee.is_empty()
        || !callee
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_' || character == ':')
    {
        return false;
    }
    // …and the group runs to the end of the body.
    let mut depth = 0i32;
    for (at, character) in body.char_indices().skip(open) {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return body[at + 1..].trim().is_empty();
                }
            }
            _ => {}
        }
    }
    false
}
/// Does the replacement list contain an operator that binds looser than a primary expression, outside any nested
/// parentheses? A leading `-` or `!` is unary, and `->`, `>>` and `::` are not binary operators.
fn has_a_loose_operator(body: &str) -> bool {
    let mut depth = 0i32;
    for (index, character) in body.char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '+' | '-' | '*' | '/' | '%' | '&' | '|' | '^' | '?' | '<' | '>' | '=' | '!'
                if depth == 0 =>
            {
                let before = body[..index].chars().last();
                let after = body[index + 1..].chars().next();
                let is_unary = before.is_none()
                    || before.is_some_and(|previous| {
                        matches!(
                            previous,
                            '(' | '[' | '{' | ',' | '=' | '+' | '-' | '*' | '/' | '!' | '?' | ':'
                        )
                    });
                let is_a_suffix = matches!(
                    (character, after),
                    ('-', Some('>')) | ('>', Some('>')) | (':', Some(':'))
                );
                if !is_unary && !is_a_suffix {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}
