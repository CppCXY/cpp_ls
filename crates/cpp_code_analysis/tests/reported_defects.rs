//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test reported_defects -- --nocapture
//! ```
//!
//! Each of these was found by using the editor on a real project, not by reading the code, and each has a *fact-level*
//! assertion here plus an end-to-end one where the card is what was wrong (`cpp_ls`'s
//! `a_hover_names_the_instantiation_and_never_invents_a_void` for the two hover reports, and
//! `the_two_reported_hovers_answer_on_the_project_they_came_from` for the project they came from).

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

fn session_with(files: &[(&str, &str)]) -> Session<MemoryFiles> {
    let mut memory = MemoryFiles::new();
    for (path, source) in files {
        memory = memory.with_file(*path, *source);
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files(
        files
            .iter()
            .map(|(path, _)| std::path::PathBuf::from(*path)),
    );
    for (path, _) in files {
        session.want_cooked_reading(std::path::Path::new(path));
    }
    session.index_everything();
    session
}

fn parameters_of<'a>(
    summary: &'a cpp_code_analysis::FileSummary,
    name: &str,
) -> Option<&'a [String]> {
    summary
        .declarations
        .iter()
        .find(|fact| fact.name == name)
        .map(|fact| fact.parameters.as_slice())
}

/// **§B1 — a template parameter list is names, not the keywords that introduce them.**
///
/// The reported fact was MSVC's `<vector>`, whose `std::vector` declaration recorded `["class", "class"]`: the name was
/// read as "the last word before the `=`", and for an **unnamed** parameter that word is `class`. The keywords could
/// then *win* the choice of which declaration's parameters a substitution used.
///
/// A named parameter answers its own name (the case that already worked, kept as the regression), and an unnamed one is
/// an **empty slot** — it cannot be substituted into, and it is what keeps the arguments aligned with the names.
#[test]
fn a_template_parameter_is_a_name_and_an_unnamed_one_is_a_slot() {
    let session = session_with(&[(
        "/p/a.cpp",
        "template <class A, typename B, int N>\nstruct S {};\n\
         template <typename, typename> class an_unnamed_one;\n\
         template <typename, class T> struct partial;\n",
    )]);
    let summary = session
        .index()
        .summary(std::path::Path::new("/p/a.cpp"))
        .expect("indexed");

    assert_eq!(
        parameters_of(summary, "S"),
        Some(&["A".to_string(), "B".to_string(), "N".to_string()][..]),
        "a named parameter answers its name"
    );
    assert_eq!(
        parameters_of(summary, "an_unnamed_one"),
        Some(&[String::new(), String::new()][..]),
        "two slots, and no keyword pretending to be a name"
    );
    assert_eq!(
        parameters_of(summary, "partial"),
        Some(&[String::new(), "T".to_string()][..]),
        "the second slot keeps its name **and its position**"
    );
}

/// **§A2 — `static const struct { … } priority[]` has no `void` in it anywhere.**
///
/// The specifier sequence of an unnamed class type contributed no name, so the reader fell back to `void` and the fact
/// recorded `void[]`. `void` is not "no answer": it is a different type. The honest answer is that this layer cannot
/// spell the type, which the fact records as no `type_of` at all — and then a hover shows the declaration's own text.
#[test]
fn an_anonymous_structs_variable_is_not_a_void() {
    let session = session_with(&[(
        "/p/a.cpp",
        "static const struct\n{\n    unsigned char left;\n    unsigned char right;\n} priority[] = { { 1, 2 } };\n",
    )]);
    let summary = session
        .index()
        .summary(std::path::Path::new("/p/a.cpp"))
        .expect("indexed");
    let facts: Vec<(&str, Option<&str>)> = summary
        .declarations
        .iter()
        .filter(|fact| fact.name == "priority" || fact.name == "left" || fact.name == "right")
        .map(|fact| (&*fact.name, fact.type_of.as_deref()))
        .collect();
    println!("facts: {facts:?}");

    assert!(
        !facts.iter().any(|(_, type_of)| *type_of == Some("void")),
        "nothing here is a `void`: {facts:?}"
    );
    assert!(
        facts.contains(&("left", Some("unsigned char"))),
        "and the members keep their own types: {facts:?}"
    );
}

/// **§A1 — the spelling the reader wrote survives into the fact.**
///
/// The hover card for `std::map<std::string, LuaTokenKind, std::less<>>` showed only the *declaration's* parameter
/// names. The card is fixed in `cpp_ls` (it reads the use's spelling from the file), and this is the other half: the
/// fact of the variable must keep the instantiation that was written, because that is what a reader compares against.
#[test]
fn a_template_id_keeps_the_spelling_that_was_written() {
    let session = session_with(&[(
        "/p/a.cpp",
        "namespace std {\ntemplate <class K, class V, class C>\nstruct map {};\n}\n\
         std::map<int, long, int> table = {};\n",
    )]);
    let summary = session
        .index()
        .summary(std::path::Path::new("/p/a.cpp"))
        .expect("indexed");
    let table = summary
        .declarations
        .iter()
        .find(|fact| fact.name == "table")
        .expect("the variable is a fact");

    assert_eq!(
        table.type_of.as_deref(),
        Some("std::map<int, long, int>"),
        "the type the file wrote"
    );
}

/// **§A3 — a `typedef` of a qualified name is its type, not its own name.**
///
/// The report was a hover: `auto lineStartOffset = _lineOffsetVec[line];` printed `void lineStartOffset;`. The
/// chain that produced it, measured on libstdc++'s `vector`:
///
/// ```text
/// operator[] returns `reference`  ->  reference is `typedef typename _Base::reference reference;`
///                                 ->  that fact's type was `void`
///                                 ->  the subscript typed as `void`
///                                 ->  the `auto` variable typed as `void`
/// ```
///
/// The rule that reads an out-of-line definition's type ("a qualified spelling that ends in the declared name is the
/// declarator's own name, not a type") was applied to a `typedef` as well, where the two spellings are a coincidence:
/// `typedef typename _Base::reference reference;` writes the name twice and both halves are real. An alias never needs
/// the rule, so it no longer gets it.
#[test]
fn an_alias_of_a_qualified_type_is_not_a_void() {
    let session = session_with(&[(
        "/p/a.cpp",
        "template <class T> struct Base { typedef T& reference; };\n\
         template <class T> struct V {\n\
         typedef typename Base<T>::reference reference;\n\
         reference at(int i);\n\
         };\n",
    )]);
    let summary = session
        .index()
        .summary(std::path::Path::new("/p/a.cpp"))
        .expect("indexed");
    let alias = summary
        .declarations
        .iter()
        .find(|fact| fact.name == "reference" && fact.scope.as_deref() == Some("V"))
        .expect("the alias is a fact");
    println!("V::reference type_of {:?}", alias.type_of);
    assert!(
        alias.type_of.as_deref() != Some("void"),
        "an alias's type is never `void`: {:?}",
        alias.type_of
    );
    assert!(
        alias
            .type_of
            .as_deref()
            .is_some_and(|written| written.contains("Base<T>::reference")),
        "the type the typedef wrote: {:?}",
        alias.type_of
    );
}

/// **§C1 — a braced initialisation of a class template keeps its template arguments.**
///
/// Reported on `test_cpp/src/templates.cpp`:
///
/// ```cpp
/// template <typename T, typename U = int> struct Pair { T first; U second; T sum() const { … } };
/// template <typename T> struct Pair<T, T> { … };
/// …
/// Pair<int, int>{1, 2}.sum()      // hover: "`int` has no member `sum`"
/// ```
///
/// The initialiser's first child is the *type name*, and the arm that answers a class with no `type_of` of its own named
/// the **whole spelling**: `Type::named("Pair<int, int>")` is a class whose name is `Pair<int, int>` and whose argument
/// list is empty — so no argument reached the substitution, and `sum`'s written `T` was answered as `T`. The member
/// lookup was handed the same spelling, whose class is `int` to the hover.
///
/// The same change makes a **bare** braced list honest: `{1, 2}` was answered `int` (its first element), which claims the
/// list *is* an `int`; C++ makes it a `std::initializer_list<int>`, a library type this layer does not invent, so the
/// answer is that it has no type here.
#[test]
fn a_braced_initialisation_of_a_class_template_keeps_its_arguments() {
    let session = session_with(&[(
        "/p/a.cpp",
        "template <typename T, typename U = int>\n\
         struct Pair {\n\
             T first;\n\
             U second;\n\
             T sum() const { return first + second; }\n\
         };\n\
         void f() {\n\
             int a = Pair<int, int>{1, 2}.sum();\n\
             int b = Pair<int, long>{1, 2}.sum();\n\
         }\n",
    )]);
    let path = std::path::Path::new("/p/a.cpp");
    let view = session.view_of_the_file(path).expect("the written text");
    let source = view.source.clone();
    let nodes: Vec<cpp_parser::CppSyntaxNode> = view.root.descendants().collect();

    let node_at = |at: usize| -> cpp_parser::CppSyntaxNode {
        let found = nodes
            .iter()
            .find(|node| usize::from(node.text_range().start()) == at)
            .expect("a node starts here");
        found.clone()
    };

    // The member of an instantiation is the substituted one: `T` for `Pair<int, int>` is `int`.
    let used_at = source.find("Pair<int, int>{1, 2}.sum").expect("the use");
    let member = used_at + "Pair<int, int>{1, 2}.".len();
    let known = session.type_at(
        &view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(member),
    );
    assert_eq!(
        known.map(|found| found.type_of.clone()),
        cpp_code_analysis::Known::Yes("int".to_string()),
        "the member of an instantiation is read with its arguments substituted"
    );

    // The initialisation is the instantiated class, not a name that happens to contain its arguments.
    let object = nodes
        .iter()
        .find(|node| node.text() == "Pair<int, int>{1, 2}")
        .expect("the object node")
        .clone();
    let object_type = session.type_of_the_expression(&view, &object);
    assert_eq!(
        object_type.map(|found| found.type_of.clone()),
        cpp_code_analysis::Known::Yes("Pair<int, int>".to_string()),
        "the object is the instantiation"
    );

    // A bare braced list is not its first element.
    let list = source[used_at..].find('{').expect("the list") + used_at;
    let bare = node_at(list);
    let bare_known = session.type_of_the_expression(&view, &bare);
    assert!(
        matches!(bare_known, cpp_code_analysis::Known::Unknown(_)),
        "a braced list of values has no type this layer can name: {bare_known:?}"
    );
}

/// **§C2 — a member function's `const`, `&`/`&&` and its `requires`-clause are facts.**
///
/// Reported from `strings.cpp`:
///
/// ```cpp
/// struct Foo {
///     template <typename Fn> void call(Fn &&fn) { … }
///     template <typename Fn> void call(Fn &&fn) const { … }
/// };
/// struct Bar {
///     template <typename Fn> void call(Fn &&fn) requires requires { fn(*this); } { … }
///     template <typename Fn> void call(Fn &&fn) const requires requires { fn(*this); } { … }
/// };
/// ```
///
/// Hovering `cfoo.call` showed `call(Fn &&fn)` — the **non-const** declaration, which a `const Foo` cannot call — and
/// nothing of `Bar`'s `requires`. Both were missing from the fact: `DeclModifiers` had a bit for `constexpr` (a
/// specifier) and none for `const` (written after the parameter list), and the trailing clause had no field at all.
///
/// The bits and the clause must also agree **whichever path built the fact** — the summary and a member list asked from
/// the file's own scopes do — which is why this test asks both.
#[test]
fn a_member_function_records_its_qualifiers_and_its_requires_clause() {
    let session = session_with(&[(
        "/p/a.cpp",
        "struct Foo {\n\
             template <typename Fn>\n\
             void call(Fn &&fn) {}\n\
             template <typename Fn>\n\
             void call(Fn &&fn) const {}\n\
         };\n\
         struct Bar {\n\
             template <typename Fn>\n\
             void call(Fn &&fn) requires requires { fn(*this); } {}\n\
             template <typename Fn>\n\
             void call(Fn &&fn) const requires requires { fn(*this); } {}\n\
         };\n",
    )]);
    let path = std::path::Path::new("/p/a.cpp");
    let view = session.view_of_the_file(path).expect("the written text");

    let is_const = |fact: &cpp_code_analysis::DeclFact| {
        fact.modifiers.bits() & cpp_code_analysis::DeclModifiers::CONST.bits() != 0
    };

    // The summary: two `call`s per class, one of each constness.
    let summary = session.index().summary(path).expect("a summary");
    let calls: Vec<&cpp_code_analysis::DeclFact> = summary
        .declarations
        .iter()
        .filter(|fact| fact.name == "call")
        .collect();
    assert_eq!(calls.len(), 4, "four declarations: {calls:?}");
    assert_eq!(calls.iter().filter(|fact| is_const(fact)).count(), 2);
    for fact in calls
        .iter()
        .filter(|fact| fact.scope.as_deref() == Some("Bar"))
    {
        assert_eq!(
            fact.requires_clause.as_deref(),
            Some("requires requires { fn(*this); }"),
            "the clause as the declaration spells it"
        );
    }

    // And the member list, which is a *different* builder: a member's card comes from here.
    let cpp_code_analysis::Known::Yes(foo) = session.members_of(&view, "Foo") else {
        panic!("members of Foo");
    };
    assert_eq!(
        foo.members
            .iter()
            .filter(|member| member.fact.name == "call")
            .filter(|member| is_const(&member.fact))
            .count(),
        1,
        "one const `call` in Foo's member list"
    );
    let cpp_code_analysis::Known::Yes(bar) = session.members_of(&view, "Bar") else {
        panic!("members of Bar");
    };
    for member in bar
        .members
        .iter()
        .filter(|member| member.fact.name == "call")
    {
        assert_eq!(
            member.fact.requires_clause.as_deref(),
            Some("requires requires { fn(*this); }"),
            "the member list's facts carry the clause too"
        );
    }
}

/// **§C3 — a call of a type is a temporary of it, and `->` reaches through a handle.**
///
/// Reported from `smart_pointers.cpp`:
///
/// ```cpp
/// std::shared_ptr<int> aliased(shared, nullptr);   // hover on `use_count`: the class is out of the index
/// auto node = std::make_shared<Node>(3);
/// auto twin = node->twin();                        // `NotDeclaredHere("shared_ptr::twin")`
/// ```
///
/// Two rules were missing and both are the language's:
///
/// * **`T(args)` is a temporary of `T`.** The constructor is not a function the spelling names, so every function arm
///   answered nothing — `Widget(1).size()` had no type at all, and the member question then had no class to ask.
/// * **An arrow reaches what the completion already says it reaches.** `p->` on a `Widget*` reaches `Widget`; on a
///   pointer-like class it reaches the class's `element_type`, which is the standard's own protocol and what
///   `shared_ptr`/`unique_ptr` declare (`operator->`'s return type is a dependent `typename …::type`, unreadable here).
#[test]
fn a_call_of_a_type_and_an_arrow_through_a_handle() {
    let session = session_with(&[(
        "/p/a.cpp",
        "struct Widget {\n\
             int size() const { return 1; }\n\
         };\n\
         struct Node {\n\
             int value() const { return 2; }\n\
         };\n\
         template <typename T>\n\
         struct Handle {\n\
             using element_type = T;\n\
             T* operator->() const;\n\
         };\n\
         void f() {\n\
             int a = Widget(1).size();\n\
             int b = Widget().size();\n\
         }\n\
         void g(Handle<Node> handle, Node* pointer) {\n\
             int c = handle->value();\n\
             int d = pointer->value();\n\
         }\n",
    )]);
    let path = std::path::Path::new("/p/a.cpp");
    let view = session.view_of_the_file(path).expect("the written text");
    let source = view.source.clone();

    let type_of_member = |needle: &str| {
        let at = source.find(needle).expect("the use") + needle.len() - 1;
        session.type_at(
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
        )
    };

    for needle in [
        "Widget(1).size",
        "Widget().size",
        "handle->value",
        "pointer->value",
    ] {
        let known = type_of_member(needle);
        assert_eq!(
            known.clone().map(|found| found.type_of.clone()),
            cpp_code_analysis::Known::Yes("int".to_string()),
            "{needle:?} must be an `int`: {known:?}"
        );
    }
}

/// **§C4 — a type whose last segment is the declared name is a type, not a declarator.**
///
/// Reported from the editor: `std::any any = 1;` hovered as `void any;`, and `std::pair pair{1, 2};` as `void pair;`.
///
/// The specifier reader drops a specifier that spells a **declarator's** qualified name — the guard exists for
/// `std::map<std::string, LuaTokenKind> LuaLexer::LuaReserved = { … }`, where "the last name wins" would record
/// `LuaLexer::LuaReserved` as the type. It recognised that shape by the text alone, and `std::any` ends in `::` + `any`
/// exactly as `LuaLexer::LuaReserved` ends in `::` + `LuaReserved`. The type was dropped, the specifier sequence read as
/// empty, and `parse_type_spelling("")` answered **`void`** — a wrong type, which is worse than no type.
///
/// The range tells the two apart: a declarator's qualified name ends where the declared name ends, and a type written
/// before the name does not.
#[test]
fn a_type_whose_last_segment_is_the_declared_name_is_not_dropped() {
    let session = session_with(&[(
        "/p/a.cpp",
        "void f() {\n\
             std::any any = 1;\n\
             std::pair pair{1, 2};\n\
             std::string string = \"x\";\n\
         }\n",
    )]);
    let path = std::path::Path::new("/p/a.cpp");
    let view = session.view_of_the_file(path).expect("the written text");
    let source = view.source.clone();

    for (declaration, type_of) in [
        ("std::any any", "std::any"),
        ("std::pair pair", "std::pair"),
        ("std::string string", "std::string"),
    ] {
        let at = source.find(declaration).expect("the declaration")
            + declaration.rfind(' ').unwrap()
            + 1;
        let known = session.type_at(
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
        );
        assert_eq!(
            known.map(|found| found.type_of.clone()),
            cpp_code_analysis::Known::Yes(type_of.to_string()),
            "`{declaration}` must be a `{type_of}`, not `void`"
        );
    }
}

/// **§C5 — a braced initialisation of a class template deduces its arguments.**
///
/// Reported from `containers.cpp` (fig 5) and `lambdas.cpp` (fig 4):
///
/// ```cpp
/// std::pair pair{1, 2};
/// std::cout << pair.first << pair.second;          // `_T1` / `_T2`, unsubstituted
/// auto [increment, copy] = instance.make_incrementer();   // and it returns `std::pair{…}`
/// ```
///
/// C++17 deduces a class template's arguments from the constructor at a braced initialisation. The engine answered the
/// class's **bare name** — a type with an argument missing — so nothing downstream substituted and every member stayed
/// `_T1`/`_T2`.
///
/// The deduction walks the class's constructors and pairs their written parameters with the initialiser's element
/// types (`deduce_the_parameters`, which already handles a parameter written as one template parameter with reference
/// qualifiers). It answers only when **every** parameter is deduced and every candidate agrees.
#[test]
fn a_braced_initialisation_of_a_class_template_deduces_its_arguments() {
    let session = session_with(&[(
        "/p/a.cpp",
        "template <typename A, typename B>\n\
         struct Pair {\n\
             A first;\n\
             B second;\n\
             Pair(A a, B b) : first(a), second(b) {}\n\
         };\n\
         void f() {\n\
             Pair pair{1, 2.5};\n\
             int one = pair.first;\n\
             double other = pair.second;\n\
         }\n",
    )]);
    let path = std::path::Path::new("/p/a.cpp");
    let view = session.view_of_the_file(path).expect("the written text");
    let source = view.source.clone();
    let at_of = |needle: &str| {
        let at = source.find(needle).expect("the use");
        at + needle.len() - 1
    };
    let known_at = |needle: &str| {
        session.type_at(
            &view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at_of(needle)),
        )
    };

    // The declaration itself: the arguments come from `{1, 2.5}`.
    assert_eq!(
        known_at("Pair pair").map(|found| found.type_of.clone()),
        cpp_code_analysis::Known::Yes("Pair<int, double>".to_string()),
        "the braced initialisation deduces `A = int, B = double`"
    );

    // And the members are substituted, which is the report's second half.
    assert_eq!(
        known_at("pair.first").map(|found| found.type_of.clone()),
        cpp_code_analysis::Known::Yes("int".to_string()),
        "`first` is `A`, and `A` is `int` here"
    );
    assert_eq!(
        known_at("pair.second").map(|found| found.type_of.clone()),
        cpp_code_analysis::Known::Yes("double".to_string()),
        "`second` is `B`, and `B` is `double` here"
    );
}

/// **§C6 — a member inherited from a template base is written in that base's parameters.**
///
/// Reported as `shared_from_this()` having no hover and no type. The member list had it all along — through
/// `enable_shared_from_this<Node>` — but the fact said `_Tp`/`T *` and `declared_in` named the base **without** its
/// arguments, so nothing downstream could finish the type.
///
/// `bases_of` answers *names a class can be looked up by* and drops the arguments on purpose. The walk keeps the
/// **written clause** alongside, resolves its head, and runs the base's members through `substituted_members` — so
/// `Node`'s `shared_from_this` is a `Node *` and its tag names the instantiation it came from.
#[test]
fn a_member_of_a_template_base_is_substituted_with_the_base_arguments() {
    let session = session_with(&[
        (
            "/p/base.h",
            "namespace std {\n\
             template <typename T>\n\
             struct enable_shared_from_this {\n\
                 T* shared_from_this() { return nullptr; }\n\
             };\n\
             }\n",
        ),
        (
            "/p/a.cpp",
            "#include \"base.h\"\n\
             struct Node : public std::enable_shared_from_this<Node> {\n\
                 void twin() { auto p = shared_from_this(); }\n\
             };\n",
        ),
    ]);
    let path = std::path::Path::new("/p/a.cpp");
    let view = session.view_of_the_file(path).expect("the written text");

    let cpp_code_analysis::Known::Yes(listed) = session.members_of(&view, "Node") else {
        panic!("`Node` is declared in this file");
    };
    let member = listed
        .members
        .iter()
        .find(|member| member.fact.name == "shared_from_this")
        .expect("the member comes through the base walk");
    assert_eq!(
        member.fact.returns.as_deref(),
        Some("Node *"),
        "the base's `T` is the class it was instantiated with: {member:?}"
    );
    assert_eq!(
        member.declared_in, "std::enable_shared_from_this<Node>",
        "the tag names the instantiation, not just the base's name"
    );

    // And the deduction that needed it: `auto p = shared_from_this();` is a `Node *`, not `T *`.
    let source = view.source.clone();
    let at = source.find("auto p").expect("the fixture") + "auto p".len() - 1;
    let known = session.type_at(
        &view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
    );
    assert_eq!(
        known.map(|found| found.type_of.clone()),
        cpp_code_analysis::Known::Yes("Node*".to_string()),
        "the call's type is the substituted return type"
    );
}

/// **§C7 — a braced *construction* in an expression deduces its arguments too.**
///
/// `auto t = std::tuple{1, 2, 3}` answered `std::tuple` — no arguments — while the same braced list written as a
/// declaration's initialiser (`std::pair pair{1, 2}`) deduced. Two shapes were being read as one:
///
/// ```text
/// declaration's initialiser    Initializer > InitListExpr > [1, 2.5]              flat
/// construction expression      InitListExpr > [IdentifierExpr, InitListExpr]       the name, then a nested list
/// ```
///
/// and the arm read the nested list as **one element** whose type is a bare braced list — `Unknown` — so it declined.
/// `the_elements_of_a_braced_list` opens the nested list, and a **qualified** head (`std::pair`) is asked of the index
/// when the name query declines an expression node.
#[test]
fn a_braced_construction_in_an_expression_deduces_its_arguments() {
    let session = session_with(&[(
        "/p/a.cpp",
        "namespace std {\n\
         template <typename A, typename B>\n\
         struct pair {\n\
             A first;\n\
             B second;\n\
             pair(A a, B b) : first(a), second(b) {}\n\
         };\n\
         }\n\
         void f() {\n\
             auto p = std::pair{1, 2.5};\n\
         }\n",
    )]);
    let path = std::path::Path::new("/p/a.cpp");
    let view = session.view_of_the_file(path).expect("the written text");
    let source = view.source.clone();
    let at = source.find("auto p").expect("the fixture") + "auto p".len() - 1;

    assert_eq!(
        session
            .type_at(
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(at)
            )
            .map(|found| found.type_of.clone()),
        cpp_code_analysis::Known::Yes("std::pair<int, double>".to_string()),
        "the construction's arguments come from its elements"
    );
}

/// **§C8 — a structured binding over a class this project declares takes its members' types.**
///
/// `element_of` knows the standard library's shapes by name (`pair`, `map`, `tuple`, a sequence's argument) and answers
/// nothing for a class a project declares itself — so `auto [a, b] = p;` over `Pair<int, double>` refused, and the inlay
/// hint for it was missing. A pair-like type is a class with `first` and `second`, and `members_of` has their types with
/// the container's arguments substituted; the order is asked by **name**, because the member list is sorted.
#[test]
fn a_structured_binding_over_a_project_class_takes_its_members_types() {
    let session = session_with(&[(
        "/p/a.cpp",
        "template <typename A, typename B>\n\
         struct Pair {\n\
             A first;\n\
             B second;\n\
             Pair(A a, B b) : first(a), second(b) {}\n\
         };\n\
         void f() {\n\
             Pair<int, double> p{1, 2.5};\n\
             auto [a, b] = p;\n\
         }\n",
    )]);
    let path = std::path::Path::new("/p/a.cpp");
    let view = session.view_of_the_file(path).expect("the written text");
    let source = view.source.clone();
    let at = |needle: &str| source.find(needle).expect("the fixture");

    for (needle, skip, expected) in [("[a", 1, "int"), ("b]", 0, "double")] {
        assert_eq!(
            session
                .type_at(
                    &view,
                    cpp_code_analysis::file::coordinates::ReadingOffset::new(at(needle) + skip)
                )
                .map(|found| found.type_of.clone()),
            cpp_code_analysis::Known::Yes(expected.to_string()),
            "`{needle}` is the member's type with the container's arguments substituted"
        );
    }
}

/// **§C9 — a braced construction of a template-id is a type.**
///
/// `auto x = B<int>{};` answered `UnknownType("B<int>{}")` while `auto x = B{};` worked, and the member access
/// `Builder<int>{}.with(2)` reported "the type of `Builder<int>{}` is not known" (measured in `templates.cpp`).
///
/// The gate in front of the name arm rejects `(`, `)` and `,` — a whole expression is not a name — and `B<int>{}` writes
/// none of the three: its one segment is `B` once the `<…>` comes off, so the name arm answered before the braced-list arm
/// could. A braced construction is not a name either, and with the gate saying so the construction reaches the arm that
/// names its head and runs the class-template deduction.
#[test]
fn a_braced_construction_of_a_template_id_is_typed() {
    let session = session_with(&[(
        "/p/a.cpp",
        "template <typename T>\n\
         struct B {\n\
             T v;\n\
             int with(int x) { return x; }\n\
         };\n\
         void f() {\n\
             auto x = B<int>{};\n\
             auto y = B<int>{}.with(2);\n\
         }\n",
    )]);
    let path = std::path::Path::new("/p/a.cpp");
    let view = session.view_of_the_file(path).expect("the written text");
    let source = view.source.clone();

    // **At the variable's own name**, not after the `=`: the first byte of the initializer is the head of the
    // construction, and the question here is what the *variable* is.
    //
    // The second half of the fixture is deliberately not asserted: `auto y = B<int>{}.with(2);` answers the initialiser's
    // **spelling** (`B<int>{}.with`), because the type of a member call on a braced construction is a separate question —
    // the hover on the member answers `int with(int x)` while the call's own type does not. Recorded in `docs/buglist.md`
    // as the next piece of the card work rather than asserted away here.
    let at = source.find("x = ").expect("the fixture");
    assert_eq!(
        session
            .type_at(
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(at)
            )
            .map(|found| found.type_of.clone()),
        cpp_code_analysis::Known::Yes("B<int>".to_string()),
        "a braced construction of a template-id is its class with the arguments it wrote"
    );
}

/// **§D1 — a pointer's subscript is its pointee.**
///
/// `int* a; …; auto x = a[0];` — reported: "the type of `a[0]` is not known here". The subscript arm handled a
/// `Type::Array` and a class with `operator[]`; a pointer is neither, and `p[i]` is `*(p + i)`, whose type C++ fixes as
/// the pointee.
#[test]
fn a_pointer_subscript_is_its_pointee() {
    let session = session_with(&[(
        "/p/a.cpp",
        "int main() {\n    int* a;\n    a = new int(42);\n    auto x = a[0];\n    return 0;\n}\n",
    )]);
    let path = std::path::Path::new("/p/a.cpp");
    let view = session.view_of_the_file(path).expect("the written text");
    let source = view.source.clone();
    let at = source.find("auto x").expect("the fixture") + "auto x".len() - 1;
    assert_eq!(
        session
            .type_at(
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(at)
            )
            .map(|found| found.type_of.clone()),
        cpp_code_analysis::Known::Yes("int".to_string())
    );
}

/// **§D2 — `return` a local names the local's declared type.**
///
/// `auto pipeline() { std::vector<int> result; … return result; }` had no return type: the deduction knew a parameter and a
/// data member, and a local was refused — so a range-for over `pipeline()` had no element (measured on `ranges.cpp`). The
/// declaration in front of the return states the type, which is the same reading a parameter gets.
#[test]
fn a_returned_local_states_its_type() {
    let session = session_with(&[(
        "/p/a.cpp",
        "auto make() {\n    std::vector<int> result;\n    return result;\n}\n",
    )]);
    let path = std::path::Path::new("/p/a.cpp");
    let view = session.view_of_the_file(path).expect("the written text");
    let source = view.source.clone();
    let _ = &source;
    // **The index's fact, not the binding's.** `Session::definition` answers a *place to jump to* and records no return
    // type (deliberately — see `ProjectDefinition::from_binding`); the summary is where the declaration's facts live.
    let known = session
        .index()
        .definition("make", path)
        .value()
        .unwrap_or_else(|| panic!("`make` is in the index"));
    assert_eq!(
        known.fact.returns.as_deref(),
        Some("std::vector<int>"),
        "the local's declaration states the type the function returns"
    );
}

/// **§D3 — a qualified name is resolved through a namespace alias.**
///
/// Reported from `ranges.cpp`: `std::views::filter` answered `NotDeclaredHere("std::views::filter")` while
/// `std::ranges::views::filter` was declared in `<ranges>` all along. libstdc++ writes `namespace views = ranges::views;`,
/// and the name query's alias walk followed **type** aliases (`using string = basic_string<char>`) and `using`-declared
/// types — a namespace alias's fact is a `Namespace` whose `type_of` is the target, which is neither.
///
/// The alias is written relative to its own scope here (`namespace views = ranges::views;` inside `std`), which is the real
/// spelling: the rewrite has to qualify it with that scope, or the target names nothing.
///
/// **And the members of a type written through the alias** — `std::views::_Filter` — are the target's members: the same
/// substitution has to happen in a spelling's **prefix**, not only when the whole spelling is the namespace.
#[test]
fn a_qualified_name_is_resolved_through_a_namespace_alias() {
    let session = session_with(&[
        (
            "/p/views.h",
            "namespace std {\n\
             namespace ranges {\n\
                 namespace views {\n\
                     inline constexpr int filter = 1;\n\
                 }\n\
             }\n\
             namespace views = ranges::views;\n\
             }\n",
        ),
        (
            "/p/a.cpp",
            "#include \"views.h\"\nint main() { return std::views::filter; }\n",
        ),
    ]);
    let path = std::path::Path::new("/p/a.cpp");
    let view = session.view_of_the_file(path).expect("the written text");
    let source = view.source.clone();
    let at = source.find("std::views::filter").expect("the fixture") + "std::views::".len();

    let known = session.definition(
        &view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
    );
    let cpp_code_analysis::Known::Yes(found) = known else {
        panic!("the alias names a declaration: {known:?}");
    };
    assert_eq!(
        found.fact.qualified_name(),
        "std::ranges::views::filter",
        "the answer is the target's declaration, under the scope that actually declares it"
    );

    // The members of a type written through the alias are the target's.
    let session = session_with(&[
        (
            "/p/views.h",
            "namespace std {\n\
             namespace ranges {\n\
                 namespace views {\n\
                     struct _Filter { int arity; };\n\
                 }\n\
             }\n\
             namespace views = ranges::views;\n\
             }\n",
        ),
        (
            "/p/a.cpp",
            "#include \"views.h\"\nint main() { std::views::_Filter value; return value.arity; }\n",
        ),
    ]);
    let view = session.view_of_the_file(path).expect("the written text");
    let known = session.members_of(&view, "std::views::_Filter");
    let cpp_code_analysis::Known::Yes(listed) = known else {
        panic!("the alias names the class `std::ranges::views::_Filter`: {known:?}");
    };
    assert_eq!(
        listed
            .members
            .iter()
            .map(|member| member.fact.name.as_str())
            .collect::<Vec<_>>(),
        ["arity"],
        "the member is looked for in the scope that declares it"
    );
}

/// **§D4 — a type name written through an inline namespace still names the type.**
///
/// `std::string` is `using string = basic_string<char>;` inside `std::__cxx11`, so the index's qualified name is
/// `std::__cxx11::string` and `definition("std::string")` answers `NotDeclaredHere`. Measured on `basics.cpp`:
/// `std::string{"three"}`'s head had no type, so that element was unknown and the `std::tuple{1, 2.5,
/// std::string{"three"}}` around it deduced to its bare name rather than to `std::tuple<int, double, std::string>`.
///
/// The retry is the one `members_of` already makes: the last segment, and any candidate that is a type.
#[test]
fn a_type_name_through_an_inline_namespace_names_the_type() {
    let session = session_with(&[(
        "/p/a.cpp",
        "namespace std {\n\
         template <class E> struct basic_string { };\n\
         inline namespace __cxx11 { using string = basic_string<char>; }\n\
         }\n\
         void f() {\n\
             auto tuple = std::tuple<int, std::string>{};\n\
             std::string s2{\"three\"};\n\
         }\n",
    )]);
    let path = std::path::Path::new("/p/a.cpp");
    let view = session.view_of_the_file(path).expect("the written text");
    let source = view.source.clone();
    let at = source.find("s2{").expect("the fixture");

    assert_eq!(
        session
            .type_at(
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(at)
            )
            .map(|found| found.type_of.clone()),
        cpp_code_analysis::Known::Yes("std::string".to_string()),
        "the alias's name is what the file wrote, even though the index stores it under `__cxx11`"
    );
}

/// **§D5 — a generic lambda called with an `int` returns an `int`.**
///
/// `auto f = [](auto v) { return v; }; f(3)` — the lambda's parameter is a template parameter written `auto`, and its body's
/// return type was refused outright ("nothing says what `auto` stands for"). At a **call site** something does: the argument
/// types, paired with the parameters by position.
#[test]
fn a_generic_lambda_called_with_an_int_returns_an_int() {
    let session = session_with(&[(
        "/p/a.cpp",
        "int main() {\n    auto twice = [](auto v) { return v * 2; };\n    auto x = twice(2.5);\n}\n",
    )]);
    let path = std::path::Path::new("/p/a.cpp");
    let view = session.view_of_the_file(path).expect("the written text");
    let source = view.source.clone();
    let at = source.find("auto x").expect("the fixture") + "auto x".len() - 1;

    assert_eq!(
        session
            .type_at(
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(at)
            )
            .map(|found| found.type_of.clone()),
        cpp_code_analysis::Known::Yes("double".to_string()),
        "`v * 2` with `v` a `double` is a `double`"
    );
}

/// **A declaration two `#include`s away is visible** — `c.cpp` → `b.h` → `a.h`, the shape MSVC 18 gives
/// `std::endl` (it lives in `__msvc_ostream.hpp`, one hop past `<ostream>`).
///
/// Reported from an editor as "`std::endl` is not rendered as a function". Measured through `inspect`, which asks for
/// one file's reading rather than the whole closure, a chain like this answers `NotDeclaredHere` for the second hop
/// and `Yes` for the first — so this test asks the same question after the same ask the server makes (every project
/// file's reading), which is where the answer has to be right.
#[test]
fn a_declaration_two_includes_away_is_visible() {
    let session = session_with(&[
        ("/p/a.h", "namespace std { void endl(); }\n"),
        ("/p/b.h", "#include \"a.h\"\nint in_b;\n"),
        (
            "/p/c.cpp",
            "#include \"b.h\"\nvoid f() { std::endl(); }\nvoid g() { in_b = 1; }\n",
        ),
    ]);

    let from = std::path::Path::new("/p/c.cpp");
    // The first hop, which has always worked — the control.
    assert!(
        matches!(
            session.index().definition("in_b", from),
            cpp_code_analysis::Known::Yes(_)
        ),
        "the direct include's own declaration is visible"
    );

    let found = session.index().definition("std::endl", from);
    assert!(
        matches!(found, cpp_code_analysis::Known::Yes(_)),
        "the declaration one hop past the direct include is visible too: {found:?}"
    );
}

/// **An out-of-line definition declares one name, not two** — the member, and *not* its class.
///
/// `int Pair::Mine() { return 0; }` writes its name against the parameter list, and `declared_name` refuses a
/// `::`-qualified name on purpose (it belongs to the class, not to this file's scope). The fallback that reads a name
/// out of the specifier sequence — added for `explicit A(int x) : x_(x) {}`, whose declarator is only `(int x)` — saw
/// "no declarator names anything" and bound `get_name()`, which for this shape is the **qualifier**: a constructor
/// `Pair` at file scope, declared nowhere.
///
/// Measured on the report's `Mark.cpp`: the phantom fact put a `0 references` code lens over every out-of-line
/// definition (`CompleteMarker::CompleteMarker`, `Marker::Complete`), and a summary of a `.cpp` of nothing but
/// definitions declared a function named after the class once per member.
#[test]
fn an_out_of_line_definition_declares_its_member_and_not_its_class() {
    let session = session_with(&[(
        "/p/pair.cpp",
        "struct Pair {\n    Pair(int a, int b);\n    int Mine() const;\n};\nint Pair::Mine() const { return 0; }\n",
    )]);
    let summary = session
        .index()
        .summary(std::path::Path::new("/p/pair.cpp"))
        .expect("the file is indexed");

    let functions: Vec<(String, Option<String>)> = summary
        .declarations
        .iter()
        .filter(|fact| fact.kind == cpp_code_analysis::DeclKind::Function)
        .map(|fact| (fact.name.clone(), fact.scope.clone()))
        .collect();

    assert!(
        !functions.iter().any(|(name, scope)| name == "Pair" && scope.is_none()),
        "the qualifier of `int Pair::Mine()` is not a function declared at file scope: {functions:?}"
    );
    assert_eq!(
        functions,
        vec![
            ("Pair".to_string(), Some("Pair".to_string())),
            ("Mine".to_string(), Some("Pair".to_string())),
            ("Mine".to_string(), Some("Pair".to_string())),
        ],
        "the constructor declared in the class, and the member's declaration and definition"
    );
}
