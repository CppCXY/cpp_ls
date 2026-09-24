//! Probe: what does **find references** cost, and which rung of the ladder pays for it?
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example find_references
//! ```
//!
//! `roadmap.md` §3.5 says this query is expensive and that the first thing to do is measure *how* expensive:
//! "找引用要扫多少文件、筛完还剩多少要解析". This probe answers that on the real standard-library closure, for the
//! macros that are actually used most in it — picked by counting, not by taste.
//!
//! # The ladder
//!
//! `index::references` narrows in four steps, and each one is measured here separately, because the *shape* of the
//! answer decides whether the fact layer needs a new field:
//!
//! ```text
//! 0. parse the candidates          what the query would cost if step 3 were a parse — the baseline
//! 1. the candidate set             the definer and everything that transitively includes it
//! 2. text pre-filter               `text.contains(name)` — exact, and it removes most candidates
//! 3. lex the survivors             CppLexer: comments and strings cannot reach the identifier filter
//! 4. the exact check               macro_definition per hit: is the name a macro *here*, and which #define
//! ```
//!
//! If rung 3 were close to rung 0, the honest answer would be "store identifier positions in the summary and never
//! read a file". The numbers are printed so that the decision can be re-made rather than believed, and the last
//! section measures what that table would cost.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::{
    DiskFiles, FileProvider, Known, MacroReferences, OpenDocuments, ProjectIndex, ReferenceBudget,
    ReferenceKind, Session, SessionFiles, WatchFilter, macro_references, normalize_path,
};
use cpp_parser::{CppLexer, CppParser, CppTokenKind, LexerConfig, ParserConfig};

/// What the project's one file includes, so that the closure is the standard library's own 454 files.
const MAIN: &str = "\
#include \"widget.h\"\n\
#include <string>\n\
#include <vector>\n\
#include <map>\n\
void f(std::string s) { s.size(); }\n";

/// How many macros the ladder is run for.
const NAMES: usize = 4;

fn main() {
    let root = std::env::temp_dir().join("cppls-find-references");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("the probe directory");

    let main_file = root.join("main.cpp");
    std::fs::write(&main_file, MAIN).expect("the fixture writes");
    std::fs::write(
        root.join("widget.h"),
        "#define WIDGET_API\nstruct Widget { int size; };\n",
    )
    .expect("the fixture writes");
    std::fs::write(
        root.join("other.cpp"),
        "#include \"widget.h\"\nWIDGET_API int other() { return 0; }\n",
    )
    .expect("the fixture writes");

    let documents = OpenDocuments::new();
    let files = SessionFiles::new(documents.clone(), DiskFiles);

    let started = Instant::now();
    let mut session = Session::open(&root, &files, WatchFilter::new(&root));
    let opened = started.elapsed();
    session.did_open(&main_file, MAIN);

    let started = Instant::now();
    session.index_everything();
    let indexed = started.elapsed();

    println!(
        "project: {} files indexed in {indexed:?} (opened in {opened:?})",
        session.index().len()
    );

    // The texts, read once: every rung below is a question about them, and re-reading per name would measure the
    // filesystem instead of the query.
    let started = Instant::now();
    let texts: Vec<(PathBuf, String)> = session
        .index()
        .summaries()
        .filter_map(|summary| {
            files
                .read(&summary.path)
                .map(|text| (summary.path.clone(), text))
        })
        .collect();
    let bytes: usize = texts.iter().map(|(_, text)| text.len()).sum();

    println!(
        "  read {} files / {} KB in {:?}",
        texts.len(),
        bytes / 1024,
        started.elapsed()
    );

    // --- which macros to ask about ----------------------------------------------------------------
    let names = busiest_macros(&session, &texts);
    println!(
        "\nthe macros this closure uses most (every identifier of every file counted, then intersected with the \
         names something defines; top {NAMES}):"
    );

    for (name, occurrences, defined_in) in &names {
        println!("  {name:<36} {occurrences:>7} identifier occurrences, {defined_in} defining file(s)");
    }

    // --- the ladder -------------------------------------------------------------------------------
    for (name, occurrences, _) in &names {
        println!("\n{name}");

        let started = Instant::now();
        let answer = macro_references(session.index(), &files, name, ReferenceBudget::default());
        let query = started.elapsed();

        let Known::Yes(found) = &answer else {
            println!("  not indexed: {answer:?}");
            continue;
        };

        // The files the query lexes: the **candidates** whose text contains the name. Computed here, from the
        // public API, so that rungs 3 and 0 are timed on exactly the same work — the first version of this probe
        // took "every file that contains the string", which is a larger set (files that cannot see the macro are
        // not candidates) and made the parse baseline look 5× worse than it is.
        let lexed = candidates_with_the_name(session.index(), &texts, name);

        if lexed.len() != found.lexed() {
            // The probe's own understanding of the query, checked against the query: if these disagree, one of the
            // two is wrong and the numbers below are about different work.
            println!(
                "  (the probe found {} files to lex, the query says {} — the numbers below are suspect)",
                lexed.len(),
                found.lexed()
            );
        }

        let started = Instant::now();
        let identifiers = identifiers_of(&lexed, name);
        let lexing = started.elapsed();

        let started = Instant::now();
        let named = tokens_named(&lexed, name);
        let parsing = started.elapsed();

        println!(
            "  1. candidates               {:>5} files (the definers + their transitive includers)",
            found.candidates()
        );
        println!(
            "  2. read                     {:>5} files — {} of them with no occurrence of the name, skipped without \
             lexing",
            found.looked_at, found.without_the_name
        );
        println!(
            "  3. lexed                    {:>5} files in {lexing:?} ({identifiers} identifiers spelled `{name}`)",
            found.lexed()
        );
        println!(
            "  4. checked                  {:>5} references in {} files — {} defines, {} undefs, {} uses, {} uncertain",
            found.total(),
            found.files.len(),
            count(found, |kind| matches!(kind, ReferenceKind::Definition)),
            count(found, |kind| matches!(kind, ReferenceKind::Undefinition)),
            count(found, |kind| matches!(kind, ReferenceKind::Use { .. })),
            found.uncertain()
        );
        println!(
            "     rejected                 {:>5} identifiers of that name are not this macro",
            found.rejected
        );
        println!(
            "     the whole query          {query:?} — the gap to rung 3 is the per-hit `macro_definition` check"
        );
        println!(
            "  0. the same files *parsed*   {:>5} files in {parsing:?} ({named} tokens named `{name}`) — what rung 3 \
             would cost as a parse",
            lexed.len()
        );
        println!(
            "     speedup of lexing over parsing: {:.1}×   |   a rename would edit {} places   |   the string \
             occurs in the closure {} times, the *identifier* {occurrences}",
            parsing.as_secs_f64() / lexing.as_secs_f64().max(f64::MIN_POSITIVE),
            found.rename("RENAMED").len(),
            texts
                .iter()
                .map(|(_, text)| text.matches(name.as_str()).count())
                .sum::<usize>(),
        );
    }

    // --- the cursor path, which is what an editor uses ---------------------------------------------
    //
    // Two positions, both of which a user reaches by right-clicking a name: a use in a buffer, and the `#define`
    // itself. The second one is why `name_at_including_directives` exists — a directive's name is tokens in a
    // directive, not a name node the scope walker sees.
    let unsaved = "#include \"widget.h\"\n#include <string>\nint x = WIDGET_API;\n";
    session.did_open(&main_file, unsaved);
    session.index_everything();

    let view = session.view(&main_file).expect("the buffer is the text");
    let at = view.source.find("WIDGET_API").expect("the use is there");

    match session.macro_references(&view, at) {
        Known::Yes(found) => println!(
            "\ncursor on `WIDGET_API` — the project's own macro, in a buffer that was never saved:\n  {} \
             references in {} files: {} defines, {} uses, {} uncertain; a rename edits {} places\n  {}",
            found.total(),
            found.files.len(),
            count(&found, |kind| matches!(kind, ReferenceKind::Definition)),
            count(&found, |kind| matches!(kind, ReferenceKind::Use { .. })),
            found.uncertain(),
            found.rename("WIDGET_ENTRY").len(),
            places(&found),
        ),
        other => println!("\ncursor on `WIDGET_API`: {other:?}"),
    }

    let header = session.view(root.join("widget.h")).expect("the header reads");
    let definition = header.source.find("WIDGET_API").expect("the definition is there");

    println!(
        "  the same question with the cursor on the `#define` in widget.h: {}",
        match session.macro_references(&header, definition) {
            Known::Yes(found) => format!("{} references", found.total()),
            other => format!("{other:?}"),
        }
    );

    // --- what a stored position table would cost ---------------------------------------------------
    //
    // The question the numbers above are for: would it be cheaper to put every identifier position in the summary
    // and never read a file? That needs one more number — how many identifiers there are to store.
    let started = Instant::now();
    let mut identifiers = 0usize;
    let mut tokens = 0usize;

    for (_, text) in &texts {
        let mut errors = Vec::new();
        let mut lexer = CppLexer::new(text, LexerConfig::default(), &mut errors);
        for token in lexer.tokenize() {
            tokens += 1;
            if token.kind == CppTokenKind::Identifier {
                identifiers += 1;
            }
        }
    }

    println!(
        "\nevery file of the closure lexed in {:?}: {tokens} tokens, {identifiers} of them identifiers",
        started.elapsed()
    );
    println!(
        "  lexing the whole closure costs about what *one* query costs — which is the finding that decides \
         whether a position table is worth its bytes"
    );
    println!(
        "  such a table would be ≈{} KB (8 bytes per identifier + the names) on top of the {} KB of summaries on disk",
        identifiers * 8 / 1024,
        directory_size(&root.join(".cppls")) / 1024
    );
    println!("\nproject: {}", root.display());
}

/// The defined macro names that occur most often as **identifiers** in the closure, with how many files define each.
///
/// Identifiers rather than substrings, and that distinction is the first thing this probe got wrong: counting
/// `text.matches(name)` put `_inline` at the top with 22 669 occurrences, of which **two** were identifiers — the
/// rest were the tail of `__forceinline` and friends. A ladder run on a name like that measures the substring
/// search, not the query.
///
/// The intersection with "something defines it" is what makes the list *macros* rather than popular words.
fn busiest_macros(
    session: &Session<'_, DiskFiles>,
    texts: &[(PathBuf, String)],
) -> Vec<(String, usize, usize)> {
    let mut definitions: HashMap<String, usize> = HashMap::new();

    for summary in session.index().summaries() {
        for fact in &summary.macros {
            if fact.kind.is_definition() {
                *definitions.entry(fact.name.clone()).or_default() += 1;
            }
        }
    }

    let mut occurrences: HashMap<&str, usize> = HashMap::new();

    for (_, text) in texts {
        let mut errors = Vec::new();
        let mut lexer = CppLexer::new(text, LexerConfig::default(), &mut errors);

        for token in lexer.tokenize() {
            if token.kind == CppTokenKind::Identifier
                && let Some(spelling) = text.get(token.range.start_offset..token.range.end_offset())
            {
                *occurrences.entry(spelling).or_default() += 1;
            }
        }
    }

    let mut counted: Vec<(String, usize, usize)> = definitions
        .into_iter()
        .filter_map(|(name, defined_in)| {
            occurrences
                .get(name.as_str())
                .map(|count| (name.clone(), *count, defined_in))
        })
        .collect();

    counted.sort_by(|one, other| other.1.cmp(&one.1).then(one.0.cmp(&other.0)));
    counted.truncate(NAMES);
    counted
}

/// The files the query will lex: the **candidates** whose text contains the name.
///
/// The candidate rule, written out a second time — in the probe rather than in the library, and on purpose: it is
/// the measurement's job to know what the query looked at, and a probe that asked the query would be measuring
/// nothing. Disagreement between the two is reported instead of hidden.
fn candidates_with_the_name<'t>(
    index: &ProjectIndex,
    texts: &'t [(PathBuf, String)],
    name: &str,
) -> Vec<&'t str> {
    let mut pending: Vec<PathBuf> = index
        .summaries()
        .filter(|summary| {
            summary
                .macros
                .iter()
                .any(|fact| fact.name == name && fact.kind.is_definition())
        })
        .map(|summary| summary.path.clone())
        .collect();

    let mut candidates: HashSet<String> = HashSet::new();

    while let Some(path) = pending.pop() {
        if !candidates.insert(normalize_path(&path, cfg!(windows))) {
            continue;
        }

        pending.extend(index.includers_of(&path));
    }

    texts
        .iter()
        .filter(|(path, text)| {
            candidates.contains(&normalize_path(path, cfg!(windows))) && text.contains(name)
        })
        .map(|(_, text)| text.as_str())
        .collect()
}

/// How many identifier tokens in these texts spell `name` — the rung the query does, timed on its own.
fn identifiers_of(texts: &[&str], name: &str) -> usize {
    let mut found = 0;

    for text in texts {
        let mut errors = Vec::new();
        let mut lexer = CppLexer::new(text, LexerConfig::default(), &mut errors);
        for token in lexer.tokenize() {
            if token.kind == CppTokenKind::Identifier
                && text.get(token.range.start_offset..token.range.end_offset()) == Some(name)
            {
                found += 1;
            }
        }
    }

    found
}

/// The same question asked of the **parser** rather than the lexer: the baseline rung 3 is compared against.
fn tokens_named(texts: &[&str], name: &str) -> usize {
    texts
        .iter()
        .map(|text| {
            let tree = CppParser::parse(text, ParserConfig::default());
            tree.get_red_root()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .filter(|token| token.text() == name)
                .count()
        })
        .sum()
}

/// The references as `file:kind` pairs, for a one-line answer a human can check.
fn places(found: &MacroReferences) -> String {
    found
        .files
        .iter()
        .flat_map(|file| file.references.iter().map(move |reference| (file, reference)))
        .map(|(file, reference)| {
            format!(
                "{}:{}",
                file_name(&file.file),
                match reference.kind {
                    ReferenceKind::Definition => "define",
                    ReferenceKind::Undefinition => "undef",
                    ReferenceKind::Use { .. } => "use",
                    ReferenceKind::Uncertain(_) => "maybe",
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// How many references of one kind the answer holds.
fn count(found: &MacroReferences, accepts: impl Fn(&ReferenceKind) -> bool) -> usize {
    found
        .files
        .iter()
        .flat_map(|file| &file.references)
        .filter(|reference| accepts(&reference.kind))
        .count()
}

/// The size of everything under a directory, in bytes — the cache, for the table-size comparison.
fn directory_size(directory: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };

    entries
        .filter_map(Result::ok)
        .map(|entry| {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => directory_size(&path),
                Ok(_) => entry.metadata().map(|meta| meta.len()).unwrap_or(0),
                Err(_) => 0,
            }
        })
        .sum()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}
