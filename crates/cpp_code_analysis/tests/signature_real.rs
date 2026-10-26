//! **Does a real call site answer a signature?** — `docs/lsp-status.md` P2 ⑤, asked at the analysis layer.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_code_analysis --test signature_real -- --ignored --nocapture
//! ```
//!
//! The handler answered `null` for every call site on the real project while the fixture tests passed, so the split is
//! either the positions the handler builds or the query itself. This asks the query directly, with a `Session` over the
//! disk and the file's own text — the same view the handler reads its cursor from.
//!
//! # Two ways to ask this question wrongly, both made here first
//!
//! * **A "corrected" offset.** The file is CRLF and the VFS keeps the bytes it is given, so the raw byte offset *is*
//!   the view's offset; subtracting one byte per preceding line pointed 82 bytes past the call.
//! * **An offset on the callee.** `signatures_at` documents an offset **inside the argument list**: on `SetupStyle` the
//!   answer is "no call", and it is the right answer — the query is about which parameter is being typed.

use std::path::PathBuf;

use cpp_code_analysis::file::coordinates::ReadingOffset;
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

/// The project to open — **from the environment, and only when it is there**.
///
/// A probe that hardcodes one machine's checkout fails on the next machine; `None` means "no project here", which the
/// test turns into a skip. See `docs/remaining.md` §C1.
fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_REAL_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\EmmyLuaCodeStyle"));
    named.is_dir().then_some(named)
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn a_call_site_answers_a_signature() {
    let Some(project) = project() else {
        eprintln!("skipped: no project (set CPPLS_REAL_PROJECT)");
        return;
    };
    // **Two files, because the two shapes are in different ones**: `CodeFormat.cpp` has the qualified member
    // definition's call, and `LuaLexer.cpp` has the constructor call whose class's header the session never cooks.
    let needles = vec![
        ("SetupStyle(style, options)", "a call to a static member"),
        (
            "LuaParser p(file, std::move",
            "a constructor whose class lives in a header",
        ),
    ];
    {
        let source = project.join("CodeFormatCLib/src/CodeFormat.cpp");
        if !source.is_file() {
            eprintln!("skipped: the project does not hold {source:?}");
            return;
        }
        let text = std::fs::read_to_string(&source).expect("the file reads");

        let mut session = Session::open(
            project.clone(),
            SessionFiles::new(OpenDocuments::new(), DiskFiles),
            WatchFilter::new(&project),
        );
        session.add_project_files([source.clone()]);
        session.index_everything();

        let Some(written) = session.view_of_the_file(&source) else {
            panic!("the file's own text parses");
        };

        let mut asked = 0;
        for (needle, what) in needles {
            let Some(open) = text
                .find(needle)
                .map(|at| at + needle.find('(').unwrap_or(0))
            else {
                println!("{what}: not in the file");
                continue;
            };
            asked += 1;
            let inside = open + 1;
            println!(
                "{what}: `{needle}`'s `(` at {open}, asking one past it → {:?}",
                written
                    .source
                    .get(inside..(inside + 24).min(written.source.len()))
            );

            // **What the two steps inside the query say**, because "no signature" has two shapes here: the name is not
            // resolved, or it is resolved to something that has no parameters. The offset is the *start* of the callee's
            // last identifier — `end_of_the_written_name`'s own rule, and passing the `(` was this test's third mistake.
            let callee_start = open - needle.split('(').next().unwrap_or(needle).len();
            println!(
                "  definitions_at(`{}` at {callee_start}) = {:?}",
                what,
                cpp_code_analysis::sema::resolve::definitions_at(
                    &written.scopes,
                    &written.root,
                    callee_start
                )
                .map(|bindings| bindings
                    .iter()
                    .map(|binding| (binding.name.text(), format!("{:?}", binding.kind)))
                    .collect::<Vec<_>>())
            );
            // **And whether the declaring file has a reading**, which is what a signature's label is read from.
            let header = source.with_file_name("codeformat.h");
            println!(
                "  the header has a rendering: {}",
                session.view(&header).is_some()
            );
            let spelling: String = needle.split('(').next().unwrap_or(needle).to_string();
            println!(
                "  the index's definitions(`{spelling}`) = {} answer(s)",
                match session.index().definitions(&spelling, &source) {
                    cpp_code_analysis::Known::Yes(found) => found.found.len(),
                    _ => 0,
                }
            );

            // **What the index holds for the callee**, which is where a signature comes from when the declaring header has
            // no rendering: `parameter_list` is the fact's copy of the declaration's own list.
            match session.index().definitions(&spelling, &source) {
                cpp_code_analysis::Known::Yes(found) => {
                    for declaration in &found.found {
                        println!(
                            "  fact: kind {:?} name {:?} parameter_list {:?} in {}",
                            declaration.fact.kind,
                            declaration.fact.name,
                            declaration.fact.parameter_list,
                            declaration.file.display()
                        );
                    }
                }
                other => println!("  no facts: {other:?}"),
            }

            let signatures = session.signatures_at(&written, ReadingOffset::new(inside));
            println!("  {} signature(s)", signatures.len());
            for signature in signatures.iter().take(3) {
                println!("    {}", signature.label);
            }
            // **The one shape this test pins.** A call to a member declared in a header must answer even though the header
            // has no rendering: the label comes from the fact, and before the fix the candidate was dropped in silence.
            if what == "a call to a static member" {
                assert!(
                    signatures
                        .iter()
                        .any(|signature| signature.label.contains("LuaStyle &style")),
                    "{what}: the fact's parameter list is the label: {signatures:?}"
                );
            }
            // The constructor is **not** pinned: `LuaParser p(file, std::move(...))` resolves to the class, and its
            // constructors are found through `constructors_of`, which needs the class's own file — another day's work.
        }
        assert!(asked > 0, "the fixture names a call");
    }
}
