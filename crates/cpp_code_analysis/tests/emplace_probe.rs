//! **`auto &r = v.emplace_back();` — the shape the editor reports at `LuaCodeFormat.cpp:304`.**
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test emplace_probe -- --ignored --nocapture
//! ```
//!
//! The real file writes it against MSVC's `<vector>`, which is a class template whose `emplace_back` returns an
//! alias (`using reference = _Ty &;`) of a class-level template parameter. This probe reproduces the shape in a
//! fixture, one piece at a time, because the answer at line 304 — `std::vector<LuaDiagnostic>&`, the *parameter's*
//! type — is not a type that line mentions anywhere.

use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
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

/// The **last whole-word** occurrence of a spelling, so `result` is not found inside `results`.
fn last_word(source: &str, spelling: &str) -> usize {
    let is_word = |character: char| character.is_alphanumeric() || character == '_';
    let mut found = None;
    let mut from = 0usize;
    while let Some(at) = source[from..].find(spelling) {
        let at = from + at;
        let end = at + spelling.len();
        let before_is_word = source[..at].chars().next_back().is_some_and(is_word);
        let after_is_word = source[end..].chars().next().is_some_and(is_word);
        if !before_is_word && !after_is_word {
            found = Some(at);
        }
        from = end;
    }
    found.unwrap_or_else(|| panic!("{spelling:?} is not a word in the fixture"))
}

fn what(source: &str, spelling: &str) -> String {
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let offset = last_word(source, spelling);
    let reading = view
        .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(
            offset,
        ))
        .expect("the cursor is in the rendering");
    match session.type_at(&view, reading) {
        Known::Yes(found) => found.type_of,
        Known::Unknown(reason) => format!("unknown — {reason:?}"),
        Known::No => "no".to_string(),
    }
}

/// The fixture, parameterised by what the class declares, so each step can be measured alone.
fn program(class_body: &str, parameter: &str) -> String {
    format!(
        "namespace std {{\ntemplate <class _Ty> struct vector {{\n{class_body}\n}};\n}}\n\
         struct Item {{ int size; }};\nstruct Other {{ int size; }};\n\
         void f({parameter}) {{\n    std::vector<Item> results;\n    auto &result = results.emplace_back();\n}}\n"
    )
}

#[test]
#[ignore = "a probe: prints instead of asserting"]
fn a_bare_vector_class_where_the_qualified_name_is_missing() {
    // **The real project's shape, and the one the fixtures above do not have**: MSVC's `<vector>` declares `vector`
    // at *file* scope in this index (its `_STD_BEGIN` was not applied), so `definitions("vector")` answers 14
    // declarations while `definitions("std::vector")` answers nothing. Here one header declares a class `vector` at
    // file scope, and the consumer writes `std::vector<…>`.
    let header = "namespace std {\ntemplate <class _Ty> struct vector_base {\n    using reference = _Ty &;\n};\n}\n\
                  template <class _Ty> struct vector : std::vector_base<_Ty> {\n    using reference = _Ty &;\n    template <class... _Valty> _CONTAINER_EMPLACE_RETURN emplace_back(_Valty &&...);\n};\n";
    let consumer = "#include \"v.h\"\nstruct Item { int size; };\nstruct Other { int size; };\n\
                    void f(const std::vector<Other> &others) {\n    std::vector<Item> results;\n    auto &result = results.emplace_back();\n}\n";

    let session = session_with(&[("/p/v.h", header), ("/p/a.cpp", consumer)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let offset = last_word(consumer, "result");

    match session
        .index()
        .definitions("vector", std::path::Path::new("/p/a.cpp"))
    {
        Known::Yes(found) => eprintln!(
            "[emplace] bare `vector`: {} declaration(s)",
            found.found.len()
        ),
        other => eprintln!("[emplace] bare `vector`: {other:?}"),
    }
    match session
        .index()
        .definitions("std::vector", std::path::Path::new("/p/a.cpp"))
    {
        Known::Yes(found) => eprintln!(
            "[emplace] `std::vector`: {} declaration(s)",
            found.found.len()
        ),
        other => eprintln!("[emplace] `std::vector`: {other:?}"),
    }
    match session.type_at(
        &view,
        view.reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(
            offset,
        ))
        .expect("the cursor is in the rendering"),
    ) {
        Known::Yes(found) => eprintln!("[emplace] `result` -> `{}`", found.type_of),
        Known::Unknown(reason) => eprintln!("[emplace] `result` -> unknown — {reason:?}"),
        Known::No => eprintln!("[emplace] `result` -> no"),
    }
}

/// **The one difference between line 304 and every fixture above: it is inside a `for (auto &x : range)` body.**
///
/// The type trace for the real file is one line long and it says the initializer question was answered about
/// `diagnostics` — the *range* of the enclosing range-for — rather than about `results`. So this is that shape,
/// alone: the same declaration inside a range-for body, and the same declaration beside one.
#[test]
#[ignore = "a probe: prints instead of asserting"]
fn an_auto_declaration_inside_a_range_for() {
    let source = "namespace std {\ntemplate <class _Ty> struct vector {\n    using value_type = _Ty;\n    using reference = _Ty &;\n    template <class... _Valty> reference emplace_back(_Valty &&...);\n    _Ty *begin() const;\n    _Ty *end() const;\n};\n}\n\
                  struct Item { int size; };\nstruct Other { int size; };\n\
                  void f(const std::vector<Other> &diagnostics) {\n    std::vector<Item> results;\n    for (auto &diagnostic : diagnostics) {\n        auto &result = results.emplace_back();\n    }\n}\n";
    eprintln!(
        "[emplace] inside a range-for, the loop variable -> {}",
        what(source, "diagnostic")
    );
    eprintln!(
        "[emplace] inside a range-for, `result`        -> {}",
        what(source, "result")
    );

    // The same declaration with no range-for around it, for contrast.
    let beside = "namespace std {\ntemplate <class _Ty> struct vector {\n    using reference = _Ty &;\n    template <class... _Valty> reference emplace_back(_Valty &&...);\n};\n}\n\
                  struct Item { int size; };\nstruct Other { int size; };\n\
                  void f(const std::vector<Other> &diagnostics) {\n    std::vector<Item> results;\n    auto &result = results.emplace_back();\n}\n";
    eprintln!(
        "[emplace] beside a range-for, `result`        -> {}",
        what(beside, "result")
    );
}

#[test]
#[ignore = "a probe: prints instead of asserting"]
fn what_auto_deduces_from_emplace_back() {
    let cases: [(&str, &str); 5] = [
        (
            "an alias of the class parameter",
            "    using reference = _Ty &;\n    template <class... _Valty> reference emplace_back(_Valty &&...);",
        ),
        (
            "the parameter written out",
            "    template <class... _Valty> _Ty &emplace_back(_Valty &&...);",
        ),
        (
            "an alias of an alias",
            "    using value_type = _Ty;\n    using reference = value_type &;\n    template <class... _Valty> reference emplace_back(_Valty &&...);",
        ),
        (
            "a non-template member",
            "    using reference = _Ty &;\n    reference emplace_back();",
        ),
        ("no templates at all", "    Item &emplace_back();"),
    ];

    for parameter in ["const std::vector<Other> &others", "", "int other"] {
        eprintln!("[emplace] ---- parameter: {parameter:?}");
        for (label, body) in cases {
            let source = program(body, parameter);
            eprintln!("[emplace] {label:<32} -> {}", what(&source, "result"));
        }
    }
}

/// **The two shapes the real header has and the fixtures above did not.**
///
/// MSVC's `<vector>` returns `_CONTAINER_EMPLACE_RETURN` — a macro defined in `<xmemory>`, so a reading of the header
/// alone sees a *name* where the return type should be — and its `reference` is declared in the base class
/// `_Vector_val`, not in `vector`. Either could be what makes the deduction take the wrong type.
#[test]
#[ignore = "a probe: prints instead of asserting"]
fn the_shapes_the_real_header_has() {
    let fixtures: [(&str, &str); 6] = [
        (
            "return type is an undefined name",
            "namespace std {\ntemplate <class _Ty> struct vector {\n    template <class... _Valty> _CONTAINER_EMPLACE_RETURN emplace_back(_Valty &&...);\n};\n}\nstruct Item { int size; };\nstruct Other { int size; };\nvoid f(const std::vector<Other> &others) {\n    std::vector<Item> results;\n    auto &result = results.emplace_back();\n}\n",
        ),
        (
            "reference lives in a base",
            "namespace std {\ntemplate <class _Ty> struct _Vector_val {\n    using value_type = _Ty;\n    using reference = value_type &;\n};\ntemplate <class _Ty> struct vector : _Vector_val<_Ty> {\n    template <class... _Valty> reference emplace_back(_Valty &&...);\n};\n}\nstruct Item { int size; };\nstruct Other { int size; };\nvoid f(const std::vector<Other> &others) {\n    std::vector<Item> results;\n    auto &result = results.emplace_back();\n}\n",
        ),
        (
            "both, as the real header writes them",
            "namespace std {\ntemplate <class _Ty> struct _Vector_val {\n    using value_type = _Ty;\n    using reference = value_type &;\n};\ntemplate <class _Ty, class _Alloc = void> struct vector : _Vector_val<_Ty> {\n    template <class... _Valty> _CONTAINER_EMPLACE_RETURN emplace_back(_Valty &&...);\n};\n}\nstruct Item { int size; };\nstruct Other { int size; };\nvoid f(const std::vector<Other> &others) {\n    std::vector<Item> results;\n    auto &result = results.emplace_back();\n}\n",
        ),
        (
            "both, and the class is declared out of view",
            "namespace std {\ntemplate <class _Ty, class _Alloc = void> struct vector;\n}\nstruct Item { int size; };\nstruct Other { int size; };\nvoid f(const std::vector<Other> &others) {\n    std::vector<Item> results;\n    auto &result = results.emplace_back();\n}\n",
        ),
        (
            "the class is unknown, and the type is written",
            "struct Item { int size; };\nstruct Other { int size; };\nvoid f(const std::vector<Other> &others) {\n    std::vector<Item> results;\n    auto &result = results.emplace_back();\n}\n",
        ),
        (
            "a member of an unknown class",
            "struct Item { int size; };\nstruct Other { int size; };\nvoid f(const std::vector<Other> &others) {\n    std::vector<Item> results;\n    auto &result = results.begin();\n}\n",
        ),
    ];

    for (label, source) in fixtures {
        eprintln!("[emplace] {label:<40} -> {}", what(source, "result"));
    }
}
