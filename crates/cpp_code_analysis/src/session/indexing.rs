//! **Indexing** — a file's summary, its includes, and the queue of work that makes a closure ready.
//!
//! Moved here from `session/mod.rs` without a change. `catch_up` is the per-file step; `index_everything` is the batch
//! one, and the two are the same walk called with different budgets.

use super::*;

impl<F: FileProvider + Clone> Session<F> {
    /// **The translation units a pass over these files reads its environments out of** — one walk each, and cached.
    ///
    /// The project's own translation units are tried first, because one of them covers the most: a project file is
    /// a root whose closure is the program, while a header's own closure is "this header compiled standalone".
    /// A candidate that a unit already built reads is skipped — the question is "which timeline holds this file's
    /// environment", and the first unit that answers `environment_of` is that timeline. On the 138-file project
    /// this returns **one** unit, where the second pass used to walk a closure per file.
    pub(crate) fn units_for_the_pass(
        &self,
        files: &[PathBuf],
    ) -> Vec<std::sync::Arc<crate::TranslationUnit>> {
        // **The slice, and not the project.** This used to chain `self.project` in front of `files`, on the argument
        // that a project file is a root whose closure is the whole program and therefore covers the most — and the
        // argument was right about coverage and wrong about cost, because the units built here are a **local** vector
        // while [`MAX_UNITS`] caps what survives the call. So every call walked a closure for essentially every
        // project file, and threw them away.
        //
        // Measured on a real project (198 own sources + a vendored `3rd/`, 1216 project files, no compilation
        // database): `walk` was **54.3 s of a 96.5 s index — 56%** — against `parse` 10.9 s and `sweep` 9.0 s. That
        // is 19 `advance(64)` calls × 1216 candidates ≈ 23 000 closure walks to produce the environments of 1729
        // files, and it made the whole index **serial**: wall 95.6 s against 96.5 s of CPU on sixteen cores.
        //
        // A file's own timeline is the closure of that file compiled standalone, which is the environment its own
        // reading was built with — the thing the second pass is asking for. Where a project root would cover several
        // files of one slice, the `environment_of` test below still skips them, and the walk is paid once for the
        // file that needs it rather than once for every file in the project.
        //
        // Asked by the **index's own spelling** of each path: a unit's frames are keyed by the normalized form, so
        // asking with `C:\…` about a frame spelled `c:/…` answers `None` — which would build a unit per file and
        // put the whole cost back where it was.
        let mut candidates: Vec<PathBuf> = Vec::new();
        let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        for path in files.iter() {
            let path = self
                .store
                .index()
                .summary(path)
                .map(|summary| summary.path.clone())
                .unwrap_or_else(|| path.clone());
            if seen.insert(path.clone()) {
                candidates.push(path);
            }
        }

        // **Walked in parallel, and the dedup kept afterwards.** The walks are independent read-only work on `&self`
        // — a timeline is a function of the closure's summaries and text — and the process was otherwise one core of
        // sixteen: measured on a real project, **1188 walks × ~38 ms = 44.8 s of a 67 s index with CPU/wall ≈ 1.2**
        // (`examples/project_index.rs`, and `Session::walks_by_root` for the count that said the walks are one per
        // file rather than one file walked many times).
        //
        // The sequential loop this replaces skipped a candidate that an **earlier** unit already covered, so it did
        // slightly fewer walks — 1188 of 1729 candidates, so about a third fewer. That saving is what the order
        // below preserves and what running first does not: every candidate is walked, and then the **same** dedup
        // runs in the **same** order over the results. The units handed to the second pass are therefore the ones the
        // sequential version handed it, in the same order, and only the walking moved.
        //
        // Roughly 541 extra walks of ~38 ms is ~20 s of CPU, which is ~1.3 s of wall on this machine — against 44.8 s
        // of serial walking. Any future change here should re-measure rather than reason: the count above is the
        // instrument, and `CPU/wall` in the same probe says whether the cores are actually being used.
        // **Walked one at a time, and that is a measurement rather than a preference.** This was made parallel —
        // one thread per core over contiguous chunks, with the dedup moved after the walks so that the units handed
        // to the second pass stayed identical — on the argument that 1188 walks × ~38 ms is 44.8 s of a 67 s index
        // with `CPU/wall ≈ 1.2`, so fifteen idle cores had to be worth something. It was never measured before
        // being kept, and the first measurement afterwards is a **cold index of a real project going from 67.3 s to
        // 142.7 s**. The shape of that regression is not mysterious in hindsight: `advance(64)` is called about
        // twenty-seven times, each call spawning up to sixteen threads, and each thread's `translation_unit_of`
        // contends for three mutexes (`held_units`, `walks_by_root`, `held_definitions`) — so what was added is
        // thread creation plus lock contention over work that already does its own file I/O.
        //
        // The sequential loop is also the one that walks **fewer** timelines: it skips a candidate an earlier unit
        // already covers, which is roughly 541 of 1729 on that project and is exactly what running first gives up.
        let mut units: Vec<std::sync::Arc<crate::TranslationUnit>> = Vec::new();

        // **A unit already in memory may cover this file, and then there is nothing to walk.**
        //
        // `translation_unit_of` builds a closure timeline per root (~38 ms), and `MAX_UNITS` of them are held. The
        // files a re-read slice names are usually headers of the root that was just walked — the same closure, one
        // step up the graph — so asking the held table first turns "walk this header's own timeline" into "reuse the
        // timeline that already contains it". Only units known to be **complete** are offered: a provisional one is
        // a closure still missing files, and an environment read from it is the defect the pass exists to repair.
        //
        // The key carries the environment (`SummaryKey::env_hash`), so a summary built from one root's environment is
        // filed under it: reusing a unit can produce a *different* entry, never a wrong one.
        let held: Vec<std::sync::Arc<crate::TranslationUnit>> = {
            let table = self.held_units();
            table
                .held
                .values()
                .filter(|held| !held.provisional)
                .map(|held| std::sync::Arc::clone(&held.unit))
                .collect()
        };
        let mut still_to_walk: Vec<PathBuf> = Vec::new();
        for candidate in candidates {
            if let Some(unit) = held
                .iter()
                .find(|unit| unit.environment_of(&candidate).is_some())
            {
                if !units
                    .iter()
                    .any(|known| std::sync::Arc::ptr_eq(known, unit))
                {
                    units.push(std::sync::Arc::clone(unit));
                }
                continue;
            }
            still_to_walk.push(candidate);
        }

        for (candidate, unit) in self.walk_the_timelines(still_to_walk) {
            if units
                .iter()
                .any(|unit| unit.environment_of(&candidate).is_some())
            {
                continue;
            }
            if let Some(unit) = unit {
                units.push(unit);
            }
        }

        units
    }

    /// **[`Session::translation_unit_of`] for many roots at once**, **one at a time**, preserving the order of
    /// `roots`.
    ///
    /// Kept as a named function rather than folded into the caller because that is where the parallel version lived
    /// and where the next attempt will go. **Do not parallelise it again without measuring the thing it is for**:
    /// see the comment at the call site for what the last attempt cost and why its shape regressed.
    #[allow(dead_code)]
    pub(crate) fn walk_the_timelines(
        &self,
        roots: Vec<PathBuf>,
    ) -> Vec<(PathBuf, Option<std::sync::Arc<crate::TranslationUnit>>)> {
        roots
            .into_iter()
            .map(|root| {
                let unit = self.translation_unit_of(&root);
                (root, unit)
            })
            .collect()
    }

    /// **Read one file, and queue what it includes.** One step of the work, as a function because there are now two
    /// callers: the pump ([`Session::advance`]) and [`Session::catch_up`], which needs the same step *now* rather
    /// than at the pump's pace.
    ///
    /// The order inside is the whole of the pump's correctness and is documented at the call site it came from: the
    /// file is loaded into the VFS before it is read (so everything the analysis reads it is also holding), and its
    /// resolved includes are cloned out before the queue is touched (`get` borrows the store, and the queue is a
    /// field of the same struct).
    ///
    /// `ready` is the file already read and parsed, when [`Session::advance`] prepared it alongside its neighbours;
    /// `None` reads it here.
    pub(crate) fn index_one(
        &mut self,
        path: PathBuf,
        priority: Priority,
        depth: usize,
        ready: Option<crate::index::store::Prepared>,
    ) -> Step {
        {
            let loading = std::time::Instant::now();
            self.vfs.load(&path);
            if crate::probes::units() {
                eprintln!(
                    "[step] load {} took {} us",
                    path.display(),
                    loading.elapsed().as_micros()
                );
            }
        }
        let before = self.store.stats();

        // **The environment this session already holds for this file, or one it can walk now.** A unit that is *not*
        // in memory is one whose closure this session has not read — or has read, and lost the timeline of: the units
        // table is bounded by [`MAX_UNIT_BYTES`], and after a project is indexed the file a reader is typing in
        // usually has no entry. That case used to be the ordinary one and it fell back to the file's own text; what
        // decides now is [`Session::the_timeline_to_read_through`], which walks one when the file is the reader's
        // and the program it needs is already read, and answers `None` for everything else.
        //
        // **Asked by the index's own spelling**, because a unit's frames are keyed by the spelling the walk used
        // and asking with any other one answers `None`. `Session::units_for_the_pass` records the same trap from
        // the other side — a header whose name is spelled one way by the session and another by the include graph
        // is a header that gets a unit per spelling.
        //
        // The `Arc` is held on its own because a `MacroView` borrows the unit it came from, and the unit has to
        // outlive it — and it is **cloned**, because the store call below takes `&mut self` at the next step.
        let spelled = self
            .store
            .index()
            .summary(&path)
            .map(|summary| summary.path.clone())
            .unwrap_or_else(|| path.clone());
        // **And never a provisional one.** `with_the_environment` asserts that the second pass has nothing to repair,
        // and a timeline built while a file the closure needs was still queued is missing exactly what that file
        // defines — so reading a file with it and marking the result would suppress the repair for a reading that
        // needs it. A provisional timeline is a reading of most of the program, which is worth having for a *cook*
        // and is not worth asserting anything about. See [`HeldUnit`].
        let prepared_with_environment = ready
            .as_ref()
            .is_some_and(crate::index::store::Prepared::with_environment);
        let unit = if ready.is_none() {
            self.the_timeline_to_read_through(&spelled)
        } else {
            None
        };
        let timed = std::time::Instant::now();
        let timeline_ask = std::time::Instant::now();
        if crate::probes::units() {
            eprintln!(
                "[step] timeline question for {} took {} ms (timeline {})",
                path.display(),
                timeline_ask.elapsed().as_millis(),
                unit.is_some()
            );
        }
        let has_a_timeline = unit
            .as_ref()
            .is_some_and(|unit| unit.environment_of(&spelled).is_some());
        // Read before the wave's work is moved into the store, because the flag below is about which arm ran.
        let read_here = ready.is_none();

        let summary = match ready {
            // A wave prepared this file, and a wave has no unit of its own (`Session::prepare_a_wave` runs before
            // the pass that builds them), so what it produced is what it is and the second pass still looks at it.
            Some(ready) => self.store.commit(&path, ready),
            None => match &unit {
                // **Read once, through the timeline** — instead of once without one and once again in the second
                // pass. The summary this builds *is* the rendering, which is the reading every other feature and the
                // second pass were trying to reach; see `SummaryStore::prepare_with_the_timeline`.
                Some(unit) => {
                    let prepared = self.store.prepare_with_the_timeline(&path, unit);
                    self.store.commit(&path, prepared)
                }
                None => self.store.get(&path),
            },
        };
        if crate::probes::units() {
            eprintln!(
                "[step] {} read in {} ms after a {} ms timeline question (timeline {})",
                path.display(),
                timed.elapsed().as_millis(),
                timeline_ask.elapsed().as_millis(),
                unit.is_some()
            );
        }
        let tail = std::time::Instant::now();
        let includes: Vec<PathBuf> = summary
            .map(|summary| {
                summary
                    .includes
                    .iter()
                    .filter_map(|include| include.resolved.clone())
                    .collect()
            })
            .unwrap_or_default();
        let after = self.store.stats();
        let outcome = outcome_of(before, after);
        if crate::probes::units() {
            eprintln!(
                "[step] {} includes+stats took {} us ({} include(s))",
                path.display(),
                tail.elapsed().as_micros(),
                includes.len()
            );
        }

        // Everything this file includes joins the same half of the list the file came from, one level further out —
        // a header an open file includes is worth reading before the rest of the project, and a header the project's
        // tenth translation unit includes is not.
        let queueing = std::time::Instant::now();
        let include_count = includes.len();
        for include in includes {
            self.queue.add(include, priority, depth + 1);
        }
        if crate::probes::units() {
            eprintln!(
                "[step] {} queued {include_count} include(s) in {} us",
                path.display(),
                queueing.elapsed().as_micros()
            );
        }

        Step {
            path,
            priority,
            depth,
            outcome,
            // **Only when the summary was built here, from this environment.** A file the disk answered for was
            // built by whoever wrote that entry — with an environment or without one, the store cannot say — and a
            // file read without an environment is exactly what the second pass is for. Claiming either would be
            // claiming the second pass has nothing to repair, which is the whole of what this flag asserts.
            with_the_environment: prepared_with_environment
                || (read_here
                    && has_a_timeline
                    && matches!(outcome, StepOutcome::Built | StepOutcome::Unstored)),
        }
    }

    /// **Read one file now, if the editor has changed it since the index read it.**
    ///
    /// # The bug this exists for
    ///
    /// An edit reaches the index in two steps, and they are not the same step: `Session::did_open` /
    /// `did_change` replace the buffer **and drop the file's summary**, and the summary is rebuilt later, when the
    /// pump reaches that file in the queue. Between the two, the analysis holds the new *text* and the old *facts*:
    /// the scope tree is rebuilt from the new text on every parse (so locals are right), while everything that reads
    /// the index is an edit behind.
    ///
    /// Completion is where that shows, and it shows as the wrong list rather than as an error: `full.` needs the
    /// **type** of `full`, the type comes from the index, and a stale index says `full` is not declared — so the
    /// member query declines and the client is handed the names in scope, which is a list of things that cannot
    /// follow a `.`. Measured on a user's file: the popup after `myName.firstName.` was `printf`, `full`, `sum`,
    /// `main` and the keywords.
    ///
    /// # Why this and not a lock
    ///
    /// Blocking the request until the **pump** has caught up would be a lock, and it would be the wrong one: the
    /// pump reads the file's whole include closure, so a request about a file whose first line is
    /// `#include <string>` would wait for thousands of headers — seconds, to answer a keystroke.
    ///
    /// # The two cases, and why both end the same way
    ///
    /// ```text
    /// the file is queued         one file whose answer changed — an edit. Read it now.
    /// the file is *not* queued   the ordinary keystroke, and the first one of a session.
    /// ```
    ///
    /// and then, **either way**, the files it includes that the index has never read: a completion that needs a type
    /// from a header cannot be answered while that header is missing, and "the index has not reached it yet" is not a
    /// state a keystroke should be able to observe. It is the pump's own step (`Session::index_one`), so the
    /// queueing and the cascade are the same code — and a header whose answer is *already* in the index is a cache
    /// hit whenever the pump reaches it, so nothing is read twice.
    ///
    /// That second half is the case a user hit: `full2.` on a fresh session answered **four items** — the names in
    /// scope — because `std::string` was not in the index yet and nothing had read the headers that declare it.
    pub fn catch_up(&mut self, path: &Path) {
        // **The file a request is about is one the second pass may refine** — see `want_refinement`.
        self.want_refinement(path);
        if self.queue.forget(path) {
            self.index_one(path.to_path_buf(), Priority::Open, 0, None);
        }

        // **A slice of its includes, not all of them, and the difference is a hover's latency.** Reading an include
        // is a parse and can be a timeline walk, and this call is made from **inside a request** (a hover, a
        // completion): on a project whose standard-library closure is a hundred headers, the first hover of every
        // file held the write lock for **210–305 ms** — and a write held that long is every request in flight
        // waiting, not this one. Measured over the wire on `EmmyLuaCodeStyle`, per-file first hover:
        //
        // ```text
        // before    40–405 ms, 135 ms average, `a request queued the modules` holding 210–305 ms
        // ```
        //
        // Eight is the same slice the pump uses (`PREPARE_INDEX_SLICE`'s spirit): what a hover needs is the
        // declaration it is pointing at, which is in the file itself or in the first header that declares it, and the
        // rest is queued for the pump — which is woken by the caller (`read_the_modules` ends with `wake`) and reads
        // them on its own slice. Nothing is lost: `needs_catching_up` stays true while any include is unread, so the
        // next request about this file reads eight more and the pump drains the rest in between.
        const INCLUDES_A_REQUEST_READS: usize = 8;
        let mut includes = self.unread_includes_of(path);
        if includes.len() > INCLUDES_A_REQUEST_READS {
            for include in includes.split_off(INCLUDES_A_REQUEST_READS) {
                self.queue.add(include, Priority::Open, 1);
            }
        }
        for include in includes {
            self.queue.forget(&include);
            self.index_one(include, Priority::Open, 1, None);
        }
    }

    /// **The files this one includes that the index has never read**, in the order they were written.
    ///
    /// Empty for the ordinary case — a session whose pump has drained has read every include of every file it
    /// indexed — and empty as well for a file with no summary at all, which is a question the caller has just
    /// answered by reading it.
    pub(crate) fn unread_includes_of(&self, path: &Path) -> Vec<PathBuf> {
        let Some(summary) = self.store.index().summary(path) else {
            return Vec::new();
        };

        summary
            .includes
            .iter()
            .filter_map(|include| include.resolved.as_ref())
            .filter(|resolved| self.store.index().summary(resolved).is_none())
            .cloned()
            .collect()
    }

    /// **Is there anything for [`Session::catch_up`] to read?** — the question a caller asks *before* taking the
    /// write lock.
    ///
    /// `catch_up` is two reads and they are both conditional: the file itself when an edit has left its summary
    /// dropped and the pump has not rebuilt it, and the files it includes that the index has never read. For the
    /// ordinary keystroke — and for every request after the first on a file nobody has edited — both answers are
    /// "nothing", and the whole of what the call costs is the lock it takes to find that out.
    ///
    /// Measured over the wire on the latency fixture: with the same shortcut already in place for `prepare`, a
    /// completion's `catch_up` cost **0–1108 ms** for a file whose summary was current and whose includes were all
    /// indexed. Every millisecond of that was the queue.
    ///
    /// Exact rather than approximate: [`Work::is_queued`] is the test `catch_up` itself acts on (and `forget` is
    /// written in terms of it), and [`Session::unread_includes_of`] is the list `catch_up` itself walks. `false`
    /// means "read nothing", and reading nothing changes nothing.
    pub fn needs_catching_up(&self, path: &Path) -> bool {
        self.queue.is_queued(path) || !self.unread_includes_of(path).is_empty()
    }

    /// **Is every file this one can name from a summary in the index** — the question a *colouring* asks.
    ///
    /// [`Session::is_locally_ready`] asks about the file's **direct** includes, which is the right question for a
    /// query that resolves one name (`catch_up` reads exactly what that needs). It is the wrong question for the
    /// semantic tokens, which classify **every identifier in the file** from the index: a name declared ten headers
    /// deep (`std::endl` is in `<ostream>` through `<iostream>`) is unclassified when only the first level has been
    /// read, and the answer a client gets is a file that is mostly drawn in its own grammar colours.
    ///
    /// So the test is the **closure**: every file [`ProjectIndex::visible_files`] reaches, summarized. That walk is
    /// the one every cross-file query already pays (and it is memoized), and the summary lookups are hash reads.
    ///
    /// # Only the files that are *certainly* in the translation unit
    ///
    /// A `Conditional` edge is a file the preprocessor may never include, and requiring its summary is a wait that
    /// may have no end. Measured on neovim (`arglist.c`, a warm `.cppls` cache): `pending() == 0`,
    /// [`Session::is_locally_ready`] `true`, and the closure held exactly two unread files —
    /// `<windows kits>/um/winbase.inl` and `winuser.inl`, both reached through a **conditional** `#include` and never
    /// read by anything. So `is_closure_ready` was `false` and `semanticTokens` answered `null` **for every source
    /// file of the project**, in every session, no matter how long it had been open — a colouring that never starts.
    ///
    /// The names such a file declares are exactly the ones the classifier already reports as uncertain (the fact
    /// carries its guard), so leaving them out of the readiness question costs nothing that was ever certain.
    pub fn is_closure_ready(&self, path: &Path) -> bool {
        if !self.store.index().summary(path).is_some() {
            return false;
        }

        self.store
            .index()
            .visible_files(path)
            .iter()
            .all(|(file, visibility)| {
                *visibility != crate::IncludeVisibility::Unconditional
                    || self.store.index().summary(Path::new(file)).is_some()
            })
    }

    /// **Can a question about this file be answered from what has been read?** — the question a *local* request asks.
    ///
    /// [`Session::pending`] is the project's queue, and it is not this question. What a name means, where it is
    /// declared, what a call's signature is: each of those depends on the file and on the headers it includes, and on
    /// nothing else in the project. A project of five hundred sources whose *other* files are still queued is fully
    /// able to answer about the one the editor has open.
    ///
    /// The measured cost of the confusion: opening a real project queued 1122 files, and every
    /// local request was refused until the last of them — including a second pass that re-reads 773
    /// files at about half a second each, so the refusal lasted **minutes** after the answers were
    /// already available.
    ///
    /// The asymmetry with [`Session::pending`] is deliberate and is the whole of this method: a **project-wide**
    /// question — a reference search, a rename, a workspace symbol — genuinely needs coverage, and those handlers keep
    /// asking [`Session::pending`]. A **local** one does not.
    pub fn is_locally_ready(&self, path: &Path) -> bool {
        self.is_indexed(path) && !self.needs_catching_up(path)
    }

    /// Work until there is nothing left, and say how many files were read.
    ///
    /// Chunked rather than one `advance(usize::MAX)`: the steps of a whole project are a `Vec` nobody wants, and a
    /// caller that wants progress wants it per chunk.
    ///
    /// **Both kinds of work**, which is [`Session::pending_work`]'s count rather than [`Session::is_idle`]'s: a caller
    /// asking for "everything" wants the cooked readings too, and a test that read one and not the other would be
    /// asserting about the parts it happened to wait for.
    ///
    /// # Why the cooking backlog is drained here, and why that is not an optimisation
    ///
    /// `advance` only **marks** files for cooking — `want_the_closure_cooked` runs at the bottom of every slice
    /// whose index queue drained. The draining is [`Session::claim_cooking`] / [`Session::materials_for`] /
    /// [`Session::render_them`] / [`Session::commit_them`], which in production is the pump's loop and here has to
    /// be this one. Without it `pending_work` never falls to zero: the marking is idempotent, so the backlog sits
    /// at a fixed size for ever and this loop spins — **a hang, not a slowdown**, and the reason the analysis
    /// crate's test suite had to be killed rather than run.
    ///
    /// # Why it is a progress test rather than `pending_work() > 0`
    ///
    /// Because a file the index cannot describe can never be cooked, and `want_the_closure_cooked` marks it again
    /// on every slice. That case is real and is recorded at that function ("a queue that refills with files that
    /// have readings never empties… a hang, not a slowdown"). One round that neither read a file nor cooked one is
    /// the end of the work there is.
    /// **The cache database, one generation later** — called once by [`Session::open`] and by tests that open a
    /// store directly, so a collection can tell this run's rows from the last one's.
    pub fn begin_the_cache_generation(&mut self) {
        self.store.begin_generation();
    }

    pub fn index_everything(&mut self) -> usize {
        let mut total = 0;
        let tracing = crate::probes::cooking();

        loop {
            let round_started = std::time::Instant::now();
            let steps = self.advance(64);
            let advanced = round_started.elapsed();
            // **Every file a batch read names is a file to refine.** This is the review's lever: a request asks
            // (`catch_up`), and a batch caller that just read the whole project asks for the whole project. It was held
            // back while the refined reading was weaker than the raw one; the refinement now files its rendering as the
            // file's **cooked** facts (L2) and leaves the summary alone (L1), which is what the design's §2.1 says.
            //
            // # P2 wants this loop gone, and the three tests that stand in its way are named here
            //
            // Making a batch stop asking is a change of *contract* — "index everything" would mean the index and not a
            // compiler's reading of every file — and it was implemented and reverted in one round. What it costs:
            // **659 ms of a 1.54 s `import std;`**, four slices of 13–16 files rendered through a shared timeline, three
            // of which report `0 bodied name(s)` and render anyway. What blocked it, exactly:
            //
            // * `a_refinement_is_served_to_the_next_session` — its second session asserted `re_read_backlog() == 0`
            //   after `index_everything`. Under the new contract the backlog is legitimately 2, and the subject it is
            //   named for (the refinement is *served*) needs an explicit `want_refinement` in both sessions. Two lines,
            //   and the updated form passes;
            // * `a_keystroke_in_a_body_reads_the_file_through_its_timeline` — its setup asserted that the first reading
            //   of a file is the timeline's, which the eager ask is what produced. One `want_refinement` before
            //   `index_everything` keeps the subject;
            // * `a_file_read_back_through_its_own_unit_is_not_read_again_by_the_second_pass` — **not** fixed by an ask.
            //   After an edit it asserted the second pass has nothing to repair, and with the eager loop gone the edited
            //   file is queued again even though it was read with its environment. Either the edit path stops queueing
            //   environment-read files, or this assertion is about a coupling that has to be understood first.
            //
            // So the next attempt starts at that third test, not at this loop.
            //
            // # What that third test turned out to be
            //
            // Not a coupling. `advance` removes nothing from `parsed_since_the_last_pass`, so a file read **as text**
            // cold stays queued after a later edit reads it **through its timeline** — and the filter below, which keeps
            // an environment-read file off the queue, only applies to the slice in hand. Traced: the same `/p/api.h`
            // queued twice, `outcome Built environment false` and then `outcome Unstored environment true`. Under the
            // eager contract this is invisible, because the pass drains the queue anyway. Under the demand-driven one
            // the stale entry sits there for ever, and the test's "has the pass anything to repair" answered yes about
            // a file whose last read had nothing left to repair.
            //
            // The repair is one line — remove from the queue every file this slice read with its environment — and it
            // was tried. It makes that test pass and **breaks two others**, for the reason the eager contract exists:
            // `a_refinement_is_served_to_the_next_session` and
            // `a_keystroke_in_a_body_reads_the_file_through_its_timeline` both get their refinement *because* the file
            // is in the queue when the pass drains it; take it out and there is no first reading to cache or to assert.
            // So the repair belongs to the same change as the loop's removal, and neither lands alone.
            //
            // # Attempt two, and where it stopped
            //
            // Both were landed together and the five tests that depend on the eager ask were updated to ask
            // (`want_refinement`) for what they are about: `a_refinement_is_served_to_the_next_session`,
            // `a_keystroke_in_a_body_reads_the_file_through_its_timeline`, `a_file_read_back_through_its_own_unit…`,
            // `a_refined_reading_is_asked_for_again_after_an_edit` and both `stored_refinement` fixtures. All six passed.
            // The seventh did not: **`a_project_with_no_build_system_is_read_with_the_compilers_environment`**
            // (`tests/toolchain.rs`) reads a file the session discovers rather than one the test names, and an ask for
            // it — issued before `index_everything` — produced no cooked reading at all. That is a question about *when*
            // an ask can be served (the take happens only in an idle round, and a file discovered mid-drain may be
            // taken before its summary exists), not about this loop.
            //
            // So the next attempt starts there: an ask has to survive a file it cannot be served for yet.
            //
            // # Attempt three: the ask survives, and the fixtures still disagree
            //
            // `take_the_files_wanted_for_refinement` was fixed to leave an ask standing until its file has a summary.
            // With that, removing this loop and repairing the queue in `finish_a_slice` gets **eight of the nine**
            // dependants green: both session tests, the keystroke test, the unit-read-back test, `refinement.rs`'s
            // re-ask test, `toolchain.rs`'s discovered-file test (which is what the fix was for) and the two ordinary
            // suites. `tests/stored_refinement.rs` was the ninth, and it was **not** a fixture problem: its ask had not
            // been applied (a shell edit that did not match), and with it `is_refined(widget)` is true on the first
            // round after `index_everything` — measured: `refined true wanted 0 backlog 0 pending 0`. So every
            // dependant of the eager ask is a dependant of *being asked*, and the loop goes.
            let _ = &steps;
            let refined = round_started.elapsed() - advanced;
            let read = steps.len();
            // **Not asked for here, and the reason is the next defect rather than this call.** Every file this pass
            // reads would be a file to refine, which is what "index everything" sounds like it means — and it makes four
            // session tests pass. It also makes the pass **refine a fixture's own file**, and three `checks` tests then
            // see a different reading of it (`c_files_are_not_told_to_write_cpp_style`: `UseNullptr` stops firing, because
            // the re-read of a one-file fixture produces a reading the checks decline). That is a defect in the
            // **refined reading** — the same family as the C++ corpus at 41/52 — so the ask waits for it. A *request*
            // still asks (`catch_up`), which is the contract; a batch "index everything" does not, and the tests that
            // need it are listed in `docs/indexing-review.md` §4.
            total += read;

            // **Every stage of the cook is counted, because a file can be claimed and then lost.**
            //
            // `claim_cooking` pops, and the three stages after it each drop a file by answering `None` —
            // `materials_for` when a file's unit is provisional or its summary is not in the index yet, `render_them`
            // when the render itself declines, and `commit_them` when there is nothing to insert. A pop followed by a
            // drop is a file that is **no longer queued and never cooked**, and the loop below reads that state as
            // "nothing left to do": `read == 0 && cooked == 0` ends the work with a cooking backlog still standing.
            // Measured on the completion fixture: `pending 0, cooking 22` on every round, for six rounds.
            let cook_started = std::time::Instant::now();
            let cooked = {
                let claimed = self.claim_cooking(COOK_SLICE);
                if tracing && !claimed.is_empty() {
                    eprintln!("cooking: claimed {} file(s) {:?}", claimed.len(), claimed);
                }
                let materials = self.materials_for(claimed);
                let wanted = materials.len();
                let rendered = self.render_them(materials);
                let built = rendered.len();
                let committed = self.commit_them(rendered).len();
                if tracing && (built != wanted || committed != built) {
                    eprintln!(
                        "cooking: rendered {built} of {wanted} claimed, committed {committed} \
                         (cooking backlog {})",
                        self.pending_cooking()
                    );
                }
                committed
            };
            let cook = cook_started.elapsed();
            if crate::probes::units() {
                // **Three timers, each taken where it happened** rather than one derived by subtraction: the round
                // that appeared to take 56 s had an `advance` of 7 ms and a cook of 1 µs, so the 56 s was in neither
                // and the derived figure could not say where. The three now print their own walls.
                eprintln!(
                    "DRAIN round: advance {advanced:?} ({read} file(s)), refine {refined:?}, cook {cook:?} ({cooked} committed), round total {:?}",
                    round_started.elapsed(),
                );
            }

            // **The second pass is work, and this loop used to return while it was owed.**
            // `advance` drains up to [`RE_READ_SLICE`] of the backlog per call, and the backlog is
            // filled by a cold start; the loop below ended at `read 0, cooked 0`, which is exactly
            // the state a session is in while it still owes re-reads. Measured on
            // `EmmyLuaCodeStyle` (cold summary cache): `index_everything` returned with **1 205
            // files** owed, 302 slices and **27.3 s** of work left — and every one of those slices
            // reported `nothing bodied`, so the value it bought was not visible in any answer. The
            // cost *was* visible: the next keystroke's pump slice took four of those files and four
            // unit walks, and typing was **65 ms instead of 10** for about three hundred
            // keystrokes. A caller that says "index everything" is owed this too. **The second pass
            // is owed, and this loop does not pay it.** The pump already drains it, four files a
            // slice, and `pending_work` counts it, so the work still happens; what the loop bought
            // was paying for it *before* the session answered anything.
            if read == 0 && cooked == 0 {
                if tracing {
                    // **The cache as the pump leaves it.** If this is non-zero and a request still misses, the key
                    // the writer used and the key the reader computes are two different keys — which is the one
                    // failure a count cannot show and the two hashes beside it can.
                    let cached: Vec<String> = self
                        .renderings
                        .lock()
                        .map(|known| {
                            known
                                .keys()
                                .map(|(path, hash)| format!("{}#{hash}", path.display()))
                                .collect()
                        })
                        .unwrap_or_default();
                    eprintln!(
                        "cooking: stopping with read 0, cooked 0, backlog {} | cache {cached:?}",
                        self.pending_cooking()
                    );
                }
                // **The idle step, and this is the idle.** A session that has just finished indexing is the one
                // moment when collecting the cache is free of a reader: rows nothing references (a file that changed
                // while the last run was writing it, a deleted file, a summary from a previous generation) are
                // dropped, and the file is compacted — measured, a collection left a database *larger* than it
                // started (16.6 MB → 24.6 MB) until compaction returned the pages.
                //
                // Once per call rather than once per loop: `index_everything` is the "index it all" entry point, and
                // a caller that calls it twice is asking twice.
                if let Some(collected) = self.collect_the_cache() {
                    log::debug!(
                        "cache: collected {} summary(ies), {} rendering(s), {} projection(s)",
                        collected.summaries,
                        collected.renderings,
                        collected.projections
                    );
                }
                // **And the memory budget, at the same idle moment.** A project whose summaries exceed
                // [`SUMMARY_BUDGET_BYTES`] forgets the largest of the files nobody is looking at; the ones it forgets
                // are re-read on demand, which is the path a cache miss already takes.
                let forgotten = self.enforce_the_summary_budget(SUMMARY_BUDGET_BYTES);
                if forgotten > 0 {
                    log::debug!("index: forgot {forgotten} summary(ies) over the byte budget");
                }
                return total;
            }
        }
    }

    /// How many files are queued and not yet indexed.
    ///
    /// Counted by **file**, not by queue entry: a path that was discovered once and opened later sits in two places
    /// and is one file to read.
    ///
    /// **The index queue only.** The cooking backlog is [`Session::pending_cooking`], and a caller that wants to
    /// know whether the session has any work at all — a pump deciding whether to keep going — wants their sum,
    /// [`Session::pending_work`].
    pub fn pending(&self) -> usize {
        self.queue.pending()
    }

    /// **Everything the session has left to do**: the index queue, the cooking backlog, and the second pass.
    ///
    /// What a pump loops on. A caller looping on [`Session::pending`] alone would stop while the files it has open
    /// still have no cooked reading — which is the state the closure marking exists to leave, not the state a
    /// finished session is in.
    ///
    /// **The second pass is in here, and it was not.** A cold start parses a file before its includes are in the
    /// index, so its reading has to be built again once the closure is in hand — the backlog
    /// [`Session::re_read_backlog`] counts. That backlog used to be invisible to this count, so a pump stopped with
    /// **1 205 files and 27 s still owed** and the next keystroke's slice paid for four of them: typing measured
    /// **65 ms a keystroke instead of 10** until the debt was gone. A session that owes a re-read is not idle.
    ///
    /// The visibility gate that reads this — `checks_may_be_trusted` in the server, which withholds check findings
    /// until the index has settled — therefore withholds them until the second pass has run. That is the same
    /// argument that put the cooking backlog here: a summary the pass is about to revise is not a summary a check may
    /// call a fact.
    pub fn pending_work(&self) -> usize {
        self.queue.pending() + self.cooking.len() + self.refinements_wanted()
    }

    /// Is everything the session knows about already **read**?
    ///
    /// **The indexing queue's question, not [`Session::pending`]'s**: the two differ by the cooking backlog, and the
    /// difference is load-bearing. A file's cooked reading is built after its includes have been read (their macros
    /// are its environment), and one more thing happens at that same moment — the second parse of a file whose
    /// meaning depended on a header that arrived later. Cooking before that pass would build a reading of an
    /// environment the summaries are about to revise.
    /// **What the queue physically holds against what it says is pending** — a diagnostic, and the measurement behind
    /// §6 of `docs/buglist.md`.
    ///
    /// `pending()` is the number of paths whose standing is `Queued`; the deques also hold entries that were re-armed
    /// into the other half, upgraded, forgotten or worked — `pop` walks them and skips them, and the two numbers
    /// drifting apart is what makes one `advance` step take seconds while `index_one` itself takes milliseconds.
    ///
    /// `(pending, open deque, rest deque)`.
    pub fn queue_sizes(&self) -> (usize, usize, usize) {
        (
            self.queue.pending(),
            self.queue.open.len(),
            self.queue.rest.len(),
        )
    }

    pub fn is_idle(&self) -> bool {
        self.queue.pending() == 0
    }

    /// How many files are waiting to be **cooked** — the half of [`Session::pending`] that is not indexing.
    ///
    /// A caller reporting progress wants the two apart ("63 files to read" and "9 of them read as a compiler would"
    /// are different sentences), and a caller deciding whether it may stop pumping wants the sum.
    pub fn pending_cooking(&self) -> usize {
        self.cooking.len()
    }

    /// **How many files the second pass has still to re-read** — the backlog `advance` drains four at a time.
    ///
    /// A third kind of pending work, and the one no count used to name: [`Session::pending`] is the
    /// index queue and [`Session::pending_cooking`] the cook queue, while the
    /// `parsed_since_the_last_pass` backlog is drained only by a slice's tail and only while the
    /// index queue is empty. Measured on `EmmyLuaCodeStyle` right after `index_everything`
    /// returned, on a cold summary cache: **1 205 files**, which took **302 slices and 27.3 s** to
    /// clear — and every one of those slices reported `nothing bodied`, so the whole tail rebuilt
    /// no summary at all. Until it was cleared, the next keystroke's slice paid for four of those
    /// files and four unit walks: measured **65 ms a keystroke** instead of 10, for about three
    /// hundred keystrokes.
    pub fn re_read_backlog(&self) -> usize {
        self.parsed_since_the_last_pass.len()
    }

    /// **How many files are waiting because somebody asked for them** — the only re-reads a start does not defer.
    pub fn refinements_wanted(&self) -> usize {
        self.refined_on_request
            .lock()
            .map(|wanted| wanted.len())
            .unwrap_or(0)
    }

    /// **Ask for a file's reading to be refined** — what an open or a request does.
    ///
    /// It used to answer only for a file still in `parsed_since_the_last_pass`, on the argument that a file outside it
    /// "has either been refined already or never read". **Both halves of that were wrong**, and the cost was the whole
    /// capability: the pump drains that backlog before a request ever arrives, so every ask was refused, `advance`'s pass
    /// ran on an empty list, and no summary was ever refined. Measured on the C++ corpus: **51/52 type answers before
    /// phase 0, 41/52 after**, with the failing test naming the mechanism
    /// (`a_class_scoped_by_another_files_macro_body_is_scoped_through_the_session`, `Widget` → `NotDeclaredHere`).
    ///
    /// A file that `is_refined` is the one real no-op: asking again would re-read a summary that already has its
    /// environment. Everything else is recorded, and the pump refines it once — `take_the_files_wanted_for_refinement`
    /// removes the ask, so an ask is served once.
    pub fn want_refinement(&self, path: &Path) {
        if self.is_refined(path) {
            return;
        }
        if let Ok(mut wanted) = self.refined_on_request.lock() {
            wanted.insert(path.to_path_buf());
        }
    }

    /// **Take the files that were asked for**, at most `at_most`, and drop them from the backlog.
    ///
    /// The asks are the queue, not the backlog: a file a request named is refined wherever it is, which is what makes
    /// "answer from L1, ask for L2" reachable after a start has drained. A file that is *also* still in
    /// `parsed_since_the_last_pass` is taken out of it, so one file is re-read once.
    ///
    /// # An ask the pass cannot serve is left standing
    ///
    /// The take happens in an **idle** round — `finish_a_slice` calls this when the index queue is empty — but a file
    /// can be asked about before its summary exists: a request for a file the drain has not reached, or one the session
    /// only discovers while indexing. Handing that to the pass consumes the ask and buys nothing, because a re-read
    /// needs a summary to re-read — and then nothing ever asks again.
    ///
    /// That is not hypothetical: it is what stopped the demand-driven contract
    /// (`docs`: the round that removed `index_everything`'s eager ask). `tests/toolchain.rs`'s
    /// `a_project_with_no_build_system_is_read_with_the_compilers_environment` reads a file the session *discovers*,
    /// asked for it before `index_everything`, and got no cooked reading at all — the ask had been consumed by a round
    /// that could not serve it. Left standing, it is served by the round that can.
    pub(crate) fn take_the_files_wanted_for_refinement(&mut self, at_most: usize) -> Vec<PathBuf> {
        let mut taken = Vec::new();
        let Ok(mut wanted) = self.refined_on_request.lock() else {
            return taken;
        };
        // Asks that cannot be served **yet**: out of the set so the loop advances past them, and back in before this
        // returns. Without the round trip one unready ask would block every ready one behind it; without the put-back
        // it would be dropped, which is the defect above.
        let mut not_yet: Vec<PathBuf> = Vec::new();
        while taken.len() < at_most {
            let Some(path) = wanted.iter().next().cloned() else {
                break;
            };
            wanted.remove(&path);
            if self.store.index().summary(&path).is_none() {
                if crate::probes::cooking() {
                    eprintln!("refinement ask not yet: {} (no summary)", path.display());
                }
                not_yet.push(path);
                continue;
            }
            if crate::probes::cooking() {
                eprintln!("refinement ask taken: {}", path.display());
            }
            if let Some(at) = self
                .parsed_since_the_last_pass
                .iter()
                .position(|queued| queued == &path)
            {
                self.parsed_since_the_last_pass.swap_remove(at);
            }
            taken.push(path);
        }
        for path in not_yet {
            wanted.insert(path);
        }
        taken
    }

    // **Drop everything the cook queue is waiting for, and say how much it was.**
    //

    /// **Is this file`s summary about to be revised?** — the second pass has it queued for a re-read.
    ///
    /// A summary parsed before its include closure was indexed has no macro environment, so the facts in it are the
    /// shape-only reading: `_STD` is a name rather than `::std::`, and a class declared under a macro is at the wrong
    /// scope. The second pass repairs that, and until it runs, a **check** reading this summary is reading an answer
    /// that is about to change.
    pub fn is_owed_a_re_read(&self, path: &Path) -> bool {
        let key = crate::file::paths::normalize_path(path, cfg!(windows));
        self.parsed_since_the_last_pass
            .iter()
            .any(|queued| crate::file::paths::normalize_path(queued, cfg!(windows)) == key)
    }

    /// **Does anything this file can see wait for the second pass?** — the closure question behind
    /// [`Session::is_owed_a_re_read`].
    ///
    /// A check about this file resolves names through the headers it includes, so a provisional summary among those
    /// is as misleading as a provisional summary of the file itself. The walk is [`ProjectIndex::visible_files`],
    /// which is the memoized one every cross-file query already pays.
    pub fn its_closure_is_owed(&self, path: &Path) -> bool {
        if self.parsed_since_the_last_pass.is_empty() {
            return false;
        }
        if self.is_owed_a_re_read(path) {
            return true;
        }
        self.store
            .index()
            .visible_files(path)
            .iter()
            .any(|(file, _)| self.is_owed_a_re_read(Path::new(file)))
    }
    /// For the pump`s breaker: a file the cook claims and cannot build leaves the queue`s count where it was, and
    /// a loop that keeps taking it never goes idle (measured once as `2 to cook, 0 left` once a second for as long
    /// as a window stayed open). Dropping is safe rather than lossy: every query that needs one of these files asks
    /// for it again (`want_cooked_reading`, [`Session::want_the_closure_cooked`]), and the next drain takes it with
    /// whatever has changed since.
    ///
    /// Nothing is forgotten about the *files* — only the want. A dropped file that could be built will be built the
    /// next time something names it, and one that could not will not spin the loop meanwhile.
    pub fn abandon_the_cooking_queue(&mut self) -> usize {
        self.cooking.abandon()
    }

    /// **Write the sharing cache into the project's database**, so that the next run starts warm — §8 step 4.
    ///
    /// It is a table rather than a `<cache>/sharing.bin` beside the database, and that is a deliberate move: the file
    /// was written whole and decoded whole, so a start paid for every entry before it could answer anything, and an
    /// interrupted write lost the whole cache. The rows carry their own identity (the file's fingerprint and the
    /// projection of its conditions), so a stale row cannot answer a different question — it simply fails to hit.
    ///
    /// `Err` only when there is no database to write to: a session with the cache turned off is not a failure, but a
    /// caller that asked to save something is owed the difference. `0` entries is the honest answer for a run that
    /// walked nothing.
    pub fn save_the_sharing(&self) -> std::io::Result<usize> {
        if self.store.database().is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "this project has no summary database to write the sharing cache into",
            ));
        }
        Ok(self.store.save_the_contributions())
    }

    /// **Read the sharing cache back** from the project's database.
    ///
    /// A missing table, an unreadable row or a byte this version cannot decode is a cold start, not an error: the
    /// cache is an accelerator and the walk is always correct without it. Say how many entries were restored, because
    /// "it loaded nothing" and "it did not run" are different facts and only one of them is interesting.
    pub fn load_the_sharing(&self) -> usize {
        self.store.load_the_contributions()
    }

    /// Is this file's summary in the index — the question [`Session::pending`] answers for the whole project.
    pub fn is_indexed(&self, path: impl AsRef<Path>) -> bool {
        self.store.index().summary(path.as_ref()).is_some()
    }

    // ---------------------------------------------------------------------------------------------
    // The questions
    // ---------------------------------------------------------------------------------------------
}
