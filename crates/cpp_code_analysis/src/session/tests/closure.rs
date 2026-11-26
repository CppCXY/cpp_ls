//! **Closure** — what is known, and when a closure is ready

use crate::CompletionSet;
use crate::ProjectLanguage;
use crate::file;
use crate::file::coordinates::ReadingOffset;
use crate::file::paths::MemoryFiles;
use crate::include::config::{CommandLineMacro, CompilerConfig};
use crate::include::toolchain::{Toolchain, ToolchainSource};
use crate::session;
use crate::symbol::Known;
use std::path::{Path, PathBuf};

use super::*;

#[test]
pub(crate) fn the_compilation_is_known_only_when_the_compiler_answered_with_its_macros() {
    // The licence for one claim — *a name none of the witnesses mentions is not defined* — pinned on the three
    // shapes a session can be in. The middle one is why this is a function rather than a field read at the call
    // site: a toolchain **exists** on a machine whose headers were found by a convention and whose compiler
    // could not be asked, and its macro table is empty. Claiming completeness there would read all five hundred
    // of the compiler's own names as "not defined" — `#ifdef _MSC_VER` coming out false *on MSVC* — which is a
    // wrong answer rather than a missing one, the direction this layer must never fail in.
    assert!(
        !session::the_compilation_is_known(None),
        "no compiler answered, so nothing witnesses the built-ins"
    );

    let unasked = Toolchain {
        compiler: None,
        version: None,
        system_include_paths: vec![PathBuf::from("/usr/include")],
        builtin_macros: Vec::new(),
        dialect: None,
        cuda: false,
        standard: None,
        source: ToolchainSource::SystemHeaders,
        note: Some("no compiler could be asked".to_string()),
    };
    assert!(
        !session::the_compilation_is_known(Some(&unasked)),
        "directories without a macro table are half a toolchain, and the missing half is the built-ins"
    );

    let asked = Toolchain {
        compiler: Some(PathBuf::from("cl")),
        builtin_macros: vec![CommandLineMacro::defined("_MSC_VER")],
        ..unasked
    };
    assert!(
        session::the_compilation_is_known(Some(&asked)),
        "a compiler that answered is the only witness there is to the names no file defines"
    );
}

/// **The closure, not the direct includes** — the question a *colouring* asks.
///
/// `is_locally_ready` asks whether the file's own includes are read, which is exactly what `catch_up` does and
/// what one name lookup needs. A semantic token pass classifies **every** identifier, so what it needs is the
/// whole closure: `std::endl` is declared in `<ostream>`, reached through `<iostream>`, and a request answered
/// while only the first level was read drew it with the client's own grammar colour (measured from an editor:
/// a variable at startup, a function after any edit).
#[test]
pub(crate) fn a_file_is_closure_ready_only_once_what_it_reaches_is_read() {
    let files = MemoryFiles::new()
        .with_file("/p/mid.h", "#include \"deep.h\"\n")
        .with_file("/p/deep.h", "namespace std { int endl(); }\n")
        .with_file(
            "/p/a.cpp",
            "#include \"mid.h\"\nint main() { return std::endl(); }\n",
        );
    let fixture = Memory::new("is-closure-ready", &files);

    let mut session = fixture.session();
    session.add_project_files([PathBuf::from("/p/a.cpp")]);
    session.index_everything();

    assert!(
        session.is_locally_ready(Path::new("/p/a.cpp")),
        "the file and its direct includes are read"
    );
    assert!(
        session.is_closure_ready(Path::new("/p/a.cpp")),
        "and so is everything they reach"
    );

    // **The distinction, made by dropping the deepest file's summary** — the state a project is in while its
    // queue is still being read: `a.cpp`'s direct include (`mid.h`) is in, its include's (`deep.h`) is not.
    session.store.forget(Path::new("/p/deep.h"));

    assert!(
        session.is_locally_ready(Path::new("/p/a.cpp")),
        "the direct question is unchanged — `catch_up` has nothing left to read"
    );
    assert!(
        !session.is_closure_ready(Path::new("/p/a.cpp")),
        "but a name declared in `deep.h` has no classification, which is the question colouring asks"
    );
}

/// **A keystroke in a file the closure does not reach does not re-run another file's pull.**
///
/// The memo's key used to carry the store's **global revision**, which moves on any insert — so editing any file in
/// the project invalidated every other file's findings, and a reader typing in one file paid a full re-run (143 ms
/// measured, 97% of it inference) for the file beside it. The review in `docs/perf-review.md` names it.
///
/// What replaces it is the closure: the file's own key, the content of every file it reaches, how many files the
/// index holds, the rules and the language — see [`crate::session::Session::the_state_of_a_pull`]. This test is both
/// directions, because a key that is too narrow is a stale finding:
///
/// * an edit to a file **outside** the closure is a memo hit (the finding is the same answer);
/// * an edit to a header **inside** it is a miss (the answer could have changed).
#[test]
pub(crate) fn a_keystroke_outside_the_closure_does_not_rerun_another_files_pull() {
    let files = MemoryFiles::new()
        .with_file("/p/h.h", "struct Widget { int size; };\n")
        .with_file(
            "/p/a.cpp",
            "#include \"h.h\"\nint main() { int count = \"three\"; return 0; }\n",
        )
        .with_file("/p/b.cpp", "int other() { return 1; }\n");
    let memory = Memory::new("pull-key", &files);
    let mut session = memory.session();
    let a = PathBuf::from("/p/a.cpp");
    let b = PathBuf::from("/p/b.cpp");
    session.add_project_files([a.clone(), b.clone()]);
    session.index_everything();
    session.cook(&a).expect("the file is cooked");
    session.cook(&b).expect("the file is cooked");

    let first = session.diagnostics(&a).expect("a cooked reading");
    assert!(
        !first.checks.is_empty(),
        "the fixture has a finding to remember: {first:?}"
    );
    let (hits, runs) = session.pull_counts();

    // **Outside the closure**: `b.cpp` is not in `a.cpp`'s program, so nothing `a.cpp` is checked against moved.
    session.did_change(&b, "int other() { return 2; }\n");
    session.advance(64);
    session.cook(&b);

    let unchanged = session.diagnostics(&a).expect("a cooked reading");
    assert_eq!(
        unchanged.checks, first.checks,
        "the same findings, because nothing the pull read changed"
    );
    let (hits_after, runs_after) = session.pull_counts();
    assert_eq!(
        (hits_after - hits, runs_after - runs),
        (1, 0),
        "the pull was answered from the memo rather than run again"
    );

    // **Inside the closure**: the header `a.cpp` includes is re-read, which can change what a check says about it.
    session.did_change(PathBuf::from("/p/h.h"), "struct Widget { long size; };\n");
    session.advance(64);
    session.cook(&a);
    let _ = session.diagnostics(&a).expect("a cooked reading");
    let (hits_last, runs_last) = session.pull_counts();
    assert_eq!(
        (hits_last - hits_after, runs_last - runs_after),
        (0, 1),
        "a header in the closure is a different state, so the rules run again"
    );
}

/// **One cold closure, eight threads, one walk.**
///
/// A miss in the visibility memo is computed outside the lock that guards it — right for the lock, wrong for the
/// work: two callers that miss the same entry used to build the same closure twice. The pair that does it in a
/// server is the pump prewarming an open file and the reader's own request arriving, and `docs/perf-review.md`
/// measured two concurrent cold walks of one closure at ~24 s each.
///
/// The claim is a `HashSet` of the roots being walked plus a `Condvar`, so the second caller waits for the first's
/// answer instead of repeating it. This test is the measurement: eight threads released by one barrier, and the
/// question is how many walks the index built.
///
/// **The negative measurement is the reason the test exists**: with the claim removed, `walks_built` reaches 8 (the
/// threads all arrive inside the same millisecond), and `walks_met` is 0. With it, 1 and 7.
#[test]
pub(crate) fn a_cold_closure_is_walked_once_for_everyone_who_asks_at_the_same_time() {
    // **A chain of headers**, so the walk is long enough for eight threads to arrive while it runs: forty files is
    // ~1 ms, which is a thousand times the spread of a barrier's threads.
    const DEPTH: usize = 40;
    let mut files = MemoryFiles::new();
    for depth in 0..DEPTH {
        let next = match depth + 1 < DEPTH {
            true => format!("#include \"h{}.h\"\n", depth + 1),
            false => String::new(),
        };
        files = files.with_file(
            format!("/p/h{depth}.h"),
            format!("{next}struct S{depth} {{ int field; }};\n"),
        );
    }
    files = files.with_file("/p/a.cpp", "#include \"h0.h\"\nint main() { return 0; }\n");

    let memory = Memory::new("one-walk", &files);
    let mut session = memory.session();
    let path = PathBuf::from("/p/a.cpp");
    session.add_project_files([path.clone()]);
    session.index_everything();

    // **Cold**: a dropped entry is a closure nobody has walked since, which is the state the first request after an
    // edit is in. Without it the threads would all find the memo and this test would prove nothing.
    session
        .store
        .forget(Path::new(&format!("/p/h{}.h", DEPTH - 1)));
    let (built_before, _, _) = session.index().walk_counts();

    const THREADS: usize = 8;
    let barrier = std::sync::Barrier::new(THREADS);
    let answers: Vec<usize> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    session.index().visible_files(&path).len()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("a walker"))
            .collect()
    });

    let (built, hits, met) = session.index().walk_counts();
    assert!(
        answers.iter().all(|seen| *seen == answers[0]),
        "every caller gets the same closure: {answers:?}"
    );
    assert!(
        answers[0] > 1,
        "and it is the closure, not just the file: {} file(s)",
        answers[0]
    );
    assert_eq!(
        built - built_before,
        1,
        "one walk for eight callers ({met} met an in-flight walk, {hits} hit the memo)"
    );
}

/// **A pull whose closure is not read does not run the rules, and says so.**
///
/// The gate that withholds a check's finding used to be asked by the *server*, after [`Session::diagnostics`] had
/// already run every rule: a file whose closure was still filling paid for four rules resolving types and then had
/// its answer thrown away. Measured on a 4 MB translation unit, that is seconds per pull, on the first pull after a
/// window opens — the one pull a reader is waiting for.
///
/// The question is the same one ([`Session::is_closure_ready`]); what changed is where it is asked. This test is the
/// state that matters and that a settled project does not show, built the way the test above builds it: the summary
/// of one file in the closure is dropped, which is what a project mid-index looks like.
#[test]
pub(crate) fn a_pull_whose_closure_is_unread_withholds_its_checks_and_says_so() {
    let files = MemoryFiles::new()
        .with_file("/p/mid.h", "#include \"deep.h\"\n")
        // A check that will fire: `count` is initialized from a string literal.
        .with_file("/p/deep.h", "namespace std { int endl(); }\n")
        .with_file(
            "/p/a.cpp",
            "#include \"mid.h\"\nint main() { int count = \"three\"; return 0; }\n",
        );
    let memory = Memory::new("checks-withheld", &files);
    let mut session = memory.session();
    let path = PathBuf::from("/p/a.cpp");
    session.add_project_files([path.clone()]);
    session.index_everything();
    session.cook(&path).expect("the file is cooked");

    let ready = session.diagnostics(&path).expect("a cooked reading");
    assert!(
        !ready.checks_withheld,
        "every file in the closure is read, so the rules run: {:?}",
        ready.checks
    );

    // **The closure goes cold**, which is the state the index is in while the queue is still being read.
    session.store.forget(Path::new("/p/deep.h"));
    assert!(
        !session.is_closure_ready(&path),
        "the deepest file's summary is gone"
    );

    let withheld = session
        .diagnostics(&path)
        .expect("the file's own reading is untouched — it is the closure that is cold");
    assert!(
        withheld.checks_withheld,
        "the pull says why its checks are empty: {withheld:?}"
    );
    assert!(
        withheld.checks.is_empty(),
        "and it ran none of them: {:?}",
        withheld.checks
    );
}
/// **CUDA's vocabulary is offered in a CUDA project and nowhere else** — the completion half of the family switch.
///
/// `__global__` is an identifier to the lexer and a specifier to `nvcc`, so the list of what may be written at a
/// statement's start depends on the *project*, not on the file: the same text is a kernel definition to a CUDA
/// toolchain and a declaration of a variable called `__global__` to a C++ one. A list that offered the qualifiers
/// everywhere would be teaching a spelling no compiler on that project will accept.
///
/// Measured through the session rather than through the table, because the interesting question is what a reader sees:
/// `__dev` is a prefix, and the list has to filter on it, rank the namespace names around it, and stop at the
/// project boundary.
#[test]
pub(crate) fn cuda_words_are_offered_only_by_a_cuda_project() {
    let text = "void f() {\n    __dev\n}\n";
    let files = MemoryFiles::new().with_file("/p/a.cu", text);
    let fixture = Memory::new("cuda-completion", &files);
    let mut session =
        fixture.session_with(CompilerConfig::new().with_language_family(ProjectLanguage::Cuda));
    session.did_open("/p/a.cu", text);
    session.index_everything();

    let path = std::path::PathBuf::from("/p/a.cu");
    let view = session.view_of_the_file(&path).expect("a view");
    let at = text.find("__dev\n").expect("the fixture") + "__dev".len();
    let labels = |set: &CompletionSet| {
        set.items
            .iter()
            .map(|item| item.label.clone())
            .collect::<Vec<String>>()
    };

    let cuda = session.completions(&view, file::coordinates::FileOffset::new(at));
    let offered = labels(&cuda);
    assert!(
        offered.iter().any(|label| label == "__device__"),
        "a CUDA project completes `__device__`: {offered:?}"
    );

    // **The same text, read as C++.** The words are gone; what is left is whatever the file itself declares, which for
    // this fixture is nothing that starts with `__dev`.
    let mut session = fixture.session();
    session.did_open("/p/a.cu", text);
    session.index_everything();
    let view = session.view_of_the_file(&path).expect("a view");
    let cpp = session.completions(&view, file::coordinates::FileOffset::new(at));
    let offered = labels(&cpp);
    assert!(
        !offered.iter().any(|label| label.starts_with("__")),
        "a project that is not CUDA is not offered CUDA's vocabulary: {offered:?}"
    );
}

/// **Every kind of CUDA word completes**, not only the qualifiers: a reader typing `thr` wants `threadIdx`, and one
/// typing `float` wants `float4` beside `float`.
#[test]
pub(crate) fn the_built_in_variables_and_vector_types_complete_too() {
    let text = "void f() {\n    thr\n}\n";
    let files = MemoryFiles::new().with_file("/p/a.cu", text);
    let fixture = Memory::new("cuda-completion-kinds", &files);
    let mut session =
        fixture.session_with(CompilerConfig::new().with_language_family(ProjectLanguage::Cuda));
    session.did_open("/p/a.cu", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/a.cu");
    let view = session.view_of_the_file(&path).expect("a view");
    let at = text.find("thr\n").expect("the fixture") + "thr".len();
    let set = session.completions(&view, file::coordinates::FileOffset::new(at));
    let labels: Vec<&str> = set.items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        labels.contains(&"threadIdx"),
        "a built-in variable completes: {labels:?}"
    );
    assert!(
        labels.contains(&"threadIdx") && !labels.contains(&"ThreadIdx"),
        "and it is the spelling that is offered"
    );
}
/// **A module implementation unit sees the interface unit's declarations without importing it.**
///
/// `math.cpp` writes `module math;` (no `export`, no `import`) and defines `Vec2::length`, whose body reads `x_` — a
/// member only `math.cppm` declares. A compiler accepts that because a module implementation unit implicitly imports
/// its own module's interface unit, so the analysis has to model the same edge; it did not, and the reading of a real
/// project was measured: `x_` and `square(x_)` resolved to nothing, every hover in the file was empty, and a
/// completion inside a member function listed two hundred names, all of them macros and functions from the headers
/// the file includes and not one member of `Vec2`.
#[test]
pub(crate) fn an_implementation_unit_sees_its_interface_units_declarations() {
    let interface_text = "export module math;\n\
                          export struct Vec2 {\n\
                          \x20   double x_;\n\
                          \x20   double square(double value) const { return value * value; }\n\
                          \x20   double length() const;\n\
                          };\n";
    let unit_text = "module;\n\
                     #include <cmath>\n\
                     module math;\n\
                     double Vec2::length() const { return std::sqrt(square(x_)); }\n";

    let files = MemoryFiles::new()
        .with_file("/p/math.cppm", interface_text)
        .with_file("/p/math.cpp", unit_text);
    let fixture = Memory::new("implementation-unit", &files);
    let mut session = fixture.session();
    session.add_project_files([
        std::path::PathBuf::from("/p/math.cppm"),
        std::path::PathBuf::from("/p/math.cpp"),
    ]);
    session.did_open("/p/math.cpp", unit_text);
    session.index_everything();
    // **The read the server does before answering a name question**: `import`s, and now the implementation unit's own
    // interface.
    session.read_the_modules_a_file_imports(std::path::Path::new("/p/math.cpp"));
    session.index_everything();
    while session.pending_work() > 0 {
        session.advance(64);
    }

    let path = std::path::PathBuf::from("/p/math.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    // **`x_` is a member of `Vec2`**, and it is declared in the interface unit alone.
    let at = unit_text.find("x_)").expect("the fixture") + 1;
    let typed = session.type_at(&view, file::coordinates::ReadingOffset::new(at));
    assert!(
        matches!(&typed, Known::Yes(found) if found.type_of.contains("double")),
        "`x_` must have the type the interface unit gives it: {typed:?}"
    );

    // **And a member function of the class completes with its members**, not with the headers' macros.
    let at = unit_text.find("sqrt(").expect("the fixture") + "sqrt(".len();
    let members = session.member_completions(&view, file::coordinates::FileOffset::new(at));
    if let Known::Yes(members) = members {
        let names: Vec<&str> = members
            .members
            .members
            .iter()
            .map(|member| member.fact.name.as_str())
            .collect();
        assert!(
            names.contains(&"x_") || names.contains(&"square"),
            "a member of `Vec2` must be offered: {names:?}"
        );
    }
}
/// **A module's interface unit is a translation unit**: a header it includes can define what a later `#if` asks.
///
/// This is the shape GCC ships the standard module in, in miniature. `bits/std.cc` is `module;` +
/// `#include <bits/stdc++.h>` + `export module std;` + a few thousand `using std::x;` re-exports, and
/// `bits/stdc++.h`'s standard-header block is guarded by `#if _GLIBCXX_HOSTED` — a macro that `bits/c++config.h`
/// defines and that the headers *above* the block include first.
///
/// The module walk recorded *where* each include led and never folded a file's own definitions into its state, so
/// `_GLIBCXX_HOSTED` read as undefined, the block answered `Inactive`, and `#include <vector>` was dropped. Measured on
/// `test_module` with MinGW GCC 15: `include/c++/vector` was not in the visible set, `std::vector` had only the
/// `using std::vector;` re-export as its fact, and the member list was empty — "the class is out of the index".
#[test]
pub(crate) fn a_modules_includes_define_what_a_later_condition_asks() {
    let interface_text = "module;\n\
                          #include \"feature.h\"\n\
                          #include \"library.h\"\n\
                          export module std;\n\
                          export namespace std\n\
                          {\n\
                          \x20   using std::vector;\n\
                          }\n";
    let consumer_text = "import std;\nvoid f() { std::vector<int> v; v.push_back(1); }\n";

    let files = MemoryFiles::new()
        .with_file("/p/std.cppm", interface_text)
        .with_file("/p/feature.h", "#define FEATURE 1\n")
        .with_file(
            "/p/library.h",
            "#if FEATURE\n#include \"detail.h\"\n#endif\n",
        )
        .with_file(
            "/p/detail.h",
            "namespace std { template <class T> struct vector { void push_back(const T &value); }; }\n",
        )
        .with_file("/p/a.cpp", consumer_text);
    let fixture = Memory::new("a-modules-includes", &files);
    let mut session = fixture.session();
    session.add_project_files([
        std::path::PathBuf::from("/p/std.cppm"),
        std::path::PathBuf::from("/p/library.h"),
        std::path::PathBuf::from("/p/detail.h"),
        std::path::PathBuf::from("/p/a.cpp"),
    ]);
    session.did_open("/p/a.cpp", consumer_text);
    session.index_everything();
    // The read the server does before answering a name question, and the drain that follows it.
    session.read_the_modules_a_file_imports(std::path::Path::new("/p/a.cpp"));
    session.index_everything();
    while session.pending_work() > 0 {
        session.advance(64);
    }

    let path = std::path::PathBuf::from("/p/a.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    // **The class is what `std::vector` names**, not the re-export in the interface unit.
    let at = consumer_text.find("push_back").expect("the fixture") + 2;
    let member = session.member_completions(&view, file::coordinates::FileOffset::new(at));
    let Known::Yes(member) = member else {
        panic!("`v.` must be answered: {member:?}");
    };
    assert_eq!(
        member.class, "std::vector",
        "the object's type is the class the header declares"
    );
    assert_eq!(
        member.members.members.len(),
        1,
        "and its member list is the class's: {:?}",
        member
            .members
            .members
            .iter()
            .map(|member| member.fact.name.as_str())
            .collect::<Vec<_>>()
    );
}

/// **A cook that outlived the text it was made from is dropped.**
///
/// Reported from an editor, and the second half of "编辑不一定改变诊断": a `didChange` deleting a `;` was followed by
/// one pull, and the pull answered **`reading Cooked, 0 error(s)`** — a cooked reading of the *previous* text,
/// filed under the current one. The cause is the shape of cooking itself: it is two halves under two locks
/// (`render_a_cooked` then `commit_a_cooked`), and the pump claims a file, renders it in the background, and
/// commits — so an edit that lands in between is committed *over*. Nothing re-cooks a file that has a cooked
/// reading, which is what made it permanent rather than momentary.
///
/// The commit now compares the hash of the text the rendering was made from against the file's text, and this test
/// is that comparison driven by hand: claim, edit, render, commit.
#[test]
pub(crate) fn a_cook_of_text_that_has_since_changed_is_not_filed() {
    let files = MemoryFiles::new().with_file("/p/a.cpp", "int main() { return 0; }\n");
    let memory = Memory::new("stale-cook", &files);
    let mut session = memory.session();
    let path = PathBuf::from("/p/a.cpp");
    session.add_project_files([path.clone()]);
    session.load(&path);
    session.index_everything();

    // **The claim**, which is what the pump does before it renders: materials carry the text *and* its hash.
    let materials = session
        .cooking_materials(&path)
        .expect("the file has a summary and a unit");
    let rendered = session
        .render_a_cooked(&materials)
        .expect("the renderer reads the file's own text");

    // **The edit**, between the two halves — exactly where a reader's keystroke lands.
    let changed = "int main() { return 0 }\n";
    session.did_change(&path, changed);

    // The commit must refuse it: this rendering is of text nobody has any more.
    assert!(
        session.commit_a_cooked(rendered).is_none(),
        "a rendering of text that has since changed may not be filed under the new text"
    );
    assert!(
        session.diagnostics(&path).is_none(),
        "and the file has no cooked reading, so nothing answers with the old text's errors"
    );

    // **The honest path still works**: cook the new text and the answer is the new text's.
    session.did_change(&path, "int main() { return 0; }\n");
    session.advance(64);
    assert!(
        session.cook(&path).is_some(),
        "the pump re-cooks the file it dropped"
    );
    let found = session
        .diagnostics(&path)
        .expect("a cooked reading of the current text");
    assert_eq!(
        found.errors.len(),
        0,
        "the text has its `;` back: {:?}",
        found.errors
    );
}

/// **A pull is remembered for the reading it was made from, and an edit is not what it holds.**
///
/// A pull builds a `SemanticModel` and runs every rule — measured **143.4 ms** on `EmmyLuaCodeStyle`, 97 % of it type
/// inference — and the model cannot outlive the call (it borrows the tree and holds a non-`Send` cache of header views).
/// The findings can: they are plain data and a function of everything the pull reads, so the state is keyed and the
/// second pull of the same state is a lookup. What must never happen is the memo outliving an edit, which is what this
/// test is about.
#[test]
pub(crate) fn a_pull_is_remembered_until_the_file_changes() {
    let source = "int f() { int *p = 0; return 1; }\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let fixture = Memory::new("pull-memo", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", source);
    session.index_everything();

    let first = session
        .diagnostics("/p/a.cpp")
        .expect("the file reads")
        .checks
        .len();
    let again = session
        .diagnostics("/p/a.cpp")
        .expect("the file reads")
        .checks
        .len();
    assert_eq!(first, again, "the same reading answers the same");

    let edited = "int f() { int *p = 0; return 1; }\nint g() { int *q = 0; return 2; }\n";
    session.did_change("/p/a.cpp", edited);
    session.advance(4);
    let _ = session.cook("/p/a.cpp");
    let after = session
        .diagnostics("/p/a.cpp")
        .expect("the edited file reads")
        .checks
        .len();
    assert!(
        after > first,
        "an edit is seen, not the memory of it: {first} -> {after}"
    );
}

/// **A second pull reuses the inference the first one computed**, and the memo is keyed on the reading it was made from.
///
/// `TypeMemo` lives for one request — by design, since its answers are positions in one tree — so every pull re-derived
/// every type it asked about. The index keeps the answers across requests now (`ProjectIndex::type_answers`), and the
/// lifetime rule is the one the other memos have: an insert or a forget clears it. This test proves the reuse by
/// forgetting the **pull's own** memo between two pulls, so the second one really runs its checks again.
#[test]
pub(crate) fn a_second_pull_reuses_the_inference_of_the_first() {
    let source = "void f() {\n    int count = \"three\";\n    (void)count;\n}\n";
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let fixture = Memory::new("type-memo-across-pulls", &files);
    let mut session = fixture.session();
    session.did_open("/p/a.cpp", source);
    session.index_everything();

    let first = session.diagnostics("/p/a.cpp").expect("the file reads");
    assert!(!first.checks.is_empty(), "the fixture has a finding");
    let (hits_before, misses_before) = session.index().type_cache_stats();

    // The pull's own memo, gone: the next pull recomputes its checks and can only be fast through the type memo.
    *session
        .checks_memo
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    let second = session.diagnostics("/p/a.cpp").expect("the file reads");
    let (hits_after, misses_after) = session.index().type_cache_stats();

    assert_eq!(
        first.checks.len(),
        second.checks.len(),
        "the same reading answers the same"
    );
    assert!(
        hits_after > hits_before,
        "the second pull answered type questions from the memo: {hits_before} -> {hits_after} hit(s), \
         {misses_before} -> {misses_after} computed"
    );
}

/// **A parameter of an out-of-class member definition is a declaration in its body** — two reports, one root cause.
///
/// `void Archive::put(const Entry& entry) { counts += entry.key; }` and `void Report::add(int line) { lines(…) }` are
/// the shapes a real project writes, and both were reported by a reader: `entry` and `line` "are not declared", and
/// hovering `line` answered `std::source_location::line` — a *namespace* member found because the parameter had no
/// binding of its own.
///
/// The qualified declarator is the common factor: `declared_name` refuses a qualified name on purpose (a `count` bound
/// in this file's scope would be a name that cannot be used unqualified), and the question this test asks is whether
/// that refusal also dropped the **parameters**, which are in scope in the body regardless of how the function is
/// spelled.
#[test]
pub(crate) fn a_parameter_of_an_out_of_class_definition_is_declared() {
    let text = "namespace report {\n\
                \x20   struct Entry { int key; int count; };\n\
                \x20   class Archive {\n\
                \x20   public:\n\
                \x20       void put(const Entry& entry);\n\
                \x20   private:\n\
                \x20       int counts;\n\
                \x20   };\n\
                \x20   void Archive::put(const Entry& entry) {\n\
                \x20       counts += entry.key;\n\
                \x20   }\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/archive.cpp", text);
    let fixture = Memory::new("a-parameter-of-an-out-of-class-definition", &files);
    let mut session = fixture.session();
    session.did_open("/p/archive.cpp", text);
    session.index_everything();
    let path = std::path::PathBuf::from("/p/archive.cpp");
    let view = session.view_of_the_file(&path).expect("a view");

    // **The use, not the declaration**: `entry` in `entry.key`, inside the body.
    let at = text.find("entry.key").expect("the fixture") + 1;
    let found = session.definition(&view, ReadingOffset::new(at));
    assert!(
        matches!(found, Known::Yes(_)),
        "the body's `entry` is the parameter: {found:?}"
    );
    let Known::Yes(found) = found else {
        unreachable!()
    };
    assert_eq!(
        found.fact.name.as_str(),
        "entry",
        "and it is the parameter, not a type or a namespace member: {:?}",
        found.fact
    );
}

/// **A member defined in a namespace, whose class is in another file, still declares itself** — the fact Go to
/// Implementation scans for.
///
/// `report.cpp` (a module implementation unit) is the shape: `namespace report { void Report::add(std::string line)
/// { … } }`, with `Report` declared in `report.cppm`. Two rules stood in the way and both were the same mistake, made in
/// two places: "a qualified declaration inside a `CompoundStat` declares nothing", written for a *statement* — and a
/// namespace body is a `CompoundStat`, so the whole definition was skipped:
///
/// * the scope walker opened no function scope and bound no parameters (the previous round's fix);
/// * `qualified_declarations`, the pass that builds a fact from the written qualifier, skipped it too — so the file
///   had **no `Function` fact at all** and Go to Implementation from the interface found nothing.
///
/// And the scope has to be the *fully* qualified class: a definition inside `namespace report` spells `Report`, while
/// the interface files the declaration under `report::Report`.
#[test]
pub(crate) fn a_member_defined_inside_a_namespace_declares_itself() {
    let text = "namespace report {\n\
                \x20   void Report::add(int line) {\n\
                \x20       line = line + 1;\n\
                \x20   }\n\
                }\n";
    let files = MemoryFiles::new().with_file("/p/report.cpp", text);
    let fixture = Memory::new("a-member-defined-inside-a-namespace", &files);
    let mut session = fixture.session();
    session.did_open("/p/report.cpp", text);
    session.index_everything();

    let summary = session
        .parsed_summary(Path::new("/p/report.cpp"))
        .expect("the file was read");
    let facts: Vec<(String, Option<String>)> = summary
        .declarations
        .iter()
        .filter(|fact| fact.name.as_str() == "add")
        .map(|fact| {
            (
                format!("{:?}", fact.kind),
                fact.scope.as_ref().map(|scope| scope.to_string()),
            )
        })
        .collect();
    assert!(
        facts
            .iter()
            .any(|(kind, scope)| kind == "Function" && scope.as_deref() == Some("report::Report")),
        "the definition is a `Function` in `report::Report`: {facts:?}"
    );

    // **And it is still not a name this file can use unqualified** — the rule the qualified pass exists to respect.
    let view = session
        .view_of_the_file(Path::new("/p/report.cpp"))
        .expect("a view");
    let at = text.find("line + 1").expect("the fixture");
    assert!(
        matches!(
            session.definition(&view, ReadingOffset::new(at)),
            Known::Yes(_)
        ),
        "the parameter is still declared"
    );
}
