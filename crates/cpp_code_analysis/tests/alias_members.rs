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
