//! The name index against a scan of the summaries — a test that the index's answers and the facts agree.
//!
//! Moved out of `project/mod.rs` without a change.

use super::*;
use crate::CompilerConfig;
use crate::FileIndexer;
use crate::MemoryFiles;
use crate::cache::SummaryKey;
use crate::index::summarize;
use crate::preprocess;

const WORDS: [&str; 6] = ["Widget", "size", "run", "Gadget", "count", "widest"];

/// A small deterministic generator: the corpus has to be the same on every machine and every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, below: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % below
    }
}

fn source_of(number: usize, generator: &mut Lcg) -> String {
    let mut text = String::new();
    for _ in 0..generator.next(3) {
        text.push_str(&format!(
            "#include \"f{}.h\"\n",
            generator.next(number.max(1))
        ));
    }
    text.push_str(&format!("#define DECL{number}(n) struct n {{ int v; }};\n"));
    text.push_str(&format!("DECL{number}(Made{})\n", generator.next(4)));
    text.push_str(&format!("namespace ns{} {{\n", generator.next(3)));
    for _ in 0..1 + generator.next(3) {
        let class = WORDS[generator.next(WORDS.len())];
        let member = WORDS[generator.next(WORDS.len())];
        text.push_str(&format!(
            "struct {class} {{ int {member}; void {member}2(); }};\n"
        ));
    }
    text.push_str("}\n");
    text.push_str(&format!("int {};\n", WORDS[generator.next(WORDS.len())]));
    text.push_str(&format!(
        "void fn{number}() {{ int {}; }}\n",
        WORDS[generator.next(WORDS.len())]
    ));
    text
}

fn file(index: &mut ProjectIndex, number: usize, source: &str) {
    let path = format!("/p/f{number}.h");
    let mut summary = summarize(Path::new(&path), source, SummaryKey::new(number as u64, 0));
    for include in &mut summary.includes {
        include.resolved = Some(PathBuf::from(format!("/p/{}", include.spelling)));
    }
    index.insert(summary);
}

fn cook(index: &mut ProjectIndex, number: usize, source: &str) {
    let path = format!("/p/f{number}.h");
    let (tokens, _) = cpp_parser::lex(source, &cpp_parser::LexerConfig::default());
    let rendered = preprocess::cooked::cook(source, &tokens).render();
    let provider = MemoryFiles::new();
    let config = CompilerConfig::default();
    let indexed = FileIndexer::new(&provider, &config).index_rendering(
        Path::new(&path),
        &rendered,
        SummaryKey::new(number as u64, 0),
    );
    index.insert_cooked(Path::new(&path), indexed.into());
}

type Row = (PathBuf, String, Option<String>, usize, IncludeVisibility);

fn row(file: &Path, fact: &DeclFact, visibility: IncludeVisibility) -> Row {
    (
        file.to_path_buf(),
        fact.name.to_string(),
        fact.scope.clone().map(|scope| scope.to_string()),
        fact.range.start_offset,
        visibility,
    )
}

/// Every fact a query can see, with the raw-then-cooked deduplication, the way the per-file scan did it.
fn scanned(index: &ProjectIndex, from: &Path, accepts: impl Fn(&DeclFact) -> bool) -> Vec<Row> {
    let visible: HashMap<String, IncludeVisibility> =
        index.visible_files(from).into_iter().collect();
    let mut rows = Vec::new();

    for summary in index.summaries() {
        let Some(visibility) = visible.get(&normalize(&summary.path)).copied() else {
            continue;
        };
        let raw: Vec<&DeclFact> = summary
            .declarations
            .iter()
            .filter(|fact| accepts(fact))
            .collect();
        rows.extend(raw.iter().map(|fact| row(&summary.path, fact, visibility)));

        for fact in index
            .cooked_declarations(&summary.path)
            .into_iter()
            .flatten()
            .filter(|fact| accepts(fact))
        {
            if !raw
                .iter()
                .any(|known| known.name == fact.name && known.kind == fact.kind)
            {
                rows.push(row(&summary.path, fact, visibility));
            }
        }
    }

    rows
}

fn rows(found: &[VisibleDeclaration<'_>]) -> Vec<Row> {
    found
        .iter()
        .map(|found| row(&found.file, found.fact, found.visibility))
        .collect()
}

/// The workspace search as it was written before the index: every declaration of every file.
fn symbols_by_scanning(
    index: &ProjectIndex,
    query: &str,
    limit: usize,
) -> Vec<(PathBuf, String, Option<String>, usize)> {
    let wanted = query.trim().to_lowercase();
    let mut matches: Vec<(usize, String, String, PathBuf, &DeclFact)> = Vec::new();

    for summary in index.summaries() {
        let raw: Vec<&DeclFact> = summary
            .declarations
            .iter()
            .filter(|fact| !fact.local)
            .collect();
        let fresh: Vec<&DeclFact> = index
            .cooked_declarations(&summary.path)
            .into_iter()
            .flatten()
            .filter(|fact| {
                !fact.local
                    && !raw
                        .iter()
                        .any(|known| known.name == fact.name && known.kind == fact.kind)
            })
            .collect();

        for fact in raw.into_iter().chain(fresh) {
            let qualified = fact.qualified_name();
            let Some(rank) = rank_of(&qualified, &fact.name, &wanted) else {
                continue;
            };
            let qualified = qualified.to_lowercase();
            let name = match fact.name.is_empty() {
                false => fact.name.to_lowercase(),
                true => qualified
                    .rsplit("::")
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            };
            matches.push((rank, name, qualified, summary.path.clone(), fact));
        }
    }

    matches.sort_by(|one, two| {
        one.0
            .cmp(&two.0)
            .then_with(|| one.1.cmp(&two.1))
            .then_with(|| one.2.cmp(&two.2))
            .then_with(|| one.3.cmp(&two.3))
    });
    matches
        .into_iter()
        .take(limit)
        .map(|(_, _, _, file, fact)| {
            (
                file,
                fact.name.to_string(),
                fact.scope.clone().map(|scope| scope.to_string()),
                fact.range.start_offset,
            )
        })
        .collect()
}

fn check(index: &ProjectIndex, files: usize, step: &str) {
    let asked_from: Vec<PathBuf> = (0..files)
        .step_by(3)
        .map(|n| PathBuf::from(format!("/p/f{n}.h")))
        .collect();

    for from in &asked_from {
        for word in WORDS.iter().copied().chain([
            "Made0",
            "Made3",
            "nothing",
            "ns0::Widget",
            "::size",
            "ns1::Gadget",
        ]) {
            assert_eq!(
                rows(&index.files_declaring(word, from)),
                scanned(index, from, |fact| matches(fact, word)),
                "{step}: files_declaring({word:?}) from {from:?}"
            );
        }
        for scope in [
            "ns0",
            "ns1",
            "ns2",
            "ns0::Widget",
            "ns1::Gadget",
            "ns2::size",
            "nowhere",
        ] {
            assert_eq!(
                rows(&index.declarations_in(scope, from)),
                scanned(index, from, |fact| fact.scope.as_deref() == Some(scope)),
                "{step}: declarations_in({scope:?}) from {from:?}"
            );
        }
    }

    for query in [
        "w",
        "wid",
        "Widget",
        "size",
        "ma",
        "made",
        "run2",
        "ns0::wid",
        "ns1::Gadget::si",
        "Widget::",
        "::widget",
        "ns0::",
        "zzz",
        "  count ",
        "e",
    ] {
        for limit in [1, 4, 1000] {
            let found: Vec<_> = index
                .symbols_matching(query, limit)
                .into_iter()
                .map(|symbol| {
                    (
                        symbol.file,
                        symbol.fact.name.to_string(),
                        symbol.fact.scope.clone().map(|scope| scope.to_string()),
                        symbol.fact.range.start_offset,
                    )
                })
                .collect();
            assert_eq!(
                found,
                symbols_by_scanning(index, query, limit),
                "{step}: symbols_matching({query:?}, {limit})"
            );
        }
    }

    let defining: Vec<PathBuf> = index
        .summaries()
        .filter(|summary| {
            summary
                .macros
                .iter()
                .any(|fact| fact.name == "DECL3" && fact.kind.is_definition())
        })
        .map(|summary| summary.path.clone())
        .collect();
    assert_eq!(
        index.files_defining_macro("DECL3"),
        defining,
        "{step}: files_defining_macro"
    );
}

/// A name declared thousands of times is cut at the limit rather than ordered — and the cut is deterministic.
#[test]
fn a_name_declared_everywhere_answers_the_first_hits_in_file_order() {
    let mut index = ProjectIndex::new();
    for number in 0..POPULAR_NAME + 50 {
        file(
            &mut index,
            number,
            &format!("struct C{number} {{ void run(); }};\n"),
        );
    }

    let found = index.symbols_matching("run", 10);
    assert_eq!(found.len(), 10);
    assert_eq!(
        found,
        index.symbols_matching("run", 10),
        "the same answer twice"
    );
    assert!(
        found.iter().all(|symbol| {
            let number: usize = symbol.file.to_string_lossy()[4..]
                .trim_end_matches(".h")
                .parse()
                .unwrap();
            number < 10
        }),
        "the cut takes the files in the order the index saw them: {found:?}"
    );
}

#[test]
fn after_every_kind_of_edit() {
    const FILES: usize = 24;
    let mut generator = Lcg(7);
    let mut sources: Vec<String> = Vec::new();
    let mut index = ProjectIndex::new();

    for number in 0..FILES {
        sources.push(source_of(number, &mut generator));
        file(&mut index, number, &sources[number]);
    }
    check(&index, FILES, "after building");

    for number in (0..FILES).step_by(2) {
        cook(&mut index, number, &sources[number]);
    }
    check(&index, FILES, "after cooking half of them");
    // The corpus is not vacuous: the collisions the checks are about are really there.
    assert!(
        index.symbols_matching("widget", 1000).len() > 10,
        "one name, many files and scopes"
    );
    assert!(
        index
            .symbols_matching("made", 1000)
            .iter()
            .any(|symbol| symbol.fact.name.starts_with("Made")),
        "a name only the cooked reading declares"
    );
    assert!(
        !index
            .declarations_in("ns0", Path::new("/p/f21.h"))
            .is_empty()
    );

    // An edit, as a session does one: the summary is replaced under the same key and the reading is dropped.
    for number in [3, 4, 11] {
        index.forget(Path::new(&format!("/p/f{number}.h")));
        sources[number] = source_of(number + 100, &mut generator);
        file(&mut index, number, &sources[number]);
    }
    check(&index, FILES, "after forgetting and re-reading three files");

    // A replacement under a *different* key (the key is made from the text) drops the reading with it.
    let path = Path::new("/p/f6.h");
    sources[6].push_str(
        "int rekeyed;
",
    );
    let mut summary = summarize(path, &sources[6], SummaryKey::new(999, 0));
    for include in &mut summary.includes {
        include.resolved = Some(PathBuf::from(format!("/p/{}", include.spelling)));
    }
    index.insert(summary);
    assert!(
        index.cooked_declarations(path).is_none(),
        "the reading described a different key"
    );
    check(&index, FILES, "after a re-key");

    for number in [0, 2, 8] {
        index.forget_cooked(Path::new(&format!("/p/f{number}.h")));
    }
    check(&index, FILES, "after forgetting three readings");

    cook(&mut index, 8, &sources[8]);
    cook(&mut index, 8, &sources[8]);
    check(&index, FILES, "after cooking one twice");

    for number in 0..FILES {
        index.forget(Path::new(&format!("/p/f{number}.h")));
    }
    assert_eq!(
        index.distinct_names(),
        0,
        "an index with no files has no names"
    );
    assert!(index.symbols_matching("w", 10).is_empty());
}
