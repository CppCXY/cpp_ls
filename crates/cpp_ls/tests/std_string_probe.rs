//! **What the member list answers for `std::string`'s `operator[]`** — the fact the type layer reads.
//!
//! ```text
//! cargo test --release -p cpp_ls --test std_string_probe -- --ignored --nocapture
//! ```
//!
//! `s[0]` on a `std::string&` answers `_Elem&` where a compiler answers `char&` (measured with `inspect type`), and the
//! question is which layer loses the substitution: the member list of `std::string` (an alias of `basic_string<char,
//! …>`, so the class's parameters have to be substituted from the alias's arguments) or the type layer that reads it.

use std::path::PathBuf;

use cpp_code_analysis::file::paths::DiskFiles;
use cpp_code_analysis::index::watch::WatchFilter;
use cpp_code_analysis::session::{OpenDocuments, Session, SessionFiles};

#[test]
#[ignore = "a probe: opens a real project and prints the member fact the type layer reads"]
fn what_the_member_list_says_about_strings_subscript() {
    let root = PathBuf::from(
        std::env::var("CPPLS_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\github\EmmyLuaCodeStyle".to_string()),
    );
    let file = root.join("CodeFormatServer/src/LSP/LSP.cpp");
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("skipped: {} is not here", file.display());
        return;
    };
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, &text);
    let _ = session.index_everything();
    let Some(view) = session.view_of_the_file(&file) else {
        eprintln!("no view");
        return;
    };

    for class in ["std::string", "std::basic_string<char>", "std::vector<int>"] {
        let started = std::time::Instant::now();
        let listed = session.members_of(&view, class);
        let cost = started.elapsed();
        println!("[members] {class}: {cost:?} for one list");
        match listed {
            cpp_code_analysis::Known::Yes(listed) => {
                println!(
                    "[members] {class}: {} member(s), {} overload(s) of operator[]",
                    listed.members.len(),
                    listed
                        .members
                        .iter()
                        .filter(|member| member.fact.name == "operator[]")
                        .count()
                );
                for member in listed
                    .members
                    .iter()
                    .filter(|member| member.fact.name == "operator[]")
                {
                    println!(
                        "  returns {:?}, in the class {:?}, parameters {:?}, const {}",
                        member.fact.returns,
                        member.fact.returns_in_the_class(),
                        member.fact.parameters(),
                        member
                            .fact
                            .modifiers
                            .contains(cpp_code_analysis::DeclModifiers::CONST)
                    );
                }
                for name in ["reference", "const_reference", "value_type"] {
                    if let Some(alias) = listed
                        .members
                        .iter()
                        .find(|member| member.fact.name == name)
                    {
                        println!("  {name}: type_of {:?}", alias.fact.type_of);
                    }
                }
            }
            other => println!("[members] {class}: {other:?}"),
        }
    }
}
