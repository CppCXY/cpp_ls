//! **Types** — the type answers: aliases, members, overloads, enumerators

use crate::file::paths::MemoryFiles;
use crate::symbol::Known;

use super::*;

/// **The same, with everything the real project has that the fixture above does not**: a namespace, a *private*
/// alias, and the alias declared after the members that use it.
///
/// The previous fixture passes and the real `std::_Tree` does not, so the difference has to be one of these three —
/// this fixture is the instrument that says which.
#[test]
pub(crate) fn a_private_namespaced_alias_still_resolves_in_a_return_type() {
    let text = "namespace std {\n\
                template <typename T> struct _Tree_base { using iterator = T *; };\n\
                template <typename T> struct _Tree {\n\
                public:\n\
                \x20   _Mybase::iterator find(int key) const;\n\
                private:\n\
                \x20   using _Mybase = _Tree_base<T>;\n\
                };\n\
                }\n\
                void f(std::_Tree<int> &t) {\n\
                \x20   t.find(1);\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("a-private-namespaced-alias", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let at = text.find("find(1)").expect("the fixture") + "find".len();
    let typed = session.type_at(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(typed) = typed else {
        panic!("`t.find(1)` must have a type: {typed:?}");
    };
    assert_eq!(
        typed.type_of, "int*",
        "`_Mybase::iterator` is `_Tree_base<int>::iterator` — a private alias still names a type here"
    );
}
/// **A call whose return type is a *qualified alias of its own class*** — `_Mybase::iterator`, MSVC's spelling.
///
/// Measured on a real project: `LuaReserved.find(text)` typed as `_Mybase::iterator` — `_Tree::find` returns
/// `_Mybase::iterator`, and `_Mybase` is a member alias of the very class the function belongs to. Everything
/// downstream then asks a class called `_Mybase::iterator` for its members, and the answer is whatever the last hop
/// happened to be. The finishing walk (`finish_a_nested_name`) reads one *bare* name at a time, which is why it could
/// not follow this one.
#[test]
pub(crate) fn a_calls_return_type_is_a_qualified_alias_of_its_own_class() {
    let text = "template <typename T> struct base { using iterator = T *; };\n\
                template <typename T> struct tree {\n\
                \x20   using _Mybase = base<T>;\n\
                \x20   _Mybase::iterator find(int key) const;\n\
                };\n\
                void f(tree<int> &t) {\n\
                \x20   t.find(1);\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("a-qualified-alias-return", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let at = text.find("find(1)").expect("the fixture") + "find".len();
    let typed = session.type_at(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(typed) = typed else {
        panic!("`t.find(1)` must have a type: {typed:?}");
    };
    assert_eq!(
        typed.type_of, "int*",
        "`_Mybase::iterator` is `base<int>::iterator`, which is `int*`"
    );
}
/// **`map<…>::find(k)->second`, with the comparator written out** — the real project's own line.
///
/// MSVC's `map` takes four template arguments and `LuaReserved`'s type writes three
/// (`std::map<std::string, LuaTokenKind, std::less<>>`). The element of a map is `pair<const K, V>` whatever the
/// comparator is, and the layer's element rule required **exactly two** arguments — so it answered "no element" and the
/// call fell back to its declaration's `_Mybase::iterator`, which is a dependent type nobody can open. Measured on the
/// real project before this fixture: `LuaReserved.find(text)->second` → `Unknown(NotDeclaredHere("std::conditional<
/// _Test, _Ty1, _Ty2>::type::second"))`; after: `LuaTokenKind`.
#[test]
pub(crate) fn a_maps_find_reaches_the_element_when_the_comparator_is_written() {
    let text = "template <typename A, typename B> struct pair { A first; B second; };\n\
                template <typename K, typename V, typename C> struct map_like {\n\
                \x20   pair<const K, V> find(const K &key) const;\n\
                };\n\
                void f(map_like<int, float, int> &m) {\n\
                \x20   m.find(1)->second;\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("a-maps-find-with-a-comparator", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    // **The call itself**, which is the element the iterator rule answers: `pair<const int, float>`.
    let at = text.find("find(1)").expect("the fixture") + "find".len();
    let typed = session.type_at(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(typed) = typed else {
        panic!("`m.find(1)` must have a type: {typed:?}");
    };
    assert!(
        typed.type_of.contains("pair") && typed.type_of.contains("float"),
        "the element of the map, not the iterator: {:?}",
        typed.type_of
    );

    // **And the member reached through it**: `second` is `float`.
    let second = text.find("->second").expect("the fixture") + "->".len();
    let found = session.definitions(&view, crate::file::coordinates::ReadingOffset::new(second));
    let Known::Yes(found) = found else {
        panic!("`find(1)->second` must resolve: {found:?}");
    };
    assert_eq!(found.found[0].fact.name, "second");
}
/// **`map::find(…)`'s answer, the shape the real project has** — the call's type, then the arrow, then the element.
///
/// The previous fixture asked the arrow directly. The real report is one hop longer: `LuaReserved.find(text)` is a
/// *call*, whose answer is an iterator, whose `operator->` answers a trait alias. On the real project that chain
/// answered `class "conditional_t": 0 member(s)` **without entering the alias walk at all**, which is what this
/// fixture is for — it reproduces the same three steps in one file, where a fix takes five seconds to try.
#[test]
pub(crate) fn a_calls_iterator_answer_reaches_the_element_through_a_trait_alias() {
    let text = "template <bool B, class T, class F> struct conditional {};\n\
                template <class T, class F> struct conditional<false, T, F> { using type = F; };\n\
                template <bool B, class T, class F>\n\
                using conditional_t = typename conditional<B, T, F>::type;\n\
                struct element { int first; int second; };\n\
                template <typename T> struct tree_iterator {\n\
                \x20   using pointer = conditional_t<false, void, T> *;\n\
                \x20   pointer operator->() const;\n\
                };\n\
                struct map_like {\n\
                \x20   using value_type = element;\n\
                \x20   tree_iterator<value_type> find(int key) const;\n\
                };\n\
                void f(map_like &m) {\n\
                \x20   m.find(1)->second = 2;\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("a-calls-iterator-through-a-trait", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let at = text.find("->second").expect("the fixture") + "->".len();
    let found = session.definitions(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(found) = found else {
        panic!("`m.find(1)->second` must resolve: {found:?}");
    };
    assert_eq!(
        (
            found.found[0].fact.name.as_str(),
            found.found[0].fact.scope.as_deref()
        ),
        ("second", Some("element")),
        "the call's iterator, its `pointer` alias, and the trait alias in it"
    );
}
/// **`auto s = std::format("x {}", 1)` has the type of the overload the *arguments* select.**
///
/// The engine refused an ambiguous callee name and stopped there, so `auto` had no initializer type and no type at
/// all — reported from an editor on `auto enenn = std::format(FULL_NAME_FORMAT, myName.getFullName(0));`, where the
/// card said "a variable" and nothing else. The ranking that decides it already existed (the signature popup and the
/// hover's `→` marker use it); the callee's declaration simply never asked it.
///
/// Measured on the real file after this: `enenn` answers
/// `basic_string<char, char_traits<char>, allocator<char>>`. The narrow/wide tie is broken by the character width the
/// arguments' types carry, which is the rule the fix's own comment states.
/// **A constant's value is folded, and an enumerator's initialiser can name one.**
///
/// Reported from an editor with two screenshots: `const int FIRST_RESERVED = UCHAR_MAX + 1;` showed no value, and
/// `enum LuaTokenTypeDetail { TK_AND = FIRST_RESERVED, … }` showed none for `TK_AND` either. The layer that folds it
/// is `sema::constant` — arithmetic, literals, and names it can be told the value of (a file-local constant, an
/// enumerator, an object-like macro).
#[test]
pub(crate) fn a_constants_value_is_folded_and_an_enumerators_initialiser_reads_it() {
    let text = "#define UCHAR_MAX 0xff\n\
                const int FIRST_RESERVED = UCHAR_MAX + 1;\n\
                enum Kind { TK_AND = FIRST_RESERVED, TK_BREAK, TK_SHIFT = 1 << 4 };\n\
                int plain = 5;\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("a-folded-constant", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let value_at = |written: &str| {
        let at = text.find(written).expect("the fixture");
        session.constant_value_at(&view, crate::file::coordinates::ReadingOffset::new(at))
    };

    assert_eq!(
        value_at("FIRST_RESERVED = "),
        Some(256),
        "`UCHAR_MAX + 1` folds: `0xff` + 1"
    );
    assert_eq!(
        value_at("TK_AND"),
        Some(256),
        "the enumerator names the constant"
    );
    assert_eq!(
        value_at("TK_BREAK"),
        Some(257),
        "and the next one counts on"
    );
    assert_eq!(value_at("TK_SHIFT"), Some(16), "`1 << 4` folds");
    assert_eq!(
        value_at("plain = 5"),
        None,
        "a non-const variable has a value but not a constant one"
    );
}
/// **A member list through an alias written with an absolute target** — the shape the renderer produces.
///
/// `std::string` is `using string = basic_string<char, …>;`, and a target read out of the **rendering** is spelled
/// `:: std :: basic_string<char, …>`: the preprocessor writes a macro's expansion spaced and absolute. Every later
/// lookup keys on the spelling, so the chain gave up (`is_declared` answered false for `:: std :: basic_string`) and
/// `members_of` answered **0 members** — which is what an editor saw as "the type is deduced but nothing completes
/// after `enenn.`".
#[test]
pub(crate) fn a_member_list_is_reached_through_an_alias_written_absolutely() {
    let text = "namespace std {\n\
                template <class T> struct basic_string {\n\
                \x20   void append(const T &value);\n\
                \x20   int size() const;\n\
                };\n\
                using string = ::std::basic_string<char>;\n\
                }\n\
                void g() {\n\
                \x20   std::string s;\n\
                \x20   s.\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("an-absolute-alias-target", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let at = text.find("s.\n").expect("the fixture") + "s.".len();
    let completions =
        session.member_completions(&view, crate::file::coordinates::FileOffset::new(at));
    let Known::Yes(completions) = completions else {
        panic!("`s.` must list `basic_string`'s members: {completions:?}");
    };
    let mut names: Vec<&str> = completions
        .members
        .members
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        names,
        vec!["append", "size"],
        "the alias's absolute target names a class the index has"
    );
}

#[test]
pub(crate) fn a_calls_overload_is_the_one_its_arguments_select() {
    let text = "namespace std {\n\
                struct string { };\n\
                struct wstring { };\n\
                template <class... T> struct format_string { };\n\
                template <class... T> struct wformat_string { };\n\
                template <class... T> string format(const format_string<T...>, T &&...);\n\
                template <class... T> wstring format(const wformat_string<T...>, T &&...);\n\
                }\n\
                void g() {\n\
                \x20   auto narrow = std::format(\"x {}\", 1);\n\
                \x20   auto wide = std::format(L\"x {}\", 1);\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("a-calls-selected-overload", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    for (written, expected) in [("auto narrow", "string"), ("auto wide", "wstring")] {
        let at = text.find(written).expect("the fixture") + "auto ".len();
        let typed = session.type_at(&view, crate::file::coordinates::ReadingOffset::new(at));
        let Known::Yes(typed) = typed else {
            panic!("`{written} = std::format(…)` must have a type: {typed:?}");
        };
        assert_eq!(
            typed.type_of, expected,
            "the arguments select the overload — {written}"
        );
    }
}
/// **A name with several declarations and no type they agree on says *that*, rather than "unknown type".**
///
/// Reported from an editor: `auto enenn = std::format(FULL_NAME_FORMAT, myName.getFullName(0));` showed no type at
/// all — no type in the card, no inlay hint — and nothing said why. Measured on the real project, `std::format` has
/// four overloads visible (narrow and wide, with and without a locale) and the call selects none of them, so refusing
/// is right; what was wrong is that the refusal said `UnknownType("std::format")`, which reads like a missing name.
/// `UnknownReason::Ambiguous` has a sentence a reader can act on, and the hover card now prints it.
#[test]
pub(crate) fn a_name_whose_declarations_disagree_answers_with_the_disagreement() {
    // **Two headers, one name, two return types** — the shape `std::format` has: the *index* answers it, so the
    // scope tree's own "first declaration" rule is not what decides. (A single file's two declarations are resolved by
    // `definition_at` to the first, which is the ordinary rule for one scope and is pinned by its own tests.)
    let header = "int f();\n";
    let other = "double f();\n";
    let text = "#include \"a.h\"\n#include \"b.h\"\nvoid g() {\n    auto x = f();\n}\n";
    let files = MemoryFiles::new()
        .with_file("/p/a.h", header)
        .with_file("/p/b.h", other)
        .with_file("/p/a.cpp", text);
    let fixture = Memory::new("a-disagreeing-name", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let at = text.find("auto x").expect("the fixture") + "auto ".len();
    let typed = session.type_at(&view, crate::file::coordinates::ReadingOffset::new(at));
    match typed {
        Known::Unknown(crate::UnknownReason::Ambiguous(name)) => assert_eq!(&*name, "f"),
        other => panic!("`int f()` and `double f()` agree on no type: {other:?}"),
    }
}
/// **An unscoped enumeration's enumerators are visible one scope *out*.** — the C++ rule, and the one place a scope
/// tree built as a mirror of the syntax gets it wrong.
///
/// Reported from an editor: `enum { MyClass = 123, AnotherClass };` with `printf("…", AnotherClass)` coloured the
/// *definition* as an enum member and nothing at all at the use — the client's own fallback drew it as an unknown
/// variable, "go to definition" found nothing, and the same would be true of hover and completion. The bindings live
/// in the enum's own scope, which is a **child** of the scope that can see them, so the upward lookup never reached
/// them; [`crate::sema::resolve::definitions_at`] now looks one scope out for an `enum` and not for an `enum class`.
#[test]
pub(crate) fn an_unscoped_enumerations_member_is_visible_in_the_enclosing_scope() {
    let text = "enum { MyClass = 123, AnotherClass };\n\
                enum class Scoped { Hidden };\n\
                int main() {\n\
                \x20   int a = AnotherClass;\n\
                \x20   int b = Hidden;\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("an-unscoped-enum", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    // The use of `AnotherClass` resolves — and the name it resolves to is the enumerator.
    let at = text.find("= AnotherClass").expect("the fixture") + 2;
    let found = session.definitions(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(found) = found else {
        panic!("a bare `AnotherClass` must resolve: {found:?}");
    };
    assert_eq!(
        found.found[0].fact.name, "AnotherClass",
        "the enumerator, not the enumeration"
    );

    // …and it is classified as an **enum member**, which is what the highlighter draws.
    let classified: Vec<(String, crate::semantic::NameKind)> = session
        .classified_names(&view)
        .into_iter()
        .map(|name| {
            (
                text[name.range.start_offset..]
                    .chars()
                    .take(12)
                    .collect::<String>(),
                name.kind,
            )
        })
        .collect();
    let kinds: Vec<&crate::semantic::NameKind> = classified
        .iter()
        .filter(|(spelled, _)| spelled.starts_with("AnotherClass"))
        .map(|(_, kind)| kind)
        .collect();
    assert!(
        kinds.contains(&&crate::semantic::NameKind::EnumMember),
        "the use is an enum member too, not just the declaration: {classified:?}"
    );
    assert!(
        kinds.len() >= 2,
        "both the declaration and the use are coloured: {classified:?}"
    );

    // **The declaration answers too.** An enumerator's name is a plain token rather than a `NameExpr`, so
    // `definitions` at the *definition* used to answer `Unknown(UnparsableName)` — the second half of the editor
    // report ("在匿名枚举的定义本身没有 hover 结果") — while hovering the use worked.
    let declaration = text.find("AnotherClass }").expect("the fixture");
    let found = session.definitions(
        &view,
        crate::file::coordinates::ReadingOffset::new(declaration),
    );
    let Known::Yes(found) = found else {
        panic!("the declaration of `AnotherClass` must answer: {found:?}");
    };
    assert_eq!(found.found[0].fact.name, "AnotherClass");

    // **`enum class` still hides its enumerators**: `Scoped::Hidden` is the only way to name it.
    let hidden = text.find("= Hidden").expect("the fixture") + 2;
    let found = session.definitions(&view, crate::file::coordinates::ReadingOffset::new(hidden));
    assert!(
        !matches!(&found, Known::Yes(found) if found.found[0].fact.name == "Hidden"),
        "a scoped enumeration's member is not visible here: {found:?}"
    );
}
/// **A trait alias selects its specialization** — `conditional_t<false, void, T>` is `T`, and the arrow reaches it.
///
/// The lookup therefore has to pick the declaration the *use* is an instance of — the pattern rule,
/// with the arguments split where the language splits them.
#[test]
pub(crate) fn a_trait_alias_selects_the_specialization_its_arguments_name() {
    let text = "struct Item { int value; void grow(); };\n\
                template <bool B, class T, class F> struct conditional {};\n\
                template <class T, class F> struct conditional<false, T, F> { using type = F; };\n\
                template <bool B, class T, class F>\n\
                using conditional_t = typename conditional<B, T, F>::type;\n\
                template <typename T> struct handle {\n\
                \x20   using value_type = conditional_t<false, void, T>;\n\
                \x20   value_type *operator->() const;\n\
                };\n\
                void g(handle<Item> h) {\n\
                \x20   h->\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("a-trait-alias-selects", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let written = "h->";
    let at = text.find(written).expect("the fixture") + written.len();
    let completions =
        session.member_completions(&view, crate::file::coordinates::FileOffset::new(at));
    let Known::Yes(completions) = completions else {
        panic!("`{written}` must reach `Item`: {completions:?}");
    };
    let names: Vec<&str> = completions
        .members
        .members
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect();
    assert_eq!(
        completions.class, "Item",
        "`conditional_t<false, void, T>` is `T`, and the arrow reaches it ({names:?})"
    );
}
/// **`find()->second`**: an arrow whose answer is a *name* the class declares as an alias.
///
/// `std::map::find` answers an iterator, and MSVC's `_Tree_iterator::operator->` is declared as
///
/// ```cpp
/// using pointer = _Val*;          // _Val is the tree's value_type — a `pair<const K, V>`
/// _NODISCARD pointer operator->() const noexcept;
/// ```
///
/// so the arrow's answer is the *spelling* `pointer`, not a class.
#[test]
pub(crate) fn an_arrow_through_an_alias_reaches_the_pointee() {
    let text = "template <typename K, typename V> struct pair {\n\
                \x20   K first;\n\
                \x20   V second;\n\
                };\n\
                template <typename T> struct iterator {\n\
                \x20   using pointer = T *;\n\
                \x20   pointer operator->() const;\n\
                };\n\
                template <typename K, typename V> struct map {\n\
                \x20   using value_type = pair<const K, V>;\n\
                \x20   iterator<value_type> find(const K &key) const;\n\
                };\n\
                void f(map<int, float> &m) {\n\
                \x20   m.find(1)->second = 2;\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("an-arrow-through-an-alias", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let at = text.find("->second").expect("the fixture") + "->".len();
    let found = session.definitions(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(found) = found else {
        panic!("`find()->second` must resolve through `pointer`: {found:?}");
    };
    assert_eq!(
        (
            found.found[0].fact.name.as_str(),
            found.found[0].fact.scope.as_deref()
        ),
        ("second", Some("pair")),
        "the arrow's `pointer` is the value type, and `second` is its member"
    );
}
/// **An out-of-line definition's type is the type it writes, not its own qualified name.**
///
/// Reported from an editor, three times in one sitting, always as a *type* that was really a name:
///
/// ```cpp
/// std::map<std::string, LuaTokenKind, std::less<>> LuaLexer::LuaReserved = { … };   // hover: `LuaLexer::LuaReserved`
/// std::size_t LuaSource::GetLine(std::size_t offset) const { … }                   // hover: `LuaSource::GetLine`
/// ```
///
/// and the member lookups that followed answered "`LuaLexer::LuaReserved` has no member `find`". The grammar puts a
/// qualified definition's name in the **specifier sequence** — the declarator holds only the parameter list — so a
/// reader that takes "the last name of the specifiers" as the type takes the declarator's own name. A definition and
/// its declaration are both in the index, and whoever asks may get either.
#[test]
pub(crate) fn an_out_of_line_definition_records_the_type_it_writes() {
    let text = "namespace std {\n\
                template <typename K, typename V> struct map { V &at(const K &key); };\n\
                }\n\
                struct Lexer {\n\
                \x20   static std::map<int, int> Reserved;\n\
                \x20   std::size_t GetLine(std::size_t offset) const;\n\
                };\n\
                std::map<int, int> Lexer::Reserved = {};\n\
                std::size_t Lexer::GetLine(std::size_t offset) const { return offset; }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("an-out-of-line-definition", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");

    let summary = session.index().summary(&path).expect("the file is read");
    for (name, expected) in [
        ("Reserved", "std::map<int, int>"),
        ("GetLine", "std::size_t"),
    ] {
        let written: Vec<Option<&str>> = summary
            .declarations
            .iter()
            .filter(|fact| fact.name == name)
            .map(|fact| fact.type_of.as_deref().or(fact.returns.as_deref()))
            .collect();
        assert!(
            written.contains(&Some(expected)),
            "`{name}` declares `{expected}`, and one of its facts must say so: {written:?}"
        );
        let qualified = format!("::{name}");
        assert!(
            !written
                .iter()
                .any(|found| found.is_some_and(|found| found.ends_with(&qualified))),
            "and none of them may record the declarator's own qualified name (`…{qualified}`): {written:?}"
        );
    }
}
/// **A class template's member answer is substituted with the use's arguments** — `b[0].value` and `b->value`.
///
/// The two reports from an editor (`events[Pos].U` on a `std::vector<MarkEvent>`, `file->setUri` on a
/// `std::shared_ptr<LuaSource>`) both ended the same way: the member question was asked about a class called `_Ty` or
/// `_Ty2` — the *declaration's* spelling of the class's own parameter, not the type the use wrote. Measured on the real
/// project with `tests/inference_probe.rs`:
///
/// ```text
/// events[Pos].   → Unknown(NotDeclaredHere("_Ty"))
/// file->         → class "_Ty2": 0 member(s)
/// ```
///
/// This fixture is the same question with names that say what is happening.
#[test]
pub(crate) fn a_class_templates_members_are_read_through_the_uses_arguments() {
    // **Three shapes, because the standard library uses all three.** `T& operator[]` states the parameter itself;
    // `reference operator[]` names an alias *of the class*, and the alias is what says `T&` — `std::vector`'s spelling,
    // and the one that answered `NotDeclaredHere("_Ty")` on a real project; `T *operator->` is `shared_ptr`'s.
    let text = "template <typename T> struct Box {\n\
                \x20   T &operator[](int index);\n\
                \x20   T *operator->();\n\
                };\n\
                template <typename T> struct AliasBox {\n\
                \x20   using reference = T &;\n\
                \x20   reference operator[](int index);\n\
                };\n\
                struct Item { int value; void grow(); };\n\
                void f(Box<Item> box) {\n\
                \x20   box[0].value;\n\
                \x20   box->grow();\n\
                }\n\
                void g(AliasBox<Item> box) {\n\
                \x20   alias[0].value;\n\
                }\n";
    let text = text.replace("alias[0]", "box[0]");
    let files = MemoryFiles::new().with_file("/p/a.cpp", &text);
    let fixture = Memory::new("a-class-templates-members", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", &text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    // **The member after the operator, as a definition question** — the shape both reports had: `events[Pos].U` and
    // `file->setUri` are jumps, and a jump that asks about `_Ty` finds nothing.
    //
    // The third one searches *after* the first two, because the fixture writes `box[0].value` twice and the cursor of
    // each case has to be in its own function.
    let mut searched = 0usize;
    for (written, expected) in [
        ("box[0].value", ("value", Some("Item"))),
        ("box->grow", ("grow", Some("Item"))),
        ("box[0].value", ("value", Some("Item"))),
    ] {
        let found_at = text[searched..]
            .find(written)
            .map(|at| searched + at)
            .expect("the fixture writes it");
        searched = found_at + written.len();
        let at = found_at + written.len() - expected.0.len();
        let found = session.definitions(&view, crate::file::coordinates::ReadingOffset::new(at));
        let Known::Yes(found) = found else {
            panic!("`{written}` must resolve to the element's member: {found:?}");
        };
        assert_eq!(
            (
                found.found[0].fact.name.as_str(),
                found.found[0].fact.scope.as_deref()
            ),
            expected,
            "`{written}` is the element's member"
        );
    }
}
/// **A member template's own parameter is not the class's** — the shape `std::shared_ptr` is written in.
///
/// MSVC's `<memory>` declares
///
/// ```cpp
/// template <class _Ty> class shared_ptr : public _Ptr_base<_Ty> {
///     template <class _Ty2 = _Ty, enable_if_t<!is_array_v<_Ty2>, int> = 0>
///     _NODISCARD _Ty2* operator->() const noexcept { return get(); }
/// ```
///
/// so the member's answer mentions `_Ty2` — a parameter of the *member template*, defaulted to the class's `_Ty`. The
/// fact records the member's parameters as names, without their defaults, so substituting with the class's arguments
/// leaves `_Ty2` standing and the member lookup asks about a class called `_Ty2` (measured on EmmyLuaCodeStyle's
/// `LuaFormat.cpp`: `file->` → `class "_Ty2": 0 member(s)`).
///
/// What settles it is the convention every pointer-like type in the standard library follows: `element_type` is what
/// the arrow reaches.
#[test]
pub(crate) fn an_arrow_through_a_member_template_reaches_the_element_type() {
    // **The shape MSVC writes**, because it is the one that broke: the base is named through an alias, and the alias
    // being re-exposed (`using typename _Mybase::element_type;`) declares no type of its own.
    let header = "template <typename T> struct Holder {\n\
                  \x20   using element_type = T;\n\
                  };\n\
                  template <typename T> struct Handle {\n\
                  \x20 private:\n\
                  \x20   using _Mybase = Holder<T>;\n\
                  \x20 public:\n\
                  \x20   using typename _Mybase::element_type;\n\
                  \x20   template <typename U = T, int = 0>\n\
                  \x20   U *operator->() const;\n\
                  };\n";
    let source = "#include \"handle.h\"\n\
                  struct Item { int value; void grow(); };\n\
                  void g(Handle<Item> file) {\n\
                  \x20   file->\n\
                  }\n";
    let files = MemoryFiles::new()
        .with_file("/p/handle.h", header)
        .with_file("/p/a.cpp", source);
    let fixture = Memory::new("an-arrow-through-a-member-template", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", source);
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let written = "file->";
    let at = source.find(written).expect("the fixture") + written.len();
    let completions =
        session.member_completions(&view, crate::file::coordinates::FileOffset::new(at));
    let Known::Yes(completions) = completions else {
        panic!("`{written}` must list the element's members: {completions:?}");
    };
    let names: Vec<&str> = completions
        .members
        .members
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect();
    assert_eq!(
        completions.class, "Item",
        "the arrow reaches `element_type`, not the member template's parameter name ({names:?})"
    );
}
/// **The real shape: a base in another file, a namespace, and an alias with arguments.**
///
/// The fixture above keeps `element_type` in the same file; this one is MSVC's layout — `std::_Ptr_base` in one header,
/// `std::shared_ptr` in another, the base named through `using _Mybase = _Ptr_base<_Ty>;` and the alias re-exposed with
/// `using typename _Mybase::element_type;`. It exists because the arrow's fallback works on the small fixture and did
/// **not** work on a real project, and the difference had to be found somewhere cheaper than a 95-second probe run.
#[test]
pub(crate) fn an_arrow_through_a_base_named_by_a_member_alias_in_another_file() {
    // **A forward declaration first**, which is what MSVC's `<memory>` has and what a member list must look past: the
    // first visible declaration of a class is not always the one with a body.
    // **The trait, not the parameter** — MSVC's `_Ptr_base` says `using element_type = remove_extent_t<_Ty>;`, and
    // `remove_extent_t` is an alias *template* whose target the library writes through `typename remove_extent<T>::type`.
    // A layer that follows spellings arrives at the bare name `remove_extent_t`, which is an alias and not a class; the
    // answer for a pointer-like handle is the class's own argument, and that is what the last part of the arrow's
    // fallback is for.
    let base = "namespace std {\n\
                template <typename T> class _Ptr_base;\n\
                template <typename T> struct remove_extent { using type = T; };\n\
                template <typename T> using remove_extent_t = typename remove_extent<T>::type;\n\
                template <typename T> class _Ptr_base {\n\
                public:\n\
                \x20   using element_type = remove_extent_t<T>;\n\
                };\n\
                }\n";
    let memory = "#include \"ptr_base.h\"\nnamespace std {\n\
                  template <typename T> class shared_ptr {\n\
                  private:\n\
                  \x20   using _Mybase = _Ptr_base<T>;\n\
                  public:\n\
                  \x20   using typename _Mybase::element_type;\n\
                  \x20   template <typename U = T, int = 0>\n\
                  \x20   U *operator->() const;\n\
                  \x20   element_type *get() const;\n\
                  };\n\
                  }\n";
    let source = "#include \"memory.h\"\n\
                  struct Item { int value; void grow(); };\n\
                  void g(std::shared_ptr<Item> file) {\n\
                  \x20   file->\n\
                  }\n";
    let files = MemoryFiles::new()
        .with_file("/p/ptr_base.h", base)
        .with_file("/p/memory.h", memory)
        .with_file("/p/a.cpp", source);
    let fixture = Memory::new("an-arrow-through-a-real-shape", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", source);
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let written = "file->";
    let at = source.find(written).expect("the fixture") + written.len();
    let completions =
        session.member_completions(&view, crate::file::coordinates::FileOffset::new(at));
    let Known::Yes(completions) = completions else {
        panic!("`{written}` must list the element's members: {completions:?}");
    };
    let names: Vec<&str> = completions
        .members
        .members
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect();
    assert_eq!(
        completions.class, "Item",
        "the arrow reaches the base alias's `element_type` ({names:?})"
    );
}
/// **The same question when the class is declared in a header** — the shape the real project has, and the one that
/// answers `_Ty`.
///
/// `std::vector`'s `operator[]` is declared `reference operator[](size_type)`, and `reference` is `_Ty&` — an alias
/// *inside* the class. When the class is declared in another file, the alias is followed by
/// `ProjectIndex::template_parameters_of`/`resolve_aliases`, which has the *spelling* and not the use's arguments, so
/// it answers `_Ty&` and the member lookup then asks about a class called `_Ty`. Measured on EmmyLuaCodeStyle's
/// `Mark.cpp` (`events[Pos].U`): `Unknown(NotDeclaredHere("_Ty"))`.
#[test]
pub(crate) fn a_class_templates_alias_return_is_substituted_too() {
    let header = "template <typename T> struct AliasBox {\n\
                  \x20   using reference = T &;\n\
                  \x20   reference operator[](int index);\n\
                  };\n";
    let source = "#include \"box.h\"\n\
                  struct Item { int value; void grow(); };\n\
                  void g(AliasBox<Item> box) {\n\
                  \x20   box[0].value;\n\
                  }\n";
    let files = MemoryFiles::new()
        .with_file("/p/box.h", header)
        .with_file("/p/a.cpp", source);
    let fixture = Memory::new("a-class-templates-alias", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", source);
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let written = "box[0].value";
    let at = source.find(written).expect("the fixture") + written.len() - "value".len();
    let found = session.definitions(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(found) = found else {
        panic!("`{written}` must resolve to the element's member: {found:?}");
    };
    assert_eq!(
        (
            found.found[0].fact.name.as_str(),
            found.found[0].fact.scope.as_deref()
        ),
        ("value", Some("Item")),
        "the alias's target is the use's argument, not the class's parameter name"
    );
}
/// **An anonymous enum declares its enumerators** — `enum { LAJF };`, the C spelling of a set of constants.
///
/// Reported from an editor as "an unnamed enum is not supported", and it was literally true: the declaration names
/// nothing, `is_unnamed_declaration` was consulted before the body was walked, and the walker returned — so every
/// enumerator of every anonymous enum was missing from the scope tree and from the index. Two lines above it the file
/// declares `LAJF`, and `definitions("LAJF")` answered `NotDeclaredHere`.
#[test]
pub(crate) fn an_anonymous_enum_declares_its_enumerators() {
    let text = "enum { LAJF, LBJF };\nvoid f() { int x = LAJF; }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("an-anonymous-enum", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");

    for name in ["LAJF", "LBJF"] {
        let found = session.index().definitions(name, &path);
        let Known::Yes(found) = found else {
            panic!("`{name}` is declared by the anonymous enum: {found:?}");
        };
        assert_eq!(
            found.found[0].fact.scope, None,
            "an unnamed enum has no scope name"
        );
    }

    // **And the name is classified as an enumerator**, which is what the semantic pass draws.
    let view = session.view_of_the_file(&path).expect("a view");
    let names = session.classified_names(&view);
    let lajf = names
        .iter()
        .find(|name| name.range.start_offset == text.find("LAJF").expect("the fixture"))
        .expect("`LAJF` is a name");
    assert_eq!(lajf.kind, crate::semantic::NameKind::EnumMember);
}
/// **An enumerator of an anonymous enum is an enumerator in the file that *uses* it too.**
///
/// The declaration was already right (the test above), and the use in another file was not: the index has one kind
/// for "a name that holds a value" and used to infer an enumerator from *a written type being absent* plus *a scope
/// being present*. Both halves fail for the shape C writes most, an anonymous `enum { WILD_FREE, … };` whose members
/// have no scope — measured on neovim, `WILD_FREE` in `cmdexpand.c` was drawn as a variable while its own
/// declaration card said "an enumerator of (an anonymous enum)". The fact carries the flag now
/// ([`DeclFact::enumerator`]), and this is the test that reads it from the far side.
#[test]
pub(crate) fn an_enumerators_use_in_another_file_is_an_enum_member() {
    let header = "enum { WILD_FREE = 1, WILD_NEXT = 2 };\n";
    let source = "#include \"api.h\"\nint f(void) { return WILD_FREE; }\n";
    let files = MemoryFiles::new()
        .with_file("/p/api.h", header)
        .with_file("/p/main.c", source);
    let fixture = Memory::new("an-enumerators-use", &files);
    let mut session = fixture.session();
    session.did_open("/p/main.c", source);
    session.index_everything();
    session.catch_up(std::path::Path::new("/p/main.c"));

    let path = std::path::PathBuf::from("/p/main.c");
    let view = session.view_of_the_file(&path).expect("a view");
    let at = source.find("WILD_FREE").expect("the use");
    let used = session
        .classified_names(&view)
        .into_iter()
        .find(|name| name.range.start_offset == at)
        .expect("`WILD_FREE` is a name");
    assert_eq!(
        used.kind,
        crate::semantic::NameKind::EnumMember,
        "the declaration says an enumerator, so the use must too"
    );
}

/// **A member reached through `operator->`** — `config->Generate` on a smart-pointer-shaped class.
///
/// Reported twice from an editor, in one sentence: *"a member cannot be found through a smart pointer, and there is no
/// completion either"*. The handle's own member list is `operator->`, `get`, `reset`; the names a reader is reaching
/// for after an arrow are the pointee's. Both halves are answered here: the lookup follows what `operator->` returns
/// (substituted with the arguments the use wrote, because the return type is the *class's* spelling — `_Ty *` for
/// `std::shared_ptr`), and the member list after `->` is the pointee's.
#[test]
pub(crate) fn a_member_through_an_operator_arrow_is_the_pointees() {
    let text = "template <typename T> struct handle { T *operator->(); void reset(); };\n\
                struct Config { int Generate(int uri); };\n\
                void g(handle<Config> config) { config->Generate(1); }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("through-an-arrow", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    // **The lookup**: `Generate` is `Config`'s, not the handle's.
    let at = text.find("Generate(1)").expect("the fixture") + 1;
    let found = session.definition(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(found) = found else {
        panic!("`config->Generate` must resolve through `operator->`: {found:?}");
    };
    assert_eq!(
        (found.fact.name.as_str(), found.fact.scope.as_deref()),
        ("Generate", Some("Config")),
        "the member is the pointee's"
    );

    // **And the list**, after the arrow: the pointee's declarations.
    let after = text.find("config->Generate").expect("the fixture") + "config->".len();
    let completions =
        session.member_completions(&view, crate::file::coordinates::FileOffset::new(after));
    let Known::Yes(completions) = completions else {
        panic!("`config->` must list the pointee's members: {completions:?}");
    };
    assert_eq!(completions.class, "Config");
    let names: Vec<&str> = completions
        .members
        .members
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect();
    assert_eq!(names, ["Generate"], "and not the handle's own list");
}

/// **A call of an object whose `operator()` returns `auto`** — the shape every range adaptor is written in.
///
/// MSVC and libc++ spell a niebloid as a struct whose `operator()` is `constexpr auto operator()(…)` with the
/// answer in the body, and that keyword is what a fact records. `std::views::filter(pred)` therefore had no type at
/// all, and with it no range pipeline: `items | std::views::filter(…)` could not be typed and the inlay hint was
/// silent. The return has to be *deduced* from the operator's body, the way a function whose fact carries no usable
/// return type already is (see `deduced_returns_of_substituted`).
#[test]
pub(crate) fn a_call_of_an_object_deduces_an_auto_returning_operator() {
    let text = "struct closure { int value; };\n\
                struct functor {\n\
                \x20   auto operator()(int x) const { return closure{x}; }\n\
                };\n\
                functor make_one;\n\
                void use() {\n\
                \x20   auto c = make_one(1);\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("an-auto-returning-operator", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let at = text.find("make_one(1)").expect("the fixture") + "make_one".len();
    let typed = session.type_at(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(typed) = typed else {
        panic!("`make_one(1)` must have a type: {typed:?}");
    };
    assert_eq!(
        typed.type_of, "closure",
        "the operator's body returns `closure`, and `auto` is not a type"
    );
}

/// **The same deduction across a file boundary** — the shape `<ranges>` actually has.
///
/// The unit test above keeps the functor and its call in one file; a range adaptor is a class in a *header* and the
/// call is in the buffer. `operator_returns_of` reads the member through `member_fact` and then the body through the
/// `declaring` closure, and either step can lose the class when it lives in another file.
#[test]
pub(crate) fn a_call_of_an_object_in_another_file_deduces_the_operator() {
    let header = "struct closure { int value; };\n\
                  struct functor {\n\
                  \x20   auto operator()(int x) const { return closure{x}; }\n\
                  };\n\
                  extern functor make_one;\n";
    let source = "#include \"a.hpp\"\n\
                  void use() {\n\
                  \x20   auto c = make_one(1);\n\
                  }\n";
    let files = MemoryFiles::new()
        .with_file("/p/a.hpp", header)
        .with_file("/p/a.cpp", source);
    let fixture = Memory::new("an-auto-returning-operator-across-files", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", source);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let at = source.find("make_one(1)").expect("the fixture") + "make_one".len();
    let typed = session.type_at(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(typed) = typed else {
        panic!("`make_one(1)` must have a type: {typed:?}");
    };
    assert_eq!(
        typed.type_of, "closure",
        "the operator is declared in the header and its body returns `closure`"
    );
}

/// **The `<ranges>` shape exactly**: a class in a header, a namespace alias, and `inline constexpr` adaptor.
///
/// This is the fixture that failed end to end — `items | std::views::filter(…)` had no type while the same call
/// through an unqualified name of a local functor deduced correctly. Everything the real header does is here:
/// `std::views` is a namespace **alias** for `std::ranges::views`, the adaptor is an `inline constexpr` variable
/// whose `operator()` returns `auto`, and the call is written through the alias.
#[test]
pub(crate) fn an_adaptor_behind_a_namespace_alias_deduces_its_operator() {
    let header = "namespace std {\n\
                  namespace ranges {\n\
                  namespace views {\n\
                  struct closure { int value; };\n\
                  struct _Filter_fn {\n\
                  \x20   template <class Pred>\n\
                  \x20   auto operator()(Pred&& pred) const { return closure{1}; }\n\
                  };\n\
                  inline constexpr _Filter_fn filter;\n\
                  }\n\
                  }\n\
                  namespace views = ranges::views;\n\
                  }\n";
    let source = "#include \"a.hpp\"\n\
                  void use() {\n\
                  \x20   auto c = std::views::filter(1);\n\
                  }\n";
    let files = MemoryFiles::new()
        .with_file("/p/a.hpp", header)
        .with_file("/p/a.cpp", source);
    let fixture = Memory::new("an-adaptor-behind-an-alias", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", source);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let at =
        source.find("std::views::filter(1)").expect("the fixture") + "std::views::filter".len();
    let typed = session.type_at(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(typed) = typed else {
        panic!("`std::views::filter(1)` must have a type: {typed:?}");
    };
    assert_eq!(
        typed.type_of, "closure",
        "the adaptor's `operator()` returns `closure`, through a namespace alias and a header"
    );
}

/// **The operator's return depends on the operator's own template parameter** — `<ranges>`'s exact shape.
///
/// `_Filter_fn::operator()` returns `_Range_closure<_Filter_fn, decay_t<_Pr>>{…}`, so the body's expression names
/// the *argument's* type. A deduction that does not know what the call passed cannot spell it, and the call stays
/// unknown. This asserts the answer the argument decides.
#[test]
pub(crate) fn an_auto_returning_operator_naming_its_argument_is_deduced() {
    let text = "template <class T> struct wrapper { T value; };\n\
                struct functor {\n\
                \x20   template <class Pred>\n\
                \x20   auto operator()(Pred&& pred) const { return wrapper<Pred>{}; }\n\
                };\n\
                functor make_one;\n\
                void use() {\n\
                \x20   auto c = make_one(1);\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("an-operator-naming-its-argument", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let at = text.find("make_one(1)").expect("the fixture") + "make_one".len();
    let typed = session.type_at(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(typed) = typed else {
        panic!("`make_one(1)` must have a type: {typed:?}");
    };
    assert_eq!(
        typed.type_of, "wrapper<int>",
        "the body returns `wrapper<Pred>` and the call passed an `int`"
    );
}

/// **When the argument has no nameable type, the answer keeps the parameter** — better than nothing.
///
/// A lambda's type is unnamed, so nothing can substitute `_Pr` in
/// `_Range_closure<_Filter_fn, decay_t<_Pr>>` — and refusing the whole call for that reason is what made
/// `std::views::filter([](const Item& item) { … })` answer `UnknownType`. The spelling with the parameter standing
/// is a type a reader can hover, and it is what the file wrote.
#[test]
pub(crate) fn an_auto_returning_operator_called_with_a_lambda_still_answers() {
    let text = "template <class T> struct remove_reference { using type = T; };\n\
                template <class T> using decay_t = typename remove_reference<T>::type;\n\
                template <class F, class P> struct range_closure { };\n\
                struct _Filter_fn {\n\
                \x20   template <class Pred>\n\
                \x20   auto operator()(Pred&& pred) const { return range_closure<_Filter_fn, decay_t<Pred>>{}; }\n\
                };\n\
                inline constexpr _Filter_fn filter;\n\
                void use() {\n\
                \x20   auto c = filter([](int v) { return v > 0; });\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("an-operator-called-with-a-lambda", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let at = text.find("filter([]").expect("the fixture") + "filter".len();
    let typed = session.type_at(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(typed) = typed else {
        panic!("`filter(lambda)` must answer something: {typed:?}");
    };
    assert!(
        typed.type_of.starts_with("range_closure<"),
        "the spelling the body wrote is the answer, with the parameter standing: {:?}",
        typed.type_of
    );
}

/// **The overloaded shape the real adaptor has**: one `operator()` that takes a range and a predicate, one that
/// takes the predicate alone. `member_fact` answers the *first* declaration of a name, and the first one here is
/// the two-argument form — whose return is the same closure, so the answer must not depend on which was chosen.
#[test]
pub(crate) fn an_overloaded_adaptor_deduces_the_operator_that_the_arguments_fit() {
    let text = "template <class F, class P> struct range_closure { };\n\
                struct range { int value; };\n\
                struct view { };\n\
                struct _Filter_fn {\n\
                \x20   template <class Rng, class Pred>\n\
                \x20   auto operator()(Rng&& rng, Pred&& pred) const { return view{}; }\n\
                \x20   template <class Pred>\n\
                \x20   auto operator()(Pred&& pred) const { return range_closure<_Filter_fn, Pred>{}; }\n\
                };\n\
                inline constexpr _Filter_fn filter;\n\
                void use() {\n\
                \x20   auto c = filter([](int v) { return v > 0; });\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("an-overloaded-adaptor", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let at = text.find("filter([]").expect("the fixture") + "filter".len();
    let typed = session.type_at(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(typed) = typed else {
        panic!("`filter(lambda)` must answer something: {typed:?}");
    };
    assert!(
        typed.type_of.starts_with("range_closure<"),
        "the one-argument form is the one the call fits: {:?}",
        typed.type_of
    );
}

/// **The exact combination `<ranges>` writes**: an alias template in the return *and* a lambda argument.
///
/// Either half alone is covered and passes — `decay_t` with an `int` argument, and a lambda with a return that names
/// the parameter directly. Together they are the real adaptor, and this is the shape that still fails end to end.
#[test]
pub(crate) fn an_alias_in_the_return_survives_a_lambda_argument() {
    let text = "template <class T> struct remove_reference { using type = T; };\n\
                template <class T> using decay_t = typename remove_reference<T>::type;\n\
                template <class F, class P> struct range_closure { };\n\
                struct _Filter_fn {\n\
                \x20   template <class Pred>\n\
                \x20   auto operator()(Pred&& pred) const { return range_closure<_Filter_fn, decay_t<Pred>>{}; }\n\
                };\n\
                inline constexpr _Filter_fn filter;\n\
                void use() {\n\
                \x20   auto c = filter([](int v) { return v > 0; });\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", text);
    let fixture = Memory::new("an-alias-in-the-return-and-a-lambda", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let at = text.find("filter([]").expect("the fixture") + "filter".len();
    let typed = session.type_at(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(typed) = typed else {
        panic!("`filter(lambda)` must answer something: {typed:?}");
    };
    assert!(
        typed.type_of.starts_with("range_closure<"),
        "the body's spelling is the answer: {:?}",
        typed.type_of
    );
}

/// **Every feature of the real adaptor at once**: a header, a namespace alias, two overloads (the two-argument one
/// first), an alias template in the return, and a call through the alias.
///
/// Each of those is covered on its own and passes; this is the combination the live server fails on, and it is the
/// last shape that can be checked without the server.
#[test]
pub(crate) fn the_real_adaptor_shape_deduces_the_closure() {
    let header = "namespace std {\n\
                  namespace ranges {\n\
                  namespace views {\n\
                  template <class T> struct remove_reference { using type = T; };\n\
                  template <class T> using decay_t = typename remove_reference<T>::type;\n\
                  template <class F, class P> struct range_closure { };\n\
                  struct view { };\n\
                  struct _Filter_fn {\n\
                  \x20   template <class Rng, class Pred>\n\
                  \x20   auto operator()(Rng&& rng, Pred&& pred) const { return view{}; }\n\
                  \x20   template <class Pred>\n\
                  \x20   auto operator()(Pred&& pred) const { return range_closure<_Filter_fn, decay_t<Pred>>{}; }\n\
                  };\n\
                  inline constexpr _Filter_fn filter;\n\
                  }\n\
                  }\n\
                  namespace views = ranges::views;\n\
                  }\n";
    let source = "#include \"a.hpp\"\n\
                  void use() {\n\
                  \x20   auto c = std::views::filter(1);\n\
                  }\n";
    let files = MemoryFiles::new()
        .with_file("/p/a.hpp", header)
        .with_file("/p/a.cpp", source);
    let fixture = Memory::new("the-real-adaptor-shape", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", source);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    let at =
        source.find("std::views::filter(1)").expect("the fixture") + "std::views::filter".len();
    let typed = session.type_at(&view, crate::file::coordinates::ReadingOffset::new(at));
    let Known::Yes(typed) = typed else {
        panic!("`std::views::filter(1)` must have a type: {typed:?}");
    };
    // **The call answers**, which is the property this test exists for: the one-argument overload is selected by
    // arity, its placeholder return is deduced from the body, and the class template it names is the answer. That
    // the alias template *inside* the spelling is left unresolved (decay_t<int> rather than int) is the next
    // stage of the work, recorded in docs/ranges.md; asserting the prefix keeps this test about the chain.
    assert!(
        typed.type_of.starts_with("range_closure<_Filter_fn,"),
        "the one-argument overload's closure is the answer: {:?}",
        typed.type_of
    );
}
