//! **What a use-side inverted index would cost in the summary** — the sizing that decides how references are
//! stored, before any of it is built.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example use_index_cost -- <dir> [<entry.cpp>]
//! ```
//!
//! # The design this is sizing
//!
//! `references` today resolves the cursor to a symbol and then **scans every candidate file's tokens** at query
//! time (`index/references.rs:441-494`) — a lexer scan with a resolved *subject*, which over-reports and costs
//! `O(tokens in the closure)` per query. The replacement both this project and IntelliJ use is an **inverted
//! index**: `name → use sites`, built once per file, so a query is a lookup plus a verification pass.
//!
//! The reason it is keyed by **name** and not by resolved symbol identity is the one thing that must not change
//! when overload resolution and template deduction arrive: this engine's contract is three-valued, and an index
//! that stored a resolved identity would have to write `Unknown` as `not a reference`. A name-keyed candidate set
//! is true *today* and gains a resolution column later **without being rewritten** — which is why the numbers
//! below are about the candidate side only.
//!
//! # What it measures
//!
//! ```text
//!   identifier uses per file      every identifier token that is not a declaration's own name
//!     …inside a `CompoundStat`    the ones a local reference could be — resolvable within the file, so they
//!                                 belong in a per-file fact that depends on nothing else
//!     …outside                    the ones a cross-file index has to carry
//!   projected bytes               (name id, offset) per use, plus the file's own name table
//!   the summary's own bytes       the cache directory as it stands, for the ratio that matters
//! ```
//!
//! The ratio is the whole question: a use table that doubles the on-disk index is a different decision from one
//! that adds 20%.
//!
//! # What it does not
//!
//! It does not build the index, does not decide the encoding, and does not count *distinct* uses of a name —
//! deduplication is an encoding question and this is a volume question. It also does not measure the verification
//! pass, which is the expensive half of a query and is bounded by the candidate set this sizes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use cpp_parser::CppSyntaxKind;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

/// Bytes one `(name id, offset)` pair costs: two `u32`s. Delta-encoding a file's offsets in use order would beat
/// this, and a per-file name table is added separately — so this is the pessimistic reading of the volume, which
/// is the right direction for a decision about whether there is room.
const BYTES_PER_USE: usize = 8;

/// Bytes one entry of a file's own name table: the name's bytes plus a length. The names are shared with the
/// summary's macro and declaration tables, so this is an upper bound rather than an addition.
const BYTES_PER_NAME_HEADER: usize = 4;

fn main() {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: use_index_cost <dir> [<entry.cpp>]");
        std::process::exit(2);
    };
    let home = PathBuf::from(&dir);
    let entry = std::env::args().nth(2).map(PathBuf::from);

    let mut session = Session::open(
        home.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&home),
    );
    if let Some(entry) = &entry {
        let text = std::fs::read_to_string(entry).unwrap_or_default();
        session.did_open(entry, &text);
    }
    loop {
        let steps = session.advance(64);
        if steps.is_empty() && session.pending() == 0 {
            break;
        }
    }

    let mut files = 0usize;
    let mut uses = 0usize;
    let mut uses_in_bodies = 0usize;
    let mut names: HashSet<String> = HashSet::new();
    let mut locals_with_uses = 0usize;
    let mut local_uses = 0usize;
    let mut local_micros = 0usize;

    for summary in session.index().summaries() {
        let Ok(text) = std::fs::read_to_string(&summary.path) else {
            continue;
        };
        files += 1;

        // The declaration's own name is not a use of it. Asked by offset, which is what a declaration records —
        // see `DeclFact::name_range`.
        let declared: HashSet<usize> = summary
            .declarations
            .iter()
            .map(|fact| fact.name_range.start_offset)
            .collect();

        let (lexed, _) = cpp_parser::lex(&text, &cpp_parser::LexerConfig::default());
        let tree = cpp_parser::CppParser::parse(&text, cpp_parser::ParserConfig::default());
        let root = tree.get_red_root();

        // **The local half, computed rather than approximated.** `local_references_of` is the real resolution —
        // it walks the scope chain — so this is the number that decides whether the per-file table is affordable
        // and what it is worth, both at once.
        {
            let scopes =
                cpp_code_analysis::sema::scopes::build_scopes(&root, &cpp_code_analysis::sema::scopes::NoMacroBodies);
            let started = std::time::Instant::now();
            let locals = cpp_code_analysis::sema::declarations::local_references_of(&scopes, &root);
            local_micros += started.elapsed().as_micros() as usize;
            for entry in &locals {
                if entry.uses.is_empty() {
                    continue;
                }
                locals_with_uses += 1;
                local_uses += entry.uses.len();
            }
        }

        // **Inside a statement block** — where a local reference lives, and therefore where a per-file fact could
        // answer without any cross-file work at all.
        let in_a_body = |node: &cpp_parser::CppSyntaxNode| {
            node.ancestors()
                .any(|above| CppSyntaxKind::from(above.kind()) == CppSyntaxKind::CompoundStat)
        };

        for element in root.descendants_with_tokens() {
            let Some(token) = element.as_token() else {
                continue;
            };
            if cpp_parser::CppTokenKind::from(token.kind()) != cpp_parser::CppTokenKind::Identifier {
                continue;
            }
            let at = usize::from(token.text_range().start());
            if declared.contains(&at) {
                continue;
            }
            let name = token.text();
            if name.is_empty() {
                continue;
            }
            uses += 1;
            names.insert(name.to_string());
            if element.parent().is_some_and(|parent| in_a_body(&parent)) {
                uses_in_bodies += 1;
            }
        }
        let _ = lexed;
    }

    let projected = uses * BYTES_PER_USE + names.len() * BYTES_PER_NAME_HEADER;
    let on_disk = directory_bytes(&home.join(".cppls"));

    println!("{files} file(s) indexed");
    println!("  identifier uses                     {uses:>12}");
    println!(
        "    …inside a statement block         {uses_in_bodies:>12}   {:>5.1}%  (a per-file fact can answer these)",
        share(uses_in_bodies, uses)
    );
    println!("    …outside                          {:>12}", uses - uses_in_bodies);
    println!("  distinct names                        {:>12}", names.len());
    println!();
    println!("  projected use index                 {projected:>12} bytes  ({} per use, names separate)", BYTES_PER_USE);
    println!("  the summaries on disk               {on_disk:>12} bytes");
    println!(
        "  ratio                               {:>12}",
        if on_disk == 0 {
            "no cache to compare".to_string()
        } else {
            format!("{:.1}% of it", projected as f64 * 100.0 / on_disk as f64)
        }
    );

    // **The local half, resolved rather than guessed.** These are uses the file can answer about itself — the
    // table that needs no closure, which is the first half of the references work.
    let local_bytes = local_uses * BYTES_PER_USE + locals_with_uses * 4;
    println!();
    println!("  local declarations with uses        {locals_with_uses:>12}");
    println!(
        "  local uses (resolved)               {local_uses:>12}   {:>5.1}% of all uses",
        share(local_uses, uses)
    );
    println!(
        "  …as a per-file table                {local_bytes:>12} bytes   {:>5.1}% of the summaries",
        if on_disk == 0 { 0.0 } else { local_bytes as f64 * 100.0 / on_disk as f64 }
    );
    println!(
        "  resolving them cost                 {:>12}   ({:.1} µs per file, {:.1} µs per identifier use)",
        format!("{:.1} ms", local_micros as f64 / 1000.0),
        local_micros as f64 / files.max(1) as f64,
        local_micros as f64 / uses.max(1) as f64
    );
    let _ = Path::new("");
}

fn share(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

/// Every byte under `dir`, recursively — the summaries as they actually sit on disk, which is the number the
/// projection has to be argued against.
fn directory_bytes(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_bytes(&entry.path()),
            _ => entry.metadata().map(|data| data.len() as usize).unwrap_or(0),
        })
        .sum()
}
