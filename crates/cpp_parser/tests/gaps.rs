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

use cpp_parser::{CppParser, CppSyntaxKind, CppSyntaxTree, CppTokenKind, ParserConfig};

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

/// A macro **from a header** can stand where a declaration goes, and only where nothing else can.
///
/// The shape that made this necessary is `namespace std _GLIBCXX_VISIBILITY(default) {`, which opens **every**
/// libstdc++ header, and `_GLIBCXX_BEGIN_NAMESPACE_VERSION` on a line of its own inside the body. Neither name is
/// in any table the parser can be handed — the `#define`s are in `bits/c++config.h`, an *included* file — so the
/// shape is what decides, and it decides only where it has no competitor. Measured on the closure of six standard
/// headers: 48 of 185 files read cleanly before these two rules, 78 after (`docs/std-library.md`).
///
/// The negative assertions are the point of the test. Each is a shape that looks similar and must keep the
/// reading it already had:
///
/// ```text
/// x = 1;                the `=` continues an expression — the first version of the rule read this as a macro
/// FOO(x);               the most vexing parse: a declaration, and the `;` is what says so
/// TEST(A, B) { … }      a definition whose body is the block
/// COUNT (in a body)     a missing `;`, which must stay an error
/// ```
///
/// The first and the fourth are the two mistakes the rule was rewritten to avoid, so they are pinned here rather
/// than left to the rules that own them: widening this one again is exactly what would break them.
#[test]
fn a_macro_from_a_header_can_stand_where_a_declaration_goes() {
    // The namespace head, which is the shape in every standard header.
    let header = "namespace std _GLIBCXX_VISIBILITY(default)\n{\n  struct Widget { int size; };\n}\n";
    assert!(
        contains(header, CppSyntaxKind::NamespaceDecl),
        "the namespace is still a namespace"
    );
    assert!(
        contains(header, CppSyntaxKind::CompoundStat),
        "and it still opens a body — the macro is between the name and the brace, not instead of either"
    );
    assert!(
        CppParser::parse(header, ParserConfig::default())
            .get_errors()
            .is_empty(),
        "and the head no longer costs the whole file"
    );

    // A bare invocation standing for a declaration, with and without arguments.
    let bare = "_GLIBCXX_BEGIN_NAMESPACE_VERSION\ntemplate <typename T> struct S { T x; };\n";
    assert!(contains(bare, CppSyntaxKind::MacroCall));
    assert!(
        CppParser::parse(bare, ParserConfig::default())
            .get_errors()
            .is_empty(),
        "and what follows it is read as the declaration it is"
    );
    assert!(contains(
        "_GLIBCXX_BEGIN_INLINE_ABI_NAMESPACE(__cxx11)\nint x;\n",
        CppSyntaxKind::MacroCall
    ));

    // The shapes that must keep their reading.
    assert!(
        !contains("x = 1;\n", CppSyntaxKind::MacroCall),
        "an assignment: no declarator follows an `=`, and asking about declarators is what read this as a macro"
    );
    assert!(
        contains("FOO(x);\n", CppSyntaxKind::Declaration),
        "the most vexing parse is a declaration, and the `;` is what says so"
    );
    assert!(
        contains("TEST(A, B) { int x = 1; }\n", CppSyntaxKind::Declaration),
        "a macro used as a definition: the block is the declaration's body"
    );
    assert!(
        !contains("void f() { COUNT\n  return; }\n", CppSyntaxKind::MacroCall),
        "inside a body a missing `;` is the likelier story, and an error is the honest answer"
    );
    assert!(
        !contains("void f() { FOO(x) }\n", CppSyntaxKind::MacroCall),
        "and a call inside a body is a call, however it is spelled"
    );
}

/// The compiler's own attribute spellings are attributes, in every position the standard spelling works in.
///
/// `__attribute__((…))` and `__declspec(…)` mean what `[[…]]` means, and they are the *compiler's* extension
/// rather than the file's macro: the standard reserves both names, so matching them by spelling is not the
/// convention-without-evidence that `docs/grammar-gaps.md` entry 16 warns about — there is no `#define` anywhere
/// that could make them something else. libstdc++ writes them in positions a declaration cannot otherwise have
/// anything in: between a template head and the declaration it wraps, between `extern "C++"` and the declaration,
/// and after a parameter list.
///
/// The three positions are pinned separately because they are read by three different call sites — the template
/// head, the specifier sequence and the declarator's suffix — and one of them working says nothing about the
/// others.
#[test]
fn the_compilers_attribute_spellings_are_attributes() {
    let parses = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(
            tree.get_errors(),
            [],
            "this must parse cleanly: {source:?}"
        );
        tree
    };

    // Between the template head and the declaration it wraps — `bits/move.h`.
    let wrapped = parses("template <typename T>\n__attribute__((__always_inline__))\ninline T* addressof(T& r);\n");
    assert!(
        wrapped
            .get_red_root()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::AttributeList),
        "and it is an attribute node, not a name the specifier sequence took for a type"
    );

    // In the specifier sequence, after a linkage specification — `bits/c++config.h`.
    parses("extern \"C++\" __attribute__ ((__noreturn__, __always_inline__))\ninline void f() noexcept { }\n");

    // After a parameter list, where the standard spelling already worked.
    parses("void f() __attribute__ ((__noreturn__));\n");

    // The other extension, spelled the other way.
    parses("__declspec(dllexport) void g();\n");

    // And the run: two and three macro names before the declaration they decorate, which is how libstdc++ opens
    // a nested namespace and then a versioned one.
    parses("namespace n { }\n_GLIBCXX_BEGIN_NAMESPACE_VERSION\n_GLIBCXX_BEGIN_NAMESPACE_CONTAINER\ntemplate <typename> struct S;\n");

    // What the run must **not** swallow: a declaration whose type and name are both plain identifiers, and whose
    // declarator happens to carry a parameter list. The first version of the run scanner stepped one token too
    // far past a group and read this as two macro invocations, which left the `;` on the next declaration.
    let declaration = parses("namespace std {\nsize_t\n_Hash_bytes(const void* p);\n}\n");
    assert!(
        !declaration
            .get_red_root()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall),
        "`size_t _Hash_bytes(const void*);` is a declaration with a macro-looking type, not two macros"
    );
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
            // A **macro invocation used where a definition goes** — how gtest, Catch2 and every benchmark library
            // write a test. File scope (or a namespace) is what makes it that shape: inside a function body a call
            // followed by a block is a real error, and the rule refuses it there — so this list is the only one it
            // can be read from. The arguments are the macro's tokens, neither values nor declarators, which is why
            // the shape is the whole of the test.
            "TEST(A, B) { }",
            "TEST(FormatPerformance, 1k_row) { }",
            "TEST(A, B) { int x = 1; }",
            "namespace n { TEST(A, B) { } }",
            // A **macro from a header** in declaration position, which is the shape every libstdc++ header opens
            // with: the name is `#define`d in an *included* file, so no table the parser can be handed knows it and
            // the shape is what decides. File scope is where that works — see
            // `a_macro_from_a_header_can_stand_where_a_declaration_goes` for the shapes that must *not* be read
            // this way, and `docs/grammar-gaps.md` entry 23.
            "namespace std _GLIBCXX_VISIBILITY(default) { int x; }",
            "_GLIBCXX_BEGIN_NAMESPACE_VERSION\nint x;\n",
            "_GLIBCXX_BEGIN_INLINE_ABI_NAMESPACE(__cxx11)\nint x;\n",
            // A **run** of them, which is how libstdc++ opens a versioned namespace inside a container one.
            "_GLIBCXX_BEGIN_NAMESPACE_VERSION\n_GLIBCXX_BEGIN_NAMESPACE_CONTAINER\ntemplate <typename> struct S;\n",
            // The compiler's own attribute spellings, in the three positions the standard one works in. See
            // `the_compilers_attribute_spellings_are_attributes`.
            "template <typename T>\n__attribute__((__always_inline__))\ninline T* addressof(T& r);\n",
            "extern \"C++\" __attribute__ ((__noreturn__, __always_inline__))\ninline void f() noexcept { }\n",
            "void f() __attribute__ ((__noreturn__));\n",
            "__declspec(dllexport) void g();\n",
            // A **conditional handler**: a directive can land at either joint of a `try`, and the first one is the
            // one that used to break the statement — with `try` separated from its block, the block was not the
            // try's block at all, and the `catch` became a statement with no statement before it.
            "#if !defined(_DEBUG)\ntry\n#endif\n{\n    g();\n}\n#if !defined(_DEBUG)\ncatch (const E& e) {\n    h();\n}\n#endif",
            "void f() {\ntry {\n    g();\n}\n#if !defined(_DEBUG)\ncatch (const E& e) {\n#endif\n    h();\n}\n}",
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
            // no right operand and therefore cannot be a multiplication. The bare-name form, `(MyType)1.5`, was
            // the half that stayed a documented trade-off for longer — what retires it is the token *after* the
            // `)`: an operand there cannot follow an expression, so the parentheses held a type.
            "auto d = (T*)p;",
            "auto d = (MyType*)p;",
            "auto d = (T&)x;",
            "auto d = (ns::T*)p;",
            "auto d = (T<int>*)p;",
            "auto d = (const T*)p;",
            "auto d = (T*)p->q;",
            "auto d = (T*)p + 1;",
            "g((T*)p, (U*)q);",
            "auto d = (MyType)1.5;",
            "auto d = (MyType)x;",
            "auto d = (size_t)size;",
            "buf = (char *)malloc((size_t)size + 1);",
            "auto d = (MyType)new T;",
            "auto d = (MyType)sizeof(T);",
            // **Adjacent string literals**, which are one literal rather than a grammar rule — translation phase
            // 6. Reading one and stopping ended the initialiser after the first, so this is how every long message
            // wrapped across lines used to fail.
            "auto s = \"a\" \"b\";",
            "auto s = \"a\" \"b\" \"c\";",
            "g(\"a\" \"b\", 1);",
            // A **user-defined literal** in an expression: the lexer named the kind so the parser would not read
            // it as a plain number, and no rule was reading it at all.
            "auto x = 1_km;",
            "auto x = \"a\"_km;",
            // A `for` header's **step**, which is an expression: the declaration reading had no `;` to fail on
            // and consumed the name as a type, so `i++` was never read at all.
            "for (;; i++) { }",
            "for (;; i++, k++) { }",
            "for (i = 0, k = 0; i < n; i++, k++) { }",
            // A **`sizeof` whose operand is not a bare name**. The type reading was accepted as soon as it parsed
            // and consumed something, and a type-id can stop early — a name alone is a complete one — so the
            // cursor was left on the `[` and `expected )` was reported against it. Everything but a bare name and
            // a keyword type failed, which in C is most of the `sizeof`s there are.
            "auto n = sizeof(a[0]);",
            "auto n = sizeof(a.b);",
            "auto n = sizeof(a + b);",
            "auto n = sizeof(a());",
            "auto n = sizeof(int*);",
            "auto n = sizeof(unsigned long);",
            "auto n = typeid(a[0]);",
            // An **elaborated type specifier** in a type-id: `struct S` is one specifier, so the name after the
            // keyword belongs to the type even where a type-id allows only one name — which is why `sizeof(struct
            // S)` and `(struct S*)p` used to leave the name behind and never reach the `)`.
            "auto n = sizeof(struct S);",
            "auto n = sizeof(union U);",
            "auto n = (struct S*)p;",
            "using A = struct S;",
            // An **array type** in a type-id, and the same tokens as an index — the file's own table is what tells
            // them apart. Beside them the shapes that must stay expressions.
            "auto n = sizeof(int[4]);",
            "auto n = sizeof(char[256]);",
            "auto n = sizeof(int[2][3]);",
            "auto n = sizeof(int*[4]);",
            "auto n = sizeof(int[]);",
            "Vec<int[4]> v;",
            "auto v = Vec<int[4]>{};",
            "auto x = a[b < c];",
            // A **cv-qualifier after the type**: `char const w[]` used to read `w` into the type and turn the
            // declarator into a structured binding of nothing — silently — and `char const w[2]` reported it.
            "char const w[] = { 'a' };",
            "char const w[2] = { 'a' };",
            "int const x = 1;",
            "unsigned const int y = 1;",
            "struct S const s;",
            // **Qualified declarator names**: a definition's name is folded into the type, so its parentheses are
            // a parameter list and nothing else — with and without a storage specifier in front, and with a
            // template-id in the name. The questions that decide this used to be asked of the declaration's first
            // token, which a `static` pushes out of the way.
            "static void A::f<int>(int);",
            "inline void A::f<int>(int);",
            "extern void A::f<int>(int);",
            "static void A<int>::f<int>(int);",
            "static void A::f<int>(int) { }",
            "static void Widget::draw(T) { }",
            "void Widget::draw(T) { }",
            "void ns::C::method() { }",
            // `<` as a **comparison** rather than a template-id opener — the reading is speculative, and what
            // decides it is the token after the list (see `operators.rs`). `if (n < 0 || n > 100000)` is how a
            // range check is written, and the old lookahead paired the `<` of the first comparison with the `>`
            // of the second. Beside them: the template-ids that must keep their reading.
            "x = a < b > c;",
            "x = a < b || c > d;",
            "if (n < 0 || n > 100000) { }",
            "while (i < n && j > k) { }",
            "g(a < b, c > d);",
            "auto n = A<B>::value;",
            "g<int>(1);",
            "auto p = new A<B>();",
            "auto v = std::vector<int>{};",
            "std::vector<int> v;",
            "Foo<int> x;",
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
            // …and the same construct **without** `extern`, which the standard writes as one production:
            // `explicit-instantiation: extern(opt) template declaration`. A function's template-id is its *name*,
            // a class's is its *head*, and a variable template's is its name again.
            "template void f<int>(int);",
            "template void f<int>(int) { }",
            "template class C<int>;",
            "template struct S<int>;",
            "template int v<int>;",
            // **Explicit specializations**, whose empty head is what tells them from a template declaration. The
            // declaration they introduce names a specialization, so its declarator is a template-id too.
            "template <> void f<int>(int);",
            "template <> int v<int>;",
            "template <> struct S<int>;",
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
            // …and a comparison **inside parentheses** in a clause, where the operand after the `>` belongs to
            // the constraint rather than to the declaration that follows the clause.
            "template <int N> requires (N < 0 || N > 3) void f();",
            "template <int N> requires (N > 0) void f();",
            "template <typename T> requires (C<T>) T value = T{};",
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
            // A **K&R (old-style) function definition**, whose parameters are declared *after* the parenthesis
            // rather than inside it. It is C's spelling from before 1989, and the file that found it is a CMake
            // compiler-id probe — generated into every CMake build tree, so it is not an exotic file. Both the
            // definition and the body-less head are listed, because a conditional can put the head in one branch
            // and the body in neither; `old_style.rs` owns the shapes, this list only says that they are read.
            "int main(argc, argv)\nint argc;\nchar *argv[];\n{ return argc; }",
            "int main(argc, argv)\nint argc;\nchar *argv[];",
            "int f(a)\nint a;\nfloat b;\n{ return a; }",
            // Enumerators **with initializers**, which is where the enumerator list is really a list: a reader
            // that takes the comma swallows the members after the first initialised one (B26), and an enum whose
            // members have no initializers cannot see that. `enum Color { Red, Green }` was the example for years.
            "enum E { A = 0, B };",
            "enum E { A = 0, B, };",
            "enum class E : unsigned { A = 1, B = 2, C };",
            "enum E { A = 1 << 2, B = f(1, 2), C };",
            // A **byte-order mark**: not a construct, but the first character of a file Visual Studio saved, and
            // it used to be the first *diagnostic* of that file too (37 of 200 files in a real project).
            "\u{feff}int x;",
            // An **export macro** in front of the type: two names where a declaration has its type, and the
            // declarator is the third. `ast.rs` pins the name it comes out with; this only says it is read.
            "MY_API Widget *p;",
            "EMMY_API Result f(int x);",
            "EXPORT std::string g();",
            // Two closers in one token. The lexer glues `>>` together (`a >> b` is a shift), so a nested
            // template-id ends with a token that closes **two** lists — and the empty argument list
            // `std::less<>` has no argument to read before the closer arrives, which is why it failed while the
            // non-empty spelling worked. Any scan that counts angles has to answer for `>>`; see
            // `angle_depth_delta`.
            "class D : public Base<K, std::shared_ptr<V>> { };",
            "class D : public std::vector<std::pair<K, V>> { };",
            "void f(const std::map<int, int, std::less<>> &m);",
            "std::map<int, int, std::less<>> &Get();",
            "void f(std::map<int, std::less<int>> m);",
            // **Pointers to members**, in every spelling: the operator after the type (`C::*`), the parenthesised
            // form with a name, the abstract form a parameter uses, and the expression operators. `gaps.rs` also
            // pins the *shape* below, because the unparenthesised form used to parse with the declarator nested
            // inside the type — lossless, well formed, no diagnostic, wrong.
            "int C::*p;",
            "typedef int (C::*fp)(int);",
            "void g(int (C::*h)(int));",
            "void h(int (C::*)(int));",
            "struct C { };\nvoid f(C* c) { (c->*h)(1); }",
            "struct C { };\nvoid f(C& c) { (c.*h)(1); }",
            // The literal suffixes those arguments use — `1k_row` is one token, not a number and a name. The
            // macro-definition shape itself is in the file-scope list above: inside a body it is a call followed
            // by a block, which is a real error and is refused on purpose.
            "auto x = 1k_row;",
            // A **macro whose body is a whole statement**, invoked without a `;`. The `#define` in the fragment is
            // what makes it a macro rather than a call whose `;` is missing — `macros.rs` owns both halves of that
            // test, and the negative half is pinned in the list of what is not read yet.
            "#define NUMBER_OPTION(op) if (auto v = Get(op); !v.empty()) { }\nNUMBER_OPTION(tab_width)",
            "#define IF_EXIST(op) if (!Get(op).empty())\nIF_EXIST(a) { g(); }",
            // A **macro invocation used where a definition goes, inside a function body** — `IF_EXIST(k) { … }`,
            // which is how a "set this option if it is configured" block is written. Inside a body the same shape
            // is also a real mistake (a call whose `;` is missing, followed by a block), so the reading is taken
            // only for a name spelled the way a macro is — see the negative pin in the list of what is not read
            // yet, which is the other half of this one.
            "IF_EXIST(k) { g(); }",
            "TEST(A, B) { g(); }",
            // A **function type as a template argument** — a predicate parameter, in a declaration, an alias and a
            // `sizeof`. The angle-matching scan stopped at the first `)`, so the template-id was never attempted:
            // `A<bool(T)> x;` came out as the *comparison* `A < bool(T) > x`, with no diagnostic at all.
            "A<bool(T)> x;",
            "std::function<bool(TokenKind)> pred;",
            "using F = A<bool(T)>;",
            "auto n = sizeof(A<bool(T)>);",
            "void h(A<bool(T)> x);",
            // A **class-like definition followed by its declarator** — `struct S { … } x;`, and the C idiom
            // `static const struct { … } name[] = { … };`. The body is a complete type, but the backward walk for
            // "is the type complete?" only saw the `}`, so the declarator's name joined the *type* instead: the
            // declaration declared nothing, silently, and reported `expected a declarator name` as soon as the
            // declarator carried anything (`x = { 1 }`, `x[2]`).
            "struct S { int a; } x;",
            "struct { int a; } x[] = { { 1 } };",
            "static const struct { unsigned char left; } priority[] = { { 1 } };",
            "typedef struct { int a; } Alias;",
            "enum E { A } e;",
            "class C { int a; } c = {};",
            // A **global-qualified name in parentheses** is an expression; the `::` used to make it a cast type.
            "auto y = (::x);",
            "auto y = (::abs(x) > 1);",
            // A **functional-notation conversion with a keyword type** — `bool(x)`, the other way of writing
            // `(bool)x`.
            "auto y = bool(x);",
            "g(\"key\", bool(lint[\"codeStyle\"]));",
            "auto timeout = 100ms;",
            "auto text = \"name\"sv;",
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
            // Inside a **member function's body** the class body is no longer the innermost brace, and a `:` there
            // is never a bit-field's width: a range-`for`'s separator and a label were both read as widths, which
            // cost a whole header in a real project.
            "void f() { for (auto &v: vec) { } }",
            "void f() { again: g(); }",
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
            // Nothing is left of the C-style cast that used to be here, and the record of why is worth keeping —
            // it is the third time a "deliberate trade-off" turned out to be a missing rule.
            //
            // The **pointer form** was here first, described as "the canonical example of a deliberate trade-off:
            // `*` is both the pointer operator and the multiplication operator, and the type table is what tells
            // them apart". That claim was wrong twice over:
            //
            //   `(a * b)` and `(MyType*)p` are not the same shape — `a * b` has an operand on both sides of the
            //   `*`, `MyType*` has nothing on its left — so the difference **is** visible in the tokens; and
            //
            //   a `*` immediately before the `)` cannot be a binary operator at all, because a binary operator
            //   needs a right operand.
            //
            // The **bare-name form** — `(MyType)1.5`, `(MyType)x` — was the second half of the same entry, and
            // the same argument retires it: what *follows* the `)` decides. Two operands in a row is not an
            // expression in any grammar, so `)` followed by an identifier, a literal or a prefix keyword means the
            // parentheses held a type. It was found in a real C file, where `(size_t)size` is not exotic but
            // routine.
            //
            // What genuinely remains is the token that means something in *both* grammars, and it is not a gap:
            // `(a)*p` is a multiplication, `(a)-b` a subtraction, `(a)(b)` a call. All are valid expressions, so
            // the cast reading would take working code apart. See `an_operand_after_the_parentheses_decides_the_
            // cast_and_nothing_else_does` in `operators.rs`.
        ],
    );

    // …and the shapes that are **still errors**, which is the other half of the macro reading: inside a
    // function body a call whose `;` is missing, followed by a block, is not a macro definition — the name is
    // not spelled like one — and reporting it is the point. The convention buys the macro case and keeps the
    // typo.
    assert_does_not_read_yet(
        Where::Body,
        &[
            (
                "g(x) { }",
                "a call with its `;` missing, followed by a block — not a macro, since the name is not spelled \
                 like one; see B36 in docs/grammar-gaps.md",
            ),
            (
                "NUMBER_OPTION(tab_width)\ng(x)",
                "a call with its `;` missing and **no `#define` in the file**: the macro reading needs that \
                 evidence, and a spelling convention is not enough for it; see B41 and `macros.rs`",
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
            //
            // The next gap: a **function-try-block**, whose `try` sits between the declarator and the body.
            // `try { } catch (const E& e) { }` as a *statement* is read (it is in the list above); the same words
            // in a function's own position are not, and the declarator's tail accepts `{`, `;`, `:`, `requires`
            // and an old-style parameter list — not `try`. Found while writing `old_style.rs`, whose list of
            // "what a modern function declarator continues with" was meant to contain no unsupported spelling.
            // Rare outside constructors, and loud rather than silent, so it is logged rather than fixed.
            (
                "void f() try { } catch (const E& e) { }",
                "the declarator's tail has no rule for `try` — see B25 in docs/grammar-gaps.md",
            ),
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
        // A **function type as a template argument**, where the wrong reading is invisible: the angle-matching scan
        // stopped at the parameter list's `)`, the template-id was never attempted, and `A<bool(T)> x;` came out as
        // the comparison `A < bool(T) > x` — an `ExpressionStat` that a consumer reads as "a statement with no
        // declaration in it", with nothing reported. The declaration reading is the assertion.
        shape("A<bool(T)> x;", Where::File, CppSyntaxKind::Declaration),
        shape(
            "std::function<bool(TokenKind)> pred;",
            Where::File,
            CppSyntaxKind::Declaration,
        ),
        // A **class-like definition in front of its declarator**: `struct S { … } x;` swallowed `x` into the type
        // and declared nothing — silently when the declarator was bare, loudly as `expected a declarator name` as
        // soon as it carried an initializer or a bound.
        shape(
            "struct S { int a; } x;",
            Where::File,
            CppSyntaxKind::Declaration,
        ),
        shape(
            "static const struct { int a; } priority[] = { { 1 } };",
            Where::File,
            CppSyntaxKind::Declaration,
        ),
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
fn assert_fragment_contains(fragments: &[(&str, CppSyntaxKind)]) {    let missing: Vec<String> = fragments
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

/// A header name is **one token**, whatever the name is made of.
///
/// The bug this pins: the fold accepted `Identifier | Dot | Slash | Minus | Plus | IntegerLiteral` between the
/// delimiters, and `c++config` lexes as `c`, `++`, `config` — so `#include <bits/c++config.h>` did not fold, and
/// that is the most common include in libstdc++: every standard header writes it.
///
/// What it cost was not a worse tree but a **wrong target**. The analysis layer reconstructs the name from the
/// tokens when the fold declines, and its reconstruction started at the `<`, so the target came out
/// `<bits/c++config.h` and resolved to nothing. A file with an unresolved include is deliberately never cached
/// (`index::store`), so every one of those headers was re-parsed on every session and every declaration in it
/// was invisible — 54 of the 169 files in a measured `<vector>` closure. Both halves are pinned:
/// `cpp_code_analysis/tests/preprocess.rs` has the reconstruction, and this has the fold.
///
/// A whitelist here is a rule about the *lexer's token kinds* pretending to be a rule about header names, which
/// is why the fix is a list of what cannot appear rather than of what can. See `docs/grammar-gaps.md` entry 22.
#[test]
fn a_header_name_is_one_token_whatever_is_in_it() {
    let folds = |source: &str| {
        let tree = CppParser::parse(source, ParserConfig::default());
        tree.get_red_root()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .any(|token| CppTokenKind::from(token.kind()) == CppTokenKind::HeaderName)
    };

    assert!(folds("#include <vector>\n"), "the ordinary case");
    assert!(folds("#include \"local.h\"\n"), "and the quoted one");
    assert!(
        folds("#include <bits/c++config.h>\n"),
        "`++` splits the name into three tokens, and a header name is not limited to the tokens a whitelist \
         happened to list"
    );
    assert!(folds("#include <sys/a-b.h>\n"), "`-` is a token of its own too");
    assert!(folds("#include <a/b/c.hpp>\n"));

    // What really ends a header name — and the assertion that the widening did not swallow the line.
    assert!(
        !folds("#include <unterminated\nint x;\n"),
        "a newline ends the directive, so there is no header name to fold"
    );
    assert!(
        !folds("a < b;\n"),
        "and outside a directive `<` is still less-than, which is why this is a parser rule and not a lexer one"
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
        // A C-style cast to a name the file never declares — the reading the `)` decides. See `operators.rs`.
        ("auto d = (MyType)1.5;", CppSyntaxKind::CastExpr),
        ("auto d = (size_t)size;", CppSyntaxKind::CastExpr),
        (
            "buf = (char *)malloc((size_t)size + 1);",
            CppSyntaxKind::CastExpr,
        ),
        // …and the shapes that must *not* become casts, because they are valid expressions.
        ("auto d = (a) - b;", CppSyntaxKind::ParenExpr),
        ("auto d = (a)(b);", CppSyntaxKind::CallExpr),
        // `<` decided by what follows the list: an operand means it was a comparison, and the shape says so —
        // `x = a < b > c` is `x = ((a < b) > c)`, so the *absence* of a `TemplateArgumentList` is the assertion
        // and it lives in `operators.rs` (this helper asserts presence, not absence). What is pinned here is the
        // other side: a genuine template-id keeps its argument list.
        ("void f() { x = a < b > c; }", CppSyntaxKind::BinaryExpr),
        (
            "void f() { auto n = A<B>::value; }",
            CppSyntaxKind::TemplateArgumentList,
        ),
        // Adjacent string literals are one literal, so the initialiser holds one node. See `expressions.rs`.
        ("auto s = \"a\" \"b\" \"c\";", CppSyntaxKind::LiteralExpr),
        // A `for` header's step is an expression, not a declaration that named nothing.
        (
            "void f() { for (;; i++, k++) { } }",
            CppSyntaxKind::ExpressionStat,
        ),
        // An elaborated type specifier reaches the *type* reading: a `TypeId` in the payload is the assertion, and
        // the expression reading could not produce one.
        ("auto n = sizeof(struct S);", CppSyntaxKind::TypeId),
        ("auto n = (struct S*)p;", CppSyntaxKind::CastExpr),
        // A qualified declarator name is a definition head: the parameter list is the assertion, because the wrong
        // reading made it an initializer (or nothing at all).
        ("static void A::f<int>(int);", CppSyntaxKind::ParameterList),
        (
            "static void Widget::draw(T) { }",
            CppSyntaxKind::ParameterList,
        ),
        (
            "static void Widget::draw(T) { }",
            CppSyntaxKind::CompoundStat,
        ),
        // A cv-qualifier after the type: the name is the declarator's, so there is an `InitDeclarator` and an
        // `ArrayType` — the wrong reading put the name in the type and made the declarator a structured binding of
        // nothing, silently.
        ("char const w[2];", CppSyntaxKind::InitDeclarator),
        ("char const w[2];", CppSyntaxKind::ArrayType),
        // An array type in a type-id, and an index that must not become one — the *absence* of an `ArrayType`
        // for `a[0]` is asserted in `expressions.rs`, since this helper asserts presence.
        ("auto n = sizeof(int[4]);", CppSyntaxKind::ArrayType),
        ("auto n = sizeof(a[0]);", CppSyntaxKind::IndexExpr),
        // A `new` reads its own bounds: the `ArrayType` is the new-declarator's, so the `TypeId` is bare.
        ("auto p = new int[4];", CppSyntaxKind::TypeId),
        ("auto p = new int[4];", CppSyntaxKind::ArrayType),
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

    // A K&R definition: the second list is the assertion, because the *first* one is what the parenthesis says
    // either way — `argc` is a good parameter type, so a modern parameter list reads the same tokens. The
    // declarations after the parenthesis are what says they were names. See `old_style.rs` for the full shape.
    let source = "int main(argc, argv)\nint argc;\nchar *argv[];\n{ return argc; }";
    assert!(
        contains(source, CppSyntaxKind::OldStyleParameterList),
        "the parameters declared after the parenthesis get a list of their own"
    );
    assert!(
        contains(source, CppSyntaxKind::CompoundStat),
        "and the body is still the definition's"
    );

    // A base clause with a **nested template-id**: the `BaseSpecifier` is the assertion, because the failure was
    // that the head was not recognised as one at all — the angle scan read the `>>` as a single closer's worth of
    // depth, the `{` of the body arrived with the depth still positive, and the class definition failed against
    // its own name. A tree without the base clause is lossless, well formed and one diagnostic away from right.
    let source = "class D : public Base<K, std::shared_ptr<V>> { };";
    assert!(
        contains(source, CppSyntaxKind::BaseSpecifier),
        "the nested base clause is read as a base clause"
    );
    // And both nested lists are lists: the empty `std::less<>` inside a parameter list is two argument lists, not
    // one that swallowed the other's closer.
    let source = "void f(const std::map<int, int, std::less<>> &m);";
    assert_eq!(
        CppParser::parse(source, ParserConfig::default())
            .get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TemplateArgumentList)
            .count(),
        2,
        "two template argument lists, one of them empty"
    );

    // A **pointer to member**: the `C::*` is the declarator's, and the type is only `int`. Read the other way —
    // `C::` joining the type — the tree was lossless, well formed, free of diagnostics and *wrong*: the
    // declarator sat inside the type node, so a consumer asking the declaration for its type got `int C::` and
    // one asking for its declarator got nothing. The text of the specifier sequence is what tells the two apart,
    // and it is the only assertion here that sees it.
    let source = "int C::*p;";
    let specifiers = CppParser::parse(source, ParserConfig::default())
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DeclSpecifierSeq)
        .expect("a specifier sequence")
        .text()
        .to_string();
    assert_eq!(
        specifiers, "int ",
        "the member-pointer operator belongs to the declarator, not to the type"
    );

    // A **macro invocation used as a definition**: the block is the declaration's *body*, not a statement that
    // follows it, and the statement inside it is read as a statement. Both halves matter — the shape is what the
    // reader of a test file needs (the body of a `TEST(...)` is where the code is), and the body being attached is
    // what `TEST(A, B) { int x = 1; }` failing used to cost.
    let source = "TEST(FormatPerformance, 1k_row) { int x = 1; }";
    let parsed = CppParser::parse(source, ParserConfig::default());
    let body = parsed
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CompoundStat)
        .expect("the block is in the tree");
    assert_eq!(
        body.parent()
            .map(|parent| CppSyntaxKind::from(parent.kind())),
        Some(CppSyntaxKind::Declaration),
        "the block is the definition's body, not a statement after it"
    );
    assert_eq!(
        body.text().to_string(),
        "{ int x = 1; }",
        "and its own declarations are inside it"
    );

    // The **named** spelling of the same shape, asked of the tree because the typed layer's `get_name_text`
    // answers with the class's own name — the class is what the declaration is *about* — and the question here is
    // whether the declarator exists at all. The defect was that it did not: the name had joined the type.
    let source = "static const struct { int a; } priority[] = { { 1 } };";
    let named = CppParser::parse(source, ParserConfig::default())
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::InitDeclarator)
        .flat_map(|declarator| declarator.descendants())
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::NameExpr)
        .map(|name| name.text().to_string())
        .any(|name| name == "priority");
    assert!(
        named,
        "the declarator is named `priority`, not part of the type"
    );

    // The `:` of a range-`for` inside a **member function** is the range separator, and the assertion is the
    // statement kind — the wrong reading produced a well-formed bit-field declaration whose width was the range
    // expression, with no diagnostic at all in the version that had a `,` after it.
    let source = "struct S { void f() { for (auto &v: vec) { } } };";
    assert!(
        contains(source, CppSyntaxKind::RangeForStat),
        "a range-for inside a member function is a range-for, not a bit-field"
    );
    // …and the same for a label, which is the other statement whose first token a declaration reading can take.
    let source = "struct S { void f() { again: g(); } };";
    assert!(
        contains(source, CppSyntaxKind::LabelStat),
        "a label inside a member function is a label"
    );
    // The bit-field itself is unchanged, at the member level it belongs to.
    let source = "struct S { int bits : 3; };";
    assert!(
        contains(source, CppSyntaxKind::Initializer),
        "a member's `:` is still a bit-field width"
    );

    // **How many enumerators an enum has.** This is the assertion the A0 defect of B26 needed and did not have:    // with the initializer read at the comma-operator level, `enum E { A = 0, B };` came out as *one*
    // `EnumeratorDecl` whose initializer was the expression `0, B` — lossless, well formed, no diagnostic, and
    // with `B` no longer a member of the enum. Counting members is the only question that sees it, and the census
    // above cannot ask it ("does this construct parse?" was answered `yes` by the wrong tree all along).
    for (source, expected) in [
        ("enum E { A = 0, B };", 2),
        ("enum E { A = 0, B, };", 2),
        ("enum class E : unsigned { A = 1, B = 2, C };", 3),
        ("enum E { A, B, C };", 3),
        ("enum E { A = 0 };", 1),
    ] {
        assert_eq!(
            CppParser::parse(source, ParserConfig::default())
                .get_red_root()
                .descendants()
                .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::EnumeratorDecl)
                .count(),
            expected,
            "{source:?} has {expected} enumerators"
        );
    }
}
