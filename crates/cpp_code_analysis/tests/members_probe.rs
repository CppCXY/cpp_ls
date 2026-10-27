//! **One class, asked what it holds** — the probe the anonymous-`struct` question needs.
//!
//! ```text
//! $env:CPPLS_PROJECT_ROOT = "E:\neovim"
//! $env:CPPLS_FILE         = "E:\neovim\src\nvim\api\private\helpers.c"     # any file of the project
//! $env:CPPLS_CLASS        = "Object"
//! cargo test --release -p cpp_code_analysis --test members_probe -- --ignored --nocapture
//! ```
//!
//! # Why this exists next to `resolution_rate`
//!
//! The scoreboard answers "how many member questions resolve" over a whole project and takes minutes. When one of its
//! buckets names a class — "a list from **no visible declaration**" was 21 539 of 40 000 refusals on `neovim` — the next
//! question is about that class, and re-running the project to ask it is the wrong shape. This asks it in one index.
//!
//! What it prints is the four answers that separate the causes:
//!
//! ```text
//! the facts filed under the spelling       (scope postings)
//! what `members_of` answers, and its size
//! how many declarations are *visible* from the asking file
//! what the class's own declaration says its type is  (the `typedef struct { … } Name;` shape)
//! ```

use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn what_one_class_answers_for_its_members() {
    let root = PathBuf::from(
        std::env::var("CPPLS_PROJECT_ROOT").expect("CPPLS_PROJECT_ROOT names the project"),
    );
    let file = PathBuf::from(std::env::var("CPPLS_FILE").expect("CPPLS_FILE names a file in it"));
    let asked = std::env::var("CPPLS_CLASS").expect("CPPLS_CLASS names the class");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let started = std::time::Instant::now();
    session.add_project_files([file.clone()]);
    session.index_everything();
    while session.pending_work() > 0 {
        session.advance(64);
    }
    println!(
        "indexed {} file(s) in {:?}",
        session.cache_sizes().summaries,
        started.elapsed()
    );

    let view = session.view_of_the_file(&file).expect("a view of the asking file");
    println!(
        "\n{asked}: {} fact(s) filed under that scope, {} visible from {}",
        session.index().scope_postings(&asked),
        session.index().declarations_in(&asked, &file).len(),
        file.display()
    );

    // **The class's own declaration**, which is where a `typedef struct { … } Name;` keeps its answer: the members of
    // an anonymous struct are declared in a scope with no name, and the typedef is the only thing that names them.
    for (where_it_is, fact) in session.index().every_fact_named(&asked) {
        println!(
            "  declared in {}: kind {:?} scope {:?} type_of {:?}",
            where_it_is.file_name().unwrap_or_default().to_string_lossy(),
            fact.kind,
            fact.scope,
            fact.type_of
        );
    }

    match session.members_of(&view, &asked) {
        Known::Yes(list) => {
            println!("members_of: {} member(s)", list.members.len());
            for member in list.members.iter().take(30) {
                println!(
                    "  {:?} {:?} scope {:?} type {:?}",
                    member.fact.kind, member.fact.name, member.fact.scope, member.fact.type_of
                );
            }
        }
        Known::Unknown(reason) => println!("members_of: no list, {reason:?}"),
        Known::No => println!("members_of: no list"),
    }
}
