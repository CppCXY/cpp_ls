//! What type a declaration has — including the ones that wrote `auto`.
//!
//! `type_of_expression` reads a name's type off its declaration, which works for every spelling that *is* a type
//! and fails for the one that is a placeholder. These tests are about the second case: the initializer's type,
//! what the declaration wrote around `auto`, and the shapes that are refused because the language's rule is not the
//! one a spelling suggests (`auto&&`) or because the declaration is not in this file's syntax.
//!
//! The type is always the **written** spelling — `const int&` is those three tokens — which is the same boundary
//! every other query in this crate has; what is new here is that the spelling is *computed* rather than copied.

use cpp_code_analysis::{CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

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
    session.add_project_files(files.iter().map(|(path, _)| std::path::PathBuf::from(*path)));
    session.index_everything();

    session
}

/// The type of the **use** of a name in a file, as the query layer answers it.
///
/// The use rather than the declaration, because that is the question every consumer asks: a cursor on `n` in
/// `n + 1`, a member access `w.` after `auto w = …`. The occurrence is found **as a word** — `rfind("n")` would
/// find the `n` of `return`, which is how a fixture ends up testing an offset nobody meant.
fn type_of_use(source: &str, spelling: &str) -> Option<String> {
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let offset = last_word(source, spelling);

    match session.type_at(&view, offset) {
        cpp_code_analysis::Known::Yes(found) => Some(found.type_of),
        _ => None,
    }
}

/// The offset of the last **whole-word** occurrence of a spelling.
fn last_word(source: &str, spelling: &str) -> usize {
    let is_word = |character: char| character.is_alphanumeric() || character == '_';
    let mut found = None;
    let mut from = 0usize;

    while let Some(at) = source[from..].find(spelling) {
        let start = from + at;
        let end = start + spelling.len();
        let before = source[..start].chars().next_back();
        let after = source[end..].chars().next();

        if !before.is_some_and(is_word) && !after.is_some_and(is_word) {
            found = Some(start);
        }
        from = end;
    }

    found.unwrap_or_else(|| panic!("the fixture uses `{spelling}`"))
}

/// The five shapes a modern codebase actually writes, each deduced from what the declaration wrote.
#[test]
fn an_auto_declaration_takes_the_type_of_its_initializer() {
    let cases = [
        (
            "int count();\nint f() { auto n = count(); return n; }\n",
            "n",
            "int",
        ),
        (
            "struct Widget { int size; };\nint f() { auto w = Widget{}; return w.size; }\n",
            "w",
            "Widget",
        ),
        (
            "int f() { int base = 1; auto copy = base; return copy; }\n",
            "copy",
            "int",
        ),
        (
            "int f() { int base = 1; const auto& r = base; return r; }\n",
            "r",
            "const int&",
        ),
        (
            "int f() { int base = 1; auto p = &base; return *p; }\n",
            "p",
            "int*",
        ),
    ];

    for (source, spelling, expected) in cases {
        assert_eq!(
            type_of_use(source, spelling).as_deref(),
            Some(expected),
            "in {source:?}"
        );
    }
}

/// **A chain of `auto`s resolves**, because the deducer asks the same question about the initializer's declaration
/// — which is what the depth guard exists for.
#[test]
fn a_chain_of_auto_declarations_resolves() {
    let source = "\
int count();
int f() {
    auto first = count();
    auto second = first;
    const auto& third = second;
    return third;
}
";
    assert_eq!(type_of_use(source, "first").as_deref(), Some("int"));
    assert_eq!(type_of_use(source, "second").as_deref(), Some("int"));
    assert_eq!(type_of_use(source, "third").as_deref(), Some("const int&"));
}

/// **What the declaration wrote around `auto` is what the type keeps**: the qualifier in front and the operators
/// behind, spelled the way the file would have spelled them had it named the type.
#[test]
fn the_written_decoration_is_kept() {
    let source = "\
int f() {
    int base = 1;
    const auto& kept = base;
    auto* pointer = &base;
    return kept;
}
";
    assert_eq!(type_of_use(source, "kept").as_deref(), Some("const int&"));
    assert_eq!(type_of_use(source, "pointer").as_deref(), Some("int*"));
}

/// **`auto&&` is refused.** The language deduces `T&` or `T&&` from the initializer's *value category*, which a
/// declaration's spelling does not record: reporting what was written around `auto` would be wrong half the time.
#[test]
fn a_forwarding_reference_is_not_deduced() {
    let source = "int f() { int base = 1; auto&& bound = base; return bound; }\n";
    assert_eq!(type_of_use(source, "bound"), None);
}

/// **`auto* p = x;` where `x` is not a pointer is refused** — the declaration does not compile, and inventing the
/// pointee would hide that.
#[test]
fn a_pointer_declaration_needs_a_pointer_initializer() {
    let source = "int f() { int base = 1; auto* wrong = base; return base; }\n";
    assert_eq!(type_of_use(source, "wrong"), None);
}

/// **A self-referential declaration terminates.** `auto x = x;` is ill-formed C++ and perfectly parseable, so the
/// recursion has to stop: past the depth bound the answer is `Unknown`, which is what every unreadable type gets.
#[test]
fn a_declaration_that_refers_to_itself_is_refused_rather_than_looping() {
    let source = "int f() { auto x = x; return 0; }\n";
    assert_eq!(type_of_use(source, "x"), None);
}

/// A declaration with nothing to deduce from — `auto n;` — is refused rather than answered with the word `auto`.
#[test]
fn a_declaration_with_no_initializer_is_refused() {
    let source = "int f() { auto n; return 0; }\n";
    assert_eq!(type_of_use(source, "n"), None);
}

/// **`auto` in another file is refused**, and the boundary is the syntax rather than the reading: the initializer
/// is in that header, and this layer holds one file's tree. The index's spelling for it is `auto`, which is not a
/// type, so saying nothing is the honest answer.
#[test]
fn an_auto_declaration_in_another_file_is_refused() {
    let header = "int count();\nauto measured = count();\n";
    let source = "#include \"b.h\"\nint f() { return measured; }\n";
    let session = session_with(&[("/p/a.cpp", source), ("/p/b.h", header)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let offset = last_word(source, "measured");

    match session.type_at(&view, offset) {
        cpp_code_analysis::Known::Yes(found) => assert_ne!(
            found.type_of, "auto",
            "the placeholder is never reported as a type"
        ),
        cpp_code_analysis::Known::Unknown(_) | cpp_code_analysis::Known::No => {}
    }
}

/// **The payoff: a member access through an `auto` variable now lists the class's members.**
///
/// This is what the deduction is *for* — `auto w = Widget{}; w.` is how modern C++ is written, and until the type
/// was computed the completion list was empty because the object's type was the word `auto`.
#[test]
fn a_member_access_through_auto_lists_the_members() {
    let source = "\
struct Widget { int size; int scaled(int factor) const; };
int f() {
    auto w = Widget{};
    w.
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let offset = source.rfind("w.").expect("the fixture writes the access") + 2;

    match session.member_completions(&view, offset) {
        cpp_code_analysis::Known::Yes(found) => {
            let names: Vec<String> = found
                .members
                .members
                .iter()
                .map(|member| member.fact.name.clone())
                .collect();
            assert!(
                names.contains(&"size".to_string()) && names.contains(&"scaled".to_string()),
                "the members of the deduced class: {names:?}"
            );
        }
        other => panic!("an `auto` object's members are listable now, got {other:?}"),
    }
}

/// A **plain** declaration is unaffected: the written spelling is still the answer, character for character.
#[test]
fn a_declaration_that_names_its_type_keeps_it() {
    assert_eq!(
        type_of_use(
            "struct Widget { int size; };\nint f() { const Widget* w = nullptr; return 0; }\n",
            "w"
        )
        .as_deref(),
        Some("Widget *")
    );
}
