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

/// **`auto&&` collapses, and the collapse is what decides the reference.**
///
/// The old rule refused a forwarding reference outright, on the argument that the language deduces `T&` or `T&&` from
/// the initializer's *value category*, which a declaration's spelling does not record. That argument is right about
/// the category and wrong about the answer: this layer reads an **lvalue** initializer as an lvalue, so `auto&& bound
/// = base;` is `int&` and `auto&& bound = 42;` is `int&&` — the same rule the language applies, over the one category
/// this engine can see.
///
/// The refusal was also load-bearing in the wrong direction: `auto& vfs = _server->GetVFS();` (a `T&` initializer)
/// went through the same code and produced **`T&&`**, and six hover cards on a real project were wrong because of it.
#[test]
fn a_forwarding_reference_collapses() {
    // A prvalue initializer: `int`, so `auto&&` is `int&&`.
    let from_a_value = "int f() { auto&& bound = 42; return bound; }\n";
    assert_eq!(type_of_use(from_a_value, "bound").as_deref(), Some("int&&"));
    // **A reference-returning call collapses**, which is the half that was breaking real code: the deduced type
    // already says `Widget&`, so `auto&&` is `Widget&` and not `Widget&&&`.
    let from_a_reference = "\
struct Widget { int size; };
Widget &the_widget();
int f() { auto&& bound = the_widget(); return bound.size; }
";
    assert_eq!(
        type_of_use(from_a_reference, "bound").as_deref(),
        Some("Widget&")
    );
    // **A named initializer is an lvalue**, so `auto&&` on it is `int&` — the collapse C++ performs, decided from the
    // initializer's *shape* (a name is an lvalue; a literal or a call is not) because its type is `int` for both.
    let from_a_name = "int f() { int base = 1; auto&& bound = base; return bound; }\n";
    assert_eq!(type_of_use(from_a_name, "bound").as_deref(), Some("int&"));
}

/// **`auto&` on a reference-returning call is one reference, not two.**
///
/// `auto &vfs = _server->GetVFS();` where `GetVFS()` returns `VirtualFileSystem&` deduced **`VirtualFileSystem&&`**:
/// the initializer's reference was kept *and* the declarator's was applied. Everything downstream then failed, because
/// `&&` names no class — measured on a real project, `vfs.GetVirtualFile(...)` answered
/// `Unknown(NotDeclaredHere("VirtualFileSystem::GetVirtualFile"))`.
#[test]
fn auto_reference_on_a_reference_returns_one_reference() {
    let source = "\
struct Widget { int size; };
Widget &the_widget();
int f() {
    auto &widget = the_widget();
    return widget.size;
}
";
    assert_eq!(
        type_of_use(source, "widget").as_deref(),
        Some("Widget&"),
        "a reference in the initializer never survives into `auto`"
    );
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

/// **A member of a class declared in a header has its type in the method's body.**
///
/// Reported from `EmmyLuaCodeStyle`: `LuaParser::_tokens` is `std::vector<LuaToken>` — its own hover says so, straight
/// out of the index's member list — and yet `_tokens.size()` in `LuaParser.cpp` answered *"the type of `_tokens` is
/// not known here"*, so `size` could not be resolved at all.
///
/// The reason is the two-layer split: the file's scope tree has no scope for a class declared in another file, and
/// the index's unqualified lookup does not match a fact whose scope is `Parser`. The class is named by the
/// *definition's* own declarator (`Parser::Parse`), and the query that knows a member of it is the one the hover uses.
#[test]
fn a_member_of_a_class_declared_in_a_header_has_its_type_in_the_body() {
    let header = "\
struct Token { int kind; };
struct Parser {
    Token _token;
    int Parse();
};
";
    let source = "\
#include \"api.h\"
int Parser::Parse() { return _token.kind; }
";
    let session = session_with(&[("/p/a.cpp", source), ("/p/api.h", header)]);
    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "_token").as_deref(),
        Some("Token"),
    );
    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "kind").as_deref(),
        Some("int"),
        "and the member access through it reads the member's own type"
    );
}

/// **The member is the class's, even when three classes declare that name.**
///
/// This is the real shape `EmmyLuaCodeStyle` has, and the one the first fixture above could not fail on: `_tokens`
/// is a member of `LuaLexer`, `LuaParser` **and** `LuaSyntaxTree`, so the index's unqualified lookup answers
/// `Ambiguous` — and the type engine had no way to ask "the class whose member function this is", which is what C++
/// looks in first. Measured on `LuaParser.cpp`: `type_at("_tokens")` answered `Unknown(Ambiguous)`, so
/// `_tokens.size()` could not resolve `size` at all.
#[test]
fn a_member_name_several_classes_declare_is_the_enclosing_class() {
    let header = "\
struct Token { int kind; };
struct Lexer {
    Token _token;
};
struct Parser {
    Token _token;
    int Parse();
};
";
    let source = "\
#include \"api.h\"
int Parser::Parse() { return _token.kind; }
";
    let session = session_with(&[("/p/a.cpp", source), ("/p/api.h", header)]);
    eprintln!(
        "definition(_token) = {:?}",
        session
            .index()
            .definition("_token", std::path::Path::new("/p/a.cpp"))
    );
    assert_eq!(
        type_of_use_in(&session, "/p/a.cpp", source, "_token").as_deref(),
        Some("Token"),
        "the enclosing class's member, not an ambiguous name"
    );
}

/// **A union's members are reachable through the union, and the union has no spelling of its own.**
///
/// Reported from `EmmyLuaCodeStyle`, `Mark.h`:
///
/// ```cpp
/// struct MarkEvent {
///     MarkEventType Type;
///     union {
///         struct { std::size_t Parent; LuaSyntaxNodeKind Kind; } Start;
///         struct { std::size_t Index; LuaTokenKind Kind; } Token;
///         struct { LuaTokenKind TokenKind; LuaParserErrorKind ErrorKind; } Error;
///     } U;
/// };
/// ```
///
/// `e.U.Start.Kind` in `LuaSyntaxTree.cpp` answered *"the type of `e.U` is not known here"*: `U`'s type is an unnamed
/// union, so the fact recorded no `type_of` at all, and the chain stopped at `U`. The type's name is the one the
/// parser already gives the anonymous type's **members** — `MarkEvent::U` — and that is what this asserts, member by
/// member down the chain.
#[test]
fn a_member_of_an_unnamed_union_has_a_type_and_its_members_are_reachable() {
    let source = "\
struct MarkEvent {
    int Type;
    union {
        struct { int Parent; int Kind; } Start;
        struct { int Index; } Token;
    } U;
};
int f(MarkEvent e) { return e.U.Start.Kind; }
";
    assert_eq!(type_of_use(source, "U").as_deref(), Some("MarkEvent::U"));
    assert_eq!(
        type_of_use(source, "Start").as_deref(),
        Some("MarkEvent::U::Start")
    );
    assert_eq!(
        type_of_use(source, "Kind").as_deref(),
        Some("int"),
        "the chain ends at the real member's type"
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
    auto& back = s.back();
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
        // The declarator writes the reference, so the substitution's `&` survives it — `auto back = …` would deduce
        // the *value* `char`, which is the language's rule and not this test's question.
        "and the same for a member that is *called*, when the declarator asks for a reference"
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
fn a_compiler_builtin_reads_as_a_bool_and_the_library_lookalike_does_not() {
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
    auto& y = x.back();
}
";
    assert_eq!(
        type_of_use(source, "y").as_deref(),
        Some("char&"),
        "`back()` on a `basic_string<char>` returns `char&`, and the declarator asked for a reference"
    );
}

/// The same substitution, with the class used **directly as a template-id** rather than through an alias:
/// `std::vector<int> v; auto& z = v.front();` is `int&`.
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
    auto& z = v.front();
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

/// **A subscript whose operator returns the class's own `reference`** — the shape every container writes, and the one
/// the fact used to refuse.
///
/// `reference operator[](size_type)` is how `std::string`, `std::span` and every hand-written container declare it, and
/// `reference` is a typedef of the class. A fact is read by consumers that have no class, so
/// [`cpp_code_analysis::DeclFact::returns`] is `None` for that declaration — 60 false positives of `undefined-field`
/// taught that rule. The subscript is the one consumer that *does* have the class, so the spelling is kept beside it
/// ([`cpp_code_analysis::DeclFact::returns_in_the_class`]) and resolved there: `reference` is `_CharT&`.
///
/// Measured before: `s[0]` answered `Unknown(UnknownType("s[0]"))`.
#[test]
fn a_subscript_whose_operator_returns_the_classs_own_reference() {
    use cpp_code_analysis::file::coordinates::FileOffset;
    let source = "\
template <class _CharT>
class basic_string {
public:
    typedef _CharT& reference;
    reference operator[](unsigned long index);
};
void f() {
    basic_string<char> s;
    char c = s[0];
    (void)c;
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is cooked");
    let at = source.find("s[0]").expect("the fixture subscripts") + 1;
    let reading = view
        .reading_offset_of(FileOffset::new(at))
        .expect("the offset is in the file");

    let answer = match session.type_at(&view, reading) {
        cpp_code_analysis::Known::Yes(found) => Some(found.type_of),
        other => {
            println!("`s[0]` at file {at} → {other:?}");
            None
        }
    };
    assert_eq!(
        answer.as_deref(),
        Some("char&"),
        "`reference` is the class's own alias for `_CharT&`, and `_CharT` is `char`"
    );
}

/// **A member of an unnamed union, and of an unnamed struct inside it** — the shape `MarkEvent` is written in, and the
/// report behind it ("我们依然无法正确的推断出 Union 的后续").
///
/// ```cpp
/// struct MarkEvent {
///     union {
///         struct { std::size_t Parent; LuaSyntaxNodeKind Kind; } Start;
///         …
///     } U;
/// };
/// event.U.Start.Kind = …;
/// ```
///
/// The chain is three questions and every one of them is answerable: `U`'s type is the unnamed union — named by
/// [`cpp_code_analysis::sema::declarations::an_unnamed_class_body_named`] after the scope it is written in
/// (`MarkEvent::U`) — `Start`'s type is the unnamed struct inside it (`MarkEvent::U::Start`), and its members are
/// `Parent` and `Kind`. Measured: all four answers are right, so **what fails on the report is not this layer**: there
/// the object is `events[Pos]` on a `std::vector<MarkEvent>`, and that is the subscript (`operator_returns_of`), whose
/// note records the alias chain a `std::vector` still needs.
#[test]
fn a_member_of_an_unnamed_union_and_of_an_unnamed_struct_inside_it() {
    use cpp_code_analysis::file::coordinates::FileOffset;
    const SOURCE: &str = "\
enum class Kind { None, Start };
struct MarkEvent {
    int Type;
    union {
        struct { unsigned long Parent; Kind KindOf; } Start;
        struct { unsigned long Index; Kind Token; } Token;
    } U;
};
void f(MarkEvent &event) {
    event.U.Start.KindOf = Kind::None;
    unsigned long p = event.U.Start.Parent;
}
";
    let session = session_with(&[("/p/a.cpp", SOURCE)]);
    let view = session.view("/p/a.cpp").expect("cooked");
    let answered = |needle: &str| {
        let at = SOURCE.find(needle).expect("the fixture writes it") + needle.len() - 1;
        let reading = view
            .reading_offset_of(FileOffset::new(at))
            .expect("the offset is in the file");
        match session.type_at(&view, reading) {
            cpp_code_analysis::Known::Yes(found) => Some(found.type_of),
            cpp_code_analysis::Known::No => None,
            cpp_code_analysis::Known::Unknown(reason) => {
                println!("`{needle}` → unknown({reason:?})");
                None
            }
        }
    };

    assert_eq!(
        answered("event.U").as_deref(),
        Some("MarkEvent::U"),
        "the union is named after the scope it is written in"
    );
    assert_eq!(
        answered("event.U.Start").as_deref(),
        Some("MarkEvent::U::Start"),
        "and so is the unnamed struct inside it"
    );
    assert_eq!(
        answered("event.U.Start.KindOf").as_deref(),
        Some("Kind"),
        "a member of the unnamed struct is a member like any other"
    );
    assert_eq!(
        answered("event.U.Start.Parent").as_deref(),
        Some("unsigned long")
    );
}

/// **The standard library's allocator chain** — `std::vector<MarkEvent>::at()` is `MarkEvent&`, and `v[i]` too.
///
/// libstdc++ writes the element access type as an alias of an alias of an alias, and every level of it is a name that
/// only one class can resolve:
///
/// ```text
/// vector<T,A>::reference          _Alloc_traits::reference
///   _Alloc_traits                 __gnu_cxx::__alloc_traits<_Tp_alloc_type>     `_Tp_alloc_type` is vector's own
///     __alloc_traits::reference   value_type&
///       value_type                _Base_type::value_type
///         _Base_type              std::allocator_traits<_Tp_alloc_type>
///           value_type            _Alloc::value_type
///             _Alloc              std::allocator<T>            ← through `rebind<T>::other`
/// ```
///
/// The fixture below is that shape with the names changed (no standard library, 0.02 s): a nested class template
/// `rebind<U>::other`, an alias template `rebind_alloc`, an inheritance, and a class that re-exports its base's alias.
///
/// Reported from `EmmyLuaCodeStyle`'s `Mark.cpp`: `auto &event = p.GetEvents().at(Start);` answered
/// `__gnu_cxx::__alloc_traits<std::allocator<MarkEvent>>::template rebind<MarkEvent>::other&`, and `events[Pos]`
/// answered `_Tp_alloc_type&` — a spelling no reader can use, and every member behind it unknown.
#[test]
fn a_containers_element_access_through_the_allocator_chain() {
    use cpp_code_analysis::file::coordinates::FileOffset;
    const SOURCE: &str = "\
namespace mine {
template <typename Tp>
class allocator {
public:
    typedef Tp value_type;
    template <typename Tp1>
    struct rebind { typedef allocator<Tp1> other; };
};
template <typename Alloc>
struct allocator_traits {
    typedef typename Alloc::value_type value_type;
    template <typename Up>
    using rebind_alloc = typename Alloc::template rebind<Up>::other;
};
}
namespace mygnu {
template <typename Alloc>
struct alloc_traits : mine::allocator_traits<Alloc> {
    typedef mine::allocator_traits<Alloc> base_type;
    typedef typename base_type::value_type value_type;
    typedef value_type& reference;
    template <typename Tp>
    struct rebind { typedef typename base_type::template rebind_alloc<Tp> other; };
};
template <typename Tp, typename Alloc>
struct vector_base {
    typedef typename mygnu::alloc_traits<Alloc>::template rebind<Tp>::other tp_alloc_type;
};
template <typename Tp, typename Alloc>
class vector : protected vector_base<Tp, Alloc> {
    typedef vector_base<Tp, Alloc> base;
    typedef typename base::tp_alloc_type tp_alloc_type;
    typedef mygnu::alloc_traits<tp_alloc_type> alloc_traits_type;
public:
    typedef typename alloc_traits_type::reference reference;
    reference at(int i);
    reference operator[](int i);
};
}
void f() {
    mygnu::vector<int, mine::allocator<int>> v;
    int x = v[0];
    int y = v.at(1);
    (void)x;
    (void)y;
}
";
    let session = session_with(&[("/p/a.cpp", SOURCE)]);
    let view = session.view("/p/a.cpp").expect("cooked");
    let answered = |needle: &str| {
        let at = SOURCE.find(needle).expect("the fixture writes it") + 2;
        let reading = view
            .reading_offset_of(FileOffset::new(at))
            .expect("the offset is in the file");
        match session.type_at(&view, reading) {
            cpp_code_analysis::Known::Yes(found) => Some(found.type_of),
            cpp_code_analysis::Known::No => None,
            cpp_code_analysis::Known::Unknown(reason) => {
                println!("`{needle}` → unknown({reason:?})");
                None
            }
        }
    };

    assert_eq!(
        answered("v[0]").as_deref(),
        Some("int"),
        "a subscript on `vector<int, allocator<int>>` is `operator[]`'s `reference`, and `reference` is `int&` — \
         answered here as its decayed value, which is what a cursor on `v[0]` is asking about"
    );
    assert_eq!(
        answered("v.at(1)").as_deref(),
        Some("int&"),
        "and a call of `at` is the same alias chain, answered as the reference it is"
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
            + needle
                .len()
                .saturating_sub(needle.trim_start_matches(['=', ' ', '(', ')']).len());
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
fn a_class_own_operators_have_their_declared_return_type() {
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

/// **An anonymous union declared in a *header*, asked about from a file that includes it** — the corpus's shape.
///
/// `struct line` is in `corpus.h` with `union { long integer; double real; };` inside it, and the use is
/// `line.integer` in `types.c`. The one-file fixture passes; this is the two-file one, where the members come from the
/// header's facts rather than from the queried file's scope tree.
#[test]
fn an_anonymous_unions_members_are_facts_of_the_struct_it_is_declared_in() {
    let header = "\
struct line {
    char colour;
    union {
        long integer;
        double real;
    }; /* anonymous */
};
";
    let source = "#include \"corpus.h\"\nint f(struct line *line) { return (int)line->integer; }\n";
    let session = session_with(&[("/p/corpus.h", header), ("/p/a.c", source)]);
    let view = session.view("/p/a.c").expect("the file is held");

    let members = match session.members_of(&view, "line") {
        cpp_code_analysis::Known::Yes(list) => list
            .members
            .iter()
            .map(|member| member.fact.name.clone())
            .collect::<Vec<_>>(),
        other => panic!("`line` has a member list: {other:?}"),
    };
    assert!(
        members.iter().any(|name| name == "integer"),
        "the union is anonymous, so `integer` is `line`'s member: {members:?}"
    );
}

/// **A variable whose type is an unnamed struct declared inline**, and its members.
///
/// ```c
/// struct { unsigned three : 3; } bits = {5};
/// return (int)bits.three;
/// ```
///
/// The type has no spelling, so the only key the members can have is the declarator's name — measured on a nine-file C
/// corpus, `bits.three` and `bits.signed_wide` (and `anonymous_union.integer`) answered nothing.
#[test]
fn an_unnamed_struct_type_is_keyed_by_the_variable_it_declares() {
    let source = "int f(void) { struct { unsigned three : 3; unsigned four : 3; } bits = {5, 1}; return (int)bits.three; }\n";
    let session = session_with(&[("/p/a.c", source)]);
    let view = session.view("/p/a.c").expect("the file is held");

    // **The *query* is what this test asserts, and the fact deliberately stays empty.** A file-scope
    // `static const struct { … } priority[]` is hovered as the declaration's own text, and naming that type made the
    // hover read `priority[] priority;` — so the key exists where a member question needs it (the query path, for a
    // local) and nowhere a reader reads a type.
    assert_eq!(
        type_of_use(source, "bits").as_deref(),
        Some("bits"),
        "the query path keys the type by the variable's own name"
    );
    let members = match session.members_of(&view, "bits") {
        cpp_code_analysis::Known::Yes(list) => list
            .members
            .iter()
            .map(|member| member.fact.name.clone())
            .collect::<Vec<_>>(),
        other => panic!("`bits` has a member list: {other:?}"),
    };
    assert!(
        members.iter().any(|name| name == "three"),
        "and its members are filed under that name: {members:?}"
    );
}

/// **`__auto_type` is GNU's `auto`**, and its type is its initialiser's.
///
/// Measured on a nine-file C corpus: `__auto_type value = 41;` had a *fact* typed `int` (the fact builder deducts) while
/// `type_at(value)` answered `__auto_type` — the query path reads the syntax and the placeholder check did not know the
/// GNU spelling.
#[test]
fn gnu_auto_type_is_deduced_from_its_initializer() {
    let source = "int f(void) { __auto_type value = 41; return value; }\n";
    assert_eq!(
        type_of_use(source, "value").as_deref(),
        Some("int"),
        "the placeholder's type is the initialiser's"
    );
}
/// **A variable whose type is a function-pointer typedef** — `binary_op operation = add;`.
///
/// `binary_op` is `typedef int (*binary_op)(int, int);`, and the declaration's type is that alias's name — the
/// spelling the file wrote. Measured on a nine-file C corpus: `operation` and `compare` answered `<no type>`, so
/// `operation(1, 2)` could not be resolved as a call of a known signature.
#[test]
fn a_function_pointer_typedef_names_the_variable_it_types() {
    let source = "\
typedef int (*binary_op)(int left, int right);
static int add(int left, int right) { return left + right; }
int f(void) { binary_op operation = add; return operation(1, 2); }
";
    assert_eq!(
        type_of_use(source, "operation").as_deref(),
        Some("binary_op"),
        "the alias's name is the type the file wrote"
    );
}

/// **A function-pointer typedef declared in a *header*** — the corpus's shape, and the one the one-file fixture misses.
///
/// `corpus.h` writes `typedef int (*binary_op)(int left, int right);` and `functions.c` uses it as
/// `binary_op operation = add;`. Measured on a nine-file C corpus: `operation` and `compare` answered `<no type>`,
/// while every other local in every other file answered. The one-file fixture in `types.rs` passes, so what is being
/// measured here is the **cross-file** spelling: a binding whose type names a typedef the file did not write.
#[test]
fn a_function_pointer_typedef_from_a_header_types_its_variable() {
    let header = "\
typedef int (*binary_op)(int left, int right);
typedef int comparer(const void *left, const void *right);
";
    let source = "\
#include \"corpus.h\"
#include <stddef.h>
#include <stdio.h>
static int add(int left, int right) { return left + right; }
int f(void) { binary_op operation = add; comparer *compare = 0; return operation(1, 2) + (compare != 0); }
";
    let session = session_with(&[("/p/corpus.h", header), ("/p/a.c", source)]);
    let view = session.view("/p/a.c").expect("the file is held");

    let type_at = |needle: &str| {
        let offset = source.find(needle).expect("the needle is written") + 1;
        let reading = view
            .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(
                offset,
            ))
            .expect("the cursor is in the rendering");
        match session.type_at(&view, reading) {
            cpp_code_analysis::Known::Yes(found) => Some(found.type_of),
            _ => None,
        }
    };

    // **The engine expands the alias**, and that is the better answer — `int (*)(int, int)` is the type, while
    // `binary_op` is its name. This fixture therefore pins the *expansion*; what the corpus answers for the same shape
    // (nothing at all) is still open.
    let operation = type_at("binary_op operation").expect("the variable has a type");
    assert!(
        operation.contains("int") && operation.contains("(int, int)"),
        "a function-pointer typedef expands to the type it names: {operation}"
    );
    let compare = type_at("comparer *compare").expect("the pointer has a type");
    assert!(
        compare.contains("int") && compare.contains("void"),
        "and so does the other typedef: {compare}"
    );
}

/// **A member whose specifier starts with `_BitInt(N)` is not recorded**, and one that starts with `unsigned` is.
///
/// `struct bits { unsigned _BitInt(3) three : 3; _BitInt(13) signed_wide : 13; };` — the corpus's own shape, and the
/// measurement is the *summary*, because the facts are what a member list is built from.
#[test]
fn both_bitfields_of_a_bitint_struct_are_recorded() {
    let source = "\
struct bits {
    unsigned _BitInt(3) three : 3;
    _BitInt(13) signed_wide : 13;
    _BitInt(5) plain;
    unsigned _BitInt(4) other : 2;
};
";
    let session = session_with(&[("/p/a.c", source)]);
    let summary = session
        .parsed_summary(std::path::Path::new("/p/a.c"))
        .expect("the file has a summary");
    let names: Vec<&str> = summary
        .declarations
        .iter()
        .map(|fact| fact.name.as_str())
        .collect();
    eprintln!("facts: {names:?}");
    assert!(
        names.contains(&"three") && names.contains(&"signed_wide") && names.contains(&"plain"),
        "both bit-fields and the plain member are members: {names:?}"
    );
}

/// **`__STDC_VERSION__` is a macro every C compiler defines**, and its value follows from the standard the file is read
/// as — not from a toolchain that may not exist.
#[test]
fn the_standards_own_macros_answer_from_the_language() {
    let session = session_with(&[("/p/a.c", "int f(void) { return 0; }\n")]);
    assert_eq!(
        session.predefined_macro("__STDC_VERSION__", std::path::Path::new("/p/a.c")),
        Some("202311L".to_string()),
        "a permissive C reading takes the latest standard"
    );
    assert_eq!(
        session.predefined_macro("__STDC__", std::path::Path::new("/p/a.c")),
        Some("1".to_string()),
    );
    assert_eq!(
        session.predefined_macro("__GNUC__", std::path::Path::new("/p/a.c")),
        None,
        "a compiler's own macro is not the standard's, and no compiler was asked here"
    );
}

/// **A class template's members, through an instantiation** — `Box<int> box; box.get()`.
///
/// The template's body is ordinary C++: `get` and `set` are members of `Box`, and an instantiation has them. Measured on
/// a C++20 project: `techniques::Box<int> box(7); box.` answered **0 items** — not a wrong list, an empty one — which is
/// what a reader sees as "completion does not work on my template class".
#[test]
fn a_class_templates_members_answer_through_its_instantiation() {
    let source = "\
template <typename T>
class Box {
public:
    explicit Box(T value) : value_(value) {}

    T get() const { return value_; }
    void set(T value) { value_ = value; }

private:
    T value_;
};

int f() {
    Box<int> box(7);
    return box.get();
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    let names = |class: &str| match session.members_of(&view, class) {
        cpp_code_analysis::Known::Yes(list) => list
            .members
            .iter()
            .map(|member| member.fact.name.clone())
            .collect::<Vec<_>>(),
        other => panic!("`{class}` has a member list: {other:?}"),
    };

    assert!(
        names("Box<int>").contains(&"get".to_string()),
        "the instantiation has the template's members: {:?}",
        names("Box<int>")
    );
    assert!(
        names("Box").contains(&"get".to_string()),
        "and so does the template's own name: {:?}",
        names("Box")
    );
}

/// **Class template argument deduction** — `Box box(7);` where `Box` is a class template.
///
/// The implicit guide is the constructor's parameter list: `Box(T value)` makes `Box box(7)` a `Box<int>`, and that is
/// ordinary C++17. Measured on a real C++ project (`techniques.h` + `techniques.cpp`): the declaration's type came back
/// as the **template's name without its argument** — `techniques::Box` — while `Box<int> box(7)` was right, so the
/// deduction is what is missing rather than the template.
#[test]
fn a_class_template_argument_is_deduced_from_the_initializer() {
    let source = "\
template <typename T>
class Box {
public:
    explicit Box(T value) : value_(value) {}

    T get() const { return value_; }

private:
    T value_;
};

int f() {
    Box box(7);
    return box.get();
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let offset = last_word(source, "box") + 2;
    let found = match session.type_at(&view, as_reading(&view, offset)) {
        cpp_code_analysis::Known::Yes(found) => found.type_of,
        other => panic!("`box` has a type: {other:?}"),
    };
    assert!(
        found.contains("Box<int>"),
        "the argument is deduced from the initializer: {found}"
    );

    // **And the members follow the deduced type**, which is the question a reader actually asks of the name.
    let names = match session.members_of(&view, "Box<int>") {
        cpp_code_analysis::Known::Yes(list) => list
            .members
            .iter()
            .map(|member| member.fact.name.clone())
            .collect::<Vec<_>>(),
        other => panic!("`Box<int>` has a member list: {other:?}"),
    };
    assert!(
        names.contains(&"get".to_string()),
        "the deduced instantiation has the template's members: {names:?}"
    );
}

/// The same deduction through a **namespace**, which is how a header's template is reached: `techniques::Box box(7)`.
#[test]
fn a_class_template_argument_is_deduced_through_a_namespace() {
    let source = "\
namespace techniques {
template <typename T>
class Box {
public:
    explicit Box(T value) : value_(value) {}
    T get() const { return value_; }
private:
    T value_;
};
}  // namespace techniques

int f() {
    techniques::Box box(7);
    return box.get();
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let offset = last_word(source, "box") + 2;
    let found = match session.type_at(&view, as_reading(&view, offset)) {
        cpp_code_analysis::Known::Yes(found) => found.type_of,
        other => panic!("`box` has a type: {other:?}"),
    };
    assert!(
        found.contains("Box<int>"),
        "the argument is deduced: {found}"
    );
}

/// The same deduction when the class is in a **header** — the shape the project actually has, and the one that still
/// answers the bare template name there.
#[test]
fn a_class_template_argument_is_deduced_across_a_header() {
    let header = "\
#pragma once
namespace techniques {
template <typename T>
class Box {
public:
    explicit Box(T value) : value_(value) {}
    T get() const { return value_; }
private:
    T value_;
};
}  // namespace techniques
";
    let source = "\
#include \"techniques.h\"

int f() {
    techniques::Box box(7);
    return box.get();
}
";
    let session = session_with(&[("/p/techniques.h", header), ("/p/a.cpp", source)]);
    if let Some(summary) = session.parsed_summary(std::path::Path::new("/p/techniques.h")) {
        for fact in &summary.declarations {
            eprintln!(
                "[fact] name={:?} scope={:?} kind={:?} params={:?}",
                fact.name, fact.scope, fact.kind, fact.parameter_list
            );
        }
    }
    let view = session.view("/p/a.cpp").expect("the file is held");
    let offset = last_word(source, "box") + 2;
    let found = match session.type_at(&view, as_reading(&view, offset)) {
        cpp_code_analysis::Known::Yes(found) => found.type_of,
        other => panic!("`box` has a type: {other:?}"),
    };
    assert!(
        found.contains("Box<int>"),
        "the argument is deduced: {found}"
    );
}

/// **A hand-written deduction guide**, which is the language's own spelling of the rule — and the only way to deduce a
/// class whose constructor does not take the template parameter.
///
/// ```cpp
/// template <typename T> struct Wrap { Wrap(std::string); };
/// template <typename T> Wrap(T) -> Wrap<T>;
/// Wrap w(7);        // Wrap<int>, and nothing about Wrap's constructor says so
/// ```
///
/// Measured: the guide is read by the grammar (the file parses) and was not read by the deduction, so `w` answered the
/// bare template name.
#[test]
fn a_hand_written_deduction_guide_is_read() {
    let source = "\
#include <string>

template <typename T>
struct Wrap {
    Wrap(std::string);
    T get() const;
};

template <typename T>
Wrap(T) -> Wrap<T>;

int f() {
    Wrap wrapped(7);
    return wrapped.get();
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let offset = last_word(source, "wrapped") + 2;
    let found = match session.type_at(&view, as_reading(&view, offset)) {
        cpp_code_analysis::Known::Yes(found) => found.type_of,
        other => panic!("`w` has a type: {other:?}"),
    };
    assert!(found.contains("Wrap<int>"), "the guide deduces it: {found}");
}
/// **Which overload `describe(1.5)` calls**, asked the way go-to-definition asks it.
///
/// Two declarations differ only in their `enable_if` condition, so the *only* thing that picks between them is whether
/// `std::is_floating_point<double>::value` is true — and `sema::constraint` decides exactly that
/// (`the_constraint_evaluator_decides_the_fundamental_traits`). What is missing is the **set** to decide it on.
///
/// Measured: the answer is the first declaration (the `is_integral` one) for either call, because two declarations of one
/// name in one scope reach the answer as **one** candidate. The chain is:
///
/// ```text
/// the scope tree     binds template functions … one binding for both
/// definitions_at     "every declaration in the scope that answered" — a set of one
/// the index          holds both facts, and folds them into one (`one_entity_declared_twice`, which is about a header's
///                    declaration and the `.cpp`'s definition, not about two same-file overloads)
/// ```
///
/// Ignored rather than deleted: the assertion is the goal, and the day an overload set exists this is the test that says
/// so.
#[test]
fn an_enable_if_overload_is_chosen_by_its_condition() {
    let source = "\
#include <type_traits>
#include <string>

template <typename T>
typename std::enable_if<std::is_integral<T>::value, std::string>::type describe(T value) {
    return \"integral\";
}

template <typename T>
typename std::enable_if<std::is_floating_point<T>::value, std::string>::type describe(T value) {
    return \"floating\";
}

std::string f() {
    describe(41);
    return describe(1.5);
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    // **The rendering's text**, because a fact's range is a range there — the include closure is inlined into it.
    let rendering = view.root.text().to_string();
    let declaration_at = |which: usize| -> usize {
        rendering
            .match_indices("describe(T value)")
            .nth(which)
            .map(|(at, _)| at)
            .expect("both declarations are in the rendering")
    };

    // **`describe(1.5)` picks the floating-point overload**: its condition is `is_floating_point<T>` with `T = double`.
    let floating_call = rendering
        .find("describe(1.5)")
        .expect("the call is written");
    let one = match session.definition(
        &view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(floating_call + 3),
    ) {
        cpp_code_analysis::Known::Yes(found) => found,
        other => panic!("`describe(1.5)` resolves: {other:?}"),
    };
    assert_eq!(
        one.fact.name_range.start_offset,
        declaration_at(1),
        "the condition picks the floating-point overload"
    );

    // **And the other direction**, because a narrowing that answered the last candidate for every call would pass the
    // assertion above and be just as wrong.
    let integral_call = rendering.find("describe(41)").expect("the call is written");
    let integral = match session.definition(
        &view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(integral_call + 3),
    ) {
        cpp_code_analysis::Known::Yes(found) => found,
        other => panic!("`describe(41)` resolves: {other:?}"),
    };
    assert_eq!(
        integral.fact.name_range.start_offset,
        declaration_at(0),
        "the condition picks the integral overload"
    );
}

/// **The condition evaluator**, decision by decision — the module the `enable_if` overloads rest on.
#[test]
fn the_constraint_evaluator_decides_the_fundamental_traits() {
    use cpp_code_analysis::sema::constraint::{satisfies, the_condition_of};

    let bind = |spelling: &str| vec![("T".to_string(), spelling.to_string())];

    // The two declarations of `describe`, verbatim.
    let integral = the_condition_of(
        "typename std::enable_if<std::is_integral<T>::value, std::string>::type describe(T value)",
    )
    .expect("the declaration carries a condition");
    assert_eq!(integral, "std::is_integral<T>::value");
    assert_eq!(satisfies(integral, &bind("int")), Some(true));
    assert_eq!(satisfies(integral, &bind("double")), Some(false));
    assert_eq!(
        satisfies(integral, &bind("std::string")),
        Some(false),
        "a class is not integral"
    );

    let floating = the_condition_of(
        "typename std::enable_if<std::is_floating_point<T>::value, int>::type describe(T value)",
    )
    .expect("the declaration carries a condition");
    assert_eq!(satisfies(floating, &bind("double")), Some(true));
    assert_eq!(satisfies(floating, &bind("int")), Some(false));

    // The three spellings of one answer.
    for spelling in [
        "std::is_integral<T>::value",
        "std::is_integral<T>{}",
        "std::is_integral_v<T>",
        "std::is_integral<T>()",
    ] {
        assert_eq!(
            satisfies(spelling, &bind("long")),
            Some(true),
            "{spelling} says a `long` is integral"
        );
    }

    // The operators, and a trait this layer does not know.
    assert_eq!(
        satisfies("!std::is_integral<T>::value", &bind("double")),
        Some(true)
    );
    assert_eq!(
        satisfies(
            "std::is_integral<T>::value && !std::is_same<T, char>::value",
            &bind("int")
        ),
        Some(true)
    );
    assert_eq!(
        satisfies(
            "std::is_integral<T>::value || std::is_floating_point<T>::value",
            &bind("float")
        ),
        Some(true)
    );
    assert_eq!(
        satisfies("std::is_user_defined<T>::value", &bind("int")),
        None,
        "a trait this layer does not know is *cannot tell*, never `false`"
    );
    assert_eq!(
        satisfies("std::is_integral<U>::value", &bind("int")),
        None,
        "an unbound parameter is cannot-tell as well"
    );
    // **The decorators**, which answer from the spelling alone — and for *any* type, so `std::string&` is a reference
    // whichever class `string` is.
    for (condition, spelling, expected) in [
        ("std::is_reference_v<T>", "int&", true),
        ("std::is_reference_v<T>", "int&&", true),
        ("std::is_reference_v<T>", "int", false),
        ("std::is_reference_v<T>", "std::string&", true),
        ("std::is_const_v<T>", "const int", true),
        ("std::is_const_v<T>", "int", false),
        ("std::is_const_v<T>", "const int*", false),
        ("std::is_volatile_v<T>", "volatile int", true),
        ("std::is_volatile_v<T>", "int", false),
        ("std::is_array_v<T>", "int[4]", true),
        ("std::is_array_v<T>", "int[]", true),
        ("std::is_array_v<T>", "int", false),
        ("std::is_function_v<T>", "int(int)", true),
        ("std::is_function_v<T>", "int", false),
        ("std::is_pointer_v<T>", "int*", true),
        ("std::is_pointer_v<T>", "int", false),
    ] {
        assert_eq!(
            satisfies(condition, &bind(spelling)),
            Some(expected),
            "{condition} of {spelling}"
        );
    }
    // **A class name is not a scalar this layer can place**, so a scalar trait of one is *cannot tell* — and an unbound
    // parameter stays unanswerable for the decorators too.
    assert_eq!(
        satisfies("std::is_integral_v<T>", &bind("std::string")),
        Some(false)
    );
    assert_eq!(satisfies("std::is_const_v<T>", &bind("U")), None);
    assert_eq!(satisfies("std::is_class_v<T>", &bind("std::string")), None);

    assert_eq!(
        the_condition_of("void plain(int value)"),
        None,
        "an ordinary function carries no condition"
    );
}

/// Doctors: how many `describe` bindings the scope tree keeps, looked up from inside the function body.
#[test]
fn dump_the_overload_bindings() {
    let source = "\
#include <type_traits>

template <typename T>
typename std::enable_if<std::is_integral<T>::value, int>::type describe(T value) { return 1; }

template <typename T>
typename std::enable_if<std::is_floating_point<T>::value, int>::type describe(T value) { return 2; }

int f() { return describe(1.5); }
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let inside = source.find("return describe").expect("the call");
    let reading = as_reading(&view, inside);
    let scope = view.scopes.scope_at(reading.get()).expect("a scope");
    let mut at = Some(scope);
    while let Some(current) = at {
        let data = view.scopes.scope(current).expect("the scope");
        let named: Vec<_> = data
            .bindings
            .iter()
            .filter(|binding| binding.name.identifier_text() == Some("describe"))
            .map(|binding| (binding.kind, binding.name_range.start_offset))
            .collect();
        eprintln!(
            "[bindings] scope {:?} kind {:?}: {} describe {named:?}",
            current,
            data.kind,
            named.len()
        );
        at = data.parent;
    }
    let all: Vec<_> = view
        .scopes
        .scope(scope)
        .expect("the scope")
        .bindings
        .iter()
        .map(|binding| (binding.kind, binding.name.identifier_text()))
        .collect();
    eprintln!("[all] {all:?}");
}
/// Doctors: the tree above a call's callee.
#[test]
fn dump_the_call_shape() {
    let source = "int describe(double v) { return 1; }\nint f() { return describe(1.5); }\n";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let at = source.find("describe(1.5)").expect("the call") + 3;
    let reading = as_reading(&view, at);
    let node = cpp_code_analysis::sema::resolve::expression_at(&view.root, reading.get());
    eprintln!("[call] expression={:?}", node.as_ref().map(|n| n.kind()));
    let mut chain = Vec::new();
    if let Some(node) = node {
        for ancestor in node.ancestors() {
            chain.push(format!("{:?}", ancestor.kind()));
        }
    }
    eprintln!("[call] ancestors={chain:?}");
    let call = cpp_code_analysis::sema::resolve::expression_at(&view.root, reading.get()).and_then(
        |node| {
            node.ancestors()
                .find(|n| format!("{:?}", n.kind()).contains("Call"))
        },
    );
    if let Some(call) = call {
        eprintln!(
            "[call] children={:?}",
            call.children()
                .map(|c| format!("{:?}", c.kind()))
                .collect::<Vec<_>>()
        );
    }
}

/// **`enable_if` in a *template parameter*** — the other spelling of the same idiom.
///
/// ```cpp
/// template <typename T, typename = std::enable_if_t<std::is_integral<T>::value>>
/// void describe(T value);
/// ```
///
/// Measured: the same call-site question as the return-type form, and the same answer for either call, because the
/// condition is found but the parameter that carries it (`typename = …`) declares no name — so the bindings shifted and
/// `T` was bound to nothing.
#[test]
fn an_enable_if_in_a_template_parameter_is_read() {
    let source = "\
#include <type_traits>
#include <string>

template <typename T, typename = std::enable_if_t<std::is_integral<T>::value>>
std::string describe(T value) {
    return \"integral\";
}

template <typename T, typename = std::enable_if_t<std::is_floating_point<T>::value>>
std::string describe(T value) {
    return \"floating\";
}

std::string f() {
    describe(41);
    return describe(1.5);
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let rendering = view.root.text().to_string();
    let declaration_at = |which: usize| -> usize {
        rendering
            .match_indices("describe(T value)")
            .nth(which)
            .map(|(at, _)| at)
            .expect("both declarations are in the rendering")
    };
    let floating_call = rendering
        .find("describe(1.5)")
        .expect("the call is written");
    let one = match session.definition(
        &view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(floating_call + 3),
    ) {
        cpp_code_analysis::Known::Yes(found) => found,
        other => panic!("`describe(1.5)` resolves: {other:?}"),
    };
    assert_eq!(
        one.fact.name_range.start_offset,
        declaration_at(1),
        "the template parameter's condition picks the floating-point overload"
    );
}
/// **A `requires` clause** — C++20's own spelling of the same condition.
///
/// ```cpp
/// template <typename T> requires std::is_integral_v<T>
/// std::string describe(T value);
/// ```
///
/// `enable_if` is found by name; a `requires` clause is a different keyword and was not read at all, so both overloads
/// stayed candidates and the call answered the first one written.
#[test]
fn a_requires_clause_is_read() {
    let source = "\
#include <type_traits>
#include <string>

template <typename T> requires std::is_integral_v<T>
std::string describe(T value) {
    return \"integral\";
}

template <typename T> requires std::is_floating_point_v<T>
std::string describe(T value) {
    return \"floating\";
}

std::string f() {
    describe(41);
    return describe(1.5);
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let rendering = view.root.text().to_string();
    let declaration_at = |which: usize| -> usize {
        rendering
            .match_indices("describe(T value)")
            .nth(which)
            .map(|(at, _)| at)
            .expect("both declarations are in the rendering")
    };
    let floating_call = rendering
        .find("describe(1.5)")
        .expect("the call is written");
    let one = match session.definition(
        &view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(floating_call + 3),
    ) {
        cpp_code_analysis::Known::Yes(found) => found,
        other => panic!("`describe(1.5)` resolves: {other:?}"),
    };
    assert_eq!(
        one.fact.name_range.start_offset,
        declaration_at(1),
        "the `requires` clause picks the floating-point overload"
    );

    // **And the other direction**, as in the `enable_if` test: an answer that always took the last candidate would pass
    // the assertion above.
    let integral_call = rendering.find("describe(41)").expect("the call is written");
    let integral = match session.definition(
        &view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(integral_call + 3),
    ) {
        cpp_code_analysis::Known::Yes(found) => found,
        other => panic!("`describe(41)` resolves: {other:?}"),
    };
    assert_eq!(
        integral.fact.name_range.start_offset,
        declaration_at(0),
        "and the integral overload for an int"
    );
}

/// **A named concept** — `template <integral T>` where `integral` is a concept the file defines.
///
/// ```cpp
/// template <typename T> concept integral = std::is_integral_v<T>;
/// template <integral T> std::string describe(T value);
/// ```
///
/// The constraint is a *name*, so there is no expression in the declaration to evaluate: it has to be found, and its own
/// right-hand side evaluated with the same bindings. Measured: both overloads stayed candidates and the call answered the
/// first.
#[test]
fn a_named_concept_decides_the_overload() {
    let source = "\
#include <type_traits>
#include <string>

template <typename T>
concept integral = std::is_integral_v<T>;

template <typename T>
concept floating = std::is_floating_point_v<T>;

template <integral T>
std::string describe(T value) {
    return \"integral\";
}

template <floating T>
std::string describe(T value) {
    return \"floating\";
}

std::string f() {
    describe(41);
    return describe(1.5);
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let rendering = view.root.text().to_string();
    let declaration_at = |which: usize| -> usize {
        rendering
            .match_indices("describe(T value)")
            .nth(which)
            .map(|(at, _)| at)
            .expect("both declarations are in the rendering")
    };
    let floating_call = rendering
        .find("describe(1.5)")
        .expect("the call is written");
    let one = match session.definition(
        &view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(floating_call + 3),
    ) {
        cpp_code_analysis::Known::Yes(found) => found,
        other => panic!("`describe(1.5)` resolves: {other:?}"),
    };
    assert_eq!(
        one.fact.name_range.start_offset,
        declaration_at(1),
        "the concept picks the floating-point overload"
    );
}
/// **A concept that composes another** — `concept signed_integral = integral<T> && is_signed_v<T>`.
///
/// Both overloads are constrained, so the call is decided by *which condition is true*, and the composed one can only be
/// answered by expanding `integral<T>` first with its own parameter bound. Measured before expansion existed: the verdict
/// was "cannot tell", both candidates stayed, and the call answered the first declaration.
#[test]
fn a_composed_concept_decides_the_overload() {
    let source = "\
#include <type_traits>
#include <string>

template <typename T>
concept integral = std::is_integral_v<T>;

template <typename T>
concept signed_integral = integral<T> && std::is_signed_v<T>;

template <signed_integral T>
std::string describe(T value) {
    return \"signed\";
}

template <typename T> requires std::is_floating_point_v<T>
std::string describe(T value) {
    return \"floating\";
}

std::string f() {
    describe(-3);
    return describe(1.5);
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let rendering = view.root.text().to_string();
    let declaration_at = |which: usize| -> usize {
        rendering
            .match_indices("describe(T value)")
            .nth(which)
            .map(|(at, _)| at)
            .expect("both declarations are in the rendering")
    };

    // **`describe(1.5)`** — the composed condition is `integral<double> && is_signed<double>`, which is false once
    // `integral<double>` is expanded, so the floating overload is the only survivor.
    let floating_call = rendering
        .find("describe(1.5)")
        .expect("the call is written");
    let floating = match session.definition(
        &view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(floating_call + 3),
    ) {
        cpp_code_analysis::Known::Yes(found) => found,
        other => panic!("`describe(1.5)` resolves: {other:?}"),
    };
    assert_eq!(
        floating.fact.name_range.start_offset,
        declaration_at(1),
        "the composed concept is false for a double"
    );

    // **And `describe(-3)`** — `integral<int> && is_signed<int>` is true, so the signed overload wins.
    let signed_call = rendering.find("describe(-3)").expect("the call is written");
    let signed = match session.definition(
        &view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(signed_call + 3),
    ) {
        cpp_code_analysis::Known::Yes(found) => found,
        other => panic!("`describe(-3)` resolves: {other:?}"),
    };
    assert_eq!(
        signed.fact.name_range.start_offset,
        declaration_at(0),
        "and true for a signed int"
    );
}

/// **A `No` from the type relation is not always a refusal** — `value_or(0)` against `_Ty2&&`.
///
/// The relation answers `No` for `int` → `_Ty2&&` because `_Ty2` is not `int`, and `_Ty2` is a *template parameter*. Only a
/// class the index can find decides those; one it cannot see makes the answer `Unknown`, and `Unknown` reports nothing.
#[test]
fn a_template_parameter_target_is_not_a_refusal() {
    use cpp_code_analysis::sema::convert::{Conversion, conversion};
    use cpp_code_analysis::sema::types::{Type, parse_type_spelling};

    let an_int = Type::builtin("int");
    assert_eq!(
        conversion(&an_int, "_Ty2&&", |_| None),
        Conversion::Unknown,
        "an unread template parameter is cannot-tell, not a refusal"
    );
    assert_eq!(
        conversion(&an_int, "T", |_| None),
        Conversion::Unknown,
        "and so is a bare `T`"
    );
    // **A class the index knows still refuses**: `int` cannot be built into a `Widget` that has no such constructor.
    assert_eq!(
        conversion(&an_int, "const Widget&", |_| Some(false)),
        Conversion::Refused,
    );
    // **And an unread class on the *source* side is cannot-tell too**, not a refusal.
    let a_string = parse_type_spelling("std::string");
    assert_eq!(conversion(&a_string, "int", |_| None), Conversion::Unknown);
}

/// **A member's deduced return type, through the header that declares it** — `auto copy_of() const { return value_; }`.
///
/// The class is written in a header and the call is in the `.cpp`, which is the shape a project actually has. The declared
/// type is `auto`, so the *body* is what says `int` once the class's `T` is substituted, and the body is reachable because
/// the header is part of the same translation unit.
#[test]
fn a_members_deduced_return_type_is_read_through_a_header() {
    let header = "\
template <typename T>
class Box {
public:
    explicit Box(T value) : value_(value) {}
    auto copy_of() const { return value_; }
    auto doubled() const { return value_ + value_; }

private:
    T value_;
};
";
    let source = "\
#include \"a.h\"

int f() {
    Box<int> box(7);
    auto copied = box.copy_of();
    auto doubled = box.doubled();
    return copied + doubled;
}
";
    let session = session_with(&[("/p/a.cpp", source), ("/p/a.h", header)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let offset = last_word(source, "copied");
    match session.type_at(&view, as_reading(&view, offset)) {
        cpp_code_analysis::Known::Yes(found) => {
            assert_eq!(found.type_of, "int", "`copy_of` returns the field's type");
        }
        other => panic!("`copied` is typed: {other:?}"),
    }
    let offset = last_word(source, "doubled");
    match session.type_at(&view, as_reading(&view, offset)) {
        cpp_code_analysis::Known::Yes(found) => {
            assert_eq!(found.type_of, "int", "`value_ + value_` is an `int`");
        }
        other => panic!("`doubled` is typed: {other:?}"),
    }
}

/// **A fold expression's type** — `template <typename... Vs> auto sum_of(Vs... values) { return (values + ...); }`.
///
/// The return type is `auto`, so the body decides, and the body is a fold: the type is every argument the call passed,
/// widened by `+` — four `int`s are an `int`, and `1` with `2.5` is a `double`.
#[test]
fn a_fold_expressions_type_is_the_widest_argument() {
    let source = "\
namespace t {
template <typename... Values>
auto sum_of(Values... values) {
    return (values + ...);
}
}

int f() {
    auto total = t::sum_of(1, 2, 3, 4);
    auto mixed = t::sum_of(1, 2.5);
    return total + static_cast<int>(mixed);
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    for (name, expected) in [("total", "int"), ("mixed", "double")] {
        let offset = last_word(source, name);
        match session.type_at(&view, as_reading(&view, offset)) {
            cpp_code_analysis::Known::Yes(found) => {
                assert_eq!(found.type_of, expected, "{name}");
            }
            other => panic!("`{name}` is typed: {other:?}"),
        }
    }
}

/// **A lambda called through its own `operator()`** — `auto doubler = [](int v) { return v * 2; }; doubler(3)`.
#[test]
fn a_lambda_call_has_the_type_its_body_returns() {
    let source = "\
int f() {
    auto doubler = [](int value) { return value * 2; };
    auto result = doubler(3);
    return result;
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    for (name, _) in [("doubler", "closure"), ("result", "int")] {
        let offset = last_word(source, name);
        match session.type_at(&view, as_reading(&view, offset)) {
            cpp_code_analysis::Known::Yes(found) => println!("{name} → {}", found.type_of),
            other => println!("{name} → {other:?}"),
        }
    }
    let offset = last_word(source, "result");
    match session.type_at(&view, as_reading(&view, offset)) {
        cpp_code_analysis::Known::Yes(found) => assert_eq!(found.type_of, "int"),
        other => panic!("`result` is typed: {other:?}"),
    }
}

/// **A structured binding over a `pair`** — `auto [number, fraction] = pair;` gives the components, in order.
///
/// Measured before this: **both** names answered `int()` — a spelling invented by the `auto` path after the pattern's
/// initializer was never found, because the `=` of `auto [a, b] = pair;` is inside the `InitDeclarator` and the lookup only
/// scanned the `Declaration` above it.
#[test]
fn a_structured_binding_over_a_pair_has_the_components_types() {
    let source = "\
#include <utility>

int f() {
    std::pair<int, double> pair{1, 2.5};
    auto [number, fraction] = pair;
    return number + static_cast<int>(fraction);
}
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    for (name, expected) in [("number", "int"), ("fraction", "double")] {
        let offset = last_word(source, name);
        match session.type_at(&view, as_reading(&view, offset)) {
            cpp_code_analysis::Known::Yes(found) => {
                assert_eq!(found.type_of, expected, "{name}");
            }
            other => panic!("`{name}` is typed: {other:?}"),
        }
    }
}

/// **`auto x = std::move(y)` is a value, not an rvalue reference.**
///
/// Reported from an editor with a screenshot: `auto diagnostic_results = std::move(_Arg.result.Data);` drew its inlay
/// type as `std::vector<LuaDiagnosticInfo>&&`, and every later query on it fell apart — a range-`for` over it could
/// not type its element, so `diagnostic.Message` answered "the type of `diagnostic` is not known".
///
/// The language rule is the one this pins: **`auto` alone declares a new object**, so the deduced type is the
/// initializer's type with its reference (and top-level cv) removed — `std::move` returning `T&&` deduces `T`. Only a
/// declarator that *writes* a reference keeps one, and that case has its own test
/// (`auto_reference_on_a_reference_returns_one_reference`).
#[test]
fn a_value_auto_drops_the_reference_its_initializer_had() {
    let source = "\
namespace std {
    template <class T> T&& move(T& value) noexcept;
}
struct Item { int size; };
int f() {
    Item item;
    auto moved = std::move(item);
    return moved.size;
}
";
    assert_eq!(
        type_of_use(source, "moved").as_deref(),
        Some("Item"),
        "`auto` alone is a new object, so the reference is not part of its type"
    );
}

/// **A class template's member alias is substituted with the instantiation's arguments.**
///
/// `std::vector<T>::emplace_back()` returns `reference`, which the class itself defines as `value_type&`. The declaration
/// names the element type, so the answer for `auto &result = v.emplace_back()` is `int&` — and a project class that
/// writes `using reference = T&;` must answer the same way, which is why this is a substitution along the class's own
/// aliases rather than a table of container members.
#[test]
fn a_container_members_alias_is_substituted_with_its_element_type() {
    let source = "\
namespace std {
    template <class T> class vector {
    public:
        using value_type = T;
        using reference = value_type&;
        reference emplace_back();
    };
}
int f() {
    std::vector<int> values;
    auto &result = values.emplace_back();
    result = 3;
    return result;
}
";
    assert_eq!(
        type_of_use(source, "result").as_deref(),
        Some("int&"),
        "`reference` is `value_type&`, and `value_type` is the argument the declaration wrote"
    );
}

/// **A dependent alias of a class template is substituted too** — the chain the standard library actually writes.
///
/// `reference` is `allocator_traits<value_type>::value_type&`: an alias whose right-hand side is a **member of another
/// class template**, instantiated with this class's own argument. MSVC's `vector` writes exactly this shape
/// (`using reference = _Alty&;` with `_Alty` a traits alias), which is why the reported `emplace_back()` had no type.
#[test]
fn a_dependent_alias_of_a_class_template_is_substituted() {
    let source = "\
namespace std {
    template <class T> struct allocator_traits { using value_type = T; };
    template <class T> class vector {
    public:
        using value_type = T;
        using reference = allocator_traits<value_type>::value_type&;
        reference emplace_back();
    };
}
int f() {
    std::vector<int> values;
    auto &result = values.emplace_back();
    result = 3;
    return result;
}
";
    assert_eq!(
        type_of_use(source, "result").as_deref(),
        Some("int&"),
        "`allocator_traits<value_type>::value_type` is `int` for `vector<int>`"
    );
}

/// **A class the index does not hold is answered from the rendering** — the shape of every system header.
///
/// The index summarizes the project's files; `<vector>` is not one of them, so `std::vector` has no entry and its members
/// cannot be looked up — measured on `EmmyLuaCodeStyle`: "the type of `std::vector<LuaDiagnostic>&` is not known here".
/// But the **rendering** has the class's body inlined, because that is what cooking means, and this crate parses it for
/// every cooked file. The fixture makes that shape without the standard library: `lib.h` is *included* and deliberately
/// **not** a project file.
#[test]
fn a_class_the_index_does_not_hold_is_answered_from_the_rendering() {
    const LIB_H: &str = "\
namespace std {
    template <class T> struct allocator_traits { using value_type = T; };
    template <class T> class vector {
    public:
        using value_type = T;
        using reference = allocator_traits<value_type>::value_type&;
        reference emplace_back();
    };
}
";
    let source = "\
#include \"lib.h\"
int f() {
    std::vector<int> values;
    auto &result = values.emplace_back();
    return result;
}
";
    let memory = MemoryFiles::new()
        .with_file("/p/lib.h", LIB_H)
        .with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    // **Only the translation unit is a project file.** `lib.h` is read because it is included, which is exactly what a
    // system header is to this index.
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();

    let view = session.view("/p/a.cpp").expect("the file is held");
    let offset = last_word(source, "result");
    match session.type_at(&view, as_reading(&view, offset)) {
        cpp_code_analysis::Known::Yes(found) => assert_eq!(
            found.type_of, "int&",
            "the class's member alias is in the rendering, even though the index has no `vector`"
        ),
        _ => panic!("the rendering holds `std::vector`, so `result` has a type"),
    }
}

/// **A member is found through a class template's instantiation, or it is not found at all.**
///
/// The plain `struct` and the class template declare the same member. If the first answers and the second does not, the
/// defect is in the member query's handling of a class template — not in the alias substitution, not in the index's
/// coverage, and not in the rendering. Written because `inspect complete` on `results.` in `EmmyLuaCodeStyle` printed
/// `scope std::vector, 0 item(s)` while the MSVC header is summarized with 993 declarations.
#[test]
fn a_member_is_found_through_a_plain_class_and_through_a_template() {
    const LIB_H: &str = "\
struct Plain { int emplace_back(); };
template <class T> class Temp { public: int emplace_back(); };
";
    let source = "\
#include \"lib.h\"
int f() {
    Plain plain;
    auto a = plain.emplace_back();
    Temp<int> temp;
    auto b = temp.emplace_back();
    return a + b;
}
";
    let memory = MemoryFiles::new()
        .with_file("/p/lib.h", LIB_H)
        .with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/lib.h"),
        std::path::PathBuf::from("/p/a.cpp"),
    ]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();

    let view = session.view("/p/a.cpp").expect("the file is held");
    for (spelling, expected) in [("a", "int"), ("b", "int")] {
        let offset = last_word(source, spelling);
        let found = session.type_at(&view, as_reading(&view, offset));
        assert!(
            matches!(&found, cpp_code_analysis::Known::Yes(known) if known.type_of == expected),
            "`{spelling}` should be `{expected}`, got {found:?}"
        );
    }
}
/// **The standard library's own idiom**: a class template inside macro-opened namespaces, with macro-guarded members.
///
/// Every simplified fixture answers correctly, so the one thing the real header has that they do not is this shape:
/// MSVC writes `_STD_BEGIN template <class _Ty…> class vector { … _CONSTEXPR20 reference emplace_back() … } _STD_END`.
/// If this fails, the defect is the macro-region handling around a class template, not the member query and not the
/// alias substitution.
#[test]
fn a_class_template_in_macro_opened_namespaces_is_the_standard_library_idiom() {
    const LIB_H: &str = "\
#define _STD_BEGIN namespace std {
#define _STD_END }
#define _CONSTEXPR20
#define _NODISCARD [[nodiscard]]
_STD_BEGIN
template <class _Ty> class vector {
public:
    using value_type = _Ty;
    using reference = value_type&;
    _NODISCARD _CONSTEXPR20 reference emplace_back();
};
_STD_END
";
    let source = "\
#include \"lib.h\"
int f() {
    std::vector<int> values;
    auto &result = values.emplace_back();
    result = 3;
    return result;
}
";
    let memory = MemoryFiles::new()
        .with_file("/p/lib.h", LIB_H)
        .with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/lib.h"),
        std::path::PathBuf::from("/p/a.cpp"),
    ]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();

    let view = session.view("/p/a.cpp").expect("the file is held");
    let offset = last_word(source, "result");
    let found = session.type_at(&view, as_reading(&view, offset));
    assert!(
        matches!(&found, cpp_code_analysis::Known::Yes(known) if known.type_of == "int&"),
        "the standard library's idiom should answer `int&`, got {found:?}"
    );
}

/// **Two levels of include, with guards** — the shape a standard header arrives through.
///
/// Every fixture so far had the class one include away from the file. A real translation unit reaches `<vector>` through
/// a chain (`std::string` → `xstring` → …), and each header is guarded. This is that shape with the types this crate
/// can see: `base.h` (guarded) declares the class, `mid.h` includes it, the `.cpp` includes only `mid.h`.
#[test]
fn a_member_is_found_two_includes_away_through_guards() {
    const BASE_H: &str = "\
#ifndef BASE_H
#define BASE_H
namespace std {
    template <class T> class vector {
    public:
        using value_type = T;
        using reference = value_type&;
        reference emplace_back();
    };
}
#endif
";
    const MID_H: &str = "\
#pragma once
#include \"base.h\"
";
    let source = "\
#include \"mid.h\"
int f() {
    std::vector<int> values;
    auto &result = values.emplace_back();
    result = 3;
    return result;
}
";
    let memory = MemoryFiles::new()
        .with_file("/p/base.h", BASE_H)
        .with_file("/p/mid.h", MID_H)
        .with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/base.h"),
        std::path::PathBuf::from("/p/mid.h"),
        std::path::PathBuf::from("/p/a.cpp"),
    ]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();

    let view = session.view("/p/a.cpp").expect("the file is held");
    let offset = last_word(source, "result");
    let found = session.type_at(&view, as_reading(&view, offset));
    assert!(
        matches!(&found, cpp_code_analysis::Known::Yes(known) if known.type_of == "int&"),
        "two includes away, through a guard, is still `int&`, got {found:?}"
    );
}

/// **An alias template used as a base has its arguments substituted into the alias's target.** — *open*
///
/// The standard library writes `allocator_traits<_Alloc> : conditional_t<…>` — a base that is an **alias template**,
/// so the base is not a class until the alias's parameters are replaced by what the clause wrote. Measured on
/// `EmmyLuaCodeStyle`, the member list reported
/// `NotDeclaredHere("conditional<_Test,_Ty1,_Ty2>::type")`: the target's own parameter names were still standing,
/// and the base was `unlisted`, so `size_type` was in no member list.
///
/// Asserted and ignored rather than deleted: it is the reproducer for the next step of the generics work, and it says
/// what the answer must be.
///
/// # Why the fix is not "call the alias walk here"
///
/// It was tried. `a_spelling_through_the_aliases` turns such a spelling into a type and is the obvious tool, but it
/// asks member questions, a member question builds a member list, and a member list walks bases — **measured as a
/// stack overflow** (`STATUS_STACK_OVERFLOW` on this very test). The resolution therefore has to be a static one: the
/// alias fact's own parameters and target, read from the index, then the target member's fact, substituted; and it
/// has to bound its own steps. For `conditional_t` it also has to stay sound — the condition is a library trait over
/// a template parameter and cannot be evaluated, so the honest answer is the members **both** branches agree on, the
/// same agreement rule `the_return_the_overloads_agree_on` applies.
#[test]
fn an_alias_template_used_as_a_base_substitutes_its_arguments() {
    let source = "\
template <class T> struct chosen { using size_type = unsigned long long; };
template <class T> struct other { using size_type = unsigned long long; };
template <class X, class Y> struct pick { using type = X; };
template <class X, class Y> using pick_t = typename pick<X, Y>::type;
template <class T> struct traits : pick_t<chosen<T>, other<T>> { };
struct User : traits<int> { };
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let list = cpp_code_analysis::index::project::members_of(
        session.index(),
        &view.scopes,
        &view.root,
        std::path::Path::new("/p/a.cpp"),
        "User",
    );
    let cpp_code_analysis::Known::Yes(list) = list else {
        panic!("`User` is a class: {list:?}");
    };
    let names: Vec<&str> = list
        .members
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect();
    assert!(
        names.contains(&"size_type"),
        "the alias's target names `chosen<T>`, whose members are the base's: {names:?} (unlisted: {:?})",
        list.unlisted
            .iter()
            .map(|one| one.spelling.as_str())
            .collect::<Vec<_>>()
    );
}

/// **A class template that is forward-declared and then defined has the *definition's* base clause.**
///
/// MSVC's `<xmemory>` writes `struct allocator_traits;` at line 554 and, at 755,
/// `struct allocator_traits : conditional_t<…> {};` — an **empty body** whose every member is inherited. The base
/// walk read the first Type fact it found, which is the forward declaration, whose `bases` is empty: measured on
/// `EmmyLuaCodeStyle`, `members_of("std::allocator_traits")` answered `0 listed, 0 unlisted`, and `size_type` was in
/// no list at all.
#[test]
fn a_class_template_forward_declared_then_defined_uses_the_definitions_bases() {
    let source = "\
template <class T> struct traits;
template <class T> struct base {
    using size_type = unsigned long long;
};
template <class T> struct traits : base<T> { };
struct User : traits<int> { };
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let list = cpp_code_analysis::index::project::members_of(
        session.index(),
        &view.scopes,
        &view.root,
        std::path::Path::new("/p/a.cpp"),
        "User",
    );
    let cpp_code_analysis::Known::Yes(list) = list else {
        panic!("`User` is a class: {list:?}");
    };
    let names: Vec<&str> = list
        .members
        .iter()
        .map(|member| member.fact.name.as_str())
        .collect();
    assert!(
        names.contains(&"size_type"),
        "the inherited member comes through the definition's base: {names:?} (unlisted: {:?})",
        list.unlisted
            .iter()
            .map(|one| one.spelling.as_str())
            .collect::<Vec<_>>()
    );
}

/// **A container's `size()` is a size, not the name of an internal alias.**
///
/// MSVC's `<vector>` writes the chain in four steps, and the last one is why this is a cross-file fixture:
///
/// ```cpp
/// using _Alty        = _Rebind_alloc_t<_Alloc, _Ty>;          // an alias template from <xmemory>
/// using _Alty_traits = allocator_traits<_Alty>;
/// using size_type    = typename _Alty_traits::size_type;
/// ```
///
/// `_Rebind_alloc_t` is declared in `<xmemory>`, which `<vector>` includes — and in the same file the chain resolves.
/// The fixture puts it where the standard library has it, because that is the shape the walk stops on.
///
/// Reported from an editor: `auto pos = _events.size();` in `LuaParser.cpp:51` drew `_Alty_traits::size_type` — the
/// walk stopped at `allocator_traits<_Alty>` without substituting `_Alty`.
#[test]
fn a_containers_size_called_on_an_object_is_its_size_type() {
    const XMEMORY: &str = "\
namespace std {
template <class T> struct allocator { using value_type = T; using size_type = unsigned long long; };
template <class A> struct allocator_traits { using size_type = typename A::size_type; };
template <class A, class T> struct _Rebind_alloc { using type = A; };
template <class A, class T> using _Rebind_alloc_t = typename _Rebind_alloc<A, T>::type;
}
";
    let source = "\
#include \"xmemory\"
namespace std {
template <class _Ty, class _Alloc = allocator<_Ty>> class vector {
    using _Alty        = _Rebind_alloc_t<_Alloc, _Ty>;
    using _Alty_traits = allocator_traits<_Alty>;
public:
    using size_type = typename _Alty_traits::size_type;
    size_type size() const;
};
}
std::vector<int> events;
void f() {
    auto pos = events.size();
    (void)pos;
}
";
    let session = session_with(&[("/p/xmemory", XMEMORY), ("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let offset = last_word(source, "pos");
    let found = session.type_at(&view, as_reading(&view, offset));
    assert!(
        matches!(&found, cpp_code_analysis::Known::Yes(known) if known.type_of == "unsigned long long"),
        "the chain ends at the size type, not at an alias of the allocator's: {found:?}"
    );
}

/// **A deduced type keeps the alias the source wrote.** `auto text = b.get();` where `get` returns
/// `std::string` — not `basic_string<char, char_traits<char>, allocator<char>>`, which is what the alias *stands for*
/// and what an editor drew in the inlay hint of `LuaCodeFormat.cpp:131`.
///
/// The alias walk (`a_spelling_through_the_aliases`) exists to turn a member's own spelling into a type the reader can
/// use — `pointer` → `_Val*`, `_Alloc_traits::reference` → `std::size_t&` — and it was also following a name qualified
/// by a **namespace**, where there is nothing to make usable: `std::string` is already a name.
#[test]
fn a_deduced_type_keeps_the_alias_the_source_wrote() {
    let source = "\
namespace std {
template <class T> struct allocator { };
template <class C> struct char_traits { };
template <class C, class T, class A> struct basic_string { };
using string = basic_string<char, char_traits<char>, allocator<char>>;
}
struct Builder { std::string get(); };
void f(Builder b) {
    auto text = b.get();
    (void)text;
}
";
    assert_eq!(
        type_of_use(source, "text").as_deref(),
        Some("std::string"),
        "the spelling the header wrote, not what the alias stands for"
    );
}

/// **The range of a range-based `for` written as a reference is still the range.**
///
/// `for (auto &x : someContainer)` where `someContainer` is a parameter is the ordinary spelling in every C++ code
/// base, and a parameter is almost always `const std::vector<T>&`. [`crate::index`]'s `range_element_of` matched only
/// `Type::Named`, so a one-token difference — the `&` the reader never thinks about — left the loop variable with no
/// type at all. Measured through the fixture in `tests/emplace_probe.rs`: `void f(const std::vector<Other>
/// &diagnostics) { for (auto &d : diagnostics) }` typed `d` as `std::vector<Other>&`, the container.
#[test]
fn a_range_written_as_a_reference_still_binds_its_element() {
    let source = "\
#include <vector>
int f(const std::vector<int> &values) {
    for (auto x : values) {
        int k = x;
        (void)k;
    }
    return 0;
}
";
    let session = session_with(&[
        (
            "/p/vector",
            "namespace std { template <class T> struct vector { }; }\n",
        ),
        ("/p/a.cpp", source),
    ]);

    let view = session.view("/p/a.cpp").expect("the file is held");
    // The last `k` is the use in `(void)k;`, whose type is the declaration's.
    let offset = last_word(source, "k");
    let found = session.type_at(&view, as_reading(&view, offset));
    assert!(
        matches!(&found, cpp_code_analysis::Known::Yes(known) if known.type_of == "int"),
        "the element of a `const std::vector<int>&` is `int`, got {found:?}"
    );
}

/// **An `auto` declaration inside a range-for body is deduced from *its own* initializer, not from the loop's range.**
///
/// Reported from an editor: `LuaCodeFormat.cpp:304`, `auto &result = results.emplace_back();`, the first statement of
/// `for (auto &diagnostic: diagnostics)`, answered `std::vector<LuaDiagnostic>&` — the **parameter's** type, which that
/// line never mentions. `range_for_element_type` reached the enclosing `RangeForStat` by ancestry and answered before
/// the ordinary deduction ran, so every `auto` in every loop body was typed as the loop variable.
#[test]
fn an_auto_declaration_in_a_range_for_body_is_deduced_from_its_own_initializer() {
    let source = "\
namespace std { template <class T> struct vector { T &emplace_back(); }; }
struct Item { int size; };
struct Other { int size; };
void f(const std::vector<Other> &diagnostics) {
    std::vector<Item> results;
    for (auto &diagnostic : diagnostics) {
        auto &result = results.emplace_back();
        int k = result.size;
        (void)k;
    }
}
";
    let session = session_with(&[("/p/a.cpp", source)]);

    let view = session.view("/p/a.cpp").expect("the file is held");
    let type_of = |offset: usize| match session.type_at(&view, as_reading(&view, offset)) {
        cpp_code_analysis::Known::Yes(known) => known.type_of,
        other => panic!("a type is expected at {offset}: {other:?}"),
    };

    // The loop variable is the element of the range, reference and all.
    let loop_variable = last_word(source, "diagnostic ");
    assert_eq!(
        type_of(loop_variable),
        "Other",
        "the loop variable is the range's element"
    );

    // And the declaration in the body is *its* initializer's type, which is `Item&`.
    assert_eq!(
        type_of(last_word(source, "result")),
        "Item&",
        "the declaration inside the body is not the loop variable"
    );
}
