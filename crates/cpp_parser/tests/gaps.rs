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
            // Conversion operators: the name is the type it converts to, and it begins the declaration, so no
            // specifier sequence runs before it.
            "struct S { operator int(); };",
            "struct S { operator bool() const; };",
            "struct S { operator const char*(); };",
            "struct S { operator std::string() const; };",
            "struct S { operator std::vector<int>(); };",
            "struct S { operator T&(); };",
            "struct S { operator T&&(); };",
            "struct S { operator unsigned long long(); };",
            "struct S { operator int() { return 1; } };",
            "struct S { operator int() = delete; };",
            "struct S { operator int() const &; };",
            "struct S { operator bool() &&; };",
            "struct S { virtual operator bool() const noexcept; };",
            // Aliases to array and function types: the type's suffixes have nothing enclosing them here.
            "using Arr = int[4];",
            "using Fn = int(char);",
            "using Ptr = int(*)[4];",
            // Attributes, in the positions that used to be gaps.
            "[[nodiscard]] int attributed();",
            "int attributed2 [[gnu::aligned(16)]];",
            "int attributed3() [[carries_dependency]];",
            "void attributed4() [[noreturn]] { for (;;) {} }",
            "void attributed5(int x [[maybe_unused]]);",
            "void attributed6([[maybe_unused]] int x);",
            "using Alias [[deprecated]] = int;",
            "template <typename T> [[nodiscard]] T attributed7();",
            "enum class Attributed8 { A [[deprecated]] = 1, B };",
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
            // An **alias template**, which is a `using`-declaration with a template head in front of it.
            "template <typename U> using rebind = Rebind<U>;",
            "template <typename... Us> using Pack = std::tuple<int, char>;",
            // A pack expansion as a template argument, in a declaration's type rather than in an expression.
            "template <typename... Ts> using All = std::tuple<Ts...>;",
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
            // A C++20 generic lambda: a template parameter list between the capture list and the parameters,
            // parsed by the same rule a class template uses.
            "auto g = []<typename T>(T t) { return t; };",
            "auto g = []<class T>(T t) { return t; };",
            "auto g = []<typename... Ts>(Ts... ts) { };",
            "auto g = []<typename T> { };",
            // Coroutine statements: `co_return` is `return`, `co_yield` is `throw`'s shape, and `co_await` is a
            // unary operator.
            "auto x = co_await f();",
            "co_await f();",
            "co_return 1;",
            "co_return;",
            "co_yield 1;",
            // The `template` disambiguator, in the three places a dependent name can be used. It exists to say
            // that the `<` after the name starts template arguments rather than a comparison.
            "auto x = T::template f<int>();",
            "auto x = obj.template f<int>();",
            "auto x = p->template f<int>();",
            // Pack *expansion* — the use half of a pack, whose declaration half is above. One rule covers all of
            // it: a `...` after a pattern expands it, wherever the pattern is written.
            "g(args...);",
            "g(h(ts)...);",
            "g(args..., more);",
            "auto n = sizeof...(Ts);",
            "auto t = std::tuple<Ts...>();",
            "auto t = std::tuple<Ts..., Us...>();",
            "auto s = static_cast<Ts&&>(args)...;",
            // Fold expressions: the same binary rule with the `...` read as an operand, in all four spellings.
            "auto n = (ts + ...);",
            "auto n = (... + ts);",
            "auto b = (ts && ...);",
            "auto b = (... && ts);",
            // A pack expansion in a capture list, which is how a variadic forwarding lambda is written. The
            // capture rule reads the capture itself rather than an expression, so it needed its own `...`.
            "auto g = [args...] { return g(args...); };",
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
            // Destructors, in every spelling: declared, defined, defaulted, deleted, virtual, and written
            // out of line with a qualified name.
            "~Probe();",
            "~Probe() {}",
            "virtual ~Probe() = default;",
            "~Probe() = delete;",
            "~Probe() noexcept {}",
            "template <typename T> void f(T t);",
            // A member alias template: the same head-and-`using` shape as the file-scope form, written where
            // the member rule reaches it.
            "template <typename U> using rebind = Probe<U>;",
            "struct Nested { int x; };",
            "enum Color { Red, Green };",
            // Bit-fields, including a run of them and a nameless one.
            "int bits : 3;",
            "unsigned flags : 1, spare : 7;",
            "int : 0;",
            // A using-declaration of a base member, in the two shapes it is written in.
            "using Base::method;",
            "using Base::Alias;",
            "using Alias = int;",
            // A `friend` declaration *followed by another member*. The friend is a specifier whose payload is
            // the whole declaration — `;` included — so the member written after it is where the old rule ran
            // out of tokens and turned everything into error nodes.
            "friend void swap(Probe&, Probe&); int after = 0;",
            "friend class Other; int later = 0;",
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
            // file never declares, so `(MyType*)p` is not distinguishable from `(a * b)`. This is the one entry
            // that is a *decision* rather than a missing rule, and it is not on any list to be fixed.
            (
                "auto d = (MyType*)p;",
                "a C-style cast to a pointer of an undeclared type. `*` is both the pointer operator and the \
                 multiplication operator, and the type table is what tells them apart.",
            ),
        ],
    );

    // Pack expansion used to be here, and it is worth recording what the entries were, because they looked
    // like three separate gaps and were one rule — "a `...` after a pattern expands it":
    //
    //   `g(args...)` — an argument list;
    //   `sizeof...(Ts)` — which additionally needed the lexer's two tokens (`sizeof`, `...`) joined, since it
    //   has no token of its own;
    //   `std::tuple<Ts...>` — a template argument, where the type reading stops at the ellipsis and the list's
    //   loop then read it as an argument of its own.
    //
    // The declaration half of packs was never a gap (`template <typename... Ts> void f(Ts... ts);` has been in
    // the list above since it was written). Fold expressions came with the same change: `(ts + ...)` is the
    // ordinary binary rule with the `...` read as its right operand.

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
            // Everything that was here is read now. The entries were:
            //
            //   `int bits : 3;` — a bit-field, which needed the class-body context to tell its `:` from a
            //   constructor's member-initializer list;
            //   `using Base::method;` — a using-declaration, which needed the declaration rule to dispatch
            //   `using` at all (at file scope the statement rule happened to claim it first);
            //   `union Value { int i; ~Value() {} };` — a destructor definition, which needed the tilde
            //   claimed before the specifier sequence could read it as an operator.
            //
            // They are pinned as *read* in the test above instead, and this list is kept — rather than deleted —
            // so that the next gap has a place to go.
        ],
    );
}
