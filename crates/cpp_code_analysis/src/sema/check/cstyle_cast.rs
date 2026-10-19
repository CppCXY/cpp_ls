//! `cppcoreguidelines-pro-type-cstyle-cast`, from clang-tidy: **a C-style cast hides which conversion it is.**
//!
//! ```cpp
//! double d = (double)n;             // this check says: static_cast<double>(n)
//! char* p = (char*)const_pointer;   // …and this one hides a const_cast that a compiler would refuse if written out
//! ```
//!
//! The rule's real content is the second line: a C-style cast tries `const_cast`, `static_cast` and `reinterpret_cast` in
//! that order and takes the first that compiles, so it can remove `const` without saying so. Naming the cast is what makes
//! the conversion reviewable.
//!
//! # How it is found
//!
//! Over a parenthesised expression whose **inside looks like a type** — an identifier with qualifiers, `::`, `*` or `&`, and
//! no operator — that is **followed by an operand**. That second half is what separates `(int)n` from `(a + b) * c`, and it
//! is the whole reason this is not a regex over the source: a parenthesised expression is only a cast when something is
//! being cast.

use super::{Check, CheckContext, DiagnosticCode};

pub struct CStyleCast;

impl Check for CStyleCast {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::CStyleCast];

    fn check(context: &mut CheckContext<'_>) {
        // **Not a rule in C.** A C program has no `static_cast`, no `const_cast` and no `reinterpret_cast` — the C-style cast
        // is the only cast the language has, and telling a reader to name the one they meant would be telling them to write
        // C++ in a C file. `is_c` is `None`-tolerant: a configuration that enables both families is not "C", so an
        // unconfigured project keeps the findings it had.
        if context.is_c() {
            return;
        }
        // **A scan of the text**, because the parenthesis the tree holds for a cast also holds the operand and its node kind
        // does not say "cast". The question is a spelling's shape: `(` a type `)` then something that is not an operator.
        // **The file, not the reading.** A cast written in a `#define` is written *there*, and that is where a reader
        // edits it; `add_in_the_file` is the method for a range that already is a file offset. The reading would answer
        // "where the macro was invoked", which is the right place for a finding about the *program* and the wrong one for
        // a finding about a spelling.
        let source = context.written;
        let protected = context.comments_and_literals();
        let mut at = 0usize;
        while let Some(open) = source[at..].find('(') {
            let open = at + open;
            let Some(close) = source[open..].find(')').map(|close| open + close) else {
                break;
            };
            at = open + 1;
            let inner = source[open + 1..close].trim();
            if !looks_like_a_type(inner) {
                continue;
            }
            if context.in_a_comment_or_literal(open, &protected) {
                continue;
            }
            // **A preprocessor condition is not a cast.** `#if (_MSC_VER)` and `#if (ASIO_USE_… )` are the shapes the first
            // census run reported 813 times over 46 files, and every one of them was this: an identifier in parentheses on
            // a directive line, which is a condition the preprocessor evaluates and not something being cast.
            let line_start = source[..open]
                .rfind('\n')
                .map(|newline| newline + 1)
                .unwrap_or(0);
            if source[line_start..open].trim_start().starts_with('#') {
                continue;
            }
            // **A template argument is not a cast.** The census's remaining examples — `(nullptr_t)`, `(Executor)`,
            // `(OtherAnyExecutor)` — were all in a template argument list, where a parenthesised name is an argument and not
            // something being converted. The tell is the line: an unmatched `<` before the `(` means this is inside an
            // angle-bracket list, and `f((int)x)` on a line with no `<` is still a cast.
            let line_start = source[..open]
                .rfind('\n')
                .map(|newline| newline + 1)
                .unwrap_or(0);
            let line = &source[line_start..open];
            if line.matches('<').count() > line.matches('>').count() {
                continue;
            } // **A parameter list is not a cast.** `void (const any_io_executor& e)` inside a declaration — the shape the
            // census reported 376 times — is a *type* with a *name* after it, which is what a parameter is and what a cast's
            // parenthesised type never is. The last word being an identifier that is not a type keyword is the evidence.
            if is_a_parameter_list(inner) {
                continue;
            }
            // …and the operand has to be on the same line: `#if (X)\n` is not `(X)` applied to whatever follows.
            if source[close + 1..]
                .chars()
                .take_while(|character| character.is_whitespace())
                .any(|character| character == '\n')
            {
                continue;
            }
            // What follows the `)`, skipping whitespace — an operand starts a cast, and an operator does not.
            let after = source[close + 1..].trim_start();
            let Some(next) = after.chars().next() else {
                continue;
            };
            if !(next.is_alphanumeric() || matches!(next, '_' | '(' | '*' | '&' | '!' | '"' | '\''))
            {
                continue;
            }
            // …and a *statement* keyword in the parentheses is not a type.
            if matches!(
                inner,
                "if" | "for"
                    | "while"
                    | "switch"
                    | "catch"
                    | "return"
                    | "sizeof"
                    | "decltype"
                    | "alignof"
                    | "static_assert"
                    | "assert"
            ) {
                continue;
            }
            context.add_in_the_file(
                DiagnosticCode::CStyleCast,
                cpp_parser::SourceRange::new(open, close + 1 - open),
                format!("({inner})"),
                format!(
                    "`({inner})` is a C-style cast: it tries `const_cast`, `static_cast` and `reinterpret_cast` in that \
                     order and takes the first that compiles, so it can remove `const` without saying so — name the one \
                     you meant"
                ),
            );
        }
    }
}
/// Does this look like a type written inside parentheses? An identifier with qualifiers, `::`, pointers and references —
/// and **no operator**, which is what tells `(int)` from `(a + b)`.
fn looks_like_a_type(written: &str) -> bool {
    !written.is_empty()
        && written
            .chars()
            .next()
            .is_some_and(|first| first.is_alphabetic() || first == '_' || first == ':')
        && written.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '_' | ':' | '*' | '&' | ' ')
        })
        && !written.contains("  ")
}

/// Is this written parenthesis a **parameter list** — `const any_io_executor& e`, `int n`, `Widget* w` — rather than a
/// type? The tell is a trailing identifier that is not a type keyword: a cast's parenthesised spelling has no name after
/// its type, and a parameter has nothing else.
fn is_a_parameter_list(inner: &str) -> bool {
    const TYPE_WORDS: &[&str] = &[
        "const", "volatile", "unsigned", "signed", "long", "short", "int", "float", "double",
        "char", "bool", "void", "auto", "struct", "class", "enum", "union", "typename",
    ];
    let Some(last) = inner.split_whitespace().last() else {
        return false;
    };
    let last = last.trim_start_matches(['&', '*']).trim();
    let is_a_name = !last.is_empty()
        && last
            .chars()
            .next()
            .is_some_and(|first| first.is_alphabetic() || first == '_' || first == ':')
        && last
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '_' | ':'))
        && !TYPE_WORDS.contains(&last);
    // …and there has to be something before it: `(int)` is a type with no name, `(int n)` is a parameter.
    is_a_name && inner.split_whitespace().count() > 1
}
