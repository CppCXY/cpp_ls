//! **An alias template's members are the members of the class it names, with its own arguments substituted.**
//!
//! ```cpp
//! template <typename T> struct Store { void put(T value); };
//! template <template <typename> class C, typename T> using MadeOf = C<T>;
//! ```
//!
//! `MadeOf<Store, int>` is `Store<int>`, so its members are `Store`'s. Reported from an editor on `test_cpp`'s
//! `templates.cpp`, where the same shape is written with `std::vector`:
//!
//! ```cpp
//! template <template <typename, typename> class Container, typename T>
//! using ContainerOf = Container<T, std::allocator<T>>;
//! …
//! ContainerOf<std::vector, int>{}.size()
//! ```
//!
//! and the hover on `size` answered *"The type of `ContainerOf<std::vector, int>{}` is not known here"*.
//!
//! # Two bugs had to be fixed for this, and both are why this test exists
//!
//! `members_of_inner` handed the alias walk `base_type_name(class)` — the spelling **without its arguments** — so a
//! template alias had nothing to substitute: `Container<T, std::allocator<T>>` stayed as written, its `class_name()`
//! was `Container`, and the answer was *"0 member(s), 1 unlisted base `Container`"*. The walk itself also stripped an
//! alias's own argument list when it contained a `::` (it read `<…>` as a *qualifier's* list), which is the other
//! half — see `member_facts::tests::the_close_of_an_argument_list_is_its_matching_angle`.

use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

const SOURCE: &str = "\
template <typename T>
struct Store {
    T held{};

    void put(T value) { held = value; }
    T get() const { return held; }
};

template <template <typename> class C, typename T>
using MadeOf = C<T>;

template <typename T>
using Twice = Store<T>;

void use() {
    MadeOf<Store, int> made;
    made.put(1);
    Twice<int> twice;
    twice.put(2);
}
";

#[test]
fn an_alias_templates_members_are_the_members_of_the_class_it_names() {
    let memory = MemoryFiles::new().with_file("/p/a.cpp", SOURCE);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let view = session
        .view_of_the_file("/p/a.cpp")
        .expect("the file is readable");

    for alias in ["MadeOf<Store, int>", "Twice<int>", "Store<int>"] {
        let Known::Yes(list) = session.members_of(&view, alias) else {
            panic!("`{alias}` is a class once its alias is followed");
        };
        let names: Vec<String> = list
            .members
            .iter()
            .map(|member| member.fact.name.to_string())
            .collect();
        assert!(
            names.iter().any(|name| name == "put") && names.iter().any(|name| name == "get"),
            "`{alias}` names a `Store`, so it has `put` and `get`: {names:?}"
        );
        assert!(
            list.unlisted.is_empty(),
            "and nothing is missing from the answer: {:?}",
            list.unlisted
        );
    }
}

/// **A construction of an alias declared in an anonymous namespace is still a construction of a class.**
///
/// The reported shape was `ContainerOf<std::vector, int>{}.size()` on `test_cpp`'s `templates.cpp`, where the alias is
/// at the top of the file inside `namespace { … }` and the use is outside it. That detail decided the answer: the
/// file's scopes do not reach into the anonymous namespace from the use, so the *index* answered the head's
/// declaration, whose `type_of` is the alias's target `Container<T, std::allocator<T>>` — `Some`, so the
/// construction arm's old guard (`type_of().is_none()`, meant to tell a class from an alias) sent it to the
/// spelling fallback, and that fallback refuses a spelling with a space in it. Two template arguments are written
/// `A, B`, so the construction answered "the type … is not known here". The same alias at file scope worked,
/// because the binding came from the tree and its `type_of` was `None`: the same declaration answered differently
/// depending on where in the file it was written.
#[test]
fn a_construction_of_an_alias_in_an_anonymous_namespace_is_a_class() {
    const SOURCE: &str = "\
template <typename First, typename Second>
struct Duo {
    void put(First first, Second second) { (void)first; (void)second; }
};

template <template <typename, typename> class Container, typename T>
using MadeOutside = Container<T, T>;

namespace {
template <template <typename, typename> class Container, typename T>
using MadeInside = Container<T, T>;
}  // namespace

void use() {
    MadeOutside<Duo, int>{}.put(1, 2);
    MadeInside<Duo, int>{}.put(3, 4);
}
";
    let memory = MemoryFiles::new().with_file("/p/a.cpp", SOURCE);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let view = session
        .view_of_the_file("/p/a.cpp")
        .expect("the file is readable");

    for alias in ["MadeOutside<Duo, int>", "MadeInside<Duo, int>"] {
        let wanted = format!("{alias}{{}}");
        let construction = view
            .root
            .descendants()
            .find(|node| node.text().to_string() == wanted)
            .unwrap_or_else(|| panic!("`{wanted}` is in the tree"));
        let Known::Yes(found) = session.type_of_the_expression(&view, &construction) else {
            panic!("`{wanted}` is a temporary of the class the alias names");
        };
        let Known::Yes(list) = session.members_of(&view, &found.type_of) else {
            panic!("`{}` is a class, so it has members", found.type_of);
        };
        let names: Vec<String> = list
            .members
            .iter()
            .map(|member| member.fact.name.to_string())
            .collect();
        assert!(
            names.iter().any(|name| name == "put"),
            "`{wanted}` is a `Duo<int, int>`, whose members are `put`: {names:?}"
        );
    }
}
