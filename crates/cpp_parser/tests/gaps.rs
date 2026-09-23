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

/// A construct and the node kind its **statement** has to come out as.
///
/// The two lists above this one answer "does it parse?", and that question has a blind spot wide enough to hide a
/// whole class of defect. A tree can be well formed, lossless, free of every diagnostic and of every `ErrorNode`,
/// and still describe the wrong construct:
///
/// ```text
/// void f() { x = 1; }
///
/// Syntax(Declaration)              <- an assignment, read as a declaration
///   Syntax(DeclSpecifierSeq)  x
///   Syntax(InitDeclarator)
///     Token(Assign) "="
///     Syntax(Initializer) 1
/// ```
///
/// Nothing above catches that. Well-formedness holds for a wrong tree as much as a right one; `reads` looks for
/// errors and error nodes, and there are none; and the scope layer *already* declines to bind a declarator that
/// named nothing, so the false tree and the true one yield the same (empty) set of names. It was found by hand,
/// while probing something else.
///
/// So this list asks the question the others cannot: **what did it read it as?** Each entry names the construct
/// and the kind of the statement it must produce, and the statement is taken as the child of the enclosing body
/// — a leaf token has no children, so the constructs are exactly the nodes that do.
///
/// A statement kind is not enough on its own for an operator, though: `f(a, b)` and `f((a, b))` are both
/// `ExpressionStat`, and the whole difference between them is *inside*. So an entry may also name an expression
/// node that the construct must contain, and whether it is required or forbidden. That is what pins the comma
/// operator — the defect it could cause is a call with one argument instead of two, which no statement-level
/// assertion can see.
struct Shape {
    construct: &'static str,
    /// File scope input, or a fragment to place inside `void f() { ... }`.
    place: Where,
    /// The kind the statement node must have.
    kind: CppSyntaxKind,
    /// An expression node the construct must contain, or must not contain.
    expression: Option<(CppSyntaxKind, Presence)>,
}

/// Whether the expression named by a [`Shape`] has to be there or has to be absent.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Presence {
    /// The construct is this — a comma expression, a cast.
    Required,
    /// The construct must **not** be this: the tokens that look like it belong to something else — the comma of
    /// an argument list, the `*` of a multiplication.
    Forbidden,
}

/// The statement a fragment comes out as: the child of the body that holds it, or the root's own child at file
/// scope.
///
/// The statement is picked out by **kind**, not by "has children": a `return;` is two tokens and no child nodes
/// at all, so a filter on having children skips it and answers with the function declaration that encloses it.
fn statement_kind(source: &str) -> Option<CppSyntaxKind> {
    let tree = CppParser::parse(source, ParserConfig::default());
    let root = tree.get_red_root();

    /// The kinds a fragment is allowed to be read as. Tokens are `None`, which is what filters the trivia out.
    fn statement_kind_of(kind: CppSyntaxKind) -> Option<CppSyntaxKind> {
        matches!(
            kind,
            CppSyntaxKind::Declaration
                | CppSyntaxKind::ExpressionStat
                | CppSyntaxKind::ReturnStat
                | CppSyntaxKind::IfStat
                | CppSyntaxKind::ForStat
                | CppSyntaxKind::RangeForStat
                | CppSyntaxKind::WhileStat
                | CppSyntaxKind::DoWhileStat
                | CppSyntaxKind::SwitchStat
                | CppSyntaxKind::TryStat
                | CppSyntaxKind::ThrowStat
                | CppSyntaxKind::BreakStat
                | CppSyntaxKind::ContinueStat
                | CppSyntaxKind::CompoundStat
                | CppSyntaxKind::LabelStat
                | CppSyntaxKind::EmptyStat
        )
        .then_some(kind)
    }

    root.descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CompoundStat)
        .and_then(|body| {
            body.children()
                .filter_map(|child| statement_kind_of(CppSyntaxKind::from(child.kind())))
                .last()
        })
        .or_else(|| {
            root.children()
                .next()
                .and_then(|node| statement_kind_of(CppSyntaxKind::from(node.kind())))
        })
}

/// Is there a node of this kind anywhere in the parsed fragment?
fn contains(source: &str, kind: CppSyntaxKind) -> bool {
    CppParser::parse(source, ParserConfig::default())
        .get_red_root()
        .descendants()
        .any(|node| CppSyntaxKind::from(node.kind()) == kind)
}

/// A construct whose statement kind is all that is asserted.
fn shape(construct: &'static str, place: Where, kind: CppSyntaxKind) -> Shape {
    Shape {
        construct,
        place,
        kind,
        expression: None,
    }
}

/// A construct that must come out as `kind` **and** contain an expression of `expression`.
fn requiring(shape: Shape, expression: CppSyntaxKind) -> Shape {
    Shape {
        expression: Some((expression, Presence::Required)),
        ..shape
    }
}

/// A construct that must come out as `kind` and must **not** contain an expression of `expression` — for the
/// tokens that look like an operator but belong to the construct around them.
fn forbidding(shape: Shape, expression: CppSyntaxKind) -> Shape {
    Shape {
        expression: Some((expression, Presence::Forbidden)),
        ..shape
    }
}

#[track_caller]
fn assert_statement_kind(shapes: &[Shape]) {
    let wrong: Vec<String> = shapes
        .iter()
        .filter_map(|shape| {
            let source = match shape.place {
                Where::File => shape.construct.to_string(),
                Where::Body => format!("void probe() {{ {} }}", shape.construct),
                Where::Class => format!("struct Probe {{ {} }};", shape.construct),
            };

            match statement_kind(&source) {
                Some(kind) if kind != shape.kind => Some(format!(
                    "  {}\n      read as {kind:?}, must be {:?}",
                    shape.construct, shape.kind
                )),
                None => Some(format!(
                    "  {}\n      no statement node at all",
                    shape.construct
                )),
                Some(_) => match shape.expression {
                    None => None,
                    Some((want, Presence::Required)) if contains(&source, want) => None,
                    Some((want, Presence::Required)) => {
                        Some(format!("  {}\n      no {want:?} in it", shape.construct))
                    }
                    Some((unwanted, Presence::Forbidden)) if contains(&source, unwanted) => {
                        Some(format!(
                            "  {}\n      contains {unwanted:?}, which belongs to something else",
                            shape.construct
                        ))
                    }
                    Some(_) => None,
                },
            }
        })
        .collect();

    assert!(
        wrong.is_empty(),
        "{} of {} constructs were read as the wrong node:{}",
        wrong.len(),
        shapes.len(),
        wrong
            .iter()
            .map(|failure| format!("\n{failure}"))
            .collect::<String>()
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
            // `alignas`, which is a specifier whose payload is a constant-expression *or* a type-id.
            "alignas(16) struct Aligned { int x; };",
            "alignas(16) int aligned_global;",
            "alignas(int) int aligned_to_int;",
            "alignas(16) alignas(32) int doubly_aligned;",
            "struct Aligned2 { alignas(16) int member; };",
            "alignas(16) Aligned2 aligned_object;",
            "void aligned_param(alignas(16) int x);",
            "alignas(16) char aligned_buffer[64];",
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
            // A **braced-init-list** where the grammar says *initializer-clause* rather than *expression*: the
            // right-hand side of an assignment and an argument of a call. Both were reached only after the
            // empty-declarator defect was fixed, because `x = {1};` used to be read — silently, and wrongly — as
            // a declaration of a variable whose declarator named nothing. See `expressions.rs`.
            "x = {1};",
            "x = {1, 2};",
            "x = {};",
            "x += {1};",
            "v.push_back({1, 2});",
            "f({1});",
            "f(a, {1});",
            // A **requires-expression**, which is one of the three places a braced-init-list is *not* read: a
            // `{` inside a constraint opens a requirement.
            "auto r = requires { g(); };",
            "if constexpr (requires { g(); }) { }",
            "static_assert(requires { g(); });",
            // A parenthesised variable is an expression, not a cast — the shape that made the cast rule
            // precise rather than eager.
            "x = (a);",
            "x = (a + b);",
            "x = ((a));",
            "x = a * (b + c);",
            // The **comma operator**, in the positions where a comma really is an operator rather than
            // punctuation. The container positions are pinned by `a_comma_in_a_container_is_still_a_separator`
            // in `operators.rs`, because those are the ones this could take away.
            "auto x = (a, b);",
            "x = 1, y = 2;",
            "return 1, 2;",
            "for (a = 0, b = 0; ; ) { }",
            "g(a, (b, c));",
            // The **C-style cast**. `(T*)p` needs no type table, because a `*` that closes the parentheses has
            // no right operand and therefore cannot be a multiplication. The bare-name form, `(MyType)1.5`, is
            // the half that stays a documented trade-off and lives in the list below.
            "auto d = (T*)p;",
            "auto d = (MyType*)p;",
            "auto d = (T&)x;",
            "auto d = (ns::T*)p;",
            "auto d = (T<int>*)p;",
            "auto d = (const T*)p;",
            "auto d = (T*)p->q;",
            "auto d = (T*)p + 1;",
            "g((T*)p, (U*)q);",
            // **Alternative operator spellings** — the `<iso646.h>` names, which are real C++ rather than an
            // extension. They arrive as identifiers, because the lexer has no keyword for them.
            "auto x = a and b;",
            "auto x = a or b;",
            "auto x = not a;",
            "auto x = a bitand b;",
            "auto x = a bitor b;",
            "auto x = a xor b;",
            "auto x = compl a;",
            "auto x = a not_eq b;",
            "a and_eq b;",
            "a or_eq b;",
            "a xor_eq b;",
            "if (a and b) { }",
            "while (not done) { }",
            // **throw-expressions**: `throw` where a *value* is expected, as opposed to the statement form. The
            // two are different nodes; see `modern.rs`.
            "x = throw 1;",
            "auto y = cond ? throw 1 : 2;",
            "return throw 1;",
            "g(throw 1);",
            // **Explicit instantiation declarations**, which are `extern template`, not a linkage specification.
            "extern template struct S<int>;",
            "extern template class C<int>;",
            "extern template void f<int>(int);",
            // **Inline namespaces**, whose members are also members of the enclosing namespace.
            "inline namespace v1 { }",
            "inline namespace v1 { int x; }",
            "inline namespace v1 = a::b;",
            // **`decltype` in type position**, in every position a type can be written. The tricky part is not
            // the payload but what follows it: a `)` ends the specifier, and the declarator's name comes after.
            "decltype(x) y;",
            "decltype(x) y = 1;",
            "decltype(auto) y;",
            "decltype(auto) x = f();",
            "decltype(x)* p;",
            "decltype(x) v[2];",
            "noexcept(f()) g();",
            "const decltype(x) y = 1;",
            // **`concept` and `requires`** — the largest single block of missing grammar. Four constructs share
            // the one word: a constrained template parameter, a requires-clause after the template parameter
            // list, the same clause after a declarator, and a requires-expression, which is a primary expression
            // and can therefore be nested anywhere an expression can.
            "template <typename T> concept C = true;",
            "template <typename T> concept C = requires(T t) { t.f(); };",
            "template <typename T> concept C = C2<T> && C3<T>;",
            "template <C T> void f(T t);",
            "template <C<T> U> void f(U u);",
            "template <typename T> requires C<T> void f(T t);",
            "template <typename T> requires C<T> && C2<T> void f(T t);",
            "template <typename T> requires (C<T>) void f(T t);",
            "template <typename T> void f(T t) requires C<T>;",
            "template <typename T> void f(T t) requires C<T> { }",
            "template <typename T> void f(T t) requires requires(T t) { t.f(); };",
            "void f() requires true;",
            // The clause **after a trailing return type**, which is the only order the standard allows: the
            // clause belongs to the init-declarator, so it follows the whole declarator, the `-> T` included.
            "template <typename T> auto g(T t) -> int requires C<T>;",
            // A parenthesised constraint — which the standard *requires* around anything that is not a
            // conjunction of primary expressions: `requires (N > 0)` is valid and `requires N > 0` is not.
            "template <typename T> requires (sizeof(T) > 1) && (sizeof(T) < 64) void h(T t);",
            "template <int N> requires ((N >> 1) > 0) void h();",
            "template <typename T> void f(T t) requires C<T> && C2<T> { }",
            // A function whose parameter is **unnamed**: `void f(T)`, not `void f(T t)`. The same tokens as a
            // direct-initialised variable, and at file scope the variable reading used to win — silently, since
            // a variable declaration with a direct initialiser is perfectly well formed. See `direct_init.rs`.
            "void f(T);",
            "void f(T) { }",
            "template <typename T> void f(T) { }",
            "template <typename T> void f(T) requires C<T> { }",
            "void f(std::vector<T>);",
            // The same two words used as **ordinary names**, which is what says they are contextual keywords
            // rather than keywords: the lexer hands both over as identifiers and the grammar asks for the spelling
            // where the standard gives it a meaning. See `concepts.rs`.
            "int requires = 1;",
            "int concept = 2;",
            "void requires();",
            "void f(int requires, int concept);",
            "struct S { int requires; int concept; };",
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
            // The same shape with a *declarator* after it rather than another whole declaration, which is the
            // half the first pair does not reach: `Probe& method();` is a type, a reference and a name, and the
            // specifier sequence has to know that the type was named even though the friend consumed it.
            "friend void swap(Probe&, Probe&); Probe& method();",
            "friend class Other; Probe& method2();",
            // An **explicit object parameter** (C++23). It was a *silent* gap: the member came out as nothing
            // and the tokens after the `this` were read as a second, phantom member, with no diagnostic — which
            // is why this list could not see it and a census of common C++ found it.
            "void f(this Probe& self);",
            "void f(this Probe&& self) &&;",
            "void f(this auto&& self) {}",
            "void g(this Probe& self, int x);",
            "void h(this Probe& self) const;",
            "void i(this Probe&);",
            "void j(this Probe& self) { self.x = 1; }",
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
            // A C-style cast whose type is a **plain undeclared name**, which is the residue of what used to be
            // the whole of T1. `(MyType)` is a valid parenthesised expression *and* a valid type-id, and only
            // name lookup tells them apart — the same trade as direct-initialisation, in the same direction.
            //
            // The whole *pointer* form used to be here as well, described as "the canonical example of a
            // deliberate trade-off: `*` is both the pointer operator and the multiplication operator, and the
            // type table is what tells them apart". That claim was wrong twice over:
            //
            //   `(a * b)` and `(MyType*)p` are not the same shape — `a * b` has an operand on both sides of the
            //   `*`, `MyType*` has nothing on its left — so the difference **is** visible in the tokens; and
            //
            //   a `*` immediately before the `)` cannot be a binary operator at all, because a binary operator
            //   needs a right operand.
            //
            // So the pointer form was a missing **rule** and not a missing type table. It reads now — see
            // `closes_with_a_pointer_operator` in `exprs.rs` and the entries in the list above. What is left
            // here is the one case where the tokens really are silent.
            (
                "auto d = (MyType)1.5;",
                "a C-style cast to an undeclared type. `(MyType)` is a valid parenthesised expression and a \
                 valid type-id, and only name lookup tells them apart.",
            ),
            (
                "auto d = (MyType)x;",
                "the same ambiguity with a name as the operand.",
            ),
        ],
    );

    // `decltype` in **type position** used to be here, and the record of what it looked like is worth keeping
    // because the symptom pointed at the wrong thing entirely:
    //
    //   `decltype(x) y;`          read
    //   `decltype(x) y = 1;`      did NOT — "expected primary expression" against the `decltype`
    //   `decltype(auto) x = f();` did NOT
    //
    // Two defects were stacked, and neither was where the message pointed:
    //
    //   1. `decltype` was not a declaration anchor, so the declaration reading was never taken from it at all;
    //   2. once it was, the specifier loop asked "did this specifier name a type?" of the **last token it
    //      consumed** — which for `decltype(a)` is the `)` of its payload, the very token `alignas(16)` ends on
    //      and the one that question exists to answer *no* for. So the type was left unfinished as far as the
    //      loop was concerned, the **declarator's own name** was taken into the type, the declaration came out
    //      with no declarator, and the statement fell back to an expression. The initializer had nothing to do
    //      with it.
    //
    // Both are fixed; the entries are in the list above, and `modern.rs` pins the shapes. What remains is the
    // **expression** side, which is the one place the tokens really are silent:
    //
    //   `decltype(x);`      refuses — a `decltype` is not a value
    //   `decltype(x) + 1;`  refuses — likewise
    //
    // Those are refused rather than misread, which is the cheaper direction and the one this file registers.

    // The record of how the pointer form was resolved, kept because the *reason* it was mis-filed for so long
    // is the useful part: it was judged by how hard it looked rather than by whether it needed information from
    // outside the file.
    //
    //   `auto d = (MyType*)p;`   — reads: the `*` closes the parentheses, so it is a type
    //   `auto d = (T*)p;`        — likewise
    //   `auto d = (int)1.5;`     — reads through the keyword-type path
    //   `auto d = (a * b);`      — reads as a multiplication, and must keep doing so
    //   `auto d = (a* b);`       — likewise

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

    // `alignas` used to be here, with the note that "the specifier loop has no branch for it, so the `(` is read
    // as an expression and the declaration reading is refused".
    //
    // What it actually needed was three things, and only the first was the missing branch: the specifier itself
    // (`AlignasSpec`, holding a payload that is a constant-expression *or* a type-id), `alignas` as a declaration
    // anchor, and — the one that was not obvious — the specifier loop's "has a specifier been seen?" flag split
    // into "has a **type** been seen?", because `alignas(16) MyType value;` consumes a specifier and names no
    // type. Without the split the name was read as a second word of a finished type.

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

/// What the parser read each construct **as**, not merely whether it read it.
///
/// The third question, and the one the other two are blind to. See [`Shape`] for why a well-formed, lossless,
/// diagnostic-free tree can still be the wrong construct, and for the case that was found that way.
///
/// The entries below are the statement kinds that matter to a consumer. They are deliberately not "every
/// statement parses": each line is a reading that a defect could silently change, so the list grows when a new
/// ambiguity is settled rather than when a new construct is added.
#[test]
fn constructs_are_read_as_the_right_node() {
    assert_statement_kind(&[
        // The defect this list exists for. An assignment to a name the file has no type for used to come out as
        // a `Declaration` whose declarator named nothing — the same reading as `int x = 1;`, which is a
        // *declaration*, so the two had to be told apart by whether an initialiser has something to initialise.
        shape("x = 1;", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("x = a + b;", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("value = other;", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("x = f();", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("x = a ? b : c;", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("x = { 1 };", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("x = { 1, 2 };", Where::Body, CppSyntaxKind::ExpressionStat),
        // Two template-ids joined by `&&`. The declaration reading is available — `C<T>` is a type and `&&` an
        // rvalue reference — and it used to win, producing a *declaration* whose declarator was named `C2<T>`:
        // well formed, lossless, and no diagnostic. A declarator's name cannot have template arguments, which is
        // what settles the reading. See `expressions.rs`.
        shape("C<T> && C2<T>;", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("x = 1;", Where::File, CppSyntaxKind::ExpressionStat),
        // The other side of the same gate: a declaration *is* a declaration, and a qualified declarator keeps
        // its declaration reading even though it names nothing for the declarator rule to take — the name was
        // folded into the type.
        shape("int x = 1;", Where::Body, CppSyntaxKind::Declaration),
        shape(
            "int ns::count = 0;",
            Where::File,
            CppSyntaxKind::Declaration,
        ),
        shape(
            "int ns::Widget::count = 0;",
            Where::File,
            CppSyntaxKind::Declaration,
        ),
        // Direct-initialisation, the reading the whole file-local type table exists for.
        shape("Widget w(1, 2);", Where::Body, CppSyntaxKind::Declaration),
        shape(
            "std::string s(\"x\");",
            Where::Body,
            CppSyntaxKind::Declaration,
        ),
        // A call keeps its parentheses, which is the direction a naive fix breaks.
        shape("g(1, 2);", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("use(x);", Where::Body, CppSyntaxKind::ExpressionStat),
        // Statements that are not declarations at all, pinned so that a future gate cannot claim them.
        shape("++i;", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("delete p;", Where::Body, CppSyntaxKind::ExpressionStat),
        shape("if (x) { }", Where::Body, CppSyntaxKind::IfStat),
        shape("for (;;) { break; }", Where::Body, CppSyntaxKind::ForStat),
        shape("return;", Where::Body, CppSyntaxKind::ReturnStat),
        // **The comma operator**, on both sides. The required half is that the comma really is an operator
        // where it is an operator; the forbidden half is the defect it could cause — a container's commas read
        // as one expression, so `g(a, b)` becomes a call with one argument. The statement kind is the same in
        // both readings, which is exactly why the expression assertion exists.
        //
        // `auto x = (a, b);` would *not* do for these: `auto` is a type keyword, so the whole thing is a
        // declaration and the statement kind says nothing about the comma. The parenthesised form is used
        // instead, where the expression *is* the statement.
        requiring(
            shape("(a, b);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::BinaryExpr,
        ),
        requiring(
            shape("x = 1, y = 2;", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::BinaryExpr,
        ),
        requiring(
            shape("return 1, 2;", Where::Body, CppSyntaxKind::ReturnStat),
            CppSyntaxKind::BinaryExpr,
        ),
        forbidding(
            shape("g(a, b);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::BinaryExpr,
        ),
        forbidding(
            shape("g(a, b, c);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::BinaryExpr,
        ),
        // **The C-style cast**, on both sides. `(T*)p` is a cast because a `*` that closes the parentheses has
        // no right operand; `(a * b)` is a product because its `*` has one. Both parse, so only the shape can
        // tell them apart.
        requiring(
            shape("(T*)p;", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::CastExpr,
        ),
        requiring(
            shape("(MyType*)p;", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::CastExpr,
        ),
        forbidding(
            shape("(a * b);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::CastExpr,
        ),
        forbidding(
            shape("(a* b);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::CastExpr,
        ),
        // `(a)` parses as a type-id — one name, no declarator — so a rule that tried the type reading whenever
        // it could would turn every parenthesised variable into a cast.
        forbidding(
            shape("(a);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::CastExpr,
        ),
        // And a call of a parenthesised name keeps its arguments, which is the price of the bare-name cast
        // staying unread.
        forbidding(
            shape("(f)(x);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::CastExpr,
        ),
        // **throw** has two forms and they are different nodes. A consumer looking for one must not find the
        // other, and the statement form would look plausible in either position.
        requiring(
            shape("x = throw 1;", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::ThrowExpr,
        ),
        requiring(
            shape("throw 1;", Where::Body, CppSyntaxKind::ThrowStat),
            CppSyntaxKind::ThrowStat,
        ),
        forbidding(
            shape("x = throw 1;", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::ThrowStat,
        ),
        // An **alternative spelling** is the operator, so the node is the one the punctuation would produce.
        requiring(
            shape("auto x = a and b;", Where::Body, CppSyntaxKind::Declaration),
            CppSyntaxKind::BinaryExpr,
        ),
        requiring(
            shape("auto x = not a;", Where::Body, CppSyntaxKind::Declaration),
            CppSyntaxKind::UnaryExpr,
        ),
        // A name that merely looks like a spelling stays a name.
        forbidding(
            shape("auto x = android;", Where::Body, CppSyntaxKind::Declaration),
            CppSyntaxKind::BinaryExpr,
        ),
        // `this` as an **argument** is an expression; only a parameter position makes it a type.
        forbidding(
            shape("g(this);", Where::Body, CppSyntaxKind::ExpressionStat),
            CppSyntaxKind::Parameter,
        ),
        // `decltype` in type position is a **declaration**, and the tricky half is that the spelling alone does
        // not say so — `decltype(x) + 1;` is an expression. The anchor asks whether a declarator follows.
        requiring(
            shape(
                "decltype(x) y = 1;",
                Where::File,
                CppSyntaxKind::Declaration,
            ),
            CppSyntaxKind::DeclSpecifierSeq,
        ),
        requiring(
            shape("decltype(auto) y;", Where::File, CppSyntaxKind::Declaration),
            CppSyntaxKind::DeclSpecifierSeq,
        ),
    ]);
}

/// A construct that must contain a node of `kind`, with the statement kind it must have.
#[track_caller]
fn assert_fragment_contains(fragments: &[(&str, CppSyntaxKind)]) {
    let missing: Vec<String> = fragments
        .iter()
        .filter(|(fragment, kind)| !contains(&format!("void probe() {{ {fragment} }}"), *kind))
        .map(|(fragment, kind)| format!("  {fragment}\n      no {kind:?} in it"))
        .collect();

    assert!(
        missing.is_empty(),
        "{} of {} fragments are missing a node:{}",
        missing.len(),
        fragments.len(),
        missing
            .iter()
            .map(|failure| format!("\n{failure}"))
            .collect::<String>()
    );
}

/// The node kinds the C++23 and C++20 constructs must produce, where the *statement* kind says nothing.
///
/// A parameter, a namespace and a `this` are not statements, so [`assert_statement_kind`] cannot reach them —
/// and the two that matter most here are exactly the ones a wrong reading would keep well formed:
///
/// * an **explicit object parameter** was silently read as *nothing*, with the tokens after the `this` forming a
///   phantom second member. The parameter count is the assertion that catches it;
/// * an **alternative spelling** must be the operator rather than a name, and the difference between `a and b`
///   and `a android` is one the shape has to see.
#[test]
fn modern_constructs_produce_the_right_nodes() {
    assert_fragment_contains(&[
        ("void f(this S& self);", CppSyntaxKind::Parameter),
        ("void f(this S& self);", CppSyntaxKind::ThisExpr),
        ("void f(this auto&& self);", CppSyntaxKind::Parameter),
        // The statement and the expression form, in the two positions they are written.
        ("throw 1;", CppSyntaxKind::ThrowStat),
        ("x = throw 1;", CppSyntaxKind::ThrowExpr),
        ("auto y = cond ? throw 1 : 2;", CppSyntaxKind::ThrowExpr),
        // An inline namespace is a namespace, not a declaration of something called `namespace`.
        ("inline namespace v1 { }", CppSyntaxKind::NamespaceDecl),
        // `concept` and `requires`: the declaration, the clause and the expression are three different nodes,
        // and the *clause* is the one a wrong reading keeps well formed. `RequiresExpr` is a primary expression,
        // so the three places it can be written are three different rules reaching the same node.
        (
            "template <typename T> concept C = true;",
            CppSyntaxKind::ConceptDecl,
        ),
        (
            "template <typename T> requires C<T> void f(T t);",
            CppSyntaxKind::RequiresClause,
        ),
        (
            "template <typename T> void f(T t) requires C<T> { }",
            CppSyntaxKind::RequiresClause,
        ),
        // The clause after a trailing return type, and one after a template head on a class. A clause after a
        // *class head* is deliberately not read: the grammar gives it none, and reading one used to hand the
        // class body to the statement rule — see `concepts.rs`.
        (
            "template <typename T> auto g(T t) -> int requires C<T>;",
            CppSyntaxKind::RequiresClause,
        ),
        (
            "template <typename T> requires C<T> struct S { };",
            CppSyntaxKind::RequiresClause,
        ),
        (
            "template <typename T> concept C = requires(T t) { t.f(); };",
            CppSyntaxKind::RequiresExpr,
        ),
        ("auto r = requires { g(); };", CppSyntaxKind::RequiresExpr),
        (
            "auto r = requires(T t) { t.f(); };",
            CppSyntaxKind::Requirement,
        ),
        // A braced-init-list in *expression* position, which is an `InitListExpr` and not the `Initializer` a
        // declaration produces. `x = {1};` was a silent wrong tree until the empty-declarator defect was fixed,
        // and the two nodes are how a consumer tells "a declaration initialised with braces" from "an assignment
        // of a braced-init-list".
        ("x = {1};", CppSyntaxKind::InitListExpr),
        ("v.push_back({1, 2});", CppSyntaxKind::InitListExpr),
        ("int x = {1};", CppSyntaxKind::Initializer),
        // An **unnamed parameter** is a parameter, and the count is the assertion: the variable reading produced
        // zero of them and a well-formed declaration instead. See `direct_init.rs`.
        ("void f(T);", CppSyntaxKind::Parameter),
        (
            "template <typename T> void f(T) { }",
            CppSyntaxKind::ParameterList,
        ),
        (
            "template <typename T> void f(T) { }",
            CppSyntaxKind::CompoundStat,
        ),
        // The two constraint words as ordinary names: a call is a call and an assignment is an expression, with no
        // construct in either. A keyword token made all four of these unreadable. See `concepts.rs`.
        ("void f() { requires(); }", CppSyntaxKind::CallExpr),
        ("void f() { concept(); }", CppSyntaxKind::CallExpr),
        ("void f() { concept = 2; }", CppSyntaxKind::ExpressionStat),
        ("void f() { requires = 1; }", CppSyntaxKind::ExpressionStat),
    ]);

    // Exactly one parameter: the silent version produced zero here and a phantom member beside it.
    let source = "struct Probe { void f(this S& self); };";
    assert_eq!(
        CppParser::parse(source, ParserConfig::default())
            .get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Parameter)
            .count(),
        1,
        "an explicit object parameter is one parameter"
    );

    // And exactly one clause: a clause read as ending at its first term leaves the rest of the constraint to be
    // read by whatever follows, which is well formed and wrong. See `concepts.rs`.
    let source = "template <typename T> requires C<T> && C2<T> void f(T t);";
    assert_eq!(
        CppParser::parse(source, ParserConfig::default())
            .get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::RequiresClause)
            .count(),
        1,
        "a conjunction of two concepts is one clause"
    );
}
