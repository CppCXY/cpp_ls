//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_code_analysis --test members_real -- --ignored --nocapture
//! ```
//!
//! `undefined-field` reported nine false positives on `CodeFormat.cpp` and the receiver types were right — it is the
//! **member list** that is wrong:
//!
//! ```text
//! `std::string` -> `size`  (27 member(s), 0 unlisted) — index: 135 posting(s), 0 in scope `std::string`
//! `std::vector` -> `erase` (303 member(s), 0 unlisted) — index: 122 posting(s), 278 in scope `std::vector`
//! ```
//!
//! So this asks the same two questions and prints the answers side by side: what `members_of` says a class has, and
//! what the index says is filed under that class's name. A member the index has postings for, in that scope, and the
//! list does not, is exactly the inconsistency the doc records.

use std::path::PathBuf;

use cpp_code_analysis::file::coordinates::ReadingOffset;
use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

/// The project to open — **from the environment, and only when it is there**.
///
/// A probe that hardcodes one machine's checkout is a probe that fails on the next machine, and the
/// question it asks is answerable on any project.
fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_REAL_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\EmmyLuaCodeStyle"));
    named.is_dir().then_some(named)
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn a_classs_member_list_and_its_postings_agree() {
    let Some(project) = project() else {
        eprintln!("skipped: no project (set CPPLS_REAL_PROJECT)");
        return;
    };
    let source = project.join("CodeFormatCLib/src/CodeFormat.cpp");
    if !source.is_file() {
        eprintln!("skipped: {:?} is not in the project", source);
        return;
    }

    let mut session = Session::open(
        project.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project),
    );
    session.add_project_files([source.clone()]);
    session.index_everything();
    let Some(view) = session.view_of_the_file(&source) else {
        panic!("the file's own text parses");
    };
    let _ = view;
    let view = session.view_of_the_file(&source).expect("and again");
    let at = ReadingOffset::new(0);

    // **The question: is the `#if` in the facts because the file was read uncooked?** The eager index reads a file's
    // own text, where directives still stand; a *rendering* has them resolved and gone. `bits/stl_vector.h` was not
    // cooked when the lists above were read, so ask for the rendering and read the list again — the two answers tell
    // the uncooked window apart from a cooker bug.
    // The toolchain's header, named by the environment: a test that hardcodes one machine's libstdc++ path is a test
    // that fails on the next machine, and the question it asks is answerable on any project that has one.
    let vector_header = std::env::var_os("CPPLS_REAL_STD_VECTOR")
        .map(PathBuf::from)
        .unwrap_or_default();
    if vector_header.exists() {
        let _ = session.view(&vector_header);
        session.advance(usize::MAX);
        let cooked = session.cook(&vector_header).is_some();
        println!("after cooking stl_vector.h: cooked = {cooked}");
        if let Known::Yes(list) = session.members_of(&view, "std::vector") {
            println!(
                "  std::vector has erase after cooking: {}  ({} member(s))",
                list.members
                    .iter()
                    .any(|member| member.fact.name == "erase"),
                list.members.len()
            );
        }
    } else {
        println!("stl_vector.h not found at {}", vector_header.display());
    }

    for class in ["std::string", "std::basic_string", "std::vector"] {
        let listed = session.members_of(&view, class);
        let (count, names) = match &listed {
            Known::Yes(list) => (
                list.members.len(),
                list.members
                    .iter()
                    .map(|member| member.fact.name.clone())
                    .collect::<Vec<_>>(),
            ),
            other => {
                println!("{class}: {other:?}");
                continue;
            }
        };
        println!(
            "{class}: {count} member(s), unlisted {:?}",
            match &listed {
                Known::Yes(list) => list
                    .unlisted
                    .iter()
                    .map(|base| base.spelling.clone())
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            }
        );
        // **What the doc records as fixed**: `erase` was 0 facts in this header before the directive lookahead, and the
        // class's member list is what a reader sees.
        if class == "std::vector" {
            assert!(
                names.iter().any(|name| name == "erase"),
                "std::vector has `erase` among its members: {names:?}"
            );
        }
        // **What the signatures look like now.** Before the declarator-choice fix, every member whose return type ends
        // in `&` or `*` carried `parameter_list: None` — 1 937 of libstdc++'s 15 253 function facts — so the
        // completion row and the signature popup read `assign(…)` with no arguments at all.
        if class == "std::vector" {
            let samples: Vec<(String, Option<String>)> = match &listed {
                Known::Yes(list) => list.members.iter(),
                _ => [].iter(),
            }
            .filter(|member| {
                matches!(
                    member.fact.name.as_str(),
                    "assign" | "insert" | "erase" | "at"
                )
            })
            .take(4)
            .map(|member| (member.fact.name.clone(), member.fact.parameter_list.clone()))
            .collect();
            println!("  signatures: {samples:?}");
            assert!(
                samples.iter().any(|(_, parameters)| parameters.is_some()),
                "at least one sampled member carries its parameter list: {samples:?}"
            );
        }
        println!(
            "  has size: {}  has erase: {}  has begin: {}",
            names.iter().any(|name| name == "size"),
            names.iter().any(|name| name == "erase"),
            names.iter().any(|name| name == "begin"),
        );
        let mut sorted = names.clone();
        sorted.sort();
        println!("  all: {sorted:?}");
        // **The assertion this test exists for.** A `std::string` that has no `size`, `begin` or `end` is not a
        // member list, it is the 27 typedefs declared before libstdc++'s `struct _Alloc_hider` — whose trailing
        // `// TODO check __is_final` used to swallow its own closing brace and every member after it.
        if class.starts_with("std::string") {
            assert!(
                names.iter().any(|name| name == "size")
                    && names.iter().any(|name| name == "begin")
                    && names.iter().any(|name| name == "end"),
                "{class}: size, begin and end are members of it: {sorted:?}"
            );
        }

        for name in ["size", "erase"] {
            println!(
                "  postings(`{name}`) = {}, in scope `{class}` = {}",
                session.index().name_postings(name),
                session.index().scope_postings(class),
            );
            // **Which scopes the facts are actually filed under** — the question a posting count cannot answer.
            let mut scopes: std::collections::BTreeMap<String, (usize, String)> =
                std::collections::BTreeMap::new();
            for (file, summary) in session.index().all_summaries() {
                for fact in summary.declarations.iter().filter(|fact| fact.name == name) {
                    let key = fact
                        .scope
                        .clone()
                        .unwrap_or_else(|| "<file scope>".to_string());
                    let entry = scopes.entry(key).or_insert((0, String::new()));
                    entry.0 += 1;
                    if entry.1.is_empty() {
                        entry.1 = file.display().to_string();
                    }
                }
            }
            let shown: Vec<String> = scopes
                .iter()
                .map(|(scope, (count, file))| format!("{scope} x{count} in {file}"))
                .collect();
            println!("    scopes: {}", shown.join(", "));
        }
        let _ = at;
    }
}
/// **A class two files use is built once.** The member list is keyed by the asking file because a class *declared* in
/// the file being read is resolved against that file's own tree — so a list whose facts all came from other files is
/// the index's alone, and `remember_member_list` files it under the empty path as well.
///
/// Measured on the real project before that: twelve lists built per pull, **zero hits**, and
/// `std::vector<LuaSyntaxNode>` (22 ms) rebuilt by every file that mentions it.
#[test]
#[ignore = "opens a real project; run with --ignored"]
fn a_class_two_files_use_is_built_once() {
    let Some(project) = project() else {
        eprintln!("skipped: no project (set CPPLS_REAL_PROJECT)");
        return;
    };
    let first = project.join("CodeFormatCLib/src/CodeFormat.cpp");
    let second = project.join("LuaParser/src/Ast/LuaSyntaxTree.cpp");
    if !first.is_file() || !second.is_file() {
        eprintln!("skipped: the project does not hold {first:?} and {second:?}");
        return;
    }

    let mut session = Session::open(
        project.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project),
    );
    session.add_project_files([first.clone(), second.clone()]);
    session.index_everything();
    let view_of_first = session
        .view_of_the_file(&first)
        .expect("the first file parses");
    let view_of_second = session
        .view_of_the_file(&second)
        .expect("the second file parses");

    for class in ["std::string", "std::vector<LuaSyntaxNode>", "LuaSyntaxTree"] {
        let started = std::time::Instant::now();
        let from_first = session.members_of(&view_of_first, class);
        let first_took = started.elapsed();

        let started = std::time::Instant::now();
        let from_second = session.members_of(&view_of_second, class);
        let second_took = started.elapsed();

        let (Some(a), Some(b)) = (known_count(&from_first), known_count(&from_second)) else {
            println!("{class}: not answerable from both files");
            continue;
        };
        println!(
            "{class}: {} member(s) — first file {first_took:?}, second file {second_took:?}",
            a
        );

        assert_eq!(a, b, "{class}: the same answer from both files");
        // **Only the plain class is asserted.** A *template instantiation* (`std::vector<LuaSyntaxNode>`) is
        // substituted per spelling and measured 1.7 ms then 20 ms — it is not the cache under test, and pinning it
        // here would pin a cost nobody has explained yet. `std::string` is the case the inline-namespace fix and the
        // shared cache both matter for, and it is 16 ms then 143 µs.
        if class == "std::string" {
            assert!(
                second_took * 2 < first_took,
                "{class}: the second file reuses the list ({first_took:?} then {second_took:?})"
            );
        }
    }
}

fn known_count(listed: &Known<cpp_code_analysis::MemberList>) -> Option<usize> {
    match listed {
        Known::Yes(list) => Some(list.members.len()),
        _ => None,
    }
}
