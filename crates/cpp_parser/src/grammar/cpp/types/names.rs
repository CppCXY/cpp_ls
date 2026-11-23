//! **Names** — names, template-ids, overloadable operators, and the destructor spelling.
//!
//! Moved here from `types.rs` without a change.

use super::*;

/// Parse a name: `foo`, `ns::foo`, `::foo`, `Foo<int>`, `ns::Foo<int>::type`.
///
/// Template arguments are attempted speculatively, because `<` is also the less-than operator and
/// only the matching `>` (accounting for nesting) distinguishes them.
///
/// Exposed for the qualifier loop, which walks a name's segments and has to hand the rest of one back to this
/// rule: `using ns::f;` reads the first segment itself — to decide whether an `=` follows — and then asks for
/// everything after it.
pub fn parse_name(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::NameExpr);

    // A leading `::` makes the name fully qualified.
    if p.current_token() == CppTokenKind::Scope {
        p.bump();
    }

    loop {
        // A **directive where a name segment begins** — after a `::`, or where the first segment stands:
        //
        // ```cpp
        //     vector<_Tp, _Alloc>::                    // bits/vector.tcc:133
        // #if __cplusplus >= 201103L
        //     insert(const_iterator __position, const value_type& __x)
        // #else
        //     insert(iterator __position, const value_type& __x)
        // #endif
        //     { … }
        // ```
        //
        // A `#` at a segment position cannot be anything else: a segment is a name, a `template` disambiguator,
        // `operator`/`~` — or a directive. Read it and ask again, which is what every other seam in this grammar
        // does. (In `bits/vector.tcc` the branch's parameter list follows the name, and the `#else` after it is
        // read by the declarator's own suffix reader — the same alternation, one position later.)
        while p.current_token() == CppTokenKind::Hash {
            if let Err(err) = super::super::stats::parse_preprocessor_directive(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }

        // `template` as a disambiguator rather than a name: `T::template rebind<U>`. It says the `<` after
        // the name that follows starts template arguments instead of a comparison, which is the one thing
        // that cannot be known about a dependent name before its arguments are. It is a keyword in this
        // lexer, so the segment match below — which knows identifiers, `~` and `operator` — refused it, and
        // a declaration whose type was spelled that way failed with `expected a name`.
        //
        // Handled *before* the match rather than in it, because the keyword is not a segment: the name it
        // qualifies is, and the loop has to come back around to read it.
        if p.current_token() == CppTokenKind::TemplateKeyword {
            p.bump();
            if !matches!(
                p.current_token(),
                CppTokenKind::Identifier | CppTokenKind::OperatorKeyword | CppTokenKind::Tilde
            ) {
                p.close_marks_above(base);
                return Err(CppParseError::syntax_error_from(
                    &t!("expected a name after `template`"),
                    p.current_token_range(),
                ));
            }
        }

        match p.current_token() {
            // **A qualifier the file wrote as a macro**: `_STD reverse_iterator<iterator>` with
            // `#define _STD ::std::`. Read as what it is — a `MacroCall` — and then the loop comes back around to
            // read the segment it qualifies. Without this the specifier sequence took `_STD` for the type and the
            // name after it for something else: `using reverse_iterator = _STD reverse_iterator<iterator>;` came
            // out as a type plus a nested `Declaration` (a recovery), which in MSVC's `<vector>` is an
            // empty-named fact filed in `std::vector` — enough to make `std::vector` itself ambiguous.
            CppTokenKind::Identifier => p.bump(),
            // **`default` as a namespace name** — C++/CX's ABI namespace, which MSVC's own `vccorlib.h` writes:
            // `long __Platform_WindowsCreateString(const ::default::char16*, unsigned int, __abi_HSTRING*);`. The
            // keyword is a name here and nowhere else, which is why the arm demands the `::` that follows it: a
            // `switch (n) { default: }` and `= default;` cannot reach a segment position at all, and this arm cannot
            // take a `default` that is not qualifying something.
            CppTokenKind::DefaultKeyword if p.peek_next_token() == CppTokenKind::Scope => p.bump(),
            // **`decltype(expr)` as the first segment of a qualified name**: `typename decltype(__pc)::iterator`.
            // The type it names is what the rest of the name is looked up in, so it stands where a namespace or a
            // class would. MSVC's `<format>` writes `same_as<typename decltype(__pc)::iterator>` in two
            // concepts, and the name rule refused the keyword.
            CppTokenKind::DecltypeKeyword if p.peek_next_token() == CppTokenKind::LeftParen => {
                p.bump(); // `decltype`
                p.bump(); // `(`
                let payload = if p.current_token() == CppTokenKind::AutoKeyword {
                    parse_type_id(p).map(|_| ())
                } else {
                    super::super::exprs::parse_expr(p).map(|_| ())
                };
                if let Err(err) = payload.and_then(|()| expect_token(p, CppTokenKind::RightParen)) {
                    p.close_marks_above(base);
                    return Err(err);
                }
            }
            // `operator+`, `operator()`, `operator new`, `operator""_x`...
            CppTokenKind::OperatorKeyword => {
                parse_operator_name(p)?;
                break;
            }
            // `~Foo` names a destructor.
            CppTokenKind::Tilde => {
                p.bump();
                if p.current_token() == CppTokenKind::Identifier {
                    p.bump();
                }
                break;
            }
            _ => {
                p.close_marks_above(base);
                return Err(CppParseError::syntax_error_from(
                    &t!("expected a name"),
                    p.current_token_range(),
                ));
            }
        }

        // Template arguments, if this really is a template-id.
        //
        // The `<` is also the less-than operator, and the two are only distinguishable by finding
        // the matching `>`. Deciding *before* descending matters: parsing the arguments and rolling
        // back on failure would throw away the argument nodes that were already built, and the
        // caller only ever sees "this was not a name after all" instead of "the name ended here".
        if p.current_token() == CppTokenKind::Less
            && a_matching_angle_bracket_follows(p)
            && let Err(err) = parse_template_argument_list(p)
        {
            p.close_marks_above(base);
            return Err(err);
        }
        if p.current_token() == CppTokenKind::Scope {
            // A `::` *inside* the name: this name is qualified, which the declarator needs to know. Recorded
            // here because this is the loop that reads the segments, and a caller that saw only the name's first
            // token cannot tell `ns::C::method` from `method` — the two are one call to this function from
            // outside. See [`CppParser::has_qualified_declaration_type_name`].
            p.note_qualified_declaration_type();
            p.bump();
            continue;
        }

        // **`MACRO name` in a type — the type-side half of the rule the expression grammar already applies.**
        //
        // The paragraph above this loop describes the shape and reads it through a `MacroCall` node, which works
        // when the file wrote the `#define`. Standard-library headers do not: `_STD` is defined in `<yvals.h>`,
        // so to a file parsed on its own it is an ordinary **identifier** and the name it qualifies follows it —
        // two identifiers in a row with no `::` between them.
        //
        // ```cpp
        // using reverse_iterator = _STD reverse_iterator<iterator>;      // <array>, and every container
        // ```
        //
        // Refusing the pair ended the name at `_STD`, the specifier sequence took **that** for the type, and the
        // real name became a nested `Declaration` produced by recovery — which is what the paragraph above
        // describes as making `std::vector` ambiguous. Measured on MSVC's `<array>`: `class array` summarised to
        // one fact, none of its members were facts at all, and the 2000 lines written after it produced nothing.
        //
        break;
    }

    Ok(m.complete(p))
}

/// Does a `<` at the cursor look like the start of template arguments?
///
/// Exposed for the expression grammar, which parses `std::vector<int>` and `f<int>(x)` as
/// expressions and needs the same decision.
pub fn could_start_template_arguments(p: &CppParser) -> bool {
    p.current_token() == CppTokenKind::Less && a_matching_angle_bracket_follows(p)
}

/// Does a matching `>` for the `<` at the cursor appear before anything that could not be inside a
/// template argument list?
///
/// This is a lookahead, not a parse. It exists because the decision has to be made *before* the
/// argument parser runs: attempting the parse and rolling back would discard the argument nodes and
/// lose the information that the name simply ended at the `<`.
///
/// The scan is deliberately shallow — it tracks `<`/`>` nesting but does not understand expressions
/// — so it can be fooled by `a < b > c` written without spaces. That form is rare, and the failure
/// mode is benign: the tokens are still parsed, just as a template-id rather than a comparison.
///
/// # Why a top-level `,` is not a stop
///
/// A comma at depth 1 separates template arguments and is entirely ordinary: `Vec<std::vector<int>,
/// 3>` and `Map<K, V>` are template-ids. Only a comma *inside* a nested argument list means the
/// enclosing construct is a call or a declaration rather than a template-id, and that case is
/// already covered by the nesting itself — at depth 2 the scan is looking for the inner list's `>`,
/// and the stop set catches it if there is none.
///
/// Treating every comma as fatal was tried and is what made a multi-argument template-id with a
/// nested template-id in it unparsable, which is exactly the shape `Vec<std::vector<int>, 3>` has.
///
/// # Where it must not be shallow
///
/// The stop set below. The scan starts at a `<` and looks for the matching `>`; if it is willing to
/// run past a `)` or a `;`, it will find a `>` belonging to something else entirely later in the file
/// and report a template-id that is not there. That is not a rare form — `double f(const Point a) {
/// return a; }` contains a `>`-less declaration whose only `<`…`>` pair is nowhere near it, and
/// reading `Point a` as `Point<a>` swallows the parameter name and then the whole declaration.
pub(crate) fn a_matching_angle_bracket_follows(p: &CppParser) -> bool {
    // Relative offsets: `0` is the `<` at the cursor, so the scan starts at `1`.
    let mut depth = 1isize;
    // A **bracket** pair inside the arguments is not a boundary: `Vec<int[4]>` is an array type as an argument
    // and `Vec<arr[0]>` a subscript in a non-type one. Only an *unmatched* `]` ends the scan, which is the case
    // the stop set exists for — `a[b < c]`, where the `<` is a comparison inside an index.
    let mut brackets = 0isize;
    // …and neither is a **parenthesis** pair, for the same reason and with a case that is everywhere in real
    // code: a function type is a template argument —
    //
    // ```text
    // std::function<bool(TokenKind)>                 a predicate parameter
    // std::function<void(const std::string &)>       the same, with a parameter's own type inside
    // ```
    //
    // — and the parentheses around that parameter list are *matched*, so they belong to the list. Stopping at the
    // first `)` meant the template-id was never attempted at all: `sizeof(A<bool(T)>)` failed, `using F =
    // A<bool(T)>;` reported `expected ;` against its own `<`, and at file scope `A<bool(T)> x;` came out as the
    // **comparison** `A < bool(T) > x` — a declaration read as an expression, with no diagnostic anywhere, which
    // is the A0 shape this file exists to prevent.
    let mut parens = 0isize;
    // …and neither is a **braced-init-list**, which is the same argument a third time and the one that hid a
    // *silent* reading rather than a diagnostic. C++11's `T{…}` is a template argument like any other value:
    //
    // ```text
    // X<int{}> m;                     a braced functional conversion as the argument
    // X<A{1, 2}> m;                   …with a list in it
    // struct Q<T, int{}> { };         …in a partial specialisation's name
    // using A = X<int{}>;             …in an alias target
    // ```
    //
    // With `{` in the stop set the scan answered "this `<` is a comparison" for every one of them, so the name
    // was read as the bare `X` and the angles became rubble — and at file scope that meant `X<int{}> m;` came out
    // as **`X < int{} > m`**, a well-formed comparison with no diagnostic and no `ErrorNode`, while the same
    // statement with a *keyword* type inside (`int{}` has an expression reading now that the functional-conversion
    // arm takes a brace) was the only one that said anything. An unmatched `}` still ends the scan — that is the
    // enclosing declaration's own brace — so `T x{a < b}` and `struct S : B<C> {` read exactly as before.
    let mut braces = 0isize;
    // Is a `<` inside the list an **opener**, or is it the less-than operator?
    //
    // The token in front of it decides, and it is the same evidence a reader uses: a template's name is written
    // before its `<`. After a name (or after the `>` of an inner list) a `<` opens one; after anything else it is
    // a comparison, and counting it as an opener is what made the list's **own** `>` invisible — the scan ran one
    // level deep at the end and reported "no matching `>`", so the `<` was read as a less-than and the construct
    // it belonged to was not a template-id at all:
    //
    // ```text
    // C<1 < 2>                 the `<` after a literal     → a comparison, so `C<1 < 2>` closes at its own `>`
    // C<sizeof(T) < 3>         …after a `)`                 → the same
    // C<(T(0) < T(0))>         …inside a group              → the same
    // ::std::vector<int>       …after a name                → an inner list, counted
    // ```
    //
    // Measured against g++: it accepts all three of the comparison spellings as arguments of `C<…>` and reads the
    // fourth as a nested template-id, which is exactly what this test produces. The bias is unchanged for the
    // ambiguous case the scan has always had — `a < b > c` written without spaces still reads as a template-id —
    // because there the `<` *is* preceded by a name.
    let mut previous = CppTokenKind::Less;
    // How many **ternaries** are open, so that their `:` is not mistaken for the colon that ends the scan.
    //
    // `Colon` is in the stop set below because a `:` cannot appear inside a template argument list — except as the
    // middle of a conditional expression, which is an argument like any other value:
    //
    // ```cpp
    //   template<borrowed_range _Rng>                       // bits/ranges_util.h:436
    //     subrange(_Rng&&)
    //       -> subrange<iterator_t<_Rng>, sentinel_t<_Rng>,
    //           (sized_range<_Rng> || sized_sentinel_for<…>)
    //           ? subrange_kind::sized : subrange_kind::unsized>;
    // ```
    //
    // The `:` there ended the scan, so the `<` was reported as "not a template-id", the deduction guide stopped at
    // its own name, and the file reported ``expected `;` `` against the `<` of the return type. Counting the `?` is
    // the same kind of shallow shape the scan already uses for brackets and braces, and it is exact where it
    // matters: a `:` with no `?` in front of it still ends the scan.
    let mut open_questions = 0isize;

    'scan: {
        // **To the end of the file, not to a fixed window.** This scan looks for the `>` that closes the cursor's `<`,
        // and answering "no matching angle" because 128 tokens went by is a budget reported as a grammatical fact: a
        // template-id with 64 arguments (about 130 tokens) was refused, and the alias or type that used it was reported
        // as ``expected `;` `` at its own `<` (`docs/worklog.md` P2-1, measured with
        // `target/bench/threshold_probe.py`). The loop stops at the matching `>` or at EOF, so the file's length is the
        // only budget it needs.
        let from = p.current_token_index() + 1;
        let reach = p.token_count().saturating_sub(from);
        for kind in (0..reach).map(|offset| p.token_kind_at(from + offset)) {
            match kind {
                // **An angle inside matched parentheses is an expression's, not this list's.**
                //
                // The parens counter above was written to keep a matched group from *ending* the scan, and that is
                // only half of what a matched group means: nothing inside it is a boundary of this list either —
                // including the `<` of a comparison, which otherwise opens a depth that nothing closes:
                //
                // ```cpp
                // template <size_t _First, size_t _Second, size_t... _Rest>
                // struct _Maximum<_First, _Second, _Rest...>
                //     : _Maximum<(_First < _Second ? _Second : _First), _Rest...>::type { … }   // type_traits:1108
                // ```
                //
                // The `<` of `_First < _Second` made the depth 2, the list's own `>` brought it back to 1, and the
                // scan then met the `:` of `::type` and answered "this `<` is a comparison after all" — so the
                // template-id was not one, the type ended at `_Maximum`, and the base clause was reported as
                // ``expected `;` `` at the `<`. Angles inside a matched group are **balanced within it** in any
                // valid argument, so ignoring the whole family there cannot lose a `>` that closes this list.
                CppTokenKind::Less | CppTokenKind::Greater | CppTokenKind::RightShift
                    if parens > 0 => {}
                CppTokenKind::Less if can_end_a_template_name(previous) => depth += 1,
                CppTokenKind::Question => open_questions += 1,
                CppTokenKind::Colon if open_questions > 0 => open_questions -= 1,
                CppTokenKind::LeftBracket => brackets += 1,
                CppTokenKind::RightBracket if brackets > 0 => brackets -= 1,
                CppTokenKind::LeftParen => parens += 1,
                CppTokenKind::RightParen if parens > 0 => parens -= 1,
                CppTokenKind::LeftBrace => braces += 1,
                CppTokenKind::RightBrace if braces > 0 => braces -= 1,
                CppTokenKind::Greater | CppTokenKind::RightShift => {
                    // `>>` closes two levels at once; a lone `>` closes one. Both arrive here because
                    // the amount is all that differs.
                    depth -= if kind == CppTokenKind::RightShift {
                        2
                    } else {
                        1
                    };
                    if depth <= 0 {
                        break 'scan true;
                    }
                }
                // Anything that cannot appear between a `<` and its `>`, however deeply nested: the
                // structural boundaries of the enclosing declaration. A template argument list never
                // contains an unmatched one of these, so reaching one means this `<` was a less-than
                // after all.
                //
                // **A matched group hides them**, and that is the second half of the rule the counters above
                // state: a `;` inside a `{ … }` is a statement in a lambda's body or in a *requires-expression*
                // (`bool_constant<!requires(_Tp __t, void(*__f)(int)) { __f(__t); }>`, `type_traits:3947`), a `=`
                // or a `->` inside `( … )` is part of an expression, and none of them ends the enclosing
                // declaration. Only at depth zero are they the boundaries this set is for.
                CppTokenKind::Semicolon
                | CppTokenKind::Assign
                | CppTokenKind::Arrow
                | CppTokenKind::Colon
                    if parens == 0 && brackets == 0 && braces == 0 =>
                {
                    break 'scan false;
                }
                CppTokenKind::RightBrace
                | CppTokenKind::RightParen
                | CppTokenKind::RightBracket
                | CppTokenKind::Eof
                | CppTokenKind::None
                | CppTokenKind::LineComment
                | CppTokenKind::BlockComment => break 'scan false,
                _ => {}
            }

            previous = kind;
        }
        false
    }
}

/// Is the cursor on a name that is **only** a template-id — `C2<T>` rather than `C2<T>::name`?
///
/// The distinction is what [`parse_declarator_with`] needs to refuse the first as a declarator's name. It is
/// visible in the tokens and needs no name lookup: find the `>` that matches the `<`, and ask what follows it. A
/// `::` means the arguments belong to a *qualifier* and the name continues; anything else means the template-id
/// was the whole name, which no declaration can name.
///
/// The scan is the same shape as [`a_matching_angle_bracket_follows`] and stops at the same tokens, for the same
/// reason: running past a `;` would find a `>` belonging to another declaration entirely.
pub(crate) fn a_bare_template_id_is_here(p: &CppParser) -> bool {
    // An **explicit instantiation** is the one declaration whose *name* is a template-id —
    // `extern template void f<int>(int);` asks for the instantiation of `f<int>` — so the rule is suspended
    // there and nowhere else. See [`CppParser::a_template_id_may_be_the_name`].
    if p.a_template_id_may_be_the_name() {
        return false;
    }

    if p.current_token() != CppTokenKind::Identifier || p.peek_next_token() != CppTokenKind::Less {
        return false;
    }

    // Offsets are relative to the cursor: the `<` is at 1, which the depth above counts, so the scan starts at
    // 2 — the first token *inside* the list — and the answer is read one past the matching `>`.
    let mut depth = 1isize;
    // See the stop set below: an *unmatched* `)` ends the scan, a matched pair does not.
    let mut parens = 0isize;
    // …and a matched **brace** pair does not either, for the reason written on the same counter in
    // [`a_matching_angle_bracket_follows`]: `struct Q<T, int{}> { }` is a partial specialisation whose name is a
    // template-id, and a scan that stopped at the `{` called it "not a template-id" — so the class-head rule read
    // the name as the bare `Q` and the arguments became rubble.
    let mut braces = 0isize;
    // The token before the one being looked at — see the note on the same variable in
    // [`a_matching_angle_bracket_follows`]. The scan starts inside the list, so its first token's predecessor
    // is the list's own `<`, which ends no template-name.
    let mut previous = CppTokenKind::Less;

    // **To the end of the file, not to a fixed window** — the same defect the scan above had, in the rule that decides
    // whether a name in type position is a template-id rather than a comparison. See that note.
    let scan_from = p.current_token_index() + 2;
    let scan_reach = p.token_count().saturating_sub(scan_from);
    for (index, kind) in (0..scan_reach)
        .map(|offset| p.token_kind_at(scan_from + offset))
        .enumerate()
    {
        match kind {
            // The `<` question, asked of the token before it — see [`angle_depth_delta`]. The scan starts inside
            // the list, so the first token's predecessor is the list's own `<`, which ends no template-name.
            CppTokenKind::Less if can_end_a_template_name(previous) => depth += 1,
            CppTokenKind::Greater | CppTokenKind::RightShift => {
                // `>>` closes two levels at once, so it can be the matching `>` of either the inner or the
                // outer list; both arrive here because the amount is all that differs.
                depth -= if kind == CppTokenKind::RightShift {
                    2
                } else {
                    1
                };
                if depth <= 0 {
                    let after = index + 3;
                    let next = p.peek_token_kind_at(after..after + 1).first().copied();
                    // **A friend declaration may name a template-id**: `friend int f<>(const C&);` and
                    // `friend void g<int>(T);` befriend one specialization of a function template, and the
                    // arguments belong to the name. (MSVC's `<xloctime>`/`<xlocnum>` write the first.)
                    let a_friend_names_a_specialization = next == Some(CppTokenKind::LeftParen)
                        && p.is_open(CppSyntaxKind::FriendDecl);
                    return next != Some(CppTokenKind::Scope) && !a_friend_names_a_specialization;
                }
            }
            CppTokenKind::Semicolon
            | CppTokenKind::RightBracket
            | CppTokenKind::Colon
            | CppTokenKind::Assign
            | CppTokenKind::Arrow
            | CppTokenKind::Eof
            | CppTokenKind::None
            | CppTokenKind::LineComment
            | CppTokenKind::BlockComment => return false,
            // An **unmatched** `)`, exactly as in [`a_matching_angle_bracket_follows`]: a *matched* pair is a
            // function type as a template argument (`F<bool(T)>`), which this scan has to see past for the same
            // reason the other one does. The brace pair is the same shape one counter along — its guarded arm has
            // to come *before* the stop set, or the stop set's `RightBrace` swallows it.
            CppTokenKind::LeftParen => parens += 1,
            CppTokenKind::RightParen if parens > 0 => parens -= 1,
            CppTokenKind::LeftBrace => braces += 1,
            CppTokenKind::RightBrace if braces > 0 => braces -= 1,
            CppTokenKind::RightParen | CppTokenKind::RightBrace => return false,
            _ => {}
        }

        previous = kind;
    }

    false
}

/// Parse an operator name after the `operator` keyword.
/// Is the `&&` at the cursor a **ref-qualifier** rather than part of a conversion operator's type?
///
/// `operator T&&()` names an rvalue reference and `operator bool() &&` qualifies the member function. Both are
/// a `&&` followed by a `(`, so the tokens alone do not separate them — one more thing does, and it is the one
/// a *reader* uses: whether the two are written together.
///
/// ```text
/// operator T&&()      the `&&` and the `(` touch     -> the type continues
/// operator bool() &&  the `&&` and the `(` do not    -> the function is qualified
/// operator bool() &&; nothing follows but the `;`    -> the function is qualified
/// ```
///
/// Reading the tokens as the source wrote them is exactly the kind of context-free evidence this parser is
/// allowed to use. The failure mode if someone writes `operator bool()&&` with no space is a name that runs one
/// token long and a declaration that then reports a missing `;` — loud, local, and one edit away from working.
pub(crate) fn a_ref_qualifier_is_here(p: &CppParser) -> bool {
    match p.peek_token_kind_at(1..2).first() {
        // A `;` after the `&&` can only be a qualifier: no type is spelled that way.
        Some(&CppTokenKind::Semicolon) => true,
        Some(&CppTokenKind::LeftParen) => {
            let rvalue_ref = p.current_token_range();
            let parameter_list = p.peek_token_range_at(1);
            rvalue_ref.end_offset() != parameter_list.start_offset
        }
        _ => false,
    }
}

/// Parse an operator name after the `operator` keyword.
///
/// Exposed for the expression grammar, which reads the same names in the same position: `Foo::operator+()` is an
/// expression's callee before it is a declaration's name, and a second rule for it would be a second answer to
/// "what is an operator name".
pub fn parse_operator_name_here(p: &mut CppParser) -> ParseResult {
    parse_operator_name(p)
}

pub(crate) fn parse_operator_name(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::NameExpr);

    expect_token(p, CppTokenKind::OperatorKeyword)?;

    match p.current_token() {
        // `operator new`, `operator delete`, `operator new[]`, ...
        CppTokenKind::NewKeyword | CppTokenKind::DeleteKeyword => {
            p.bump();
            if p.current_token() == CppTokenKind::LeftBracket {
                expect_token(p, CppTokenKind::LeftBracket)?;
                expect_token(p, CppTokenKind::RightBracket)?;
            }
        }
        // `operator()`, `operator[]`
        CppTokenKind::LeftParen => {
            expect_token(p, CppTokenKind::LeftParen)?;
            expect_token(p, CppTokenKind::RightParen)?;
        }
        CppTokenKind::LeftBracket => {
            expect_token(p, CppTokenKind::LeftBracket)?;
            expect_token(p, CppTokenKind::RightBracket)?;
        }
        // `operator bool`, `operator int`, `operator MyType`, `operator std::string`,
        // `operator const char*` — a **conversion** operator, which is spelled with the type it converts to
        // rather than with a symbol. A keyword type is as ordinary here as a class name is.
        //
        // Read as one flat run of tokens rather than by the type grammar, and the reason is that the type
        // grammar would *own* the name. `parse_type_id` reads a declarator, and a declarator is where a name
        // is recorded — so `operator int()` came out as a declaration of a variable called `int`, and the
        // file's type table learned that `int` is a type this file declared. An operator name is a
        // declaration's *name*, and nothing may claim it on the way past.
        //
        // The run is delimited by what a conversion-type-id cannot contain: an identifier, a `::`, a
        // qualified or template-id name, the qualifiers and the pointer/reference operators, and the
        // keywords that name a type. It stops at the `(` of the parameter list and at the `;` of a
        // declaration, which is what keeps it from walking into the rest of the file.
        //
        // `const` and `volatile` are in the *entry* test as well as in the loop, and the first version of
        // this arm had them only in the loop — which is a bug with no symptom until it is written down:
        // `operator char()` parsed and `operator const char()` did not, because the guard decides on the
        // token the name *begins* with and `const` is not a type specifier.
        kind if is_type_specifier_keyword(kind)
            || is_class_like_keyword(kind)
            || matches!(
                kind,
                CppTokenKind::Identifier
                    | CppTokenKind::Scope
                    | CppTokenKind::ConstKeyword
                    | CppTokenKind::VolatileKeyword
            ) =>
        {
            loop {
                if p.current_token() == CppTokenKind::LogicalAnd && a_ref_qualifier_is_here(p) {
                    break;
                }

                match p.current_token() {
                    kind if is_type_specifier_keyword(kind)
                        || is_class_like_keyword(kind)
                        || matches!(
                            kind,
                            CppTokenKind::Identifier
                                | CppTokenKind::ConstKeyword
                                | CppTokenKind::VolatileKeyword
                                | CppTokenKind::Scope
                                | CppTokenKind::Star
                                | CppTokenKind::Ampersand
                                | CppTokenKind::LogicalAnd
                        ) =>
                    {
                        p.bump();
                    }
                    // `operator std::vector<int>` — the template arguments belong to the name, and skipping
                    // them is what keeps a nested `>` from being read as an operator somewhere else.
                    CppTokenKind::Less if could_start_template_arguments(p) => {
                        parse_template_argument_list(p)?;
                    }
                    _ => break,
                }
            }
        }
        // `operator""_suffix` — a user-defined literal operator.
        //
        // **Two spellings, and the standard allows both**: `literal-operator-id` is `operator "" identifier` as
        // well as `operator user-defined-string-literal`. Which one was written decides how much of the name the
        // lexer put in one token — written together the literal *is* the whole name (a `UserDefinedLiteral`), and
        // written apart it is a `StringLiteral` and then an identifier. Only the first was read here, so the
        // second left the suffix for the declarator: `_km` came out as a name where the parameter list was
        // expected, the declaration gave up, and the file got an `ExpressionStat` where a function definition was.
        //
        // Measured on MSVC's STL, which spells it apart: `<xstring>:1868` is
        // `_NODISCARD constexpr string_view operator"" sv(const char*, size_t) noexcept { … }`, and it is that
        // file's **first** error — 98 errors behind it, and the file is where every `std::string` fact comes from.
        // The suffix is an identifier and nothing else can follow `""` in a name, so reading it here is the
        // whole of the rule; `operator""` followed by anything else keeps the old reading.
        CppTokenKind::StringLiteral => {
            p.bump();

            if p.current_token() == CppTokenKind::Identifier {
                p.bump();
            }
        }
        // The suffix was written without a space, so the lexer already made one token of it.
        CppTokenKind::UserDefinedLiteral => p.bump(),
        // Any other overloadable operator — **joined, because the `>`-family spellings arrive broken up**.
        //
        // This arm used to say "a single token the lexer already produced, so `>=` and `>>=` are fine as-is",
        // and that stopped being true when the lexer began splitting them at the end of maximal munch: see
        // [`CppParser::joined_operator_at_the_cursor`]. Bumping once left the second half of `operator>>`
        // behind, and every file that *declares* one reported ``expected `;` `` at it —
        // `<__msvc_bit_utils.hpp>` writes `operator>>(const byte, _IntType)`, and it was most of the new
        // diagnostics on the cooked `<vector>` stream. The **name** of the operator is the joined spelling,
        // which is the whole reason the join exists.
        kind if is_overloadable_operator(kind) => {
            for _ in 0..p.joined_operator_at_the_cursor().1 {
                p.bump();
            }
        }
        _ => {
            let err = CppParseError::syntax_error_from(
                &t!("expected an operator name after `operator`"),
                p.current_token_range(),
            );
            p.close_marks_above(base);
            return Err(err);
        }
    }

    Ok(m.complete(p))
}

/// Tokens that can be an overloaded operator name.
pub(crate) fn is_overloadable_operator(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Plus
            | CppTokenKind::Minus
            | CppTokenKind::Star
            | CppTokenKind::Slash
            | CppTokenKind::Percent
            | CppTokenKind::Caret
            | CppTokenKind::Ampersand
            | CppTokenKind::Pipe
            | CppTokenKind::Tilde
            | CppTokenKind::LogicalNot
            | CppTokenKind::Assign
            | CppTokenKind::Less
            | CppTokenKind::Greater
            | CppTokenKind::PlusAssign
            | CppTokenKind::MinusAssign
            | CppTokenKind::StarAssign
            | CppTokenKind::SlashAssign
            | CppTokenKind::PercentAssign
            | CppTokenKind::CaretAssign
            | CppTokenKind::AmpersandAssign
            | CppTokenKind::PipeAssign
            | CppTokenKind::LeftShift
            | CppTokenKind::RightShift
            | CppTokenKind::LeftShiftAssign
            | CppTokenKind::RightShiftAssign
            | CppTokenKind::Equal
            | CppTokenKind::NotEqual
            | CppTokenKind::LessEqual
            | CppTokenKind::GreaterEqual
            | CppTokenKind::Spaceship
            | CppTokenKind::LogicalAnd
            | CppTokenKind::LogicalOr
            | CppTokenKind::PlusPlus
            | CppTokenKind::MinusMinus
            | CppTokenKind::Comma
            | CppTokenKind::Arrow
            | CppTokenKind::ArrowStar
            | CppTokenKind::DotStar
            // **`operator co_await`** — C++20's spelling for "this type is awaitable". It is a **keyword**, not a
            // punctuator, so the table did not hold it and the operator name was refused: measured on
            // `coroutines.cpp` of the C++ corpus, `awaiter operator co_await()` produced `ErrorNode`s, the class's
            // own node then reached past its closing brace, and its scope swallowed the three free coroutine
            // functions written after it — so `answer`/`doubled`/`custom` were filed as *members* of `Task`, carrying
            // its template parameter, and `auto task = doubled();` had no type at all.
            | CppTokenKind::CoAwaitKeyword
    )
}
