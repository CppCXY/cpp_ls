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

/// `cfoo.call(...)` on a `const Foo` showed the **non-const** `call`, and `Bar`'s `requires`-clause never appeared.
///
/// A class may declare a name twice and tell the two apart by `const` (and by a trailing `requires`). The object's
/// constness is not in the type model yet — `type_of` is `Foo` for both `Foo foo;` and `const Foo cfoo;` — so the card
/// lists **every** declaration of the name, with the qualifiers the facts now record; the reader's own line says which
/// object they are calling it on.
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

    for (needle, must_say) in [
        ("cfoo.call", "void call(Fn&& fn) const"),
        ("cbar.call", "const requires requires"),
    ] {
        let line = source.find(needle).expect("the use");
        let at = line + needle.find("call").expect("the member name");
        let hover = cpp_ls::handlers::hover::hover(
            &session,
            &view,
            ReadingOffset::new(at + 1),
            FileOffset::new(at + 1),
        )
        .expect("a member has a card");
        let text = match hover.contents {
            lsp_types::HoverContents::Markup(markup) => markup.value,
            other => format!("{other:?}"),
        };
        assert!(
            text.contains(must_say),
            "the card must say {must_say:?}: {text}"
        );
        assert!(
            text.contains("const Foo")
                || text.contains("const Bar")
                || text.contains("`const` object"),
            "and it must say which one a const object may call: {text}"
        );
    }

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
