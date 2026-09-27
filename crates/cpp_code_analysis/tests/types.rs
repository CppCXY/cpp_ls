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

/// **The class behind a linkage specification has its members.**
///
/// MSVC writes its whole iostream hierarchy as `_EXPORT_STD extern "C++" template <…> class basic_istream : …`,
/// and while the scope walk stopped at the unnamed outer declaration, the class and every member of it were
/// missing from the index — which is what `std::cin.read` and a completion after `std::cin.` had to answer from.
#[test]
fn a_member_of_a_class_behind_a_linkage_specification_resolves() {
    let source = "\
namespace lib {
extern \"C++\" template <class _Elem>
class Stream {
public:
    int member;
};
extern Stream<char> input;
}
int f() { return lib::input.member; }
";
    assert_eq!(type_of_use(source, "member").as_deref(), Some("int"));
}

/// **Two declarations of one name, one type between them.**
///
/// A name query answers `Ambiguous` when several declarations are visible, which is right for "where is this
/// declared" — and the question here is the *type*, which a redeclaration does not make ambiguous. MSVC's
/// `<iostream>` writes `cin` twice (once plain, once as `_EXPORT_STD extern "C++" … istream cin;`), and while the
/// type query could not settle it, every use of `std::cin` lost its type: measured, 8 offsets in one file.
#[test]
fn a_name_declared_twice_with_one_type_still_has_that_type() {
    let header = "struct Widget { int size; };\nextern Widget shared;\nextern Widget shared;\n";
    let source = "#include \"lib.h\"\nint f() { return shared.size; }\n";
    let session = session_with(&[("/p/a.cpp", source), ("/p/lib.h", header)]);

    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "shared").as_deref(),
        Some("Widget"),
        "the two declarations agree, so the type is not the ambiguous part"
    );
    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "size").as_deref(),
        Some("int"),
        "and the member access through it works"
    );
}

/// …but declarations that **disagree** about the type keep the `Unknown`: that is exactly what `Ambiguous` was
/// about, and picking one of them would be a guess.
#[test]
fn two_declarations_that_disagree_about_the_type_are_still_unknown() {
    let header = "struct One { int a; };\nstruct Two { int b; };\nextern One shared;\nextern Two shared;\n";
    let source = "#include \"lib.h\"\nint f() { return shared.a; }\n";
    let session = session_with(&[("/p/a.cpp", source), ("/p/lib.h", header)]);

    assert_eq!(type_of_use_in(&session, "/p/a.cpp", source, "shared"), None);
}

/// The type of the use of a name **in a session the caller built** — the two-file fixtures' way in.
fn type_of_use_in(session: &Session<MemoryFiles>, path: &str, source: &str, spelling: &str) -> Option<String> {
    let view = session.view(path).expect("the file is held");
    let offset = last_word(source, spelling);

    match session.type_at(&view, offset) {
        cpp_code_analysis::Known::Yes(found) => Some(found.type_of),
        _ => None,
    }
}

/// **A qualified name is a name.** `lib::global` is one entity, and "what type is it" has to be asked about the
/// whole chain rather than about the segment the expression happens to *start* at.
///
/// That offset is the whole subject of this test: the expression's node begins at `lib`, and a lookup at that
/// offset asks about the **namespace** — which has no type, so the answer used to be `UnknownType("lib::global")`
/// for every qualified name in every file.
#[test]
fn a_qualified_name_in_this_file_has_the_type_its_declaration_wrote() {
    let source = "\
namespace lib {
struct Widget { int size; };
extern Widget global;
}
int f() { return lib::global.size; }
";
    assert_eq!(type_of_use(source, "global").as_deref(), Some("Widget"));
    assert_eq!(
        type_of_use(source, "size").as_deref(),
        Some("int"),
        "and a member access through it reads the member's type"
    );
}

/// The same question through the **index**: the chain is declared in a header, so this file's own scopes cannot
/// answer it, and the whole qualified spelling is what the index has to be asked about.
#[test]
fn a_qualified_name_declared_in_a_header_has_its_type() {
    let header = "namespace lib { struct Widget { int size; }; extern Widget shared; }\n";
    let source = "#include \"lib.h\"\nint f() { return lib::shared.size; }\n";
    let session = session_with(&[("/p/a.cpp", source), ("/p/lib.h", header)]);

    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "shared").as_deref(),
        Some("Widget"),
        "the declaration is in the header, so the answer comes from the index"
    );
    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "size").as_deref(),
        Some("int"),
        "and the member access through it reads the member"
    );
}
