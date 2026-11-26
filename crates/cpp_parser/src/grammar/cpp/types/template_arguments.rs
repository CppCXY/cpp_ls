//! **Template arguments** — template argument lists — nested angles, `>>`, and what may stand as an argument.
//!
//! Moved here from `types.rs` without a change.

use super::*;
use crate::parser;

/// Parse a template argument list: `<T, int N, ...>`.
///
/// The closing `>` is the hard part. `std::vector<std::vector<int>>` ends in `>>`, which the lexer
/// has already produced as a single `RightShift` token, so the *last* `>` of a nested template list
/// has to be split back out here. Doing it in the parser rather than the lexer is deliberate: in
/// `a >> b` the same token really is a shift, and only the parser knows which context it is in.
///
/// The other half of that difficulty is **which** `>` closes the list, and the answer is the standard's: the first
/// one that is not nested in a bracketed group. Entering the list is therefore the whole of what this rule does
/// about it — the brackets maintain themselves, at the point of consumption, and the expression grammar asks
/// [`CppParser::greater_than_is_an_operator`]. See that method for the mechanism and for the input that showed the
/// difference (`bc<(1 > 2)>`, which `cl.exe` parses and reports only a *semantic* error about).
pub fn parse_template_argument_list(p: &mut CppParser) -> ParseResult {
    let previous_depth = p.enter_template_arguments();
    let result = parse_template_argument_list_inner(p);
    p.leave_template_arguments(previous_depth);
    result
}

pub(crate) fn parse_template_argument_list_inner(p: &mut CppParser) -> ParseResult {
    let _phase = parser::phases::during("template-arguments");
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TemplateArgumentList);

    expect_token(p, CppTokenKind::Less)?;

    // **How many conditionals were open when this list began** — see the note on the same comparison in
    // `decls::parse_parameter_list`, which is where the two spellings it separates are written out. In short: a
    // list whose *tail* is written per branch must not be confused with a declaration that is written per branch.
    // The count comes from the directive rule, because a nested reading may consume a directive this list never
    // sees.
    let conditionals_when_the_list_began = p.open_conditionals();

    while !p.is_eof() {
        // **A `>>` standing where an argument would begin was a special case here, and is not any more.**
        //
        // `std::less<>` inside `std::map<K, V, std::less<>>` is two closers in a row, and the loop below reads a
        // closer by looking at one token — which worked for `std::map<K, std::less<int>>` (the argument was read
        // first, and the trailing `>>` was taken apart on the way out) but not for the empty list, where there was
        // nothing to read first and the loop never reached the split. So an `if current_token == RightShift` sat
        // here, splitting before the argument.
        //
        // The lexer now leaves every `>` on its own from the start (`CppLexer::tokenize`), so the two spellings
        // need no telling apart: the first closer is the token the cursor is on, and the second is the token after
        // it — which is exactly what the loop below already does.

        // A `>` **here** closes this list, and no lookahead is needed to know it.
        //
        // "Here" is the boundary *between* arguments, and by the time the loop comes back around,
        // every nested list inside the argument just read has already been consumed — along with its
        // own `>`. A `>` still standing at an element boundary has no opener of its own left to
        // belong to, so it can only be ours.
        //
        // This used to scan forward from the `>` and give up as soon as it saw a `<`, reading that as
        // "an inner list still wants this `>`". The direction is the error: a `<` to the *right* says
        // nothing about a `>` to its left. The rule rejected the first of two template-ids in one
        // expression — `requires C<T> && C2<T>` failed at the `>` of `C<T>`, one template argument
        // short of the end — and that shape is not rare: it is what every conjunction of two concepts
        // is written as, and `T<A>::value < T<B>::value` is the same shape.
        //
        // The lexer has already left every `>` on its own by the time we look, so this is the only place the
        // closer is consumed.
        if p.current_token() == CppTokenKind::Greater {
            p.bump();

            // **The list's tail written once per branch** — `bits/stl_iterator.h:3090`:
            //
            // ```cpp
            //     using __iter_key_t = remove_const_t<
            // #ifdef __glibcxx_tuple_like // >= C++23
            //       tuple_element_t<0, typename iterator_traits<_InputIterator>::value_type>>;
            // #else
            //       typename iterator_traits<_InputIterator>::value_type::first_type>;
            // #endif
            // ```
            //
            // Each branch writes the last argument **and the `>` that closes this list**, plus the `;` that ends
            // the alias — the head (`remove_const_t<`) stands above the `#if`. So the `>` just consumed closes the
            // list *in this spelling*, and the branch's tail belongs to the same list. Read here, the branch's `;`
            // with it, and the declaration is told
            // ([`CppParser::note_the_terminator_came_from_a_branch`]) — it would otherwise ask for a `;` and find
            // `#endif`.
            //
            // Two conditions gate it, the same two the parameter list uses: a `;` followed by an `#else`/`#elif`,
            // **and** a conditional this list opened inside itself. Without the second one a declaration written
            // per branch (`#if X using A = B<int>; #else using A = B<long>; #endif`) would have the next branch's
            // text read as more arguments.
            let mut read_a_branch_of_the_tail = false;
            while p.open_conditionals() > conditionals_when_the_list_began
                && let Some(writes_the_terminator) =
                    super::super::decls::a_branch_continues_the_tail(p)
            {
                if writes_the_terminator {
                    p.bump(); // this branch's `;`
                    p.note_the_terminator_came_from_a_branch();
                    read_a_branch_of_the_tail = true;
                }

                while p.current_token() == CppTokenKind::Hash {
                    super::super::stats::parse_preprocessor_directive(p)?;
                }

                // …and the branch's own tail: its arguments, and the `>` that closes the list there. Each one is
                // marked the way the loop above marks an argument — a `TemplateArgument` node — because a consumer
                // counting a list's arguments must not have to know which branch it was reading.
                loop {
                    let argument = p.mark(CppSyntaxKind::TemplateArgument);
                    if parse_template_argument(p).is_err() {
                        argument.undo(p);
                        p.close_marks_above(base);
                        return Err(CppParseError::syntax_error_from(
                            &t!("expected a template argument"),
                            p.current_token_range(),
                        ));
                    }
                    argument.complete(p);

                    if p.current_token() == CppTokenKind::Comma {
                        p.bump();
                        continue;
                    }
                    break;
                }

                at_a_closing_angle(p);
                expect_token(p, CppTokenKind::Greater)?;
            }

            if read_a_branch_of_the_tail && p.current_token() == CppTokenKind::Semicolon {
                p.bump();
            }

            return Ok(m.complete(p));
        }

        // A **directive between arguments** — including in front of the first one, which is the spelling a
        // conditional default argument is written with:
        //
        // ```cpp
        // template<typename _Tp, bool _TreatAsBytes =        // bits/cpp_type_traits.h:620
        // #if __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
        //       __is_integer<_Tp>::__value
        // #else
        //       __is_byte<_Tp>::__value
        // #endif
        //         >
        // ```
        //
        // The same seam as the one between requirements (`parse_requires_expression`), between the elements of a
        // braced initialiser and between the members of a class: a `#` here cannot be anything else, because an
        // argument begins with a type, an expression — or a directive.
        if p.current_token() == CppTokenKind::Hash {
            if let Err(err) = super::super::stats::parse_preprocessor_directive(p) {
                p.close_marks_above(base);
                return Err(err);
            }
            continue;
        }

        let before = p.current_token_index();
        let argument = p.mark(CppSyntaxKind::TemplateArgument);
        if parse_template_argument(p).is_err() {
            argument.undo(p);
            p.close_marks_above(base);
            return Err(CppParseError::syntax_error_from(
                &t!("expected a template argument"),
                p.current_token_range(),
            ));
        }
        argument.complete(p);

        // An argument reading can succeed without consuming anything (it can return as soon as it
        // sees a delimiter). Without this guard a token that is neither a delimiter nor an argument
        // would spin the loop forever.
        if p.current_token_index() == before {
            p.close_marks_above(base);
            return Err(CppParseError::syntax_error_from(
                &t!("expected a template argument"),
                p.current_token_range(),
            ));
        }

        // …and the same seam **after** the argument, before the comma that would separate it from the next one —
        // the spelling this list is written with when the *arguments* are the thing being conditioned:
        //
        // ```cpp
        //     using __is_signed_integer = __is_one_of<__remove_cv_t<_Tp>,
        //       signed char, signed short, signed int, signed long,
        //       signed long long
        // #if defined(__GLIBCXX_TYPE_INT_N_0)
        //       , signed __GLIBCXX_TYPE_INT_N_0
        // #endif
        // ```
        //
        // The `#if` sits between an argument and the comma that follows it, so a seam only at the top of the loop
        // (the one above) never sees it: the loop came back around to a comma check, not to an argument. Both
        // sides of the comma need the seam, exactly as in the enumerator list.
        while p.current_token() == CppTokenKind::Hash {
            if let Err(err) = super::super::stats::parse_preprocessor_directive(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }

        match p.current_token() {
            CppTokenKind::Comma => {
                p.bump();
                // A trailing comma is not valid, but recovering here beats failing.
                continue;
            }
            _ => {
                // A `>>`-family token still needs splitting before it can close this list.
                at_a_closing_angle(p);
            }
        }
    }

    p.close_marks_above(base);
    Err(CppParseError::syntax_error_from(
        &t!("unterminated template argument list"),
        p.current_token_range(),
    ))
}

/// Is the cursor on a `>` that can close a template argument list?
///
/// A lone `>` is **not** consumed here: the caller decides whether it closes a list or is a comparison, which is
/// what lets every caller share this.
///
/// # This used to be `split_closing_angle`, and it used to do something
///
/// The lexer produced `>>`, `>=` and `>>=` as single tokens, so a `>` that closed a list had to be taken out of
/// one — in place, with `Vec::insert`, which shifts every token after the cursor. That is what made a parse cost
/// `O(k · n)`: 2759 ms on the 3.3 MB cooked `<vector>` stream, at a per-byte cost climbing 244 → 847 µs/KB.
///
/// The split now happens **once, at the end of maximal munch** (`CppLexer::tokenize`), so by the time any grammar
/// rule runs there is nothing to take apart: a `>` is one token, always. What is left is the question the callers
/// were really asking, and the name says it.
pub(crate) fn at_a_closing_angle(p: &CppParser) -> bool {
    p.current_token() == CppTokenKind::Greater
}

/// Parse one template argument: a type, a template-id, or a constant expression.
///
/// Returns `Ok` with an empty marker when the type reading applied: the argument *was* the type, and
/// the cursor is already on the delimiter that ends it. `Err` means neither reading worked.
pub(crate) fn parse_template_argument(p: &mut CppParser) -> ParseResult {
    let checkpoint = p.checkpoint();
    let start = p.current_token_index();

    // **An argument must not answer for the declaration that contains it.**
    //
    // `parse_type_id` opens a specifier sequence, and a specifier sequence announces itself with
    // [`CppParser::begin_declaration_type`] — which parks the enclosing declaration's name and clears the
    // field. Nothing put it back, so a declaration written `G<double> d(…)` lost its `"G"` the moment the
    // argument was read, and the declarator afterwards found whatever the *argument* had left behind: `"S"` for
    // `G<S>`, and **nothing** for `G<double>`, whose argument is a builtin keyword and so runs no
    // name-recording branch at all.
    //
    // Saved and restored here, around one call, rather than by giving the specifier sequence a matching "end":
    // that was tried twice and both versions poison the parse (5 diagnostics became 399, then 71), because a
    // shared park slot written on entry has to be released on *every* exit to be safe. A local has no state to
    // leak. The error paths need nothing: a failed argument fails the whole template-id, and the rollback at
    // `checkpoint` puts the field back itself.
    let outer_type = p.declaration_type_record();

    let type_read = parse_type_id(p);

    p.set_declaration_type_record(outer_type);

    // `std::tuple<Ts...>` — a **pack expansion as a template argument**. The type reading stops at the
    // ellipsis, because a `...` is not something a type-id can continue with; what it means is that the type
    // just read is the *pattern* of an expansion.
    //
    // The ellipsis is consumed here, as part of this argument, and it has to be: the list's own loop would
    // otherwise come back around, read the `...` as an argument of its own and report it as one it cannot
    // parse — which is exactly how `std::tuple<Ts...>` came to fail while the same spelling in an argument
    // list (`g(args...)`) had just started working.
    //
    // It stays a bare token rather than becoming a `PackExpansionExpr`, because the type reading has already
    // closed its node by the time the ellipsis is seen and re-parenting it here would mean reopening a
    // completed node. A consumer asking whether the argument is a pack finds the ellipsis in its tokens,
    // which is the same answer the expression path gives through its node kind.
    if type_read.is_ok() && p.current_token() == CppTokenKind::Ellipsis {
        p.bump(); // `...`
    }

    // Did the type reading get anywhere, and stop somewhere a type can end?
    //
    // The test is deliberately *not* a comparison of `<>` nesting depths. Depth recomputed on either
    // side of a sub-parse disagrees about the same token, because parsing an inner argument list
    // splits a `>>` into two `>`s and changes what the scan counts. "Did this reading consume
    // something, and is the cursor now on a token that cannot continue a type?" has one answer
    // whenever it is asked.
    //
    // **A `(` that cannot hold a parameter list is not an end**, because the type reading is then one token short
    // of the right answer rather than finished: `_S_use_relocate()` is a call, and the type reading stops after
    // the name (see [`a_parameter_list_is_the_type`] for the two spellings that share the shape).
    let stopped_at_a_group_that_is_not_a_parameter_list =
        p.current_token() == CppTokenKind::LeftParen && !a_parameter_list_is_the_type(p);

    // **A brace after the type is not the end of the argument** — it makes the argument a *value*, not a type:
    //
    // ```cpp
    // X<int{}> m;                     a braced functional conversion, argument of a template-id
    // __void_t<…, decltype(…allocate(size_t{}))>        bits/alloc_traits.h:941, where this was measured
    // X<A{1, 2}> m;
    // ```
    //
    // C++11's `T{…}` is the same construction the parenthesis form is, and an argument list is one of the places it
    // is written. The type reading stops at the `{` — a brace cannot continue a type — and *that stop was accepted
    // as the end of the argument*, because the type reading had succeeded. The list then wanted a `,` or a `>` and
    // found a brace, and the failure was **silent in the worst way**: the whole statement came back as a comparison
    // (`X < int{} > m`), well formed, lossless, with no diagnostic and no `ErrorNode`, and the declaration the user
    // wrote was simply not there. `S<size_t{}> x;` had the same reading; only the *keyword* spelling was loud,
    // because `int` has no expression reading of its own to fall back on.
    //
    // Nothing else can be at this position: a `{` inside an argument list cannot belong to an enclosing construct —
    // the list has to be closed by a `>` first — and a braced-init-list is not a template argument on its own, so
    // the expression reading below is the only one that can take these tokens. It handles both spellings: a keyword
    // type through the functional-conversion arm of `parse_primary_expr`, a name through the ordinary one.
    let a_brace_makes_it_a_value = p.current_token() == CppTokenKind::LeftBrace;

    // **Only a type reading that succeeded can end the argument**, and the difference is not pedantry: a *failed*
    // reading stopped wherever it gave up, which can be in the middle of a group it opened, and the tokens it
    // consumed on the way are still in the events. Accepting that as the argument is how
    //
    // ```cpp
    //   __enable_if_t<(__i >= sizeof...(_Types))>          // tuple:2439
    //   __get_helper(const tuple<_Types...>&) = delete;
    // ```
    //
    // came out as ``expected a declarator name`` at the `=`: the type reading on `(` consumed `(__i`, gave up at
    // `>=`, and the argument was declared *complete* there — so the loop below split the `>=` into `>` and `=`, the
    // `>` closed the list, and the rest of the declaration had no head. The expression fallback below is what
    // should have run, and it does now.
    //
    // What used to stand here was `type_read.is_ok() || !continues_a_type(p.current_token())`, whose second half
    // tried to rescue a failed reading that "stopped somewhere a type can end". There is no such thing: the token
    // kinds that end a type are the *delimiters*, and an argument that ends at a delimiter ends there whether or not
    // the reading before it succeeded — the `current_token_index() > start` test above is what says it consumed
    // anything at all. `continues_a_type` had no other caller and is gone with it.
    // **…and nothing may follow that a type cannot hold.** `g<D> == 1` is an *expression* whose left operand
    // happens to read as a type: the type reading succeeds, stops at the `==`, and taking that stop as the end of
    // the argument leaves the `==` to the list — which then wants a `,` or a `>` and reports the whole thing as a
    // comparison, so `F<g<D> == 1>`, `F<g<D>::value == 1>` and `tuple`'s
    // `_Tuple_conditional_explicit_v0<tuple_size_v<_Dest> == sizeof...(_Srcs), …>` all failed at the operator
    // with ``expected primary expression``.
    //
    // The operators listed by [`continues_an_expression`] cannot appear inside a type at all, so the argument is
    // an expression and the reading below takes it. Nothing is given up: a type reading that was *wrong* is
    // rewound by the branch below and the list fails, while an expression reading taken one token too eagerly
    // would take `S<int*>` apart — which is why the operators that decorate a type (`*`, `&`, `&&`) are not on
    // the list.
    let continues_as_an_expression = continues_an_expression(p.current_token());

    if p.current_token_index() > start
        && type_read.is_ok()
        && !stopped_at_a_group_that_is_not_a_parameter_list
        && !a_brace_makes_it_a_value
        && !continues_as_an_expression
    {
        return Ok(CompleteMarker::empty());
    }

    // Nothing usable: read it as an expression instead.
    //
    // **Below the comma**, like every other rule that spells its own separators ([`Level`] in `exprs` keeps the
    // list, and this rule is on it). `parse_expr` took the comma and made the *list* one argument:
    //
    // ```text
    // X<!C<T>, bool> f;        the argument came out as `!C < T , ` with an empty right side, and the `bool`,
    //                          the `>`, the name and the body were all rubble
    // S<3, 4> x;               reads with **no diagnostic at all**, as one argument `(3, 4)` — the comma operator
    //                          was taken for the separator, which no count can see
    // S<3, long> x;            `expected primary expression` against `long`, because the type has no expression
    //                          reading: the loud half of the same defect
    // ```
    //
    // The type reading is what hides this: `Grid<T, 3>::fill` and `array<int, 3>` put the comma *after* a type the
    // type reading already accepted, so the fallback never ran for them and the documented fix looked done.
    //
    // And **nothing more than that**: a `rollback` only truncates the event stream, so the type reading just
    // thrown away cannot be put back — it would have to be read again. An earlier version of this tried to
    // "keep the type reading" by rolling forward to a checkpoint taken after it, which truncates nothing (the
    // events are already gone) and returns `Ok` with the cursor wherever the *failed* expression stopped: the
    // markers left open that way came out as an unpaired forward reference, and `bits/tuple` panicked the tree
    // builder with "forward parent must point at a NodeStart, found Trivia". One reading, one rewind.
    p.rollback(checkpoint);
    super::super::exprs::parse_assignment_expr(p)
}

/// Can this token continue an **expression** where a type has just ended?
///
/// The operators that cannot appear inside a type in any position: equality, arithmetic, bitwise, the conditional
/// `?`, `||` — and the two **member accessors**.
///
/// **The delimiters are deliberately absent**, and the first version of this list is why the note exists: a bare
/// `>` ends the argument (the list owns it) and a `<` opens a nested list, so listing them made the rule refuse
/// *every* type argument — the 128-file libstdc++ corpus went from **128 clean files and 0 messages to 74 and
/// 865**, most of them ``expected a template argument``. `*`, `&` and `&&` are absent for the other reason: they
/// decorate a type as readily as they multiply one (`S<int*>`, `S<int&>`, `S<int&&>`), so an argument that stops at
/// one of them stays with the type reading. `>=`/`<=` are absent because a closing `>` followed by `=` is split by
/// [`at_a_closing_angle`] before this question is asked.
///
/// # `.` and `->`, and why they are not ambiguous the way `*` is
///
/// A member access is the one continuation that **settles** the element rather than decorating it: no type-id is
/// spelled with a `.` or a `->` anywhere in it — a qualified name uses `::`, which is a different token and stays
/// off this list. So `D<a.b, int>` is an argument that is an *expression*, and the type reading stopping after `a`
/// is one token short of the right answer rather than finished.
///
/// It was not on the list, and the cost was measured: `<random>` writes
///
/// ```cpp
/// using _Sx_type = conditional_t<_Params._Smax_bits <= 32, uint32_t,
///                                conditional_t<_Params._Smax_bits <= 64, uint64_t, _Unsigned128>>;
/// ```
///
/// — an argument that *begins* with a member access. The type reading took `_Params`, the argument was declared
/// complete at the `.`, and the list then wanted a `,` or a `>`: ``expected a template argument``, followed by the
/// three diagnostics that fall out of the declaration it was in.
pub(crate) fn continues_an_expression(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Equal
            | CppTokenKind::NotEqual
            | CppTokenKind::LessEqual
            | CppTokenKind::Plus
            | CppTokenKind::Minus
            | CppTokenKind::Slash
            | CppTokenKind::Percent
            | CppTokenKind::Pipe
            | CppTokenKind::Caret
            | CppTokenKind::LogicalOr
            | CppTokenKind::Question
            | CppTokenKind::Dot
            | CppTokenKind::Arrow
    )
}
