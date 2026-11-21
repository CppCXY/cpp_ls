//! **A `const` object calls the `const` overload.** — the qualifier half of choosing between two declarations.
//!
//! The member list holds both, and until now the choice was by name and arity only, so the *first* declaration
//! answered. Measured through the server on the standard library: `const std::string& s; s[0]` answered `char&` where
//! the compiler answers `const char&`, and `const std::vector<int>& v; v[0]` answered `int&` where it answers
//! `const int&`.
//!
//! Three things had to hold, and this fixture is the shape of all three:
//!
//! * the object's type keeps its qualifier across the **alias step** — `const std::string&` is `basic_string<char, …>`
//!   and the target spelling carries no `const`;
//! * the member chooser asks for the declaration whose qualifier matches;
//! * the return spelling is resolved through the class's aliases (`reference` → `value_type&` → `int&`).

use std::path::PathBuf;

use cpp_code_analysis::Known;
use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::file::paths::DiskFiles;
use cpp_code_analysis::index::watch::WatchFilter;
use cpp_code_analysis::session::{OpenDocuments, Session, SessionFiles};

const SOURCE: &str = "\
template <class T>
struct Box {
    using value_type = T;
    using reference = value_type&;
    using const_reference = const value_type&;

    reference operator[](int i);
    const_reference operator[](int i) const;
};

struct OnlyMutable {
    int& operator[](int i);
};

Box<int> plain;
const Box<int> qualified;
OnlyMutable mutable_only;
const OnlyMutable mutable_on_a_const;

int &a() { return plain[0]; }
const int &b() { return qualified[0]; }
int &c() { return mutable_only[0]; }
int &d() { return mutable_on_a_const[0]; }
";

fn the_type_of(project: &str, source: &str, needle: &str) -> String {
    // **One directory per question**: `cargo test` runs one binary's tests in parallel, and a shared directory is a
    // test that removes the fixture another one is reading.
    let root: PathBuf = std::env::temp_dir().join(format!("cppls-subscript-qualifiers-{project}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let file = root.join("a.cpp");
    std::fs::write(&file, source).expect("the fixture writes");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.want_cooked_reading(&file);
    let _ = session.index_everything();

    let view = session.view(&file).expect("a reading");
    let at = source.find(needle).expect("the fixture writes it") + needle.len();
    let reading = view
        .reading_offset_of(FileOffset::new(at))
        .expect("in the file");
    let answer = match session.type_at(&view, reading) {
        Known::Yes(found) => found.type_of.to_string(),
        Known::No => "no".to_string(),
        Known::Unknown(reason) => format!("unknown({reason:?})"),
    };
    let _ = std::fs::remove_dir_all(&root);
    answer
}

#[test]
fn a_const_object_calls_the_const_overload() {
    assert_eq!(
        the_type_of("plain", SOURCE, "return plain[0"),
        "int&",
        "a non-const object gets the non-const overload"
    );
    assert_eq!(
        the_type_of("qualified", SOURCE, "return qualified[0"),
        "const int&",
        "and a const one gets `const_reference`, through `value_type`"
    );
}

/// **A class with only the non-const overload still answers**, which is the answer it gave before this choice existed:
/// the qualifier picks *among* declarations and never removes the only one. `std::list::remove` is the standard
/// library's own example of a member a `const` object cannot call at all.
#[test]
fn a_class_with_one_overload_answers_that_one() {
    assert_eq!(
        the_type_of("mutable-only", SOURCE, "return mutable_only[0"),
        "int&"
    );
    assert_eq!(
        the_type_of("mutable-on-a-const", SOURCE, "return mutable_on_a_const[0"),
        "int&"
    );
}
