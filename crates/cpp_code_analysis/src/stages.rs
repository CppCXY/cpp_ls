//! **Where the session's time went**, by stage, so that a number can be argued with.
//!
//! A total like "indexed 138 files in 26.20 s" cannot say *why*, and every guess made without a breakdown has been
//! wrong (§8's instrument lesson: the profile found in one run what three wrong hypotheses had not). This module is
//! the instrument: a fixed set of stages, each a pair of `Instant::now()` calls around the smallest region that can
//! be named, accumulated in atomics so that reading the table works from any thread — including, later, a worker
//! pool, where a thread-local would silently report one worker's share as the whole.
//!
//! # The one rule: **the stages do not overlap**
//!
//! `total` is only meaningful if every stage measures disjoint work, so the stages are chosen that way rather than
//! for convenience: the raw parse is `Parse` + `Sweep`, the *rendering's* parse is `RenderParse` + `RenderSweep` +
//! `Map`, and nothing wraps a region that already has a timer inside it. A nested pair would double-count and the
//! table would lie in the one direction that is hard to notice — it would still add up to something.
//!
//! # Why always compiled in
//!
//! Two `Instant::now()` calls per stage: the stages here run **once per file**, not once per token, so the whole
//! instrument is a few microseconds against a per-file cost measured in milliseconds. A probe that has to be built
//! with a feature flag is a probe nobody runs when it matters.
//!
//! # What it is not
//!
//! Not a counter of work (`crate::index::store::StoreStats` is that), and not tracing per file: this answers "which
//! stage" before "which file", because the second question is only worth asking once the first has an answer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// One named region of the work.
///
/// The list is closed on purpose: an `enum` makes "what stages exist" a thing a reader can see, and makes the table
/// a fixed shape that two runs can be diffed as. Adding a stage is a deliberate act — see the module's rule about
/// overlap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Reading a file's text through the provider chain (a hit in the L1 content cache is a memory copy).
    Read,
    /// Hashing the text, for the summary's key and for the translation unit cache's validity check.
    Hash,
    /// Looking a summary up on disk: the entry read, the key check, and the include re-check.
    Lookup,
    /// Scanning a file's `#include` lines — lex, directive scan, resolution — **before** its parse, so that the files
    /// it names can be read in parallel with it (`SummaryStore::prepare_closure`).
    IncludeScan,
    /// Parsing a file's **own text** into the real syntax tree.
    Parse,
    /// Walking that tree to the file's facts (scope walk and declaration sweep) and building the summary.
    Sweep,
    /// Encoding a summary and writing it to the cache.
    Encode,
    /// Lexing a file's text into a token stream.
    Lex,
    /// Building the file's macro table out of the unit's environment (`FileMacros`).
    Macros,
    /// Expanding and splicing the file's tokens into a rendering (`cook_with(..).render()`).
    Render,
    /// Parsing a **rendering**: the program a compiler sees rather than the text a file says.
    RenderParse,
    /// Walking the rendering's tree to facts, before they are mapped back into files.
    RenderSweep,
    /// Turning every range in a rendering's summary back into a position in the file (`map_into_the_file`), and
    /// placing the rendering's errors.
    Map,
    /// Handing a cooked reading to the index.
    Insert,
    /// Walking a translation unit: the include closure, its conditionals, and its macro timeline.
    Walk,
    /// Reading a unit's closure **with its text** — what the walk is handed when it has to be built.
    Closure,
    /// Reading a unit out of the on-disk cache, closure check included.
    UnitGet,
    /// Writing a unit to the on-disk cache.
    UnitPut,
    /// Deciding **which macro bodies a reading needs**, over every file's macro facts
    /// (`SummaryStore::re_read_what_a_body_changes`'s first loop).
    ///
    /// A stage of its own because the region it lives in is not: the pass continues into a re-read that *parses*
    /// files, and a timer around the whole pass would count those parses twice — once here and once as `Parse` +
    /// `Sweep`. Measured on the 138-file project, the enclosing version reported 106% of the wall clock, which is
    /// how the overlap was found.
    BodiedScan,
    /// *Detail*: the directive and condition scan of one file (`preprocess`), inside `Sweep`.
    Scan,
    /// *Detail*: the scope tree of one file (`build_scopes`), inside `Sweep`.
    Scopes,
    /// *Detail*: the declaration facts of one file (`build_facts`), inside `Sweep`.
    Facts,
    /// *Detail*: resolving one file's `#include`s against the search path, and reading its `#define`s off its
    /// directives, inside `Sweep`.
    Includes,
    /// *Detail*: assigning each fact the conditional region it stands in (`assign_guards`, the own-guard rule, and
    /// the de-guard step), inside `Sweep`.
    Guards,
    /// *Detail*: deciding which `#define`s settle a name whichever branch is taken
    /// (`mark_settling_macro_facts`), inside `Sweep`.
    Settling,
    /// *Detail*: reading a declaration's type spelling (`declared_type_of`), inside `Facts`.
    TypeOf,
    /// *Detail*: reading an alias's target (`declared_alias_target`), inside `Facts`.
    Alias,
    /// *Detail*: reading a function's return type (`declared_returns_of`), inside `Facts`.
    Returns,
    /// *Detail*: reading a class's bases (`declared_bases_of`), inside `Facts`.
    Bases,
    /// *Detail*: reading a class template's parameter names (`declared_template_parameters_of`), inside `Facts`.
    ///
    /// It is a stage of its own because it was the one question in `Facts` that had none, and on a **unit** read —
    /// where the tree is the whole program rather than one file — it was 8.9 s of a 9.1 s `Facts`, hidden behind
    /// four detail stages that added up to 0.2 s. A detail stage is what makes the remainder attributable.
    TemplateParameters,
    /// **Rendering a whole unit into one stream** ([`crate::TranslationUnit::cook_the_unit`]) — the lex and the
    /// macro expansion of every file the walk reached, in include order.
    ///
    /// Top-level and in the *cooking* family, beside `Render` (one file) rather than inside it: on the 138-file
    /// project this is 1.7 s where a single file's rendering is milliseconds, and a unit read that reported only
    /// `render-parse` and `render-sweep` left it out of the total altogether — 3.18 s of wall clock against 1.44 s
    /// of stages, with the difference unattributed.
    UnitRender,
    /// **Repairing a unit's stream at the braces that crossed** ([`crate::RenderedUnit::neutralized`]) and
    /// **taking one file out of it** ([`crate::RenderedUnit::only`]) for the files that have to be read on their own.
    ///
    /// The name is older than the mechanism: this used to be where a leaking file's whole token stream was removed
    /// from the program (`RenderedUnit::without`), and it is now where one brace *pair* is turned into a marker that
    /// pairs with nothing. See `FileIndexer::index_unit_rendering` for why the smaller repair is the one that holds.
    UnitFence,
    /// **Splitting what a unit's parse found by the file each fact was written in** (`file_what_was_found`), once
    /// for the program and once per file whose brace was given up.
    UnitFiles,
    /// *Detail*: **destroying a parsed tree**, inside `Sweep` — a green tree is tens of thousands of `Arc`s and
    /// dropping it is not free.
    Drop,
    /// *Detail*: asking whether a macro body's shape is one a reading uses, **without** an environment
    /// (`shape_of_a_body`), inside `BodiedScan`.
    BodiedPlain,
    /// *Detail*: reading a file's declaration shapes ([`crate::DeclarationShapes::of`]), inside `Facts` and so
    /// inside `Sweep` — one pass over the tree that every question about a declaration is then answered from.
    Shapes,
    /// *Detail*: the same question **with** an environment — building the defining file's closure environment and
    /// asking `shape_of_a_body_at`, inside `BodiedScan`.
    BodiedEnv,
    /// Taking a file into the session's VFS: reading its text and building its line index
    /// (`Session::index_one`'s `vfs.load`).
    Load,
    /// Filing a summary into the project index (`ProjectIndex::insert` / `insert_cooked`).
    IndexInsert,
    /// *Detail*: building the closure environment a re-read file is rebuilt against, in the second pass's second
    /// loop (`macros_from_the_closure_with_bodies`), inside `BodiedScan`'s pass.
    ReEnv,
    /// *Detail*: the mention filter of the second pass's second loop, inside the same pass.
    ReFilter,
    /// **Classifying a file's names for a semantic highlighter** (`semantic::classified_names`) — asked once per
    /// `textDocument/semanticTokens/full`, which is once per edit in the file being edited, and therefore on the
    /// interactive path rather than the indexing one.
    ///
    /// Detail rather than a top-level stage because it does not belong to either of the two families: it is neither
    /// what a file costs to be *known* nor what it costs to be read as a compiler reads it, it is what one
    /// *question* costs. Measured on a 3 711-identifier header it is a handful of milliseconds — see
    /// `examples/semantic_probe.rs`, which is where the number is read.
    Classify,
}

impl Stage {
    /// Is this stage measured **inside** another one?
    ///
    /// The detail stages exist because "the sweep takes 14.7 s" is not actionable and "reading declaration types
    /// takes 12 s of it" is. They are therefore excluded from [`StageTimes::total`] and from the family subtotals —
    /// the totals stay a partition, and the details are printed as a second block that says what it is inside of.
    pub fn is_detail(self) -> bool {
        matches!(
            self,
            Stage::Scan
                | Stage::Scopes
                | Stage::Facts
                | Stage::Includes
                | Stage::Guards
                | Stage::Settling
                | Stage::TypeOf
                | Stage::Alias
                | Stage::Returns
                | Stage::Bases
                | Stage::TemplateParameters
                | Stage::Drop
                | Stage::Shapes
                | Stage::BodiedPlain
                | Stage::BodiedEnv
                | Stage::ReEnv
                | Stage::ReFilter
                | Stage::Classify
        )
    }
}

/// Every stage, in the order the table prints them.
pub const STAGES: [Stage; 42] = [
    Stage::Read,
    Stage::Hash,
    Stage::Lookup,
    Stage::IncludeScan,
    Stage::Parse,
    Stage::Sweep,
    Stage::Encode,
    Stage::Lex,
    Stage::Macros,
    Stage::Render,
    Stage::RenderParse,
    Stage::RenderSweep,
    Stage::Map,
    Stage::Insert,
    Stage::Walk,
    Stage::Closure,
    Stage::UnitGet,
    Stage::UnitPut,
    Stage::BodiedScan,
    Stage::Scan,
    Stage::Scopes,
    Stage::Facts,
    Stage::Includes,
    Stage::Guards,
    Stage::Settling,
    Stage::TypeOf,
    Stage::Alias,
    Stage::Returns,
    Stage::Bases,
    Stage::TemplateParameters,
    Stage::UnitRender,
    Stage::UnitFence,
    Stage::UnitFiles,
    Stage::Drop,
    Stage::Shapes,
    Stage::BodiedPlain,
    Stage::BodiedEnv,
    Stage::Load,
    Stage::IndexInsert,
    Stage::ReEnv,
    Stage::ReFilter,
    Stage::Classify,
];

impl Stage {
    /// The column this stage prints under — stable, because the table is meant to be diffed between runs.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Read => "read",
            Stage::Hash => "hash",
            Stage::Lookup => "lookup",
            Stage::IncludeScan => "include-scan",
            Stage::Parse => "parse",
            Stage::Sweep => "sweep",
            Stage::Encode => "encode",
            Stage::Lex => "lex",
            Stage::Macros => "macros",
            Stage::Render => "render",
            Stage::RenderParse => "render-parse",
            Stage::RenderSweep => "render-sweep",
            Stage::Map => "map",
            Stage::Insert => "insert",
            Stage::Walk => "walk",
            Stage::Closure => "closure",
            Stage::UnitGet => "unit-get",
            Stage::UnitPut => "unit-put",
            Stage::BodiedScan => "bodied-scan",
            Stage::Scan => "scan",
            Stage::Scopes => "scopes",
            Stage::Facts => "facts",
            Stage::Includes => "includes",
            Stage::Guards => "guards",
            Stage::Settling => "settling",
            Stage::TypeOf => "type-of",
            Stage::Alias => "alias",
            Stage::Returns => "returns",
            Stage::Bases => "bases",
            Stage::TemplateParameters => "template-params",
            Stage::UnitRender => "unit-render",
            Stage::UnitFence => "unit-fence",
            Stage::UnitFiles => "unit-files",
            Stage::Drop => "drop",
            Stage::Shapes => "shapes",
            Stage::BodiedPlain => "bodied-plain",
            Stage::BodiedEnv => "bodied-env",
            Stage::Load => "load",
            Stage::IndexInsert => "index-insert",
            Stage::ReEnv => "re-env",
            Stage::ReFilter => "re-filter",
            Stage::Classify => "classify",
        }
    }

    /// Which family the stage belongs to — the two halves of the work, printed as subtotals.
    ///
    /// The split is the one the plan turns on: **indexing** is what a file costs to be *known*
    /// (read it, hash it, look it up, parse it, sweep it, store it), and **cooking** is what it costs to be read
    /// *as a compiler reads it* (lex, macros, render, parse the rendering, map it back). A project pays the first for
    /// every file and the second only for the files somebody looks at, and a single total hides which one grew.
    pub fn family(self) -> Family {
        match self {
            Stage::Read
            | Stage::Hash
            | Stage::Lookup
            | Stage::IncludeScan
            | Stage::Parse
            | Stage::Sweep
            | Stage::Encode
            | Stage::Load
            | Stage::IndexInsert
            | Stage::BodiedScan
            | Stage::ReEnv
            | Stage::ReFilter
            | Stage::Scan
            | Stage::Scopes
            | Stage::Facts
            | Stage::Includes
            | Stage::Guards
            | Stage::Settling
            | Stage::TypeOf
            | Stage::Alias
            | Stage::Returns
            | Stage::Bases
            | Stage::TemplateParameters
            | Stage::Drop
            | Stage::Shapes
            | Stage::BodiedPlain
            | Stage::BodiedEnv
            // Detail-only, so the family is what it is *not* counted in rather than where it belongs: this stage
            // is measured inside neither of the two halves — see its own documentation. `Indexing` is the closer
            // of the two, because what it spends its time on is asking the index.
            | Stage::Classify => Family::Indexing,
            Stage::Lex
            | Stage::Macros
            | Stage::Render
            | Stage::RenderParse
            | Stage::RenderSweep
            | Stage::Map
            | Stage::Insert => Family::Cooking,
            Stage::Walk
            | Stage::Closure
            | Stage::UnitGet
            | Stage::UnitPut
            | Stage::UnitRender
            | Stage::UnitFence
            | Stage::UnitFiles => Family::Units,
        }
    }
}

/// The three families the stages fall into; see [`Stage::family`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Indexing,
    Cooking,
    Units,
}

impl Family {
    pub fn name(self) -> &'static str {
        match self {
            Family::Indexing => "indexing",
            Family::Cooking => "cooking",
            Family::Units => "units",
        }
    }
}

/// Nanoseconds per stage, in [`STAGES`] order.
static NANOS: [AtomicU64; STAGES.len()] = [const { AtomicU64::new(0) }; STAGES.len()];

/// **How many times each stage was entered.** Pairs with [`NANOS`]: one number says what a stage cost in total and
/// this says how it was spent, and a stage whose total exceeds its enclosing family's is only explicable with it.
static ENTRIES: [AtomicU64; STAGES.len()] = [const { AtomicU64::new(0) }; STAGES.len()];

/// **One check's own cost**, so that "diagnostics are slow" can be attributed to a check rather than assumed.
///
/// The five checks run together in `Checks::run`, and the one that cost 302 ms per file was found only because it
/// was counted *on its own* (`check/mod.rs:156-173`): a total over the layer says nothing about which check is the
/// price. Separate from [`StageTimes`] because that table's rule is that its stages **do not overlap**, and these
/// run inside one call rather than beside each other.
pub mod check_trace {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    /// The checks, in the order `Checks::run` calls them.
    pub const NAMES: [&str; 5] = [
        "an_include_is_found",
        "a_macro_is_not_redefined",
        "an_error_the_file_asks_for",
        "an_initializer_does_not_convert",
        "an_argument_does_not_convert",
    ];

    static NANOS: [AtomicU64; 5] = [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ];
    static CALLS: [AtomicU64; 5] = [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ];

    /// A check, timed until it is dropped. `None` when nobody is looking.
    pub struct Timing(Option<(usize, Instant)>);

    impl Drop for Timing {
        fn drop(&mut self) {
            if let Some((which, started)) = self.0 {
                NANOS[which].fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
                CALLS[which].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Time one check by its index in [`NAMES`] — `let _t = check_trace::timing(3);`.
    pub fn timing(which: usize) -> Timing {
        Timing(on().then(|| (which, Instant::now())))
    }

    fn on() -> bool {
        std::env::var_os("CPPLS_TRACE_DIAGNOSTICS").is_some()
    }

    pub fn reset() {
        for counter in NANOS.iter().chain(CALLS.iter()) {
            counter.store(0, Ordering::Relaxed);
        }
    }

    /// One line per check that ran, so that a caller can print the attribution.
    pub fn lines() -> Vec<String> {
        NAMES
            .iter()
            .enumerate()
            .filter_map(|(at, name)| {
                let calls = CALLS[at].load(Ordering::Relaxed);
                (calls > 0).then(|| {
                    format!(
                        "{name:<32} {:>7.2} ms over {calls} call(s)",
                        NANOS[at].load(Ordering::Relaxed) as f64 / 1_000_000.0
                    )
                })
            })
            .collect()
    }
}

/// A running timer for one stage: `let _timer = StageTimer::new(Stage::Parse);`
///
/// RAII rather than a start/stop pair, because the region that returns early is exactly the region that would
/// otherwise be measured as free.
pub struct StageTimer(Stage, Instant);

impl StageTimer {
    pub fn new(stage: Stage) -> StageTimer {
        ENTRIES[stage as usize].fetch_add(1, Ordering::Relaxed);
        StageTimer(stage, Instant::now())
    }

    /// Stop early, recording what has passed so far. The `Drop` that follows records nothing more.
    pub fn stop(self) {}
}

impl Drop for StageTimer {
    fn drop(&mut self) {
        let passed = self.1.elapsed().as_nanos() as u64;
        NANOS[self.0 as usize].fetch_add(passed, Ordering::Relaxed);
    }
}

/// What every stage has cost so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageTimes {
    /// Nanoseconds per stage, in [`STAGES`] order.
    nanos: [u64; STAGES.len()],
}

impl Default for StageTimes {
    /// Written out rather than derived: the standard library's `Default` for arrays stops at 32 elements, and this
    /// table is longer than that — a derive that stops compiling when a stage is added is worse than four lines.
    fn default() -> Self {
        StageTimes {
            nanos: [0; STAGES.len()],
        }
    }
}

impl StageTimes {
    /// Read the counters. Cheap, and safe from any thread.
    pub fn read() -> StageTimes {
        let mut nanos = [0u64; STAGES.len()];
        for (index, counter) in NANOS.iter().enumerate() {
            nanos[index] = counter.load(Ordering::Relaxed);
        }
        StageTimes { nanos }
    }

    /// Forget everything measured so far — a probe that wants "this call" rather than "this session".
    pub fn reset() {
        for counter in NANOS.iter() {
            counter.store(0, Ordering::Relaxed);
        }
    }

    pub fn of(&self, stage: Stage) -> Duration {
        Duration::from_nanos(self.nanos[stage as usize])
    }

    /// What one family cost — the top-level stages only.
    pub fn family(&self, family: Family) -> Duration {
        STAGES
            .iter()
            .filter(|stage| !stage.is_detail() && stage.family() == family)
            .map(|stage| self.of(*stage))
            .sum()
    }

    /// Every **top-level** stage's cost together — the number the stages are supposed to add up to.
    ///
    /// Detail stages are left out; they are inside one of these, and adding them would count their work twice.
    ///
    /// Compare it with the wall clock the caller measured: a table that accounts for most of the wall time is an
    /// instrument, and one that accounts for a third of it is telling the caller where to look next (the missing
    /// third is then the honest answer to "what else").
    pub fn total(&self) -> Duration {
        Duration::from_nanos(
            STAGES
                .iter()
                .filter(|stage| !stage.is_detail())
                .map(|stage| self.nanos[*stage as usize])
                .sum(),
        )
    }

    /// Every detail stage's cost together — what the detailed block adds up to.
    ///
    /// Read it against the top-level stage it is inside of rather than against the total: a detail block that is
    /// smaller than its parent means the parent has unaccounted work, which is itself an answer.
    pub fn detail_total(&self) -> Duration {
        Duration::from_nanos(
            STAGES
                .iter()
                .filter(|stage| stage.is_detail())
                .map(|stage| self.nanos[*stage as usize])
                .sum(),
        )
    }

    /// What happened between two readings, stage by stage.
    pub fn since(self, earlier: StageTimes) -> StageTimes {
        let mut nanos = [0u64; STAGES.len()];
        for (index, value) in nanos.iter_mut().enumerate() {
            *value = self.nanos[index].saturating_sub(earlier.nanos[index]);
        }
        StageTimes { nanos }
    }

    pub fn is_empty(&self) -> bool {
        self.nanos.iter().all(|nanos| *nanos == 0)
    }

    /// The table, one line per stage that ran, with the three family subtotals and a detail block.
    ///
    /// Stages that cost nothing are left out: on a run that parsed nothing from disk, a row of zeros is noise, and a
    /// reader looking for the stage that dominates should see the ones that exist.
    /// How many times a stage was entered, for the same run.
    pub fn entries(stage: Stage) -> u64 {
        ENTRIES[stage as usize].load(Ordering::Relaxed)
    }

    pub fn report(&self) -> String {
        let total = self.total().as_secs_f64() * 1000.0;
        let mut out = String::new();
        out.push_str(&format!(
            "{:>14}  {:>10}  {:>7}\n",
            "stage", "ms", "% of stages"
        ));

        for family in [Family::Indexing, Family::Cooking, Family::Units] {
            let family_total = self.family(family);
            if family_total.is_zero() {
                continue;
            }
            out.push_str(&format!(
                "{:>14}  {:>10.1}  {:>6.1}%\n",
                format!("[{}]", family.name()),
                family_total.as_secs_f64() * 1000.0,
                100.0 * family_total.as_secs_f64() * 1000.0 / total.max(f64::MIN_POSITIVE)
            ));
            for stage in STAGES
                .iter()
                .filter(|stage| !stage.is_detail() && stage.family() == family)
            {
                let cost = self.of(*stage);
                if cost.is_zero() {
                    continue;
                }
                out.push_str(&format!(
                    "{:>14}  {:>10.1}  {:>6.1}%\n",
                    stage.name(),
                    cost.as_secs_f64() * 1000.0,
                    100.0 * cost.as_secs_f64() * 1000.0 / total.max(f64::MIN_POSITIVE)
                ));
            }
        }

        let detail = self.detail_total();
        if !detail.is_zero() {
            out.push_str(&format!(
                "{:>14}  {:>10.1}   (inside the stages above, not added)\n",
                "[detail]",
                detail.as_secs_f64() * 1000.0
            ));
            for stage in STAGES.iter().filter(|stage| stage.is_detail()) {
                let cost = self.of(*stage);
                if cost.is_zero() {
                    continue;
                }
                out.push_str(&format!(
                    "{:>14}  {:>10.1}\n",
                    stage.name(),
                    cost.as_secs_f64() * 1000.0
                ));
            }
        }

        out.push_str(&format!("{:>14}  {:>10.1}\n", "stages total", total));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Time is recorded against the stage that was running, and read back from another thread — which is the
    /// property a worker pool will need and a thread-local would not have.
    ///
    /// What is deliberately **not** asserted is that some other stage recorded nothing: these counters are
    /// process-global on purpose, the test binary runs its tests in parallel, and "no other thread wrote" is a
    /// property of the test runner rather than of the instrument. (It was asserted once, and it failed exactly
    /// that way — the neighbouring test records `Parse`.)
    #[test]
    fn a_timer_records_against_its_own_stage_from_any_thread() {
        let before = StageTimes::read();
        {
            let _timer = StageTimer::new(Stage::Walk);
            std::thread::sleep(Duration::from_millis(2));
        }
        let delta = StageTimes::read().since(before);

        assert!(
            delta.of(Stage::Walk) >= Duration::from_millis(1),
            "the walk stage was running: {:?}",
            delta.of(Stage::Walk)
        );

        let after = StageTimes::read();
        std::thread::spawn(move || {
            let _timer = StageTimer::new(Stage::Encode);
            std::thread::sleep(Duration::from_millis(2));
        })
        .join()
        .expect("the thread finishes");

        let delta = StageTimes::read().since(after);
        assert!(
            delta.of(Stage::Encode) >= Duration::from_millis(1),
            "a worker's time is in the same table"
        );
    }

    /// The three families partition the stages: the subtotals add up to the total, and no family is empty.
    ///
    /// "Add up" is the property the table is read for — a family list that missed a stage would silently make the
    /// three subtotals disagree with the total, which is the kind of wrong number a reader would believe.
    #[test]
    fn the_families_partition_the_stages() {
        let before = StageTimes::read();
        {
            let _timer = StageTimer::new(Stage::Parse); // indexing
        }
        {
            let _timer = StageTimer::new(Stage::Render); // cooking
        }
        {
            let _timer = StageTimer::new(Stage::Walk); // units
        }
        let delta = StageTimes::read().since(before);

        for family in [Family::Indexing, Family::Cooking, Family::Units] {
            assert!(
                STAGES.iter().any(|stage| stage.family() == family),
                "{} has stages",
                family.name()
            );
        }

        let subtotals = delta.family(Family::Indexing)
            + delta.family(Family::Cooking)
            + delta.family(Family::Units);
        assert_eq!(
            subtotals, delta.total(),
            "the three subtotals are the total, stage for stage"
        );
    }

    /// A stage that ran is printed, under its family and in the detail block — the table is what a reader looks at.
    ///
    /// Only positive claims: the counters are process-global and the test binary runs its tests in parallel, so
    /// "this stage is absent" is a claim about the other tests rather than about the report. (Both negative
    /// assertions this test used to make failed exactly that way — a neighbouring test cooks a rendering.)
    ///
    /// Each timer is given something to measure. Two `Instant::now()` calls back to back can round to **zero
    /// nanoseconds**, and a stage that cost nothing prints no row — which is the report's intended behaviour and
    /// would make this test flaky in exactly the way it first was.
    #[test]
    fn the_report_names_the_stages_that_ran() {
        let before = StageTimes::read();
        {
            let _timer = StageTimer::new(Stage::Lex);
            std::thread::sleep(Duration::from_millis(1));
        }
        {
            let _timer = StageTimer::new(Stage::TypeOf);
            std::thread::sleep(Duration::from_millis(1));
        }
        let delta = StageTimes::read().since(before);
        let report = delta.report();

        assert!(report.contains("lex"), "{report}");
        assert!(report.contains("type-of"), "a detail stage is a row:\n{report}");
        assert!(report.contains("[cooking]"), "{report}");
        assert!(report.contains("[detail]"), "{report}");
    }
}



