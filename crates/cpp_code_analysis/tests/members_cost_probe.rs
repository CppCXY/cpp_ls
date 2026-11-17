//! **What a member list of an uninstantiated class template costs** — the question the inlay-hint hang left open.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "C:\Users\zc\Desktop\test_cpp"
//! cargo test --release -p cpp_code_analysis --test members_cost_probe -- --ignored --nocapture
//! ```
//!
//! # Why
//!
//! `textDocument/inlayHint` on `containers.cpp` took **22 seconds and two gigabytes**, and the request was a braced
//! initialisation (`std::tuple tuple{1, 2.5, std::string{"three"}}`) whose deduction asked for the members of a class
//! template **with no arguments**. Consulting the standard's guide before that walk fixed the request (154 ms, 132 MB),
//! but it does not answer whether the walk itself is bounded: any other query that asks for `std::tuple`'s members —
//! a hover on a bare `std::tuple`, a member completion — goes to the same place.
//!
//! So this probe asks the question directly, with the clock and the allocation counter around it, and prints what came
//! back. A list that arrives in milliseconds is a list; one that grows without end is an infinite base walk, and the
//! spelling each level substitutes (`_Tuple_impl<_Idx + 1, …>` with `_Idx = 0` → `_Tuple_impl<0 + 1, …>` → …) is
//! never equal to the last, so a walk that terminates only on a repeated spelling never terminates at all.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\test_cpp".to_string()),
    )
}

fn file(root: &Path) -> PathBuf {
    root.join(
        std::env::var("CPPLS_MEMBERS_FILE").unwrap_or_else(|_| "src/containers.cpp".to_string()),
    )
}

#[test]
#[ignore = "a probe: opens a real project, asks for a member list, and prints instead of asserting"]
fn what_a_member_list_of_a_class_template_costs() {
    let root = project();
    let entry = file(&root);
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([entry.clone()]);
    session.index_everything();
    while session.pending_work() > 0 {
        session.advance(64);
    }
    let text = std::fs::read_to_string(&entry).expect("the entry file is readable");
    session.did_open(&entry, &text);
    // **Bounded, because `advance` reads whole programs too** — a probe that asks one member question must not spend
    // its time on seventeen four-second program reads first. A member list needs the index's summaries, which the
    // first slices build; what is left after that is the cooked pass, and it is not needed here.
    let mut slices = 0usize;
    while session.pending_work() > 0 && slices < 120 {
        session.advance(64);
        slices += 1;
    }
    println!(
        "indexed in {slices} slice(s), {} work item(s) left",
        session.pending_work()
    );

    // **A file named here is cooked before the members are asked for** — the half the server's pump does and this probe
    // did not, so a member list built from the *raw* reading can be told from one built from the compiler's: a header
    // whose braces only balance per `#if` branch (`<variant>`) files its class under the enclosing namespace of whichever
    // branch the raw text leaves open.
    if let Ok(named) = std::env::var("CPPLS_MEMBERS_COOK")
        && !named.is_empty()
    {
        let cooked = session.cook(&named);
        println!(
            "cooked {named}: {} declaration(s), {}{}",
            cooked.as_ref().map_or(0, |reading| reading.declarations),
            if cooked.is_some() {
                "has a reading"
            } else {
                "NO reading"
            },
            ""
        );
    }

    let Some(view) = session.view_of_the_file(&entry) else {
        panic!("the entry file has a view");
    };
    let asked = [
        std::env::var("CPPLS_MEMBERS_CLASS").unwrap_or_else(|_| "std::tuple".to_string()),
        "std::pair".to_string(),
        "std::string".to_string(),
        "std::vector<int>".to_string(),
    ];
    for class in asked {
        let live = cpp_code_analysis::alloc::live();
        let churn = cpp_code_analysis::alloc::total();
        let started = Instant::now();
        let members = session.members_of(&view, &class);
        let (count, gaps) = members
            .clone()
            .value()
            .map(|list| (list.members.len(), list.unlisted.len()))
            .unwrap_or((0, 0));
        // **A name that appears twice is the reported defect**: `vector.begin` hovered with four declarations, two of
        // them the same pair again. Printed here, with the file and offset of each, because a duplicate that differs by
        // its offset is two *readings* of one file and one that agrees on both is one fact collected twice.
        if let Known::Yes(list) = &members
            && let Ok(wanted) = std::env::var("CPPLS_MEMBERS_NAME")
            && !wanted.is_empty()
        {
            for member in list
                .members
                .iter()
                .filter(|member| member.fact.name == wanted.as_str())
            {
                println!(
                    "    {wanted} at {}:{} (declared in {}), depth {}",
                    member.file.display(),
                    member.fact.name_range.start_offset,
                    member.declared_in,
                    member.depth
                );
            }
        }
        println!(
            "{class:<20} {:>9.1} ms  {:>4} member(s), {} unnamed base(s)  live {:>5} MB -> {:>5} MB  churn {:>6} MB",
            started.elapsed().as_secs_f64() * 1000.0,
            count,
            gaps,
            live / (1024 * 1024),
            cpp_code_analysis::alloc::live() / (1024 * 1024),
            cpp_code_analysis::alloc::total().saturating_sub(churn) / (1024 * 1024),
        );
    }
}
