//! Probe: does a member access on the **real standard library** resolve, and through what?
//!
//! ```bash
//! cargo run --release -p cpp_code_analysis --example std_query
//! ```
//!
//! It writes a small translation unit of member accesses, discovers the toolchain on this machine, indexes the
//! closure of its includes, and then asks each access the way a cursor would — [`member_across_files`] with the
//! file's own tree and scopes, against an index built from the standard library's summaries.
//!
//! # Why this is the probe that says whether the alias step works
//!
//! `std::string` is a `typedef` for `std::basic_string<char>`, and an alias has no members of its own: `substr`,
//! `size` and `find` are all declared in `basic_string`. So a lookup that stops at the spelling the user wrote
//! finds **nothing at all**, and following the alias one step is the difference between "the standard library is
//! indexed" and "the standard library can be asked about".
//!
//! Run it before and after a change to that step and the count is what the step is holding up. What it prints per
//! query is the file and scope the member was found in, so an answer can be **checked** rather than counted: a
//! jump into the wrong class shows up here as a wrong scope, where a bare "resolved" count would call it a pass.
//!
//! # Why the whole store and not just summaries
//!
//! A query is answered through the include graph: `ProjectIndex` only offers a declaration to a file that
//! **reaches** the file declaring it. Summaries built without a project have no include edges at all, so an index
//! of them answers `NotDeclaredHere` for everything — which is what the first version of this probe did, and it
//! read as "the alias step does not work" rather than as "the probe has no graph".

use std::path::Path;

use cpp_code_analysis::{
    CompilerConfig, DiskCommands, DiskFiles, Environment, IncludeBudget, Known, SummaryStore, discover,
};

/// The probe's own text. Each member access in it is one query, and the includes are what the closure is.
const PROBE: &str = "\
#include <string>\n\
#include <vector>\n\
#include <map>\n\
void f() {\n\
  std::string s;\n\
  s.size();\n\
  s.substr(1);\n\
  s.empty();\n\
  std::vector<int> v;\n\
  v.push_back(1);\n\
  v.size();\n\
  std::map<int, int> m;\n\
  m.find(1);\n\
  m.begin();\n\
}\n";

/// The member accesses to ask about, in the order the text writes them.
const QUERIES: &[&str] = &[
    "s.size",
    "s.substr",
    "s.empty",
    "v.push_back",
    "v.size",
    "m.find",
    "m.begin",
];

fn main() {
    let root = std::env::temp_dir().join("cppls-std-query");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("the probe directory");
    let probe = root.join("probe.cpp");
    std::fs::write(&probe, PROBE).expect("the probe writes");

    let files = DiskFiles;
    let toolchain = discover(&files, &DiskCommands, None, &probe, &Environment::current());

    let config = match &toolchain {
        Some(toolchain) => {
            println!(
                "toolchain: {} ({})\n  {} system include directories",
                toolchain.compiler.display(),
                toolchain.version.as_deref().unwrap_or("version not reported"),
                toolchain.system_include_paths.len()
            );
            toolchain.config(&CompilerConfig::new())
        }
        None => {
            println!("no compiler was found, so `<string>` will not resolve and every query below must fail");
            CompilerConfig::new()
        }
    };

    let started = std::time::Instant::now();
    let mut store = SummaryStore::open(&root, config);
    let indexed = store.index_includes_from(&probe, IncludeBudget::default());
    let built = started.elapsed();

    println!(
        "\nindexed {} files in {built:?} ({} parsed, {} from disk, {} not stored, {} unresolved includes)",
        indexed.indexed.len(),
        indexed.stats.rebuilt,
        indexed.stats.reused,
        indexed.stats.unstored,
        indexed.unresolved.len()
    );

    let source = PROBE;
    let tree = cpp_parser::CppParser::parse(source, cpp_parser::ParserConfig::default());
    let errors = tree.get_errors();
    assert!(
        errors.is_empty(),
        "the probe must parse cleanly, or the queries below are asking about a tree nobody meant: {errors:?}"
    );
    let root_node = tree.get_red_root();
    let scopes = cpp_code_analysis::build_scopes(&root_node);

    let mut resolved = 0usize;

    // A diagnostic, not a measurement: what the index actually holds for the names the query needs. A failure has
    // several causes that look identical from the answer alone — the name is not indexed, it is indexed but not
    // *visible* from the probe, the class's members are filed under another scope spelling, or the target of an
    // alias is missing — and this is the cheapest way to tell them apart.
    for name in ["std::string", "std::basic_string", "std::vector"] {
        match store.index().definition(name, &probe) {
            Known::Yes(found) => println!(
                "  [have] {name} -> {} kind {:?} type_of {:?} scope {:?}",
                short(&found.file),
                found.fact.kind,
                found.fact.type_of,
                found.fact.scope
            ),
            Known::Unknown(reason) => println!("  [have] {name} -> unknown: {}", reason.describe()),
            Known::No => println!("  [have] {name} -> no"),
        }
    }

    let members = store.index().declarations_in("std::basic_string", &probe);
    println!(
        "  [have] std::basic_string members -> {} (first few: {:?})",
        members.len(),
        members
            .iter()
            .take(6)
            .map(|member| member.fact.name.as_str())
            .collect::<Vec<_>>()
    );
    let named = members
        .iter()
        .filter(|member| member.fact.name == "size")
        .count();
    println!("  [have] std::basic_string members named `size` -> {named}");

    // How many of them are *functions*? A member list made only of typedefs and data members would say that
    // everything with a parameter list is being lost, which is a different defect from a wrong name.
    let functions = members
        .iter()
        .filter(|member| member.fact.kind == cpp_code_analysis::DeclKind::Function)
        .count();
    let types = members
        .iter()
        .filter(|member| member.fact.kind == cpp_code_analysis::DeclKind::Type)
        .count();
    let variables = members
        .iter()
        .filter(|member| member.fact.kind == cpp_code_analysis::DeclKind::Variable)
        .count();
    println!("  [have] by kind -> {functions} functions, {types} types, {variables} variables, {} other",
        members.len() - functions - types - variables);
    println!(
        "  [have] every name -> {:?}",
        members.iter().map(|member| member.fact.name.as_str()).collect::<Vec<_>>()
    );

    // Where in the file the members went.
    //
    // `declarations_in` answers with the facts filed under **one scope spelling**, so "the public interface is
    // missing" has two shapes, and they are different defects: the facts may not be in the summary at all (the
    // tree or the walker lost them), or they may be there under another scope spelling — facts in the wrong room,
    // which one wrong brace is enough to cause. The by-scope histogram tells the two apart in one screen, and it
    // is the only cheap way to see the second one: every query downstream filters by scope, so both shapes read
    // as "not declared" from the outside.
    //
    // The per-fact line numbers exist to be compared with the source: the facts of a class body come out in the
    // order the file writes them, so the first line where the two lists disagree is the construct that stopped
    // the reading, whether or not anything about that construct is malformed.
    for summary in store.index().summaries() {
        if !["basic_string", "cow_string", "stl_vector", "stl_map", "vector.tcc", "stl_tree"]
            .iter()
            .any(|name| summary.path.to_string_lossy().contains(name))
        {
            continue;
        }
        {
            let text = String::from_utf8_lossy(&std::fs::read(&summary.path).unwrap_or_default()).to_string();
            let line_of = |offset: usize| text[..offset.min(text.len())].lines().count();

            println!(
                "\n  [where] {} — {} facts in the summary",
                short(&summary.path),
                summary.declarations.len()
            );

            let mut by_scope: std::collections::BTreeMap<String, Vec<&cpp_code_analysis::DeclFact>> =
                std::collections::BTreeMap::new();
            for fact in &summary.declarations {
                by_scope
                    .entry(fact.scope.clone().unwrap_or_else(|| "<file scope>".to_string()))
                    .or_default()
                    .push(fact);
            }
            for (scope, facts) in &by_scope {
                let first = facts.iter().map(|fact| fact.range.start_offset).min().unwrap_or(0);
                let last = facts.iter().map(|fact| fact.range.end_offset()).max().unwrap_or(0);
                println!(
                    "    {:>5} facts  lines {:>5}..{:<5}  {scope}",
                    facts.len(),
                    line_of(first),
                    line_of(last)
                );
            }

            for (scope, facts) in by_scope.iter().filter(|(scope, _)| {
                let scope = scope.to_lowercase();
                scope.contains("basic_string")
                    || scope.contains("vector")
                    || scope.contains("map")
                    || scope == "<file scope>"
            }) {
                let mut ordered: Vec<&&cpp_code_analysis::DeclFact> = facts.iter().collect();
                ordered.sort_by_key(|fact| fact.range.start_offset);
                println!("    [lines] {scope} ({} facts)", ordered.len());
                for fact in ordered {
                    println!(
                        "      {:>5}  {:<24} {:?}{}",
                        line_of(fact.range.start_offset),
                        fact.name,
                        fact.kind,
                        if fact.clean { "" } else { "   (unclean)" }
                    );
                }
            }
        }
    }

    for query in QUERIES {
        let Some(offset) = source.rfind(query).map(|at| at + query.len() - 1) else {
            continue;
        };

        let answer = cpp_code_analysis::index::project::member_across_files(
            store.index(),
            &scopes,
            &root_node,
            &probe,
            offset,
        );

        match answer {
            Known::Yes(found) => {
                resolved += 1;
                println!(
                    "  {query:<14} -> {}  {}::{}",
                    short(&found.file),
                    found.fact.scope.as_deref().unwrap_or("<file scope>"),
                    found.fact.name
                );
            }
            Known::Unknown(reason) => println!("  {query:<14} -> unknown: {}", reason.describe()),
            Known::No => println!("  {query:<14} -> no"),
        }
    }

    println!("\nresolved {resolved}/{}", QUERIES.len());
}

/// The file's name, which is what identifies a standard-library header in a one-line answer.
fn short(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}
