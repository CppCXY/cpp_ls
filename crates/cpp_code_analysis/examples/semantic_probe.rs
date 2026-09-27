//! What a semantic highlighter would cost: one classification per identifier, and where the answers come from.
//!
//! A highlighter's question is "what is this name here", asked once per identifier in a file. The cheap halves are
//! the file's **own** bindings (the scope tree already has them, with a kind) and the **macro** names in force at
//! that offset (the preprocessor's table). The expensive half is a name the file does not declare: that is a
//! question for the index, and the index is a search over the files that include this one.
//!
//! This probe measures the three, so that the design of the feature can be decided by a number rather than by a
//! guess about which half dominates. Run it on a corpus list:
//!
//! ```text
//! cargo run --release --example semantic_probe -- <list> [--limit <n>]
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let list = std::env::args().nth(1).expect("a file list");
    let limit = std::env::args()
        .position(|argument| argument == "--limit")
        .and_then(|at| std::env::args().nth(at + 1))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(4);

    let paths: Vec<PathBuf> = std::fs::read_to_string(&list)
        .expect("the list reads")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();

    let mut session = cpp_code_analysis::Session::with_config(
        // An **empty root**: the session scans its root for sources, and a probe that pointed it at the corpus's
        // directory would index whatever else is lying there (see the architecture document's instrument notes).
        std::env::temp_dir().join("semantic-probe-root"),
        cpp_code_analysis::SessionFiles::new(
            cpp_code_analysis::OpenDocuments::new(),
            cpp_code_analysis::DiskFiles,
        ),
        cpp_code_analysis::WatchFilter::new(std::env::temp_dir().join("semantic-probe-root")),
        cpp_code_analysis::CompilerConfig::default(),
    );
    std::fs::create_dir_all(std::env::temp_dir().join("semantic-probe-root")).ok();

    let indexed = Instant::now();
    session.add_project_files(paths.iter().cloned());
    session.index_everything();
    println!(
        "indexed {} files in {:?} ({} pending)",
        session.project_files().len(),
        indexed.elapsed(),
        session.pending()
    );

    for path in paths.iter().take(limit) {
        let Some(view) = session.view(path) else {
            println!("{}: not held", path.display());
            continue;
        };

        let identifiers = view
            .tree
            .get_tokens()
            .iter()
            .filter(|token| token.kind == cpp_parser::CppTokenKind::Identifier)
            .count();

        // **The shipping path**, timed: `Session::classified_names` is what the LSP handler calls, so the number
        // below is the feature's cost rather than a model of it.
        let started = Instant::now();
        let names = session.classified_names(&view);
        let elapsed = started.elapsed();

        let mut by_kind: HashMap<String, usize> = HashMap::new();
        for name in &names {
            *by_kind
                .entry(format!("{:?}", name.kind))
                .or_default() += 1;
        }
        let declarations = names.iter().filter(|name| name.declaration).count();

        let mut kinds: Vec<(String, usize)> = by_kind.into_iter().collect();
        kinds.sort_by_key(|(_, count)| std::cmp::Reverse(*count));

        println!(
            "{:<24} {:>7} identifiers | {:>6} classified ({:>6} declarations) | {:?} | {}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            identifiers,
            names.len(),
            declarations,
            elapsed,
            kinds
                .iter()
                .map(|(kind, count)| format!("{kind} {count}"))
                .collect::<Vec<_>>()
                .join(", ")
        );

        // **No overlaps and no empties**: the protocol's encoding is a delta walk over sorted tokens, so two
        // tokens sharing a start (or a zero-length one) would be a corrupt answer — checked here on real files
        // rather than only in a unit test.
        for pair in names.windows(2) {
            assert!(
                pair[0].range.end_offset() <= pair[1].range.start_offset,
                "{}: tokens overlap or are out of order: {:?} then {:?}",
                path.display(),
                pair[0].range,
                pair[1].range
            );
        }
        assert!(
            names.iter().all(|name| name.range.length > 0),
            "{}: an empty token cannot be encoded",
            path.display()
        );
    }
}
