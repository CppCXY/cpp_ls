//! `concept` declarations, requires-clauses and requires-expressions.
//!
//! The largest single block of missing grammar, and the last one that was *loud*: `requires` had been listed in
//! `is_expression_keyword` since the keyword table was written — a promise that some rule consumes the token —
//! while no rule consumed it, so every constraint ended in `expected primary expression`.
//!
//! What makes it one block rather than four is that the same word introduces four different things, and each is
//! nested in the others:
//!
//! ```text
//! template <C T> void f();                             a constrained template parameter
//! template <typename T> requires C<T> void f();        a requires-clause, after the parameter list
//! template <typename T> void f(T t) requires C<T> { }  the same clause, after the declarator
//! template <typename T> concept C = requires(T t) { }  a concept, whose constraint is a requires-expression
//! ```
//!
//! The shapes are pinned rather than just "it parses", because the clause's *extent* is the thing that silently
//! goes wrong: a clause read as ending at its first term leaves a well-formed tree in which the rest of the
//! constraint has been absorbed by whatever follows. That is the A0 failure mode, and it is why the conjunction
//! test below asks where the second template-id ended up.

use cpp_parser::{CppParser, CppSyntaxKind, CppSyntaxTree, ParserConfig};

fn tree(source: &str) -> CppSyntaxTree {
    CppParser::parse(source, ParserConfig::default())
}

/// Parse, and require that the result is clean in **both** senses.
///
/// An empty error list is not enough on its own: the failure this module exists to catch is the quiet one, where
/// every token is present in the tree but no rule claimed it. An `ErrorNode` is how the parser says that out
/// loud, so its absence is checked separately.
fn parses(source: &str) {
    let parsed = tree(source);
    assert_eq!(
        parsed.get_errors(),
        [],
        "{source:?} must parse cleanly, got {:?}",
        parsed.get_errors()
    );

    let unclaimed = parsed.get_red_root().descendants().any(|node| {
        matches!(
            CppSyntaxKind::from(node.kind()),
            CppSyntaxKind::ErrorNode | CppSyntaxKind::MissingNode
        )
    });
    assert!(!unclaimed, "{source:?} leaves an ErrorNode behind");
    assert_eq!(parsed.to_source_text(), source, "{source:?} stays lossless");
}

fn count(source: &str, kind: CppSyntaxKind) -> usize {
    tree(source)
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
        .count()
}

/// The source text of the first node of `kind`, so a test can assert on how far a construct reaches.
fn text_of(source: &str, kind: CppSyntaxKind) -> String {
    tree(source)
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == kind)
        .unwrap_or_else(|| panic!("no {kind:?} in {source:?}"))
        .text()
        .to_string()
}

#[test]
fn a_concept_definition_is_its_own_node() {
    // `concept` is only a keyword here, and the declaration is a declaration: the `=` that follows the name is
    // what separates it from a variable, exactly as it does in `bool b = true`.
    for source in [
        "template <typename T> concept C = true;\n",
        "template <typename T> concept C = sizeof(T) > 4;\n",
        "template <typename T> concept C = (sizeof(T) > 1) && (sizeof(T) < 8);\n",
        "template <typename T> concept C = C2<T> && C3<T>;\n",
        "template <typename T> concept C = requires(T t) { t.f(); };\n",
        "template <typename T> concept C = requires(T t) { t.f(); } && true;\n",
        "template <typename T> concept Addable = requires(T a, T b) { a + b; };\n",
        "template <typename T> concept C = requires { typename T::value_type; };\n",
    ] {
        parses(source);
    }

    let source = "template <typename T> concept C = requires(T t) { t.f(); };\n";
    assert_eq!(count(source, CppSyntaxKind::ConceptDecl), 1);
    assert_eq!(
        count(source, CppSyntaxKind::RequiresExpr),
        1,
        "the constraint is a requires-expression"
    );
    assert_eq!(
        count(source, CppSyntaxKind::ConceptDecl),
        1,
        "one definition, one node"
    );
}

#[test]
fn a_requires_clause_takes_the_whole_constraint() {
    // The clause is not "the first term of the constraint". Reading it that way leaves a tree in which the `&&`
    // and everything after it have been handed to the declaration that follows — well formed, and wrong. The
    // assertion is therefore about *where the second template-id is*, not about whether an error was reported:
    // both template-ids have to be inside the clause.
    let source = "template <typename T> requires C<T> && C2<T> void f(T t);\n";
    parses(source);

    assert_eq!(count(source, CppSyntaxKind::RequiresClause), 1);
    assert_eq!(
        text_of(source, CppSyntaxKind::RequiresClause).trim(),
        "requires C<T> && C2<T>",
        "the clause reaches to the end of the constraint"
    );

    let clause = tree(source)
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::RequiresClause)
        .expect("a clause");
    assert_eq!(
        clause
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TemplateArgumentList)
            .count(),
        2,
        "both `C<T>` and `C2<T>` are arguments of the clause"
    );
}

#[test]
fn a_requires_clause_is_read_in_every_position_the_standard_allows() {
    // Two positions, one rule, and the distinction between them is a grammar fact rather than a style:
    //
    //     template-head:     template < template-parameter-list > requires-clause_opt        [temp.pre]
    //     init-declarator:   declarator requires-clause function-contract-specifier-seq_opt  [dcl.decl.general]
    //
    // The second is why a clause follows a *trailing return type* instead of preceding it, and why it is
    // restricted to declarators of **templated functions** ([dcl.decl.general]/5).
    for source in [
        // The **template head**, before whatever the head introduces — a function, a class, an alias, a variable,
        // or a concept definition's own constraint.
        "template <typename T> requires C<T> void f(T t);\n",
        "template <typename T> requires C<T> void f(T t) { }\n",
        "template <typename T> requires C<T> struct S { };\n",
        "template <typename T> requires C<T> class C2 { };\n",
        "template <typename T> requires C<T> && C2<T> void f(T t);\n",
        "template <typename T> requires (C<T>) void f(T t);\n",
        "template <typename T> requires C<T> using Alias = T;\n",
        "template <typename T> requires C<T> T value = T{};\n",
        "template <typename T> requires C<T> concept D = true;\n",
        // After the declarator of a **templated function**, with and without a trailing return type — and the
        // clause comes *after* the `-> T`, which is the half a reader is most likely to get wrong.
        "template <typename T> void f(T t) requires C<T>;\n",
        "template <typename T> void f(T t) requires C<T> { }\n",
        "template <typename T> void f(T t) requires C<T> && C2<T> { }\n",
        "template <typename T> T g(T t) requires C<T> { return t; }\n",
        "template <typename T> auto g(T t) -> int requires C<T>;\n",
        "template <typename T> auto g(T t) -> int requires C<T> { return 1; }\n",
        // A member of a class template is a templated function too, so its declarator may carry one.
        "template <typename T> struct S { void f(T t) requires C<T>; };\n",
        "template <typename T> struct S { void f(T t) requires C<T> { } };\n",
        // The constraint may itself be a requires-expression.
        "template <typename T> void f(T t) requires requires(T t) { t.f(); };\n",
        "template <typename T> requires requires(T t) { t.f(); } void f(T t);\n",
    ] {
        parses(source);
    }
}

#[test]
fn a_requires_clause_is_read_more_widely_than_the_standard_allows() {
    // Deliberate tolerance, pinned so that it is a decision rather than an accident. [dcl.decl.general]/5
    // restricts a trailing clause to declarators of **templated functions**, so a non-templated one is
    // ill-formed — and it is read:
    //
    //     void f() requires true;         a non-templated function
    //
    // It is read because the tokens say exactly what the author meant, and because being permissive here cannot
    // produce a *wrong* tree: a clause on a function declarator is followed by a body or a `;`, and no other
    // reading of those tokens exists. A language server that rejected it would lose a declaration it can see
    // perfectly well.
    for source in [
        "void f() requires true;\n",
        "void f() requires C<T> && C2<T>;\n",
        "void f() requires (sizeof(T) > 1) { }\n",
        "struct S { void f() requires true; };\n",
    ] {
        parses(source);
    }

    // The one placement that is **refused** is a clause after a class head, and the reason is not pedantry: the
    // grammar gives a class head no clause, and reading one used to *detach the class body*. The `{ }` after the
    // constraint was left for the statement rule, so
    //
    //     template <typename T> struct S requires C<T> { };
    //
    // came out as a struct declaration followed by a `CompoundStat` at file scope — well formed, lossless, no
    // diagnostic, and the class's members belonging to nothing. That is an A0-class wrong tree, and reporting
    // `expected ;` against the `requires` is both what the standard says and the only reading that keeps the
    // body where it belongs.
    let refused = "template <typename T> struct S requires C<T> { };\n";
    assert!(
        !tree(refused).get_errors().is_empty(),
        "{refused:?} is not valid C++ and must be reported, not read"
    );

    // And the two spellings that are refused for their *order* rather than their placement: the clause comes
    // after a trailing return type, never before it. The standard names this one as an error in the same
    // breath as the rule — `template<typename T> auto f3(T a) requires true -> bool; // error: requires-clause
    // precedes trailing-return-type`.
    let refused = "template <typename T> auto g(T t) requires C<T> -> int;\n";
    assert!(
        !tree(refused).get_errors().is_empty(),
        "{refused:?} is not valid C++ and must be reported, not read"
    );
}

#[test]
fn a_refused_clause_leaves_the_class_body_with_the_class() {
    // The shape behind the refusal above, asserted directly because the wrong version reported nothing at all.
    // Whatever is reported, the body must not be handed to the statement rule: a `{ }` at file scope is not a
    // statement, and a class without its members is the kind of wrong tree no diagnostic list can see.
    let source = "template <typename T> struct S requires C<T> { int member; };\n";
    let parsed = tree(source);

    assert!(
        parsed
            .get_red_root()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::StructDef),
        "the class is still a class"
    );
    assert_eq!(
        parsed
            .get_red_root()
            .children()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CompoundStat)
            .count(),
        0,
        "the body was not left at file scope as a compound statement"
    );
}

#[test]
fn a_constraint_keeps_its_template_id_when_a_declaration_follows() {
    // The `<` in a clause opens a template-id, and the token *after* it cannot decide the reading the way it does
    // in an expression: a clause is followed by the declaration it constrains, and that declaration begins with a
    // type — an identifier like `T`, or a qualified name like `std::vector<int>`. An "an operand follows" test
    // reads both as stray operands, gives `C<T>` back, and then lets the comparison eat the declaration's own type
    // as its right operand.
    //
    // So the operand rule is suspended inside a clause. These are the shapes where it must be.
    for source in [
        "template <typename T> requires C<T> T value = T{};\n",
        "template <typename T> requires C<T> std::vector<int> values;\n",
        "template <typename T> requires C<T> T f();\n",
        "template <typename T> requires C<T> && D<T> T value = T{};\n",
        "template <typename T> requires C<T> void f();\n",
    ] {
        parses(source);
    }

    assert_eq!(
        count(
            "template <typename T> requires C<T> T value = T{};\n",
            CppSyntaxKind::TemplateArgumentList
        ),
        1,
        "the constraint kept its template-id — the initialiser is a braced one, not a template-id"
    );

    // Parentheses are what tell the two apart, and the clause reads them the same way an expression does: inside
    // them the *constraint* is still going, so an operand there cannot be the declaration that follows the clause.
    // This used to be the boundary of the rule — a comparison inside parentheses in a clause hid behind the
    // `<`…`>` pair — and it is fixed by asking whether parentheses are still open rather than by guessing.
    for source in [
        "template <int N> requires (N < 0 || N > 3) void f();\n",
        "template <int N> requires (N > 0) void f();\n",
        "template <int N> requires (N == 3) void f();\n",
        "template <typename T> requires (C<T> && D<T>) void f(T t);\n",
        "template <typename T> requires (C<T>) T value = T{};\n",
    ] {
        parses(source);
    }

    let compared = "template <int N> requires (N < 0 || N > 3) void f();\n";
    assert_eq!(
        count(compared, CppSyntaxKind::BinaryExpr),
        3,
        "`<`, `||` and `>` are operators, not a template argument list"
    );
    assert_eq!(
        count(compared, CppSyntaxKind::TemplateArgumentList),
        0,
        "and nothing was read as template arguments"
    );
}

#[test]
fn the_two_words_are_ordinary_names_everywhere_else() {
    // `requires` and `concept` are **contextual keywords**: the standard gives them a meaning in particular
    // positions and leaves them as perfectly good identifiers everywhere else. The lexer used to hand both over as
    // keyword tokens, which made every one of these programs unparseable — a variable named `requires` is not an
    // exotic thing to write, and the lexer was telling the grammar something the grammar does not believe.
    //
    // Both now arrive as `Identifier`, and the grammar asks for the *spelling* where the standard gives the word a
    // meaning. Every use below is an ordinary name, and none of them may produce a concept, a clause or a
    // requires-expression.
    for source in [
        "int requires = 1;\n",
        "int concept = 2;\n",
        "void f() { int requires = 1; }\n",
        "void f() { int concept = 2; }\n",
        "void f() { requires = 1; }\n",
        "void f() { concept = 2; }\n",
        "void f() { requires(); }\n",
        "void f() { concept(); }\n",
        "void f() { g(requires); }\n",
        "void f() { g(concept); }\n",
        "void f(int requires);\n",
        "struct S { int requires; int concept; };\n",
        "void f() { requires.requires = 1; }\n",
        "void f() { auto x = requires; }\n",
        "void f() { int concept = requires; }\n",
        "void requires();\n",
    ] {
        parses(source);

        let parsed = tree(source);
        for kind in [
            CppSyntaxKind::ConceptDecl,
            CppSyntaxKind::RequiresClause,
            CppSyntaxKind::RequiresExpr,
        ] {
            assert_eq!(
                parsed
                    .get_red_root()
                    .descendants()
                    .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
                    .count(),
                0,
                "{source:?} has no {kind:?} in it — the word is a name there"
            );
        }
    }

    // And the readings are the ordinary ones, which is the half an error list cannot see: a statement that
    // mentions the name is an *expression*, and a call is a call.
    assert_eq!(
        count("void f() { concept = 2; }\n", CppSyntaxKind::ExpressionStat),
        1,
        "an assignment to a variable named `concept`"
    );
    assert_eq!(
        count("void f() { concept(); }\n", CppSyntaxKind::CallExpr),
        1,
        "a call to a function named `concept`"
    );
    assert_eq!(
        count("void f() { requires(); }\n", CppSyntaxKind::CallExpr),
        1,
        "a call to a function named `requires` — not a clause over a parenthesised constraint"
    );
    assert_eq!(
        count("void f() { g(requires); }\n", CppSyntaxKind::IdentifierExpr),
        2,
        "the callee and the argument, both names"
    );
    assert_eq!(
        count("int requires = 1;\n", CppSyntaxKind::Declaration),
        1,
        "a variable named `requires` is still a declaration"
    );

    // The other direction, in one place: the same spelling where the standard *does* give it a meaning still
    // produces the construct. Without this the test above would pass just as well on a parser that never read a
    // constraint at all.
    assert_eq!(
        count(
            "template <typename T> concept C = true;\n",
            CppSyntaxKind::ConceptDecl
        ),
        1
    );
    assert_eq!(
        count(
            "void f() { auto x = requires { g(); }; }\n",
            CppSyntaxKind::RequiresExpr
        ),
        1
    );
    assert_eq!(
        count(
            "template <typename T> requires C<T> void f(T t);\n",
            CppSyntaxKind::RequiresClause
        ),
        1
    );
    // `requires(x);` is the pair that needs both tests: the spelling is there, and what follows is an argument
    // list rather than a body. The standard reads it as a call, and so does this.
    assert_eq!(
        count("void f() { requires(x); }\n", CppSyntaxKind::RequiresExpr),
        0,
        "a call is not a requires-expression"
    );

    // The word is a name *inside* a constraint too, where the surrounding construct is a clause either way:
    // `requires;` is a simple requirement whose expression is a name, not a clause with no constraint.
    let nested = "void f() { auto x = requires { requires; }; }\n";
    parses(nested);
    assert_eq!(
        count(nested, CppSyntaxKind::RequiresClause),
        0,
        "`requires;` is a requirement, not a clause"
    );
    assert_eq!(count(nested, CppSyntaxKind::Requirement), 1);
}

#[test]
fn a_requires_expression_has_one_node_per_requirement() {
    // Four kinds of requirement, and they are told apart by what follows the first token — which is why each one
    // gets a node of its own rather than a flattened list of expressions.
    let simple = "void f() { auto x = requires(T t) { t.f(); t.g(); }; }\n";
    parses(simple);
    assert_eq!(count(simple, CppSyntaxKind::RequiresExpr), 1);
    assert_eq!(count(simple, CppSyntaxKind::Requirement), 2);

    for source in [
        // A parameter list is optional: `requires { … }` constrains nothing but the syntax inside.
        "void f() { auto x = requires { g(); }; }\n",
        // A type requirement.
        "void f() { auto x = requires { typename T::type; }; }\n",
        // A compound requirement: braces, an optional `noexcept`, an optional return type.
        "void f() { auto x = requires { { g() } -> int; }; }\n",
        "void f() { auto x = requires { { g() } noexcept; }; }\n",
        "void f() { auto x = requires { { g() } noexcept -> int; }; }\n",
        "void f() { auto x = requires { { 1 } -> int; }; }\n",
        // A nested requirement: another constraint inside this one.
        "void f() { auto x = requires { requires C<T>; }; }\n",
        "void f() { auto x = requires(T t) { requires C2<T>; t.f(); }; }\n",
        // A parameter list with a real type in it.
        "void f() { auto x = requires(std::vector<int> v) { v.size(); }; }\n",
        // The three places a requires-expression can be written as an expression.
        "void f() { if constexpr (requires { g(); }) { } }\n",
        "void f() { static_assert(requires { g(); }); }\n",
        "void f() { auto x = requires { 1 + 1; }; }\n",
    ] {
        parses(source);
    }

    // The compound requirement keeps its braces, its exception specification and its trailing return type as
    // children of the requirement — the three parts a consumer reads to answer "what does this demand?".
    let compound = "void f() { auto x = requires { { g() } noexcept -> int; }; }\n";
    for (kind, expected) in [
        (CppSyntaxKind::NoexceptSpec, 1),
        (CppSyntaxKind::TrailingReturnType, 1),
        (CppSyntaxKind::TypeId, 1),
        (CppSyntaxKind::Requirement, 1),
    ] {
        assert_eq!(
            count(compound, kind),
            expected,
            "{kind:?} in the compound form"
        );
    }
}

#[test]
fn a_constrained_template_parameter_is_read_as_a_parameter() {
    // `template <C T>` and `template <C<T> U>` were already read before the rest of this module existed, and
    // they stay read: a constraint on a parameter is a type-name in the parameter's own syntax, not a clause.
    for source in [
        "template <C T> void f(T t);\n",
        "template <Number T> void f(T t);\n",
        "template <C<T> U> void f(U u);\n",
        "template <typename T, C<T> U> void g(T t, U u);\n",
    ] {
        parses(source);
    }

    let source = "template <C<T> U> void f(U u);\n";
    assert_eq!(
        count(source, CppSyntaxKind::TemplateParameter),
        1,
        "one parameter, constrained"
    );
    assert_eq!(count(source, CppSyntaxKind::ParameterList), 1);
}

#[test]
fn a_parenthesised_atomic_constraint_is_read_as_a_constraint() {
    // The standard allows the constraint of a clause to be written in parentheses: `requires (C<T>)`. The
    // parentheses are not decoration — they are how a constraint that is a *conjunction* is kept as one atomic
    // unit, and how `requires (C<T>)` is told from `requires C<T>` when what follows would otherwise be read as
    // part of the constraint.
    //
    // It reads through the ordinary expression rule, which is the point: a parenthesised constraint is a
    // `ParenExpr` around a constraint, not a second kind of constraint. These are the spellings that has to
    // cover, including the ones where a `(` would otherwise be read as a parameter list or a call.
    for source in [
        // The plain form, after the template head and after a declarator.
        "template <typename T> requires (C<T>) void f(T t);\n",
        "template <typename T> void f(T t) requires (C<T>);\n",
        "template <typename T> void f(T t) requires (C<T>) { }\n",
        // Doubly parenthesised, and parenthesised around a compound constraint.
        "template <typename T> requires ((C<T>)) void f(T t);\n",
        "template <typename T> requires (C<T> && D<T>) void f(T t);\n",
        "template <typename T> requires (C<T> || D<T>) void f(T t);\n",
        "template <typename T> requires (sizeof(T) > 1) void f(T t);\n",
        // A `>` inside the clause, in every position a clause can be written — the angle-depth of the template
        // head used to still be in effect here, so the `>` was read as closing a template argument list that had
        // already ended. `<`, `>=` and `>>` are the same question asked with the other angle tokens.
        "template <typename T> void f(T t) requires (sizeof(T) > 1);\n",
        "template <typename T> void f(T t) requires (sizeof(T) > 1) { }\n",
        "template <typename T> concept C = (sizeof(T) > 1);\n",
        "template <typename T> requires (sizeof(T) >= 1) void f(T t);\n",
        "template <typename T> requires (sizeof(T) < 8) void f(T t);\n",
        "template <int N> requires (N > 0) void f();\n",
        "template <int N> requires ((N >> 1) > 0) void f();\n",
        // Parenthesised terms conjoined with bare ones — the form a real constraint is usually written in.
        "template <typename T> requires (C<T>) && (D<T>) void f(T t);\n",
        "template <typename T> requires (C<T>) && D<T> void f(T t);\n",
        "template <typename T> void f(T t) requires (C<T>) && (D<T>) { }\n",
        // A parenthesised **requires-expression**: the parentheses are what keep its own braces from being read
        // as the body of the definition.
        "template <typename T> requires (requires(T t) { t.f(); }) void f(T t);\n",
        "template <typename T> void f(T t) requires (requires(T t) { t.f(); }) { }\n",
        // The same clause after the template head, on a member definition out of line, and on a function whose
        // return type is trailing.
        "template <typename T> requires (C<T>) struct S { };\n",
        "template <typename T> void S<T>::f() requires (C<T>) { }\n",
        "template <typename T> auto f(T t) -> int requires (sizeof(T) > 1);\n",
        "void f() requires (true);\n",
        // And as the constraint of a concept definition, where the whole `=` payload is the constraint.
        "template <typename T> concept C = (C2<T>);\n",
        "template <typename T> concept C = (C2<T> && C3<T>);\n",
        "template <typename T> concept C = (requires(T t) { t.f(); });\n",
    ] {
        parses(source);
    }

    // The clause still reaches the end of the constraint when the constraint is parenthesised: what is inside
    // the parentheses belongs to the clause, and so do the terms conjoined after them.
    let source = "template <typename T> requires (C<T>) && (D<T>) void f(T t);\n";
    assert_eq!(count(source, CppSyntaxKind::RequiresClause), 1);
    assert_eq!(
        text_of(source, CppSyntaxKind::RequiresClause).trim(),
        "requires (C<T>) && (D<T>)"
    );
    assert_eq!(
        count(source, CppSyntaxKind::ParenExpr),
        2,
        "each parenthesised term is a ParenExpr, kept rather than dropped"
    );
    assert_eq!(
        count(source, CppSyntaxKind::RequiresClause),
        1,
        "one clause, however many terms it conjoins"
    );
}

#[test]
fn a_constraint_does_not_swallow_the_body() {
    // A clause sits between a declarator and the body of a definition, so the `{` after the constraint opens the
    // *body* —
    //
    //     template <typename T> void f(T t) requires C<T> { }
    //                                                ^ the constraint ends here
    //
    // — while the expression grammar reads a `{` after an expression as C++11's list-initialisation of a
    // temporary (`Vec<int>{1, 2}`). Left alone, `requires C<T> { }` came out as a constraint of `C<T>{}` with the
    // body missing: the tree was well formed, nothing was reported, and the function had no body. The shape is
    // the assertion, because that failure produces no error at all.
    let source = "template <typename T> void f(T t) requires C<T> { }\n";
    parses(source);

    assert_eq!(count(source, CppSyntaxKind::RequiresClause), 1);
    assert_eq!(
        count(source, CppSyntaxKind::CompoundStat),
        1,
        "the braces after the constraint are the body"
    );
    assert_eq!(
        count(source, CppSyntaxKind::InitListExpr),
        0,
        "the body's braces are not a braced initialiser of the constraint"
    );
    assert_eq!(
        text_of(source, CppSyntaxKind::RequiresClause).trim(),
        "requires C<T>",
        "the clause stops before the body"
    );

    // The refusal is scoped to the constraint: outside one, a `{` after an expression is still read as a braced
    // initialiser, which is the reading C++ gives it.
    for source in [
        "void f() { x = { 1 }; }\n",
        "void f() { x = { 1, 2 }; }\n",
        "void f() { auto x = T{ 1 }; }\n",
        "void f() { auto x = T{ 1, 2 }; }\n",
    ] {
        parses(source);
    }

    assert_eq!(
        count("void f() { x = { 1 }; }\n", CppSyntaxKind::InitListExpr),
        1,
        "a braced initialiser outside a constraint is still one"
    );
}

#[test]
fn a_requires_clause_may_constrain_a_member_definition_out_of_line() {
    // A member definition written out of line constrains the member, and the clause lands after the declarator
    // like any other. The interesting part is that the qualified name before it does not stop the clause from
    // being seen.
    for source in [
        "template <typename T> void S<T>::f() requires C<T> { }\n",
        "template <typename T> void S<T>::f() requires C<T> && C2<T> { }\n",
        "template <typename T> S<T>::S() requires C<T> { }\n",
    ] {
        parses(source);
    }
}

#[test]
fn a_requires_expression_is_read_inside_any_expression() {
    // It is a primary expression, so it nests wherever an expression does — including inside another one, which
    // is what makes the conjoined form `requires { … } && requires { … }` work.
    for source in [
        "void f() { auto x = requires { g(); } && requires { h(); }; }\n",
        "void f() { auto x = !requires { g(); }; }\n",
        "void f() { if (requires { g(); }) { } }\n",
        "void f() { return requires { g(); }; }\n",
        "template <typename T> concept C = requires { g(); } || requires { h(); };\n",
    ] {
        parses(source);
    }

    assert_eq!(
        count(
            "void f() { auto x = requires { g(); } && requires { h(); }; }\n",
            CppSyntaxKind::RequiresExpr
        ),
        2,
        "two requires-expressions, conjoined"
    );
}
