//! **One class, two answers** — `docs/lsp-status.md` P2 ⑥, asked of the real project.
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

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT").unwrap_or_else(|_| r"E:\EmmyLuaCodeStyle".to_string()),
    )
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn a_classs_member_list_and_its_postings_agree() {
    let project = project();
    let source = project.join("CodeFormatCLib/src/CodeFormat.cpp");

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
                    let key = fact.scope.clone().unwrap_or_else(|| "<file scope>".to_string());
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
