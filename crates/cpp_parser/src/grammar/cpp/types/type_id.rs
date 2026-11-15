//! **Type id** — type-ids: `typeof(…)`, `new T`, a cast's type, and the builtin names spelled as names.
//!
//! Moved here from `types.rs` without a change.

use super::*;

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
pub(crate) fn storage_or_function_specifier(kind: CppTokenKind) -> Option<CppSyntaxKind> {
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

/// **`typename [: r :]` in expression position** — the type a functional cast or a temporary names.
///
/// The type half of the same construct, and a second entry point rather than one shared rule because the caller's
/// answer differs: in a declaration the specifier loop also has to say whether a *name* may follow, while in an
/// expression the splice **is** the expression and what follows it is a postfix.
///
/// ```cpp
/// auto t = typename [: ^^Widget :]{5};      // a temporary of the reflected type
/// auto u = typename [: r :](1);             // a functional cast to it
/// auto v = typename [: ^^Box :]<int>{};     // …of its specialization
/// ```
///
/// Verified against GCC 16.2 (`-std=c++26 -freflection`): all three are accepted there, and this parser read
/// `typename` in expression position as a type specifier only — the statement failed at the `{`.
pub(crate) fn parse_a_typename_splice(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TypenameType);
    expect_token(p, CppTokenKind::TypenameKeyword)?;
    if let Err(err) = super::super::exprs::parse_splice_expression(p) {
        p.close_marks_above(base);
        return Err(err);
    }
    // A **splice specialization**: `typename [: r :]<int>` — the reflection, with template arguments applied.
    if p.current_token() == CppTokenKind::Less
        && let Err(err) = parse_template_argument_list(p)
    {
        p.close_marks_above(base);
        return Err(err);
    }
    Ok(m.complete(p))
}
/// Is this name written the way an **unexpanded macro that stands for a type** is written?
///
/// `__int64`, `_Float16`, `__m128h`, `HUGEP`, `MY_API` — the MinGW and libstdc++ spellings, and it is the
/// `_`-leading half that [`super::super::decls::looks_like_a_macro_name`] (all capitals) does not cover. Used in exactly
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
pub(crate) fn written_like_a_macro(name: &str) -> bool {
    name.starts_with('_') || super::super::decls::looks_like_a_macro_name(name)
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
pub(crate) fn written_in_the_implementations_namespace(name: &str) -> bool {
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
pub(crate) fn a_macro_call_begins_the_declaration(p: &CppParser) -> bool {
    if p.current_token() != CppTokenKind::Identifier
        || p.peek_next_token() != CppTokenKind::LeftParen
        || at_an_attribute(p)
    {
        return false;
    }

    // The `(` is the next significant token, and the scan counts parentheses from there. An unbalanced group, or a
    // `;` before it closes, means this is not the shape.
    let mut index = super::super::decls::next_significant_index(p, p.current_token_index());
    let mut depth = 0isize;
    while index < p.token_count() {
        match p.token_kind_at(index) {
            CppTokenKind::LeftParen => depth += 1,
            CppTokenKind::RightParen => {
                depth -= 1;
                if depth == 0 {
                    let after = super::super::decls::next_significant_index(p, index);
                    // `V() requires C<T>`: the name after the group is the keyword that begins a constraint, so
                    // what stands before it is a constructor's declarator and not a macro used as a type.
                    return p.token_kind_at(after) == CppTokenKind::Identifier
                        && p.token_text_at(after) != "requires";
                }
            }
            CppTokenKind::Semicolon | CppTokenKind::Eof | CppTokenKind::None => return false,
            _ => {}
        }
        index += 1;
    }

    false
}

/// Is the cursor on a `requires` that begins a constraint right after a parameter list — `V() requires C<T>`?
///
/// The shape has no competitor: a variable called `requires` is not written after a `)` and followed by something that
/// reads as a constraint, so the clause is tried and what it consumed is the answer.
pub(crate) fn has_a_clause_after_a_parameter_list(p: &mut CppParser) -> bool {
    p.current_token() == CppTokenKind::Identifier
        && p.current_token_text() == "requires"
        && p.last_consumed_token_kind() == Some(CppTokenKind::RightParen)
        && super::super::decls::starts_a_requires_clause(p)
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
    parse_type_id_here(p, a_name_may_be_a_type, true, true)
}

/// [`parse_type_id`] for a `new`, where the **array bounds are not the type's**.
///
/// `new int[4]` is the case: the standard puts the `[4]` in the *new-declarator* rather than in the type, and the
/// rule that reads those bounds is [`super::super::exprs`]' own
/// [`crate::grammar::cpp::exprs::parse_new_declarator_suffixes`]. A type-id that swallowed them would move the
/// `ArrayType` node out of the allocation's declarator and into its `TypeId` — a shape that no consumer of a `new`
/// expression expects, and one nothing would report.
///
/// `a_name_may_be_a_type` is `false` for the same call, and for its own reason: the parentheses before the type may
/// be a **placement list**. See [`parse_type_id_with`].
pub fn parse_type_id_for_an_allocation(p: &mut CppParser) -> ParseResult {
    // The third `false` is the whole reason this entry exists: after the allocated type, a `(` is the
    // **initializer** (`new T(::x)`, `new Widget(1, 2)`) and never a parameter list belonging to the type.
    // Reading it as one is what made `new T(::x)` report ``expected ), but get ::`` — the `(::x)` was taken for
    // the parameter list of a function type returning `T`, and `::x` read as a parameter. The same shape with a
    // parenthesised *type* (`new (Widget)(1)`) is claimed earlier, by the branch that reads a type beginning with
    // a parenthesis, so nothing that belongs to the type is lost here.
    parse_type_id_here(p, false, false, false)
}

/// Open the `TypeId` node and read the type in it, with the caller's answers to the two questions that decide how
/// far it reaches.
/// **Consume tokens up to the `)` that closes the parenthesis the cursor is inside.**
///
/// Used by the `decltype`/`noexcept` payload, whose end is a `)` however much of the body the expression rule
/// understood. Nested parentheses are counted so that `decltype(f(a, b))` stops at the outer one, and an
/// unbalanced input stops at end of file, which is the honest reading of text that has no closer.
///
/// The skipped tokens are wrapped in an [`CppSyntaxKind::ErrorNode`] so that they belong to something: a token no
/// rule claimed is what an error node is for, and leaving them bare would attach them to whatever encloses this
/// specifier.
pub(crate) fn skip_to_the_closing_paren(p: &mut CppParser) {
    let error = p.mark(CppSyntaxKind::ErrorNode);
    let mut depth = 0isize;
    while !p.is_eof() {
        match p.current_token() {
            CppTokenKind::LeftParen => depth += 1,
            CppTokenKind::RightParen => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
        p.bump();
    }
    error.complete(p);
}

pub(crate) fn parse_type_id_here(
    p: &mut CppParser,
    a_name_may_be_a_type: bool,
    read_array_suffixes: bool,
    read_a_parameter_list_as_the_type: bool,
) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TypeId);

    if let Err(err) = parse_type_id_inner(
        p,
        a_name_may_be_a_type,
        read_array_suffixes,
        read_a_parameter_list_as_the_type,
    ) {
        // **`end_marks_to`, not `close_marks_above`** — and the difference is a whole declaration, not a
        // tidy-up.
        //
        // `close_marks_above` **detaches** the nodes above `base` without emitting their `NodeEnd`s, on the
        // understanding that whoever owns them will emit one at its own `complete()`. That holds when the
        // failure abandoned the tokens. It does **not** hold here: this rule fails *after consuming them*, so
        // the `TypeId`, the `DeclSpecifierSeq` and the `BuiltinType` under it are left with a `NodeStart` and
        // no end, and the tree builder balances each one at the **end of the stream**. Every token after the
        // failure is then nested inside them — and inside whatever enclosed them, which is the declaration
        // being read.
        //
        // Measured on two lines, whose event stream says exactly this:
        //
        // ```cpp
        // using d = decltype(_STD x);
        // struct After { int c; };   // a `Declaration` in the stream, absent from the tree
        // ```
        //
        // ```text
        //  1: NodeStart UsingDecl
        // 10: NodeStart TypeId            ← no NodeEnd ever
        // 11: NodeStart DeclSpecifierSeq  ← none
        // 12: NodeStart BuiltinType       ← none
        // 23: NodeStart Declaration       ← `struct After`, correct and unreachable
        // ```
        //
        // `_STD` is a macro in the real world — `yvals.h` defines it as `::std::` — and an ordinary identifier
        // to a file read on its own, so `decltype(_STD x)` holds two identifiers in a row. MSVC's
        // `__msvc_iter_core.hpp` line 116 has that shape: the file's first 130 lines summarise to **43**
        // declarations, and so do its first 200 and its first 400.
        p.end_marks_to(base);
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
pub(crate) fn parse_type_id_inner(
    p: &mut CppParser,
    a_name_may_be_a_type: bool,
    read_array_suffixes: bool,
    read_a_parameter_list_as_the_type: bool,
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
        super::super::decls::parse_parameter_list(p)?;
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
    //
    // **Except the four that are keywords the lexer has no kind for.** `wchar_t`, `char8_t`, `char16_t` and
    // `char32_t` are builtin types — [lex.key] lists every one — but this lexer hands them over as
    // `Identifier`s, so they reach the specifier sequence as *names* and the file's table has never heard of
    // them. Everywhere else that costs nothing, because an unknown name is a perfectly good type: `wchar_t x;`
    // and `wchar_t a[4];` both parse. It costs something **here**, where the answer decides whether an array
    // suffix is read at all: measured on MSVC's `<filesystem>`, whose `unique_ptr<wchar_t[]>` came out with the
    // argument ending at `wchar_t` and the `[]` left for the list to trip over —
    // ``expected `;` `` at the `>` of `unique_ptr<wchar_t[]>` and ``expected primary expression`` at the same
    // `[]` in the definition below it. The same spelling with `int` is read, because `int` has a token kind.
    let the_type_is_known = p.events_contain_any(specifiers_from, &[CppSyntaxKind::BuiltinType])
        || p.declaration_type_name().is_some_and(|name| {
            p.is_a_known_type_name(name) || a_builtin_type_spelled_as_a_name(name)
        });

    // An abstract declarator: pointers, references and cv-qualifiers, but no name.
    //
    // No name may appear — this is a type-id, not a declarator — which is what makes `(int)` after it a
    // function type rather than somebody's parameter list. See [`parse_abstract_declarator`].
    parse_abstract_declarator_with(p, false, read_a_parameter_list_as_the_type)?;

    // An **array suffix**: the `[4]` of `int[4]`, the `[2][3]` of `int[2][3]`, the `[4]` of `int*[4]`.
    //
    // It is read here rather than in the abstract declarator because that rule is shared with the *declarator*
    // path, where a `[` after the specifiers is a **structured binding** (`auto [a, b] = pair`) rather than a
    // bound. A type-id has no such reading: `int[4]` is a type and nothing else can be meant by it.
    if read_array_suffixes && !super::super::decls::the_declaration_is_a_placeholder(p) {
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
/// Is this spelling one of the **builtin types the lexer has no token kind for**?
///
/// `wchar_t`, `char8_t`, `char16_t` and `char32_t` are keywords of the language ([lex.key]) and this lexer
/// hands every one of them over as an `Identifier`, so the only place that can tell them from a variable's name
/// is a reader that looks at the spelling. [`the_type_is_known`] is that reader; see its note for what the
/// difference cost.
///
/// Deliberately a short, closed list rather than a general "looks like a type" test: a name this file has not
/// declared really can be a variable, and widening that answer is how a declaration reading starts claiming
/// expressions.
pub(crate) fn a_builtin_type_spelled_as_a_name(name: &str) -> bool {
    matches!(name, "wchar_t" | "char8_t" | "char16_t" | "char32_t")
}

/// An empty bound is legal (`int[]`) and so is a run of them (`int[2][3]`).
///
/// # An empty bound is a type whoever stands in front of it
///
/// `the_type_is_known` is evidence from a **linear** scan — a name this file has *already* declared — and a file may
/// perfectly well declare a type **below** the function that uses it. `X[]` is where that shows: it is not an
/// expression at all, because an empty subscript is not legal C++, so the question the flag answers does not arise
/// for it, and asking it anyway cost the declaration reading of every forward-referenced type.
///
/// Measured, and it was the **last parse error in the standard library's cook** (184 files, every other one clean):
/// MSVC's `chrono` writes
///
/// ```cpp
/// unique_ptr<__std_tzdb_leap_info[], _Tzdb_deleter<__std_tzdb_leap_info[]>> _Reg_ls_data{ … };
/// ```
///
/// in a function that stands *above* `struct __std_tzdb_leap_info`. The array suffix was refused, the type-id ended
/// at the name, the declaration reading failed, the statement fell back to an expression — `unique_ptr <
/// __std_tzdb_leap_info [] …` — and the `[]` became an empty subscript: `expected primary expression` at the `]`.
/// Reduced to seven lines with the struct moved below the function, which is the repro that found it.
///
/// A **non-empty** bound keeps the old rule: `a[0]` really is an index far more often than it is a type, and that is
/// the direction the flag exists for.
pub(crate) fn read_array_suffixes_of_a_type_id(
    p: &mut CppParser,
    the_type_is_known: bool,
) -> ParseResult {
    while p.current_token() == CppTokenKind::LeftBracket
        // `[[` is an attribute, not a bound — the declarator's own loop asks the same question.
        && p.peek_next_token() != CppTokenKind::LeftBracket
    {
        let empty_bound = p.peek_next_token() == CppTokenKind::RightBracket;
        if !the_type_is_known && !empty_bound {
            return Ok(CompleteMarker::empty());
        }

        let array = p.mark(CppSyntaxKind::ArrayType);
        p.bump(); // `[`

        if p.current_token() != CppTokenKind::RightBracket && !p.is_eof() {
            super::super::exprs::parse_expr(p)?;
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
pub(crate) fn a_parameter_list_follows(p: &CppParser) -> bool {
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
pub(crate) fn starts_a_function_type(p: &CppParser, a_name_may_be_a_type: bool) -> bool {
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
/// own `#define` or from the include closure, which is the channel opened for exactly this kind of reading.
///
/// **An empty body is accepted only in a type-id** (`at_a_type_id`), and that boundary is measured rather than
/// argued: `_mingw.h:376` writes `#define _CONST_RETURN` with nothing after it (its `const` spelling is in the
/// other branch), so `(_CONST_RETURN wchar_t *)(_S)` needs the name to stand for nothing — but accepting empty
/// bodies in a **declaration**'s specifier sequence swallowed the declarator's own name and took the corpus from
/// 435 clean to **424** with 617 messages. A type-id has no declarator name to lose, so there the empty answer is
/// safe; a declaration has one.
/// The tokens a declaration-specifier list may be made of — the body test of
/// [`a_specifier_follows_the_group_reading_ahead`].
pub(crate) fn is_a_specifier_kind(kind: CppTokenKind) -> bool {
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
pub(crate) fn is_a_type_specifier_kind(kind: CppTokenKind) -> bool {
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
pub(crate) fn a_specifier_follows_the_group(p: &CppParser) -> bool {
    a_specifier_follows_the_group_at(p, 0)
}

/// The same question asked from the **name** `offset` tokens past the cursor.
///
/// One question, two entry points, because the run of annotations is read as a run: `Wat(1) Wobble(2) char*
/// x` has a *name* after the first group, and the name carries a group of its own — so the answer for the first
/// group is the answer for the second. Recursing is what tells the two readings apart without a second rule:
///
/// ```text
/// Wat(1) Wobble(2) char* x        the second group is followed by a specifier  → both are specifiers
/// WINOLEAPI_(HINSTANCE) CoFreeLibrary (HINSTANCE h)   …followed by `;`          → the head, and the `(…)` is
///                                                                                the declarator's parameter list
/// ```
///
/// The second shape must keep its reading — it is the measurement that put the name follower out of the simple
/// case — and it does, because the question asked from `CoFreeLibrary` finds `;` after *its* group.
pub(crate) fn a_specifier_follows_the_group_at(p: &CppParser, name_offset: usize) -> bool {
    // Remembered by where the name is, because the run recurses: see `CppParser::follower_memo`.
    let at = p.current_token_index() + name_offset;
    if let Some(answer) = p.remembered_follower(at) {
        return answer;
    }
    let answer = a_specifier_follows_the_group_reading_ahead(p, name_offset);
    p.remember_follower(at, answer);
    answer
}

pub(crate) fn a_specifier_follows_the_group_reading_ahead(
    p: &CppParser,
    name_offset: usize,
) -> bool {
    let mut depth = 0isize;

    for (position, kind) in p
        .peek_token_kind_at(name_offset + 1..name_offset + 64)
        .into_iter()
        .enumerate()
    {
        // Offsets are relative to the cursor, which is on the macro's **name**: one past it is the `(` that opens
        // its argument list.
        let offset = name_offset + 1 + position;

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
                            match p.peek_token_kind_at(after + 1..after + 2).first().copied() {
                                // A **name with a group of its own** after the group: either the next
                                // annotation of the same run or the declarator's head. Asking the same
                                // question from there is what tells them apart — see
                                // [`a_specifier_follows_the_group_at`].
                                Some(CppTokenKind::LeftParen) => {
                                    a_specifier_follows_the_group_at(p, after)
                                }
                                // Anything else means the sequence has not reached its declarator yet.
                                _ => true,
                            }
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

// Is the name `offset` tokens past the cursor a macro whose **whole body** is a declaration-specifier list?
//
// **Gone with the macro table.** The body was the evidence and the grammar has none: the text it reads has been
// preprocessed, so a name in it is a name. See [`a_specifier_follows_the_group`], which keeps the shape half of the
// question and is what the run-of-annotations reading runs on now.

/// Does a **parameter list** follow the run of `[[…]]` attributes at the cursor?
///
/// The follower test of the declarator's suffix loop, which reads an attribute standing between a declarator's
/// name and its parameters — `bool operator== [[nodiscard]] (const tuple<_Tps...>&, …)` — and leaves every other
/// attribute to the rule that owns that position. A `(` after the run can only be this declarator's parameter
/// list, so the test is a scan rather than a preference: skip the attribute run (counting brackets, because an
/// attribute's argument may contain `[`), and look at what stands after it.
pub(crate) fn a_parameter_list_follows_the_attributes(p: &CppParser) -> bool {
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
