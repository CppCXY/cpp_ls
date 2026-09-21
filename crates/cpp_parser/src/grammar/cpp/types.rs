//! Types and declarators.
//!
//! C++ splits "the type" from "the thing being declared" in a way no other common language does:
//! `int *a[10]` declares an array of pointers, `int (*a)[10]` a pointer to an array, and the
//! difference lives entirely in how the parentheses nest around the declarator. So the declarator
//! cannot be parsed as a suffix of the type; it has to be parsed on its own and nested as it is
//! read.
//!
//! Everything in this module is *syntactic*. Name lookup is deliberately absent: without a symbol
//! table the parser cannot know whether `T` is a type or a variable, so the grammar accepts the
//! superset and leaves the distinction to the caller. That is why `parse_type_id` stops at the
//! first token it cannot classify rather than reporting an error — stopping early is what lets a
//! caller turn "that was not a type after all" into a backtrack.

use crate::{
    grammar::ParseResult,
    kind::{CppSyntaxKind, CppTokenKind},
    parser::{CompleteMarker, CppParser, MarkerEventContainer},
    parser_error::CppParseError,
};

use super::expect_token;

/// Keywords that begin a type specifier.
///
/// Includes the C++20 module keywords (`module`, `import`) because they are *contextual*: the lexer
/// hands them over as identifiers, and they only matter in `parse_decl_specifier_seq` when the
/// enclosing declaration says so.
pub fn is_type_specifier_keyword(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::VoidKeyword
            | CppTokenKind::BoolLiteral
            | CppTokenKind::CharKeyword
            | CppTokenKind::ShortKeyword
            | CppTokenKind::IntKeyword
            | CppTokenKind::LongKeyword
            | CppTokenKind::FloatKeyword
            | CppTokenKind::DoubleKeyword
            | CppTokenKind::SignedKeyword
            | CppTokenKind::UnsignedKeyword
            | CppTokenKind::AutoKeyword
            | CppTokenKind::DecltypeKeyword
            | CppTokenKind::ClassKeyword
            | CppTokenKind::StructKeyword
            | CppTokenKind::UnionKeyword
            | CppTokenKind::EnumKeyword
            | CppTokenKind::TypenameKeyword
            | CppTokenKind::NullptrKeyword
    )
}

/// Keywords that are decl-specifiers but not type specifiers.
pub fn is_decl_specifier_keyword(kind: CppTokenKind) -> bool {
    is_type_specifier_keyword(kind)
        || matches!(
            kind,
            CppTokenKind::ConstKeyword
                | CppTokenKind::VolatileKeyword
                | CppTokenKind::ConstexprKeyword
                | CppTokenKind::ConstevalKeyword
                | CppTokenKind::ConstinitKeyword
                | CppTokenKind::StaticKeyword
                | CppTokenKind::ExternKeyword
                | CppTokenKind::ThreadLocalKeyword
                | CppTokenKind::MutableKeyword
                | CppTokenKind::InlineKeyword
                | CppTokenKind::VirtualKeyword
                | CppTokenKind::ExplicitKeyword
                | CppTokenKind::FriendKeyword
                | CppTokenKind::TypedefKeyword
                | CppTokenKind::RegisterKeyword
                | CppTokenKind::NoexceptKeyword
        )
}

/// Does this token *end* a decl-specifier-seq / type-id beyond doubt?
///
/// Used to decide whether an unclassifiable token can still be part of a type. `[` and `(` can
/// (array and function declarators), so they are deliberately not in this set.
pub fn definitely_ends_a_type(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Semicolon
            | CppTokenKind::LeftBrace
            | CppTokenKind::RightBrace
            | CppTokenKind::RightParen
            | CppTokenKind::RightBracket
            | CppTokenKind::Comma
            | CppTokenKind::Colon
            | CppTokenKind::Assign
            | CppTokenKind::Eof
            | CppTokenKind::None
    )
}

/// Parse a `type-id`: a type specifier sequence plus an abstract declarator.
///
/// This is the entry point used by casts, `sizeof`, `new`, template arguments and parameters. It
/// stops as soon as the next token cannot continue a type, so the caller can decide what the
/// remainder means.
pub fn parse_type_id(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TypeId);

    if let Err(err) = parse_decl_specifier_seq_stopping_at_one_name(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    // An abstract declarator: pointers, references and cv-qualifiers, but no name.
    if let Err(err) = parse_abstract_declarator(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// Parse a `decl-specifier-seq`: the run of type specifiers, cv-qualifiers and other specifiers at
/// the front of a declaration.
///
/// This is a *flat* node on purpose. C++ allows the specifiers in almost any order and repeats
/// them freely (`long long unsigned int`), so anything more structured would have to invent an
/// order the language does not have. Consumers that care about, say, the base type can look at the
/// type-specifier tokens specifically.
pub fn parse_decl_specifier_seq(p: &mut CppParser) -> ParseResult {
    parse_decl_specifier_seq_with(p, true)
}

/// Parse a decl-specifier-seq that must not run past the type, for contexts where a following name
/// belongs to something else.
///
/// `template <typename T, typename U>` is the motivating case: both `T` and `U` are names, and only
/// the comma separates them. A specifier sequence allowed to greedily take a second name would run
/// the two parameters together.
fn parse_decl_specifier_seq_stopping_at_one_name(p: &mut CppParser) -> ParseResult {
    parse_decl_specifier_seq_with(p, false)
}

/// `allow_second_name` decides whether another identifier may join the specifier sequence.
///
/// C++ declares `T x` with two adjacent names, so a specifier loop that stopped at the first name
/// would never reach the declarator. But that same tolerance is wrong for a type-id — a template
/// argument or the target of a cast — where the sequence must end at the type. Both readings are
/// grammatical; only the caller knows which applies.
fn parse_decl_specifier_seq_with(
    p: &mut CppParser,
    allow_second_name: bool,
) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::DeclSpecifierSeq);

    let mut specifiers = 0usize;
    loop {
        if let Err(err) = parse_one_decl_specifier(p, allow_second_name) {
            if specifiers == 0 {
                p.close_marks_above(base);
                return Err(err);
            }
            break;
        }
        specifiers += 1;
    }

    if specifiers == 0 {
        p.close_marks_above(base);
        return Err(CppParseError::syntax_error_from(
            "expected a type specifier",
            p.current_token_range(),
        ));
    }

    Ok(m.complete(p))
}

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
fn parse_one_decl_specifier(p: &mut CppParser, allow_second_name: bool) -> ParseResult {
    let base = p.open_marks();

    match p.current_token() {
        // Attributes may be interleaved anywhere a specifier may appear.
        CppTokenKind::LeftBracket if p.peek_next_token() == CppTokenKind::LeftBracket => {
            return parse_attribute_specifier(p);
        }

        // `enum class E`, `enum struct E` — the scoped-enum form. Checked before the general
        // class-like case because the second keyword is part of *this* specifier, not a new one.
        CppTokenKind::EnumKeyword if is_enum_class_head(p) => {
            let m = p.mark(CppSyntaxKind::BuiltinType);
            p.bump();
            p.bump();
            if p.current_token() == CppTokenKind::Identifier {
                if let Err(err) = parse_name(p) {
                    p.close_marks_above(base);
                    return Err(err);
                }
            }
            return Ok(m.complete(p));
        }

        // `class Foo : public Bar { ... };` — the whole head, because the base clause and the body
        // cannot be parsed by a specifier loop that does not know where the declarator would be.
        kind if is_class_like_keyword(kind) => return parse_class_like_head(p),

        // `decltype(expr)`, `noexcept(expr)` — a keyword with its own parenthesized payload.
        CppTokenKind::DecltypeKeyword | CppTokenKind::NoexceptKeyword => {
            let m = p.mark(CppSyntaxKind::BuiltinType);
            p.bump();
            if p.current_token() == CppTokenKind::LeftParen {
                expect_token(p, CppTokenKind::LeftParen)?;
                super::exprs::parse_expr(p)?;
                expect_token(p, CppTokenKind::RightParen)?;
            }
            return Ok(m.complete(p));
        }

        // `typename T::type` — the disambiguator for dependent names.
        CppTokenKind::TypenameKeyword => {
            let m = p.mark(CppSyntaxKind::TypenameType);
            p.bump();
            if !definitely_ends_a_type(p.current_token()) {
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

        CppTokenKind::ConstKeyword | CppTokenKind::VolatileKeyword => {
            let m = p.mark(CppSyntaxKind::ConstQual);
            p.bump();
            return Ok(m.complete(p));
        }

        // A qualified or unqualified name, possibly a template-id. This is the case that needs a
        // symbol table to be certain about; the grammar accepts it and lets the caller decide.
        CppTokenKind::Identifier | CppTokenKind::Scope => {
            // A name specifier ends the type when what follows cannot continue it. The important
            // case is a second name after a complete one: in `T U`, `U` cannot be part of the same
            // specifier, it names the entity. In `std::vector<int>` the segments are separated by
            // `::`, which *is* part of the same specifier.
            //
            // Without this distinction the specifier loop keeps going, because `T` and `U` are both
            // perfectly good type names and the two forms are indistinguishable from the tokens
            // alone.
            if specifier_already_present(p)
                && !(allow_second_name && continues_a_qualified_name(p))
            {
                return Err(CppParseError::syntax_error_from(
                    "expected a declarator name",
                    p.current_token_range(),
                ));
            }

            let m = p.mark(CppSyntaxKind::TemplateType);
            if let Err(err) = parse_name(p) {
                p.close_marks_above(base);
                return Err(err);
            }
            return Ok(m.complete(p));
        }

        _ => {}
    }

    Err(CppParseError::syntax_error_from(
        "expected a type specifier",
        p.current_token_range(),
    ))
}

/// Does the name at the cursor continue a qualified name, i.e. is it followed by `::`?
///
/// `std::vector<int>` reaches `parse_decl_specifier_seq` in pieces, because
/// [`parse_name`](parse_name) stops after each segment: the specifier loop is what walks the `::`.
/// Without this, the "two adjacent names" guard would fire in the middle of a perfectly ordinary
/// qualified type.
fn continues_a_qualified_name(p: &CppParser) -> bool {
    // Scan forward over a name and its template arguments, then check for `::`.
    let mut depth = 0isize;
    let mut result = false;

    for kind in p.peek_token_kind_at(1..96) {
        match kind {
            CppTokenKind::Less => depth += 1,
            CppTokenKind::Greater => depth -= 1,
            CppTokenKind::RightShift => depth -= 2,
            CppTokenKind::Scope if depth <= 0 => {
                result = true;
                break;
            }
            // Anything that ends a name without a following `::`.
            CppTokenKind::Comma
            | CppTokenKind::Semicolon
            | CppTokenKind::LeftBrace
            | CppTokenKind::RightBrace
            | CppTokenKind::LeftParen
            | CppTokenKind::Assign
            | CppTokenKind::Eof
            | CppTokenKind::None => break,
            _ => {}
        }
    }


    result
}

/// Is the cursor on the second name of a declaration, i.e. does a type specifier sit immediately
/// before it?
///
/// `const Foo x` and `Foo x` both have a specifier before the declarator name; `Foo` on its own does
/// not. Distinguishing them from the token stream is what keeps `Foo x;` from being read as a type
/// named `Foo x`.
fn specifier_already_present(p: &CppParser) -> bool {
    // Walk back over trivia to the previous significant token.
    let mut index = p.current_token_index();
    while index > 0 {
        index -= 1;
        let kind = p.token_kind_at(index);
        if matches!(
            kind,
            CppTokenKind::Whitespace
                | CppTokenKind::Newline
                | CppTokenKind::LineContinuation
                | CppTokenKind::LineComment
                | CppTokenKind::BlockComment
        ) {
            continue;
        }

        return matches!(
            kind,
            CppTokenKind::Identifier
                | CppTokenKind::Greater
                | CppTokenKind::RightShift
                | CppTokenKind::ConstKeyword
                | CppTokenKind::VolatileKeyword
                | CppTokenKind::AutoKeyword
                | CppTokenKind::DecltypeKeyword
                | CppTokenKind::TypenameKeyword
                | CppTokenKind::VoidKeyword
                | CppTokenKind::BoolLiteral
                | CppTokenKind::CharKeyword
                | CppTokenKind::ShortKeyword
                | CppTokenKind::IntKeyword
                | CppTokenKind::LongKeyword
                | CppTokenKind::FloatKeyword
                | CppTokenKind::DoubleKeyword
                | CppTokenKind::SignedKeyword
                | CppTokenKind::UnsignedKeyword
        );
    }

    false
}

/// Is the cursor on `enum` followed by `class` or `struct`?
fn is_enum_class_head(p: &CppParser) -> bool {
    matches!(
        p.peek_token_kind_at(1..2).first(),
        Some(CppTokenKind::ClassKeyword) | Some(CppTokenKind::StructKeyword)
    )
}

/// Parse a class, struct, union or enum head: keyword, optional name, optional base clause, optional
/// body.
///
/// This is a *single decl-specifier* rather than a whole declaration, because `class Foo { ... } x;`
/// is legal (if unusual): the head is a type specifier and `x` is the declarator. Keeping the body
/// inside the specifier and letting the declarator follow covers both spellings.
fn parse_class_like_head(p: &mut CppParser) -> ParseResult {
    let base_marks = p.open_marks();
    let keyword = p.current_token();
    let m = p.mark(class_like_kind(keyword).unwrap_or(CppSyntaxKind::ClassDef));

    p.bump(); // `class` / `struct` / `union` / `enum`

    // An optional name. `enum class` is handled before this is reached.
    if matches!(
        p.current_token(),
        CppTokenKind::Identifier | CppTokenKind::Scope
    ) {
        if let Err(err) = parse_name(p) {
            p.close_marks_above(base_marks);
            return Err(err);
        }
    }

    // An enum's underlying type: `enum E : unsigned char { ... }`.
    if keyword == CppTokenKind::EnumKeyword && p.current_token() == CppTokenKind::Colon {
        p.bump();
        if let Err(err) = parse_type_id(p) {
            p.close_marks_above(base_marks);
            return Err(err);
        }
    }

    // A base clause: `class D : public B, virtual C`. Only for classes, and only if a `{` really
    // follows — otherwise this `:` is something else entirely (a bit-field, a label).
    if keyword != CppTokenKind::EnumKeyword
        && p.current_token() == CppTokenKind::Colon
        && a_brace_follows_the_base_clause(p)
    {
        if let Err(err) = parse_base_clause(p) {
            p.close_marks_above(base_marks);
            return Err(err);
        }
    }

    // The body.
    if p.current_token() == CppTokenKind::LeftBrace {
        if keyword == CppTokenKind::EnumKeyword {
            if let Err(err) = parse_enumerator_body(p) {
                p.close_marks_above(base_marks);
                return Err(err);
            }
        } else if let Err(err) = super::decls::parse_class_body(p) {
            p.close_marks_above(base_marks);
            return Err(err);
        }
        // A class definition ends with `;`. It is consumed by the declaration, because
        // `class Foo {} x;` has a declarator after the body.
    }

    Ok(m.complete(p))
}

/// Does a `{` appear before the next `;` or `}`?
///
/// Used to tell a base clause (`class D : public B {`) from a bit-field (`int x : 3;`) without
/// lookahead support in the grammar itself.
fn a_brace_follows_the_base_clause(p: &CppParser) -> bool {
    let mut depth = 0isize;
    for kind in p.peek_token_kind_at(0..64) {
        match kind {
            CppTokenKind::Less => depth += 1,
            CppTokenKind::Greater => depth -= 1,
            CppTokenKind::LeftBrace if depth <= 0 => return true,
            CppTokenKind::Semicolon | CppTokenKind::RightBrace | CppTokenKind::Eof => return false,
            _ => {}
        }
    }
    false
}

/// Parse a base clause: `: public B, virtual private C<T>`.
fn parse_base_clause(p: &mut CppParser) -> ParseResult {
    let base_marks = p.open_marks();

    expect_token(p, CppTokenKind::Colon)?;

    loop {
        let m = p.mark(CppSyntaxKind::BaseSpecifier);

        // Access specifier and `virtual` may appear in either order.
        loop {
            match p.current_token() {
                CppTokenKind::PublicKeyword
                | CppTokenKind::PrivateKeyword
                | CppTokenKind::ProtectedKeyword
                | CppTokenKind::VirtualKeyword => p.bump(),
                _ => break,
            }
        }

        if let Err(err) = parse_name(p) {
            p.close_marks_above(base_marks);
            return Err(err);
        }

        // A pack expansion: `class D : Bases...`.
        if p.current_token() == CppTokenKind::Ellipsis {
            p.bump();
        }

        m.complete(p);

        if p.current_token() == CppTokenKind::Comma {
            p.bump();
            continue;
        }
        break;
    }

    Ok(CompleteMarker::empty())
}

/// Parse an enumerator list: `{ Red, Green = 2, Blue }`.
fn parse_enumerator_body(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::CompoundStat);

    expect_token(p, CppTokenKind::LeftBrace)?;

    while p.current_token() != CppTokenKind::RightBrace && !p.is_eof() {
        if p.current_token() != CppTokenKind::Identifier {
            break;
        }

        let enumerator = p.mark(CppSyntaxKind::EnumeratorDecl);
        p.bump();
        if p.current_token() == CppTokenKind::Assign {
            p.bump();
            if let Err(err) = super::exprs::parse_expr(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }
        enumerator.complete(p);

        if p.current_token() == CppTokenKind::Comma {
            p.bump();
        } else {
            break;
        }
    }

    if p.current_token() == CppTokenKind::RightBrace {
        p.bump();
    } else {
        p.emit_missing_node();
    }

    Ok(m.complete(p))
}

/// Parse a name: `foo`, `ns::foo`, `::foo`, `Foo<int>`, `ns::Foo<int>::type`.
///
/// Template arguments are attempted speculatively, because `<` is also the less-than operator and
/// only the matching `>` (accounting for nesting) distinguishes them.
pub fn parse_name(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::NameExpr);

    // A leading `::` makes the name fully qualified.
    if p.current_token() == CppTokenKind::Scope {
        p.bump();
    }

    loop {
        match p.current_token() {
            CppTokenKind::Identifier => p.bump(),
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
                    "expected a name",
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
        if p.current_token() == CppTokenKind::Less && a_matching_angle_bracket_follows(p) {
            if let Err(err) = parse_template_argument_list(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }
        if p.current_token() == CppTokenKind::Scope {
            p.bump();
            continue;
        }

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
fn a_matching_angle_bracket_follows(p: &CppParser) -> bool {
    // Relative offsets: `0` is the `<` at the cursor, so the scan starts at `1`.
    let mut depth = 1isize;

    let result = 'scan: {
        for kind in p.peek_token_kind_at(1..128) {
            match kind {
                CppTokenKind::Less => depth += 1,
                CppTokenKind::Greater => {
                    depth -= 1;
                    if depth == 0 {
                        break 'scan true;
                    }
                }
                CppTokenKind::RightShift => {
                    // `>>` closes two levels at once.
                    depth -= 2;
                    if depth <= 0 {
                        break 'scan true;
                    }
                }
                // Anything that cannot appear between `<` and its `>`, including the structural
                // boundaries of the enclosing declaration.
                CppTokenKind::Semicolon
                | CppTokenKind::LeftBrace
                | CppTokenKind::RightBrace
                | CppTokenKind::Eof
                | CppTokenKind::None
                | CppTokenKind::LineComment
                | CppTokenKind::BlockComment => break 'scan false,
                _ => {}
            }
        }
        false
    };

    result
}

/// Parse an operator name after the `operator` keyword.
fn parse_operator_name(p: &mut CppParser) -> ParseResult {
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
        // `operator""_suffix` — a user-defined literal operator. The literal is already one token.
        CppTokenKind::StringLiteral | CppTokenKind::UserDefinedLiteral => p.bump(),
        // Any other overloadable operator is a single token the lexer already produced. `>=` and
        // `>>=` etc. are fine as-is.
        kind if is_overloadable_operator(kind) => p.bump(),
        _ => {
            let err = CppParseError::syntax_error_from(
                "expected an operator name after `operator`",
                p.current_token_range(),
            );
            p.close_marks_above(base);
            return Err(err);
        }
    }

    Ok(m.complete(p))
}

/// Tokens that can be an overloaded operator name.
fn is_overloadable_operator(kind: CppTokenKind) -> bool {
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
    )
}

/// Parse the part of a declarator that has no name: pointers, references and cv-qualifiers.
///
/// e.g.: `*`, `* const`, `&`, `&&`, `* const*`
fn parse_abstract_declarator(p: &mut CppParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::Declarator);

    // A pointer or reference operator, then any cv-qualifiers belonging to *that* operator.
    // `int * const p` is a const pointer; `const int * p` is a pointer to const. Reading the
    // qualifiers here, before recursing, is what keeps the two apart.
    loop {
        match p.current_token() {
            CppTokenKind::Star => {
                let op = p.mark(CppSyntaxKind::PointerType);
                p.bump();
                eat_cv_qualifiers(p);
                op.complete(p);
            }
            CppTokenKind::Ampersand => {
                let op = p.mark(CppSyntaxKind::ReferenceType);
                p.bump();
                eat_cv_qualifiers(p);
                op.complete(p);
            }
            CppTokenKind::LogicalAnd => {
                let op = p.mark(CppSyntaxKind::RValueReferenceType);
                p.bump();
                eat_cv_qualifiers(p);
                op.complete(p);
            }
            // `Class::*` — a pointer to member.
            CppTokenKind::Scope
                if matches!(
                    p.peek_next_token(),
                    CppTokenKind::Star | CppTokenKind::Identifier
                ) =>
            {
                let checkpoint = p.checkpoint();
                let op = p.mark(CppSyntaxKind::PointerType);
                let mut is_member_pointer = false;
                if expect_token(p, CppTokenKind::Scope).is_ok() {
                    // `Ident :: *` is a member pointer; a bare `::*` cannot occur, so a failure
                    // here means this `::` belonged to a qualified name instead.
                    if p.current_token() == CppTokenKind::Identifier
                        && p.peek_next_token() == CppTokenKind::Scope
                    {
                        p.bump(); // the class name
                        p.bump(); // `::`
                    }
                    if p.current_token() == CppTokenKind::Star {
                        p.bump();
                        eat_cv_qualifiers(p);
                        is_member_pointer = true;
                    }
                }

                if is_member_pointer {
                    op.complete(p);
                } else {
                    p.rollback(checkpoint);
                    break;
                }
            }
            _ => break,
        }
    }

    Ok(m.complete(p))
}

/// Consume any `const` / `volatile` immediately following a pointer or reference operator.
fn eat_cv_qualifiers(p: &mut CppParser) {
    while matches!(
        p.current_token(),
        CppTokenKind::ConstKeyword | CppTokenKind::VolatileKeyword
    ) {
        p.bump();
    }
}

/// Parse a declarator: an optional abstract-declarator part plus the name it declares.
///
/// The name is optional: `void f(int)` declares a parameter with no name, and an abstract
/// declarator may have no name at all.
pub fn parse_declarator(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::Declarator);

    if let Err(err) = parse_abstract_declarator(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    // The name, if there is one.
    if matches!(
        p.current_token(),
        CppTokenKind::Identifier
            | CppTokenKind::Scope
            | CppTokenKind::OperatorKeyword
            | CppTokenKind::Tilde
    ) {
        if let Err(err) = parse_name(p) {
            p.close_marks_above(base);
            return Err(err);
        }
    }

    // Suffixes: function parameter lists and array bounds. These bind tighter than pointers, which
    // is why they attach here rather than being folded into the type.
    loop {
        match p.current_token() {
            CppTokenKind::LeftParen => {
                if let Err(err) = super::decls::parse_parameter_list(p) {
                    p.close_marks_above(base);
                    return Err(err);
                }
                eat_function_qualifiers(p);
            }
            CppTokenKind::LeftBracket => {
                let array = p.mark(CppSyntaxKind::ArrayType);
                p.bump();
                // The bound is optional: `int a[]`.
                if p.current_token() != CppTokenKind::RightBracket && !p.is_eof() {
                    if let Err(err) = super::exprs::parse_expr(p) {
                        let _ = array.undo(p);
                        p.close_marks_above(base);
                        return Err(err);
                    }
                }
                if let Err(err) = expect_token(p, CppTokenKind::RightBracket) {
                    let _ = array.undo(p);
                    p.close_marks_above(base);
                    return Err(err);
                }
                array.complete(p);
            }
            _ => break,
        }
    }

    Ok(m.complete(p))
}

/// Parse a template argument list: `<T, int N, ...>`.
///
/// The closing `>` is the hard part. `std::vector<std::vector<int>>` ends in `>>`, which the lexer
/// has already produced as a single `RightShift` token, so the *last* `>` of a nested template list
/// has to be split back out here. Doing it in the parser rather than the lexer is deliberate: in
/// `a >> b` the same token really is a shift, and only the parser knows which context it is in.
pub fn parse_template_argument_list(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TemplateArgumentList);

    // The depth measured at the `<` that opens this list. Used to tell "the `>` that closes me" from
    // "a `>` that closes an enclosing list", which matters as soon as argument parsing has split a
    // `>>` into two `>`s.
    let open_depth = angle_depth_at_cursor(p);

    expect_token(p, CppTokenKind::Less)?;

    while !p.is_eof() {
        // A `>` at our depth closes this list. `split_closing_angle` has already turned any `>>` into
        // a lone `>` by the time we look, so this is the only place the closer is consumed.
        if p.current_token() == CppTokenKind::Greater && angle_depth_at_cursor(p) <= open_depth {
            p.bump();
            return Ok(m.complete(p));
        }

        let before = p.current_token_index();
        let argument = p.mark(CppSyntaxKind::TemplateArgument);
        if parse_template_argument(p).is_err() {
            let _ = argument.undo(p);
            p.close_marks_above(base);
            return Err(CppParseError::syntax_error_from(
                "expected a template argument",
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
                "expected a template argument",
                p.current_token_range(),
            ));
        }

        match p.current_token() {
            CppTokenKind::Comma => {
                p.bump();
                // A trailing comma is not valid, but recovering here beats failing.
                continue;
            }
            _ => {
                // A `>>`-family token still needs splitting before it can close this list.
                split_closing_angle(p);
            }
        }
    }

    p.close_marks_above(base);
    Err(CppParseError::syntax_error_from(
        "unterminated template argument list",
        p.current_token_range(),
    ))
}

/// Split a `>>`-family token so its first `>` can close a template argument list.
///
/// Returns whether the cursor is on a `>` that can close a list — either a lone `>`, or one that was
/// just split out of `>>`, `>=` or `>>=`. The remainder is put back into the token stream as its own
/// token, so the next consumer sees the operator it actually is.
///
/// A lone `>` is *not* consumed here: the caller decides whether it closes a list or is a
/// comparison, and reporting `true` without consuming is what lets both callers share this.
fn split_closing_angle(p: &mut CppParser) -> bool {
    match p.current_token() {
        CppTokenKind::Greater => true,
        CppTokenKind::RightShift => {
            p.split_current_token(1, CppTokenKind::Greater, CppTokenKind::Greater);
            true
        }
        CppTokenKind::GreaterEqual => {
            p.split_current_token(1, CppTokenKind::Greater, CppTokenKind::Assign);
            true
        }
        CppTokenKind::RightShiftAssign => {
            p.split_current_token(1, CppTokenKind::Greater, CppTokenKind::GreaterEqual);
            true
        }
        _ => false,
    }
}

/// Is the cursor on a `>`-family token that can close a template argument list?
///
/// The read-only counterpart of [`split_closing_angle`], for callers that need to ask without
/// mutating the token stream.
fn closes_a_template_list(p: &CppParser) -> bool {
    matches!(
        p.current_token(),
        CppTokenKind::Greater
            | CppTokenKind::RightShift
            | CppTokenKind::GreaterEqual
            | CppTokenKind::RightShiftAssign
    )
}

/// Parse one template argument: a type, a template-id, or a constant expression.
///
/// Returns `Ok` with an empty marker when the type reading applied: the argument *was* the type, and
/// the cursor is already on the delimiter that ends it. `Err` means neither reading worked.
fn parse_template_argument(p: &mut CppParser) -> ParseResult {
    let checkpoint = p.checkpoint();
    let before = angle_depth_at_cursor(p);

    // The type reading. Its *result* matters less than where it stopped: a type-id can return an
    // error after consuming the type (a name is not allowed to be followed by another name, for
    // instance), and if the cursor is now on the delimiter that ends this argument, the argument is
    // complete either way.
    let _type_read = parse_type_id(p).is_ok();
    let at_boundary = stops_at_an_argument_boundary(p, before);

    if at_boundary {
        // Whatever was consumed belongs to this argument; keep it.
        return Ok(CompleteMarker::empty());
    }

    // Not at a delimiter: either a complete type that turned out to be an expression, or nothing that
    // parsed at all. Both want the expression reading from a clean slate.
    p.rollback(checkpoint);
    super::exprs::parse_expr(p)
}

/// Is the cursor on a token that ends a template argument belonging to the list open at `depth`?
///
/// The subtlety is that `>` closes the *innermost* list, not necessarily ours: in
/// `Vec<std::vector<int>, 3>` the `>` after `int` belongs to the inner list, so a `parse_type_id`
/// that stops there has stopped too early and would end the argument in the wrong place.
/// Comparing the depth now against the depth before the argument was read tells the two apart.
/// Does a template argument of the list open at `depth` end here?
///
/// The subtlety is that a closing `>` closes the *innermost* list, not necessarily ours: in
/// `Vec<std::vector<int>, 3>` the `>` after `int` belongs to the inner list, so a `parse_type_id`
/// that stops there has stopped too early and would end the argument in the wrong place.
///
/// `before` is the angle depth measured *before* the argument was read, and it is compared against a
/// fresh measurement taken now. It must be a value captured up front rather than recomputed: parsing
/// an inner argument list splits a `>>` into two `>`s, which changes what the same forward scan
/// counts, and recomputing on both sides would silently shift the reference point.
fn stops_at_an_argument_boundary(p: &CppParser, before: isize) -> bool {
    let now = angle_depth_at_cursor(p);
    match p.current_token() {
        CppTokenKind::Comma => true,
        CppTokenKind::Greater => now <= before,
        CppTokenKind::RightShift => now <= before + 1,
        CppTokenKind::GreaterEqual => now <= before + 1,
        _ => false,
    }
}

/// How many template argument lists are open at the cursor?
///
/// Counts `<` and `>` in the token stream up to the cursor. The count is approximate — it treats
/// every `<` as opening a list — but it is only ever used as a *relative* comparison, and both sides
/// are computed the same way, so the approximation cancels out.
fn angle_depth_at_cursor(p: &CppParser) -> isize {
    let index = p.current_token_index();
    let mut depth = 0isize;

    for position in 0..index {
        match p.token_kind_at(position) {
            CppTokenKind::Less => depth += 1,
            CppTokenKind::Greater => depth -= 1,
            CppTokenKind::RightShift => depth -= 2,
            CppTokenKind::GreaterEqual => depth -= 1,
            _ => {}
        }
    }

    depth
}

/// Consume the qualifiers and specifiers that may follow a function declarator's parameter list.
///
/// e.g.: `const`, `volatile`, `noexcept`, `override`, `final`, `&`, `&&`, `-> T`
pub fn eat_function_qualifiers(p: &mut CppParser) {
    loop {
        match p.current_token() {
            CppTokenKind::ConstKeyword
            | CppTokenKind::VolatileKeyword
            | CppTokenKind::Ampersand
            | CppTokenKind::LogicalAnd => p.bump(),
            CppTokenKind::NoexceptKeyword => {
                p.bump();
                if p.current_token() == CppTokenKind::LeftParen {
                    p.bump();
                    let checkpoint = p.checkpoint();
                    if super::exprs::parse_expr(p).is_err() {
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
            CppTokenKind::Arrow => {
                let trailing = p.mark(CppSyntaxKind::TypeId);
                p.bump();
                let parsed = parse_type_id(p);
                if parsed.is_err() {
                    let _ = trailing.undo(p);
                    return;
                }
                trailing.complete(p);
            }
            _ => return,
        }
    }
}

/// Parse an attribute specifier `[[...]]`.
pub fn parse_attribute_specifier(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::AttributeList);

    expect_token(p, CppTokenKind::LeftBracket)?;
    expect_token(p, CppTokenKind::LeftBracket)?;

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
        "unterminated attribute specifier",
        p.current_token_range(),
    ))
}