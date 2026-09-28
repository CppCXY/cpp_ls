//! **What a completion offers, at every position a reader types at** — the reading behind every claim in
//! `completion`'s documentation.
//!
//! ```text
//! cargo run --release --example completion_probe -- <dir> [<file.cpp>]
//! ```
//!
//! Four readings, and each answers a question the layer had to be built against:
//!
//! ```text
//! at every `::`      how many names one scope offers        — the "std:: lists 1517 things" symptom
//! at every `.`       how many members, and which             — the inherited-member walk
//! at every blank     how big the list is, and what is in it  — "692 names for a 78-line file"
//! the front of each  **the order**, which is the whole feature
//! ```
//!
//! The measurement that matters most is the **first five labels** at a blank line inside a function body: the
//! layer's claim is that a reader's own names come first and the standard library's come last, and that is either
//! visible in this output or it is not.

use cpp_code_analysis::{
    DiskFiles, ItemKind, Known, OpenDocuments, Session, SessionFiles, WatchFilter,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("a directory"));
    let file = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("main.cpp"));

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.index_everything();

    let Some(opened) = std::fs::read_to_string(&file).ok() else {
        panic!("{} does not read", file.display());
    };

    // `did_open` **drops** this file's summary — the text changed, so the reading built from any earlier text is
    // not an answer about this one — and until it is read again the file is not in the include graph at all, so
    // every name from every header it includes is missing. The second drain is what puts it back, and it is the
    // same sequence the server runs: open, then pump. A probe that skipped it measured an empty index and reported
    // it as an empty completion — which is the mistake this comment exists to keep from being made twice.
    session.did_open(&file, &opened);
    session.index_everything();

    let view = session.view(&file).expect("the file is held");
    let source = view.source.to_string();

    println!(
        "{}: {} bytes, {} lines | toolchain {:?} | {} include paths | headers {} (unread {})",
        file.display(),
        source.len(),
        source.lines().count(),
        session.toolchain().and_then(|toolchain| toolchain.version.clone()),
        session.config().include_paths.len(),
        session.headers().len(),
        session.headers().unread(),
    );

    // **What the query below the completion layer says**, which is the boundary between "the completion layer is
    // wrong" and "the index never had the names". Asked first, because a `std::` that offers nothing is either the
    // most interesting finding or the most boring one, and the two look identical from the outside.
    println!("\n--- the query underneath: what is visible at all ---");
    for scope in ["std", "spdlog", "fmt", "demo", "clice"] {
        println!(
            "  declarations_in({scope:?}) = {}",
            session.index().declarations_in(scope, &file).len()
        );
    }
    let visible = session.index().visible_files(&file);
    println!(
        "  visible files = {} ({} unconditional), index holds {}",
        visible.len(),
        visible
            .iter()
            .filter(|(_, visibility)| {
                *visibility == cpp_code_analysis::IncludeVisibility::Unconditional
            })
            .count(),
        session.index().len()
    );

    // ---------------------------------------------------------------- the positions a reader asks at
    // **`#include` first**, because it is the one completion whose answer is not in the program at all: the headers
    // are files on a search path, and whether the index found them is a fact about this machine rather than about
    // the file.
    println!("\n--- `#include <` : the headers on the search path ---");
    {
        // A file written for the occasion, **opened in the session like any other buffer**: the question is what
        // the *completion* answers, and asking it needs a cursor in a real file. Four prefixes in one text so that
        // one open serves all of them.
        let probe = root.join("__completion_probe.cpp");
        let text = "#include <v\n#include <iost\n#include <sys/\n#include <cstd\n";
        std::fs::write(&probe, text).expect("the probe writes");
        session.did_open(&probe, text);

        for prefix in ["v", "iost", "sys/", "cstd"] {
            let Some(offset) = after_include(&probe, prefix) else {
                continue;
            };

            let view = session.view(&probe).expect("the probe was opened");
            let found = session.completions(&view, offset);

            // **The headers a C++ reader actually writes**, asked by name rather than read off the first ten: a
            // list that begins well and omits `<vector>` is the failure this whole section exists to catch.
            let wanted = ["vector", "iostream", "cstddef", "string", "map", "algorithm"];
            let present: Vec<&str> = wanted
                .iter()
                .copied()
                .filter(|name| found.items.iter().any(|item| item.label == *name))
                .collect();
            let where_at = |name: &str| {
                found
                    .items
                    .iter()
                    .position(|item| item.label == name)
                    .map_or("—".to_string(), |at| at.to_string())
            };

            println!(
                "  {prefix:<6} | {:>4} items{} | first {:?}",
                found.items.len(),
                if found.truncated { " TRUNCATED" } else { "" },
                names(&found, 8)
            );
            println!(
                "         | standard headers present: {present:?} at {:?}",
                wanted
                    .iter()
                    .map(|name| (*name, where_at(name)))
                    .collect::<Vec<_>>()
            );
        }

        let _ = std::fs::remove_file(&probe);
    }

    // **What every `#include` in the file points at**, which is the other half of the same line: a completion
    // offers a spelling, and the jump has to turn it into the file the search found.
    println!("\n--- `#include` : where each one points ---");
    let mut at = 0usize;
    while let Some(found) = source[at..].find("#include") {
        let line_start_at = at + found;
        at = line_start_at + "#include".len();

        let Some((_, range)) = cpp_code_analysis::sema::resolve::header_name_at(&view.root, at + 1) else {
            println!(
                "{:>5} | no header name on this line | {}",
                line_of(&source, line_start_at) + 1,
                &source[line_start_at..line_end(&source, line_of(&source, line_start_at))]
            );
            continue;
        };

        match session.header_at(&view, range.start_offset + 1) {
            Known::Yes(target) => println!(
                "{:>5} | {:<28} -> {}",
                line_of(&source, line_start_at) + 1,
                target.spelling,
                target.resolved.display()
            ),
            Known::Unknown(reason) => println!(
                "{:>5} | resolves to nothing: {}",
                line_of(&source, line_start_at) + 1,
                reason.describe()
            ),
            Known::No => println!("{:>5} | no", line_of(&source, line_start_at) + 1),
        }
    }

    // Every `::` in the file, which is the question "what does this scope hold".
    println!("\n--- `::` : one scope's names ---");
    let mut at = 0usize;
    while let Some(found) = source[at..].find("::") {
        let after = at + found + 2;
        at = after;

        let found = session.completions(&view, after);
        println!(
            "{:>5}:{:<4} scope {:<24} {:>4} items  {:?}",
            line_of(&source, after) + 1,
            after - line_start(&source, after),
            format!("{:?}", found.scope),
            found.items.len(),
            names(&found, 6)
        );
    }

    // Every `.`, which is "what does this object have".
    println!("\n--- `.` : one object's members ---");
    let mut at = 0usize;
    while let Some(found) = source[at..].find('.') {
        let after = at + found + 1;
        at = after;

        let completions = session.completions(&view, after);

        // A **member** answer is the one whose items are members — a method, a field. A `.` in finished code
        // (`a.b`) asks the same question at a cursor that is not on the member, and the completion falls through
        // to the names in scope there, which is a long list about nothing.
        let is_a_member_list = completions
            .items
            .iter()
            .any(|item| matches!(item.kind, ItemKind::Method | ItemKind::Field));
        if !is_a_member_list {
            // **What it answered instead**, which is the diagnosis: a `.` that falls through to the name list is
            // either not a member position at all (the parser recovered) or the object's type could not be worked
            // out. Printing the context is what tells the two apart.
            println!(
                "{:>5}:{:<4} NOT a member list — {:?} | {:?}",
                line_of(&source, after) + 1,
                after - line_start(&source, after),
                cpp_code_analysis::context_at(&view.root, after),
                names(&completions, 5)
            );
            continue;
        }

        println!(
            "{:>5}:{:<4} class {:<24} {:>4} items (prefix {:?})  {:?}",
            line_of(&source, after) + 1,
            after - line_start(&source, after),
            completions.scope,
            completions.items.len(),
            completions.prefix,
            names(&completions, 8)
        );

        // **The duplicates**, which a real client renders as several identical rows. The label is what a reader
        // picks by, so two items with one label are one choice offered twice — and the count says how bad it is on
        // the type being completed, which on MSVC's `basic_string` is where a user meets it first.
        let mut labels: Vec<&str> = completions
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        labels.sort_unstable();
        let before = labels.len();
        labels.dedup();
        if labels.len() != before {
            let mut repeated: Vec<(String, usize, String)> = Vec::new();
            for item in &completions.items {
                let count = completions
                    .items
                    .iter()
                    .filter(|other| other.label == item.label)
                    .count();
                if count > 1 && !repeated.iter().any(|(name, _, _)| *name == item.label) {
                    let source = item
                        .fact
                        .as_ref()
                        .map_or("<no fact>".to_string(), |fact| {
                            format!("name={:?} kind={:?}", fact.name, fact.kind)
                        });
                    repeated.push((item.label.clone(), count, source));
                }
            }
            println!(
                "         | {} items share {} label(s): {repeated:?}",
                before - labels.len(),
                repeated.len()
            );
        }
    }

    // The end of every line that holds no identifier — where a reader presses the key to write something new.
    println!("\n--- a blank line inside a body: the whole list, by kind ---");
    let mut blank = 0usize;
    for (number, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !matches!(trimmed, "{" | "}") {
            continue;
        }

        let offset = line_end(&source, number);
        let found = session.completions(&view, offset);
        if found.items.is_empty() {
            continue;
        }

        blank += 1;
        if blank > 12 {
            continue;
        }

        let mut by_kind: HashMap<ItemKind, usize> = HashMap::new();
        for item in &found.items {
            *by_kind.entry(item.kind).or_default() += 1;
        }
        let mut kinds: Vec<(ItemKind, usize)> = by_kind.into_iter().collect();
        kinds.sort_by_key(|(_, count)| std::cmp::Reverse(*count));

        println!(
            "{:>5} {:>4} items ({} bytes of labels) | {:?}{}",
            number + 1,
            found.items.len(),
            found
                .items
                .iter()
                .map(|item| item.label.len())
                .sum::<usize>(),
            kinds
                .iter()
                .map(|(kind, count)| format!("{kind:?} {count}"))
                .collect::<Vec<_>>(),
            if found.truncated { "  TRUNCATED" } else { "" }
        );
        println!("        first: {:?}", names(&found, 8));
        println!("        last:  {:?}", last_names(&found, 4));
    }

    // ---------------------------------------------------------------- cost
    // **Sampled**, because a client asks once per keystroke and this loop asks once per byte: a 54 KB file is
    // fifty-five thousand requests, which is a client typing for six hours without stopping. The stride keeps the
    // reading comparable between files of different sizes — about two thousand positions, whatever the file's
    // length — and the answer is per *offset*, which is what a keystroke costs.
    //
    // The view is built **outside** the loop, and that is not a micro-optimisation: a view is the parse of the
    // file, so a loop that made one per position would be measuring the parser and reporting it as the analysis.
    println!("\n--- cost ---");
    let stride = (source.len() / 2000).max(1);
    let started = Instant::now();
    let mut asked = 0usize;
    let mut answered = 0usize;
    let mut items = 0usize;
    for offset in (0..source.len()).step_by(stride) {
        asked += 1;
        let found = session.completions(&view, offset);
        if !found.items.is_empty() {
            answered += 1;
            items += found.items.len();
        }
    }
    let elapsed = started.elapsed();

    // …and the same again, which is what a session that has been answering for a while costs. The two numbers are
    // the same today (nothing in this path caches per query beyond what the index already holds); they are printed
    // apart so that the round which adds a cache can show what it bought.
    let warm = Instant::now();
    for offset in (0..source.len()).step_by(stride * 8) {
        let _ = session.completions(&view, offset);
    }
    let warm = warm.elapsed();

    println!(
        "cold: {answered} of {asked} offsets (every {stride} bytes of {}) answer, {items} items in total, \
         {elapsed:?} ({:.3} ms per offset)",
        source.len(),
        elapsed.as_secs_f64() * 1000.0 / asked.max(1) as f64
    );
    println!(
        "warm: {:.3} ms per offset over {} positions",
        warm.as_secs_f64() * 1000.0 / (asked / 8).max(1) as f64,
        asked / 8
    );

    // **Where the time goes**, because "the completion is slow" is not a finding: the layer above does four things
    // and three of them are other queries. Each is timed separately, over the same positions, so that the round
    // that opens this path again knows which one it is opening.
    let mut graph = std::time::Duration::ZERO;
    let mut position = std::time::Duration::ZERO;
    let mut query = std::time::Duration::ZERO;
    let mut whole = std::time::Duration::ZERO;
    let mut positions = 0usize;
    for offset in (0..source.len()).step_by(stride * 8) {
        positions += 1;

        let started = Instant::now();
        let _ = session.index().visible_files(&file);
        graph += started.elapsed();

        let started = Instant::now();
        let _ = cpp_code_analysis::sema::resolve::name_position_at(&view.root, offset);
        position += started.elapsed();

        let started = Instant::now();
        let _ = session.name_completions(&view, offset);
        query += started.elapsed();

        let started = Instant::now();
        let _ = session.completions(&view, offset);
        whole += started.elapsed();
    }
    println!(
        "breakdown over {positions} positions: graph walk {:.3} ms, position {:.3} ms, \
         name query {:.3} ms, whole {:.3} ms",
        graph.as_secs_f64() * 1000.0 / positions.max(1) as f64,
        position.as_secs_f64() * 1000.0 / positions.max(1) as f64,
        query.as_secs_f64() * 1000.0 / positions.max(1) as f64,
        whole.as_secs_f64() * 1000.0 / positions.max(1) as f64,
    );
}

fn names(found: &cpp_code_analysis::CompletionSet, take: usize) -> Vec<&str> {
    found
        .items
        .iter()
        .take(take)
        .map(|item| item.label.as_str())
        .collect()
}

/// The offset just past the `#include <` of a line written as `#include <PREFIX` in `path`, if that file exists.
fn after_include(path: &std::path::Path, prefix: &str) -> Option<usize> {
    let source = std::fs::read_to_string(path).ok()?;
    let wanted = format!("#include <{prefix}");
    source.find(&wanted).map(|at| at + wanted.len())
}

fn last_names(found: &cpp_code_analysis::CompletionSet, take: usize) -> Vec<&str> {
    let at = found.items.len().saturating_sub(take);
    found
        .items
        .iter()
        .skip(at)
        .map(|item| item.label.as_str())
        .collect()
}

fn line_of(source: &str, offset: usize) -> usize {
    source[..offset.min(source.len())].matches('\n').count()
}

fn line_start(source: &str, offset: usize) -> usize {
    source[..offset.min(source.len())]
        .rfind('\n')
        .map_or(0, |at| at + 1)
}

fn line_end(source: &str, line: usize) -> usize {
    let mut at = 0usize;
    for _ in 0..line {
        match source[at..].find('\n') {
            Some(newline) => at += newline + 1,
            None => return source.len(),
        }
    }

    source[at..].find('\n').map_or(source.len(), |end| at + end)
}
