//! Expression forms: the parenthesized expression, the conditional operator, and the lambda.
//!
//! Three things that were missing together, and for one reason. `parse_primary_expr` had no rule for a `(` at
//! all, so every expression that *began* with one failed — and `(a > b) ? x : y` begins with one, which is why
//! the conditional operator looked broken while `a ? b : c` worked. The lambda is the same shape of gap in a
//! different place: `[` is both an index and a capture list, so it needs a decision rather than a rule.
//!
//! The shapes are pinned rather than just "it parses", because these three produce nodes a consumer navigates:
//! a `ParenExpr` that keeps its parentheses, a `TernaryExpr` with three parts, and a `LambdaExpr` whose capture
//! list is its own node.

use cpp_parser::{CppParser, CppSyntaxKind, ParserConfig};

fn parses(source: &str) {
    let tree = CppParser::parse(source, ParserConfig::default());
    assert_eq!(
        tree.get_errors(),
        [],
        "{source:?} must parse cleanly, got {:?}",
        tree.get_errors()
    );
    assert_eq!(tree.to_source_text(), source, "{source:?} stays lossless");
}

fn count(source: &str, kind: CppSyntaxKind) -> usize {
    let tree = CppParser::parse(source, ParserConfig::default());
    tree.get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
        .count()
}

#[test]
fn sizeof_reads_an_operand_that_is_not_a_bare_name() {
    // `sizeof` and `typeid` accept a type-id *or* an expression, and the type reading is tried first because it
    // is the one that can be refused. But a type-id can **stop early** — a name on its own is a complete one — so
    // "it parsed and consumed something" was not enough: `sizeof(a[0])` was read as the type `a`, the cursor was
    // left on the `[`, and `expected )` was reported against it. Every operand but a bare name or a keyword type
    // failed, which in C is most of the `sizeof`s there are.
    //
    // What settles it is where the reading **stopped**: the payload ends at the `)`, so a type-id that did not
    // reach it was not the reading at all.
    for source in [
        "void f() { auto n = sizeof(a[0]); }\n",
        "void f() { auto n = sizeof(a.b); }\n",
        "void f() { auto n = sizeof(a->b); }\n",
        "void f() { auto n = sizeof(a + b); }\n",
        "void f() { auto n = sizeof(a()); }\n",
        "void f() { auto n = sizeof(*p); }\n",
        "void f() { auto n = sizeof(names) / sizeof(names[0]); }\n",
        "void f() { auto n = typeid(a[0]); }\n",
        "void f() { auto n = sizeof(int); }\n",
        "void f() { auto n = sizeof(int*); }\n",
        "void f() { auto n = sizeof(const char*); }\n",
        "void f() { auto n = sizeof(unsigned long); }\n",
        "void f() { auto n = sizeof(std::vector<int>); }\n",
        // An **elaborated type specifier** is a type in a type-id, and it used to be read as the keyword alone:
        // the name after `struct` was refused because a type-id allows only one name, so `sizeof(struct S)` never
        // reached its `)`. The keyword and its name are one specifier — this is C's spelling of a type.
        "void f() { auto n = sizeof(struct S); }\n",
        "void f() { auto n = sizeof(union U); }\n",
        "void f() { auto n = sizeof(enum E); }\n",
        "void f() { auto n = sizeof(struct S*); }\n",
        "void f() { auto n = (struct S*)p; }\n",
        "void f() { struct S* p = 0; p = (struct S*)q; }\n",
        "void f() { auto n = sizeof x; }\n",
        "void f() { auto n = sizeof...(Ts); }\n",
        "void f() { auto n = alignof(a[0]); }\n",
    ] {
        parses(source);
    }

    // A truncated operand is still reported: the fallback is the expression rule, not silence.
    assert!(
        !CppParser::parse(
            "void f() { auto n = sizeof(a +); }\n",
            ParserConfig::default()
        )
        .get_errors()
        .is_empty(),
        "a truncated operand is still reported"
    );
}

#[test]
fn a_directive_inside_a_literal_run_is_read_as_a_directive() {
    // A message whose middle is conditional is how a generated file writes "this compiler's spelling of X", and
    // the preprocessor removes the directives before the string is concatenated:
    //
    // ```c
    // const char* info = "INFO" ":" "extensions["
    // #if defined(__clang__)
    //   "ON"
    // #else
    //   "OFF"
    // #endif
    //   "]";
    // ```
    //
    // A `#` in the middle of a string-literal run cannot be anything else, so it is read as the directive node it
    // is and the run continues. Everywhere else in an expression a `#` is still an error, which is the half that
    // keeps this from hiding real ones.
    parses(
        "void f() { const char* s = \"a\"\n#if X\n  \"b\"\n#else\n  \"c\"\n#endif\n  \"d\"; }\n",
    );

    let conditional = "void f() { const char* s = \"a\"\n#if X\n  \"b\"\n#endif\n  \"c\"; }\n";
    assert_eq!(
        count(conditional, CppSyntaxKind::LiteralExpr),
        1,
        "the run is one literal"
    );
    assert_eq!(
        count(conditional, CppSyntaxKind::PreprocessorDirective),
        2,
        "and both directives are in the tree"
    );

    assert!(
        !CppParser::parse("void f() { x = 1 # 2; }\n", ParserConfig::default())
            .get_errors()
            .is_empty(),
        "a stray `#` in an expression is still reported"
    );
}

/// An **array type** as the operand of `sizeof`, and the same tokens as an *index*.
///
/// `sizeof(int[4])` and `sizeof(a[0])` are the same shape, and only name lookup tells them apart — is `a` a type?
/// The type-id reader asks the file's own table, which is the bounded evidence the rest of the type grammar uses:
/// where the answer is missing the brackets are left alone and the **expression** reading applies, which is the
/// useful one for `a[0]` and never invents an array type.
#[test]
fn an_array_type_is_read_as_a_type_and_an_index_as_an_expression() {
    for source in [
        "void f() { auto n = sizeof(int[4]); }\n",
        "void f() { auto n = sizeof(char[256]); }\n",
        "void f() { auto n = sizeof(int[2][3]); }\n",
        "void f() { auto n = sizeof(int[]); }\n",
        "void f() { auto n = sizeof(int*[4]); }\n",
        // …and the shapes that must stay expressions.
        "void f() { auto n = sizeof(a[0]); }\n",
        "void f() { auto n = sizeof(a.b); }\n",
        "void f() { auto x = a[b < c]; }\n",
    ] {
        parses(source);
    }

    // The type reading reaches the brackets, and the argument-list lookahead does not mistake the `]` for a
    // boundary: `Vec<int[4]>` is a template-id whose argument is an array type.
    for source in [
        "void f() { Vec<int[4]> v; }\n",
        "void f() { auto v = Vec<int[4]>{}; }\n",
        "void f() { g<Vec<int[4]>>(); }\n",
        "void f() { Vec<arr[0]> v; }\n",
    ] {
        parses(source);
    }

    assert_eq!(
        count(
            "void f() { auto n = sizeof(int[4]); }\n",
            CppSyntaxKind::ArrayType
        ),
        1,
        "the bound is part of the type"
    );
    assert_eq!(
        count(
            "void f() { auto n = sizeof(a[0]); }\n",
            CppSyntaxKind::ArrayType
        ),
        0,
        "an index is not an array type"
    );
    assert_eq!(
        count(
            "void f() { auto n = sizeof(a[0]); }\n",
            CppSyntaxKind::IndexExpr
        ),
        1
    );

    // A `new` reads its own bounds, and they must stay out of the type: `new int[4]` is an allocation whose
    // declarator holds the `[4]`, and a type-id that swallowed it would move the `ArrayType` into the `TypeId`.
    assert_eq!(
        count(
            "void f() { auto p = new int[4]; }\n",
            CppSyntaxKind::ArrayType
        ),
        1
    );
    let new_expr = "void f() { auto p = new int[4]; }\n";
    let tree = CppParser::parse(new_expr, ParserConfig::default());
    let type_id = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TypeId)
        .expect("the allocation names a type");
    assert_eq!(
        type_id.text().to_string().trim(),
        "int",
        "the bounds are the new-declarator's, not the type's"
    );
}

#[test]
fn adjacent_string_literals_are_one_literal() {
    // Not a grammar rule but a translation phase: `"a" "b"` concatenates, and the run of literals is one string.
    // Reading the first and stopping left the rest for whatever came next, so `const char *s = "a" "b";` ended its
    // initialiser after `"a"` and reported `expected ;` against `"b"`.
    //
    // The shape is everywhere in C and C++ — it is how a long message is wrapped across lines — and it was found
    // in a real file, ten of them in one initialiser. The last two sources here are that shape.
    for source in [
        "void f() { const char *s = \"a\"; }\n",
        "void f() { const char *s = \"a\" \"b\"; }\n",
        "void f() { const char *s = \"a\" \"b\" \"c\"; }\n",
        "void f() { g(\"a\" \"b\", 1); }\n",
        "void f() { auto s = \"a\" \"b\"; }\n",
        "void f() { if (s == \"a\" \"b\") { } }\n",
        "const char *file_scope = \"a\" \"b\";\n",
        "void f() {\n    const char *msg =\n        \"line one\\n\"\n        \"line two\\n\"\n        \"line three\\n\";\n}\n",
        // An **identifier** in the run is a macro, and this parser does not run the preprocessor: after expansion
        // `"compiler[" COMPILER_ID "]"` is one string, and a string literal followed by a name is not valid C++ in
        // any other reading. It is how every diagnostic message with a version in it is spelled — CMake's
        // compiler-id file is nothing but these.
        "void f() { const char *s = \"INFO\" \":\" \"compiler[\" COMPILER_ID \"]\"; }\n",
        "void f() { const char *s = \"a\" MACRO \"b\" OTHER \"c\"; }\n",
        "void f() { char c = 'a'; auto n = 1 + 2; auto b = true; }\n",
    ] {
        parses(source);
    }

    // One node, because one string is what they produce: a consumer reading the initialiser finds a single
    // literal rather than a row of them to join itself.
    assert_eq!(
        count(
            "const char *s = \"a\" \"b\" \"c\";\n",
            CppSyntaxKind::LiteralExpr
        ),
        1
    );

    // A literal with a **user-defined suffix** is a different kind of thing — a call to `operator""` rather than a
    // string — so it ends the run rather than joining it. The plain literal before it is still one literal.
    parses("void f() { auto x = \"a\"_km; }\n");
    assert_eq!(
        count(
            "void f() { auto x = \"a\"_km; }\n",
            CppSyntaxKind::LiteralExpr
        ),
        1
    );

    // Nothing else concatenates: two literals of other kinds in a row is a syntax error, as it should be.
    assert!(
        !CppParser::parse("void f() { auto x = 1 2; }\n", ParserConfig::default())
            .get_errors()
            .is_empty(),
        "`1 2` is not a literal run"
    );
}

#[test]
fn a_for_header_reads_its_step_as_an_expression() {
    // A `for` header has no `;` of its own, so a *declaration* reading that consumed only a type and named nothing
    // had nothing left to fail on: `for (;; i++)` read `i` as the type, produced an empty declarator, and then
    // reported `expected )` against the `++` — the step was never read as an expression at all. The ordinary
    // statement path is saved by its own `;`; the header is the one place the question has to be asked directly.
    for source in [
        "void f() { for (;; i++) { } }\n",
        "void f() { for (;; ++i) { } }\n",
        "void f() { for (;; i++, k++) { } }\n",
        "void f() { for (i = 0; i < n; i++) { } }\n",
        "void f() { for (i = 0, k = 0; i < n; i++, k++) { } }\n",
        "void f() { for (int i = 0; i < n; i++) { } }\n",
        "void f() { for (int i = 0, k = 0; i < n; i++, k++) { } }\n",
        "void f() { for (auto x : items) { } }\n",
        "void f() { for (v : items) { } }\n",
        "void f() { for (;;) { } }\n",
    ] {
        parses(source);
    }

    // The step is an expression, so it is an `ExpressionStat` and not a `Declaration` — which is the shape the
    // wrong reading produced, a declaration that named nothing.
    let source = "void f() { for (;; i++, k++) { } }\n";
    assert_eq!(count(source, CppSyntaxKind::ExpressionStat), 1);
    assert_eq!(
        count(source, CppSyntaxKind::ForStat),
        1,
        "the header is still a for statement"
    );
    assert_eq!(
        count(source, CppSyntaxKind::Declaration),
        1,
        "only the function definition is a declaration"
    );
}

#[test]
fn a_parenthesized_expression_is_its_own_node() {
    // The parentheses are kept rather than dropped: they are not redundant, since a consumer that rewrites the
    // expression has to know where the grouping was.
    for source in [
        "void f() { x = (a); }\n",
        "void f() { x = (a + b); }\n",
        "void f() { x = (f()); }\n",
        "void f() { x = ((a)); }\n",
        "void f() { x = a * (b + c); }\n",
        "void f() { if ((a)) { } }\n",
        "void f() { while ((a && b)) { } }\n",
        "void f() { f((a), (b)); }\n",
    ] {
        parses(source);
    }

    assert_eq!(
        count("void f() { x = ((a)); }\n", CppSyntaxKind::ParenExpr),
        2,
        "nested parentheses are two nodes"
    );
}

#[test]
fn the_conditional_operator_parses_around_a_parenthesized_condition() {
    // The case that looked like a missing operator and was a missing operand rule.
    for source in [
        "void f() { auto x = a ? b : c; }\n",
        "void f() { x = (a > b) ? a : b; }\n",
        "void f() { auto x = (a > b) ? a : b; }\n",
        "void f() { return (a) ? (b) : (c); }\n",
        "void f() { x = (a ? b : c) ? d : e; }\n",
        "void f() { x = cond ? (a + b) : (c + d); }\n",
        "void f() { x = a ? b : c ? d : e; }\n",
        "void f() { x = a == b ? 1 : 2; }\n",
    ] {
        parses(source);
    }

    assert_eq!(
        count(
            "void f() { x = a ? b : c ? d : e; }\n",
            CppSyntaxKind::TernaryExpr
        ),
        2,
        "a conditional in the false branch nests"
    );
}

#[test]
fn an_assignment_containing_a_conditional_is_an_expression() {
    // The statement is an **expression**, not a declaration, and that is the half worth asserting: `x = ...;`
    // used to be read as a declaration of a variable whose declarator named nothing — a silent wrong tree, since
    // the shape was perfectly well formed and nothing was reported. Every assignment whose left-hand side the
    // file had not seen a type for came out that way. See `an_assignment_is_not_a_declaration` in `direct_init`.
    //
    // The *shape* of the conditional is C++'s and not the naive one: in `a ? b : c` the `?`'s left operand is a
    // full expression, so an assignment written there is nested **inside** the `TernaryExpr` — `(x = a) ? b : c`
    // — rather than the conditional sitting on the assignment's right-hand side. That is the grammar, unusual as
    // it looks, and pinning it keeps someone from "fixing" it later.
    let tree = CppParser::parse("void f() { x = a ? b : c; }\n", ParserConfig::default());
    let root = tree.get_red_root();
    let ternary = root
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TernaryExpr)
        .expect("the conditional is a ternary expression");

    assert!(
        ternary
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::BinaryExpr),
        "the assignment is the conditional's condition"
    );
    assert_eq!(
        count("void f() { x = a ? b : c; }\n", CppSyntaxKind::TernaryExpr),
        1
    );
    assert_eq!(
        count("void f() { x = a ? b : c; }\n", CppSyntaxKind::Initializer),
        0,
        "an assignment has no initialiser"
    );
}

#[test]
fn a_lambda_is_its_own_node_with_its_capture_list() {
    for source in [
        "void f() { auto a = [] {}; }\n",
        "void f() { auto b = []() { return 1; }; }\n",
        "void f() { auto c = [](int x) { return x; }; }\n",
        "void f() { auto d = [&](int x) { return x + 1; }; }\n",
        "void f() { auto e = [x](int y) { return x + y; }; }\n",
        "void f() { auto g = [&x, y]() { return x + y; }; }\n",
        "void f() { auto h = [=]() { return x; }; }\n",
        "void f() { auto i = [this]() { return n; }; }\n",
        "void f() { auto j = [*this]() { return n; }; }\n",
        "void f() { auto k = []() mutable { }; }\n",
        "void f() { auto l = []() constexpr { }; }\n",
        "void f() { auto m = []() noexcept { }; }\n",
        "void f() { auto n = [](int x) -> int { return x; }; }\n",
        "void f() { auto o = [p = 1]() { return p; }; }\n",
        "void f() { auto q = [n](auto x) { return x; }; }\n",
        "void f() { auto r = [x, &y]() { }; }\n",
        "auto file_scope = [](int x) { return x; };\n",
        "void f() { std::sort(v.begin(), v.end(), [](int i, int j) { return i < j; }); }\n",
        "void f() { call([](int i) { return i; }, 1); }\n",
    ] {
        parses(source);
    }

    // The capture list is a node of its own, so a consumer does not have to re-read the lambda's tokens to find
    // what it captures.
    assert_eq!(
        count(
            "void f() { auto g = [&x, y]() { return x + y; }; }\n",
            CppSyntaxKind::LambdaCaptureList
        ),
        1
    );
    assert_eq!(
        count(
            "void f() { auto g = [&x, y]() { return x + y; }; }\n",
            CppSyntaxKind::LambdaCapture
        ),
        2,
        "one node per capture"
    );
    assert_eq!(
        count(
            "void f() { auto g = [&x, y]() { return x + y; }; }\n",
            CppSyntaxKind::LambdaExpr
        ),
        1
    );

    // An init-capture is one capture, not two: the initialiser belongs to the name it initialises.
    assert_eq!(
        count(
            "void f() { auto o = [p = 1]() { return p; }; }\n",
            CppSyntaxKind::LambdaCapture
        ),
        1
    );
}

#[test]
fn the_index_operator_is_not_stolen_by_the_lambda_rule() {
    // `[` starts both a capture list and an index, and only what *follows* the `]` tells them apart. Every index
    // spelling in the corpus has to keep its own reading.
    for source in [
        "void f() { arr[i] = 1; }\n",
        "void f() { auto x = a[b]; }\n",
        "void f() { auto x = a[b + c]; }\n",
        "void f() { auto x = m[k][j]; }\n",
        "void f() { int a[] = {1, 2}; }\n",
        "void f() { f(a[0], b[1]); }\n",
    ] {
        parses(source);
    }

    assert_eq!(
        count("void f() { auto x = a[b]; }\n", CppSyntaxKind::LambdaExpr),
        0,
        "an index is not a lambda"
    );
    assert_eq!(
        count("void f() { auto x = a[b]; }\n", CppSyntaxKind::IndexExpr),
        1
    );
}

#[test]
fn a_lambda_body_is_a_compound_statement() {
    // The body is parsed by the statement rule, which is what keeps `return`, a local declaration and a nested
    // lambda inside it working like any other block.
    for source in [
        "void f() { auto g = []() { int x = 1; return x; }; }\n",
        "void f() { auto g = []() { if (a) { return 1; } return 2; }; }\n",
        "void f() { auto g = []() { auto h = []() { return 1; }; return h(); }; }\n",
        "void f() { auto g = [](int x) { for (int i = 0; i < x; ++i) { } }; }\n",
    ] {
        parses(source);
    }
}

#[test]
fn a_braced_init_list_is_read_where_the_grammar_wants_an_initializer_clause() {
    // C++ says the right operand of an assignment and an argument of a call are *initializer-clauses*, not
    // expressions — so `{1, 2}` is allowed in both, and a `{` there cannot begin anything else. Neither was
    // read, and the two are one gap: the expression grammar had no rule that accepts a `{` at all.
    //
    // The assignment half was hidden. `x = {1};` used to parse — as a *declaration* of a variable whose
    // declarator named nothing, the silent wrong tree described in `an_assignment_is_not_a_declaration` below.
    // Fixing that turned it into a loud error, which is how the missing rule came to be visible at all.
    for source in [
        "void f() { x = { 1 }; }\n",
        "void f() { x = { 1, 2 }; }\n",
        "void f() { x = {}; }\n",
        "void f() { x += { 1 }; }\n",
        "void f() { x = { 1 }, y = { 2 }; }\n",
        "void f() { v.push_back({ 1, 2 }); }\n",
        "void f() { v.push_back({}); }\n",
        "void f() { f({ 1 }); }\n",
        "void f() { f(a, { 1 }); }\n",
        "void f() { f({ 1 }, { 2 }); }\n",
        "void f() { m.insert({ key, value }); }\n",
        // The declaration shapes keep their own reading, which is the half that must not change.
        "void f() { int x = { 1 }; }\n",
        "void f() { int a[] = { 1, 2 }; }\n",
        "void f() { auto p = Pair{ 1, 2 }; }\n",
        "void f() { std::vector<int> v{ 1, 2 }; }\n",
    ] {
        parses(source);
    }

    // The two positions produce the same node, because they are the same production one level apart — and it is
    // not the `Initializer` a declaration wraps its `= ...` in, which is what keeps "an assignment of a braced
    // list" distinguishable from "a declaration initialised with braces".
    assert_eq!(
        count("void f() { x = { 1, 2 }; }\n", CppSyntaxKind::InitListExpr),
        1
    );
    assert_eq!(
        count("void f() { x = { 1, 2 }; }\n", CppSyntaxKind::Initializer),
        0,
        "an assignment's right-hand side is not a declaration's initialiser"
    );
    assert_eq!(
        count(
            "void f() { v.push_back({ 1, 2 }); }\n",
            CppSyntaxKind::InitListExpr
        ),
        1
    );
    assert_eq!(
        count(
            "void f() { int xx[] = { 1, 2 }; }\n",
            CppSyntaxKind::InitListExpr
        ),
        1,
        "a declaration's initialiser is still an InitListExpr, once"
    );

    // A braced list inside a *constraint* is a requirement and not an initialiser, so the reading is refused
    // there. See `concepts.rs`.
    assert_eq!(
        count(
            "template <typename T> void f(T t) requires C<T> { }\n",
            CppSyntaxKind::InitListExpr
        ),
        0,
        "the brace after a constraint is the body"
    );
}

#[test]
fn a_template_id_does_not_claim_the_next_one() {
    // `C<T> && C2<T>` is two template-ids in one expression, and the first one failed on its own closing `>`.
    //
    // The rule that decides where a template argument list ends used to scan forward from the `>` and give up as
    // soon as it saw a `<`, on the theory that an inner list still wanted the `>`. The direction is the error: a
    // `<` to the *right* says nothing about a `>` to its left. The shape is not exotic — it is how every
    // conjunction of two concepts is written, and `T<A>::value < T<B>::value` is the same shape with an operator
    // between them.
    for source in [
        "void f() { C<T> && C2<T>; }\n",
        "void f() { C<T> || C2<T>; }\n",
        "void f() { C<T> && C2<T> && C3<T>; }\n",
        "void f() { T<A>::value < T<B>::value; }\n",
        "void f() { auto x = C<T> && C2<T>; }\n",
        "void f() { if (C<T> && C2<T>) { } }\n",
        "void f() { auto x = f<A>(1) < f<B>(2); }\n",
        "void f() { std::vector<std::vector<int>> v; }\n",
        "void f() { std::map<int, std::vector<int>> m; }\n",
    ] {
        parses(source);
    }

    assert_eq!(
        count(
            "void f() { C<T> && C2<T>; }\n",
            CppSyntaxKind::TemplateArgumentList
        ),
        2,
        "two template-ids, two argument lists"
    );
    assert_eq!(
        count("void f() { C<T> && C2<T>; }\n", CppSyntaxKind::BinaryExpr),
        1,
        "the `&&` joins them rather than being absorbed"
    );
}

#[test]
fn a_declarator_name_is_never_a_bare_template_id() {
    // The other half of the same statement. With the argument list fixed, `C<T> && C2<T>;` still parsed — as a
    // *declaration*, of an rvalue reference whose declarator was named `C2<T>`: well formed, lossless, no
    // diagnostic, and a binding no compiler would accept.
    //
    // A declarator's name cannot have template arguments. `C<T> x;` gives the arguments to the **type** and names
    // `x`; a template-id in the name position is a name only when it is *qualified*, where the arguments belong
    // to the qualifier (`S<T>::f` names `f`). Refusing the bare form is what lets the declaration reading fail
    // and the statement fall back to the expression it is.
    let source = "void f() { C<T> && C2<T>; }\n";
    parses(source);
    assert_eq!(
        count(source, CppSyntaxKind::ExpressionStat),
        1,
        "the statement is an expression"
    );
    assert_eq!(
        count(source, CppSyntaxKind::Declaration),
        1,
        "only the function definition is a declaration"
    );
    assert_eq!(
        count(source, CppSyntaxKind::RValueReferenceType),
        0,
        "`&&` is the logical operator here, not a declarator's rvalue reference"
    );

    // What must keep its reading: the **explicit instantiation**, which is the one declaration whose name really
    // is a template-id, and every shape whose template-id belongs to a type or a qualifier.
    for source in [
        "extern template void f<int>(int);\n",
        "extern template class C<int>;\n",
        "extern template struct S<int>;\n",
        "extern template int v<int>;\n",
        "template <typename T> struct A<T*> { };\n",
        "template <typename T> void S<T>::f() { }\n",
        "template <typename T> void S<T>::f() requires C<T> { }\n",
        "A<int> x;\n",
        "void f() { A<int> x; }\n",
        "void f() { std::vector<int> v; }\n",
        "void f() { T<A>::value = 1; }\n",
    ] {
        parses(source);
    }

    // The flag that suspends the rule belongs to one declaration only: the explicit instantiation above must not
    // leave the *next* declaration free to read a bare template-id as a name.
    let source = "extern template void f<int>(int);\nvoid g() { C<T> && C2<T>; }\n";
    parses(source);
    assert_eq!(
        count(source, CppSyntaxKind::ExpressionStat),
        1,
        "the declaration after an instantiation is still read by the ordinary rules"
    );
}
