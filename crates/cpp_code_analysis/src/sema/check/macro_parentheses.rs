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
        for (name, start, end) in bodies {
            // **The body as the *replacement list*, with the line splices taken out.** A multi-line `#define` writes a
            // backslash at the end of every line but the last, and the first token of the body can be that backslash:
            // `#define CORPUS_DEFINE_POINT(name, x, y) \` followed by `static const point name = …` is a *declaration*,
            // and a body judge that sees `\` where it expected `static` reported it. `is_dropped_in_a_body` drops
            // splices when the definition is parsed, so this is the same rule applied to the text a reader sees.
            let body = context
                .written
                .get(start..end)
                .unwrap_or("");
            // **Comments are removed before the body is judged, and that is a measurement.** `kvi_resize_full`
            // writes its whole replacement list as a call, with the arguments grouped, after four lines of
            // `/* … */` comments about capacity — and the `*` of `/*` was read as a multiplication at depth zero:
            // "`kvi_resize_full`'s replacement list is not parenthesised" on a macro whose body is
            // `kvi_resize(v, (v).capacity << 1)`. `is_a_call` failed for the same reason (the body started with a
            // comment rather than a name). Measured on neovim's `src/klib/kvec.h:219`.
            let body = the_code_of(body);
            // A body that is one token, a braced initialiser, a call, or already parenthesised is not this rule's shape.
            if body.is_empty()
                // **The whole body is one group**, not merely a body that starts with one: `(v).capacity << 1`
                // begins with `(` and its `<<` is still outside the group — measured as a silent macro that is
                // exactly the defect this rule exists for.
                || is_one_group(&body)
                || body.starts_with('{')
                || body.starts_with("do") && !has_a_loose_operator(&body)
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
            if starts_a_declaration(&body) || body.contains("##") || is_a_call(&body) {
                continue;
            }
            if !has_a_loose_operator(&body) {
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
/// Is the whole body a single parenthesised group — `((a) > (b) ? (a) : (b))`?
///
/// A body that starts with `(` and closes before its last token is **not** one: `(v).capacity << 1` is a postfix
/// expression with a shift after it, and treating it as grouped is how the rule went silent about a real defect.
fn is_one_group(body: &str) -> bool {
    if !body.starts_with('(') {
        return false;
    }
    let mut depth = 0i32;
    for (at, character) in body.char_indices() {
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

/// **A replacement list as code**: line splices gone, comments gone.
///
/// Both are text the preprocessor removes before a compiler sees the body, and both were read as operators by the
/// scanners below — a splice is a `\\` at the end of a line, and the `*` of `/*` is a multiplication if nothing
/// knows it is inside a comment. The strings themselves are kept: a `#define` that builds a format string is still
/// a body with no loose operator, and removing the quotes would put its `%d` at depth zero.
fn the_code_of(body: &str) -> String {
    let mut code = String::with_capacity(body.len());
    let mut characters = body.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            // A line splice: the backslash and the newline it escapes.
            '\\' if characters.peek() == Some(&'\n') => {
                characters.next();
            }
            '\\' if characters.peek() == Some(&'\r') => {
                characters.next();
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
            }
            '/' if characters.peek() == Some(&'*') => {
                characters.next();
                let mut previous = '\0';
                for inside in characters.by_ref() {
                    if previous == '*' && inside == '/' {
                        break;
                    }
                    previous = inside;
                }
                code.push(' ');
            }
            '/' if characters.peek() == Some(&'/') => {
                characters.next();
                while let Some(inside) = characters.peek() {
                    if *inside == '\n' {
                        break;
                    }
                    characters.next();
                }
                code.push(' ');
            }
            // A string or a character literal is copied as it is: a `//` inside it is not a comment.
            quote @ ('"' | '\'') => {
                code.push(quote);
                while let Some(inside) = characters.next() {
                    code.push(inside);
                    if inside == '\\' {
                        if let Some(escaped) = characters.next() {
                            code.push(escaped);
                        }
                    } else if inside == quote {
                        break;
                    }
                }
            }
            other => code.push(other),
        }
    }
    code.trim().to_string()
}

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
        // **A type fragment is not an expression.** `typename ::asio::detail::async_result_helper<ct, sig>::return_type`
        // is what asio's `ASIO_INITFN_RESULT_TYPE` expands to, and a caller's expression is never spliced into a type.
        // Measured on `async_result.hpp`: seven macros of this shape were reported as "a caller's expression is
        // spliced in without its precedence".
        "typename",
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
    // **A template-id is not a comparison.** `::asio::completion_handler_for<sig>` has a `<` and a `>`, and the
    // scanner read the first as a binary operator — so every macro that expands to a *type* was a finding. Measured on
    // asio's `async_result.hpp`: three macros naming a template-id and two expanding to a template parameter list, all
    // reported as "a caller's expression is spliced in without its precedence", when no caller's expression ever
    // reaches a type.
    let mut skip_until = 0usize;
    for (index, character) in body.char_indices() {
        if index < skip_until {
            continue;
        }
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '+' | '-' | '*' | '/' | '%' | '&' | '|' | '^' | '?' | '<' | '>' | '=' | '!'
                if depth == 0 =>
            {
                if character == '<'
                    && let Some(end) = template_id_end(body, index)
                {
                    skip_until = end;
                    continue;
                }
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

/// **The end of a template-id that starts at `open`**, or `None` when the `<` there is a comparison.
///
/// This is the grammar's own disambiguation, as much of it as text can decide: a template argument list holds names,
/// qualified names, numbers, template-ids of its own, `*`, `&`, `,` and spaces — and it **closes**. A comparison's
/// right side runs into `;`, `)`, an operator or the end of the list, and none of those can appear inside a list.
///
/// The one shape it cannot tell apart is `x < y > z` written with nothing else around it, which is the same vexing
/// parse a compiler resolves from the declaration; the answer here is the quiet one — a macro body of that shape is
/// not reported, because "not reported" is the mistake this rule is allowed to make.
fn template_id_end(body: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (at, character) in body[open..].char_indices() {
        match character {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    // An empty list is not a template-id (`a < > b`), and neither is one that starts at the `>`.
                    return (at > 1).then_some(open + at + 1);
                }
            }
            _ if depth > 0 => {
                let allowed = character.is_alphanumeric()
                    || matches!(
                        character,
                        '_' | ':'
                            | ','
                            | '.'
                            | '*'
                            | '&'
                            | '('
                            | ')'
                            | '['
                            | ']'
                            | '-'
                            | '\''
                            | ' '
                    );
                if !allowed {
                    return None;
                }
            }
            _ => return None,
        }
    }
    None
}
