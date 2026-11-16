//! **Walking includes for macros** — the environment a macro name is expanded in.
//!
//! Moved here from `project/mod.rs` without a change. A macro's meaning is the `#define` in force where it is used,
//! and "in force" is a property of a *walk* over the include graph: the definitions met on the way, the guards that
//! decided which branch was taken, and the order the files were included in.

use super::*;

/// **What one file contributes to a visibility walk**, as a number — see [`ProjectIndex::the_walks_fingerprint`].
///
/// The walk reads three things from a file: which targets its `#include`s resolved to, what its guarded regions ask
/// (the conditions, as text — evaluated against the macros in force), and the macro facts a condition can read (the
/// name, whether it defines or undefines, its integer value, and the one name a body aliases). Everything else a
/// summary holds — the file's declarations, its bodies, its text — cannot change which files are visible.
///
/// A body edit therefore produces the same number, which is the whole point: the walk stays valid and a keystroke does
/// not re-walk a thousand headers.
pub(crate) fn the_walk_inputs_of(summary: &FileSummary) -> u64 {
    let mut bytes: Vec<u8> = Vec::new();
    for include in &summary.includes {
        bytes.extend_from_slice(include.spelling.as_bytes());
        bytes.push(0);
        match &include.resolved {
            Some(path) => bytes.extend_from_slice(path.to_string_lossy().as_bytes()),
            None => bytes.push(1),
        }
        bytes.push(0);
        bytes.push(u8::from(include.is_next));
        bytes.push(match include.guard {
            crate::FactGuard::Unconditional => 0,
            crate::FactGuard::Region(region) => 1 + (region.min(u32::from(u8::MAX) - 1) as u8),
        });
    }
    for conditional in &summary.guards.conditionals {
        for branch in &conditional.branches {
            // The branch's own kind, by its debug name: a guard branch is one of the conditional directives, and the
            // rest cannot appear here — and the number is only ever compared with itself, so the spelling is enough.
            bytes.extend_from_slice(format!("{:?}", branch.kind).as_bytes());
            bytes.push(0);
            match &branch.condition {
                Some(text) => bytes.extend_from_slice(text.as_bytes()),
                None => bytes.push(1),
            }
            bytes.push(0);
        }
    }
    for fact in &summary.macros {
        bytes.extend_from_slice(fact.name.as_bytes());
        bytes.push(0);
        bytes.push(u8::from(fact.kind.is_definition()));
        bytes.push(u8::from(fact.function_like));
        match &fact.value {
            Some(value) => bytes.extend_from_slice(value.as_bytes()),
            None => bytes.push(1),
        }
        bytes.push(0);
        match &fact.alias {
            Some(alias) => bytes.extend_from_slice(alias.as_bytes()),
            None => bytes.push(1),
        }
        bytes.push(0);
        bytes.push(match fact.guard {
            crate::FactGuard::Unconditional => 0,
            crate::FactGuard::Region(region) => 1 + (region.min(u32::from(u8::MAX) - 1) as u8),
        });
    }
    // **A module edge is a different kind of edge** and it is read by the same walk's second half — see
    // [`ProjectIndex::visible_files_with_modules`]. Debug is exact enough for a cache key, and a summary almost always
    // has nothing recorded here.
    bytes.extend_from_slice(format!("{:?}", summary.modules).as_bytes());
    crate::cache::fnv1a64(&bytes)
}

impl ProjectIndex {
    pub fn new() -> Self {
        ProjectIndex {
            // **Incomplete**, so that a name this index has not been told about is `Unknown` rather than "not
            // defined". An index built by hand has been told nothing, and the default has to be the answer that
            // claims nothing: `Marked::default()` on its own would decide every `#ifdef` in every file as false.
            macros: Marked::default().incomplete(),
            visibility_answers: std::sync::Mutex::new(HashMap::new()),
            type_answers: std::sync::Mutex::new(HashMap::new()),
            type_hits: std::sync::atomic::AtomicU64::new(0),
            type_misses: std::sync::atomic::AtomicU64::new(0),
            visibility: std::sync::Mutex::new(HashMap::new()),
            walking: std::sync::Mutex::new(std::collections::HashSet::new()),
            walks_built: std::sync::atomic::AtomicU64::new(0),
            walks_hit: std::sync::atomic::AtomicU64::new(0),
            walks_met: std::sync::atomic::AtomicU64::new(0),
            ..ProjectIndex::default()
        }
    }

    /// **One cold walk per root at a time** — see [`ProjectIndex::walking`].
    ///
    /// `true` means the caller owns the walk and must call [`ProjectIndex::a_walk_is_over`] when it is done, however
    /// it ends (a guard at the call site makes that a `Drop`). `false` means somebody else is building the same
    /// answer and this caller should wait.
    pub(crate) fn claim_a_walk(&self, root: &str) -> bool {
        match self.walking.lock() {
            Ok(mut held) => held.insert(root.to_string()),
            // A poisoned lock is a panicking walker. Walking without the claim is the honest reading: the work is
            // the answer, and a cache that turns one thread's panic into another thread's panic is worse.
            Err(_) => true,
        }
    }

    /// Give up a claim and wake everyone waiting for it.
    pub(crate) fn a_walk_is_over(&self, root: &str) {
        if let Ok(mut held) = self.walking.lock() {
            held.remove(root);
            self.walk_done.notify_all();
        }
    }

    /// **Wait until nobody is walking `root`** — the other half of [`ProjectIndex::claim_a_walk`].
    ///
    /// A timed wait in a loop rather than a plain `wait`: a notification that arrives between the caller's failed
    /// claim and this call would otherwise be lost, and the caller would wait for a walk that has already finished.
    /// The timeout costs one wakeup per 50 ms and makes a lost notification a delay instead of a hang.
    pub(crate) fn wait_for_the_walk(&self, root: &str) {
        let Ok(mut held) = self.walking.lock() else {
            return;
        };
        while held.contains(root) {
            match self
                .walk_done
                .wait_timeout(held, std::time::Duration::from_millis(50))
            {
                Ok((next, _)) => held = next,
                // Poisoned: the walker panicked and its guard still released the claim, so there is nothing left to
                // wait for.
                Err(_) => return,
            }
        }
    }

    /// **How many visibility walks this index has built, answered from its memo, and found already in flight.**
    ///
    /// The three numbers the review asked for, in the order they matter: a walk is seconds on a cold closure, a
    /// memo hit is a fingerprint, and a "met" that is not zero is work the single-flight saved.
    pub fn walk_counts(&self) -> (u64, u64, u64) {
        (
            self.walks_built.load(std::sync::atomic::Ordering::Relaxed),
            self.walks_hit.load(std::sync::atomic::Ordering::Relaxed),
            self.walks_met.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Tell the index what the **compilation** defines: the compiler's predefined names, then the command line's
    /// `-D`s.
    ///
    /// One environment for the whole index, which is an approximation the caller should know about: a project
    /// whose files are built with different `-D`s has one environment per *target*, and this models the one the
    /// caller supplies. Recorded rather than guessed at here.
    ///
    /// # Complete or not, and who decides
    ///
    /// The state's own [`Marked::incomplete`] flag is **kept as given**, and that flag is the difference between
    /// two answers to `#ifdef NAME` when nothing the index read defines `NAME`:
    ///
    /// ```text
    /// incomplete  → Unknown  — a file nobody read, or a `-D` nobody mentioned, could define it
    /// complete    → false    — within this compilation, nothing defines it, and the branch is not taken
    /// ```
    ///
    /// The second is what makes `#ifndef NT_INCLUDED / #include <winnt.h> / #endif` decide itself, and it is a
    /// claim about the **inputs**: the caller is saying "these are the compilation's own definitions, and every
    /// `#include` that matters resolved and was indexed". [`crate::Session`] says it when the compiler it discovered
    /// **answered with its predefined macros** — the table `-dM`/`/PD` prints, and the only place the compiler's own
    /// names exist — and a walk that runs into an `#include` it cannot read takes the claim back
    /// ([`Marked::mark_incomplete`]). See `Session::assemble` for what tying the claim to a compile database instead
    /// cost (MSVC's whole standard library, measured).
    ///
    /// A name a *conditional* might define is a smaller doubt and is handled per name: the walk records it with
    /// [`Marked::mark_uncertain`], so `#ifdef` on it is `Unknown` even in a complete environment.
    pub fn with_macros(mut self, macros: Marked) -> Self {
        self.macros = macros;
        self
    }

    /// The macros a compilation starts with — see [`ProjectIndex::with_macros`].
    pub fn macros(&self) -> &Marked {
        &self.macros
    }

    pub(crate) fn invalidate_visibility_answers(&self, path: &Path, known: bool) {
        let affected = known.then(|| {
            std::iter::once(normalize(path))
                .chain(
                    self.dependents_of(path)
                        .iter()
                        .map(|dependent| normalize(dependent)),
                )
                .collect::<HashSet<_>>()
        });
        let mut answers = self
            .visibility_answers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(affected) = affected {
            answers.retain(|(file, _), _| !affected.contains(file));
        } else {
            answers.clear();
        }
    }

    /// Add or replace one file's summary.
    ///
    /// The reverse edges are updated rather than rebuilt: an edit to one file changes only its own out-edges,
    /// and rebuilding the whole map on every keystroke is the cost the per-file design exists to avoid.
    pub fn insert(&mut self, summary: FileSummary) {
        let path = summary.path.clone();
        self.insert_at(&path, summary);
    }

    /// [`ProjectIndex::insert`] for a summary that was **read from the cache**.
    ///
    /// `path` is the file it is being filed under, and it has to be passed rather than taken from the summary
    /// because the two can legitimately differ: a cache entry is keyed on a file's *contents* and the directory it
    /// was compiled in, so two files with identical text side by side share an entry, and the entry's own `path`
    /// records whichever of them was written first. Filing the second under the first's name makes every fact in
    /// it point at the wrong file, and a definition jump into a file the user never mentioned.
    ///
    /// The facts themselves are right either way: a summary's contents are a function of the text *and its
    /// directory* — which is exactly what the key names, and the reason the directory is part of it. Two files
    /// whose keys match have the same declarations, at the same offsets, with the same ranges, *and* the same
    /// resolved includes.
    pub fn insert_at(&mut self, path: &Path, summary: FileSummary) {
        self.changed = self.changed.wrapping_add(1);
        let rewritten = normalize(path);
        let known_to_the_index =
            self.known_paths.contains(&rewritten) || self.summaries.contains_key(&rewritten);
        self.invalidate_visibility_answers(path, known_to_the_index);
        // **And the inference answers, for the same reason.** A summary that arrives can turn a type's `Unknown` into a
        // class, and nothing computed before it may answer after it.
        if let Ok(mut answers) = self.type_answers.lock() {
            answers.clear();
        }
        if let Ok(mut declared) = self.definition_answers.lock() {
            declared.clear();
        }
        // **The walk memo is dropped per contributor rather than whole** — see [`ProjectIndex::visibility`]. A
        // re-read of a file the index already knew can only change the walks that read it; a file it has never seen
        // can change how an `#include "x.h"` resolves for a closure that does not contain it, so that one clears the
        // map. Measured on `LuaSyntaxTree.cpp`: five walks per diagnostics pull, 33.5 ms, and every pull paid them
        // because a pull inserts before it checks.
        // **"Known" means "the index has filed this path at some point", not "it holds a summary right now".** An
        // edit is `forget` followed by a re-read, so the summary is absent at the moment the re-read inserts it —
        // testing the summaries map made every keystroke look like a brand-new file and cleared the whole walk memo
        // (measured: one keystroke left two walks where the targeted rule should leave one, and the survival this
        // change is for never happened). `known_paths` outlives a forget for exactly this reason.
        self.known_paths.insert(rewritten.clone());
        // **The walks are not dropped here, and no longer need to be**: each one validates the inputs it was made of
        // when it is asked (see [`ProjectIndex::the_walks_fingerprint`]), so an insert that changes nothing they read —
        // a body edit's summary — keeps them, and one that does change them makes the walks that read the file miss.
        // …and every member list this file contributed to, which is the narrow invalidation: a class whose members
        // live in another file keeps its answer. See [ProjectIndex::member_lists].
        self.forget_member_lists(path);
        self.forget_guard_answers(path);
        // …and every macro walk this file was part of, by the rule the visibility memo above states.
        self.forget_macro_walks(path, known_to_the_index);
        let mut summary = summary;
        summary.path = path.to_path_buf();

        let path = normalize(path);

        // Remove the edges the previous version of this file contributed, so that a deleted `#include` stops
        // making its target reachable. Without this, an edge would outlive the line that wrote it.
        let sequence = if let Some(previous) = self.summaries.get(&path) {
            let sequence = self.sequence[&path];

            for target in include_targets(previous) {
                if let Some(includers) = self.included_by.get_mut(&target) {
                    includers.remove(&path);
                }
            }

            // The names the previous text declared and the macros it defined stop being findable under this file.
            self.names
                .remove_declarations(sequence, 0, &previous.declarations);
            self.names.remove_macros(sequence, &previous.macros);

            // **The cooked reading describes the file *under the key it was built with***, so a summary filed under
            // a different key takes it with it. Both halves of the key are the reason: a different content hash means
            // the text changed, and a different context hash means the *compilation* did — the configuration or the
            // directory a relative include resolves against — and a rendering is a function of both.
            //
            // `forget` already drops the summary and the reading together, and that is the ordinary path (an edit, a
            // watched file, a close). This one covers the paths that have no `forget` in them: a re-read that found
            // different bytes, a cache entry replaced by a fresh index, and the whole-project re-read a changed
            // `compile_commands.json` causes. Leaving the reading there would answer for a file nobody has any more,
            // at offsets into words that are gone — a wrong answer rather than a missing one.
            if previous.key != summary.key {
                self.forget_the_readings_of(&path);
            }

            sequence
        } else {
            // **A file keeps the number it was first given.** An edit is `forget` followed by a re-read, and a fresh
            // number for the re-read would put the file in a different place in every closure it is part of — which
            // matters because a closure's ordered file list is now cached with the closure rather than rebuilt per
            // question (see `CachedWalk::files`): the cached list is a `(sequence, visibility)` pair per file, and a
            // renumbered file would either be dropped from it (its old number gone from `order`) or read out of the
            // wrong slot. Numbering a path once makes the answer stable for the whole session.
            let sequence = match self.sequence.get(&path) {
                Some(&known) => known,
                None => {
                    let sequence = self.next_sequence;
                    self.next_sequence += 1;
                    sequence
                }
            };
            self.order.insert(sequence, path.clone());
            // **The key is normalized when it is written**, not when it is asked for. Two readers
            // arrive at this map with two spellings of the same file: the visibility walk hands
            // back the *normalized* paths it built the graph from (`e:/p/app.cpp`), and every query
            // arrives with the path a client or a summary used (`E:/p/app.cpp`). Storing only the
            // second spelling meant the first was dropped silently — the walk said the file was
            // visible, `visible_in_order_of` did not find it, and the query answered as if the file
            // had never been included.
            self.sequence.insert(path.clone(), sequence);
            self.sequence.insert(normalize(Path::new(&path)), sequence);

            // A reading filed before its summary (the contract says it should not be, but `insert_cooked` does not
            // refuse) is findable from the moment the file has a number to be found under — every reading of it,
            // under the id it was filed with.
            if let Some(readings) = self.cooked.get(&path) {
                for (id, reading) in readings.every() {
                    self.names
                        .add_declarations(sequence, id, &reading.declarations);
                }
            }

            sequence
        };

        for target in include_targets(&summary) {
            self.included_by
                .entry(target)
                .or_default()
                .insert(path.clone());
        }

        // **Which file an `import m;` reaches.** The visibility walk ([`ProjectIndex::visible_files`]) holds
        // summaries and nothing else, so this map is what turns a module *name* into the file that declares it —
        // built here, where every file's own reading arrives, and rebuilt for a file when its reading changes (an
        // implementation unit that stops being one takes its name with it).
        //
        // Only an **interface unit** is recorded: an implementation unit declares the module and is not what an
        // importer sees, so pointing `import m;` at one would attribute its declarations to a module that never
        // exported them. A file that declares no module, or declares one as an implementation, records nothing.
        //
        // **A partition is recorded under its full name**, `geometry:shapes`. It is not the file `import geometry;`
        // reaches — that is the primary interface, and the rule above is about that question — but it *is* the file
        // `import :shapes;` reaches from inside the module, and a partition unit was previously absent from this map
        // altogether: `export import :shapes;` and `export import :area;` then resolved to nothing (or, worse, to a
        // module that happened to be called `shapes`), which is the reported pair of adjacent document links with one
        // underline and not the other. The key is the full name, so the two questions stay apart.

        self.module_interfaces
            .retain(|_, declaring| declaring != &path);
        if summary.modules.is_interface
            && let Some(module) = &summary.modules.module
        {
            let full = match &summary.modules.partition {
                Some(partition) => format!("{module}:{partition}"),
                None => module.to_string(),
            };
            self.module_interfaces.insert(full, path.clone());
        }

        self.names
            .add_declarations(sequence, 0, &summary.declarations);
        self.names.add_macros(sequence, &summary.macros);
        // **What a walk reads from this file**, recorded as it is filed: the includes it resolved, the conditions its
        // guards ask, and the macro facts a condition can read — and nothing else. A body edit changes none of them,
        // which is what lets the walk that read this file keep answering. See
        // [`ProjectIndex::the_walks_fingerprint`].
        self.walk_inputs
            .insert(path.clone(), the_walk_inputs_of(&summary));
        self.summaries.insert(path, summary);
    }

    /// Forget the file at `path`, and the edges its summary contributed.
    ///
    /// The one operation that makes a *deleted* file stop answering. Without it a query would go on finding
    /// declarations in a file that is no longer on disk — the summary is in memory, and nothing about it is
    /// wrong except that it describes something that is gone. The reverse edges go with it, for the same reason
    /// they are removed when a file is re-indexed: an edge outliving the line that wrote it would keep a header
    /// reachable from a file that no longer includes it.
    ///
    /// Returns whether there was anything to forget, which is what lets a watcher report "this event changed
    /// nothing" rather than counting every ignored path as work.
    pub fn forget(&mut self, path: &Path) -> bool {
        self.changed = self.changed.wrapping_add(1);
        let path = normalize(path);
        let Some(summary) = self.summaries.remove(&path) else {
            return false;
        };

        // **A file that is gone changes answers, so the memos go** — the per-condition answers and the declaration
        // answers whole, and the walks whose closure contained the file. A walk that never read it cannot answer
        // differently, and keeping those is what stopped a keystroke from paying for five closure walks: measured on
        // `LuaSyntaxTree.cpp`, 33.5 ms of a 200 ms pull. The contributor rule is also a correctness fix rather than
        // only bookkeeping — the test `name_index_agrees_with_the_scan::after_every_kind_of_edit` caught a query
        // between a forget and the next insert answering from a walk that still had the forgotten file in it.
        self.invalidate_visibility_answers(Path::new(&path), true);
        // **And the reading a walk takes from this file**, which is now what decides whether the walks that read it
        // answer: with the entry gone, every walk whose closure contained this file misses its fingerprint and is
        // rebuilt, and one that never read it answers exactly as before — the correctness rule the old contributor
        // drop implemented, without the cost of dropping entries that could not have changed.
        self.walk_inputs.remove(&path);
        // **And the inference answers, for the same reason.** A summary that arrives can turn a type's `Unknown` into a
        // class, and nothing computed before it may answer after it.
        if let Ok(mut answers) = self.type_answers.lock() {
            answers.clear();
        }
        // **The visibility walks stay** — `walk_inputs` above makes every walk that read the forgotten file a miss by
        // fingerprint, and one that never read it answers exactly as before. Dropping them here was what made a
        // keystroke in one file throw away every other file's walk.
        if let Ok(mut declared) = self.definition_answers.lock() {
            declared.clear();
        }

        // A file that is gone is a contributor that changed: every member list it was made of is dropped.
        self.forget_member_lists(Path::new(&path));
        self.forget_guard_answers(Path::new(&path));
        // **And every macro walk that read it.** A walk that never did cannot answer differently, which is what keeps
        // a header's answers alive across an edit to the `.c` file that includes it.
        self.forget_macro_walks(Path::new(&path), true);

        for target in include_targets(&summary) {
            if let Some(includers) = self.included_by.get_mut(&target) {
                includers.remove(&path);
            }
        }
        // **The cooked readings go with it, and they need the sequence to be taken back from the name index** — so
        // they are dropped before the file loses its number. A file that is no longer indexed must not keep answering
        // declaration queries out of a rendering nobody has any more: the same rule as the summary, for the same
        // reason, because an answer about a file that is gone is worse than no answer.
        self.forget_the_readings_of(&path);
        // **The number is kept**, and the reason is the cached closure order (`CachedWalk::files`): a walk holds a
        // `(sequence, visibility)` pair per file, and a re-read that renumbered the file would either be dropped from
        // that list or read out of the wrong slot until the whole closure was rebuilt. `forget` still removes
        // everything the number *finds* — the summary, the readings, the postings — so a query that reaches the path
        // through `order` finds nothing to answer with. The pair `(sequence, path)` is stable for the session.
        let sequence = self
            .sequence
            .get(&path)
            .copied()
            .or_else(|| self.sequence.get(&normalize(Path::new(&path))).copied());
        if let Some(sequence) = sequence {
            // `order` and `sequence` are **not** removed — see the comment above: a file keeps its number for the
            // session, so a cached closure order survives an edit. What it finds is gone: the summary, the readings
            // and the postings below.
            self.names
                .remove_declarations(sequence, 0, &summary.declarations);
            self.names.remove_macros(sequence, &summary.macros);
        }

        true
    }

    /// **The type of an expression, as a previous request answered it** — see [`ProjectIndex::type_answers`].
    pub(crate) fn a_remembered_type(
        &self,
        path: &Path,
        expression: &cpp_parser::CppSyntaxNode,
    ) -> Option<Known<(Type, PathBuf)>> {
        let key = the_type_key(path, expression);
        let remembered = self
            .type_answers
            .lock()
            .ok()
            .and_then(|answers| answers.get(&key).cloned());
        if remembered.is_some() {
            self.type_hits
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        } else {
            self.type_misses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        remembered
    }

    /// **How the inference memo has done**: `(answered from the memo, had to compute)`. The second number is what the
    /// memo exists to make small, and the pair is what a test or a probe can assert on.
    pub fn type_cache_stats(&self) -> (u64, u64) {
        (
            self.type_hits.load(std::sync::atomic::Ordering::Relaxed),
            self.type_misses.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// File one inference answer for the requests that follow.
    pub(crate) fn remember_a_type(
        &self,
        path: &Path,
        expression: &cpp_parser::CppSyntaxNode,
        answer: &Known<(Type, PathBuf)>,
    ) {
        let key = the_type_key(path, expression);
        if let Ok(mut answers) = self.type_answers.lock() {
            if answers.len() >= MAX_TYPE_ANSWERS {
                answers.clear();
            }
            answers.insert(key, answer.clone());
        }
    }

    pub fn len(&self) -> usize {
        self.summaries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.summaries.is_empty()
    }

    /// The summary of the file at `path`, if it has been indexed.
    ///
    /// # Why the probe counter is on this line
    ///
    /// Whether that is worth 73 call sites changing their return type depends on the **working
    /// set** — how many distinct summaries a session actually touches — and that number can only be
    /// counted here, at the one place every reader goes through. `CPPLS_TRACE_SUMMARIES=1` turns it
    /// on.
    pub fn summary(&self, path: &Path) -> Option<&FileSummary> {
        if crate::probes::summaries() {
            crate::probes::a_summary_was_asked_for(path);
        }
        self.summaries.get(&normalize(path))
    }

    /// **The file that declares module `name` to its importers**, if the project took one in.
    ///
    /// The map `import m;` is resolved through when a query asks what a file's imports brought into view — see
    /// [`ProjectIndex::visible_files_with_modules`]. `None` is the honest answer for a module whose interface unit is
    /// not part of this project (`import std;`, a prebuilt BMI), and a caller that wants to do something about that
    /// reads the file in itself: [`crate::Session::read_the_modules_a_file_imports`].
    pub fn interface_unit_of(&self, name: &str) -> Option<&str> {
        self.module_interfaces.get(name).map(String::as_str)
    }

    /// **Record the files a module's unit was rendered from** — see [`ProjectIndex::unit_closures`].
    ///
    /// Called by the session when a unit's rendering is in hand: it is the only layer that has the rendering, and the
    /// rendering's `walked` list is what an importer's view of the module is made of.
    pub fn remember_the_unit_closure(&self, module: &str, mut files: Vec<String>) {
        files.sort();
        files.dedup();
        if let Ok(mut closures) = self.unit_closures.lock() {
            closures.insert(module.to_string(), files);
        }
        // **And every remembered answer about visibility goes.** The memos were built before this module had a
        // closure — a walk that ran during the index pass, when the unit had not been read yet — and they are exactly
        // the question this changes. Measured: without this line the closure was recorded (223 files, confirmed) and
        // the cached walk still answered 151.
        if let Ok(mut walks) = self.visibility.lock() {
            walks.clear();
        }
    }

    /// The files [`ProjectIndex::remember_the_unit_closure`] recorded, for a probe or a test.
    pub fn unit_closure_of(&self, module: &str) -> Option<Vec<String>> {
        self.unit_closures
            .lock()
            .ok()
            .and_then(|closures| closures.get(module).cloned())
    }

    /// **Every module the index can point at a file for** — `(name, interface unit)`, in name order.
    ///
    /// Sorted rather than left in the map's order, because this is a completion list: two identical requests must
    /// answer with the same order, and a [`std::collections::HashMap`] iteration order is not a promise.
    pub fn interface_units(&self) -> Vec<(String, String)> {
        let mut all: Vec<(String, String)> = self
            .module_interfaces
            .iter()
            .map(|(name, unit)| (name.clone(), unit.clone()))
            .collect();
        all.sort();
        all
    }

    /// Every summary, in insertion order.
    pub fn summaries(&self) -> impl Iterator<Item = &FileSummary> {
        self.order
            .values()
            .filter_map(|path| self.summaries.get(path))
    }

    /// The files that include `path`, directly.
    pub fn includers_of(&self, path: &Path) -> Vec<PathBuf> {
        self.included_by
            .get(&normalize(path))
            .map(|includers| includers.iter().map(PathBuf::from).collect())
            .unwrap_or_default()
    }

    /// **The files that import the module `path` declares** — the module edge's reverse.
    ///
    /// The include graph keeps `included_by`; a module edge cannot have the same map, because an `import m;` names
    /// a *module* and a module may have several units, so the answer is "every file whose `ModuleReading` imports
    /// this name". The scan is over the summaries — the same place `module_interfaces` is derived from — and it is
    /// one user-initiated query (find references, the code lens beside it), not a per-keystroke one.
    ///
    /// Without this a declaration in a module interface unit answered **0 references**: the candidate rule in
    /// [`symbol_references`](crate::index::references::symbol_references) walked the reverse *include* edges, and a
    /// `.cpp` that says `import math;` includes nothing. Measured on the `cpp_modules` fixture: `Point::length_squared`
    /// used twice in `main.cpp` and reported nowhere.
    pub fn importers_of_the_module_of(&self, path: &Path) -> Vec<PathBuf> {
        let Some(summary) = self.summary(path) else {
            return Vec::new();
        };
        let Some(module) = summary.modules.module.as_deref() else {
            return Vec::new();
        };
        self.summaries()
            .filter(|other| other.modules.imports.iter().any(|name| &**name == module))
            .map(|other| other.path.clone())
            .collect()
    }

    /// **Add what a file's cooked reading found** — see the `cooked` field for what it is and why only declarations
    /// and diagnostics.
    ///
    /// The file must already be in the index ([`ProjectIndex::insert`]): a cooked reading is a *second* answer
    /// about a file the index knows, and its visibility is the file's own — working that out again per query would
    /// be a second include-graph walk for an answer already computed.
    ///
    /// Inserting **replaces** what was there: a file that was cooked again (its text changed, or its environment
    /// did) must not keep the old reading's declarations beside the new one's.
    pub fn insert_cooked(&mut self, path: &Path, reading: CookedFile) {
        self.insert_a_reading(path, None, reading);
    }

    /// **Add the reading a whole program produced for one of its files** — the same insert, filed under the program
    /// that read it.
    ///
    /// `program` is the hash of that program's rendering
    /// ([`crate::RenderedUnit::text`](crate::session::UnitReading)), which is what makes two translation units that
    /// read one header differently keep two readings of it ([`Readings`]). The session passes the one it just read;
    /// [`ProjectIndex::the_reading_for`] is what looks it up per query.
    pub fn insert_cooked_in_a_program(&mut self, path: &Path, program: u64, reading: CookedFile) {
        self.insert_a_reading(path, Some(program), reading);
    }

    pub(crate) fn insert_a_reading(
        &mut self,
        path: &Path,
        program: Option<u64>,
        reading: CookedFile,
    ) {
        let path = normalize(path);

        // **Which id this reading is filed under, and whether it changed** — decided before anything is touched,
        // because the answer is a fact about the map and the effects below are on other fields of `self`.
        let (id, changed) = {
            let readings = self.cooked.get(&path);
            let existing = match program {
                None => Some(Readings::ON_ITS_OWN),
                Some(program) => readings.and_then(|readings| readings.id_for(program)),
            };
            match existing {
                Some(id) => (
                    id,
                    readings
                        .and_then(|readings| readings.reading(id))
                        .is_none_or(|previous| previous.declarations != reading.declarations),
                ),
                // A program that has not read this file yet gets the next id.
                None => (
                    readings.map_or(2, |readings| readings.in_a_program.len() as u32 + 2),
                    true,
                ),
            }
        };

        // **A cooked reading is a second set of facts, and member lists union both — but only a *changed* one can
        // change a list.** A file is cooked again with the same text and the same environment on every pump slice
        // (the edited file, and the closure want_the_closure_cooked asks for), and invalidating on every insert
        // made the cross-request member cache useless: measured on LuaSyntaxTree.cpp, 15 hits against 59
        // computations over three keystrokes — and 37/37 once this comparison was in place.
        if changed {
            self.forget_member_lists(Path::new(&path));
        }
        // **What a cooked reading can change is the declaration answers, and nothing else here.** A walk reads
        // includes and their conditions — facts of the file's *own text*, which the cooked reading does not rewrite
        // — so clearing the walk memo for a cook was over-invalidation of the most expensive kind: the second pass
        // cooks ~1 200 files back to back, and every one of them threw away every closure the queries had built.
        // Measured: it was the reason a keystroke right after a cold start walked five closures again (33.5 ms).
        self.definition_answers
            .lock()
            .expect("the definition memo is not poisoned")
            .clear();

        // The name index holds this file's *other* reading of the same id, if any, and then this one.
        let tracing = std::env::var("CPPLS_TRACE_NAMES").is_ok()
            && (path.ends_with("stl_map.h") || path.ends_with("iostream"));
        if tracing {
            eprintln!(
                "[names] insert_a_reading {}: sequence {:?} id {id} declarations {} diagnostics {:?}",
                path,
                self.sequence.get(&path),
                reading.declarations.len(),
                reading
                    .diagnostics
                    .iter()
                    .take(3)
                    .map(|diagnostic| &diagnostic.message)
                    .collect::<Vec<_>>()
            );
        }
        if let Some(&sequence) = self.sequence.get(&path) {
            let previous = self
                .cooked
                .get(&path)
                .and_then(|readings| readings.reading(id))
                .map(|previous| &previous.declarations);
            if let Some(previous) = previous {
                self.names.remove_declarations(sequence, id, previous);
            }
            self.names
                .add_declarations(sequence, id, &reading.declarations);
        }

        // **Filed, in place of whatever that program (or the file itself) read before.**
        if std::env::var_os("CPPLS_TRACE_ALLOCS").is_some() {
            eprintln!(
                "[cook] inserted {path}, cooked map now {}",
                self.cooked.len()
            );
        }
        let readings = self.cooked.entry(path.clone()).or_default();
        match program {
            None => readings.on_its_own = Some(reading),
            Some(program) => {
                // A program's id is `2 + its position`, so the position is the id minus two.
                match readings.id_for(program) {
                    Some(id) => readings.in_a_program[id as usize - 2] = (program, reading),
                    None => readings.in_a_program.push((program, reading)),
                }
                readings.last = Some(program);
            }
        }
        debug_assert!(
            readings.reading(id).is_some(),
            "the id an insert decided on is the id it filled"
        );

        // **Bounded**: cooking runs for the closure of every open file — a thousand headers on a project that
        // includes the standard library — and each reading is a declaration list as large as the file`s own
        // summary. The map is *sparse by design* ("a file with no entry here is one nobody cooked", so a miss falls
        // back to the file`s own reading), which is what makes evicting the oldest sound.
        self.cooked_order.push_back(path);
        while self.cooked.len() > MAX_COOKED_FILES {
            let Some(oldest) = self.cooked_order.pop_front() else {
                break;
            };
            self.forget_the_readings_of(&oldest);
        }
    }

    /// **Drop every reading of one path, and the name entries they made** — the one place that has to know a reading
    /// is a list of lists.
    pub(crate) fn forget_the_readings_of(&mut self, path: &str) {
        let Some(readings) = self.cooked.remove(path) else {
            return;
        };
        if let Some(&sequence) = self.sequence.get(path) {
            for (id, reading) in readings.every() {
                self.names
                    .remove_declarations(sequence, id, &reading.declarations);
            }
        }
    }

    /// The declarations the file at `path` was **cooked** into, when it was cooked at all.
    /// **How much the cooked readings are holding** — files and declarations, what a memory report asks.
    pub fn cooked_size(&self) -> (usize, usize) {
        let declarations = self
            .cooked
            .values()
            .flat_map(|readings| readings.every())
            .map(|(_, reading)| reading.declarations.len())
            .sum();
        (self.cooked.len(), declarations)
    }

    /// **What the index is holding** — `(summaries, summary bytes, cooked files, cooked bytes)`.
    ///
    /// The per-entry estimate is [`FileSummary::heap_bytes`], which counts the facts and the strings they own; a
    /// map's spare capacity is not counted, so this is a lower bound on what the index costs. See
    /// [`crate::MemoryReport`] for why an estimate with a stated method beats a total with none.
    pub fn accounting(&self) -> (usize, usize, usize, usize) {
        let summary_bytes = self
            .summaries
            .values()
            .map(FileSummary::heap_bytes)
            .sum::<usize>();
        let mut cooked_bytes = 0usize;
        for readings in self.cooked.values() {
            for (_, reading) in readings.every() {
                cooked_bytes += std::mem::size_of::<CookedFile>()
                    + reading
                        .declarations
                        .iter()
                        .map(DeclFact::heap_bytes)
                        .sum::<usize>()
                    + reading
                        .macros
                        .iter()
                        .map(MacroFact::heap_bytes)
                        .sum::<usize>()
                    + reading.diagnostics.len() * std::mem::size_of::<crate::CookedDiagnostic>();
            }
        }
        (
            self.summaries.len(),
            summary_bytes,
            self.cooked.len(),
            cooked_bytes,
        )
    }

    /// **What the index's own tables cost**, which [`ProjectIndex::accounting`] does not answer.
    ///
    /// `accounting` counts the *facts* — the summaries and the cooked readings — and those are what the index is
    /// *about*. This counts the structure built around them, and it is not small: the inverted index holds one
    /// posting per declaration under two keys (its name and its scope), the include graph holds one set of includers
    /// per file, and four memos hold answers that outlive the request that asked for them.
    ///
    /// The gap this closes was measurable and unexplained: on a 103-source project the session's own report said
    /// **112.4 MB** while the process held **263 MB** after the index, and "the index's own tables are not in it" was
    /// one of the three candidates the audit could not rule out. See [`crate::MemoryReport`].
    ///
    /// Every number here is an estimate with the same stated method as the rest of that report: the entries' keys,
    /// the payloads they point at, and one container header each. Spare capacity and the maps' own tables are not
    /// counted, so this is a lower bound.
    pub fn table_accounting(&self) -> TableCost {
        let names = self.names.accounting();
        // **The include graph**: one set of includers per file, and the file names again as set members. A project's
        // `included_by` is where "which files does this header affect" is answered, so it grows with the edges rather
        // than with the files.
        let mut edges = 0usize;
        let mut edge_bytes = 0usize;
        for (included, includers) in &self.included_by {
            edges += includers.len();
            edge_bytes += included.len() + std::mem::size_of::<BTreeSet<String>>();
            edge_bytes += includers
                .iter()
                .map(|includer| includer.len() + std::mem::size_of::<String>())
                .sum::<usize>();
        }
        edge_bytes += self
            .included_by
            .keys()
            .map(|key| key.len() + std::mem::size_of::<String>())
            .sum::<usize>();
        let modules: usize = self
            .module_interfaces
            .iter()
            .map(|(name, path)| name.len() + path.len() + 2 * std::mem::size_of::<String>())
            .sum();
        // **The three memos that outlive a request.** `member_lists` is the big one — a list is hundreds of
        // `ProjectMember`s, each carrying a `DeclFact` — and it is bounded at `MAX_CACHED_MEMBER_LISTS`.
        let (member_list_entries, member_lists, member_list_bytes) = self
            .member_lists
            .lock()
            .map(|lists| {
                let members: usize = lists.values().map(|held| held.list.members.len()).sum();
                let bytes: usize = lists
                    .values()
                    .map(|held| {
                        let members: usize = held
                            .list
                            .members
                            .iter()
                            .map(|member| {
                                std::mem::size_of::<crate::ProjectMember>()
                                    + DeclFact::heap_bytes(&member.fact)
                            })
                            .sum();
                        let unlisted =
                            held.list.unlisted.len() * std::mem::size_of::<crate::UnlistedBase>();
                        members
                            + unlisted
                            + held.contributors.len() * std::mem::size_of::<PathBuf>()
                            + held
                                .contributors
                                .iter()
                                .map(|path| path.to_string_lossy().len())
                                .sum::<usize>()
                    })
                    .sum();
                (lists.len(), members, bytes)
            })
            .unwrap_or_default();
        let (guard_entries, guard_bytes) = self
            .guard_answers
            .lock()
            .map(|answers| {
                (
                    answers.len(),
                    answers.len()
                        * (std::mem::size_of::<(PathBuf, usize, u32)>()
                            + std::mem::size_of::<crate::Visibility>()),
                )
            })
            .unwrap_or_default();
        let (visibility_entries, visibility_bytes) = self
            .visibility
            .lock()
            .map(|walks| {
                let bytes: usize = walks
                    .values()
                    .map(|walk| {
                        // A walk's answer is one entry per file it reached, plus that file's name twice.
                        let reached = walk.seen.visibility.len();
                        let names: usize = walk
                            .seen
                            .visibility
                            .keys()
                            .map(|path| path.len() * 2 + 48)
                            .sum();
                        names
                            + reached * std::mem::size_of::<IncludeVisibility>()
                            + walk.contributors.len() * std::mem::size_of::<String>()
                    })
                    .sum();
                (walks.len(), bytes)
            })
            .unwrap_or_default();
        let (condition_entries, condition_bytes) = self
            .visibility_answers
            .lock()
            .map(|answers| {
                (
                    answers.len(),
                    answers.len()
                        * (std::mem::size_of::<(String, u32)>()
                            + std::mem::size_of::<crate::Visibility>()
                            + 24),
                )
            })
            .unwrap_or_default();
        // **The paths, twice**: `order` for iteration, `sequence` for identity. Both hold the file's own spelling.
        let path_bytes: usize = self
            .order
            .values()
            .map(|path| path.len() + std::mem::size_of::<String>())
            .sum::<usize>()
            + self
                .sequence
                .keys()
                .map(|path| path.len() + std::mem::size_of::<String>())
                .sum::<usize>();

        TableCost {
            names,
            edges,
            edge_bytes: edge_bytes + modules,
            member_list_entries,
            member_lists,
            member_list_bytes,
            guard_entries,
            guard_bytes,
            visibility_entries,
            visibility_bytes,
            condition_entries,
            condition_bytes,
            path_bytes,
        }
    }

    pub fn cooked_declarations(&self, path: &Path) -> Option<&[DeclFact]> {
        Some(self.cooked_reading(path)?.declarations.as_slice())
    }

    /// **The whole cooked reading** of the file at `path`: what a compiler sees declared there, what the parse of
    /// that text reported, and how much of it this file cannot show — see [`crate::CookedFile`].
    ///
    /// `None` when the file has no cooked reading, which is not the same as "nothing to say": it means *this
    /// reading* has not been made, and a caller that needs an answer falls back to the file's own text
    /// ([`crate::Session::diagnostics`] does exactly that).
    pub fn cooked_reading(&self, path: &Path) -> Option<&CookedFile> {
        self.cooked.get(&normalize(path))?.any()
    }

    /// **Which of a file's readings a lookup uses** — the file's own, or the one the asking program made of it.
    ///
    /// `asking` is the program that read the file the *question* is about
    /// ([`ProjectIndex::the_program_that_read`]), because that is the translation unit the reader is looking at:
    /// `a.cpp` that defines `_GNU_SOURCE` and `b.cpp` that does not must each see the header they compiled. A file
    /// that program has not read falls back to its own reading, and a file nobody read as a program has no cooked
    /// reading at all — the caller then uses the file's summary.
    ///
    /// Answers the reading's id, which is what a [`Posting`] carries, so the narrowing and the facts agree.
    pub(crate) fn the_reading_for(&self, file: u32, asking: Option<u64>) -> u32 {
        let Some(readings) = self.order.get(&file).and_then(|key| self.cooked.get(key)) else {
            return 0;
        };
        if let Some(id) = asking.and_then(|program| readings.id_for(program)) {
            return id;
        }
        if readings.on_its_own.is_some() {
            return Readings::ON_ITS_OWN;
        }
        // **Nothing read on its own, and not this program either**: the last program's reading, which is what a
        // caller with no environment has always got.
        readings
            .last
            .and_then(|program| readings.id_for(program))
            .unwrap_or(0)
    }

    /// **The program that read `path`**, if one did — the environment a question asked *from* that file is in.
    ///
    /// The last program to read it, because a file's own text does not say which of its includers is being compiled:
    /// a `.cpp` has exactly one program and the answer is exact, while a header that two programs read answers with
    /// the one read most recently. That is the honest limit of a lookup that is not told the translation unit, and
    /// it is written down here rather than guessed at the call sites.
    ///
    /// # The module's build comes first, and the own program second
    ///
    /// The order is the difference between `std::format` resolving and not. An importing file's own program — when it
    /// has one at all — is a reading of *its own text*, which cannot contain a single declaration the module exports:
    /// `import std;` is not an `#include`, and `main.cpp` compiles to nothing but an import-declaration. The module's
    /// translation unit is the one that read the headers, so its program is the environment every lookup from this
    /// file has to use. The other order lets the own program win whenever it exists, and every query from the
    /// importing file then falls back to raw readings — where MSVC's `_STD_BEGIN` is an unexpanded macro and nothing
    /// is in scope `std`. Measured on the modules fixture, with the own program winning: `std::format`,
    /// `std::to_string`, `std::vformat` and `std::make_format_args` all resolved to nothing from a file that imports
    /// `std`, while 2 734 names *were* in scope `std` from the headers some other program had read.
    pub fn the_program_that_read(&self, path: &Path) -> Option<u64> {
        if let Some(summary) = self.summaries.get(&normalize(path)) {
            for name in &summary.modules.imports {
                let Some(interface) = self.module_interfaces.get(name.as_ref()) else {
                    continue;
                };
                if let Some(program) = self
                    .cooked
                    .get(&normalize(Path::new(interface.as_str())))
                    .and_then(|readings| readings.last)
                {
                    return Some(program);
                }
            }
        }
        self.cooked
            .get(&normalize(path))
            .and_then(|readings| readings.last)
    }

    /// Forget what a file **was read as**, keeping its summary.
    ///
    /// The invalidation a cooked reading needs and a summary does not: a file's summary is a reading of its own text
    /// (so it changes when the text does — [`ProjectIndex::forget`]), while its cooked reading is a reading of its
    /// text **and of its environment**, so it also changes when something the file includes changes — and nothing
    /// about this file's text says so.
    ///
    /// Dropping rather than marking: between the change and the next cook, a query that found the old declarations
    /// would answer with a declaration whose range points into text that no longer describes it. Not found is the
    /// honest answer there, and the reading comes back one drain later.
    pub fn forget_cooked(&mut self, path: &Path) {
        let path = normalize(path);
        self.forget_member_lists(Path::new(&path));
        self.forget_guard_answers(Path::new(&path));
        // A cooked reading names things the raw one does not (`_STD_BEGIN` is `namespace std {` to it), so an
        // answer given while it was held can stop being true when it goes — the same rule as `forget`, see there.
        if let Ok(mut declared) = self.definition_answers.lock() {
            declared.clear();
        }
        self.forget_the_readings_of(&path);
    }

    /// **The project's symbols matching `query`**, best first — what a `workspace/symbol` search shows.
    ///
    /// # What is searched, and what is not
    ///
    /// Every declaration in every summary the index holds, **plus the names only the cooked reading declares**: the
    /// same two readings every other declaration query unions, asked here **without the visibility walk**, because a
    /// workspace search is about the project and not about what one file can see. A name a macro declared
    /// (`DECLARE_HANDLE(HWND)`'s `HWND__`) is a symbol a compiler knows, so a search finds it; a declaration both
    /// readings found is one answer, deduplicated the same way (`(qualified name, kind)` per file).
    ///
    /// **Locals are left out** ([`DeclFact::local`]): a variable inside a function body is not a project symbol, and
    /// a summary cannot even place it in the function it belongs to.
    ///
    /// # The match, and what the cap means
    ///
    /// Case-insensitive **substring of the name**, so `wid` finds `ns::Widget`. The ranking is exact name, then a
    /// prefix of the name, then anything — and within a rank, alphabetical **by name** and then by qualified name,
    /// because two runs over one project have to answer in the same order or a diff of two answers is noise.
    ///
    /// # Why the order is by name, and what that buys
    ///
    /// The names are kept sorted ([`crate::index::names`]), so an answer in name order is one the search can stop
    /// producing as soon as it has `limit` of them: the cost is the size of the answer, not the size of the project.
    /// (Ordering by the *qualified* name — which puts `a::widget` before `widget` before `b::widget` — would need
    /// every candidate's scope before the first could be placed.) Two declarations of one name are still adjacent,
    /// ordered by where they are.
    ///
    /// `limit` caps the answer, and hitting it is **not** an error: a search box is not a claim about the project
    /// the way a reference list is — the user narrows the query, and every editor's symbol search truncates. That is
    /// the opposite of the decision [`crate::macro_references`] makes, where a partial answer would be a false one.
    pub fn symbols_matching(&self, query: &str, limit: usize) -> Vec<ProjectSymbol> {
        let wanted = query.trim().to_lowercase();
        let bare = wanted.trim_start_matches("::");
        if bare.is_empty() || limit == 0 {
            return Vec::new();
        }

        let mut hits: Vec<Hit<'_>> = Vec::new();

        if !bare.contains("::") {
            // **One word is about the name.** The names are kept sorted by their lowercase form, which is the order
            // the answer is reported in, so the search opens them best rank first and *stops when it has enough*:
            //
            // ```text
            // exact      the one key equal to the query
            // prefix     the keys from the query on, while they begin with it        (a range, not a scan)
            // substring  the remaining keys that contain it                          (a scan that stops early)
            // ```
            if let Some(spellings) = self.names.spellings_of(bare) {
                self.open_group(spellings, Some(0), bare, limit, &mut hits);
            }

            if hits.len() < limit {
                for (key, spellings) in self.names.lowered_from(bare) {
                    if !key.starts_with(bare) {
                        break;
                    }
                    if key == bare {
                        continue;
                    }
                    self.open_group(spellings, Some(1), bare, limit, &mut hits);
                    if hits.len() >= limit {
                        break;
                    }
                }
            }

            if hits.len() < limit {
                for (key, spellings) in self.names.lowered_from("") {
                    if key.starts_with(bare) || !key.contains(bare) {
                        continue;
                    }
                    self.open_group(spellings, Some(2), bare, limit, &mut hits);
                    if hits.len() >= limit {
                        break;
                    }
                }
            }
        } else {
            // **A qualified query is about the last segment's prefix**, so only the names that begin with it are
            // opened — in name order, and the rank (exact last segment, or a prefix of it) is decided per fact by
            // the tail of its qualified name.
            let last_asked = bare.rsplit("::").next().unwrap_or_default();

            for (key, spellings) in self.names.lowered_from(last_asked) {
                if !key.starts_with(last_asked) {
                    break;
                }
                self.open_group(spellings, None, bare, limit, &mut hits);
                if hits.len() >= limit {
                    break;
                }
            }

            // A fact with no name has its scope's last segment for one, so it is not where the name order looks.
            // There are few of them, and the rank decides; whatever the ordered walk left out is later than the
            // `limit` it already has, so adding these to it cannot push out an answer that belongs.
            if !last_asked.is_empty() {
                self.open_group(&[Box::from("")], None, bare, limit, &mut hits);
            }
        }

        sort_hits(&mut hits);
        hits.into_iter()
            .take(limit)
            .map(|hit| ProjectSymbol {
                file: hit.file.to_path_buf(),
                fact: hit.fact.clone(),
            })
            .collect()
    }

    /// Open every spelling of one lowercase name: their hits, in the order [`sort_hits`] defines.
    ///
    /// **A name declared more than [`POPULAR_NAME`] times is cut at `limit` hits, in file order.** `size`, `begin` and
    /// `run` are declared once per class that has one, and ordering *all* of them by qualified name to show the first
    /// hundred would cost a project's worth of work for an answer no reader can tell from any other hundred. The cut
    /// is deterministic (postings are in insertion order), and a name below the threshold is ordered exactly.
    pub(crate) fn open_group<'a>(
        &'a self,
        spellings: &[Box<str>],
        rank: Option<usize>,
        wanted: &str,
        limit: usize,
        into: &mut Vec<Hit<'a>>,
    ) {
        let start = into.len();
        for spelling in spellings {
            let postings = self.names.named(spelling);
            let stop_at = match postings.len() > POPULAR_NAME {
                true => start + limit,
                false => usize::MAX,
            };
            self.hits_in(postings, rank, wanted, stop_at, into);
        }
        into[start..].sort_by(hit_order);
    }

    /// The hits among one name's postings, **one reading per file** — see [`ProjectIndex::visible_declarations_upto`].
    ///
    /// A file can carry two declaration lists: its summary (the file read on its own) and its cooked reading (the file
    /// read in a program). Only one of them is the file's reading, and which one is
    /// [`ProjectIndex::the_reading_is_cooked`] — so a posting that belongs to the other list is not a candidate. The
    /// deduplication this used to do (raw first, cooked shadowed by it) is gone with the union: two readings of one
    /// file are not two halves of one answer, they are two answers about two translation units.
    ///
    /// `rank` is the rank the caller already knows (a whole bucket of names shares one), or `None` when the rank has
    /// to be worked out from the declaration's qualified name.
    pub(crate) fn hits_in<'a>(
        &'a self,
        postings: &[Posting],
        rank: Option<usize>,
        wanted: &str,
        stop_at: usize,
        into: &mut Vec<Hit<'a>>,
    ) {
        for posting in postings.iter() {
            if into.len() >= stop_at {
                break;
            }
            // **One reading per file**, and a posting from another of that file's readings is not a candidate — see
            // [`ProjectIndex::the_reading_for`]. A symbol search has no translation unit to ask from, so every file
            // answers with the reading it has by default.
            if posting.reading != self.the_reading_for(posting.file, None) {
                continue;
            }
            let Some((summary, fact)) = self.resolve(*posting) else {
                continue;
            };

            let qualified = fact.qualified_name();
            let Some(rank) = rank.or_else(|| rank_of(&qualified, &fact.name, wanted)) else {
                continue;
            };

            let qualified = qualified.to_lowercase();
            // What the fact is called for ordering: its name, or — with none — the last segment of its scope.
            let name = match fact.name.is_empty() {
                false => crate::index::names::lowercase(&fact.name),
                true => qualified
                    .rsplit("::")
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            };

            into.push(Hit {
                rank,
                name,
                qualified,
                file: &summary.path,
                posting: *posting,
                fact,
            });
        }
    }

    /// The fact a posting points at, and the summary of the file it is in.
    pub(crate) fn resolve(&self, posting: Posting) -> Option<(&FileSummary, &DeclFact)> {
        let key = self.order.get(&posting.file)?;
        let summary = self.summaries.get(key)?;
        let fact = match posting.reading {
            0 => summary.declarations.get(posting.index())?,
            reading => self
                .cooked
                .get(key)?
                .reading(reading)?
                .declarations
                .get(posting.index())?,
        };
        Some((summary, fact))
    }

    /// **Forget the answers this index gave about names** — for a probe that must ask the same questions twice
    /// (`tests/patch_probe.rs`, which measures what the lookup-time guard patch still removes). Nothing a session does
    /// calls this: the memos are dropped by the changes that make them wrong, and dropping them at another moment would
    /// only cost a re-walk.
    pub fn forget_the_name_answers(&self) {
        if let Ok(mut answers) = self.definition_answers.lock() {
            answers.clear();
        }
        if let Ok(mut guards) = self.guard_answers.lock() {
            guards.clear();
        }
    }
    /// The files that **define** the macro `name`, in insertion order — one lookup, where the question used to be a
    /// scan of every file's macro facts.
    pub fn files_defining_macro(&self, name: &str) -> Vec<PathBuf> {
        self.names
            .definers_of(name)
            .iter()
            .filter_map(|sequence| self.order.get(sequence))
            .filter_map(|key| self.summaries.get(key))
            .map(|summary| summary.path.clone())
            .collect()
    }

    /// The summaries of the files that define **any** of these macros, in insertion order.
    ///
    /// The set form of [`ProjectIndex::files_defining_macro`], for a caller that holds a whole vocabulary — the
    /// words a file mentions — and wants the files that could matter to it, without visiting the ones that cannot.
    pub fn files_defining_any_macro<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Vec<&FileSummary> {
        let mut sequences: BTreeSet<u32> = BTreeSet::new();
        for name in names {
            sequences.extend(self.names.definers_of(name).iter().copied());
        }

        sequences
            .into_iter()
            .filter_map(|sequence| self.order.get(&sequence))
            .filter_map(|key| self.summaries.get(key))
            .collect()
    }

    /// How many distinct declared names the index holds.
    pub fn distinct_names(&self) -> usize {
        self.names.distinct_names()
    }

    /// **The parameters of a class template**, in declaration order, as the names its members' types are written
    /// with — `["_Ty", "_Alloc"]` for `std::vector`.
    ///
    /// The half of substitution that is a *lookup*: the arguments are written at the use (`std::vector<int>`, which
    /// is the `Type` a caller has), and the names are written at the declaration, once. Pairing them is
    /// [`crate::Type::substituted`], and this is where the names come from.
    ///
    /// # Why this reads a fact rather than a tree
    ///
    /// Because the declaration is usually in **another file**. A member query is asked about
    /// `std::vector<int>::reference` from a `.cpp` that includes `<vector>`, and the index holds summaries rather
    /// than the text it read them from — so the parameter names travel with the fact
    /// ([`DeclFact::parameters`]), which is the same reason every other thing a cross-file answer needs is stored.
    ///
    /// # The first declaration that **is a template**, not the first declaration
    ///
    /// A class template is normally declared several times: a forward declaration (`template <class T> class
    /// vector;`), the definition, and possibly a partial specialization or two. The first may have no parameter
    /// list that says anything, and a partial specialization has one that says the *wrong* thing — so the search is
    /// for the first fact that actually records parameters. That is a heuristic, and the honest version of it is:
    /// a partial specialization's list is indistinguishable from a primary template's in a `DeclFact`, because
    /// both are just names. It is the right answer whenever the first such declaration is the primary template,
    /// which is the ordinary case, and it is never a *made-up* answer — a name paired with an argument it does not
    /// belong to is still a name that appears in the class.
    /// **The parameters of the declaration that this use of the class is an instance of.**
    ///
    /// [`ProjectIndex::template_parameters_of`] answers "the first declaration of this class that has a parameter
    /// list", and for a class template with partial specializations that is not the same question: `std::vector` is
    /// declared three ways in MSVC's `<vector>` — the primary with `["_Ty", "_Alloc"]`, and two specializations whose
    /// lists are `["_Alloc"]` — and taking the first one that has *any* parameters substitutes the use's arguments
    /// into the wrong slots. Measured: `std::vector<MarkEvent>::operator[]` answered `_Ty&`, because `_Ty` is the
    /// primary's first parameter and the list in hand was a specialization's.
    ///
    /// The choice is the one [`direct_member`] already makes for members: [`a_pattern_matches`] compares the
    /// declaration's own pattern against the arguments the use wrote, and a pattern argument that is one of the class's
    /// own parameters is a slot. The first declaration that matches wins, and one that has no parameters at all is
    /// only the fallback.
    pub fn template_parameters_for(
        &self,
        class: &str,
        visible_from: &Path,
        arguments: &[String],
    ) -> Vec<String> {
        self.the_template_for(class, visible_from, arguments)
            .map(|(parameters, _)| parameters)
            .unwrap_or_default()
    }

    /// **What the same parameters default to**, in the same order — `["", "allocator<_Ty>"]` for
    /// `template <class _Ty, class _Alloc = allocator<_Ty>>`.
    ///
    /// The other half of [`ProjectIndex::template_parameters_for`], and the one a use that writes *fewer* arguments
    /// needs: see [`crate::substituted_spelling_with_defaults`] for the three-hop chain that needs it and the
    /// measurement behind it.
    pub fn template_parameter_defaults_for(
        &self,
        class: &str,
        visible_from: &Path,
        arguments: &[String],
    ) -> Vec<String> {
        self.the_template_for(class, visible_from, arguments)
            .map(|(_, defaults)| defaults)
            .unwrap_or_default()
    }

    /// **The declaration whose parameter list a use of `class` is read against**, as `(names, defaults)`.
    ///
    /// One selection for both answers: the names and the defaults have to come from the *same* declaration, or a
    /// partial specialization's names would be paired with its primary's defaults.
    pub(crate) fn the_template_for(
        &self,
        class: &str,
        visible_from: &Path,
        arguments: &[String],
    ) -> Option<(Vec<String>, Vec<String>)> {
        let bare = class.strip_prefix("::").unwrap_or(class);
        let last = bare.rsplit("::").next().unwrap_or(bare);

        let mut candidates = self.files_declaring(class, visible_from);
        if candidates.is_empty() {
            candidates = self.files_declaring(last, visible_from);
        }
        // **A declaration whose pattern has a different number of arguments is not an instance of this use**, which is
        // the one place this cannot lean on `a_pattern_matches`: that function answers `true` for a length mismatch on
        // purpose (a use that wrote fewer arguments — defaulted template parameters — keeps its candidates), and here a
        // mismatch is exactly what tells `vector<bool, _Alloc>` from `vector<_Ty, _Alloc>`.
        let of_the_same_arity = |found: &&VisibleDeclaration<'_>| {
            let pattern = found.fact.pattern.as_deref().unwrap_or_default();
            let slots = arguments_in_a_spelling(pattern).unwrap_or_default();
            slots.len() == arguments.len() && lists_usable_parameter_names(&found.fact.parameters)
        };
        candidates
            .iter()
            .filter(of_the_same_arity)
            .find(|found| {
                a_pattern_matches(
                    found.fact.pattern.as_deref(),
                    &found.fact.parameters,
                    Some(arguments),
                )
            })
            // The declaration with the **longest** parameter list is the primary template: a specialization names
            // some of its arguments concretely and therefore has fewer parameters to describe them with.
            .or_else(|| {
                candidates
                    .iter()
                    .filter(|found| lists_usable_parameter_names(&found.fact.parameters))
                    .max_by_key(|found| found.fact.parameters.len())
            })
            .map(|found| {
                (
                    found.fact.parameters.clone(),
                    found.fact.parameter_defaults.clone(),
                )
            })
    }

    pub fn template_parameters_of(&self, class: &str, visible_from: &Path) -> Vec<String> {
        let bare = class.strip_prefix("::").unwrap_or(class);
        let last = bare.rsplit("::").next().unwrap_or(bare);

        let mut candidates = self.files_declaring(class, visible_from);
        if candidates.is_empty() {
            candidates = self.files_declaring(last, visible_from);
        }

        candidates
            .iter()
            .map(|found| &found.fact.parameters)
            .find(|parameters| !parameters.is_empty())
            .cloned()
            .unwrap_or_default()
    }
    /// The files in which `name` is visible, in insertion order.
    ///
    /// `name` is matched against a declaration's **qualified** name first — `ns::Widget` — and against its bare
    /// name only as a fallback, because a qualified spelling is a much stronger claim than a name that happens to
    /// appear in some scope. A caller that gets one answer from the qualified match should prefer it to any
    /// number from the bare one.
    pub fn files_declaring(&self, name: &str, visible_from: &Path) -> Vec<VisibleDeclaration<'_>> {
        // The name is looked up under the two spellings a fact can answer to: the whole thing (a bare name) and its
        // last segment (`ns::Widget` is `Widget` declared in `ns`). `matches` then decides between them.
        let bare = name.strip_prefix("::").unwrap_or(name);
        let last = bare.rsplit("::").next().unwrap_or(bare);
        self.visible_declarations(
            visible_from,
            |fact| matches(fact, name),
            Narrow::Named(&[bare, last]),
        )
    }

    /// Every declaration written **directly in** the scope `scope`, visible from `visible_from`.
    ///
    /// The whole-scope counterpart of [`ProjectIndex::definition`]: that asks about one name, this asks about
    /// every name one scope holds, which is what a member list is made of. `scope` is a **qualified** spelling —
    /// `Widget`, `ns::Widget` — because that is what a [`DeclFact`] records; see [`DeclFact::scope`].
    ///
    /// Note what "directly in" excludes, because it is the whole reason this is not a name search: a local
    /// variable inside a member function is a fact whose scope is `None` — a function body contributes no segment
    /// to a qualified name — so `C`'s members are `C`'s bindings and not everything written between its braces.
    pub fn declarations_in(&self, scope: &str, visible_from: &Path) -> Vec<VisibleDeclaration<'_>> {
        self.visible_declarations(
            visible_from,
            |fact| fact.scope.as_deref() == Some(scope),
            Narrow::Scoped(scope),
        )
    }

    /// [`ProjectIndex::declarations_in`] with a **second** condition, applied while the facts are still borrowed.
    ///
    /// The second predicate exists for one caller and one reason: a completion has a prefix, and on a file that
    /// includes `<string>` the scope it is listing holds over a thousand declarations. A caller that collected them
    /// and then kept the ones beginning with `w` would do a `DeclFact` clone, a provenance lookup and a `Vec` push
    /// per name to throw 99% of them away — on every keystroke. The two conditions are asked together because
    /// there is no useful notion of order between them: both are filters, and a fact that fails either is not
    /// collected.
    ///
    /// A method rather than a parameter on [`ProjectIndex::declarations_in`] because that one's callers do not have
    /// a second condition, and a query with an optional filter it never uses is a query whose filter is untested.
    pub fn declarations_in_where(
        &self,
        scope: &str,
        visible_from: &Path,
        accepts: impl Fn(&DeclFact) -> bool,
    ) -> Vec<VisibleDeclaration<'_>> {
        self.visible_declarations_upto(
            visible_from,
            &|fact: &DeclFact| fact.scope.as_deref() == Some(scope) && accepts(fact),
            MAX_COLLECTED_IN_A_SCOPE,
            Narrow::Scoped(scope),
        )
    }

    /// The visibility walk over the declaration list, with the door open for a caller outside this module and
    /// capped at `MAX_COLLECTED_NAMES`.
    ///
    /// The cap and not the whole set, because the one caller is a completion and the names this drops could not
    /// have reached the top of its list — see `MAX_COLLECTED_NAMES`, which states the argument. A caller that wants
    /// **every** visible declaration asks [`ProjectIndex::files_declaring`] or [`ProjectIndex::declarations_in`],
    /// which have no cap because their answers are about one name rather than a list.
    pub fn visible_declarations_where(
        &self,
        visible_from: &Path,
        accepts: impl Fn(&DeclFact) -> bool,
    ) -> Vec<VisibleDeclaration<'_>> {
        self.visible_declarations_upto(visible_from, &accepts, MAX_COLLECTED_NAMES, Narrow::Nothing)
    }

    /// **Every file `from` reaches through its `#include`s, transitively** — the cheap half of visibility.
    ///
    /// A graph question only: the summaries record where each include resolved, so this is a breadth-first walk over
    /// hash lookups, with **no guard evaluation**. That is exactly what a rule suggesting "include this header" needs
    /// and nothing more: a name declared in a file this set already holds is treated as visible (so no finding is ever
    /// made about a header the file reaches), and the answer is conservative in the direction that matters — it can
    /// miss a name behind an inactive guard, never invent one.
    ///
    /// The cost is the closure's size in `HashSet` probes, which is what lets it be asked on every keystroke: on a
    /// project whose closure is 600 files the whole walk is under a millisecond, against **1.2 s** for the full
    /// visibility walk that evaluates every condition on the way.
    pub fn include_closure(&self, from: &Path) -> std::collections::HashSet<String> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut pending: Vec<String> = vec![normalize(from)];
        while let Some(path) = pending.pop() {
            if !seen.insert(path.clone()) {
                continue;
            }
            let Some(summary) = self.summaries.get(&path) else {
                continue;
            };
            for include in &summary.includes {
                if let Some(target) = &include.resolved {
                    let next = normalize(target);
                    if !seen.contains(&next) {
                        pending.push(next);
                    }
                }
            }
        }
        seen
    }

    /// **Every name visible from `from`, as one set** — what a rule that asks about many names needs.
    ///
    /// [`ProjectIndex::definition`] answers one name at a time and pays the visibility walk per call (memoised, but
    /// still a walk plus a `Vec` of candidates). A check that asks about *every type a file spells* wants the walk
    /// once and hash lookups after that: measured on a real project, per-name asks were **0.7 s each** on a file whose
    /// closure is large, and the whole set is one walk.
    pub fn visible_names(&self, from: &Path) -> std::collections::HashSet<String> {
        self.visible_declarations_upto(from, &|_| true, usize::MAX, Narrow::Nothing)
            .into_iter()
            .map(|declaration| declaration.fact.name.to_string())
            .collect()
    }

    /// Every fact in the index whose **bare** name is `name`, with the file it was written in.
    ///
    /// A diagnostic, for asking "is the declaration I am looking for in the index at all, and under which scope" —
    /// the question that separates "the walk did not find it" from "it was never filed where the walk looks".
    pub fn name_postings(&self, name: &str) -> usize {
        self.names.named(name).len()
    }

    /// See [`ProjectIndex::name_postings`].
    pub fn scope_postings(&self, scope: &str) -> usize {
        self.names.scoped(scope).len()
    }

    /// See [`ProjectIndex::name_postings`].
    pub fn all_summaries(&self) -> Vec<(std::path::PathBuf, &FileSummary)> {
        self.summaries
            .values()
            .map(|summary| (summary.path.clone(), summary))
            .collect()
    }

    /// See [`ProjectIndex::name_postings`].
    pub fn summaries_named_like(&self, needle: &str) -> Vec<std::path::PathBuf> {
        self.summaries
            .values()
            .map(|summary| summary.path.clone())
            .filter(|path| {
                path.to_string_lossy()
                    .to_ascii_lowercase()
                    .contains(&needle.to_ascii_lowercase())
            })
            .collect()
    }

    /// See [`ProjectIndex::name_postings`].
    pub fn every_fact_named(&self, name: &str) -> Vec<(std::path::PathBuf, &DeclFact)> {
        let mut found = Vec::new();
        for posting in self.names.named(name) {
            let Some(key) = self.order.get(&posting.file) else {
                continue;
            };
            let Some(summary) = self.summaries.get(key) else {
                continue;
            };
            if let Some(fact) = summary.declarations.get(posting.index())
                && fact.name == name
            {
                found.push((summary.path.clone(), fact));
                continue;
            }
            if let Some((_, fact)) = self.resolve(*posting)
                && fact.name == name
            {
                found.push((summary.path.clone(), fact));
            }
        }
        found
    }

    /// The declarations some predicate accepts, in the files `visible_from` can see.
    ///
    /// The one place the visibility walk is applied to the declaration list, so that a new query over facts
    /// cannot forget it and quietly answer with a declaration in a file the querying file does not include —
    /// which is a jump to something it cannot compile against. `includers_of` and [`ProjectIndex::visible_files`]
    /// are the graph; this is the graph applied to a question.
    ///
    /// # Two readings, one candidate list
    ///
    /// Each visible file contributes its **raw** declarations and, when it has been cooked, its **cooked** ones
    /// (see the `cooked` field). They are unioned rather than chosen between, and the reason is that the query
    /// already carries the discriminator: a caller asking for `std::to_string` cannot be answered by the raw
    /// reading's file-scope `to_string`, and one asking for `to_string` gets both — which is honestly *ambiguous*,
    /// because which one it means depends on the scope the caller is in. What the union must not do is count the
    /// same declaration twice: a declaration both readings found (`(qualified name, kind)`) is one candidate, or
    /// every ordinary declaration would come back as two and every lookup would answer `Ambiguous`.
    ///
    /// # Why the graph is walked once, and not once per file
    ///
    /// This used to ask `visibility_of` for **each summary** in the index, and each of those answers walked the
    /// include graph from scratch: a query over a standard-library closure therefore did three hundred
    /// breadth-first searches to answer one question. Measured, on a closure of 308 files: **573 ms** for a
    /// completion at `std::`, which is a query a client asks on every keystroke. One walk and a map lookup is the
    /// same answer in under a millisecond.
    pub(crate) fn visible_declarations<'a>(
        &'a self,
        visible_from: &Path,
        accepts: impl Fn(&DeclFact) -> bool,
        narrow: Narrow<'_>,
    ) -> Vec<VisibleDeclaration<'a>> {
        self.visible_declarations_upto(visible_from, &accepts, usize::MAX, narrow)
    }

    /// [`ProjectIndex::visible_declarations`] with a **cap** on how many are collected.
    ///
    /// The cap is what makes a completion over a standard-library closure cheap, and it is safe for the same
    /// reason the collection is: a caller that wants at most `n` names and ranks them by **how near the cursor
    /// their declaration is** cannot be affected by the `n + 1`-th, because every name this walk produces is from
    /// a file reached through an include — the tier that ranks last. See [`MAX_COLLECTED_NAMES`].
    ///
    /// `usize::MAX` is the honest spelling of "no cap" rather than an `Option`, because the two callers that pass
    /// it are asking a question about a *name* rather than a list — a jump, a rename — and their answers are
    /// allowed to be as many as there are.
    pub(crate) fn visible_declarations_upto<'a>(
        &'a self,
        visible_from: &Path,
        accepts: &impl Fn(&DeclFact) -> bool,
        limit: usize,
        narrow: Narrow<'_>,
    ) -> Vec<VisibleDeclaration<'a>> {
        // **Only the files the cursor's file can see are looked at**, and in insertion order. The walk answers with
        // a handful of hundreds even when the project holds a hundred thousand files, so the cost of the question is
        // the size of the closure and not the size of the project.
        //
        // …and **which of them are in view only through a module import**: those are the files
        // where a declaration counts only if it is exported, and the distinction is per file rather
        // than per fact because a file reached by `#include` has no such restriction at all. **The
        // translation unit this question is asked from** — the reading every file's answer is
        // chosen by.
        let asking = self.the_program_that_read(visible_from);
        let walk_started = std::time::Instant::now();
        let asks_before = CONDITION_ASKS.load(std::sync::atomic::Ordering::Relaxed);
        let nanos_before = CONDITION_NANOS.load(std::sync::atomic::Ordering::Relaxed);
        let (walk, walk_files) = self.the_visible_walk(visible_from);
        let files = walk_files.as_ref();
        let through_an_import = &walk.through_an_import;
        if crate::probes::visibility() {
            eprintln!(
                "[visibility] {} file(s) after the order step, from {}",
                files.len(),
                visible_from.display()
            );
        }
        if crate::probes::timing() && walk_started.elapsed().as_micros() >= 500 {
            let asks = CONDITION_ASKS.load(std::sync::atomic::Ordering::Relaxed) - asks_before;
            let nanos = CONDITION_NANOS.load(std::sync::atomic::Ordering::Relaxed) - nanos_before;
            eprintln!(
                "[visibility] {:>5.1} ms, {} file(s), from {} — {asks} condition(s)/{} ms",
                walk_started.elapsed().as_secs_f64() * 1000.0,
                files.len(),
                visible_from
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| visible_from.display().to_string()),
                nanos / 1_000_000,
            );
        }
        // Everything below this line is reading facts out of files that are already known to be visible: the
        // filtering, the `DeclFact` clones, and the cooked-vs-raw deduplication. Timed apart from the walk because
        // the two have nothing in common — the walk is a question about the graph and this is a question about the
        // declarations — and because a completion that is slow with a *fast* walk is a different defect.

        let mut found = Vec::new();

        // **A name or a scope narrows further**: the postings say which files hold a candidate at all, so a file
        // that has none is never opened. Without a narrowing every visible file's declarations are the candidates.
        let postings: Option<std::borrow::Cow<'_, [Posting]>> = match narrow {
            Narrow::Nothing => None,
            Narrow::Scoped(scope) => Some(std::borrow::Cow::Borrowed(self.names.scoped(scope))),
            Narrow::Named(names) => {
                let mut keys: Vec<&str> = names.to_vec();
                keys.dedup();
                match keys.as_slice() {
                    [one] => Some(std::borrow::Cow::Borrowed(self.names.named(one))),
                    _ => {
                        let mut merged: Vec<Posting> = keys
                            .iter()
                            .flat_map(|key| self.names.named(key).iter().copied())
                            .collect();
                        merged.sort_unstable();
                        merged.dedup();
                        Some(std::borrow::Cow::Owned(merged))
                    }
                }
            }
        };

        if std::env::var("CPPLS_TRACE_NAMES").is_ok()
            && let Narrow::Named(names) = narrow
            && names.iter().any(|name| *name == "cout" || *name == "map")
        {
            let postings = match &postings {
                Some(postings) => postings
                    .iter()
                    .map(|posting| {
                        format!(
                            "file {} id {} index {}",
                            posting.file,
                            posting.reading,
                            posting.index()
                        )
                    })
                    .collect::<Vec<_>>(),
                None => vec!["no narrow".to_string()],
            };
            eprintln!(
                "[names] visible_declarations {:?}: {} posting(s) — {postings:?}",
                names,
                postings.len()
            );
        }

        // One `(file, its candidates)` group at a time, in file order. Without postings a file's candidates are all of
        // its facts, which is what the empty group range stands for.
        let mut next = 0;
        let mut visible = files.iter();

        loop {
            // **The cap is checked before a file is read, not while it is.** A file is one declaration list, and
            // the only thing worth stopping on is a whole one: taking half of a file's facts would leave the
            // cooked-vs-raw deduplication below comparing against a `raw` list that is itself half a file, and
            // `(name, kind)` identity is per file. So a query takes files until it has enough, and "enough" is a
            // number no answer can reach past — see [`MAX_COLLECTED_NAMES`].
            if found.len() >= limit {
                break;
            }

            let (file, visibility, group): (u32, IncludeVisibility, Option<&[Posting]>) =
                match &postings {
                    None => {
                        let Some(&(file, visibility)) = visible.next() else {
                            break;
                        };
                        (file, visibility, None)
                    }
                    Some(postings) => {
                        let Some(first) = postings.get(next) else {
                            break;
                        };
                        let file = first.file;
                        let end = next + postings[next..].partition_point(|held| held.file == file);
                        let group = &postings[next..end];
                        next = end;

                        let Ok(at) = files.binary_search_by_key(&file, |visible| visible.0) else {
                            continue;
                        };
                        (file, files[at].1, Some(group))
                    }
                };

            let Some(key) = self.order.get(&file) else {
                continue;
            };
            let Some(summary) = self.summaries.get(key) else {
                continue;
            };

            // **A file in view only through an `import` contributes only what it exports.** The rule is per *file*
            // rather than per fact because it is about how the file became visible: `import m;` brings what the
            // module interface exported and nothing else, while an `#include` brings the whole file — and a file
            // that is both is include-visible, so this is false for it (see `visible_files_with_modules`).
            //
            // `DeclFact::exported` is the reading; on a file that declares no module it is `false` throughout, and
            // the walk never asks a file like that this question because it cannot be in `through_an_import` — only
            // a module *interface unit* is.
            let only_what_is_exported = through_an_import.contains(key);
            let contributes =
                |fact: &DeclFact| accepts(fact) && (!only_what_is_exported || fact.exported);

            // **One reading per file, and which one is the asking translation unit's answer.** The file read *on its
            // own* and the file read **inside a program** are not two views of one thing: measured on a real project,
            // **1 095 declarations over 177 files** are in one and not the other (`tests/unit_read_probe.rs`), because
            // `_CRT_…` and `_GLIBCXX_…` decide differently in the two. Offering both at once answers for two
            // translation units with one question, and offering the *wrong* one answers `b.cpp` with what `a.cpp`
            // compiled.
            //
            // So the reading is chosen per file — [`ProjectIndex::the_reading_for`], which prefers the program that
            // read **the file this question is about** (the reader's own translation unit) and falls back to the
            // file's own reading. Its id is what a posting carries, so the narrowing below stays exact.
            let reading = self.the_reading_for(file, asking);
            let cooked = self
                .cooked
                .get(key)
                .and_then(|readings| readings.reading(reading));

            // **A reading that does not hold the name cannot answer for it, and the summary is the file's own text.**
            // The choice above is the right default — it is the asking program's reading — but a program's reading is a
            // rendering, and a name can be missing from it for a reason that is about *that cook* rather than about the
            // file: `print`'s whole body sits under `#ifdef __cpp_lib_print`, and a module's cook that lost the
            // feature-test macro contributed a reading of `print` with **no** `println` while the summary had all
            // four — and that reading shadowed the summary for every query, which is what a hover on `std::println`
            // answered `null` for. So the chosen reading is used when it has something to say about this question, and
            // the summary when it does not.
            // **A reading that does not hold the name cannot answer for it, and the summary is the file's own text.**
            // The choice above is the right default — it is the asking program's reading — but a program's reading is a
            // rendering, and a name can be missing from it for a reason that is about *that cook* rather than about the
            // file: `print`'s whole body sits under `#ifdef __cpp_lib_print`, and a module's cook contributed a
            // reading of `print` with **no** `println` while the summary had all four — that reading shadowed the
            // summary for every query, which is what a hover on `std::println` answered `null` for. So the chosen
            // reading is used when it has something to say about this question, and the summary when it does not.
            // **The asking program's own reading answers alone.** A name it does not hold is a name that program did
            // not compile — `#if 0 struct Dead` is in the file's environment-free summary and in no rendering — so the
            // summary may not speak for it. The fallback below stays for the other case, which is a real one: a cook
            // that went wrong (a module's cook losing a feature-test macro) must not shadow a declaration the file does
            // hold unconditionally elsewhere.
            let the_askers_own = asking.is_some_and(|program| {
                self.order
                    .get(&file)
                    .and_then(|key| self.cooked.get(key))
                    .and_then(|readings| readings.id_for(program))
                    .is_some()
            });
            let (facts, reading): (&[DeclFact], u32) = match cooked {
                Some(cooked) if cooked.declarations.iter().any(&accepts) => {
                    (cooked.declarations.as_slice(), reading)
                }
                // **A reading with no declaration at all is a cook that produced nothing**, and the summary is the
                // file's own text. The gate below ("the asking program's own reading answers alone") is about a
                // reading that *has* declarations and lacks this name — `#if 0 struct Dead`, which that program did not
                // compile. A reading that holds nothing compiled nothing, and its cause is a cook or a parse that went
                // wrong, not the file. Measured on a MinGW project importing `std`: the program's parse nested
                // `<print>` inside a `<mutex>` function body (a brace paired across two files), so the file's four
                // `println` overloads were missing from its reading while its summary held all forty-one declarations;
                // letting that reading speak deleted six of `std::println`'s six overloads from the hover.
                Some(cooked)
                    if cooked.declarations.is_empty()
                        && summary.declarations.iter().any(&accepts) =>
                {
                    (summary.declarations.as_slice(), 0)
                }
                // **The summary's postings carry reading `0`** — measured, not guessed: `[names]` printed
                // `file 172 id 0 index 2` for `iostream`'s `cout`, while `Readings::ON_ITS_OWN` is `1`, so a fallback
                // that claimed `1` matched no posting and answered nothing. `0` is what `insert_at` files a summary's
                // declarations under. The policy itself is unchanged: the asking program's own reading answers alone,
                // because a name it does not hold is a name that program did not compile.
                Some(_) if !the_askers_own && summary.declarations.iter().any(&accepts) => {
                    (summary.declarations.as_slice(), 0)
                }
                Some(cooked) => (cooked.declarations.as_slice(), reading),
                None => (summary.declarations.as_slice(), 0),
            };

            let candidates: Box<dyn Iterator<Item = &DeclFact> + '_> = match group {
                None => Box::new(facts.iter()),
                Some(group) => Box::new(
                    group
                        .iter()
                        .filter(|posting| posting.reading == reading)
                        .filter_map(|posting| facts.get(posting.index())),
                ),
            };

            for fact in candidates.filter(|fact| contributes(fact)) {
                found.push(VisibleDeclaration {
                    file: summary.path.clone(),
                    fact,
                    visibility,
                });
            }
        }

        found
    }
    /// every declaration query reports in.
    ///
    /// [`ProjectIndex::visible_in_order_of`] is the same thing for a caller that has **already walked**: the
    /// declaration queries need both halves of the walk's answer (the files, and which of them are in view only
    /// through an `import`) from one walk, and calling this and `visible_files_with_modules` would walk the graph
    /// twice per question.
    pub(crate) fn visible_in_order_of(
        &self,
        visible: Vec<(String, IncludeVisibility)>,
    ) -> Vec<(u32, IncludeVisibility)> {
        // **The lookup below is a suspect, and it is left as it was until the effect of fixing it is understood.**
        //
        // The walk's keys are normalized (`e:/p/app.cpp`) and `sequence` is keyed by the path the summary was filed
        // under, so a file whose two spellings differ is dropped **silently** — the walk says it is visible and this
        // step removes it. That is a bug, and *fixing it alone makes the answers worse*, which is the finding:
        //
        // * a normalized fallback **here**, and
        // * normalizing the key when it is **written** (both spellings inserted, both removed),
        //
        // were each measured on the same cold store and each moved the scoreboard the same way — 4637 (77.1%) →
        // 4609 (76.7%), **+28 `NotDeclaredHere` and not one `Ambiguous`**. And the scoreboard now splits those
        // refusals: of the 77 that had a receiver type, **47 had no visible declaration of the class and 30 had a
        // member list built from no visible declaration** — not one was "the list does not hold the member". So the
        // loss is not a different declaration being chosen; it is the same file becoming visible **twice**, under
        // both spellings, which doubles its summary in the name index and changes what the member walk reads.
        //
        // The next step is therefore to make the **identity** unique — one file, one summary, one sequence number, one
        // set of postings — and only then to stop dropping the spelling. Either half alone is a regression.
        //
        // **That was the hypothesis, and it has been counted: it is false.** The index holds 1581
        // summaries for 1581 distinct identities and **not one file twice**, so the loss is not a
        // duplicate summary. What is left is the only other thing a wider view changes: which
        // declarations exist *at all* for a name — and the scoreboard says every refusal with a
        // receiver type had **no visible declaration of the class** (47) or a list built from none
        // (30), so the 28 lost questions are ones where one visible declaration used to be enough
        // and two now are not. Neither experiment is in the tree; the next round starts from this
        // number, not from a theory.
        let mut files: Vec<(u32, IncludeVisibility)> = visible
            .into_iter()
            .filter_map(|(key, visibility)| Some((*self.sequence.get(&key)?, visibility)))
            .collect();

        files.sort_unstable_by_key(|(sequence, _)| *sequence);
        files.dedup_by_key(|(sequence, _)| *sequence);
        files
    }

    /// **The walks' key: the readings they were made of, per file.**
    ///
    /// A visibility walk crosses `#include`s and evaluates their guards against the macros in force. So what it depends
    /// on is exactly what [`the_walk_inputs_of`] hashes: the resolved targets of the includes, the conditions the
    /// guards ask, and the macro facts a condition can read. **Not the text.** A comment, a reformat, a new function in
    /// a body — none of them can change which files are visible, which is why an edit that touches a body does not have
    /// to re-walk the closure.
    ///
    /// # Why not a revision, and why not a content hash
    ///
    /// The revision rule dropped a walk whenever anything was inserted, and the pump inserts and re-reads constantly;
    /// the content-hash rule dropped it on any edit at all, including a body edit that changed no input. Both were
    /// measured and neither is here: the first made every request pay the walk (`EmmyLuaCodeStyle`: **1.1 s per name
    /// question**, five in one hover), and the second had **0 hits in 58 lookups** during indexing.
    ///
    /// Measured: **~0.2 ms** to validate a 1063-file closure this way, against **1.1 s** to rebuild it.
    pub(crate) fn the_walks_fingerprint(&self, contributors: &[String]) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        /// A file in the closure the index does not hold: a fixed value, so "absent" is a state the fingerprint can
        /// name rather than a hole in it.
        const NOT_IN_THE_INDEX: u64 = 0x1d57_9b40_c3e8_2a6f;
        let mut fingerprint = FNV_OFFSET;
        let mut eat = |value: u64| {
            for byte in value.to_le_bytes() {
                fingerprint ^= u64::from(byte);
                fingerprint = fingerprint.wrapping_mul(FNV_PRIME);
            }
        };
        for path in contributors {
            eat(crate::cache::fnv1a64(path.as_bytes()));
            eat(self
                .walk_inputs
                .get(path)
                .copied()
                .unwrap_or(NOT_IN_THE_INDEX));
        }
        fingerprint
    }

    /// Every file `from` can see, and how — one walk of the include graph.
    ///
    /// The whole-graph answer to what used to be a per-file question: "can this file see that one" was asked once
    /// per summary, and each answer walked the include graph. This returns the answer for every file at once, so
    /// one walk serves a whole query — measured on a closure of 308 files, that turned a 573 ms completion into a
    /// 3 ms one, and every cross-file query over declarations goes through it.
    ///
    /// **Every file that can see `path`** — its transitive includers, nearest first.
    ///
    /// The reverse of [`ProjectIndex::visible_files`], and the question a *change* to a file asks: whose reading was
    /// built while this file said what it used to say? A cooked reading is a reading of its environment as well as of
    /// its text, so a file that defines a macro is read differently by everything that includes it, at any depth.
    ///
    /// Breadth-first over the reverse edges, deduplicated and bounded by the same depth the forward walk uses: an
    /// include graph is not a tree — every header guard makes it a diamond — so "included by, transitively, without
    /// asking about a file twice" is the whole job. The order is the walk's, which is deterministic because the
    /// reverse edges are a `BTreeSet`.
    ///
    /// The **file itself is not in the answer**: a caller dropping that file's own reading has its own reason to, and
    /// one list that means two things is how a caller ends up dropping something twice or not at all.
    pub fn dependents_of(&self, path: &Path) -> Vec<PathBuf> {
        let start = normalize(path);

        let mut seen: HashSet<String> = HashSet::from([start.clone()]);
        let mut pending: VecDeque<(String, usize)> = VecDeque::from([(start, 0)]);
        let mut dependents = Vec::new();

        while let Some((current, depth)) = pending.pop_front() {
            if depth >= MAX_VISIBILITY_DEPTH {
                continue;
            }

            let Some(includers) = self.included_by.get(&current) else {
                continue;
            };

            for includer in includers {
                if !seen.insert(includer.clone()) {
                    continue;
                }
                dependents.push(PathBuf::from(includer));
                pending.push_back((includer.clone(), depth + 1));
            }
        }

        dependents
    }

    /// The visibility recorded is the **best** one found: a file reached both unconditionally and through a
    /// guarded `#include` is unconditional, because the unconditional path is the one that is always there. That
    /// is why a file may be relaxed rather than only visited — the first path found is not necessarily the best,
    /// and a graph with a diamond in it (which every header guard produces) has exactly that shape.
    pub fn visible_files(&self, from: &Path) -> Vec<(String, IncludeVisibility)> {
        self.visible_files_with_modules(from).0
    }

    /// **The walk's answer, validated by the inputs it was made of** — see [`ProjectIndex::the_walks_fingerprint`].
    ///
    /// `None` is a miss, and a miss is the caller's cue to claim the walk (see
    /// [`ProjectIndex::visible_files_with_modules`]). Split out of that function because the single-flight checks it
    /// twice: once before claiming, and once after waiting for somebody else's walk.
    pub(crate) fn the_remembered_walk(
        &self,
        from: &str,
    ) -> Option<(Vec<(String, IncludeVisibility)>, HashSet<String>)> {
        let remembered = self
            .visibility
            .lock()
            .ok()
            .and_then(|walks| walks.get(from).cloned())?;
        let now = self.the_walks_fingerprint(&remembered.contributors);
        if crate::probes::visibility() {
            eprintln!(
                "[visibility]   remembered walk from {from}: {} ({} contributor(s))",
                if now == remembered.fingerprint {
                    "hit"
                } else {
                    "MISS — an input it was made of changed"
                },
                remembered.contributors.len()
            );
        }
        if now != remembered.fingerprint {
            return None;
        }

        let seen = &remembered.seen;
        let mut order: Vec<(String, IncludeVisibility)> = Vec::with_capacity(seen.visibility.len());
        order.push((from.to_string(), IncludeVisibility::Unconditional));
        order.extend(
            seen.visibility
                .iter()
                .filter(|(path, _)| path.as_str() != from)
                .map(|(path, visibility)| (path.clone(), *visibility)),
        );
        Some((order, seen.through_an_import.clone()))
    }

    /// [`ProjectIndex::visible_files`], and **which of those files are in view only because of an `import`**.
    ///
    /// The second half is what the exported bit is applied against: a declaration written in a module interface
    /// unit is visible to an importer **only if it is exported** ([`DeclFact::exported`]), while a file reached
    /// through `#include` has no such restriction — so a query that did not know *how* a file became visible could
    /// only choose between offering names that do not compile and hiding names that do.
    ///
    /// A file that is both included and imported is **not** in the second list: the include makes all of it visible,
    /// and the narrower rule would hide what the wider one grants.
    pub fn visible_files_with_modules(
        &self,
        from: &Path,
    ) -> (Vec<(String, IncludeVisibility)>, HashSet<String>) {
        let from = normalize(from);

        // **The memo first, then one walk at a time.** A miss used to be computed straight away, which is right for
        // the lock (a walk is milliseconds, and a lock held across it is a lock every query waits on) and wrong for
        // the work: two callers that miss the same entry build the same closure twice. That is not hypothetical —
        // the pump prewarms the files a reader has open while the reader's own request arrives, and the review in
        // `docs/perf-review.md` measured two concurrent cold walks of one closure. So a walk is claimed before it
        // starts ([`ProjectIndex::claim_a_walk`]) and a caller that finds its root claimed waits for that answer
        // instead of repeating it.
        //
        // The loop is what makes the wait safe in both directions: the memo is re-checked after every wait, and a
        // walker that could not file an answer (a poisoned lock, a panic) leaves the next turn to become the walker.
        let waiting_since = std::time::Instant::now();
        let mut met = false;
        loop {
            if let Some(answer) = self.the_remembered_walk(&from) {
                self.walks_hit
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return answer;
            }
            if self.claim_a_walk(&from) {
                break;
            }
            if !met {
                met = true;
                self.walks_met
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            // **A walk that never finishes must not wedge a request for ever.** The bound is absurdly generous for a
            // real closure (the largest measured is 3.4 s) and exists only so that a defect in the walk is a slow
            // answer rather than a deadlock.
            if waiting_since.elapsed() > std::time::Duration::from_secs(30) {
                break;
            }
            self.wait_for_the_walk(&from);
        }
        self.walks_built
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _claim = WalkClaim {
            index: self,
            root: from.clone(),
        };

        // **One traversal, and the state it carries is what answers the conditions.** The version this replaced
        // asked `environment::visibility_at` per guarded edge, and that call builds the file's whole closure state
        // — one walk of the include graph *per condition in the closure*. Measured on the MSVC standard library at
        // the latency fixture's cursor: 114 conditions, 1428 ms of a 1431 ms query, 12.5 ms each.
        //
        // `macro_candidates` is the same traversal that already answers "what is in force at this point", and it
        // evaluates a guarded edge as it crosses it, against the state the route has built. So the question this
        // function asks is not a second implementation of the walk — it is the same walk, told to report the files
        // it enters. That is the rule the module keeps being bitten by: one question, one implementation.
        let mut seen = Seen::new(&from);
        let macros_started = std::time::Instant::now();
        let mut state = self.macros.clone();
        let macros_cloned = macros_started.elapsed();
        let certain = self.certainly_in_the_translation_unit(Path::new(&from));
        if crate::probes::timing() && macros_started.elapsed().as_micros() >= 300 {
            eprintln!(
                "[visibility] the state: macro table cloned in {macros_cloned:?} ({} entry(ies)), \
                 the translation unit's certain set in {:?}",
                self.macros.len(),
                macros_started.elapsed() - macros_cloned
            );
        }

        self.macro_candidates(
            Path::new(&from),
            &mut Vec::new(),
            false,
            &mut HashSet::new(),
            &certain,
            &mut Collecting::Visibility(&mut seen),
            &mut state,
            None,
        );

        // **And then the module frontier**, which is a different edge with a different rule: an `import` is not
        // textual, so the walk above could not descend through one without putting another module's macros into a
        // macro environment. What the *visibility* question still owes is everything behind those edges — the
        // imported interface's own includes, its partitions, the partitions' includes — so this is a second walk,
        // over the same summaries, that carries no macro state because nothing on this side of an `import` is a
        // macro fact.
        //
        // It shares `visited`'s *purpose* and not its set: the walk above has already entered the include closure,
        // and re-entering a file it reached would evaluate conditions with a state that has drifted. `module_seen`
        // is what keeps this walk to the files the first one could not reach.
        //
        // **The unit's own closure comes first, for the asking file and for every file the frontier reaches.** A
        // module is not a header: what an importer sees is the unit's whole translation unit, and walking the
        // interface's `#include` edges answers a weaker question — each guard evaluated against a state that is not the
        // unit's, so a header the unit included behind `_HAS_CXX20` is dropped. Measured on the reader's project:
        // `report.cppm` importing `std` saw **151 files and no `std::string`**, while `main.cpp` saw 173 only because
        // its own `#include "extra/widget.h"` pulled the same tree in textually. The rendering has already decided
        // every one of those guards, once, for the module — see [`ProjectIndex::unit_closures`].
        let mut module_seen: HashSet<String> = HashSet::new();
        self.add_the_closures_behind_the_imports(Path::new(&from), &mut seen);
        while let Some(reached) = seen.module_work.pop() {
            if !module_seen.insert(reached.clone()) {
                continue;
            }
            self.add_the_closures_behind_the_imports(Path::new(&reached), &mut seen);

            let mut state = self.macros.clone();
            self.walk_the_module_edges(
                Path::new(&reached),
                &mut seen,
                &mut state,
                &mut module_seen,
            );
        }

        // The order the per-edge version produced: the root first, then the walk's crossing order. Rebuilt from the
        // map rather than recorded during the walk because a file can be crossed more than once and only the first
        // *crossing* is its place in the answer — the better visibility may arrive later, from another route.
        let mut order: Vec<(String, IncludeVisibility)> = Vec::with_capacity(seen.visibility.len());
        order.push((from.clone(), IncludeVisibility::Unconditional));
        order.extend(
            seen.visibility
                .iter()
                .filter(|(path, _)| **path != from)
                .map(|(path, visibility)| (path.clone(), *visibility)),
        );

        // Remembered before it is handed out, and `through_an_import` is cloned out of the shared copy: the caller
        // owns its set, and the memo has to be immutable to be shared.
        //
        // **The contributor list is the closure, normalized once here** so that [`ProjectIndex::insert_at`] can
        // drop precisely the walks a re-read invalidates without normalizing a closure per insert.
        let through_an_import = seen.through_an_import.clone();
        let mut contributors: Vec<String> = Vec::with_capacity(seen.visibility.len() + 1);
        contributors.push(from.clone());
        contributors.extend(
            seen.visibility
                .keys()
                .filter(|path| **path != from)
                .map(|path| normalize(Path::new(path))),
        );
        contributors.sort_unstable();
        contributors.dedup();
        let fingerprint = self.the_walks_fingerprint(&contributors);
        // **The files in sequence order, computed with the walk rather than per question** — the one shape every
        // cross-file question iterates by. See [`CachedWalk::files`].
        let files = self.visible_in_order_of(
            seen.visibility
                .iter()
                .map(|(path, visibility)| (path.clone(), *visibility))
                .collect(),
        );
        if let Ok(mut walks) = self.visibility.lock() {
            walks.insert(
                from,
                std::sync::Arc::new(CachedWalk {
                    seen: std::sync::Arc::new(seen),
                    contributors: std::sync::Arc::new(contributors),
                    fingerprint,
                    files: std::sync::Arc::new(files),
                    stamp: self.changed,
                }),
            );
        }

        (order, through_an_import)
    }

    /// **The walk and its ordered files, memoised** — what a query that iterates the closure needs.
    ///
    /// [`ProjectIndex::visible_files_with_modules`] answers with owned vectors, which is the right shape for its own
    /// callers (a `Vec` of paths for "which files does this file see") and the wrong one for the declaration queries:
    /// they ask the same closure question thousands of times per request (measured: **15 489** for one cold inlay), so
    /// a clone of every path per question was seconds of the request. This hands out the shared walk instead.
    pub(crate) fn the_visible_walk(
        &self,
        from: &Path,
    ) -> (
        std::sync::Arc<Seen>,
        std::sync::Arc<Vec<(u32, IncludeVisibility)>>,
    ) {
        let from = normalize(from);
        let remembered = self
            .visibility
            .lock()
            .ok()
            .and_then(|walks| walks.get(&from).cloned());
        if let Some(known) = remembered {
            // **The stamp first.** A request asks the same closure question thousands of times, and between its
            // questions the index does not change at all: the walk built (or validated) at this stamp answers without
            // touching the closure. Measured on a cold inlay this is the difference between ~0.1 ms and ~1 µs per
            // question, and there are 15 489 of them.
            if known.stamp == self.changed {
                return (
                    std::sync::Arc::clone(&known.seen),
                    std::sync::Arc::clone(&known.files),
                );
            }
            if self.the_walks_fingerprint(&known.contributors) == known.fingerprint {
                let files = if known.files.len() == known.seen.visibility.len() {
                    std::sync::Arc::clone(&known.files)
                } else {
                    // **The order is complete only if every visible file had a sequence when the walk was made.** A
                    // walk can be built while a file it can see is not yet in the index, so the stored order is one
                    // short. The lengths are the check and this is the repair — cheap and rare.
                    std::sync::Arc::new(
                        self.visible_in_order_of(
                            known
                                .seen
                                .visibility
                                .iter()
                                .map(|(path, visibility)| (path.clone(), *visibility))
                                .collect(),
                        ),
                    )
                };
                if let Ok(mut walks) = self.visibility.lock() {
                    walks.insert(
                        from.clone(),
                        std::sync::Arc::new(CachedWalk {
                            seen: std::sync::Arc::clone(&known.seen),
                            contributors: std::sync::Arc::clone(&known.contributors),
                            fingerprint: known.fingerprint,
                            files: std::sync::Arc::clone(&files),
                            stamp: self.changed,
                        }),
                    );
                }
                return (std::sync::Arc::clone(&known.seen), files);
            }
        }
        // A miss builds through the one implementation of the walk, and the freshly memoised entry is the answer.
        let _ = self.visible_files_with_modules(Path::new(&from));
        if let Some(known) = self
            .visibility
            .lock()
            .ok()
            .and_then(|walks| walks.get(&from).cloned())
        {
            return (
                std::sync::Arc::clone(&known.seen),
                std::sync::Arc::clone(&known.files),
            );
        }
        (
            std::sync::Arc::new(Seen::default()),
            std::sync::Arc::new(Vec::new()),
        )
    }

    /// **Everything behind one module edge**: the target's includes, its own module edges, and theirs.
    ///
    /// The second half of [`ProjectIndex::visible_files_with_modules`], and a separate walk because the first one
    /// carries a macro environment and a macro does not cross an `import`. What crosses is *visibility*: a header
    /// a module's interface includes is visible to whoever imports the module — measured on the fixture where
    /// `std.ixx` is `export module std;` plus two `#include`s and a file that writes `import std;` must be offered
    /// `std::string`.
    ///
    /// The guards on the includes it crosses are evaluated against the compilation's own environment and the
    /// target's facts, which is the same state the first walk starts from — a module unit's macros are its own, and
    /// nothing outside it reaches them.
    pub(crate) fn walk_the_module_edges(
        &self,
        path: &Path,
        seen: &mut Seen,
        state: &mut Marked,
        module_seen: &mut HashSet<String>,
    ) {
        let path = normalize(path);
        let Some(summary) = self.summaries.get(&path) else {
            return;
        };

        // **The unit's own definitions, folded in before its includes are judged.** A module interface unit is a
        // *translation unit*: a header it includes can define what a later `#if` in the same unit asks about, and a
        // walk that only recorded *where* each include leads — as this one did — judged those questions with an
        // environment that never learned anything.
        //
        // Measured on `import std;` with MinGW GCC 15: `bits/std.cc` includes `bits/stdc++.h`, whose standard-header
        // block is guarded by `#if _GLIBCXX_HOSTED`; `_GLIBCXX_HOSTED` is `#define`d in `bits/c++config.h`, which the
        // headers *above* that block include. The guard therefore read as undefined, the verdict was `Inactive`, and
        // the block — `#include <vector>` among a hundred others — was dropped: `include/c++/vector` was not in the
        // visible set, `std::vector` had only the `using std::vector;` re-export as its fact, and the member list was
        // empty.
        //
        // Only the **unconditional** facts are folded: a definition inside a condition this walk has not decided is a
        // value it cannot stand behind, and leaving it out is the honest reading.
        for fact in &summary.macros {
            if fact.guard == FactGuard::Unconditional {
                apply_fact(state, fact, false, FactReach::Active);
            }
        }

        let mut verdicts: HashMap<(u32, usize), Option<bool>> = HashMap::new();

        for unit in &summary.modules.header_units {
            let next = normalize(unit);
            seen.visibility
                .entry(next.clone())
                .or_insert(IncludeVisibility::Unconditional);
            if module_seen.insert(next.clone()) {
                seen.module_work.push(next);
            }
        }
        for unit in &summary.modules.partitions {
            let next = normalize(unit);
            seen.visibility
                .entry(next.clone())
                .or_insert(IncludeVisibility::Unconditional);
            if module_seen.insert(next.clone()) {
                self.walk_the_module_edges(Path::new(&next), seen, state, module_seen);
            }
        }
        for imported in &summary.modules.imports {
            let Some(interface) = self.module_interfaces.get(imported.as_ref()) else {
                continue;
            };
            seen.visibility
                .entry(interface.clone())
                .or_insert(IncludeVisibility::Unconditional);
            seen.through_an_import.insert(interface.clone());
            if module_seen.insert(interface.clone()) {
                self.walk_the_module_edges(Path::new(interface), seen, state, module_seen);
            }
        }

        // The includes, each edge's own guard decided against the state the module unit reached it with.
        for include in &summary.includes {
            let visibility = self.include_visibility(
                summary,
                &mut verdicts,
                state,
                &mut Collecting::Visibility(&mut *seen),
                include.guard,
                include.range.start_offset,
            );
            if visibility == Visibility::Inactive {
                continue;
            }

            let Some(target) = &include.resolved else {
                continue;
            };
            let next = normalize(target);
            let step = match visibility {
                Visibility::Active => IncludeVisibility::Unconditional,
                Visibility::Unknown => IncludeVisibility::Conditional,
                Visibility::Inactive => continue,
            };

            match seen.visibility.get(&next) {
                Some(known) if *known <= step => {}
                _ => {
                    seen.visibility.insert(next.clone(), step);
                }
            }
            // An `#include` makes the whole file visible, however it was reached before.
            seen.through_an_import.remove(&next);

            if module_seen.insert(next.clone()) {
                self.walk_the_module_edges(Path::new(&next), seen, state, module_seen);
            }
        }
    }

    /// **Every file behind the `import`s a file writes** — the unit's own closure, when it is known.
    ///
    /// The closure is the unit's **rendering** (`RenderedUnit::walked`, recorded by
    /// [`ProjectIndex::remember_the_unit_closure`]): every file that contributed to what an importer compiles. A file
    /// in it is visible unconditionally and is **not** subject to the exported-bit rule — the module exported it, that
    /// is what being in the unit's translation unit means — so it is also removed from `through_an_import`.
    pub(crate) fn add_the_closures_behind_the_imports(&self, of: &Path, seen: &mut Seen) {
        let Some(summary) = self.summaries.get(&normalize(of)) else {
            return;
        };
        if summary.modules.imports.is_empty() {
            return;
        }
        let Ok(closures) = self.unit_closures.lock() else {
            return;
        };
        for imported in &summary.modules.imports {
            let Some(files) = closures.get(imported.as_ref()) else {
                continue;
            };
            for file in files {
                seen.visibility
                    .entry(file.clone())
                    .or_insert(IncludeVisibility::Unconditional);
                seen.through_an_import.remove(file);
            }
        }
    }

    /// [`ProjectIndex::visible_files`] the way this query computed it **before** the walk carried its state.
    ///
    /// Kept as the transition's other side, and for one reason: the visibility set is what a completion list is
    /// built from, so a walk that produces a *different* set is a different answer rather than a slower one. The
    /// cost is the thing being replaced — it asks `environment::visibility_at` per guarded edge, and that call
    /// rebuilds the file's closure state from scratch. Measured on the latency fixture's cursor before the change:
    /// **114 conditions, 1428 ms of a 1431 ms query, 12.5 ms each.**
    ///
    /// `examples/completion_latency.rs` prints both sets and their differences, which is how the replacement was
    /// checked rather than argued for.
    pub fn visible_files_by_asking_each_condition(
        &self,
        from: &Path,
    ) -> Vec<(String, IncludeVisibility)> {
        let from = normalize(from);

        // The file itself is visible to itself, and unconditionally: a question asked in a file is about what
        // that file says before anything it includes.
        let mut best: HashMap<String, IncludeVisibility> =
            HashMap::from([(from.clone(), IncludeVisibility::Unconditional)]);
        let mut order: Vec<String> = vec![from.clone()];
        let mut pending: Vec<(String, IncludeVisibility, usize)> =
            vec![(from, IncludeVisibility::Unconditional, 0)];
        // The files in view **only** through an `import`, which is where the exported bit applies.
        let mut through_an_import: HashSet<String> = HashSet::new();

        while let Some((current, so_far, depth)) = pending.pop() {
            if depth > MAX_VISIBILITY_DEPTH {
                continue;
            }

            let Some(summary) = self.summaries.get(&current) else {
                continue;
            };

            // **Header units are followed exactly like `#include`s** — `import <vector>;` names a header, and a header
            // unit exports what the header declares, so nothing about the declarations it brings in is restricted. The
            // path was resolved when the file was read (the walk can search nothing), and one that could not be resolved
            // is simply absent.
            for unit in &summary.modules.header_units {
                let next = normalize(unit);

                if let std::collections::hash_map::Entry::Vacant(slot) = best.entry(next.clone()) {
                    slot.insert(IncludeVisibility::Unconditional);
                    order.push(next.clone());
                    pending.push((next, IncludeVisibility::Unconditional, depth + 1));
                }
            }

            // **…and so is a partition of this file's own module.** `export import :area;` makes the partition's
            // declarations part of *this* module's interface, so a file that imports the module has to see them — and
            // the file that declares them is reached by exactly the same kind of resolved path. Not following it is a
            // false negative, which is the direction this project refuses everywhere else: measured on the partition
            // fixture (`target/build_partitions.bat`), `definition("perimeter")` resolved to the interface unit while
            // `rectangle` and `square`, declared in the partition it re-exports, answered "nothing declares it".
            //
            // `Unconditional`, and for a reason that is *not* the same as the import note below: a partition import is
            // not conditional in practice — the grammar does not allow `import :part;` inside a conditional region of a
            // module unit, because the module's interface has to be known in full before anything is compiled against
            // it.
            for unit in &summary.modules.partitions {
                let next = normalize(unit);

                if let std::collections::hash_map::Entry::Vacant(slot) = best.entry(next.clone()) {
                    slot.insert(IncludeVisibility::Unconditional);
                    order.push(next.clone());
                    pending.push((next, IncludeVisibility::Unconditional, depth + 1));
                }
            }

            // **A module import is the other visibility edge**, and it is not an `#include` with another spelling:
            // an import is not textual (no macro travels along it) and it names a *module* rather than a file, so
            // the file behind it is found through the module the project declares. That is what makes
            // `import mathlib;` bring `mathlib::add` into view — measured on the module fixture, where the answer
            // was `None` from a file that imports `mathlib` and whose sibling `mathlib.ixx` exports it.
            //
            // `Unconditional`, and that is a simplification with a direction: an `import` inside an `#if` is
            // recorded here like any other, so a module reached only through a condition the compilation does not
            // satisfy is offered. The `#include` path above is careful about exactly this (see its own note on
            // guards); making imports as careful needs the same per-condition machinery and is registered rather
            // than guessed at — see `plan-units.md` §35.
            for imported in &summary.modules.imports {
                let Some(interface) = self.module_interfaces.get(imported.as_ref()) else {
                    // No file in this project declares it: `import std;` and prebuilt modules land here, and the
                    // honest reading is "unknown", never "nothing" — a name that cannot be seen is not a name that
                    // does not exist. Nothing is added, and nothing is claimed.
                    continue;
                };

                if best.contains_key(interface) {
                    continue;
                }

                best.insert(interface.clone(), IncludeVisibility::Unconditional);
                order.push(interface.clone());
                through_an_import.insert(interface.clone());
                pending.push((
                    interface.clone(),
                    IncludeVisibility::Unconditional,
                    depth + 1,
                ));
            }

            for include in &summary.includes {
                let Some(resolved) = &include.resolved else {
                    continue;
                };
                let next = normalize(resolved);

                // **A guarded `#include` stays `Conditional` here, and the reason is now cost**.
                //
                // The conditions *are* answerable — `crate::index::environment::visibility_at` answers
                // `#if _STL_COMPILER_PREPROCESSOR` correctly once its two upstream bugs are fixed (`own_guard` not
                // recognising `#pragma once` + `#ifndef`, and a fact in an `#else` judged by the `#if`'s verdict) —
                // and wiring it in here is what finally resolved MSVC's `std::string`: the probe went 2/9 → 7/9.
                //
                // What it also did is make the pinned probe run for **minutes**: `visible_files` walks the whole
                // include graph, and asking the evaluator per guarded include builds a closure state per include
                // (`macros_at`). The libstdc++ closure has hundreds of them. So the rule is right and the
                // *implementation* is not: it needs the incremental state `ProjectIndex::macro_environment` builds
                // (one walk per file, conditions answered as they are reached) before it can be used here — and
                // until then this line stays, because a query that takes minutes is not an answer.
                let step = match include.guard {
                    FactGuard::Unconditional => so_far,
                    // **The condition is asked, and the answer is remembered**. Asking costs the file's
                    // whole closure state; the same question is asked by every query and every walk over hundreds of
                    // edges, and the answer cannot change until a summary is inserted — so it is memoised on the
                    // index (`visibility_answers`), which `insert_at` invalidates for affected files.
                    //
                    // Only `Active` is *used*: a condition that holds makes the include unconditional, and one
                    // nobody can decide leaves the answer exactly where it was (`Conditional`). What `Inactive`
                    // does — dropping the edge — is decided below, where the visibility is turned into a step.
                    FactGuard::Region(region) => {
                        let key = (current.clone(), region);
                        let answer = match self
                            .visibility_answers
                            .lock()
                            .ok()
                            .and_then(|answers| answers.get(&key).copied())
                        {
                            Some(answer) => answer,
                            None => {
                                // **What a condition costs, counted** — the probe that says whether a warm walk is
                                // warm because the memo works or because there was nothing to ask.
                                let started = std::time::Instant::now();
                                let answer = crate::index::environment::visibility_at(
                                    self,
                                    Path::new(&current),
                                    include.guard,
                                    include.range.start_offset,
                                );
                                if crate::probes::timing() {
                                    CONDITION_NANOS.fetch_add(
                                        started.elapsed().as_nanos() as u64,
                                        std::sync::atomic::Ordering::Relaxed,
                                    );
                                    CONDITION_ASKS
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                }
                                if let Ok(mut answers) = self.visibility_answers.lock() {
                                    answers.insert(key, answer);
                                }
                                answer
                            }
                        };

                        match answer {
                            crate::Visibility::Active => so_far,
                            // **A condition that was decided, and not taken**: the file is not part of this
                            // translation unit at all, so nothing in it is visible — the edge is dropped rather
                            // than downgraded, and so is everything only reachable through it.
                            //
                            // This rule was written twice and reverted twice, and the comment that stood here said
                            // why: the *evaluator* was wrong on two corpora (an unrecognised own guard, a fact in an
                            // `#else`), and a rule that can only add facts is the safe direction while that is true.
                            // Both bugs are fixed, so it was re-measured, and the number it is worth is large:
                            // measured on one real file, `#include <arm_neon.h>` and `#include <zmmintrin.h>` —
                            // ARM and AVX-512 intrinsics, gated on `_M_ARM64`/`_M_AVX512`, both decidable and both
                            // **false** for this compilation — were in the visibility list as `Conditional`, and
                            // their names were **8 of every 10 completion items**: 14351 items, 4.5 MB of JSON, for
                            // a cursor in a 78-line file (`zmmintrin.h` 5256, `arm64_neon.h` 2852, `arm_neon.h` 2117).
                            crate::Visibility::Inactive => continue,
                            // A condition nobody can decide: the file may or may not be there, which is exactly what
                            // `Conditional` means to every consumer.
                            crate::Visibility::Unknown => IncludeVisibility::Conditional,
                        }
                    }
                };
                match best.get(&next) {
                    // Already known, and no worse: nothing new to explore through it.
                    Some(known) if *known <= step => continue,
                    Some(_) => {}
                    None => order.push(next.clone()),
                }

                // **An `#include` makes the whole file visible**, however the file was reached before: a file that
                // is both included and imported is not subject to the narrower rule, because the wider one already
                // grants what it would hide.
                through_an_import.remove(&next);

                best.insert(next.clone(), step);
                pending.push((next, step, depth + 1));
            }
        }

        order
            .into_iter()
            .filter_map(|path| {
                best.get(&path)
                    .map(|visibility| (path.clone(), *visibility))
            })
            .collect()
    }

    /// Which declaration a name written in `visible_from` refers to, across the project.
    ///
    /// The cross-file half of [`crate::sema::resolve::definition_at`], and the answer to the
    /// the single-file query returns when a name is somewhere else.
    ///
    /// # The four answers
    ///
    /// * `Yes` — exactly one visible declaration, unconditionally reachable.
    /// * `Unknown(NotDeclaredHere)` — nothing in the project declares it, or nothing that is visible. **Not**
    ///   `No`: the project's index is a subset of what a compiler would see (the standard library, a header
    ///   outside every include path), so "not here" and "nowhere" are still different claims.
    /// * `Unknown(Ambiguous)` — several declarations are visible and nothing chooses between them. Overloads, a
    ///   name declared in two headers the file includes, a bare name declared in two namespaces.
    /// * `Unknown(ConditionalCompilation)` — the only match is reached through an `#include` inside an `#if`, so
    ///   whether it is in scope depends on macros this layer does not have.
    ///
    /// # It is the *one* projection of a plural answer
    ///
    /// [`ProjectIndex::definitions`] is the query; this is the same answer restricted to the names that have
    /// exactly one declaration. A consumer that can show a list — `textDocument/definition` is one — should ask
    /// that instead, because `Ambiguous` is a dead end there: measured on one real file, **46** of its identifiers
    /// answered `Ambiguous`, and every one of them had a perfectly good list behind it (an overload set, a
    /// redeclaration, a namespace).
    pub fn definition(&self, name: &str, visible_from: &Path) -> Known<ProjectDefinition> {
        match self.definitions(name, visible_from) {
            Known::Yes(mut found) if found.found.len() == 1 => Known::Yes(found.found.remove(0)),
            // **A `using`-declaration is a synonym, not a second entity.** C++ says so in as many words: a
            // using-declaration "does not introduce a new declaration; it introduces a synonym". The index stores a
            // fact for it anyway (kind `Other`, the coarsest thing this vocabulary has), so a name that one file
            // *declares* and another *uses* arrives here as two declarations and this query refused with
            // `Ambiguous` — measured on `using std::IsTriviallyCopyConstructible;` over an `enum class` in a header:
            // every hover on the enumerator's qualifier answered `Ambiguous`, and the enumerator itself could then
            // not be looked up at all.
            //
            // Dropping the synonyms is not a guess about which declaration is meant: after the drop either exactly
            // one entity remains (the answer) or the name really does have several, and `Ambiguous` stays honest.
            Known::Yes(mut found) => {
                found
                    .found
                    .retain(|declaration| declaration.fact.kind != DeclKind::Other);
                // **Which *entity* the spelling names.** A class template asked without arguments is the fact with no
                // pattern; `vector<bool>` is a partial specialization with one. Mixing the two is what made a hover on
                // `std::vector` answer `Ambiguous` with four candidates listed: two forward declarations, the primary
                // definition, and `vector<bool, _Alloc>`.
                let group: Vec<usize> = if !name.contains('<') {
                    let plain: Vec<usize> = found
                        .found
                        .iter()
                        .enumerate()
                        .filter(|(_, declaration)| declaration.fact.pattern.is_none())
                        .map(|(at, _)| at)
                        .collect();
                    if plain.is_empty() {
                        (0..found.found.len()).collect()
                    } else {
                        plain
                    }
                } else {
                    (0..found.found.len()).collect()
                };

                // **And a definition beats a forward declaration inside that group** — `struct _Ty;` beside
                // `struct _Ty { … };` are two facts of one entity, and a reader sent to the first sees an empty class
                // where the index holds the whole one. Only one definition in the group chooses; two are two entities.
                let definitions: Vec<usize> = group
                    .iter()
                    .copied()
                    .filter(|at| found.found[*at].fact.definition)
                    .collect();
                if definitions.len() == 1 {
                    return Known::Yes(found.found.remove(definitions[0]));
                }
                if group.len() == 1 {
                    return Known::Yes(found.found.remove(group[0]));
                }
                match found.found.len() {
                    1 => Known::Yes(found.found.remove(0)),
                    _ => Known::Unknown(UnknownReason::Ambiguous(Box::from(name))),
                }
            }
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::No,
        }
    }

    /// **A type name resolved where it was written** — the lookup C++ does and a plain name lookup does not.
    ///
    /// A declaration's type is written in the scope the declaration is in, and C++ looks for an **unqualified**
    /// name there first — in the class, then in what encloses it — before it looks anywhere else. Every other
    /// query in this type is handed a name and a file, and `std::_Allocation_guard::_Alloc_ptr_t` is not what
    /// the header wrote: it wrote `_Alloc_ptr_t`, a **member alias** of the class the declaration sits in.
    ///
    /// Measured on the closure of one file that includes `<format>`, `<vector>` and `<string>`: of 2129 type
    /// spellings the reader could name, 185 named something no lookup answered, and the largest group by far was
    /// this shape — `_Alloc_ptr_t`, `_Alty_traits::pointer`, `_Alloc_size_t`. Not one was a name that is not
    /// there; every one is a member of the class whose member wrote it.
    ///
    /// # Why this is a separate query rather than a change to [`ProjectIndex::definition`]
    ///
    /// Because it *qualifies* the name, and `definition`'s callers ask about names a user wrote at a cursor —
    /// where the enclosing class is already part of the spelling, or is a member access the caller resolved. A
    /// caller that has a written type and the scope it was written in is the caller that wants this, and there
    /// is one way to get it wrong: qualifying a name that is already absolute.
    ///
    /// # Why this walks outward rather than trying one scope
    ///
    /// An unqualified name is looked for **in the innermost scope first and then in each scope around it**, and
    /// the standard library is why one step is not enough: `_Alloc_ptr_t` is used inside `std::_Allocation_guard`
    /// and declared as a free alias template at **namespace scope in `std`** (`xmemory`, "`template <class
    /// _Alloc> using _Alloc_ptr_t = …`"). Asking only the use's own scope answers
    /// `std::_Allocation_guard::_Alloc_ptr_t` — a spelling nothing declares — for a name that is written two
    /// lines up in the same namespace.
    ///
    /// # What it does not do
    ///
    /// It does not substitute template arguments and it does not instantiate: `_Alloc_ptr_t` is
    /// `typename allocator_traits<_Alloc>::pointer`, and **finding the alias is not the same as finishing its
    /// type** — the answer here is the alias's own declaration, which is what a caller needs to say where the
    /// name comes from. Substitution is [`crate::Type::substituted`], driven by a caller that has the arguments,
    /// and nothing drives it yet. Nor does it follow a **base class**: a name inherited from a base is written
    /// unqualified too, and finding it means walking the bases of every class on the chain.
    pub fn definition_where_written(
        &self,
        written: &str,
        in_scope: Option<&str>,
        visible_from: &Path,
    ) -> Known<ProjectDefinition> {
        // The name as written, first. A qualified name, and a name declared at file scope, both answer here —
        // and a name that answers must not be re-asked under a different spelling, because the two answers are
        // different declarations whenever a class has a member of the same name.
        let direct = self.definition(written, visible_from);
        if !matches!(direct, Known::Unknown(UnknownReason::NotDeclaredHere(_))) {
            return direct;
        }

        // `::name` is absolute and takes no scope — that is what the leading `::` is for.
        let Some(scope) = in_scope.filter(|_| !written.starts_with("::")) else {
            return direct;
        };

        // **Outward, one segment at a time.** `std::a::b::name`, then `std::a::name`, then `std::name`, and the
        // bare name has already been tried — which is the order C++ looks in, and the reason a name declared in
        // an enclosing namespace is found from a class nested two deep inside it.
        let segments: Vec<&str> = scope.split("::").collect();
        for take in (1..=segments.len()).rev() {
            let enclosing = segments[..take].join("::");
            if enclosing.is_empty() {
                continue;
            }

            let through_the_scope =
                self.definition(&format!("{enclosing}::{written}"), visible_from);
            if !matches!(
                through_the_scope,
                Known::Unknown(UnknownReason::NotDeclaredHere(_))
            ) {
                return through_the_scope;
            }
        }

        // **The original reason, not the last attempt's.** Returning `NotDeclaredHere` for a spelling the caller
        // never wrote would have a consumer explaining a name the user cannot find in their file.
        direct
    }

    /// **Every declaration a name refers to** — the plural answer, for a consumer that can show a list.
    ///
    /// # Why a list, and why it is not a preference
    ///
    /// One name covering several entities is ordinary C++, and the language says what they are: `find` in
    /// `std::basic_string` is one **overload set** (seventeen declarations in MSVC's library, every one of them an
    /// answer to "where is this declared"), `std::char_traits` is a class template and its specializations, and
    /// `cin` is written twice in `<iostream>` — one variable, declared twice, which the language calls the same
    /// entity. `Ambiguous` is the honest answer to *which one*, and it is a dead end for a client; the protocol
    /// has a shape for the honest answer, so this is it.
    ///
    /// # The one case that is collapsed, and why that is not taste
    ///
    /// When **every** candidate is a namespace, there is exactly one entity: a namespace is reopened by every file
    /// that writes `namespace std { … }`, and the language says all of those are the same namespace. Measured on
    /// the file that motivated this query: `std` has **58** declarations in the index, so a peek list of
    /// fifty-eight entries answers a question nobody asked — while `size`'s seventeen and `cin`'s two are exactly
    /// what a reader wants to see. The collapse is therefore about the *kind* of entity, not about the count.
    ///
    /// # The order, which is part of the answer
    ///
    /// Unconditional first, then by file and offset. Imposed rather than inherited: `summaries()` is a map, so
    /// leaving the order alone would make the list depend on hashing, and a client's peek list would reorder
    /// itself between two identical requests.
    pub fn definitions(&self, name: &str, visible_from: &Path) -> Known<ProjectDefinitions> {
        // **A measurement switch, not a behaviour**: what a diagnostics pull costs *without* the
        // index lookups — the sibling of `CPPLS_NO_INFERENCE`, and it exists to tell "the engine"
        // from "the index" before either is optimised.
        if crate::probes::no_index_lookup() {
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(name)));
        }
        // **One answer per spelling per pull** — see [`ProjectIndex::definition_answers`]. Measured on
        // `LuaSource.cpp`: 95 of one pull's 99 slow lookups were the same name (`std`), at 0.6–1.3 ms each.
        let key = (name.to_string(), normalize(visible_from));
        if let Some(known) = self
            .definition_answers
            .lock()
            .ok()
            .and_then(|memo| memo.get(&key).cloned())
        {
            return known;
        }
        let started = std::time::Instant::now();
        let answer = self.definitions_inner(name, visible_from);
        if crate::probes::timing() && started.elapsed().as_micros() >= 200 {
            eprintln!(
                "[lookup] {:>5.1} ms  {name}  ({} answer(s))",
                started.elapsed().as_secs_f64() * 1000.0,
                match &answer {
                    Known::Yes(found) => found.found.len(),
                    _ => 0,
                }
            );
        }
        if let Ok(mut memo) = self.definition_answers.lock() {
            if memo.len() >= MAX_DEFINITION_ANSWERS {
                memo.clear();
            }
            memo.insert(key, answer.clone());
        }
        answer
    }

    pub(crate) fn definitions_inner(
        &self,
        name: &str,
        visible_from: &Path,
    ) -> Known<ProjectDefinitions> {
        let (mut certain, conditional) = match self.certain_declarations(name, visible_from) {
            Ok(candidates) => candidates,
            Err(reason) => return Known::Unknown(reason),
        };

        // **A namespace is asked about more than any other name, and it is the cheapest question in the index.**
        // `std` is written in the spelling of every qualified name in a file: measured on `LuaSource.cpp`, **95 of
        // the 99 slow declaration lookups in one diagnostics pull were `std`**, at 1.3 ms each — because the
        // deduplication below is quadratic in the candidates, and MSVC declares `namespace std` in a hundred
        // headers. The namespace rule collapses all of them to one answer *whatever* their identity, so apply it
        // first and skip the quadratic pass: same answer, 1.3 ms → 0.03 ms.
        let all_namespaces = certain
            .iter()
            .all(|found| found.fact.kind == DeclKind::Namespace);
        if all_namespaces {
            certain.truncate(1);
        }

        // **One entity, or two answers?** The measurement this rests on — 19 ambiguous identifiers in one real
        // file, their candidates paired up — produced **97 pairs that stand in the same file and agree on every
        // field a consumer can read**, against **0 across files**: the duplicates are *inside* one header (MSVC's
        // `<iostream>` writes `cin` plain and again behind `_EXPORT_STD`), while two files declaring one name are
        // two places a reader may want to go. See [`the_same_declaration_in_one_file`] for the conditions.
        let mut distinct: Vec<ProjectDeclaration> = Vec::new();
        for found in certain {
            if let Some(at) = distinct
                .iter()
                .position(|kept| the_same_declaration_in_one_file(kept, &found))
            {
                // **A definition beats a forward declaration written earlier in the same file** — the shape MSVC's
                // standard library uses: `struct _Ty;` near the top of a header and `struct _Ty { … };` further down.
                // Keeping the first sent a reader, and every query built on `definition`, to the empty one.
                if found.fact.definition && !distinct[at].fact.definition {
                    distinct[at] = found;
                }
                continue;
            }
            distinct.push(found);
        }
        let mut certain = distinct;

        // **A `using`-declaration is a synonym, not a second entity.** C++ says so in as many words: it "does not
        // introduce a new declaration". The index stores a fact for it anyway (kind `Other`, the coarsest thing this
        // vocabulary has), so a name that one file declares and another uses arrives as two declarations — measured
        // on `using std::IsTriviallyCopyConstructible;` over an `enum class` in a header, where every hover on the
        // enum's name answered `Ambiguous` and the enumerator's qualified lookup died behind it.
        //
        // The synonyms go **only when a real declaration remains**: a name that is nothing but a using-declaration
        // still points a reader at that line, which is better than "not declared".
        if certain
            .iter()
            .any(|found| found.fact.kind != DeclKind::Other)
        {
            certain.retain(|found| found.fact.kind != DeclKind::Other);
        }

        Known::Yes(ProjectDefinitions {
            found: certain
                .into_iter()
                .map(|found| ProjectDefinition {
                    file: found.file,
                    fact: found.fact,
                })
                .collect(),
            conditional,
        })
    }

    /// **Every declaration a spelling can denote where it is written** — [`ProjectIndex::definitions`] narrowed by
    /// the one rule the index cannot put in its key: **a bare name does not reach into a class**.
    ///
    /// # The defect this answers
    ///
    /// Measured on a two-file project — a header declaring `struct Name { void printf(int); };` and
    /// `void Name::printf(int i) { … }`, and a `.cpp` that includes it and calls `printf("…")` — go-to-definition on
    /// `printf` in the `.cpp` answered **three** declarations: the member's declaration, its out-of-class definition,
    /// and `stdio.h`'s `printf`. The first two are a function the `.cpp` cannot name: `printf` written unqualified at
    /// file scope denotes the one at file scope, and a member is denoted by a member access or by a qualified name.
    ///
    /// The index keys a fact by its **name** ([`DeclFact::name`]), so every `printf` in every included header is a
    /// candidate; C++ keys it by the **scope it is written in**. This query is the second half of the key.
    ///
    /// # The rule, and the two directions it must not go
    ///
    /// * A fact whose [`DeclFact::scope`] is a **class** is dropped from a **bare** spelling's answer. A qualified
    ///   spelling (`Name::printf`) keeps it — that is how a member is named from outside — and so does a caller that
    ///   already knows it is looking at a member ([`ProjectIndex::definitions`], which this one is a filter over).
    /// * A scope **nothing in the index declares as a class** keeps its members: `false` there means "I cannot
    ///   tell", and a lookup that cannot tell must not become a lookup that says there is nothing.
    ///
    /// `Unknown(NotDeclaredHere)` when the scope rule leaves nothing, which is the same answer as a name that is not
    /// declared here — and it is the honest one, because no declaration the caller can write reaches the name.
    pub fn definitions_in_scope(
        &self,
        name: &str,
        visible_from: &Path,
    ) -> Known<ProjectDefinitions> {
        // The reason is passed through as it stands: this method has nothing to add to `NotDeclaredHere`,
        // `Ambiguous` or `ConditionalCompilation`, and re-asking the query to keep the value would run a visibility
        // walk twice for an answer that is already in hand.
        let mut found = match self.definitions(name, visible_from) {
            Known::Yes(found) => found,
            other => return other,
        };

        // A qualified spelling names the scope it writes, so the key is already as narrow as the language's.
        if name.contains("::") {
            return Known::Yes(found);
        }

        // **One lookup per distinct scope**, not per fact: an overload set is several facts in one scope, and a
        // visibility walk per candidate would be paid once per overload rather than once per class.
        let mut decided: Vec<(String, bool)> = Vec::new();
        let mut kept = Vec::with_capacity(found.found.len());

        for declaration in found.found {
            let Some(scope) = declaration.fact.scope.clone() else {
                // File scope: exactly where a bare name looks.
                kept.push(declaration);
                continue;
            };

            let a_class = match decided.iter().find(|(known, _)| *known == scope) {
                Some((_, answer)) => *answer,
                None => {
                    let answer = self.scope_names_a_class(&scope, visible_from);
                    decided.push((scope.clone(), answer));
                    answer
                }
            };

            if !a_class {
                kept.push(declaration);
            }
        }

        if kept.is_empty() {
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(name)));
        }

        found.found = kept;
        Known::Yes(found)
    }

    /// **Does the scope a qualified spelling names hold declarations as a class?** — the question
    /// [`ProjectIndex::definitions_in_scope`] asks before it drops a member.
    ///
    /// The scope's own declaration is what answers it, and the comparison is on the **qualified** name rather than
    /// on [`matches`]'s looser rule: `files_declaring("Name")` also answers a *nested* class called `Name` (its
    /// fact's `name` is `Name` and its scope is `Outer`), and taking that for a class called `Name` would hide the
    /// members of a namespace that happens to have a same-named class somewhere inside it.
    pub(crate) fn scope_names_a_class(&self, scope: &str, visible_from: &Path) -> bool {
        self.files_declaring(scope, visible_from)
            .iter()
            .any(|declaration| {
                declaration.fact.kind == DeclKind::Type
                    && declaration.fact.qualified_name() == scope
            })
    }

    /// **What kind of thing a name is, when every declaration the index holds for it agrees** — and
    /// [`UnknownReason::Ambiguous`] when they do not.
    ///
    /// The question a semantic highlighter asks about a name it cannot resolve to one declaration: it has to draw a
    /// colour, and a colour is a claim about the *kind*. An overload set is one kind (`pick(int)` and `pick(double)`
    /// are both functions, and a reader who sees both drawn as functions has been told the truth about both), while
    /// `size` as a member of one class and a free function somewhere else is genuinely two answers, and a client
    /// shown either one of them has been told something false about the other.
    ///
    /// This is not [`ProjectIndex::definition`] with a looser rule — it is the same candidate set, read for the two
    /// fields a highlighter needs instead of cloned whole into an answer a jump would use. See
    /// [`crate::semantic::classified_names`], which is its only caller.
    pub fn kind_of(&self, name: &str, visible_from: &Path) -> Known<IndexedKind> {
        let (certain, _) = match self.certain_declarations(name, visible_from) {
            Ok(candidates) => candidates,
            Err(reason) => return Known::Unknown(reason),
        };

        // **A member's declaration cannot be what a bare spelling means.** `class TextRange { TextRange(); … }` files
        // three facts whose bare name is `TextRange` — the class, and its two constructors — so this question came
        // back `Ambiguous`, and the *token* `TextRange` then got no colour at all.
        //
        // Measured on EmmyLuaCodeStyle's `LuaParser/include/LuaParser/Ast/LuaSyntaxNode.h`: `LuaSyntaxTree` was
        // classified `Type` and `TextRange` was not, and the difference was exactly this — `LuaSyntaxTree` is only
        // forward-declared in that header, while `TextRange`'s class declares constructors. `files_declaring` takes
        // the **bare** name, so the constructors — whose qualified name is `TextRange::TextRange` — were counted as
        // other meanings of it.
        //
        // A fact whose own qualified name is longer than the one asked about is a member, and a qualified request
        // (`TextRange::StartOffset`) still matches: its qualified name is the name asked for.
        // **An enumerator is not a member for this question.** An unscoped enumeration's enumerators are visible in
        // the enclosing scope — `TK_ELSEIF` is written bare in `case TK_ELSEIF:` — and a scoped one's are what
        // `Color::Red` names with the qualifier dropped by the token being coloured. Their facts have a scope (the
        // enumeration's) and are not members of a class, so the rule below must not drop them: measured on
        // `EmmyLuaCodeStyle`, `TK_ELSEIF` declared in `LuaTokenTypeDetail.h` was filtered out here and the use in
        // `SpaceAnalyzer.cpp` got **no token at all** (`0 classification(s)`, and the editor drew it as
        // `variable.other.unknown`).
        let certain: Vec<_> = certain
            .into_iter()
            .filter(|found| {
                found.fact.scope.is_none()
                    || found.fact.qualified_name() == name
                    || IndexedKind::of(&found.fact).enumerator
            })
            .collect();

        let mut kinds: Vec<IndexedKind> = certain
            .into_iter()
            .map(|found| IndexedKind::of(&found.fact))
            .collect();
        kinds.sort_unstable();
        kinds.dedup();

        match kinds.as_slice() {
            [only] => Known::Yes(only.clone()),
            [] => Known::Unknown(UnknownReason::ConditionalCompilation),
            _ => Known::Unknown(UnknownReason::Ambiguous(Box::from(name))),
        }
    }

    /// The declarations the index holds for `name` and `visible_from` can see: the ones that are certainly in scope,
    /// in the order every declaration answer reports in, plus a **count** of the ones reachable only through a
    /// conditional `#include`.
    ///
    /// The walk [`ProjectIndex::definitions`] and [`ProjectIndex::kind_of`] share. It is a function rather than two
    /// copies because the rule it applies — a declaration the file writes itself wins over one it includes, ordered
    /// by file then offset, with the conditional ones counted rather than offered — is the part of a definition
    /// answer that is easy to get subtly different in two places.
    /// **Are these two declarations one entity** — a function declared in a header and defined in the file that includes
    /// it — rather than two things that share a spelling?
    ///
    /// Only a function, and only when the parameter list agrees: `f(void)` in a header and `f(void)` in the file are one
    /// function, while `Widget` in a header and `Widget` in the file are two types. The comparison is of what the facts
    /// record, so it is an answer about spellings rather than about types — a parameter list written differently
    /// (`(int n)` against `(int)`) reads as two entities, which keeps the previous behaviour for that case.
    pub(crate) fn one_entity_declared_twice(
        left: &VisibleDeclaration<'_>,
        right: &VisibleDeclaration<'_>,
    ) -> bool {
        left.fact.kind == DeclKind::Function
            && right.fact.kind == DeclKind::Function
            && left.fact.name == right.fact.name
            && left.fact.scope == right.fact.scope
            && left.fact.parameter_list == right.fact.parameter_list
            // **Only across files.** The rule this serves is "a header declares, the `.cpp` defines" — and inside one file
            // two declarations with the same parameter list can be two *overloads*: the two `describe`s differ only in
            // their `enable_if` condition, which is in the return type and therefore in no field this compares.
            // Measured: `describe(1.5)` answered the first declaration, because the second had been folded into it.
            && left.file != right.file
    }

    /// **Ask for a file's reading to be refined**, because a query could not answer from it.
    ///
    /// The trigger is the one shape this index can *see*: a **qualified** name that found nothing while its bare name
    /// exists **with no scope**. That is what a header looks like before it has been read through its macros — MSVC's
    /// `<xmemory>` declares `allocator_traits` inside `_STD_BEGIN`, so the bare fact is filed at file scope and
    /// `std::allocator_traits` does not exist until the file is re-read with the body in force.
    ///
    /// Measured on `EmmyLuaCodeStyle` with `<vector>` refined and `<xmemory>` not: `auto pos = _events.size();`
    /// answered `_Alty_traits::size_type`, because `allocator_traits` had no members to look `size_type` up in.
    ///
    /// Idempotent and cheap — a set insert. The session decides when to serve it; see [`ProjectIndex::take_refinement_asks`].
    pub fn ask_for_refinement(&self, path: impl Into<std::path::PathBuf>) {
        if let Ok(mut asked) = self.refinement_asks.lock() {
            asked.insert(path.into());
        }
    }

    /// **Take the files a query asked for**, emptying the set — the session's half of
    /// [`ProjectIndex::ask_for_refinement`].
    pub fn take_refinement_asks(&self) -> Vec<std::path::PathBuf> {
        self.refinement_asks
            .lock()
            .map(|mut asked| asked.drain().collect())
            .unwrap_or_default()
    }

    pub(crate) fn certain_declarations(
        &self,
        name: &str,
        visible_from: &Path,
    ) -> Result<(Vec<ProjectDeclaration>, usize), UnknownReason> {
        let mut candidates = self.files_declaring(name, visible_from);

        // **A member spelled through an alias.** `std::string::size_type` is not the qualified name of anything
        // the index holds — `size_type` is a member of `basic_string`, and `std::string` is a `using`
        // declaration for it — so the literal lookup finds nothing while the declaration is right there.
        // Measured as a pair, which is what says this is the missing half rather than member resolution being
        // absent altogether:
        //
        // ```text
        // std::basic_string::size_type   -> Yes            member resolution works
        // std::string::size_type         -> NotDeclaredHere   the alias is what does not
        // ```
        //
        // Tried **only after the literal name found nothing**, so a name that resolves today cannot change its
        // answer — the whole change is additive, which matters for a query this many callers share.
        if candidates.is_empty()
            && let Some(rewritten) = self.through_an_alias(name, visible_from)
        {
            candidates = self.files_declaring(&rewritten, visible_from);
        }

        // **A qualified name that failed while its bare name exists with no scope is a header that has not been read
        // through its macros.** `std::allocator_traits` finds nothing when `<xmemory>` was indexed plainly — the class
        // is there, filed at file scope, because the header opens `std` with `_STD_BEGIN` and the plain reading has no
        // body to expand. Refining that one file is what makes the name exist, and it is a walk only the pump may
        // spend, so this asks for it and the answer stays `NotDeclaredHere` until the pump has run.
        //
        // Asked only for the *qualified* spelling: the bare name answering is the normal case for a header that has
        // not been refined, and asking on every miss would turn each lookup of an unknown name into an ask.
        if candidates.is_empty()
            && let Some((_, bare)) = split_at_the_last_scope(name)
        {
            for found in self.files_declaring(&bare, visible_from) {
                if found.fact.scope.is_none() {
                    self.ask_for_refinement(found.file);
                }
            }
        }

        if candidates.is_empty() {
            return Err(UnknownReason::NotDeclaredHere(Box::from(name)));
        }

        // A declaration the file itself writes wins over one it includes, and is the one a reader means: a
        // header's `Widget` and this file's `Widget` are different entities, and C++ resolves to the local one.
        // Overloads of one name written *here* are still several declarations, and they are all answers.
        let own = normalize(visible_from);
        if candidates.iter().any(|found| normalize(&found.file) == own) {
            // **Except one function the header declares and this file defines.** Those are one *entity*, and the
            // header's declaration is the one that makes a use **above** the definition legal — the shape a C file has
            // when its declarations live in a header: `corpus.h` declares `functions_selector` at the top, the call is
            // at line 61, the definition at line 182. Dropping the header's fact left the call with no declaration
            // before it, and `undefined-name` reported it. Measured on a nine-file C corpus: two findings, and
            // reproduced in `tests/checks.rs` with the declaration in a header.
            //
            // The rule above still holds for everything else: a header's `Widget` and this file's `Widget` are two
            // entities, and only a *function* whose parameter list agrees is the same one written twice.
            let local: Vec<VisibleDeclaration<'_>> = candidates
                .iter()
                .filter(|found| normalize(&found.file) == own)
                .cloned()
                .collect();
            candidates.retain(|found| {
                normalize(&found.file) == own
                    || local
                        .iter()
                        .any(|kept| Self::one_entity_declared_twice(kept, found))
                    // **A name in another scope is another entity.** The rule above is about one name written in one
                    // scope in two files; applied to every scope at once, a *member* declared here hid the *class* of
                    // the same name declared in the header. Measured on `LSPHandle.cpp`: `definition("LSPHandle")`
                    // answered `Ambiguous` between two constructors — the header's and this file's own
                    // `LSPHandle::LSPHandle(LanguageServer *)` — while the class's `Type` fact, which the index holds
                    // (`every_fact_named` finds it at `LSPHandle.h:300`), was dropped as "not local". A reader's
                    // `LSPHandle::RefreshDiagnostic` then resolved its qualifier to a constructor, which is the
                    // report "`LSPHandle` was read as the constructor itself rather than the class".
                    //
                    // The class is at file scope (`scope: None`) and the local constructor is at `LSPHandle`'s, so
                    // the two are not competing declarations of one entity; keeping the out-of-scope candidate is the
                    // paragraph above's argument, one level in.
                    || !local
                        .iter()
                        .any(|kept| kept.fact.scope == found.fact.scope)
            });
        }

        // **A declaration in a branch this compilation does not take is not an answer.** libstdc++ declares
        // `_M_data_ptr` twice — `value_type*` under `#else` and `pointer_traits<_Ptr>::element_type*` under
        // `#if __cplusplus >= 201103L` — and `insert` the same way, so both branches arrived as answers and a hover could
        // name the one a compiler never reads (measured: `facts_vs_rendering` counted five such duplicates over
        // `stl_vector.h` and `stl_bvector.h`).
        //
        // The question is not the file's own `#if` but the macros **at that offset**: `visibility_at` walks them, and
        // `Unknown` keeps the declaration — a guard this layer cannot decide must not become a lookup that says the name
        // is not declared.
        //
        // **Skippable by a probe switch, and that is how its remaining work is measured.** `CPPLS_NO_GUARD_FILTER`
        // answers without this step, so the two answers can be compared over the same questions: with a rendering for
        // most files (⑱ makes the rendering the reading) this filter should have almost nothing left to remove, and
        // "almost nothing" is a number this crate's style asks for rather than a claim. See `tests/patch_probe.rs`.
        if !crate::probes::no_guard_filter() {
            candidates.retain(|found| {
                self.a_guard_is_taken(&found.file, found.fact.guard, found.fact.range.start_offset)
            });
        }
        // scope depends on macros this layer does not have, and a jump to a declaration that may not be there is a
        // wrong answer rather than a missing one. The count is what keeps the answer from claiming there is
        // nothing else — "one declaration, and two that might be" is not "one declaration".
        let conditional = candidates
            .iter()
            .filter(|found| found.visibility == IncludeVisibility::Conditional)
            .count();

        let mut certain: Vec<ProjectDeclaration> = candidates
            .into_iter()
            .filter(|found| found.visibility == IncludeVisibility::Unconditional)
            .map(|found| ProjectDeclaration::of(&found))
            .collect();

        if certain.is_empty() {
            return Err(UnknownReason::ConditionalCompilation);
        }

        certain.sort_by(|one, other| {
            one.file
                .cmp(&other.file)
                .then_with(|| one.start_offset.cmp(&other.start_offset))
        });

        Ok((certain, conditional))
    }

    /// **The qualified name a member has when it was spelled through an alias** — see
    /// [`ProjectIndex::certain_declarations`], which asks this only when the literal name found nothing.
    ///
    /// `std::string::size_type` becomes `std::basic_string::size_type`, and the lookup is tried again. The walk
    /// is over the name's **prefixes, longest first**: the alias may be at any position (`a::b::c::d` where `b`
    /// is the alias), the last segment is the name being looked for and replacing *it* is what the caller has
    /// already tried, and an inner alias is the one that decides what the outer name means.
    ///
    /// The target is spelled **relative to the scope the alias was written in**, which is where C++ looks for
    /// it: `using string = basic_string<char, …>` inside `namespace std` names `std::basic_string`. A target the
    /// alias spelled with a qualification of its own (`::other::Thing`) is already absolute and is used as it
    /// stands.
    ///
    /// `None` when no prefix is an alias, or when the chain does not settle — an alias that leads back to
    /// itself is not legal C++, but a bound is cheaper than proving there is none, and a query that loops is an
    /// editor that stops answering.
    pub(crate) fn through_an_alias(&self, name: &str, visible_from: &Path) -> Option<String> {
        /// How many substitutions one name may go through before the answer is "not this way".
        const ALIAS_DEPTH: usize = 4;

        let mut rewritten = name.to_string();
        let tracing = std::env::var("CPPLS_TRACE_ALIAS").is_ok();

        for _ in 0..ALIAS_DEPTH {
            let segments: Vec<&str> = rewritten.split("::").collect();
            if tracing {
                eprintln!("[alias] trying {rewritten}");
            }

            // The last segment is excluded: it is the name being looked for, and substituting it would be
            // answering a different question from the one the caller asked.
            let substituted = (1..segments.len()).rev().find_map(|take| {
                let prefix = segments[..take].join("::");
                // **The entity the prefix names**, preferring a declaration over a synonym: a file that writes both
                // `using std::E;` and (through the header) the `enum class E` has two facts for `E`, and the
                // synonym is the one that says nothing about where `E` really lives.
                let mut candidates = self.files_declaring(&prefix, visible_from);
                candidates.sort_by_key(|found| found.fact.kind == DeclKind::Other);
                if tracing {
                    eprintln!(
                        "[alias] prefix {prefix}: {:?}",
                        candidates
                            .iter()
                            .map(|found| format!(
                                "{} {:?} scope {:?} type {:?}",
                                found.fact.name,
                                found.fact.kind,
                                found.fact.scope,
                                found.fact.type_of
                            ))
                            .collect::<Vec<_>>()
                    );
                }
                let alias = candidates.into_iter().next()?;

                // A prefix that names the entity **itself** — an `enum class` reached through a `using`, a class
                // reached through a typedef from a namespace — is rewritten to the entity's own qualified name, so
                // the member is looked for in the scope that actually declares it. This is the half that was
                // missing: only *aliases* were followed (`type_of` is an alias's target), while a `using`-declared
                // enum has no target at all and its `qualified_name` is the only spelling its members are stored
                // under.
                if let Some(qualified) = self.qualified_name_of_the_entity(alias.fact, &prefix)
                    // **A rewrite that changes nothing is not a rewrite.** The entity's own qualified name *is*
                    // the prefix for an ordinary **type alias** — `using json = basic_json<>;` in `namespace
                    // nlohmann` gives `nlohmann::json` back unchanged — and returning it here skipped the alias arm
                    // below, so `nlohmann::json::object` was retried as `nlohmann::json::object` and answered
                    // `NotDeclaredHere` while `members_of(nlohmann::json)` listed the member. Measured on
                    // `EmmyLuaCodeStyle`'s `LSP.cpp:12`, `auto object = nlohmann::json::object();`: the call's type
                    // was `UnknownType`, and both the hover and go-to-definition stopped at the alias.
                    && qualified != prefix
                {
                    return Some(format!("{qualified}::{}", segments[take..].join("::")));
                }

                // **A namespace alias** — `namespace views = ranges::views;`, which is how libstdc++ spells
                // `std::views`. Its fact is a `Namespace` whose `type_of` is the target, so it is neither a type alias
                // (the case below) nor an entity with a qualified name of its own (the case above): the target is the
                // rewrite. Measured: `std::views::filter` answered `NotDeclaredHere` while `std::ranges::views::filter`
                // was declared in `<ranges>` all along.
                if alias.fact.kind == DeclKind::Namespace
                    && let Some(written_target) = alias.fact.type_of.as_deref()
                {
                    let qualified = if written_target.contains("::") {
                        written_target.to_string()
                    } else {
                        match &alias.fact.scope {
                            Some(scope) => format!("{scope}::{written_target}"),
                            None => written_target.to_string(),
                        }
                    };
                    if qualified != prefix {
                        return Some(format!("{qualified}::{}", segments[take..].join("::")));
                    }
                }

                if alias.fact.kind != DeclKind::Type {
                    return None;
                }
                let target =
                    crate::sema::types::parse_type_spelling(alias.fact.type_of.as_deref()?);
                let class = target.class_name()?;

                let qualified = if class.contains("::") {
                    class.to_string()
                } else {
                    match &alias.fact.scope {
                        Some(scope) => format!("{scope}::{class}"),
                        None => class.to_string(),
                    }
                };

                Some(format!("{qualified}::{}", segments[take..].join("::")))
            });

            let next = substituted?;
            if next == rewritten {
                return None;
            }
            rewritten = next;

            if !self.files_declaring(&rewritten, visible_from).is_empty() {
                return Some(rewritten);
            }
        }

        None
    }

    /// **The qualified name of the entity a spelling names**, when that is not the spelling itself.
    ///
    /// `E` for an `enum class` declared in `std` and reached through `using std::E;` is the measured case: the
    /// spelling the file writes has no `std::`, while every member of the enum is stored under
    /// `std::E::…`. `None` when the fact's own qualified name *is* the spelling (an ordinary name, which is most
    /// of them) — the caller has nothing to rewrite then, and rewriting to the same string only costs a lookup.
    ///
    /// Only a declaration that can **own** members is rewritten: a type or a namespace. A variable or a function
    /// reached through a `using` names no scope, and a rewrite built from it would be a spelling nothing declares.
    pub(crate) fn qualified_name_of_the_entity(
        &self,
        fact: &DeclFact,
        prefix: &str,
    ) -> Option<String> {
        if !matches!(fact.kind, DeclKind::Type | DeclKind::Namespace) {
            return None;
        }

        let qualified = fact.qualified_name();
        if qualified == prefix || qualified.is_empty() {
            return None;
        }
        // **The tail has to be the same name.** `using std::E;` introduces `E`, and the entity's trailing segment is
        // `E`; a fact whose tail differs is a rewrite that changes what is being looked for.
        let tail = qualified.rsplit("::").next().unwrap_or_default();
        let written = prefix.rsplit("::").next().unwrap_or_default();
        (tail == written).then_some(qualified)
    }

    /// What the macro name `name` is at `offset` in the file at `visible_from`.
    ///
    /// # Translation order is the whole answer
    ///
    /// A preprocessor reads a translation unit as one stream: the file's own lines, with each `#include` replaced
    /// by the whole of the file it names — recursively — and the macros in force at a point are decided by the
    /// **last** `#define` or `#undef` of that name to have gone past. So the position of a fact is not an offset
    /// but a **chain** of them: the `#include` that pulled its file in, then the `#include` inside *that* file, and
    /// so on, ending with the fact's own offset. Comparing two chains lexicographically *is* comparing where they
    /// came in the stream, which is why this needs no separate rules for "written here" and "written in a header":
    ///
    /// ```text
    /// a.cpp:   #define MAX 1        position [0]         -> the header's wins
    /// a.cpp:   #include "x.h"       position [10, 4]
    /// a.cpp:   #include "x.h"                          -> the local one wins
    /// a.cpp:   #define MAX 1         position [0]
    /// ```
    ///
    /// # The four answers
    ///
    /// * `Yes` — the last fact in that order is a `#define`.
    /// * `Unknown(UndefinedHere)` — it is an `#undef`. Not `No`, and not a pointer at the `#define` it used to
    ///   have: a name that was undefined above the cursor is an ordinary identifier, and a jump to a definition
    ///   that is no longer in force would be a wrong answer rather than a missing one.
    /// * `Unknown(ConditionalCompilation)` — every fact that could decide it is inside an `#if`, or reached
    ///   through an `#include` that is. See below for the rule that keeps this from swallowing the common case.
    /// * `Unknown(NotDeclaredHere)` — nothing in the index touches the name. **Not** "there is no such macro":
    ///   the index holds the files it has been asked about, and a macro defined in a header nobody indexed looks
    ///   exactly like a name that is not a macro at all.
    ///
    /// # Why the answer prefers the unconditional fact
    ///
    /// The same rule [`ProjectIndex::definition`] uses, for the same reason. An unconditional `#define` above a
    /// *guarded* one is what the name certainly is; reporting `ConditionalCompilation` instead would refuse to
    /// answer a question that has an answer. Only when nothing unconditional is in the running does the answer
    /// become `Unknown` — and the residual uncertainty is stated rather than hidden: a guarded `#undef` *after* an
    /// unconditional `#define` is treated as not having happened.
    pub fn macro_definition(
        &self,
        name: &str,
        visible_from: &Path,
        offset: usize,
    ) -> Known<ProjectMacro> {
        match self.macro_environment(name, visible_from).at(offset) {
            Known::Yes(found) => Known::Yes(ProjectMacro {
                file: found.file.clone(),
                fact: found.fact.clone(),
            }),
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::No,
        }
    }

    /// Everything this file can know about one macro name, **computed once** and then asked about any offset.
    ///
    /// [`ProjectIndex::macro_definition`] is one walk of the include graph per call, which is the right shape for a
    /// query about one cursor. It is the wrong shape for a query about **thousands** of positions in the same file,
    /// and that is not a hypothetical: `macro_references` asked it once per hit and took **3.8 s** on
    /// `STDMETHODCALLTYPE` in the standard-library closure — 4 079 hits, each walking the same graph, to produce an
    /// answer whose every input was identical. This is that walk, done once per file, with the offset applied to the
    /// result instead of to the search.
    ///
    /// The cost is one traversal of the reachable files per caller, so a caller that asks about one position should
    /// use [`ProjectIndex::macro_definition`] and one that asks about many should hold this.
    pub fn macro_environment(&self, name: &str, visible_from: &Path) -> MacroEnvironment {
        // **A name nothing defines cannot be a macro, and that is the answer most hovers want.** This walk is a
        // traversal of every file the cursor can reach — **1.0 s** on `EmmyLuaCodeStyle` for `LSPHandle.cpp`'s
        // standard-library closure — and a hover asks it about *every* name: a local, a member, a class, a `std::`
        // type. The index already holds each file's `#define`/`#undef` by name, and the compilation's own table
        // (the `-D`s and the compiler's predefines) holds the rest, so a name neither of them mentions is not a macro
        // in any file and the walk can only return nothing. Two lookups instead of a closure walk.
        //
        // **A file the index has not read yet is the one case this can be wrong about**, and it degrades rather than
        // lies: the answer is "no macro here", which is the same answer the walk would give for an unread file, and the
        // name questions below it still answer. The macro the unread file defines comes back when the index reaches it
        // — the memo is dropped for the file it arrives from.
        if self.names.definers_of(name).is_empty() && !self.macros.names_the_compilation(name) {
            return MacroEnvironment {
                name: name.to_string(),
                candidates: Vec::new(),
            };
        }
        // **The walk is remembered, because it is the whole cost and it is asked again.** Measured on neovim:
        // 1.19 s per walk, three identical walks in a row, and every repeated question paid it again — see
        // [`ProjectIndex::macro_environments`] for the numbers and for the invalidation rule.
        let key = (normalize(visible_from), name.to_string());
        if let Ok(memo) = self.macro_environments.lock()
            && let Some(cached) = memo.get(&key)
        {
            return cached.environment.clone();
        }

        let mut candidates = Vec::new();
        let certain = self.certainly_in_the_translation_unit(visible_from);
        // The walk's own visited set is the contributor list — the same evidence [`ProjectIndex::visibility`] keeps.
        let mut visited = HashSet::new();

        self.macro_candidates(
            visible_from,
            &mut Vec::new(),
            false,
            &mut visited,
            &certain,
            &mut Collecting::Name(name, &mut candidates),
            &mut self.macros.clone(),
            None,
        );

        let environment = MacroEnvironment {
            name: name.to_string(),
            candidates,
        };

        let mut contributors: Vec<String> = visited.into_iter().collect();
        contributors.sort();
        if let Ok(mut memo) = self.macro_environments.lock() {
            if memo.len() >= MAX_CACHED_MACRO_WALKS {
                memo.clear();
            }
            memo.insert(
                key,
                CachedMacroWalk {
                    contributors,
                    environment: environment.clone(),
                },
            );
        }

        environment
    }

    /// **Every macro name this file's environment defines**, from **one** walk of the include graph.
    ///
    /// # The measurement this exists for
    ///
    /// The colouring pass asks "is this spelling a macro" once per identifier it cannot otherwise classify, and it
    /// used to ask [`ProjectIndex::macro_environment`] — one whole walk of the graph — **per distinct spelling**.
    /// Measured on this repository's own `codeformat.h`: `semanticTokens` answered **521 tokens in 526 ms**, and the
    /// walk was essentially all of it (a header mentions a few hundred names nothing in the file declares). One walk
    /// that collects the names answers every one of them from a hash lookup.
    ///
    /// The precision is the same as the per-name form for the case that reaches here: a macro an `#include` brought in
    /// is in force for the whole file, and a `#define` the file writes itself is answered by
    /// [`crate::semantic`]'s own step 2, which is already whole-file. An `#undef` of an *included* macro is the one
    /// shape this cannot express, and it is not expressible in step 2 either.
    pub fn macro_names_in_force(&self, visible_from: &Path) -> HashSet<String> {
        // The same memo as [`ProjectIndex::macro_environment`], keyed by the file alone: the colouring pass asks for
        // the whole set once per file and it is the one question that needs the closure.
        let key = normalize(visible_from);
        if let Ok(memo) = self.macro_names.lock()
            && let Some(cached) = memo.get(&key)
        {
            return cached.names.clone();
        }

        let mut names = HashSet::new();
        let certain = self.certainly_in_the_translation_unit(visible_from);
        let mut visited = HashSet::new();

        self.macro_candidates(
            visible_from,
            &mut Vec::new(),
            false,
            &mut visited,
            &certain,
            &mut Collecting::Names(&mut names),
            &mut self.macros.clone(),
            None,
        );

        let mut contributors: Vec<String> = visited.into_iter().collect();
        contributors.sort();
        if let Ok(mut memo) = self.macro_names.lock() {
            if memo.len() >= MAX_CACHED_MACRO_WALKS {
                memo.clear();
            }
            memo.insert(
                key,
                CachedMacroNames {
                    contributors,
                    names: names.clone(),
                },
            );
        }

        names
    }

    /// **The macros in force at one point**, as a walk from that file computes them.
    ///
    /// The compilation's own environment (what the compiler predefines and the command line says) plus every fact
    /// the walk passes on the way to `offset`, applied in the order a preprocessor applies them — which is the only
    /// order that is right: a `#define` written below the point is not in force, one written in a header that comes
    /// later is not either, and a header that comes earlier is.
    ///
    /// Two things this is *not*: it is not a claim that the answer is complete (a header outside the index, or a
    /// `-D` nobody mentioned, is simply not in it — see [`crate::index::environment`]), and it is not a summary of
    /// the whole translation unit (the walk stops at `offset`, which is what makes it a question about a point).
    ///
    /// Costs one traversal of the files that precede the point, so a caller asking about **one** position should use
    /// this and a caller asking about thousands of them should use the incremental state
    /// [`ProjectIndex::macro_environment`] builds while it walks.
    pub fn macros_at(&self, path: &Path, offset: usize) -> Marked {
        let mut state = self.macros.clone();
        let certain = self.certainly_in_the_translation_unit(path);

        self.macro_candidates(
            path,
            &mut Vec::new(),
            false,
            &mut HashSet::new(),
            &certain,
            &mut Collecting::Nothing,
            &mut state,
            Some(offset),
        );

        state
    }

    /// The files this query reaches through **unguarded includes only** — no `#if` anywhere on the way.
    ///
    /// This is the answer to "is this file part of the translation unit whatever the macros are", and it is asked
    /// once per walk instead of being inferred from the route the walk happened to take: `visited` enters a
    /// file once, so the first route to reach it decides how everything it defines is filed, and that first route
    /// can be a conditional include while an unguarded one exists elsewhere in the same translation unit.
    ///
    /// # Why not ask `visible_files`
    ///
    /// It answers the same question and answers it *better* — its `Unconditional` also counts an `#include` whose
    /// condition was evaluated and found taken. It cannot be used here: that walk evaluates conditions, evaluating
    /// one calls [`ProjectIndex::macros_at`], and `macros_at` is what builds this set. Written that way it was a
    /// stack overflow on the first run (`macros_at` → `visible_files` → `visibility_at` → `macros_at` → …, with the
    /// memo unable to break the cycle because an answer is only recorded once it has been computed). So what is
    /// taken from that idea is the part that needs **nothing evaluated**, and a guarded include that *is* taken
    /// still counts for the walk through its own route: `visibility == Active` already keeps a route certain, see
    /// `macro_candidates`.
    pub(crate) fn certainly_in_the_translation_unit(&self, from: &Path) -> HashSet<String> {
        let from = normalize(from);

        // The file itself, always: a question asked in a file is about what that file says.
        let mut certain = HashSet::from([from.clone()]);
        let mut pending = vec![from];

        while let Some(current) = pending.pop() {
            let Some(summary) = self.summaries.get(&current) else {
                continue;
            };

            for include in &summary.includes {
                if include.guard != FactGuard::Unconditional {
                    continue;
                }

                let Some(resolved) = &include.resolved else {
                    continue;
                };

                let next = normalize(resolved);
                if certain.insert(next.clone()) {
                    pending.push(next);
                }
            }
        }

        certain
    }

    /// Every fact about `name` in this file and everything it includes, with where each one sits.
    ///
    /// A file is expanded once per query. That is enough for the answer — the facts are the same however many
    /// paths reach them — and it is what makes a cycle of includes terminate. What it costs is the ordering *among
    /// facts reached through the same top-level include*, which is the one case where a header reached twice by
    /// different routes could be placed at the earlier of its two positions rather than the later; the fact it
    /// reports is the same either way.
    ///
    /// No offset is applied here: where a fact *is* in the stream is the fact, and which of them is in force at a
    /// particular cursor is [`MacroEnvironment::at`]'s question. That split is what lets one walk answer thousands
    /// of positions.
    ///
    /// # The stream is one stream
    ///
    /// A file's `#define`s and its `#include`s are read **in offset order**, and the state in `state` is brought up
    /// to each point before anything is asked about that point. That is not an implementation detail: it is what
    /// makes `#ifdef X` decidable in a header whose *includer* defined `X`, and it is why the file's own directives
    /// and the ones its includes bring in are **one** stream rather than two lists. Two lists walked separately
    /// would decide every condition against the environment as it was before the file was read.
    ///
    /// # What the state is allowed to take from a fact
    ///
    /// Only what is **certain**, because the state decides later conditions and a wrong entry would make them
    /// decided wrongly rather than undecided:
    ///
    /// ```text
    /// the file is certainly in the translation unit, and the fact's region is taken → the name, with its value
    /// the fact settles the name whatever the branch                                  → definedness alone, never the value
    /// anything else (the region is unknown, or the file may not be included)         → nothing
    /// ```
    ///
    /// The second line is [`MacroFact::settles_the_name`] doing the same job it does for references: `#ifndef NAME /
    /// #define NAME` says the name *is* a macro afterwards whichever branch ran, and it does **not** say which body
    /// it has — so the name is defined and its value stays unknown.
    ///
    /// The first line used to read "the whole **path** is unconditional", where the path meant the route this walk
    /// took — see `certain`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn macro_candidates(
        &self,
        path: &Path,
        chain: &mut Vec<usize>,
        path_conditional: bool,
        visited: &mut HashSet<String>,
        certain: &HashSet<String>,
        collecting: &mut Collecting<'_>,
        state: &mut Marked,
        upto: Option<usize>,
    ) {
        let path = normalize(path);
        if !visited.insert(path.clone()) {
            return;
        }

        // **A file this query reaches unconditionally is certainly part of the translation unit**, whatever route
        // this particular walk took to get here. The flag above is about *this* path — `visited` means a file
        // is entered once, so the first route to reach it decided everything below — and the first route can be a
        // conditional include while an unconditional one exists elsewhere in the same translation unit. Asking the
        // graph instead of the route is what lets the certain path be used: `windef.h` includes `winnt.h`
        // unconditionally, so `winnt.h`'s macros are in force, whatever `minwindef.h` did earlier.
        let path_conditional = path_conditional && !certain.contains(&path);

        let Some(summary) = self.summaries.get(&path) else {
            // A file the walk cannot read is a hole in what this state knows: it may define anything, so from here
            // on a name nothing mentions is unknown rather than undefined.
            state.mark_incomplete();
            return;
        };

        // **The module edges, before the `#include`s and before the file's own text.** `import m;` is not an
        // `#include` with another spelling: it is not textual (no macro travels along it) and it names a *module*
        // rather than a file, so the file behind it is found through the module the project declares. All three
        // paths — a header unit, a partition of this file's own module, and an imported module's interface — are
        // `Unconditional`, which is the simplification `visible_files_by_asking_each_condition` documents: an
        // `import` inside an `#if` is recorded like any other, so a module reached only through a condition the
        // compilation does not satisfy is offered rather than hidden.
        //
        // **Recorded for a collector that is walking for visibility, and for no other one.** A module edge is not
        // an `#include`: it brings declarations into view and carries no `#define`, so a walk whose job is a macro
        // environment must not descend through one — which is what `crossed_a_module` says by queueing the target
        // for a second pass rather than recursing. See [`Seen::module_work`].
        for unit in &summary.modules.header_units {
            collecting.crossed_a_module(&normalize(unit));
        }
        for unit in &summary.modules.partitions {
            collecting.crossed_a_module(&normalize(unit));
        }
        for imported in &summary.modules.imports {
            let Some(interface) = self.module_interfaces.get(imported.as_ref()) else {
                // No file in this project declares it: `import std;` and prebuilt modules land here, and the
                // honest reading is "unknown", never "nothing" — a name that cannot be seen is not a name that
                // does not exist. Nothing is added, and nothing is claimed.
                continue;
            };
            collecting.crossed(interface, IncludeVisibility::Unconditional, true);
            collecting.crossed_a_module(&normalize(Path::new(interface)));
        }

        // **A module implementation unit implicitly imports its own interface unit** — `module math;` in a file that
        // is not `export module math;` gives that unit every name the interface declares, without an `import`
        // anywhere. A compiler relies on it: `math.cpp` defines `Vec2::length` and reads `x_`, which only
        // `math.cppm` declares.
        //
        // Measured on `test_module/src/math.cpp` before this existed: `x_` and `square(x_)` in the bodies resolved to
        // nothing, the hover was empty, and the completion inside a member function listed 200 names — every one of
        // them a macro or a function from the headers `math.cpp` includes, and not one member of `Vec2`.
        //
        // Only the *primary* module is crossed into: a partition implementation unit (`module m:part;`) is a
        // different question with its own rules, and `partition.is_none()` keeps this from answering it wrongly.
        if !summary.modules.is_interface
            && summary.modules.partition.is_none()
            && let Some(module) = &summary.modules.module
            && let Some(interface) = self.module_interfaces.get(module.as_ref())
        {
            collecting.crossed(interface, IncludeVisibility::Unconditional, true);
            collecting.crossed_a_module(&normalize(Path::new(interface)));
        }

        // What this walk made of this file's regions, recorded the first time each is asked about. The answer is
        // the one the *condition's own point* gives, and the first fact or include inside a region is the first
        // point at which the walk can ask — before anything inside the region has been applied. Asking again later
        // would read the region's own body as evidence about its condition, which is backwards for exactly the
        // shape that matters: `#ifndef NAME / #define NAME / #include <a.h> / #endif` has to include `a.h` on the
        // first pass, and a fresh look at the same condition after the `#define` would say it does not.
        //
        // **Keyed by the region *and the branch asked about***, which is the pair the answer is actually a function
        // of: "is branch *b* of region *r* the one that gets compiled" depends on the branches before *b* and on
        // nothing else, so it does not move as the walk advances. Keyed by the region alone it did move — the
        // branch asked about is the one the **fact** sits in, which is a position, so a verdict recorded for a fact
        // inside the `#if` was reused for a fact inside the `#else`. Measured on `vcruntime.h`: `_STL_LANG`'s live
        // definition at 7124 put `Region(32) → Some(true)` (7124 is in the `#if`) in this map, and the dead
        // `#define _STL_LANG 0L` at 7422 (in the `#else`) read that verdict back, so a `#define` no compiler reads
        // was applied and `_HAS_CXX17` came out `0`.
        let mut verdicts: HashMap<(u32, usize), Option<bool>> = HashMap::new();

        // The two lists are each in offset order (the indexer sorts them), so one merge of them *is* the order a
        // preprocessor reads in. A fact and an include can never start at the same offset.
        let mut facts = summary.macros.iter().peekable();
        let mut includes = summary.includes.iter().peekable();

        loop {
            let next_fact = facts.peek().map(|fact| fact.range.start_offset);
            let next_include = includes.peek().map(|include| include.range.start_offset);

            let (at, take_fact) = match (next_fact, next_include) {
                (Some(fact), Some(include)) => (fact.min(include), fact <= include),
                (Some(fact), None) => (fact, true),
                (None, Some(include)) => (include, false),
                (None, None) => break,
            };

            if upto.is_some_and(|upto| at >= upto) {
                break;
            }

            if take_fact {
                let fact = facts.next().expect("peeked");
                let reach = self.fact_reach(
                    summary,
                    &mut verdicts,
                    state,
                    &mut *collecting,
                    fact.guard,
                    at,
                );
                if reach == FactReach::Inactive {
                    continue;
                }

                collecting.fact(summary, chain, fact, path_conditional, reach);

                // …and then the name is in force for everything below this point. After the fact was read for its
                // own guard, which is the order a preprocessor reads in: `#ifndef NAME / #define NAME` is taken
                // *because* the name is not defined yet.
                apply_fact(state, fact, path_conditional, reach);

                continue;
            }

            let include = includes.next().expect("peeked");

            let visibility = self.include_visibility(
                summary,
                &mut verdicts,
                state,
                &mut *collecting,
                include.guard,
                at,
            );

            if visibility == Visibility::Inactive {
                continue;
            }

            let Some(target) = &include.resolved else {
                // An `#include` that did not resolve is the one gap the walk cannot make smaller: the file it
                // names may define anything at all, so every later "nothing defines this name" answer is off.
                state.mark_incomplete();
                continue;
            };

            // **The edge, recorded where it is crossed.** A collector that is walking for the *visibility* of the
            // root's closure wants this and nothing else, and this is the only point at which the truth about how
            // a file became visible is still available — see [`Collecting::crossed`]. Recorded *before* the
            // descent, because a file the walk cannot read still became visible by being named here: the old walk
            // recorded the edge and then failed to enter, and a conditional include of a header nobody indexed is
            // exactly the `Conditional` a "there may be more" answer is about.
            let step = match visibility {
                Visibility::Active => IncludeVisibility::Unconditional,
                Visibility::Unknown => IncludeVisibility::Conditional,
                // Unreachable: the branch above skips an inactive edge, and dropping it is the rule that makes a
                // decidable `#if` worth deciding at all.
                Visibility::Inactive => continue,
            };
            collecting.crossed(&normalize_path(target, cfg!(windows)), step, false);
            // **An `#include` makes the whole file visible**, however it was reached before: a file that is both
            // included and imported is not subject to the narrower exported-only rule, because the wider one
            // already grants what it would hide.
            collecting.reached_by_include(&normalize_path(target, cfg!(windows)));

            chain.push(include.range.start_offset);
            self.macro_candidates(
                target,
                chain,
                path_conditional || visibility == Visibility::Unknown,
                visited,
                certain,
                &mut collecting.reborrow(),
                state,
                // An included file is pasted *at* the include, so all of it is read before the caller's next line.
                None,
            );
            chain.pop();
        }
    }

    /// What the walk makes of one fact's region, recorded per file so that every condition in it is decided the
    /// same way.
    pub(crate) fn fact_reach(
        &self,
        summary: &FileSummary,
        verdicts: &mut HashMap<(u32, usize), Option<bool>>,
        state: &Marked,
        collecting: &mut Collecting<'_>,
        guard: FactGuard,
        offset: usize,
    ) -> FactReach {
        // A reborrow, so that the caller keeps its own use of `collecting` — a `&mut` is moved by default and the
        // walk needs the collector again on the next line of the file.
        match self.include_visibility(summary, verdicts, state, &mut *collecting, guard, offset) {
            Visibility::Active => FactReach::Active,
            Visibility::Inactive => FactReach::Inactive,
            Visibility::Unknown => FactReach::Unknown,
        }
    }

    /// Was the code the guard names compiled, with the state the walk has built and the verdicts it has recorded?
    pub(crate) fn include_visibility(
        &self,
        summary: &FileSummary,
        verdicts: &mut HashMap<(u32, usize), Option<bool>>,
        state: &Marked,
        collecting: &mut Collecting<'_>,
        guard: FactGuard,
        offset: usize,
    ) -> Visibility {
        let FactGuard::Region(region) = guard else {
            return Visibility::Active;
        };

        // **The file's own include guard is not a condition**. `#ifndef _STRING_ / #define _STRING_`
        // followed by the file's includes is how every header is written, and by the time the walk reaches an
        // include the name has been defined *by the line above it* — so evaluating that region says "not taken"
        // and the walk skips **every include of the file**. The visible consequence was MSVC's whole library:
        // `_STL_COMPILER_PREPROCESSOR`, defined by `<yvals_core.h>`, came out `defined = Some(false)` in the state
        // at `<string>`'s own `#include <xstring>`, so `#if _STL_COMPILER_PREPROCESSOR` — a condition that holds —
        // was answered `Inactive`, and every fact behind it read as `Conditional`.
        //
        // The index already treats the facts guarded by exactly this region as unconditional
        // (`deguard_the_files_own_guard`), and `SummaryGuards::own_guard` exists so that a **walk** can apply the
        // same rule — which is what this is. Entering the file at all is what the guard means.
        if summary.guards.own_guard == Some(region) {
            return Visibility::Active;
        }

        let mut unknown = false;

        for at in summary.guards.conditions_of(region) {
            // The branch this position sits in, which is what the verdict below is *about* — and half of the key it
            // is remembered under, because "is this branch compiled" does not depend on where the fact stands.
            let Some(branch) = summary
                .guards
                .region_at(at.region, offset)
                .map(|region| region.active_branch)
            else {
                // A guard naming a region this summary does not describe — a summary written before the regions
                // carried their conditions, or one whose bytes were produced by something else. Nothing can be said
                // about it, and `Unknown` is what every query said about every region before this existed.
                unknown = true;
                continue;
            };

            // **The walk answering its own question answers it here**, which is the difference between one
            // traversal of the closure and one per condition. `None` is every other collector, and then the state
            // below is the one this walk was handed rather than the one it built.
            let holds = match verdicts.get(&(at.region, branch)) {
                Some(recorded) => *recorded,
                None => {
                    let decided = match collecting.decided_at(
                        summary,
                        state,
                        at.region,
                        at.condition_at,
                        offset,
                    ) {
                        Some(verdict) => verdict,
                        None => summary
                            .guards
                            .region_at(at.region, offset)
                            .and_then(|region| {
                                region.visibility(
                                    &crate::index::environment::MacrosHere::from_walk(
                                        state,
                                        at.condition_at,
                                    ),
                                )
                            }),
                    };

                    verdicts.insert((at.region, branch), decided);
                    decided
                }
            };

            match holds {
                Some(true) => {}
                Some(false) => {
                    // **An `#else` is taken precisely when the condition is *not*.** The verdict above
                    // answers "did this region's `#if` hold", which is the right question for the branch the
                    // condition guards and the wrong one for the `#else`, whose whole meaning is "nothing before me
                    // was taken". One of those decides MSVC's entire STL: `yvals_core.h` writes
                    //
                    // ```cpp
                    // #if defined(RC_INVOKED) || defined(Q_MOC_RUN) || defined(__midl)
                    // #define _STL_COMPILER_PREPROCESSOR 0
                    // #else
                    // #define _STL_COMPILER_PREPROCESSOR 1     // <- this is the definition that is in force
                    // #endif
                    // ```
                    //
                    // so the definition that *is* compiled was judged "not taken" (measured: `reach = Inactive`
                    // for both facts), `#if _STL_COMPILER_PREPROCESSOR` then had no value, and every query in the
                    // library answered `ConditionalCompilation`.
                    //
                    // # The inversion compensated for the memo, and the memo is fixed
                    //
                    // Removing it is the *obvious* correction — `region_at` chooses a branch by offset, so
                    // `Region::visibility` already answers "is the branch in force here the one compiled", and one
                    // inversion on top of that turns a dead `#else` into a live one.
                    //
                    // It was tried once and reverted: **eight MSVC headers went from 319 deduced to 251, refused
                    // 312 → 380**. That measurement was taken **before** `verdicts` was keyed by `(region, branch)`.
                    // With the memo keyed by region alone, a `Some(false)` for a position inside an `#else` was
                    // almost unreachable — the verdict read back was the one recorded for a fact inside the `#if` —
                    // so this rule was load-bearing for a reason that was itself a bug. The numbers above are a
                    // measurement of that bug, not of this rule.
                    return Visibility::Inactive;
                }
                None => unknown = true,
            }
        }

        if unknown {
            Visibility::Unknown
        } else {
            Visibility::Active
        }
    }
}

/// What a walk is collecting as it goes: the facts about one name, the visibility of every file it enters, or
/// nothing.
///
/// The same walk answers all three questions — "where is this name a macro"
/// ([`ProjectIndex::macro_environment`]), "what is in force at this point" ([`ProjectIndex::macros_at`]) and "what
/// can this file see" ([`ProjectIndex::visible_files_with_modules`]) — because they are the same traversal and the
/// rules that make it right are the ones that must not be written twice. The second caller wants the *state* and no
/// candidates, which is what `Nothing` says, rather than a name no macro has.
pub(crate) enum Collecting<'a> {
    Name(&'a str, &'a mut Vec<MacroCandidate>),
    /// **Every macro name the walk applies**, for a caller that has to ask about many of them — the colouring pass
    /// asks "is this spelling a macro" once per identifier, and asking the graph once per spelling was 526 ms on a
    /// real header. See [`Collecting::fact`].
    Names(&'a mut HashSet<String>),
    /// **Which files this root can see, and how** — filled in as the walk crosses each `#include`.
    ///
    /// This variant is the whole of why the visibility walk is affordable. Asking the same question per *edge* —
    /// `visible_files_with_modules` as it was written — evaluates each guarded include against a state built for
    /// that one file (`environment::visibility_at`, which is a closure walk), so a query over the standard library
    /// pays one walk per condition in the closure. Carried this way the state is already in hand when the edge is
    /// crossed, which is what [`crate::index::environment::MacrosHere::from_walk`] exists for.
    Visibility(&'a mut Seen),
    Nothing,
}

/// The files one root can see, and which of them are in view only through an `import`.
///
/// One memoised visibility walk: what it found, and **the files it was made of**.
///
/// The contributors are the normalized paths of the closure (the root and every file [`Seen::visibility`] records),
/// so a re-read of one of them drops exactly the walks that could have answered differently. See
/// [`ProjectIndex::visibility`] for the measurement that made this worth carrying.
/// One memoised visibility walk: what it found, **the files it was made of**, and the inputs it read from them as one
/// fingerprint. The fingerprint is what decides whether the entry may still answer — see
/// [`ProjectIndex::the_walks_fingerprint`].
#[derive(Debug, Clone)]
pub(crate) struct CachedWalk {
    pub(crate) seen: std::sync::Arc<Seen>,
    pub(crate) contributors: std::sync::Arc<Vec<String>>,
    /// The inputs this walk was made of, as one number. An entry answers only while every contributor's
    /// [`ProjectIndex::walk_inputs`] still hashes to what it did here.
    pub(crate) fingerprint: u64,
    /// **The files the walk found, in sequence order** — the order every cross-file question iterates by. Computed
    /// once with the walk: rebuilding it per question (clone every path, look up every sequence, sort) was seconds of
    /// a cold request, because a cold inlay asks thousands of those questions.
    pub(crate) files: std::sync::Arc<Vec<(u32, IncludeVisibility)>>,
    /// The value of [`ProjectIndex::changed`] when this entry was last built or validated: while the index has not
    /// changed since, there is nothing to validate and the fingerprint is not recomputed.
    pub(crate) stamp: u64,
}

/// **A claimed visibility walk**, released however the walk ends — see [`ProjectIndex::claim_a_walk`].
///
/// A guard rather than a call at the end of the walk because a walk has several exits (a file the index cannot
/// describe, a module edge that resolves to nothing) and each of them must wake whoever is waiting. `Drop` is the
/// only thing that runs on all of them.
pub(crate) struct WalkClaim<'a> {
    pub(crate) index: &'a ProjectIndex,
    pub(crate) root: String,
}

impl Drop for WalkClaim<'_> {
    fn drop(&mut self) {
        self.index.a_walk_is_over(&self.root);
    }
}

/// Two outputs from one walk because the second is a property of *how* a file was reached, and a walk that has
/// already crossed the edge is the only thing that knows. See [`ProjectIndex::visible_files_with_modules`].
#[derive(Default, Debug)]
pub(crate) struct Seen {
    pub(crate) visibility: HashMap<String, IncludeVisibility>,
    pub(crate) through_an_import: HashSet<String>,
    /// **The files whose module edges still have to be followed.**
    ///
    /// A module edge is not an `#include`, and the difference is why this frontier exists rather than another
    /// recursive descent: an `import` brings *declarations* into view and carries no `#define`, so
    /// `macro_candidates` — whose whole job is a macro environment — must not walk through one. But the
    /// *visibility* of a partition's declarations is asked the same question, so this collector keeps its own list
    /// and works it after the walk. Measured: without it, `rectangle` and `square` — declared in the partition
    /// `shapes.cppm` re-exports — answered `NotDeclaredHere`.
    pub(crate) module_work: Vec<String>,
}

impl Seen {
    pub(crate) fn new(root: &str) -> Seen {
        Seen {
            visibility: HashMap::from([(root.to_string(), IncludeVisibility::Unconditional)]),
            through_an_import: HashSet::new(),
            module_work: Vec::new(),
        }
    }
}

impl Collecting<'_> {
    /// **One** fact, if the caller is collecting facts about that name — or **every** name, when it is collecting
    /// the environment ([`Collecting::Names`]).
    pub(crate) fn fact(
        &mut self,
        summary: &FileSummary,
        chain: &[usize],
        fact: &MacroFact,
        path_conditional: bool,
        region: FactReach,
    ) {
        // **The name-collecting arm, and the reason it exists**: the colouring pass used to ask
        // `macro_environment(name, file)` once per *spelling* not declared in the file, and each of those is a whole
        // walk of the include graph. Measured on this project's `codeformat.h` — **526 ms** for 521 tokens, because a
        // header mentions a few hundred names and each one paid the walk. Collecting every name in one walk answers
        // all of them from one pass, and the question "is this spelling a macro here" is the same question.
        if let Collecting::Names(out) = self {
            out.insert(fact.name.clone());
            return;
        }

        let Collecting::Name(name, out) = self else {
            return;
        };

        if fact.name != *name {
            return;
        }

        let mut position = chain.to_vec();
        position.push(fact.range.start_offset);

        out.push(MacroCandidate {
            position,
            file: summary.path.clone(),
            fact: fact.clone(),
            path_conditional,
            region,
        });
    }

    /// The same collection, for a recursive call.
    pub(crate) fn reborrow(&mut self) -> Collecting<'_> {
        match self {
            Collecting::Name(name, out) => Collecting::Name(name, out),
            Collecting::Names(out) => Collecting::Names(out),
            Collecting::Visibility(seen) => Collecting::Visibility(seen),
            Collecting::Nothing => Collecting::Nothing,
        }
    }

    /// **A file has just become visible through an `#include`**, at the visibility the edge was given.
    ///
    /// Recorded at the *crossing* rather than when the file is entered, and the difference is a wrong answer
    /// rather than a tidy one: `visited` enters a file once, so the second route to it never reaches its own
    /// `enter`, and a file reached by a guarded include first and an unguarded one second would be filed
    /// `Conditional` for the rest of the walk. The better of the two wins, which is what the per-edge version
    /// computed and is the only reading that is true.
    pub(crate) fn crossed(
        &mut self,
        target: &str,
        visibility: IncludeVisibility,
        through_an_import: bool,
    ) {
        let Collecting::Visibility(seen) = self else {
            return;
        };

        match seen.visibility.get(target) {
            // Already known, and no worse: unconditional is the strongest answer and nothing improves on it.
            Some(known) if *known <= visibility => {}
            _ => {
                seen.visibility.insert(target.to_string(), visibility);
            }
        }

        if through_an_import {
            seen.through_an_import.insert(target.to_string());
        }
    }

    /// An `#include` reaches `target`, so the exported-only rule stops applying to it.
    ///
    /// The order the two edges arrive in does not matter, and that is the point: a file imported first and
    /// included later is include-visible, and one included first and imported later was already so.
    pub(crate) fn reached_by_include(&mut self, target: &str) {
        let Collecting::Visibility(seen) = self else {
            return;
        };

        seen.through_an_import.remove(target);
    }

    /// **A module edge was crossed**, so the target's own module edges still have to be followed.
    ///
    /// Recorded for a second pass rather than descended, for the reason [`Seen::module_work`] gives: the walk this
    /// collector rides on is building a macro environment, and a macro does not travel across an `import`.
    ///
    /// **Queued whether or not the file was already visible**, which is the difference between following the edges
    /// of what this root reaches and following the edges of one arbitrary file: a partition's own `export import`
    /// is reachable only through this queue, and a file that is already in the visibility map for another reason
    /// still has them. Measured: gated on "not seen before", `rectangle` and `square` answered `NotDeclaredHere`.
    pub(crate) fn crossed_a_module(&mut self, target: &str) {
        let Collecting::Visibility(seen) = self else {
            return;
        };

        seen.visibility
            .entry(target.to_string())
            .or_insert(IncludeVisibility::Unconditional);

        if !seen.module_work.iter().any(|queued| queued == target) {
            seen.module_work.push(target.to_string());
        }
    }

    /// **One guarded edge**, and this walk is the one answering it.
    ///
    /// The sink [`ProjectIndex::include_visibility`] calls instead of building a closure state for the file: the
    /// state is in hand, because this walk is already carrying it, and that is the difference between one traversal
    /// and one traversal per condition. `state` is the walk's state **at the condition's own point**, which is what
    /// the answer is a function of — a `#define` above the condition is in force and one below is not.
    ///
    /// The answer is **the raw verdict** — `Some(true)` the condition holds, `Some(false)` it does not, `None`
    /// nobody can say — and deliberately not a [`Visibility`]: the two are not the same thing, because a guard's
    /// `#else` is compiled exactly when its `#if` does *not* hold. `include_visibility` owns that inversion, and a
    /// second copy of it here is how this function first read `Unknown` as `Inactive`.
    ///
    /// `None` means "not this walk's question", which is every other variant of [`Collecting`].
    pub(crate) fn decided_at(
        &mut self,
        summary: &FileSummary,
        state: &Marked,
        region: u32,
        condition_at: usize,
        offset: usize,
    ) -> Option<Option<bool>> {
        let Collecting::Visibility(seen) = self else {
            return None;
        };
        let _ = seen;

        // **The file's own include guard is not a condition**, the same rule the branch above applies: by the time
        // the walk reaches anything in the file the name has been defined by the line above it, so evaluating that
        // region would say "not taken" and drop every include in the file. `true` is "the code behind it is
        // compiled", which is what entering the file means.
        if summary.guards.own_guard == Some(region) {
            return Some(Some(true));
        }

        let asked = summary.guards.region_at(region, offset)?;
        Some(
            asked.visibility(&crate::index::environment::MacrosHere::from_walk(
                state,
                condition_at,
            )),
        )
    }
}

/// What a fact does to the state, if anything — see [`ProjectIndex::macro_candidates`] for the three cases.
///
/// Called *after* the fact's own guard has been read, which is the order a preprocessor reads in: the
/// `#ifndef NAME / #define NAME` shape is taken because the name is not defined yet, and a state that had already
/// absorbed the `#define` would decide it the other way round — for every include guard in the corpus.
pub(crate) fn apply_fact(
    state: &mut Marked,
    fact: &MacroFact,
    path_conditional: bool,
    reach: FactReach,
) {
    // A fact in a file that may not have been included at all, or in a region nobody can decide: the name may or
    // may not be defined here, and a later condition must not be told either way.
    if path_conditional || reach == FactReach::Unknown {
        // …unless the region settles the name whatever its own condition says, which is the one thing an
        // undecidable branch *can* still say — and it says it about definedness only, never about the value.
        match (fact.settles_the_name, fact.kind) {
            (true, crate::MacroKind::Definition) => state.define_name(&fact.name),
            (true, crate::MacroKind::Undefinition) => state.undefine(&fact.name),
            _ => state.mark_uncertain(&fact.name),
        }

        return;
    }

    match reach {
        // In force: the name and its value, as the file wrote them.
        FactReach::Active => state.observe(fact),
        // Handled above.
        FactReach::Inactive | FactReach::Unknown => {}
    }
}

/// How reachable a fact is, as the walk sees it: whether its own region is decided.
///
/// A name of its own rather than [`Visibility`], because the walk has a fourth case the guard layer does not: a
/// fact that is *not* written in a conditional at all, which is certain without anything being evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FactReach {
    /// The fact is outside every conditional, or its region is decided and taken.
    Active,
    /// The region is decided and **not** taken: a compiler would not have read this fact.
    Inactive,
    /// At least one enclosing condition cannot be decided here.
    Unknown,
}
