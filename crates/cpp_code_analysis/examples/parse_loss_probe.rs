//! **Where a header loses its own members, and how many.**
//!
//! ```text
//! parse_loss_probe <dir-or-file> [<dir-or-file> …]
//! ```
//!
//! The index reads a file's **own text** (a rendering resolves directives, but that is the cooked path), so every
//! shape the grammar cannot read costs members of that file until something cooks it. Two measurements say where:
//!
//! * a **function fact with no parameter list** — `DeclFact::parameter_list` is `None` on a `Function`, which means
//!   the declaration was split (the name was read and its `( … )` was not). That is what `std::vector::erase` was:
//!   a `#if` between the return type and the name, till the follower question learned to see through directives.
//! * an **error node**, counted by parsing the same text — a file with many has rubble, and each one is a declaration
//!   that did not arrive.
//!
//! Both are ranked, so the worst shape is the first line of the report rather than a guess.

use std::path::{Path, PathBuf};

fn files_under(path: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, found);
        } else if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("h" | "hpp" | "hh" | "inc" | "cpp" | "cc" | "cxx")
        ) {
            found.push(path);
        }
    }
}

fn main() {
    let roots: Vec<String> = std::env::args().skip(1).collect();
    if roots.is_empty() {
        eprintln!("usage: parse_loss_probe <dir-or-file> [<dir-or-file> …]");
        std::process::exit(2);
    }

    // `--show <file> [<n>]`: the split declarations themselves, one per line — the shape is what a fix is written
    // against, and it is not visible from a count.
    if roots.len() >= 2 && roots[0] == "--show" {
        let path = PathBuf::from(&roots[1]);
        let limit: usize = roots.get(2).and_then(|n| n.parse().ok()).unwrap_or(40);
        let source = std::fs::read_to_string(&path).expect("the file is readable");
        let summary = cpp_code_analysis::summarize(
            &path,
            &source,
            cpp_code_analysis::SummaryKey::new(0, 0),
        );
        let mut shown = 0;
        for fact in &summary.declarations {
            if fact.kind != cpp_code_analysis::DeclKind::Function
                || fact.parameter_list.is_some()
                || shown >= limit
            {
                continue;
            }
            let written = source
                .get(fact.range.start_offset..fact.range.end_offset())
                .unwrap_or("<out of range>");
            let one_line = written.split_whitespace().collect::<Vec<_>>().join(" ");
            println!(
                "  @{:<7} local {:<5} {:<40} {:?} name {:?}",
                fact.range.start_offset,
                fact.local,
                one_line.chars().take(90).collect::<String>(),
                fact.scope,
                fact.name
            );
            shown += 1;
        }
        println!("{shown} shown");
        return;
    }

    let mut files = Vec::new();
    for root in &roots {
        let path = PathBuf::from(root);
        if path.is_dir() {
            files_under(&path, &mut files);
        } else {
            files.push(path);
        }
    }
    files.sort();

    // **The two losses, per file**, and the totals they add up to.
    let mut split: Vec<(usize, usize, PathBuf)> = Vec::new();
    let mut rubble: Vec<(usize, PathBuf)> = Vec::new();
    let mut total_functions = 0usize;
    let mut total_split = 0usize;
    let mut total_rubble = 0usize;
    let mut total_facts = 0usize;

    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        let summary = cpp_code_analysis::summarize(
            path,
            &source,
            cpp_code_analysis::SummaryKey::new(0, 0),
        );

        let mut functions = 0usize;
        let mut without_parameters = 0usize;
        for fact in &summary.declarations {
            if !matches!(fact.kind, cpp_code_analysis::DeclKind::Function) {
                continue;
            }
            // **A local is not this file's member.** The most-vexing-parse reading of `f(x);` inside a body makes a
            // declaration of a variable; a function-shaped one is the same phenomenon and equally local, so it is
            // counted apart rather than as a lost member.
            if fact.local {
                continue;
            }
            functions += 1;
            if fact.parameter_list.is_none() {
                without_parameters += 1;
            }
        }

        let tree = cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default());
        let errors = tree.get_errors().len();
        let error_nodes = tree
            .get_red_root()
            .descendants()
            .filter(|node| {
                matches!(
                    cpp_parser::CppSyntaxKind::from(node.kind()),
                    cpp_parser::CppSyntaxKind::ErrorNode
                )
            })
            .count();

        total_facts += summary.declarations.len();
        total_functions += functions;
        total_split += without_parameters;
        total_rubble += error_nodes;

        if without_parameters > 0 {
            split.push((without_parameters, functions, path.clone()));
        }
        if error_nodes > 0 {
            rubble.push((error_nodes, path.clone()));
        }
        let _ = errors;
    }

    println!(
        "{} file(s), {total_facts} fact(s), {total_functions} function(s), \
         {total_split} with no parameter list, {total_rubble} error node(s)",
        files.len()
    );

    split.sort_by(|left, right| right.0.cmp(&left.0));
    println!("\nfunctions with no parameter list (the declaration was split):");
    for (without_parameters, functions, path) in split.iter().take(20) {
        println!(
            "  {without_parameters:5} of {functions:5}  {}",
            path.display()
        );
    }

    rubble.sort_by(|left, right| right.0.cmp(&left.0));
    println!("\nerror nodes (declarations that did not arrive):");
    for (errors, path) in rubble.iter().take(20) {
        println!("  {errors:5}  {}", path.display());
    }
}
