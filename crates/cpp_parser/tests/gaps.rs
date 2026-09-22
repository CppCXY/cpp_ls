//! Grammar coverage: what the parser reads today, and what it does not.
//!
//! # Why a file like this exists
//!
//! A parser for C++ is never "done", and the useful question is never "does it work?" but "which constructs
//! does it read, and which ones does it give up on?". Without a file that answers it, the answer is folklore:
//! every gap is rediscovered by whoever hits it, and a construct that starts working is never noticed.
//!
//! So the two lists below are the answer. [`constructs_the_parser_reads`] pins the shapes that work, and
//! [`constructs_the_parser_does_not_read_yet`] pins the ones that do not — each with a comment saying what the
//! syntax is and why it is not covered.
//!
//! # What a "gap" means here
//!
//! A construct in the second list produces a syntax error or an `ErrorNode`. It does **not** mean the file
//! fails to parse: the parser is total, so the tokens are all still in the tree and everything around the gap
//! is read normally. That is the property the editor depends on, and it is why the gaps are worth listing
//! rather than hiding — a gap costs one construct, not the rest of the file.
//!
//! When one of these starts working, the test that pins it fails, and the fix is to move the line from the
//! second list to the first. That is the whole mechanism, and it is deliberately the cheapest one available.

use cpp_parser::{CppParser, CppSyntaxKind, CppSyntaxTree, ParserConfig};

/// The three places a construct can be written, because the same tokens are read differently in each.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Where {
    /// At file scope, outside every brace.
    File,
    /// Inside a function body.
    Body,
    /// As a member of a class body.
    Class,
}

/// Parse `fragment` in `place` and report whether it came out clean.
fn reads(fragment: &str, place: Where) -> Result<(), String> {
    let source = match place {
        Where::File => fragment.to_string(),
        Where::Body => format!("void probe() {{ {fragment} }}"),
        Where::Class => format!("struct Probe {{ {fragment} }};"),
    };

    let tree = CppParser::parse(&source, ParserConfig::default());
    report(&source, &tree)
}

/// The empty string when the parse is clean, or a description of the first thing wrong with it.
fn report(source: &str, tree: &CppSyntaxTree) -> Result<(), String> {
    if let Some(error) = tree.get_errors().first() {
        return Err(format!("{} at {:?}", error.message, error.range));
    }

    // A `MissingNode` is a claim that a token should have been there, and an `ErrorNode` is a construct the
    // grammar gave up on. Both are failures for this file's purposes even when nothing was *reported*: a
    // missing declarator name is how `int bits : 3` is read today, and no error message says so.
    for node in tree.get_red_root().descendants() {
        match CppSyntaxKind::from(node.kind()) {
            CppSyntaxKind::ErrorNode => {
                return Err(format!("ErrorNode over {:?}", node.text_range()));
            }
            CppSyntaxKind::MissingNode => {
                return Err(format!("MissingNode at {:?}", node.text_range()));
            }
            _ => {}
        }
    }

    let _ = source;
    Ok(())
}

/// Pin a list of constructs as readable, reporting **every** failure rather than the first.
#[track_caller]
fn assert_reads(place: Where, constructs: &[&str]) {
    let failures: Vec<String> = constructs
        .iter()
        .filter_map(|construct| {
            reads(construct, place)
                .err()
                .map(|why| format!("  {construct}\n      {why}"))
        })
        .collect();

    assert!(
        failures.is_empty(),
        "{} of {} constructs no longer parse:{}",
        failures.len(),
        constructs.len(),
        failures
            .iter()
            .map(|failure| format!("\n{failure}"))
            .collect::<String>()
    );
}

/// Pin a list of constructs as **not** readable, so that each one is a deliberate entry rather than a
/// surprise. A construct that starts working fails here, which is the signal to move it up.
#[track_caller]
fn assert_does_not_read_yet(place: Where, constructs: &[(&str, &str)]) {
    let now_working: Vec<&str> = constructs
        .iter()
        .filter(|(construct, _)| reads(construct, place).is_ok())
        .map(|(construct, _)| *construct)
        .collect();

    assert!(
        now_working.is_empty(),
        "these constructs parse now, so move them into the list of what is read:\n  {}",
        now_working.join("\n  ")
    );
}

#[test]
fn constructs_the_parser_reads() {
    // Declarations, including the ones this file's own history made possible.
    assert_reads(
        Where::File,
        &[
            // Direct-initialisation with a type the file never declares: the point of the whole exercise.
            "Widget w(1, 2, 3);",
            "Widget w(1);",
            "std::string s(\"hi\");",
            "std::vector<std::vector<int>> vv;",
            "struct P { int x; } origin;",
            "struct F; struct G { struct H { int y; }; };",
            // Function and object types written with parenthesised declarators.
            "int (*signal(int sig))(int);",
            "using Callback = void (*)(int);",
            "typedef void (*Old)(int);",
            // Linkage.
            "extern \"C\" void f();",
            "extern \"C\" { void f(); }",
            // `bool` is a type keyword, and it used to be lexed as an identifier.
            "bool g = true;",
            // Contextual and modern specifiers.
            "struct S final : B { };",
            "struct S { explicit operator bool() const; };",
            "struct S { bool operator==(const S&) const = default; };",
            "struct S { S& operator=(const S&) = delete; };",
            "enum class E : unsigned char { A, B };",
            "static_assert(sizeof(int) == 4, \"int\");",
            "[[nodiscard]] int f();",
            "using namespace std;",
            "namespace fs = std::filesystem;",
            "namespace A::B { int x; }",
            "auto f() -> int { return 1; }",
            "int f() noexcept { return 1; }",
            "constexpr int k = 1;",
            "inline constexpr int k = 1;",
            "thread_local int t = 0;",
        ],
    );

    // Template parameter packs, in all three spellings.
    assert_reads(
        Where::File,
        &[
            "template <typename... Ts> struct S { };",
            "template <class... Ts> struct S { };",
            "template <typename T, typename... R> void f(T t, R... r);",
            "template <typename... A> void f(A&&... a);",
            "template <typename T = int> struct R { };",
            "template <template <class> class C> struct Q { };",
        ],
    );

    // Statements and expressions.
    assert_reads(
        Where::Body,
        &[
            "Widget w(1, 2, 3);",
            "Widget w(g(), h());",
            "Widget w{a, b};",
            "int n = sizeof(int);",
            "auto n = alignof(int);",
            "auto d = static_cast<int>(1.5);",
            "auto d = reinterpret_cast<int*>(p);",
            "auto d = (int)1.5;",
            "auto [a, b] = pair;",
            "auto [a, b] = std::pair<int, int>{1, 2};",
            "for (auto&& [k, v] : m) { }",
            "auto g = [v = 1] { return v; };",
            "auto g = [v = total + 1] { return v; };",
            "auto g = [p = std::move(q)] { };",
            // Braced initialization of a temporary: the `T{...}` form, in every position an expression can
            // stand. This is the rule that made `auto v = Vec<int>{1, 2};` a declaration rather than an error.
            "auto v = Vec<int>{1, 2};",
            "auto s = std::string{\"x\"};",
            "auto e = Vec<int>{};",
            "return Vec<int>{1, 2};",
            "g(Vec<int>{1});",
            "auto sum = Vec<int>{1} + other;",
            "auto p = new Widget(1, 2);",
            "auto p = new Widget;",
            "auto p = new Widget();",
            "auto p = new int[4];",
            "auto p = new int[4]{1, 2, 3, 4};",
            "auto p = new int[];",
            "auto p = new int[2][3];",
            "auto p = new std::string(\"x\");",
            "auto p = new (buf) Widget();",
            "auto p = new (buf, size) Widget();",
            "auto p = new (1) Widget();",
            "auto q = new Widget(1)->run();",
            "delete[] p;",
            "int n = sizeof(void(int));",
            "auto x = a ? b : c;",
            // The C++17 initializer in a condition: a declaration, a `;`, and the condition itself.
            "if (auto q = find(x); q != nullptr) { }",
            "switch (auto q = f(); q) { }",
            "if (x > 0) { }",
            "if (Foo* p = get()) { }",
            "if (auto q = f(); q) { } else { }",
            "try { } catch (const E& e) { }",
            "goto label; label: ;",
            "int x = {1};",
            // A parenthesised variable is an expression, not a cast — the shape that made the cast rule
            // precise rather than eager.
            "x = (a);",
            "x = (a + b);",
            "x = ((a));",
            "x = a * (b + c);",
        ],
    );

    // Class members. The class body is `struct Probe { ... }`, so a constructor written here has to be
    // called `Probe` — a constructor whose name is not its class's does not exist in C++, and it was the
    // test that was wrong rather than the parser when this list was first written.
    assert_reads(
        Where::Class,
        &[
            "mutable int cache = 0;",
            "[[nodiscard]] int pure() const noexcept;",
            "friend void swap(S&, S&);",
            "virtual ~S() = default;",
            "void f() override;",
            "static inline int count = 0;",
            "Probe(int v) : a(v) {}",
            "Probe() : a(1) {}",
            "Probe() : b{2} {}",
            "template <typename T> void f(T t);",
            "struct Nested { int x; };",
            "enum Color { Red, Green };",
        ],
    );
}

#[test]
fn constructs_the_parser_does_not_read_yet() {
    // Each entry is `(construct, why not)`. The reason is the important half: it says whether the gap is a
    // missing rule that would be cheap to add or a decision that needs an ambiguity resolved first.

    assert_does_not_read_yet(
        Where::Body,
        &[
            // The same trade as direct-initialisation, seen from the expression side: `MyType` is a type this
            // file never declares, so `(MyType*)p` is not distinguishable from `(a * b)` and is read as an
            // expression. Declaring the type anywhere in the file makes it a cast.
            (
                "auto d = (MyType*)p;",
                "a C-style cast to a pointer of an undeclared type. `*` is both the pointer operator and the \
                 multiplication operator, and the type table is what tells them apart.",
            ),
            // The following are read now, and used to be entries here:
            //
            //   `if (auto q = find(x); q != nullptr)` and its `switch` form — the C++17 initializer, which is
            //   the condition rule seeing a declaration, a `;` and then an expression;
            //   `auto v = Vec<int>{1, 2};` — a braced initializer after an expression, which the postfix loop
            //   now reads.
            //
            // They are pinned as *read* in the test above instead, which is where the entries went.
            (
                "auto x = co_await f();",
                "`co_await` is a keyword the lexer knows and the expression grammar does not: it is not a \
                 unary operator there. `co_return` and `co_yield` are in the same position.",
            ),
            (
                "auto l = []<typename T>(T t) { return t; };",
                "a C++20 template parameter list on a lambda. The capture list is recognised, and the `<...>` \
                 after it is not.",
            ),
            (
                "auto x = T::template f<int>();",
                "the `template` disambiguator. `parse_name` has no rule for it, so the keyword that exists to \
                 make a dependent name parse is itself unparseable.",
            ),
            (
                "auto x = obj.template f<int>();",
                "the same disambiguator after a member access.",
            ),
        ],
    );

    assert_does_not_read_yet(
        Where::File,
        &[
            (
                "alignas(16) struct A { int x; };",
                "`alignas` is not a decl-specifier: the specifier loop has no branch for it, so the `(` is \
                 read as an expression and the declaration reading is refused.",
            ),
        ],
    );

    assert_does_not_read_yet(
        Where::Class,
        &[
            (
                "int bits : 3;",
                "a bit-field. The `:` after a member declarator is taken for a constructor's \
                 member-initializer list — the two are the same token in the same position, and only the fact \
                 that this is a class body tells them apart — so the width is read as an initializer and the \
                 declarator is left without a name.",
            ),
            (
                "using Base::method;",
                "a using-*declaration* of a base member. `parse_using_declaration` reads a name and then the \
                 `::`-qualified path, but not the `Base::method` shape this needs.",
            ),
            (
                "union Value { int i; ~Value() {} };",
                "a destructor definition in a union — or in anything else. `~Value() {}` is read as a member \
                 declarator that never binds `(` to the name, and one error node is left where the braces \
                 were; the declaration is otherwise read. A *declared* destructor (`~Value();`) works, so the \
                 gap is in the body.",
            ),
        ],
    );
}
