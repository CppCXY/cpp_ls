//! **Units** — a translation unit read as a program, the timelines that decide which one is current, and the budget
//! that keeps a program read from costing a keystroke.
//!
//! Moved here from `session/mod.rs` without a change. A unit is keyed by its *inputs* (the closure, the environment,
//! the flags), which is what makes a cached unit reusable across runs and what makes a stale one detectable.

use super::*;

impl<F: FileProvider + Clone> Session<F> {
    /// **The query entry point for one file** — a [`crate::sema::model::SemanticModel`] over `view`, with the
    /// session as its resolver.
    ///
    /// This is the thing the four `type_of_expression` call sites were missing, and the difference is one closure:
    ///
    /// ```text
    ///   before   type_of_expression(index, &mut |_: &Path| None, …)   inference gives up at the first header
    ///   now      session.semantic_model(&view)                         inference can read that header's tree
    /// ```
    ///
    /// # Why it takes only `&self`, and why the memo is still sound
    ///
    /// The resolver it installs *is* the session: the model may need another file's tree, and obtaining one is
    /// [`Session::view_of_the_file`] — a read through the VFS, and a parse. That is a **read**, so the model borrows
    /// the session shared, which is what lets a caller hold the model and still ask the session other questions
    /// beside it. The first version took `&mut self` on the theory that a parse is a mutation; it is not — the VFS
    /// is already behind its own lock — and the mutable borrow bought nothing while forbidding the model and its
    /// session from being used together.
    ///
    /// The memo is sound for a different reason than the borrow: its answers are keyed by node ranges in `view`,
    /// and `view` is the caller's — an edit replaces the view rather than mutating it, so a model cannot outlive the
    /// text its answers are about. There is no invalidation rule to write because there is nothing to invalidate.
    pub fn semantic_model<'s>(
        &'s self,
        view: &'s FileView,
    ) -> crate::sema::model::SemanticModel<'s> {
        // **The resolver: a path in, that file's own reading out.** `view_of_the_file` and not `view`, because a
        // declaration's type is written in the file it was written in — this file's *rendering* is not that file's
        // tree, and answering a question about `<vector>` with a rendering of the file being edited would answer in
        // the wrong coordinates.
        //
        // The session is reached through a **shared** reborrow, and that is what lets the model hold both at once:
        // the resolver only needs `&Session` (obtaining a file's tree is a read of the VFS and a parse, not a
        // mutation of the analysis), so it does not conflict with the index reference taken beside it.
        let session: &'s Session<F> = self;
        let index: &'s crate::ProjectIndex = session.store.index();

        // **The views this model has already been given, by path.** The resolver parses the file it is asked about,
        // and one diagnostic pass asks about the same few headers again and again: measured on EmmyLuaCodeStyle's
        // `LuaParser/src/File/LuaSource.cpp`, `type_of` took **55 ms** per expression and the parse of the header
        // that declares `std::size_t` was most of it — repeated for every expression, because the view was dropped
        // after the answer.
        //
        // Kept **here and not in the session**: a `FileView` holds rowan nodes, which are not `Send`, and a session
        // is shared across threads. This closure lives as long as the model — one diagnostic pass — which is exactly
        // the lifetime in which the repetition happens, and no other thread can see these readings.
        let seen: std::cell::RefCell<std::collections::HashMap<PathBuf, Option<FileView>>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
        let resolver = Box::new(move |path: &Path| -> Option<FileView> {
            if let Some(known) = seen.borrow().get(path) {
                return known.clone();
            }
            let started = std::time::Instant::now();
            let made = session.view_of_the_file(path);
            if false {
                eprintln!(
                    "[resolver] {:>5} ms  {} ({})",
                    started.elapsed().as_millis(),
                    path.file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string()),
                    if made.is_some() { "read" } else { "none" }
                );
            }
            seen.borrow_mut().insert(path.to_path_buf(), made.clone());
            made
        });

        crate::sema::model::SemanticModel::new(
            index,
            &view.root,
            &view.scopes,
            &view.source,
            &view.path,
            resolver,
        )
    }

    /// The text the analysis reads for a path — the buffer when it is open, the file otherwise.
    ///
    /// [`Session::view`] without the parse, for a consumer that wants the text rather than the tree: a hover that
    /// shows a declaration the cursor is not in, a search over a file the index already holds. `None` when there is
    /// neither a buffer nor a readable file.
    ///
    /// Read through the VFS, so the file is held afterwards and a second question about it — its lines, its text —
    /// costs nothing.
    pub fn text(&self, path: impl AsRef<Path>) -> Option<String> {
        Some(self.vfs.held(path)?.text.to_string())
    }

    /// Read a file in and hold it, answering with the id the VFS gave it.
    ///
    /// The **writer** path's way to hold a file: a notification, an indexing step, a caller that knows it is about
    /// to ask several questions about one file. After this, [Session::view] and [Session::text] answer for it.
    pub fn load(&mut self, path: impl AsRef<Path>) -> Option<crate::FileId> {
        self.vfs.load(path)
    }

    /// The files the session is holding: their text and their line indexes.
    ///
    /// Exposed because "which text is this analysis working from" is a question a caller diagnosing a wrong answer
    /// asks, and because it is the one place that knows how much of a project has been read *as text* rather than
    /// as facts.
    pub fn files(&self) -> &Vfs<SessionFiles<F>> {
        &self.vfs
    }

    /// **The translation unit `path` belongs to**, walked once and then reused.
    ///
    /// # Why this is the whole point of plan A
    ///
    /// A file's macro environment is a *position in one walk of the unit*
    /// ([`crate::TranslationUnit`]): the walk visits every file the unit enters, records where each was entered, and
    /// every later question — "what is `_STD_BEGIN` in `<xstring>`" — is a lookup in that timeline rather than a
    /// closure walk of its own. Measured before this existed (the census, 255 files of the Windows SDK):
    /// **17 618 794 conditional facts evaluated, 147 s of a 174 s run**, because each file's environment was built
    /// as if it were the only one.
    ///
    /// [`Session::cook`] built one anyway — *per cooked file* — so cooking the 138 files of one real project's
    /// closure walked that closure 138 times. Three levels of reuse, cheapest first:
    ///
    /// ```text
    /// this session, this unit        the in-memory map below          — no walk, no read, no decode
    /// a previous run, same content   TranslationUnitCache on disk     — a read and a hash of the closure
    /// otherwise                      the walk, once                   — and it is written for the next run
    /// ```
    ///
    /// # What invalidates it
    ///
    /// ```text
    /// the in-memory map    this session's own edits — and only the edited file's entry (Session::buffer_changed)
    /// the disk entry       whatever RootKey says, which for a file being typed into is its *preamble*
    /// ```
    ///
    /// The disk rule is the one that used to be wrong. It hashed every file of the closure whole, the root
    /// included, so **the file a person was typing into could never be reused** — its own hash moved on every
    /// keystroke — and every cook of it paid a walk of its whole closure. See [`crate::tu_cache::RootKey`]: the
    /// root is keyed on everything a walk can read out of it, which its body is not.
    ///
    /// **The timeline of one program** — public for the probes and tests that measure a reading by its environment
    /// rather than by a request. A caller that merely wants an answer wants a query instead: this walks (or loads)
    /// the whole unit, which is the most expensive thing this crate does.
    pub fn translation_unit_of(
        &self,
        path: &Path,
    ) -> Option<std::sync::Arc<crate::TranslationUnit>> {
        self.unit_and_state_of(path).map(|(unit, _)| unit)
    }

    /// [`Session::translation_unit_of`], and **whether the timeline is of the whole program** — see [`HeldUnit`].
    ///
    /// The flag is needed by the one caller that must not use a provisional timeline at all:
    /// [`Session::cooking_materials`], because a rendering is filed and never revisited, where a timeline is
    /// re-walked the moment the queue drains.
    pub(crate) fn unit_and_state_of(
        &self,
        path: &Path,
    ) -> Option<(std::sync::Arc<crate::TranslationUnit>, bool)> {
        let key = queue_key(path);

        // **The guard is bound and dropped before the body**, not held across it: `held_units()` is a `Mutex`, and
        // a `MutexGuard` temporary in an `if let` scrutinee lives until the whole statement ends — so taking it
        // again inside the body (the `remove` below) is a re-entrant lock on one thread, which is a deadlock and
        // not an error the type can report.
        let held = self.held_units().get(&key);
        if let Some((provisional, unit)) = held {
            // **A provisional timeline is dropped once there is nothing left to arrive.** It was walked while a
            // file its closure needed was still queued; if the queue has since drained, that file has been read
            // and this is a timeline of a smaller program — see [`HeldUnit`]. Until then it is served, because a
            // reading of most of the program is what the index has and the alternative is a walk per step.
            if !provisional || self.queue.pending() > 0 {
                self.note_a_unit(Which::FromMemory);
                return Some((unit, provisional));
            }
            self.held_units().remove(&key);
        }

        let context = self.store.context_hash(path);
        // nothing is read from a timeline cache — see `cached` below
        // (`root_key_of` is gone with the timeline cache — see the note at `cached` below)

        // Read out of the cache under a timer, then matched: a stage timer inside a `match` scrutinee is a block
        // clippy asks to hoist, and hoisting it is what the timer wants anyway — it should cover the read only.
        // **No timeline cache on disk.** `tu/` stored built `TranslationUnit`s and was the last of the old
        // per-directory files (`cache.redb` holds the summaries and the unit renderings now). A timeline is
        // cheap to rebuild — 38 ms — and rebuilding it is the only way its environment is guaranteed to be the one
        // the current index describes; a stored one outlives the files it was built from. So the walk runs.
        let cached: Option<crate::TranslationUnit> = None;

        let (unit, provisional) = match cached {
            Some(unit) => {
                self.note_a_unit(Which::FromDisk);
                // An entry on disk is never provisional: `put` refuses to write one. See [`HeldUnit`].
                //
                // Boxed into an `Arc` because both arms of this match must agree, and the walk arm shares its value
                // with every later root whose closure is the same — see [`Session::walked_by_content`].
                (std::sync::Arc::new(unit), false)
            }
            None => {
                // **The file the index cannot describe in full, and the order that answers it.**
                //
                // A file a reader is editing has no summary — [`Session::did_change`] drops it — so
                // the walk has no root to start from, and until this existed the file's timeline
                // was unobtainable exactly while the reader was working on it. Its **directives**
                // are the part of it that can be read without a parse, and they are also the part
                // the walk needs first, because they name the includes.
                //
                // The fallback is for a file a reader is looking at, and only for one: a directive reading is a lex
                // and a directive scan, which is cheap but not free, and the project's other ten thousand files have
                // no claim on it — they are read as text and refined by the second pass.
                let owned_root;
                let index = self.store.index();
                let root = match index.summary(path) {
                    Some(summary) => summary,
                    None if self.documents.is_open(path) => {
                        owned_root = self.store.directive_summary(path)?;
                        // **A timeline of the whole program or none at all.** A walk stops at a file it cannot look
                        // up and treats it as defining nothing, which is the right reading for a file outside the
                        // analysis and the wrong one for a file nobody has read yet: the rendering made from such a
                        // timeline is the macro left unexpanded, which is worse than the file's own text (see
                        // [`HeldUnit`]). A file whose program is not read yet is left to the raw path, and the second
                        // pass renders it once the closure is in hand.
                        self.the_closure_of_the_directives(path, &owned_root)?;
                        owned_root.as_ref()
                    }
                    None => return None,
                };

                let read_started = std::time::Instant::now();
                if false {
                    eprintln!("[unit] gathering the closure of {path:?}");
                }
                let (closure, closure_by_key) = { self.closure_with_text_from(path, Some(root)) };
                let read_took = read_started.elapsed();
                let closure_bytes: usize = closure.iter().map(|(_, text)| text.len()).sum();
                if false {
                    eprintln!(
                        "[unit] closure of {path:?}: {} file(s), {closure_bytes} bytes in {read_took:?}",
                        closure.len()
                    );
                }
                let walk_started = std::time::Instant::now();

                // **The session's own definition cache**, not a fresh one: the walk reads back every `#define` a
                // file sees, and the same definition reaches hundreds of files — `<vector>` and `<string>` both see
                // `_STD_BEGIN`, whose text is in `yvals_core.h`. One cache per *walk* is one parse per definition
                // per file, which the census measured at 12.2 s of a 40 s run. Invalidated **per path** when a file
                // changes, since the keys are offsets in that file.
                // **Held rather than taken**, for the reason the units table carries: this cache is what made the
                // unit walk need `&mut self`, and `&mut self` is what put the walk inside the writer. A lock of its
                // own is enough — the walk is the only thing that ever holds it for long.
                let mut definitions = self.held_definitions();

                // **Has this exact closure been walked already in this run?** A `TranslationUnit` is a function of
                // its closure's content and the context, so two roots that agree on both have the same timeline and
                // the second one is 68 s of guard evaluation for an answer already in hand. Measured on a real
                // project: 2110 roots, 786 distinct closures, **46% of walks removable**. See
                // [`Session::walked_by_content`].
                let identity = self.closure_identity(&closure, context);
                let shared = self
                    .walked_by_content
                    .lock()
                    .ok()
                    .and_then(|mut held| held.get(&identity).cloned());

                let (unit, provisional) = if let Some(shared) = shared {
                    // A shared timeline is never provisional: only a finished walk is ever stored, for the same
                    // reason the disk entry refuses a provisional one.
                    self.note_a_unit(Which::FromMemory);
                    (shared, false)
                } else {
                    // An `Arc` from the start, because a shared timeline hands the *same* value to every root that
                    // asks for it: `TranslationUnit` is not `Clone` and does not need to be — the units table has
                    // always held `Arc`s, and this is the same value reaching more callers.
                    // **Is the closure still growing?** Asked *before* the walk, because a unit whose closure can still
                    // grow is incomplete by construction: it must neither fill the sharing cache nor read from it.
                    let provisional = self.closure_is_still_growing(&closure, &closure_by_key);
                    let walked = std::sync::Arc::new({
                        // **The root's own summary comes from `root`, not from the index.** For a file the index
                        // cannot describe — the one a reader is editing, whose directives are all there is — the
                        // lookup would otherwise answer `None` for the very file the walk starts at, and a timeline
                        // with no root is a timeline of an empty program.
                        let root_key = queue_key(path);
                        // **The predefines as the file's language sees them** — see [	he_seed_for]: the probe is a
                        // C++ source, and a .c program read with __cplusplus takes every C++ branch in every
                        // system header (measured: 24 xtern "C++" blocks in Lua's ldebug.c stream, and 15
                        // parse errors after them).
                        let seed =
                            crate::session::the_marked_seed_for(&self.config, index.macros(), path);
                        crate::TranslationUnit::walk(
                            root,
                            |wanted| {
                                let (held, text) =
                                    closure.get(*closure_by_key.get(&queue_key(wanted))?)?;
                                let summary = if queue_key(held) == root_key {
                                    root
                                } else {
                                    index.summary(held)?
                                };
                                Some((summary, text.as_str()))
                            },
                            &seed,
                            &mut definitions,
                            // **Off, and this is why.** The sharing replays a file's cached contribution at the position
                            // it was recorded at in a *different* walk, and a preprocessor's positions are not portable
                            // between programs: measured on `import std;`, `<limits>` lost its `<cwchar>` and
                            // `<xstddef>` includes — the second defines the `_MEMBER_CALL_*` macros — so
                            // `<xtr1common>` was cooked without them and the rendering kept their invocations as text.
                            // `CPPLS_WALK_SHARE=1` turns it back on for an A/B measurement.
                            !provisional && self.share_contributions,
                            self.store.contributions(),
                        )
                        .with_the_command_spelling(
                            self.config
                                .presumed_name_for(path)
                                .map(std::path::Path::to_path_buf),
                        )
                    });
                    let walk_took = walk_started.elapsed();

                    // **Is this timeline of the whole program, or of as much of it as has been read?** A file the
                    // closure could not offer is either outside the analysis — the permanent reading, and the one
                    // the walk's own early return documents — or *not yet read*, which is a fact about the queue and
                    // not about the file. See [`HeldUnit`] for what storing the second kind cost.

                    let mut put_took = std::time::Duration::ZERO;
                    if !provisional {
                        // Written for the **next run**.
                        let put_started = std::time::Instant::now();
                        // (the disk cache is gone — see the note at `cached` above)
                        put_took = put_started.elapsed();

                        // …and offered to the **rest of this one**, under the same condition: a provisional
                        // timeline is one the queue will make obsolete, and handing it to a second root would
                        // spread the smaller program rather than save a walk.
                        //
                        // The weight is the timeline's own size, because that is what the budget is about: two
                        // roots of a header-heavy project differ in closure size by an order of magnitude and a
                        // count would treat them the same.
                        let weight = walked.heap_bytes();
                        if let Ok(mut held) = self.walked_by_content.lock() {
                            held.insert(identity, std::sync::Arc::clone(&walked), weight);
                        }
                    }

                    // **Counted here and not after the match**: reaching this arm is what a walk *is*, and a call
                    // that was served off the disk reaches the other one. Counting after the match counts both,
                    // which is the mistake this line was written with.
                    self.note_a_unit(Which::Walked);
                    // …and counted **by root**, which the total above cannot say. See [`Session::walks_by_root`].
                    if let Ok(mut walks) = self.walks_by_root.lock() {
                        *walks.entry(queue_key(path)).or_insert(0) += 1;
                    }

                    if false {
                        eprintln!(
                            "[unit] read {:>4} ms, walk {:>4} ms, put {:>4} ms, {} file(s) / {} bytes  {}",
                            read_took.as_millis(),
                            walk_took.as_millis(),
                            put_took.as_millis(),
                            closure.len(),
                            closure_bytes,
                            path.file_name()
                                .map(|name| name.to_string_lossy().to_string())
                                .unwrap_or_else(|| path.display().to_string())
                        );
                    }

                    (walked, provisional)
                };

                (unit, provisional)
            }
        };

        // Already an `Arc` from both arms of the match above — the units table has always held `Arc`s, and a shared
        // timeline is the same value reaching more than one root.
        self.held_units()
            .insert(key, std::sync::Arc::clone(&unit), provisional);
        Some((unit, provisional))
    }

    /// **Does this session hold a macro environment for `path` that is worth reading the file with?**
    ///
    /// Asked by [`Session::advance`] before it takes a file's summary out of the wave: a wave parses a file as a
    /// pure function of its text, which cannot place a scope that a macro in **another** file opens, and this
    /// session may already know better. See [`HeldUnit`] for why a provisional timeline does not count — it is
    /// missing whatever the queue has not read, so reading a file with it and marking the result would suppress the
    /// second pass for a reading that needs it.
    ///
    /// Asked by the index's own spelling of the path, because a unit's frames are keyed by the spelling the walk
    /// used — the same trap [`Session::units_for_the_pass`] records from the other side.
    pub(crate) fn holds_an_environment_for(&self, path: &Path) -> bool {
        let spelled = self
            .store
            .index()
            .summary(path)
            .map(|summary| summary.path.clone())
            .unwrap_or_else(|| path.to_path_buf());
        let held = self.held_units().get(&queue_key(&spelled));
        held.is_some_and(|(provisional, _)| !provisional)
    }

    /// **Will this file be read as the program it is in?** — the question a wave asks before it parses a file as
    /// text, and the union of the two ways a file gets a macro environment here.
    ///
    /// ```text
    /// the timeline is in hand                      the pump's ordinary case: a body edit keeps it
    /// it can be walked now — see below             what this closes: the unit was evicted, or this is the
    ///                                              first read after `did_open`
    /// ```
    ///
    /// [`Session::advance`] and [`Session::prepare_a_wave`] both ask it for every file they are about to hand to a
    /// wave, because a wave parses a file as a pure function of its text — which cannot place a scope that a macro
    /// in **another** file opens, and would then be *written to the disk cache* as the file's reading. A file this
    /// answers `true` for is left out of the wave and read by [`Session::index_one`] instead, where the timeline is.
    ///
    /// # A held timeline is worth it only for a file somebody is looking at
    ///
    /// `a_timeline_can_be_walked_for` already says so in its first condition, and `holds_an_environment_for` did
    /// not — and that asymmetry is what made a **batch** pay a program read per file. Measured on the modules
    /// fixture with `import std;`: the drain after the module unit was queued had nine `advance(64)` batches, seven
    /// of them 20–60 ms and one of them **45.9 s for 33 files** — the batch the pass had just built timelines for,
    /// so every one of them took this arm and `index_one` re-walked a closure for a file nobody had opened.
    ///
    /// The reading itself is not lost: a file read as text is refined by the second pass, which is the pipeline the
    /// rest of the project relies on, and a *request* for an open file still gets the timeline (`want_cooked_reading`
    /// is what a request calls).
    pub(crate) fn will_read_it_as_the_program(&self, path: &Path) -> bool {
        if !self.documents.is_open(path) {
            return false;
        }
        self.holds_an_environment_for(path) || self.a_timeline_can_be_walked_for(path)
    }

    /// **Can a timeline for this file be walked right now, without one being in hand?**
    ///
    /// Three conditions, and each is one of the costs this has to be worth:
    ///
    /// * **A reader is looking at it.** A walk reads the whole closure and evaluates every conditional in it — 38 ms
    ///   on a real project's — and it is worth that for the file somebody is typing in and for no other. The rest of
    ///   the project is read as text and refined by the second pass, which is what the pipeline already does.
    /// * **It has no summary, or its whole closure is read.** A file with no summary is one that was just edited or
    ///   just opened: its *directives* are the only part of it the index can describe, and they are enough to say
    ///   whether the program it needs is in the index ([`Session::the_closure_of_the_directives`]).
    /// * **Nothing is left queued inside that closure**, which is the same statement: a file whose summary was
    ///   dropped has no entry, and the check above fails on it — so a cold start answers `false` for everything and
    ///   pays nothing but the directive scan.
    pub(crate) fn a_timeline_can_be_walked_for(&self, path: &Path) -> bool {
        if !self.documents.is_open(path) {
            return false;
        }

        match self.store.index().summary(path) {
            Some(_) => self.is_closure_ready(path),
            None => self
                .store
                .directive_summary(path)
                .is_some_and(|directives| {
                    self.the_closure_of_the_directives(path, &directives)
                        .is_some()
                }),
        }
    }

    /// **The timeline `path` may be read through now** — the one in hand, or the one that can be walked.
    ///
    /// `None` means the file falls back to its own text (and the second pass refines it later), which is the cold
    /// start's path and stays it: an environment that is not the whole program renders *worse* than the raw text.
    pub(crate) fn the_timeline_to_read_through(
        &self,
        path: &Path,
    ) -> Option<std::sync::Arc<crate::TranslationUnit>> {
        if let Some((false, unit)) = self.held_units().get(&queue_key(path))
            && unit.environment_of(path).is_some()
        {
            return Some(unit);
        }

        if !self.a_timeline_can_be_walked_for(path) {
            return None;
        }

        // **This is the 3.6 s, and it is required behaviour** — see the two contract tests below, which fail if it is
        // removed: `a_file_opened_after_the_index_was_built_is_read_through_a_walked_timeline` and
        // `a_keystroke_in_a_body_reads_the_file_through_its_timeline`. Measured on `LSPHandle.cpp`
        // (`tests/codestyle_probe.rs::what_keeps_the_pump_busy`): with this arm skipped, one `advance` step after an
        // edit falls from **3.73 s to 8.75 ms** — so the cost is *acquiring* the timeline (disk decode of a
        // whole-program rendering, or a walk), and it is paid **per keystroke** because the bounded units table has
        // evicted this file's entry during the project read.
        //
        // The fix is therefore not here: it is to keep the **open file's timeline hot** (pinned against eviction), so
        // the contract holds at milliseconds. See `docs/worklog.md`.
        let (unit, provisional) = self.unit_and_state_of(path)?;
        (!provisional && unit.environment_of(path).is_some()).then_some(unit)
    }

    /// **Could this closure still grow?** — the question a timeline's cache entry has to answer, and the one the
    /// walk itself cannot.
    /// # The two meanings of a file the closure does not hold
    ///
    /// [`crate::TranslationUnit::walk`] stops at a file the closure cannot offer, and its early return says why
    /// that is right: a system header nobody indexed is **outside the analysis**, and treating it as "may define
    /// anything" was measured to collapse the corpus's conditional evidence from millions of facts to tens of
    /// thousands. That reading is permanent, and a timeline built under it is the whole program.
    ///
    /// While an index is running the same absence means **not yet**: the file is in the queue and will be read.
    /// The timeline is then of *part* of the program — and the difference is visible here and nowhere else,
    /// because the queue is this type's.
    ///
    /// So a target that is missing **and queued** is a target that is coming. `queue.is_queued` is exact rather
    /// than a guard against the future: a path in the work list is a path whose summary was dropped or never built,
    /// and every one of them is read before the queue empties.
    pub(crate) fn closure_is_still_growing(
        &self,
        closure: &[(PathBuf, String)],
        in_closure: &HashMap<String, usize>,
    ) -> bool {
        let index = self.store.index();

        closure.iter().any(|(path, _)| {
            index.summary(path).is_some_and(|summary| {
                summary.includes.iter().any(|include| {
                    include.resolved.as_ref().is_some_and(|target| {
                        !in_closure.contains_key(&queue_key(target)) && self.queue.is_queued(target)
                    })
                })
            })
        })
    }

    /// Count one answer from [`Session::translation_unit_of`] — see [`UnitStats`] for what the three mean.
    pub(crate) fn note_a_unit(&self, which: Which) {
        use std::sync::atomic::Ordering::Relaxed;

        let counter = match which {
            Which::FromMemory => &self.unit_from_memory,
            Which::FromDisk => &self.unit_from_disk,
            Which::Walked => &self.unit_walked,
        };
        counter.fetch_add(1, Relaxed);
    }

    /// **What this session's translation units have cost it.** See [`UnitStats`].
    ///
    pub fn unit_stats(&self) -> UnitStats {
        use std::sync::atomic::Ordering::Relaxed;

        UnitStats {
            from_memory: self.unit_from_memory.load(Relaxed),
            from_disk: self.unit_from_disk.load(Relaxed),
            walked: self.unit_walked.load(Relaxed),
        }
    }

    /// **How many times each root's timeline has been walked**, most-walked first — see [`Session::unit_stats`] for
    /// the total this breaks down.
    ///
    /// The question it exists for: a total of two hundred walks is either one root walked two hundred times or two
    /// hundred roots walked once, and those want opposite fixes. Measured on a real project
    /// (`examples/project_index.rs`), opening one file took `walk` from 10.6 s to 45.8 s over the same 1729 files —
    /// so the walks are not spread evenly, and this says which root is paying.
    pub fn walks_by_root(&self) -> Vec<(PathBuf, u64)> {
        let Ok(walks) = self.walks_by_root.lock() else {
            return Vec::new();
        };
        let mut walked: Vec<(PathBuf, u64)> = walks
            .iter()
            .map(|(path, count)| (PathBuf::from(path), *count))
            .collect();
        walked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        walked
    }

    // **What a unit rooted at `path` is keyed on** — the file's preamble, when its text is in reach.
    //
    // A file that cannot be read is [`RootKey::WholeText`], which is the rule that can only ever be less

    /// **What makes two timelines the same timeline: the closure's content, hashed.**
    ///
    /// Used as the in-run key of [`Session::walked_by_content`]. Two roots whose closures hold the same files with
    /// the same text produce the same walk, whatever the roots are called and whatever order the includes were
    /// discovered in — so the files are hashed **by content and in a canonical order**, which is what makes the
    /// answer independent of both.
    ///
    /// The context hash is mixed in because the same closure really can walk differently under a different
    /// configuration — `-D`s, `-I`s, the standard — and that is the reason `tu_cache` keeps the two apart on disk
    /// as well.
    ///
    /// **Sorted, not in closure order**: `closure_paths` is a breadth-first discovery order, which is a property of
    /// how the graph was read rather than of what is in it. Two roots that reach the same headers through a different
    /// chain have the same closure and would otherwise hash differently — which is the whole case this exists for.
    pub(crate) fn closure_identity(
        &self,
        closure: &[(PathBuf, String)],
        context: u64,
    ) -> (u64, u64) {
        let mut contents: Vec<u64> = closure
            .iter()
            .map(|(_, text)| crate::cache::content_hash(text))
            .collect();
        contents.sort_unstable();

        // A plain FNV over the sorted hashes: this is a **key**, not a security boundary. The file count is mixed in
        // at the end for the one case a hash of a sorted list cannot separate on its own — a prefix of a list whose
        // remaining entries all hashed to zero, which is not reachable in practice but costs nothing to exclude.
        let mut identity: u64 = 0xcbf2_9ce4_8422_2325;
        for hash in &contents {
            identity ^= *hash;
            identity = identity.wrapping_mul(0x0000_0100_0000_01b3);
        }
        identity ^= (contents.len() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        (context, identity)
    }

    /// The units table, with the lock taken — the one place the poison rule is written down.
    /// **Which translation unit the index says reads this file** — the root a header's environment belongs to.
    ///
    /// A header is not the root of its own program: `wincon.h` includes neither `windef.h` nor `windows.h`, so a
    /// timeline rooted at the header has no `WINAPI` in force. Which *other* file's timeline answers used to be
    /// "a held unit that contains the header", and `held_units` is a hash map — so the answer changed with the map's
    /// layout **and with what the session had cooked**. The census measured it: `grid.h` answered 2 parse errors in
    /// the 517-file session and 0 alone (the docs' §6), because `main.c` defines `EXTERN` for its own translation
    /// unit before including anything, and whether `main.c`'s unit answered depended on the run.
    ///
    /// The **index** names the unit now: breadth-first over [`ProjectIndex::includers_of`], shallowest first and
    /// smallest path within a level, stopping at a file that can be the root of its own program — one with a
    /// compile command, or one nothing includes. That is a function of the include graph and the project's file
    /// list, both of which are the same on every run.
    ///
    /// Not cached: the walk is over the include graph's reverse edges, a handful of steps per header, and a cache
    /// would have to be invalidated by every index insertion — the thing this method exists to stop depending on.
    pub(crate) fn the_unit_that_reads(&self, path: &Path) -> Option<PathBuf> {
        let project: std::collections::HashSet<String> =
            self.project.iter().map(|root| queue_key(root)).collect();
        let a_root = |candidate: &Path| {
            if !project.contains(&queue_key(candidate)) {
                return false;
            }
            self.config
                .database
                .as_ref()
                .is_some_and(|database| database.command_for(candidate).is_some())
                || self.store.index().includers_of(candidate).is_empty()
        };

        let mut seen: std::collections::HashSet<PathBuf> =
            std::collections::HashSet::from([path.to_path_buf()]);
        let mut level = self.store.index().includers_of(path);
        while !level.is_empty() {
            level.sort();
            level.dedup();
            if let Some(root) = level.iter().find(|candidate| a_root(candidate)) {
                return Some(root.clone());
            }
            let mut next: Vec<PathBuf> = Vec::new();
            for includer in &level {
                if !seen.insert(includer.clone()) {
                    continue;
                }
                next.extend(self.store.index().includers_of(includer));
            }
            level = next;
        }
        None
    }

    pub(crate) fn held_units(&self) -> std::sync::MutexGuard<'_, UnitTable> {
        self.units
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// **The walk's own answer, for a differential test** — a cold walk, not a served rendering.
    ///
    /// `read_the_unit` and `gather_a_unit_read` both answer from a rendering an earlier call cooked when they can, and
    /// a served rendering carries no timeline; `cache::env::differential` needs the timeline, so this walks. The pair
    /// it uses is the one `read_the_program` uses — `closure_with_text_from` and `TranslationUnit::walk` — with the
    /// contribution sharing **off**, because a shared contribution is replayed at a position recorded in a different
    /// program and a preprocessor's positions are not portable between programs.
    #[cfg(test)]
    pub(crate) fn walked_bindings(
        &mut self,
        root: &Path,
    ) -> Option<Vec<(std::path::PathBuf, Vec<(String, Option<bool>, String)>)>> {
        let summary = self.store.index().summary(root)?;
        let root_path = summary.path.clone();
        let root_key = queue_key(&root_path);
        let (closure, by_key) = self.closure_with_text_from(&root_path, Some(summary));
        let mut definitions = self.held_definitions();
        let index = self.store.index();
        let unit = crate::TranslationUnit::walk(
            summary,
            |wanted| {
                let (held, text) = closure.get(*by_key.get(&queue_key(wanted))?)?;
                let summary = if queue_key(held) == root_key {
                    summary
                } else {
                    index.summary(held)?
                };
                Some((summary, text.as_str()))
            },
            index.macros(),
            &mut definitions,
            false,
            self.store.contributions(),
        );
        let mut found = Vec::new();
        for file in unit.files() {
            let Some(view) = unit.environment_of(file) else {
                continue;
            };
            // **Both channels**, because a view's answer to "is this a macro" is the union: `for_each_definition`
            // is the definitions it contributes, and `for_each_body_in_force` is the bodies only the in-force channel
            // has (an `_STD_BEGIN`-shaped `#define` inside a conditional region). A consumer reading the environment
            // reads both, so Gate 0 compares both.
            let mut bindings: Vec<(String, Option<bool>, String)> = Vec::new();
            cpp_parser::MacroFacts::for_each_definition(&view, &mut |facts| {
                bindings.push((
                    facts.name.to_string(),
                    Some(facts.function_like),
                    facts
                        .body_text
                        .map(|text| text.to_string())
                        .unwrap_or_default(),
                ));
            });
            let definitions = bindings.len();
            cpp_parser::MacroFacts::for_each_body_in_force(&view, &mut |facts| {
                let name = facts.name.to_string();
                if bindings[..definitions].iter().any(|known| known.0 == name) {
                    return;
                }
                bindings.push((name, facts.function_like, facts.body.to_string()));
            });
            found.push((file.to_path_buf(), bindings));
        }
        Some(found)
    }
    /// The macro definition cache, with the lock taken — same poison rule, see [`Session::held_units`].
    ///
    /// It is behind a lock for the same reason the units table is: this cache was what made a unit walk need
    /// `&mut self`, and `&mut self` put the walk inside the writer where every request waits for it.
    pub(crate) fn held_definitions(&self) -> std::sync::MutexGuard<'_, crate::MacroDefinitions> {
        self.definitions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// **Cook one translation unit into a stream, without indexing it** — the compiler's own reading of the program,
    /// as this session would parse it.
    ///
    /// This is [`Session::read_the_unit`]'s first half, and it exists as a call of its own because the reading is
    /// worth having **before** anything is done with it: it is the one artifact in this crate that can be put beside
    /// a real preprocessor's output, and that comparison is what
    /// [`crate::align`] is for. A caller that only wanted the facts calls [`Session::read_the_unit`] and never sees
    /// this; a caller checking whether the reading is *right* needs exactly this and nothing else.
    ///
    /// # What comes back
    ///
    /// Every file the unit's walk reached, stitched in include order with the branches the conditions chose already
    /// taken — [`crate::RenderedUnit`], whose `spans` say which file each token **stands in** and where in that file
    /// a consumer should act on it. Nothing is parsed and nothing is filed: the same stream, asked for twice, is the
    /// same stream.
    ///
    /// # Why the text comes through the session's overlay
    ///
    /// A file the reader has open and has **not saved** is the file that should be read — an editor's reading of a
    /// program that ignores the buffer in front of the user is a reading of some other program. A file the provider
    /// cannot read at all is simply absent, and the stream counts it as a hole
    /// ([`crate::RenderedUnit::missing`]) rather than as an empty file, because those are different programs.
    ///
    /// `None` when the file has no summary or the walk cannot be built — nothing has read it yet, which is a state
    /// and not an error. A caller with a path the index spells differently should hand over the index's own spelling;
    /// [`Session::read_the_unit`] is the one that has to do that, because it puts the result in the index.
    /// **A translation unit`s macro tables, built by folding effects** — see [`crate::effect`].
    ///
    /// This is the model`s alternative to cooking an include closure: every file in the program contributes its
    /// *effect*, an effect is a function of the few macro names the file tests, and folding them is a hash lookup
    /// per include. What it answers is the table each file was read under, which is what the second pass has been
    /// walking timelines to reconstruct.
    ///
    /// `None` when the root has no summary — nothing has read it, which is a state rather than an error.
    pub fn effect_walk(
        &self,
        root: &Path,
    ) -> Option<(HashMap<PathBuf, u64>, crate::effect::Walked)> {
        self.store.index().summary(root)?;
        let reads = SessionReads {
            files: &self.files,
            index: self.store.index(),
        };
        // **Both caches are shared between walks.** Without the surfaces every walk re-lexes every file (measured:
        // 1.05 s against the 38 ms timeline walk this would replace); without the effects every condition is
        // re-evaluated, which is the rest of it.
        let mut cache = self
            .effects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // **The effect table is bounded by count, not by bytes**: each effect is a small list of folds, and the
        // map outlives every walk, so a long session over a big project would otherwise keep one entry per
        // (file, environment) pair it ever saw — see [`crate::cache_lru::EFFECT_ENTRIES`].
        if cache.len() >= cache::cache_lru::EFFECT_ENTRIES {
            cache.clear();
        }
        let seed = crate::effect::Macros::new();
        let surfaces = self
            .surfaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (signatures, walked) = crate::effect::Chain::new(&reads, &mut cache)
            .with_surfaces(&surfaces)
            .at_signatures(root, &seed);
        Some((signatures, walked))
    }

    pub fn render_the_unit(&self, root: &Path) -> Option<crate::RenderedUnit> {
        let unit = self.translation_unit_of(root)?;
        let (sources, key) = self.the_units_inputs(&unit);

        // **The same inputs, already rendered.** A reader asks for the same unit again and again — every keystroke
        // that lands on a name, every drain — and a render is a walk, a cook of every file in include order and a
        // stitched stream of megabytes. The plan's M4 is this call: the second answer must not cost what the first
        // did.
        if let Ok(known) = self.rendered_unit.lock()
            && let Some((seen, cached)) = known.as_ref()
            && *seen == key
        {
            return Some(cached.clone());
        }

        // **The database may already have this program.** The key is a hash over every `(path,
        // content)` the closure entered, so a rendering filed under it is this rendering — and
        // reading it back is a decode instead of a cook. Measured: the cook is **899 ms** for a 1
        // 991 163-byte program, which is the whole reason a unit read could not be repeated; the
        // decode is the difference between a second unit read costing a second and costing a
        // moment.
        if let Some(cached) = self.store.cached_unit(key) {
            if false {
                eprintln!("unit render: key {key:016x} served from the cache");
            }
            if let Ok(mut known) = self.rendered_unit.lock() {
                *known = Some((key, cached.clone()));
            }
            return Some(cached);
        }
        if false {
            eprintln!(
                "unit render: key {key:016x} MISSED ({} source(s) to cook)",
                sources.len()
            );
        }

        let definitions = unit.definitions();
        let seed = MacroTable::from_marked(self.store.index().macros());
        // **The include search, so `__has_include` is answerable.** Without it the operator answers `Unknown`, which
        // is honest but decides nothing, and `#if __has_include(<span>)` is a shape real headers are full of, so a
        // cook that cannot answer it reads a branch on no evidence. The resolver's own search is what an `#include`
        // would use, which is what makes the two agree.
        let search = crate::preprocess::cooked::Search::new(&self.files, &self.config);
        // **The compiler's answers for this program.** A `#if __has_builtin(…)` in a header decides which
        // declarations the program has; without the probe the unit would take the else branch while the file's own
        // cook takes the then branch, and the two readings would disagree about which branch was compiled.
        let features = self.feature_probe(root);
        let stream = {
            unit.cook_the_unit_with_features(
                &sources,
                &definitions,
                Some(&seed),
                true,
                Some(&search),
                features.as_deref(),
            )
        };

        // **Kept under the key, provisionally or not — the key is what makes that safe.** It folds every `(path,
        // content)` the walk entered, so a rendering of a closure that was still filling has a key the finished
        // closure cannot repeat: the row is never hit again, it is merely wasted, and a collection drops it once no
        // file references it.
        self.store.remember_unit(key, &stream);
        if let Ok(mut known) = self.rendered_unit.lock() {
            *known = Some((key, stream.clone()));
        }
        Some(stream)
    }

    /// **Does this stored rendering still describe the files the walk entered?**
    ///
    /// The same fold `the_units_inputs` makes — every file the **walk** entered (`RenderedUnit::walked`), whose text is
    /// readable, sorted by path, `path|hash;` — against the key the rendering is filed under. An edit between two asks
    /// makes the two disagree, which is a miss rather than a stale answer.
    pub(crate) fn the_rendering_still_describes(
        &self,
        rendered: &crate::RenderedUnit,
        stored: u64,
    ) -> bool {
        let mut inputs: Vec<(&std::path::Path, u64)> = Vec::new();
        for path in &rendered.walked {
            let Some(text) = self.files.read(path) else {
                return false;
            };
            inputs.push((path.as_path(), crate::cache::content_hash(&text)));
        }
        crate::cache::a_rendering_key(&inputs) == stored
    }

    /// **The texts a unit's cook reads, and the key its rendering is filed under.**
    ///
    /// Extracted because two callers need exactly this pair and neither should compute it twice: the render
    /// ([`Session::render_the_unit`]) and the gather ([`Session::gather_a_unit_read`]).
    ///
    /// # The key is the inputs, not the root path
    ///
    /// A file the reader has open and has **not saved** comes from the overlay, so an edit changes the program without
    /// changing its name, and a cache keyed on the name would answer with a stream built from text the files no longer
    /// have. Folding every file's *content hash* in is what makes a hit mean "the same input", which is the only thing
    /// that makes reusing the output sound.
    ///
    /// Sorted, so two runs that read the same files in a different order of *this loop* still agree; the include order
    /// that matters is already fixed inside the stream by the walk.
    pub(crate) fn the_units_inputs(
        &self,
        unit: &crate::TranslationUnit,
    ) -> (HashMap<PathBuf, String>, u64) {
        let mut sources: HashMap<PathBuf, String> = HashMap::new();
        for path in unit.files() {
            if let Some(text) = self.files.read(path) {
                sources.insert(path.to_path_buf(), text);
            }
        }

        let inputs: Vec<(&std::path::Path, u64)> = sources
            .iter()
            .map(|(path, text)| (path.as_path(), crate::cache::content_hash(text)))
            .collect();
        // **Folded with the reader, not only with the texts** — see [`crate::cache::a_rendering_key`], whose note
        // records why: a rendering is a function of the reader as well as of the bytes, and a key that names only the
        // bytes cannot invalidate a stream whose inputs happen not to have moved.
        let key = crate::cache::a_rendering_key(&inputs);
        if false {
            eprintln!(
                "unit inputs: {} file(s), reader {:016x}, key {key:016x}",
                inputs.len(),
                crate::cache::READING_FINGERPRINT
            );
        }

        (sources, key)
    }

    /// **Everything a program read needs, gathered under the lock that is already held.**
    ///
    /// The point of this type is *where the time is spent*, and it is measured: of the 5.09 s a program read costs,
    /// the **walk and the closure's texts are 44 ms** and they are the only part that needs the session — the cook
    /// (1.25 s), the parse and the sweep (3.66 s) are a function of the unit, the texts and the configuration.
    /// [`UnitReadMaterials::read_the_program`] is that part, and it takes no session at all, so the caller may drop
    /// every lock while it runs — which is what lets a real project's program be read without a keystroke waiting.
    ///
    /// The cheap lookups happen here rather than in `read_the_program`, because they are cache reads the store owns:
    /// the rendering by the closure's key, and (when the rendering is already known) the facts by the rendering's own
    /// hash.
    ///
    /// `None` when the file has no summary or the walk cannot be built — nothing has read it yet, which is a state
    /// and not an error.
    pub fn gather_a_unit_read(&self, root: &Path) -> Option<UnitReadMaterials<SessionFiles<F>>> {
        let started = std::time::Instant::now();
        let revision = self.revision();
        // The index's own spelling again, for the same reason [`Session::cook`] takes it: the unit's frames are
        // keyed by the normalized path.
        let root = self.store.index().summary(root)?.path.clone();
        let context = self.store.context_hash(&root);

        // **The key without the walk, when this session has already walked this program.** The rendering records the
        // walk's own file list (`RenderedUnit::walked`), so the key it is filed under can be reproduced from the
        // rendering and checked against those files' current texts — an edit between the two asks is a miss.
        if let Some(units_key) = self.the_units_key_of(&root, context)
            && let Some(rendered) = self
                .rendered_unit
                .lock()
                .ok()
                .and_then(|known| {
                    known
                        .as_ref()
                        .filter(|(seen, _)| *seen == units_key)
                        .map(|(_, cached)| cached.clone())
                })
                .or_else(|| self.store.cached_unit(units_key))
            && self.the_rendering_still_describes(&rendered, units_key)
        {
            if false {
                eprintln!(
                    "unit gather: key {units_key:016x} served without the walk in {:?} ({} walked, {} token file(s))",
                    started.elapsed(),
                    rendered.walked.len(),
                    rendered.files.len(),
                );
            }
            let program = crate::cache::content_hash(&rendered.text);
            let known = self
                .unit_index
                .lock()
                .ok()
                .and_then(|memo| {
                    memo.as_ref()
                        .filter(|(seen, _)| *seen == program)
                        .map(|(_, cached)| cached.clone())
                })
                .or_else(|| self.store.cached_indexed_unit(program));
            return Some(UnitReadMaterials {
                root,
                revision,
                key: SummaryKey::new(0, context),
                units_key,
                unit: None,
                sources: HashMap::new(),
                macros: self.store.index().macros().clone(),
                config: self.config.clone(),
                files: self.files.clone(),
                database: self.store.database(),
                program: Some(program),
                rendered: Some(rendered),
                known,
            });
        }

        let unit = self.translation_unit_of(&root)?;
        let walked = started.elapsed();
        let (sources, units_key) = self.the_units_inputs(&unit);
        let hashed = started.elapsed() - walked;
        let key = SummaryKey::new(0, context);

        // **The rendering, if this session or the database already has it.** Without it the program's hash is unknown
        // and neither cache can be asked — so the facts are looked up in `read_the_program`, after the cook.
        let rendered = self
            .rendered_unit
            .lock()
            .ok()
            .and_then(|known| {
                known
                    .as_ref()
                    .filter(|(seen, _)| *seen == units_key)
                    .map(|(_, cached)| cached.clone())
            })
            .or_else(|| self.store.cached_unit(units_key));
        if false {
            eprintln!(
                "unit gather: key {units_key:016x} {} — walked {walked:?}, key {hashed:?} ({} file(s))",
                if rendered.is_some() {
                    "served"
                } else {
                    "MISSED"
                },
                unit.files().count(),
            );
        }

        let (program, known) = match &rendered {
            Some(rendered) => {
                let program = crate::cache::content_hash(&rendered.text);
                let known = self
                    .unit_index
                    .lock()
                    .ok()
                    .and_then(|memo| {
                        memo.as_ref()
                            .filter(|(seen, _)| *seen == program)
                            .map(|(_, cached)| cached.clone())
                    })
                    .or_else(|| self.store.cached_indexed_unit(program));
                (Some(program), known)
            }
            None => (None, None),
        };
        Some(UnitReadMaterials {
            root,
            revision,
            key,
            units_key,
            unit: Some(unit),
            sources,
            macros: self.store.index().macros().clone(),
            config: self.config.clone(),
            files: self.files.clone(),
            database: self.store.database(),
            program,
            rendered,
            known,
        })
    }

    /// Record the closure only when the program reading is committed.
    pub(crate) fn remember_the_walked_closure(&self, root: &Path, walked: &[PathBuf]) {
        if walked.is_empty() {
            return;
        }
        let Some(module) = self
            .store
            .index()
            .summary(root)
            .and_then(|summary| summary.modules.module.clone())
        else {
            return;
        };
        let files: Vec<String> = walked
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect();
        self.store.index().remember_the_unit_closure(&module, files);
    }

    /// **A unit read worked out, and nothing changed** — the expensive half, under `&self`.
    ///
    /// The whole of it: gather ([`Session::gather_a_unit_read`], which needs the session for the walk) and then read
    /// ([`UnitReadMaterials::read_the_program`], which does not). A caller that has a *lock* to keep free — the pump —
    /// calls the two apart; this is the caller that does not care.
    ///
    /// The counterpart of [`Session::commit_a_unit_reading`], and the split exists for the reason every other
    /// prepare/commit pair in this session does: a unit read is a walk, a cook of every file in include order, a
    /// **parse of the whole program** and a sweep of every declaration in it — **5.09 s** for a 159-file,
    /// 737 525-token program on a real project, and **85 ms** for the same program again.
    pub fn prepare_a_unit_reading(&self, root: &Path) -> Option<PreparedUnitReading> {
        let materials = self.gather_a_unit_read(root)?;
        // **Remember which key this root's rendering came out under** — in this session, and in the project's database,
        // so the *next* session can skip the walk too. Recorded here rather than inside `read_the_program`, which has no
        // session to write to and whose materials know the key.
        let spelling = self
            .store
            .index()
            .summary(root)
            .map(|summary| summary.path.clone())
            .unwrap_or_else(|| root.to_path_buf());
        let context = self.store.context_hash(&spelling);
        let units_key = materials.units_key();
        if let Ok(mut known) = self.unit_roots.lock() {
            known.insert((spelling.clone(), context), units_key);
        }
        self.store
            .remember_unit_root(&format!("{context}:{}", spelling.display()), units_key);
        materials.read_the_program()
    }

    /// **The units key a root's rendering was filed under** — this session's memory first, the project's database
    /// second. See [`Session::unit_roots`] and `cache_db`'s `UNIT_ROOTS`.
    pub(crate) fn the_units_key_of(&self, root: &Path, context: u64) -> Option<u64> {
        if let Some(known) = self
            .unit_roots
            .lock()
            .ok()
            .and_then(|known| known.get(&(root.to_path_buf(), context)).copied())
        {
            return Some(known);
        }
        self.store
            .cached_unit_root(&format!("{context}:{}", root.display()))
    }

    /// Remember what a program's parse came out as — the one-entry memo `Session::unit_index` holds.
    pub(crate) fn remember_the_unit_index(&self, program: u64, indexed: &crate::IndexedUnit) {
        if let Ok(mut memo) = self.unit_index.lock() {
            *memo = Some((program, indexed.clone()));
        }
    }

    /// **How many items one completion may answer with** — `[completion] max_items` in `.cppls.toml`, clamped.
    ///
    /// Read per query rather than stored at assembly: the project file is re-read when it changes, and a budget
    /// captured at start-up would go on answering the old number. The clamp is `1..=20 000` — twenty thousand items is
    /// a megabyte of JSON per keystroke, and a budget of zero would answer "there is nothing here".
    pub(crate) fn the_completion_budget(&self) -> usize {
        self.project_config()
            .config
            .completion
            .max_items
            .unwrap_or(crate::completion::ITEM_BUDGET)
            .clamp(1, MAX_COMPLETION_ITEMS)
    }

    /// **Put what [`Session::prepare_a_unit_reading`] worked out into the index** — one `insert_cooked` per file, and
    /// the only part of a unit read that needs `&mut self`.
    ///
    /// Returns `None` when the session or its inputs changed after preparation.
    /// **The per-file cache records a program read writes, prepared with a read lock.**
    ///
    /// The environment fingerprint each key needs builds the file's whole closure — **~39 ms**, so 460 of them are
    /// 17.9 s — and doing that under the writer froze every request for that long (measured on neovim's `ui.c`, whose
    /// program is 460 files). Prepared here, the writer is left with encode-and-insert: **~0.4 s** for the same 460.
    pub fn prepare_the_unit_records(
        &self,
        prepared: &PreparedUnitReading,
    ) -> Vec<(crate::cache::cache_db::CacheKey, Vec<u8>)> {
        let files = &prepared.indexed.files;
        let prepare = |(path, cooked): &(std::path::PathBuf, crate::CookedFile)| {
            self.store.prepare_a_cooked_record(path, cooked)
        };
        // **In parallel, because each record's fingerprint is a walk of its own file's closure.** They share nothing
        // but the store's caches, which are read-mostly; measured on neovim's `ui.c`, 460 records took **17.08 s** of
        // one core while the program read's own six seconds of cook-and-parse had nothing to do with it. The count is
        // the machine's cores, and the chunks are cut to keep the answers in the caller's order.
        let threads = std::thread::available_parallelism()
            .map(|cores| cores.get())
            .unwrap_or(4)
            .min(files.len().max(1));
        if threads <= 1 || files.len() < 16 {
            return files.iter().filter_map(prepare).collect();
        }
        let chunk = files.len().div_ceil(threads);
        let mut records = Vec::with_capacity(files.len());
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(threads);
            for piece in files.chunks(chunk) {
                handles.push(scope.spawn(move || {
                    piece
                        .iter()
                        .filter_map(|file| self.store.prepare_a_cooked_record(&file.0, &file.1))
                        .collect::<Vec<_>>()
                }));
            }
            for handle in handles {
                records.extend(handle.join().unwrap_or_default());
            }
        });
        records
    }

    pub fn commit_a_unit_reading(
        &mut self,
        prepared: PreparedUnitReading,
        records: Vec<(crate::cache::cache_db::CacheKey, Vec<u8>)>,
    ) -> Option<UnitReading> {
        if !self.is_current(&prepared.revision) {
            log::debug!("discarding a unit reading prepared for an older session revision");
            return None;
        }
        let PreparedUnitReading {
            root,
            program,
            indexed,
            walked,
            revision: _,
        } = prepared;
        // **What an importer of this module sees** — the closure the cold path only had here, and the warm path also
        // records at gather time. See [`ProjectIndex::unit_closures`](crate::ProjectIndex).
        self.remember_the_walked_closure(&root, &walked);
        // **And the memo, which is the difference between a second program read costing 3.66 s and costing a clone.**
        // It is filled here rather than in `read_the_program` because the memo is the session's; a program read off
        // the lock still writes the database rows, so a session that never commits this one (a dropped batch) loses
        // the fast path for one read and nothing else.
        self.remember_the_unit_index(program, &indexed);

        // **The gate is gone, and what replaced it is a repair rather than a refusal.** A parse error is filed
        // against the file it is in; a brace the parser paired across two files is neutralised in the stream and
        // counted (`repaired`), and the file that owned it is read on its own as well — see
        // `index_unit_rendering`, which is where both happen.
        //
        // What used to be here was `if indexed.crossings == 0` around the whole loop below, and it was the single
        // most expensive rule in this crate: one mis-paired brace anywhere in a five-million-byte program left the
        // index exactly as empty as it started. That is how `std::format` came to have no members while the file
        // that declares them was in the program, and it is the failure the plan's §4 rule 1 is written against —
        // *"no layer may give up an answer it already has because it is unsure about something else."* The
        // declarations of the files that did **not** leak were never in doubt, and refusing them answered a
        // question nobody had asked.
        let mut placed = 0usize;
        if false {
            let names: Vec<String> = indexed
                .files
                .iter()
                .map(|(path, _)| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default()
                })
                .filter(|name| name == "format" || name == "string" || name == "vector")
                .collect();
            eprintln!(
                "unit commit: {} file(s); interesting: {names:?}",
                indexed.files.len()
            );
        }
        for (path, cooked) in indexed.files {
            placed += 1;
            if false && path.file_name().and_then(|name| name.to_str()) == Some("format") {
                let in_std = cooked
                    .declarations
                    .iter()
                    .filter(|fact| fact.name.as_str() == "format")
                    .map(|fact| fact.scope.clone().unwrap_or_default())
                    .take(4)
                    .collect::<Vec<_>>();
                eprintln!(
                    "unit commit: <format> {} declaration(s); format in {in_std:?}",
                    cooked.declarations.len()
                );
            }
            // **And the queue is told, so no per-file step reads it again.**
            self.queue.mark_worked(&path);
            // **The summary is left alone, and that is a measurement rather than an omission.** The tempting next
            // step here is "replace the file's summary declarations with the program's reading of it", and it is
            // wrong for a reason `tests/unit_read_probe.rs` shows: after this pipeline has run, a file's summary is
            // *already* a rendering — the second pass re-read every file whose declarations a directive split or a
            // body macro moved. What differs between the two lists is not raw-versus-rendering, it is **file-read-
            // on-its-own versus file-read-in-this-program**: 1 095 declarations over 177 files are in one and not the
            // other, because `_CRT_…` and `_GLIBCXX_…` answer differently when the file is compiled standalone and
            // when it is compiled inside `Selection.cpp`. So replacing one list with the other would answer for the
            // wrong translation unit — the problem §5 step two (a key with `env_hash`) exists to solve, not one a
            // rewrite can.
            // **Filed under the program that read it**, not under the file: the same header read by two translation
            // units is two readings, and a lookup asks for the asking file's one
            // (`ProjectIndex::the_reading_for`). See `Readings` in `index/project.rs`.

            // **And the file's own record, under its closure key.** The program id is the in-session half of the
            // reading; this is the disk's: the same record a per-file cook writes, and the one a warm start's
            // 	ake_the_stored_refinements serves instead of re-reading the program. Without it the per-file
            // readings of a whole program were written nowhere, so the next session rebuilt every one of them.
            self.store
                .index_mut()
                .insert_cooked_in_a_program(&path, program, cooked);
            // **A file a program read is refined.** The reading this just filed *is* the compiled one — the strongest
            // answer this crate produces — and `want_refinement` asks `is_refined`, which until now knew only about
            // the two paths that write under the file's own `env_hash` (a served record, and a per-file refinement).
            // A file from a program read therefore looked unrefined, and `index_everything`'s loop asked for every one
            // of them, which puts them in the second pass's backlog and pays a unit walk per file in `prepare_a_wave`.
            //
            // **Measured, and it did not move the module read**: marking them closed the backlog, and the two slow
            // `advance` batches of the modules fixture (49–67 s for 33 files, 44 s for none) were unchanged — so the
            // cost there is `index_one`'s own unit walk, not this backlog. The mark stays because it is true and
            // because the backlog it removes is real work for a drain that would otherwise walk files it has just
            // read; the batch cost is P6's next measurement, not this line's.
            if let Ok(mut refined) = self.refined_readings.lock() {
                refined.insert(queue_key(&path));
            }
        }

        // **The writer's half only**: the records were prepared with a read lock (see
        // `prepare_the_unit_records`), and what is left is the encode-and-insert this loop does in milliseconds.
        for (key, bytes) in records {
            self.store.put_a_cooked_record(key, &bytes);
        }
        Some(UnitReading {
            root,
            files: placed,
            declared: 0,
            tokens: indexed.tokens,
            files_with_tokens: indexed.files_with_tokens,
            missing: indexed.missing,
            unplaced: indexed.unplaced,
            unbalanced: indexed.unbalanced,
            braces: indexed.braces,
            errors: indexed.errors,
        })
    }

    /// **Read one translation unit as a program, and file its facts under every file it read.**
    ///
    /// # What this is for
    ///
    /// Everything else in this session reads a **file**: its own text, its own closure, its own environment. This is
    /// the one call that reads a **program** — the closure stitched into one stream in include order
    /// ([`crate::TranslationUnit::cook_the_unit`]), parsed **once**
    /// ([`crate::FileIndexer::index_unit_rendering`]) — and hands every declaration it finds to the file it was
    /// written in.
    ///
    /// That is what makes the index hold "what the compiler saw" for files **nobody has opened**: without it a
    /// header's cooked facts exist only for the files some request happened to name, so a cross-file answer depends
    /// on whether a reader had looked at the header yet.
    ///
    /// # The two halves, and where they are paid
    ///
    /// [`Session::prepare_a_unit_reading`] does the work and takes `&self`;
    /// [`Session::commit_a_unit_reading`] is the inserts. This function is the two together, for a caller that
    /// already holds the write lock and does not care — a test, a batch, `read_a_looked_at_unit`. The pipeline pays
    /// them apart, so that the five seconds a program takes to read does not hold the lock a keystroke waits for.
    ///
    /// # Why it was **not** wired into the pump until now
    ///
    /// Because an earlier attempt made three readings worse and could not say why (`declarations_in("std")` 2008 →
    /// 1959, and two `definition` counts). `tests/unit_read_probe.rs` re-measured that on a real project and it does
    /// not reproduce: 159 files, 737 525 tokens, **zero** errors, 39 080 cooked declarations added, and not one raw
    /// fact lost. The cause was the one `index_unit_rendering`'s crossing fence now quarantines — a file whose
    /// unclosed scope used to swallow the rest of the program.
    pub fn read_the_unit(&mut self, root: &Path) -> Option<UnitReading> {
        let gathering = std::time::Instant::now();
        let prepared = self.prepare_a_unit_reading(root)?;
        // The per-file records are prepared here, while the same read-side state is in hand — see
        // `prepare_the_unit_records` for the 17.9 s this keeps out of the writer.
        let records = self.prepare_the_unit_records(&prepared);
        let gathered = gathering.elapsed();
        let committing = std::time::Instant::now();
        let reading = self.commit_a_unit_reading(prepared, records)?;
        if false {
            eprintln!(
                "unit read: prepare {gathered:?} (walk + cook + parse), commit {:?} ({} file(s) filed)",
                committing.elapsed(),
                reading.files
            );
        }
        Some(reading)
    }

    /// **Is a whole program going to be read from this drain?** — the one rule that keeps a unit read off the path
    /// a keystroke takes.
    ///
    /// The split between [`Session::prepare_a_unit_reading`] and [`Session::commit_a_unit_reading`] moves the cost
    /// off the *write* lock, and that is not enough on its own: the preparing half holds the **read** lock for five
    /// seconds, and a `didChange` takes the write lock — so a reader typing into the file being read would wait for
    /// the whole program. Nothing in a session may start one while somebody is typing, and this is what says so.
    ///
    /// Two conditions, and the second is what makes it testable rather than merely polite:
    ///
    /// * **a reader has the file open** — a project nobody is looking at is not worth a program (`read_the_unit` is
    ///   still the API for a caller that wants one, and `read_a_unit_of_the_project` for a caller that wants all);
    /// * **no edit for [`UNTIL_A_UNIT_MAY_BE_READ`]** — the instant [`Session::buffer_changed`] records. A person
    ///   typing defers it indefinitely, which is the point: the reading is a *comfort* that makes the whole closure
    ///   queryable, and typing is the thing the session exists to answer.
    pub(crate) fn a_unit_may_be_read_now(&self) -> bool {
        self.last_edit.elapsed() >= UNTIL_A_UNIT_MAY_BE_READ
    }

    /// **Move the editing clock back**, so that a test can ask about a schedule that waits ten seconds without
    /// sleeping for them — the two thresholds in this file are a person's typing rhythm, and a test that waited for
    /// the longer one would be a test nobody runs.
    #[cfg(test)]
    pub(crate) fn pretend_a_pause(&mut self, for_how_long: std::time::Duration) {
        self.last_edit = std::time::Instant::now()
            .checked_sub(for_how_long)
            .unwrap_or_else(std::time::Instant::now);
    }

    pub fn the_wait_before_a_program_may_be_read(&self) -> Option<std::time::Duration> {
        // **One clock.** An open file becomes readable one second after the reader stops typing, and nothing else is
        // read as a program at all — see [`UNTIL_A_UNIT_MAY_BE_READ`].
        self.the_next_unit_to_read()
            .map(|_| UNTIL_A_UNIT_MAY_BE_READ.saturating_sub(self.last_edit.elapsed()))
    }

    /// **The next file whose whole program is worth reading** — an open file the program of which nobody has read,
    /// and — the clock — which nothing has been typed into for a moment.
    ///
    /// Open files only, in the order the client sent them: a project with two hundred translation units does not
    /// read two hundred programs because one file is open. `units_read` is what makes it once per program, and it is
    /// keyed by the root rather than by the program's bytes because the root is what the schedule iterates.
    ///
    /// **Nothing here prices a program.** Two bounds used to — a source size and a session-wide byte budget — and both
    /// were removed when a counter that counts frees showed what they were bounding: seventeen whole-program reads take
    /// a session from 250 MB to 478 MB of live bytes. See the note where the constants were, in `session/mod.rs`.
    ///
    /// [`Session::the_next_unit_to_read`] is the same question without the clock, for a caller that is itself the
    /// signal that now is a good moment (`Session::read_a_looked_at_unit`).
    pub fn the_program_to_read(&self) -> Option<PathBuf> {
        // **Open files only.** A project with two hundred translation units does not read two hundred programs because
        // one file is open — the files nobody has open keep the reading their own text gives them, which is what
        // `clangd` does too: it parses a unit when a file is opened, not for the whole project in the background.
        let open = self.the_next_unit_to_read()?;
        self.a_unit_may_be_read_now().then_some(open)
    }

    /// The root a program read would be for, ignoring the clock — see [`Session::the_program_to_read`].
    pub(crate) fn the_next_unit_to_read(&self) -> Option<PathBuf> {
        // **The module interface units first, then the open documents.** A module is a *dependency* of the files that
        // import it: until `bits/std.cc` has been read as a translation unit, every header in its closure answers from
        // its raw summary, and an `import std;` file's completions are wrong for as long as that lasts. They are
        // units of the project too, and they are nobody's open document.
        let mut wanted: Vec<PathBuf> = self
            .programs_wanted
            .iter()
            .filter(|key| !self.units_read.contains(*key))
            .map(PathBuf::from)
            .collect();
        wanted.sort();
        if let Some(unit) = wanted.into_iter().next() {
            return Some(unit);
        }

        // **The focused document, and only it.** Every open document used to be a candidate, and twenty files open was
        // measured as **19 whole-program reads in a twenty-second idle window** on `test_cpp` — each re-parsing the
        // same 116 headers, ~90 MB of churn apiece, for tabs nobody was looking at. A program read is the most
        // expensive thing this session does, and the file that was just opened, edited or saved is the one whose
        // answers the next request needs. Everything else keeps the reading its own text gives it, which is what
        // every file nobody has open gets already.
        let focused = self
            .focused_document
            .lock()
            .ok()
            .and_then(|focused| focused.clone());
        let open = focused.into_iter().find(|path| {
            self.documents.is_open(path)
                && !self.units_read.contains(&queue_key(path))
                // **A file that imports a module is not read as a program.** Its `import std;` is not an `#include`:
                // the unit walk would read the file *alone* — measured, 1 file and 33 tokens — and replace its
                // rendering with one that has no `std` in it at all, after which every completion in the file is
                // empty. The module's own unit is what supplies those declarations, and it is read instead
                // (`programs_wanted`, above); the file keeps the reading its own text gives it, with the module's
                // facts reached through `module_interfaces`.
                && self
                    .store
                    .index()
                    .summary(path)
                    .is_none_or(|summary| summary.modules.imports.is_empty())
        });
        if open.is_some() {
            return open;
        }

        // **Then the module interface units**, which are units of the project too and are nobody's open document — the
        // same list the walk above started from, and answered here for a root that a document did not supply.
        let mut wanted: Vec<PathBuf> = self
            .programs_wanted
            .iter()
            .filter(|key| !self.units_read.contains(*key))
            .map(PathBuf::from)
            .collect();
        wanted.sort();
        wanted.into_iter().next()
    }

    // **The next translation unit of the project nobody has read** — gone.
    //
    // `clangd` does not do this either: it parses a translation unit when a file is opened or asked
    // about. A file nobody has open keeps the reading its own text gives it, and the reader's own
    // files still get the compiler's reading one second after they stop typing.
    //
    // What is lost is only ⑲'s widest case — the same header read two ways by two programs *neither of which is
    // open*. Everything a reader can look at keeps its per-program reading.

    // **The two questions that priced a program are gone** — `the_program_is_too_large_to_read_at_once` and
    // `the_cost_of_reading_a_program`. They summed the closure's source bytes and compared that against an eight-megabyte
    // ceiling and a session-wide 512 MB budget, and their justification was a counter that only ever added: measured
    // with one that counts frees as well, seventeen whole-program reads take a session from 250 MB to 478 MB of live
    // bytes. Nothing prices a program now; `docs/open-questions.md` records the measurement that removed them.

    /// **The project's translation units, read once each** — what the pump calls when its queue drains.
    ///
    /// One per call rather than all of them, for the reason the pump is sliced at all: a unit read is a walk, a
    /// render and a parse, and a session that did a hundred of them in one call would hold the writer for as long
    /// as a compiler takes to build the project. [`Session::pending_work`] counts the ones still unread, so a
    /// caller looping on it keeps coming back.
    pub fn read_a_unit_of_the_project(&mut self) -> Option<UnitReading> {
        let root = self
            .project
            .iter()
            .find(|root| !self.units_read.contains(&queue_key(root)))
            .cloned()?;

        let reading = self.read_the_unit(&root);
        self.units_read.insert(queue_key(&root));
        reading
    }

    /// **A unit of one file a reader is looking at**, read as a program — one per call.
    ///
    /// The policy is [`Session::cook`]'s: a reading is built when something looks at the file. What changed is the
    /// **unit** the reading is built for — not "this file and its direct includes", but the whole translation unit
    /// the file is read in, parsed once, with every declaration in it filed under the file it was written in.
    ///
    /// One per call, like every other step here, and bounded by the number of *open* files rather than by the
    /// project: a project with two hundred translation units does not read two hundred programs because one file is
    /// open. [`Session::read_a_unit_of_the_project`] is the caller that wants all of them, and the pipeline's own
    /// schedule is [`Session::a_unit_may_be_read_now`] — this is the caller that says "now" regardless of the clock.
    /// **Is a module unit still waiting to be read as a program?** — the question a request asks before deciding that
    /// there is nothing to do.
    ///
    /// A unit the index already *names* is not a unit the index has *read*: the pump's queue puts it in the project and
    /// asks for a cook, and the program read is a separate step. A caller that skipped the step because the module was
    /// named answered a hover from the raw summaries — measured, "`std::shared_ptr<std::string>` has no member
    /// `c_str`" while the completion at the same cursor offered 187 of `std::string`'s members.
    pub fn a_module_unit_is_waiting_for_its_program(&self) -> bool {
        self.programs_wanted
            .iter()
            .any(|key| !self.units_read.contains(key.as_str()))
    }

    /// **Which class a `.` or `->` written against a type reaches** — the completion's own reader, for a caller that
    /// has a type and needs the class whose members the operator names.
    ///
    /// The hover used to ask `members_of` about the *written* type, so `ptr->c_str` on a `std::shared_ptr<std::string>`
    /// looked `c_str` up in `shared_ptr` — measured, "`std::shared_ptr<std::string>` has no member `c_str`" while the
    /// completion at the same cursor offered 187 of `std::string`'s members. One reader for the question is what makes
    /// the two agree; see [`crate::completion::the_class_after_the_operator`].
    pub fn the_class_after_the_operator(
        &self,
        view: &crate::FileView,
        written: &str,
        arrow: bool,
    ) -> Option<String> {
        let reached = self.the_type_after_the_operator(view, written, arrow)?;
        let reached = crate::sema::types::parse_type_spelling(&reached);
        let bare = reached.class_name().map(str::to_string)?;
        // **The class as the index files it.** A member is recorded under the class's qualified name, and a type
        // deduced from a `std::` function is spelled without the namespace: `make_shared` answers
        // `shared_ptr<Node>` while the class is `std::shared_ptr`, whose `element_type` is the member being read.
        if bare.contains("::") {
            return Some(bare);
        }
        Some(
            crate::index::project::qualified_from_the_index(self.store.index(), &view.path, &bare)
                .unwrap_or(bare),
        )
    }

    /// **[`Session::the_class_after_the_operator`], as the type the operator reaches** — arguments and all.
    ///
    /// The class is what a member list is *keyed* by; the type is what a card needs when the class is a template.
    /// `Cursor<Duo<int, double>>::operator->` returns `const value_type*`, so what an arrow on it reaches is
    /// `Duo<int, double>` — and a member list of the bare `Duo` substitutes nothing, so the card for `it->second`
    /// showed the class template's letter (`B second`) while `members_of("Duo<int, double>")` answered `double`.
    pub fn the_type_after_the_operator(
        &self,
        view: &crate::FileView,
        written: &str,
        arrow: bool,
    ) -> Option<String> {
        let written = crate::sema::types::parse_type_spelling(written);
        // **The pointee, by `element_type` with the type's own arguments substituted** — `shared_ptr<Node>` reaches
        // `Node`, where the completion's resolver answered `std::shared_ptr` because `element_type` is written `_Tp`. See
        // [`crate::index::project::the_pointee_of_a_handle`]; the resolver below stays for a raw pointer and for an
        // `operator->` whose return type is a class.
        if arrow && let crate::sema::types::Type::Named { name, arguments } = &written {
            // **The class as the index files it.** A member is recorded under the class's qualified name, and a type
            // deduced from a `std::` function is spelled without the namespace: `make_shared` answers
            // `shared_ptr<Node>` while the class is `std::shared_ptr`, whose `element_type` is the member being read.
            let class = match name.contains("::") {
                true => name.clone(),
                false => crate::index::project::qualified_from_the_index(
                    self.store.index(),
                    &view.path,
                    name,
                )
                .unwrap_or_else(|| name.clone()),
            };
            if let Some((pointee, _)) = crate::index::project::the_pointee_of_a_handle(
                self.store.index(),
                &view.scopes,
                &view.root,
                &view.path,
                &class,
                arguments,
            ) {
                return Some(pointee);
            }
        }
        crate::completion::the_type_after_the_operator(
            self.store.index(),
            &view.scopes,
            &view.root,
            &view.path,
            &written,
            arrow,
        )
        .map(|(reached, _)| reached.to_string())
    }

    /// **Read one queued module unit as a program, now** — the compiler's reading of a module, for a caller that is
    /// about to answer a question the module supplies a name for.
    ///
    /// A request cannot wait for the pump: a hover on `ptr->c_str` in a file that says `import std;` arrived before the
    /// unit had been read as a program, and the member list of `std::shared_ptr<std::string>` was built from the
    /// closure's **raw** headers — measured, the hover said "`std::shared_ptr<std::string>` has no member `c_str`"
    /// while the completion at the same cursor offered 187 of `std::string`'s members. The unit read is what makes the
    /// two agree, and it is a one-off per session: `units_read` keeps the pump from doing it again.
    ///
    /// `false` when nothing is queued, so a caller can loop until the queue is empty.
    pub fn read_a_module_unit(&mut self) -> bool {
        let Some(key) = self
            .programs_wanted
            .iter()
            .find(|key| !self.units_read.contains(*key))
            .cloned()
        else {
            return false;
        };
        let path = std::path::PathBuf::from(&key);
        // **The whole closure must be summarised before the program is read.** A walk over files the index has not read
        // contributes what it can — and then the reading is filed and marked read, so the *partial* program reading
        // shadows every summary from then on. Measured: in the editor `definitions("std::println", demo.cpp)` answered
        // **0** while the same query in a session that indexed the closure first answered **6**; the difference was
        // exactly this — the request path read `bits/std.cc` before its 400 headers had summaries, so `<print>`'s body
        // was missing from the reading the query then used.
        //
        // The queue is what reads them: `want_the_closure_cooked` walks the includes and queues them, and the drain is
        // the same one the request path already pays for a module (measured: 1.7 s of summaries for `std`, once).
        if self.store.index().summary(&path).is_none() {
            self.load(&path);
        }
        self.want_the_closure_cooked(&path);
        let mut rounds = 0;
        while self.pending() > 0 && rounds < 2_000 {
            self.advance(256);
            rounds += 1;
        }
        let reading = self.read_the_unit(&path);
        // **Marked read only when it was.** A reading that could not be built has to stay retryable, or the unit keeps
        // its raw summaries for the rest of the session.
        if reading.is_some() {
            self.units_read.insert(key);
        }
        reading.is_some()
    }

    pub fn read_a_looked_at_unit(&mut self) -> Option<UnitReading> {
        let root = self.the_next_unit_to_read()?;

        // **A program's root has to have a summary, so build it here when the index has not caught up.** The schedule
        // accepts an open file whose summary is absent — and the read then used to fail, which ended the drain with a
        // translation unit unread. Measured on CI: `both translation units are read: ["/p/a.cpp"]`, one root summarised
        // by `index_everything` and the other not. The one parse is the pump's own `index_one`, so nothing new is
        // introduced: the caller that asks for a program is allowed to do the work the program needs.
        if self.store.index().summary(&root).is_none() {
            let ready = self.prefetched.remove(&queue_key(&root));
            self.index_one(root.clone(), Priority::Open, 0, ready, true);
        }

        let reading = self.read_the_unit(&root);
        // **A reading that could not be built is retried, not marked done.** This used to mark the root read whatever
        // happened, and for a module unit that was fatal: the unit's summary is queued rather than read when the pump
        // first names it, so the first program read found nothing, marked it read, and the whole closure kept its raw
        // summaries for the rest of the session — measured as a hover that said "`std::shared_ptr<std::string>` has no
        // member `c_str`" while the completion at the same cursor offered 187 of `std::string`'s members. Three
        // attempts and it is treated as unreadable, which is the policy `commit_a_program` already had.
        let key = queue_key(&root);
        let attempts = self.units_attempted.entry(key.clone()).or_insert(0);
        *attempts += 1;
        if reading.is_some() || *attempts >= 3 {
            self.units_read.insert(key);
        }
        reading
    }
    /// **Queue the interface units of the modules the open files import**, for the pump to read like any other file.
    ///
    /// The counterpart of [`Session::read_the_modules_a_file_imports`], and the difference is who pays. That one
    /// reads the whole closure **now**, holding the write lock from the first interface unit to the last file it
    /// drags in — measured at **6 151 ms** for `std` on a cold machine. A request that is already waiting on an
    /// answer can afford that; the pump cannot, because its design is that the write lock is held for milliseconds at
    /// a time — and a *diagnostic* cannot either, because a diagnostic is a read and must not mutate the index.
    ///
    /// So this only **queues**: the unit lands in the project and asks to be cooked, and the ordinary slices read it
    /// with the budget the loop already applies. It is called from the pump when the queue has drained, so the files
    /// it adds are read before the end-of-drain refresh rather than after it.
    ///
    /// `std` is the ordinary case and the reason this exists: libstdc++ ships its interface unit as
    /// `<c++ include>/bits/std.cc`, MSVC as `<VC>/Tools/MSVC/<version>/modules/std.ixx`, and neither is part of any
    /// include closure — nothing in the project includes it, so nothing would ever read it on its own.
    ///
    /// Returns how many units were queued, so the caller can go round the loop again instead of publishing an answer
    /// computed before they were read.
    pub fn queue_the_modules_the_open_files_import(&mut self) -> usize {
        let mut queued = 0;
        for path in self.documents.paths() {
            let wanted = self.modules_the_index_cannot_name(&path);
            if wanted.is_empty() {
                continue;
            }

            for unit in self.module_interface_units_of(&wanted, &path) {
                // **Cooked as well as read**: the names a module exports are in its *rendering* — `bits/std.cc`
                // opens with `#include <bits/stdc++.h>` — and a summary of the raw text is the reading without a
                // macro expansion, which for a module source is the reading without most of the names.
                self.add_project_files([unit.clone()]);
                // **And a translation unit, so it is read as one.** The per-file cook beside this is the cheap half — it
                // renders the unit's own text, with `#include` as a boundary — and it is what makes the names appear
                // quickly; the *program* read is what makes them **right**, because it renders every header in the
                // closure inside this unit's environment. Recorded rather than run: the pump reads a unit at its own
                // budget (`the_next_unit_to_read`).
                self.programs_wanted.insert(queue_key(&unit));
                self.want_cooked_reading(&unit);
                queued += 1;
            }
        }
        queued
    }

    /// **Queue the interface units of the modules `path` imports** — the cheap half of
    /// [`Session::read_the_modules_a_file_imports`], for a caller on a request path.
    ///
    /// The read that method does is the **whole closure**, and it is not small: measured on a real project
    /// (`test_module`, `src/std_module_demo.cpp`, `import std;`), it held the analysis's write lock for **1 717 ms**
    /// — `bits/std.cc` → `bits/stdc++.h`, four hundred files — and every diagnostic pull behind it was answered
    /// **1.4 s** late by an index with nothing left to do.
    ///
    /// A query may not spend that. It does not have to: the unit only has to be **queued**, and the pump reads it in
    /// slices under the budget it applies to every other file. The answer to the query that asked is then "not yet",
    /// which is what the protocol's `isIncomplete` is for and is true.
    ///
    /// Returns how many units were queued.
    pub fn queue_the_modules_a_file_imports(&mut self, path: &Path) -> usize {
        // **`[index] modules = false` means exactly that** — no unit read, and therefore no minutes of background CPU
        // after a cache invalidation. A project that turned it off gets no `std` from `import std;`, which is the
        // trade it asked for, and the queries that would have used it answer "not read" instead of a fragment.
        if self.project_config().config.index.modules == Some(false) {
            return 0;
        }
        let wanted = self.modules_the_index_cannot_name(path);
        if wanted.is_empty() {
            return 0;
        }

        let found = self.module_interface_units_of(&wanted, path);
        for unit in &found {
            // The project list is what the pump's scan reads; the cooking queue is what makes the *rendering* be
            // built, and a module source's names are in its rendering (`bits/std.cc` writes `#include
            // <bits/stdc++.h>`, and its `export namespace std { using std::… }` blocks after it).
            self.add_project_files([unit.clone()]);
            self.want_cooked_reading(unit);
            // **And a translation unit, read as one.** The cook above renders the text of the unit itself only
            // (`#include` is a boundary), so the library's headers keep their raw summaries — and a raw summary of
            // `shared_ptr_base.h` files `__shared_ptr_access` as a nested class of `_Sp_counted_ptr_inplace`. The
            // request path reads it as a program (`read_a_module_unit`) before answering, because a hover that arrives
            // first says "no member" about a name the completion at the same cursor offers.
            self.programs_wanted.insert(queue_key(unit));
            // **And queued for the index**, without which nothing ever reads the unit at all: `add_project_files`
            // only records that the file is part of the project, and a file nothing queues keeps no summary — so the
            // pump's program read declined three times and marked the unit unreadable, and `ptr->` stayed at the raw
            // list. The read-now variant queues it the same way.
            self.queue.again(unit.clone(), Priority::Open, 0);
        }
        found.len()
    }

    /// **The module interface units `path` imports, read in** — the files behind `import m;` that are not part of
    /// the project.
    ///
    /// `import mathlib;` reaches `mathlib.ixx` because the project scan indexed it: the visibility walk holds
    /// summaries, and a summary of the *importing* file names a module rather than a file, so the index has to know
    /// which file declares that module. That is `ProjectIndex::module_interfaces`, and it is filled from the files
    /// the project took in — so a module whose interface unit is **outside the project** is a module nothing can see.
    ///
    /// # The case that makes this concrete, and the measurement behind the design
    ///
    /// `import std;` (C++23). MSVC ships the *source* of `std` as `<VC>/Tools/MSVC/<version>/modules/std.ixx` —
    /// 3 194 bytes of `export module std;` followed by an `#include` of every standard header — so the answer is a
    /// file this analysis can read like any other interface unit, and nothing about it is special except that it is
    /// not in the project. Measured on the fixture `tests/fixtures/modules/std_only.cpp` (a file whose only import is
    /// `import std;`), before this existed: `std::string`, `std::vector` and `std::cout` all `None`, and the
    /// completion after `std::` empty. Measured after it, on the same fixture: the **first** call reads 401 files in
    /// **6 151 ms** (nothing cached, which is the first time on a machine), the warm call is **547.7 ms**, and a call
    /// with everything already in the index reads 0 files in **0.004 ms** — `std::string` resolves to `<xstring>` and
    /// `std::cout` to `<iostream>`.
    ///
    /// # Why it is a call and not a step of the pump
    ///
    /// Six seconds of indexing is not work a language server may do for every workspace it opens, and it is not work
    /// to do when nobody asks for a name from `std`. So it is a **request-driven** read, like
    /// [`Session::read_the_unit`]: the caller that is about to answer a name query on `path` says so, and pays for it
    /// once — the next call finds the interface unit already in the index and does nothing.
    ///
    /// # Recursion is a queue, not a stack
    ///
    /// Reading a module in is not reading **one file**. MSVC's `std.ixx` is 3 194 bytes whose entire body is an
    /// `#include` of every standard header, and *those* are where the names are, so a call that stopped at the
    /// interface unit would answer `None` for `std::string` with the module read — measured, and the reason the
    /// interface units are queued in the **open** half and the queue is drained after them rather than left to the
    /// pump.
    ///
    /// `MAX_MODULES_READ_AT_ONCE` bounds the rounds rather than the files: an interface unit that imports a module
    /// whose interface unit imports it back is a cycle, and a cycle terminates on its own (a path already in the
    /// index is never read again) — so the cap is for the graph that is merely large, and the honest answer for the
    /// rest is "not read yet".
    ///
    /// Returns how many interface units were read in, so a caller can say whether anything changed.
    pub fn read_the_modules_a_file_imports(&mut self, path: &Path) -> usize {
        let mut read_in = 0;
        // **Where this call's seconds go**, under the same switch the unit trace uses — the question it exists for is
        // "which of the four phases is the one that costs a minute", and no single number outside can answer it: the
        // same call measured 25 s, 44 s, 112 s and 506 s on unchanged inputs, against a 24 MB cache, so the answer is
        // not the cache and not the closure.
        let tracing = false;
        let started = std::time::Instant::now();
        let mut last = started;

        for _ in 0..MAX_MODULES_READ_AT_ONCE {
            // The module names the index cannot point at a file for, from the file itself and from every file the
            // previous round brought in — a fresh walk each round, because a round is a handful of hashmap lookups
            // and the alternative is a worklist that has to be kept in step with the index.
            let wanted = self.modules_the_index_cannot_name(path);
            if tracing {
                eprintln!(
                    "module read: wanted {:?} after {:?}",
                    wanted,
                    last.elapsed()
                );
                last = std::time::Instant::now();
            }

            if wanted.is_empty() {
                break;
            }

            let found = self.module_interface_units_of(&wanted, path);
            if tracing {
                eprintln!(
                    "module read: resolved {} file(s) {:?} after {:?}",
                    found.len(),
                    found,
                    last.elapsed()
                );
                last = std::time::Instant::now();
            }
            if found.is_empty() {
                break;
            }

            read_in += found.len();
            self.add_project_files(found.clone());

            for unit in found {
                // **The interface unit is a translation unit, so it is read the way a compiler reads one.** This used
                // to queue the unit for a *per-file* cook and drain the index behind it — and a per-file cook renders
                // one file with `#include` as a boundary, so every header in the closure kept its **raw** summary,
                // where both branches of every `#if` are present in the text at once and no compiler would ever see
                // that stream.
                //
                // Measured on `shared_ptr_base.h` read raw: `_Sp_counted_ptr_inplace`'s class body never closes —
                // the class head is written three times under `#if`/`#else` — so `__shared_ptr_access`, hundreds of
                // lines later, is filed as a *nested* class of it, and every member of the real `__shared_ptr_access`
                // is unreachable: `std::shared_ptr<std::string>::operator->` cannot be found, and a completion after
                // `ptr->` offers the `shared_ptr`'s own four names instead of `std::string`'s sixty-eight.
                //
                // `read_the_unit` is the compiler's reading: it walks the closure, renders each file **in this
                // unit's environment** (`#if` decided once, macros expanded), parses that once, and files every file's
                // declarations from it — `insert_cooked_in_a_program`, which is the reading a query about that file
                // asks for (`ProjectIndex::the_reading_for`). Its cost is the closure walk, which this call already
                // paid: **6 151 ms** for `std` on a cold machine, 547.7 ms warm.
                //
                // It is *recorded* here rather than run here because the walk needs the unit's summary, which is
                // queued below and read by the drain — the first round would find nothing. `the_next_unit_to_read`
                // hands it to the pump as a program like any other, which is also what keeps a module's six seconds
                // out of the write lock a request is waiting on.
                self.programs_wanted.insert(queue_key(&unit));
            }

            // **Everything this made reachable, read** — the interface units, the headers they include, and the
            // second pass over the files whose scopes came out of a macro body in one of those headers, which is
            // what `advance` runs when the queue drains (`std::basic_string` is spelled `basic_string` at file scope
            // until `yvals_core.h` has been read).
            //
            // The whole queue, not only these files: `Session::advance` takes the front of the open half and the
            // open half is the project, so a drain that spared the rest would leave the module half-read. The cost is
            // the same work the pump was going to do anyway, moved to the moment a reader asked for an answer that
            // needs it.
            self.index_everything();

            // **And then the module's own unit, read as the program it is** — the step that was only ever *recorded*.
            //
            // The comments above explain at length why the unit is queued rather than read here: the walk needs the
            // summaries the drain is about to read, and a module's six seconds do not belong under a request's write
            // lock. Both are true, and both are about *the pump*: a session with a work loop will pick this up on its
            // next slice. A caller that has no pump — a batch, a test, this probe — asked for `import std;` and gets
            // headers and no module.
            //
            // Measured before this: the module read resolves `std.ixx`, queues it, drains 401 files, and answers
            // **0** of the module's exports — `std::format` is not offered after `import std;`, and reading three
            // *other* programs first does not change that (`probe: reading programs does not make the standard
            // module's exports visible`). The unit read is the missing step: `read_the_unit` walks the closure, renders
            // each file in the unit's environment, parses it once and files every declaration from it
            // (`insert_cooked_in_a_program`), which is where `export namespace std { using std::format; … }` lands.
            //
            // It costs what the comments above say — 6.2 s for `std` on a cold machine, 0.55 s warm — and it is paid
            // **here, after the drain**, so the summaries the walk needs are all in the index by the time it starts.
            let due: Vec<PathBuf> = {
                let mut due: Vec<PathBuf> = self
                    .programs_wanted
                    .iter()
                    .filter(|key| !self.units_read.contains(*key))
                    .map(PathBuf::from)
                    .collect();
                due.sort();
                due
            };
            for unit in due {
                let reading = std::time::Instant::now();
                let read = self.read_the_unit(&unit);
                if tracing {
                    eprintln!(
                        "module read: unit {} in {:?} ({} file(s))",
                        unit.display(),
                        reading.elapsed(),
                        read.map_or(0, |reading| reading.files),
                    );
                }
            }
        }

        read_in
    }

    /// **The module names reachable from `path` that the index cannot point at a file for** — the list
    /// [`Session::read_the_modules_a_file_imports`] reads in, and the question
    /// [`Session::imports_a_module_that_is_not_read`] asks.
    ///
    /// One implementation of both, because they are the same question: the caller that wants to know whether there
    /// is work must not be able to disagree with the caller that does it.
    pub(crate) fn modules_the_index_cannot_name(&self, path: &Path) -> Vec<String> {
        self.modules_in_view_of(path)
            .into_iter()
            .filter(|module| self.store.index().interface_unit_of(module).is_none())
            .collect()
    }

    /// **Would [`Session::read_the_modules_a_file_imports`] read anything?** — the question a caller asks *before*
    /// taking the write lock.
    ///
    /// The call is a **request-driven** read precisely so that it happens once: the second time, the interface unit
    /// is in the index and the answer is "nothing to do" — measured at **0.004 ms** for the work. What is not
    /// 0.004 ms is the write lock it takes to discover that, and the pump holds the session for read while it parses
    /// an include closure.
    ///
    /// So a file that imports nothing — which is nearly every file — asks this, gets `false`, and never queues for
    /// the lock at all. A file that does import a module pays the lock exactly once, which is the call's whole point.
    pub fn imports_a_module_that_is_not_read(&self, path: &Path) -> bool {
        !self.modules_the_index_cannot_name(path).is_empty()
    }

    /// **Which file each of these module names is declared in** — resolved the way `scan_imports` resolves them.
    /// The naming convention, the importing file's own directory, and each include path's sibling `modules`
    /// directory (which is where a standard module lives: `<VC>/Tools/MSVC/<version>/modules/std.ixx` is *beside* the
    /// include directory, not inside it). Going through [`ModuleScanner`] rather than a second implementation is what
    /// keeps "which file is module `m`" a question with one answer in this crate.
    ///
    /// A file already in the index is dropped here: the caller is asking because the index cannot name a file for the
    /// module, and a candidate that the index *does* hold a summary of would mean the map and the summaries disagree.
    pub(crate) fn module_interface_units_of(
        &self,
        wanted: &[String],
        importing: &Path,
    ) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = Vec::new();
        let mut interner = PathInterner::new(self.files.is_case_insensitive());
        let mut scanner = ModuleScanner::new(&self.files, &self.config);

        // **The directory, not the file.** `ModuleScanner::resolve_module`'s naming convention
        // looks for a module "beside the importing file", which it does by joining the name onto
        // the directory it is given — and this call passed the importing *file*, so every candidate
        // became `<dir>/main.cpp/math.cppm`, which is not a path any filesystem has. The result was
        // not an error but a silence: `read_the_modules_a_file_imports` answered **0** for every
        // file, `imports_a_module_that_is_not_read` stayed true for ever, and a request in a module
        // file therefore took the session's write lock on **every keystroke** to discover that
        // there was nothing to read. The other two callers of this resolution (`scan_imports`,
        // `module_reading`) already pass a directory, which is why the standard-module tests
        // passed: their module is found through an include path's sibling `modules/` directory, and
        // the broken first entry was never the one that answered.
        let directory =
            crate::paths::parent_normalized(importing, self.files.is_case_insensitive());

        for module in wanted {
            if let ImportOutcome::Resolved(unit) =
                scanner.resolve_module(module, &directory, &mut interner)
                && let Some(entry) = scanner.units().iter().find(|entry| entry.file == unit)
            {
                found.push(entry.path.clone());
            }
        }

        found.retain(|unit| self.store.index().summary(unit).is_none());
        found.sort();
        found.dedup();
        found
    }

    /// **Every module name reachable from `path` through `import`s** — the file's own, and those of every file that
    /// came into view because of one.
    ///
    /// The same walk the visibility rules use ([`ProjectIndex::visible_files_with_modules`]) would be the wrong
    /// question here: what this needs is not "which files are in view" but "which module names might name a file
    /// nobody has read", and that is answered by following the import edges from summaries the index holds.
    pub(crate) fn modules_in_view_of(&self, path: &Path) -> Vec<String> {
        let mut wanted: Vec<String> = Vec::new();
        let mut pending: Vec<PathBuf> = vec![path.to_path_buf()];
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        while let Some(current) = pending.pop() {
            if !seen.insert(queue_key(&current)) {
                continue;
            }

            let Some(summary) = self.store.index().summary(&current) else {
                continue;
            };

            for imported in &summary.modules.imports {
                if !wanted.iter().any(|held| held == imported.as_ref()) {
                    wanted.push(imported.to_string());
                }

                // …and the file that module is declared in, when the index knows it: a module brought in by a
                // module is as much a file nobody has read as one the file wrote itself.
                if let Some(interface) = self.store.index().interface_unit_of(imported) {
                    pending.push(PathBuf::from(interface));
                }
            }

            // **A module implementation unit implicitly imports its own interface unit**, so `module math;` alone is
            // a reason to read `math.cppm` — the visibility walk crosses the same edge. Without this the file is
            // never read in, because it imports nothing: measured on `test_module/src/math.cpp`, whose every name
            // resolved to nothing until the interface unit was in the index.
            if !summary.modules.is_interface
                && summary.modules.partition.is_none()
                && let Some(module) = &summary.modules.module
                && !wanted.iter().any(|held| held == module.as_ref())
            {
                wanted.push(module.to_string());

                if let Some(interface) = self.store.index().interface_unit_of(module) {
                    pending.push(PathBuf::from(interface));
                }
            }
        }

        wanted
    }

    /// How many units of **looked-at** files have not been read as programs yet.
    ///
    /// # Why this is **not** part of [`Session::pending_work`]
    ///
    /// It was, for one round, and that was a **hang**: a counter of work is a promise that the pump will do it, and
    /// the only step that marks a unit read ([`Session::read_a_looked_at_unit`]) is not wired into the pump — so
    /// `pending_work` never reached zero and [`Session::index_everything`]'s `while pending_work() > 0` spun at full
    /// speed for ever, doing nothing each round. A test binary pegged a core and had to be killed.
    ///
    /// The rule that follows is the one this type already learned once (§8's "a queue that exists for one path must
    /// be invisible to the other"): **a work counter may only count work the pump performs.** A question about
    /// something the pump does not do is a *question*, and it is asked by name.
    pub fn unread_units(&self) -> usize {
        self.documents
            .paths()
            .iter()
            .filter(|path| !self.units_read.contains(&queue_key(path)))
            .count()
    }
}
