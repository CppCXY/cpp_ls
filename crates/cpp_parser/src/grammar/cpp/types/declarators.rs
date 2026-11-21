//! **Declarators** — declarators: pointers, references, arrays, parameter lists, and the suffixes after them.
//!
//! Moved here from `types.rs` without a change.

use super::*;

/// Does the `(` at the cursor open a *parenthesised abstract declarator* rather than a parameter list or a
/// parenthesized name?
///
/// The question is answered by the token after the `(`, and it has to be asked before the parenthesis is
/// consumed because the readings are all grammatical and go to different rules:
///
/// ```text
/// void (*)(int)     a parenthesised abstract declarator — a pointer to a function
/// int (x)           a parenthesized *name* — the declarator's own parentheses
/// int (int)         a parameter list — a function type
/// ```
///
/// Only a pointer or reference operator inside the parentheses makes it the first: nothing else can be
/// **The length of an attribute group at `from`**, including both ends of its own token span — `None` when there is
/// none there.
///
/// `__attribute__((__stdcall__))` is an identifier, a `(`, a balanced run of tokens and the `)` that closes it;
/// `__declspec(noreturn)` is the same shape with one level; `[[gnu::always_inline]]` is the C++ spelling. Every one
/// of them can stand **inside a declarator**, which is the case this exists for.
pub(crate) fn attribute_group_length(p: &CppParser, from: usize) -> Option<usize> {
    // **The C++ spelling** — `[[gnu::always_inline]]`, two brackets at each end.
    if p.peek_token_kind_at(from..from + 2).as_slice()
        == [CppTokenKind::LeftBracket, CppTokenKind::LeftBracket]
    {
        let mut at = from + 2;
        while at < from + 128 {
            if p.peek_token_kind_at(at..at + 2).as_slice()
                == [CppTokenKind::RightBracket, CppTokenKind::RightBracket]
            {
                return Some(at + 2 - from);
            }
            at += 1;
        }
        return None;
    }

    // **The GNU and MSVC spelling** — a keyword and one balanced parenthesis group. The attribute's own arguments
    // may hold further parentheses, so the length is the one the matching `)` closes.
    let keyword = p.peek_token_text_at(from);
    if !matches!(keyword, "__attribute__" | "__attribute" | "__declspec") {
        return None;
    }
    if p.peek_token_kind_at(from + 1..from + 2).as_slice() != [CppTokenKind::LeftParen] {
        return None;
    }

    let mut depth = 0usize;
    let mut at = from + 1;
    while at < from + 256 {
        match p.peek_token_kind_at(at..at + 1).as_slice() {
            [CppTokenKind::LeftParen] => depth += 1,
            [CppTokenKind::RightParen] => {
                depth -= 1;
                if depth == 0 {
                    return Some(at + 1 - from);
                }
            }
            _ => {}
        }
        at += 1;
    }
    None
}

/// wrapped in them without a name, since a declarator's parentheses have to *hold* a declarator. `int (int)`
/// reaches this with a type keyword inside, is refused here, and stays a parameter list; `int (x)` reaches it
/// with a name, is refused for the same reason, and stays a name.
pub(crate) fn a_parenthesised_abstract_declarator_follows(p: &CppParser) -> bool {
    matches!(
        p.peek_token_kind_at(1..2).first(),
        Some(&CppTokenKind::Star)
            | Some(&CppTokenKind::Ampersand)
            | Some(&CppTokenKind::LogicalAnd)
            // `(::*)` — a pointer to member, where the operator comes after the class name.
            | Some(&CppTokenKind::Scope)
    )
    // `(C::*)` — the same operator with its class name written out, and no name after it: an *unnamed* parameter
    // of member-pointer-to-function type, `void h(int (C::*)(int));`. The named spelling `(C::*h)` never reaches
    // here — [`a_parenthesised_declarator_with_a_name_follows`] claims it first.
    || pointer_to_member_operator_length(p, 1).is_some()
    // …and the same pointer-to-member group with a **calling convention in front of the class name** —
    // `( __cdecl A :: * )`, which is how MSVC's `type_traits` writes every member-pointer predicate:
    //
    // `cpp
    // #define _IS_MEMFUNPTR(CALL_OPT, CV_OPT, REF_OPT, NOEXCEPT_OPT)                       \
    //     template <class _Ret, class _Arg0, class... _Types>                               \
    //     struct _Is_memfunptr<_Ret (CALL_OPT _Arg0::*)(_Types...) CV_OPT REF_OPT ...>      \
    //         : _Arg_types<CV_OPT _Arg0*, _Types...> { ... }                                // type_traits:406
    // `
    //
    // `CALL_OPT` is `__cdecl`/`__stdcall`/empty, so the class name stands **one token further along** than
    // the line above looks, and the group had no reading at all: the type-id ended at the return type, the
    // argument reader wanted a `,` or a `>` and found a `(`, and the whole specialisation was reported as
    // `expected a template argument`. The follower is the group's own ), which is the only thing that can
    // stand there — the *named* spelling `( __cdecl A :: * p )` is claimed by the named predicate, whose arm
    // requires the name for exactly that reason.
    //
    // It took better evidence to see this one: in the per-file reading a macro chain kept the calling convention
    // unexpanded, so the tokens were the ordinary names and the shape was never reached. The one-walk
    // `TranslationUnit` expands what the compiler expands, and 	type_traits:409 became that file's first error.
    || (written_like_a_macro(p.peek_token_text_at(1))
        && pointer_to_member_operator_length(p, 2).is_some_and(|length| {
            p.peek_token_kind_at(2 + length..3 + length) == [CppTokenKind::RightParen]
        }))    // …and a **calling convention in front of the operator**, with no name after it: `( __cdecl * )`. That is the
    // abstract spelling of `int (__cdecl * _onexit_t)(void)`, which a `using` alias writes —
    // `using new_handler = void (__cdecl *)();` (`new:111`), and the same type in `type_traits`. The named
    // spelling reaches [`a_parenthesised_declarator_with_a_name_follows`] instead; here the group has no name,
    // and the macro's **spelling** is what keeps an ordinary `(x *)` out, exactly as it does there.
    // The **follower** is required, and it is not decoration: `(_Rng&&)` is a *parameter list* holding one
    // parameter whose name happens to be written the way an implementation macro is (`_Rng`), and claiming it
    // turned three libstdc++ headers into rubble — `template<borrowed_range _Rng> subrange(_Rng&&)` reported
    // ``expected ), but get identifier``. A calling convention decorates something, so the group it stands in is
    // always followed by the parameter list of what it decorates.
    || (matches!(
        p.peek_token_kind_at(1..4).as_slice(),
        [
            CppTokenKind::Identifier,
            CppTokenKind::Star | CppTokenKind::Ampersand | CppTokenKind::LogicalAnd,
            CppTokenKind::RightParen
        ]
    ) && p.peek_token_kind_at(4..5) == [CppTokenKind::LeftParen]
        && written_like_a_macro(p.peek_token_text_at(1)))
}

/// Does the `(` at the cursor wrap a declarator that has a **name** in it: `(*f)(int)`, `(&f)(int)`?
///
/// The form [`parse_declarator_with`] has to claim for itself. Without it the parentheses are read as an
/// abstract declarator by [`parse_abstract_declarator`], which parses `*` and `(int)` — a perfectly good
/// pointer-to-function *type* — and then finds `f` where it wanted only a `)`. The result was `expected ), but
/// get identifier` against every classic C function-pointer declaration:
///
/// ```text
/// typedef void (*OldCallback)(int);
/// int (*signal(int sig))(int);
/// void (*handlers[4])(int) = {};
/// ```
///
/// # Why the token after the name decides it
///
/// The name has to be the *last* thing inside the parentheses — `(*f)` — because that is the only shape where
/// the parentheses exist to bind the `*` to `f`: `(* const)` has no name, and `(int)` has a type, and both are
/// something else. The `(` of a parameter list also has a name somewhere inside it — `void f(int x)` — but
/// never immediately followed by the `)` that closes the group, which is why the scan asks about the token
/// after the name rather than looking for one.
pub(crate) fn a_parenthesised_declarator_with_a_name_follows(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::LeftParen {
        return false;
    }
    // **An attribute before the pointer** — `typedef int (__attribute__((__stdcall__)) *FN)(int);`, which is how
    // every MinGW typedef of a calling-convention function pointer is written (`combaseapi.h` has two of them, and
    // they were the last two parse errors the GCC 15 closure produced). The group begins with the attribute rather
    // than with the `*`, so the arm above cannot see it — and the group has to be claimed, because its other
    // reading is a parameter list whose first parameter is `__attribute__((__stdcall__)) *FN`, which is not a
    // parameter at all.
    if let Some(length) = attribute_group_length(p, 1) {
        let after = 1 + length;
        return matches!(
            p.peek_token_kind_at(after..after + 3).as_slice(),
            [
                CppTokenKind::Star
                    | CppTokenKind::Ampersand
                    | CppTokenKind::LogicalAnd
                    | CppTokenKind::Caret,
                CppTokenKind::Identifier,
                CppTokenKind::RightParen
            ]
        );
    }

    if matches!(
        p.peek_token_kind_at(1..4).as_slice(),
        [
            CppTokenKind::Star
                | CppTokenKind::Ampersand
                | CppTokenKind::LogicalAnd
                | CppTokenKind::Caret,
            CppTokenKind::Identifier,
            CppTokenKind::RightParen
        ]
    ) {
        return true;
    }

    // **A group whose inner declarator is a function** — `(*f())`, and not the `(*f)(int)` shape above.
    //
    // This is what a macro standing for a variable produces: MinGW's `#define environ (*__p__environ())` turns
    // redis's `extern char **environ;` into `extern char **(*__p__environ());`, and the group closes on `;` rather
    // than on a parameter list. Refused here, the group was read as an *abstract* pointer-to-function type, whose
    // reading wants a parameter list after the `)` — there is none, the whole declaration failed ("expected primary
    // expression"), and because the damage lands on the line after the includes it looked like the includer's fault
    // (`sentinel.c:26`, `server.c:2620`; `docs/worklog.md`). `gcc -std=gnu11` accepts the shape.
    //
    // The `(` after the name is the whole test, and it is also what keeps `(*f)(int)` on the arm above: there the
    // token after the name is the group's `)`.
    if matches!(
        p.peek_token_kind_at(1..4).as_slice(),
        [
            CppTokenKind::Star
                | CppTokenKind::Ampersand
                | CppTokenKind::LogicalAnd
                | CppTokenKind::Caret,
            CppTokenKind::Identifier,
            CppTokenKind::LeftParen
        ]
    ) {
        return true;
    }

    // **A bare name whose group is followed by a parameter list** — the declarator spelling neovim's
    // `src/lpeg/lptree.c:1308` writes, as a parameter of a callback-taking function:
    //
    // ```c
    // static void createcat (lua_State *L, const char *catname, int (catf) (int))
    // ```
    //
    // `(catf)` holds a declarator and no type, so it is neither a parameter list nor a type-id; the `(int)` after
    // it is the parameter list the parenthesised name declares. Neither reading of the pair is reachable any
    // other way — a parameter list is never followed by another `(` in the same parameter, and a type-id's
    // parentheses cannot contain a *named* declarator — so claiming it takes nothing from the shapes around it.
    //
    // Measured: without this the group was left as `(catf)`'s tokens between two error nodes, the declaration's
    // parentheses never closed, and the whole definition — body included — was read as one expression statement.
    if matches!(
        p.peek_token_kind_at(1..4).as_slice(),
        [
            CppTokenKind::Identifier,
            CppTokenKind::RightParen,
            CppTokenKind::LeftParen
        ]
    ) {
        return true;
    }

    // `(WINAPI PM_OPEN_PROC)` — a **macro before the name**, which is how the COM headers write a calling
    // convention, and the group is followed by the parameter list that belongs to it:
    //
    // ```cpp
    // typedef DWORD (WINAPI PM_OPEN_PROC)(LPWSTR);          // winperf.h:180 — `WINAPI` is `__stdcall`
    // ```
    //
    // Two identifiers and the `)` close the group. Read as a parameter list instead — which is what happened
    // before this case existed — `WINAPI` becomes a parameter of type `PM_OPEN_PROC` and the declaration falls
    // apart at its `;`. A declarator cannot *begin* with a parameter list (a function's name comes first), so
    // claiming this group takes nothing from the reading that owns parameter lists; and the first identifier must
    // be spelled like a macro, so an ordinary pair of names is left exactly as it was.
    //
    // The **follower** is what keeps it out of a parameter list that looks the same: `void C::f(_Predicate __pred)
    // { }` has the identical group, and claiming it there turned a member definition into rubble (the body's
    // declarations landed outside it, and the error surfaced on a `typedef` three lines down). A parameter list is
    // never followed by another `(` that belongs to the same declarator, and the function-pointer typedef this
    // case was written for always is:
    //
    // ```cpp
    // typedef DWORD (WINAPI PM_OPEN_PROC)(LPWSTR);      // the `(LPWSTR)` is the declarator's suffix
    // ```
    if matches!(
        p.peek_token_kind_at(1..4).as_slice(),
        [
            CppTokenKind::Identifier,
            CppTokenKind::Identifier,
            CppTokenKind::RightParen
        ]
    ) && p.peek_token_kind_at(4..5) == [CppTokenKind::LeftParen]
        && written_like_a_macro(p.peek_token_text_at(1))
    {
        return true;
    }

    // …and the **abstract** spelling of that same shape: a macro before the operator and **no name at all**,
    // which is how a function-pointer type with a calling convention is written as a *type* rather than as a
    // declaration of something:
    //
    // ```cpp
    // typedef void (__cdecl *)(void);      // ucrt: the signature of `_onexit_t`
    // typedef int  (__stdcall *)(int);
    // ```
    //
    // Its two neighbours above both need a name (`(WINAPI PM_OPEN_PROC)`, `(*STDAPICALLTYPE LPFN…)`), so this
    // group fell through to the abstract-declarator reading, which read the `(` and then stopped on the macro —
    // leaving `(`, `__cdecl`, `*`, `)` as bare tokens and reporting ``expected `;` `` at the `(`. The macro's
    // **spelling** is what keeps the case out of everything else, exactly as it does above: `(x *)` is not a
    // declarator in any reading, but only a name written the way an implementation macro is gets read as one.
    if matches!(
        p.peek_token_kind_at(1..4).as_slice(),
        [
            CppTokenKind::Identifier,
            CppTokenKind::Star | CppTokenKind::Ampersand | CppTokenKind::LogicalAnd,
            CppTokenKind::RightParen
        ]
    ) && p.peek_token_kind_at(4..5) == [CppTokenKind::LeftParen]
        && written_like_a_macro(p.peek_token_text_at(1))
    {
        return true;
    }
    // …and the same macro **around the declarator's own operator**, which is the other two positions a calling
    // convention and an attribute are written in — the two halves of one shape, and the reason each is here:
    //
    // ```cpp
    // typedef HRESULT (STDAPICALLTYPE *LPFNGETCLASSOBJECT) (REFCLSID, REFIID, LPVOID *);   // combaseapi.h:358
    // typedef void (*_GLIBCXX11_DEPRECATED unexpected_handler) ();                         // exception:87
    // ```
    //
    // `STDAPICALLTYPE` is `__stdcall` and `_GLIBCXX11_DEPRECATED` is `__attribute__((__deprecated__))`, and both
    // are *between* the parentheses that exist to bind the `*` to the declarator's name — so neither can be part
    // of any other reading: a parameter list has no `NAME NAME` in it either (`(* a b)` is not a parameter), and
    // the group's tokens are the declarator's own. `written_like_a_macro` is asked of the **macro**, not of the
    // name, so `(x y)` and `(* x y)` keep the readings they had.
    //
    // The first of the two was written once before and reverted: the predicate and the reader were added and the
    // error moved from column 16 to column 51 without the file reading. What was
    // missing is the **reader's** other half — a macro *before* the operator — which is what
    // [`parse_parenthesised_declarator`] now consumes.
    if matches!(
        p.peek_token_kind_at(1..4).as_slice(),
        [
            CppTokenKind::Star | CppTokenKind::Ampersand | CppTokenKind::LogicalAnd,
            CppTokenKind::Identifier,
            CppTokenKind::Identifier
        ]
    ) && p.peek_token_kind_at(4..5) == [CppTokenKind::RightParen]
        && written_like_a_macro(p.peek_token_text_at(2))
    {
        return true;
    }

    // `(MACRO & name)` — the same shape with a reference operator — and it needs the **follower** for a reason
    // the operator arms above did not have to face: `(IDENT & IDENT)` is not a rare declarator group, it is the
    // single most ordinary *parameter list* there is.
    //
    // ```cpp
    // void _Container_base12::_Swap_proxy_and_iterators_unlocked(_Container_base12& _Right) noexcept { … }
    // void C::g(C& r) { }
    // void C::g(T& r) { }
    // ```
    //
    // `written_like_a_macro` is a *spelling* test — a leading underscore, or all capitals — and every one of
    // those parameter types passes it: `_Container_base12` starts with `_`, and `C`, `T`, `M`, `X` are single
    // capitals, which `looks_like_a_macro_name` accepts. So the group was claimed as a declarator, `C` was read
    // as the macro, `& r` as what it decorates, and the declaration fell apart: measured on `xmemory` (the only
    // remaining failure in MSVC's STL that was not a real gap) and reproduced in isolation by
    // `struct C { }; void C::g(C& r) { }` — which is what the test below pins.
    //
    // What separates the two readings is what follows the group, exactly as it does two arms up: a declarator
    // group is followed by the declarator's own suffix — the parameter list of what it decorates — and a
    // parameter list is followed by `)`, `,`, a qualifier or a body. So the `(` is required, and the parameter
    // list keeps its reading.
    if matches!(
        p.peek_token_kind_at(1..4).as_slice(),
        [
            CppTokenKind::Identifier,
            CppTokenKind::Star | CppTokenKind::Ampersand | CppTokenKind::LogicalAnd,
            CppTokenKind::Identifier
        ]
    ) && p.peek_token_kind_at(4..5) == [CppTokenKind::RightParen]
        && p.peek_token_kind_at(5..6) == [CppTokenKind::LeftParen]
        && written_like_a_macro(p.peek_token_text_at(1))
    {
        return true;
    }

    // `(FAR WINAPI *FARPROC)`, `(__RPC_STUB __RPC_FAR * RPC_DISPATCH_FUNCTION)` — the same calling-convention
    // group with **two names before the operator**, which every arm above is one name short of: the abstract arm
    // wants `(MACRO *)`, the named ones want a single name on either side of the `*`.
    //
    // ```cpp
    // typedef INT_PTR (FAR WINAPI *FARPROC)();                                          // minwindef.h:223
    // typedef void (__RPC_STUB __RPC_FAR * RPC_DISPATCH_FUNCTION) (IN OUT PRPC_MESSAGE); // rpcdcep.h:183
    // ```
    //
    // The **shape alone** decides it here, and that is a deliberate difference from the arms above: their
    // `written_like_a_macro` gate exists to protect a reading that is otherwise *valid* — `(T& r)` is a parameter
    // list, and a spelling test is the only thing that can tell it from `(MACRO& name)`. Two names and then an
    // operator and then a name cannot be a parameter list at all: a parameter is a type and a declarator, so the
    // second name would have to start a declarator, and no declarator begins with a name. There is therefore
    // nothing to protect, and requiring the spelling would refuse `far winapi`-style spellings that no compiler
    // refuses.
    if matches!(
        p.peek_token_kind_at(1..5).as_slice(),
        [
            CppTokenKind::Identifier,
            CppTokenKind::Identifier,
            CppTokenKind::Star | CppTokenKind::Ampersand | CppTokenKind::LogicalAnd,
            CppTokenKind::Identifier
        ]
    ) && p.peek_token_kind_at(5..6) == [CppTokenKind::RightParen]
    {
        return true;
    }

    // …and the same group with a **calling convention in front of the class name**, which is how MSVC's
    // `type_traits` writes every member-pointer predicate:
    //
    // ```cpp
    // #define _IS_MEMFUNPTR(CALL_OPT, CV_OPT, REF_OPT, NOEXCEPT_OPT)                       \
    //     template <class _Ret, class _Arg0, class... _Types>                               \
    //     struct _Is_memfunptr<_Ret (CALL_OPT _Arg0::*)(_Types...) CV_OPT REF_OPT …>        \
    //         : _Arg_types<CV_OPT _Arg0*, _Types...> { … }                                  // type_traits:406
    // ```
    //
    // `CALL_OPT` is `__cdecl`/`__stdcall`/empty, so the tokens are `( __cdecl A :: * )` and the class name stands
    // one token further along than the arm below looks. Read as anything else the group is an expression the
    // argument reader cannot parse, and the whole specialisation is reported as ``expected a template argument``.
    //
    // **Before** the `let … else` below, and that is not tidiness: that binding returns `false` for a group whose
    // first token is not the class name, so an arm placed after it is unreachable — which is exactly how the first
    // version of this was written and why it changed nothing.
    //
    // It took better evidence to see this one at all: the macro chain that produces the spelling was not expanded
    // in the per-file reading, so the tokens arrived as `CALL_OPT A :: *` — an ordinary name — and the shape was
    // never reached. Same story as every other gap this corpus has exposed.
    if p.peek_token_kind_at(1..2) == [CppTokenKind::Identifier]
        && written_like_a_macro(p.peek_token_text_at(1))
        && let Some(length) = pointer_to_member_operator_length(p, 2)
    {
        let after = 2 + length;
        let follower = p.peek_token_kind_at(after..after + 2);

        // **With a name**, which is what this predicate is about: `( __cdecl A :: * p )`. The nameless spelling
        // `( __cdecl A :: * )` has no declarator to hold and belongs to the abstract predicate, which claims it
        // for the same reason the arm below leaves the nameless `(C::*)` alone.
        //
        // ```cpp
        // struct _Is_memfunptr<_Ret (__cdecl _Arg0::*)(_Types...) &> : …     // named, in the specialisation
        // using member = void (__cdecl _Arg0::*)();                           // nameless, in an alias
        // ```
        return follower.as_slice() == [CppTokenKind::Identifier, CppTokenKind::RightParen];
    }

    // `(C::*h)`, `(A::B::*h)` — a **pointer to member**, whose class name stands where the operator would, and
    // whose name stands after the `*`. The same group is also written without a name — `(C::*)`, which is the
    // abstract spelling a type-id uses (`void g(int (C::*)(int));`) — and both are groups this rule has to claim,
    // because the alternative reading of those tokens is a parameter list, and `C::*` is not a parameter.
    //
    // Leaving them out is what made the parenthesised member-pointer *typedef* — `typedef int (C::*fp)(int);` —
    // and every parameter of a member-function-pointer type fail together: the parentheses were read as a
    // parameter list, `C` became a parameter of an unknown type, and the declaration fell apart at its `;`.
    let Some(length) = pointer_to_member_operator_length(p, 1) else {
        return false;
    };
    let after = 1 + length;

    // **With a name**, which is the group this rule owns. The nameless `(C::*)` is the abstract spelling and
    // belongs to [`parse_abstract_declarator`] — claiming it here would take it to a rule that requires a name
    // and fail on a parameter that is perfectly good: `void h(int (C::*)(int));`.
    if p.peek_token_kind_at(after..after + 2).as_slice()
        == [CppTokenKind::Identifier, CppTokenKind::RightParen]
    {
        return true;
    }

    // …and the same group with a **calling convention in front of the class name**, which is how MSVC's
    // `type_traits` writes every member-pointer predicate:
    //
    // ```cpp
    // #define _IS_MEMFUNPTR(CALL_OPT, CV_OPT, REF_OPT, NOEXCEPT_OPT)                       \
    //     template <class _Ret, class _Arg0, class... _Types>                               \
    //     struct _Is_memfunptr<_Ret (CALL_OPT _Arg0::*)(_Types...) CV_OPT REF_OPT …>        \
    //         : _Arg_types<CV_OPT _Arg0*, _Types...> { … }                                  // type_traits:406
    // ```
    //
    // `CALL_OPT` is `__cdecl`/`__stdcall`/empty, so the tokens are `( __cdecl A :: * )` and the class name stands
    // one token further along than the arm above looks. Read as anything else, the group is an expression the
    // argument reader cannot parse and the whole specialisation is reported as ``expected a template argument``.
    //
    // It took better evidence to see this one: the macro chain that produces the spelling was not expanded in the
    // per-file reading, so the tokens arrived as `CALL_OPT A :: *` — an ordinary name — and the shape was never
    // reached. That is the same story as every other gap this corpus has exposed, and the reason the measurement
    // is the *reading* and not the file.
    false
}

/// Parse the suffixes that bind to a declarator: parameter lists and array bounds, in any order.
///
/// e.g.: the `(int)` and `[4]` of `void (*[4])(int)`
///
/// A second copy of the suffix loop in [`parse_declarator_with`], which cannot be reused here: that one is
/// driven by a declarator's *name* and by the declaration/expression decision, and neither exists in a
/// type-id. What the two share is the rule for what a suffix is — `(` starts a parameter list and `[` an
/// array bound — and that is small enough to state twice rather than to parameterise.
///
/// Exposed for the alias rule, which needs it for the same reason the type-id does: `using Arr = int[4];`
/// writes a type whose array part has nothing enclosing it to attach to.
pub fn parse_declarator_suffixes(p: &mut CppParser) -> ParseResult {
    let _phase = crate::parser::phases::during("declarator-suffixes");
    loop {
        match p.current_token() {
            CppTokenKind::LeftParen => {
                let _ = super::super::decls::parse_parameter_list(p)?;
                eat_function_qualifiers(p);
            }
            CppTokenKind::LeftBracket => {
                let array = p.mark(CppSyntaxKind::ArrayType);
                p.bump();
                // The bound is optional: `void (*[])()`.
                if p.current_token() != CppTokenKind::RightBracket
                    && !p.is_eof()
                    && let Err(err) = super::super::exprs::parse_expr(p)
                {
                    array.undo(p);
                    return Err(err);
                }
                if let Err(err) = expect_token(p, CppTokenKind::RightBracket) {
                    array.undo(p);
                    return Err(err);
                }
                array.complete(p);
            }
            _ => return Ok(CompleteMarker::empty()),
        }
    }
}

/// Parse a declarator: an optional abstract-declarator part plus the name it declares.
///
/// The name is optional: `void f(int)` declares a parameter with no name, and an abstract
/// declarator may have no name at all.
pub fn parse_declarator(p: &mut CppParser) -> ParseResult {
    parse_declarator_with(p, false)
}

/// [`parse_declarator`], optionally allowing a structured binding where the name would go.
///
/// `auto& [k, v] = m;` is why the flag exists. The `&` is an abstract declarator and the `[…]` after
/// it is the binding pattern, so by the time the name would be read the brackets are already past the
/// point where a declarator rule can see them. A separate entry point rather than a general rule:
/// outside a variable declaration's declarator a `[` here is an array bound or a lambda capture, and
/// reading it as a binding pattern would take those away from the rules that own them.
pub fn parse_declarator_with(p: &mut CppParser, allow_structured_binding: bool) -> ParseResult {
    let _phase = crate::parser::phases::during("declarator");
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::Declarator);

    // Where this declarator's own events begin. The suffix loop below asks "has this declarator named
    // anything yet?" of the event stream, and this is the bound that keeps the *type*'s name — recorded before
    // it, by the specifier sequence — out of the answer.
    let declarator_from = p.current_event_count();

    // The answer for the declarator about to be parsed; each way of becoming a function sets it.
    p.set_last_declarator_is_function(false);

    // A destructor's name is written before anything else: `~S()`, `~S() = default`. The tilde is part of the
    // *name*, so it belongs to the declarator — and the specifier sequence, which is what runs first, reads a
    // `~` as a unary operator it cannot use and refuses the whole declaration. Claiming the pair here is what
    // gives the declaration a name to hang its parameter list on; without it `~S();` came out as an error node
    // holding the tilde and a declaration of nothing at all.
    if p.current_token() == CppTokenKind::Tilde {
        if let Err(err) = parse_destructor_name(p) {
            p.close_marks_above(base);
            return Err(err);
        }

        if let Err(err) = parse_declarator_function_suffixes(p, declarator_from) {
            p.close_marks_above(base);
            return Err(err);
        }
        return Ok(m.complete(p));
    }

    // `(*f)(int)` — a parenthesised declarator with a *name* inside. Claimed here, before
    // [`parse_abstract_declarator`] can read the same tokens as an abstract pointer-to-function type and then
    // choke on the name; see [`a_parenthesised_declarator_with_a_name_follows`].
    let named_inside_parentheses = a_parenthesised_declarator_with_a_name_follows(p);

    if named_inside_parentheses {
        if let Err(err) = parse_parenthesised_declarator(p) {
            p.close_marks_above(base);
            return Err(err);
        }
    } else if let Err(err) = parse_abstract_declarator(
        p,
        // A name may follow, which is the position this declarator stands in: `int (x)` names `x` and
        // `void f(int)` continues with a parameter list, so `(int)` here is never a function type.
        true,
    ) {
        p.close_marks_above(base);
        return Err(err);
    }

    // A structured binding, in the position the name would occupy.
    if allow_structured_binding
        && p.current_token() == CppTokenKind::LeftBracket
        && super::super::decls::the_declaration_is_a_placeholder(p)
    {
        if let Err(err) = super::super::decls::parse_structured_binding(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        return Ok(m.complete(p));
    }

    // The name, if there is one.
    //
    // A declarator's name can never be a **bare template-id**. `C<T> x;` declares `x` with the type `C<T>`: the
    // arguments belong to the *type*, and the name that follows is a plain identifier. Reading them as the name
    // is what made `C<T> && C2<T>;` — two template-ids joined by a `&&` — a declaration of an rvalue reference
    // whose declarator was named `C2<T>`: well formed, lossless, no diagnostic, and a binding no compiler would
    // accept. A template-id in the name position is a name only when it is *qualified*, where the arguments
    // belong to the qualifier: `S<T>::f` names `f`.
    //
    // Refusing here is what turns that statement back into the expression it is: the declaration reading fails,
    // and the caller falls back — the same bounded backtracking the declaration/expression ambiguity already
    // relies on. See `a_bare_template_id_is_here`.
    let clause_ahead = p.current_token() == CppTokenKind::Identifier
        && !named_inside_parentheses
        && super::super::at_requires(p)
        && (p.last_declarator_is_function()
            || p.last_consumed_token_kind() == Some(CppTokenKind::RightParen))
        && super::super::decls::starts_a_requires_clause(p);
    if !clause_ahead && a_bare_template_id_is_here(p) {
        p.close_marks_above(base);
        return Err(CppParseError::syntax_error_from(
            &t!("a declarator's name cannot have template arguments"),
            p.current_token_range(),
        ));
    }

    // **`requires` after a parameter list begins a constraint, it does not name anything**: in
    // `V(const V&) requires C<T> = default;` the declarator is a constructor's — no type, no name — and the keyword
    // is the next thing written. Taken for the declarator's name, the clause's `C<T>` was then "a name with template
    // arguments" and the whole member failed, silently, in nine places of MSVC's `<variant>`/`<ranges>`. A real
    // variable called `requires` is still one: its tokens do not read as a clause.
    let begins_a_clause = p.current_token() == CppTokenKind::Identifier
        && !named_inside_parentheses
        && super::super::at_requires(p)
        && (p.last_declarator_is_function()
            || p.last_consumed_token_kind() == Some(CppTokenKind::RightParen))
        && super::super::decls::starts_a_requires_clause(p);
    let named = named_inside_parentheses
        || (!begins_a_clause
            && matches!(
                p.current_token(),
                CppTokenKind::Identifier
                    | CppTokenKind::Scope
                    | CppTokenKind::OperatorKeyword
                    | CppTokenKind::Tilde
            ));
    if named
        && !named_inside_parentheses
        && let Err(err) = parse_name(p)
    {
        p.close_marks_above(base);
        return Err(err);
    }

    // Suffixes: function parameter lists and array bounds. These bind tighter than pointers, which
    // is why they attach here rather than being folded into the type.
    //
    // A declarator with a name gets them unconditionally — and so does one whose declarator name was taken by
    // the specifier sequence as the last segment of a **qualified** name. `void ns::C::method()` is the case:
    // the sequence walks `ns::C::method` segment by segment as one name, so by the time the declarator is
    // reached there is no name left for it to read, and the parameter list had nothing to attach to. A
    // qualified name in type position is the head of a definition, which is what makes this safe: an
    // unqualified `Foo(1, 2);` is a call, and it is not affected because nothing forced its name into the type.
    //
    // A declarator *without* one gets them only when the
    // direct-initialisation reading has been chosen, and the question is put to the same rule the main declarator
    // path would put it to rather than to a proxy for it: `Widget w(1, 2);` opens the loop and reads an
    // initializer, while the `g(1, 2);` and `Max(a, b);` of a body do not open it at all and keep their
    // parentheses for the expression reading. An abstract declarator's real reason to exist is a type-id such as
    // `int(void)`, and that is reached through `parse_type_id`, not here.
    if named
        || a_qualified_name_is_the_type(p)
        || super::super::decls::the_head_of_the_declaration_is_qualified(p)
        || super::super::decls::a_declaration_is_the_better_reading(p, declarator_from)
        // …or a **macro invocation used as a definition**, which is the one shape with no answer to the
        // declaration/expression question at all: `TEST(FormatPerformance, 1k_row) { … }`. Its arguments are the
        // macro's tokens, so `the_arguments_look_like_declarators` says no and the loop would never open — which
        // is exactly how `TEST(A, B) { }` came to work while `TEST(A, 1) { }` did not.
        || super::super::decls::a_macro_definition_follows(p, declarator_from)
    {
        // **How many parameter lists the suffixes have read.** See the `(` arm: a second group after one is another
        // suffix, while a group straight after the name is a call the expression grammar should have read.
        let mut parameter_lists_read = 0usize;
        loop {
            match p.current_token() {
                CppTokenKind::LeftParen => {
                    if let Err(err) = super::super::decls::parse_function_suffix_or_initializer(
                        p,
                        declarator_from,
                    ) {
                        p.close_marks_above(base);
                        return Err(err);
                    }
                    parameter_lists_read += 1;
                    // A `(` that is neither a parameter list nor an initializer belongs to whatever comes next.
                    //
                    // For a declarator **without** a name this loop must stop rather than fail: `g(1, 2);` reaches
                    // the declaration reading this way, and the expression reading is what should have the
                    // parentheses. An abstract declarator's real reason to exist is a type-id such as
                    // `int(void)`, and that is reached through `parse_type_id`, not here.
                    if p.current_token() == CppTokenKind::LeftParen {
                        if named && parameter_lists_read == 0 {
                            // For a declarator **with** a name, stopping is not an option: `Widget w(g())` would
                            // leave the declarator parsed, the parentheses unread, and the declaration to fail
                            // later at its `;` — turning a statement the expression grammar could have read into
                            // an error node. Failing here rewinds the whole declaration attempt instead, and the
                            // caller reads the statement as the expression it was.
                            p.close_marks_above(base);
                            return Err(CppParseError::syntax_error_from(
                                &t!("expected a parameter list or an initializer"),
                                p.current_token_range(),
                            ));
                        }
                        // **A group *after* a parameter list is one more suffix.** The grammar of C has no
                        // `T name(a)(b)`, but the text here is **unpreprocessed** and a macro standing for a type
                        // makes exactly that shape out of a legal declaration — MinGW's `winsock2.h:899` is
                        // `typedef unsigned __LONG32 (WSAAPI *LPFN_INET_ADDR)(const char *cp);`, where `__LONG32`
                        // is `long`. Failing there rewound the whole declaration and the **recovery** then ate the
                        // declarations after it: measured on libuv 1.53.1, every file that includes `winsock2.h`
                        // lost the header's declarations from `WSAGetLastError` on, three `undefined-name` findings
                        // (`docs/c-support.md` §7.1). The `Widget w(g())` shape above is untouched: its group comes
                        // straight after the name, with no parameter list in between.
                        if !named {
                            break;
                        }
                        continue;
                    }
                }
                // A `[[` here is an **attribute**, not an array bound: `int x [[maybe_unused]];`.
                //
                // The two share their first token, and reading it as an array bound is what made the whole
                // declaration fail — the bound's expression rule met `[` where an operand should be, the
                // declarator was refused, and the statement was re-read as an expression. The check is the same
                // one [`parse_attribute_specifiers`] uses, and it can be made here without lookahead beyond the
                // next token because no array declarator is spelled `[[`.
                //
                // **Two positions, and which one this is decides who reads it.** An attribute after the
                // declarator-id and before the *parameters* belongs to the declarator — the standard writes the
                // sequence there, and the parameter list that follows is still this declarator's suffix:
                //
                // ```cpp
                // bool operator== [[nodiscard]] (const tuple<_Tps...>& __t, const tuple<_Ups...>& __u)
                //                         └── the declaration's own name is complete, the parameters are not
                // ```
                //
                // — so it is read here and the loop continues, which is what puts the `ParameterList` inside the
                // `Declarator` where every consumer looks for it. An attribute *after* the parameters
                // (`void f() [[noreturn]];`, `int x [[maybe_unused]] = 1;`) is the other position, and it stays
                // with [`super::super::decls::finish_init_declarator`], which owns it. The follower is what tells them
                // apart, and it is a shape rather than a preference: a `(` after the attribute run can only be
                // this declarator's parameter list.
                CppTokenKind::LeftBracket if p.peek_next_token() == CppTokenKind::LeftBracket => {
                    if !a_parameter_list_follows_the_attributes(p) {
                        break;
                    }
                    if let Err(err) = parse_attribute_specifiers(p) {
                        p.close_marks_above(base);
                        return Err(err);
                    }
                }
                CppTokenKind::LeftBracket => {
                    let array = p.mark(CppSyntaxKind::ArrayType);
                    p.bump();
                    // The bound is optional: `int a[]`.
                    if p.current_token() != CppTokenKind::RightBracket
                        && !p.is_eof()
                        && let Err(err) = super::super::exprs::parse_expr(p)
                    {
                        array.undo(p);
                        p.close_marks_above(base);
                        return Err(err);
                    }
                    if let Err(err) = expect_token(p, CppTokenKind::RightBracket) {
                        array.undo(p);
                        p.close_marks_above(base);
                        return Err(err);
                    }
                    array.complete(p);
                }
                _ => break,
            }
        }
    }

    Ok(m.complete(p))
}

/// Parse a parenthesised declarator that has a name in it: `(*f)`, `(&f)`, `(**f)`.
///
/// Produces `Declarator(Declarator(PointerType, NameExpr))` — the parentheses and their contents as one
/// declarator, which is what makes `void (*f)(int)` a pointer to a function rather than a function returning a
/// pointer: the `(int)` that follows binds to the *outer* declarator.
///
/// The inner declarator deliberately does **not** consume suffixes. In `int (*f(int))(int)` the `(int)` inside
/// the parentheses belongs to `f` and the one outside belongs to the pointer, and a rule that let the inner
/// one read suffixes would take both.
pub(crate) fn parse_parenthesised_declarator(p: &mut CppParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::Declarator);
    // The inner parameter list's suffixes are read by the same loop the outer declarator uses, which asks "has this
    // declarator named anything yet?" of the events from this offset — see `parse_declarator_function_suffixes`.
    let declarator_from = p.current_event_count();

    expect_token(p, CppTokenKind::LeftParen)?;

    // **An attribute before the declarator's own operator** — `( __attribute__((__stdcall__)) *FN )`, which is
    // what `STDAPICALLTYPE` expands to in MinGW's `combaseapi.h`. The macro arm below expects the group's first
    // token to be a name followed by a `*`; here it is the attribute, and the `*` stands after it.
    while let Some(length) = attribute_group_length(p, 0) {
        let attribute = p.mark(CppSyntaxKind::AttributeList);
        for _ in 0..length {
            p.bump();
        }
        attribute.complete(p);
    }
    // The inner declarator's parentheses wrap a declarator, never a parameter list, so no name may appear
    // there: `(*f)` is a pointer and `(*f(int))` a pointer to a *function*, whose parameter list is read by
    // the outer declarator's suffix loop.
    parse_abstract_declarator(p, false)?;

    // A macro **before the declarator's own operator**, which is where a COM header writes the calling
    // convention:
    //
    // ```cpp
    // typedef HRESULT (STDAPICALLTYPE *LPFNGETCLASSOBJECT) (REFCLSID, REFIID, LPVOID *);   // combaseapi.h:358
    // ```
    //
    // The `*` has not been read yet at this point — the abstract declarator above found nothing, because the
    // first token is a name — so this is the position the macro occupies: it stands *before* the pointer rather
    // than after it, and after it the ordinary abstract-declarator reading continues. Claiming it here is what
    // lets the `*` and the name after it be read by the rules that already own them; without it `STDAPICALLTYPE`
    // was taken for the name, the `*` had no owner, and the declaration fell apart inside its own parentheses.
    if p.current_token() == CppTokenKind::Identifier
        && matches!(
            p.peek_next_token(),
            CppTokenKind::Star | CppTokenKind::Ampersand | CppTokenKind::LogicalAnd
        )
        && written_like_a_macro(p.current_token_text())
    {
        let m = p.mark(CppSyntaxKind::MacroCall);
        let name = p.mark(CppSyntaxKind::NameExpr);
        p.bump();
        name.complete(p);
        m.complete(p);
        parse_abstract_declarator(p, false)?;
    }

    // A **macro before the name** inside the parentheses — the call convention a COM header writes:
    //
    // ```cpp
    // typedef DWORD (WINAPI PM_OPEN_PROC)(LPWSTR);          // winperf.h:180 — `WINAPI` is `__stdcall`
    // typedef DWORD (WINAPI PM_COLLECT_PROC)(LPWSTR, …);    // winperf.h:181
    // ```
    //
    // Two names in a row cannot be a declarator — `NAME NAME` is not one in any grammar, so the first can only be
    // a macro and the second is the name this declarator is about. The same "the sequence is impossible" argument
    // as for a macro-shaped name between a type and a declarator, one level down: there the name stood between the
    // type and the declarator, here *inside* the parentheses, before the name.
    //
    // **A run of them**, because a calling convention can be spelled in two names and the operator can come after
    // the second: `(FAR WINAPI *FARPROC)` (`minwindef.h:223`) and
    // `(__RPC_STUB __RPC_FAR * RPC_DISPATCH_FUNCTION)` (`rpcdcep.h:183`). What tells the run's *last* name from
    // the declarator's own is what follows it: a name followed by a name or by an operator is still part of the
    // run, and the name the group is about is the one the `)` closes on. The single-name case is the same test
    // with one iteration.
    //
    // `written_like_a_macro` keeps it from firing on a pair that is merely broken code: `(x y)` is still read the
    // way it was, and only a name spelled the way a macro is spelled takes this reading.
    while p.current_token() == CppTokenKind::Identifier
        && written_like_a_macro(p.current_token_text())
        && matches!(
            p.peek_next_token(),
            CppTokenKind::Identifier
                | CppTokenKind::Star
                | CppTokenKind::Ampersand
                | CppTokenKind::LogicalAnd
        )
    {
        let m = p.mark(CppSyntaxKind::MacroCall);
        let name = p.mark(CppSyntaxKind::NameExpr);
        p.bump();
        name.complete(p);
        m.complete(p);
    }

    // …and the operator the run stood in front of, which is an ordinary abstract declarator from there on:
    // `(FAR WINAPI *FARPROC)` reaches this point with `* FARPROC` left, and the reading that already owns `*`
    // takes it — the same call the single-macro arm above makes.
    if matches!(
        p.current_token(),
        CppTokenKind::Star | CppTokenKind::Ampersand | CppTokenKind::LogicalAnd
    ) {
        parse_abstract_declarator(p, false)?;
    }

    // A group with **no name at all** is the abstract spelling of the shape the fourth case in
    // `a_parenthesised_declarator_with_a_name_follows` claims — `typedef void (__cdecl *)(void);` — and there is
    // nothing to read where the name would go. The groups that do have one (`(*f)`, `(WINAPI PM_OPEN_PROC)`) are
    // unaffected: their cursor is on the name, not on the `)`.
    if p.current_token() != CppTokenKind::RightParen {
        parse_name(p)?;
        // **The name's own parameter list, when it is written inside the group** — `(*f())`, the shape a macro
        // standing for a variable makes out of a declaration (`docs/worklog.md`, redis's `environ`). Without this
        // the group ended at the name and the `(` was left for the outer declarator's suffix loop, which wanted the
        // `)` here; `(*f)(int)` is unaffected because there the group closes on `)` and the list belongs to the
        // pointer.
        if p.current_token() == CppTokenKind::LeftParen {
            parse_declarator_function_suffixes(p, declarator_from)?;
        }
    }
    expect_token(p, CppTokenKind::RightParen)?;

    Ok(m.complete(p))
}

/// Parse a destructor's name: the `~` and the class name it destroys.
///
/// Produces `NameExpr(Tilde, Identifier)`, which is the node every other kind of name gets — `parse_name`
/// already builds one for `~Foo` when it reaches the tilde, and this is the same shape reached from the
/// declarator's own entry point instead.
///
/// The class name is optional in the grammar because a *definition* may be written out of line with a qualified
/// name — `Foo::~Foo()` reaches here with `Foo::` already consumed by the specifier sequence — and because a
/// missing name is ordinary in a file being edited.
pub(crate) fn parse_destructor_name(p: &mut CppParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::NameExpr);

    expect_token(p, CppTokenKind::Tilde)?;
    if p.current_token() == CppTokenKind::Identifier {
        p.bump();
    }

    Ok(m.complete(p))
}

/// Parse the parameter list and qualifiers of a declarator whose name has already been read.
///
/// For the destructor and the conversion operator, whose names are claimed before the usual declarator path
/// runs. The loop is the same shape as the one in [`parse_declarator_with`], and it is a second copy for the
/// same reason the first is not reusable there: that loop is driven by the declaration/expression decision,
/// which both of these have already answered by existing.
///
/// The `last_declarator_is_function` flag is set for the same reason it is set everywhere else — it is what
/// makes a `{` after the declarator a *body* rather than a braced initializer, and `~S() {}` is a definition.
pub fn parse_declarator_function_suffixes(
    p: &mut CppParser,
    declarator_from: usize,
) -> ParseResult {
    loop {
        match p.current_token() {
            CppTokenKind::LeftParen => {
                super::super::decls::parse_function_suffix_or_initializer(p, declarator_from)?;
                // **And a *second* parameter list is another suffix**, not the end of this declarator. The grammar
                // of a function returning a function is not legal C++ — but the text this parser reads is
                // **unpreprocessed**, and a macro standing in for a type makes `X(a)(b)` out of a perfectly legal
                // declaration: MinGW's `winsock2.h:899` writes
                //
                // ```c
                // typedef unsigned __LONG32 (WSAAPI *LPFN_INET_ADDR)(const char *cp);
                // ```
                //
                // where `__LONG32` is `long`. Breaking here made the declaration fail, and the **recovery** then ate
                // the declarations after it: measured on libuv 1.53.1, every file that includes `winsock2.h` lost
                // the header's 107 declarations from `WSAGetLastError` on — three `undefined-name` findings with the
                // whole rest of the enum-era fixes already in place (`docs/c-support.md` §7.1).
            }
            CppTokenKind::LeftBracket => {
                let array = p.mark(CppSyntaxKind::ArrayType);
                p.bump();
                if p.current_token() != CppTokenKind::RightBracket
                    && !p.is_eof()
                    && let Err(err) = super::super::exprs::parse_expr(p)
                {
                    array.undo(p);
                    return Err(err);
                }
                if let Err(err) = expect_token(p, CppTokenKind::RightBracket) {
                    array.undo(p);
                    return Err(err);
                }
                array.complete(p);
            }
            _ => break,
        }
    }

    Ok(CompleteMarker::empty())
}

/// Is the declaration's type a **qualified** name, which makes the declarator's own name the last segment?
///
/// `void ns::C::method()` is the case. The specifier sequence treats `ns::C::method` as one name — it walks a
/// qualified name segment by segment — so the declarator that follows has no name of its own to read, and the
/// question this answers is what lets its parameters still be recognised as parameters.
///
/// A qualified name in type position is the head of a *definition* rather than the name of a value, which is
/// the whole argument for allowing it: `Foo(1, 2);` is a call and its name was never forced into the type, so
/// the unqualified shape is untouched. The `::` that must be present for a true answer is what separates them.
pub(crate) fn a_qualified_name_is_the_type(p: &CppParser) -> bool {
    let Some(name) = p.declaration_type_name() else {
        return false;
    };

    p.has_qualified_declaration_type_name() && !name.is_empty()
}
