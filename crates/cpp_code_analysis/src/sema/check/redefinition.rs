//! **`redefinition`**: a name declared twice in one scope.
//!
//! ```cpp
//! void f() {
//!     int count = 1;
//!     int count = 2;   // `count` is already declared here
//! }
//! ```
//!
//! # One scope, and only the kinds that cannot be repeated
//!
//! Two declarations of one name in one scope is a mistake **when the language has no reading for it**: an overload
//! set is exactly the reading for functions, so two `void run();` are two overloads and nothing is reported, while two
//! variables of one name cannot both exist. The kinds this judges are therefore the ones with no second reading — see
//! [`a_kind_that_cannot_be_repeated`] — and the scopes are the ones a reader writes: a function body, a block, a
//! lambda.
//!
//! # Why not a class body, and why not a file
//!
//! ```text
//!   class body    `class A;` then `class A { … };` is a forward declaration and its definition — one entity
//!                 written twice, which the scope tree records twice. Judging that as a redefinition would be a
//!                 false positive on the commonest declaration in C++.
//!   file scope    `extern int value;` then `int value = 0;` is the same entity too, and legal.
//!   a macro       a binding that came from a macro's expansion has a range inside the macro — a finding about it
//!                 would point at code the reader wrote once
//! ```
//!
//! # What it caught
//!
//! `test_cpp/main.cpp` writes `auto x = 123;` on line 22 and `auto x = anotherVec[1];` on line 39, in one function —
//! a program no compiler accepts, and one this analysis reported nothing about until this rule.

use std::collections::HashMap;

use super::{Check, CheckContext, DiagnosticCode};

/// **`redefinition`** — see the module documentation.
pub struct Redefinition;

impl Check for Redefinition {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::Redefinition];

    fn check(context: &mut CheckContext<'_>) {
        // **Collected before they are reported**, because a finding needs `&mut context` while the scan holds it
        // shared — the same split every check in this layer makes.
        let mut repeated: Vec<(cpp_parser::SourceRange, String)> = Vec::new();
        for scope in context.model.scopes().scopes() {
            // Only where a reader writes a declaration and a repeat is always an error — see the module notes on
            // the class body and the file scope.
            if !matches!(
                scope.kind,
                crate::ScopeKind::Function | crate::ScopeKind::Block | crate::ScopeKind::Lambda
            ) {
                continue;
            }

            // The first declaration of each name, so a third one is still one finding per repeat.
            let mut declared: HashMap<&str, cpp_parser::SourceRange> = HashMap::new();
            for binding in &scope.bindings {
                let Some(name) = binding.name.identifier_text() else {
                    continue;
                };
                if !a_kind_that_cannot_be_repeated(binding.kind) {
                    continue;
                }
                // **A call this parser read as a declaration.** `xfree(*matches);` is a call, and the grammar also
                // reads it as a declaration of `matches` — `xfree (*matches)` is the classic ambiguous shape, and only
                // *lookup* resolves it. The scope walk binds `matches` a second time from that reading, and this rule
                // reported it: measured on neovim's `cmdexpand.c:3389` (`xfree(*matches);`, reported as "`matches` is
                // already declared in this scope"). A declaration's first name cannot be a function, so the index
                // decides it — see [`the_declaration_is_really_a_call`].
                if the_declaration_is_really_a_call(context, binding.range) {
                    continue;
                }
                // A declaration a macro wrote is not this file's text: its range is inside the directive, and a
                // finding there would underline one line for a name the reader typed somewhere else.
                if binding.origin.is_some() {
                    continue;
                }
                // **`T * T;` is a multiplication, not a declaration**, and this is the one place that can say so.
                //
                // C resolves the shape by the *name*: `T * x;` is a declaration when `T` is a typedef name and a
                // multiplication when it is a variable. The scope walk has no symbol table when it binds, so
                // `#define SQUARE(v) ({ int s = (v); s * s; })` — used **once** — recorded two variables called `s`,
                // and this rule reported the second: measured on a nine-file C corpus, one finding, and pinned by
                // `the_corpus_square_body_reads_its_last_statement_as_an_expression`.
                //
                // Both sides being the *same spelling* is what makes this decidable here: a pointer declaration
                // `T * T;` needs `T` to name a type, and a name that names a type is bound as one — which
                // [`a_kind_that_cannot_be_repeated`] already refuses to judge. The binding's own range covers the
                // *declarator* (`"*corpus_square"`), so the type is read from the text just before it.
                let declarator: &str = context
                    .source
                    .get(binding.range.start_offset..binding.range.end_offset())
                    .unwrap_or("");
                let specifier = context
                    .source
                    .get(..binding.range.start_offset)
                    .unwrap_or("")
                    .trim_end()
                    .rsplit(|character: char| !(character.is_alphanumeric() || character == '_'))
                    .next()
                    .unwrap_or("");
                if declarator.trim_start().starts_with('*') && specifier == name {
                    continue;
                }

                match declared.get(name) {
                    // The *same* range twice is one declaration recorded twice, not two declarations.
                    Some(first) if first.start_offset == binding.name_range.start_offset => {}
                    Some(_) => {
                        repeated.push((binding.name_range, name.to_string()));
                    }
                    None => {
                        declared.insert(name, binding.name_range);
                    }
                }
            }
        }

        for (range, name) in repeated {
            context.add(
                DiagnosticCode::Redefinition,
                range,
                name.clone(),
                t!("`%{name}` is already declared in this scope", name = name),
            );
        }
    }
}

/// **Can one scope hold two of these under one name?**
///
/// Only the variables. Everything else the scope tree binds has a second reading — a function name is an overload set,
/// a class name is a forward declaration and its definition, a `using`-declaration is a synonym — and a rule that
/// judged those would report code every compiler accepts.
fn a_kind_that_cannot_be_repeated(kind: crate::BindingKind) -> bool {
    matches!(kind, crate::BindingKind::Variable)
}
/// **Is the declaration this binding belongs to really a call?**
///
/// `xfree(*matches);` is two readings of one line: a call of `xfree` with a dereference for its argument, and a
/// declaration of `matches` as a pointer to `xfree`. The grammar takes the second — it is the shape
/// [`the_arguments_look_like_declarators`] exists for — and only lookup can say which one the program has: a
/// declaration's first name is a type, and `xfree` is a function.
///
/// Asked only of a binding that repeats a name, so the cost is one index lookup per finding rather than per
/// declaration.
fn the_declaration_is_really_a_call(
    context: &CheckContext<'_>,
    range: cpp_parser::SourceRange,
) -> bool {
    let Some(token) = cpp_parser::token_at(context.model.tree(), range.start_offset) else {
        return false;
    };
    let Some(declaration) = token.parent_ancestors().find(|node| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::Declaration
    }) else {
        return false;
    };
    // **The statement's first name** — the spelling the call would use, and the one a declaration would treat as its
    // type.
    let Some(first) = declaration
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| !cpp_parser::is_trivia(cpp_parser::CppTokenKind::from(token.kind())))
    else {
        return false;
    };
    if cpp_parser::CppTokenKind::from(first.kind()) != cpp_parser::CppTokenKind::Identifier {
        return false;
    }
    let name = first.text().to_string();
    matches!(
        context.model.index().definition(name.as_str(), context.path),
        crate::Known::Yes(found) if found.fact.kind == crate::DeclKind::Function
    )
}