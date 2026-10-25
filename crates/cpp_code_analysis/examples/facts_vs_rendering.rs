//! **What a declaration's return type is, written text against rendering** — the two readings, asked about the same
//! names.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo run --release -p cpp_code_analysis --example facts_vs_rendering -- <file> [<file> …]
//! ```
//!
//! The eager index reads a file's **own text**, where a directive is still a directive. `bits/stl_bvector.h` writes
//! `emplace_back`'s return type as
//!
//! ```text
//! #if __cplusplus > 201402L
//!     _GLIBCXX20_CONSTEXPR
//!     reference
//! #else
//!     void
//! #endif
//!     emplace_back(_Args&&... __args)
//! ```
//!
//! so the nearest type to the name is `void` — the answer a reader hovering `_configs.emplace_back(` was shown, from a
//! C++20 project where the function returns `reference`. The **rendering** removes the inactive branch before the
//! parse and the same declaration reads `reference`.
//!
//! This probe counts how often the two readings disagree about a name, which is the size of the problem rather than an
//! anecdote about one function.

use std::collections::HashMap;
use std::path::PathBuf;

use cpp_code_analysis::{
    summarize, DiskFiles, OpenDocuments, Session, SessionFiles, SummaryKey, WatchFilter,
};

fn main() {
    let root = PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT").unwrap_or_else(|_| r"E:\EmmyLuaCodeStyle".to_string()),
    );
    let files: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if files.is_empty() {
        eprintln!("usage: facts_vs_rendering <file> [<file> …]");
        std::process::exit(2);
    }

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(files.iter().cloned());
    session.index_everything();

    let mut total = 0usize;
    let mut differing = 0usize;
    let mut deferred = 0usize;
    let mut guarded = 0usize;
    for file in &files {
        let Some(rendered) = session.rendering_of(file) else {
            println!("{}: no rendering", file.display());
            continue;
        };
        let Ok(written_text) = std::fs::read_to_string(file) else {
            println!("{}: not readable", file.display());
            continue;
        };
        let written = summarize(file, &written_text, SummaryKey::new(0, 0));
        let read = summarize(file, &rendered.text, SummaryKey::new(0, 0));

        // **By name and scope**, because the two readings number their offsets separately.
        let key = |fact: &cpp_code_analysis::DeclFact| {
            (
                fact.name.clone(),
                fact.scope.as_ref().map(|scope| scope.to_string()),
            )
        };
        let mut from_the_rendering: HashMap<_, Vec<&cpp_code_analysis::DeclFact>> = HashMap::new();
        for fact in &read.declarations {
            from_the_rendering.entry(key(fact)).or_default().push(fact);
        }

        let mut local_differing = Vec::new();
        for fact in &written.declarations {
            let Some(candidates) = from_the_rendering.get(&key(fact)) else {
                continue;
            };
            // **A local has no identity across the two readings**: its `scope` is `None` (a body contributes no
            // qualified name), so two different functions' `__s` match each other and the comparison says nothing. The
            // question this probe asks is about declarations that a lookup can name.
            if fact.local {
                continue;
            }
            // **A fact inside a conditional region is not a contradiction**: the file writes both branches and *which
            // one is compiled* is the configuration's answer, so the summary honestly holds both (a lookup filters the
            // dead one, which `tests/rendering_real.rs::a_dead_branch_is_not_an_answer` asserts).
            if fact.guard != cpp_code_analysis::FactGuard::Unconditional {
                guarded += 1;
                continue;
            }
            total += 1;
            // A name may be overloaded; the written fact **contradicts** the rendering only if no reading of that name
            // agrees with it. A written `None` is not a contradiction: it is the fact declining to name a branch — see
            // `declarations.rs`, where a specifier sequence split by a directive states no single type.
            if candidates.iter().any(|other| other.returns == fact.returns) {
                continue;
            }
            if fact.returns.is_none() {
                deferred += 1;
                continue;
            }
            differing += 1;
            local_differing.push((
                fact.name.clone(),
                fact.returns.clone(),
                candidates.first().and_then(|other| other.returns.clone()),
            ));
        }
        println!(
            "{}: {} written fact(s), {} disagree with the rendering about the return type",
            file.file_name().unwrap_or_default().to_string_lossy(),
            written.declarations.len(),
            local_differing.len()
        );
        for (name, written, rendered) in local_differing.iter().take(10) {
            println!("  {name}: written {written:?} → rendering {rendered:?}");
        }
    }
    println!(
        "totals: {total} fact(s) matched by name and scope, **{differing}** contradict the rendering about the \
         return type, {deferred} decline to name one (a directive splits the declaration), {guarded} are branch-guarded"
    );
}
