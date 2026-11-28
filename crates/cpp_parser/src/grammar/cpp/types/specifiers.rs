//! **Specifiers** — the decl-specifier sequence: which keywords may stand in a type, and in what order.
//!
//! Moved here from `types.rs` without a change.

use super::*;
use crate::features;
use crate::grammar;
use crate::parser;

pub fn parse_decl_specifier_seq(p: &mut CppParser) -> ParseResult {
    let _phase = parser::phases::during("specifiers");
    parse_decl_specifier_seq_with(p, true, true)
}

/// Parse a decl-specifier-seq that must not run past the type, for contexts where a following name
/// belongs to something else.
///
/// `template <typename T, typename U>` is the motivating case: both `T` and `U` are names, and only
/// the comma separates them. A specifier sequence allowed to greedily take a second name would run
/// the two parameters together.
pub(crate) fn parse_decl_specifier_seq_stopping_at_one_name(p: &mut CppParser) -> ParseResult {
    parse_decl_specifier_seq_with(p, false, false)
}

/// `allow_second_name` decides whether another identifier may join the specifier sequence.
///
/// C++ declares `T x` with two adjacent names, so a specifier loop that stopped at the first name
/// would never reach the declarator. But that same tolerance is wrong for a type-id — a template
/// argument or the target of a cast — where the sequence must end at the type. Both readings are
/// grammatical; only the caller knows which applies.
///
/// Inside the loop, `has_specifier` and `name_allowed` carry the rest of the decision. It is made
/// from what the loop has *consumed* rather than from the surrounding tokens, because the tokens
/// cannot answer it: "is there a specifier before this name?" is true for both `const Point` (where
/// the name is still part of the type) and `Point p` (where it is the declarator), and guessing
/// wrong there loses the whole declaration — once the loop eats `p` as part of the type there is no
/// declarator left and the `;` never matches.
pub(crate) fn parse_decl_specifier_seq_with(
    p: &mut CppParser,
    allow_second_name: bool,
    reading_a_declaration: bool,
) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::DeclSpecifierSeq);

    let mut specifiers = 0usize;
    // See `name_joins_the_type`: what the loop has consumed so far decides whether the next name
    // belongs to the type or is the declarator.
    let mut has_specifier = false;
    // Whether a **type** has been named, as opposed to merely a specifier consumed. `alignas` is why the two
    // questions are separate — see [`name_joins_the_type`].
    let mut has_type_specifier = false;
    let mut name_allowed = allow_second_name;
    // The first name written in type position, recorded for the reader that has to decide whether a `(` after
    // the declarator is an argument list or a parameter list — see
    // [`crate::grammar::cpp::decls::a_declaration_is_the_better_reading`]. Captured here because this is the
    // only place that knows where the type ends and the declarator begins; recovering it later means walking
    // back over the tokens, which lands on the declarator's name instead.
    //
    // `None` until a *name* is written, because that is what the reader looks up. Whether any specifier has
    // been consumed is a different question and is answered by `specifiers` — a declaration
    // beginning with `explicit` has a specifier and no name.
    p.begin_declaration_type();
    // Where the specifier sequence begins, so the loop can read back what it has produced. See the note on
    // `a_further_name_may_join` below — and on `#else`, which moves it forward: the evidence is per branch.
    let mut specifiers_from = p.current_event_count();
    loop {
        // **An attribute where a specifier goes.** `[[…]]` is one of the things the grammar allows in a
        // decl-specifier-sequence, and reading it is what keeps a declaration that *starts* with one from being
        // refused: MSVC's `sourceannotations.h` writes every one of its structs this way,
        //
        // ```cpp
        // [repeatable] [source_annotation_attribute(All)] struct PreAttribute { … };
        // ```
        //
        // and a sequence that stopped at the `[` left it to the expression rules, which read it as an **array
        // subscript** and reported `expected primary expression` — once per attribute, and the recovery from each
        // one left a brace unclosed. Measured on a project that includes `<string>`: 22 of those errors inside
        // `sourceannotations.h` were enough for its `namespace vc_attributes` to swallow the remaining 2.1 MB of
        // the translation unit, which is why `std::string` had no declaration anywhere and `std::basic_string`
        // could not be found as a member of `std`.
        //
        // [`at_an_attribute`] is the same test every other position uses, so an array bound (`int a[2];`) and a
        // lambda's capture list are untouched: neither is spelled `[[` here. The second half of the condition is
        // MSVC's single-bracket spelling ([`at_a_single_bracket_attribute`]), which only a *declaration* may use —
        // which is exactly the position this loop is in.

        // **MSVC's single-bracket attribute where a specifier goes.** `[repeatable] [source_annotation_attribute(All)]
        // `struct PreAttribute { … };` — `codeanalysis/sourceannotations.h` writes every declaration it has this
        // way, and a sequence that stopped at the `[` left it to the expression rules, which read it as an *array
        // subscript* and reported `expected primary expression` once per attribute. Measured on a project that
        // includes `<string>`: the recovery from those errors left `namespace vc_attributes` open, so the rest of
        // the 2.1 MB translation unit — `<xstring>`, `<vector>`, `main.cpp` — was read as a member of it, and
        // `std::basic_string` lived in `vc_attributes::std`.
        //
        // **Only the single-bracket spelling**, which is what makes this safe to do in a specifier sequence: `[[`
        // here is *not* an attribute. This loop also reads template arguments
        // (`parse_decl_specifier_seq_stopping_at_one_name`), where a `[[` is a subscript of a subscript —
        // `template <typename _Tp, int _Nm> constexpr bool d<_Tp[_Nm]> = true;` — and claiming it there lost the
        // `ArrayType` and failed three tests at once. MSVC's spelling passes the shape test in
        // [`at_a_single_bracket_attribute`], which a subscript cannot.
        if reading_a_declaration
            && at_a_single_bracket_attribute(p)
            && let Err(err) = parse_attribute_specifiers_where_a_declaration_may_write_them(p)
        {
            p.close_marks_above(base);
            return Err(err);
        }

        // A **compiler keyword** is stepped over *here*, outside [`parse_one_decl_specifier`], and the reason is
        // one the two functions have to agree about: that function decides "did this specifier name a type?" by
        // looking at the last token the specifier consumed, and one of these keywords is an *identifier* —
        // `__cdecl`, `__extension__` — which that test reads as a type name. `__forceinline size_t f() { }` was
        // the symptom: the flag said a type had been named, so `size_t` looked like the declarator's name, the
        // `f()` after it like a macro suffix, and the body had no declaration to belong to.
        //
        // Stepping over them out here cannot lose anything, because they are not specifiers *of the type*: the
        // node each one produces (a `RestrictQual`, an `InlineSpec`, or a bare token) is still written inside the
        // `DeclSpecifierSeq` this loop has open.
        while an_implementation_keyword(p).is_some() {
            parse_an_implementation_keyword(p);
        }

        // **A macro whose body is a specifier list** — `_CONST_RETURN` is `const`, `__LONG32` is `long` — stands
        // exactly where a specifier goes, and reading it as one is not a guess: the body is a real file's text
        // (this file's `#define`, or one the include closure carried in) and **every** token of it is a specifier.
        //
        // Two casts in the corpus are written this way, and both were reported at the name the macro stands for:
        //
        // ```cpp
        // return (_CONST_RETURN wchar_t *)(_S);      // wchar.h — `expected ), but get identifier`
        // … (unsigned __LONG32) …                    // basetsd.h — `expected primary expression`
        // ```
        //
        // Read as an invocation inside the sequence rather than expanded, when the body says the name is a
        // specifier. That reading is gone with the macro table: see `a_specifier_follows_the_group`.

        // …and its **function-like** half, which is the same position one spelling along — an attribute macro
        // written *with* its argument list, before the type:
        //
        // ```cpp
        //   template<typename _Operation, typename _Tp>                        // backward/binders.h:133
        //     _GLIBCXX11_DEPRECATED_SUGGEST("std::bind")
        //     inline binder1st<_Operation>
        //     bind1st(const _Operation& __fn, const _Tp& __x)
        // ```
        //
        // The evidence for it is **absence** — no table describes the name and no table can say what it expands to
        // here — and that is the finding from the file it was written for: a header that only *uses* such a macro
        // has no environment at all. `backward/binders.h` contains no `#include`, so read on its own (which is how
        // the census reads it) every table is empty and `_GLIBCXX11_DEPRECATED_SUGGEST` is a name nobody has
        // described:
        //
        // ```text
        // MACRO _GLIBCXX11_DEPRECATED_SUGGEST in binders.h   evidence false | positional body None
        //                                                    | in-force body None | context 0 seeds
        // ```
        //
        // Three versions of this arm were measured, and the two that failed are what fixes the gate:
        //
        // ```text
        // require evidence (is_function_like)      moved NOTHING — binders.h has none to require
        // shape alone                              moved two files BACKWARDS: objbase.h:95 and ole2.h:58, both
        //                                          `WINOLEAPI_(HINSTANCE) CoLoadLibrary (…)`
        // require evidence at all (either channel) +2/-2 …which is this version
        // ```
        //
        // The two channels are separate and both have to be asked: `WINOLEAPI_` has no *definition* in the closure
        // (it is `#define`d inside a conditional branch of `_mingw.h`, so it reaches a use site as a **body in
        // force** and not as a definition), so a gate that asked only [`CppParser::macro_evidence`] still claimed
        // it and took the shape away from [`a_macro_call_begins_the_declaration`], which is the rule that owns it.
        // Asking both leaves this arm with exactly the names **nobody can read any other way** — evidence first,
        // the same order [`crate::parser::MacroNames`] documents.
        //
        // What completes the shape is a **follower** — a *specifier* after the group, which is what separates this
        // shape from the one [`a_macro_call_begins_the_declaration`] owns (a *name* after the group). See
        // [`a_specifier_follows_the_group`] for both the reason and the measurement that put the name case on the
        // other side of the line.
        //
        // Why a **name and a group** are enough: no declaration begins with a call — a declaration's first tokens
        // are specifiers — so a name and a balanced group there is a shape nothing else wants. A member that is only
        // an invocation is claimed before any of this by [`super::super::decls::at_a_macro_member`], and the compiler's own
        // attribute spellings are refused here (see [`at_an_attribute`], which has a reader that knows what they
        // are), so what is left is the shape this arm is for.
        //
        // The arguments are kept as **raw tokens** ([`super::super::decls::parse_balanced_token_group`], the reader the
        // other macro arms use): what stands there is the macro's own text, and `("std::bind")` is not a grammar
        // this file can read — a `#define` in another file is what knows whether it is a string, a type or an
        // expression.
        //
        // **What the evidence gate is, after the MSVC measurement.** The arm used to require that **nobody**
        // describes the name (`macro_evidence` and `macro_body_kinds_at` both `None`), and the reason is still
        // right: a name some rule can *read* belongs to that rule. But a described name is not the same as a name
        // some rule can read — the SAL annotations are the counterexample the measurement found. `<xstring>:592`
        // writes
        //
        // ```cpp
        // constexpr bool _Traits_equal(_In_reads_(_Left_size) const _Traits_ptr_ _Left, …)
        // ```
        //
        // and `_In_reads_` **is** described (it is `#define`d in `<sal.h>`, inside the closure), so this arm stood
        // aside, the specifier loop read `_In_reads_` as a *type*, the `(n)` as a declarator, and `const` came out
        // as ``expected `)`, but get const``. What separates the two worlds is not whether a body is known but
        // whether the **group is followed by a specifier**: `WINOLEAPI_(HINSTANCE) CoLoadLibrary (…)` has a *name*
        // after its group and stays with [`a_macro_call_begins_the_declaration`], which is the reading the
        // two-files-backwards measurement was about. Asking the follower instead of the evidence keeps that line
        // where it is and lets a described annotation macro through.
        //
        // **"Before the type", not "at the head of the sequence"**. The gate was `specifiers == 0`, which
        // allows exactly one annotation and only as the sequence's first token — and MSVC's UCRT writes **runs** of
        // them, so the second was read as a *type*, its group as a *declarator*, and the declaration that followed
        // came out as `expected primary expression` reported against its own first line:
        //
        // ```c
        // _Check_return_opt_
        // _ACRTIMP int __cdecl _rmtmp(void);
        //
        // _CRT_INSECURE_DEPRECATE(setvbuf)
        // _ACRTIMP void __cdecl setbuf(
        //     _Inout_                                             FILE* _Stream,
        //     _Inout_updates_opt_(BUFSIZ) _Post_readable_size_(0) char* _Buffer    // ucrt/stdio.h:400-402
        //     );
        // ```
        //
        // Each annotation here is followed by a specifier (`_ACRTIMP`, `char`), which is the discriminator this arm
        // already asks for; what the count was really protecting is the case the paragraph above it describes — a
        // name-plus-group *after* a type has been named is a declarator's parameter list. `has_type_specifier` says
        // exactly that ("a type has been named, as opposed to merely a specifier consumed"), and it is the state the
        // rest of this loop already reasons with ([`name_joins_the_type`]) — so the gate is that flag and not the
        // number of specifiers.
        //
        // Two halves were needed, and the measurement is what found the second: with the flag alone the closure did
        // not move at all, because [`a_specifier_follows_the_group`] refused the shape — a *name* follows the group
        // and the name carries a group of its own, which is exactly what a declarator's parameter list looks like.
        // Asking the same question from the second name is the other half (`Wobble(2)` is followed by `char`, so both
        // are specifiers; `CoFreeLibrary (…)` is followed by `;`, so that one is the declarator). Measured together:
        // the MSVC closure 276 → **255** messages with no file lost, `stdio.h`'s first error 400 → 609, the two
        // libstdc++ readings and every parser test unchanged.
        // **A name the tables know to be a macro is not a type**. The loop reads an unknown identifier as a
        // type name — that is how `Wat x;` works — and a macro with an **empty body** read there is
        // indistinguishable from the tokens alone: MSVC's `_NODISCARD` is `#define _NODISCARD` (in force as an
        // empty body), so with it in front of the annotation the arm refused itself ("a type has been named") and
        // the declaration took `_Post_equal_to_` for the *declarator's* name, reporting ``expected a parameter list
        // or an initializer`` at its group:
        //
        // ```cpp
        // _EXPORT_STD template <class _Ty>
        // _NODISCARD _Post_equal_to_(_Left < _Right ? _Right : _Left) constexpr const _Ty& // utility:40
        //     (max) (const _Ty& _Left, const _Ty& _Right) noexcept(…)
        // ```
        //
        // **Both halves are needed, and each keeps a neighbour out**. The specifier in front must be a
        // *macro* — the loop reads an unknown identifier as a type name, so an empty-bodied `_NODISCARD` counts as
        // one and the annotation behind it gets refused — **and** a *type keyword* must follow the group.
        //
        // ```text
        // template <class T> _NODISCARD _Post_equal_to_(x) int f(T x)   macro in front, `int` after  → a specifier
        // int main(argc, argv)  int argc;  char *argv[];  { … }         `int` in front (not a macro)  → K&R, unchanged
        // _Check_return_wat_ _Success_(…) _ACRTIMP errno_t fopen_s(…)   macro in front, a name after → the run
        // ```
        //
        // The middle row is why the type keyword alone is not enough: a K&R definition's parameter declarations
        // begin with a type keyword too, and taking the group for a macro's arguments lost
        // `OldStyleParameterList` — `modern_constructs_produce_the_right_nodes` caught it. The first row is the
        // other way round: the macro's own evidence is what says `_NODISCARD` is not the type it was read as.
        // **`a_macro_stands_in_front` used to be asked here**, of the token behind the cursor, and it is gone with
        // the macro table: the answer was "the caller's environment says this name is a macro with a body", and the
        // grammar no longer has an environment to ask. What it bought — `_NODISCARD` not being read as the type it
        // looks like — belongs to the cooked stream now: by the time this rule runs the macro is replaced by what
        // it expands to.
        // **`_BitInt(width)` is a name with a group that is not a macro**, and it has a specifier after the group
        // whenever the member is written `_BitInt(13) signed_wide : 13;` — the `signed_wide` the shape looks for. Read
        // as a macro specifier, it left the declaration with `signed_wide` as its *type* and no declarator, which is
        // the one class of member the summary dropped (measured: `struct bits` recorded `three` and `other` and lost
        // `signed_wide` and `plain`). See [`super::super::decls::a_bitint_declaration_starts_here`].
        if !has_type_specifier
            && allow_second_name
            && p.current_token() == CppTokenKind::Identifier
            && p.peek_next_token() == CppTokenKind::LeftParen
            && !at_an_attribute(p)
            && !super::super::decls::a_bitint_declaration_starts_here(p)
            // **`_Atomic(type) name` is a declaration, not a macro specifier.** The follower is a name, which this
            // arm reads as a further specifier (any identifier can be a type), so it consumed `n` into the type and
            // left the declaration with no declarator — `expected a declarator name` at the `=`. The compiler's own
            // spelling is read by the `_Atomic` arm of [`parse_one_decl_specifier_inner`] instead.
            && p.current_token_text() != "_Atomic"
            && a_specifier_follows_the_group(p)
        {
            let checkpoint = p.checkpoint();
            let call = p.mark(CppSyntaxKind::MacroCall);
            let name = p.mark(CppSyntaxKind::NameExpr);
            p.bump();
            name.complete(p);

            if super::super::decls::parse_balanced_token_group(p, CppSyntaxKind::ArgumentList)
                .is_err()
            {
                // An unterminated group is not this shape: give the name back and let the loop report whatever it
                // reported before, the same way [`super::super::decls::eat_a_macro_suffix`] does.
                p.rollback(checkpoint);
            } else {
                call.complete(p);
                specifiers += 1;
                has_specifier = true;
                continue;
            }
        }

        // A **directive between two specifiers**, which is the same seam as the ones `parse_try_statement`,
        // `parse_if_statement` and `parse_class_body_members` document, and it has the same justification: a `#`
        // cannot be a specifier, so it is read as the node it is and the question is asked again about the token
        // that follows.
        //
        // ```cpp
        // #if __cplusplus > 201703L              // bits/basic_string.h:1310
        //   [[deprecated("use shrink_to_fit() instead")]]
        // #endif
        //   _GLIBCXX20_CONSTEXPR
        //   void
        //   reserve();
        // ```
        //
        // The `#if` is read where a class member goes (see `parse_class_body_members`); the `#endif` arrives
        // *inside* the declaration, after the attribute the sequence has already taken as a specifier. Without
        // this the sequence stopped at the `#`, the declaration failed, and the whole attempt was rolled back —
        // so the attribute and the directive became rubble, and the `reserve()` overload written after them was
        // not a member of the class at all.
        //
        // Only **between** specifiers, and only in a declaration: a `#` where the *first* specifier should be is
        // not this rule's, because a declaration never begins with a directive — the loop that asked for the
        // declaration reads directives itself, and one read here would be one it can no longer see.
        if allow_second_name && specifiers > 0 && p.current_token() == CppTokenKind::Hash {
            // **Which directive is it?** Ask *before* reading it, because the answer decides how much of what the
            // sequence knows survives: `#else`/`#elif` opens the **other branch**, and the branches are
            // alternatives — one spelling of the same declaration each. What the branch just read said about the
            // type therefore says nothing about this one, and carrying it over is a real misreading:
            //
            // ```cpp
            // template<typename _Tp, typename... _Args>              // bits/alloc_traits.h:430
            // #if __cpp_concepts && __cpp_constexpr_dynamic_alloc
            //   requires __can_construct<_Alloc, _Tp, _Args...>
            //   static constexpr void
            // #else
            //   static __enable_if_t<__can_construct<_Alloc, _Tp, _Args...>>
            // #endif
            //   construct(_Alloc& __a, _Tp* __p, _Args&&... __args)
            // ```
            //
            // The `#if` branch names `void`, so "a type has been named" was already true when the `#else` branch's
            // `__enable_if_t<…>` arrived — and a name that is not the first type of its own head was refused the
            // type position, became the **declarator**, and the declaration then ended at the `#endif` with no `;`
            // (the error surfaced on the next member). Forget the flags at the branch boundary and the name joins
            // its own branch's type, which leaves `construct` — the name that comes after the `#endif` — as the
            // declarator, where it belongs.
            //
            // `#endif` is deliberately **not** a boundary of this kind: after it the declaration continues with
            // whatever the branches agreed on, which is exactly the knowledge the tail needs.
            let starts_the_other_branch = matches!(p.peek_token_text_at(1), "else" | "elif");
            if let Err(err) = super::super::stats::parse_preprocessor_directive(p) {
                p.close_marks_above(base);
                return Err(err);
            }
            if starts_the_other_branch {
                // The same three things the loop started with, put back: no type named *in this branch*, the
                // one-name allowance unspent, and the evidence window (what the sequence has produced, read back
                // by `a_further_name_may_join` below) starting after the directive rather than at the head.
                has_type_specifier = false;
                name_allowed = allow_second_name;
                specifiers_from = p.current_event_count();
            }

            // **The other branch's head.** libstdc++ writes two variants of one member with a head each, and the
            // `#else` falls *between* the first head's specifiers and the second head:
            //
            // ```cpp
            // #if __cplusplus >= 201103L                  // bits/basic_string.h:1673
            //   template<class _InputIterator,
            //            typename = std::_RequireInputIter<_InputIterator>>
            //     _GLIBCXX20_CONSTEXPR
            // #else
            //   template<class _InputIterator>
            // #endif
            //     basic_string&
            //     append(_InputIterator __first, _InputIterator __last)
            // ```
            //
            // The sequence has already taken `_GLIBCXX20_CONSTEXPR` — the first branch's specifier — so the head
            // that follows the directive cannot be read by the caller's head loop, which is behind the cursor.
            // Read here, and go round again: the second `#endif` is this branch's again, and `basic_string&` is
            // the next specifier.
            //
            // The head ends up **inside** the `DeclSpecifierSeq`, which is the honest place for it: the declaration
            // has one leading part, the file wrote both branches of it there, and a consumer that reads the type
            // off this node sees both branches' specifiers rather than only the first. What it must not do is take
            // the whole text for a spelling — see the note on `DeclFact::returns`.
            while p.current_token() == CppTokenKind::TemplateKeyword {
                p.set_a_template_id_may_be_the_name(true);
                if let Err(err) = super::super::decls::parse_template_head(p) {
                    p.close_marks_above(base);
                    return Err(err);
                }
            }
            continue;
        }

        // A **macro invocation standing where the declaration's specifiers go** — a macro from a header nobody
        // indexed, called with an argument list, with the declaration's own declarator after it:
        //
        // ```cpp
        // WINOLEAPI_(void) CoFreeLibrary (HINSTANCE hInst);      // objbase.h:96
        // WINOLEAPI_ (void) OleUninitialize (void);              // ole2.h:58
        // ```
        //
        // The macro is the whole type (`EXTERN_C DECLSPEC_IMPORT type STDAPICALLTYPE` in `combaseapi.h`), so the
        // three readings that could apply are all wrong: the specifier loop cannot take `WINOLEAPI_` for a type and
        // `(void)` for a declarator's parameter list — that would declare a function called `WINOLEAPI_` and leave
        // `CoFreeLibrary` with nowhere to go — and the expression fallback makes the call a
        // *statement*, after which the declarator is a syntax error. What the file wrote is a name, a group, and a
        // declaration, which is what this reads.
        //
        // Claimed **before** the specifier loop asks its question, and only where the shape has no competitor: a
        // name **with a balanced group** followed by an **identifier** — `MACRO(args) name (…)`. A `;` or a `{`
        // after the group is the shapes that already own them (a macro standing for a whole declaration, a macro
        // definition), and evidence keeps its precedence: a name this file `#define`s is read by the rule that knows
        // what a macro is.
        //
        // **`_BitInt(width)` is the one name with a group that is not a macro.** C23 makes it a type whose width is a
        // constant expression, so `_BitInt(13) signed_wide : 13;` has exactly this shape — a name, a balanced group,
        // an identifier — and taking it for a macro call is what made the declaration's specifier `MacroCall` plus the
        // *type* `signed_wide`, leaving the bit-field with no declarator: no fact, no member. The anchor is asked here
        // and not in [`super::super::decls::starts_declaration`], where a `_BitInt` that begins a *statement* is already read
        // correctly, and where adding it made `_BitInt(37) wide = (_BitInt(37))1 << 30;` fail instead.
        if allow_second_name
            && specifiers == 0
            && !super::super::decls::a_bitint_declaration_starts_here(p)
            // **`_Atomic(int) n` is a declaration, not a macro call.** The shape is a name, a group and a name,
            // which is what the macro arm below looks for; `_Atomic` is the compiler's own spelling and the
            // specifier rule above reads it, so the arm stands aside for it exactly as it does for `_BitInt`.
            && p.current_token_text() != "_Atomic"
            && a_macro_call_begins_the_declaration(p)
        {
            if let Err(err) = super::super::stats::parse_a_macro_that_stands_for_a_declaration(p) {
                p.close_marks_above(base);
                return Err(err);
            }
            // **The sequence ends here**, and that is the point rather than a shortcut: what the macro expands to
            // is not knowable, so a name read after it could be either the declarator or another word of the type
            // this layer cannot see. Stopping leaves the declarator to the reader that owns it — `CoFreeLibrary
            // (HINSTANCE hInst)` is an ordinary function declarator, read by `parse_declarator`.
            specifiers += 1;
            break;
        }

        let specifier_seen = specifiers > 0;
        // May a **further** name still join the type, beyond the one that is already in it?
        //
        // Three conditions, and the third is the one this question had to grow:
        //
        // * **This is a declaration, not a type-id.** `allow_second_name` is the caller's answer, and in a type-id
        //   it is `false` because the sequence has to end at the type — there is no declarator for a second name
        //   to make room for. Asking the question in a type-id is how a trailing return type swallowed the clause
        //   after it: `-> int requires C<T>` came out as the type `int requires C<T>`, and the clause vanished
        //   from a well-formed, lossless, diagnostic-free tree.
        // * **A name has joined already.** That is the shape being read: an unexpanded macro (`MY_API`) followed by
        //   the real type. Read back from the events the sequence produced rather than tracked as a flag, for the
        //   same reason [`parse_one_decl_specifier`] reads `has_type_specifier` back from them — a branch that
        //   forgot to set a flag is a silent one. A name-based type specifier is a `TemplateType`.
        // * **or the name joining is written like a macro and a type is already there** — the *mirror* shape, with
        //   the macro on the other side of the type:
        //
        // ```cpp
        // void HUGEP **ppvData                 // windef.h's macro, in every COM signature (oleauto.h:71)
        // unsigned __int64 POINTER_64_INT;     // basetsd.h:11, corecrt.h:35
        // unsigned __int64 x;                  // …the same shape without a typedef
        // ```
        //
        // The spelling is what separates this from the tokens' other meaning, and they are the same tokens:
        //
        // ```cpp
        // int x MY_DECL_SUFFIX;                // `x` is the declarator, the macro is a **suffix** (gaps.rs)
        // ```
        //
        // `x` is not written like a macro and `__int64`/`HUGEP` are, so both readings survive — which the first
        // version of this rule did not manage: relaxing the condition to "a type has been named" read the second
        // line as the type `int x` with a declarator named `MY_DECL_SUFFIX`, losing the variable's name and
        // producing an A0-class wrong tree instead of the silent one it was written to fix. Two existing tests
        // caught it (`a_macro_can_stand_among_a_declarators_suffixes`, and the asm-label assertion of
        // `a_using_enum_declaration_and_the_register_specifier_read`).
        //
        // Nothing else changes: with no type named yet the name is the type whatever it looks like (the branch
        // above `a_further_name_may_join` in [`name_joins_the_type`]), and `MY_API Widget *p` still joins
        // `Widget` — an ordinary name — through the second condition.
        let a_further_name_may_join = allow_second_name
            && (p.events_contain_any(
                specifiers_from,
                &[CppSyntaxKind::TemplateType, CppSyntaxKind::NamedType],
            ) || (has_type_specifier && written_like_a_macro(p.current_token_text())));
        // …and the same question one context along, where there is **no declarator at all**: a **type-id**.
        //
        // ```cpp
        // static __inline unsigned __LONG32 HandleToULong (const void *h)          // basetsd.h:68
        // { return ((unsigned __LONG32) (ULONG_PTR) h); }                          // the cast's type-id
        // ```
        //
        // `allow_second_name` is `false` here, and for the question it was written for that is right — a second
        // *name* in a type-id would run `template <typename T, typename U>` together. A name **written like a
        // macro** is a different claim: it is what an unexpanded type spelling looks like, and a type-id has no
        // declarator for it to be, so it can only be another word of the type. Nothing else is relaxed —
        // `-> int requires C<T>` still ends at `int`, because `requires` is not written like a macro.
        let a_macro_shaped_name_closes_a_type_id = !allow_second_name
            && has_type_specifier
            && written_like_a_macro(p.current_token_text());
        // …and has the sequence written a **class-like definition**, body and all?
        //
        // That body is a complete type, and the backward walk [`type_is_already_complete`] makes cannot see it: it
        // meets the `}` that closes the body and answers "no type yet", which is the right answer for the *other*
        // meaning of a `}` — the end of an enclosing block. So the question is asked of the events instead, and
        // the name that follows a definition is the declarator rather than one more word of the type. See
        // [`name_joins_the_type`].
        let a_class_definition_was_written = p.events_contain_any(
            specifiers_from,
            &[CppSyntaxKind::ClassBody, CppSyntaxKind::EnumDef],
        );
        // **A known type name followed by a name ends the specifier sequence.**
        //
        // `EXTERN colnr_T dollar_vcol INIT( = -1);` — neovim's `globals.h`, sixteen times — writes a macro, a typedef
        // and a declarator in a row, and the sequence would join all three to the type and take `INIT` for the
        // declarator. The language's rule is the one applied here: the type is a *name*, and the declarator is the
        // next name — the evidence for the split being that the name just read is a **known type**, which is a fact
        // this file's own declarations answer. Two unknown names have no order between them and keep the old reading.
        //
        // `has_type_specifier` is what keeps it to the *second* name: `Widget w;` is read by the ordinary arm (the
        // first name is the type, whatever it is), and only a name that arrives after a type can be the last one.
        let a_known_type_ends_the_type = has_type_specifier
            && p.token_kind_at(p.current_token_index()) == CppTokenKind::Identifier
            && p.is_a_known_type_name(p.current_token_text())
            && p.token_kind_at(super::super::decls::next_significant_index(
                p,
                p.current_token_index(),
            )) == CppTokenKind::Identifier;

        if let Err(err) = parse_one_decl_specifier(
            p,
            &mut has_specifier,
            &mut has_type_specifier,
            &mut name_allowed,
            NamesMayJoin {
                a_further_name: a_further_name_may_join,
                a_macro_shaped_name_closes_a_type_id,
            },
            a_class_definition_was_written,
            specifier_seen,
        ) {
            if specifiers == 0 {
                // **`end_marks_to`** — the specifier that failed here had already consumed tokens (its
                // keyword, its parentheses, part of its payload), so the nodes it opened must keep what they
                // read instead of being detached. Detached, their `NodeStart`s are balanced by the tree builder
                // at the end of the stream, and every token after the failure is nested inside them — including
                // the declaration that follows. See the note on [`parse_type_id_here`], which is the same fix one
                // level out, and the measurement there.
                p.end_marks_to(base);
                return Err(err);
            }
            break;
        }
        specifiers += 1;
        if a_known_type_ends_the_type {
            break;
        }
    }

    if specifiers == 0 {
        p.close_marks_above(base);
        return Err(CppParseError::syntax_error_from(
            &t!("expected a type specifier"),
            p.current_token_range(),
        ));
    }

    Ok(m.complete(p))
}

/// The spelling MSVC gives a class key: `__interface I : IUnknown { … };`.
///
/// It is an identifier rather than a keyword, so every rule that recognises it matches on text — the same reading
/// `__try` gets in the statement grammar.
pub(crate) const THE_KEYWORD_INTERFACE_IS_SPELLED: &str = "__interface";

/// Which kind of class-like declaration does this keyword introduce?
pub fn class_like_kind(kind: CppTokenKind) -> Option<CppSyntaxKind> {
    match kind {
        CppTokenKind::ClassKeyword => Some(CppSyntaxKind::ClassDef),
        CppTokenKind::StructKeyword => Some(CppSyntaxKind::StructDef),
        CppTokenKind::UnionKeyword => Some(CppSyntaxKind::UnionDef),
        CppTokenKind::EnumKeyword => Some(CppSyntaxKind::EnumDef),
        _ => None,
    }
}

/// Is this a keyword that starts a class-like head, i.e. one where a `:` introduces a base clause
/// rather than a bit-field or a label?
pub fn is_class_like_keyword(kind: CppTokenKind) -> bool {
    class_like_kind(kind).is_some()
}

/// One specifier of a decl-specifier-seq. Returns `Err` without consuming anything when the
/// current token cannot start one, which is how the enclosing loop knows to stop.
///
/// This is a wrapper around [`parse_one_decl_specifier_inner`] whose only job is the bookkeeping
/// the inner function's dozen early returns would otherwise each have to remember: a specifier was
/// consumed, so [`name_joins_the_type`] must know it.
pub(crate) fn parse_one_decl_specifier(
    p: &mut CppParser,
    has_specifier: &mut bool,
    has_type_specifier: &mut bool,
    name_allowed: &mut bool,
    names_may_join: NamesMayJoin,
    a_class_definition_was_written: bool,
    specifier_seen: bool,
) -> ParseResult {
    let events_before = p.current_event_count();
    let result = parse_one_decl_specifier_inner(
        p,
        has_type_specifier,
        name_allowed,
        names_may_join,
        a_class_definition_was_written,
        specifier_seen,
    );

    if result.is_ok() {
        *has_specifier = true;

        // Did that specifier name a **type**? `alignas` is why the distinction exists: `alignas(16) MyType value;`
        // has consumed a specifier and named no type, so a flag meaning "something was consumed" made `MyType` a
        // second name of a finished type and left `value` with nowhere to be a declarator. See
        // [`name_joins_the_type`].
        //
        // Asked of the last token the specifier consumed, which is where its own name stands: a type keyword for
        // `int`, an identifier for a class name, `::` for a qualified one — and a `>` for `std::vector<int>`,
        // whose argument list the *name* rule consumes, so the specifier ends on the closing angle rather than on
        // the name. `alignas(16)` ends in `)`, so it answers no, which is the whole point of the distinction.
        //
        // Asking the *tokens* rather than threading a boolean through the dozen branches of the rule below is
        // deliberate: the branches that forgot the boolean would be the silent ones. Two were found this way and
        // neither by an `alignas` test — `std::vector<int> values;`, whose specifier ends on `>`, and `friend`,
        // whose payload is the whole declaration that follows it.
        //
        // `friend` is the one specifier the question cannot be asked of, and for the reason it is special: its
        // payload *is* the declaration that follows — `friend void swap(D&, D&);` — so by the time it returns, the
        // cursor is on the *next* member and "the last token consumed" describes that member's start rather than
        // this specifier. The answer there is "a type was named", because a friend declaration always names one,
        // and the flag it leaves is what the next member's declarator reads.
        if p.take_declaration_ended_inside_specifiers() {
            *has_type_specifier = true;
        } else {
            // A specifier that produced a **`BuiltinType` node** named a type, and saying so from the event
            // rather than from the last token is what closes the one case the token test cannot see.
            //
            // The token test reads "the last token this specifier consumed", and asks whether that token is a
            // name or a type keyword. For `decltype(a)` the last token is the `)` of its payload — the very same
            // token `alignas(16)` ends on, and the one the test exists to answer *no* for. So `decltype(a) x;`
            // left `has_type_specifier` false, [`name_joins_the_type`] answered "this name can only be the
            // type", and the declarator's name was taken into the type: the declaration came out with no
            // declarator, the declaration reading failed, and the statement fell back to an expression.
            //
            // `BuiltinType` is the marker because it is produced by exactly the branches that name a keyword
            // type — and a rule that *forgot* to set a flag is the mistake this whole function exists to avoid,
            // so the record is read back from what the rule actually produced.
            // **Sticky**, and that word is the whole of the fix for a defect this flag had for as long as it has
            // existed: the answer is "has a *type* been named in this sequence", not "did the specifier that just
            // ran name one". A cv-qualifier names no type, but it does not *unname* one either — and assigning
            // rather than accumulating let it do exactly that:
            //
            // ```text
            // char const w[]   after `const` the flag said "no type yet", so `w` could only *be* the type: the
            //                  type came out as `char const w`, the declarator was left with `[]`, and that became
            //                  a structured binding — no diagnostic at all. `char const w[2]` was the same
            //                  reading with a bound in it, which is where it was reported.
            // int x const …    the same shape in C++: `int const x = 1;` declared `x` as part of the type.
            // ```
            //
            // `char const* p` escaped it only because the `*` after the qualifier ends the specifier sequence
            // before any name is seen.
            //
            // Accumulating changes nothing for a specifier that *does* name a type (the disjunction already held
            // it true) and nothing for `alignas(16) MyType value;` (the flag was false before the alignment and
            // stays false after it, so `MyType` still joins).
            *has_type_specifier = *has_type_specifier
                || p.events_contain_any(events_before, &[CppSyntaxKind::BuiltinType])
                || p.last_consumed_token_kind().is_some_and(|kind| {
                    matches!(
                        kind,
                        CppTokenKind::Identifier | CppTokenKind::Scope | CppTokenKind::Greater
                    ) || is_type_specifier_keyword(kind)
                        || is_class_like_keyword(kind)
                });
        }
    }

    result
}

pub(crate) fn parse_one_decl_specifier_inner(
    p: &mut CppParser,
    has_type_specifier: &mut bool,
    name_allowed: &mut bool,
    names_may_join: NamesMayJoin,
    a_class_definition_was_written: bool,
    specifier_seen: bool,
) -> ParseResult {
    let base = p.open_marks();
    // Where this specifier begins, for the backward questions a name specifier has to ask — see
    // [`super::super::decls::type_name_at`]. `base` above cannot answer them: it is a marker-stack length, not a
    // position in the token stream.
    let type_start = p.anchor();

    match p.current_token() {
        // Attributes may be interleaved anywhere a specifier may appear — in either spelling, which is the whole
        // point of [`at_an_attribute`]: `extern "C++" __attribute__ ((…))` before a declaration is the same
        // position as `[[nodiscard]]` before one, and the standard library's headers are written with the former.
        // Checked before the name branch below, which would otherwise take the name for a type.
        _ if at_an_attribute(p) => {
            return parse_attribute_specifier(p);
        }

        // One of the **compiler's own keywords** where a specifier goes: `__extension__ inline int f();`,
        // `int __cdecl g(void);`, `__forceinline size_t h();`. They are consumed by the loop in
        // [`parse_decl_specifier_seq_with`] rather than here — see the note there for why the specifier reader
        // must not be the one to see them — so reaching this point with one at the cursor means the caller is a
        // context that does not go through that loop, and the keyword is read here.
        _ if an_implementation_keyword(p).is_some() => {
            return Ok(parse_an_implementation_keyword(p));
        }

        // **C23's `_BitInt(N)`** — an integer type whose width is a constant expression. It is a *contextual* keyword
        // (`_BitInt` is an ordinary identifier outside this position), so it is an identifier followed by a `(` and a
        // width, and the width is what tells it from a call: `_BitInt(37) wide;` declares a type, while `f(37)` is a
        // call. Measured on a nine-file C corpus: the last syntax error, in `c23.c`, which left the `struct bits` that
        // holds it unrecovered and took that struct's member answers with it.
        CppTokenKind::Identifier
            if p.current_token_text() == "_BitInt"
                && p.peek_next_token() == CppTokenKind::LeftParen
                && p.language().support(features::Feature::BitInt) =>
        {
            let m = p.mark(CppSyntaxKind::BuiltinType);
            p.bump(); // `_BitInt`
            expect_token(p, CppTokenKind::LeftParen)?;
            super::super::exprs::parse_assignment_expr(p)?;
            expect_token(p, CppTokenKind::RightParen)?;
            return Ok(m.complete(p));
        }

        // **C11's `_Atomic`** — a type specifier in both of its spellings, `_Atomic(int) n;` and `_Atomic int n;`.
        // Read here as a `BuiltinType` so the declaration has a type specifier: without this, `_Atomic(int)` was a
        // `MacroCall` in the specifier sequence, so the type of `n` was the *call* and `_Atomic int` a name.
        //
        // The parenthesised form carries a type name rather than an expression — `_Atomic(int)` is C11 6.7.2.4's
        // `_Atomic ( type-name )` — so the group is read with the type-id rule, not as a call's arguments.
        CppTokenKind::Identifier
            if p.current_token_text() == "_Atomic"
                && p.language().support(features::Feature::Atomic) =>
        {
            let m = p.mark(CppSyntaxKind::BuiltinType);
            p.bump(); // `_Atomic`
            if p.current_token() == CppTokenKind::LeftParen {
                p.bump(); // `(`
                parse_type_id(p)?;
                expect_token(p, CppTokenKind::RightParen)?;
            }
            return Ok(m.complete(p));
        }

        // A **type this target's compiler adds to the language**: `__int128`, `_Float16`, `__int64`. Read here
        // rather than in the skip above, because a `BuiltinType` is what tells the caller a type has been named —
        // see [`a_type_the_compiler_spells`].
        _ if a_type_the_compiler_spells(p) => {
            let m = p.mark(CppSyntaxKind::BuiltinType);
            p.bump();
            return Ok(m.complete(p));
        }
        // `enum class E`, `enum struct E` — the scoped-enum form. Checked before the general
        // class-like case because the second keyword is part of *this* specifier, not a new one.
        CppTokenKind::EnumKeyword if is_enum_class_head(p) => {
            let m = p.mark(CppSyntaxKind::BuiltinType);
            p.bump();
            p.bump();
            if p.current_token() == CppTokenKind::Identifier {
                // The enum's name is a name specifier, so it spends the allowance: a further name
                // is the declarator, as in `enum class E e;`.
                *name_allowed = false;
                // `enum class E { ... }` declares `E` to be a type, exactly as `class E { ... }` does — this
                // branch is a second spelling of the same head, so it has to record the same fact or a later
                // `E e(1);` is read as a call.
                let declared_name = p.current_token_text().to_string();
                if let Err(err) = parse_name(p) {
                    p.close_marks_above(base);
                    return Err(err);
                }
                p.declare_type_name(&declared_name);
            }

            // The underlying type is part of *this* specifier too: `enum class E : unsigned char`.
            // Leaving it to the specifier loop instead would make `unsigned char` a second
            // specifier of the same type, and the `{` after it would then be read as a body with no
            // declaration to belong to — which is how this spelling ended up as an `ErrorNode`.
            if p.current_token() == CppTokenKind::Colon {
                p.bump();
                if let Err(err) = parse_type_id(p) {
                    p.close_marks_above(base);
                    return Err(err);
                }
            }

            // `enum class E { ... }` — the body belongs to the specifier, exactly as it does for a
            // plain `enum E { ... }`.
            if p.current_token() == CppTokenKind::LeftBrace
                && let Err(err) = parse_enumerator_body(p)
            {
                p.close_marks_above(base);
                return Err(err);
            }

            return Ok(m.complete(p));
        }

        // **MSVC's `__interface`** — a class key spelled as an identifier, and structurally a class: a name, an
        // optional base clause, a body. (The compiler adds rules about the members — every one public and pure
        // virtual — and none of them changes how the declaration is *written*.) So the spelling is all this arm is
        // about: `parse_class_like_head` is the rule that already reads `class Name : Base { … }`, and it names the
        // node `ClassDef` because `class_like_kind` has no entry for an identifier.
        //
        // Without it the word is read as a name and the declaration becomes an expression:
        // `__interface I : IUnknown { … };` reported ``expected `;` after expression`` at the `:`.
        CppTokenKind::Identifier
            if p.current_token_text() == THE_KEYWORD_INTERFACE_IS_SPELLED
                && p.language().support(features::Feature::Interface)
                && a_body_follows_the_class_head(p) =>
        {
            return parse_class_like_head(p);
        }

        // A class-like *definition* head: `class Foo : public Bar { ... };`.
        //
        // Only when a body really follows. In a template parameter list, `class T` is a type
        // parameter and `template <class> class C` is a template template parameter — neither has a
        // body, and reading `template <class T, int N>` as the start of a class definition is how
        // the whole rest of the file ends up in an error node.
        kind if is_class_like_keyword(kind) && a_body_follows_the_class_head(p) => {
            return parse_class_like_head(p);
        }

        // The type half of a template parameter pack whose name follows the marker: `class... Ts`,
        // `typename... Rest`, `int... Ns`. Each of these keywords is a type specifier on its own, and the
        // specifier loop would otherwise keep going and ask the `...` to be one too — which is the
        // `expected a type specifier` these parameters used to report.
        //
        // Only the *unnamed* half is claimed here. `T...` is the other spelling — the name comes first and the
        // marker after it — and that is eaten by [`super::super::decls::parse_template_parameter`] once the declarator
        // has been parsed, so the two do not overlap.
        CppTokenKind::TypenameKeyword | CppTokenKind::ClassKeyword | CppTokenKind::Identifier
            if p.peek_next_token() == CppTokenKind::Ellipsis =>
        {
            let m = p.mark(CppSyntaxKind::TemplateType);
            p.bump();
            return Ok(m.complete(p));
        }

        // `typename T` in a template parameter list, with no name to introduce. The general branch
        // below expects a name after `typename` and would fail on the `,` that ends this parameter —
        // or on the `...` of a pack whose name comes after the marker, as in `typename... Rest`.
        CppTokenKind::TypenameKeyword
            if matches!(
                p.peek_token_kind_at(1..2).first(),
                Some(&CppTokenKind::Comma)
                    | Some(&CppTokenKind::Greater)
                    | Some(&CppTokenKind::Ellipsis)
            ) =>
        {
            let m = p.mark(CppSyntaxKind::TypenameType);
            p.bump();
            return Ok(m.complete(p));
        }

        // `decltype(expr)`, `decltype(auto)`, `noexcept(expr)` — a keyword with its own parenthesized payload.
        //
        // The type is *complete* once the payload is read, and that is recorded through the `BuiltinType` node
        // this branch produces rather than by a flag set here — see [`parse_one_decl_specifier`], where the
        // record is read back. The distinction matters because the payload ends on a `)`, the same token
        // `alignas(16)` ends on, so "did this specifier name a type?" cannot be asked of the last token alone.
        // Without the record, `has_type_specifier` stayed false, [`name_joins_the_type`] answered "this name can
        // only be the type", and the **declarator's** name was taken into the type: the declaration came out
        // with no declarator, the declaration reading failed, and the statement fell back to an expression. That
        // is why `decltype(a) x;` parsed while `decltype(a) x = 1;` did not — the difference was never the
        // initializer, it was the name.
        CppTokenKind::DecltypeKeyword | CppTokenKind::NoexceptKeyword => {
            let m = p.mark(CppSyntaxKind::BuiltinType);
            p.bump();
            if p.current_token() == CppTokenKind::LeftParen {
                expect_token(p, CppTokenKind::LeftParen)?;

                // The payload is an expression — `decltype(a + b)`, `noexcept(f())` — with **two exceptions**: a
                // *type*, which C23's `typeof` and GNU's `__typeof__` both allow, and `auto`, which is the same thing
                // written with a keyword no expression rule accepts.
                //
                // Measured on a nine-file C corpus: `typeof(struct point) origin = {3, 4};` reported `expected primary
                // expression` at the `struct` — four of `c23.c`'s syntax errors and two of `gnu.c`'s, and they took
                // every member and type answer about those files down with them.
                //
                // The two readings are told apart by the tokens that cannot begin an expression: a builtin type
                // specifier, an elaborated specifier (`struct`, `union`, `enum`, `class`), or a cv-qualifier. A name,
                // a literal and a `(` all begin expressions and stay on that side.
                if p.current_token() == CppTokenKind::AutoKeyword
                    || (p.language().support(features::Feature::Typeof) && starts_a_type_id(p))
                {
                    parse_type_id(p)?;
                } else {
                    super::super::exprs::parse_expr(p)?;
                }

                // **The payload's end is the matching `)`, and that is true however the expression read went.**
                //
                // A body that is not an expression stops the expression rule early — it reads what it can and
                // returns — and the `)` is then not where the rule left the cursor. That is not an error worth
                // failing the declaration for: the parentheses are balanced, so this type specifier has a
                // well-defined end, and what stands between is what the file wrote.
                //
                // Measured on two lines. `_STD` is a macro — `yvals.h` defines it as `::std::` — and a file read on
                // its own has not seen that header, so it arrives as a plain identifier and `_STD x` is two
                // identifiers in a row:
                //
                // ```cpp
                // using d = decltype(_STD x);
                // struct After { int c; };   // read, and unreachable
                // ```
                //
                // The alias failed, its markers were resolved by the error path, and `After` ended up **inside**
                // the `UsingDecl` rather than beside it — a `Declaration` in the event stream and nothing in the
                // tree. MSVC's `__msvc_iter_core.hpp` writes the same shape at line 116, and `bits/*` throughout.
                if !matches!(p.current_token(), CppTokenKind::RightParen) {
                    skip_to_the_closing_paren(p);
                }
                expect_token(p, CppTokenKind::RightParen)?;

                // **A name looked up in the type `decltype` denotes**: `decltype(f())::type`, the form
                // `<iterator>` and `<ranges>` write their return types in. The `::` and what follows it are part of
                // the same type specifier — read as a separate one the declaration ended at the `)`, and in a class
                // body that was silent: the rest of the member list became the tail of the declaration.
                if p.current_token() == CppTokenKind::Scope
                    && matches!(
                        p.peek_next_token(),
                        CppTokenKind::Identifier | CppTokenKind::TemplateKeyword
                    )
                    && let Err(err) = parse_name(p)
                {
                    p.close_marks_above(base);
                    return Err(err);
                }
            }
            return Ok(m.complete(p));
        }

        // `typename T::type` — the disambiguator for dependent names.
        CppTokenKind::TypenameKeyword => {
            let m = p.mark(CppSyntaxKind::TypenameType);
            p.bump();
            // **A type splice** (C++26, P2996): `typename [: r :]` — the type the reflection `r` represents. The
            // colon after the bracket is what tells it from every other `[` after `typename` (an array bound, a
            // lambda, an attribute), so it is a two-token test and nothing is guessed.
            if p.current_token() == CppTokenKind::LeftBracket
                && p.peek_next_token() == CppTokenKind::Colon
            {
                *name_allowed = false;
                if let Err(err) = grammar::cpp::exprs::parse_splice_expression(p) {
                    p.close_marks_above(base);
                    return Err(err);
                }
                // **A splice specialization** (C++26, P2996): `typename [: r :] <int>` — the reflection `r`, with
                // template arguments applied to it. No ambiguity is possible here, unlike `Name <`: a type cannot be
                // followed by a comparison operator, so a `<` after the `:]` is always the argument list.
                if p.current_token() == CppTokenKind::Less
                    && let Err(err) = parse_template_argument_list(p)
                {
                    p.close_marks_above(base);
                    return Err(err);
                }
                return Ok(m.complete(p));
            }
            if !definitely_ends_a_type(p.current_token()) {
                // The name after `typename` is part of this specifier, so a further name is the
                // declarator, as in `typename T::value_type v;`.
                *name_allowed = false;
                if let Err(err) = parse_name(p) {
                    p.close_marks_above(base);
                    return Err(err);
                }
            }
            return Ok(m.complete(p));
        }

        kind if is_type_specifier_keyword(kind) => {
            let m = p.mark(CppSyntaxKind::BuiltinType);
            p.bump();
            return Ok(m.complete(p));
        }

        // **`volatile` is its own qualifier node**, and it used to be a `ConstQual`: the two share one rule here, so
        // every reader that asked the tree "is this const" was told yes for a `volatile` declaration, and
        // `volatile S v; auto x = v;` typed `x` as `const S` (`docs/worklog.md` P1-3). The kinds are
        // separate because the questions are: `const` says assignment is not allowed, `volatile` says every access is
        // observable, and a member list can hold `f()`, `f() const` and `f() volatile` at once.
        CppTokenKind::ConstKeyword | CppTokenKind::VolatileKeyword => {
            let kind = match p.current_token() {
                CppTokenKind::VolatileKeyword => CppSyntaxKind::VolatileQual,
                _ => CppSyntaxKind::ConstQual,
            };
            let m = p.mark(kind);
            p.bump();
            return Ok(m.complete(p));
        }

        // `friend` is a declaration of its own, not a specifier of one: `friend class X;` declares
        // `X` to be a friend, it does not declare a class. Wrapping the whole thing keeps the
        // declaration node from claiming the friend is a variable of type `void`.
        CppTokenKind::FriendKeyword => {
            let m = p.mark(CppSyntaxKind::FriendDecl);
            p.bump();
            if let Err(err) = super::super::decls::parse_declaration(p) {
                p.close_marks_above(base);
                return Err(err);
            }
            // `friend` is the one specifier whose payload *is* the declaration that follows it, its `;`
            // included. Saying so is what lets the declaration rule *around* it stop when the specifiers end,
            // rather than looking for an init-declarator that is not there and rewinding over the member.
            p.note_declaration_ended_inside_specifiers();
            return Ok(m.complete(p));
        }

        // Storage-class and function specifiers: `static`, `extern`, `inline`, `constexpr`, ...
        //
        // These say nothing about the *type*, so they are their own node rather than part of the
        // type specifier — but they must be accepted here or the specifier loop stops before the
        // type and every declaration carrying one is misread. `inline constexpr int kMax = 16;` is
        // the common case: without this the loop gives up at `inline`, the declaration is not
        // recognised, and the tokens are re-read as an expression.
        kind if storage_or_function_specifier(kind).is_some() => {
            let m = p.mark(storage_or_function_specifier(kind).expect("checked by the guard"));
            p.bump();

            // A linkage specification — `extern "C" void f();` — is a *declaration*, not a specifier, so it is
            // claimed by [`super::super::decls::parse_linkage_specification`] before the specifier loop is ever
            // reached. What can still arrive here is an `extern` whose string came after something else, which
            // is not valid C++; consuming the string keeps the tokens in the tree rather than spinning on it.
            if kind == CppTokenKind::ExternKeyword
                && p.current_token() == CppTokenKind::StringLiteral
            {
                p.bump();
            }

            // `explicit` may carry a condition in C++20: `explicit(false) T(int);`.
            if p.current_token() == CppTokenKind::LeftParen {
                p.bump();
                if let Err(err) = super::super::exprs::parse_expr(p) {
                    p.close_marks_above(base);
                    return Err(err);
                }
                expect_token(p, CppTokenKind::RightParen)?;
            }

            return Ok(m.complete(p));
        }

        // `alignas(16)`, `alignas(int)`, `alignas(64) struct A { };`.
        //
        // A specifier of its own rather than a type specifier: it says nothing about the *type*, and putting it
        // in [`is_type_specifier_keyword`] would make `can_begin_a_type` claim a declaration that begins with it
        // — which is asked in three places that are all about types. The specifier loop is the one place it has
        // to be accepted, and the payload is what makes it a node rather than a keyword.
        CppTokenKind::AlignasKeyword => {
            let m = p.mark(CppSyntaxKind::AlignasSpec);
            p.bump(); // `alignas`

            // The parentheses are required by the grammar, and a missing one is reported rather than skipped:
            // `alignas 16 struct A { };` is not a thing, and consuming the `16` as if it were would leave the
            // declaration to fail somewhere further along with a message about the wrong token.
            expect_token(p, CppTokenKind::LeftParen)?;

            // The payload is a constant-expression **or** a type-id, and the two overlap completely on the
            // tokens: `alignas(16)` is a value and `alignas(int)` a type, while `alignas(alignof(int))` is a
            // value again. Read by the same rule `sizeof(...)` uses, which is the same ambiguity and the same
            // answer — try the type first, because a type-id is the reading that can be *refused*, and read an
            // expression when it is. That rule is shared rather than copied, so this cannot drift from it.
            if let Err(err) = super::super::exprs::parse_type_id_or_expression(p) {
                p.close_marks_above(base);
                return Err(err);
            }
            expect_token(p, CppTokenKind::RightParen)?;

            return Ok(m.complete(p));
        }

        // An `operator` name where a type specifier would go. It is never one: `operator` names a *function*,
        // and the whole point of the spelling is that it stands where a declarator's name stands. Letting the
        // specifier loop take it — which the "a name may join the type" rule does, since `operator` arrives as
        // an ordinary identifier — left the conversion operator of `explicit operator bool() const` split
        // across an error node and a declaration of a type called `bool`.
        //
        // `foo operator+(a, b)` is the reason this is not simply "any declaration may start with `operator`":
        // there a user-defined type could legitimately be named `operator`, and taking the keyword away from
        // the loop only costs the declaration reading of that shape. It is a C-with-classes idiom, not a C++
        // one, and it is the trade this grammar makes everywhere else too.
        CppTokenKind::OperatorKeyword if specifier_seen => {}

        // The `~` of a destructor name in a *qualified* declaration: `Foo::~Foo`, `ns::C::~C`.
        //
        // Part of a name rather than a specifier, and it arrives here in the middle of one: the loop above walks
        // a qualified name one segment per iteration, so `Foo::` has already been consumed as the type when the
        // tilde shows up. Accepting it continues the same name, which is what makes an out-of-line destructor a
        // declaration of `Foo::~Foo` instead of a name-less declaration with an error node where the tilde was.
        //
        // The condition is **the token just consumed is the `::`**, and it used to be "a type name is already in
        // hand". That older test was true of `Foo::~Foo` and false of an ordinary destructor only as long as a
        // type name meant a *type*: an unexpanded macro is a name specifier too, so `bits/stl_vector.h:372`
        //
        // ```cpp
        // _GLIBCXX20_CONSTEXPR          // an unknown macro: it joins the type, and it is a "type name" by that test
        // ~_Vector_base() _GLIBCXX_NOEXCEPT
        // { _M_deallocate(_M_impl._M_start, _M_impl._M_end_of_storage - _M_impl._M_start); }
        // ```
        //
        // had its destructor read as a *further name of the type*: `~_Vector_base` became a `TemplateType`,
        // the `()` after it had no rule, and the class — `_Vector_base`, and with it everything `std::vector`
        // inherits — collapsed into error nodes from there to the end of the file.
        //
        // `::` is the whole difference and it is what the grammar says: a destructor's name is qualified in a
        // definition written outside the class and unqualified inside it. Nothing else can precede a `~` with a
        // name in hand. `virtual ~Shape();` still never reaches this arm — there the last token is `virtual`, and
        // the declarator's own [`parse_destructor_name`] claims the pair.
        CppTokenKind::Tilde if p.last_consumed_token_kind() == Some(CppTokenKind::Scope) => {
            let m = p.mark(CppSyntaxKind::TemplateType);
            if let Err(err) = parse_name(p) {
                p.close_marks_above(base);
                return Err(err);
            }
            return Ok(m.complete(p));
        }

        // A qualified or unqualified name, possibly a template-id. This is the case that needs a
        // symbol table to be certain about; the grammar accepts it and lets the caller decide.
        CppTokenKind::Identifier | CppTokenKind::Scope => {
            // **`requires` after a parameter list is a constraint, not a word of the type**: `V(const V&) requires
            // C<T> = default;` has no type at all — the specifier sequence ends where the clause begins.
            if has_a_clause_after_a_parameter_list(p) {
                return Err(CppParseError::syntax_error_from(
                    &t!("expected a declarator name"),
                    p.current_token_range(),
                ));
            }

            // A name specifier ends the type when it cannot be part of it. Two forms are ambiguous
            // from the tokens alone, and both are grammatical:
            //
            //     T x        // a type `T` and the declarator `x`
            //     std::vector<int> v
            //
            // The rule below resolves them the way the corpus needs. A name may join the type only
            // while no *name* has joined it yet and the specifier before it is not already a
            // complete type; the `::` of a qualified name is walked by this loop one segment at a
            // time, so each later segment is still the same name.
            let joins = name_joins_the_type(
                p,
                *has_type_specifier,
                *name_allowed,
                names_may_join,
                a_class_definition_was_written,
            );
            if !joins {
                return Err(CppParseError::syntax_error_from(
                    &t!("expected a declarator name"),
                    p.current_token_range(),
                ));
            }

            // A name that ends here has joined the type; the allowance is spent so the next one is
            // read as the declarator.
            *name_allowed = false;

            // The first name in type position is the type, and it is worth remembering — see the note where the
            // loop begins. The whole name is reported, not the first token of it, because a qualified type has to
            // be recorded under the name a *lookup* would use: `std::string` is the type `string`, qualified.
            p.note_declaration_type_name(super::super::decls::type_name_at(p, type_start));

            // **A plain name is a `NamedType`; a template-id is a `TemplateType`.**
            //
            // This is what `CppDeclSpecifierSeq::get_type_name`'s own note describes — *"a plain name is a `NameExpr`
            // child, while a template-id (`std::vector<int>`) is wrapped in a `TemplateType` node with the `NameExpr`
            // inside it"* — and the implementation wrapped **both** in `TemplateType`. That is why `String` in
            // `struct String;` came out as a template instantiation with no `<` anywhere: measured from the tree the
            // report started with,
            //
            // ```text
            //   struct String;
            //     DeclSpecifierSeq > BuiltinType "struct " > TemplateType "String" > NameExpr "String"
            // ```
            //
            // The kind was answering "is this a template instantiation?" with *yes* for every named type in a
            // specifier sequence — a claim a reader matching on kind would believe, and one this parser paid for four
            // times over.
            //
            // **A name still gets a node**, and that is not a detail: leaving it a bare `NameExpr` failed four tests
            // at once, because the parser itself asks whether a *type name* has been written by looking for
            // [`CppSyntaxKind::TemplateType`] among the events — see `a_further_name_may_join`. So the plain case gets
            // [`CppSyntaxKind::NamedType`], and the two questions stay separate: "is a type named here" and "is it a
            // template-id".
            //
            // Which of the two it is comes from what `parse_name` produced, not from looking ahead: it consumes the
            // `<…>` when there is one.
            let events_before = p.current_event_count();
            let mut m = p.mark(CppSyntaxKind::TemplateType);
            if let Err(err) = parse_name(p) {
                p.close_marks_above(base);
                return Err(err);
            }
            if !p.events_contain_any(events_before, &[CppSyntaxKind::TemplateArgumentList]) {
                m.set_kind(p, CppSyntaxKind::NamedType);
            }
            return Ok(m.complete(p));
        }

        _ => {}
    }

    Err(CppParseError::syntax_error_from(
        &t!("expected a type specifier"),
        p.current_token_range(),
    ))
}

/// Does the name at the cursor continue a qualified name, i.e. is it followed by `::`?
///
/// `std::vector<int>` reaches `parse_decl_specifier_seq` in pieces, because
/// [`parse_name`](parse_name) stops after each segment: the specifier loop is what walks the `::`.
/// Without this, the "two adjacent names" guard would fire in the middle of a perfectly ordinary
/// How much does this token move the **template angle depth** a lookahead scan is keeping?
///
/// `<` opens one list and `>` closes one — but the lexer has already glued `>>` into a single `RightShift`
/// token, because it cannot know which it is: in `a >> b` the token really is a shift, and in `Base<K,
/// std::shared_ptr<V>>` it is two closers. Only the parser's context can tell them apart, so one *token* can
/// close **two** lists and every scan that counts angles has to say so.
///
/// Three scans count angles and each wrote the rule out for itself; this is what happens when the copies drift.
/// `a_body_follows_the_class_head` counted `Greater` alone, so in `class D : public Base<K, std::shared_ptr<V>> {`
/// the depth was still one when the scanner reached the `{`, the head was read as *not* opening a body, and the
/// class definition failed with `expected ;` against its own name — while the same base clause with a
/// single-level argument list parsed fine. That is maintenance convention #14: the
/// second use of a predicate is where the exception gets forgotten, so it is extracted the second time.
///
/// **The `previous` argument is the second thing the copies drifted on**, and it is why the rule takes one now. A
/// `<` opens a level only when the token in front of it can *end a template-name* — a name, or the `>` of an inner
/// list — because that is how the language writes one:
///
/// ```text
/// ::std::vector<int>       the `<` after a name       → an inner list
/// C<1 < 2>                 …after a literal           → the less-than operator
/// C<sizeof(T) < 3>         …after a `)`               → the same
/// C<(T(0) < T(0))>         …inside a group            → the same
/// ```
///
/// Counting every `<` is what hid the list's **own** `>` from all three scans: the depth was one too deep at the
/// end, so the scan reported "no matching `>`" and the construct was not a template-id (a base clause, a
/// declarator's name) or not a body — for `struct S : public __bool_constant<_Tp(-1) < _Tp(0)> { }`, which is
/// `type_traits:987`, all three at once. g++ reads the three comparison spellings as arguments and the first as a
/// nested template-id, which is exactly what this test produces.
pub(crate) fn angle_depth_delta(previous: CppTokenKind, kind: CppTokenKind) -> isize {
    match kind {
        CppTokenKind::Less if can_end_a_template_name(previous) => 1,
        CppTokenKind::Greater => -1,
        // One token, two lists closed.
        CppTokenKind::RightShift => -2,
        _ => 0,
    }
}

/// Could this token be the last one written of a **template-name** — the thing a `<` binds to?
///
/// See [`angle_depth_delta`], which is the only caller that needs the question asked of the token *before* the
/// one it is looking at.
pub(crate) fn can_end_a_template_name(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Identifier | CppTokenKind::Greater | CppTokenKind::RightShift
    )
}

/// qualified type.
pub(crate) fn continues_a_qualified_name(p: &CppParser) -> bool {
    // Scan forward over a name and its template arguments, then check for `::`.
    let mut depth = 0isize;
    let mut offset = 1usize;
    // The token before the one being looked at — see [`angle_depth_delta`]. The scan starts just past the name,
    // which can end a template-name, so a `<` right there opens a list.
    let mut previous = CppTokenKind::Identifier;

    loop {
        let Some(kind) = p.peek_token_kind_at(offset..offset + 1).first().copied() else {
            return false;
        };

        match kind {
            CppTokenKind::Scope if depth <= 0 => {
                // A `::` that a `*` follows is the **pointer-to-member operator**, not a name continuation:
                //
                // ```text
                // int C::*p;        a pointer to a member of `C`, named `p`
                // int A::B *p;      a pointer `p` to the type `A::B`
                // ```
                //
                // Both start with a name, then `::`, so only what follows the `::` tells them apart — and saying
                // "yes" to the first made `C::` part of the *type*: the specifier sequence took it, the `*p` that
                // followed became a declarator **inside the type node**, and the declaration came out lossless,
                // well formed, diagnostic-free and wrong. `a_pointer_to_member_operator_is_here` reads the
                // operator itself; see [`parse_abstract_declarator`].
                return p
                    .peek_token_kind_at(offset + 1..offset + 2)
                    .first()
                    .copied()
                    != Some(CppTokenKind::Star);
            }
            // Anything that ends a name without a following `::`.
            //
            // **`[` is one of them**, and leaving it out is what made `static bool t[::g + 1];` unreadable.
            // Without it the scan *stepped over* the bracket, found the `::` of the bound, and answered "this
            // name continues as a qualified name" — so `t` joined the type instead of naming the declarator, the
            // `[::g + 1]` that followed had no declarator to belong to, and the declaration was refused at its
            // own name (``expected `;` ``, measured). The bound's `::` belongs to the *bound*: an array suffix
            // ends the name, and no type is spelled `T[::N::M]`.
            //
            // The same bracket is why `<chrono>`'s and `<filesystem>`'s `T x[::std::f<int>() + 1]` needed the
            // whole scan to stop here — the argument list inside the bound is reached only through the `[`.
            CppTokenKind::Comma
            | CppTokenKind::Semicolon
            // **A single `:` ends the name.** A qualified name is written with `::` and never with `:`, and the
            // colon here is the one a **range-for header** (or a bit-field) puts between the declarator and what
            // follows it: `for (int value : std::views::iota(1, 6))`. Stepping over it found the `::` of the *range
            // expression*, answered "this name continues as a qualified name", and `value` joined the type — leaving
            // the declaration with no declarator, so the header fell back to the C-style reading and reported
            // `expected primary expression` against `int`. Measured on the C++ corpus: four range-for headers, all of
            // them spelled with a `views::` name after the colon.
            | CppTokenKind::Colon
            | CppTokenKind::Question
            | CppTokenKind::LeftBrace
            | CppTokenKind::RightBrace
            | CppTokenKind::LeftParen
            // **`(` and `)` both end the name, and `)` is the one this list was missing.** A parameter is decided
            // inside its own parentheses, and the `::` of a *trailing return type* is on the other side of the
            // closing one:
            //
            // ```cpp
            // auto f(std::string_view value) -> std::string;   // `value` is the declarator, not another type
            // auto f(std::string_view value) -> int;           // …and this one always read correctly
            // ```
            //
            // Stepping over the `)` found the `::` of `std::string`, answered "this name continues as a qualified
            // name", and `value` joined the specifier sequence — so the parameter had no declarator, the scope tree
            // bound no name, and `unresolved-call-or-receiver` reported the parameter as undeclared on correct code.
            // Measured: every function written `auto … -> a::qualified::name` lost **all** of its parameter names.
            // A qualified name never has a `)` between two of its segments, so nothing valid is refused here.
            | CppTokenKind::RightParen
            | CppTokenKind::LeftBracket
            | CppTokenKind::RightBracket
            | CppTokenKind::Assign
            | CppTokenKind::Eof
            | CppTokenKind::None => return false,
            kind => depth += angle_depth_delta(previous, kind),
        }

        previous = kind;
        offset += 1;
    }
}

/// Does a **pointer-to-member operator** start `at` tokens past the cursor: `C::*`, or a longer nested name
/// `A::B::*` — and how many tokens does it span?
///
/// The `::` of a nested-name-specifier normally continues a name — `A::B` is a type — and only a `*` right after
/// it makes the whole thing an operator. So the test is written as the walk itself: names separated by `::`, and
/// the last of those `::` followed by a `*`. `A::B *p` fails it at the last step (the token after the final `::`
/// is a name, not a `*`), which is exactly the type-then-pointer spelling it has to stay.
///
/// The length is returned rather than a bare `true` because the callers look at what *follows* the operator:
/// [`a_parenthesised_declarator_with_a_name_follows`] asks whether a name and a `)` close the group.
pub(crate) fn pointer_to_member_operator_length(p: &CppParser, at: usize) -> Option<usize> {
    let mut offset = at;
    loop {
        if p.peek_token_kind_at(offset..offset + 1).first().copied()
            != Some(CppTokenKind::Identifier)
        {
            return None;
        }
        offset += 1;

        if p.peek_token_kind_at(offset..offset + 1).first().copied() != Some(CppTokenKind::Scope) {
            return None;
        }
        offset += 1;

        if p.peek_token_kind_at(offset..offset + 1).first().copied() == Some(CppTokenKind::Star) {
            return Some(offset + 1 - at);
        }
    }
}
