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
         struct Handle {\n\
             using element_type = Node;\n\
             Node* operator->() const;\n\
         };\n\
         void f() {\n\
             int a = Widget(1).size();\n\
             int b = Widget().size();\n\
         }\n\
         void g(Handle handle, Node* pointer) {\n\
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
