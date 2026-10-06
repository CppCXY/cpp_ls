//! What a standard-library closure looks like to this parser: cost, cleanliness, and where it breaks.
//!
//! ```text
//! g++ -M -std=c++20 t.cpp | tr '\\' '/' | tr ' ' '\n' | sort -u > files.txt
//! cargo run --release -p cpp_code_analysis --example std_probe -- files.txt
//! ```
//!
//! records the numbers this prints and what they decide. It exists so that the numbers can
//! be reproduced after a fix rather than remembered: the point of the standard-library work is to move "failing
//! files" towards zero, and a claim about progress that cannot be re-measured is not a claim.
//!
//! Three things are printed, in increasing order of how much they decide:
//!
//! 1. **the cost** — files, lines, bytes, and how long the parse takes;
//! 2. **the census** — how many files parse cleanly, and which messages account for the rest, most common first.
//!    A message count is *not* a defect count: the standard headers cascade, so one unread construct costs a
//!    hundred errors;
//! 3. **the first error of every failing file**, with its source line — which is the only view that shows the
//!    *cause*, because the first error is the one nothing above it explains. Plus the share of those lines that
//!    mention a macro the closure defines, which is what says whether the dominant family is "a macro the parser
//!    does not know" or something else.
//!
//! …and, on the one-walk path (`--seeds --closure`), a **fourth reading** that is about the program rather than
//! about the files: `the unit as one stream` — every file the walk reached cooked into one text in include order
//! (`TranslationUnit::cook_the_unit`), parsed once, with each error mapped back to the file it stands in. The
//! per-file census cannot answer "does the program read"; this can, and for the SDK corpus it says the two agree
//! (22 errors in `sourceannotations.h` either way, 4 over 3 files on the 455 corpus).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// The seam the two readings share: the unit's `MacroView` and the closure path's owned `MacroEnvironment` are
/// asked the same questions through this trait — see where `environment` is bound.
use cpp_parser::MacroFacts;

fn main() {
    let list = std::env::args().nth(1).expect("a file list");
    let paths: Vec<PathBuf> = std::fs::read_to_string(&list)
        .expect("the list reads")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();

    let key = cpp_code_analysis::SummaryKey::new(0, 0);

    // Every macro name the closure defines, which is the upper bound on what a table could know, and each
    // file's own — the difference between the two is the whole question of whether the file-local table is
    // enough.
    let mut closure_macros: HashSet<String> = HashSet::new();
    let mut own_macros: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    // The full summaries as well, because the positional second pass needs each file's **includes** and each
    // included file's macro facts — that is what `macros_from_direct_includes` turns into parser evidence.
    let mut summaries: HashMap<PathBuf, cpp_code_analysis::FileSummary> = HashMap::new();
    let mut definition_sources: HashMap<PathBuf, std::sync::Arc<str>> = HashMap::new();
    let seeded = std::env::args().any(|argument| argument == "--seeds");
    // `--closure` walks each direct include's **own** closure rather than stopping one hop down. The two answer
    // different questions — one hop is what the file literally includes, the closure is what the preprocessor
    // sees — and the difference is what the bill of remaining failures turns on.
    let closure = std::env::args().any(|argument| argument == "--closure");
    // `--macro NAME` asks what the environment knows about **one name** in each file, channel by channel. It is the
    // question that separates "the evidence was never built" from "the evidence is there and no rule used it" —
    // the same distinction the seed counts make one level up, asked about the macro a rule is waiting for.
    // `--macro NAME[,NAME…]` asks what the environment knows about those names in each file, channel by channel. It
    // is the question that separates "the evidence was never built" from "the evidence is there and no rule used
    // it" — the same distinction the seed counts make one level up, asked about the macros a rule is waiting for.
    // A list, because a condition is answered from a *chain* of names: asking about one of them at a time is how a
    // chain takes five runs to trace.
    let watched: Vec<String> = std::env::args()
        .position(|argument| argument == "--macro")
        .and_then(|at| std::env::args().nth(at + 1))
        .map(|names| names.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    // `--standard c++17`: what the **configuration** decides, injected as predefined macros. It is a flag rather
    // than a constant because the point is to measure it: a corpus read as C++11 and the same corpus read as C++20
    // answer `#if __cplusplus >= …` differently, and which branches that wakes up is a fact about the corpus.
    let standard: Option<String> = std::env::args()
        .position(|argument| argument == "--standard")
        .and_then(|at| std::env::args().nth(at + 1));
    // `--no-toolchain` is the "no compiler was found" world: then the **configuration alone** is what the
    // condition layer has, which is exactly what injecting the standard and the target is for.
    let without_toolchain = std::env::args().any(|argument| argument == "--no-toolchain");
    // `--cooked`: parse **what a compiler would parse** — the file preprocessed with the closure's macros —
    // instead of the file's own text. Message *counts* are the comparable thing; the positions are offsets
    // into the rendering (`CookedStream::render`), not into the file, so the first-error list is not.
    let cooked_mode = std::env::args().any(|argument| argument == "--cooked");
    // `--no-in-force-bodies`: ask the cooker for the **conservative** reading (a body is not a definition, so
    // nothing from the in-force channel is used). The default follows the library's own default, which is the
    // in-force channel — see `configuration_from_environment`. The flag used to be the other way round, and that
    // is worth a sentence: after the library's default changed, the probe kept pinning the *old* side of the
    // switch, so every census went on measuring a mode the product no longer used.
    let without_in_force_bodies = std::env::args().any(|argument| argument == "--no-in-force-bodies");
    // `--cooked-index`: also **index** the cooked reading of every file and map it back into the file, then report
    // what the two readings declare — the number that says what indexing the cooked stream is worth (a symbol a
    // macro declares exists only there, and a symbol in a branch nobody takes exists only in the raw reading).
    //
    // Off by default because it is a second indexing pass over the whole corpus (scopes + facts per file) and the
    // census's other numbers do not need it: an instrument that pays for every experiment it can run is an
    // instrument nobody runs.
    let cooked_index = std::env::args().any(|argument| argument == "--cooked-index");
    // `--dump-raw-only <path>`: write **every** name the raw reading declares and the cooked reading does not,
    // one line each, with what kind of declaration it is, whether it is inside a conditional block, and where it
    // is. The counts `--cooked-index` prints answer "how many"; this answers "**which**", which is the question a
    // decision about a raw-reading-only rule needs: a name that exists in a branch nobody takes is a different
    // cost from a name the file declares unconditionally.
    //
    // Sorted, and one record per line, so that two runs — with and without the rule — can be diffed as text.
    let dump_raw_only: Option<PathBuf> = std::env::args()
        .position(|argument| argument == "--dump-raw-only")
        .and_then(|at| std::env::args().nth(at + 1))
        .map(PathBuf::from);
    // `--session`: the same corpus through the **session**, which is the layer the product uses. Everything the
    // census does above is this probe's own loop over the list; this drives a `Session` with the same list —
    // `add_project_files` then `index_everything` — and splits the time into "indexed" and "cooked", because those
    // are the two things a language server does to a project and the second one is the round's subject. The two
    // measurements should agree in magnitude; where they do not, the difference is the session's own work (a unit
    // walk per file, the second pass that re-reads a file a later header decided, and the queues).
    let session_mode = std::env::args().any(|argument| argument == "--session");
    // The **one** lexical setting this probe uses, computed once and handed to every lexer below.
    //
    // It is the product's own default (`LexerConfig::default()`) and there is deliberately no flag to move it:
    // *the definition names the cooker expands come from the index*, which lexes with the library's reading, so a
    // flag here could only change half the lexing and would then measure a mixture of two readings rather than
    // either one. A reading is compared by changing it and running the corpus twice, which is what the `$` entry
    // in the architecture document records. Deriving this per call site is how the `$` family went unnoticed for a
    // round: `__$allowed_on_return` is a macro MSVC's SAL headers really define, and the parser refused the
    // character it is spelled with, so the definition arrived as three tokens and was lost — a property of the
    // *configuration*, invisible in the file, and one that only shows up if every layer reads the same way.
    let lexer_config = cpp_parser::LexerConfig::default();
    // `--render-to <dir>`: write the cooked rendering of every **failing** file into `dir`. The excerpt printed
    // below is 120 characters and the question a failure asks is "what did the expansion produce here, and what
    // should it have produced" — which needs the whole rendering, not a window. Written rather than printed so
    // that it can be read beside the file's own text in an editor.
    let render_to: Option<PathBuf> = std::env::args()
        .position(|argument| argument == "--render-to")
        .and_then(|at| std::env::args().nth(at + 1))
        .map(PathBuf::from);
    // `--tu-cache <dir>`: keep the translation unit's walk between runs, and reuse it while the closure is
    // byte-for-byte the same (see `cpp_code_analysis::TranslationUnitCache` for the key and why it is the content).
    let tu_cache: Option<cpp_code_analysis::TranslationUnitCache> = std::env::args()
        .position(|argument| argument == "--tu-cache")
        .and_then(|at| std::env::args().nth(at + 1))
        .map(cpp_code_analysis::TranslationUnitCache::new);

    // **A real indexer, not the convenience `summarize`**: that one resolves no includes at all (`NoFiles`), so a
    // probe built on it seeds nothing and the whole positional experiment would be a silent no-op — which the
    // `positional macro evidence:` line below now reports, so the two cannot be confused again.
    //
    // The corpus *is* the search path: every directory a listed file lives in, offered for both forms of
    // `#include`. That is an approximation of what the compiler searched (no `-I` order, no `#include_next`
    // subtleties), and the seed count is what says how far it got.
    // **One read of each file per run** (L1): the census reads every file to index it, and the TU cache reads
    // every file again to check that its entry is still valid — the same bytes, twice, and two readers can
    // disagree if the file moves between them. `CachedFiles` makes it one read, and the count is printed rather
    // than asserted: a cache nobody uses looks exactly like one that works.
    let files = cpp_code_analysis::CachedFiles::new(cpp_code_analysis::DiskFiles);
    let mut config = cpp_code_analysis::CompilerConfig::default();
    // **Sorted, and that is not tidiness**: the order of the search path decides which file an `#include` that
    // several directories can satisfy resolves to, and every number this probe prints is downstream of the
    // include facts. A `HashSet` here made the search order a matter of the run's hash seed: measured on one
    // binary and one 455-file list, three runs gave 1 460 025 / 1 589 438 / 1 606 474 seeds and 427 / 432 / 433
    // files with context — a 10% swing that is indistinguishable from a change to a *reading*, which is the one
    // thing this probe exists to measure. Error counts were stable throughout, which is why it went unnoticed.
    let mut directories: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    for path in &paths {
        if let Some(parent) = path.parent() {
            directories.insert(parent.to_path_buf());
        }
    }
    for directory in &directories {
        config = config
            .with_include_path(directory.clone())
            .with_system_include_path(directory.clone());
    }
    if let Some(standard) = standard.as_deref() {
        config = config.with_standard(standard);
    }
    let indexer = cpp_code_analysis::FileIndexer::new(&files, &config);

    // **What the compilation starts with** — and it is built even when no compiler was found, because that is the
    // case the configuration is supposed to cover by itself: `-std=c++17` decides `__cplusplus`, a target triple
    // decides the platform names, and a compiler that answered `-dM` is the *fuller* answer rather than the only
    // one. A run with neither is `incomplete()` — "nobody said" — which the evaluator turns into `Unknown`, so no
    // branch is answered wrongly; see `predefined_macros_of` and `Session`'s environment, which is built the same
    // way.
    let seed = if seeded && closure {
        let mut marked = cpp_code_analysis::graph::Marked::default();
        let mut found_a_toolchain = false;

        if !without_toolchain
            && let Some(toolchain) = paths.first().and_then(|first| {
                cpp_code_analysis::discover(
                    &files,
                    &cpp_code_analysis::DiskCommands,
                    cpp_code_analysis::BuildStatement::default(),                    first,
                    &cpp_code_analysis::Environment::current(),
                    &cpp_code_analysis::include::msvc::WindowsLayout::current(),
                )
            })
        {
            found_a_toolchain = true;
            for (name, value) in toolchain.macros() {
                marked.define_on_the_command_line(name, value);
            }
        }

        // …and then what the **configuration** decides, over the top: the project's `-std=` beats the compiler's
        // own default invocation, which is the point `Toolchain::search_paths` makes about passing it on.
        for definition in cpp_code_analysis::predefined_macros_of(&config) {
            marked.define_on_the_command_line(&definition.name, definition.value.as_deref());
        }

        if found_a_toolchain { marked } else { marked.incomplete() }
    } else {
        // Not seeding at all: an empty, incomplete state, so that nothing here pretends to know anything.
        cpp_code_analysis::graph::Marked::default().incomplete()
    };

    // The seed is what every `#ifdef __cplusplus` in the corpus is answered against, so whether it really holds the
    // compiler's names is worth one line rather than an assumption: `Unknown` silently keeps every conditional
    // `#define` out of the evidence, and a wrong branch answers the wrong reading. Printed whether or not a
    // toolchain was found, because "no compiler" is a case the configuration is supposed to cover by itself — the
    // **value** `__cplusplus` holds is asserted by `predefined_macros_of`'s own tests.
    if seeded && closure {
        use cpp_code_analysis::condition::MacroValues;

        println!(
            "compilation seed: __cplusplus defined {:?} | _WIN32 defined {:?} | uncertain about __cplusplus {} | \
standard {}",
            seed.lookup("__cplusplus").is_defined(),
            seed.lookup("_WIN32").is_defined(),
            seed.is_uncertain("__cplusplus"),
            standard.as_deref().unwrap_or("(none — the configuration decides nothing)"),
        );
    }

    // One cache of parsed #defines for the whole run: a definition does not depend on which file is being seeded, so
    // the feed costs one parse per definition rather than one per definition per file.
    let mut macro_definitions = cpp_code_analysis::MacroDefinitions::default();

    let started = std::time::Instant::now();
    for path in &paths {
        // Through the **content cache**, so that the TU cache's validity check below (which reads every file of
        // the closure) costs a hash rather than a second read of the disk.
        let Some(source) = files.text(path) else {
            continue;
        };
        let summary = indexer.index(path, &source, key);
        let names: HashSet<String> = summary
            .macros
            .iter()
            .filter(|fact| fact.kind.is_definition())
            .map(|fact| fact.name.clone())
            .collect();
        closure_macros.extend(names.iter().cloned());
        own_macros.insert(path.clone(), names);
        summaries.insert(path.clone(), summary);
        definition_sources.insert(path.clone(), source);
    }
    let indexed = started.elapsed();

    // **Who includes each file — along the chain the seed translation unit actually takes.**
    //
    // The corpus **is** the closure of one translation unit (the list's first entry), so the honest context of a
    // header is the file that reaches it along *that* TU's includes: the macro state a header sees is the state its
    // real includer had at the `#include`. What stood here was "the alphabetically first indexed file that
    // includes it", which is a different translation unit's state — and it cost a round of conclusions: the six
    // headers that appeared to be *broken* by the in-force switch (`cstdint`, `utility`, `tuple`, `new`,
    // `type_traits`) read **cleanly with no messages at all** when each was run on its own, so what the table said
    // was a property of the files was a property of the context the probe picked for them.
    //
    // The walk is breadth-first over each file's includes **in source order**, and first-reached wins — the same
    // determinism the previous rule was written for, for the same reason: a probe whose numbers move on their own
    // cannot measure anything (three runs of one binary once differed by 8% in seed count).
    //
    // A file the seed cannot reach gets **no context**, which is the honest answer rather than a worse one: nothing
    // in this translation unit includes it, so nothing here says what it sees.
    let mut includers: HashMap<PathBuf, (PathBuf, usize)> = HashMap::new();
    if let Some(seed) = paths.first().cloned() {
        let mut queue: std::collections::VecDeque<PathBuf> = std::collections::VecDeque::new();
        let mut reached: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        reached.insert(seed.clone());
        queue.push_back(seed);

        while let Some(path) = queue.pop_front() {
            let Some(summary) = summaries.get(&path) else {
                continue;
            };
            for include in &summary.includes {
                let Some(resolved) = include.resolved.as_ref() else {
                    continue;
                };
                if !summaries.contains_key(resolved) || !reached.insert(resolved.clone()) {
                    continue;
                }
                includers.insert(resolved.clone(), (path.clone(), include.range.start_offset));
                queue.push_back(resolved.clone());
            }
        }
    }

    let mut clean = 0usize;
    let mut failing = 0usize;
    let mut by_message: HashMap<String, usize> = HashMap::new();
    let mut explained_by_own = 0usize;
    let mut explained_by_closure = 0usize;
    let mut details: Vec<String> = Vec::new();
    // How many errors each file has, so the histogram below can say how much of the tail is one construct away.
    let mut counts: Vec<usize> = Vec::new();
    // **How many seeds the run actually produced.** A census that is *meant* to change a reading and does not is
    // either a finding or a broken instrument, and the two are indistinguishable without this number: the
    // convenience `summarize` resolves no includes at all (`NoFiles`), so a probe built on it seeds **nothing**.
    // **How many decision points a parse has** — how often the grammar asks what a name is as a macro — was counted
    // here, and the counters are gone with the macro evidence itself: see `ParserConfig`. What replaced the question
    // is the cook, which answers it once for the whole program instead of asking it per name.
    let mut total_seeds = 0usize;
    let mut bodies_with_text = 0usize;
    let mut files_with_seeds = 0usize;
    let mut seeding_time = std::time::Duration::ZERO;
    // How many conditional facts the closure walk met, and how many of them the condition layer put **in force**.
    // Two numbers rather than one, because "no conditional evidence" and "conditional evidence that was asked
    // about and refused" are different worlds and the corpus numbers look the same in both.
    let mut conditional_asked = 0usize;
    let mut conditional_taken = 0usize;
    // Replacement lists of macros whose definition is conditional but in force — the second channel, the one a rule
    // reads (`MacroEnvironment::with_bodies_in_force`). Counted apart from the definitions on purpose.
    let mut bodies_in_force = 0usize;
    // How many files were read **inside** a translation unit that includes them — the includer's macros are part
    // of what a header sees, and this says whether the corpus could supply them at all.
    let mut files_with_context = 0usize;
    // The **shape** distribution of what the seeds say. A name alone enables the rules that ask "is this a macro",
    // and a shape is what enables the ones that ask "what may stand here" — so "the evidence arrived and changed
    // nothing" has two very different causes, and this is what tells them apart.
    let mut seeds_by_shape: HashMap<&'static str, usize> = HashMap::new();
    let mut total_bytes = 0usize;
    let mut total_lines = 0usize;
    // **How much of the corpus the rendering actually contains.** A cook that decides a file's whole body is
    // inside an inactive branch renders *nothing*, and an empty file has no errors — so a census that counts only
    // "clean" counts such a file as read when it was dropped. Measured, which is why this exists: an A/B of one
    // cooker decision gave 249 clean / 49 messages against 248 / 70, and the *better-looking* side was the one
    // rendering **158 of 255 files to nothing** (the other side, 103) — the numbers said "one file worse" about a
    // change that made 55 more files readable at all.
    let mut rendered_to_nothing = 0usize;
    let mut rendered_bytes = 0usize;
    // Why a definition the evidence *has* did not make it into the table, summed over the corpus — see the four
    // fields of `cpp_code_analysis::Configuration`.
    //
    // **Counted once per unit on the one-walk path**, and that is a change of meaning worth naming: the unit's
    // facts are read once ([`cpp_code_analysis::TranslationUnit::definitions`]), so an unusable fact is counted
    // where it *is* rather than once per file that could see it. The per-file sum was `without_a_body` × the
    // number of files that include the header, which is not a measurement of the corpus; the closure path below
    // still counts per file, because there it is per file's closure that the number describes.
    let mut unusable_in_force = 0usize;
    let mut unusable_function_like = 0usize;
    let mut unusable_without_a_body = 0usize;
    let mut unusable_unreadable = 0usize;
    // Where the cooked half of a census actually goes, split at the one boundary that matters: building the macro
    // table out of the evidence, and cooking with it. `parse alone` covers both plus the walk, so without this
    // split "the parse is slow" and "the table is slow" are the same sentence.
    let mut table_time = std::time::Duration::ZERO;
    let mut cook_time = std::time::Duration::ZERO;
    // …and the other two halves of the same total: what it costs to *build a file's view* of the unit, and what
    // the parse of the rendering costs. `parse alone` is one number over all four, and a number that size cannot
    // say which of them to work on.
    let mut evidence_time = std::time::Duration::ZERO;
    // **One definition cache for the whole corpus.** Every file's configuration is built from definitions the
    // timeline already produced, and the same `#define` reaches hundreds of files: without this the run re-lexes
    // and re-parses each one once per file (2.5 million times for 42 939 definitions, 12.2 s of a 40 s census).
    let mut parsed_definitions = cpp_code_analysis::ParsedDefinitions::new();
    let mut view_time = std::time::Duration::ZERO;
    let mut parse_time = std::time::Duration::ZERO;
    // `--cooked-index`: the second indexing pass, and what the two readings declare — see the flag's note.
    let mut cooked_index_time = std::time::Duration::ZERO;
    let mut declarations_raw = 0usize;
    let mut declarations_cooked = 0usize;
    let mut gained = 0usize;
    let mut lost = 0usize;
    let mut cooked_mapped = 0usize;
    let mut cooked_dropped = 0usize;
    let mut cooked_errors_placed = 0usize;
    let mut cooked_errors_unplaced = 0usize;
    let mut gained_examples: Vec<String> = Vec::new();
    let mut shown_cooked_index_samples = 0usize;
    let mut gained_names: Vec<String> = Vec::new();
    let mut cooked_by_file: Vec<(PathBuf, cpp_code_analysis::CookedFile)> = Vec::new();
    // Every name only the raw reading declares, as a line of the `--dump-raw-only` file: the name, what kind of
    // declaration it is, whether it is conditional, and where it is. The name is first so that a `sort` of the whole
    // file groups a name with every file that declares it.
    let mut raw_only_records: Vec<String> = Vec::new();

    // **The translation unit, walked once** — the census's whole cost model changed when this replaced the
    // per-file closure walk, and the printed numbers say by how much: building every file's environment by walking
    // its own closure measured 147 s of a 174 s run, 2 489 142 macro entries and 17 618 794 condition evaluations
    // for 255 files. One walk answers every file's environment as a view, and a condition is evaluated once.
    //
    // `--closure` selects it, so the two readings stay comparable — the old path is still there, and the day it is
    // deleted is the day the two censuses agree.
    //
    // **`--tu-cache <dir>` keeps it between runs** (see `cpp_code_analysis::TranslationUnitCache`): the entry is
    // served while every file the walk entered still hashes the same, and the census prints which of the two
    // happened — "the numbers are the same because nothing was re-walked" and "the numbers are the same because
    // the walk agrees" are otherwise the same sentence.
    let mut the_unit_came_from_the_cache = false;
    let timeline = (seeded && closure).then(|| {
        let started = std::time::Instant::now();
        let root = paths[0].clone();
        // The key is the **compilation**: the list the corpus was read from, the standard, and the directories that
        // were searched. A different key is a different unit, and the two entries never overwrite each other.
        let key = cpp_code_analysis::fnv1a64(
            format!(
                "{}|{}|{}",
                root.display(),
                standard.as_deref().unwrap_or(""),
                directories
                    .iter()
                    .map(|directory| directory.display().to_string())
                    .collect::<Vec<_>>()
                    .join(";")
            )
            .as_bytes(),
        );
        // The **same** content cache the census read through: the cache's validity check reads every file of the
        // closure, and those bytes are the ones the census already has — see where `files` is built.
        if let Some(cache) = tu_cache.as_ref()
            && let Some(unit) = cache.get(&root, key, &files)
        {
            seeding_time += started.elapsed();
            the_unit_came_from_the_cache = true;
            return unit;
        }

        let unit = cpp_code_analysis::TranslationUnit::walk(
            summaries.get(&root).expect("the list's first file was indexed"),
            |wanted| {
                summaries.get(wanted).map(|summary| {
                    (summary, definition_sources.get(wanted).map(|text| &**text).unwrap_or(""))
                })
            },
            &seed,
            &mut macro_definitions,
        );
        seeding_time += started.elapsed();

        if let Some(cache) = tu_cache.as_ref() {
            let _ = cache.put(&root, key, &unit, &files);
        }
        unit
    });
    if let Some(unit) = timeline.as_ref() {
        conditional_asked += unit.conditional_facts;
        conditional_taken += unit.facts_in_force;
        total_seeds += unit.len();
        files_with_seeds += unit.files().count();
        let (with_a_body, in_force) = unit.bodies();
        bodies_with_text += with_a_body;
        bodies_in_force += in_force;
    }

    // **The unit's definitions, read once** — the second half of what the one-walk path replaced. A file's cook
    // used to walk its whole environment (every name the unit knows), parse each definition through the run's
    // cache and copy the result into a table of its own: 4.1 s of the 255-file census, for definitions that do not
    // depend on which file is asking. Only the **offset** a binding is in force from does, and the view resolves
    // that per query (`cpp_code_analysis::FileMacros`).
    let mut definitions_time = std::time::Duration::ZERO;
    let unit_definitions = timeline.as_ref().map(|unit| {
        let reading = std::time::Instant::now();
        let definitions = unit.definitions();
        definitions_time += reading.elapsed();
        definitions
    });
    if let Some(definitions) = unit_definitions.as_ref() {
        // Once per **unit**, not once per file: see the note where these four are declared.
        unusable_in_force += definitions.in_force_without_a_parameter_list;
        unusable_function_like += definitions.function_like_without_parameters;
        unusable_without_a_body += definitions.without_a_body;
        unusable_unreadable += definitions.unreadable;
    }

    // **What the compiler predefines**, built once: it is the same for every file, and it is the oldest layer of
    // every file's cook. The seed holds the compiler's own predefined names (`-dM`: `__cplusplus`, `_MSC_VER`,
    // `_WIN32`, …) plus what the configuration decides (`predefined_macros_of`) — without it the cook's own
    // condition evaluator meets `#ifdef __cplusplus` with no entry and applies C's rule, taking the **C branch of
    // every C++ header that asks** (measured on `codeanalysis/sourceannotations.h`: `SA_All` where the compiler
    // sees `All`).
    //
    // The order is the one that makes the closure win: the builtins are the **oldest** layer, and a definition the
    // closure carries shadows them for the same name — a header that redefines a builtin is in force over it.
    let mut seed_table = cpp_code_analysis::MacroTable::new();
    {
        let building_the_table = std::time::Instant::now();
        for name in seed.defined_names() {
            if let Some(definition) = seed.get(&name) {
                seed_table.define(definition.clone());
            }
        }
        table_time += building_the_table.elapsed();
    }

    let started = std::time::Instant::now();
    for (position, path) in paths.iter().enumerate() {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        total_bytes += source.len();
        total_lines += source.lines().count();

        // **The positional second pass**: what the file's own `#include`s contribute, each macro in force from the
        // offset its include ended at. `--seeds` turns it on, so the two censuses are the same command otherwise.
        let Some(summary) = summaries.get(path) else {
            continue;
        };
        // What the unit contributed, for the `--macro` line below: whether the file is **in** the walked
        // translation unit at all, and whether what it sees carries the name being watched.
        let mut in_the_unit = false;
        let mut the_view_carries_the_watched_name = false;
        // **The two readings have two types and one interface.** The unit's answer is a `MacroView` — a position
        // in the timeline, which materialises nothing — and the closure path's is an owned `MacroEnvironment`. The
        // probe asks both the same questions through `&dyn MacroFacts`, which is what that trait is for, and the
        // view is taken first so that the two are exclusive: a run with a unit never walks a closure.
        let mut the_view = None;
        if seeded && let Some(unit) = timeline.as_ref() {
            let building_the_view = std::time::Instant::now();
            the_view = unit.environment_of(path);
            view_time += building_the_view.elapsed();
            in_the_unit = the_view.is_some();
            if the_view.is_some() {
                files_with_context += 1;
            }
            the_view_carries_the_watched_name = the_view
                .as_ref()
                .is_some_and(|view| watched.iter().any(|name| view.knows(name)));
        }

        let owned_environment: Option<cpp_parser::MacroEnvironment> =
            if seeded && timeline.is_none() {
                // Building the evidence is itself a measurement: the closure version reads the whole include graph
                // of every direct include, so its cost is the thing that decides whether this layer can be
                // per-file.
                let started = std::time::Instant::now();
                let (seeds, bodies) = if closure {
                let look_up = |wanted: &std::path::Path| {
                    summaries.get(wanted).map(|summary| {
                        (summary, definition_sources.get(wanted).map(|text| &**text).unwrap_or(""))
                    })
                };
                let evidence =
                    cpp_code_analysis::macros_from_the_closure_with_bodies(summary, look_up, &seed, &mut macro_definitions);
                conditional_asked += evidence.conditional_facts;
                conditional_taken += evidence.facts_in_force;

                // **This file as part of the translation unit that includes it**: the includer's own macros up to
                // the point of its `#include`, seeded at offset 0 because another file's offsets mean nothing here.
                // That is the only way a header sees a name none of its own includes define — `commdlg.h` and
                // `STDMETHOD`, whose definition is in a file its own include list does not mention.
                let context = includers.get(path).map(|(includer, at)| {
                    let summary = summaries.get(includer).expect("an includer is indexed");
                    cpp_code_analysis::macros_in_force_before_the_include(summary, *at, look_up, &seed, &mut macro_definitions)
                });
                if context.is_some() {
                    files_with_context += 1;
                }
                if let Some(context) = context.as_ref() {
                    the_view_carries_the_watched_name = watched.iter().any(|name| {
                        context.macros.iter().any(|entry| &*entry.name == name)
                            || context
                                .conditional_bodies
                                .iter()
                                .any(|(defined, _, _, _)| &**defined == name)
                    });
                    conditional_asked += context.conditional_facts;
                    conditional_taken += context.facts_in_force;
                }

                // The file's **own** closure comes second, so a name its own includes define wins over the unit's.
                let (mut seeds, mut bodies) = match context {
                    Some(context) => (context.macros, context.conditional_bodies),
                    None => (Vec::new(), Vec::new()),
                };
                seeds.extend(evidence.macros);
                bodies.extend(evidence.conditional_bodies);
                (seeds, bodies)
            } else {
                (
                    cpp_code_analysis::macros_from_direct_includes_with_bodies(summary, |wanted| {
                        summaries.get(wanted).map(|summary| {
                            (summary, definition_sources.get(wanted).map(|text| &**text).unwrap_or(""))
                        })
                    }),
                    Vec::new(),
                )
            };
            seeding_time += started.elapsed();
            bodies_in_force += bodies.len();
            total_seeds += seeds.len();
            bodies_with_text += seeds
                .iter()
                .filter(|seed| seed.body_text.as_deref().is_some_and(|text| !text.trim().is_empty()))
                .count();
            if !seeds.is_empty() {
                files_with_seeds += 1;
            }
            for seed in &seeds {
                if let Some(cpp_parser::SymbolKind::Macro { body, .. }) = &seed.definition {
                    let shape = match body {
                        cpp_parser::MacroBody::Specifier => "Specifier",
                        cpp_parser::MacroBody::Statement => "Statement",
                        cpp_parser::MacroBody::Block => "Block",
                        cpp_parser::MacroBody::Expression => "Expression",
                        cpp_parser::MacroBody::Type => "Type",
                        cpp_parser::MacroBody::Unknown => "Unknown",
                    };
                    *seeds_by_shape.entry(shape).or_default() += 1;
                }
            }
            Some(cpp_parser::MacroEnvironment::from_included_macros(seeds).with_bodies_in_force(bodies))
        } else {
            None
        };

        // One value, two implementations — see the note where the view is taken.
        let environment: Option<&dyn cpp_parser::MacroFacts> = match the_view.as_ref() {
            Some(view) => Some(view),
            None => owned_environment
                .as_ref()
                .map(|environment| environment as &dyn cpp_parser::MacroFacts),
        };

        // Both parses take the **same** lexical reading — see `lexer_config`. A probe whose two arms read
        // differently cannot compare them, and the failure mode is silent: the numbers still print.
        let parser_config = || {
            cpp_parser::ParserConfig::default().with_lexer_config(lexer_config)
        };
        // **The environment is not handed to the parse.** The grammar reads a cooked stream and makes no reading
        // turn on a name's being a macro; see `FileIndexer::index`, which records the coordinate mismatch that made
        // the old wiring wrong rather than merely useless.
        let _ = environment;
        let raw_config = parser_config();

        if let Some(environment) = environment {
            for name in &watched {
                let Some(at) = source.find(name.as_str()) else {
                    continue;
                };

                let trim = |text: Option<&str>| {
                    text.map(|text| text.trim().chars().take(48).collect::<String>())
                };
                println!(
                    "MACRO {name} in {:<24} evidence {:<5} | positional body {:?} | in-force body {:?} | in the unit \
{in_the_unit}, the view carries it {the_view_carries_the_watched_name}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    environment.kind_of(name, at).is_some(),
                    trim(environment.body_text_of(name, at)),
                    trim(environment.body_text_in_force(name)),
                );
            }
        }
        // The rendering, when this run is a cooked one: parsed below, and written out when `--render-to` asked.
        //
        // **Cooked once**, and that is a repair rather than a tidy-up: this value used to be produced a second
        // time in the failure branch to find the error's window, which doubled the cost of every failing file and
        // — worse — left two recipes for the same stream, so an edit to one of them would have made the printed
        // window describe a stream the parser never saw.
        let rendered = if cooked_mode {
            let (tokens, _) = cpp_parser::lex(&source, &lexer_config);
            let converting = std::time::Instant::now();
            // **Two readings, two starting states, one interface.** On the one-walk path a file's starting state
            // is a `FileMacros`: the unit's definitions as *this* file sees them, over the builtins — a view, built
            // in constant time, with nothing copied. The closure path still materialises a `Configuration`, and
            // that is the whole difference the two censuses are there to measure.
            let file_macros = match (the_view, unit_definitions.as_ref()) {
                (Some(view), Some(definitions)) => Some(cpp_code_analysis::FileMacros::new(
                    view,
                    definitions,
                    Some(&seed_table),
                    !without_in_force_bodies,
                )),
                _ => None,
            };
            let configuration = match (&file_macros, environment) {
                (None, Some(environment)) => cpp_code_analysis::configuration_from_environment_and(
                    environment,
                    !without_in_force_bodies,
                    &mut parsed_definitions,
                ),
                _ => cpp_code_analysis::Configuration::default(),
            };
            // The builtins are the **oldest** layer of every file, whether the file is in the unit or not: a file
            // the unit never reaches (the 455-header census asks about files outside the walked closure) still
            // sees what the compiler predefines, and without it the cook takes the C branch of every `#ifdef
            // __cplusplus`. Measured: dropping it cost 7 of 455 files, which is how this line came to exist.
            let layered;
            let starting: &dyn cpp_code_analysis::MacroBindings = match &file_macros {
                Some(file_macros) => file_macros,
                None if environment.is_none() => &seed_table,
                None => {
                    layered = cpp_code_analysis::Over::new(&configuration.table, &seed_table);
                    &layered
                }
            };
            evidence_time += converting.elapsed();
            let building_the_table = std::time::Instant::now();
            let built_the_table = building_the_table.elapsed();

            // What the table **could not** take from the evidence, summed over the corpus. These are the reasons a
            // name stayed a name, and without them a census can only say "the macro did not expand" — which is the
            // same sentence for "the evidence has no body", "the body has no parameter list" and "the evidence was
            // never built at all", three different pieces of work. (On the one-walk path they were counted once
            // for the unit — see where they are declared.)
            unusable_in_force += configuration.in_force_without_a_parameter_list;
            unusable_function_like += configuration.function_like_without_parameters;
            unusable_without_a_body += configuration.without_a_body;
            unusable_unreadable += configuration.unreadable;

            let rendering = cpp_code_analysis::cook_with(&source, &tokens, starting).render();
            table_time += built_the_table;
            cook_time += building_the_table.elapsed() - built_the_table;
            Some(rendering)
        } else {
            None
        };

        let (tree, audit) = match &rendered {
            Some(rendered) => {
                let parsing = std::time::Instant::now();
                rendered_bytes += rendered.text.len();
                if rendered.text.trim().is_empty() {
                    rendered_to_nothing += 1;
                }
                let tree = cpp_parser::CppParser::parse_with_audit(&rendered.text, parser_config());
                parse_time += parsing.elapsed();

                // **`--cooked-index`**: the same rendering, indexed as a file of its own and mapped back. The
                // question is not whether the plumbing holds (the level-0 probe answered that: every fact range
                // maps) but **what the reading is worth** — which declarations a compiler sees that the file's
                // own text does not say, and which it does not see because their branch is not taken.
                if cooked_index {
                    let indexing = std::time::Instant::now();
                    let indexed = indexer.index_rendering(path, rendered, key);
                    let cooked_summary = &indexed.summary;
                    let report = indexed.mapped;
                    cooked_index_time += indexing.elapsed();
                    cooked_mapped += report.placed;
                    cooked_dropped += report.dropped;
                    // **Where the errors land** — the question the diagnostics channel asks of this layer: the
                    // rendering is what a compiler parses, so its errors are the honest answer, and they are only
                    // publishable for the ones the map can place in *this* file. A count of "clean files" cannot
                    // tell "nothing was wrong" from "something was wrong and could not be shown here".
                    cooked_errors_placed += indexed.diagnostics.len();
                    cooked_errors_unplaced += indexed.unplaced;

                    if let Some(raw) = summaries.get(path) {
                        let names = |facts: &[cpp_code_analysis::DeclFact]| -> HashSet<String> {
                            facts.iter().map(|fact| fact.qualified_name()).collect()
                        };
                        let raw_names = names(&raw.declarations);
                        let cooked_names = names(&cooked_summary.declarations);
                        declarations_raw += raw_names.len();
                        declarations_cooked += cooked_names.len();
                        gained += cooked_names.difference(&raw_names).count();
                        lost += raw_names.difference(&cooked_names).count();
                        // The **smallest** name of the difference rather than whichever the hash order offered
                        // first: the examples are printed beside the counts, and an example that changes between
                        // two runs of the same build invites reading a difference into the sample.
                        if let Some(example) = cooked_names.difference(&raw_names).min() {
                            gained_examples.push(example.clone());
                        }

                        // **The names themselves, for a few files.** A count of names that appear only after
                        // expansion is a claim about the reading, and a claim about a reading is exactly the kind
                        // that turns out to be an artefact — so the sample is printed, and what it should look
                        // like is `DECLARE_HANDLE`'s generated struct and its `unused` member, a typedef out of a
                        // macro, a function pointer type a macro declares.
                        if shown_cooked_index_samples < 4 {
                            shown_cooked_index_samples += 1;
                            println!("{}", path.display());
                            let mut only_cooked: Vec<&String> =
                                cooked_names.difference(&raw_names).collect();
                            let mut only_raw: Vec<&String> =
                                raw_names.difference(&cooked_names).collect();
                            only_cooked.sort();
                            only_raw.sort();
                            for name in only_cooked.into_iter().take(6) {
                                println!("   only after expansion: {name}");
                            }
                            for name in only_raw.into_iter().take(6) {
                                println!("   only in the raw reading: {name}");
                            }
                        }

                        // Kept for the **query** the index is asked after the loop: every name only the cooked
                        // reading declares, and what each file was cooked into.
                        //
                        // **All of them, not a first few** — and the sample below is taken from the *sorted* list.
                        // It used to take two per file off a `HashSet` difference, which is not a stable set: two
                        // runs of the same build sampled different names, so "the index answers for 4 of 40" and
                        // "6 of 40" were two samples rather than two builds, and an A/B against that number would
                        // have read its own sampling noise as a regression. A sorted sample of a fixed size is the
                        // same 200 names whenever the two builds agree, which is what makes it a gate.
                        gained_names.extend(cooked_names.difference(&raw_names).cloned());
                        // The **records**, for the question the counts cannot answer. A fact's guard says whether
                        // the declaration is inside a conditional block, which is the difference between "this name
                        // is in a branch nobody compiles" and "this name is in the file's own unconditional text" —
                        // and it is the first thing a reader of the dump needs to know.
                        if dump_raw_only.is_some() {
                            for fact in &raw.declarations {
                                let name = fact.qualified_name();
                                if cooked_names.contains(&name) {
                                    continue;
                                }

                                raw_only_records.push(format!(
                                    "{}\t{:?}\t{}\t{}\t{}",
                                    name,
                                    fact.kind,
                                    match &fact.guard {
                                        cpp_code_analysis::FactGuard::Unconditional => "unconditional",
                                        cpp_code_analysis::FactGuard::Region(_) => "conditional",
                                    },
                                    path.display(),
                                    fact.name_range.start_offset,
                                ));
                            }
                        }
                        cooked_by_file.push((
                            path.clone(),
                            cpp_code_analysis::CookedFile {
                                declarations: cooked_summary.declarations.clone(),
                                macros: cooked_summary.macros.clone(),
                                diagnostics: indexed.diagnostics.clone(),
                                unplaced: indexed.unplaced,
                            },
                        ));
                    }
                }

                tree
            }
            None => cpp_parser::CppParser::parse_with_audit(&source, raw_config),
        };
        let _ = audit;
        let errors = tree.get_errors();

        // **`--render-to <dir>`: write the rendering out — every file, before the census decides which ones
        // failed.** A 120-character excerpt is enough to see *that* a failure is in expanded text and not enough to
        // see what the expansion should have been; the rendering is the product's own output, so writing it beside
        // the census is inspection rather than a second implementation of it. Every file, not only the failing
        // ones, because the question a reading change asks is **comparative** — "what did this file's expansion
        // become" — and the files it *fixed* answer it as much as the ones it broke. (The first version of this
        // wrote only the failures, and could not answer the question it was added for.) The name carries the list
        // index because basenames repeat across the SDK's `um\` and `shared\` — `winnt.h` is both.
        if let (Some(directory), Some(rendered)) = (render_to.as_ref(), rendered.as_ref())
            && let Some(name) = path.file_name()
        {
            let _ = std::fs::create_dir_all(directory);
            let _ = std::fs::write(
                directory.join(format!(
                    "{:04}_{}.rendered",
                    position,
                    name.to_string_lossy()
                )),
                &rendered.text,
            );
        }

        if errors.is_empty() {
            clean += 1;
            counts.push(0);
            // The invariants hold on this corpus too, and are checked rather than assumed: a header is not a
            // gentler input than a test fixture. **Not in cooked mode**: there the tree is over the rendering,
            // which is not the file — the directives are gone and one branch of every conditional with them.
            // Losslessness is the raw stream's property (`CppSyntaxTree::get_tokens`), not this text's.
            if !cooked_mode {
                assert_eq!(
                    tree.to_source_text(),
                    source,
                    "losslessness broke on {}",
                    path.display()
                );
            }
            continue;
        }

        failing += 1;
        counts.push(errors.len());
        for error in errors {
            *by_message.entry(error.message.clone()).or_default() += 1;
        }

        // In cooked mode the errors are offsets into the **rendering** — the same one written above, and the same
        // one that was parsed — so neither the line index nor the text window below can be read against the file.
        // The map is what turns one into the other, and this is its first consumer: `reported_at` is the place to
        // point a reader at (the call site when a macro produced the text), `written_at` is where the spelling is
        // *when it is in this file*, and `written_span` where a node came from. Without the map a cooked failure
        // would be a message with no address at all.
        let (line, column, window) = match &rendered {
            Some(rendered) => {
                let at = usize::from(errors[0].range.start());
                let cooked = &rendered.text[at.saturating_sub(60).min(rendered.text.len())
                    ..at.saturating_add(60).min(rendered.text.len())];
                // **The reported place, not the written one**: a token a macro produced was written in the
                // header that defines it (or in a reconstruction, which is nowhere), and what the reader can act
                // on is the invocation in *this* file. `reported_at` is always a position in this file;
                // `written_at` is `None` for everything that came from elsewhere, which is why it is printed
                // beside the window rather than used as the address.
                let reported = rendered.reported_at(at);
                let written = rendered.written_at(at);
                // The file position, as line and column — counted here rather than through the line index
                // because the offset came from the map and not from the file.
                let offset = reported.map_or(0, |range| range.start_offset).min(source.len());
                let before = &source[..offset];
                let line = before.lines().count();
                let column = before.len() - before.rfind('\n').map_or(0, |at| at + 1);
                (
                    line,
                    column,
                    format!(
                        // The marker goes **first** because the printed window is cut to 70 characters — a note
                        // at the end of it is a note nobody reads. It says whether the text the error is about was
                        // written in this file or pasted in from a header, which decides whether the line printed
                        // beside it is the line to fix.
                        "RENDERED[{}] …{cooked}…",
                        match written {
                            Some(_) => "here",
                            None => "elsewhere",
                        }
                    ),
                )
            }
            None => {
                let index = cpp_parser::LineIndex::parse(&source);
                // **A file whose first error has no position still gets a line here.** What stood here was a
                // `continue`, which drops the file from the list while leaving it in the failure count — a silent
                // hole in the only view of a failure's *cause*. It was investigated on a suspicion that turned out
                // to be wrong (the 255-file SDK corpus reads 37 files as failing, and all 37 are printed; the
                // "eleven missing" were my own grep refusing a four-digit line number), and it is kept anyway:
                // silence is the wrong answer at a position the index refuses, and saying so costs one `match`.
                let (line, column, refused) =
                    match index.position_of(usize::from(errors[0].range.start()), &source) {
                        Some((line, column)) => (line, column, false),
                        None => (source.lines().count(), 0, true),
                    };
                let window: Vec<&str> =
                    source.lines().skip(line.saturating_sub(2)).take(3).collect();
                let joined = window.join(" ");
                (
                    line,
                    column,
                    if refused {
                        format!(
                            "NO POSITION FOR OFFSET {} (past the end of {} bytes) {}",
                            usize::from(errors[0].range.start()),
                            source.len(),
                            joined
                        )
                    } else {
                        joined
                    },
                )
            }
        };
        let window = [window.as_str()];


        let mentions = |names: &HashSet<String>| {
            window.join(" ").split(|c: char| !(c.is_alphanumeric() || c == '_')).any(
                |word| word.len() > 2 && names.contains(word),
            )
        };

        if own_macros.get(path).is_some_and(&mentions) {
            explained_by_own += 1;
        }
        if mentions(&closure_macros) {
            explained_by_closure += 1;
        }

        // **Which of the names on the line the environment can actually *read*** — the question that separates
        // "the evidence was never built" from "the evidence is there and no rule used it", which look identical in
        // every other number this probe prints. A name is listed when a **body** for it is in force: the file's own
        // `#define` or one the closure carried in.
        let line_text = window.join(" ");
        let readable: Vec<&str> = line_text
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|word| word.len() > 2)
            .filter(|word| {
                environment.as_ref().is_some_and(|environment| {
                    // **Both channels**, because a body can arrive either way: positionally (an unconditional
                    // `#define`) or through the conditional one that only a branch in force fills.
                    environment.body_text_of(word, 0).is_some()
                        || environment.body_text_in_force(word).is_some()
                })
            })
            .collect();
        let readable = if readable.is_empty() {
            String::new()
        } else {
            format!(" | bodies in force: {}", readable.join(", "))
        };

        details.push(format!(
            // **The full path**, not the file's name: a closure reaches several copies of a header that share a
            // name (`winnt.h` exists in the SDK's `um\` and in `shared\`, and the list spellings are normalized),
            // so a reader who goes to `<name>:<line>` by name reads a *different file* and concludes the line
            // number is wrong — measured, on the Windows SDK corpus, before this was changed.
            "{:>4}:{:<3} {:<44} | {} :: {}{}",
            line + 1,
            column,
            errors[0].message,
            window.last().unwrap_or(&"").trim().chars().take(70).collect::<String>(),
            path.display(),
            readable,
        ));
    }
    let parsed = started.elapsed();

    // **The query the index is asked**: can it answer for a name only the cooked reading declares?
    //
    // The counts above are about the *summaries*; this is about the **index**, which is what a feature asks. The
    // same project index, the same names, asked twice — once with only the raw summaries and once with the cooked
    // declarations added — so the difference is the reading and nothing else.
    let mut resolved_before = 0usize;
    let mut resolved_after = 0usize;
    let mut sampled = 0usize;
    if cooked_index && !gained_names.is_empty() && !paths.is_empty() {
        // The sample, in a **fixed order** — see where the names are collected: a `HashSet`'s order is not stable
        // between runs, and a gate whose sample moves cannot be compared with itself.
        gained_names.sort();
        gained_names.dedup();
        sampled = gained_names.len().min(200);

        let answered = |index: &cpp_code_analysis::ProjectIndex| {
            gained_names[..sampled]
                .iter()
                .filter(|name| {
                    // **"Can the index answer at all"**, not "is the answer unambiguous": a name two visible
                    // headers declare is `Ambiguous`, which is an answer about the project rather than a miss, and
                    // counting it as a miss would hide exactly what this line is measuring.
                    !matches!(
                        index.definition(name, &paths[0]),
                        cpp_code_analysis::Known::Unknown(
                            cpp_code_analysis::UnknownReason::NotDeclaredHere(_)
                        )
                    )
                })
                .count()
        };

        let mut index = cpp_code_analysis::ProjectIndex::new();
        for summary in summaries.values() {
            index.insert(summary.clone());
        }
        resolved_before = answered(&index);

        for (path, reading) in cooked_by_file.drain(..) {
            index.insert_cooked(&path, reading);
        }
        resolved_after = answered(&index);
    }

    // **The other half of the reading: the unit as one program.**
    //
    // Everything above cooks *one file at a time*, which answers "does this header read on its own" and cannot
    // answer "does the program read": a declaration in `vector` and a use in the source are two streams with
    // nothing tying them together. This stitches the walk's own order (`TranslationUnit::cook_the_unit`) and
    // parses the result once, so the number below is a reading of the program rather than of a pile of headers.
    //
    // Only on the one-walk path: the closure path has no single unit to stitch, which is exactly the difference
    // the two paths exist to measure.
    let unit_stream: String;
    if let (Some(unit), Some(definitions)) = (timeline.as_ref(), unit_definitions.as_ref()) {
        let stitching = std::time::Instant::now();
        let stitched = unit.cook_the_unit(
            &definition_sources,
            definitions,
            Some(&seed_table),
            !without_in_force_bodies,
            // The census has no include search here, so `__has_include` answers `Unknown` — which the census counts
            // rather than decides on, and which is the honest answer for a probe that resolved nothing.
            None,
        );
        let stitching = stitching.elapsed();

        let parsing = std::time::Instant::now();
        let tree = cpp_parser::CppParser::parse_with_audit(
            &stitched.text,
            cpp_parser::ParserConfig::default().with_lexer_config(lexer_config),
        );
        let parsing = parsing.elapsed();

        // **Where an error is**: the unit's map turns an offset in the stitched stream into the file it stands
        // in, which is the only thing that makes a count of errors in a whole program useful.
        let errors = tree.0.get_errors();
        let mut files = std::collections::HashSet::new();
        let mut first = None;
        for error in errors {
            if let Some((file, written)) = stitched.written_at(usize::from(error.range.start())) {
                files.insert(file);
                if first.is_none() {
                    first = stitched
                        .file_of(file)
                        .map(|path| format!("{}:{written:?}", path.display()));
                }
            }
        }
        unit_stream = format!(
            "the unit as one stream: {} tokens from {} of {} files ({} without text) | stitched {stitching:?} | \
             parsed {parsing:?} | {} errors over {} files{}\n         ",
            stitched.len(),
            stitched.files_with_tokens(),
            stitched.files.len(),
            stitched.missing,
            errors.len(),
            files.len(),
            match &first {
                Some(where_it_is) => format!(" — first at {where_it_is}"),
                None => String::new(),
            },
        );
    } else {
        unit_stream = "the unit as one stream: no unit — see `--seeds --closure`\n         ".to_string();
    }

    // Which experiment this run is, stated in the output: "the evidence arrived and changed nothing" and "the
    // evidence was never built" look identical in the numbers, and that confusion has already cost this project
    // one vacuous census.
    if seeded {
        println!(
            "seeding mode: {}\n",
            if closure {
                "the closure of each direct include"
            } else {
                "direct includes only"
            }
        );
    }


    // The **lexical** reading, on its own line, because it is a reading a run can be wrong about: with `$` refused
    // the MSVC SAL headers lose every `__$allowed_*` definition, and the numbers below then describe the lexer
    // rather than the parser. Stated rather than assumed.
    println!(
        "reading: {} | `$` {}",
        if cooked_mode {
            "the cooked rendering"
        } else {
            "the file's own text"
        },
        if lexer_config.dollar_in_identifier {
            "accepted in identifiers (the product's default)"
        } else {
            "refused (the standard's answer)"
        }
    );

    // How many definitions the run actually read — the number that used to be "files × definitions in force".
    //
    // **Two paths, two caches.** The closure path parses through the run's `ParsedDefinitions` (keyed by the
    // definition's text, because the text is all it has). The one-walk path reads the unit's definitions once and
    // keeps them **with the positions they were written at**, so each is parsed once per unit and the count is the
    // unit's own vocabulary — see `TranslationUnit::definitions`.
    let distinct_definitions = match unit_definitions.as_ref() {
        Some(definitions) => definitions.len(),
        None => parsed_definitions.len(),
    };

    // The **cooked index** line — what each reading declares, and what the cooked one buys. Only on
    // `--cooked-index`, because it is a second indexing pass over the corpus.
    let cooked_index_line = if cooked_index {
        let mut examples = gained_examples.clone();
        examples.sort_unstable();
        examples.dedup();
        examples.truncate(6);

        // **What one edit would cost**, from the same index: the session drops the cooked reading of every file a
        // changed file can be seen from (`ProjectIndex::dependents_of`), so the widest dependent cone in a corpus is
        // the number of files a single keystroke in that header would have to cook again. The cone of every file is
        // one breadth-first walk each — quadratic in the corpus and measured anyway, because the alternative is a
        // number nobody has: an edit to a header that most of the project includes is the case that decides whether
        // cooking the whole project is affordable, and "how wide is it" is not answerable by reading the code.
        let mut widest = (0usize, String::new());
        let mut cones: Vec<usize> = Vec::new();
        // The graph the session has: the same summaries, indexed for their include edges. `dependents_of` is the
        // reverse walk, and what it needs is exactly what a summary carries.
        let mut graph = cpp_code_analysis::ProjectIndex::new();
        for summary in summaries.values() {
            graph.insert(summary.clone());
        }
        for summary in graph.summaries() {
            let dependents = graph.dependents_of(&summary.path).len();
            cones.push(dependents);
            if dependents > widest.0 {
                widest = (dependents, summary.path.to_string_lossy().to_string());
            }
        }
        cones.sort_unstable();

        format!(
            "the cooked index: declarations {declarations_raw} raw / {declarations_cooked} cooked | \
             +{gained} only after expansion | -{lost} only in the raw reading | \
             {cooked_mapped} ranges mapped back, {cooked_dropped} dropped | indexed in {cooked_index_time:?} | \
             the errors it found: {cooked_errors_placed} of {} placed in their own file | \
             the index answers for {resolved_before} of {sampled} of those names before and {resolved_after} after | \
             the dependent cone: widest {} files ({}), median {} | \
             for example {}\n         ",
            cooked_errors_placed + cooked_errors_unplaced,
            widest.0,
            widest.1,
            cones.get(cones.len() / 2).copied().unwrap_or(0),
            if examples.is_empty() {
                "(none)".to_string()
            } else {
                examples.join(", ")
            }
        )
    } else {
        String::new()
    };

    // **`--session`: the corpus through the layer the product uses.** `indexed` is "every file's summary is in the
    // index", `cooked` is "and every file has a reading of what a compiler sees" — the two moments the session
    // distinguishes ([`Session::is_idle`] and [`Session::pending_work`]), and the second one is what the round is
    // about: the diagnostic channel publishes an answer for every indexed file, so a file with no reading is a file
    // answered from its own text.
    let session_line = if session_mode {
        // **An empty root, created here.** `Session::with_config` scans its root for sources, so pointing it at the
        // corpus's own directory measures the *neighbourhood* rather than the list: with the root set to the first
        // file's directory this probe once indexed 31 unrelated scratch files from that directory — among them a
        // 160 KB single-line file that costs 36 s to parse — and reported "indexed in 88 s" for a corpus the census
        // reads in three. Nothing here is resolved through the root (a quoted include resolves against the including
        // file's directory), so an empty one is the honest setting: the corpus is the list and nothing else. The
        // index size is printed below so that "the corpus grew" cannot be invisible again.
        let root = std::env::temp_dir().join("stdprobe-session-root");
        let _ = std::fs::create_dir_all(&root);
        let documents = cpp_code_analysis::OpenDocuments::new();
        let providers = cpp_code_analysis::SessionFiles::new(documents, cpp_code_analysis::DiskFiles);
        let filter = cpp_code_analysis::WatchFilter::new(&root);
        let mut session = cpp_code_analysis::Session::with_config(
            &root,
            providers,
            filter,
            cpp_code_analysis::CompilerConfig::default(),
        );

        let added = session.add_project_files(paths.iter().cloned());
        let started = std::time::Instant::now();
        while !session.is_idle() {
            session.advance(64);
        }
        let indexed = started.elapsed();
        while session.pending_work() > 0 {
            session.advance(64);
        }
        let cooked = started.elapsed();

        let with_a_reading = paths
            .iter()
            .filter(|path| session.index().cooked_declarations(path).is_some())
            .count();

        format!(
            "the session: {added} project files | indexed in {indexed:?} | cooked in {cooked:?} (total) | \
             the index holds {} files | {with_a_reading} of {} have a reading | left: {} to read, {} to cook\n         ",
            session.index().len(),
            paths.len(),
            session.pending(),
            session.pending_cooking(),
        )
    } else {
        String::new()
    };

    println!(
        "files {} | clean {} | failing {} | {} KB | {} lines\n\
         index (parse + scopes + facts) {:?} | parse alone {:?}\n\
         content: {} reads for {} files ({:.2} per file) — one read per file per run, shared with the TU cache's \
validity check\n\
         {failing} failures: first error on a line mentioning a macro this file defines {explained_by_own} \
         ({:.0}%), any macro the closure defines {explained_by_closure} ({:.0}%)\n\
         positional macro evidence: {total_seeds} seeds over {files_with_seeds} files ({bodies_with_text} with body text), \
built in {seeding_time:?}\n\
         conditional facts met {conditional_asked} | branches in force {conditional_taken} | bodies in force \
{bodies_in_force} (no toolchain means none can be answered)\n\
         read inside an includer {files_with_context} files (the translation unit's half of the environment)\n\
         cooked: definitions {definitions_time:?} ({distinct_definitions} read) | evidence {evidence_time:?} | \
macro table {table_time:?} | expansion {cook_time:?} | view {view_time:?} | parse {parse_time:?}\n\
         rendering: {rendered_to_nothing} of {} files rendered to nothing (whitespace only) | {rendered_bytes} bytes \
of rendering for {total_bytes} of text{}{}\n\
         table: left out — bodies in force without a parameter list {unusable_in_force} | function-like definitions \
without one {unusable_function_like} | definitions without a body {unusable_without_a_body} | unreadable \
definitions {unusable_unreadable}\n\
         {unit_stream}         {cooked_index_line}{session_line}seed shapes: {}\n\
         macro-question counters: removed with the parser's macro evidence",
        paths.len(),
        clean,
        failing,
        total_bytes / 1024,
        total_lines,
        indexed,
        parsed,
        files.reads(),
        files.paths_read(),
        files.reads() as f64 / files.paths_read().max(1) as f64,
        explained_by_own as f64 * 100.0 / failing.max(1) as f64,
        explained_by_closure as f64 * 100.0 / failing.max(1) as f64,
        paths.len(),
        // A **clean** file whose rendering is empty was not read at all, and the two readings of the same corpus
        // are told apart by this and by nothing else — see `rendered_to_nothing`.
        if cooked_mode && rendered_to_nothing > 0 {
            "  ← an empty rendering has no errors: those files were dropped, not read"
        } else {
            ""
        },
        // **Where the unit came from**, beside the reading: a cached run and a walked run print the same numbers by
        // design, so "nothing was re-walked" is the only thing that tells a cache measurement from a real one.
        if seeded && closure {
            if the_unit_came_from_the_cache {
                "  · the translation unit came **from the cache** (nothing was walked)"
            } else if tu_cache.is_some() {
                "  · the translation unit was **walked** and kept"
            } else {
                ""
            }
        } else {
            ""
        },
        {
            let mut pairs: Vec<(&str, usize)> = seeds_by_shape.iter().map(|(k, v)| (*k, *v)).collect();
            pairs.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
            pairs
                .into_iter()
                .map(|(shape, count)| format!("{shape} {count}"))
                .collect::<Vec<_>>()
                .join(" | ")
        },
    );

    // **Where the index's time went**, stage by stage — the table `crate::stages` keeps, printed here because a
    // census line says *what* got slower and only a table says *where*.
    println!("{}", cpp_code_analysis::stages::StageTimes::read().report());

    // **The raw-only names, written where two runs can be diffed** — see `--dump-raw-only`. Sorted, so a `diff` of
    // two runs shows what a rule change moved rather than what order the walk happened to visit files in.
    if let Some(target) = &dump_raw_only {
        raw_only_records.sort();
        let unconditional = raw_only_records
            .iter()
            .filter(|record| record.split('\t').nth(2) == Some("unconditional"))
            .count();

        std::fs::write(target, raw_only_records.join("\n") + "\n")
            .unwrap_or_else(|error| panic!("cannot write {}: {error}", target.display()));

        println!(
            "\n--- raw-only declarations: {} records, {} unconditional, {} conditional (written to {}) ---",
            raw_only_records.len(),
            unconditional,
            raw_only_records.len() - unconditional,
            target.display()
        );
    }

    let mut ranked: Vec<(&String, &usize)> = by_message.iter().collect();    ranked.sort_by(|one, other| other.1.cmp(one.1));
    // **The total is printed, not just the top of the list**: the list is truncated, so "add up what you see" is
    // not the number — and every number in `docs/` that came from this probe is a total. (Found the hard way:
    // 15 lines summed to 920 while the file really had 941 messages, because the tail beyond the top 15 is real.)
    let messages: usize = by_message.values().sum();
    println!(
        "\n--- messages: {messages} in total over {} kinds, the 15 most common first \
         (a count is not a defect count: these cascade) ---",
        by_message.len()
    );
    for (message, count) in ranked.iter().take(15) {
        println!("{count:6}  {message}");
    }

    // How much of the tail is "one construct away"? A file with a single error is a file one rule from clean,
    // while a file with fifty is a file whose first error hid everything after it — and the two want different
    // work, which a list of first errors cannot say.
    let mut histogram = [0usize; 4];
    for count in counts.iter() {
        histogram[match count {
            0 => 0,
            1 => 1,
            2..=5 => 2,
            _ => 3,
        }] += 1;
    }
    println!(
        "\n--- how many errors each file has ---\n\
         clean {} | exactly one {} | two to five {} | more {}",
        histogram[0], histogram[1], histogram[2], histogram[3]
    );

    println!("\n--- the first error of every failing file, which is the one nothing above explains ---");
    for detail in &details {
        println!("{detail}");
    }
}

