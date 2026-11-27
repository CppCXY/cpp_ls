//! **The regions an editor greys out**: what the preprocessor already decided, asked of a session.
//!
//! The spans come from the branch decision the cook makes anyway (`CookedStream::inactive`), copied into the rendering
//! (`RenderedCooked::inactive`) and persisted with the cooked record. This file is the session-level contract: a file
//! nobody has rendered answers `None` (not an empty list), a cooked file answers its dead branches, and a file with no
//! directives answers an empty list.
//!
//! **Each test gets its own root**, because a session's cache lives in `<root>/.cppls` and a shared root would hand the
//! test a rendering from a previous run — which is exactly the difference the first test is about.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn session_of(files: &[(&str, &str)]) -> (Session<MemoryFiles>, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "cppls-inactive-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    let session = session_at(&root, files);
    (session, root)
}

/// A session over `root` — the **same** cache directory for a second session, which is what a warm start needs.
fn session_at(root: &Path, files: &[(&str, &str)]) -> Session<MemoryFiles> {
    let mut memory = MemoryFiles::new();
    let mut paths = Vec::new();
    for (name, text) in files {
        let path = root.join(name);
        memory = memory.with_file(path.clone(), *text);
        paths.push(path);
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        root,
        providers,
        WatchFilter::new(root),
        CompilerConfig::default(),
    );
    session.add_project_files(paths.clone());
    for path in &paths {
        session.load(path);
    }
    session
}

const A_DEAD_BRANCH: &str = "\
#define ON 1
#define OFF 0

#if ON
int live;
#else
int dead;
#endif

#if OFF
int also_dead;
#endif
";

/// **A dead branch is a span of the file's own text, and `None` means "nobody has read this yet".**
///
/// The distinction is the feature's contract with its caller: an editor must not clear a grey range because the next
/// request arrived before the file was rendered, and a file with no dead code must clear it.
#[test]
fn a_dead_branch_is_a_span_and_an_unread_file_says_nothing() {
    let (mut session, root) = session_of(&[("a.cpp", A_DEAD_BRANCH)]);
    let path = root.join("a.cpp");

    // **No rendering yet**: not "nothing is inactive", but "nobody has read this file as a compiler would".
    assert_eq!(
        session.inactive_regions(&path),
        None,
        "the first request asks for a rendering and answers nothing"
    );

    session.index_everything();
    session.cook(&path);
    let answer = session
        .inactive_regions(&path)
        .expect("the cooked file answers");
    assert!(
        answer.is_exact(),
        "every branch in this fixture is decided by a macro the file defines: {answer:?}"
    );
    let spans = &answer.spans;
    // **One span per dead code run**: directives between two dead branches are structure, not grey code, so the two
    // dead statements are two spans unless their tokens touch.
    assert!(
        !spans.is_empty(),
        "the dead branches are recorded: {spans:?}"
    );

    // Every span is a range of the file's own text, covering the dead code and not the live code.
    let live = A_DEAD_BRANCH.find("int live;").expect("the live statement");
    let dead = A_DEAD_BRANCH.find("int dead;").expect("the dead statement");
    let also_dead = A_DEAD_BRANCH
        .find("int also_dead;")
        .expect("the second dead statement");
    let covered = |at: usize| {
        spans
            .iter()
            .any(|span| span.start_offset <= at && at < span.end_offset())
    };
    assert!(!covered(live), "the live branch is not greyed out");
    assert!(covered(dead), "the dead branch is");
    assert!(covered(also_dead), "and so is the second one");
    for span in spans {
        assert!(
            span.end_offset() <= A_DEAD_BRANCH.len(),
            "a span is inside the file it describes: {span:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// **A file with no directives is answered, not skipped.**
///
/// `Some(vec![])` is "nothing here is inactive"; `None` is reserved for an unread file, and a caller that conflated the
/// two would leave a grey range on a plain source file forever.
#[test]
fn a_file_with_no_directives_has_no_regions() {
    let (mut session, root) = session_of(&[("plain.cpp", "int main() { return 0; }\n")]);
    let path = root.join("plain.cpp");
    session.index_everything();
    session.cook(&path);
    assert_eq!(
        session.inactive_regions(&path).map(|answer| answer.spans),
        Some(Vec::new())
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// **A dead branch with an `#include` is still one span of its ordinary code.**
///
/// The directive lines are not grey; the code after the dead include is.
#[test]
fn a_dead_branch_that_includes_a_header_is_still_one_span() {
    let source = "\
#define ON 1

#if ON
int live;
#else
#include \"not_here.h\"
int dead;
#endif
";
    let (mut session, root) = session_of(&[("a.cpp", source)]);
    let path = root.join("a.cpp");
    session.index_everything();
    session.cook(&path);
    let spans = session
        .inactive_regions(&path)
        .expect("the cooked file answers")
        .spans;
    assert_eq!(spans.len(), 1, "one dead branch, one span: {spans:?}");
    let include = source.find("#include").expect("the include line");
    assert!(
        !(spans[0].start_offset <= include && include < spans[0].end_offset()),
        "the directive line is not part of the grey range"
    );
    let dead = source.find("int dead;").expect("the dead statement");
    assert!(
        spans[0].start_offset <= dead && dead < spans[0].end_offset(),
        "the ordinary code is inside the dead branch's span"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// **A cook served from the disk answers the same spans as one that rendered.**
///
/// The rendering cannot answer this after the fact — it has no dead text, and "no token" is indistinguishable from a
/// comment — so the record carries the spans. This is the warm-start half of the contract, asserted through a second
/// session over the same root: it reads the record the first session wrote, and must answer what the first did.
#[test]
fn a_warm_start_answers_the_spans_the_record_carries() {
    let (mut cold, root) = session_of(&[("a.cpp", A_DEAD_BRANCH)]);
    let path = root.join("a.cpp");
    cold.index_everything();
    cold.cook(&path);
    let from_the_render = cold
        .inactive_regions(&path)
        .expect("the cooked file answers");
    // **The record is written by the quiet pass**, not by the commit — a write inside the commit is what made a
    // program read hold the writer for seconds (`Session::persist_cooked`). The second session can only read what
    // this one wrote down.
    let _ = cold.persist_cooked(std::time::Duration::ZERO);
    drop(cold);

    let mut warm = session_at(&root, &[("a.cpp", A_DEAD_BRANCH)]);
    // A rendering is looked up through the index (its path and its record), so the second session indexes first.
    warm.index_everything();
    let from_the_record = warm
        .inactive_regions(&path)
        .expect("the second session answers");
    assert_eq!(
        from_the_record, from_the_render,
        "a warm start must not answer different spans than a cold one"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// **One guessed branch does not hide the exact ones.**
///
/// A file can have a `#if ON` branch decided by a definition and a `#if UNDEF` branch decided by assumption. The
/// second must not suppress the first: the client is sent the exact subset and the count still says one branch was a
/// guess. This is the difference between "no answer" and "one answer withheld".
#[test]
fn a_guess_does_not_hide_the_exact_regions() {
    let source = "\
#define ON 1
#if ON
int live;
#else
int exact_dead;
#endif
#if UNDEFINED_ANYWHERE
int guessed_dead;
#endif
";
    let (mut session, root) = session_of(&[("a.cpp", source)]);
    let path = root.join("a.cpp");
    session.index_everything();
    session.cook(&path);
    let answer = session
        .inactive_regions(&path)
        .expect("the cooked file answers");
    assert!(
        answer.assumed_undefined >= 1,
        "the second branch used the undefined-is-zero rule: {answer:?}"
    );
    assert!(!answer.is_exact(), "the whole answer is not exact");
    assert!(
        !answer.exact_spans.is_empty(),
        "but the branch decided by ON is still paintable: {answer:?}"
    );
    let exact_dead = source.find("int exact_dead;").expect("the exact branch");
    let guessed_dead = source
        .find("int guessed_dead;")
        .expect("the guessed branch");
    let covered = |spans: &[cpp_parser::SourceRange], at: usize| {
        spans
            .iter()
            .any(|span| span.start_offset <= at && at < span.end_offset())
    };
    assert!(
        covered(&answer.exact_spans, exact_dead),
        "the exact branch is in the paintable subset"
    );
    assert!(
        !covered(&answer.exact_spans, guessed_dead),
        "the guessed branch is not"
    );
    let _ = std::fs::remove_dir_all(&root);
}
///
/// This is the gate the plan puts before the protocol: `#if SOMETHING` with no definition anywhere is C's rule that
/// an undefined identifier is `0`, and the branch is chosen **by a guess about the configuration**. A client that
/// painted the resulting span grey would be drawing over code the user's real build may compile, so the answer
/// carries the count and `is_exact()` answers "may I paint this?".
#[test]
fn a_branch_decided_by_an_undefined_name_is_not_exact() {
    let source = "\
#if NOT_DEFINED_ANYWHERE
int guessed;
#else
int live;
#endif
";
    let (mut session, root) = session_of(&[("a.cpp", source)]);
    let path = root.join("a.cpp");
    session.index_everything();
    session.cook(&path);
    let answer = session
        .inactive_regions(&path)
        .expect("the cooked file answers");
    assert!(
        answer.assumed_undefined >= 1,
        "the branch was chosen by the undefined-is-zero rule: {answer:?}"
    );
    assert!(!answer.is_exact(), "so the spans must not be painted");
    assert!(
        !answer.spans.is_empty(),
        "and there is still a span to report: {answer:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
