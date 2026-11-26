//! **What the fold must get right**, on fixtures small enough to hold in the head.
//!
//! The fixtures are read through a real session: the summaries are the ones the index produced, so a test that passes
//! here is testing the composition against the same facts every query sees, not against a hand-built summary that
//! could agree with a broken fold.

use crate::NoMacros;
use std::path::Path;

use crate::cache::env::{Composed, Env, Skeleton, compose};
use crate::file::paths::MemoryFiles;
use crate::include::config::CompilerConfig;
use crate::index::watch::WatchFilter;
use crate::session::{OpenDocuments, Session, SessionFiles};

fn session_with(files: &[(&str, &str)]) -> Session<MemoryFiles> {
    let mut memory = MemoryFiles::new();
    for (path, text) in files {
        memory = memory.with_file(*path, *text);
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
            .map(|(path, _)| std::path::PathBuf::from(*path))
            .collect::<Vec<_>>(),
    );
    session.index_everything();
    session
}

/// The composition of `root` from an empty environment, with the session answering for every include.
fn composed(session: &Session<MemoryFiles>, root: &str) -> Composed {
    let path = Path::new(root);
    let summary = session
        .index()
        .summary(path)
        .unwrap_or_else(|| panic!("{root} is indexed"));
    let text = session.text(path).expect("the text is here");
    let skeleton = Skeleton::of(summary, &text);
    let guards = summary.guards.clone();
    compose(
        &skeleton,
        &guards,
        &Env::empty(),
        &NoMacros,
        &mut |target| {
            let summary = session.index().summary(target)?;
            let text = session.text(target)?;
            Some((Skeleton::of(summary, &text), summary.guards.clone()))
        },
    )
}

/// **A definition in an included header is in force after the include**, with its body.
///
/// The body is what a rendering pastes, so it is the part worth asserting: a fold that carried only the name would
/// pass a "is it defined" test and produce a rendering that drops the replacement list.
#[test]
fn a_define_reaches_the_file_that_includes_it() {
    let session = session_with(&[
        ("/p/b.h", "#define B 41 + 1\n"),
        ("/p/a.cpp", "#include \"b.h\"\nint x = B;\n"),
    ]);
    let composed = composed(&session, "/p/a.cpp");
    let definition = composed
        .final_env
        .get("B")
        .expect("B is in force after the include");
    assert_eq!(definition.body(), Some("41 + 1"));
}

/// **An `#undef` after the include takes the name away**, and the older definition does not resurface.
#[test]
fn an_undef_after_an_include_takes_the_name_away() {
    let session = session_with(&[
        ("/p/b.h", "#define B 1\n"),
        ("/p/a.cpp", "#include \"b.h\"\n#undef B\n"),
    ]);
    let composed = composed(&session, "/p/a.cpp");
    assert!(
        composed.final_env.get("B").is_none(),
        "the undef is the last thing the chain says about B"
    );
}

/// **An include under a condition that is false is not composed**, and one under a true condition is.
///
/// The `#if 0` arm is the case that separates a fold from a scanner: a scanner that recursed into every include would
/// compose an environment no compiler builds, and every macro from that branch would be a wrong answer downstream.
#[test]
fn an_include_under_a_false_condition_is_not_composed() {
    let session = session_with(&[
        ("/p/b.h", "#define FROM_B 1\n"),
        ("/p/c.h", "#define FROM_C 1\n"),
        (
            "/p/a.cpp",
            "#if 0\n#include \"b.h\"\n#endif\n#if 1\n#include \"c.h\"\n#endif\n",
        ),
    ]);
    let composed = composed(&session, "/p/a.cpp");
    assert!(
        composed.final_env.get("FROM_B").is_none(),
        "#if 0's include is not read"
    );
    assert!(
        composed.final_env.get("FROM_C").is_some(),
        "#if 1's include is"
    );
}

/// **An include guard makes the second include change nothing.**
///
/// The state after the second inclusion must equal the state after the first — that is what "read me once" means, and
/// it is decided by evaluating `#ifndef G_H` at the condition's own offset, against the table *before* the body that
/// writes `#define G_H`.
#[test]
fn an_include_guard_makes_the_second_include_change_nothing() {
    let session = session_with(&[
        (
            "/p/g.h",
            "#ifndef G_H\n#define G_H\nstruct G { int field; };\n#endif\n",
        ),
        ("/p/a.cpp", "#include \"g.h\"\n#include \"g.h\"\n"),
    ]);
    let composed = composed(&session, "/p/a.cpp");
    assert!(
        composed.final_env.get("G_H").is_some(),
        "the guard is defined"
    );
    assert_eq!(
        composed.after_each_include.len(),
        2,
        "both includes were taken — the guard stops the second one's *body*, not the include itself"
    );
    assert_eq!(
        composed.after_each_include[0].1.id(),
        composed.after_each_include[1].1.id(),
        "and the second leaves the environment exactly as the first did"
    );
}

/// **Two compositions of one file are the same value.**
#[test]
fn two_compositions_of_one_file_have_the_same_id() {
    let session = session_with(&[
        ("/p/b.h", "#define B 1\n"),
        ("/p/a.cpp", "#include \"b.h\"\n"),
    ]);
    let first = composed(&session, "/p/a.cpp");
    let second = composed(&session, "/p/a.cpp");
    assert_eq!(first.final_env.id(), second.final_env.id());
    assert_eq!(first.final_env.len(), 1);
}

/// **A function-like macro's body starts after its parameters**, which is what `body_range` promises and what an
/// expansion needs.
#[test]
fn a_function_like_body_is_sliced_after_its_parameters() {
    let session = session_with(&[("/p/a.h", "#define SUM(a, b) ((a) + (b))\n")]);
    let composed = composed(&session, "/p/a.h");
    let definition = composed.final_env.get("SUM").expect("SUM is defined");
    assert!(definition.is_function_like());
    assert_eq!(definition.body(), Some("((a) + (b))"));
}
