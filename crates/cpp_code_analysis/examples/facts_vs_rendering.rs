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
    let mut unmatchable = 0usize;
    let mut unmatchable_guarded = 0usize;
    let mut unmatchable_orphaned = 0usize;
    let mut orphaned_samples: Vec<(String, cpp_code_analysis::DeclKind, Option<String>)> = Vec::new();
    let mut type_differing = 0usize;
    // **How much macro text is still read as a type.** A macro is not declared as a class or a typedef, so a type
    // spelling the index cannot declare anywhere is a name the parser took from the file's raw text — the shape that
    // `_GLIBCXX20_CONSTEXPR` was in (`operator bool`). Counted over the facts that have no rendering counterpart too,
    // because those are exactly the ones a lookup would answer with.
    let mut macro_shaped = 0usize;
    let mut macro_shaped_samples: Vec<(String, String)> = Vec::new();
    let mut differing_types: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    // **The macro names the index knows**, which is the exact test: a type spelling that is a macro is text the raw
    // reading could not expand, and `facts_vs_rendering`'s whole point is to say what that costs.
    let mut macro_names = std::collections::HashSet::new();
    for file in &files {
        if let Some(summary) = session.index().summary(file) {
            for fact in &summary.macros {
                macro_names.insert(fact.name.clone());
            }
        }
    }

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
                // **A fact the rendering does not have at all.** Two causes, and the split between them is the answer
                // to "how much of what the parser sees is macro text": a declaration in a branch the compiler does
                // not read (`guarded` — expected, and the lookup filters it), or a declaration the raw reading
                // *invented* out of unexpanded text (`orphaned` — a real defect).
                unmatchable += 1;
                if fact.guard != cpp_code_analysis::FactGuard::Unconditional {
                    unmatchable_guarded += 1;
                } else {
                    unmatchable_orphaned += 1;
                    if orphaned_samples.len() < 10 {
                        orphaned_samples.push((
                            fact.name.clone(),
                            fact.kind,
                            fact.returns.clone().or_else(|| fact.type_of.clone()),
                        ));
                    }
                }
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
            // Both fields, because either one feeds a lookup.
            for spelling in [&fact.returns, &fact.type_of].into_iter().flatten() {
                // **The last identifier**, because a spelling carries its operators: `_Bit_type*` names `_Bit_type`.
                let Some(name) = spelling
                    .split(|character: char| !(character.is_alphanumeric() || character == '_'))
                    .filter(|piece| !piece.is_empty())
                    .next_back()
                else {
                    continue;
                };
                let reserved = name.starts_with('_')
                    && name.chars().any(|character| character.is_ascii_uppercase());
                if reserved && macro_names.contains(name) {
                    macro_shaped += 1;
                    if macro_shaped_samples.len() < 8 {
                        macro_shaped_samples.push((fact.name.clone(), name.to_string()));
                    }
                }
            }
            total += 1;
            // A name may be overloaded; the written fact **contradicts** the rendering only if no reading of that name
            // agrees with it. A written `None` is not a contradiction: it is the fact declining to name a branch — see
            // `declarations.rs`, where a specifier sequence split by a directive states no single type.
            if candidates.iter().any(|other| other.returns == fact.returns) {
                // **The other two type fields tell the same story.** `type_of` is read from the same specifier
                // sequence and `parameter_list` from the same declarator, so a macro the raw reading takes for a
                // type shows up here too — this is what "the parser still sees macros" looks like in the index.
                if candidates.iter().any(|other| other.type_of == fact.type_of) {
                    continue;
                }
                type_differing += 1;
                if type_differing <= 8 {
                    differing_types.push((
                        fact.name.clone(),
                        fact.type_of.clone(),
                        candidates.first().and_then(|other| other.type_of.clone()),
                    ));
                }
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
    for (name, kind, spelling) in &orphaned_samples {
        println!("  no rendering counterpart: {name} ({kind:?}) {spelling:?}");
    }
    for (name, spelling) in &macro_shaped_samples {
        println!("  macro read as a type: {name} → {spelling}");
    }
    for (name, written, rendered) in &differing_types {
        println!("  type_of {name}: written {written:?} → rendering {rendered:?}");
    }
    println!(
        "totals: {total} fact(s) matched by name and scope, **{differing}** contradict the rendering about the \
         return type, {type_differing} about the type, {deferred} decline to name one (a directive splits the \
         declaration), {guarded} are branch-guarded, {macro_shaped} name a spelling the index declares nowhere \
         (macro text read as a type)"
    );
    println!(
        "  written facts with no counterpart in the rendering: {unmatchable} — {unmatchable_guarded} inside a \
         conditional region (the lookup filters them), **{unmatchable_orphaned}** unconditional"
    );
}
