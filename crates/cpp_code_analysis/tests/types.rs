//! What type a declaration has — including the ones that wrote `auto`.
//!
//! `type_of_expression` reads a name's type off its declaration, which works for every spelling that *is* a type
//! and fails for the one that is a placeholder. These tests are about the second case: the initializer's type,
//! what the declaration wrote around `auto`, and the shapes that are refused because the language's rule is not the
//! one a spelling suggests (`auto&&`) or because the declaration is not in this file's syntax.
//!
//! The type is always the **written** spelling — `const int&` is those three tokens — which is the same boundary
//! every other query in this crate has; what is new here is that the spelling is *computed* rather than copied.

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
    session.add_project_files(
        files
            .iter()
            .map(|(path, _)| std::path::PathBuf::from(*path)),
    );

    // **The ask, and then the pump** - see `tests/completion.rs`'s `cooked_view`. A view is of the file's *cooked
    // reading*, and a rendering is built because something asked for it.
    for (path, _) in files {
        session.want_cooked_reading(std::path::Path::new(path));
    }
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

    match session.type_at(&view, as_reading(&view, offset)) {
        cpp_code_analysis::Known::Yes(found) => Some(found.type_of),
        _ => None,
    }
}

/// **A file offset in the rendering's coordinates**, for the queries that read `view.root`.
///
/// The same crossing `tests/signature.rs`'s `signature_where_marked` makes, and the same mistake this file made at
/// three call sites: a rendering has its **line breaks collapsed** and its comments removed, so a position computed
/// against the fixture's own text is not a position in it. Measured: thirteen tests in this suite answered `None`
/// where a type was expected, every one of them through this offset.
fn as_reading(
    view: &cpp_code_analysis::FileView,
    offset: usize,
) -> cpp_code_analysis::file::coordinates::ReadingOffset {
    view.reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(
        offset,
    ))
    .expect("the cursor is in the rendering")
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
/// **A `using` alias declares the type after its `=`** — and nothing next to it inherits it.
///
/// `using X = Y;` writes the type on the **right of the `=`**, in a `TypeId` of its own: not in a declarator, not
/// in a specifier sequence, which is why the reader that answers "what type does this name declare" needs a branch
/// that reads neither. The review registered this path as having no test of its own ("`using X = Y;` 与 `typedef`
/// … 这条路还没有专门的测试"), and it is the branch that reads `Shape::type_id`.
///
/// The question is asked of **member accesses** rather than of the declarations: a declaration answers with the
/// spelling the file wrote (`size_type value;` *is* a `size_type`), and following the alias is what the use-side
/// reader does. Asking the declaration and expecting `unsigned long` is a test that fails against correct code —
/// which is how this one started.
///
/// The **neighbours** are the other half, and they are why this is not one assertion: a target read out of the
/// wrong node — which is what picking the enclosing construct instead of the innermost one does — shows up first
/// as a neighbour with the alias's type.
#[test]
fn a_using_alias_member_declares_the_type_after_the_equals() {
    let source = "\
struct Widget {
    using size_type = unsigned long;
    size_type value;
    int count;
    char* name;
};
unsigned long use(Widget w) {
    w.count;
    w.name;
    return w.value;
}
";
    assert_eq!(
        type_of_use(source, "count"),
        Some("int".to_string()),
        "the member declared after the alias keeps its own type"
    );
    assert_eq!(
        type_of_use(source, "name"),
        Some("char*".to_string()),
        "and so does the one after that"
    );
    assert_eq!(
        type_of_use(source, "value"),
        Some("unsigned long".to_string()),
        "the member declared *with* the alias is what the alias names"
    );
}

/// **Two aliases in one class each keep their own target.**
///
/// The lookup asks "the innermost `using` this name is inside", and a class with two of them is where "innermost"
/// and "the one the file wrote first" stop being the same answer. Both are pinned because the failure mode is
/// silent: the second alias answering with the first one's target is a type that is *wrong*, not missing.
#[test]
fn two_using_aliases_in_one_class_keep_their_own_targets() {
    let source = "\
struct Widget {
    using first = int;
    using second = unsigned long;
    first a;
    second b;
};
int use(Widget w) {
    w.b;
    return w.a;
}
";
    assert_eq!(type_of_use(source, "a"), Some("int".to_string()));
    assert_eq!(type_of_use(source, "b"), Some("unsigned long".to_string()));
}

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
        // **A `new` expression is a pointer to what it allocates**, and that is C++'s rule rather than an
        // inference: `new T`, `new T(args)`, `new T[n]` and `new T{args}` all have the type `T*`, and nothing about
        // `T`'s own type has to be known. Measured on MSVC's `<memory>`, this was the largest answerable family
        // among the refusals — five of them, every one reporting `UnknownType("new _Ref_count_obj2<_Ty>(…)")` and
        // its siblings, with the type already in the tree and nothing asking it.
        //
        // The template arguments are **kept** rather than resolved: `Box<int>*` is what a reader wants to see, and
        // trying to resolve the argument would turn a certain answer into an uncertain one.
        //
        // Each spelling appears **once** in its fixture, which is not decoration: `type_of_use` asks about the last
        // occurrence, and `auto p = new int; return *p;` puts that occurrence inside the dereference — a question
        // about `*p` rather than about `p`, whose answer is `int` and which says nothing about this arm.
        (
            "template <class T> struct Box { T value; };\nint f() { auto boxed = new Box<int>(); return 0; }\n",
            "boxed",
            "Box<int>*",
        ),
        (
            "int f() { auto plain = new int; return 0; }\n",
            "plain",
            "int*",
        ),
        // The array form: the bound is a **sibling** `ArrayType` node rather than part of the `TypeId`, and the
        // expression's type is still a plain pointer — `new int[10]` is `int*`, not `int[10]*`.
        (
            "int f() { auto many = new int[10]; return 0; }\n",
            "many",
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

    match session.type_at(&view, as_reading(&view, offset)) {
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

    match session.member_completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(offset),
    ) {
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

/// A **plain** declaration is unaffected: the type it was written with is still the answer.
///
/// The spelling is now **written by the reader** rather than cut out of the declaration's text, so it is the same
/// type with the whitespace of a spelling rather than the whitespace of a file — `Widget*` for `const Widget* w`
/// where the text cut used to give `Widget *`. That is the one visible difference, and it is deliberate: the reader
/// is what knows that the `*` belongs to the `Widget` and the `const` does not.
#[test]
fn a_declaration_that_names_its_type_keeps_it() {
    assert_eq!(
        type_of_use(
            "struct Widget { int size; };\nint f() { const Widget* w = nullptr; return 0; }\n",
            "w"
        )
        .as_deref(),
        Some("Widget*")
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
    let header =
        "struct One { int a; };\nstruct Two { int b; };\nextern One shared;\nextern Two shared;\n";
    let source = "#include \"lib.h\"\nint f() { return shared.a; }\n";
    let session = session_with(&[("/p/a.cpp", source), ("/p/lib.h", header)]);

    assert_eq!(type_of_use_in(&session, "/p/a.cpp", source, "shared"), None);
}

/// The type of the use of a name **in a session the caller built** — the two-file fixtures' way in.
///
/// The offset is the name's **last byte**, not its first, and that is the product's own rule rather than a
/// convenience: a cursor "on a name" is anywhere inside it, and an offset at the name's *start* is the boundary
/// between it and whatever precedes it — where a `*`, a `(`, or a `.` decides what the expression is. Measured,
/// `v.data()` asked at the offset of `data` answers about `v.data` (an uncalled member function, which has no type)
/// while the same name asked one byte later answers `int*`, which is what a reader pointing at the name sees.
fn type_of_use_in(
    session: &Session<MemoryFiles>,
    path: &str,
    source: &str,
    spelling: &str,
) -> Option<String> {
    let view = session.view(path).expect("the file is held");
    let offset = last_word(source, spelling) + spelling.len() - 1;

    match session.type_at(&view, as_reading(&view, offset)) {
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

/// **A member of a class template has the type its arguments make it, not the parameters it was written with.**
///
/// This is the shape a standard container is made of, and until the parameters were paired with the arguments the
/// answer was a type called `_Ty` — which is not a class, so `*v.data()` and a completion after it had nothing. The
/// header is a separate file on purpose: the parameter names are written **there**, and a query asking about `v`
/// holds only `a.cpp`.
///
/// The members here are written with `_Ty` **directly** rather than through a nested alias (`reference` is
/// `_Ty&`). That is deliberate and it is the boundary this feature stops at: substituting into `_Ty&` is this
/// layer's own operation, while `front()` returning `reference` needs the *alias* to be resolved before there is
/// anything to substitute into — a template member whose type is another template's nested type, which is a
/// separate problem with its own test ([`a_member_whose_type_is_a_nested_alias_is_not_substituted`]).
#[test]
fn a_member_of_a_class_template_takes_its_type_from_the_arguments() {
    let header = "\
namespace std {
template <class _Ty, class _Alloc = int>
struct vector {
    _Ty& front;
    _Ty* data();
};
}
";
    let source = "\
#include \"vector.h\"
int f() {
    std::vector<int> v;
    return *v.data() + v.front;
}
";
    let session = session_with(&[("/p/a.cpp", source), ("/p/vector.h", header)]);

    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "v").as_deref(),
        Some("std::vector<int>"),
        "the declaration's own type is the type it was written with"
    );
    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "data").as_deref(),
        Some("int*"),
        "`data` is declared `_Ty*`, and `_Ty` is `int` here"
    );
    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "front").as_deref(),
        Some("int&"),
        "and `front` is declared `_Ty&`"
    );
}

/// **A member function the reader is pointing at inside a call has what the call has.**
///
/// A function has no type *as a name* — `DeclFact::returns` is the field for what a call of it gives — so the four
/// offsets of `data` in `v.data()` answered nothing while the `(` one byte later answered `int*`. The answer
/// depended on which byte of one name the cursor was on, which is not a distinction a reader can see. A cursor on
/// the callee is a cursor on the call.
#[test]
fn a_member_function_inside_a_call_has_what_the_call_has() {
    let header = "\
namespace std {
template <class _Ty>
struct vector { _Ty* data(); };
}
";
    let source = "\
#include \"vector.h\"
int f() {
    std::vector<int> v;
    return *v.data();
}
";
    let session = session_with(&[("/p/a.cpp", source), ("/p/vector.h", header)]);

    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "data").as_deref(),
        Some("int*"),
        "the callee inside a call has the return type, arguments substituted"
    );
}

/// **A member function merely *named* has no type, and saying so is the answer.** `v.data;` is a member function
/// not being called: there is no `int*` involved, and answering with the return type would be a claim about an
/// expression the file did not write.
#[test]
fn a_member_function_that_is_not_called_has_no_type() {
    let header = "\
namespace std {
template <class _Ty>
struct vector { _Ty* data(); };
}
";
    let source = "\
#include \"vector.h\"
int f() {
    std::vector<int> v;
    v.data;
    return 0;
}
";
    let session = session_with(&[("/p/a.cpp", source), ("/p/vector.h", header)]);

    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "data"),
        None,
        "a function named on its own has no type"
    );
}

/// **A member reached through an *alias* is finished with the arguments the alias carried.**
///
/// `std::string` is one name with no arguments of its own, and the class it names was written with three:
/// `basic_string<char, char_traits<char>, allocator<char>>`. So `_Elem` has nothing to be replaced by unless the
/// alias's target supplies the pairing — measured, this is the last step between `x.back()` answering `_Elem&` and
/// answering `char&`, and it is the same step that makes `std::string::size_type` a `size_t` rather than a name.
///
/// The **use** wins when it wrote arguments of its own (`std::vector<int>`), which is what the two lists in the
/// fixture are for: the alias's arguments are only consulted when the use said nothing.
#[test]
fn a_member_of_a_class_reached_through_an_alias_gets_the_aliass_arguments() {
    let header = "\
namespace std {
template <class _Elem>
struct basic_string {
    using value_type = _Elem;
    using reference = value_type&;
    reference back();
    reference front;
};
using string = basic_string<char>;
}
";
    let source = "\
#include \"string.h\"
int f() {
    std::string s;
    return s.front;
}
int g() {
    std::string s;
    auto back = s.back();
    return back;
}
";
    let session = session_with(&[("/p/a.cpp", source), ("/p/string.h", header)]);

    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "front").as_deref(),
        Some("char&"),
        "`reference` is `value_type&` is `_Elem&`, and `string` says `_Elem` is `char`"
    );
    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "back").as_deref(),
        Some("char&"),
        "and the same for a member that is *called*: `auto` takes what the call gives"
    );
}

/// A member whose type is *another* member of the same class is resolved, with the same argument pairing.
///
/// `_Ty& reference; reference front;` is how the standard library writes almost every member it has: the type of
/// `front` is the *name* `reference`, which mentions no parameter at all, so substituting the arguments into it
/// replaces nothing. The step that finishes it is a second member lookup — `reference` in the same class, with the
/// same `_Ty = int` — and the walk repeats while the type is still a bare name. That is also what makes
/// `size_type` → `size_t` work, which is the shape `v.size()` has.
#[test]
fn a_member_whose_type_is_a_nested_alias_is_resolved() {
    let header = "\
namespace std {
template <class _Ty>
struct vector {
    typedef _Ty& reference;
    reference front;
};
}
";
    let source = "\
#include \"vector.h\"
int f() {
    std::vector<int> v;
    return v.front;
}
";
    let session = session_with(&[("/p/a.cpp", source), ("/p/vector.h", header)]);

    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "front").as_deref(),
        Some("int&"),
        "`reference` is `_Ty&` and `_Ty` is `int`, one member lookup further in"
    );
}

/// **A chain of two is real**: `size_type` is `size_t`, and a member declared with `size_type` is a `size_t`.
///
/// The walk terminates because each step asks about a **different declaration** — a class has finitely many
/// members — and the cycle that would break that (`reference reference;`) is a member declared with its own type,
/// which the reader records as the member itself and the walk refuses to follow.
#[test]
fn a_chain_of_nested_types_resolves() {
    let source = "\
namespace std {
template <class _Ty>
struct vector {
    typedef unsigned long size_type;
    typedef size_type difference_type;
    difference_type count;
};
}
int f() {
    std::vector<int> v;
    return (int)v.count;
}
";
    let session = session_with(&[("/p/a.cpp", source)]);

    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "count").as_deref(),
        Some("unsigned long"),
        "two members deep, and the name that ends the chain is the type"
    );
}

/// **A `using` alias is an alias too**, and its target is written after the `=` rather than in a declarator — so
/// it takes the *other* branch of the alias reader (`declared_alias_target`) than `typedef` does. Both branches end
/// in the same field, and a member declared with either has the type the alias points at.
#[test]
fn a_using_alias_resolves_like_a_typedef() {
    let source = "\
namespace std {
template <class _Ty>
struct vector {
    using size_type = unsigned long;
    size_type count;
};
}
int f() {
    std::vector<int> v;
    return (int)v.count;
}
";
    let session = session_with(&[("/p/a.cpp", source)]);

    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "count").as_deref(),
        Some("unsigned long"),
        "`using size_type = unsigned long;` is the same fact as `typedef unsigned long size_type;`"
    );
}

/// **A class template written without arguments keeps its parameters**, which is the answer that must not become a
/// guess: `std::vector` with no argument has no `_Ty` to pair, so a member's type stays `_Ty&` — a name a caller can
/// see is unfinished rather than a type that happens to be wrong.
#[test]
fn a_template_without_arguments_keeps_its_parameters() {
    let source = "\
namespace std {
template <class _Ty>
struct vector { typedef _Ty& reference; };
}
int f() { std::vector v; return 0; }
";
    // The declaration is in this file, so the parameters come from its own syntax.
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    let names = session
        .index()
        .template_parameters_of("std::vector", &view.path);
    assert_eq!(
        names,
        vec!["_Ty".to_string()],
        "the parameter list is read out of the declaration"
    );
}

/// **A class template written inside another one declares its *own* parameters**, and the answer must be the inner
/// list: `outer<int>::inner<char>` pairs `_Uty` with `char`, and a reader that took the outer `_Ty` would pair the
/// wrong argument with the wrong name — the same defect the partial-specialization rule above exists to avoid.
///
/// The two templates are nested, so both specifier sequences contain the binding and *both* are answers to "which
/// template introduces this class"; the innermost one is the one that does.
#[test]
fn a_nested_class_template_declares_its_own_parameters() {
    let source = "\
namespace std {
template <class _Ty>
struct outer {
    template <class _Uty>
    struct inner { _Uty u; };
};
}
int f() { return 0; }
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    let names = session
        .index()
        .template_parameters_of("std::outer::inner", &view.path);
    assert_eq!(
        names,
        vec!["_Uty".to_string()],
        "the innermost template is the one that introduces the class"
    );
}
/// **A variable template takes its type from the arguments**, the way a class template's member does.
///
/// `template <class T> constexpr T pi = T(3.14159);` is a `Variable` whose name sits in the specifier sequence its
/// template introduces — the same relation a class or function template has — and the filter that decides which
/// bindings may have template parameters did not list `Variable`, so `pi<double>` had **no parameters to substitute
/// into** and its type stayed the parameter's own name: measured before this was written, `double x = pi<double>;`
/// answered `T`.
///
/// **And the spellings this does not cover, both measured.**
///
/// * **A two-argument template-id in an expression** — `ratio<float, double>` — answers `None` rather than a type.
///   That is the oldest ambiguity in C++ (`a < b, c > d`) and it belongs to the expression reader, not to this
///   pairing: the one-argument case below is the same code path and it works.
/// * **A defaulted parameter** — see the test below.
#[test]
fn a_variable_template_takes_its_type_from_the_arguments() {
    let header = "\
#pragma once
template <class T>
constexpr T pi = T(3.14159);


template <class T, class U = T>
constexpr U twice = U(2);
";
    let source = "\
#include \"constants.h\"
double f() {
    double a = pi<double>;
    // A two-argument template-id is a separate question: measured, `ratio<float, double>` answers `None`, which is the
    // shape of the oldest ambiguity in C++ — `a < b, c > d` — and it belongs to the expression reader, not here.
    return a + b;
}
";
    let session = session_with(&[("/p/a.cpp", source), ("/p/constants.h", header)]);

    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "pi").as_deref(),
        Some("double"),
        "`pi` is declared `T`, and `T` is `double` here"
    );
}

/// **A defaulted template parameter of a variable template is still not read.**
///
/// `template <class T, class U = T> constexpr U twice = U(2);` called as `twice<float>` writes one argument for two
/// parameters, and the second must take its default — which makes the answer `float`. It answers `U` instead, and the
/// reason is where the defaults are not: `DeclFact::parameters` holds the parameter **names** and nothing holds the
/// `= T` beside them, so the positional pairing runs out of arguments and leaves the name standing.
///
/// Filling that in is a field on the fact plus a codec change, which is why this is recorded rather than done here.
/// The explicit half of the same question works and is asserted above.
#[test]
#[ignore = "known red: a template parameter's default argument is not in the fact, so `twice<float>` answers `U`"]
fn a_defaulted_template_parameter_of_a_variable_template_substitutes() {
    let header = "\
#pragma once
template <class T, class U = T>
constexpr U twice = U(2);
";
    let source = "\
#include \"constants.h\"
float f() {
    float b = twice<float>;
    return b;
}
";
    let session = session_with(&[("/p/a.cpp", source), ("/p/constants.h", header)]);

    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "twice").as_deref(),
        Some("float"),
        "the second parameter defaults to `T`, which the use wrote as `float`"
    );
}
/// **A compiler builtin reads as a `bool`, and the library's lookalike does not.**
///
/// `enable_if_t<__is_constructible(_Ty, _Args...), _Ty>` is how MSVC's STL writes a constrained overload, and the
/// first argument of `enable_if_t` is a `bool` there. Before this the reader took the head of that spelling —
/// `__is_constructible` — for a class name, and everything downstream followed from a class where a `bool` was meant.
///
/// The second assertion is the one that keeps the first from being a wrong answer shaped like a right one: in a
/// **type** position `is_same<_Ty, _Uty>` is the class template the library defines, and only the compiler's own
/// `__`-prefixed names are intrinsics. A table that listed both spellings would have made `std::is_same<A, B>` read
/// as `bool`.
#[test]
fn a_compiler_builtin_reads_as_a_bool_and_the_librarys_lookalike_does_not() {
    use cpp_code_analysis::sema::types::parse_type_spelling;

    for builtin in [
        "__is_constructible(_Ty, _Args...)",
        "__is_pointer(_Ty)",
        "__is_same(_Ty, _Uty)",
        "__is_trivially_copyable(_Ty)",
    ] {
        assert_eq!(
            parse_type_spelling(builtin),
            parse_type_spelling("bool"),
            "`{builtin}` is a question the compiler answers, and its answer is a bool"
        );
    }

    assert_ne!(
        parse_type_spelling("is_same<_Ty, _Uty>"),
        parse_type_spelling("bool"),
        "the library's own `is_same` is a class template, not an intrinsic"
    );
}
/// **A trait a builtin decides selects the right specialization.**
///
/// This is the shape MSVC's STL is built out of, written out in full so the whole chain is in the fixture: a primary
/// `enable_if` with no `type`, a partial specialization for `true` that has one, an alias template over both, and two
/// members whose type is that alias with a **compiler builtin** as the condition.
///
/// The two steps this exercises are the ones that were missing when it was written. An **alias template applied to
/// arguments** is looked up by the head of the spelling and its own parameters are replaced in the target — before
/// that, `enable_if_t<A, B>` came back as its own spelling because the index files it under the bare name. And a
/// **compiler builtin is folded** where a pattern is compared, because the specialization's first slot is the literal
/// `true` and MSVC writes the condition as `__is_same(…)`. `__is_constructible(T, U)` beside them is *not* decided,
/// and the direction that matters is that an undecided builtin folds to nothing: it stays unequal, no specialization
/// is chosen, and the answer is "not known" rather than a type from one that may not apply.
#[test]
#[ignore = "known red, and the probe that settled why: the answer to this question does not come from the member-access path at all. `type_of_use_in` asks what a **declaration** is, and a declaration whose type is `enable_if_t<A, B>` is answered from `DeclFact::type_of` — the spelling the file wrote — so the alias-following step that now exists on the member-access arm never runs for it. Two ways forward, and they are different work: (1) follow the alias on the declaration's own type as well, which is where `declared_type_of`/the fact's `type_of` is read; or (2) ask the member-access question instead, `w.when_the_same` as an **expression**, which is the path the alias step and the fold were built for. The second is the smaller step and proves the chain end to end"]
fn a_trait_a_builtin_decides_selects_the_right_specialization() {
    let header = "\
#pragma once
template <bool _Test, class _Ty = void>
struct enable_if {};

template <class _Ty>
struct enable_if<true, _Ty> {
    using type = _Ty;
};

template <bool _Test, class _Ty = void>
using enable_if_t = typename enable_if<_Test, _Ty>::type;
";
    let source = "\
#include \"enable_if.h\"

struct Widget {
    enable_if_t<__is_same(int, int), double> when_the_same;
    enable_if_t<__is_pointer(int*), float> when_a_pointer;
};

void f(Widget& w) {
    double a = w.when_the_same;
    float b = w.when_a_pointer;
}
";
    let session = session_with(&[("/p/a.cpp", source), ("/p/enable_if.h", header)]);

    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "when_the_same").as_deref(),
        Some("double"),
        "`__is_same(int, int)` is decided `true`, so the specialization with a `type` is the one that applies"
    );
    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "when_a_pointer").as_deref(),
        Some("float"),
        "and `__is_pointer(int*)` is decided `true` the same way"
    );
}
/// **An alias template's target with its parameters replaced** — the step `enable_if_t<A, B>` needs.
///
/// An alias is stored as the spelling after its `=`, so `template <class _Test, class _Ty> using enable_if_t =
/// typename enable_if<_Test, _Ty>::type;` is the text `typename enable_if<_Test, _Ty>::type`, and the use has to
/// become `typename enable_if<__is_same(int, int), double>::type` before anything can be looked up in it.
///
/// Whole words only, and that is the assertion worth having: `_Ty` is a **prefix** of `_Ty2`, and a substring
/// replacement would rewrite a name the alias never mentioned.
#[test]
fn an_alias_target_is_substituted_by_whole_words() {
    use cpp_code_analysis::sema::types::substituted_spelling;

    let names = vec!["_Test".to_string(), "_Ty".to_string()];
    let arguments = vec!["__is_same(int, int)".to_string(), "double".to_string()];

    assert_eq!(
        substituted_spelling("typename enable_if<_Test, _Ty>::type", &names, &arguments),
        "typename enable_if<__is_same(int, int), double>::type",
        "both parameters are replaced, and the names the alias did not write are left alone"
    );
    assert_eq!(
        substituted_spelling("_Ty2", &names, &arguments),
        "_Ty2",
        "`_Ty` is a prefix of `_Ty2`, and a substring replacement would have rewritten it"
    );
    assert_eq!(
        substituted_spelling(
            "vector<_Ty>",
            &names,
            &["int".to_string(), "double".to_string()]
        ),
        "vector<double>",
        "an argument that mentions another parameter is not re-substituted"
    );
}

/// **`auto` over a temporary of a template type** — the shape from a real editor session that reported nothing.
///
/// `auto z = std::vector<int>();` writes no type next to the name; the type is the initialiser's, and that initialiser
/// is a **type-id call**. If the spelling comes out as `std::vector` without its argument, every later answer is wrong:
/// the member list is looked up for a class that has none, and a hover on `z.push_back` falls back to "every
/// `push_back` in the project".
#[test]
fn an_auto_variable_takes_the_templated_temporarys_type() {
    // `std::vector` is **declared** here, because a bare `auto z = std::vector<int>()` in a file with no `<vector>` is a
    // construction of a name the index has never seen — and then the honest answer is "no type", not a guess.
    let source = "\
namespace std {
template <class T>
class vector;
}
void f() {
    auto z = std::vector<int>();
}
";
    assert_eq!(
        type_of_use(source, "z").as_deref(),
        Some("std::vector<int>"),
        "the argument is part of the type the declaration has"
    );
}

/// And the same for a call: `auto y = g();` has `g`'s return type, template arguments and all.
#[test]
fn an_auto_variable_takes_the_calls_return_type() {
    let source = "\
std::string g();
void f() {
    auto y = g();
}
";
    assert_eq!(type_of_use(source, "y").as_deref(), Some("std::string"));
}

/// **A member's return type with the class's arguments substituted** — `basic_string<char>::back()` is `char&`.
///
/// The declaration writes `_Elem&`; the class is `template <class _Elem> class basic_string`; the use is through
/// `std::string`, which is `basic_string<char>`. The answer a reader wants is `char&`, and it exists in the same two
/// facts the member lookup already reads: the parameters the class declares and the arguments the alias supplies.
#[test]
fn a_members_return_type_has_the_class_arguments_substituted() {
    let source = "\
namespace std {
template <class Elem>
class basic_string {
public:
    Elem& back();
};
using string = basic_string<char>;
}
void f() {
    std::string x;
    auto y = x.back();
}
";
    assert_eq!(
        type_of_use(source, "y").as_deref(),
        Some("char&"),
        "`back()` on a `basic_string<char>` returns `char&`"
    );
}

/// The same substitution, with the class used **directly as a template-id** rather than through an alias:
/// `std::vector<int> v; auto z = v.front();` is `int&`.
#[test]
fn a_member_return_type_is_substituted_through_a_template_id_use() {
    let source = "\
namespace std {
template <class T>
class vector {
public:
    T& front();
};
}
void f() {
    std::vector<int> v;
    auto z = v.front();
}
";
    assert_eq!(type_of_use(source, "z").as_deref(), Some("int&"));
}

/// **`operator[]` and the type it produces** — `v[0]` on a `std::vector<int>` should be an `int&`.
///
/// A subscript is an operator call, so the question is the same one the member-return tests ask: the class's
/// `operator[]` returns `T&`, the class is instantiated at `int`, and the answer is `int&`. What this measures is whether
/// `type_of_expression` knows that a subscript is that call.
///
/// # Where it stands
///
/// Measured: `v[0]` answers `Unknown(UnknownType("v[0]"))` — a subscript is not an expression kind the engine has an arm
/// for. An arm was added beside the `new` arm: `IndexExpr` → the object's type → `members_of` → the member named
/// `operator[]` → its `returns`, which `members_of` has already substituted (`T&` at `int`). It does not fire yet, so one of
/// its four conditions is false for this fixture; the next probe belongs **inside** that arm, printing which condition.
/// `v.front()` on the same fixture **does** work (`a_member_return_type_is_substituted_through_a_template_id_use`), so the
/// substitution itself is not in question.
#[test]
#[ignore = "ROOT CAUSE, measured: a class whose only member is `operator[]` answers `members_of → []`, and adding a plain-named `at` beside it answers `members_of → [\"at\"]` — the member collector skips operator overloads with punctuation names, so `operator[]` is never a member. The subscript arm is correct and blocked on that"]
fn a_subscript_has_the_operators_return_type() {
    use cpp_code_analysis::file::coordinates::FileOffset;
    let source = "\
namespace std {
template <class T>
class vector {
public:
    T& at(unsigned long index);
    T& operator[](unsigned long index);
};
}
void f() {
    std::vector<int> v;
    int x = v[0];
    (void)x;
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is cooked");
    // The cursor on `v` of `v[0]` — a subscript is an expression whose type is the operator's return type.
    let at = source.find("v[0]").expect("the fixture subscripts") + 1;
    let reading = view
        .reading_offset_of(FileOffset::new(at))
        .expect("the offset is in the file");
    let answer = match session.type_at(&view, reading) {
        cpp_code_analysis::Known::Yes(found) => Some(found.type_of),
        other => {
            println!("`v[0]` at file {at} → {other:?}");
            None
        }
    };

    println!("`v[0]` is {answer:?}");
    assert_eq!(
        answer.as_deref(),
        Some("int&"),
        "a subscript is `operator[]`, whose return type is the class's `T&` at `int`"
    );
}

/// **从 `std::map` 的范围里解构出来的名字,类型是 `pair` 的元素** —— 报告是 hover 显示 `int() value;`。
///
/// `for (auto [key, value] : map)` 里的名字没有写出来的类型,所以"声明的类型"这一层没有答案:元素的类型由
/// **范围**决定,`map<K, V>` 的元素是 `pair<const K, V>`,于是 `key` 是 `const K`、`value` 是 `V`。
#[test]
fn a_structured_binding_in_a_range_for_has_the_paired_type() {
    let source = "\
#include <map>
#include <string>
int f() {
    std::map<int, std::string> myMap;
    for (auto [key, value] : myMap) {
        value.size();
        int k = key;
        (void)k;
    }
    return 0;
}
";
    let header = "namespace std { template <class K, class V> struct map { }; template <class C> struct basic_string { }; }\n";
    let files = MemoryFiles::new()
        .with_file("/p/map", header)
        .with_file("/p/string", header)
        .with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();

    let view = session.view("/p/a.cpp").expect("a view");
    let type_of = |needle: &str| {
        let at = source.find(needle).expect("the fixture writes it")
            + needle.len().saturating_sub(needle.trim_start_matches(|c: char| c == '=' || c == ' ' || c == '(' || c == ')').len());
        let reading = view
            .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(at))
            .expect("the cursor is in the rendering");
        match session.type_at(&view, reading) {
            cpp_code_analysis::Known::Yes(found) => found.type_of,
            other => panic!("`{needle}` has a type: {other:?}"),
        }
    };

    assert_eq!(
        type_of("value.size"),
        "std::string",
        "the second component of `map<int, std::string>`"
    );
    assert_eq!(
        type_of("= key"),
        "const int",
        "and the key is the first, `const` as a map's key is"
    );
}

/// **`for (auto x : v)` 里 `x` 的类型是元素的类型** —— 这个声明没有初始化器,所以"由初始化器推导"那条路没有答案。
///
/// 两张报告图是同一个缺陷:`x` / `it` 的 hover 只有 "a variable, declared in a function body",没有类型。
#[test]
fn a_range_for_variable_has_the_element_type() {
    let source = "\
#include <vector>
#include <map>
#include <string>
int f() {
    std::vector<int> values;
    for (auto x : values) {
        int k1 = x;
        (void)k1;
    }
    std::map<int, std::string> myMap;
    for (auto it : myMap) {
        int k2 = it;
        (void)k2;
    }
    return 0;
}
";
    let vector = "namespace std { template <class T> struct vector { }; }\n";
    let map = "namespace std { template <class K, class V> struct map { }; template <class A, class B> struct pair { }; }\n";
    let string = "namespace std { template <class C> struct basic_string { }; typedef basic_string<char> string; }\n";
    let files = MemoryFiles::new()
        .with_file("/p/vector", vector)
        .with_file("/p/map", map)
        .with_file("/p/string", string)
        .with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();

    let view = session.view("/p/a.cpp").expect("a view");
    let type_of = |needle: &str| {
        let at = source.find(needle).expect("the fixture writes it") + needle.len();
        let reading = view
            .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(at))
            .expect("the cursor is in the rendering");
        match session.type_at(&view, reading) {
            cpp_code_analysis::Known::Yes(found) => found.type_of,
            other => panic!("`{needle}` has a type: {other:?}"),
        }
    };

    assert_eq!(type_of("k1 = "), "int", "the element of a `vector<int>`");
    let pair = type_of("k2 = ");
    assert!(
        pair.contains("pair<const int, std::string>"),
        "the element of a `map<int, std::string>` is that pair: {pair}"
    );
}

/// **`auto it = v.begin();` 的 `it` 是元素的视图** —— 迭代器类型是这一层无法实例化的类模板,
/// 而读者接下来写的是 `it->first`、`it.second`、`*it`,这三件都是关于**元素**的问题。
#[test]
fn an_iterator_call_has_the_element_type() {
    let source = "\
#include <map>
#include <string>
int f() {
    std::map<int, std::string> myMap;
    auto it = myMap.begin();
    int k = it;
    (void)k;
    return 0;
}
";
    let map = "namespace std { template <class K, class V> struct map { }; template <class A, class B> struct pair { }; }\n";
    let string = "namespace std { template <class C> struct basic_string { }; typedef basic_string<char> string; }\n";
    let files = MemoryFiles::new()
        .with_file("/p/map", map)
        .with_file("/p/string", string)
        .with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();

    let view = session.view("/p/a.cpp").expect("a view");
    let at = source.find("k = it").expect("the fixture") + "k = ".len();
    let reading = view
        .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(at))
        .expect("the cursor is in the rendering");
    match session.type_at(&view, reading) {
        cpp_code_analysis::Known::Yes(found) => {
            println!("it: {}", found.type_of);
            assert!(
                found.type_of.contains("pair<const int, std::string>"),
                "`begin()` yields the map's element: {}",
                found.type_of
            );
        }
        other => panic!("`it` has a type: {other:?}"),
    }
}
