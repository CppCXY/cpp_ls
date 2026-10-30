//! **`unused`**: a local declaration nothing reads.
//!
//! # What is reported, and what is deliberately not
//!
//! ```text
//!   reported      a local variable declared in a **source** file whose name is written exactly once
//!                 in the whole file — the declaration itself
//!   not reported  a name beginning with `_`, which is how a reader says "I know" ([`is_deliberately_unused`])
//!   not reported  anything declared in a header, where "nothing in this file uses it" says nothing at all
//!   not reported  a parameter, a member, a global, a function, a type
//! ```
//!
//! The last line is a decision about **noise**, and the corpus is what decides it: this analysis reports nothing on
//! a header it read correctly, and an unused parameter is the most common deliberate thing in C++ — a virtual
//! function's, a callback's, a `main`'s `argv`. A rule that fires on those is a rule the reader turns off, and the
//! cost of that is not this finding, it is every other finding it stops them from seeing.
//!
//! # Why the whole file rather than the enclosing scope
//!
//! A local is visible only inside its block, so the honest question is "is it read inside the block it is declared
//! in". This asks the **file** instead, which is strictly more permissive: a spelling written anywhere else — in
//! another function, in a macro body, in a string that the lexer does not treat as an identifier — makes this
//! check stay quiet. That is the direction to be wrong in. Measured on MSVC's headers, the narrower rule reported
//! `__pragma`-heavy declarations whose uses are written inside macros, and the file-wide question does not.

use std::collections::HashMap;

use cpp_parser::CppTokenKind;

use super::{Check, CheckContext, DiagnosticCode};

/// The headers whose "unused" says nothing: a file included by others is used by *them*.
///
/// Decided by extension rather than by asking the index whether anything includes this file, because a check may
/// not ask — see [`Checks`](super::CheckContext)'s note on why it is handed a summary and not a session. The
/// extension is the same evidence the reader has.
fn is_a_header(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("h" | "hh" | "hpp" | "hxx" | "inl" | "ipp" | "tpp" | "inc")
    )
}

/// Is the declaration at t (a **rendering** offset) a **member of a type** rather than a local variable?
///
/// The innermost scope decides, and only a class scope counts — see the call site for why the chain below the
/// innermost one is not the question.
fn declares_a_member(context: &CheckContext<'_>, at: usize) -> bool {
    let scopes = context.model.scopes();
    scopes
        .scope_at(at)
        .and_then(|scope| scopes.scope(scope))
        .is_some_and(|data| data.kind == crate::ScopeKind::Class)
}
fn is_deliberately_unused(name: &str) -> bool {
    name.starts_with('_')
}

/// **Does the declaration whose name is at `at` carry a `[[maybe_unused]]`?**
///
/// The attribute is written **before** the name — `[[maybe_unused]] int local = 3;` — and it is found by scanning the
/// declaration's own prefix, which is the text between the previous statement boundary and the name. The fact's range
/// starts at the name rather than at the declaration's first token, so the window is not "what precedes `at`": the type
/// sits between the attribute and the name.
///
/// The boundary set is what keeps a *previous* declaration's attribute out of the answer: `[[maybe_unused]] int a = 1;`
/// followed by `int b = 2;` must not excuse `b`, and the `;` between them is what stops it. `,` and `(` are boundaries
/// too, because a declaration can follow both (`int a = 1, b;`, `for (int i = 0; …`).
///
/// `[[maybe_unused, other]]` and `[[other]] [[maybe_unused]]` are both written, and both contain the word, so the test is
/// a search rather than a parse.
fn is_marked_maybe_unused(source: &str, at: usize) -> bool {
    let before = source.get(..at.min(source.len())).unwrap_or_default();
    let from = before
        .rfind([';', '{', '}', '(', ','])
        .map_or(0, |boundary| boundary + 1);
    before[from..].contains("maybe_unused")
}

/// **Does the declared type say, by itself, that the variable cannot be a side effect?**
///
/// A local whose type has a destructor is not "unused": `Guard guard(counter);` exists for its constructor and its
/// destructor, and `std::shared_lock reader(mutex);` for its unlock. This layer cannot ask whether a type has a
/// destructor — the answer is in the index, and a check may not ask — so it asks the narrower question its own evidence
/// answers: **is the type one the language itself says has no destructor**? Only a builtin is. `int local = 3;` with
/// nothing read is a fact about the code; `Guard guard(counter);` with nothing read is the code doing its job.
///
/// The declared type is the evidence and the only evidence: `auto` says nothing about a destructor either, so a local
/// whose type the fact did not record is left alone.
fn its_type_cannot_have_a_destructor(fact: &crate::DeclFact) -> bool {
    fact.type_of
        .as_deref()
        .is_some_and(super::undefined_name::is_a_builtin_spelling)
}
/// A name that is not worth asking about: a keyword-shaped spelling, or nothing at all.
fn is_a_candidate(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_' || first == '$')
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '$'
        })
}

/// **`unused`** — see the module documentation for what it reports and what it stays quiet about.
pub struct Unused;

impl Check for Unused {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::Unused];

    fn check(context: &mut CheckContext<'_>) {
        if is_a_header(context.path) {
            return;
        }

        // **One pass over the file's identifiers, counted by spelling.** What the check needs is "how many times
        // is this word written", and a count answers it for every declaration at once — where asking per
        // declaration would walk the token stream once per name, which is quadratic in a file the size of a real
        // header. Measured on MSVC's `<vector>`: 20 247 nodes, and the per-declaration form was the whole cost of
        // the layer.
        let mut written: HashMap<&str, usize> = HashMap::new();
        // **This file's own tokens**, not the rendering's: a token inside an inlined header is another file's
        // spelling, and counting it would make this rule answer about a name in `<string>`. See `CheckContext::own`.
        for token in context.own_tokens() {
            if token.kind != CppTokenKind::Identifier {
                continue;
            }
            let text = context
                .source
                .get(token.range.start_offset..token.range.end_offset())
                .unwrap_or_default()
                .trim();
            *written.entry(text).or_default() += 1;
        }

        // **The function bodies of this file**, which is where a local can be: the rule's own scope, checked in the
        // tree rather than trusted from `DeclFact::local`. Measured, and it is why this is here: on MSVC's `<string>`
        // the summary records `sprintf_s` as an **`Variable`** marked `local` — a macro ate its parameter list, so
        // `kind=Variable type=Some("_CSTD")`, and `xstring`'s `addressof` came out with `type=Some("_STD(_STD,
        // _Right)")`, the parameters inside the type. Four such facts were reported as "declared here and never used"
        // on a corpus that is known good, which is the one measurement this layer has to pass.
        let bodies: Vec<cpp_parser::CppSyntaxNode> = super::function_definitions(context)
            .into_iter()
            .filter_map(|function| super::body_of(&function))
            .collect();

        for fact in &context.summary.declarations {
            // **A local variable inside a function body**, which is the only shape this rule claims — see the module
            // documentation for the reasoning behind each condition.
            if !fact.local || fact.kind != crate::DeclKind::Variable {
                continue;
            }
            // **The comparison happens in the rendering's coordinates**, because the two sides come from different
            // places: `fact.range` is the file's — see `CheckContext::written` — while a body's range is the tree's,
            // which is the rendering. Asking without the crossing is how this guard first went wrong: a fixture whose
            // comment the cook removes shifted every offset by 28 bytes, and a local inside `main` read as outside it.
            let Some(at) =
                context
                    .view
                    .reading_offset_of(crate::file::coordinates::FileOffset::new(
                        fact.range.start_offset,
                    ))
            else {
                continue;
            };
            let inside_a_body = bodies.iter().any(|body| {
                usize::from(body.text_range().start()) < at.get()
                    && at.get() < usize::from(body.text_range().end())
            });
            if !inside_a_body {
                continue;
            }
            // **A member of a type is not a local variable**, even when the type is written inside a body:
            // `sizeof(union { int i; double d; })` declares a union and its two members inline, and both members were
            // reported "declared here and never used" — a type in an unevaluated context has no reader to use it.
            // Measured on a nine-file C corpus: two findings in `aggregates.c`, and they appeared the moment that file
            // parsed. The *innermost* scope is what decides: a member-function local sits in a block scope, whose chain
            // merely passes through the class.
            if declares_a_member(context, at.get()) {
                continue;
            }

            if is_deliberately_unused(&fact.name) || !is_a_candidate(&fact.name) {
                continue;
            }

            // **`[[maybe_unused]]` is the reader saying "I know"**, and it is honoured here for the same reason a
            // leading `_` is: a rule that reports a variable the reader has already marked is a rule that teaches them
            // to delete the mark. Measured on `attributes.cpp` of the C++ corpus, whose only `unused` finding was a
            // variable written `[[maybe_unused]]` three lines above it.
            if is_marked_maybe_unused(context.source, at.get()) {
                continue;
            }

            // **A type that can have a destructor is not this rule's business** — see
            // [`its_type_cannot_have_a_destructor`]. `Guard guard(counter);` and `std::shared_lock reader(mutex);`
            // were the two findings on `exceptions.cpp` and `concurrency.cpp` of the C++ corpus, and both are objects
            // whose whole use is the side effect of their lifetime.
            if !its_type_cannot_have_a_destructor(fact) {
                continue;
            }

            // **Written once means the declaration is the only place.** The count is over the *whole file*, so a
            // use in another function, in a macro body or in a string keeps the count above one and this stays
            // quiet — which is the direction to be wrong in.
            if written.get(fact.name.as_str()).copied().unwrap_or_default() > 1 {
                continue;
            }

            // **The name's range, not the declaration's.**
            //
            // This reported `fact.range`, which covers the whole declaration — so `std::string full2;` underlined
            // from `std` to the `;`. The reader's question is "which name is unused", and the answer is one
            // identifier. `DeclFact` carries both, and [`crate::sema::symbol::Binding`] says why they are separate:
            // *"Collapsing them is the difference between a rename that edits one identifier and one that deletes the
            // whole declaration."* This check collapsed them.
            //
            // The name is also the part that stays right when the declaration is reached through a macro: a span a
            // macro produced reports the invocation, so the name's own range is the one a reader can act on.
            context.add_in_the_file(
                DiagnosticCode::Unused,
                fact.name_range,
                fact.name.as_str(),
                // **A translated message**: the key is the English template, `%{…}` is rust-i18n's placeholder, and
                // a language that has not translated it answers with the key — which *is* the English sentence.
                t!(
                    "`%{name}` is declared here and never used",
                    name = fact.name
                ),
            );
        }
    }
}
