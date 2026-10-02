//! What each name in a file is — the classifications a highlighter draws colours from.
//!
//! The interesting cases are the ones where two kinds look alike from the text: a parameter against a local, a
//! member function against a free one, an enumerator against a variable, and a macro (which is not a declaration
//! in the C++ tree at all). Each of them is pinned here, and so is the refusal: a name this analysis cannot place
//! gets **no** classification rather than a plausible one.

use cpp_code_analysis::semantic::{Name, NameKind, Provenance};
use cpp_code_analysis::Known;
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

/// Every classification of one spelling, as `(kind, is a declaration)` in source order.
fn all_of(source: &str, spelling: &str) -> Vec<(NameKind, bool)> {
    classified(source)
        .into_iter()
        .filter(|(text, _, _)| text == spelling)
        .map(|(_, kind, declaration)| (kind, declaration))
        .collect()
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

/// **A use is classified by what it resolves to, and a shadowed name resolves to the shadowing declaration.**
///
/// The case a spelling map cannot answer: this file declares a member `Widget::size`, a free `size(int)`, and a
/// parameter called `size`, and every use of the spelling has to be drawn as the one it means. The first version of
/// this pass classified all of them by the spelling, so the parameter came out as a free **function**.
#[test]
fn a_use_resolves_to_the_declaration_it_names_not_to_the_spelling() {
    let source = "\
struct Widget { int size; };
int size(int value);
int scaled(int size) { return size * size; }
int area(Widget& w) { return w.size; }
";
    let size = all_of(source, "size");
    assert_eq!(
        size,
        vec![
            // The member's declaration, the free function's declaration, the parameter's declaration and its two
            // uses — and then **nothing** for the member access, see the test below.
            (NameKind::Variable, true),
            (NameKind::Function, true),
            (NameKind::Parameter, true),
            (NameKind::Parameter, false),
            (NameKind::Parameter, false),
        ],
        "a parameter is not the file-scope function that shares its spelling"
    );
}

/// **A member access is not the file-scope declaration that shares its spelling** — the shape that made the old
/// spelling map most visibly wrong.
///
/// `w.size` is looked up in the type of `w`, which a scope chain cannot do, so the index is asked — and when the
/// class is not among the files this one can see, the question is asked about a *name*, where the file's own free
/// `size` is a different declaration with the same spelling. That is genuinely ambiguous, and an ambiguous name gets
/// **no** colour rather than one of the two: drawing `w.size` as a free function is the mistake this module was
/// rewritten to stop making, and drawing it as a member would be a guess from the spelling.
#[test]
fn a_member_access_whose_spelling_is_ambiguous_is_left_unclassified() {
    let source = "\
struct Widget { int size; };
int size(int value);
int area(Widget& w) { return w.size; }
";
    let size = all_of(source, "size");
    assert_eq!(
        size,
        vec![
            (NameKind::Variable, true),
            (NameKind::Function, true),
        ],
        "the two declarations are drawn; the member access is not"
    );
}

/// **A member access whose class the index knows is classified from that class.** `w.size` is a member of whatever
/// `w` is, and the index is what knows a class's members — the same `Widget::size` question
/// `textDocument/definition` asks.
#[test]
fn a_member_access_is_classified_from_the_class_it_is_a_member_of() {
    let header = "struct Widget { int size; };\n";
    let source = "#include \"b.h\"\nint measure(Widget& w) { return w.size; }\n";
    let session = session_with(&[("/p/a.cpp", source), ("/p/b.h", header)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let names: Vec<(String, NameKind, Provenance)> = session
        .classified_names(&view)
        .into_iter()
        .map(|name| {
            (
                source[name.range.start_offset..name.range.end_offset()].to_string(),
                name.kind,
                name.provenance,
            )
        })
        .collect();

    assert!(
        names.contains(&("size".to_string(), NameKind::Variable, Provenance::Found)),
        "the member's declaration is what `w.size` names: {names:?}"
    );
}

/// **Which layer answered is part of the answer.** A declaration's own name, a use the file's own scopes resolved,
/// a name only the index knows, and a macro are four different kinds of evidence, and a consumer that draws a
/// colour is entitled to tell them apart.
#[test]
fn the_provenance_says_which_layer_answered() {
    let header = "struct Widget { int size; };\n";
    let source = "\
#include \"b.h\"
#define LIMIT 8
int scale(int factor) { return factor * LIMIT; }
int measure(Widget& w) { return w.size; }
";
    let session = session_with(&[("/p/a.cpp", source), ("/p/b.h", header)]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    let provenance = |spelling: &str| -> Vec<Provenance> {
        session
            .classified_names(&view)
            .into_iter()
            .filter(|name| {
                &source[name.range.start_offset..name.range.end_offset()] == spelling
            })
            .map(|name| name.provenance)
            .collect()
    };

    assert_eq!(provenance("scale"), vec![Provenance::DeclaredHere], "written here");
    assert_eq!(
        provenance("factor"),
        vec![Provenance::DeclaredHere, Provenance::Held],
        "a parameter's own name, then the use the scope chain resolved"
    );
    assert_eq!(
        provenance("size"),
        vec![Provenance::Found],
        "a member of a class in another file is the index's answer"
    );
    assert_eq!(provenance("Widget"), vec![Provenance::Found], "and so is the class itself");
    assert_eq!(provenance("LIMIT"), vec![Provenance::Macro, Provenance::Macro], "a #define, not a declaration");
}

/// A client asked for a colour on a name it cannot have one on: `int`, `return` and a literal are not identifiers,
/// which is the filter this pass applies before it looks anything up.
#[test]
fn keywords_and_literals_are_not_classified() {
    let source = "struct Widget { int size; };\nint f() { return 1; }\n";
    let kinds = classified(source);

    for (text, _, _) in &kinds {
        assert!(
            !matches!(text.as_str(), "int" | "return" | "struct"),
            "`{text}` is not a name this layer classifies: {kinds:?}"
        );
    }
}

/// **A member the class inherits is found, because the query walks the chain that the member list walks.**
///
/// `pointer` is declared in `_Normal`, and `allocator_traits` names it only in its **base clause** — so the name
/// is not declared in the class at all, and a lookup that asked the index for `std::allocator_traits::pointer`
/// answered `NotDeclaredHere` while the member list of the very same class held it. Measured on MSVC's library,
/// where the clause is a `conditional_t<…>` rather than a plain base and the walk has to look at the classes the
/// clause names as well as the one it spells.
///
/// The second half is the one that keeps the first honest: a name the chain does not hold is still nowhere, and
/// answering with a base's member would be inventing a declaration.
#[test]
fn a_written_type_resolves_a_member_the_class_inherits() {
    let session = session_with(&[
        (
            "/p/base.h",
            "template <class _Alloc>\nstruct _Normal {\n  using pointer = int*;\n  int size;\n};\n",
        ),
        (
            "/p/a.cpp",
            "#include \"base.h\"\nnamespace std {\ntemplate <class _Alloc>\nstruct allocator_traits : _Normal<_Alloc> {};\n}\n",
        ),
    ]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    match session.definition_of_a_written_type(&view, "std::allocator_traits::pointer", None, 0) {
        Known::Yes(found) => assert_eq!(found.fact.name, "pointer"),
        other => panic!("`pointer` is inherited from `_Normal` and has to be found through it: {other:?}"),
    }

    assert!(
        matches!(
            session.definition_of_a_written_type(&view, "std::allocator_traits::nothing_here", None, 0),
            Known::Unknown(_)
        ),
        "the walk reads the chain; it does not invent a member: {:?}",
        session.definition_of_a_written_type(&view, "std::allocator_traits::nothing_here", None, 0)
    );
}
/// **A local's type is looked up from the namespace the file spells around it.**
///
/// [`DeclFact::scope`] is `None` for a declaration inside a function body — a body contributes no segment to a
/// qualified name — so a local `Widget` in `namespace app` is a name with **nothing to be looked up from**, and
/// every type written in a body was unresolvable. The view's scope chain at the declaration's own offset says
/// which namespaces enclose it, and that is what the offset parameter is for.
///
/// This is user code's shape rather than the standard library's, and the difference is real: a view is built
/// with **no macro evidence** (`FileView::parse`), so `namespace app {` is a scope in it while `_STD_BEGIN` —
/// whose replacement list lives in a header nothing here has read — is not. The second half of the test is that
/// limit written down, so that it is a known boundary rather than a surprise: the same declaration inside a
/// macro-opened namespace needs the namespace to travel **on the fact**, which is a change to what a summary
/// stores rather than to what this query asks.
#[test]
fn a_local_type_is_looked_up_from_the_namespace_around_it() {
    let session = session_with(&[(
        "/p/a.cpp",
        "namespace app {\nstruct Widget { int size; };\nvoid f() { Widget w; }\n}\n",
    )]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    // The local's own position, which is where its type is written.
    let at = "namespace app {\nstruct Widget { int size; };\nvoid f() { ".len();

    match session.definition_of_a_written_type(&view, "Widget", None, at) {
        Known::Yes(found) => assert_eq!(found.fact.qualified_name(), "app::Widget"),
        other => panic!("a local `Widget` in `namespace app` names `app::Widget`: {other:?}"),
    }

    // …and the fallback **qualifies** the name, it does not invent one.
    //
    // The first version of this asserted that `Widget` is unreachable from file scope, and it is reachable:
    // `ProjectIndex::definition` matches a fact on its **bare** name as well as its qualified one, which is what
    // lets a cursor in one file answer for a name declared in a namespace of another. That behaviour is older
    // than this query and is not what the fallback changes; what it must not do is turn a name nothing declares
    // into one that resolves.
    assert!(
        matches!(
            session.definition_of_a_written_type(&view, "Nothing_here_at_all", None, at),
            Known::Unknown(_)
        ),
        "a name no declaration holds is still nowhere: {:?}",
        session.definition_of_a_written_type(&view, "Nothing_here_at_all", None, at)
    );
}
/// **The fact carries the namespace even when the file never spells one.**
///
/// This is what [`DeclFact::in_namespace`] exists for, and it is the case a view **cannot** answer. A view is
/// built with no macro evidence, so `namespace app {` written literally is a scope in it and one a macro opened
/// is not — and every standard-library local is the second kind, because `_STD_BEGIN` is where `std` comes from.
/// The summary is read with the closure's macro bodies in hand, so the fact knows the namespace even though no
/// token in the file spells it.
///
/// Both halves are asserted, because either one alone would pass for the wrong reason: the fact must carry
/// `app`, **and** the view must not be able to supply it — otherwise this would be testing the fallback that was
/// already there rather than the field that was added.
#[test]
fn a_local_fact_carries_a_namespace_the_file_only_opens_with_a_macro() {
    let session = session_with(&[
        // **The macro is in another file, and that is the whole of the distinction.** A definition written in
        // the file itself *is* evidence the scope walk can use — it reads the file's own `#define` bodies as it
        // walks — so `BEGIN_APP` here would be understood from the buffer alone and the test would prove
        // nothing. `_STD_BEGIN` lives in `yvals.h` for the same reason: the namespace every standard-library
        // local sits in is spelled by a header the file only includes.
        (
            "/p/ns.h",
            "#define BEGIN_APP namespace app {\n#define END_APP }\n",
        ),
        (
            "/p/a.cpp",
            "#include \"ns.h\"\nBEGIN_APP\nstruct Widget { int size; };\nvoid f() { Widget w; }\nEND_APP\n",
        ),
    ]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    // The body's `Widget w;` — the local, and not the class of the same name above it.
    let local = session
        .index()
        .summaries()
        .flat_map(|summary| summary.declarations.iter())
        .find(|fact| fact.local && fact.name == "w")
        .expect("the local is declared");

    assert_eq!(
        local.in_namespace.as_deref(),
        Some("app"),
        "the summary is read with the closure's macros in hand, so the namespace is known"
    );
    assert!(
        local.scope.is_none(),
        "…and a body contributes no segment to a qualified name, which is why the field is needed: {:?}",
        local.scope
    );

    // The view cannot say it: `BEGIN_APP` is an identifier to a parse of **this file**, whose replacement list
    // is in `ns.h` and which nothing in a view has read. So no `app` scope is ever opened there.
    //
    // Asked of the **scope tree** rather than of a lookup, and this is the second test in this file corrected
    // for the same reason: `ProjectIndex::definition` matches a fact on its **bare** name as well as its
    // qualified one, so it finds `app::Widget` from a context that never mentions `app` — correctly, and
    // whatever namespace is passed. What the field changes is what the scope tree can supply, so that is what
    // is asked.
    assert!(
        view.scopes.scope_with_qualified_name("app").is_none(),
        "the macro that opens `app` is in another file, so the view has no such scope to walk from"
    );

    // …and with the fact's answer in hand the same name resolves.
    match session.definition_of_a_written_type(
        &view,
        "Widget",
        local.in_namespace.as_deref(),
        local.range.start_offset,
    ) {
        Known::Yes(found) => assert_eq!(found.fact.qualified_name(), "app::Widget"),
        other => panic!("`Widget` written in `app` names `app::Widget`: {other:?}"),
    }
}