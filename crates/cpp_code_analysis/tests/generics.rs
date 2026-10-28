//! **Generics**: what a template instantiation answers, after the arguments are substituted.
//!
//! Templates are most of the C++ a reader meets — the standard library is one long template — and every question a
//! server answers about a `std::vector<int>` is really two: what the class is (`std::vector`), and what it is
//! *instantiated at* (`int`). A layer that answers the first and forgets the second offers the wrong members, types a
//! member's result wrongly, and cannot check the arguments a call passes.
//!
//! Each test below states one of those, with the shape a reader writes.

use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    file::coordinates::FileOffset,
};

const VECTOR: &str = "\
namespace std {
template <class T>
class vector {
public:
    void push_back(T value);
    T& front();
    unsigned long size();
};
}
";

fn session_with(source: &str) -> Session<MemoryFiles> {
    let memory = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    // The ask, then the pump — see `tests/completion.rs`'s note: a view is of a *rendering*, and a rendering is built
    // because something asked for one.
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();
    session
}

/// The members offered after `marker` — `"z."` for `z.` at the cursor.
fn members_after(source: &str, marker: &str) -> Vec<String> {
    let session = session_with(source);
    let view = session.view("/p/a.cpp").expect("the file is cooked");
    let at = source.find(marker).expect("the fixture writes the marker") + marker.len();
    match session.member_completions(&view, FileOffset::new(at)) {
        Known::Yes(found) => found
            .members
            .members
            .iter()
            .map(|member| member.fact.name.clone())
            .collect(),
        _ => Vec::new(),
    }
}

/// The type of the **last** use of a name, as the query layer answers it.
fn type_of_use(source: &str, spelling: &str) -> Option<String> {
    let session = session_with(source);
    let view = session.view("/p/a.cpp").expect("the file is cooked");
    let at = source.rfind(spelling).expect("the fixture names it");
    let reading = view.reading_offset_of(FileOffset::new(at))?;
    match session.type_at(&view, reading) {
        Known::Yes(found) => Some(found.type_of),
        _ => None,
    }
}

/// **A member list is the instantiated class's**, and the argument decides which class it is.
///
/// `z.push_back` on a `std::vector<int>` completes `push_back`; `append` — a `basic_string` member — must not appear,
/// because no `std::vector` has one. This is the case a reader reported: a hover and a completion that disagreed about
/// the same expression.
#[test]
fn a_member_list_is_the_instantiated_classs() {
    let source = format!("{VECTOR}void f() {{\n    std::vector<int> z;\n    z.\n}}\n");
    let offered = members_after(&source, "z.\n");
    assert!(
        offered.iter().any(|name| name == "push_back"),
        "the vector's member is offered: {offered:?}"
    );
    assert!(
        !offered.iter().any(|name| name == "append"),
        "and a `basic_string` member is not: {offered:?}"
    );
}

/// **The same, through an alias**: `using string = basic_string<char>;` reads as `basic_string<char>` everywhere.
#[test]
fn an_alias_to_an_instantiation_offers_that_classs_members() {
    let source = "\
namespace std {
template <class T>
class basic_string {
public:
    void append(T value);
    T& back();
};
using string = basic_string<char>;
}
void f() {
    std::string s;
    s.
}
";
    let offered = members_after(source, "s.\n");
    assert!(
        offered.iter().any(|name| name == "append"),
        "the alias's class is `basic_string`, whose members are these: {offered:?}"
    );
    assert!(
        !offered.iter().any(|name| name == "push_back"),
        "and not another class's: {offered:?}"
    );
}

/// **A nested instantiation** keeps both its members and its argument: `std::vector<std::vector<int>>` is a vector, and
/// its `front()` is a `std::vector<int>&`.
#[test]
fn a_nested_instantiation_has_its_members_and_its_argument() {
    let source = format!(
        "{VECTOR}void f() {{\n    std::vector<std::vector<int>> grid;\n    auto row = grid.front();\n}}\n"
    );
    assert_eq!(
        type_of_use(&source, "row").as_deref(),
        Some("std::vector<int>&"),
        "the member's type has the outer argument substituted, and keeps the inner one"
    );
}

/// **The substitution itself**, measured where it happens: the member list of an instantiated use records the
/// **argument's** spelling, not the class's parameter.
///
/// This is what makes completion, hover and the two argument checks read the same thing for `std::vector<int>` — and it
/// is why `push_back`'s parameter is `int value` and `front`'s return is `int&`, rather than `T` in both places.
#[test]
fn an_instantiated_member_list_records_the_arguments() {
    let source = format!("{VECTOR}void f() {{\n    std::vector<int> z;\n    z.push_back(1);\n}}\n");
    let session = session_with(&source);
    let view = session.view("/p/a.cpp").expect("the file is cooked");
    let at = source.find("z.push_back").expect("the fixture calls it");

    let reading = view
        .reading_offset_of(FileOffset::new(at))
        .expect("the offset is in the file");
    let Known::Yes(named) = session.type_at(&view, reading) else {
        panic!("the receiver's type is known");
    };
    assert_eq!(named.type_of, "std::vector<int>");

    let Known::Yes(list) = session.members_of(&view, &named.type_of) else {
        panic!("the instantiated member list answers");
    };
    let push_back = list
        .members
        .iter()
        .find(|member| member.fact.name == "push_back")
        .expect("the member is on the class");
    assert_eq!(
        push_back.fact.parameter_list.as_deref(),
        Some("(int value)"),
        "the class's parameter is substituted with the use's argument"
    );

    let front = list
        .members
        .iter()
        .find(|member| member.fact.name == "front")
        .expect("the member is on the class");
    // The substituted spelling keeps the class's own spacing (`int &`), which is the same type.
    assert_eq!(
        front
            .fact
            .returns
            .as_deref()
            .map(|written| written.replace(' ', "")),
        Some("int&".to_string())
    );
}

/// **An `operator->` inherited from a template base reaches the pointee.** `Ptr<S>`'s arrow is
/// `Base<S>::operator->`, whose return type is written `T*`; read literally that is a class called `T`, which has no
/// members — so the completion after `ptr->` was **empty**. It has to be read through the base clause's arguments
/// (and those through the derived class's own). Measured on the fixture: 0 items before, the pointee's member after.
///
/// The shape is libstdc++'s `std::shared_ptr`, whose `operator->` is declared in `__shared_ptr<_Tp, _Lp>` — which is
/// the report this came from: `std::shared_ptr<std::string> ptr; ptr->` offered nothing.
#[test]
fn an_operator_in_a_template_base_reaches_the_pointee() {
    let source = "\
struct S { void c_str(); };
template <class T>
struct Base { T* operator->(); };
template <class T>
struct Ptr : Base<T> {};
void f() {
    Ptr<S> p;
    p->;
}
";
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let mut session = Session::with_config(
        "/p",
        SessionFiles::new(OpenDocuments::new(), files),
        WatchFilter::new("/p"),
        CompilerConfig::default().with_standard("c++23"),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let mut rounds = 0;
    while session.pending() > 0 && rounds < 200 {
        session.advance(16);
        rounds += 1;
    }
    let view = session
        .view_of_the_file(std::path::Path::new("/p/a.cpp"))
        .expect("a view");
    let at = source.find("p->").expect("the fixture") + 3;
    let set = session.completions(&view, FileOffset::new(at));
    let labels: Vec<&str> = set.items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        labels.contains(&"c_str"),
        "`Ptr<S>::operator->` returns `S*`, so the members are `S`'s: {labels:?}"
    );
}

/// **An `operator->` two bases down, whose return names the base's own typedef.** This is libstdc++'s `shared_ptr`
/// exactly: `shared_ptr<_Tp> : __shared_ptr<_Tp, _Lp> : __shared_ptr_access<_Tp, _Lp, …>`, where the operator is
/// `element_type* operator->()` and `element_type` is `using element_type = _Tp;` in that base. The completion after
/// `ptr->` offered the `shared_ptr`'s own four names — measured on a real project, `append`/`c_str` were unreachable.
///
/// Two things have to be right for it: the substitution has to be **composed along the base chain** (`_Tp` at the
/// deepest level is `S` at the use), and the return type has to be read through the base's **typedef**.
#[test]
fn an_operator_two_bases_down_names_a_typedef_of_its_base() {
    let source = "\
struct S { void c_str(); };
template <class T, class U>
struct Leaf { using element_type = T; element_type* operator->(); };
template <class T>
struct Mid : Leaf<T, int> {};
template <class T>
struct Ptr : Mid<T> {};
void f() {
    Ptr<S> p;
    p->;
}
";
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let mut session = Session::with_config(
        "/p",
        SessionFiles::new(OpenDocuments::new(), files),
        WatchFilter::new("/p"),
        CompilerConfig::default().with_standard("c++23"),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let mut rounds = 0;
    while session.pending() > 0 && rounds < 200 {
        session.advance(16);
        rounds += 1;
    }
    let view = session
        .view_of_the_file(std::path::Path::new("/p/a.cpp"))
        .expect("a view");
    let at = source.find("p->").expect("the fixture") + 3;
    let set = session.completions(&view, FileOffset::new(at));
    let labels: Vec<&str> = set.items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        labels.contains(&"c_str"),
        "two bases down, `operator->` returns `element_type*` and `element_type` is `S`: {labels:?}"
    );
}

/// **A name the standard library brings into a namespace with a using-declaration is one namespace in.**
///
/// libstdc++'s ABI-1 `std::string` is `typedef basic_string<char> string;` in `std`, and the class it names is
/// `std::__cxx11::basic_string`: the `using __cxx11::basic_string;` that makes `std::basic_string` a name is a
/// *using-declaration*, which the index does not record as a declaration. The alias walk resolved the target by exact
/// spelling only, found neither `std::basic_string` nor `basic_string`, and gave up one namespace short —
/// measured on `import std;`, `members_of("std::string")` answered **0 members** with
/// `NotDeclaredHere("std::basic_string")` while `std::__cxx11::basic_string` was in the index all along.
///
/// The fixture writes the same shape without libstdc++: `string`'s target is unqualified, and the only declaration of
/// `basic_string` sits in `std::__cxx11`.
#[test]
fn an_alias_target_one_namespace_in_is_found() {
    let source = "\
namespace std {
namespace __cxx11 {
struct basic_string {
    void push_back();
    void clear();
};
}
using string = basic_string;
}
void f() {
    std::string text;
    text.;
}
";
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let mut session = Session::with_config(
        "/p",
        SessionFiles::new(OpenDocuments::new(), files),
        WatchFilter::new("/p"),
        CompilerConfig::default().with_standard("c++23"),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.index_everything();
    let mut rounds = 0;
    while session.pending() > 0 && rounds < 200 {
        session.advance(16);
        rounds += 1;
    }
    let view = session
        .view_of_the_file(std::path::Path::new("/p/a.cpp"))
        .expect("a view");
    let at = source.find("text.").expect("the fixture") + 5;
    let set = session.completions(&view, FileOffset::new(at));
    let labels: Vec<&str> = set.items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        labels.contains(&"push_back") && labels.contains(&"clear"),
        "`std::string` is `std::__cxx11::basic_string`, one namespace in from where its alias says `basic_string`: {labels:?}"
    );
}
