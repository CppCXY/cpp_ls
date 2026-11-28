//! **`unresolved-call-or-receiver`**: a name read where nothing declares it.
//!
//! **The name is the whole of P2-4 of `docs/worklog.md`.** It used to be `undefined-name`, which promises a
//! complete undefined-name check and delivers two positions: the object of a member access, and a call's callee. An
//! ordinary assignment to a name nothing declares — `missing = 1;` — is silent here, and a reader told the code is
//! `undefined-name` reads that silence as "the analysis believes this is declared". The check is unchanged; the code now
//! says which question it answers.
//!
//! ```cpp
//! void f() {
//!     missing.size();     // nothing anywhere declares `missing`
//!     Name n;
//!     n.size();           // …and this is `undefined-field`'s question, not this one
//! }
//! ```
//!
//! # What it asks, and of whom
//!
//! Only about the **object of a member access**, and only when that object is written as one bare name. That is the
//! narrowest place where "nothing declares this" is a fact rather than an unfinished reading:
//!
//! ```text
//!   checked      myName.getFullName(0)     one identifier, whose declaration must exist
//!   not checked  a.b.c                     the object is an expression; the inner `a` is checked instead
//!   not checked  this->size                a keyword, not a name
//!   not checked  std::string::npos         qualified: a namespace lookup, not this question
//!   not checked  (*p).size()               an expression
//! ```
//!
//! # The four reasons to stay quiet
//!
//! Measured against the rule this layer is written against — a wrong squiggle is worse than none, because a reader
//! who is taught to ignore one has turned the whole channel off:
//!
//! ```text
//!   a macro            `#define name value` names it, and the rendering has already expanded it — but a name used
//!                      as a *receiver* may still be the macro's own spelling in a region the walk did not expand
//!   a template         any access inside a `template <…>` is dependent: `T::value`'s members exist only at
//!                      instantiation, and so may an unqualified name inherited from a dependent base
//!   a lambda           a capture is a declaration the scope tree does not bind, so its body is not judged here
//!   an unknown answer  a name in a header nobody read, in a module this project does not contain, or behind a
//!                      condition that could not be evaluated is *not* "not declared" — it is "not read"
//! ```
//!
//! # What counts as declared
//!
//! Two sources, and both have to say no before a finding is added:
//!
//! * **The file's own scopes**, at the offset of the use: a local is visible from its declaration onward, and a class
//!   member is visible throughout the class (a complete-class context), which is why the position test is skipped for
//!   a class scope and applies everywhere else.
//! * **The index**: a declaration in another file is visible, because the files that see this one are its includes —
//!   and a declaration in *this* file is already the scope tree's business, so it is not counted twice (the two
//!   coordinate systems, the rendering's and the file's, are not comparable offset by offset).

use crate::Known;
use crate::ScopeKind;
use crate::UnknownReason;
use crate::file::coordinates::ReadingOffset;
use crate::index;
use crate::sema;
use cpp_parser::{CppAstNode, CppSyntaxKind, CppTokenKind};

use super::{Check, CheckContext, DiagnosticCode};

/// **`unresolved-call-or-receiver`** — see the module documentation.
pub struct UndefinedName;

impl Check for UndefinedName {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UndefinedName];

    fn check(context: &mut CheckContext<'_>) {
        for (node, access) in context.member_accesses() {
            // **One bare identifier**, and nothing else — see the module documentation's table.
            let Some(token) = a_bare_name(&access.object) else {
                continue;
            };
            let name = token.text().to_string();
            let at = usize::from(token.text_range().start());

            // A keyword is not a name: `this->size` and, in some grammars, `super`.
            if matches!(CppTokenKind::from(token.kind()), CppTokenKind::ThisKeyword) {
                continue;
            }

            // **A literal with a user-defined suffix is not a name.** `"numbers"sv.size()` is
            // `operator""sv("numbers").size()`, and the token the tree hands over is the literal's own spelling — not an
            // identifier, and nothing a declaration could be. Measured on `strings.cpp` of the C++ corpus.
            if CppTokenKind::from(token.kind()) != CppTokenKind::Identifier {
                continue;
            }

            if in_a_template(&node) || in_a_lambda(context, at) {
                continue;
            }
            if a_macro_is_visible(context, &name, at) {
                continue;
            }
            if context.model.names_a_template_parameter(&name, at) {
                continue;
            }
            if a_declaration_is_visible(context, &name, at) {
                continue;
            }

            context.add(
                DiagnosticCode::UndefinedName,
                cpp_parser::source_range(token.text_range()),
                name.clone(),
                t!(
                    "`%{name}` is not declared: nothing this file can see names it",
                    name = name
                ),
            );
        }

        // **A call's callee**, which is the other half of "a name is read here": `printh("…", 12)` on line 74 of the
        // sample file, with `void printh(…)` defined on line 83, is the same error as an undeclared receiver and the
        // same silence.
        //
        // The extra guard a receiver does not need is **ADL**: an unqualified call may be found through the
        // namespaces of its arguments' types, so nothing may be said unless every argument's type is one the
        // language knows (a builtin has no associated namespace). An argument whose type this layer cannot work out
        // is "may be a class", and that is silence.
        for call in context.own::<cpp_parser::CppCallExpr>().collect::<Vec<_>>() {
            let Some(callee) = call.get_callee() else {
                continue;
            };
            let callee = callee.syntax().clone();
            let call_node = call.syntax().clone();
            // A **bare** callee only: `a.f(…)` is the receiver's question above, and `Namespace::f(…)` and
            // `(*p)(…)` are not this question at all.
            let Some(token) = a_bare_name(&callee) else {
                continue;
            };
            if super::member_access_above(&callee).is_some() {
                continue;
            }
            // **A literal with a user-defined suffix is not a name.** `"numbers"sv` is the call
            // `operator""sv("numbers")`, and the token the tree hands over is the literal's own spelling — not an
            // identifier, and nothing a declaration could be. Measured on `strings.cpp` of the C++ corpus.
            if CppTokenKind::from(token.kind()) != CppTokenKind::Identifier {
                continue;
            }
            let name = token.text().to_string();
            let at = usize::from(token.text_range().start());
            if in_a_template(&call_node) || in_a_lambda(context, at) {
                continue;
            }
            if a_macro_is_visible(context, &name, at) {
                continue;
            }
            if context.model.names_a_template_parameter(&name, at) {
                continue;
            }
            // **A keyword spelling is never a callee, and a builtin is never a name a project declares.**
            //
            // Both guards exist because the *reading* can put a construct this layer knows is not a call into a
            // `CallExpr`: a header parsed without the C11 features turns `_Static_assert(…)` and `_Generic(…)` into one
            // (`<stdio.h>`, inlined into a C file, is where those came from — measured, **11 findings** on one
            // nine-file C corpus), and `__builtin_expect(…)` is supplied by the compiler and declared nowhere. Neither
            // is a claim about the reader's code, so neither is a finding.
            if is_a_keyword_construct(&name) || is_the_implementations_builtin(&name) {
                continue;
            }
            if a_declaration_is_visible(context, &name, at) {
                // **Declared as a value, and called.** In C a variable can be called only when its type is a pointer
                // to a function — there is no `operator()` — so `int value = 0; value();` is an error in every
                // compiler, and it was silent here because the name *is* declared. C++ is not asked: a class with
                // `operator()` is called exactly this way, and deciding it needs the member list.
                if context.is_c()
                    && the_declaration_is_a_value(context, &name, at)
                    && matches!(context.type_of(&callee), Known::Yes((type_of, _)) if whether_it_can_be_called(&type_of) == Some(false))
                {
                    context.add(
                        DiagnosticCode::UndefinedName,
                        cpp_parser::source_range(token.text_range()),
                        name.clone(),
                        t!(
                            "`%{name}` is not a function: this file declares it as a value",
                            name = name
                        ),
                    );
                }
                continue;
            }
            if an_argument_may_take_part_in_adl(context, &call_node) {
                continue;
            }

            context.add(
                DiagnosticCode::UndefinedName,
                cpp_parser::source_range(token.text_range()),
                name.clone(),
                t!(
                    "`%{name}` is not declared: nothing this file can see names it",
                    name = name
                ),
            );
        }
        // **A bare name read**, which is the third position a name can appear in and the one the module
        // documentation says this code does *not* answer — `return missing_symbol;` was silent while
        // `missing_function()` was reported. It is asked only in C, where a name that no declaration and no macro
        // names is a name nothing declares; in C++ a dependent context or a `using` this layer has not read can make
        // the same words correct, and the guards below are narrower than that.
        if context.is_c() {
            for name_node in context.own::<cpp_parser::CppNameExpr>().collect::<Vec<_>>() {
                let node = name_node.syntax().clone();
                let Some(token) = a_bare_name(&node) else {
                    continue;
                };
                if CppTokenKind::from(token.kind()) != CppTokenKind::Identifier {
                    continue;
                }
                // **A name that declares rather than reads** — a declarator's own name, a parameter, a type in a
                // specifier sequence, a designated member: all of them are `NameExpr` nodes too, and none of them
                // is a read of a name.
                if !a_read_and_not_a_declaration(&node) {
                    continue;
                }
                let name = token.text().to_string();
                let at = usize::from(token.text_range().start());
                // **A keyword spelling is never a name a project declares** — the same guard the call half keeps,
                // and the reason it is here: `__declspec(dllexport)` and `__attribute__((…))` in a C file are the
                // compiler's own words, and a bare-name rule without this reported 59 `__declspec` and 27
                // `__attribute__` findings on sqlite.
                if is_a_keyword_construct(&name) || is_the_implementations_builtin(&name) {
                    continue;
                }
                if in_a_template(&node) || in_a_lambda(context, at) {
                    continue;
                }
                if a_macro_is_visible(context, &name, at) {
                    continue;
                }
                if context.model.names_a_template_parameter(&name, at) {
                    continue;
                }
                if a_declaration_is_visible(context, &name, at) {
                    continue;
                }
                context.add(
                    DiagnosticCode::UndefinedName,
                    cpp_parser::source_range(token.text_range()),
                    name.clone(),
                    t!(
                        "`%{name}` is not declared: nothing this file can see names it",
                        name = name
                    ),
                );
            }
        }
    }
}

/// **Is this `NameExpr` a read of a name, rather than the name a declaration writes?**
///
/// The node kinds below are the ones a *name* appears in without being read: a declarator's id, a parameter, a type
/// in a specifier sequence, a designated initialiser's member, a class or enum tag. Every one of them is a
/// `NameExpr`, so a walk that asked only "is this an identifier nothing declares" reported the declarations
/// themselves.
///
/// A callee and a member access's object are *reads*, but they are the two positions this check already answers
/// from its own loops — with an ADL guard and a receiver rule the walk does not have — so they are left to those.
fn a_read_and_not_a_declaration(node: &cpp_parser::CppSyntaxNode) -> bool {
    if let Some(parent) = node.parent() {
        // The callee of a call, and the object of a member access, are those loops' questions.
        if cpp_parser::CppCallExpr::cast(parent.clone()).is_some() {
            return false;
        }
    }
    let mut above = node.parent();
    while let Some(current) = above {
        // **The compiler's own parentheses are not names.** `__declspec(dllexport)` and `__attribute__((unused))` are
        // one construct written with an argument, and the argument is a word the compiler reads: measured on sqlite,
        // the guard that skips `__declspec` itself left `dllexport` reported.
        if let Some(first) = current.first_token()
            && matches!(
                first.text(),
                "__declspec"
                    | "__attribute__"
                    | "__attribute"
                    | "_Pragma"
                    | "__pragma"
                    | "alignas"
                    | "_Alignas"
            )
        {
            return false;
        }
        if cpp_parser::CppDeclarator::cast(current.clone()).is_some()
            || cpp_parser::CppParam::cast(current.clone()).is_some()
            || cpp_parser::CppDesignatedInitExpr::cast(current.clone()).is_some()
            || cpp_parser::CppTypeId::cast(current.clone()).is_some()
        {
            return false;
        }
        // A specifier sequence is a type, and the name inside it is not read.
        if cpp_parser::CppSyntaxKind::from(current.kind())
            == cpp_parser::CppSyntaxKind::DeclSpecifierSeq
        {
            return false;
        }
        above = current.parent();
    }
    true
}
///
/// The two sources `a_declaration_is_visible` reads, asked for the *kind* instead of the existence: the file's own
/// scope tree ([`BindingKind`]) and the index ([`DeclKind`]). `false` as soon as one of them is a function, a
/// constructor or an operator — a name that can be a call is not this question's.
fn the_declaration_is_a_value(context: &CheckContext<'_>, name: &str, at: usize) -> bool {
    use crate::BindingKind;
    use crate::DeclKind;

    // **The innermost binding decides.** `scopes_visible_from` walks from the scope the use is in outwards, which is
    // the language's own lookup order: a local `value` shadows a file-scope `void value(void)`, and a call through it
    // is a call through a value.
    let scopes = context.model.scopes();
    if let Some(scope) = scopes.scope_at(at) {
        for id in scopes.scopes_visible_from(scope) {
            let Some(data) = scopes.scope(id) else {
                continue;
            };
            let from_anywhere = matches!(data.kind, ScopeKind::Class | ScopeKind::Enum);
            for binding in &data.bindings {
                if binding.name.identifier_text() != Some(name) {
                    continue;
                }
                if !from_anywhere && binding.name_range.start_offset > at {
                    continue;
                }
                return matches!(
                    binding.kind,
                    BindingKind::Variable | BindingKind::Enumerator
                );
            }
        }
    }

    // **A name the scope tree does not hold is the index's question** — a file-scope variable of this file, or a
    // declaration in a header. Only a definite answer is used: an unread header may declare the function this calls.
    match context.model.index().definitions(name, context.path) {
        Known::Yes(found) => {
            let mut is_a_value = false;
            for declaration in found
                .found
                .iter()
                .filter(|entry| entry.file != context.path)
            {
                if declaration.fact.kind == DeclKind::Function {
                    return false;
                }
                is_a_value = true;
            }
            is_a_value
        }
        Known::Unknown(_) => false,
        Known::No => false,
    }
}

/// **Can a value of this type be called?** — `Some(true)` for a function or a pointer to one, `Some(false)` for a value
/// that certainly cannot be, and `None` for everything this layer cannot settle.
///
/// The `None` is load-bearing: `map_visit_fn visit` is a **typedef** of a function pointer, and the type layer answers
/// a `Named` for it — a `false` there reported every call through a typedef'd callback, which is the first thing a C
/// project writes (measured: two findings in this very corpus). Only a `Builtin` — a number — and a pointer to one are
/// certainly not callable. Both shapes of a callable are real: the model answers `Pointer { to: Function }` for
/// `int (*p)(int)` in some places and a bare `Function` in others (see `param_type_check`'s note on it).
fn whether_it_can_be_called(type_of: &sema::types::Type) -> Option<bool> {
    match type_of {
        sema::types::Type::Qualified { of, .. } => whether_it_can_be_called(of),
        sema::types::Type::Reference { to, .. } => whether_it_can_be_called(to),
        sema::types::Type::Function { .. } => Some(true),
        // A pointer to a function is the one callable pointer; a pointer to a *name* may be one through a typedef, and
        // this layer cannot see through it.
        sema::types::Type::Pointer { to } => whether_it_can_be_called(to),
        sema::types::Type::Builtin { spelling }
            if !matches!(spelling.as_str(), "void" | "auto" | "decltype(auto)") =>
        {
            Some(false)
        }
        _ => None,
    }
}

/// **Could argument-dependent lookup find this call's callee?**
///
/// ADL considers the namespaces of the **class and enum types among the arguments**, so a call whose every argument
/// is a builtin cannot be found that way — and only then may a missing declaration be called missing. An argument
/// whose type this layer cannot work out might be anything, which is the same as "yes".
fn an_argument_may_take_part_in_adl(
    context: &CheckContext<'_>,
    call: &cpp_parser::CppSyntaxNode,
) -> bool {
    let Some(call_expression) = cpp_parser::CppCallExpr::cast(call.clone()) else {
        return true;
    };
    call_expression.get_args().iter().any(|argument| {
        !matches!(
            context.type_of(argument.syntax()),
            Known::Yes((type_of, _)) if is_a_builtin_spelling(&type_of.to_string())
        )
    })
}

/// **A type spelling the language already knows** — the ones with no associated namespace.
/// **Spellings that are grammar, not names** — see the call half of [`UndefinedName::check`].
///
/// Every one of them is a keyword in the dialect that has it, so a `CallExpr` whose callee is one is a reading this
/// layer produced out of a construct it did not recognise: `_Static_assert(…)` and `_Generic(…)` in a header parsed
/// without the C11 features are the two measured shapes.
fn is_a_keyword_construct(name: &str) -> bool {
    matches!(
        name,
        "_Static_assert"
            | "static_assert"
            | "_Generic"
            | "_Atomic"
            | "_Alignas"
            | "alignas"
            | "_Alignof"
            | "alignof"
            | "_Noreturn"
            | "_Thread_local"
            | "thread_local"
            | "typeof"
            | "typeof_unqual"
            | "__typeof__"
            | "__typeof"
            | "__typeof_unqual__"
            | "_BitInt"
            | "noexcept"
            | "decltype"
            | "sizeof"
            | "offsetof"
            // **The preprocessor's own operators, which are written like calls.** `_Pragma("…")` is a unary operator
            // the standard defines ([cpp.pragma]); `__pragma(…)` is MSVC's spelling of the same. Neither is a name
            // anything declares, and the preprocessor that expands them is *this* code's own — measured on
            // `preprocessor.cpp` of the C++ corpus, whose `_Pragma` in a macro body was reported undeclared.
            | "_Pragma"
            | "__pragma"
    )
}

/// **Names the implementation reserves for itself** — `__builtin_expect`, `__sync_fetch_and_add`, `_InterlockedAdd`.
///
/// The double underscore (and MSVC's leading underscore plus a capital) is the standard's own mark for "this belongs to
/// the implementation", and a project that uses one has no way to declare it: the compiler supplies it. "Nothing this
/// file can see names it" is therefore true of every such name and says nothing about the program.
fn is_the_implementations_builtin(name: &str) -> bool {
    name.starts_with("__builtin_")
        || name.starts_with("__sync_")
        || name.starts_with("__atomic_")
        || name.starts_with("__builtin")
        || name.starts_with("_Interlocked")
        || name.starts_with("_mm")
}
pub(super) fn is_a_builtin_spelling(written: &str) -> bool {
    const BUILTIN: &[&str] = &[
        "void", "bool", "char", "char8_t", "char16_t", "char32_t", "wchar_t", "short", "int",
        "long", "float", "double", "signed", "unsigned",
    ];
    written
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
        .all(|word| BUILTIN.contains(&word) || matches!(word, "const" | "volatile" | "constexpr"))
}

/// **The single identifier an expression is written as**, or `None` — see the call site.
fn a_bare_name(object: &cpp_parser::CppSyntaxNode) -> Option<cpp_parser::CppSyntaxToken> {
    let mut tokens = object
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| {
            !matches!(
                CppTokenKind::from(token.kind()),
                CppTokenKind::Whitespace | CppTokenKind::Newline
            )
        });
    let token = tokens.next()?;
    if tokens.next().is_some() {
        return None;
    }
    Some(token)
}

/// **Is this access inside a `template <…>`?** — see the module documentation on why a dependent context is silent.
///
/// A `template <…>` head is a **sibling** of the declaration it parameterises, not an ancestor of it: the tree is
///
/// ```text
/// Declaration
///   TemplateDecl "template <class T>"        ← the head
///   DeclSpecifierSeq
///     StructDef … ClassBody … count.size     ← the body, one level down
/// ```
///
/// so a walk up the parents finds the body's ancestors and never passes through the head. The question is therefore
/// asked of each ancestor's **declaration**: does the declaration that owns this node also carry a `TemplateDecl`?
fn in_a_template(node: &cpp_parser::CppSyntaxNode) -> bool {
    for ancestor in node.ancestors() {
        if CppSyntaxKind::from(ancestor.kind()) == CppSyntaxKind::TemplateDecl {
            return true;
        }
        let Some(parent) = ancestor.parent() else {
            continue;
        };
        if cpp_parser::CppDeclaration::can_cast(parent.kind().into())
            && parent
                .children()
                .any(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::TemplateDecl)
        {
            return true;
        }
    }
    false
}

/// **Is this offset inside a lambda's body?** — a capture is a declaration the scope tree does not bind.
fn in_a_lambda(context: &CheckContext<'_>, at: usize) -> bool {
    let Some(scope) = context.model.scopes().scope_at(at) else {
        return false;
    };
    context.model.scopes().scope_chain(scope).iter().any(|id| {
        context
            .model
            .scopes()
            .scope(*id)
            .is_some_and(|data| data.kind == ScopeKind::Lambda)
    })
}

/// **Is this spelling a macro the file can see**, including one defined in another header?
///
/// The summary only holds the file's **own** `#define`s. A macro from `<assert.h>` is not there, so `assert(...)`
/// was read as an undeclared callee when the cook left it unexpanded (the common case for a conditional header
/// macro). The index's macro environment knows every reachable definition; a name it calls a macro — including one
/// whose guard is undecided — is not a name this rule may report as undeclared.
fn a_macro_is_visible(context: &CheckContext<'_>, name: &str, at: usize) -> bool {
    if context
        .summary
        .macros
        .iter()
        .any(|macro_fact| macro_fact.name == name)
    {
        return true;
    }

    // The tree offset is in the rendering's coordinates; the macro environment is keyed by the file's.
    let in_the_file = context
        .view
        .file_offset_of(ReadingOffset::new(at))
        .map(|offset| offset.get())
        .unwrap_or(at);
    match context
        .model
        .index()
        .macro_environment(name, context.path)
        .is_a_macro_at(in_the_file)
    {
        Known::Yes(_) | Known::Unknown(UnknownReason::ConditionalCompilation) => true,
        Known::Unknown(_) | Known::No => false,
    }
}

/// **Does anything name `name` where this use is written?**
///
/// `false` only when both sources say no — see the module documentation's two sources, and its note that every other
/// answer means "not read" rather than "not declared".
fn a_declaration_is_visible(context: &CheckContext<'_>, name: &str, at: usize) -> bool {
    let scopes = context.model.scopes();
    // **An enumerator is bound in its enumeration scope**, which is not on the chain a use outside it walks — a
    // file-scope `enum log_level { LOG_NORMAL }` then read as undeclared one line later. Measured: `LOG_NORMAL`
    // in this corpus` own `main.c`.
    if context
        .summary
        .declarations
        .iter()
        .any(|fact| fact.enumerator && !fact.local && fact.name == name)
    {
        return true;
    }

    if let Some(scope) = scopes.scope_at(at) {
        // **An unnamed namespace's members are visible here too** — see [`ScopeTree::scopes_visible_from`], which
        // is the chain plus every unnamed namespace hanging off it. Without it every call to a file-local helper read
        // as "not declared" (22 findings on the C++ corpus, one per helper).
        for id in scopes.scopes_visible_from(scope) {
            let Some(data) = scopes.scope(id) else {
                continue;
            };
            // A class body is a *complete-class context*: everything it declares is visible everywhere inside it,
            // including a member written after the function that uses it. Every other scope is read top to bottom.
            let from_anywhere = matches!(data.kind, ScopeKind::Class | ScopeKind::Enum);
            for binding in &data.bindings {
                if binding.name.identifier_text() != Some(name) {
                    continue;
                }
                if from_anywhere || binding.name_range.start_offset <= at {
                    return true;
                }
            }
        }
    }

    // **A member of the class the cursor is in, when the body is defined out of class.** The scope chain above cannot
    // answer this one: `bool C::Initialize() { … }` written in a `.cpp` has no class scope in *this* file — the class
    // is in a header — so the chain runs from the function straight to the file, and a member named in the body read
    // as undeclared. The function's own qualified name (`C::Initialize`) says which class it is, and that qualified
    // spelling is what the index keys the member under. Measured on a fixture of that layout: `Initialize()` called
    // from the constructor's body was reported, while `_server` — which the *index* half below finds — was not.
    //
    // Asked with the **qualified** spelling, which is what keeps a bare name from reaching a class: only the class the
    // cursor is actually inside is offered here.
    if let Some(class) = index::project::class_of_the_enclosing_definition(scopes, at)
        && let Known::Yes(found) = context
            .model
            .index()
            .definitions_in_scope(&format!("{class}::{name}"), context.path)
        && !found.found.is_empty()
    {
        return true;
    }

    // **A declaration in another file**, which reaches this one through an `#include`. A declaration in *this* file
    // is the scope tree's answer above, in the rendering's coordinates — the index records the file's own offsets,
    // and the two are not comparable one by one once a directive has been expanded.
    match context.model.index().definitions(name, context.path) {
        Known::Yes(found) => found
            .found
            .iter()
            .any(|declaration| declaration.file != context.path),
        // **"Not declared here" is the answer this rule exists for**, and it is an `Unknown` — the index does not
        // claim absence unless it was asked and found nothing. Every other reason is a name this layer cannot speak
        // about: ambiguous (declared twice), behind an unread include, in a module the project does not contain —
        // all of them are silence rather than a finding.
        Known::Unknown(UnknownReason::NotDeclaredHere(_)) => false,
        Known::Unknown(_) => true,
        Known::No => false,
    }
}
