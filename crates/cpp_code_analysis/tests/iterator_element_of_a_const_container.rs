//! **`auto it = container.find(key);` on a `const` container** — the reported shape from an editor.
//!
//! ```cpp
//! std::string GetOption(const std::map<std::string, std::string, std::less<>> &configMap, const std::string &key) {
//!     auto it = configMap.find(key);
//!     if (it != configMap.end()) {
//!         return it->second;      // ← "the type of `it` is not known here"
//!     }
//!     return "";
//! }
//! ```
//!
//! Measured on `CodeFormatCore/src/Config/LuaStyle.cpp:20` of `EmmyLuaCodeStyle`: `it` was typed
//! `_Tree_const_iterator<_Scary_val>` — the standard library's member alias read through one level of substitution,
//! with a template parameter left standing — and `it->second` answered *"has no member `second`"*.
//!
//! # Why the element and not the iterator
//!
//! [`cpp_code_analysis`]'s call layer answers an iterator-producing member (`begin`, `find`, `lower_bound`, …) with
//! the **element** the iterator ranges over — its own note explains why: iterator types are class templates this
//! layer cannot instantiate, while the three things a reader writes next (`it->first`, `it->second`, `*it`) are all
//! questions about the element.
//!
//! That path was answering nothing for this shape, and the reason was one `const&`: `range_element_of` matched
//! `Type::Named` alone, while a by-reference parameter's type is `Qualified`/`Reference` around it. So the element
//! answer was skipped, the call fell back to the member's declared return type, and the half-substituted iterator
//! type is what a reader saw.
//!
//! The fixture keeps the names the standard library uses because the element rule keys on the container's *name*
//! (`map`, `vector`, …) — the rule is the standard's, and a type this analysis cannot see into must be recognised by
//! the name the standard gave it.

use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

const SOURCE: &str = "\
namespace std {
struct string {
    friend bool operator!=(const string &, const string &);
};

template <class A, class B>
struct pair {
    A first;
    B second;
};

template <class Traits>
struct _Tree_val {
    using value_type = typename Traits::value_type;
};

template <class Traits>
struct _Tree_const_iterator {
    using value_type = typename _Tree_val<Traits>::value_type;

    const value_type* operator->() const;
};

template <class Traits>
struct _Tree {
    using _Scary_val = _Tree_val<Traits>;
    using const_iterator = _Tree_const_iterator<_Scary_val>;

    const_iterator find(int key) const;
    const_iterator end() const;
};

template <class Key, class Value>
struct _Tmap_traits {
    using value_type = pair<const Key, Value>;
};

template <class Key, class Value, class Compare>
struct map : _Tree<_Tmap_traits<Key, Value>> {
    using _Mybase = _Tree<_Tmap_traits<Key, Value>>;
    using const_iterator = typename _Mybase::const_iterator;
};
}  // namespace std

std::string GetOption(const std::map<std::string, std::string, int> &configMap) {
    auto it = configMap.find(0);
    if (it != configMap.end()) {
        return it->second;
    }
    return 0;
}
";

/// **The innermost node written exactly like this** — a `CallExpr` rather than the `Initializer` that holds it, an
/// `MemberExpr` rather than the statement around it. `descendants()` is pre-order, so the *first* match is the
/// outermost node with that text, which is the wrong one to ask a type question about; an `Initializer` and its
/// expression have the same text and the same span, so the depth is what tells them apart.
fn the_node(view: &cpp_code_analysis::FileView, text: &str) -> cpp_parser::CppSyntaxNode {
    view.root
        .descendants()
        .filter(|node| node.text() == text)
        .max_by_key(|node| node.ancestors().count())
        .unwrap_or_else(|| panic!("`{text}` is in the tree"))
}

#[test]
fn a_find_on_a_const_container_is_the_element_it_ranges_over() {
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

    // **The call is the element**, not the iterator the standard library's `find` declares: `map`'s element is
    // `pair<const Key, Value>`, and `range_element_of` is where that rule lives.
    let Known::Yes(the_type) =
        session.type_of_the_expression(&view, &the_node(&view, "configMap.find(0)"))
    else {
        panic!("`configMap.find(0)` answers the element `find` ranges over");
    };
    assert_eq!(
        the_type.type_of, "std::pair<const std::string, std::string>",
        "a `const std::map<…>&`'s element is `pair<const Key, Value>`, and a reference is transparent to that question"
    );

    // **And the use the report was about.** `it->second` is a `std::string`; before the fix the whole chain stopped
    // at `_Tree_const_iterator<_Scary_val>` and this was *"has no member `second`"*.
    let Known::Yes(second) = session.type_of_the_expression(&view, &the_node(&view, "it->second"))
    else {
        panic!("`it->second` is the element's second member");
    };
    assert_eq!(
        second.type_of, "std::string",
        "`it->second` on a `map<string, string>` is a `std::string`"
    );
}
