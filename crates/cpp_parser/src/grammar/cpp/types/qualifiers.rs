//! **Qualifiers** — function qualifiers and the implementation keywords (`__cdecl`, `__restrict`, …).
//!
//! Moved here from `types.rs` without a change.

use super::*;

/// Consume the qualifiers and specifiers that may follow a function declarator's parameter list.
///
/// e.g.: `const`, `volatile`, `noexcept`, `override`, `final`, `&`, `&&`, `-> T`
pub fn eat_function_qualifiers(p: &mut CppParser) {
    loop {
        match p.current_token() {
            CppTokenKind::ConstKeyword
            | CppTokenKind::VolatileKeyword
            // `mutable` is a specifier on a member function and a qualifier on a lambda's `operator()`, and it
            // sits in the same position either way: after the parameter list, before the body. `constexpr` and
            // `consteval` may follow a lambda's parameter list for the same reason.
            | CppTokenKind::MutableKeyword
            | CppTokenKind::ConstexprKeyword
            | CppTokenKind::ConstevalKeyword
            // **`static`, the C++23 lambda-specifier** (P1169R4, "`static operator()`"), which is the same family
            // as the three above and the same position: `[](int x) static { return x; }`.
            //
            // Not here for any *declaration*: `void f() static;` is not C++, and nothing in the standard library
            // writes it. What does write it is MSVC's own `<iostream>` and `<ranges>`, in the lambda each uses to
            // wrap a console call — and without this arm the lambda's body was never reached, so the whole
            // statement was reported and the declaration after it went with it.
            | CppTokenKind::StaticKeyword
            | CppTokenKind::Ampersand
            | CppTokenKind::LogicalAnd => p.bump(),
            // A **dynamic exception specification** — `throw()`, `throw(int)`, `throw(T, U&)` — which sits exactly
            // where `noexcept` sits and is the C++98 spelling of the same idea for the empty form. Removed in
            // C++17, and still written in the standard library's own headers: 63 occurrences in 14 files of the
            // measured closure (`has_facet(const locale&) throw();`, `void f() throw(int);`, `~A() throw();`).
            //
            // It is a *suffix*, not a throw-expression, and the position is what says so: the parameter list has
            // already ended, so there is no statement for a throw to be part of, and no declaration continues with
            // the keyword. `throw` as an *expression* is the same token one grammar layer down, which is why this
            // arm has to be here rather than in the expression rules.
            //
            // The payload is a list of **types** — that is the whole difference from `noexcept(expr)` above it —
            // so it is read with the type-id rule, one `TypeId` node per type, exactly as a template argument list
            // reads the same tokens.
            CppTokenKind::ThrowKeyword if p.peek_next_token() == CppTokenKind::LeftParen => {
                p.bump(); // `throw`
                p.bump(); // `(`

                while p.current_token() != CppTokenKind::RightParen && !p.is_eof() {
                    if parse_type_id(p).is_err() {
                        break;
                    }
                    if p.current_token() != CppTokenKind::Comma {
                        break;
                    }
                    p.bump(); // `,`
                }

                if expect_token(p, CppTokenKind::RightParen).is_err() {
                    return;
                }
            }
            CppTokenKind::NoexceptKeyword => {
                p.bump();
                if p.current_token() == CppTokenKind::LeftParen {
                    p.bump();
                    let checkpoint = p.checkpoint();
                    if super::super::exprs::parse_expr(p).is_err() {
                        p.rollback(checkpoint);
                    }
                    if expect_token(p, CppTokenKind::RightParen).is_err() {
                        return;
                    }
                }
            }
            // `override` and `final` are contextual keywords, so they arrive as identifiers.
            CppTokenKind::Identifier if matches!(p.current_token_text(), "override" | "final") => {
                p.bump()
            }
            // A **contract specifier** is not a macro: `int f(int x) pre(x > 0)` writes one after the parameter
            // list, which is exactly where this arm would take `pre(...)` for an invocation of a macro named `pre`.
            // C++26 (P2900) makes that position a contract, so the arm stands aside and the declarator's own rule
            // reads it — see [`super::super::decls::parse_contract_specifiers`].
            CppTokenKind::Identifier if super::super::decls::at_a_contract_specifier(p) => return,
            // A **macro** among the suffixes, which is where libstdc++ puts one: `void f() _GLIBCXX_NOEXCEPT`,
            // `T* addressof(T&) _GLIBCXX_NOEXCEPT`, `… const _GLIBCXX_NOEXCEPT`. A name here has no other
            // reading — the two contextual keywords are the arm above, and `requires` is refused inside — so the
            // shape is decisive: see [`super::super::decls::eat_a_macro_suffix`].
            CppTokenKind::Identifier if super::super::decls::eat_a_macro_suffix(p) => {}
            CppTokenKind::Arrow => {
                // The `->` goes in a `TrailingReturnType` wrapper rather than inside the `TypeId`,
                // so the type node's text is the type the user wrote (`int*`) and not the arrow that
                // introduced it. Without the wrapper every consumer of a trailing return type would
                // have to strip the `->` itself, and most would forget.
                let trailing = p.mark(CppSyntaxKind::TrailingReturnType);
                p.bump(); // `->`
                let parsed = parse_type_id(p);
                if parsed.is_err() {
                    trailing.undo(p);
                    return;
                }
                p.set_last_declarator_is_function(true);
                trailing.complete(p);
            }
            _ => return,
        }
    }
}

/// What an implementation keyword is, once it is read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ImplementationKeyword {
    /// A **qualifier**: `__restrict` / `__restrict__`, which is C's `restrict` spelled the way GCC and MSVC
    /// spell it. The kind table already has a node for it — [`CppSyntaxKind::RestrictQual`] — and nothing was
    /// producing one.
    Qualifier,
    /// A specifier that **means another specifier**: `__forceinline` is `inline` and nothing else (the closure's
    /// own `_mingw.h` defines it as `inline __attribute__((__always_inline__))`), so it is read as the node the
    /// standard spelling produces rather than as something new.
    Means(CppSyntaxKind),
    /// Neither: a **calling convention** (`__cdecl`, `__stdcall`, …) or a marker that suppresses a warning
    /// (`__extension__`, `__unaligned`). These say nothing about the type or the name, so they stay bare tokens
    /// where they were written — dressing them up as a specifier node would claim a meaning this layer does not
    /// read.
    Bare,
}

/// Which of the compiler's own keywords is at the cursor, if any?
///
/// # Why a spelling test is the right one here
///
/// The same two conditions as the attribute spellings ([`at_an_attribute`]) and the standard library's own
/// `__try`/`__catch`: the names are **reserved to the implementation** — a double underscore, so no conforming
/// program may define them — and they belong to the *compiler*, so no `#define` in any header a project writes
/// can turn `__cdecl` into something else. `__extension__` is GCC's, `__cdecl` and `__unaligned` are MSVC's,
/// and MinGW's headers are written with all of them at once.
///
/// # What the wrong reading cost, and it was not a diagnostic
///
/// `int __cdecl g(void);` parsed *successfully* as a declaration whose declarator was named `__cdecl` and whose
/// suffix was a macro call `g(void)` — the sequence type, name, macro-suffix, which is a shape this grammar
/// reads on purpose (see [`super::super::decls::eat_a_macro_suffix`]). Well formed, lossless, no diagnostic, and every
/// name in it wrong. That is the class of defect opens with, and the reason the second
/// position below matters as much as the first: `int *__cdecl _errno(void);` writes the keyword after the `*`.
///
/// # Measured, on the closure of six standard headers
///
/// `__cdecl` **1464 occurrences in 19 files**, `__restrict` 365 in 8, `__extension__` 61 in 12, `__int64` 81 in
/// 14, `__forceinline` 3 in 2, and `__thiscall`, `__w64`, `__unaligned`, `__ptr64` once or twice each. Three of
/// those files (`stdio.h`, `wchar.h`, `stdlib.h`) report the keyword's line as their **first** error, and every
/// file that merely *parses* today is one of the silent wrong trees above.
///
/// The whole calling-convention family is listed, not only the one with hits, because they are one concept and
/// one closed set of reserved names: adding `__stdcall` when a file needs it would be the same rule again. What
/// is deliberately **not** here is `__int64`: the closure's own `_mingw.h` `#define`s it to `long long`, so it is
/// a macro, and the reading it already has (a name in type position) coincides with what it means.
/// Is one of the compiler's own keywords written at the cursor?
///
/// The question the *expression* grammar asks, where the answer is the only thing needed: it steps over one and
/// reads the operand again — see the `__extension__` arm of `parse_unary_expr`. Kept as a separate predicate
/// rather than exposing [`an_implementation_keyword`], whose return type says *which* keyword it is and is this
/// module's business.
pub(crate) fn at_an_implementation_keyword(p: &CppParser) -> bool {
    an_implementation_keyword(p).is_some()
}

pub(crate) fn an_implementation_keyword(p: &CppParser) -> Option<ImplementationKeyword> {
    if p.current_token() != CppTokenKind::Identifier {
        return None;
    }

    // **CUDA's placement qualifiers, and only in a CUDA unit.** `nvcc` adds them to the language: they say where a
    // function or a variable lives (host, device, both, constant memory, shared memory), and to a C++ parser they are
    // just names — so `__global__ void k() {}` reads as a declaration of a variable `__global__` of type `void`,
    // followed by `k()` at file scope, which is not a declaration at all. Reported shape: the kernel's own body turned
    // into an error node and everything after it in the unit was lost.
    //
    // Gated on [`CppParser::cuda`] because that is the whole point of the switch: in a project that has never seen
    // `nvcc`, `__global__` is an ordinary identifier and must keep parsing as one.
    if p.cuda() {
        let cuda = match p.current_token_text() {
            // The placement qualifiers: no semantics to this layer beyond "a specifier stands here".
            "__global__" | "__device__" | "__host__" | "__shared__" | "__constant__"
            | "__managed__" | "__grid_constant__" => Some(ImplementationKeyword::Bare),
            // The inline family: `__forceinline__` is `inline` and nothing else, exactly as MSVC's `__forceinline`.
            "__forceinline__" | "__noinline__" => {
                Some(ImplementationKeyword::Means(CppSyntaxKind::InlineSpec))
            }
            // **A qualifier written with arguments** — `__launch_bounds__(1024, 2)`, `__maxnreg__(64)`,
            // `__align__(16)`, `__cluster_dims__(2, 1, 1)`. The arguments are a token group this layer keeps as
            // written (see `parse_an_implementation_keyword`), the same treatment an asm payload gets.
            "__launch_bounds__" | "__maxnreg__" | "__align__" | "__cluster_dims__" => {
                Some(ImplementationKeyword::Bare)
            }
            _ => None,
        };
        if cuda.is_some() {
            return cuda;
        }
    }

    ordinary_implementation_keyword(p)
}

/// The spellings that are the compiler's own in **every** dialect — the ones `an_implementation_keyword` answers
/// before it asks whether the unit is CUDA.
pub(crate) fn ordinary_implementation_keyword(p: &CppParser) -> Option<ImplementationKeyword> {
    Some(match p.current_token_text() {
        // **GNU's spelling of C99's `restrict`**, and gated for the same reason as the other extensions: a strict
        // C++23 parse has no `__restrict`, and reading it as a qualifier hides that from the reader.
        "__restrict" | "__restrict__"
            if p.language().support(crate::features::Feature::RestrictGnu) =>
        {
            ImplementationKeyword::Qualifier
        }
        "__forceinline" => ImplementationKeyword::Means(CppSyntaxKind::InlineSpec),
        "__cdecl" | "__stdcall" | "__fastcall" | "__thiscall" | "__vectorcall"
        | "__extension__" | "__unaligned" | "__ptr64" | "__ptr32" | "__w64" => {
            ImplementationKeyword::Bare
        }
        // **GCC's unary operators** — `__real__ x`, `__imag__ x`, the spellings `_Complex` support is written in.
        // They are operators rather than specifiers, and the arm that matters is the **expression** one
        // (`parse_unary_expr`'s implementation-keyword case, which steps the keyword and parses the operand); this
        // list is what makes that arm see them, and it is also what keeps them out of a declaration's specifier
        // sequence. Measured on libstdc++'s `<complex>`: without them `constexpr float real() const { return
        // __real__ _M_value; }` did not parse, the recovery swallowed the class body, and every file rendered after
        // it lost its declarations — `std::cout` among them (`docs/worklog.md`).
        "__real__" | "__imag__" => ImplementationKeyword::Bare,
        _ => return None,
    })
}

/// Is the cursor on a **type that this target's compiler adds to the language**?
///
/// Read as a `BuiltinType` specifier, with the *spelling* kept in the node's text — the same arrangement
/// [`ImplementationKeyword::Means`] uses for `__forceinline`, and the reason it goes through the ordinary
/// specifier rule rather than the compiler-keyword skip above: a rule that produces a `BuiltinType` is what tells
/// [`parse_one_decl_specifier`] that **a type has been named** (`has_type_specifier`), and without that answer
/// `_Float16 h = 1;` has its declarator name taken into the type — the declaration then has no declarator, fails,
/// and the statement falls back to an expression.
///
/// All of these are reserved to the implementation (a double underscore, or the `_FloatN` family the standard
/// names for it), so no `#define` in any header can turn one into something else, and the only question left is
/// *which* implementation — which is the dialect:
///
/// ```text
/// GNU       __int128, _Float16, _Float32, _Float64, _Float128, __float128, __fp16, __bf16
/// MSVC      __int8, __int16, __int32, __int64
/// ```
///
/// Read as names — which is what happened before this existed — the failure is silent and shaped like a
/// declaration of something else: `unsigned __int128 x;` came out as a declaration of a variable named
/// `__int128` whose **suffix** was the macro `x` (measured on `bits/bmi2intrin.h`, whose first error that was).
/// `_Float16` and `__bf16` are in the corpus too (`avx512fp16intrin.h`, the `avx10_2` headers).
/// # Why this is still `dialect()` and not a feature
///
/// Because the two vendors **disagree about the same spelling**, and that is a different question from "which syntax does
/// this project accept". `__int64` is a type under MSVC and a macro under GNU (`_mingw.h` defines it), so a parse that
/// knows both vendors cannot answer "is `__int64` a type" by asking whether either is enabled — it needs to know *which
/// one is reading the file*. `Dialect` is exactly that, and it is the one place where it still is.
pub(crate) fn a_type_the_compiler_spells(p: &CppParser) -> bool {
    match p.dialect() {
        Dialect::Gnu => matches!(
            p.current_token_text(),
            "__int128"
                | "_Float16"
                | "_Float32"
                | "_Float64"
                | "_Float128"
                | "__float128"
                | "__fp16"
                | "__bf16"
        ),
        // MSVC's own integer types. Under GNU they are *not* types: MinGW's `_mingw.h` `#define`s every one of
        // them (`#define __int64 long long`), so reading them as types there would be reading a macro as a
        // keyword — the same mistake in the other direction.
        Dialect::Msvc => matches!(
            p.current_token_text(),
            "__int8" | "__int16" | "__int32" | "__int64"
        ),
    }
}
