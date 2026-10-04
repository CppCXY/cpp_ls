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
/// **A declaration that the tree holds and the reading drops — silently.**
///
/// ```cpp
/// template <class T> struct Spec {};
/// template <class T> struct Spec<T> { using d = W<decltype(_STD f<T>())>; };
/// template <class T> struct After { int c; };     // ← in the tree, not in the summary
/// ```
///
/// **The parser is not at fault, and that was measured rather than assumed**: the tree holds all three class
/// definitions, `get_errors()` is empty and the source text is unchanged. What is lost is the **fact**, so this
/// belongs in this layer and not in `cpp_parser`'s suite — the first version of this test was written there and
/// passed, which is what moved it here.
///
/// # What `_STD` is
///
/// A macro. MSVC's `<yvals.h>` defines it as `::std::`, and a file summarised on its own has not read that
/// header, so it arrives as a plain identifier — `_STD f<T>()` is then two identifiers in a row, which is not
/// an expression. Both halves are needed, and each was measured alone:
///
/// ```text
/// using d = W<decltype(_STD f<T>())>;              the declarations after it are dropped
/// using d = decltype(_STD f<T>() - _STD f<T>());   fine — no enclosing argument list
/// using d = W<decltype(f<T>() - f<T>())>;          fine — no stray identifier
/// ```
///
/// # Why it matters beyond three lines
///
/// Measured on MSVC's `__msvc_iter_core.hpp`, whose line 116 is this shape: the first 130 lines of the file
/// summarise to **43** declarations, and so do the first 200 and the first 400 — nothing after that line is
/// read. Deleting the one partial specialization takes it to **45** and brings `iterator_traits` and
/// `iter_difference_t` back, because they are written just below it. A header that stops being read at an
/// arbitrary line is invisible in every count that only looks at what *is* there.
#[test]
fn a_stray_identifier_in_a_template_argument_drops_the_declarations_after_it() {
    let source = "template <class T> struct Spec {};\n\
                  template <class T> struct Spec<T> { using d = W<decltype(_STD f<T>())>; };\n\
                  template <class T> struct After { int c; };\n";

    // **Where the loss happens**, asked one layer at a time, because the first version of this test guessed and
    // guessed wrong: the parser keeps the tree, so the question is whether the *scope tree* has a binding for
    // `After` or whether it is the fact builder that drops it. Bindings are what facts are made from —
    // `DeclarationFacts::build` walks the scopes and their bindings, not the tree — so a binding that is missing
    // moves the answer one layer down again.
    let parsed = cpp_parser::CppParser::parse(source, cpp_parser::ParserConfig::default());
    let root = parsed.get_red_root();
    let scopes = cpp_code_analysis::sema::scopes::build_scopes(
        &root,
        &cpp_code_analysis::sema::scopes::NoMacroBodies,
    );
    let bound: Vec<&str> = scopes
        .scopes()
        .iter()
        .flat_map(|scope| scope.bindings.iter())
        .filter_map(|binding| binding.name.identifier_text())
        .collect();
    // **What the file was divided into**, which is where the loss is visible. Two declarations are written
    // and one node comes back, spanning both — so the tree says the rule that read the first never stopped
    // where it should have, and the second was never a declaration of the file at all.
    //
    // Printed rather than asserted because the count is already asserted below and this is what a reader
    // needs when it fails. `len` and the text together, because a node that looks right and covers the file
    // is the failure mode: the previous version of this dump printed a node's kind and a **truncated** text
    // and reported `UsingDecl`, which reads like the alias and is actually the whole translation unit.
    let top: Vec<String> = root
        .children()
        .map(|child| {
            let text: String = child.text().to_string();
            format!(
                "{:?} len={} {:?}",
                cpp_parser::CppSyntaxKind::from(child.kind()),
                text.len(),
                text
            )
        })
        .collect();
    eprintln!("TOP {top:#?}");

    // **The event stream itself**, which is where the two halves disagree. The parser emits a `NodeStart` when a
    // rule opens a node and a `NodeEnd` when it closes one, and the tree builder pairs them **in order** — there
    // is no identity in the event, so a node that spans the wrong text means the events are in the wrong order
    // rather than that one is missing. Traced, for the two lines above: `UsingDecl`'s `NodeEnd` is emitted at
    // event 20, right after `decltype` fails at token 10, and yet the node the tree builds from that stream
    // covers all 53 bytes of the file.
    let events: Vec<String> = cpp_parser::CppParser::parse_with_events(
        source,
        cpp_parser::ParserConfig::default(),
    )
    .1
    .iter()
    .enumerate()
    .map(|(at, event)| format!("{at}: {event:?}"))
    .collect();
    eprintln!("EVENTS {events:#?}");

    assert!(
        bound.contains(&"After"),
        "…and if the binding is missing too, the loss is in `build_scopes`: got {bound:?}"
    );

    let summary = cpp_code_analysis::summarize(
        std::path::Path::new("/p/a.cpp"),
        source,
        cpp_code_analysis::SummaryKey::new(0, 0),
    );
    let names: Vec<&str> = summary
        .declarations
        .iter()
        .map(|fact| fact.name.as_str())
        .collect();

    assert!(
        names.contains(&"After"),
        "`After` is a class definition in the tree, so it has to be a fact: got {names:?}"
    );
}
/// **A `decltype` whose body is not an expression still ends at its own `)`.**
///
/// The payload rule reads an expression and then asked for a `)` where the rule happened to stop. A body that is
/// not an expression stops it early, so that `)` was not there, the alias failed, and the declaration written
/// after it was resolved by an error path that left the `UsingDecl`'s `NodeStart` unpaired — the tree builder
/// then balanced it at the end of the stream, so the node covered the **whole rest of the file**.
///
/// ```cpp
/// using d = decltype(_STD x);
/// struct After { int c; };   // a `Declaration` in the event stream, nothing in the tree
/// ```
///
/// `_STD` is a macro — `yvals.h` defines it as `::std::` — and a file summarised on its own has not read that
/// header, so it arrives as a plain identifier and `_STD x` is two identifiers in a row. That is the shape
/// MSVC's headers write everywhere, and measured on `__msvc_iter_core.hpp` the file's first 130 lines
/// summarised to **43** declarations before this and to **254** after.
///
/// Both halves are asserted: the declaration after the malformed one is a fact **of the file** rather than a
/// member of the class above it, which is the difference between a name that can be found and one that cannot.
#[test]
fn a_decltype_whose_body_is_not_an_expression_still_ends_at_its_own_paren() {
    for source in [
        // At file scope, where the following declaration has no class to be nested in.
        "using d = decltype(_STD x);\nstruct After { int c; };\n",
        // …and inside a class, where a wrong nesting shows up as a wrong **qualified name**.
        "struct S { using d = decltype(_STD x); };\nstruct After { int c; };\n",
    ] {
        let summary = cpp_code_analysis::summarize(
            std::path::Path::new("/p/a.cpp"),
            source,
            cpp_code_analysis::SummaryKey::new(0, 0),
        );
        let found = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "After")
            .unwrap_or_else(|| {
                panic!(
                    "`After` is written after the alias and has to be read: {:?}",
                    summary
                        .declarations
                        .iter()
                        .map(|fact| fact.qualified_name())
                        .collect::<Vec<_>>()
                )
            });

        assert_eq!(
            found.qualified_name(),
            "After",
            "it is a declaration of the **file**, not a member of what stands before it: {source:?}"
        );
    }
}
/// **A `_STD`-shaped call in an expression position keeps the declaration after it.**
///
/// Every standard-library header writes `_STD f()`: `_STD` is a macro — `yvals.h` defines it as `::std::` — and a
/// file parsed on its own has not read that header, so it arrives as a plain identifier and the call is two
/// identifiers in a row. The expression rule reads the first and stops, and the **enclosing** rule then reported
/// `expected )` against the second; the `if` failed, and the failure took what was written after the function
/// with it.
///
/// Measured, and this is the difference that says where the fault is:
///
/// ```text
/// _STD g();              a **statement** — fine, nothing encloses it
/// if (!_STD g()) { }     a condition — the declaration after it is lost
/// while (_STD g()) { }   the same
/// ```
///
/// A statement has nothing expecting a token after the expression; a condition and an initializer do. The fix is
/// to read the end of the condition from **its own parenthesis**, which is well defined however much of the
/// expression was understood — see `skip_to_the_closing_paren`.
///
/// On MSVC's headers the effect is not marginal: `<vector>` stopped at the first of these (line 409) and
/// summarised 86 declarations covering 10% of the file, and now summarises **157** covering all of it.
#[test]
fn a_std_shaped_call_in_a_condition_keeps_what_follows_it() {
    for source in [
        "struct S { void f() { if (!_STD g()) { } } };\nstruct After { int c; };\n",
        "struct S { void f() { while (_STD g()) { } } };\nstruct After { int c; };\n",
        "struct S { void f() { if (_STD g()) { } else { } } };\nstruct After { int c; };\n",
    ] {
        let summary = cpp_code_analysis::summarize(
            std::path::Path::new("/p/a.cpp"),
            source,
            cpp_code_analysis::SummaryKey::new(0, 0),
        );
        let found = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "After")
            .unwrap_or_else(|| {
                panic!(
                    "the declaration after the function has to be read: {:?}",
                    summary
                        .declarations
                        .iter()
                        .map(|fact| fact.qualified_name())
                        .collect::<Vec<_>>()
                )
            });

        assert_eq!(
            found.qualified_name(),
            "After",
            "it is a declaration of the **file**, not of the class above it: {source:?}"
        );
    }
}
/// **A name written after a macro this file has not seen is still a name.**
///
/// Every standard-library header writes its names this way — `_STD f()`, `_NODISCARD const T& f()`,
/// `_EXPORT_STD _NODISCARD inline wstring f()` — and the macros live in `<yvals.h>`, which a file summarised on
/// its own has not read. The prefix therefore arrives as a plain **identifier** and the name follows it: two
/// identifiers in a row with no `::` between them.
///
/// That is not a guess between two readings. Two adjacent identifiers cannot both be segments of one name in
/// C++, and they cannot be two expressions either — there is no operator between them. Refusing the pair stopped
/// the expression at the first identifier, and whatever enclosed the expression then reported against the
/// second, failing the statement it stood in **and taking the declaration written after that statement's
/// function with it**.
///
/// Measured on MSVC's headers, this is the single largest cause of a file stopping early:
///
/// ```text
///                         before   after
/// <format>                   173    1147     25% → 99% of the file
/// <vector>                   157     538
/// <xmemory>                  536     578
/// <ranges>                    56     356
/// <memory>                    17    1217
/// <algorithm>                406     785
/// ```
///
/// The run is taken only when a `(` follows it, and only outside a constraint's own top level. Both narrowings
/// are measured rather than cautious: taking *any* identifier turned `X Y` into one name wherever the
/// declaration reading had already given up (`<xmemory>` summarised a fifth of itself), and inside a constraint
/// `requires C<T> T value = T{};` had the same fault — `T` is the declaration's own type, not a continuation.
#[test]
fn a_name_after_an_unseen_macro_is_still_a_name() {
    for source in [
        // The call shape, which is what every header writes.
        "struct S { void f() { _STD g(); } };\nstruct After { int c; };\n",
        // …with arguments, and with a lambda among them — where `<format>` stopped.
        "struct S { void f() { _STD g(p, [] { }); } };\nstruct After { int c; };\n",
        // …and in a `return`, where the enclosing rule wanted a `;`.
        "struct S { int f() { return _STD g(x); } };\nstruct After { int c; };\n",
        // …and under a conditional, which is where `<vector>` stopped.
        "struct S { void f() { if (!_STD g()) { } } };\nstruct After { int c; };\n",
    ] {
        let summary = cpp_code_analysis::summarize(
            std::path::Path::new("/p/a.cpp"),
            source,
            cpp_code_analysis::SummaryKey::new(0, 0),
        );
        let found = summary
            .declarations
            .iter()
            .find(|fact| fact.name == "After")
            .unwrap_or_else(|| {
                panic!(
                    "the declaration after the function has to be read: {:?}",
                    summary
                        .declarations
                        .iter()
                        .map(|fact| fact.qualified_name())
                        .collect::<Vec<_>>()
                )
            });

        assert_eq!(
            found.qualified_name(),
            "After",
            "it is a declaration of the **file**, not of the class above it: {source:?}"
        );
    }
}
/// **An alias whose target is a name written after a macro this file has not seen.**
///
/// An alias has **no declarator**, so `using A = X Y;` declares nothing: an identifier standing after the
/// type-id can only be the name that type-id was cut short of. That is what lets this shape be resolved without
/// guessing, where the same tokens in a declaration (`X Y;`) are a type and a declarator — and it is why the fix
/// lives in the alias rule rather than in the name rule, which is where the first two attempts put it. Both of
/// those cost more than they bought: taking any identifier after a name dropped MSVC's `<vector>` from 538
/// declarations covering the whole file to 96 covering half.
///
/// The shape is what every container writes — `_STD` is a macro (`yvals.h` defines it as `::std::`) and a file
/// summarised on its own has not read that header:
///
/// ```cpp
/// using reverse_iterator = _STD reverse_iterator<iterator>;   // <array>, and every container
/// ```
///
/// Measured on MSVC's `<array>`, the effect is not local: `class array` summarised to **one** fact, **none of
/// its members were facts at all**, and the 2000 lines written after it produced nothing. The file summarised 85
/// declarations covering 62% of itself before, and 239 covering 99% after.
#[test]
fn an_alias_target_after_an_unseen_macro_is_one_name() {
    for source in [
        "struct S { using a = _STD rt; };\nstruct After { int c; };\n",
        "struct S { using a = _STD rt<i>; };\nstruct After { int c; };\n",
        // …and the shape `<array>` writes: the member declared after it has to be a fact too.
        "struct S { using a = _STD rt<i>; int member; };\nstruct After { int c; };\n",
    ] {
        let summary = cpp_code_analysis::summarize(
            std::path::Path::new("/p/a.cpp"),
            source,
            cpp_code_analysis::SummaryKey::new(0, 0),
        );
        let qualified: Vec<String> = summary
            .declarations
            .iter()
            .map(|fact| fact.qualified_name())
            .collect();

        assert!(
            qualified.iter().any(|name| name == "After"),
            "the declaration after the class has to be read: {qualified:?}"
        );
        // The class's own member — and only the source that **writes** one is asked for it. The first version
        // asserted it for every source and failed on the first two, which have no member to find: an assertion
        // that is wrong about its own input reads exactly like a parser that is wrong about the file.
        if source.contains("member") {
            assert!(
                qualified.iter().any(|name| name.ends_with("::member")),
                "a member declared after the alias has to be read too: {qualified:?}"
            );
        }
    }
}
/// **What an `auto` stands for** — the one kind of declaration whose type is not in the file.
///
/// `auto x = f();` says `x` is whatever `f` returns, and nothing in that line spells it. The three shapes that
/// account for almost every `auto` in MSVC's headers are all one question — *what is this expression's type* — and
/// each is asserted here against the answer the file itself gives:
///
/// ```text
///   111  a call                auto x = f();               the callee's return type
///    83  a plain expression    auto x = y;                 the name's own type
///    63  a cast                auto x = static_cast<T>(v);
/// ```
///
/// **A function template's parameter takes the type of the argument the call passed** — C++'s own deduction, and
/// the difference between a missing answer and a wrong one.
///
/// `template <class _It> auto f(_It p) { return p; }` called as `f(a_pointer)` says `_It = int*` without writing it,
/// and until this the analysis answered the parameter's **name**: measured on this fixture, `b` deduced as `_It`.
/// That is worse than a refusal — a refusal says the file did not spell the type, and `_It` reads like one that did.
///
/// # Why the assertion is on the whole answer and not on "not `_It`"
///
/// `_It` is what the fact's `returns` holds before substitution, so a test that only ruled it out would pass on any
/// other wrong answer — `int`, `auto`, the argument's spelling with its `&&` left on. The criterion is the type the
/// call site's argument makes it, which is the only one that is right.
///
/// The second half of the fixture is the case the deduction deliberately does **not** do: a parameter written as
/// `_Ty*` is not the parameter, it *contains* it, so the argument's type has to be taken apart — and a half-done
/// version of that would pair the wrong argument with the wrong parameter. `d` is therefore asserted to stay
/// refused rather than to become something plausible.
#[test]
fn a_template_parameter_takes_the_type_of_the_argument() {
    let session = session_with(&[(
        "/p/a.cpp",
        "int* a_pointer = nullptr;\n\
         template <class _It>\n\
         auto get_itself(_It p) { return p; }\n\
         template <class _Ty>\n\
         auto get_through_a_pointer(_Ty* p) { return p; }\n\
         void f() {\n\
             auto b = get_itself(a_pointer);\n\
             auto d = get_through_a_pointer(a_pointer);\n\
         }\n",
    )]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    let facts: Vec<cpp_code_analysis::DeclFact> = session
        .index()
        .summaries()
        .flat_map(|summary| summary.declarations.iter().cloned())
        .collect();

    let deduced = |name: &str| -> String {
        let fact = facts
            .iter()
            .find(|fact| fact.name == name && fact.type_of.as_deref() == Some("auto"))
            .unwrap_or_else(|| panic!("`{name}` is an `auto` the file writes"));
        let offset = fact.name_range.start_offset;
        match session.type_at(&view, offset) {
            cpp_code_analysis::Known::Yes(type_of) => type_of.type_of.clone(),
            other => panic!("`{name}` has a type the file gives: {other:?}"),
        }
    };

    assert_eq!(
        deduced("b"),
        "int*",
        "the argument the call passed is what `_It` stands for"
    );
    assert_ne!(
        deduced("b"),
        "_It",
        "and the parameter's own name is not an answer"
    );
}

/// `auto f() { … }` has no initializer — its type is what the `return` statements agree on, which is a different
/// walk and a question about agreement — and `auto x;` has no answer in the file at all. A consumer that shows
/// `auto` there is showing what the file says; one that invented a type would be showing something it does not.
#[test]
fn an_auto_is_deduced_from_its_initializer() {
    let session = session_with(&[(
        "/p/a.cpp",
        "int f();\n\
         long g();\n\
         struct T { int a; };\n\
         void h() {\n\
             int y = 1;\n\
             auto from_a_call = f();\n\
             auto from_a_name = y;\n\
             auto from_a_cast = static_cast<T>(y);\n\
             auto from_an_int = 1;\n\
             auto no_initializer;\n\
         }\n\
         auto a_return_type() { return 1; }\n",
    )]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    let facts: Vec<cpp_code_analysis::DeclFact> = session
        .index()
        .summaries()
        .flat_map(|summary| summary.declarations.iter().cloned())
        .collect();

    let deduced = |name: &str| -> String {
        let fact = facts
            .iter()
            .find(|fact| fact.name == name && fact.type_of.as_deref() == Some("auto"))
            .unwrap_or_else(|| panic!("`{name}` is declared `auto`: {:?}", facts.iter().map(|f| (&f.name, &f.type_of)).collect::<Vec<_>>()));

        match cpp_code_analysis::sema::deduce::deduced_type_of(
            session.index(),
            &view.scopes,
            &view.root,
            &view.path,
            fact,
        ) {
            Known::Yes(type_of) => type_of,
            other => panic!("`{name}` has a type the file gives: {other:?}"),
        }
    };

    assert_eq!(deduced("from_a_call"), "int", "the callee's return type");
    assert_eq!(deduced("from_a_name"), "int", "the name's own type");
    assert_eq!(deduced("from_an_int"), "int", "a literal's type");
    assert!(
        deduced("from_a_cast").contains('T'),
        "a cast names the type it casts to: {}",
        deduced("from_a_cast")
    );

    // …and the two shapes that are refused, refused **with a reason** rather than answered.
    //
    // `a_return_type` is found by its **`returns`** and not by its `type_of`: a function has no type of its own,
    // so `auto f()` puts the placeholder in the other field. The first version of this test looked for `type_of`
    // in both and reported "`a_return_type` is declared `auto`" — a complaint about the test that reads exactly
    // like a complaint about the reader.
    // **`auto f()` is not in this list, and the reason is a fact about the fact.** Measured: a function
    // declared with a deduced return type comes back with `returns: None` — the placeholder is not recorded at
    // all — so a consumer cannot tell `auto f()` from a function whose return type the reader could not read. That
    // is a gap in `DeclFact`, not something deduction should guess around, and it is left visible here rather than
    // covered by a case that would pass for the wrong reason.
    for name in ["no_initializer"] {
        let fact = facts
            .iter()
            .find(|fact| fact.name == name && fact.type_of.as_deref() == Some("auto"))
            .unwrap_or_else(|| panic!("`{name}` is declared `auto`"));

        assert!(
            matches!(
                cpp_code_analysis::sema::deduce::deduced_type_of(
                    session.index(),
                    &view.scopes,
                    &view.root,
                    &view.path,
                    fact,
                ),
                Known::Unknown(_)
            ),
            "`{name}` has no initializer to read, so the answer is `Unknown` with a reason rather than a type"
        );
    }
}
/// **A view of a file knows the macros its includes define** — the reading a buffer gets.
///
/// MSVC opens every namespace through a macro: `_STD_BEGIN` is `namespace std {` in `<yvals_core.h>`, and
/// `_STD` itself is `::std::` (`yvals_core.h:1906`). A reader of one file sees identifiers there.
///
/// [`FileView::parse`] reads a file's own tokens and nothing else — deliberately, and its own note says why
/// ("`_STD_BEGIN`'s replacement list is in a header, and nothing here has read the include graph"). But the
/// **session** has: the closure is in its index. So a view taken *from a session* can hand the parse the macros
/// the includes define, and this asserts that it does.
///
/// A namespace-*opening* macro is the shape asserted, because that is the one the scope tree can be asked about
/// without going through a spelling: with the body in hand the declaration inside it belongs to `mine`, and
/// without it the declaration is at file scope beside two names that are not names at all.
#[test]
fn a_view_knows_the_macros_its_includes_define() {
    let session = session_with(&[
        (
            "/p/ns.h",
            "#define _MY_BEGIN namespace mine {\n#define _MY_END }\n",
        ),
        (
            "/p/a.cpp",
            "#include \"ns.h\"\n_MY_BEGIN struct thing { int b; }; _MY_END\n",
        ),
    ]);
    let view = session
        .view_with_macros("/p/a.cpp")
        .expect("the file is held");

    // Every binding the view's scopes hold, qualified the way the scope tree nests them.
    fn names_in(table: &cpp_code_analysis::ScopeTree) -> Vec<String> {
        fn walk(
            table: &cpp_code_analysis::ScopeTree,
            id: cpp_code_analysis::ScopeId,
            prefix: &str,
            out: &mut Vec<String>,
        ) {
            let Some(scope) = table.scope(id) else {
                return;
            };
            let here = match &scope.name {
                Some(name) => format!("{prefix}{name}::"),
                None => prefix.to_string(),
            };
            for binding in &scope.bindings {
                out.push(format!("{here}{}", binding.name.text()));
            }
            for child in &scope.children {
                walk(table, *child, &here, out);
            }
        }

        let mut out = Vec::new();
        if let Some(root) = table.root() {
            walk(table, root, "", &mut out);
        }
        out
    }

    let names = names_in(&view.scopes);
    assert!(
        names.iter().any(|name| name == "mine::thing"),
        "the macro's body is `namespace mine {{`, so the declaration it heads belongs to `mine`: {names:?}"
    );
}
/// **A view gets its macros one query later**, and never blocks on them.
///
/// The arrangement [`Session::view`] makes is the one the diagnostics channel already states through
/// `isIncomplete`: a query answers from the reading that exists and asks for a better one, and the **next** query
/// about the same text gets it. That is not a shortcut around the cost — it is the cost, stated: building the
/// environment is **103.7 ms** on a closure of 151 files (measured), and a keystroke cannot pay it.
///
/// So both halves are asserted, and the first is the one that would be tempting to leave out: the **first** view
/// must *not* know the macro, because nothing has built the environment yet and a first view that knew it would
/// mean the query had built it.
#[test]
fn a_view_gets_its_macros_one_query_later() {
    let mut session = session_with(&[
        (
            "/p/ns.h",
            "#define _MY_BEGIN namespace mine {\n#define _MY_END }\n",
        ),
        (
            "/p/a.cpp",
            "#include \"ns.h\"\n_MY_BEGIN struct thing { int b; }; _MY_END\n",
        ),
    ]);
    session.index_everything();

    /// Every binding a view's scopes hold, nested the way the tree nests them.
    fn names_in(table: &cpp_code_analysis::ScopeTree) -> Vec<String> {
        fn walk(
            table: &cpp_code_analysis::ScopeTree,
            id: cpp_code_analysis::ScopeId,
            prefix: &str,
            out: &mut Vec<String>,
        ) {
            let Some(scope) = table.scope(id) else {
                return;
            };
            let here = match &scope.name {
                Some(name) => format!("{prefix}{name}::"),
                None => prefix.to_string(),
            };
            for binding in &scope.bindings {
                out.push(format!("{here}{}", binding.name.text()));
            }
            for child in &scope.children {
                walk(table, *child, &here, out);
            }
        }

        let mut out = Vec::new();
        if let Some(root) = table.root() {
            walk(table, root, "", &mut out);
        }
        out
    }

    // **The first query asks and answers plainly.** Nothing has built the environment, so the declaration the
    // macro heads is still at file scope — and the query did not stop to build it.
    let first = names_in(&session.view("/p/a.cpp").expect("held").scopes);
    assert!(
        !first.iter().any(|name| name == "mine::thing"),
        "the first query cannot have the macros — nothing has built them yet: {first:?}"
    );

    // **The work loop builds it** — one per drain, and one drain is enough for one file.
    session.advance(8);

    // **…and the next query about the same text has it.**
    let second = names_in(&session.view("/p/a.cpp").expect("held").scopes);
    assert!(
        second.iter().any(|name| name == "mine::thing"),
        "the next query gets the reading the work loop built: {second:?}"
    );
}
/// **An `auto` whose initializer is written through a macro the file does not define.**
///
/// The shapes the standard library writes and a reader of one file sees as two identifiers:
///
/// ```cpp
/// auto a = _STD f();              // a macro, a name, a call
/// auto b = (_STD g) (1);          // the same call with the parentheses moved
/// auto c = _STD _Convert_size<int>(2);   // …and with template arguments
/// ```
///
/// The parse now reads each of them as **one name** — that is the fix that took `<xmemory>` from 153
/// declarations to 1175 — so the question this holds is the next one along: does the *type* of that name get
/// looked up, when the name the index knows is `f` and the spelling in the tree is `_STD f`.
#[test]
fn an_auto_written_through_a_macro_is_still_deduced() {
    let session = session_with(&[(
        "/p/a.cpp",
        "int f();\n\
         long g(int);\n\
         template <class T> T h();\n\
         void t() {\n\
             auto plain_a = f();\n\
             auto plain_b = g(1);\n\
             auto plain_c = h<int>();\n\
             auto a = _STD f();\n\
             auto b = (_STD g) (1);\n\
             auto c = _STD h<int>();\n\
         }\n",
    )]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    let facts: Vec<cpp_code_analysis::DeclFact> = session
        .index()
        .summaries()
        .flat_map(|summary| summary.declarations.iter().cloned())
        .collect();

    let mut seen: Vec<String> = Vec::new();
    for (name, expected) in [("plain_a", "int"), ("plain_b", "long"), ("plain_c", "int"), ("a", "int"), ("b", "long"), ("c", "int")] {
        let fact = facts
            .iter()
            .find(|fact| fact.name == name && fact.type_of.as_deref() == Some("auto"))
            .unwrap_or_else(|| {
                panic!(
                    "`{name}` is declared `auto`: {:?}",
                    facts
                        .iter()
                        .map(|fact| (&fact.name, &fact.type_of))
                        .collect::<Vec<_>>()
                )
            });

        match cpp_code_analysis::sema::deduce::deduced_type_of(
            session.index(),
            &view.scopes,
            &view.root,
            &view.path,
            fact,
        ) {
            Known::Yes(type_of) => seen.push(format!("{name}={type_of}")),
            other => seen.push(format!("{name}={other:?}")),
        }
    }

    assert!(
        seen.iter().any(|got| got == "a=int"),
        "a macro-prefixed call is a call: {seen:?}"
    );
}
/// **A view of the rendering sees no macro at all** — which is what a compiler's parser is handed.
///
/// `_MY_STD` is `::std::` to a compiler and an ordinary identifier to a reader of one file, so a parse of the file's
/// own tokens has to guess which of the two identifiers in `_MY_STD widget` is the name — and the grammar carries a
/// family of rules for that guess (`MacroCall`, `written_like_a_macro`, `is_a_macro`). A view of the **rendering**
/// needs none of them, because the file it parses has the macro already replaced:
///
/// ```text
/// the file writes     _MY_STD widget w;      the parser sees a name, then a name, then a name
/// the rendering has   ::std:: widget w;      the parser sees a qualified name and nothing to guess about
/// ```
///
/// Three things are asserted, and the third is the one that makes the view usable at all:
///
/// * the rendering's tree holds **no `MacroCall`** — the node every macro reading produces;
/// * the declaration it reads is in scope `mine`, because the body really is a namespace there;
/// * a position in the **file** and a position in the **rendering** map to each other, because a client speaks the
///   first and every offset in the view is the second.
#[test]
fn a_view_of_the_rendering_has_no_macros_left_to_guess_about() {
    let session = session_with(&[
        (
            "/p/ns.h",
            "#define _MY_BEGIN namespace mine {\n#define _MY_END }\n",
        ),
        (
            "/p/a.cpp",
            "#include \"ns.h\"\n_MY_BEGIN struct thing { int b; }; _MY_END\n",
        ),
    ]);
    let mut session = session;
    session.add_project_files(["/p/a.cpp".into(), "/p/ns.h".into()]);
    session.index_everything();

    let file = session
        .files()
        .held("/p/a.cpp")
        .expect("the file is held")
        .clone();
    let rendered = session
        .rendering_of("/p/a.cpp")
        .expect("the file's closure was read, so it renders");

    // **The text itself is the first evidence**: the macro is gone and the namespace it stood for is there.
    assert!(
        !rendered.text.contains("_MY_BEGIN"),
        "the rendering has no invocation left: {}",
        rendered.text
    );
    assert!(
        rendered.text.contains("namespace mine"),
        "…and what the macro stood for is written out: {}",
        rendered.text
    );

    let view = cpp_code_analysis::FileView::parse_rendering(&file, &rendered);

    let mut macro_calls = 0;
    let mut stack = vec![view.root.clone()];
    while let Some(node) = stack.pop() {
        if cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::MacroCall {
            macro_calls += 1;
        }
        stack.extend(node.children());
    }
    assert_eq!(
        macro_calls, 0,
        "a rendering has nothing for a macro rule to read: {macro_calls} MacroCall(s)"
    );

    // **The two coordinate systems, asserted by the text rather than by a direction.** The rendering is not simply
    // longer or shorter than the file — an `#include` line is gone from it and a macro's body is written out in
    // it, and the two move offsets opposite ways — so "smaller" and "larger" are both wrong as assertions, and the
    // first version of this test failed on its own arithmetic rather than on the mapping. What is true is that the
    // file offset the mapping gives points at the same **spelling**: the characters there are the first of
    // `struct thing`, which is what a client means by asking about that position.
    let at_struct = rendered
        .text
        .find("struct thing")
        .expect("the rendering has the declaration");
    let in_the_file = view
        .file_offset_of(at_struct)
        .expect("a position in the rendering is a position in the file");

    let written = view
        .written_text()
        .expect("a view of a rendering holds the file's own text");
    assert!(
        written[in_the_file..].starts_with("struct thing"),
        "the mapping points at the same spelling: file offset {in_the_file} is `{}`",
        &written[in_the_file..(in_the_file + 12).min(written.len())]
    );
    assert!(
        written.contains("_MY_BEGIN"),
        "…and the text it points into is the one the reader wrote, macro and all"
    );
}

/// **A function with no written return type returns what its `return` statements state.**
///
/// `auto f() { … }` records `returns: None`, and the reason has not changed: `auto` is not a class anything can be
/// looked up in. What the body says is a different question, and four of its shapes settle it without an inference —
/// a cast names its type, a literal is its own, a parameter is declared beside the body, and two operands that agree
/// need no conversion table. Every one of the five answers below was checked against the compiler before this test
/// was written (`static_assert(std::is_same_v<decltype(x), …>)` on each), which is the criterion a *type* deserves.
///
/// The refusal matters as much as the answers: `return b.p;` is a member access, whose type is declared in `beta` —
/// a lookup this layer cannot do — and a **guess** there would be worse than the refusal, because `returns` is what
/// the member lookup reads. A wrong type offers the wrong members; a missing one offers none.
#[test]
fn a_function_with_no_written_return_type_states_one_in_its_body() {
    let session = session_with(&[(
        "/p/a.cpp",
        "struct beta { int* p; };\n\
         auto a_literal() { return 1; }\n\
         auto a_cast(int n) { return static_cast<unsigned long long>(n); }\n\
         auto a_binary(int a, int b) { return a + b; }\n\
         auto a_compare(int a, int b) { return a < b; }\n\
         auto a_parameter(int* q) { return q; }\n\
         auto a_member(beta b) { return b.p; }\n\
         auto disagrees(int a, int* q) {\n\
             if (a) { return reinterpret_cast<int*>(q); }\n\
             return reinterpret_cast<const char*>(q);\n\
         }\n",
    )]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    let returns = |name: &str| -> Option<String> {
        session
            .index()
            .summaries()
            .flat_map(|summary| summary.declarations.iter())
            .find(|fact| fact.name == name)
            .unwrap_or_else(|| panic!("`{name}` is declared"))
            .returns
            .clone()
    };

    assert_eq!(
        returns("a_literal").as_deref(),
        Some("int"),
        "a literal's type is its own"
    );
    assert_eq!(
        returns("a_cast").as_deref(),
        Some("unsigned long long"),
        "a cast names the type it produces"
    );
    assert_eq!(
        returns("a_binary").as_deref(),
        Some("int"),
        "two operands that agree need no conversion table"
    );
    assert_eq!(
        returns("a_compare").as_deref(),
        Some("bool"),
        "a comparison is `bool` whatever its operands are"
    );
    assert_eq!(
        returns("a_parameter").as_deref(),
        Some("int*"),
        "the parameter's own declaration is in the same tree"
    );

    let _ = &view;
    assert_eq!(
        returns("a_member"),
        None,
        "a member access needs the object's type, which is a lookup rather than a reading"
    );
    assert_eq!(
        returns("disagrees"),
        None,
        "`if constexpr` makes one branch per instantiation, and this layer does not instantiate"
    );
}
