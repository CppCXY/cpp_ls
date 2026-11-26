//! **Attributes** — attribute specifiers, in both the standard and the single-bracket spelling.
//!
//! Moved here from `types.rs` without a change.

use super::*;

/// Is an attribute written at the cursor — in any of the three spellings?
///
/// The standard's `[[…]]` and the two extensions that mean the same thing: GNU's `__attribute__((…))` and MSVC's
/// `__declspec(…)`. One predicate, because they are one concept at one set of positions, and every caller that
/// allows an attribute should allow all three — the standard library is written with the GNU spelling, which sits
/// in `bits/c++config.h` and in `bits/move.h` (`__attribute__((__always_inline__))` between a template head and
/// the declaration it wraps).
///
/// GNU accepts the spelling with **one** trailing underscore as well, and the difference is not cosmetic: GCC's
/// own headers write it that way when they must stay readable by a compiler that does not know the extension —
///
/// ```cpp
/// extern "C"                                                          // libstdc++-v3/include/parallel/compatibility.h:48
/// __attribute((dllimport)) void __attribute__((stdcall)) Sleep (unsigned long);
/// ```
///
/// — so `__attribute` is a spelling, not a name, and reading it as one costs the declaration it stands in front
/// of. That was measured: the file above reported `expected ;` against its own `extern "C"`, and with the
/// spelling added the declaration reads. `g++ -std=c++17` accepts the same line, which is the evidence that this
/// is the compiler's extension rather than this parser's guess.
///
/// A **spelling** test rather than a table one, and reading a spelling is not by itself evidence. Two things make
/// it evidence here: the standard reserves these names to
/// the implementation, so a program that `#define`s `__attribute__` is not a program this has to read; and the
/// extension is the *compiler's*, not the file's, so no `#define` in any header is what makes it one. Both halves
/// have to hold for a spelling test to be legitimate, and neither holds for `MY_API`.
pub fn at_an_attribute(p: &CppParser) -> bool {
    match p.current_token() {
        CppTokenKind::LeftBracket => p.peek_next_token() == CppTokenKind::LeftBracket,
        CppTokenKind::Identifier => {
            matches!(
                p.current_token_text(),
                "__attribute__" | "__attribute" | "__declspec"
            ) && p.peek_next_token() == CppTokenKind::LeftParen
        }
        _ => false,
    }
}

/// Parse a run of attribute specifiers at the cursor, consuming as many as are written.
///
/// A *run*, because C++ lets attributes repeat — `[[nodiscard]] [[deprecated]] int f();` — and because the
/// grammar writes them as a list of specifiers rather than as one. The GNU spelling repeats too
/// (`__attribute__((a)) __attribute__((b))`), and so does a mixture of the two in a header that supports both.
///
/// Silent when there is nothing to read: this is called at positions where an attribute is *allowed* rather
/// than required, so a cursor that is not on one returns without touching the cursor and without an error.
/// [`at_an_attribute`] is what keeps it away from the forms where a `[` means something else — an array bound, a
/// lambda capture, a structured binding, an index — and from every ordinary name.
pub fn parse_attribute_specifiers(p: &mut CppParser) -> ParseResult {
    while at_an_attribute(p) {
        parse_attribute_specifier(p)?;
    }

    Ok(CompleteMarker::empty())
}

/// Read a run of attribute specifiers **where a declaration may write one**, which includes the single-bracket
/// spelling ([`at_a_single_bracket_attribute`]).
///
/// The one caller is the decl-specifier sequence, and the restriction is the same one the language makes: MSVC's
/// `[repeatable]` is accepted where a specifier goes and nowhere else. Keeping the wider reader for that one
/// position is what stops `a[i]` in an expression from ever reaching these rules — an index is a single bracket
/// around an expression, and a reader that claimed it everywhere would take the subscript out of every array use
/// in every file.
pub(crate) fn parse_attribute_specifiers_where_a_declaration_may_write_them(
    p: &mut CppParser,
) -> ParseResult {
    loop {
        if at_an_attribute(p) {
            parse_attribute_specifier(p)?;
            continue;
        }
        if at_a_single_bracket_attribute(p) {
            parse_single_bracket_attribute(p)?;
            continue;
        }
        return Ok(CompleteMarker::empty());
    }
}

/// Is the cursor on **MSVC's single-bracket pseudo-attribute** — `[repeatable]`, `[source_annotation_attribute(All)]`?
///
/// # What this spelling is, and why it is read here
///
/// It is not standard C++ and it is not `[[…]]`: it is the annotation syntax the *source-annotation* headers are
/// written in, and the compiler that owns them accepts it in exactly one position — where a decl-specifier goes.
/// MSVC's `sal.h` produces it from a macro (`#define REPEATABLE [repeatable]`) and
/// `codeanalysis/sourceannotations.h` writes it literally:
///
/// ```cpp
/// [source_annotation_attribute(SA(Method))] struct __M_impl { … };
/// REPEATABLE [source_annotation_attribute(SA(ReturnValue))] struct __R_impl { … };
/// ```
///
/// # Why it has to be read rather than reported
///
/// Because of what the recovery from it costs, which was measured rather than assumed. A decl-specifier sequence
/// that stopped at the single `[` left it to the expression rules, which read it as an **array subscript** and
/// reported `expected primary expression`; the recovery from each one left a brace unclosed, and 22 of them were
/// enough for `namespace vc_attributes` to swallow the remaining 2.1 MB of a translation unit that included
/// `<string>`. `std::basic_string` then lived in `vc_attributes::std`, so `std::string` — and every member query
/// through it — had no declaration anywhere.
///
/// # Why the shape test is this narrow
///
/// `[` means at least four other things, and all of them are legal in a declaration, so this refuses anything that
/// is not the exact shape above:
///
/// ```text
/// [name]                  accepted    the whole attribute
/// [name( … )]              accepted    its arguments, one balanced group
/// [2]         / []         refused     a bound is an expression, not a name
/// [name, other]            refused     a subscript with a comma operator
/// [name][2]                refused     an attribute followed by an array bound: read the bound
/// [name( … )][name]        refused     two subscripts
/// ```
///
/// The difference from `[[` is one token, so this is the *other* branch of the same decision rather than a second
/// spelling of it: [`at_an_attribute`] answers for both, and the reader below consumes whichever it finds.
pub fn at_a_single_bracket_attribute(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::LeftBracket {
        return false;
    }

    // The tokens after the `[`, up to a bound: the attribute is an identifier, an optional balanced argument
    // group, and a `]`. Anything longer than that is not one, and the bound is what keeps this from being a scan
    // of the rest of the file in a position that fires for every declaration.
    const LONGEST: usize = 64;
    let kinds = p.peek_token_kind_at(1..LONGEST);
    let mut at = 0usize;

    if kinds.first() != Some(&CppTokenKind::Identifier) {
        return false;
    }
    at += 1;

    if kinds.get(at) == Some(&CppTokenKind::LeftParen) {
        let mut depth = 0i32;
        loop {
            match kinds.get(at) {
                Some(CppTokenKind::LeftParen) => depth += 1,
                Some(CppTokenKind::RightParen) => {
                    depth -= 1;
                    if depth == 0 {
                        at += 1;
                        break;
                    }
                }
                // A bracket inside the arguments ends the reading: it is either a nested attribute (not this
                // spelling) or an index, and neither is a shape this reader claims.
                Some(CppTokenKind::LeftBracket) | Some(CppTokenKind::RightBracket) | None => {
                    return false;
                }
                Some(_) => {}
            }
            at += 1;
        }
    }

    kinds.get(at) == Some(&CppTokenKind::RightBracket)
}

/// Read the single-bracket spelling, into the same [`CppSyntaxKind::AttributeList`] node as the other three.
///
/// The contents are kept as **tokens inside the node**, not parsed: what is written between the brackets is the
/// annotation's own language (`source_annotation_attribute(SA(Method))` names a macro that expands to an
/// identifier), and nothing in this layer may interpret it. That is the same rule the `__attribute__` spelling
/// follows, and it is why a consumer asking "which attributes does this declaration carry" reads the node's text.
pub(crate) fn parse_single_bracket_attribute(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::AttributeList);

    p.bump(); // `[`
    let mut depth = 1usize;

    while !p.is_eof() {
        match p.current_token() {
            CppTokenKind::LeftBracket => {
                p.bump();
                depth += 1;
            }
            CppTokenKind::RightBracket => {
                p.bump();
                depth -= 1;
                if depth == 0 {
                    return Ok(m.complete(p));
                }
            }
            _ => p.bump(),
        }
    }

    p.close_marks_above(base);
    Err(CppParseError::syntax_error_from(
        &t!("unterminated attribute"),
        p.current_token_range(),
    ))
}

/// Parse an attribute specifier, in any of the three spellings, into one [`CppSyntaxKind::AttributeList`].
///
/// One node kind for all three, because a consumer asking "what attributes does this declaration carry" must not
/// have to know which compiler's spelling the file used — and because the *positions* are the same, so a consumer
/// that finds one in a place where only `[[…]]` was expected is looking at a file written for another compiler
/// rather than at a different construct.
pub fn parse_attribute_specifier(p: &mut CppParser) -> ParseResult {
    if p.current_token() == CppTokenKind::Identifier {
        return parse_word_attribute(p);
    }

    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::AttributeList);

    expect_token(p, CppTokenKind::LeftBracket)?;
    expect_token(p, CppTokenKind::LeftBracket)?;

    // `depth` counts the `]` still owed for the two `[` consumed above, so it starts at two and each
    // unmatched `[` inside the attribute list adds one. It must not start lower: one bracket short makes the
    // rule return early, leaving the last `]` for the enclosing rule — which is how a well-formed
    // `[[nodiscard]]` ends up reporting an error on the token *after* it rather than on itself.
    //
    // The `?` on the two `expect_token` calls matters for the same reason it does everywhere else: without
    // it, a caller that is not looking at an attribute list walks the loop below and swallows the rest of
    // the file into it.
    let mut depth = 2usize;
    while !p.is_eof() {
        match p.current_token() {
            CppTokenKind::LeftBracket => {
                p.bump();
                depth += 1;
            }
            CppTokenKind::RightBracket => {
                p.bump();
                depth -= 1;
                if depth == 0 {
                    return Ok(m.complete(p));
                }
            }
            _ => p.bump(),
        }
    }

    p.close_marks_above(base);
    Err(CppParseError::syntax_error_from(
        &t!("unterminated attribute specifier"),
        p.current_token_range(),
    ))
}

/// Read `__attribute__ ((…))` or `__declspec(…)`, into the same node the standard spelling produces.
///
/// The parentheses are a **balanced token group**, not a grammar: `__attribute__ ((noreturn))`,
/// `__attribute__ ((__mode__ (TI)))`, `__attribute__ ((__format__ (gnu_printf, 1, 2)))` — the contents are the
/// compiler's business, and nothing here may interpret them. Note the double parentheses of the GNU spelling:
/// they are not a special case for this reader, because balancing counts them like any other nesting, and the
/// *outer* group is the one that ends the attribute.
pub(crate) fn parse_word_attribute(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::AttributeList);

    // The name is read as a `NameExpr`, like any other name in the tree, so that the spelling stays visible and
    // a consumer can tell `__declspec` from `__attribute__` without counting parentheses.
    let name = p.mark(CppSyntaxKind::NameExpr);
    p.bump();
    name.complete(p);

    if let Err(err) =
        super::super::decls::parse_balanced_token_group(p, CppSyntaxKind::ArgumentList)
    {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// Read the implementation keyword at the cursor, as the node [`an_implementation_keyword`] says it is.
///
/// Three outcomes and each is a decision rather than a branch:
///
/// * a **qualifier** gets [`CppSyntaxKind::RestrictQual`], the node the kind table already names for it and
///   which nothing produced until now;
/// * a keyword that **means a specifier** gets that specifier's node, so `__forceinline` is an `InlineSpec` and
///   a consumer asking "is this function inline" does not have to know which compiler's spelling was used;
/// * a **calling convention** stays a bare token in the sequence it was written in. It is not nothing — it is in
///   the tree, and a consumer can see and print it — but this layer reads no meaning out of it, and inventing a
///   node for a meaning nobody reads is how a tree ends up claiming more than it knows.
pub(crate) fn parse_an_implementation_keyword(p: &mut CppParser) -> CompleteMarker {
    match an_implementation_keyword(p) {
        Some(ImplementationKeyword::Qualifier) => {
            let m = p.mark(CppSyntaxKind::RestrictQual);
            p.bump();
            m.complete(p)
        }
        Some(ImplementationKeyword::Means(kind)) => {
            let m = p.mark(kind);
            p.bump();
            m.complete(p)
        }
        _ => {
            // **A qualifier written with arguments keeps them as a token group.** `__launch_bounds__(1024, 2)` is a
            // specifier followed by a parenthesised list whose contents are the compiler's business (occupancy
            // hints), so the group is consumed whole rather than read as expressions — the same treatment an `asm`
            // payload gets, and for the same reason: there is no meaning here to claim. Without it the `(` was left
            // to the declarator, and a kernel written with `__launch_bounds__` lost the body that followed.
            let spelling = p.current_token_text().to_string();
            p.bump();
            if ((p.family().is_cuda()
                && matches!(
                    spelling.as_str(),
                    "__launch_bounds__" | "__maxnreg__" | "__align__" | "__cluster_dims__"
                ))
                || (p.family().is_ascend_c()
                    && matches!(
                        spelling.as_str(),
                        "__mix__" | "__launch_bounds__" | "__maxnreg__" | "__schedmode__"
                    ))
                // **`__based(p)` names the storage a pointer is based on**, and its argument is the compiler's
                // pointer language (`void`, a name, an expression) — kept as a token group for the same reason.
                || spelling == "__based")
                && p.current_token() == CppTokenKind::LeftParen
            {
                let _ =
                    super::super::decls::parse_balanced_token_group(p, CppSyntaxKind::ArgumentList);
            }
            CompleteMarker::empty()
        }
    }
}
/// **The token after the directive that starts at `index`** — the lookahead half of a directive that stands *inside* a
/// declaration.
///
/// A directive is not a token of the declaration it sits in: the text on either side of it belongs to one
/// declaration, and a question asked about the follower of a name has to see through it. The end is the directive's
/// **logical line** (`stats::logical_line_end`), which is also where the directive's own rule stops, so a
/// `\`-spliced directive is skipped whole.
pub(crate) fn after_the_directive(p: &CppParser, index: usize) -> usize {
    let Some(range) = p.token_range_at(index) else {
        return index + 1;
    };
    let end = super::super::stats::logical_line_end(p.origin_text(), range.start_offset);
    let mut next = index + 1;
    while next < p.token_count() {
        let Some(token) = p.token_range_at(next) else {
            break;
        };
        if token.start_offset >= end {
            break;
        }
        next += 1;
    }
    next
}
