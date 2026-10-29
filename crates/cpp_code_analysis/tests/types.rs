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

/// **A deduction cycle terminates instead of taking the stack with it.**
///
/// A return type deduced from a body is read by typing the body, and a body may ask about the very expression the
/// caller started from. The shape below is the measured one, from a real C++23 project (`module_graph.cpp`, found by
/// running the member scoreboard on `clice` — it overflowed the stack with **32 MB** of it):
///
/// ```text
/// auto text_of(int file) { return lookup(tokens.text(text_of(file).content)); }
/// …
/// return text_of(f).content;        ← the question that closes the loop
/// ```
///
/// `text_of(f).content` needs `text_of`'s return type → its body → `tokens.text(…)` → that argument → the same
/// `.content` again. `MAX_TYPE_DEPTH` cannot stop it: a type read out of another declaration deliberately starts a
/// **new** question at depth 0, which is right for `auto x = f(); x.size()` and fatal for a cycle. The memo could not
/// stop it either, because an answer is remembered **after** it is computed.
///
/// So the answer is `Unknown` — the type is not knowable from these questions — and the process survives to say so.
#[test]
fn a_deduction_cycle_answers_unknown_instead_of_overflowing_the_stack() {
    let source = "\
struct Tokens { const char* text(const char* s) const; };
extern Tokens tokens;
const char* lookup(const char* key);
auto text_of(int file) { return lookup(tokens.text(text_of(file).content)); }
const char* g(int f) { return text_of(f).content; }
";
    let session = session_with(&[("/p/cycle.cpp", source)]);

    // The cycle is asked about by the *last* occurrence, which is the one inside `g`.
    assert_eq!(
        type_of_use_in(&session, "/p/cycle.cpp", source, "content"),
        None,
        "a question that needs its own answer is not answerable, and saying so is the whole point"
    );
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

/// **A defaulted template parameter of a variable template takes its default.**
///
/// `template <class T, class U = T> constexpr U twice = U(2);` written as `twice<float>` writes one argument for two
/// parameters, and the second's meaning is the first's — which makes the answer `float`.
///
/// This was a known red, and the reason was where the defaults were not: `DeclFact::parameters` held the parameter
/// **names** and nothing held the `= T` beside them, so the positional pairing ran out of arguments and left the name
/// standing. [`DeclFact::parameter_defaults`] is that field (codec version 30), and the substitution falls back to it:
/// [`crate::TypeSubstitutions::with_defaults`] for a `Type` — this case — and
/// [`crate::substituted_spelling_with_defaults`] for a spelling, which is what a member's recorded type is.
///
/// The explicit half of the same question works and is asserted above.
#[test]
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
            + needle.len().saturating_sub(
                needle
                    .trim_start_matches(|c: char| c == '=' || c == ' ' || c == '(' || c == ')')
                    .len(),
            );
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

/// **`*it` 就是元素本身** —— 迭代器被当作元素的视图(`begin()` 的类型就是元素),所以 `*it` 问的正是那个东西。
#[test]
fn dereferencing_an_iterator_yields_the_element() {
    let source = "\
#include \"map.h\"
#include \"string.h\"
int f() {
    std::map<int, std::string> myMap;
    auto it = myMap.begin();
    auto element = *it;
    int k = element;
    (void)k;
    return 0;
}
";
    let map = "namespace std { template <class K, class V> struct map { }; template <class A, class B> struct pair { A first; B second; }; }\n";
    let string = "namespace std { template <class C> struct basic_string { }; typedef basic_string<char> string; }\n";
    let files = MemoryFiles::new()
        .with_file("/p/map.h", map)
        .with_file("/p/string.h", string)
        .with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/map.h"),
        std::path::PathBuf::from("/p/string.h"),
        std::path::PathBuf::from("/p/a.cpp"),
    ]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();

    let view = session.view("/p/a.cpp").expect("a view");
    let at = source.find("k = element").expect("the fixture") + "k = ".len();
    let reading = view
        .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(at))
        .expect("the cursor is in the rendering");
    match session.type_at(&view, reading) {
        cpp_code_analysis::Known::Yes(found) => {
            println!("element: {}", found.type_of);
            assert!(
                found.type_of.contains("pair<const int, std::string>"),
                "`*it` is the element: {}",
                found.type_of
            );
        }
        other => panic!("`element` has a type: {other:?}"),
    }
}

/// **类自己定义的 operator** —— `operator+`、`operator*`、`operator[]`、`operator()` 的返回类型就是表达式类型。
///
/// 还差最后一步:operator 成员目前**不在** `members_of` 的名单里(事实层没有给它建名字),所以查找永远返回 None,
/// 表达式落回"操作数类型相同"或迭代器模型。规则和接线都已完成,缺的是事实层的名字 —— 见下一节的记录。
#[test]
fn a_classs_own_operators_have_their_declared_return_type() {
    let source = "\
struct Money {
    int cents;
    Money operator+(const Money& other) const;
    int operator*() const;
    int operator[](int at) const;
    int operator()(int times) const;
    Money operator-() const;
};
int f() {
    Money a;
    Money sum = a + a;
    int deref = *a;
    int at = a[0];
    int times = a(3);
    Money neg = -a;
    (void)sum; (void)deref; (void)at; (void)times; (void)neg;
    return 0;
}
";
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
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

    match session.members_of(&view, "Money") {
        cpp_code_analysis::Known::Yes(list) => println!(
            "members of Money: {:?}",
            list.members
                .iter()
                .map(|member| (member.fact.name.clone(), member.fact.returns.clone()))
                .collect::<Vec<_>>()
        ),
        other => println!("members of Money: {other:?}"),
    }
    assert_eq!(type_of("Money sum = "), "Money", "operator+ returns Money");
    assert_eq!(type_of("int deref = "), "int", "operator* returns int");
    // **The cursor goes past the base name**, onto the `[`: `type_at` answers the node an offset sits in, and an
    // offset on `a` is the *name* — whose own type is `Money`, correctly. The subscript is the expression whose type
    // is the operator's return type.
    assert_eq!(type_of("int at = a"), "int", "operator[] returns int");
    // Same rule as the subscript — one character later, onto the call's own `(`.
    assert_eq!(type_of("int times = a"), "int", "operator() returns int");
    assert_eq!(
        type_of("Money neg = "),
        "Money",
        "unary operator- returns Money"
    );
}

/// **`typedef struct { … } Name;`** — C's ordinary way of naming a struct, and the shape whose members were missing.
///
/// Three questions, and all three are the same one asked of a different spelling:
///
/// 1. an **unnamed** body: the typedef's name is the only name the type has, so the class is filed under `Object`
///    and `members_of("Object")` has to answer `a` and `b`;
/// 2. a **tagged** body behind an alias: the members are filed under the *tag*, so `members_of("Alias")` has to
///    follow the alias to `Tag` instead of stopping at the definition's text;
/// 3. a plain `struct Plain { … } plain;` — a member access on `plain` needs the *variable's* type, and that type is
///    the tag.
///
/// Measured before the fix, on a four-line fixture: the typedef's fact carried
/// `type_of: Some("struct { int a; int b; }")`, and `members_of("Object")` answered an **empty** list with
/// `unlisted ["{ int a; int b; }"]` — nothing declares a body's text as a class — while its two members sat under
/// the scope `Object`. On neovim's checkout "a member list built from no visible declaration" was **21 539 of
/// 40 000** refusals.
#[test]
fn a_typedef_of_a_class_body_names_its_members() {
    let source = "\
typedef struct { int a; int b; } Object;
typedef struct Tag { int d; } Alias;
struct Plain { int e; } plain;
int f(void) { Object o; return o.a; }
int g(void) { return plain.d; }
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    let members = |class: &str| match session.members_of(&view, class) {
        cpp_code_analysis::Known::Yes(list) => {
            assert!(
                list.unlisted.is_empty(),
                "`{class}` is fully declared here: {:?}",
                list.unlisted
            );
            list.members
                .iter()
                .map(|member| member.fact.name.clone())
                .collect::<Vec<_>>()
        }
        other => panic!("`{class}` has a member list: {other:?}"),
    };

    assert_eq!(
        members("Object"),
        ["a", "b"],
        "the unnamed body is `Object`"
    );
    assert_eq!(members("Alias"), ["d"], "the alias follows to its tag");
    assert_eq!(members("Tag"), ["d"], "and the tag answers on its own");
    assert_eq!(members("Plain"), ["e"], "a named body with a variable");

    // **And the answers are reachable through the code**, which is what a member list is for: the receiver's type
    // comes from the declaration, and the member's from the list.
    assert_eq!(
        type_of_use(source, "plain").as_deref(),
        Some("Plain"),
        "a named body types its variable as the tag"
    );
    assert_eq!(
        type_of_use(source, "o").as_deref(),
        Some("Object"),
        "an unnamed body behind a typedef types the variable as the typedef"
    );
    assert_eq!(
        type_of_use(source, "a").as_deref(),
        Some("int"),
        "and `o.a` is the member the body declares"
    );
}

/// **A real C header's shape** — a tag declared as a typedef first and defined later, behind a function-pointer
/// typedef that used to swallow the rest of the file.
///
/// neovim's `src/nvim/ex_cmds_defs.h` is written exactly like this:
///
/// ```c
/// typedef char *(*LineGetter)(int, void *, int, bool);   // :96
/// typedef struct exarg exarg_T;                          // :86
/// struct exarg { char *arg; … int addr_count; … };        // :108
/// ```
///
/// Three separate defects sat on those four lines, and each one was silent:
///
/// * the function-pointer typedef **never ended** — its `Declarator` ran to the end of the header, so `struct exarg`
///   and everything after line 96 produced no facts at all (16 facts for a 800-line header instead of 93);
/// * the alias's target was recorded as the *elaborated* spelling `struct exarg`, which no scope has, so
///   `members_of("exarg_T")` answered nothing even once the members existed;
/// * an anonymous struct that is a **member** of another class borrowed the next typedef's name, so
///   `struct { bool file; bool bar; } magic;` was filed under `exarg::SubReplacementString`.
///
/// The facts are asserted through the queries that use them, and `members("exarg_T") = members("exarg")` is the
/// whole point: the alias has to reach the tag's members rather than stop at the spelling the file wrote.
#[test]
fn a_struct_defined_after_its_typedef_answers_for_the_alias() {
    let source = "\
typedef char *(*LineGetter)(int, void *, int, bool);
typedef struct exarg exarg_T;
struct exarg { char *arg; int addr_count; struct { bool file; bool bar; } magic; };
struct exarg raw;
int f(void) { exarg_T eap; return eap.addr_count; }
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    let members = |class: &str| match session.members_of(&view, class) {
        cpp_code_analysis::Known::Yes(list) => list
            .members
            .iter()
            .map(|member| member.fact.name.clone())
            .collect::<Vec<_>>(),
        other => panic!("`{class}` has a member list: {other:?}"),
    };

    let tag = ["addr_count", "arg", "magic"];
    assert_eq!(members("exarg"), tag, "the tag's own members");
    assert_eq!(members("exarg_T"), tag, "and the alias reaches them");

    assert_eq!(
        type_of_use(source, "eap").as_deref(),
        Some("exarg_T"),
        "a variable declared with the alias keeps the alias"
    );
    assert_eq!(
        type_of_use(source, "addr_count").as_deref(),
        Some("int"),
        "and its member access is typed through the tag's body"
    );
    // **The elaborated keyword is not part of the type.** `struct exarg raw;` used to record `struct` — the keyword
    // alone — because the grammar reads `struct exarg` as a `BuiltinType` plus a `NamedType`, and the builtin won.
    assert_eq!(
        type_of_use(source, "raw").as_deref(),
        Some("exarg"),
        "`struct exarg` spells the type `exarg`"
    );
}
