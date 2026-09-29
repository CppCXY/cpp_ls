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
    let header = "struct One { int a; };\nstruct Two { int b; };\nextern One shared;\nextern Two shared;\n";
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
fn type_of_use_in(session: &Session<MemoryFiles>, path: &str, source: &str, spelling: &str) -> Option<String> {
    let view = session.view(path).expect("the file is held");
    let offset = last_word(source, spelling) + spelling.len() - 1;

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
fn a_template_without_arguments_keeps_its_parameters() {    let source = "\
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
