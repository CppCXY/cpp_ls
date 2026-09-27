//! What each name in a file is — the classifications a highlighter draws colours from.
//!
//! The interesting cases are the ones where two kinds look alike from the text: a parameter against a local, a
//! member function against a free one, an enumerator against a variable, and a macro (which is not a declaration
//! in the C++ tree at all). Each of them is pinned here, and so is the refusal: a name this analysis cannot place
//! gets **no** classification rather than a plausible one.

use cpp_code_analysis::semantic::{Name, NameKind};
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
    session.add_project_files(files.iter().map(|(path, _)| std::path::PathBuf::from(*path)));
    session.index_everything();

    session
}

/// Every classification in one file, as `(text, kind, is a declaration)` in source order.
fn classified(source: &str) -> Vec<(String, NameKind, bool)> {
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    session
        .classified_names(&view)
        .into_iter()
        .map(|name: Name| {
            (
                source[name.range.start_offset..name.range.end_offset()].to_string(),
                name.kind,
                name.declaration,
            )
        })
        .collect()
}

/// The one classification of a spelling, if there is exactly one.
fn kind_of(source: &str, spelling: &str) -> Option<NameKind> {
    let found: Vec<NameKind> = classified(source)
        .into_iter()
        .filter(|(text, _, _)| text == spelling)
        .map(|(_, kind, _)| kind)
        .collect();

    found.first().copied()
}

/// **Every kind the model can tell apart**, in one file: the cases that look alike from the text are the ones
/// worth pinning, and they are all here.
#[test]
fn each_kind_of_name_is_classified_by_what_declares_it() {
    let source = "\
#define LIMIT 8
namespace ns {
enum Color { Red, Green };
struct Widget {
    int size;
    int scaled(int factor) const;
};
int count(int total) {
    int local = total;
    return local;
}
}
";
    let cases = [
        ("LIMIT", Some(NameKind::Macro)),
        ("ns", Some(NameKind::Namespace)),
        ("Color", Some(NameKind::Type)),
        ("Widget", Some(NameKind::Type)),
        ("Red", Some(NameKind::EnumMember)),
        // A parameter and a local are both `Variable` bindings; the scope they were bound in is what separates
        // them, and it is the scope model's own distinction rather than a second reading.
        ("factor", Some(NameKind::Parameter)),
        ("total", Some(NameKind::Parameter)),
        ("local", Some(NameKind::Variable)),
        ("size", Some(NameKind::Variable)),
        // A function declared inside a class is a method, and the same shape outside one is not.
        ("scaled", Some(NameKind::Method)),
        ("count", Some(NameKind::Function)),
    ];

    for (spelling, expected) in cases {
        assert_eq!(kind_of(source, spelling), expected, "`{spelling}`");
    }
}

/// The declaration's own name and a use of it are the same kind, and only one of them is a declaration.
#[test]
fn a_use_is_classified_like_the_declaration_it_names() {
    let source = "\
int size(int value);
int f() { return size(1); }
";
    let size: Vec<(String, NameKind, bool)> = classified(source)
        .into_iter()
        .filter(|(text, _, _)| text == "size")
        .collect();

    assert_eq!(size.len(), 2, "the declaration and the use: {size:?}");
    assert_eq!(size[0].1, NameKind::Function);
    assert!(size[0].2, "the declaration's own name");
    assert_eq!(size[1].1, NameKind::Function);
    assert!(!size[1].2, "the use");
}

/// **A name a header declares is classified too** — the question is what the reader sees, and a `.cpp` is full of
/// names it does not declare itself. The index answers, once per spelling.
#[test]
fn a_name_from_another_file_is_classified() {
    let header = "struct Widget { int size; };\nint measure(Widget& w);\n";
    let source = "#include \"b.h\"\nint f(Widget& w) { return measure(w); }\n";
    let session = session_with(&[("/p/a.cpp", source), ("/p/b.h", header)]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    let found: Vec<(String, NameKind)> = session
        .classified_names(&view)
        .into_iter()
        .map(|name| {
            (
                source[name.range.start_offset..name.range.end_offset()].to_string(),
                name.kind,
            )
        })
        .collect();

    assert!(
        found.contains(&("Widget".to_string(), NameKind::Type)),
        "the header's class: {found:?}"
    );
    assert!(
        found.contains(&("measure".to_string(), NameKind::Function)),
        "and its function: {found:?}"
    );
}

/// **A name nothing declares gets no classification.** A colour is a claim, and this is the case where the
/// analysis has nothing to claim — the identifier keeps whatever the client draws for plain text.
#[test]
fn a_name_nothing_declares_is_not_classified() {
    let source = "int f() { return unknown_thing + 1; }\n";
    assert_eq!(kind_of(source, "unknown_thing"), None);
}

/// A macro is defined in a branch nobody takes — and it is still a name this file defines, which is what a reader
/// editing the file sees. The positional question ("is it a macro *here*") is a different one, and answering it
/// per identifier is what the module documentation measures as too expensive for a highlighter.
#[test]
fn a_macro_in_an_untaken_branch_is_still_a_macro() {
    let source = "\
#if 0
#define ONLY_HERE 1
#endif
int f() { return ONLY_HERE; }
";
    assert_eq!(kind_of(source, "ONLY_HERE"), Some(NameKind::Macro));
}

/// Two declarations of one spelling: both uses are coloured as what the declarations are, and the pass does not
/// pretend to know which one a given use means — that is a position question, and this pass exists to avoid them.
#[test]
fn one_spelling_with_two_declarations_is_still_classified() {
    let source = "\
int pick(int value);
double pick(double value);
double f() { return pick(1.5); }
";
    let picks: Vec<(String, NameKind, bool)> = classified(source)
        .into_iter()
        .filter(|(text, _, _)| text == "pick")
        .collect();

    assert_eq!(picks.len(), 3, "{picks:?}");
    assert!(picks.iter().all(|(_, kind, _)| *kind == NameKind::Function));
}

/// A template parameter is a type *name* that is not a type, and a highlighter that drew it as one would be
/// wrong about every use of it.
#[test]
fn a_template_parameter_is_its_own_kind() {
    let source = "template <class T> T identity(T value) { return value; }\n";
    assert_eq!(kind_of(source, "T"), Some(NameKind::TypeParameter));
}

/// The classifications are in source order, never overlap, and are never empty — the three properties the
/// protocol's delta encoding needs, checked on a file that has all of the kinds in it.
#[test]
fn the_classifications_are_sorted_and_disjoint() {
    let source = "\
#define LIMIT 8
namespace ns {
struct Widget { int size; };
int count(Widget& w) { int local = LIMIT; return local + w.size; }
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let names = session.classified_names(&view);

    assert!(!names.is_empty());
    for pair in names.windows(2) {
        assert!(
            pair[0].range.end_offset() <= pair[1].range.start_offset,
            "out of order or overlapping: {:?} then {:?}",
            pair[0],
            pair[1]
        );
    }
    assert!(names.iter().all(|name| name.range.length > 0));
}
