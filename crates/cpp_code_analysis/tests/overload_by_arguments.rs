//! **The arguments choose the overload** — for a member call and for a subscript.
//!
//! A name with several declarations is an overload set, and a *cursor* needs one answer, so the member lookup takes
//! the first declaration ("a jump takes the first, a list takes them all"). A **call** is neither: the language picks
//! by converting the arguments, and this layer answered the first declaration for both shapes:
//!
//! ```text
//! struct S {
//!     int get(int v);
//!     double get(const char *v);
//!     int &operator[](int i);
//!     double &operator[](const char *i);
//! };
//! S s;
//! s.get(0)    int     ✓  the first declaration happens to be the one `0` selects
//! s.get("z")  int     ✗  a compiler answers `double`
//! s[0]        int&    ✓
//! s["z"]      int&    ✗  a compiler answers `double&`
//! ```
//!
//! The ranking already existed — `crate::sema::overload::choose`, the one `std::format`'s overloads go through — and
//! what it was missing is the candidate list. `member_candidates` is that list as **one posting query for the name**,
//! which is why it is affordable on a diagnostics pass: a member list substitutes every member and walks the bases
//! (measured: 2.0–2.5 ms warm for `std::vector<int>`), while this asks the index for the declarations filed in one
//! class scope.

use std::path::PathBuf;

use cpp_code_analysis::Known;
use cpp_code_analysis::file::paths::DiskFiles;
use cpp_code_analysis::index::watch::WatchFilter;
use cpp_code_analysis::session::{OpenDocuments, Session, SessionFiles};

const SOURCE: &str = "\
struct S {
    int get(int v);
    double get(const char *v);

    int &operator[](int i);
    double &operator[](const char *i);
};

S s;

int a() { auto x = s.get(0); return x; }
double b() { auto y = s.get(\"z\"); return y; }
int &c() { return s[0]; }
double &d() { return s[\"z\"]; }
";

/// The type of the expression `needle` stands for, read from the fixture.
fn the_type_of(project: &str, needle: &str) -> String {
    let root: PathBuf = std::env::temp_dir().join(format!("cppls-overload-by-arguments-{project}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let file = root.join("a.cpp");
    std::fs::write(&file, SOURCE).expect("the fixture writes");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.want_cooked_reading(&file);
    let _ = session.index_everything();

    let view = session.view(&file).expect("a reading");
    // **The expression node itself**, not an offset: `type_at` answers the *innermost* expression at a position, which
    // for a call's name is the member access (`s.get`) rather than the call. The node whose text is the whole
    // expression is the question this is about.
    let expression = view
        .root
        .descendants()
        .filter(|node| node.text() == needle)
        .max_by_key(|node| node.ancestors().count())
        .expect("the fixture writes the expression");
    let answer = match session.type_of_the_expression(&view, &expression) {
        Known::Yes(found) => found.type_of.to_string(),
        Known::No => "no".to_string(),
        Known::Unknown(reason) => format!("unknown({reason:?})"),
    };
    let _ = std::fs::remove_dir_all(&root);
    answer
}

/// **A member call's arguments select its overload** — the `const char*` one for a string literal, the `int` one for
/// an integer.
#[test]
fn a_member_calls_arguments_select_its_overload() {
    assert_eq!(
        the_type_of("call-int", "s.get(0)"),
        "int",
        "an `int` argument takes the `int` overload"
    );
    assert_eq!(
        the_type_of("call-text", "s.get(\"z\")"),
        "double",
        "a string literal takes the `const char*` overload"
    );
}

/// **A subscript's index is the argument an `operator[]` overload is chosen by**, and the answer has to come out as a
/// *type* — `int&` rather than the `reference` the declaration writes.
#[test]
fn a_subscripts_index_selects_its_overload() {
    assert_eq!(the_type_of("index-int", "s[0]"), "int&");
    assert_eq!(
        the_type_of("index-text", "s[\"z\"]"),
        "double&",
        "the index's type is what chooses between the two declarations"
    );
}
