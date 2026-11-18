//! **The card a reported hover needed** — a member of a class-template instantiation.

use std::path::PathBuf;

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

/// `Pair<int, int>{1, 2}.sum()` answered "`int` has no member `sum`".
///
/// Two defects met here, and both are fixed: the initialisation's type was the whole spelling as a *name*
/// (`Named { name: "Pair<int, int>" }`, no arguments) rather than the instantiation, so no substitution reached `sum`'s
/// written `T`; and the card read the object's type at its **last byte**, which for a braced initialisation is inside the
/// list — `int`, the first element's type.
///
/// The analysis-layer half is pinned by `cpp_code_analysis`'s
/// `a_braced_initialisation_of_a_class_template_keeps_its_arguments`; this is the card.
#[test]
fn a_member_of_a_class_template_instantiation_is_named() {
    let root: PathBuf = std::env::temp_dir().join("cppls-hover-template");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let file = root.join("a.cpp");
    let source = "template <typename T, typename U = int>\n\
                  struct Pair {\n\
                      T first;\n\
                      U second;\n\
                      T sum() const { return first + second; }\n\
                  };\n\
                  void f() {\n\
                      int a = Pair<int, int>{1, 2}.sum();\n\
                  }\n";
    std::fs::write(&file, source).expect("the fixture writes");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, source);
    session.catch_up(&file);
    let _ = session.cook(&file);
    let _ = session.index_everything();

    let view = session.view_of_the_file(&file).expect("the written text");
    let at =
        source.find("Pair<int, int>{1, 2}.sum").expect("the use") + "Pair<int, int>{1, 2}.".len();
    let hover = cpp_ls::handlers::hover::hover(
        &session,
        &view,
        ReadingOffset::new(at),
        FileOffset::new(at),
    )
    .expect("a member has a card");
    let text = match hover.contents {
        lsp_types::HoverContents::Markup(markup) => markup.value,
        other => format!("{other:?}"),
    };
    assert!(
        !text.contains("has no member"),
        "the member is resolved through the instantiation: {text}"
    );
    assert!(text.contains("sum"), "the card names the member: {text}");

    let _ = std::fs::remove_dir_all(&root);
}

/// **A member's card speaks in the instance's terms** — `Box<int>::query` returns `std::optional<int>`.
///
/// Reported with a screenshot: hovering `box.query(1)` on a `Box<int>` printed `std::optional<V> query(int key)` — the
/// class's own spelling. The member list the card was built from had already substituted the arguments
/// (`members_of("Box<int>")` answers `returns: Some("std::optional<int>")`, measured); what lost them was the card's
/// own re-ask of the index, whose condition was `nothing to render || no annotation` — and every ordinary declaration
/// has no annotation, so the substituted fact was replaced by the index's unsubstituted copy.
#[test]
fn a_member_of_an_instance_answers_in_the_instance_terms() {
    let root: PathBuf = std::env::temp_dir().join("cppls-hover-instance");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let file = root.join("a.cpp");
    let source = "template <class V>\n\
                  struct Box {\n\
                      std::optional<V> query(int key);\n\
                  };\n\
                  struct C {\n\
                      Box<int> box;\n\
                      void f() {\n\
                          auto x = box.query(1);\n\
                      }\n\
                  };\n";
    std::fs::write(&file, source).expect("the fixture writes");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, source);
    session.catch_up(&file);
    let _ = session.cook(&file);
    let _ = session.index_everything();

    let view = session.view_of_the_file(&file).expect("the written text");
    let at = source.find("box.query").expect("the use") + "box.".len();
    let hover = cpp_ls::handlers::hover::hover(
        &session,
        &view,
        ReadingOffset::new(at),
        FileOffset::new(at),
    )
    .expect("a member has a card");
    let text = match hover.contents {
        lsp_types::HoverContents::Markup(markup) => markup.value,
        other => format!("{other:?}"),
    };
    assert!(
        text.contains("std::optional<int>"),
        "the arguments are substituted into the member's return type: {text}"
    );
    assert!(
        !text.contains("std::optional<V>"),
        "and the class's own parameter is not what the reader sees: {text}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// `cfoo.call(...)` on a `const Foo` showed the **non-const** `call`, and `Bar`'s `requires`-clause never appeared.
///
/// **A `const` object's card is the overload it can call; a mutable one's lists both.**
///
/// A class may declare a name twice and tell the two apart by `const` (and by a trailing `requires`). This test used to
/// assert that the card **always** listed both declarations with the sentence "a `const` object can only call the
/// `const` one" — the reader had to apply the rule themselves. The card now applies it: [`Session::the_object_is_const`]
/// reads the object's own specifier sequence, so `cfoo.call(…)` shows only `void call(Fn&& fn) const`, and a mutable
/// `foo.call(…)` still shows both, because there both are callable.
#[test]
fn a_member_declared_twice_shows_both_declarations_and_their_clauses() {
    let root: PathBuf = std::env::temp_dir().join("cppls-hover-qualifiers");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let file = root.join("a.cpp");
    let source = "struct Foo {\n\
                  template <typename Fn> void call(Fn &&fn) {}\n\
                  template <typename Fn> void call(Fn &&fn) const {}\n\
                  };\n\
                  struct Bar {\n\
                  template <typename Fn> void call(Fn &&fn) requires requires { fn(*this); } {}\n\
                  template <typename Fn> void call(Fn &&fn) const requires requires { fn(*this); } {}\n\
                  };\n\
                  void f() {\n\
                  const Foo cfoo;\n\
                  cfoo.call([](auto &&x) {});\n\
                  const Bar cbar;\n\
                  cbar.call([](auto &&x) {});\n\
                  Foo foo;\n\
                  foo.call([](auto &&x) {});\n\
                  }\n";
    std::fs::write(&file, source).expect("the fixture writes");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, source);
    session.catch_up(&file);
    let _ = session.cook(&file);
    let _ = session.index_everything();
    let view = session.view_of_the_file(&file).expect("the written text");

    // **The *last* occurrence**, because `foo.call` is a substring of `cfoo.call` — the first match was the const
    // object's use, and the mutable one's card was never asked for (measured: the assertion below failed with the
    // const object's card).
    let card_at = |needle: &str| -> String {
        let line = source.rfind(needle).expect("the use");
        let at = line + needle.find("call").expect("the member name");
        let hover = cpp_ls::handlers::hover::hover(
            &session,
            &view,
            ReadingOffset::new(at),
            FileOffset::new(at),
        )
        .expect("a member has a card");
        match hover.contents {
            lsp_types::HoverContents::Markup(markup) => markup.value,
            other => format!("{other:?}"),
        }
    };

    // **A `const` object may only call the `const` overload**, so that one is the card — and the other is not listed.
    for (needle, the_one, the_other) in [
        (
            "cfoo.call",
            "void call(Fn&& fn) const",
            "void call(Fn&& fn)\n",
        ),
        (
            "cbar.call",
            "const requires requires",
            "void call(Fn&& fn) requires requires",
        ),
    ] {
        let text = card_at(needle);
        assert!(text.contains(the_one), "the card names {the_one:?}: {text}");
        assert!(
            !text.contains(the_other),
            "and not the overload a `const` object cannot call ({the_other:?}): {text}"
        );
    }

    // **A mutable object can call either**, so nothing decides between them and both are shown with the rule.
    let text = card_at("foo.call");
    assert!(
        text.contains("void call(Fn&& fn)\n") && text.contains("void call(Fn&& fn) const"),
        "both declarations are listed for a mutable object: {text}"
    );
    assert!(
        text.contains("`const` object can only call"),
        "and the card says which one a `const` object would mean: {text}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **A local whose type is deduced shows the type it was deduced to** — reported with a screenshot: hovering
/// `auto &syntaxTree = opSyntaxTree.value();` showed "`syntaxTree` — a variable, declared in a function body" and
/// **no type at all**, while the inlay hint drawn beside the same name said `LuaSyntaxTree&`.
///
/// The fact records what the file *wrote* (`auto&`), which is the spelling a lookup wants and no answer at all to
/// "what is this". The engine has the answer — `type_at`, the same question the inlay asks — and the card now shows it.
#[test]
fn a_deduced_local_shows_its_type() {
    let root: PathBuf = std::env::temp_dir().join("cppls-hover-deduced");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let file = root.join("a.cpp");
    let source = "struct LuaSyntaxTree {\n\
                      bool HasError() const { return false; }\n\
                  };\n\
                  struct Holder {\n\
                      LuaSyntaxTree& value();\n\
                  };\n\
                  void f() {\n\
                      Holder op;\n\
                      auto &tree = op.value();\n\
                      bool bad = tree.HasError();\n\
                  }\n";
    std::fs::write(&file, source).expect("the fixture writes");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, source);
    session.catch_up(&file);
    let _ = session.cook(&file);
    let _ = session.index_everything();

    let view = session.view_of_the_file(&file).expect("the written text");
    let at = source.find("tree.HasError").expect("the use");
    let hover = cpp_ls::handlers::hover::hover(
        &session,
        &view,
        ReadingOffset::new(at),
        FileOffset::new(at),
    )
    .expect("a local has a card");
    let text = match hover.contents {
        lsp_types::HoverContents::Markup(markup) => markup.value,
        other => format!("{other:?}"),
    };
    assert!(
        text.contains("LuaSyntaxTree&"),
        "the card shows the deduced type: {text}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **A smart pointer's arrow reaches its pointee's member in the card too** — "shared_ptr 没有 -> 补全和 hover 识别".
///
/// Same rule as the completion (`element_type` is the pointer protocol; libstdc++ keeps `operator->` two bases down, in
/// `__shared_ptr_access`), asked through the card: before the fix this said "`std::shared_ptr<State>` has no member
/// `called`". The base here is declared and nothing more, which is the state the real project was in.
#[test]
fn a_member_through_a_smart_pointers_arrow_is_named() {
    let root: PathBuf = std::env::temp_dir().join("cppls-hover-arrow");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let file = root.join("a.cpp");
    let source = "namespace std {\n\
                      template <typename T, int Lp>\n\
                      class __shared_ptr;\n\
                      template <typename T>\n\
                      class shared_ptr : public __shared_ptr<T, 2> {\n\
                      public:\n\
                          using element_type = T;\n\
                          shared_ptr() = default;\n\
                      };\n\
                  }\n\
                  struct State {\n\
                      bool called;\n\
                  };\n\
                  std::shared_ptr<State> state_;\n\
                  void f() {\n\
                      state_->called = true;\n\
                  }\n";
    std::fs::write(&file, source).expect("the fixture writes");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, source);
    session.catch_up(&file);
    let _ = session.cook(&file);
    let _ = session.index_everything();

    let view = session.view_of_the_file(&file).expect("the written text");
    let at = source.find("called = true").expect("the member");
    let hover = cpp_ls::handlers::hover::hover(
        &session,
        &view,
        ReadingOffset::new(at),
        FileOffset::new(at),
    )
    .expect("a member has a card");
    let text = match hover.contents {
        lsp_types::HoverContents::Markup(markup) => markup.value,
        other => format!("{other:?}"),
    };
    assert!(
        !text.contains("has no member"),
        "the member is reached through the handle: {text}"
    );
    assert!(
        text.contains("State") && text.contains("called"),
        "the card says the member belongs to the pointee: {text}"
    );
    assert!(text.contains("called"), "the card names the member: {text}");

    let _ = std::fs::remove_dir_all(&root);
}

/// **A local variable is never answered by a same-named declaration from the index.**
///
/// Reported from `EmmyLuaCodeStyle`: hovering the variable in
/// `auto result = std::make_shared<lsp::DocumentFormattingResult>();` showed
/// `` `codecvt_base::result` — a type of type `int`, in `codecvt_base`, declared in `xlocale:581:11` `` — a real
/// `using result = int;` from the standard library — and hovering the variable `object` in `LSP.cpp:12` showed a
/// static member function `object`.
///
/// The cause was in the **card**, not in the lookup: `declaration_markdown` re-asks the index for a declaration whose
/// fact has no type, no return type and no parameter list — which is every fact built from a **binding** — and a
/// local's `qualified_name()` is its bare spelling, because a function body contributes no scope segment. So the bare
/// name found another entity entirely. `definitions`, `definition` and `type_at` had the local right all along.
/// See `docs/buglist.md` section 7.
#[test]
fn a_local_is_not_answered_by_a_same_named_declaration_from_the_index() {
    let root: PathBuf = std::env::temp_dir().join("cppls-hover-local");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let header = root.join("standard.h");
    std::fs::write(
        &header,
        "namespace std {\nstruct codecvt_base {\n    using result = int;\n};\n}\n",
    )
    .expect("the header writes");
    let file = root.join("a.cpp");
    let source = "#include \"standard.h\"\nvoid f() {\n    auto result = 1;\n}\n";
    std::fs::write(&file, source).expect("the fixture writes");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone(), header.clone()]);
    session.did_open(&file, source);
    session.catch_up(&file);
    let _ = session.cook(&file);
    let _ = session.index_everything();

    let view = session.view_of_the_file(&file).expect("the written text");
    let at = source.find("result").expect("the local") + 1;
    let text = cpp_ls::handlers::hover::hover_text(
        &session,
        &view,
        ReadingOffset::new(at),
        FileOffset::new(at),
    )
    .expect("the local has a card");

    assert!(
        !text.contains("codecvt_base"),
        "the card is about the local, not about the standard library's `result`: {text}"
    );
    assert!(
        text.contains("a variable"),
        "and it says what the local is: {text}"
    );
    assert!(text.contains("int"), "and what it deduced to: {text}");

    let _ = std::fs::remove_dir_all(&root);
}

/// **A documentation link's target has a card** — Rust's ``[`Widget::grow`]``, hovered.
///
/// The link is read from the file's own text (a comment is trivia, so the rendering has none of it) and the target
/// is resolved with the same query a jump uses, so the card and the jump cannot disagree.
#[test]
fn a_documentation_links_target_has_a_card() {
    let root: PathBuf = std::env::temp_dir().join("cppls-hover-doc-link");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let file = root.join("a.cpp");
    let source = "\
struct Widget {
    /// See [`Widget::grow`] for the rule.
    void grow(int by);
};
";
    std::fs::write(&file, source).expect("the fixture writes");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, source);
    session.catch_up(&file);
    let _ = session.cook(&file);
    let _ = session.index_everything();

    let view = session.view_of_the_file(&file).expect("the written text");
    let at = source.find("See [`").expect("the fixture") + "See [`Widget::".len();
    let hover = cpp_ls::handlers::hover::hover(
        &session,
        &view,
        ReadingOffset::new(at),
        FileOffset::new(at),
    )
    .expect("a link has a card");
    let text = match hover.contents {
        lsp_types::HoverContents::Markup(markup) => markup.value,
        other => format!("{other:?}"),
    };
    assert!(
        text.contains("void grow(int by)"),
        "the card is the declaration the link names: {text}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **A lambda's closure type has no name, and the card says so instead of printing the body.**
///
/// Reported with a screenshot of `auto show_division = [](double a, double b) { … };`: the card read
/// "the type of `[](double a, double b) { … }` is not known here" over five lines — the initializer's whole text inside
/// a code span. A lambda's type is unique and cannot be written, so it has its own reason
/// (`UnknownReason::UnnamedLambda`), which carries no text: there is nothing to shorten.
#[test]
fn a_lambda_initializer_is_explained_rather_than_printed() {
    let root: PathBuf = std::env::temp_dir().join("cppls-hover-lambda");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let file = root.join("a.cpp");
    let source = "void f() {\n\
                      auto show_division = [](double a, double b) {\n\
                          int lambda_body_marker = 1;\n\
                          (void)lambda_body_marker;\n\
                      };\n\
                      show_division(10.0, 4.0);\n\
                  }\n";
    std::fs::write(&file, source).expect("the fixture writes");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, source);
    session.catch_up(&file);
    let _ = session.cook(&file);
    let _ = session.index_everything();

    let view = session.view_of_the_file(&file).expect("the written text");
    let at = source.find("show_division =").expect("the declaration") + "show_divisi".len();
    let hover = cpp_ls::handlers::hover::hover(
        &session,
        &view,
        ReadingOffset::new(at),
        FileOffset::new(at),
    )
    .expect("the declaration has a card");
    let text = match hover.contents {
        lsp_types::HoverContents::Markup(markup) => markup.value,
        other => format!("{other:?}"),
    };
    assert!(
        text.contains("lambda") && text.contains("closure type"),
        "the reason names the lambda and says why it has no type: {text}"
    );
    assert!(
        !text.contains("lambda_body_marker"),
        "the body is not the answer: {text}"
    );
    assert!(
        !text.contains("the type of `[]"),
        "and it is not printed as a type spelling: {text}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **An alias's construction is hovered as the class it names, wherever the alias was declared.**
///
/// Reported with a screenshot on `test_cpp`'s `templates.cpp`: `ContainerOf<std::vector, int>{}.size()` answered "The
/// type of `ContainerOf<std::vector, int>{}` is not known here, so `size` cannot be resolved", while the identical
/// alias at file scope answered the member. The alias there is inside `namespace { … }` and the use is outside it —
/// see `cpp_code_analysis`'s `a_construction_of_an_alias_in_an_anonymous_namespace_is_a_class` for the analysis half.
/// This is the card: a member of the class the alias names, not a sentence about an unknown type.
#[test]
fn a_construction_of_an_alias_in_a_namespace_names_the_member() {
    let root: PathBuf = std::env::temp_dir().join("cppls-hover-alias-namespace");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let file = root.join("a.cpp");
    let source = "template <typename First, typename Second>\n\
                  struct Duo {\n\
                  \x20   void put(First first, Second second) { (void)first; (void)second; }\n\
                  };\n\
                  \n\
                  namespace {\n\
                  template <template <typename, typename> class Container, typename T>\n\
                  using MadeInside = Container<T, T>;\n\
                  }  // namespace\n\
                  \n\
                  void use() {\n\
                  \x20   MadeInside<Duo, int>{}.put(1, 2);\n\
                  }\n";
    std::fs::write(&file, source).expect("the fixture writes");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, source);
    session.catch_up(&file);
    let _ = session.cook(&file);
    let _ = session.index_everything();

    let view = session.view_of_the_file(&file).expect("the written text");
    let at = source
        .find("MadeInside<Duo, int>{}.put")
        .expect("the use")
        + "MadeInside<Duo, int>{}.".len();
    let hover = cpp_ls::handlers::hover::hover(
        &session,
        &view,
        ReadingOffset::new(at),
        FileOffset::new(at),
    )
    .expect("a member has a card");
    let text = match hover.contents {
        lsp_types::HoverContents::Markup(markup) => markup.value,
        other => format!("{other:?}"),
    };
    assert!(
        !text.contains("is not known here"),
        "the construction's type is the class the alias names: {text}"
    );
    assert!(
        text.contains("put"),
        "and the card names the member that was hovered: {text}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

