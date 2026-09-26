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
    kind::{CppSyntaxKind, CppTokenKind, Dialect},
    parser::{CompleteMarker, CppParser, Marker, MarkerEventContainer},
    parser_error::CppParseError,
    symbols::SymbolKind,
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
        // `register` — a storage-class specifier, removed in C++17 and still written by C headers (see
        // [`CppSyntaxKind::RegisterSpec`]). It is in this table rather than in a rule of its own because it *is*
        // a storage class: it stands wherever `static` stands, and the specifier loop is what knows that.
        CppTokenKind::RegisterKeyword => CppSyntaxKind::RegisterSpec,
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

/// Is this name written the way an **unexpanded macro that stands for a type** is written?
///
/// `__int64`, `_Float16`, `__m128h`, `HUGEP`, `MY_API` — the MinGW and libstdc++ spellings, and it is the
/// `_`-leading half that [`super::decls::looks_like_a_macro_name`] (all capitals) does not cover. Used in exactly
/// one place: [`name_joins_the_type`]'s decision whether a name may join a type that is already there, where the
/// same three tokens
///
/// ```text
/// unsigned __int64 x;      the name joining is the *type*: `unsigned __int64`, and `x` is the declarator
/// int x MY_DECL_SUFFIX;    the name joining would be the *declarator*, and the macro is a suffix
/// ```
///
/// would otherwise be decided the same way. The convention is the same last resort [`looks_like_a_macro_name`]
/// documents — it is a spelling, not a grammar rule — and it is used here only to choose between two readings
/// that both occur in real code, never to decide that something *is* a macro.
pub(super) fn written_like_a_macro(name: &str) -> bool {
    name.starts_with('_') || super::decls::looks_like_a_macro_name(name)
}

/// Is this name one the **implementation reserved** — a leading underscore, which the standard gives to the
/// compiler and its library and to nobody else?
///
/// The narrower half of [`written_like_a_macro`], and the two are separate because they answer different
/// questions. That one chooses between two readings of *type* tokens (`unsigned __int64 x` against
/// `int x MY_DECL_SUFFIX`), where an all-caps name is as good as an underscored one. This one decides whether a
/// **statement** may be read as an invocation whose `;` the macro's body supplies — a reading that accepts code
/// which is not valid C++ unless the macro supplies the rest — and there the reserved namespace is what says the
/// name is the implementation's and not the user's:
///
/// ```cpp
/// __glibcxx_function_requires(_LessThanComparableConcept<_Tp>)     // bits/stl_algobase.h:237 — that rule
/// FOO(x)                                                          // a user's function or macro — still an error
/// ```
///
/// `FOO` is *also* spelled the way a macro is spelled, but a macro of the user's own written in this file would be
/// `#define`d here, and that is evidence the other rule has and this one does not. See
/// [`crate::grammar::cpp::stats::at_a_macro_call_statement_without_evidence`].
pub(super) fn written_in_the_implementations_namespace(name: &str) -> bool {
    name.starts_with('_')
}

/// Does a **macro invocation with an argument list** begin a declaration, with the declaration's own declarator
/// after it?
///
/// The shape is `NAME ( tokens ) NAME …`, and the second name is what makes it a declaration rather than the two
/// things that look like it:
///
/// ```text
/// WINOLEAPI_(void) CoFreeLibrary (HINSTANCE hInst);   a declaration of `CoFreeLibrary` — this rule
/// _GLIBCXX_BEGIN_NAMESPACE_VERSION                    a macro standing for a whole declaration — the statement rule
/// IF_EXIST(k) { … }                                   a macro whose body is a block — the statement rule
/// ```
///
/// Three conditions, each of which keeps a shape with an owner away from this rule:
///
/// * the name is **not one this file `#define`s** — evidence first, the same order [`crate::parser::MacroNames`]
///   documents, because a macro the file defines has a *body* and the rules that know it read it better;
/// * the name is **not one of the compiler's own spellings** — `__attribute__((__nonnull__)) void f();` is a name,
///   a balanced group and an identifier too, and it has a reader that knows what it is
///   ([`at_an_attribute`]). Reading it here as a macro specifier ended the specifier sequence at the attribute, so
///   `_Rb_tree_node_base* _Rb_tree_rebalance_for_erase(…)` had no type and `bits/stl_tree.h` — which was clean —
///   reported `expected ;` in the middle of the declaration;
/// * the group after the name is **balanced** and what follows it is an **identifier** — the declarator's name. A
///   `;` after the group is `FOO(x);` (a statement or a macro standing for a declaration), a `{` is a definition,
///   and neither is this;
/// * the scan gives up at the first `;` that is not inside the group, so a declaration that ends before the shape
///   completes is not read as one.
fn a_macro_call_begins_the_declaration(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::Identifier
        || p.peek_next_token() != CppTokenKind::LeftParen
        || at_an_attribute(p)
    {
        return false;
    }

    if p.macro_evidence(p.current_token_text()).is_some() {
        return false;
    }

    // The `(` is the next significant token, and the scan counts parentheses from there. An unbalanced group, or a
    // `;` before it closes, means this is not the shape.
    let mut index = super::decls::next_significant_index(p, p.current_token_index());
    let mut depth = 0isize;
    while index < p.token_count() {
        match p.token_kind_at(index) {
            CppTokenKind::LeftParen => depth += 1,
            CppTokenKind::RightParen => {
                depth -= 1;
                if depth == 0 {
                    let after = super::decls::next_significant_index(p, index);
                    return p.token_kind_at(after) == CppTokenKind::Identifier;
                }
            }
            CppTokenKind::Semicolon | CppTokenKind::Eof | CppTokenKind::None => return false,
            _ => {}
        }
        index += 1;
    }

    false
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
    parse_type_id_here(p, a_name_may_be_a_type, true)
}

/// [`parse_type_id`] for a `new`, where the **array bounds are not the type's**.
///
/// `new int[4]` is the case: the standard puts the `[4]` in the *new-declarator* rather than in the type, and the
/// rule that reads those bounds is [`super::exprs`]' own
/// [`crate::grammar::cpp::exprs::parse_new_declarator_suffixes`]. A type-id that swallowed them would move the
/// `ArrayType` node out of the allocation's declarator and into its `TypeId` — a shape that no consumer of a `new`
/// expression expects, and one nothing would report.
///
/// `a_name_may_be_a_type` is `false` for the same call, and for its own reason: the parentheses before the type may
/// be a **placement list**. See [`parse_type_id_with`].
pub fn parse_type_id_for_an_allocation(p: &mut CppParser) -> ParseResult {
    parse_type_id_here(p, false, false)
}

/// Open the `TypeId` node and read the type in it, with the caller's answers to the two questions that decide how
/// far it reaches.
fn parse_type_id_here(
    p: &mut CppParser,
    a_name_may_be_a_type: bool,
    read_array_suffixes: bool,
) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TypeId);

    if let Err(err) = parse_type_id_inner(p, a_name_may_be_a_type, read_array_suffixes) {
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
fn parse_type_id_inner(
    p: &mut CppParser,
    a_name_may_be_a_type: bool,
    read_array_suffixes: bool,
) -> ParseResult {
    if p.current_token() == CppTokenKind::LeftParen
        && starts_a_function_type(p, a_name_may_be_a_type)
    {
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

    // Where the specifiers begin, so "did they name a type?" can be asked of what they produced — see
    // [`read_array_suffixes_of_a_type_id`], which needs the answer and cannot ask it later: by then an abstract
    // declarator may have run, and its `*` is the last token.
    let specifiers_from = p.current_event_count();

    parse_decl_specifier_seq_stopping_at_one_name(p)?;

    // Did those specifiers name a **type**, or only a name this file has never heard of? Two sources, and the
    // first is decisive on its own: a `BuiltinType` node is produced by exactly the branches that name a keyword
    // type. A name needs the file's table, because a bare unknown name is what a *variable* looks like.
    let the_type_is_known = p.events_contain_any(specifiers_from, &[CppSyntaxKind::BuiltinType])
        || p.declaration_type_name()
            .is_some_and(|name| p.is_a_known_type_name(name));

    // An abstract declarator: pointers, references and cv-qualifiers, but no name.
    //
    // No name may appear — this is a type-id, not a declarator — which is what makes `(int)` after it a
    // function type rather than somebody's parameter list. See [`parse_abstract_declarator`].
    parse_abstract_declarator(p, false)?;

    // An **array suffix**: the `[4]` of `int[4]`, the `[2][3]` of `int[2][3]`, the `[4]` of `int*[4]`.
    //
    // It is read here rather than in the abstract declarator because that rule is shared with the *declarator*
    // path, where a `[` after the specifiers is a **structured binding** (`auto [a, b] = pair`) rather than a
    // bound. A type-id has no such reading: `int[4]` is a type and nothing else can be meant by it.
    if read_array_suffixes {
        read_array_suffixes_of_a_type_id(p, the_type_is_known)?;
    }

    Ok(CompleteMarker::empty())
}

/// Read the array suffixes of a type-id, when the type in front is one this file can **prove** is a type.
///
/// ```text
/// sizeof(int[4])     `int` is a keyword type          -> the brackets are a bound
/// sizeof(MyType[4])  …or a name the file declared     -> likewise
/// sizeof(a[0])       `a` is neither                   -> an *index*, and the expression rule owns it
/// ```
///
/// The difference is not visible in the tokens, so the file's own table answers it — the same bounded evidence the
/// rest of the type grammar uses, and the reason [`crate::parser::TypeNames`] exists. A miss costs the type
/// reading, and the expression reading is the one that then applies, which is the safe direction here: `a[0]` is
/// far more often an index than a type.
///
/// `the_type_is_known` is decided by the caller, which is where the specifier sequence is: by the time this runs,
/// an abstract declarator may have consumed a `*`, and *that* is the last token — which is how `sizeof(int*[4])`
/// came to be left alone, the guard having judged the `*` rather than the `int`.
///
/// An empty bound is legal (`int[]`) and so is a run of them (`int[2][3]`).
fn read_array_suffixes_of_a_type_id(p: &mut CppParser, the_type_is_known: bool) -> ParseResult {
    if !the_type_is_known {
        return Ok(CompleteMarker::empty());
    }

    while p.current_token() == CppTokenKind::LeftBracket
        // `[[` is an attribute, not a bound — the declarator's own loop asks the same question.
        && p.peek_next_token() != CppTokenKind::LeftBracket
    {
        let array = p.mark(CppSyntaxKind::ArrayType);
        p.bump(); // `[`

        if p.current_token() != CppTokenKind::RightBracket && !p.is_eof() {
            super::exprs::parse_expr(p)?;
        }
        expect_token(p, CppTokenKind::RightBracket)?;

        array.complete(p);
    }

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
/// Is the name at the cursor a macro whose **whole** body is a declaration-specifier list?
///
/// `Some(names_a_type)` when it is — and `names_a_type` is `true` only when the body holds a token that **is** a
/// type (`long`, `int`, `unsigned`, …), because then the sequence has named a type and a name written after it is
/// the declarator rather than a second type. `const` alone names none: in `(_CONST_RETURN wchar_t *)` the `wchar_t`
/// is still the type.
///
/// Every token of the body has to be a specifier. That is what keeps `#define THIS_ INTERFACE *This,` — and every
/// expression macro — out, and a name nobody has a body for is not claimed at all. The bodies come from this file's
/// own `#define` or from the include closure, which is the channel B89 opened for exactly this kind of reading.
///
/// **An empty body is accepted only in a type-id** (`at_a_type_id`), and that boundary is measured rather than
/// argued: `_mingw.h:376` writes `#define _CONST_RETURN` with nothing after it (its `const` spelling is in the
/// other branch), so `(_CONST_RETURN wchar_t *)(_S)` needs the name to stand for nothing — but accepting empty
/// bodies in a **declaration**'s specifier sequence swallowed the declarator's own name and took the corpus from
/// 435 clean to **424** with 617 messages. A type-id has no declarator name to lose, so there the empty answer is
/// safe; a declaration has one.
fn a_macro_that_is_a_specifier(p: &CppParser, at_a_type_id: bool) -> Option<bool> {
    if p.current_token() != CppTokenKind::Identifier {
        return None;
    }

    let offset = p.current_token_range().start_offset;
    let kinds = p.macro_body_kinds_at(p.current_token_text(), offset)?;

    if kinds.is_empty() {
        return at_a_type_id.then_some(false);
    }

    if !kinds.iter().all(|kind| is_a_specifier_kind(*kind)) {
        return None;
    }

    Some(kinds.iter().any(|kind| is_a_type_specifier_kind(*kind)))
}

/// The tokens a declaration-specifier list may be made of — the body test of [`a_macro_that_is_a_specifier`].
fn is_a_specifier_kind(kind: CppTokenKind) -> bool {
    is_a_type_specifier_kind(kind)
        || matches!(
            kind,
            CppTokenKind::ConstKeyword
                | CppTokenKind::VolatileKeyword
                | CppTokenKind::InlineKeyword
                | CppTokenKind::StaticKeyword
                | CppTokenKind::ExternKeyword
                | CppTokenKind::RegisterKeyword
                | CppTokenKind::VirtualKeyword
        )
}

/// The subset of those that **name a type**, which is the question `has_type_specifier` asks.
fn is_a_type_specifier_kind(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::SignedKeyword
            | CppTokenKind::UnsignedKeyword
            | CppTokenKind::ShortKeyword
            | CppTokenKind::LongKeyword
            | CppTokenKind::IntKeyword
            | CppTokenKind::CharKeyword
            | CppTokenKind::FloatKeyword
            | CppTokenKind::DoubleKeyword
            | CppTokenKind::VoidKeyword
            | CppTokenKind::AutoKeyword
    )
}

/// Does a **specifier** follow the group of a macro invocation at the head of a declaration?
///
/// The follower test of the function-like specifier arm in [`parse_decl_specifier_seq_with`], and it is what
/// splits that shape from the one [`a_macro_call_begins_the_declaration`] owns — two shapes that begin with the
/// same tokens and continue differently:
///
/// ```text
/// MACRO(args) inline More          …a specifier follows  → the invocation is a *specifier*, and the sequence
///                                                          has more of itself to read    (this test)
/// MACRO(args) CoFreeLibrary (…)    …a name follows       → the invocation is the declaration's **head** and the
///                                                          declarator is what follows    (the arm below)
/// ```
///
/// The second is read by the arm a few lines down in the loop, which reads the invocation and **ends the
/// sequence** there — "what the macro expands to is not knowable, so a name read after it could be either the
/// declarator or another word of the type". Claiming it here instead was measured: the sequence then joined
/// `CoFreeLibrary` into the type the invocation stands for, and `WINOLEAPI_(void) CoFreeLibrary (HINSTANCE hInst);`
/// — a shape that reads today — came out as a call expression with ``expected `;` after expression``. So the name
/// follower is deliberately **not** in the set below; a name is a shape with an owner.
///
/// The other half of the question is `FOO(x);`: the group closes and the declaration is already over, so this is a
/// **call** — the reading the expression rules own. Without this test the specifier arm claimed it, the sequence
/// had no declarator, and the statement came out as a declaration of nothing: no error, no `ErrorNode`, no
/// `MissingNode`, and the call the user wrote nowhere in the tree.
///
/// The set is the specifier set the loop itself uses rather than a third list: [`is_a_specifier_kind`] and
/// [`storage_or_function_specifier`] are asked, which is what makes `inline` — the token `binders.h` writes after
/// its `_GLIBCXX11_DEPRECATED_SUGGEST("std::bind")` — the answer here.
fn a_specifier_follows_the_group(p: &CppParser) -> bool {
    let mut depth = 0isize;

    for (position, kind) in p.peek_token_kind_at(1..64).into_iter().enumerate() {
        // Offsets are relative to the cursor, which is on the macro's **name**: `1` is the `(` that opens its
        // argument list.
        let offset = position + 1;

        match kind {
            CppTokenKind::LeftParen => depth += 1,
            CppTokenKind::RightParen => {
                depth -= 1;
                if depth == 0 {
                    let after = offset + 1;
                    return match p.peek_token_kind_at(after..after + 1).first().copied() {
                        Some(kind)
                            if is_a_specifier_kind(kind)
                                || storage_or_function_specifier(kind).is_some() =>
                        {
                            true
                        }
                        // **A name that is a specifier macro** — or a name that simply *continues* the sequence.
                        //
                        // `bits/stl_function.h:1040` writes the same shape one word longer, and that word is a
                        // macro the same way the head is:
                        //
                        // ```cpp
                        //     _GLIBCXX17_DEPRECATED_SUGGEST("std::not_fn")
                        //     _GLIBCXX14_CONSTEXPR                      // ← `constexpr`, in c++config.h
                        //     inline unary_negate<_Predicate>
                        //     not1(const _Predicate& __pred)
                        // ```
                        //
                        // The first half of the question is the one [`a_macro_that_is_a_specifier`] already
                        // answers — "is the whole body a declaration-specifier list?" — asked of a *lookahead*
                        // name instead of the token under the cursor. It is not enough on its own, and the file
                        // above is why: read on its own `stl_function.h` has **no environment at all** (the probe
                        // says `context 0 seeds`, the same emptiness `binders.h` has), so `_GLIBCXX14_CONSTEXPR`
                        // has no body to consult.
                        //
                        // So the shape answers where the tables cannot, and the shape is the token **after** the
                        // name: a follower that is itself followed by `(` is the *declarator* — the shape
                        // [`a_macro_call_begins_the_declaration`] owns (`WINOLEAPI_(void) CoFreeLibrary (…)`) —
                        // while anything else there means the sequence has not reached its declarator yet.
                        Some(CppTokenKind::Identifier) => {
                            a_specifier_macro_at(p, after)
                                || p.peek_token_kind_at(after + 1..after + 2).first().copied()
                                    != Some(CppTokenKind::LeftParen)
                        }
                        _ => false,
                    };
                }
            }
            // A `;` before the group closes means the shape does not complete before the declaration ends.
            CppTokenKind::Semicolon | CppTokenKind::Eof | CppTokenKind::None => return false,
            _ => {}
        }
    }

    false
}

/// Is the name `offset` tokens past the cursor a macro whose **whole body** is a declaration-specifier list?
///
/// The lookahead form of [`a_macro_that_is_a_specifier`]'s body test, sharing its predicate
/// ([`is_a_specifier_kind`]) so the two cannot drift: that one is asked about the token under the cursor, this one
/// about a token ahead of it — see [`a_specifier_follows_the_group`], which is the only caller.
fn a_specifier_macro_at(p: &CppParser, offset: usize) -> bool {
    let name = p.peek_token_text_at(offset);
    let at = p.peek_token_range_at(offset).start_offset;

    p.macro_body_kinds_at(name, at).is_some_and(|kinds| {
        !kinds.is_empty() && kinds.iter().all(|kind| is_a_specifier_kind(*kind))
    })
}

/// Does a **parameter list** follow the run of `[[…]]` attributes at the cursor?
///
/// The follower test of the declarator's suffix loop, which reads an attribute standing between a declarator's
/// name and its parameters — `bool operator== [[nodiscard]] (const tuple<_Tps...>&, …)` — and leaves every other
/// attribute to the rule that owns that position. A `(` after the run can only be this declarator's parameter
/// list, so the test is a scan rather than a preference: skip the attribute run (counting brackets, because an
/// attribute's argument may contain `[`), and look at what stands after it.
fn a_parameter_list_follows_the_attributes(p: &CppParser) -> bool {
    let mut depth = 0isize;
    let mut offset = 0usize;

    while offset < 64 {
        let Some(kind) = p.peek_token_kind_at(offset..offset + 1).first().copied() else {
            return false;
        };

        match kind {
            CppTokenKind::LeftBracket => depth += 1,
            CppTokenKind::RightBracket => {
                depth -= 1;
                if depth == 0 {
                    // The run ended just now, and what stands after it is the answer.
                    return matches!(
                        p.peek_token_kind_at(offset + 1..offset + 2).first(),
                        Some(CppTokenKind::LeftParen)
                    );
                }
            }
            CppTokenKind::None | CppTokenKind::Eof => return false,
            _ => {}
        }

        offset += 1;
    }

    false
}

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
        // Read as an invocation inside the sequence rather than expanded: the file's own tokens are all that enters
        // the tree, and the body is what says the name is a specifier. See `docs/grammar-gaps.md` B91.
        if let Some(names_a_type) = a_macro_that_is_a_specifier(p, !allow_second_name) {
            let call = p.mark(CppSyntaxKind::MacroCall);
            let name = p.mark(CppSyntaxKind::NameExpr);
            p.bump();
            name.complete(p);
            call.complete(p);

            specifiers += 1;
            has_specifier = true;
            has_type_specifier |= names_a_type;
            continue;
        }

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
        // Why the **head of the sequence** and the **group** are enough: no declaration begins with a call — a
        // declaration's first tokens are specifiers — so a name and a balanced group there is a shape nothing else
        // wants. A member that is only an invocation is claimed before any of this by
        // [`super::decls::at_a_macro_member`], and the compiler's own attribute spellings are refused here (see
        // [`at_an_attribute`], which has a reader that knows what they are), so what is left is the shape this arm
        // is for. The `specifiers == 0` half is what keeps it from firing *inside* a sequence that has already
        // named a type, where a name-plus-group is a declarator's parameter list.
        //
        // The arguments are kept as **raw tokens** ([`super::decls::parse_balanced_token_group`], the reader the
        // other macro arms use): what stands there is the macro's own text, and `("std::bind")` is not a grammar
        // this file can read — a `#define` in another file is what knows whether it is a string, a type or an
        // expression.
        // **What the evidence gate is, after the MSVC measurement (B124).** The arm used to require that **nobody**
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
        if specifiers == 0
            && allow_second_name
            && p.current_token() == CppTokenKind::Identifier
            && p.peek_next_token() == CppTokenKind::LeftParen
            && !at_an_attribute(p)
            && a_specifier_follows_the_group(p)
        {
            let checkpoint = p.checkpoint();
            let call = p.mark(CppSyntaxKind::MacroCall);
            let name = p.mark(CppSyntaxKind::NameExpr);
            p.bump();
            name.complete(p);

            if super::decls::parse_balanced_token_group(p, CppSyntaxKind::ArgumentList).is_err() {
                // An unterminated group is not this shape: give the name back and let the loop report whatever it
                // reported before, the same way [`super::decls::eat_a_macro_suffix`] does.
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
            if let Err(err) = super::stats::parse_preprocessor_directive(p) {
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
                if let Err(err) = super::decls::parse_template_head(p) {
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
        // `CoFreeLibrary` with nowhere to go — and the expression reading (B70's fallback) makes the call a
        // *statement*, after which the declarator is a syntax error. What the file wrote is a name, a group, and a
        // declaration, which is what this reads.
        //
        // Claimed **before** the specifier loop asks its question, and only where the shape has no competitor: a
        // name **with a balanced group** followed by an **identifier** — `MACRO(args) name (…)`. A `;` or a `{`
        // after the group is the shapes that already own them (a macro standing for a whole declaration, a macro
        // definition), and evidence keeps its precedence: a name this file `#define`s is read by the rule that knows
        // what a macro is.
        if allow_second_name && specifiers == 0 && a_macro_call_begins_the_declaration(p) {
            if let Err(err) = super::stats::parse_a_macro_that_stands_for_a_declaration(p) {
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
        // caught it (`a_macro_can_stand_among_a_declarators_suffixes`, and the asm-label assertion of B71).
        //
        // Nothing else changes: with no type named yet the name is the type whatever it looks like (the branch
        // above `a_further_name_may_join` in [`name_joins_the_type`]), and `MY_API Widget *p` still joins
        // `Widget` — an ordinary name — through the second condition.
        let a_further_name_may_join = allow_second_name
            && (p.events_contain_any(specifiers_from, &[CppSyntaxKind::TemplateType])
                || (has_type_specifier && written_like_a_macro(p.current_token_text())));
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

fn parse_one_decl_specifier_inner(
    p: &mut CppParser,
    has_type_specifier: &mut bool,
    name_allowed: &mut bool,
    names_may_join: NamesMayJoin,
    a_class_definition_was_written: bool,
    specifier_seen: bool,
) -> ParseResult {
    let base = p.open_marks();
    // Where this specifier begins, for the backward questions a name specifier has to ask — see
    // [`super::decls::type_name_at`]. `base` above cannot answer them: it is a marker-stack length, not a
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

                // The payload is an expression — `decltype(a + b)`, `noexcept(f())` — with **one exception**:
                // `decltype(auto)` holds a type, and `auto` is a keyword that no expression rule accepts, so
                // the expression reading reported `expected primary expression` against it. The two readings
                // are told apart by the one token that cannot be an expression.
                if p.current_token() == CppTokenKind::AutoKeyword {
                    parse_type_id(p)?;
                } else {
                    super::exprs::parse_expr(p)?;
                }

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
            if let Err(err) = super::exprs::parse_type_id_or_expression(p) {
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
            if !name_joins_the_type(
                p,
                *has_type_specifier,
                *name_allowed,
                names_may_join,
                a_class_definition_was_written,
            ) {
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
/// single-level argument list parsed fine. That is maintenance convention #14 in `docs/grammar-gaps.md`: the
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
fn angle_depth_delta(previous: CppTokenKind, kind: CppTokenKind) -> isize {
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
fn can_end_a_template_name(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Identifier | CppTokenKind::Greater | CppTokenKind::RightShift
    )
}

/// qualified type.
fn continues_a_qualified_name(p: &CppParser) -> bool {
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
            CppTokenKind::Comma
            | CppTokenKind::Semicolon
            | CppTokenKind::LeftBrace
            | CppTokenKind::RightBrace
            | CppTokenKind::LeftParen
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
fn pointer_to_member_operator_length(p: &CppParser, at: usize) -> Option<usize> {
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

/// The two ways a name may still become a **word of the type**, computed by the specifier loop and asked about
/// again inside it.
///
/// One value rather than two parameters because they answer the same question in two contexts — "may this name be
/// part of the type?" — and because the specifier loop's signature is already at the limit a reader can hold:
///
/// ```text
/// a_further_name                            a declaration: `MY_API Widget *p`, `unsigned __int64 x;`
/// a_macro_shaped_name_closes_a_type_id      a type-id, where there is no declarator: `(unsigned __LONG32) h`
/// ```
#[derive(Clone, Copy)]
struct NamesMayJoin {
    /// A further name may join a type that is already in the sequence — see [`name_joins_the_type`] for the three
    /// conditions and for the two spellings it decides between (`unsigned __int64 x;` against `int x
    /// MY_DECL_SUFFIX;`).
    a_further_name: bool,
    /// A **macro-shaped** name may close a **type-id**, where there is no declarator to make room for: the cast in
    /// `return ((unsigned __LONG32) (ULONG_PTR) h);`. See [`written_like_a_macro`] and `docs/grammar-gaps.md` B74.
    a_macro_shaped_name_closes_a_type_id: bool,
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
/// * `has_type_specifier` — false while nothing has named a *type*, where a lone name is certainly the type
///   (`Foo` in `Foo x`, `T` in `alignas(16) T x`). A specifier that is not a type — `alignas`, `const`,
///   `static` — leaves this false on purpose: it says nothing about the type, so the next name still has room
///   to be one. Reading it as "a specifier was consumed" is what made `alignas(16) MyType value;` take `MyType`
///   for the declarator and leave `value` with nowhere to go.
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
/// * `a_class_definition_was_written` — the sequence has a **body** in it, and a body is a complete type:
///   `struct S { … } x;`, `struct { … } x;`, `enum E { A } e;`, `typedef struct { … } Alias;`. The walk
///   [`type_is_already_complete`] makes cannot see it — it meets the `}` and answers "no type yet", which is the
///   right answer for the other meaning of a `}` — so the name that follows a definition must be the declarator.
fn name_joins_the_type(
    p: &CppParser,
    has_type_specifier: bool,
    name_allowed: bool,
    names_may_join: NamesMayJoin,
    a_class_definition_was_written: bool,
) -> bool {
    // A **class-like definition** is a complete type, body and all, and the name after it is the declarator:
    //
    // ```text
    // struct S { int a; } x;          declares `x`
    // struct { int a; } x[] = { … };  the C idiom this was found in — an unnamed struct and its variable
    // union { int a; } u = { 1 };     …and the same shape at block scope, which is where it was missed
    // enum E { A } e;                 the same for an enum
    // typedef struct { … } Alias;     the body is the type, `Alias` is the name
    // ```
    //
    // Read the other way the name joined the *type*, so the declaration had no declarator at all: silently for
    // `struct S { … } x;` (a well-formed declaration of nothing, no diagnostic), and loudly as soon as the
    // declarator carried anything — `x = { 1 }` reported `expected a declarator name` against the `=`, and
    // `x[2]` reported `expected ], but get integer literal`. `LuaDefine.h` in the first real C++ project is the
    // third shape, 45 diagnostics from one declaration.
    //
    // **Asked before the "no type yet" early return below**, and that order is the whole of B75: an **anonymous**
    // definition never writes a name, so `has_type_specifier` is still false when the declarator arrives and the
    // early return took it for the type —
    //
    // ```cpp
    // union { __m128h __a[2]; __m256h __v; } __u = { .__v = __A };     // avx512fp16vlintrin.h:155
    // ```
    //
    // — leaving a declaration whose type is `union { … } __u` and which declares nothing. Without an initializer
    // that was **silent** (no diagnostic, no `ErrorNode`, no `MissingNode`); with one it was reported at the `=`,
    // because an initializer needs something to initialise. The body is a complete type whatever the sequence's
    // flags say, so the question is asked first.
    if a_class_definition_was_written {
        return false;
    }
    // No type yet, so this name can only be the type.
    if !has_type_specifier {
        return true;
    }
    // A **type-id**, where a macro-shaped name after a type is simply another word of the type and there is no
    // declarator to make room for:
    //
    // ```cpp
    // static __inline unsigned __LONG32 HandleToULong (const void *h)
    // { return ((unsigned __LONG32) (ULONG_PTR) h); }        // basetsd.h:68 — the cast's type-id
    // ```
    //
    // `allow_second_name` is `false` in a type-id, and that is right for the question it was written for (a second
    // *name* would run two template parameters together); what it must not do is refuse a name that is written the
    // way an unexpanded macro is written. The caller has already checked that: `has_type_specifier` says a type is
    // there, [`written_like_a_macro`] says this name is spelled like a macro, and there is no follower question to
    // ask because a type-id has no declarator. Nothing else is relaxed — `-> int requires C<T>` still ends at
    // `int` because `requires` is not written like a macro.
    if names_may_join.a_macro_shaped_name_closes_a_type_id {
        return true;
    }
    // An **elaborated type specifier**: `struct S`, `union U`, `enum class E`. The keyword is not the type — the
    // name after it is — so this name joins the type whatever the caller said about a *second* name:
    //
    // ```text
    // struct S *p;          a declaration, where a second name is allowed
    // sizeof(struct S)      a type-id, where it is not
    // using A = struct S;
    // (struct S *)p;
    // ```
    //
    // In a type-id the allowance had already been spent on the keyword, so `S` was refused and the type-id came
    // out as `struct` on its own: the payload never reached its `)`, and `sizeof(struct S)` — an ordinary thing to
    // write in C — was reported as `expected primary expression`. The keyword and its name are one specifier, so
    // the allowance never applied to this name in the first place.
    if p.last_consumed_token_kind()
        .is_some_and(is_class_like_keyword)
    {
        return true;
    }
    if continues_a_qualified_name(p) {
        return true;
    }
    // A **second name in the specifier sequence**, which is what an export or attribute macro looks like:
    //
    // ```text
    // MY_API Widget *p;          a declaration of `p`
    // EMMY_API RangeResult f();  a declaration of `f`
    // EXPORT std::string g();    the macro, then a qualified type, then the declarator
    // ```
    //
    // A macro is not expanded here, so `MY_API` is an ordinary name to this parser and the type that follows it
    // is a *second* name in the same sequence. The allowance is spent after the first, so the second was refused
    // and became the declarator name, and the real declarator (`p`, `f`) had nowhere to go: the declaration
    // failed and the whole line came back as an expression statement — `expected ;` against the return type. It
    // is how `CodeFormatCLib.cpp` fails, and there is nothing rare about the spelling: every real C++ project
    // with a DLL/export boundary writes it.
    //
    // What makes the name part of the type rather than the declarator is that **the declaration has not reached a
    // declarator yet**: after it come more names (`B C d`), a `*`, a `&`/`&&`, a `::` or a `<` — the things a
    // declarator is written from. When what follows is `;`, `)`, `,`, `=`, `{` or `(`, the name is the
    // declarator and the allowance still decides, exactly as before.
    //
    // Nothing valid is taken from the expression reading by this: `Name Name Name` and `Name Name * Name` are not
    // expressions in any grammar, so the only statements that change are the ones that had no reading at all.
    if names_may_join.a_further_name && a_declarator_still_follows_the_name(p) {
        return true;
    }
    // A name the **caller's table** says is a type, joining a sequence that has already taken a name: that is the
    // shape a modifier macro leaves behind —
    //
    // ```text
    // MY_API Widget *p;         the ordinary spelling, which the shape rule above already reads
    // MY_API Widget const w;    …and the one it cannot: `const` is not a declarator, so the follower rule says
    //                           "no declarator follows" and the type would end at `const`
    // ```
    //
    // The first name is what makes this safe to ask: before a name has joined, a type name is what the *sequence*
    // is for, and `name_allowed` already decides it.
    if names_may_join.a_further_name
        && p.symbol_kind(p.current_token_text())
            .is_some_and(|kind| matches!(kind, SymbolKind::Type | SymbolKind::Template))
    {
        return true;
    }
    let complete = type_is_already_complete(p);
    let called = a_parenthesis_follows_the_name(p);
    if complete || called {
        return false;
    }
    name_allowed
}

/// Does what follows the name at the cursor still leave room for a **declarator**?
///
/// The question [`name_joins_the_type`] asks about a *second* name in the specifier sequence — the one an export
/// macro leaves behind. The name may be qualified (`EXPORT std::string g();`) or templated
/// (`MY_API Vector<int> *make();`), so the whole name is walked first and the token after it is what answers.
///
/// Walking *past* a template argument list rather than stopping at its `<` is the whole subtlety, and both
/// directions are real code:
///
/// ```text
/// MY_API Vector<int> *make();     a `*` after the list needs a declarator, so `Vector<int>` is the type
/// template MyType f<int>(int);    a `(` after the list calls the name, so `f<int>` is the *declarator's* name
/// ```
///
/// Answering at the `<` reads the second line as a type `MyType f<int>` with no declarator left, and the explicit
/// instantiation fails — which is exactly what a first version of this rule did.
pub(super) fn a_declarator_still_follows_the_name(p: &CppParser) -> bool {
    let mut after = super::decls::next_significant_index(p, p.current_token_index());
    while p.token_kind_at(after) == CppTokenKind::Scope {
        let segment = super::decls::next_significant_index(p, after);
        if p.token_kind_at(segment) != CppTokenKind::Identifier {
            break;
        }
        after = super::decls::next_significant_index(p, segment);
    }

    // A template argument list is part of the name, so step over it. `angle_depth_delta` is the same rule the
    // lookahead scans use — including the `>>` that closes two lists at once.
    if p.token_kind_at(after) == CppTokenKind::Less {
        let mut depth = 0isize;
        let mut index = after;
        // The `<` at `after` is preceded by the name the walk above stepped over — see [`angle_depth_delta`].
        let mut previous = CppTokenKind::Identifier;
        while index < p.token_count() {
            let kind = p.token_kind_at(index);
            depth += angle_depth_delta(previous, kind);
            previous = kind;
            index += 1;
            if depth <= 0 {
                break;
            }
        }
        after = super::decls::next_significant_index(p, index.saturating_sub(1));
    }

    // An **identifier** after the name means the declaration has not reached its declarator yet — `unsigned
    // __int64 x`, `MY_API Widget *p`.
    matches!(
        p.token_kind_at(after),
        CppTokenKind::Identifier
            | CppTokenKind::Star
            | CppTokenKind::Ampersand
            | CppTokenKind::LogicalAnd
            // **An operator-function-name is a declarator**, and it is the one that does not begin with an
            // identifier:
            //
            // ```cpp
            // #ifdef __glibcxx_string_view          // bits/basic_string.h:1025
            //   template<typename _Tp>
            //     _GLIBCXX20_CONSTEXPR
            //     _If_sv<_Tp, basic_string&>
            //     operator=(const _Tp& __svt)
            //     { return this->assign(__svt); }
            // #endif
            // ```
            //
            // Read without this, `_If_sv<_Tp, basic_string&>` was the *declarator's* name — the template-id
            // allowance below the caller makes for a variable template's partial specialization took it — so the
            // member came out as a **variable** called `_If_sv`, and the real declarator, its parameter list and
            // its body had nowhere to go: they landed beside the declarator instead of inside it, and every member
            // written after them was read as a child of that one declaration rather than as a member of the class.
            //
            // Nothing valid is taken from the expression reading by this: `Name operator` is not an expression in
            // any grammar, so the only statements that change are the ones that had no reading at all.
            | CppTokenKind::OperatorKeyword
    )
        // **A type keyword after the name**: the type has not started yet, so the name is still part of it.
        //
        // ```cpp
        // _GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR    // bits/basic_string.h:1329
        // bool
        // empty() const _GLIBCXX_NOEXCEPT
        // { return _M_string_length == 0; }
        // ```
        //
        // Two unexpanded macros and then a keyword type. The first name joined the type (the sequence had nothing
        // yet), and the second was refused here because `bool` is not an identifier, a `*`, a `&` or an `operator`
        // — so `_GLIBCXX20_CONSTEXPR` became the *declarator's* name, the declaration came out as a **variable**
        // of that name, and `empty()`, its body and every member after it were read as children of it instead of
        // as members of the class. That is what `std::basic_string::empty` was missing for.
        //
        // A keyword type is what a declaration writes where a declarator cannot go, so it is evidence for the same
        // reason the `*` is: `Name bool` is not an expression in any grammar.
        || is_type_specifier_keyword(p.token_kind_at(after))
        // …and **a specifier keyword** for the same reason, one step further into the type:
        //
        // ```cpp
        // template<typename _CharT, typename _Traits, typename _Alloc>
        //   _GLIBCXX_NODISCARD _GLIBCXX20_CONSTEXPR      // bits/basic_string.h:3837
        //   inline basic_string<_CharT, _Traits, _Alloc>
        //   operator+(const basic_string<…>& __lhs, const basic_string<…>& __rhs)
        // ```
        //
        // Here the follower of the second macro is `inline`, and *its* follower question is asked about the
        // template-id, whose follower is the `operator`. Neither `inline` nor a keyword type can begin a
        // declarator, so each one says the same thing: the sequence has not reached the declarator yet, and the
        // name in front of it is still part of the type. Without this the file's first error sat on the
        // `operator+` — the whole tail of `bits/basic_string.h` unread, and the two `operator+` overloads, which
        // are how `std::string` is concatenated, were not members of anything.
        || is_a_declaration_specifier(p.token_kind_at(after))
}

/// May this keyword stand among a declaration's leading words — where a *declarator* can never go?
///
/// The companion of [`is_type_specifier_keyword`], and deliberately built from the lists that already exist
/// rather than from a third one: `storage_or_function_specifier` is the set the specifier sequence itself reads
/// as specifiers, and `const`/`volatile` are the two cv-qualifiers, which name no type but continue one.
///
/// It answers the same question for the *other* kind of specifier: a type keyword says the type is still coming,
/// and a specifier keyword says the same — `MY_API Widget const w;` and `MY_API inline Widget f();` are both
/// declarations whose type is not finished at the name.
fn is_a_declaration_specifier(kind: CppTokenKind) -> bool {
    storage_or_function_specifier(kind).is_some()
        || matches!(kind, CppTokenKind::ConstKeyword | CppTokenKind::VolatileKeyword)
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
    // The token before the one being looked at — see [`angle_depth_delta`]. The scan starts just past the name,
    // so the first token's predecessor is that very name, which *can* end a template-name: `<` right after it
    // opens a list, which is what `A<int>(x)` needs.
    let mut previous = CppTokenKind::Identifier;

    for kind in p.peek_token_kind_at(1..64) {
        match kind {
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
            kind => depth += angle_depth_delta(previous, kind),
        }

        previous = kind;
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
/// # What the walk steps over
/// # What the walk steps over
///
/// A run of `alignas(…)` specifiers, payloads and keywords alike: it is the one specifier whose tokens end in a
/// bracket, and neither its payload nor its keyword finishes a type. Stepping over the whole run is what keeps
/// `alignas(16) MyType value;` from reading `MyType` as a second name of a *finished* type.
///
/// Everything else that finishes no type is in the exclusion list at the end, and the walk is bounded by the
/// statement's own tokens — a `;`, a brace, or the start of the file.
///
/// # Why the indices are raw
///
/// [`CppParser::current_token_index`] is what the walk starts from, and it indexes the source text's token array.
/// [`CppParser::token_kind_at`] and [`CppParser::token_text_at`] index the same array, so every index here is raw
/// and every question is asked with the same accessor family. Mixing in an index counted over *significant* tokens
/// is the mistake this walk is most prone to, and it is invisible in the arithmetic.
fn type_is_already_complete(p: &CppParser) -> bool {
    let mut index = p.current_token_index();

    while index > 0 {
        index -= 1;
        let kind = p.token_kind_at(index);

        // Trivia is not a token of the declaration.
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

        // A **cv-qualifier** neither finishes a type nor unfinishes one, and *where* it sits is what tells the two
        // spellings apart:
        //
        // ```text
        // const char w[]      the `const` comes *before* the type…
        // char const w[]      …and here it comes after, so the type was already finished when it arrived
        // ```
        //
        // Both are a qualified `char`, and in both the declarator is `w`. Judging the qualifier itself answered
        // "not complete" for the second spelling, so `w` joined the *type*: the declaration came out with no
        // declarator at all, and `char const w[2] = { 'a' };` was reported as a broken expression. (`char const* p`
        // was unaffected only because the `*` follows the qualifier and stops the sequence first.)
        //
        // So the walk steps over it and judges what is in front.
        if matches!(
            kind,
            CppTokenKind::ConstKeyword | CppTokenKind::VolatileKeyword
        ) {
            continue;
        }

        // A run of `alignas(…)` specifiers, payloads and keyword alike: it is the one specifier whose tokens end
        // in a bracket, and neither its payload nor its keyword finishes a type. Stepping over the whole run is
        // what keeps `alignas(16) MyType value;` from reading `MyType` as a second name of a *finished* type.
        if kind == CppTokenKind::AlignasKeyword {
            continue;
        }
        if kind == CppTokenKind::RightParen
            && let Some(keyword) = the_alignas_introducing_the_payload_ending_at(p, index)
        {
            if keyword == 0 {
                return false;
            }
            index = keyword;
            continue;
        }

        // A statement boundary means the walk ran out of *this* declaration without finding a type, which is the
        // opposite of finding a complete one.
        if matches!(
            kind,
            CppTokenKind::Semicolon
                | CppTokenKind::LeftBrace
                | CppTokenKind::RightBrace
                | CppTokenKind::Eof
                | CppTokenKind::None
        ) {
            return false;
        }

        // A class keyword names a *kind* of type and is not a type by itself — what it introduces still has to be
        // named or given a body. See the note above about `struct Foo f;`.
        return !matches!(
            kind,
            CppTokenKind::ClassKeyword
                | CppTokenKind::StructKeyword
                | CppTokenKind::UnionKeyword
                | CppTokenKind::EnumKeyword
        );
    }

    false
}

/// The raw index of the `alignas` keyword introducing the payload whose `)` is at `raw_index`, if that is what
/// the parenthesis is.
///
/// Walks back over the payload's own brackets to its `(` and answers only when the token before it is the
/// keyword. Bounded by the first token that cannot be inside an alignment — a `;`, a brace, or the start of the
/// file — so a parenthesis belonging to something else cannot make this walk run away.
fn the_alignas_introducing_the_payload_ending_at(p: &CppParser, raw_index: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut cursor = raw_index;

    loop {
        match p.token_kind_at(cursor) {
            CppTokenKind::RightParen => depth += 1,
            CppTokenKind::LeftParen => {
                depth -= 1;
                if depth == 0 {
                    let keyword = the_significant_token_before(p, cursor)?;
                    return (p.token_kind_at(keyword) == CppTokenKind::AlignasKeyword)
                        .then_some(keyword);
                }
            }
            CppTokenKind::Semicolon
            | CppTokenKind::LeftBrace
            | CppTokenKind::RightBrace
            | CppTokenKind::Eof
            | CppTokenKind::None => return None,
            _ => {}
        }

        if cursor == 0 {
            return None;
        }
        cursor -= 1;
    }
}

/// The raw index of the nearest non-trivia token before `raw_index`, or `None` at the start of the file.
fn the_significant_token_before(p: &CppParser, raw_index: usize) -> Option<usize> {
    let mut cursor = raw_index;
    while cursor > 0 {
        cursor -= 1;
        if !matches!(
            p.token_kind_at(cursor),
            CppTokenKind::Whitespace
                | CppTokenKind::Newline
                | CppTokenKind::LineContinuation
                | CppTokenKind::LineComment
                | CppTokenKind::BlockComment
        ) {
            return Some(cursor);
        }
    }
    None
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

    // A **macro between the class-key and the name**, which is where a compiler's alignment attribute is written:
    //
    // ```cpp
    // typedef struct DECLSPEC_ALIGN (8) _XSAVE_AREA_HEADER { … } XSAVE_AREA_HEADER, *PXSAVE_AREA_HEADER;
    //         ^^^^^^ ^^^^^^^^^^^^^^ (8)  the macro and its argument list, then the tag
    // ```
    //
    // `winnt.h` writes this shape for every aligned structure, and the same position holds
    // `__attribute__((packed))` and `__declspec(align(8))`. The macro is defined in `_mingw.h` — *another file* —
    // so the file-local macro table cannot know it, and no spelling convention would be evidence here. What makes
    // accepting it free is the shape: after a class-key the grammar allows an attribute-specifier-sequence, a
    // name, `{`, `:` or `;`, and a **name followed by a parenthesised group** is none of those — there is no other
    // reading to take away. The guard is what follows: this only fires when the run ends somewhere a class head
    // can continue, so a genuine mistake is still a mistake. The same rule the namespace head uses for
    // `namespace std _GLIBCXX_VISIBILITY(default) {` — see `eat_namespace_head_macros`.
    while p.current_token() == CppTokenKind::Identifier
        && p.peek_next_token() == CppTokenKind::LeftParen
    {
        let checkpoint = p.checkpoint();
        p.bump(); // the macro's name

        let group_is_read =
            super::decls::parse_balanced_token_group(p, CppSyntaxKind::ArgumentList).is_ok();
        let continues = matches!(
            p.current_token(),
            CppTokenKind::Identifier
                | CppTokenKind::LeftBrace
                | CppTokenKind::Colon
                | CppTokenKind::Semicolon
        );

        if !group_is_read || !continues {
            p.rollback(checkpoint);
            break;
        }
    }

    // …and the same position holds a **bare** macro with no argument list: `class _GLIBCXX17_DEPRECATED
    // unary_negate : public …` (`bits/stl_function.h:1021`), where the macro is the standard library's
    // deprecation marker and the name follows it.
    //
    // Two names in a row is not a class head in any reading — after the class-key the grammar allows an
    // attribute, one name, `{`, `:` or `;` — so the first name is the macro. Two guards keep the shapes that
    // *look* similar out of it, and both were needed:
    //
    // * the second name must be followed by something a class head continues with (`{`, `:`, `;`, a directive),
    //   which is what refuses `struct S requires C<T> { }` — there the second name is followed by another name;
    // * `final`, `override` and `requires` are **spellings** rather than tokens (C++11 made the first two
    //   contextual and C++20 the third), so a second name spelled that way is never the class's name here:
    //   `class A final : B` is a class named `A`.
    if p.current_token() == CppTokenKind::Identifier {
        let second = p.peek_token_kind_at(1..2);
        let after_second = p.peek_token_kind_at(2..3);

        let second_is_a_name = second.first() == Some(&CppTokenKind::Identifier)
            && !matches!(p.peek_token_text_at(1), "final" | "override" | "requires");
        let the_head_continues = matches!(
            after_second.first(),
            Some(&CppTokenKind::LeftBrace)
                | Some(&CppTokenKind::Colon)
                | Some(&CppTokenKind::Semicolon)
                | Some(&CppTokenKind::Hash)
        );

        if second_is_a_name && the_head_continues {
            p.bump(); // the macro's name; the class's own name is read below
        }
    }

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
    //
    // A **directive between the name and the base clause** is read first, and the two are one loop because the
    // two orders occur: the head's base clause may be written inside a branch, or the whole head may be written
    // in two branches that each carry their own `:`:
    //
    // ```cpp
    // class move_iterator                                  // bits/stl_iterator.h:1435
    // #ifdef __glibcxx_ranges
    //   : public __detail::__move_iter_cat<_Iterator>
    // #endif
    // { … };
    // ```
    //
    // Without this the head ended at the name, the declaration asked for a `;`, and the error landed on the `:`
    // of the next line — a diagnostic about a line that is not wrong. The directives are read as the nodes they
    // are, so the tree keeps them and both branches stay visible.
    loop {
        let mut read_a_directive = false;

        while p.current_token() == CppTokenKind::Hash {
            let checkpoint = p.checkpoint();

            if super::stats::parse_preprocessor_directive(p).is_err() {
                p.rollback(checkpoint);
                break;
            }

            read_a_directive = true;
        }

        if keyword != CppTokenKind::EnumKeyword
            && p.current_token() == CppTokenKind::Colon
            && a_brace_follows_the_base_clause(p)
        {
            if let Err(err) = parse_base_clause(p) {
                p.close_marks_above(base_marks);
                return Err(err);
            }

            continue;
        }

        // Nothing else to read: the head is as long as it is.
        if !read_a_directive {
            break;
        }
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
///
/// # Why a head inside a directive ends the question
///
/// Because a conditional can hold **another head for the same declaration**, and then the `{` belongs to that one:
///
/// ```cpp
/// template<typename _Tp, typename _Up>                                   // bits/alloc_traits.h:72
/// #if __cpp_concepts
///   requires requires { typename _Tp::template rebind<_Up>::other; }
///   struct __rebind<_Tp, _Up>                    // ← this head has no body: the other branch does
/// #else
///   struct __rebind<_Tp, _Up, __void_t<typename _Tp::template rebind<_Up>::other>>
/// #endif
///   { using type = …; };
/// ```
///
/// Reading through the directive, the first head saw the second branch's `{` and took it for its own body: the
/// body rule was entered with a `#` at the cursor, failed, and the whole declaration came apart — `bits/alloc_traits.h`
/// lost `__allocator_traits_base` and everything after it. A class-like keyword *after a directive* is what says
/// "the head this body belongs to is not me", and it is the only thing that does: a base clause may also live
/// inside a conditional (`struct S #if X : public B #endif { }`), and there the `{` really is this head's.
fn a_body_follows_the_class_head(p: &CppParser) -> bool {
    let mut depth = 0isize;
    // A **braced-init-list inside the arguments** is not the body, and this is the third place the same question
    // had to be asked — after the two template-id scans above. The head
    //
    // ```cpp
    // template<typename _Alloc>
    //   struct __is_allocator<_Alloc, __void_t<…, decltype(std::declval<_Alloc&>().allocate(size_t{}))>>
    //   : true_type { };
    // ```
    //
    // writes a `{` **and a `}`** while the head is still going, and the `}` ended the scan: the class was read as
    // body-less, so the base clause and the body became rubble and the diagnostic landed at the end of the line
    // (`bits/alloc_traits.h:941`). Counted the way the angles are — and a `}` that closes nothing still ends the
    // scan, because that one is the enclosing body's own.
    let mut braces = 0isize;
    let mut after_a_directive = false;
    // The token before the one being looked at, for the `<` question [`angle_depth_delta`] answers. The scan
    // starts at the head's own `:`, which cannot end a template-name, so that is what it starts as.
    let mut previous = CppTokenKind::Colon;

    for kind in p.peek_token_kind_at(0..64) {
        if after_a_directive && is_class_like_keyword(kind) {
            return false;
        }

        match kind {
            CppTokenKind::LeftBrace if depth <= 0 => return true,
            CppTokenKind::LeftBrace => braces += 1,
            CppTokenKind::RightBrace if braces > 0 => braces -= 1,
            // **A `;` inside a braced group is not the head's end.** The same "a matched group hides the
            // structural boundary" rule the angle scan states: `type_traits:3946` writes a class head whose base
            // clause carries a *requires-expression*, and the `;` in its body ended this scan — so the head was
            // read as body-less, the base clause became rubble, and the diagnostic landed on the `:` of a head
            // that is perfectly good C++20.
            CppTokenKind::Semicolon if braces > 0 => {}
            CppTokenKind::Semicolon | CppTokenKind::RightBrace | CppTokenKind::Eof => return false,
            // The `#` of a directive: everything past it is on another line, and may be another branch.
            CppTokenKind::Hash => after_a_directive = true,
            kind => depth += angle_depth_delta(previous, kind),
        }

        previous = kind;
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

        // A base is a **class-or-decltype**: a name, or a `decltype`-specifier. The second spelling is the one
        // every `__or_`/`__and_` in libstdc++ uses to name a base it computes rather than writes —
        //
        // ```cpp
        //   template<typename... _Bn>
        //     struct __or_
        //     : decltype(__detail::__or_fn<_Bn...>(0))     // type_traits:199
        //     { };
        // ```
        //
        // — and reading only a name reported `expected a name` against the `:`, which took the whole class head
        // with it. Nothing else about the clause changes: what follows the type is still either a pack expansion
        // or the end of this base.
        if p.current_token() == CppTokenKind::DecltypeKeyword {
            if let Err(err) = parse_type_id(p) {
                p.close_marks_above(base_marks);
                return Err(err);
            }
        } else if let Err(err) = parse_name(p) {
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
        // A **directive between enumerators**, which is the same seam as the one in the parameter list, the
        // template parameter list and the specifier sequence, and it is written the same way:
        //
        // ```cpp
        //       bad_file_descriptor = EBADF,
        // #ifdef EBADMSG
        //       bad_message = EBADMSG,
        // #endif
        //       broken_pipe = EPIPE,              // x86_64-w64-mingw32/bits/error_constants.h:52
        // ```
        //
        // A `#` where an enumerator should be is not an enumerator, so the directive is read as the node it is and
        // the question is asked again about the token that follows it. The seam is **before** the enumerator and
        // after the comma, so it also covers a directive that closes a branch in the middle of the list.
        while p.current_token() == CppTokenKind::Hash {
            if let Err(err) = super::stats::parse_preprocessor_directive(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }

        if p.current_token() != CppTokenKind::Identifier {
            break;
        }

        let enumerator = p.mark(CppSyntaxKind::EnumeratorDecl);
        p.bump();

        // Attributes on the enumerator: `enum E { A [[deprecated]] = 1, B };`. Written after the name and
        // before the `=`, so the initializer rule below would otherwise meet a `[[` it has no rule for.
        if let Err(err) = parse_attribute_specifiers(p) {
            p.close_marks_above(base);
            return Err(err);
        }

        // **A macro in the same position** (`omp.h:74`), which is the same seam one spelling along — the
        // attribute the compiler is told about by name rather than written out:
        //
        // ```c
        //   omp_proc_bind_master __GOMP_DEPRECATED_5_1
        //     = omp_proc_bind_primary,
        // ```
        //
        // `__GOMP_DEPRECATED_5_1` is `__attribute__((__deprecated__))` — defined in that file, in two branches —
        // and read as a name the enumerator ended at it: the loop then wanted a `,` or a `}` where the `=` stood,
        // and the whole `typedef enum … } omp_proc_bind_t;` came out as rubble from there on.
        //
        // Only a **name** is taken, and only here: after an enumerator's own name the valid tokens are `[[`, `=`,
        // `,`, a directive and `}` — a name is none of them, so there is no other reading for it to compete with.
        // That is the same argument [`super::decls::eat_a_macro_suffix`] documents for the declarator's suffixes,
        // and it is the reader used here rather than a second copy of "a macro invocation".
        while super::decls::eat_a_macro_suffix(p) {}

        if p.current_token() == CppTokenKind::Assign {
            p.bump();
            // Read **below the comma operator**, because the enumerator list is comma-separated:
            // `enum E { A = 0, B };` is two enumerators, and an initializer reader that took the comma swallowed
            // `B` whole — the tree then held one `EnumeratorDecl` whose initializer was the expression `0, B`, with
            // every token present, no `ErrorNode` and no diagnostic. A consumer asking the enum for its members
            // lost every one of them after the first initialised one, and nothing said so.
            //
            // This is the **same rule the bit-field width follows**, and for the same reason: a rule that spells a
            // comma itself has to opt out of the operator that spells commas. That one was fixed first and this
            // one was left behind, which is how the defect survived a green test suite — the enum examples in
            // `gaps.rs` all happen to have no initializer.
            if let Err(err) = super::exprs::parse_assignment_expr(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }
        enumerator.complete(p);

        // …and the same seam on the **other side of the enumerator**, because a directive may also close a branch
        // before the comma: `b = 2` `#endif` `,` is written as often as the comma-first spelling, and the comma is
        // what the loop below is looking for.
        while p.current_token() == CppTokenKind::Hash {
            if let Err(err) = super::stats::parse_preprocessor_directive(p) {
                p.close_marks_above(base);
                return Err(err);
            }
        }

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
/// Does the macro written at the cursor supply the **`::` a qualified name starts with**? (B121)
///
/// The question is asked of the replacement list's **last** token, and it has to be asked of the body rather than
/// of the name, because no spelling answers it: `_STD` is three letters that could stand for anything, and
/// `#define _STD ::std::` is a fact about a header. A body that ends at a `::` is a nested-name-specifier, so the
/// name after the invocation continues the same qualified name — which is what makes `_STD addressof(*p)` one
/// expression and `_STD reverse_iterator<iterator>` one type, instead of two names in a row.
///
/// **One predicate, two grammars**: the segment loops of [`parse_name`] here and of `parse_primary_expr` in
/// `exprs.rs` ask the same question, and the second copy is where the exception gets forgotten (maintenance
/// convention 14). A buffer parsed with no include closure gets `None` — nobody says — and keeps today's reading:
/// the invocation is a name of its own, and the name after it is the syntax error it looks like.
pub(super) fn a_macro_qualifies_the_name(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::Identifier {
        return false;
    }

    let offset = p.current_token_range().start_offset;
    p.macro_body_kinds_at(p.current_token_text(), offset)
        .is_some_and(|kinds| kinds.last() == Some(&CppTokenKind::Scope))
}

/// Can the token at the cursor continue a qualified name as its next **segment**?
///
/// The four ways a segment is spelled, matching the arms of the segment loops that call this. Asked only after a
/// macro supplied the `::`, so that a qualifier with nothing to qualify ends the name there rather than reporting
/// a name that is missing because of a `::` the file never wrote.
pub(super) fn at_a_name_segment(p: &CppParser) -> bool {
    matches!(
        p.current_token(),
        CppTokenKind::Identifier
            | CppTokenKind::Tilde
            | CppTokenKind::OperatorKeyword
            | CppTokenKind::TemplateKeyword
    )
}

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
            if let Err(err) = super::stats::parse_preprocessor_directive(p) {
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
                    "expected a name after `template`",
                    p.current_token_range(),
                ));
            }
        }

        match p.current_token() {
            // **A qualifier the file wrote as a macro** (B121): `_STD reverse_iterator<iterator>` with
            // `#define _STD ::std::`. Read as what it is — a `MacroCall` — and then the loop comes back around to
            // read the segment it qualifies. Without this the specifier sequence took `_STD` for the type and the
            // name after it for something else: `using reverse_iterator = _STD reverse_iterator<iterator>;` came
            // out as a type plus a nested `Declaration` (a recovery), which in MSVC's `<vector>` is an
            // empty-named fact filed in `std::vector` — enough to make `std::vector` itself ambiguous.
            CppTokenKind::Identifier if a_macro_qualifies_the_name(p) => {
                let call = p.mark(CppSyntaxKind::MacroCall);
                let name = p.mark(CppSyntaxKind::NameExpr);
                p.bump();
                name.complete(p);
                call.complete(p);

                if !at_a_name_segment(p) {
                    break;
                }
                continue;
            }
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
        for kind in p.peek_token_kind_at(1..128) {
            match kind {
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
fn a_bare_template_id_is_here(p: &CppParser) -> bool {
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

    for (index, kind) in p.peek_token_kind_at(2..128).iter().enumerate() {
        match kind {
            // The `<` question, asked of the token before it — see [`angle_depth_delta`]. The scan starts inside
            // the list, so the first token's predecessor is the list's own `<`, which ends no template-name.
            CppTokenKind::Less if can_end_a_template_name(previous) => depth += 1,
            CppTokenKind::Greater | CppTokenKind::RightShift => {
                // `>>` closes two levels at once, so it can be the matching `>` of either the inner or the
                // outer list; both arrive here because the amount is all that differs.
                depth -= if *kind == CppTokenKind::RightShift {
                    2
                } else {
                    1
                };
                if depth <= 0 {
                    let after = index + 3;
                    return !matches!(
                        p.peek_token_kind_at(after..after + 1).first(),
                        Some(&CppTokenKind::Scope)
                    );
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

        previous = *kind;
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
fn a_ref_qualifier_is_here(p: &CppParser) -> bool {
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
pub fn parse_abstract_declarator(p: &mut CppParser, name_possible: bool) -> ParseResult {
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
            // **A pointer to member written after the type**: `int C::*p;`, `int (C::*h)(int);`,
            // `void g(int (C::*)(int));`.
            //
            // The grammar puts the nested-name-specifier *inside* the ptr-operator (`nested-name-specifier *`),
            // so the name that begins it stands where the abstract declarator begins — this loop — and not where
            // the declarator's own name does. Reading it as a type instead is what the silent defect looked like:
            // `C::` joined the specifier sequence, `*p` became a declarator **inside the type node**, and nothing
            // anywhere said so. `continues_a_qualified_name` now refuses a `::` that a `*` follows, which is what
            // brings the tokens here.
            //
            // The parenthesised spelling had one failure mode on top of that one: `int (C::*h)(int);` reaches the
            // declarator with `C` at the cursor, this loop broke immediately on a name, and the parentheses came
            // out empty and the declaration failed — the form an out-of-line member-pointer *typedef* is written
            // in.
            CppTokenKind::Identifier if pointer_to_member_operator_length(p, 0).is_some() => {
                let _ = container!();
                let op = p.mark(CppSyntaxKind::PointerType);

                // The nested-name-specifier: `A::B::` is a name and a `::` per segment, and the `::` that ends
                // the specifier is the one the `*` follows.
                while p.peek_next_token() == CppTokenKind::Scope {
                    p.bump(); // a name of the nested-name-specifier
                    p.bump(); // its `::`
                }
                p.bump(); // `*`
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
            // **A directive inside the run of pointer/reference operators**, which is the same seam as
            // everywhere else in this file and the one that decides how much of `bits/basic_string.h` reads:
            //
            // ```cpp
            // #else                                        // bits/basic_string.h:2631
            //   template<class _InputIterator>
            // #ifdef _GLIBCXX_DISAMBIGUATE_REPLACE_INST
            //     typename __enable_if_not_native_iterator<_InputIterator>::__type
            // #else
            //     basic_string&
            // #endif
            //     replace(iterator __i1, iterator __i2, _InputIterator __k1, _InputIterator __k2)
            // ```
            //
            // One declaration with a conditional *return type*, so the `&` of `basic_string&` is followed by the
            // `#endif` that closes the type's alternation and only then by the declarator's name. The `#ifdef` half
            // is read by the specifier sequence, which is where the other branch's head is read too; this directive
            // is the last token of the construct and had no owner at all — the declarator came out **nameless**,
            // the declaration ended at the `&`, and every member written after it was read as a child of that
            // nameless declarator instead of as a member of the class. Silent, lossless, no diagnostic.
            //
            // Read as the node it is and go round again — the next token is the name, or another operator. A `#`
            // cannot be part of a declarator: nothing in `int * # x` has a reading in any grammar.
            //
            // Only once an operator has been read (`container` is open): a directive *before* the first one is not
            // this rule's, and taking it here would take it away from the loop that owns the declaration's
            // specifiers — the rule that has to see it to keep its own sequence going.
            CppTokenKind::Hash if container.is_some() => {
                super::stats::parse_preprocessor_directive(p)?;
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
/// A `)` is excluded as well **unless a keyword type stands in front of it**: `void ()` is a function type with no
/// parameters, and it is the one case where the parentheses hold nothing at all, so nothing inside them can
/// answer. The token before them does: a keyword type is a type whatever follows it — `std::function<void()>` is
/// everywhere, and it was read as `void` with a stray `()` after it, which cost the whole argument — while a
/// *name* before an empty group may be a call (`f()`), and stays one: the argument reader falls back to the
/// expression reading for that case, and it is the more useful reading of the two.
fn a_parameter_list_is_the_type(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::LeftParen {
        return false;
    }

    let kinds = p.peek_token_kind_at(0..64);

    if kinds.get(1) == Some(&CppTokenKind::RightParen) {
        return p
            .last_consumed_token_kind()
            .is_some_and(is_type_specifier_keyword);
    }

    // **Every** element has to start like a parameter, not just the first one, and that is what tells the two
    // readings of a parenthesised group apart when the group's first token is a name:
    //
    // ```text
    // new T(int, char)   a function type, spelled with its parameter list
    // new T(a, *q)       an allocation of `T` initialised with `(a, *q)` — `*q` is not a parameter
    // ```
    //
    // A parameter is a type and then a declarator, so nothing but a type can follow a `(` or a top-level `,`. The
    // first version of this predicate asked only about the first token, so `a` decided for the whole group: the
    // group was read as a parameter list, `*q` had no type, and the allocation came out as
    // `expected a type specifier` against the `*` — measured in `bits/uses_allocator.h`,
    // `bits/node_handle.h` and `memory_resource.h` of the standard-library closure.
    let mut depth = 0isize;

    for (offset, kind) in kinds.iter().enumerate() {
        let starts_an_element =
            offset == 1 || (depth == 1 && kinds.get(offset - 1) == Some(&CppTokenKind::Comma));

        if starts_an_element && !begins_a_parameter_type(*kind) {
            return false;
        }

        // **A token only an expression has** means the group is an initializer, not a parameter list — and the
        // element-start check above cannot see it, because it only looks at each element's *first* token:
        //
        // ```text
        // ::new (std::__addressof(_M_alloc)) _NodeAlloc(__nh._M_alloc.release());   // bits/node_handle.h:157
        // new T(x + 1)          an allocation initialised with `x + 1`
        // ```
        //
        // `(__nh._M_alloc.release())` begins with a name, so every element started like a parameter, the group was
        // claimed as a *function type*, and the parameter reader then met the `.` and reported `expected ), but get
        // .` — while the parentheses are the allocation's own **initializer**, which is what
        // `parse_new_initializer` reads once the type stops where it should.
        //
        // `-` is deliberately **not** in the list: `= -1` is a default argument, which a parameter list may hold.
        // Nothing here can cost a real parameter list much either way — this predicate is only asked in a type-id,
        // where a parameter has no name and a default argument has no meaning.
        if depth == 1
            && matches!(
                kind,
                CppTokenKind::Dot
                    | CppTokenKind::Arrow
                    | CppTokenKind::Plus
                    | CppTokenKind::Slash
                    | CppTokenKind::Percent
                    | CppTokenKind::Pipe
                    | CppTokenKind::Caret
                    | CppTokenKind::Tilde
                    | CppTokenKind::LogicalOr
                    | CppTokenKind::Equal
                    | CppTokenKind::NotEqual
                    | CppTokenKind::Question
            )
        {
            return false;
        }

        match kind {
            CppTokenKind::LeftParen | CppTokenKind::LeftBracket | CppTokenKind::LeftBrace => depth += 1,
            CppTokenKind::RightParen => {
                depth -= 1;

                if depth == 0 {
                    // The group closed, and every element began like a parameter.
                    return true;
                }
            }
            CppTokenKind::RightBracket | CppTokenKind::RightBrace => depth -= 1,
            CppTokenKind::Eof | CppTokenKind::None => return false,
            _ => {}
        }
    }

    // Unterminated within the lookahead: not a group this rule can claim.
    false
}

/// Can a **parameter** begin with this token?
///
/// A parameter is a type and then a declarator, so this is the set of tokens a type may begin with — the same set
/// the first element has always been judged by, plus `...` for `void f(...)`, which is a parameter list and
/// nothing else.
fn begins_a_parameter_type(kind: CppTokenKind) -> bool {
    is_type_specifier_keyword(kind)
        || matches!(
            kind,
            CppTokenKind::Identifier
                | CppTokenKind::Scope
                | CppTokenKind::ConstKeyword
                | CppTokenKind::VolatileKeyword
                | CppTokenKind::Ellipsis
        )
}

/// Consume any `const` / `volatile` — or an implementation qualifier — immediately following a pointer,
/// reference or pointer-to-member operator.
///
/// The second half is the same rule as the specifier position, one token later: `int *__cdecl _errno(void);` and
/// `const char * __restrict__ _Src` both write the keyword *after* the `*`, because the `*` belongs to the
/// return or element type. `__cdecl` there is a calling convention rather than a qualifier, and it is accepted
/// here for the same reason: nothing else can stand in this position, and refusing it loses the declaration.
fn eat_cv_qualifiers(p: &mut CppParser) {
    loop {
        if matches!(
            p.current_token(),
            CppTokenKind::ConstKeyword | CppTokenKind::VolatileKeyword
        ) {
            p.bump();
            continue;
        }

        if an_implementation_keyword(p).is_some() {
            parse_an_implementation_keyword(p);
            continue;
        }

        // **An attribute** in the same position, which is the third thing written there and the one this rule was
        // missing:
        //
        // ```cpp
        // extern __inline void * __attribute__((__gnu_inline__, __always_inline__, __artificial__))
        // __slwpcb (void) { … }                      // lwpintrin.h:43 — the whole family of `*intrin.h` files
        // ```
        //
        // The doc above already says this is "the same rule as the specifier position, one token later", and an
        // attribute is one of the things written in the specifier position — so it belongs here for the reason
        // `__cdecl` does: the attribute belongs to the return type, and nothing else can stand between the `*` and
        // the declarator's name (the name cannot be `__attribute__`).
        //
        // What it cost to leave out is not a diagnostic but a **silent wrong tree**: `void * __attribute__((x)) f;`
        // came out as a declarator *named* `__attribute__` and initialised with `(x)`, with `f` standing after it
        // as a macro — lossless, well-formed, no `ErrorNode` — and the same shape with a body reported
        // ``a declarator takes only one initializer`` at the `{`, because the declarator already had one.
        if at_an_attribute(p) {
            let checkpoint = p.checkpoint();
            if parse_attribute_specifier(p).is_err() {
                // An unterminated `[[` is not this shape: give the tokens back and let the declarator report
                // whatever it reported before — the same shape [`super::decls::eat_a_macro_suffix`] uses.
                p.rollback(checkpoint);
                return;
            }
            continue;
        }

        // **A macro that expands to nothing** stands here too, and `basetsd.h` is where it is written:
        // `#define POINTER_32` is empty on a 64-bit target, and the file uses it exactly where a pointer qualifier
        // goes — `void *POINTER_32 PtrToPtr32 (const void *p)` and the cast `(void *POINTER_32) p`. Nothing is what
        // it stands for, so skipping it is not a guess; a name nobody has a body for is not claimed at all, and the
        // body has to be **known** empty rather than merely unknown.
        if p.current_token() == CppTokenKind::Identifier
            && p.macro_body_kinds_at(p.current_token_text(), p.current_token_range().start_offset)
                .is_some_and(|kinds| kinds.is_empty())
        {
            let macro_call = p.mark(CppSyntaxKind::MacroCall);
            let name = p.mark(CppSyntaxKind::NameExpr);
            p.bump();
            name.complete(p);
            macro_call.complete(p);
            continue;
        }

        return;
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
    // `(C::*)` — the same operator with its class name written out, and no name after it: an *unnamed* parameter
    // of member-pointer-to-function type, `void h(int (C::*)(int));`. The named spelling `(C::*h)` never reaches
    // here — [`a_parenthesised_declarator_with_a_name_follows`] claims it first.
    || pointer_to_member_operator_length(p, 1).is_some()
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

    if matches!(
        p.peek_token_kind_at(1..4).as_slice(),
        [
            CppTokenKind::Star | CppTokenKind::Ampersand | CppTokenKind::LogicalAnd,
            CppTokenKind::Identifier,
            CppTokenKind::RightParen
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
    // error moved from column 16 to column 51 without the file reading (B98, `docs/grammar-gaps.md`). What was
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

    if matches!(
        p.peek_token_kind_at(1..4).as_slice(),
        [
            CppTokenKind::Identifier,
            CppTokenKind::Star | CppTokenKind::Ampersand | CppTokenKind::LogicalAnd,
            CppTokenKind::Identifier
        ]
    ) && p.peek_token_kind_at(4..5) == [CppTokenKind::RightParen]
        && written_like_a_macro(p.peek_token_text_at(1))
    {
        return true;
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
    p.peek_token_kind_at(after..after + 2).as_slice()
        == [CppTokenKind::Identifier, CppTokenKind::RightParen]
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
    if a_bare_template_id_is_here(p) {
        p.close_marks_above(base);
        return Err(CppParseError::syntax_error_from(
            "a declarator's name cannot have template arguments",
            p.current_token_range(),
        ));
    }

    let named = named_inside_parentheses
        || matches!(
            p.current_token(),
            CppTokenKind::Identifier
                | CppTokenKind::Scope
                | CppTokenKind::OperatorKeyword
                | CppTokenKind::Tilde
        );
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
        || super::decls::the_head_of_the_declaration_is_qualified(p)
        || super::decls::a_declaration_is_the_better_reading(p, declarator_from)
        // …or a **macro invocation used as a definition**, which is the one shape with no answer to the
        // declaration/expression question at all: `TEST(FormatPerformance, 1k_row) { … }`. Its arguments are the
        // macro's tokens, so `the_arguments_look_like_declarators` says no and the loop would never open — which
        // is exactly how `TEST(A, B) { }` came to work while `TEST(A, 1) { }` did not.
        || super::decls::a_macro_definition_follows(p, declarator_from)
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
                // with [`super::decls::finish_init_declarator`], which owns it. The follower is what tells them
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
    // B72 and B73 use, one level down: there the name stood between the type and the declarator, here *inside* the
    // parentheses, before the name.
    //
    // `written_like_a_macro` keeps it from firing on a pair that is merely broken code: `(x y)` is still read the
    // way it was, and only a name spelled the way a macro is spelled takes this reading.
    if p.current_token() == CppTokenKind::Identifier
        && p.peek_next_token() == CppTokenKind::Identifier
        && written_like_a_macro(p.current_token_text())
    {
        let m = p.mark(CppSyntaxKind::MacroCall);
        let name = p.mark(CppSyntaxKind::NameExpr);
        p.bump();
        name.complete(p);
        m.complete(p);
    }

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

    // **How many conditionals were open when this list began** — see the note on the same comparison in
    // `decls::parse_parameter_list`, which is where the two spellings it separates are written out. In short: a
    // list whose *tail* is written per branch must not be confused with a declaration that is written per branch.
    // The count comes from the directive rule, because a nested reading may consume a directive this list never
    // sees.
    let conditionals_when_the_list_began = p.open_conditionals();

    while !p.is_eof() {
        // A **`>>` standing where an argument would begin** is two closers, and the first of them closes this
        // list. That is the *empty* argument list: `std::less<>` inside `std::map<K, V, std::less<>>`.
        //
        // The split has to happen before the argument is read. Everywhere else it happens on the way *out* — the
        // argument is read, and then `split_closing_angle` in the `match` below turns the trailing `>>` into a
        // lone `>` — which is why the non-empty spelling `std::map<K, std::less<int>>` has always worked while
        // the empty one reported `expected a template argument` against the `>>`: there was nothing to read
        // first, so the loop never reached the split.
        if p.current_token() == CppTokenKind::RightShift {
            split_closing_angle(p);
        }

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
        // `split_closing_angle` has already turned any `>>` into a lone `>` by the time we look, so
        // this is the only place the closer is consumed.
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
                && let Some(writes_the_terminator) = super::decls::a_branch_continues_the_tail(p)
            {
                if writes_the_terminator {
                    p.bump(); // this branch's `;`
                    p.note_the_terminator_came_from_a_branch();
                    read_a_branch_of_the_tail = true;
                }

                while p.current_token() == CppTokenKind::Hash {
                    super::stats::parse_preprocessor_directive(p)?;
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
                            "expected a template argument",
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

                split_closing_angle(p);
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

            if let Err(err) = super::stats::parse_preprocessor_directive(p) {
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

            if let Err(err) = super::stats::parse_preprocessor_directive(p) {
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

/// Parse one template argument: a type, a template-id, or a constant expression.
///
/// Returns `Ok` with an empty marker when the type reading applied: the argument *was* the type, and
/// the cursor is already on the delimiter that ends it. `Err` means neither reading worked.
fn parse_template_argument(p: &mut CppParser) -> ParseResult {
    let checkpoint = p.checkpoint();
    let start = p.current_token_index();

    let type_read = parse_type_id(p);

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
    if p.current_token_index() > start
        && type_read.is_ok()
        && !stopped_at_a_group_that_is_not_a_parameter_list
        && !a_brace_makes_it_a_value
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
    super::exprs::parse_assignment_expr(p)
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
            // A **macro** among the suffixes, which is where libstdc++ puts one: `void f() _GLIBCXX_NOEXCEPT`,
            // `T* addressof(T&) _GLIBCXX_NOEXCEPT`, `… const _GLIBCXX_NOEXCEPT`. A name here has no other
            // reading — the two contextual keywords are the arm above, and `requires` is refused inside — so the
            // shape is decisive: see [`super::decls::eat_a_macro_suffix`].
            CppTokenKind::Identifier if super::decls::eat_a_macro_suffix(p) => {}
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
enum ImplementationKeyword {
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
/// reads on purpose (see [`super::decls::eat_a_macro_suffix`]). Well formed, lossless, no diagnostic, and every
/// name in it wrong. That is the class of defect `docs/grammar-gaps.md` opens with, and the reason the second
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
pub(super) fn at_an_implementation_keyword(p: &CppParser) -> bool {
    an_implementation_keyword(p).is_some()
}

fn an_implementation_keyword(p: &CppParser) -> Option<ImplementationKeyword> {
    if p.current_token() != CppTokenKind::Identifier {
        return None;
    }

    Some(match p.current_token_text() {
        "__restrict" | "__restrict__" => ImplementationKeyword::Qualifier,
        "__forceinline" => ImplementationKeyword::Means(CppSyntaxKind::InlineSpec),
        "__cdecl" | "__stdcall" | "__fastcall" | "__thiscall" | "__vectorcall" | "__extension__"
        | "__unaligned" | "__ptr64" | "__ptr32" | "__w64" => ImplementationKeyword::Bare,
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
fn a_type_the_compiler_spells(p: &CppParser) -> bool {
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
/// A **spelling** test rather than a table one, and that is not the convention-without-evidence that
/// `docs/grammar-gaps.md` entry 16 warns about. Two things make it evidence: the standard reserves these names to
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
        "unterminated attribute specifier",
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
fn parse_word_attribute(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::AttributeList);

    // The name is read as a `NameExpr`, like any other name in the tree, so that the spelling stays visible and
    // a consumer can tell `__declspec` from `__attribute__` without counting parentheses.
    let name = p.mark(CppSyntaxKind::NameExpr);
    p.bump();
    name.complete(p);

    if let Err(err) = super::decls::parse_balanced_token_group(p, CppSyntaxKind::ArgumentList) {
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
pub(super) fn parse_an_implementation_keyword(p: &mut CppParser) -> CompleteMarker {
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
            p.bump();
            CompleteMarker::empty()
        }
    }
}

