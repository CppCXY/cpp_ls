//! **The pump** — cooking what a request needs, in slices, under a budget.
//!
//! Moved here from `session/mod.rs` without a change. `advance` is the server's timer callback: it reads, cooks and
//! commits in bounded slices so a keystroke never waits for a closure the size of a standard library. The waves and
//! drains it drives are the program read's three phases.

use super::*;
use crate::FileSummary;
use crate::TranslationUnit;
use crate::cache;
use crate::index;

impl<F: FileProvider + Clone> Session<F> {
    /// Mark **the files a reader is looking at** for cooking, skipping the ones that already have a reading.
    ///
    /// # What this used to cover, and the measurement that changed it
    ///
    /// Cooking used to cover the open file's **whole closure** and then **every indexed file**. Measured on one real
    /// project — a single `main.cpp` including `<cstdio>`, `<iostream>`, `<optional>` and `<string>`, whose closure
    /// is **138 files**:
    ///
    /// ```text
    /// index (summaries, cached)            cold 24.2 s | warm  4.2 s
    /// cooking every file of the closure    cold 11.7 s | warm 11.9 s   ← the cache cannot help: a reading is not stored
    /// ```
    ///
    /// and the reading that half buys is **one declaration in a thousand**: `cook(<string>)` reports 1000
    /// declarations, of which **1** is one the raw reading does not already have. The rest — the scopes, the
    /// members, the aliases, `std::string` itself — comes from the raw reading once the closure's **macro bodies**
    /// are in hand ([`crate::FileIndexer::with_macro_bodies`]).
    ///
    /// # What is looked at, and why the direct includes are part of it
    ///
    /// ```text
    /// the open files                    what a reader is reading
    /// the files their text names        `#include "api.h"` is a line the reader wrote; a declaration a macro
    ///                                   writes in `api.h` is **`api.h`'s** declaration, and a question asked from
    ///                                   `main.cpp` has to be answered with a jump into `api.h`
    /// ```
    ///
    /// and nothing else. A file one level further down has no reading until something looks at it —
    /// [`Session::want_cooked_reading`] is that "something" for everything the editor did not open, and it is one
    /// file at a time rather than a subtree.
    ///
    /// # Why a file that already has a reading is not wanted again
    ///
    /// Because this runs at **every** idle drain, so "want it" has to mean "if it is not already read": a queue
    /// that is refilled with files that have readings never empties, and a session whose backlog never empties
    /// never reports itself idle — which is a hang, not a slowdown. (Measured the hard way: the pump spun for over
    /// a minute in `a_file_nobody_looked_at_is_not_cooked_and_one_a_request_names_is`, and every request behind it
    /// waited.)
    /// **Cook what a reader of this file will ask about**: its own reading, its direct includes (up to
    /// [`COOK_ONE_LEVEL_FURTHER`]) and the headers those include (up to [`COOK_TWO_LEVELS_FURTHER`]).
    ///
    /// This is what the pump does for **every open document** once the index is idle, and it is the contract a
    /// caller who is *looking at* a file gets: a header is where the declarations behind a `#include` live, and a
    /// header that was never cooked contributes no **cooked** declarations — so a name a macro writes
    /// (`UV_EXTERN int uv_run(…)`) is in neither the shape-only summary nor the index until something cooks it.
    ///
    /// Public because a caller outside the pump can be looking at a file too — `tests/neovim_probe.rs` reads a
    /// project the way an editor does, and asking for one file's reading instead of this was measuring a smaller
    /// machine than the server is.
    pub fn want_the_closure_cooked(&mut self, root: &Path) {
        let already_read =
            |session: &Self, path: &Path| session.store.index().cooked_declarations(path).is_some();

        // **A file the index cannot describe is not a file to want.** `Session::cooking_materials` answers `None`
        // for it — the summary lookup is the first thing a cook needs — so wanting it is a queue entry that can never
        // be served: the slice claims it, gathers no materials, commits nothing, and the call that wanted it (this
        // one, per drain) wants it again. Measured on `import std;` in a real project, that is a **livelock**: the
        // server log showed `index slice: prepared 0, committed 0, write lock held 0 ms, 2 to cook, 0 left` once a
        // second for as long as the window stayed open, and every diagnostic pull — which waits for
        // `pending_work() == 0` — took **1.4 s** to be answered by an index that had nothing left to do.
        //
        // The two files in that measurement were the standard module's source and its companion, wanted by a file
        // that says `import std;`: they are reachable through the *module* graph, not through `#include`, so no read
        // ever filed a summary for them under the path this walk knows.
        let can_be_cooked =
            |session: &Self, path: &Path| session.store.index().summary(path).is_some();

        let direct = self.direct_includes_of(root);
        // **And a file a cook has already refused is not wanted again.** The guard above covers the file the index
        // cannot describe; a file whose *materials* cannot be gathered — a provisional unit, a render that declines —
        // passes it, is claimed, builds nothing, and is wanted again by the next drain. Measured on a real session:
        // `4 to cook, 1 left` / `2 left` / `0 left` every second, forever, at 28 % of a core.
        let refused = |session: &Self, path: &Path| {
            session
                .cooking_refused
                .lock()
                .map(|refused| refused.contains(&queue_key(path)))
                .unwrap_or(false)
        };
        // **And a file this session has already asked for is not asked for again.** `already_read` answers from the
        // index's *bounded* cooked map, so an evicted reading reads as "nobody cooked this" and this loop — once per
        // open document per idle drain — queued it a second, third and fourth time. See
        // [`Session::cooked_or_wanted`] for the measurement that made the difference between a bounded pump and one
        // that never idles.
        let asked = |session: &Self, path: &Path| {
            session
                .cooked_or_wanted
                .lock()
                .map(|asked| asked.contains(&queue_key(path)))
                .unwrap_or(false)
        };
        let remember = |session: &Self, path: &Path| {
            if let Ok(mut asked) = session.cooked_or_wanted.lock() {
                asked.insert(queue_key(path));
            }
        };

        for included in &direct {
            if !already_read(self, included)
                && !asked(self, included)
                && can_be_cooked(self, included)
                && !refused(self, included)
            {
                remember(self, included);
                self.cooking.want(included);
            }
        }

        // **One level further, because a macro that opens a namespace lives one file down.**
        //
        // MSVC's `<string>` is a thin layer: it declares `getline` and the numeric conversions, and everything a
        // reader actually asks for — `basic_string`, and the `string` alias itself — is written in `<xstring>`,
        // which `<string>` includes. A **raw** reading of `<xstring>` cannot place any of it: its text says
        // `_STD_BEGIN`, not `namespace std {`, so every declaration in it is filed at **file scope** —
        // `::basic_string`, not `std::basic_string` — and the macro's own body is only substituted by a cook.
        //
        // Measured on a file whose whole text is `#include <string>`:
        //
        // ```text
        // std::  → 23 items  ["getline", "stod", "stof", "stoi", "stol", "stold"]
        //           every one of them `<string>`'s own, and `std::string` is not there
        // #include <xstring> instead → 25 items, `basic_string` among them
        // ```
        //
        // So the level that was missing was exactly the one between the two, and this is it. It is bounded
        // because the unbounded version is the 11.7 s above: a translation unit that includes all of the standard
        // library has hundreds of files at this depth, and cooking is a render and an index per file.
        let mut budget = COOK_ONE_LEVEL_FURTHER;
        let mut nested_files: Vec<PathBuf> = Vec::new();
        for included in &direct {
            for nested in self.direct_includes_of(included) {
                if budget == 0 {
                    break;
                }
                if already_read(self, &nested) || asked(self, &nested) || refused(self, &nested) {
                    continue;
                }
                if !nested_files.contains(&nested) {
                    nested_files.push(nested.clone());
                }
                remember(self, &nested);
                self.cooking.want(&nested);
                budget -= 1;
            }
        }

        // **And one level beyond that**, which is where a class a header *uses* is declared.
        //
        // The level above reaches the headers a file includes; the class those headers talk about is often one more
        // `#include` down. MSVC's `<string>` is the ordinary case: it is a thin layer over `<xstring>`, and
        // `basic_string` — and therefore `std::string` — is declared in `xstring`. Measured on EmmyLuaCodeStyle's
        // `LuaParser/src/File/LuaSource.cpp` (whose `LuaSource.h` includes `<string>`):
        //
        // ```text
        //                    std::string before   after cooking `xstring` by hand
        //   members_of        NotDeclaredHere      Yes(222 members)
        // ```
        //
        // — so `std::string` was unknown in a file that includes the header that includes the header that declares
        // it, and the same reading left `std::basic_string` and `std::size_t` unknown. The cook of `xstring` costs
        // **2.17 s** and happens once per session.
        //
        // Bounded like the level above, and for the same reason: this is a queue of work for the pump, and a
        // translation unit that includes the standard library has hundreds of files at every depth.
        let mut budget = COOK_TWO_LEVELS_FURTHER;
        for nested in &nested_files {
            for deeper in self.direct_includes_of(nested) {
                if budget == 0 {
                    break;
                }
                if already_read(self, &deeper) || asked(self, &deeper) || refused(self, &deeper) {
                    continue;
                }
                remember(self, &deeper);
                self.cooking.want(&deeper);
                budget -= 1;
            }
        }

        if !already_read(self, root) && !asked(self, root) {
            remember(self, root);
            self.cooking.want(root);
        }
    }

    /// The files `path` includes **directly**, as the index resolved them.
    pub(crate) fn direct_includes_of(&self, path: &Path) -> Vec<PathBuf> {
        self.store
            .index()
            .summary(path)
            .map(|summary| {
                summary
                    .includes
                    .iter()
                    .filter_map(|include| include.resolved.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// **Mark one file for cooking** — "something is about to look at this".
    ///
    /// The entry point for a caller that knows which file a question is about: the shell calls it for every file a
    /// request names, so the answer *after* this one is the compiler's reading and this one is the file's own. It
    /// is deliberately not a "cook it now": the caller is a query, the cooking is the pump's work, and a keystroke
    /// must not wait for a unit walk.
    /// **The diagnostics of a whole project**, which until now no consumer could ask for.
    ///
    /// `diagnostics` answers for a file whose **cooking** the index holds, and a reading is cooked because something asked
    /// for one — so a batch consumer had to know three things the server's own pump knew and nothing published: that cooking
    /// is a queue of its own, that `advance` does not drive it, and that its slices must be rendered *and committed*. Measured
    /// on a 1216-file project: cooking all of it took 56 s, and a consumer that drove only `advance` cooked 46 files and saw
    /// nothing.
    ///
    /// This is that loop as one method: ask for every rendering, drain the index queue, drain the cooking queue in slices
    /// (rendering each and committing it), then answer with every file the index has.
    ///
    /// ```text
    /// 160 s   index       the caller does this, or `index_everything`
    ///  56 s   cook        this method
    ///   …     diagnose    the checks, per file
    /// ```
    pub fn diagnose_everything(
        &mut self,
        up_to: usize,
    ) -> Vec<(std::path::PathBuf, FileDiagnostics)> {
        let paths: Vec<std::path::PathBuf> = self
            .store
            .index()
            .summaries()
            .map(|summary| summary.path.clone())
            .take(up_to.max(1))
            .collect();

        self.want_everything_cooked();

        // The index queue first: a reading cannot be cooked for a file whose summary is not there.
        let mut waves = 0usize;
        while self.pending() > 0 {
            if self.advance(64).is_empty() {
                break;
            }
            waves += 1;
            if waves > 100_000 {
                break;
            }
        }

        // **Then the cooking queue**, in the pump's own three steps: claim a slice, build its materials, render **and
        // commit** them. Committing is the step that makes the reading visible to `diagnostics` — a rendering that is
        // returned and dropped is a rendering nobody can ask about.
        let mut cooked = 0usize;
        loop {
            let claimed = self.claim_cooking(64);
            if claimed.is_empty() {
                break;
            }
            let materials = self.materials_for(claimed);
            let rendered = self.render_them(materials);
            cooked += self.commit_them(rendered).len();
            if cooked > up_to.max(1) * 2 + 1024 {
                break;
            }
        }

        paths
            .into_iter()
            .filter_map(|path| self.diagnostics(&path).map(|found| (path, found)))
            .collect()
    }
    pub fn want_cooked_reading(&mut self, path: &Path) {
        // **A refusal is not cleared by asking again, and that is the fix for a live loop.** It used to be: every
        // diagnostics pull called this, cleared the refusal of the file the pull was about, and re-queued it — so a
        // file whose cook can never succeed was queued, claimed, refused, cleared and queued again, for as long as
        // the window stayed open, at one core. Measured from a real session's log: an `index slice` line every
        // ~250 ms, minutes after the index had drained. A refusal is cleared where the *reason* could have changed —
        // an edit ([`Session::buffer_changed`]) — not by a request that changes nothing.
        let refused = self
            .cooking_refused
            .lock()
            .map(|refused| refused.contains(&queue_key(path)))
            .unwrap_or(false);
        if refused {
            return;
        }
        // **And read it, if nothing has.** A reading is built out of a summary ([`Session::cook`] answers `None`
        // without one), and a file a request names may be one no pump has reached — a header outside every
        // translation unit, a file the workspace scan did not list. Wanting it cooked and not reading it is a
        // promise the session never keeps: the next request gets the same raw reading and the `isIncomplete` flag
        // goes on telling the client to come back for an answer that is not coming.
        if self.store.index().summary(path).is_none() {
            self.queue.add(path.to_path_buf(), Priority::Open, 0);
        }
        // **"Want" means "if it is not already read", and that guard was missing here.** The same rule
        // [`Session::want_the_closure_cooked`] has had all along, and the reason it has it: a queue refilled with
        // files that have readings never empties. Every diagnostics pull calls this for the file it is about, so
        // without the guard **every pull cooked the open file again** — measured on a real project with
        // `CPPLS_TRACE_COOKING`, ten pulls in 2.8 s: `cook slice: asked 1, rendered 1` ten times, each a render and a
        // parse of a 4 000-line file, and the same again for one of its includes.
        //
        // The reading is dropped by [`Session::buffer_changed`] the moment the text or an included file changes, so a
        // file that *needs* a new reading never reaches this line with a stale one.
        //
        // **"Has a reading" is not "has what a view needs".** A reading the store **served** carries the facts and no
        // rendering — `take_the_stored_refinements` inserts the record and marks the file refined, and a rendering is
        // the caller's half. A file in that state answered this line's `return` and never got a rendering, so every
        // `view` of it was `None` — which is why the wave used to queue **all** served files for a full cook, eagerly,
        // and why a warm start of the modules fixture cost 20–506 s doing renders and parses for facts the store had
        // already answered. The question this line has to ask is therefore the *other* one: is there a rendering for
        // this text? If not, the cook is still owed, and it is owed when someone asks — this call — rather than for
        // every file a wave served.
        if self.store.index().cooked_declarations(path).is_some() {
            let rendered = self
                .vfs
                .held(path)
                .is_some_and(|file| self.known_rendering_of(&file.path, &file.text).is_some());
            if rendered {
                return;
            }
        }
        self.cooking.want(path);
    }

    /// **Would [`Session::want_cooked_reading`] change anything for this file?** — the question a caller asks *before*
    /// taking the write lock.
    ///
    /// # Why this exists, and what it is worth
    ///
    /// A request's wait is the time until the lock it needs is free, and the pump holds the session for a hundred
    /// milliseconds at a time and sometimes for more than a second — it is parsing a project's include closure, and
    /// parsing has to read the index. So a request that takes the **write** lock for work it does not have to do
    /// waits for all of it. Measured over the wire on the latency fixture: a completion's three write-lock
    /// acquisitions cost **8, 9, 0 ms** of work and **9, 8, 499, 427, 137, 293, 179, 1147, 119, 306 ms** of *wait* —
    /// every millisecond of it the queue, none of it the work.
    ///
    /// And the work is usually nothing at all: the file a request is about is normally one the editor has open, so
    /// the VFS is holding its text (`didOpen`) and its cooked reading is either built or already asked for.
    ///
    /// # Why it is not a second opinion about anything
    ///
    /// It asks the two questions the mutation itself asks, of the same state: the VFS's own `held`
    /// table, the index's own cooked-reading slot, and the cooking queue's own membership set
    /// ([`Cooking::wants`], which reads the set [`Cooking::want`] inserts into).
    ///
    /// Conservative in the safe direction: `false` means "take the lock and do the work", which is always correct.
    /// `true` means the two calls above would each do nothing, so skipping them changes no answer — the query that
    /// follows reads what is already there.
    pub fn is_ready_for_a_request_about(&self, path: &Path) -> bool {
        self.vfs.held(path).is_some()
            && (self.store.index().cooked_declarations(path).is_some() || self.cooking.wants(path))
    }

    /// **Mark every indexed file for cooking** — the project-wide sweep, kept behind the caller's choice rather
    /// than done on every drain.
    ///
    /// It is what a client that publishes diagnostics for **every file in the workspace** needs (the workspace
    /// pass, and a pull request about a file it is showing): a file with no cooked reading is answered from its own
    /// text, where a declaration a macro writes is a guess and a branch nobody takes is still there. It is also
    /// what made starting a one-file project cost **11.7 s** (the measurement in
    /// [`Session::want_the_closure_cooked`]), which is why the session no longer does it by default: what is looked
    /// at is cooked, and a file nobody looks at is answered from its own text and says so
    /// ([`crate::DiagnosticReading`]).
    ///
    /// A caller that wants the whole project read as a compiler reads it says so — that is what this is for.
    pub fn want_everything_cooked(&mut self) {
        let wanted: Vec<PathBuf> = self
            .store
            .index()
            .summaries()
            .filter(|summary| {
                self.store
                    .index()
                    .cooked_declarations(&summary.path)
                    .is_none()
            })
            .map(|summary| summary.path.clone())
            .collect();

        for path in wanted {
            self.cooking.want(&path);
        }
    }

    // ---------------------------------------------------------------------------------------------
    // The work
    // ---------------------------------------------------------------------------------------------

    /// Index up to `steps` files, and say what each one did.
    ///
    /// One file per step, in the order [`Worklist`](crate::Worklist)'s documentation fixes — what the user is
    /// looking at, then what those files include, then the rest of the project — because that order is the
    /// difference between a first answer after one file and after ten thousand. The session's list is not a
    /// [`Worklist`](crate::Worklist) for two reasons: it outlives any one batch (a notification re-seeds it), and a
    /// `Worklist` borrows the store mutably, so a session holding one could not answer a query between two steps.
    /// The *order* is the same one, and the outcome of a step is read by the same rule ([`outcome_of`]).
    ///
    /// A caller decides how much: one step per idle tick is a server that stays responsive, `advance(64)` is a
    /// server that catches up after a notification, and the loop in [`Session::index_everything`] is what a test or
    /// a batch caller wants.
    ///
    /// # The parallel half, and why it changes no answer
    ///
    /// Reading a file into the index is a parse, and a parse is a pure function of the file: the summary is made of
    /// the text, the configuration and the filesystem, and never of what the index already holds
    /// ([`SummaryStore::prepare`]). So the files at the head of the queue are **prepared together**, on the machine's
    /// cores, before the loop starts — and then the loop is what it always was: pop the next file, put its summary in
    /// the index, queue what it includes. The order the index sees files in, the order includes are discovered in and
    /// what a step reports are all the sequential ones; only the waiting is shared out. A prepared file the loop does
    /// not reach in this call (a file it discovers outranks it) is simply dropped and read again when its turn comes.
    pub fn advance(&mut self, steps: usize) -> Vec<Step> {
        let mut done = Vec::new();

        // **The read-ahead is trimmed before it is added to**, so the budget is a ceiling on what this call holds and
        // not something the call can overshoot. Measured on `EmmyLuaCodeStyle`: a session that had indexed the
        // project held **183 MB** of prepared-but-uncommitted slices, because the bound on the read-ahead was a
        // *count* (`steps * 4` entries) and one entry of a standard-library header is a megabyte.
        self.trim_the_read_ahead();

        // **What the last round of queries asked to have refined** — see `ProjectIndex::ask_for_refinement`. A query
        // cannot walk a closure, so it leaves the file here; the pump serves it in `finish_a_slice` when idle, the
        // same way it serves a file a request named. Measured on `EmmyLuaCodeStyle`: `std::allocator_traits` answered
        // `NotDeclaredHere` and `auto pos = _events.size();` stopped at `_Alty_traits::size_type`, because
        // `<xmemory>` had never been asked for and its `_STD_BEGIN` scopes were therefore absent.
        for asked in self.store.index().take_refinement_asks() {
            self.want_refinement(&asked);
        }

        // **What one batch costs**, under the unit trace. The modules fixture's module read is nine of these and
        // 48–59 s, and no number outside could say which of the nine — or which half of a batch, the wave's closure
        // preparation or the per-file steps — was the seconds.

        for taken in 0..steps {
            let Some((path, priority, depth)) = self.queue.pop() else {
                break;
            };

            // **A file nobody prepared is the front of a wave**: the file just popped, the files queued behind it,
            // and — this is the part that makes the wave as wide as the project — everything *they* include, found by
            // scanning `#include` lines before any of them is parsed ([`SummaryStore::prepare_closure`]). The first
            // file of a closure is read alone; the moment it has been lexed, every core is reading what it names.
            //
            // Bounded by what this call was asked to do, twice over: at most two calls' worth of files are read
            // ahead, and the rest wait for the next call — a slice that read a whole closure would hold the writer
            // for the whole of a cold start.
            let key = queue_key(&path);
            let left = steps - taken;
            // **A file that is going to be read through a timeline is left out of the wave.** The wave parses on the
            // read path, where a summary is a pure function of the text — which is exactly what cannot place a scope
            // a macro in **another** file opens. For the one file this session knows better about, the wave would
            // build the wrong reading *and write it to the disk cache*, and then `index_one`'s own attempt would be
            // served that entry as a hit: measured, dropping the wave's answer afterwards turned the step into
            // `Reused` and left the environment unused, which is worse than either.
            //
            // So it is not a root of this wave, and its includes are prepared by the iteration that reads it.
            // **This is what makes §5.2's steps 1–3 reachable at all**: they live at `index_one`'s `None` arm, and
            // the pump's path always reached it with `Some`, so the mechanism was correct, tested, and dead.
            //
            // The question is [`Session::will_read_it_as_the_program`], which is the timeline in hand **or one that
            // can be walked now** — the file a reader is typing in, whose unit was evicted from the bounded table or
            // has never been built. It is asked here, before the wave, because after it the raw reading would already
            // have been written down.
            //
            // **And only when a wave is about to be built.** `left == 1` is the single-file step — the one a pull and
            // a `didOpen` take — and it never consults `prefetched` for a gate: with nothing prepared, `index_one`
            // decides for itself, so asking here would be the same question twice and the second answer is the one
            // that matters.
            let wave_would_parse_it_as_text = left > 1
                && !self.will_read_it_as_the_program(&path)
                && !self.prefetched.contains_key(&key);
            if wave_would_parse_it_as_text {
                let mut wave = vec![path.clone()];
                wave.extend(
                    self.queue
                        .peek(left - 1)
                        .into_iter()
                        .filter(|queued| !self.prefetched.contains_key(&queue_key(queued))),
                );

                let room = (steps * 4).saturating_sub(self.prefetched.len());
                let made = {
                    let queue = &self.queue;
                    let prefetched = &self.prefetched;
                    self.store.prepare_closure(
                        &wave,
                        |target| {
                            !queue.is_worked(target) && !prefetched.contains_key(&queue_key(target))
                        },
                        room.min(steps * 2).max(wave.len()),
                    )
                };

                for (path, made) in made {
                    self.remember_a_prepared(path, made);
                }
            }

            // **Read here rather than out of the wave**, for the file the wave was told to leave alone: with no
            // prepared entry, `index_one` builds the summary itself and can hand it the environment. Every other
            // file still takes the wave's answer.
            let ready = self.prefetched.remove(&key);
            done.push(self.index_one(path, priority, depth, ready, true));
        }

        // Nothing left to read means nothing left to read *ahead*: what is still here was prepared for a queue that
        // has since been emptied by something else, and would be stale by the next time it is looked at.
        if self.queue.pending() == 0 {
            self.forget_the_read_ahead();
        }

        // **The units are the drain's, and `finish_a_slice` builds them for exactly the slice it takes.** This used
        // to root them at `done` — the files this call just read — and the two are different sets: the drain takes
        // its files from `parsed_since_the_last_pass`, a backlog that outlives one call. Rooted at `done`, a slice
        // whose files the units do not cover is re-read in the pass's fallback form (a closure walk per file), and
        // with `advance(64)` that is up to **sixty-four timeline walks per call**: measured through
        // `examples/checks_probe --index-only` on `EmmyLuaCodeStyle`, **224 s** against **20 s** for the same index
        // through the pump, which had already been given the right slice. Passing nothing is not "passing no
        // environment" — `finish_a_slice` builds the four the pass needs, outside a writer this path never holds.
        let units = Vec::new();

        let drain = { self.finish_a_slice(&done, units) };

        // **A file this slice read is NOT queued for cooking, and three tests are why.**
        //
        // It was tried, and it is the third time this session that "queue more things to cook" has been the wrong
        // answer:
        //
        // ```text
        //   a_file_nobody_looked_at_is_not_cooked_and_one_a_request_names_is   FAILED
        //   a_header_the_user_never_opened_is_cooked_when_a_request_names_it   FAILED
        //   a_unit_read_reads_the_whole_program_once                           FAILED
        // ```
        //
        // They assert that **a request is what causes a cook**, and they are right: reading a file is not asking a
        // question about it, and a project of ten thousand files must not be rendered because a scan walked past
        // them. The coverage a caller needs for a specific file is [`Session::want_cooked_reading`] — one file, named
        // by whoever wants it — which is what the completion fixtures now call before asking. The reader is what
        // decides what is worth reading the way a compiler reads it; the pump is not.

        // **This path pays for its own drain, here.** `advance` is the non-pump entry point — a test, a batch caller —
        // and it holds no lock at all, so the split that exists to keep the pump's write lock short costs it one
        // call each way. The answer is the same: what `finish_a_slice` planned is worked out and applied.
        let prepared = self.prepare_the_drain(drain, usize::MAX, usize::MAX);
        let _committed = self.commit_the_drain(prepared);

        // **And a slice of the cooking queue, which this path used to leave to two other callers.** `index_everything`
        // and `diagnose_everything` both claim cooks; `advance` — the server's timer callback — did not, so a file a
        // *query* asked to have rendered (`Session::view`'s ask, the demand-driven contract) had no reader while the
        // index queue was empty: measured on `EmmyLuaCodeStyle`, with `LSP.cpp` open, `pending_cooking()` stayed at
        // **33** and `re_read_backlog()` at **1 681** across 4 000 calls, and `pending_work()` — which counts the cook
        // queue — never reached zero, so every caller looping on it spun while nothing was rendered.
        //
        // The gate is the index queue's emptiness, for the reason the cooking queue has always been served after it: a
        // render needs the file's summary, and a summary is what the queue produces.
        if self.is_idle() {
            let claimed = self.claim_cooking(COOK_SLICE);
            if !claimed.is_empty() {
                // **Each stage's count, because a file can be claimed and then dropped by any of them** — the hole
                // `docs/open-questions.md` names "claimed and dropped": `materials_for` when a file's unit is
                // provisional or its summary absent, `render_them` when the render declines, `commit_them` when there
                // is nothing to insert. Without the three numbers a dropped file and a cooked one look the same.

                let materials = self.materials_for(claimed);
                let rendered = self.render_them(materials);
                self.commit_them(rendered);
            }
        }

        // **And a whole program if one is owed**, in the same three phases the pump takes apart — this path has no
        // lock to keep free, so it takes them in a row. The clock and the budget are inside
        // [`Session::read_a_program_now`] (`the_program_to_read`), so a caller that has just edited reads nothing.
        //
        // **Where a batch's seconds go, when they go anywhere.** The timer here exists because everything before it
        // measures in milliseconds: pre-finish, `finish_a_slice` and `prepare_the_drain` are all under 60 ms on the
        // modules fixture, and the batches that take 11 s and 47 s have *nothing else* in them. A program read is a
        // closure walk, a render of every file in it, a parse of the result and a sweep of every declaration —
        // 8.9 s for `std.ixx` measured on its own (`tests/unit_probe.rs`), and this is where a *batch* pays it.
        if self.is_idle() && self.pending_cooking() == 0 && self.refinements_wanted() == 0 {
            self.read_a_program_now();
        }

        done
    }

    /// **Prepare everything a slice of `steps` is going to need, holding nothing but `&self`.**
    ///
    /// The expensive half of indexing a wave — reading the text, lexing, parsing, building a summary — is a pure
    /// function of the file, the configuration and the filesystem (`SummaryStore::prepare`). It changes nothing, so
    /// it does not need the write lock that a query is waiting for, and this is the entry point that lets a caller
    /// pay for it under a **read** lock instead.
    ///
    /// [`Session::commit_a_wave`] is the other half: it takes the same files and puts their summaries in the index,
    /// which is cheap and needs `&mut self`. The pair exists because of what the write lock costs — measured, a slice
    /// of thirty-two files held it for **84, 503, 882, 1190 and 2430 ms**, so every completion arriving inside that
    /// window waited for all of it. With the parse moved out, the same slice holds it for **9–12 ms**.
    pub fn prepare_a_wave(&self, steps: usize) -> PreparedWave {
        self.prepare_wave(steps, false)
    }

    pub fn prepare_background_wave(&self, steps: usize) -> PreparedWave {
        self.prepare_wave(steps, true)
    }

    pub(crate) fn prepare_wave(&self, steps: usize, defer_open_files: bool) -> PreparedWave {
        let revision = Some(self.revision());
        // **The files [`Session::commit_a_wave`] is about to pop**, which is what `peek` answers — the queue as it
        // stands now, without taking anything. A file a step discovers later can outrank these and is prepared on a
        // later wave, which is why the committing half still has a slow path.
        let candidates: Vec<PathBuf> = self
            .queue
            .peek(steps)
            .into_iter()
            .filter(|path| {
                let key = queue_key(path);
                !self.prefetched.contains_key(&key)
                    && !self.queue.is_worked(path)
                    && (!defer_open_files || !self.documents.is_open(path))
            })
            .collect();
        let mut prepared = Vec::new();
        let mut wave = Vec::new();
        for path in candidates {
            if self.documents.is_open(&path) {
                let spelled = self
                    .store
                    .index()
                    .summary(&path)
                    .map(|summary| summary.path.as_path())
                    .unwrap_or(&path);
                if let Some(unit) = self.the_timeline_to_read_through(spelled) {
                    if prepared.is_empty() {
                        let reading = self.store.prepare_with_the_timeline(&path, &unit);
                        prepared.push((path, reading));
                    }
                    continue;
                }
            }
            wave.push(path);
        }

        // **The units, rooted at the files the pass after this one will actually re-read** — which is the slice
        // [`Session::finish_a_slice`] is about to drain, **not** the wave about to be parsed.
        //
        // The first version of this asked for units over `wave`, and it measured **worse** — 2636 ms against
        // 1105 ms — for a reason that is worth writing down, because it is a property of what a unit *is*: a
        // translation unit is a **timeline over a whole include closure**, so rooting one at thirty-two files
        // produces a bigger timeline than rooting one at four, and `definitions()` on it costs more. The cache is
        // keyed by the content of the closure as well, so the two are separate entries and the walk happens twice.
        //
        // What the second pass costs is set by the slice it drains, and the reason is in `re_read_what_a_body_changes`:
        // it scans every `#define` in the project and narrows the scan to the names the re-read files' **texts**
        // mention. So the slice chooses how many names are in play, and the measurement agrees exactly — 1 file is
        // 150 ms, 8 is 1105 ms, and a wave of 32 is 2636 ms.
        //
        // `parsed_since_the_last_pass` is a field, so the slice the commit will drain is known *here*: take the same
        // prefix it will take, and the units cover precisely the files they are used for.
        //
        // **Computed before the empty-wave return below, and that is a measured defect rather than tidiness.** The
        // last stretch of every start-up is the drain phase: the index queue empties, and the pump then commits
        // slice after slice of re-reads, each wave having **nothing to prepare** — so this function returned early
        // and handed the drain `units: []`. `prepare_the_re_read` reads that as "a caller with no timelines" and
        // changes shape: it copies **every indexed file's text** into a map and asks a question about **every
        // `#define` in the project** (see the `eager` arm there). Measured on `EmmyLuaCodeStyle` (1761 files): a
        // warm start was **66 s**, of which 60 s was twelve such calls — 5 s each for four files — while the index
        // itself took 4 s. One unit per drained slice is what the pass was designed around, and the early return was
        // silently withholding it.
        let slice: Vec<PathBuf> = self
            .parsed_since_the_last_pass
            .iter()
            .take(RE_READ_SLICE)
            .cloned()
            .collect();
        // **What the cache already refined, before any timeline is built.** A file whose reading was refined under
        // its own closure needs no unit, no render and no parse: the summary goes into the index at commit time. What
        // is left is the honest work, and the timelines are built for that and nothing else.
        let mut refinements: Vec<FileSummary> = Vec::new();
        for path in &slice {
            // **A miss for this content is permanent, so it is asked once** — see the field.
            let key = self
                .store
                .index()
                .summary(path)
                .map(|summary| (queue_key(path), summary.key.content_hash));
            if key.as_ref().is_some_and(|key| {
                self.refinement_misses
                    .lock()
                    .is_ok_and(|misses| misses.contains(key))
            }) {
                continue;
            }
            if let Some(summary) = self.store.the_stored_refinement(path) {
                refinements.push(summary);
            } else if let Some(key) = key
                && let Ok(mut misses) = self.refinement_misses.lock()
            {
                misses.insert(key);
            }
        }
        // **No timelines are built for the pass any more** (phase 2): the reading is composed from the index's macro
        // facts, so the whole-closure walk was the cost of a question it no longer answers. `units` stays in the plan so
        // the callers that still expect one see an empty list rather than a different shape.
        let units = Vec::new();

        if wave.is_empty() {
            return PreparedWave {
                prepared,
                units,
                refinements,
                revision,
            };
        }

        let room = (steps * 4)
            .saturating_sub(self.prefetched.len())
            .max(wave.len());
        let queue = &self.queue;
        let prefetched = &self.prefetched;
        prepared.extend(self.store.prepare_closure(
            &wave,
            |target| {
                !queue.is_worked(target)
                    && !prefetched.contains_key(&queue_key(target))
                    && (!self.documents.is_open(target)
                        || wave.iter().any(|root| queue_key(root) == queue_key(target)))
            },
            room,
        ));
        PreparedWave {
            prepared,
            units,
            refinements,
            revision,
        }
    }

    /// **Throw the read-ahead away**, because what it was prepared for no longer holds: an edit, a close, a
    /// filesystem event, an emptied queue. The two halves go together — a key left in the order is harmless (the trim
    /// skips it), but a map with no order would let a later trim drop entries it cannot account for.
    pub(crate) fn forget_the_read_ahead(&mut self) {
        self.prefetched.clear();
        self.prefetched_order.clear();
    }

    /// **How much read-ahead this session may hold**, in bytes of prepared summaries.
    ///
    /// The bound used to be a *count*: `steps * 4` entries, which on a 64-step slice is 256 files. That is a bound on
    /// the number of answers and not on their size, and a standard-library header's prepared summary is a megabyte —
    /// measured on `EmmyLuaCodeStyle`, a session with the project indexed held **183 MB** of prepared-but-uncommitted
    /// slices. A read-ahead exists to make the *next* slice cheap; it does not have to be a slice and a half ahead of
    /// anything, and 32 MB is several slices' worth of the files a reader is likely to reach.
    const READ_AHEAD_BUDGET_BYTES: usize = 32 * 1024 * 1024;

    /// Put a prepared slice in the read-ahead, remembering when it was made.
    fn remember_a_prepared(&mut self, path: std::path::PathBuf, made: index::store::Prepared) {
        let key = queue_key(&path);
        if self.prefetched.insert(key.clone(), made).is_none() {
            self.prefetched_order.push_back(key);
        }
    }

    /// **Drop the files furthest ahead until the read-ahead is inside its budget.**
    ///
    /// The newest entries go first: the order is the order the waves were prepared in, so the entry made last is the
    /// one the queue reaches last, and a later wave will prepare it again if it is still wanted. Nothing about the
    /// answer changes — a file with no prepared slice is read when its turn comes, which is the same path a file
    /// outside the read-ahead already takes.
    fn trim_the_read_ahead(&mut self) {
        let mut total = self
            .prefetched
            .values()
            .map(index::store::Prepared::heap_bytes)
            .sum::<usize>();
        while total > Self::READ_AHEAD_BUDGET_BYTES {
            let Some(newest) = self.prefetched_order.pop_back() else {
                break;
            };
            if let Some(dropped) = self.prefetched.remove(&newest) {
                total = total.saturating_sub(dropped.heap_bytes());
            }
        }
        // **And the paths already served leave the order**, so a long session does not grow a key list it never reads.
        // The map is the authority: a key that is no longer in it is exactly one this loop has nothing to do for.
        if self.prefetched_order.len() > self.prefetched.len().saturating_mul(2) {
            let known = &self.prefetched;
            self.prefetched_order.retain(|key| known.contains_key(key));
        }
    }

    /// **Put what [`Session::prepare_a_wave`] prepared into the index**, and queue what it includes.
    ///
    /// The cheap half: no parsing, no reading, no walking — `SummaryStore::commit` per file and `Worklist::add` for
    /// each include it names. Everything the parse needed was paid for under the read lock, and so was the unit walk
    /// that the pass after this one needs.
    /// **Put what [`Session::prepare_a_wave`] prepared into the index**, and queue what it includes.
    ///
    /// The cheap half: no parsing, no reading, no walking — `SummaryStore::commit` per file and `Worklist::add` for
    /// each include it names. Everything the parse needed was paid for under the read lock, and so was the unit walk
    /// that the pass after this one needs.
    ///
    /// **What is left to do comes back as a [`Drain`]**, and this is the second half of the same idea: the pass that
    /// used to run here re-reads files and walks environments, which is 150 ms per file, and it is `&self` work. The
    /// caller takes the drain to [`Session::prepare_the_drain`] under a read lock and brings it back to
    /// [`Session::commit_the_drain`] — so the write lock is held for the inserts and for nothing else.
    pub fn commit_a_wave(&mut self, wave: PreparedWave, steps: usize) -> (Vec<Step>, Drain) {
        if !wave
            .revision
            .as_ref()
            .is_some_and(|revision| self.is_current(revision))
        {
            log::debug!("discarding an index wave prepared for an older session revision");
            return (Vec::new(), Drain::default());
        }
        let PreparedWave {
            prepared,
            units,
            refinements,
            revision: _,
        } = wave;
        for (path, made) in prepared {
            self.remember_a_prepared(path, made);
        }
        // **And the read-ahead is trimmed here too**, because this is the path the *server's* pump takes — `advance`
        // is the in-process one. Trimming only in `advance` would leave a budget that nothing on the real path
        // enforced, which is the shape of a fix that measures well and changes nothing.
        self.trim_the_read_ahead();
        // **The readings that came from the cache are applied here**, and their files leave the second pass's backlog:
        // there is nothing left to refine in them.
        for summary in refinements {
            self.parsed_since_the_last_pass
                .retain(|queued| queued != &summary.path);
            self.store.apply_a_stored_refinement(summary);
        }

        // **How long one commit may spend inserting** — the write hold this half controls.
        //
        // A count is a guess about a sum that varies with the files a wave happened to hold: measured on
        // `EmmyLuaCodeStyle`, an ordinary header's insert is **1–8 ms** and the largest (`xlocale`,
        // `windows.media.audio.h`) **13–27 ms**, so 24 files were 44.3 ms in one run and 78.4 ms in the next. Spending a
        // budget instead bounds the hold by the work that determines it. The files not popped stay at the front of the
        // queue, already prepared in `prefetched`, and the next slice commits them without parsing them again — so the
        // wave stays large, and the wave is what sets throughput (one second pass per slice).
        const COMMIT_MS: u128 = 20;
        let mut done = Vec::new();
        let committing = std::time::Instant::now();
        for _ in 0..steps {
            if !done.is_empty() && committing.elapsed().as_millis() >= COMMIT_MS {
                break;
            }
            // **A guard used to break out of this loop when the head was an open file with no prepared reading** —
            // and `prepare_wave` *defers* open files, so it never has one. The head was then never popped, nothing
            // committed, and the pump prepared the same wave for ever: measured on `test_cpp/src/containers.cpp`
            // after one diagnostic pull, **17 rounds a second, one full core and ~90 MB/s**, with `pending` stuck
            // at 66 and every session table at 66 MB. `index_one` builds the summary itself when the wave left the
            // file alone — the treatment [`Session::advance`] already gives it, at the cost of one parse inside the
            // write lock. A parse that is paid once beats a loop that is paid for ever.
            let Some((path, priority, depth)) = self.queue.pop() else {
                break;
            };
            let ready = self.prefetched.remove(&queue_key(&path));
            done.push(self.index_one(path, priority, depth, ready, true));
        }

        if self.queue.pending() == 0 {
            self.forget_the_read_ahead();
        }

        let drain = self.finish_a_slice(&done, units);

        (done, drain)
    }

    pub fn next_open_file_to_read(&self) -> Option<PathBuf> {
        self.queue
            .peek(1)
            .into_iter()
            .find(|path| self.documents.is_open(path))
    }

    pub fn next_file_to_catch_up(&self, path: &Path) -> Option<PathBuf> {
        if self.queue.is_queued(path) {
            return Some(path.to_path_buf());
        }
        self.unread_includes_of(path)
            .into_iter()
            .chain(self.unread_modules_of(path))
            .find(|file| self.queue.is_queued(file))
    }

    /// Requeue an unread include so its preparation can happen outside the session lock.
    pub fn ensure_file_to_catch_up(&mut self, path: &Path) -> Option<PathBuf> {
        self.want_refinement(path);
        if let Some(next) = self.next_file_to_catch_up(path) {
            return Some(next);
        }
        let next = self
            .unread_includes_of(path)
            .into_iter()
            .chain(self.unread_modules_of(path))
            .next()?;
        self.queue.again(next.clone(), Priority::Open, 1);
        Some(next)
    }

    pub fn gather_a_file_read(&self, path: &Path) -> Option<FileReadMaterials<SessionFiles<F>>> {
        if !self.queue.is_queued(path) {
            return None;
        }
        let revision = self.revision();
        let spelled = self
            .vfs
            .held(path)
            .filter(|file| file.open)
            .map(|file| file.path.as_path())
            .or_else(|| {
                self.project_paths
                    .get(&queue_key(path))
                    .map(PathBuf::as_path)
            })
            .or_else(|| self.vfs.held(path).map(|file| file.path.as_path()))
            .or_else(|| {
                self.store
                    .index()
                    .summary(path)
                    .map(|summary| summary.path.as_path())
            })
            .unwrap_or(path);
        let timeline = if self.documents.is_open(path) {
            self.the_timeline_to_read_through(spelled)
        } else {
            None
        };
        let input = self.store.detached_read(spelled, timeline)?;
        let view =
            self.vfs
                .held(spelled)
                .filter(|file| file.open)
                .map(|file| BufferViewMaterials {
                    file: file.clone(),
                    cuda: self.config.cuda(),
                    language: Some(self.config_for_file(&file.path).language),
                    environment: self.known_macros_of(&file.path, &file.text),
                });
        Some(FileReadMaterials {
            path: spelled.to_path_buf(),
            revision,
            input,
            view,
        })
    }

    pub fn commit_a_file_read(&mut self, file: PreparedFile) -> bool {
        if !self.is_current(&file.revision) || !self.queue.forget(&file.path) {
            log::debug!(
                "discarding an obsolete file read for {}",
                file.path.display()
            );
            return false;
        }
        let path = file.path;
        self.prefetched.remove(&queue_key(&path));
        let rendering = file.prepared.rendering();
        let step = self.index_one(path.clone(), Priority::Open, 0, Some(file.prepared), true);
        if step.with_the_environment {
            self.parsed_since_the_last_pass
                .retain(|queued| queued != &path);
        } else if matches!(
            step.outcome,
            StepOutcome::Built | StepOutcome::Unstored | StepOutcome::Reused
        ) {
            self.parsed_since_the_last_pass.push(path.clone());
        }
        if let Some((hash, rendered)) = rendering {
            let weight = rendered.weight();
            self.renderings
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert((path.clone(), hash), rendered, weight);
            self.cooking.forget(&path);
        }
        if let Some(view) = file.view
            && let Some(current) = self.vfs.held(&path)
            && view.key
                == buffer_view_key(
                    current,
                    self.config.cuda(),
                    self.known_macros_of(&current.path, &current.text),
                )
        {
            self.views
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(view.key, Arc::new(view.parsed), view.weight);
        }
        if !step.with_the_environment {
            self.want_refinement(&path);
        }
        true
    }

    /// **What has to happen once a slice's files are in the index** — the second pass, the cooks, the environments.
    ///
    /// Split out of [`Session::advance`] because there are now **two** ways a slice gets committed — `advance`, which
    /// parses inside the write lock, and [`Session::commit_a_wave`], which parses under a read lock and only commits
    /// here — and both must finish the same way. Measured: leaving this out of `commit_a_wave` fixed the latency
    /// (write lock 9–12 ms against 84–2430) and made every completion **empty**, because the cooking half lives here
    /// and a completion reads the cooked reading.
    pub(crate) fn finish_a_slice(
        &mut self,
        done: &[Step],
        units: Vec<std::sync::Arc<TranslationUnit>>,
    ) -> Drain {
        // **The second pass, at the moment the closure is in hand**. `SummaryStore::get` reads one file with
        // the evidence the index has at that moment, and for MSVC's STL that is no evidence at all: `<vector>`'s
        // `std` scope is written in `yvals_core.h`'s `_STD_BEGIN` — a file `<vector>` *includes*, and a file is read
        // before its includes. So the files this session parsed are read again, once, now that everything they
        // include is in the index. `SummaryStore::index_includes_from` does the same thing for its own walk; this is
        // that pass on the closure this caller built one step at a time.
        //
        // **Only a file that was parsed can have a reading that changed under it**: a summary that came from the
        // disk cache was stored with whatever reading the run that wrote it had, and a file whose text did not
        // change reads the same way. So the warm path — everything reused — pays nothing, and an editing drain pays
        // for the one file the edit produced.
        //
        // **That premise is now false, and the `Reused` half below is the correction.** It was true while a stored
        // summary was a reading of the file's **own text**: the text is in the key, so the same key is the same
        // reading. A summary is now a reading of the text *and of its environment* — `Widget` is in `std` because a
        // header this file includes says so — and that second half is in no key a lookup can compute today. So the
        // disk holds **unrefined** readings (a refined one is refused at write time for exactly this reason, see
        // `prepare_the_re_read`), and a reused summary is one that may still be missing its environment. It has to
        // go through the pass like a parsed one, and the pass decides: a file that mentions no bodied macro is
        // skipped there for nothing, while one that does is rendered.
        //
        // The cost is that a warm start re-reads what a cold one re-reads.
        //
        // **And not the files that were parsed *with* their environment.** This pass exists to repair a reading
        // made without one, and `Step::with_the_environment` is the mark that says the repair has already happened
        // — a file whose summary was built from the closure's macro bodies the first time has nothing a re-parse
        // could change. That is the whole of what the flag is for; see `SummaryStore::prepare_with_the_environment`
        // for why the session can hold an environment for a file at all.
        //
        // **And a file read *with* its environment is taken back off the queue, not merely kept off it.** The filter
        // below has always kept such a file from being added, but `advance` removes nothing: a file read as text cold
        // stays queued after a later edit reads it through its timeline, and under the demand-driven contract nothing
        // drains that stale entry — traced as `/p/api.h` queued twice, `outcome Built environment false` then
        // `outcome Unstored environment true`. The same statement the filter makes, applied to the queue.
        if done.iter().any(|step| step.with_the_environment) {
            let repaired: std::collections::HashSet<PathBuf> = done
                .iter()
                .filter(|step| step.with_the_environment)
                .map(|step| step.path.clone())
                .collect();
            self.parsed_since_the_last_pass
                .retain(|queued| !repaired.contains(queued));
        }
        self.parsed_since_the_last_pass.extend(
            done.iter()
                .filter(|step| {
                    matches!(
                        step.outcome,
                        StepOutcome::Built | StepOutcome::Unstored | StepOutcome::Reused
                    )
                })
                .filter(|step| !step.with_the_environment)
                .map(|step| step.path.clone()),
        );

        // **A slice of them, not all of them.** The pass is the expensive half of this function — measured from the
        // server's own budget warning, one `index slice` held the writer for **10.9 s** where the insert it is named
        // after takes 8–19 ms, and this is what was in the rest of it. A file is re-read here because its reading
        // may have changed once its includes were in the index, so the work is proportional to how many files were
        // parsed since the last pass — which after a cold start is *every file in the closure*.
        //
        // Slicing changes what the unit walk sees: it is asked about fewer files at a time, so it may build a unit
        // for one slice that a later slice would have shared. That costs a cache lookup rather than a rebuild —
        // `translation_unit_of` caches by the content of the closure — and it buys the property this function is
        // now judged by: the writer is held for one slice's worth of work and no more.
        //
        // **The slice is claimed here and re-read outside the writer.** What this function decides is *which* files,
        // which is a drain of a `Vec`; the parse, the environment walk and the encode that follow are 150 ms per file
        // measured, and they are `&self` work — see [`Session::prepare_the_drain`].
        //
        // **The timelines came from the preparing half, rooted at exactly this slice.** They used to be built
        // here, and moving them out is the change that took a **1003 ms** unit walk out of the writer — the
        // largest single thing a drain did. What made the first attempt at that measure *worse* (2636 ms against
        // 1105) was rooting them at the wave instead of at this slice: a unit is a timeline over a whole include
        // closure, so a bigger root set is a bigger timeline and the pass is proportional to it. Rooted here, the
        // walk happens once, at the size it is used at, and under the read lock.
        let files = if self.is_idle() {
            self.take_the_files_wanted_for_refinement(RE_READ_SLICE)
        } else {
            Vec::new()
        };

        // **What the cache already refined, before any timeline is built.** The second pass's answer is a function of
        // the file's own closure, so a stored one is served and its file needs no unit at all — the difference between
        // a second start that re-reads the project and one that reads a handful of files.
        let served = self.store.take_the_stored_refinements(files);
        // **And the rendering of each served file is still owed**, because a cooked record carries facts, not text:
        // `Session::view` parses a rendering, so the file goes to the cook queue rather than being dropped as done.
        // It is the ordinary cook — render and parse — with the composition the record already answered left out.
        //
        // **Measured, and the whole of a warm start's cost**: on the modules fixture the module read is
        // `index_everything()`, this queue is what it drains, and with 289 served files it cost 20.2 s, 112 s and
        // 506 s in three runs on unchanged inputs — while parsing nothing.
        //
        // **A served file is not queued here**, and that is P6's first fix. Its facts are in the index already
        // (`take_the_stored_refinements` inserts them); its *rendering* is the half a view needs, and the ask belongs
        // to the caller that needs one: [`Session::want_cooked_reading`] now queues a cook when the facts are in but
        // no rendering is, so a request for a view of a served file still gets it, while a wave that merely walked
        // the closure pays nothing. Queuing all of them here made a warm start do a render and a parse for every fact
        // it had just read from the store.
        let files = served.still_to_do;

        // **A caller that passed no timelines for a slice it is about to drain.** `advance` roots its units at the
        // files it just read, which is not the slice this pass takes — so on the drain phase (nothing left to read,
        // a backlog in hand) it passed an empty list, and `prepare_the_re_read` changed shape because of it: every
        // indexed file's text copied, every `#define` in the project asked about. See
        // [`Session::prepare_a_wave`] for the same defect on the pump's path, where it was 28 s of a 48 s start-up.
        //
        // Building them here is inside the writer on the pump's path and free of it on `advance`'s — and the pump
        // never reaches this line, because `prepare_a_wave` now always hands it the slice's timelines.
        // **No timelines for the pass** — see `prepare_a_wave`. The composition in `prepare_the_re_read` needs none.
        let _ = &units;

        // **And the files whose cooked reading is out of date**, now that their environments are complete: a file's
        // macros are the ones its includes brought in, and the index queue emptying is the first moment that is
        // true. This is the product's only caller of the cooked reading — every declaration query picks it up from
        // the index, and the files are the ones a reader has open, or has open and just edited.
        //
        // A **slice**, like the indexing steps: cooking a file is a unit walk, a parse and an index, and a session
        // that did a whole closure in one call would hold the writer for seconds while the user types.
        //
        // **The plan this builds is what the caller pays for.** Both of the expensive things a drain does after its
        // inserts are named here and worked out in [`Session::prepare_the_drain`], which takes `&self`: the second
        // pass over `files`, and the rendering of `environments`. What is left in this function is a queue pop, a
        // summary lookup and a `want`.
        // **A slice of one or two files keeps the per-file walk**, and the threshold is the cache rather than the
        // clock: a reading built through a *slice* unit has the root's environment, not the file's own, so it cannot
        // be stored as the file's refinement (`the_closure_fingerprint`). Two session tests pin that a small project
        // serves its refinement to the next session, and a shared unit would break them for a saving of one walk.
        // From `WALKS_WORTH_SHARING` files up, the walks are many and the cacheability of one small file is not the
        // trade it was.
        let slice_units = if files.len() < WALKS_WORTH_SHARING {
            Vec::new()
        } else {
            self.units_for_the_pass(&files)
        };
        let mut drain = Drain {
            files,
            // **One timeline per file of the slice, not one per candidate the pass then finds.** The units the second
            // pass reads its environments out of were left empty here, so every candidate a slice reached fell to
            // `a_timeline_of_its_own` — a whole closure walk each. Measured on the modules fixture: a slice of six
            // files reached **44 candidates** and the re-read cost **46.8 s**, which was the whole module read.
            // `units_for_the_pass` was written for exactly this and was marked dead code because nothing called it.
            units: {
                let mut all = units;
                all.extend(slice_units);
                all
            },
            environments: Vec::new(),
            revision: Some(self.revision()),
        };

        if self.is_idle() {
            // **NOT WIRED HERE, and now for a better reason than the one this note used to give.**
            //
            // A whole-program read is 5.09 s for a 159-file, 7.07 MB program (`tests/unit_read_probe.rs`), and its
            // answers were re-measured on that project rather than assumed: 159 files, 737 525 tokens, zero errors,
            // 39 080 cooked declarations added, and not one raw fact lost. The three readings the old note recorded
            // as worse do not reproduce; their cause was the unclosed scope that `index_unit_rendering`'s crossing
            // fence now quarantines.
            //
            // What keeps it out of *this* function is the writer: a drain is planned inside `commit_a_wave`, so
            // anything decided here is paid under the write lock. The read itself is
            // [`Session::read_a_program_now`], and the pump takes it as three phases — `Session::gather_a_unit_read`
            // under the read lock, [`UnitReadMaterials::read_the_program`] with **every lock dropped** (4.9 s of the
            // 5.09 s needs no session at all), and `Session::commit_a_unit_reading` under the writer for the inserts.
            // A caller that is already the writer (this function's callers `advance`) takes the three in a row.

            // **What a reader is looking at**, and nothing else: the open files and the files their own text names,
            // plus whatever a request named ([`Session::want_cooked_reading`]). See
            // [`Session::want_the_closure_cooked`] for the measurement that narrowed this down from the whole
            // closure — 11.7 s of every startup for one declaration in a thousand — and
            // [`Session::want_everything_cooked`] for the caller that still wants the whole project read that way.
            //
            // **One closure, for the document in front.** Every open file used to be a closure, and twenty files open
            // measured as **232 per-file readings inserted in a twenty-second idle window** on `test_cpp` — the same
            // headers cooked once per tab. The rule is the program schedule's, for the same reason: a request that
            // names a header asks for that header itself (`want_cooked_reading`), and a background tab keeps the
            // reading its own text gives it.
            let focused = self
                .focused_document
                .lock()
                .ok()
                .and_then(|focused| focused.clone())
                .filter(|path| self.documents.is_open(path));
            for open in focused.into_iter() {
                self.want_the_closure_cooked(&open);
            }

            // **The project is NOT swept here, and three tests are why.**
            //
            // A project opened from disk with nothing open has no file queued, so every `view` in it answers `None`
            // — a real coverage hole, and the raw reading used to *hide* it by answering (wrongly). The obvious fix
            // is to queue the rest of the project once the reader's files are done, and it was tried:
            //
            // ```text
            //   a_file_nobody_looked_at_is_not_cooked_and_one_a_request_names_is   FAILED
            //   a_header_the_user_never_opened_is_cooked_when_a_request_names_it   FAILED
            //   a_unit_read_reads_the_whole_program_once                           FAILED
            // ```
            //
            // Those three assert the deliberate opposite — that a request is what causes a cook, and that the cost
            // is paid by whoever asked. Sweeping the project at every idle drain contradicts them, and they are
            // right: it would put ten thousand cooks in front of the one file the reader is looking at.
            //
            // So the hole is closed **on the asking side** instead: [`Session::view`] queues what it could not
            // answer, and the pump cooks it. A caller gets `None` once and the reading after that, which is the
            // contract the deferral was for. See `Session::is_ready_for_a_request_about` for the caller that wants
            // to know which of the two it is holding.

            // **The cooking queue is *not* drained here**, and that is the fix for the latency the product had.
            //
            // This loop used to be `for path in self.cooking.take(COOK_SLICE) { self.cook(&path); }` — the whole of a
            // cook (unit walk, lex, render, parse, insert) inside the **write** lock. Measured from the server's own
            // log on one open file: the last index slice held the writer for **749 ms**, and after it five seconds
            // passed with no slice logged at all, which is this loop. A completion arriving anywhere in that window
            // waited for all of it — the user's report was *"I type `s` and the completion takes forever"*, and the
            // numbers agree: 1623 / 1049 / 2552 / 685 ms for the first four, 5 ms once the index had settled.
            //
            // The work is not gone, it moved: [`Session::take_cooking_materials`] gathers what a cook needs (a
            // summary lookup and a unit out of the cache, cheap), [`Session::render_them`] does the lex, the render
            // and the parse **under the read lock the pump takes**, and [`Session::commit_them`] inserts under the
            // write one. `finish_a_slice` is called from inside `commit_a_wave`, so anything it drains here is by
            // construction inside the writer.
            //
            // What stays is the queueing: a file a reader has open, and the includes it names, are what has to be
            // cooked first, and that order is what `want_the_closure_cooked` writes into the queue.

            // **…and what a query asked for on its own, which is a *cook* rather than an environment.**
            //
            // `macro_work` is the asking side's queue: a request holds `&self` and cannot touch `cooking` (which
            // needs `&mut self`), so it asks through that lock — exactly as the note on the field says. The pump is
            // the `&mut self` half, and what it owes those requests is a **cooked reading**, because that is what
            // `Session::view` could not answer from: it queues here precisely when `known_rendering_of` missed.
            //
            // Measured before this: a request for a file in a project nobody had opened queued **only** here, this
            // loop handed it to `drain.environments`, and `cooking` stayed empty — so `claim_cooking` returned
            // nothing, no rendering was ever built, and every later request got `None` again. The trace that named
            // it: `cooking: stopping with read 0, cooked 0, backlog 0 | cache []`, with the file's path printed by
            // `view` as the thing it had just asked for.
            //
            // Moved rather than copied: `cooking.want` is idempotent (`Cooking::want` returns false on a repeat), and
            // draining the asking queue is what stops the same file being moved on every pass for ever.
            while let Some(path) = self
                .macro_work
                .lock()
                .ok()
                .and_then(|mut work| work.take(1).into_iter().next())
            {
                self.cooking.want(&path);
            }

            // **…and the macro environments a query asked for**, once each per pass.
            //
            // One per drain rather than four, and the difference from `COOK_SLICE` is measured rather than chosen:
            // cooking a file is a render and a parse of **that file**, while building an environment walks the
            // file's whole closure and copies every file's text — 103.7 ms against a cook's fraction of that on the
            // same headers. This is the budget that keeps a pass responsive while still getting there: the query
            // that asked read the plain view, and the next one about the same text finds the environment.
            //
            // **Named here, rendered outside the writer.** [`Session::rendering_of`] takes `&self` — the lookup, the
            // cached unit, the lex and the cook all read and change nothing — so the 65–190 ms it costs per header is
            // paid by [`Session::prepare_the_drain`], and what is left inside the writer is the `Arc` insert.
            for _ in 0..MACRO_SLICE {
                let Some(path) = self
                    .macro_work
                    .lock()
                    .ok()
                    .and_then(|mut work| work.take(1).into_iter().next())
                else {
                    break;
                };
                drain.environments.push(path);
            }
        }

        drain
    }

    /// **Read one whole program now, if one is owed and allowed** — the inline form of the pump's three phases.
    ///
    /// A program read is three phases around a lock ([`Session::gather_a_unit_read`], the read itself, and
    /// [`Session::commit_a_unit_reading`]), because the 4.9 s in the middle takes no session at all. A caller that
    /// *is* the writer — [`Session::advance`], a test, a batch — has no lock to keep free, so it takes them in a row
    /// and this is that shape. The pump takes them apart and drops the lock between.
    pub fn read_a_program_now(&mut self) -> Option<UnitReading> {
        let root = self.the_program_to_read()?;
        let materials = self.gather_a_unit_read(&root);
        let prepared = materials.and_then(UnitReadMaterials::read_the_program);
        let records = match &prepared {
            Some(prepared) => self.prepare_the_unit_records(prepared),
            None => Vec::new(),
        };
        self.commit_a_program(&root, prepared, records)
    }

    /// **File what a program read produced** — the third phase, and the one place `units_read` is written.
    ///
    /// `prepared` is `None` when the read declined (a root the walk cannot reach, a render that failed) and the root is
    /// **still marked read**: a root that cannot be walked would otherwise be chosen again on the next wake, and a
    /// schedule that keeps choosing the same unbuildable root is one that never goes idle.
    pub fn commit_a_program(
        &mut self,
        root: &Path,
        prepared: Option<PreparedUnitReading>,
        records: Vec<(cache::cache_db::CacheKey, Vec<u8>)>,
    ) -> Option<UnitReading> {
        let Some(prepared) = prepared else {
            // **A declined read is retried — three times.** The walk can be refused for a reason that heals: a
            // dependency the index does not have *yet* (the schedule fires when the queue is empty, but a file the
            // session cannot read at all leaves a hole that stays). Marking it read on the first decline, which this
            // did, made a temporarily-incomplete closure permanent: measured on a real project, three of a project's
            // own roots were never read because one attempt declined (`tests/project_read_probe.rs`).
            //
            // The bound is what keeps a genuinely unbuildable root from being chosen on every wake: three attempts,
            // and the file keeps the reading the per-file path gave it. The number is small because each attempt is a
            // whole-program read's *gather* — a walk — and a fourth would not learn anything the third did not.
            let key = queue_key(root);
            let attempts = self.units_attempted.entry(key).or_default();
            *attempts += 1;
            if *attempts >= PROGRAM_READ_ATTEMPTS {
                self.units_read.insert(queue_key(root));
            }
            return None;
        };
        if queue_key(root) != queue_key(&prepared.root) || !self.is_current(&prepared.revision) {
            log::debug!("discarding a program prepared for an older session revision");
            return None;
        }
        let committed = self.commit_a_unit_reading(prepared, records)?;
        // **Nothing is charged for it.** This used to add the read's priced cost to `program_read_bytes`, the session's
        // budget for whole-program reads — and the number that budget existed for (1 094 MB in 34 s on `test_cpp` with
        // twenty files open) came from a counter that only ever added, so it was churn. The budget and the pricing are
        // both gone; see the note where the constants were in `session/mod.rs`.
        self.units_read.insert(queue_key(root));
        Some(committed)
    }

    /// **Work out the expensive half of a drain, holding nothing but `&self`.**
    ///
    /// The counterpart of [`Session::prepare_a_wave`], for the work that happens *after* a slice's inserts: the second
    /// pass ([`SummaryStore::prepare_the_re_read`]) and the macro environments a query asked for. Both are pure
    /// functions of the session as it stands — the re-read re-parses files it already has the text of, and a rendering
    /// is built from the index and the unit cache — and both were inside the write lock only because the code that
    /// *decided* to do them was. Measured from the server's own budget warning: a slice that committed **no files at
    /// all** held it for 260–572 ms, and this is the whole of what it was doing.
    ///
    /// [`Session::commit_the_drain`] applies the answer, and the pair is what lets the pump give a request the lock
    /// back between the two.
    /// **Apply what a bounded part of the drain planned** — see [`Session::prepare_the_drain`].
    ///
    /// `at_most_files` and `at_most_environments` are the bound on **one read hold**: the rest comes back in the
    /// retry lists and [`Session::commit_the_drain`] puts it back on the queues, so a drain that needs a second of
    /// work is several holds of fifty milliseconds rather than one hold of a second.
    pub fn prepare_the_drain(
        &self,
        drain: Drain,
        at_most_files: usize,
        at_most_environments: usize,
    ) -> PreparedDrain {
        let Drain {
            mut files,
            units,
            mut environments,
            revision,
        } = drain;
        let left_files = if files.len() > at_most_files {
            files.split_off(at_most_files)
        } else {
            Vec::new()
        };
        let left_environments = if environments.len() > at_most_environments {
            environments.split_off(at_most_environments)
        } else {
            Vec::new()
        };
        // **Everything this drain is about**, before the bound takes its part: a commit that finds the revision moved
        // owes the whole drain, not the leftovers.
        let mut owed_files = files.clone();
        owed_files.extend(left_files.iter().cloned());
        let mut owed_environments = environments.clone();
        owed_environments.extend(left_environments.iter().cloned());
        if !revision
            .as_ref()
            .is_some_and(|revision| self.is_current(revision))
        {
            // **Nothing was applied, so every file is a retry** — the leftovers and the ones this call would have
            // read, in one list.
            files.extend(left_files);
            environments.extend(left_environments);
            return PreparedDrain {
                revision,
                retry_files: files,
                retry_environments: environments,
                owed_files,
                owed_environments,
                ..PreparedDrain::default()
            };
        }
        // **The two halves are timed apart**, because a start-up regression of **90 %** was found inside this
        // function on a warm cache: 1761 summaries reused from disk in 4 s of real work, and 60 s in twelve calls of
        // this. `prepare_the_re_read` and one environment look the same from outside and have nothing to do with each
        // other — one re-parses four files with their closure's macros in hand, the other renders a file's whole
        // closure — so a single number for the drain names neither.
        let re_read =
            // **`true`: this caller can walk a timeline**, so the pass renders a file no slice unit covers instead of
            // keeping the raw reading of it. See `SummaryStore::prepare_the_re_read`'s `walk_its_own`.
            (!files.is_empty()).then(|| self.store.prepare_the_re_read(&files, &units, false, true));

        // **Remembered under the text it was read from**, which is what the next `view` looks up. The text is taken
        // from the VFS rather than from the rendering: the key is the *file's* content, because that is what a
        // client's buffer changes — and it is read here as well as rendered here, because both are the same read
        // lock and neither changes the session.
        //
        // **In parallel, for the same reason the program read's records are.** Each environment walks its file's whole
        // closure and copies every text in it — 103.7 ms on MSVC's headers — and a query that names several hundred
        // files of a closure costs tens of seconds of one core. Measured on neovim's `ui.c`, where a cold start's
        // drain was **25.0 s** of 57 s while `prepare_the_re_read` accounted for 0.9 s of it: the rest is this loop.
        // The chunks keep the caller's order, and both halves run with the same read lock.
        let prepare_environment = |path: PathBuf| {
            let reading = self.rendering_of(&path)?;
            let text = self.text(&path)?;
            // **And the macro environment for the file's own view** - the other half of what a query asked for here.
            // ``Session::view_of_the_file`` reads the text the reader is typing with the macros **in force** for it,
            // and the environment it reads is the one this builds: without this call the map was never written, so
            // every such view was parsed with an *empty* environment and a macro from a header was a name it could
            // not expand. Outside the writer, like the rendering below, and for the same measured reason: building an
            // environment walks the file's whole closure and copies every file's text (103.7 ms on MSVC's headers).
            let _ = self.the_macros_of(&path, &text);

            Some((
                (path, cache::content_hash(&text)),
                std::sync::Arc::new(reading),
            ))
        };
        let cores = std::thread::available_parallelism()
            .map(|cores| cores.get())
            .unwrap_or(4)
            .min(environments.len().max(1));
        let mut rendered = Vec::new();
        if cores <= 1 || environments.len() < 8 {
            rendered.extend(environments.into_iter().filter_map(prepare_environment));
        } else {
            let chunk = environments.len().div_ceil(cores);
            std::thread::scope(|scope| {
                let mut handles = Vec::with_capacity(cores);
                for piece in environments.chunks(chunk) {
                    handles.push(scope.spawn(move || {
                        piece
                            .iter()
                            .filter_map(|path| prepare_environment(path.clone()))
                            .collect::<Vec<_>>()
                    }));
                }
                for handle in handles {
                    rendered.extend(handle.join().unwrap_or_default());
                }
            });
        }

        PreparedDrain {
            re_read,
            rendered,
            revision,
            retry_files: left_files,
            retry_environments: left_environments,
            owed_files,
            owed_environments,
        }
    }

    /// **Put a drain's unread part back on the queues** — what a bounded drain leaves, and what a drain prepared for
    /// an older revision returns whole.
    ///
    /// A file is put back only if it still exists: the point of the retry list is work that has not been done, and a
    /// file that was deleted between the plan and the commit is work that cannot be done.
    fn put_the_drain_back(&mut self, files: Vec<PathBuf>, environments: Vec<PathBuf>) {
        if files.is_empty() && environments.is_empty() {
            return;
        }
        let mut wanted = self
            .refined_on_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for path in files {
            if self.files.exists(&path) {
                wanted.insert(path);
            }
        }
        drop(wanted);
        let mut work = self
            .macro_work
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for path in environments {
            if self.files.exists(&path) {
                work.want(&path);
            }
        }
    }

    /// **Apply what [`Session::prepare_the_drain`] worked out** — the index inserts, and the remembered renderings.
    pub fn commit_the_drain(&mut self, prepared: PreparedDrain) -> usize {
        if !prepared
            .revision
            .as_ref()
            .is_some_and(|revision| self.is_current(revision))
        {
            if prepared
                .revision
                .as_ref()
                .is_some_and(|revision| Arc::ptr_eq(&self.identity, &revision.session))
            {
                self.put_the_drain_back(prepared.owed_files, prepared.owed_environments);
            }
            log::debug!("discarding a drain prepared for an older session revision");
            return 0;
        }
        // **The part a bounded drain did not read.** Not a stale drain: the part this call was not allowed to do,
        // which the next one takes, so that no single read hold is the whole drain.
        let retry_files = prepared.retry_files.clone();
        let retry_environments = prepared.retry_environments.clone();
        for (key, rendered) in prepared.rendered {
            // **The key is normalized here too**, because this is the insert a cook's own rendering takes and the
            // lookup normalizes: a drain prepared for a canonicalized spelling would otherwise be found by
            // nobody. See `a_rendering_key`.
            let key = a_rendering_key(&key.0, key.1);
            if let Ok(mut known) = self.renderings.lock() {
                let weight = rendered.weight();
                known.insert(key, rendered, weight);
            }
        }

        let Some(plan) = prepared.re_read else {
            return 0;
        };

        // **A refinement that could not be taken is retried, not dropped.** So while there is still
        // work in the queue the file goes back on it, and once the index is complete it is left
        // alone: a dependency nothing will ever index is a fact about the project, not about
        // timing, and the file keeps the reading the first pass gave it.
        let waiting: Vec<PathBuf> = plan.waiting_for_the_index().to_vec();
        // **Every rebuilt reading, not only the storable ones.** What this set says is "the index holds the refined
        // reading of this file now", and a reading rebuilt through a sibling file's timeline is exactly as refined for
        // this session — that it cannot be written down is the cache's question, not this one.
        let refined: Vec<PathBuf> = plan
            .rebuilt
            .iter()
            .map(|(summary, _)| summary.path.clone())
            .collect();
        let re_read = self.store.commit_the_re_read(plan);
        if let Ok(mut known) = self.refined_readings.lock() {
            known.extend(refined.iter().map(|path| queue_key(path)));
        }
        if !waiting.is_empty() && self.pending_work() > 0 {
            for path in waiting {
                self.queue.again(path, Priority::Rest, 0);
            }
        }
        self.put_the_drain_back(retry_files, retry_environments);

        re_read
    }
}
