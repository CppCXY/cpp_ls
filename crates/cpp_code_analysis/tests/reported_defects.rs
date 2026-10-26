//! **The defects `docs/remaining.md` §A and §B reported, as fixtures** — one test per defect.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test reported_defects -- --nocapture
//! ```
//!
//! Each of these was found by using the editor on a real project, not by reading the code, and each has a *fact-level*
//! assertion here plus an end-to-end one where the card is what was wrong (`cpp_ls`'s
//! `a_hover_names_the_instantiation_and_never_invents_a_void` for the two hover reports, and
//! `the_two_reported_hovers_answer_on_the_project_they_came_from` for the project they came from).

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
    for (path, _) in files {
        session.want_cooked_reading(std::path::Path::new(path));
    }
    session.index_everything();
    session
}

fn parameters_of<'a>(
    summary: &'a cpp_code_analysis::FileSummary,
    name: &str,
) -> Option<&'a [String]> {
    summary
        .declarations
        .iter()
        .find(|fact| fact.name == name)
        .map(|fact| fact.parameters.as_slice())
}

/// **§B1 — a template parameter list is names, not the keywords that introduce them.**
///
/// The reported fact was MSVC's `<vector>`, whose `std::vector` declaration recorded `["class", "class"]`: the name was
/// read as "the last word before the `=`", and for an **unnamed** parameter that word is `class`. The keywords could
/// then *win* the choice of which declaration's parameters a substitution used.
///
/// A named parameter answers its own name (the case that already worked, kept as the regression), and an unnamed one is
/// an **empty slot** — it cannot be substituted into, and it is what keeps the arguments aligned with the names.
#[test]
fn a_template_parameter_is_a_name_and_an_unnamed_one_is_a_slot() {
    let session = session_with(&[(
        "/p/a.cpp",
        "template <class A, typename B, int N>\nstruct S {};\n\
         template <typename, typename> class an_unnamed_one;\n\
         template <typename, class T> struct partial;\n",
    )]);
    let summary = session
        .index()
        .summary(std::path::Path::new("/p/a.cpp"))
        .expect("indexed");

    assert_eq!(
        parameters_of(summary, "S"),
        Some(&["A".to_string(), "B".to_string(), "N".to_string()][..]),
        "a named parameter answers its name"
    );
    assert_eq!(
        parameters_of(summary, "an_unnamed_one"),
        Some(&[String::new(), String::new()][..]),
        "two slots, and no keyword pretending to be a name"
    );
    assert_eq!(
        parameters_of(summary, "partial"),
        Some(&[String::new(), "T".to_string()][..]),
        "the second slot keeps its name **and its position**"
    );
}

/// **§A2 — `static const struct { … } priority[]` has no `void` in it anywhere.**
///
/// The specifier sequence of an unnamed class type contributed no name, so the reader fell back to `void` and the fact
/// recorded `void[]`. `void` is not "no answer": it is a different type. The honest answer is that this layer cannot
/// spell the type, which the fact records as no `type_of` at all — and then a hover shows the declaration's own text.
#[test]
fn an_anonymous_structs_variable_is_not_a_void() {
    let session = session_with(&[(
        "/p/a.cpp",
        "static const struct\n{\n    unsigned char left;\n    unsigned char right;\n} priority[] = { { 1, 2 } };\n",
    )]);
    let summary = session
        .index()
        .summary(std::path::Path::new("/p/a.cpp"))
        .expect("indexed");
    let facts: Vec<(&str, Option<&str>)> = summary
        .declarations
        .iter()
        .filter(|fact| fact.name == "priority" || fact.name == "left" || fact.name == "right")
        .map(|fact| (&*fact.name, fact.type_of.as_deref()))
        .collect();
    println!("facts: {facts:?}");

    assert!(
        !facts.iter().any(|(_, type_of)| *type_of == Some("void")),
        "nothing here is a `void`: {facts:?}"
    );
    assert!(
        facts.contains(&("left", Some("unsigned char"))),
        "and the members keep their own types: {facts:?}"
    );
}

/// **§A1 — the spelling the reader wrote survives into the fact.**
///
/// The hover card for `std::map<std::string, LuaTokenKind, std::less<>>` showed only the *declaration's* parameter
/// names. The card is fixed in `cpp_ls` (it reads the use's spelling from the file), and this is the other half: the
/// fact of the variable must keep the instantiation that was written, because that is what a reader compares against.
#[test]
fn a_template_id_keeps_the_spelling_that_was_written() {
    let session = session_with(&[(
        "/p/a.cpp",
        "namespace std {\ntemplate <class K, class V, class C>\nstruct map {};\n}\n\
         std::map<int, long, int> table = {};\n",
    )]);
    let summary = session
        .index()
        .summary(std::path::Path::new("/p/a.cpp"))
        .expect("indexed");
    let table = summary
        .declarations
        .iter()
        .find(|fact| fact.name == "table")
        .expect("the variable is a fact");

    assert_eq!(
        table.type_of.as_deref(),
        Some("std::map<int, long, int>"),
        "the type the file wrote"
    );
}
