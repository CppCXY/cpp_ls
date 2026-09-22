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
    parser::{CompleteMarker, CppParser, Marker, MarkerEventContainer},
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
            // The `bool` type, whose token kind is named `BoolLiteral` — see its documentation.
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

/// The syntax kind for a storage-class or function specifier, if this token is one.
///
/// Kept as a lookup rather than an inline `match` so the guard and the node kind cannot drift apart:
/// the specifier loop asks twice — once to decide, once to build the node.
fn storage_or_function_specifier(kind: CppTokenKind) -> Option<CppSyntaxKind> {
    Some(match kind {
        CppTokenKind::StaticKeyword => CppSyntaxKind::StaticSpec,
        CppTokenKind::ExternKeyword => CppSyntaxKind::ExternSpec,
        CppTokenKind::ThreadLocalKeyword => CppSyntaxKind::ThreadLocalSpec,
        CppTokenKind::MutableKeyword => CppSyntaxKind::MutableSpec,
        CppTokenKind::InlineKeyword => CppSyntaxKind::InlineSpec,
        CppTokenKind::VirtualKeyword => CppSyntaxKind::VirtualSpec,
        CppTokenKind::ExplicitKeyword => CppSyntaxKind::ExplicitSpec,
        CppTokenKind::ConstexprKeyword
        | CppTokenKind::ConstevalKeyword
        | CppTokenKind::ConstinitKeyword => CppSyntaxKind::ConstexprSpec,
        _ => return None,
    })
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
    parse_type_id_with(p, true)
}

/// [`parse_type_id`] with the caller's answer to one question: may a bare **name** be a type here?
///
/// The question only arises for a type that begins with `(`, where the tokens inside are a parameter list:
///
/// ```text
/// sizeof(void(int))     the `int` is a type, so the parentheses are a parameter list
/// new (Widget)(1)       `Widget` is not usable as a type here, so `(Widget)` is a placement argument
/// ```
///
/// In a `sizeof`, a cast or a template argument, an identifier in type position *is* a type — nothing else
/// could be there. In a `new`, the parentheses may be a placement list, and the placement reading is the one
/// that can be checked against a smaller set: the name has to be one the file declares to be a type, because a
/// parameter list whose parameter is an unknown name is a guess, while a placement argument is a fact.
pub fn parse_type_id_with(p: &mut CppParser, a_name_may_be_a_type: bool) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TypeId);

    if let Err(err) = parse_type_id_inner(p, a_name_may_be_a_type) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// The body of [`parse_type_id`], so the `TypeId` node is opened once no matter which branch applies.
///
/// # The type that begins with a parenthesis
///
/// A specifier sequence starts every type, and one form starts with something that is not a specifier at all:
/// a **function type spelled with its parameter list directly** — `void (int)`, `int (char, double)`. Read
/// left to right, `(` cannot begin a decl-specifier-seq, so the sequence refuses and the whole type-id fails
/// with `expected a type specifier` — which is how `new (Widget)(1)` came to report an error against a
/// statement that is perfectly ordinary C++.
///
/// It cannot be handled inside the sequence, because a parameter list is not a specifier. So the parenthesis
/// is claimed *before* the sequence runs, and only when the sequence would otherwise have nothing to say —
/// which is what keeps the reading away from a declaration, where a `(` after the specifiers is a declarator's
/// own parenthesis rather than part of the type.
///
/// e.g.: the `(int)` of `void (int)`, as written in `sizeof(void(int))` or `new (Widget)(1)`
fn parse_type_id_inner(p: &mut CppParser, a_name_may_be_a_type: bool) -> ParseResult {
    if p.current_token() == CppTokenKind::LeftParen && starts_a_function_type(p, a_name_may_be_a_type) {
        // An empty specifier sequence, kept so that a consumer asking a type for its specifiers finds an
        // answer rather than having to special-case this spelling. A function type has no leading type, which
        // is exactly what "empty" says.
        let specifiers = p.mark(CppSyntaxKind::DeclSpecifierSeq);
        specifiers.complete(p);

        let function_type = p.mark(CppSyntaxKind::FunctionType);
        super::decls::parse_parameter_list(p)?;
        eat_function_qualifiers(p);
        // A trailing `(int)` — as in `void (*(int))(int)` — is the rest of the type. It is only read when it
        // really is a parameter list, so the `(1)` of `new (Widget)(1)` stops here rather than being taken for
        // one: a parameter list holds types, and `1` is not one.
        if a_parameter_list_follows(p) {
            parse_declarator_suffixes(p)?;
        }
        function_type.complete(p);

        return Ok(CompleteMarker::empty());
    }

    parse_decl_specifier_seq_stopping_at_one_name(p)?;

    // An abstract declarator: pointers, references and cv-qualifiers, but no name.
    //
    // No name may appear — this is a type-id, not a declarator — which is what makes `(int)` after it a
    // function type rather than somebody's parameter list. See [`parse_abstract_declarator`].
    parse_abstract_declarator(p, false)?;

    Ok(CompleteMarker::empty())
}

/// Do the parentheses at the cursor hold a parameter list?
///
/// The companion to [`starts_a_function_type`], asked after a function type has been read to decide whether
/// the `(` at the cursor continues it. The test is the same one: a parameter list begins with a type, so a
/// parenthesis whose first token cannot begin one belongs to something else — `(1)`, `(x + 1)`, `(f())`.
fn a_parameter_list_follows(p: &CppParser) -> bool {
    starts_a_function_type(p, true)
}

/// Do the parentheses at the cursor hold a parameter list, making this a function type?
///
/// Called when a type-id begins with `(`, and it answers the same question [`a_parameter_list_is_the_type`]
/// answers one level down: a parameter list begins with a type. A `*` or `&` inside is *not* this — that is
/// the abstract declarator's own parenthesis, `(*)(int)` — so refusing those here is what sends them down
/// that path instead.
///
/// `a_name_may_be_a_type` is passed through from [`parse_type_id_with`] and only affects the identifier case.
fn starts_a_function_type(p: &CppParser, a_name_may_be_a_type: bool) -> bool {
    if p.current_token() != CppTokenKind::LeftParen {
        return false;
    }

    match p.peek_token_kind_at(1..2).first() {
        Some(&kind) if is_type_specifier_keyword(kind) => true,
        Some(&CppTokenKind::Scope) => true,
        Some(&CppTokenKind::ConstKeyword) | Some(&CppTokenKind::VolatileKeyword) => true,
        Some(&CppTokenKind::Identifier) => {
            // A name is a type when the caller says so, and when it does not, the file's own declarations get
            // the say: `new (Widget)(1)` is an allocation of a type the file knows, while `new (buf) Widget()`
            // is placement into a buffer nobody declared as a type.
            a_name_may_be_a_type || p.is_a_known_type_name(p.peek_token_text_at(1))
        }
        _ => false,
    }
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
///
/// Inside the loop, `has_specifier` and `name_allowed` carry the rest of the decision. It is made
/// from what the loop has *consumed* rather than from the surrounding tokens, because the tokens
/// cannot answer it: "is there a specifier before this name?" is true for both `const Point` (where
/// the name is still part of the type) and `Point p` (where it is the declarator), and guessing
/// wrong there loses the whole declaration — once the loop eats `p` as part of the type there is no
/// declarator left and the `;` never matches.
fn parse_decl_specifier_seq_with(p: &mut CppParser, allow_second_name: bool) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::DeclSpecifierSeq);

    let mut specifiers = 0usize;
    // See `name_joins_the_type`: what the loop has consumed so far decides whether the next name
    // belongs to the type or is the declarator.
    let mut has_specifier = false;
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
    loop {
        let specifier_seen = specifiers > 0;
        if let Err(err) =
            parse_one_decl_specifier(p, &mut has_specifier, &mut name_allowed, specifier_seen)
        {
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
///
/// This is a wrapper around [`parse_one_decl_specifier_inner`] whose only job is the bookkeeping
/// the inner function's dozen early returns would otherwise each have to remember: a specifier was
/// consumed, so [`name_joins_the_type`] must know it.
fn parse_one_decl_specifier(
    p: &mut CppParser,
    has_specifier: &mut bool,
    name_allowed: &mut bool,
    specifier_seen: bool,
) -> ParseResult {
    let result = parse_one_decl_specifier_inner(p, has_specifier, name_allowed, specifier_seen);

    if result.is_ok() {
        *has_specifier = true;
    }

    result
}

fn parse_one_decl_specifier_inner(
    p: &mut CppParser,
    has_specifier: &mut bool,
    name_allowed: &mut bool,
    specifier_seen: bool,
) -> ParseResult {
    let base = p.open_marks();
    // Where this specifier begins, for the backward questions a name specifier has to ask — see
    // [`super::decls::type_name_at`]. `base` above cannot answer them: it is a marker-stack length, not a
    // position in the token stream.
    let type_start = p.anchor();

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
        // marker after it — and that is eaten by [`super::decls::parse_template_parameter`] once the declarator
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

        CppTokenKind::ConstKeyword | CppTokenKind::VolatileKeyword => {
            let m = p.mark(CppSyntaxKind::ConstQual);
            p.bump();
            return Ok(m.complete(p));
        }

        // `friend` is a declaration of its own, not a specifier of one: `friend class X;` declares
        // `X` to be a friend, it does not declare a class. Wrapping the whole thing keeps the
        // declaration node from claiming the friend is a variable of type `void`.
        CppTokenKind::FriendKeyword => {
            let m = p.mark(CppSyntaxKind::FriendDecl);
            p.bump();
            if let Err(err) = super::decls::parse_declaration(p) {
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
            // claimed by [`super::decls::parse_linkage_specification`] before the specifier loop is ever
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
                if let Err(err) = super::exprs::parse_expr(p) {
                    p.close_marks_above(base);
                    return Err(err);
                }
                expect_token(p, CppTokenKind::RightParen)?;
            }

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
        // Conditional on a name having been **written**, not merely on a specifier having been consumed. That
        // distinction is load-bearing and it was worth a bug: `virtual ~Shape();` has a specifier (`virtual`) and
        // no name, so a check for "a specifier was seen" let this arm claim the tilde and the declaration became
        // `virtual ~ Shape()` — an abstract declarator for a function type. Inside a namespace that member then
        // consumed its way past the namespace's closing brace, and the namespace swallowed the rest of the file.
        // `Foo::~Foo` is the only shape that reaches here with a name already in hand.
        CppTokenKind::Tilde if p.declaration_type_name().is_some() => {
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
            if !name_joins_the_type(p, *has_specifier, *name_allowed) {
                return Err(CppParseError::syntax_error_from(
                    "expected a declarator name",
                    p.current_token_range(),
                ));
            }

            // A name that ends here has joined the type; the allowance is spent so the next one is
            // read as the declarator.
            *name_allowed = false;

            // The first name in type position is the type, and it is worth remembering — see the note where the
            // loop begins. The whole name is reported, not the first token of it, because a qualified type has to
            // be recorded under the name a *lookup* would use: `std::string` is the type `string`, qualified.
            p.note_declaration_type_name(super::decls::type_name_at(p, type_start));

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

/// May the name at the cursor still be part of the type being parsed, rather than the declarator?
///
/// This is *the* ambiguity of a declaration, and it cannot be settled by looking at one token:
///
/// ```text
/// T x;                    // type `T`, declarator `x`      -> the name joins the type
/// const Point p;          // type `const Point`, `p`       -> the name joins the type
/// std::vector<int> v;     // type `std::vector<int>`, `v`  -> the name is the declarator
/// void f();               // type `void`, declarator `f`   -> the name is the declarator
/// ```
///
/// Three conditions, and each covers a case the others get wrong:
///
/// * `has_specifier` — false only at the very start, where a lone name is certainly a type (`Foo`
///   in `Foo x`); nothing precedes it for it to be a declarator *of*.
/// * `continues_a_qualified_name` — the loop walks `std::vector` one segment per iteration and every
///   segment is part of the same name.
/// * `name_allowed` — whether a name may still join. `T x` lets `x` in and then spends the
///   allowance; `void f` never spends it, because `void` is a keyword and a keyword can never be
///   the declarator. That is the whole reason `T x;` and `void f();` can both be right.
/// * `type_is_already_complete` — the sequence so far stands on its own as a type, so a following
///   name must be the declarator even though the allowance is unspent. Without this `void f;` reads
///   `f` as a second word of the type.
/// * `a_parenthesis_follows_the_name` — the name about to join is *called*. No type is written that
///   way, so it is the declarator and the parentheses are what initialises it. See
///   [`a_parenthesis_follows_the_name`] for the whole argument; it is what makes `Widget w(1, 2, 3);`
///   a declaration without asking the type table.
fn name_joins_the_type(p: &CppParser, has_specifier: bool, name_allowed: bool) -> bool {
    if !has_specifier || continues_a_qualified_name(p) {
        return true;
    }
    if type_is_already_complete(p) || a_parenthesis_follows_the_name(p) {
        return false;
    }
    name_allowed
}

/// Is the identifier at the cursor immediately followed by a `(`?
///
/// The signal that separates the type from the declarator in the one shape this parser used to need a type
/// table for:
///
/// ```text
/// Widget w(1, 2, 3);   `Widget` is the type, `w` the declarator — the `(` calls `w`
/// unsigned int x;      `x` is not called
/// void f();            `f` is the declarator already, by the keyword rule above
/// ```
///
/// A name followed by `(` in *type* position has no reading: nothing in a decl-specifier-seq is called, and a
/// type's own parentheses — `void (*)(int)` — belong to a declarator that has already been introduced by a
/// `*`. So the `(` can only mean the name is not part of the type, which is the reading the declaration needs
/// and the reason `Widget w(1, 2, 3);` no longer depends on the file having declared `Widget` first.
///
/// # What it is not allowed to break
///
/// The name is only refused when a *type* is already in hand. Asked about the first name of a declaration —
/// `g(1, 2);`, `A(B);` — the answer is no, because refusing there leaves the specifier sequence with nothing
/// and the declaration reading fails before the expression reading can be reached. Those statements have a
/// single name, so the name is taken for the type and the parentheses are left for the caller, which is
/// exactly what happened before this rule existed.
///
/// A name that legitimately joins a type and is *then* followed by a `(` — which is every function declarator
/// there is — never reaches the `(` for a different reason: [`type_is_already_complete`] has already answered
/// before this is asked. `void f(int)`, `Foo::Foo()` and `template <typename T> void g(T)` all stop at the
/// name for the older rule.
///
/// The name itself is exempt too, and that is the guard that keeps a *redeclaration* working:
/// `Widget Widget(1);` — a variable named like its type, or a function whose name is the type's — is left to
/// the type table rather than being split by this rule.
fn a_parenthesis_follows_the_name(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::Identifier {
        return false;
    }

    let Some(type_name) = p.declaration_type_name() else {
        // No type yet: this name can only *be* the type. See the note above.
        return false;
    };

    if p.token_text_at(p.current_token_index()) == type_name {
        return false;
    }

    // Over the name and any template argument list it carries, then look for the `(`. The scan stops at
    // anything that ends a name, so it cannot run into a `(` belonging to a later construct.
    //
    // A closing parenthesis ends the question in the other direction, and it has to be checked *before* the
    // operators below: `(a && b)` is a condition, and the `&&` in it would otherwise read as the reference
    // operator of a declarator. What separates the two is which comes first — `(a && b)` closes before the
    // operator is reached in a way that matters, while `void f(Args&&... a)` has the operator and no `)` in
    // between — so the scan answers on the first of the two it meets.
    //
    // A pointer, reference or pack operator is a stop as well, and it is the subtlest of the stops: in
    // `void g(Args&&... args)` the `(` after the operator belongs to the *declarator*, not to the name, and a
    // scan that ran past the `&&` would refuse `Args` its place in the type and leave the parameter without
    // one. Those operators are part of the type's own syntax, which is exactly what the name is being tested
    // for.
    let mut depth = 0isize;

    for kind in p.peek_token_kind_at(1..64) {
        match kind {
            CppTokenKind::Less => depth += 1,
            CppTokenKind::Greater => depth -= 1,
            CppTokenKind::RightShift => depth -= 2,
            CppTokenKind::LeftParen if depth <= 0 => return true,
            CppTokenKind::RightParen if depth <= 0 => return false,
            // The name ended without a `(`: an operator, a separator, an initialiser, a body, or the
            // `::` of a longer qualified name — which is the case `continues_a_qualified_name` owns.
            CppTokenKind::Identifier
            | CppTokenKind::Comma
            | CppTokenKind::Semicolon
            | CppTokenKind::LeftBrace
            | CppTokenKind::RightBrace
            | CppTokenKind::LeftBracket
            | CppTokenKind::Assign
            | CppTokenKind::Colon
            | CppTokenKind::Scope
            | CppTokenKind::Star
            | CppTokenKind::Ampersand
            | CppTokenKind::LogicalAnd
            | CppTokenKind::Ellipsis
            | CppTokenKind::Eof
            | CppTokenKind::None => return false,
            _ => {}
        }
    }

    false
}

/// Does the specifier just consumed already stand on its own as a complete type?
///
/// Asked by walking back to the previous significant token. A *qualifier* is not a type — `const`
/// alone is not one, so `const Point p` still has room for `Point` to join — while every keyword
/// that names a type, and every template-id's closing `>`, ends one.
///
/// # Why a class keyword is not one either
///
/// `struct`, `class`, `union` and `enum` name a *kind* of type and are not a type by themselves: what they
/// introduce still has to be named or given a body. Treating them as complete is what made
/// `struct Foo f;` declare a variable called `Foo` and then report `expected ';'` against `f` — the
/// elaborated-type-specifier spelling, which is how C code and a great deal of C++ still declares a variable of
/// a struct type. With them excluded, `Foo` joins the type as the elaborated name and `f` is the declarator.
///
/// The keyword is not left unrecognised: the branch above that parses a class-like head takes `struct Foo {`
/// and `struct Foo;` before this is ever asked, and `enum class E` is handled there too. What reaches here is
/// the bare keyword of a declaration whose name and declarator are still to come.
fn type_is_already_complete(p: &CppParser) -> bool {
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

        return !matches!(
            kind,
            CppTokenKind::ConstKeyword
                | CppTokenKind::VolatileKeyword
                | CppTokenKind::ClassKeyword
                | CppTokenKind::StructKeyword
                | CppTokenKind::UnionKeyword
                | CppTokenKind::EnumKeyword
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
    // The name is read before it is parsed, because the parser needs it afterwards and re-deriving it from the
    // event stream would be a second implementation of "what did that name say".
    let declared_name = if p.current_token() == CppTokenKind::Identifier {
        Some(p.current_token_text().to_string())
    } else {
        None
    };

    if matches!(
        p.current_token(),
        CppTokenKind::Identifier | CppTokenKind::Scope
    ) && let Err(err) = parse_name(p)
    {
        p.close_marks_above(base_marks);
        return Err(err);
    }

    // `class Widget { ... }` declares `Widget` to be a type, and the parser needs to know that to read
    // `Widget w(1, 2);` as a declaration rather than as a call.
    //
    // Taken from the **first identifier** of the name, which is the entity being declared: `class ns::Widget`
    // declares `Widget`, and that is what a later declaration spells. A qualified head is therefore recorded
    // under the name it introduces rather than under its qualifier, which is what makes the lookup useful —
    // `Widget w(1, 2);` never mentions `ns`.
    if let Some(name) = declared_name {
        p.declare_type_name(&name);
    }

    // Attributes on the class head: `class C [[deprecated]] { ... }`. They may also be written before
    // the keyword, and that form is consumed as a leading decl-specifier by the caller — so both
    // spellings reach the body, and skipping this one makes the class look as though it ended at the
    // attribute. `enum class E [[deprecated]]` already worked, which is exactly why the gap here was
    // easy to miss.
    while p.current_token() == CppTokenKind::LeftBracket
        && p.peek_next_token() == CppTokenKind::LeftBracket
    {
        if parse_attribute_specifier(p).is_err() {
            break;
        }
    }

    // `class D final : public B` — the virt-specifier of a class head. It is written between the name and the
    // base clause, and it is an *identifier* to the lexer (C++11 made `final` and `override` contextual), so
    // no token kind distinguishes it. Without this the `:` after it is never seen as a base clause — the head
    // has ended, the declaration looks for a `;` and finds `:`, and the class body is then read as a compound
    // statement at file scope.
    //
    // Accepted wherever an identifier with that spelling appears in this position, which is the whole test:
    // a class can also be *named* `final` (`class final { };`), and that spelling is claimed before this by
    // the name rule above.
    if keyword != CppTokenKind::EnumKeyword
        && p.current_token() == CppTokenKind::Identifier
        && matches!(p.current_token_text(), "final" | "override")
    {
        p.bump();
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
        && let Err(err) = parse_base_clause(p)
    {
        p.close_marks_above(base_marks);
        return Err(err);
    }

    // The body.
    if p.current_token() == CppTokenKind::LeftBrace {
        let body = if keyword == CppTokenKind::EnumKeyword {
            parse_enumerator_body(p)
        } else {
            super::decls::parse_class_body(p)
        };
        if let Err(err) = body {
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
    a_body_follows_the_class_head(p)
}

/// Does a class-like head at the cursor actually open a body?
///
/// This is the difference between `class Foo { ... };` and `template <class T> ...`: both start with
/// the `class` keyword, and only the first has a body. It is answered by looking ahead for a `{`
/// that no `;` or `}` intervenes — which is exactly what "the head is followed by a definition"
/// means.
fn a_body_follows_the_class_head(p: &CppParser) -> bool {
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
        while matches!(
            p.current_token(),
            CppTokenKind::PublicKeyword
                | CppTokenKind::PrivateKeyword
                | CppTokenKind::ProtectedKeyword
                | CppTokenKind::VirtualKeyword
        ) {
            p.bump();
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
/// nested template-id in it unparseable, which is exactly the shape `Vec<std::vector<int>, 3>` has.
///
/// # Where it must not be shallow
///
/// The stop set below. The scan starts at a `<` and looks for the matching `>`; if it is willing to
/// run past a `)` or a `;`, it will find a `>` belonging to something else entirely later in the file
/// and report a template-id that is not there. That is not a rare form — `double f(const Point a) {
/// return a; }` contains a `>`-less declaration whose only `<`…`>` pair is nowhere near it, and
/// reading `Point a` as `Point<a>` swallows the parameter name and then the whole declaration.
fn a_matching_angle_bracket_follows(p: &CppParser) -> bool {
    // Relative offsets: `0` is the `<` at the cursor, so the scan starts at `1`.
    let mut depth = 1isize;

    'scan: {
        for kind in p.peek_token_kind_at(1..128) {
            match kind {
                CppTokenKind::Less => depth += 1,
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
                CppTokenKind::Semicolon
                | CppTokenKind::LeftBrace
                | CppTokenKind::RightBrace
                | CppTokenKind::RightParen
                | CppTokenKind::RightBracket
                | CppTokenKind::Colon
                | CppTokenKind::Assign
                | CppTokenKind::Arrow
                | CppTokenKind::Eof
                | CppTokenKind::None
                | CppTokenKind::LineComment
                | CppTokenKind::BlockComment => break 'scan false,
                _ => {}
            }
        }
        false
    }
}

/// Parse an operator name after the `operator` keyword.
/// Parse an operator name after the `operator` keyword.
///
/// Exposed for the expression grammar, which reads the same names in the same position: `Foo::operator+()` is an
/// expression's callee before it is a declaration's name, and a second rule for it would be a second answer to
/// "what is an operator name".
pub fn parse_operator_name_here(p: &mut CppParser) -> ParseResult {
    parse_operator_name(p)
}

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
        // `operator bool`, `operator int`, `operator MyType` — a *conversion* operator, which is spelled with
        // the type it converts to rather than with a symbol. A keyword type is as ordinary here as a class
        // name is, which is why this case has to be in the list: without it `explicit operator bool()` reports
        // the keyword as an operator name it does not recognize.
        kind if is_type_specifier_keyword(kind) || is_class_like_keyword(kind) => {
            p.bump();

            // A composed name: `operator unsigned long`, `operator const char*`. Consumed here because the
            // operator's own tokens are the whole name — a caller looking for it would otherwise have to
            // re-derive where a type ends, and this rule is already at that position.
            while is_type_specifier_keyword(p.current_token())
                || matches!(
                    p.current_token(),
                    CppTokenKind::ConstKeyword | CppTokenKind::VolatileKeyword
                )
            {
                p.bump();
            }
            while p.current_token() == CppTokenKind::Star
                || p.current_token() == CppTokenKind::Ampersand
            {
                p.bump();
            }
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
/// The abstract part of a declarator: pointer and reference operators, each with the cv-qualifiers
/// that belong to *that* operator.
///
/// The node is created lazily, because "no abstract declarator" is the common case rather than an
/// edge case: in `int counter;` there is no pointer, so demanding a node here would put an empty
/// `Declarator` inside every single declarator in the program. `parse_declarator` already opens the
/// `Declarator` node that `int counter;` needs, so an eager marker here produces
/// `Declarator(Declarator(NameExpr))` — a redundant level that also breaks the 1:1 pairing of
/// `NodeStart`/`NodeEnd` events, since the inner one is empty and gets dropped.
///
/// # `name_possible`
///
/// Whether a declarator's *name* may still appear where this is called, which is what decides one reading:
/// `int (int)`. A name may appear in the position a declarator stands in — `void f(int)` — and then those
/// parentheses are a parameter list rather than part of the type, so they are left alone. It may not appear
/// in a type-id, or inside a parenthesised declarator, and then `(int)` *is* a function type.
///
/// The recursive call passes `false`, because an inner declarator's parentheses wrap a declarator and never a
/// parameter list: `(*f)`, `(* const)`, `(**)`.
fn parse_abstract_declarator(p: &mut CppParser, name_possible: bool) -> ParseResult {
    let mut container: Option<Marker> = None;

    /// Open the `Declarator` node on first use, so an abstract declarator that is not there leaves
    /// no node behind.
    macro_rules! container {
        () => {
            *container.get_or_insert_with(|| p.mark(CppSyntaxKind::Declarator))
        };
    }

    // A pointer or reference operator, then any cv-qualifiers belonging to *that* operator.
    // `int * const p` is a const pointer; `const int * p` is a pointer to const. Reading the
    // qualifiers here, before recursing, is what keeps the two apart.
    loop {
        match p.current_token() {
            CppTokenKind::Star => {
                let _ = container!();
                let op = p.mark(CppSyntaxKind::PointerType);
                p.bump();
                eat_cv_qualifiers(p);
                op.complete(p);
            }
            CppTokenKind::Ampersand => {
                let _ = container!();
                let op = p.mark(CppSyntaxKind::ReferenceType);
                p.bump();
                eat_cv_qualifiers(p);
                op.complete(p);
            }
            CppTokenKind::LogicalAnd => {
                let _ = container!();
                let op = p.mark(CppSyntaxKind::RValueReferenceType);
                p.bump();
                eat_cv_qualifiers(p);
                op.complete(p);
            }
            // `Class::*` — a pointer to member.
            //
            // The node is only opened once the `*` has actually been seen: opening it up front and
            // rewinding on failure leaves a registered marker behind, which suppresses the
            // `container!` call on the next iteration and can spin this loop forever on a
            // qualified name.
            CppTokenKind::Scope
                if matches!(
                    p.peek_next_token(),
                    CppTokenKind::Star | CppTokenKind::Identifier
                ) =>
            {
                let checkpoint = p.checkpoint();
                p.bump(); // `::`

                // `Ident :: *` is a member pointer; a bare `::*` cannot occur, so falling through
                // here means this `::` belonged to a qualified name instead.
                if p.current_token() == CppTokenKind::Identifier
                    && p.peek_next_token() == CppTokenKind::Scope
                {
                    p.bump(); // the class name
                    p.bump(); // `::`
                }

                if p.current_token() == CppTokenKind::Star {
                    let _ = container!();
                    let op = p.mark(CppSyntaxKind::PointerType);
                    p.bump();
                    eat_cv_qualifiers(p);
                    op.complete(p);
                } else {
                    p.rollback(checkpoint);
                    break;
                }
            }
            _ => break,
        }
    }

    // A **parenthesised** abstract declarator, or a function type whose parentheses hold the parameter list.
    //
    // The first form is `void (*)(int)`, `int (&)[10]`: the parentheses are what let the `*` bind to the
    // function rather than to its return type, which is the whole reason the syntax exists — `void *f(int)` is
    // a function returning a pointer, `void (*f)(int)` a pointer to a function. A type-id — a `using` alias, a
    // cast, the target of a `sizeof` — has no name to hang the parentheses on, so it reaches this with nothing
    // parsed yet, and without the branch the alias is left with a specifier sequence and three stray tokens.
    //
    // The second is `void (int)`, `int (char, double)` — a function type, spelled with the parameter list
    // directly. It is the same shape as the `(*)(int)` form and reached by *dropping* the pointer, which is
    // how `new (Widget)(1)` writes an allocation whose type is parenthesised. The two are told apart by what
    // the parentheses hold: a type specifier means the list is a parameter list, and anything else means the
    // parentheses wrap a declarator.
    //
    // Whether the parentheses belong here at all is the question {@link
    // a_parenthesised_abstract_declarator_follows} answers, and it is asked *before* the parenthesis is
    // consumed: `int (x)` is a parenthesized declarator for the caller to read, and `void f(int)` has
    // parentheses that are not this rule's business at all.
    // Whether the parentheses belong to this declarator is asked before they are consumed, and the answer
    // depends on something the caller knows and this function does not: whether a **name** may still appear.
    //
    // * `void f(int)` — a name may appear next, so `(int)` is the parameter list of `f`, and it is not this
    //   rule's business at all. Consuming it here would take the whole declaration apart.
    // * `new (int)(1)` — no name may appear, so the `(int)` *is* the type: a function type spelled with its
    //   parameter list directly.
    // * `new (Widget)(1)` — the same, with the parameter list holding a type name.
    //
    // Both readings are the same four tokens, so the decision cannot come from them; it comes from who is
    // asking, which is what `name_possible` carries.
    if p.current_token() == CppTokenKind::LeftParen {
        if a_parenthesised_abstract_declarator_follows(p) {
            let _ = container!();

            expect_token(p, CppTokenKind::LeftParen)?;
            // The inner abstract declarator, recursively: `(* const)`, `(**)`, `(&)`.
            parse_abstract_declarator(p, false)?;
            expect_token(p, CppTokenKind::RightParen)?;

            parse_declarator_suffixes(p)?;
        } else if !name_possible && a_parameter_list_is_the_type(p) {
            let function_type = p.mark(CppSyntaxKind::FunctionType);
            super::decls::parse_parameter_list(p)?;
            eat_function_qualifiers(p);
            // The `noexcept` of `void () noexcept` and a trailing return type both belong to the type.
            parse_declarator_suffixes(p)?;
            function_type.complete(p);
        }
    }

    match container {
        Some(m) => Ok(m.complete(p)),
        // No abstract declarator here at all — the caller's own `Declarator` node covers it.
        None => Ok(CompleteMarker::empty()),
    }
}

/// Do the parentheses at the cursor hold a parameter list, making them a *function type*?
///
/// e.g.: the `(int)` of `new (void(int))()` or of `sizeof(void(int))`
///
/// A parameter list begins with a type, so the test is that the first token inside the parentheses can begin
/// one. That is deliberately weaker than "parses as a parameter list" — the caller parses it and reports
/// whatever goes wrong — and it is deliberately *not* satisfied by a pointer or reference operator, because
/// `(*)(int)` is the parenthesised-declarator form and is handled before this is asked.
///
/// A `)` is excluded as well: `void ()` is a function type with no parameters, and it is the one case where
/// the parentheses hold nothing at all. It is admitted only when the `(` follows a complete type, which is
/// the caller's state rather than this function's, so the trade is made there — see the branch above.
fn a_parameter_list_is_the_type(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::LeftParen {
        return false;
    }

    matches!(
        p.peek_token_kind_at(1..2).first(),
        Some(&kind) if is_type_specifier_keyword(kind)
            || matches!(
                kind,
                CppTokenKind::Identifier
                    | CppTokenKind::Scope
                    | CppTokenKind::ConstKeyword
                    | CppTokenKind::VolatileKeyword
            )
    )
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
/// wrapped in them without a name, since a declarator's parentheses have to *hold* a declarator. `int (int)`
/// reaches this with a type keyword inside, is refused here, and stays a parameter list; `int (x)` reaches it
/// with a name, is refused for the same reason, and stays a name.
fn a_parenthesised_abstract_declarator_follows(p: &CppParser) -> bool {
    matches!(
        p.peek_token_kind_at(1..2).first(),
        Some(&CppTokenKind::Star)
            | Some(&CppTokenKind::Ampersand)
            | Some(&CppTokenKind::LogicalAnd)
            // `(::*)` — a pointer to member, where the operator comes after the class name.
            | Some(&CppTokenKind::Scope)
    )
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
fn a_parenthesised_declarator_with_a_name_follows(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::LeftParen {
        return false;
    }

    matches!(
        p.peek_token_kind_at(1..4).as_slice(),
        [
            CppTokenKind::Star | CppTokenKind::Ampersand | CppTokenKind::LogicalAnd,
            CppTokenKind::Identifier,
            CppTokenKind::RightParen
        ]
    )
}

/// Parse the suffixes that bind to a declarator: parameter lists and array bounds, in any order.
///
/// e.g.: the `(int)` and `[4]` of `void (*[4])(int)`
///
/// A second copy of the suffix loop in [`parse_declarator_with`], which cannot be reused here: that one is
/// driven by a declarator's *name* and by the declaration/expression decision, and neither exists in a
/// type-id. What the two share is the rule for what a suffix is — `(` starts a parameter list and `[` an
/// array bound — and that is small enough to state twice rather than to parameterise.
fn parse_declarator_suffixes(p: &mut CppParser) -> ParseResult {
    loop {
        match p.current_token() {
            CppTokenKind::LeftParen => {
                let _ = super::decls::parse_parameter_list(p)?;
                eat_function_qualifiers(p);
            }
            CppTokenKind::LeftBracket => {
                let array = p.mark(CppSyntaxKind::ArrayType);
                p.bump();
                // The bound is optional: `void (*[])()`.
                if p.current_token() != CppTokenKind::RightBracket
                    && !p.is_eof()
                    && let Err(err) = super::exprs::parse_expr(p)
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
    if allow_structured_binding && p.current_token() == CppTokenKind::LeftBracket {
        if let Err(err) = super::decls::parse_structured_binding(p) {
            p.close_marks_above(base);
            return Err(err);
        }
        return Ok(m.complete(p));
    }

    // The name, if there is one.
    let named = named_inside_parentheses
        || matches!(
            p.current_token(),
            CppTokenKind::Identifier
                | CppTokenKind::Scope
                | CppTokenKind::OperatorKeyword
                | CppTokenKind::Tilde
        );
    if named && !named_inside_parentheses && let Err(err) = parse_name(p) {
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
        || super::decls::a_declaration_is_the_better_reading(p, declarator_from)
    {
        loop {
            match p.current_token() {
                CppTokenKind::LeftParen => {
                    if let Err(err) =
                        super::decls::parse_function_suffix_or_initializer(p, declarator_from)
                    {
                        p.close_marks_above(base);
                        return Err(err);
                    }
                    // A `(` that is neither a parameter list nor an initializer belongs to whatever comes next.
                    //
                    // For a declarator **without** a name this loop must stop rather than fail: `g(1, 2);` reaches
                    // the declaration reading this way, and the expression reading is what should have the
                    // parentheses. An abstract declarator's real reason to exist is a type-id such as
                    // `int(void)`, and that is reached through `parse_type_id`, not here.
                    if p.current_token() == CppTokenKind::LeftParen {
                        if named {
                            // For a declarator **with** a name, stopping is not an option: `Widget w(g())` would
                            // leave the declarator parsed, the parentheses unread, and the declaration to fail
                            // later at its `;` — turning a statement the expression grammar could have read into
                            // an error node. Failing here rewinds the whole declaration attempt instead, and the
                            // caller reads the statement as the expression it was.
                            p.close_marks_above(base);
                            return Err(CppParseError::syntax_error_from(
                                "expected a parameter list or an initializer",
                                p.current_token_range(),
                            ));
                        }
                        break;
                    }
                }
                CppTokenKind::LeftBracket => {
                    let array = p.mark(CppSyntaxKind::ArrayType);
                    p.bump();
                    // The bound is optional: `int a[]`.
                    if p.current_token() != CppTokenKind::RightBracket
                        && !p.is_eof()
                        && let Err(err) = super::exprs::parse_expr(p)
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
fn parse_parenthesised_declarator(p: &mut CppParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::Declarator);

    expect_token(p, CppTokenKind::LeftParen)?;
    // The inner declarator's parentheses wrap a declarator, never a parameter list, so no name may appear
    // there: `(*f)` is a pointer and `(*f(int))` a pointer to a *function*, whose parameter list is read by
    // the outer declarator's suffix loop.
    parse_abstract_declarator(p, false)?;
    parse_name(p)?;
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
fn parse_destructor_name(p: &mut CppParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::NameExpr);

    expect_token(p, CppTokenKind::Tilde)?;
    if p.current_token() == CppTokenKind::Identifier {
        p.bump();
    }

    Ok(m.complete(p))
}

/// Parse the parameter list and qualifiers of a declarator whose name has already been read.
///
/// For the destructor above, whose name is claimed before the usual declarator path runs. The loop is the same
/// shape as the one in [`parse_declarator_with`], and it is a second copy for the same reason the first is not
/// reusable there: that loop is driven by the declaration/expression decision, which a destructor has already
/// answered by existing.
///
/// The `last_declarator_is_function` flag is set for the same reason it is set everywhere else — it is what
/// makes a `{` after the declarator a *body* rather than a braced initializer, and `~S() {}` is a definition.
fn parse_declarator_function_suffixes(
    p: &mut CppParser,
    declarator_from: usize,
) -> ParseResult {
    loop {
        match p.current_token() {
            CppTokenKind::LeftParen => {
                super::decls::parse_function_suffix_or_initializer(p, declarator_from)?;
                if p.current_token() == CppTokenKind::LeftParen {
                    break;
                }
            }
            CppTokenKind::LeftBracket => {
                let array = p.mark(CppSyntaxKind::ArrayType);
                p.bump();
                if p.current_token() != CppTokenKind::RightBracket
                    && !p.is_eof()
                    && let Err(err) = super::exprs::parse_expr(p)
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
fn a_qualified_name_is_the_type(p: &CppParser) -> bool {
    let Some(name) = p.declaration_type_name() else {
        return false;
    };

    p.has_qualified_declaration_type_name() && !name.is_empty()
}

/// Parse a template argument list: `<T, int N, ...>`.///
/// The closing `>` is the hard part. `std::vector<std::vector<int>>` ends in `>>`, which the lexer
/// has already produced as a single `RightShift` token, so the *last* `>` of a nested template list
/// has to be split back out here. Doing it in the parser rather than the lexer is deliberate: in
/// `a >> b` the same token really is a shift, and only the parser knows which context it is in.
pub fn parse_template_argument_list(p: &mut CppParser) -> ParseResult {
    // Inside the list, `>` closes it instead of comparing, so the expression grammar has to be told.
    // The depth is restored on every exit — including the error returns inside the helper — because
    // leaving it set would make every later `a > b` in the file parse as a template closer.
    let previous_depth = p.enter_template_arguments();
    let result = parse_template_argument_list_inner(p);
    p.leave_template_arguments(previous_depth);
    result
}

fn parse_template_argument_list_inner(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TemplateArgumentList);

    expect_token(p, CppTokenKind::Less)?;

    while !p.is_eof() {
        // A `>` closes this list exactly when it has no opener of its own: scanning forward from it,
        // the angles balance at zero before any unmatched `<` appears.
        //
        // This is deliberately *not* a comparison of nesting depths. Recomputing a depth on either
        // side of a sub-parse gives different answers for the same token, because parsing an inner
        // list splits a `>>` into two `>`s and changes what the scan counts. Asking "does this `>`
        // already belong to someone else?" has one answer regardless of when it is asked.
        //
        // `split_closing_angle` has already turned any `>>` into a lone `>` by the time we look, so
        // this is the only place the closer is consumed.
        if p.current_token() == CppTokenKind::Greater && closer_belongs_to_this_list(p) {
            p.bump();
            return Ok(m.complete(p));
        }

        let before = p.current_token_index();
        let argument = p.mark(CppSyntaxKind::TemplateArgument);
        if parse_template_argument(p).is_err() {
            argument.undo(p);
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
/// Is the `>` at the cursor the closer of the template argument list being parsed?
///
/// A `>` belongs to this list when no `<` between it and here is still waiting for it — that is, when
/// scanning forward from the `>` the angle nesting returns to zero before going positive.
///
/// The alternative, comparing nesting depths computed before and after a sub-parse, is unreliable:
/// parsing an inner argument list splits `>>` into two `>`s, which changes what the same forward scan
/// counts and silently shifts the reference point. Looking forward from the `>` itself has one answer
/// whenever it is asked.
fn closer_belongs_to_this_list(p: &CppParser) -> bool {
    let mut depth = 0isize;

    for kind in p.peek_token_kind_at(1..96) {
        match kind {
            // An unmatched `<` after this `>` means the `>` was already claimed by it.
            CppTokenKind::Less => return false,
            CppTokenKind::Greater => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            CppTokenKind::RightShift => {
                depth -= 2;
                if depth < 0 {
                    return true;
                }
            }
            // A `>` never closes across one of these.
            CppTokenKind::Semicolon
            | CppTokenKind::LeftBrace
            | CppTokenKind::RightBrace
            | CppTokenKind::LeftParen
            | CppTokenKind::Eof
            | CppTokenKind::None => return true,
            _ => {}
        }
    }

    // Reached the end of the lookahead window without finding an opener: treat it as ours, so a
    // truncated template argument list still closes rather than reporting a bogus nesting error.
    true
}

/// Parse one template argument: a type, a template-id, or a constant expression.
///
/// Returns `Ok` with an empty marker when the type reading applied: the argument *was* the type, and
/// the cursor is already on the delimiter that ends it. `Err` means neither reading worked.
fn parse_template_argument(p: &mut CppParser) -> ParseResult {
    let checkpoint = p.checkpoint();
    let start = p.current_token_index();

    let type_read = parse_type_id(p);

    // Did the type reading get anywhere, and stop somewhere a type can end?
    //
    // The test is deliberately *not* a comparison of `<>` nesting depths. Depth recomputed on either
    // side of a sub-parse disagrees about the same token, because parsing an inner argument list
    // splits a `>>` into two `>`s and changes what the scan counts. "Did this reading consume
    // something, and is the cursor now on a token that cannot continue a type?" has one answer
    // whenever it is asked.
    if p.current_token_index() > start
        && (type_read.is_ok() || !continues_a_type(p.current_token()))
    {
        return Ok(CompleteMarker::empty());
    }

    // Nothing usable: read it as an expression instead.
    p.rollback(checkpoint);
    super::exprs::parse_expr(p)
}

/// Can a type-id continue with this token?
///
/// Used to decide whether a type reading that stopped early stopped *correctly*. `*` and `&` can
/// continue a declarator, so a stop there is premature; a literal or a `,` cannot, so a stop there is
/// the end of the type.
fn continues_a_type(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Star
            | CppTokenKind::Ampersand
            | CppTokenKind::LogicalAnd
            | CppTokenKind::LeftBracket
            | CppTokenKind::LeftParen
            | CppTokenKind::Scope
            | CppTokenKind::Less
            | CppTokenKind::Identifier
            | CppTokenKind::ConstKeyword
            | CppTokenKind::VolatileKeyword
    )
}

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

/// Parse an attribute specifier `[[...]]`.
pub fn parse_attribute_specifier(p: &mut CppParser) -> ParseResult {
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
        "unterminated attribute specifier",
        p.current_token_range(),
    ))
}
