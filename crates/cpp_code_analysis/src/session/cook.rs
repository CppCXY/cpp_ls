//! **Cooking** — rendering a file's closure the way a compiler reads it, and filing the cooked reading.
//!
//! Moved here from `session/mod.rs` without a change.

use super::*;

/// **One file a cooking slice claimed**, and where its reading will come from.
///
/// A cook used to have one shape — gather, render, parse — and every file paid for it. The disk can hold the answer
/// under the file's own key ([`SummaryStore::the_stored_cooked_reading_with_rendering`]), and then there is nothing to
/// render: the two arms are kept apart so the commit can say which one it is filing.
pub enum AClaimedCook {
    /// The disk already holds this file's reading **and its rendering**, validated against its content, configuration
    /// and closure.
    Stored(
        PathBuf,
        crate::index::CookedFile,
        crate::preprocess::cooked::RenderedCooked,
    ),
    /// This slice must render and parse it.
    Fresh(CookingMaterials),
}

/// **What a cooking slice produced** — the two kinds of reading, kept apart because they commit differently.
#[derive(Default)]
pub struct AClaimedSlice {
    /// Readings the disk held; committing one caches its rendering and inserts its facts.
    pub stored: Vec<(
        PathBuf,
        crate::index::CookedFile,
        crate::preprocess::cooked::RenderedCooked,
    )>,
    /// Readings this slice rendered and parsed; committing one checks the text and files it.
    pub fresh: Vec<RenderedCook>,
}

impl AClaimedCook {
    /// The file this claim is for.
    pub fn path(&self) -> &Path {
        match self {
            AClaimedCook::Stored(path, _, _) => path,
            AClaimedCook::Fresh(materials) => &materials.path,
        }
    }
}

impl AClaimedSlice {
    pub fn len(&self) -> usize {
        self.stored.len() + self.fresh.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every file the slice produced a reading for, in the order the claims arrived.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.stored.iter().map(|(path, _, _)| path.as_path()).chain(
            self.fresh
                .iter()
                .map(|rendered| rendered.indexed.summary.path.as_path()),
        )
    }
}

impl<F: FileProvider + Clone> Session<F> {
    /// Read one file the way a compiler reads it**, and let the index answer for what came out.    ///
    /// One call that is the whole foundation, in the order the layers were built: walk the file's own translation
    /// unit ([`crate::TranslationUnit::walk`], over the summaries the index already holds and the text this session
    /// is holding), read the unit's definitions once, cook the file against **its** environment
    /// ([`crate::FileMacros`]) and the compilation's own definitions ([`MacroTable::from_marked`]), render, index
    /// the rendering and map every range back into the file ([`crate::FileIndexer::index_rendering`]), and hand the
    /// declarations to the index ([`crate::ProjectIndex::insert_cooked`]).
    ///
    /// # Why it is worth doing, and when
    ///
    /// The raw reading — the file's own text — cannot see what a macro declares, and it cannot see the *scope* a
    /// namespace-opening macro puts a declaration in: MSVC's `_STD_BEGIN` is `namespace std {` in `yvals_core.h`,
    /// so every declaration in `<string>` is `std::`-qualified to a compiler and at file scope to a reader. Measured
    /// on the 255-file SDK corpus, 3 250 declaration names exist **only** after expansion, and the index answers for
    /// 4 of 40 sampled ones before this call and 40 of 40 after.
    ///
    /// It is **not free** (a unit walk plus a parse of the rendering per file), so the session does it for the files
    /// a reader is actually looking at, at the moment its queue drains: [`Session::cook_the_open_files`] is what the
    /// indexing loop calls, and this is what a caller that wants one file cooked says.
    ///
    /// `None` when the file has no summary or no text — nothing has read it yet, which is a state and not an error.
    pub fn cook(&mut self, path: impl AsRef<Path>) -> Option<CookedReading> {
        let started = std::time::Instant::now();
        // **The disk before the render.** A record with its rendering — the file's content, its configuration and its
        // closure, all validated by the reader — is the reading this call would produce, and the rendering is what
        // makes filing it different from filing facts: `view` answers from it immediately.
        let resolved = self.store.index().summary(path.as_ref())?.path.clone();
        if let Some((cooked, rendered)) = self
            .store
            .the_stored_cooked_reading_with_rendering(&resolved)
        {
            let committed = self.commit_a_stored_cook(resolved, cooked, rendered);
            if crate::probes::cooking() {
                log::info!(
                    "cook: {} served from the disk in {:?}",
                    path.as_ref().display(),
                    started.elapsed()
                );
            }
            return committed;
        }
        let materials = self.cooking_materials(path.as_ref())?;
        let gathered = started.elapsed().as_millis();
        let render_started = std::time::Instant::now();
        let rendered = self.render_a_cooked(&materials)?;
        let rendered_for = render_started.elapsed().as_millis();
        let committed = self.commit_a_cooked(rendered);
        if crate::probes::cooking() {
            log::info!(
                "cook: {} gathered {} ms | rendered {} ms | committed {} ms | total {} ms | {}",
                path.as_ref().display(),
                gathered,
                rendered_for,
                started
                    .elapsed()
                    .as_millis()
                    .saturating_sub(gathered + rendered_for),
                started.elapsed().as_millis(),
                if committed.is_some() {
                    "filed"
                } else {
                    "dropped"
                }
            );
        }
        committed
    }

    /// **Everything a cook needs that is cheap to gather** — and it takes `&self`, which is the whole point of it.
    ///
    /// The expensive half of cooking a file — lexing it, rendering it, parsing the rendering — is a function of the
    /// file's own text and its environment, and it changes nothing. So it is [`Session::render_a_cooked`], which takes
    /// `&self`, and this is the short part that has to happen first: resolve the path, ask the index for the summary,
    /// take the translation unit **out of the cache** (a hit whenever the file has been indexed) and read the text.
    ///
    /// Split for the same reason [`Session::prepare_a_wave`] is: the write lock is what a query waits for. Measured,
    /// a cooking slice of *zero* index files held it for up to **1527 ms**, and the parts measured here are
    /// `unit 15–42 ms` (the cache lookup) against `own 9–269 ms` (the render and parse).
    ///
    /// **And `&self` rather than `&mut self`, which it was until the measurement said so.** Every step of it reads:
    /// the summary lookup, `context_hash`, the unit (behind its own lock, deliberately — see `Session::units`), the
    /// text, the seed. What needed `&mut self` was the *queue pop*, and that is [`Session::claim_cooking`] — so a
    /// slice is claimed under the write lock, which is a `Vec` operation, and gathered here under the read one.
    /// Measured from the server's own budget warning, the gather was **58–76 ms** of every drain's write hold and the
    /// pop was nothing.
    pub fn cooking_materials(&self, path: &Path) -> Option<CookingMaterials> {
        let path = self.store.index().summary(path)?.path.clone();
        let key = SummaryKey::new(0, self.store.context_hash(&path));

        // What the file's own text already declares — the names the cooked reading adds are the ones missing here.
        let already_declared: std::collections::HashSet<String> = self
            .store
            .index()
            .summary(&path)
            .into_iter()
            .flat_map(|summary| summary.declarations.iter())
            .map(crate::DeclFact::qualified_name)
            .collect();

        // **Not from a provisional timeline.** A rendering made from an environment missing whatever the queue has
        // not read yet is a rendering with the macro left unexpanded — and unlike the timeline, the rendering
        // *sticks*: it is filed as the file's cooked reading, and nothing re-cooks a file that has one. Leaving the
        // file uncooked keeps it wanted (`want_the_closure_cooked` asks for every open file without a reading), so
        // a later drain renders it when the timeline is the whole program. See [`HeldUnit`].
        // **A header is not the root of its own translation unit**, and cooking one as if it were is a *wrong* answer
        // rather than a smaller one.
        //
        // `wincon.h` writes `WINBASEAPI WINBOOL WINAPI SetConsoleOutputCP(UINT wCodePageID);` and includes neither
        // `windef.h` nor `windows.h` — those arrive from the **includer**, and `windows.h` includes them before it.
        // So a timeline rooted at `wincon.h` itself has no `WINAPI` and no `WINBASEAPI` in force, the cook leaves the
        // specifiers spelled out, and [`crate::sema::types::read_specifiers`] takes the **last** name of the sequence
        // (the rule that fixes `_NODISCARD _CONSTEXPR20 reference back()`) — which here is `WINAPI`. Measured: the
        // hover on `SetConsoleOutputCP` said "a function returning `WINAPI`", the declaration says `WINBOOL`, and the
        // *wrong* one won, because a cooked reading outranks the file's own summary.
        //
        // Two rules, in this order:
        //
        // 1. **A unit that already contains the file is the environment to use.** The project's translation unit is
        //    normally held (it was walked when its file was read), and its timeline is exactly the state at the
        //    header's inclusion — no walk, no guess.
        // 2. **Otherwise a file that can be the root of its own program is walked as one** — a `.cpp` with a compile
        //    command, a file nothing includes, a buffer the reader has open. Its timeline is the whole closure.
        // 3. **Otherwise the environment is composed from the index** — `the_indexed_environment`, the same reading the
        //    second pass makes and the same one the *summary* of this file is rebuilt from. This arm is new, and it is
        //    what a header gets: it used to `return None`, because the alternative was an **empty** environment and a
        //    rendering made from one is a wrong answer. A composed environment is not empty, so the reason that rule
        //    existed is gone and the rule with it.
        //
        // **The measurement that made rule 3 exist.** After phase 2 stopped building units for the pass, rule 2 was
        // reached for every header, rule 3 was `return None`, and `render_a_cooked`'s `unit.environment_of(path)?`
        // answered `None`: `include/techniques.h` came out `NOT COOKED` and the C++ corpus went **51/52 → 41/52** type
        // answers, 10/10 → 9/10 member answers. A capability that depends on a *cache* holding some other file's
        // timeline is not a capability; the environment has to be a function of the index.
        //
        // **Rules 1 and 2 come first, and that is measured too.** Composing *everything* was tried first and left the
        // corpus at 41/52: a timeline sees the guarded `#define`s a closure's facts do not (`FactGuard::Special`), so a
        // composed environment is weaker exactly where a standard library puts its specifiers. Composition is the
        // fallback that makes a header cookable at all, not a replacement for the walk.
        // **Which unit answers for a header, and the index is what says so.** A file that is the root of its own
        // program is read through its own walk (the arm below), so the question is only about a file some *other*
        // walk reached — and every file that includes it is one such walk. They are not interchangeable: neovim's
        // `grid.h` reads clean on its own, and reads `__declspec(dllexport) ScreenGrid …` when `main.c`'s unit
        // answers for it, because `main.c:3` defines `EXTERN` **for its own translation unit** before including
        // anything.
        //
        // The choice used to be "the smallest root among the units that happen to be held", which is a function of
        // **what the session had cooked**, not of the code: the census measured `grid.h` at 2 parse errors in the
        // 517-file session and 0 alone, and two runs of one binary disagreed about which files erred. Now the
        // index names the unit ([`Session::the_unit_that_reads`]) and a unit it does not name never answers: if the
        // root the index names is in memory its timeline is used, and otherwise the environment is composed from
        // the index (the same reading the second pass builds a summary from) — a weaker rendering, and the honest
        // one, rather than a stronger rendering of a program this header is not part of.
        // **The file whose unit answers for this one**, from the include graph — computed before the search because
        // the walk below needs it too: a *header* is read through the unit that includes it, and a unit rooted at the
        // header is a different program (its _HAS_CXX20 is false, its body is the #else branch).
        let designated_root = self.the_unit_that_reads(&path);
        let held = {
            let designated = designated_root.as_deref().map(queue_key);
            let held_units = self.held_units();
            match designated {
                Some(designated) => held_units
                    .values()
                    .filter(|held| {
                        !held.provisional
                            && held.unit.environment_of(&path).is_some()
                            && held
                                .unit
                                .files()
                                .next()
                                .is_some_and(|root| queue_key(root) == designated)
                    })
                    .map(|held| std::sync::Arc::clone(&held.unit))
                    .next(),
                // **The index names no unit** — a file nothing in the project includes, or an index still being
                // built. The smallest held root is the deterministic fallback, and it is the old rule.
                None => held_units
                    .values()
                    .filter(|held| !held.provisional && held.unit.environment_of(&path).is_some())
                    .min_by(|left, right| left.unit.files().next().cmp(&right.unit.files().next()))
                    .map(|held| std::sync::Arc::clone(&held.unit)),
            }
        };
        // The definition cache is the run's, not this file's: a definition's text does not depend on which file asked
        // for it, and without it every cook re-lexes and re-parses the whole closure's bodies (`configuration_from_
        // environment_and`'s own note measures 12.2 s of a 40 s census on exactly that).
        let mut parsed = self
            .parsed_definitions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut composed = || {
            self.store
                .the_indexed_environment(&path)
                .map(|environment| {
                    crate::preprocess::cooked::configuration_from_environment_and(
                        &environment,
                        true,
                        &mut parsed,
                    )
                })
        };
        // includes it renders 217 875. Nothing is walked when the designated root's timeline is already in hand
        // (held), which is the case the fast path above exists for.
        // **S1: a cook composes, and never walks.** The environment a file needs is a function of its text and the
        // macros in force where it is included, so the composed arm is the answer at file scale; walking the whole
        // closure was measured at 2.8-8.8 s and +174-355 MB for each open file (`docs/module-cache.md`).
        // **A timeline answers positionally and stays preferred**; the composed configuration is the fallback for a
        // file no walk covers. Converting both to a `Configuration` was tried and is *not* equivalent: the two
        // `semantic.rs` tests about a directive-split return type failed with it, because a materialised table is the
        // state a file starts with rather than the state at each line.
        let (unit, configuration) = match held {
            Some(unit) => (Some(unit), None),
            // **The index must be current before a composition answers.** A header that was just edited has its
            // summary dropped and is queued; its *text* is what the including file actually sees, and a composed
            // environment would read the stale summary — or no summary — and answer a namespace that is no longer
            // there. Measured: `editing_a_header_stales_the_cooked_reading_of_a_file_that_includes_it` answered
            // `NotDeclaredHere("two::Widget")` after an edit, where the walk (self-contained) answered the reading.
            //
            // So the window in which the closure is not ready is a window for the walk, and only that window: the
            // walk is what the pump's walk budget must not remove, and a *ready* closure never pays for it.
            // **The walk is for the document the reader has open, not for every file a drain cooks.** A drain with
            // a cold index cooks the focused closure — hundreds of headers — and an unready closure walked for each of
            // them is the cook itself doing wholesale walks: measured as one `advance(8)` with 82 documents open taking
            // longer than ten minutes. An open document is the case the arm exists for (a header the reader is
            // typing in, whose summary the edit dropped); a background header composes, as it did before.
            None if self.documents.is_open(&path) && !self.is_closure_ready(&path) => {
                match self.unit_and_state_of(&path).map(|(unit, _)| unit) {
                    Some(unit) => (Some(unit), None),
                    None => (None, Some(composed()?)),
                }
            }
            None => (None, Some(composed()?)),
        };
        drop(parsed);

        let text = self.files.read(&path)?;
        let text_hash = crate::cache::content_hash(&text);
        let seed = MacroTable::from_marked(self.store.index().macros());

        Some(CookingMaterials {
            path,
            key,
            unit,
            configuration,
            text,
            text_hash,
            seed,
            already_declared,
            revision: self.revision(),
        })
    }

    /// Gather a cook that can be rendered without holding the session lock.
    pub fn gather_a_cook(&self, path: &Path) -> Option<DetachedCook<F>> {
        Some(DetachedCook {
            files: self.files.clone(),
            config: self.config.clone(),
            materials: self.cooking_materials(path)?,
        })
    }

    /// **Lex, render and parse one file's text** — the expensive half of cooking, and it takes `&self`.
    ///
    /// Everything it reads is owned by its argument, so a caller may hold it under a **read** lock while a query runs
    /// beside it. [`Session::commit_a_cooked`] is the other half: one insert, `&mut self`.
    ///
    /// The starting macro state and the seed are moved in rather than borrowed because the caller has to let go of the
    /// session between the two halves — that is the whole point of the split — and a borrow of `&self` cannot survive it.
    pub fn render_a_cooked(&self, materials: &CookingMaterials) -> Option<RenderedCook> {
        if !self.is_current(&materials.revision) {
            return None;
        }
        Self::render_cook(&self.files, &self.config, materials)
    }

    pub(crate) fn render_cook(
        files: &SessionFiles<F>,
        config: &CompilerConfig,
        materials: &CookingMaterials,
    ) -> Option<RenderedCook> {
        let CookingMaterials {
            path,
            key,
            unit,
            configuration,
            text,
            text_hash,
            seed,
            already_declared,
            revision,
        } = materials;

        let (tokens, _) = { cpp_parser::lex(text, &cpp_parser::LexerConfig::default()) };
        // **Two ways to start a rendering, and the walk's is the accurate one.**
        //
        // A `MacroView` is a *position* in a walk, so it answers a `#define` that arrives mid-file and a directive that
        // splits a declaration — which is what a compiler sees, and what every reading this session built before this
        // change used. It requires a **held translation unit**, and after phase 2 stopped building units for the pass no
        // header had one: `include/techniques.h` came out `NOT COOKED` and the C++ corpus went 51/52 → 41/52 type
        // answers. A capability that depends on a *cache* holding another file's timeline is not a capability, so when
        // no walk covers the file its environment is **composed from the index**
        // (`configuration_from_environment_and`, over the run's shared `ParsedDefinitions`) and rendered as
        // `Over(configuration, seed)`.
        //
        // **The fallback is not a substitute**, and the difference is measured: converting *both* to a `Configuration`
        // failed the two `tests/semantic.rs` tests about a directive-split return type, because a materialised table is
        // the state a file *starts* with rather than the state at each line.
        let rendered = match (unit, configuration) {
            (Some(unit), _) => {
                let definitions = unit.definitions();
                let view = unit.environment_of(path)?;
                let macros = crate::preprocess::cooked::FileMacros::new(
                    &view,
                    &definitions,
                    Some(seed),
                    true,
                );
                crate::preprocess::cooked::cook_with(text, &tokens, &macros).render()
            }
            (None, Some(configuration)) => {
                let start = crate::preprocess::cooked::Over::new(&configuration.table, seed);
                crate::preprocess::cooked::cook_with(text, &tokens, &start).render()
            }
            (None, None) => return None,
        };
        let indexer = FileIndexer::new(files, config);
        if crate::probes::cooking() {
            eprintln!(
                "render_cook: {} text {} tokens {} rendered {} ({})",
                path.display(),
                text.len(),
                tokens.len(),
                rendered.text.len(),
                if unit.is_some() { "unit" } else { "composed" }
            );
        }
        let indexed = indexer.index_rendering(path, &rendered, *key);
        let reading = CookedReading {
            declarations: indexed.summary.declarations.len(),
            only_after_expansion: indexed
                .summary
                .declarations
                .iter()
                .filter(|fact| !already_declared.contains(&fact.qualified_name()))
                .count(),
            diagnostics: indexed.diagnostics.len(),
            unplaced: indexed.unplaced,
            mapped: indexed.mapped,
        };
        Some(RenderedCook {
            text_hash: *text_hash,
            indexed,
            reading,
            revision: revision.clone(),
        })
    }

    /// **Put a rendered reading into the index** — one insert, and it is the whole of what needs `&mut self`.
    ///
    /// # The one check, and the defect it answers
    ///
    /// The rendering was made from the text the session held when the cook was **claimed**, and the file can be
    /// edited between the claim and this call. Committing then would file a reading of text nobody has, *under the
    /// current text* — and nothing re-cooks a file that has a cooked reading, so the wrong answer is permanent until
    /// the next edit. Measured through the protocol: a `didChange` deleting a `;`, then one pull —
    /// `reading Cooked, 0 error(s)` for a file that does not parse. So a reading whose text is no longer the file's
    /// is **dropped**, and the file is asked for again: the edit that made it stale already queued a cook (see
    /// [`Session::buffer_changed`]), and asking again here costs a hash lookup.
    pub fn commit_a_cooked(&mut self, rendered: RenderedCook) -> Option<CookedReading> {
        let RenderedCook {
            text_hash,
            indexed,
            reading,
            revision,
        } = rendered;
        let path = indexed.summary.path.clone();
        let started = std::time::Instant::now();

        // **The check.** `self.text` is the file's text *now*, and the hash it is compared against is the one the
        // rendering was made from. A file that has become unreadable is the same answer — nothing to file.
        let current = self
            .text(&path)
            .map(|text| crate::cache::content_hash(&text));
        let text_hash_ms = started.elapsed().as_millis();
        if !self.is_current(&revision) || current != Some(text_hash) {
            if crate::probes::cooking() {
                eprintln!(
                    "commit_a_cooked: dropped {} | the text changed while it was being rendered",
                    path.display()
                );
            }
            if Arc::ptr_eq(&self.identity, &revision.session) && self.files.exists(&path) {
                self.want_cooked_reading(&path);
            }
            return None;
        }

        // **The rendering is remembered, not dropped.** This is the half that was missing, and its absence is why
        // `Session::view` could answer `None` for a file whose rendering had *just* been built and parsed: the facts
        // went into the index and the tree went in the bin, so nothing could reconstruct the reading until some
        // other path happened to remember one. With this, "the file has been read the way a compiler reads it" and
        // "the reading is available to a query" are the same state — which is what the rest of this layer assumes.
        //
        // Keyed by the hash of the text it was **made from** — the same one the check above just compared — rather
        // than by a second read of the file, so the entry cannot describe one text under another's key.
        if let Ok(mut known) = self.renderings.lock() {
            let key = a_rendering_key(&path, text_hash);
            if crate::probes::view() {
                eprintln!("commit: path {:?} hash {:?}", path, text_hash);
            }
            let before = known.len();
            let weight = indexed.rendered.weight();
            known.insert(key, std::sync::Arc::clone(&indexed.rendered), weight);
            if crate::probes::cooking() {
                eprintln!(
                    "commit_a_cooked: inserted {} | cache {} -> {}",
                    path.display(),
                    before,
                    known.len()
                );
            }
        } else if crate::probes::cooking() {
            // **The other way this can silently not happen**: the cache lock is poisoned, which leaves it exactly as
            // it was while the facts go into the index — "read" and "available" apart again, which is the state this
            // whole change is about.
            eprintln!(
                "commit_a_cooked: NOT inserted {} | lock {}",
                path.display(),
                self.renderings.lock().is_ok()
            );
        }
        let before_the_disk = started.elapsed().as_millis();
        {
            // **The reading goes to the disk as well as the index**, which is what makes a second start free: the
            // next session's `take_the_stored_refinements` serves this file instead of rebuilding it — measured at
            // 45 ms rebuilt against 1.2 ms served, see `docs/refinement.md` §3.2. Written *before* the index insert
            // because the write borrows the store and the insert needs it mutably; both are cheap.
            // **The rendering, kept beside the facts** --- a record with only the facts is one a hit cannot answer
            // from, because every query that reads the tree needs the text and the spans.
            let rendered = std::sync::Arc::clone(&indexed.rendered);
            let cooked: crate::index::CookedFile = indexed.into();
            if crate::probes::cooking() {
                // **How many declarations the cook found**, which the cache line above does not say: a cook that
                // renders an empty stream is filed as "this file has been read" and then shadows the file's
                // summary, and the count is the one number that tells that apart from a file that declares
                // nothing.
                eprintln!(
                    "commit_a_cooked: {} declaration(s) for {}",
                    cooked.declarations.len(),
                    path.display()
                );
            }
            if crate::probes::allocs() {
                eprintln!(
                    "[cook] committing {} with {} declaration(s)",
                    path.display(),
                    cooked.declarations.len()
                );
            }
            let declared = cooked.declarations.len();
            let writing = std::time::Instant::now();
            self.store
                .put_the_cooked_reading_with_rendering(&path, &cooked, &rendered);
            let written = writing.elapsed().as_millis();
            let inserting = std::time::Instant::now();
            self.store.index_mut().insert_cooked(&path, cooked);
            let inserted = inserting.elapsed().as_millis();
            if crate::probes::view() {
                eprintln!(
                    "commit timing {}: total {} ms | text+hash {} ms | cache {} ms | disk {} ms | index {} ms | {} declaration(s)",
                    path.display(),
                    started.elapsed().as_millis(),
                    text_hash_ms,
                    before_the_disk.saturating_sub(text_hash_ms),
                    written,
                    inserted,
                    declared
                );
            }
            // **Cooked here counts as asked**, so the closure loop does not queue it again the moment its reading is
            // evicted from the index's bounded map — a file cooked by a program read or by a query is exactly the
            // "already read" this loop is trying to reach.
            if let Ok(mut asked) = self.cooked_or_wanted.lock() {
                asked.insert(queue_key(&path));
            }
        }
        Some(reading)
    }

    /// **Which files the next cooking slice is for** — the queue pop, and the only part of taking a slice that needs
    /// the write lock.
    ///
    /// The materials themselves are [`Session::cooking_materials`], which takes `&self`: a summary lookup, a unit out
    /// of the cache and the text. Naming the files here and gathering them there is what lets the pump hold the read
    /// lock for `unit 15–42 ms` per file rather than the write lock a query waits for.
    ///
    /// It takes the queue's front, so a caller that gathers, renders and commits them in order has done exactly what
    /// `finish_a_slice`'s loop used to do — with everything but the pop outside the write lock.
    pub fn claim_cooking(&mut self, how_many: usize) -> Vec<PathBuf> {
        // **An A/B for the measurement, not a setting**: with `CPPLS_NO_COOKS` set the pump cooks nothing, which
        // is how much of the `containers.cpp` growth is the cooking pass itself rather than the indexing.
        if crate::probes::no_cooks() {
            return Vec::new();
        }
        self.cooking.take(how_many)
    }

    /// **The materials for a claimed slice**, skipping the files the index cannot describe yet.
    ///
    /// A file the cook queue names but the index cannot describe is dropped rather than re-queued:
    /// `want_cooked_reading` and the closure sweep both ask for it again, and a queue that re-adds its own failures
    /// is a queue that never empties — the note on `want_the_closure_cooked` records that hang.
    /// **Remember the files a cooking slice could not build**, so the closure walk stops asking for them.
    ///
    /// `asked` is what the slice claimed and `built` is what `materials_for` could gather from it; the difference is
    /// the set that would otherwise be wanted again on every drain. See [`Session::cooking_refused`], and
    /// [`Session::want_cooked_reading`] for the one caller that clears a refusal.
    pub fn refuse_the_cooks_that_built_nothing(&mut self, asked: &[PathBuf], built: &[PathBuf]) {
        let Ok(mut refused) = self.cooking_refused.lock() else {
            return;
        };
        for path in asked {
            if !built.iter().any(|made| made == path) {
                if crate::probes::cooking() {
                    eprintln!("refused a cook that built nothing: {}", path.display());
                }
                refused.insert(queue_key(path));
            } else if crate::probes::cooking() {
                eprintln!("a cook built: {}", path.display());
            }
        }
    }

    /// **The cook the disk already holds for `path`** — the record with its rendering, as the pump's claim needs it.
    ///
    /// The name is resolved the way every cook resolves it (the index's own spelling), because the store's key is
    /// built from the path the index holds and a caller's spelling of the same file is not that path.
    pub fn a_stored_cook(
        &self,
        path: &Path,
    ) -> Option<(
        PathBuf,
        crate::index::CookedFile,
        crate::preprocess::cooked::RenderedCooked,
    )> {
        let resolved = self.store.index().summary(path)?.path.clone();
        match self.store.the_stored_cook_reason(&resolved) {
            Ok((cooked, rendered)) => {
                if crate::probes::cooking() {
                    eprintln!(
                        "cooking: {} served from the disk ({} declaration(s))",
                        resolved.display(),
                        cooked.declarations.len()
                    );
                }
                Some((resolved, cooked, rendered))
            }
            Err(reason) => {
                // **Named, because "0 hits" is not a diagnosis.** `not stored` is a cold cache; `no closure` is the
                // index not holding every file the key is built from yet, which is a race the pump can lose.
                if crate::probes::cooking() {
                    eprintln!(
                        "cooking: {} has no stored cook: {reason}",
                        resolved.display()
                    );
                }
                None
            }
        }
    }
    pub fn materials_for(&self, paths: Vec<PathBuf>) -> Vec<AClaimedCook> {
        paths
            .into_iter()
            .filter_map(|path| {
                // **The disk before the render.** The key is the file's content, its configuration and its closure,
                // and the reader validates all three, so a hit is the reading this slice would produce — rendering
                // included. A record a program read wrote has no rendering and is not a hit: `view` answers from a
                if let Some((cooked, rendered)) =
                    self.store.the_stored_cooked_reading_with_rendering(&path)
                {
                    if crate::probes::cooking() {
                        eprintln!(
                            "cooking: {} served from the disk ({} declaration(s))",
                            path.display(),
                            cooked.declarations.len()
                        );
                    }
                    return Some(AClaimedCook::Stored(path, cooked, rendered));
                }
                let built = self.cooking_materials(&path);
                // **A claimed file that yields no materials is a file that is now neither queued nor cooked.**
                // `claim_cooking` popped it and this answered `None`, so it is gone from both places and the loop
                // that decides whether there is work left reads `cooked == 0` as "nothing to do". Naming the reason
                // is what separates "the summary is not in the index yet" (a state) from "the unit is provisional"
                // (also a state) from a defect.
                if built.is_none() && crate::probes::cooking() {
                    let indexed = self.store.index().summary(&path).is_some();
                    let (unit, provisional) = match self.unit_and_state_of(&path) {
                        Some((_, provisional)) => ("held", provisional),
                        None => ("none", false),
                    };
                    eprintln!(
                        "cooking: {} yields no materials | summary indexed {indexed} | unit {unit} \
                         provisional {provisional}",
                        path.display()
                    );
                }
                built.map(AClaimedCook::Fresh)
            })
            .collect()
    }

    /// **Lex, render and parse a slice's worth of materials** — the expensive half, under a read lock.
    pub fn render_them(&self, claimed: Vec<AClaimedCook>) -> AClaimedSlice {
        let tracing = crate::probes::cooking();
        let mut slice = AClaimedSlice::default();
        for cook in claimed {
            match cook {
                // Nothing to render: the disk holds this file's reading and its rendering, and the commit files them.
                AClaimedCook::Stored(path, cooked, rendered) => {
                    slice.stored.push((path, cooked, rendered));
                }
                AClaimedCook::Fresh(materials) => {
                    let path = materials.path.clone();
                    let built = self.render_a_cooked(&materials);
                    // **The last silent drop.** A claimed file whose render declines is gone from the queue and was
                    // never cooked, which the pump reads as "no work left".
                    match built {
                        Some(rendered) => slice.fresh.push(rendered),
                        None if tracing => eprintln!("cooking: {} did not render", path.display()),
                        None => {}
                    }
                }
            }
        }
        slice
    }

    /// **Put a slice's worth of readings into the index** — one insert each, under the write lock.
    pub fn commit_them(&mut self, slice: AClaimedSlice) -> Vec<CookedReading> {
        let mut readings = Vec::new();
        for (path, cooked, rendered) in slice.stored {
            if let Some(reading) = self.commit_a_stored_cook(path, cooked, rendered) {
                readings.push(reading);
            }
        }
        for rendered in slice.fresh {
            if let Some(reading) = self.commit_a_cooked(rendered) {
                readings.push(reading);
            }
        }
        readings
    }

    /// **File a reading the disk already held, with the rendering it was read from** — the commit half of a stored
    /// cook, and the one place the two kinds of reading meet.
    ///
    /// The record was validated when it was taken ([`SummaryStore::the_stored_cooked_reading_with_rendering`]
    /// recomputes the key from the file's content, its configuration and its closure), so there is no text to check
    /// and nothing to render. What is left is the insert the fresh path ends with, the rendering cache entry that
    /// makes `view` answerable, and the same "cooked here counts as asked" mark.
    fn commit_a_stored_cook(
        &mut self,
        path: PathBuf,
        cooked: crate::index::CookedFile,
        rendered: crate::preprocess::cooked::RenderedCooked,
    ) -> Option<CookedReading> {
        let text_hash = self
            .text(&path)
            .map(|text| crate::cache::content_hash(&text))?;
        // **The rendering, where a `view` will look for it** — the reason this record carries one at all.
        if let Ok(mut known) = self.renderings.lock() {
            let weight = rendered.weight();
            known.insert(
                a_rendering_key(&path, text_hash),
                std::sync::Arc::new(rendered),
                weight,
            );
        }
        // **The declarations the file's own text does not have** — the same count `render_cook` makes, and the only
        // number a stored record cannot carry: it is a comparison with the summary, not a property of the reading.
        let already_declared: std::collections::HashSet<String> = self
            .store
            .index()
            .summary(&path)?
            .declarations
            .iter()
            .map(crate::DeclFact::qualified_name)
            .collect();
        let reading = CookedReading {
            declarations: cooked.declarations.len(),
            only_after_expansion: cooked
                .declarations
                .iter()
                .filter(|fact| !already_declared.contains(&fact.qualified_name()))
                .count(),
            diagnostics: cooked.diagnostics.len(),
            unplaced: cooked.unplaced,
            // **Not stored, and not claimed.** The map is a property of the parse this slice did not do.
            mapped: crate::MapReport::default(),
        };
        if crate::probes::cooking() {
            eprintln!(
                "cooking: filed {} from the disk | {} declaration(s)",
                path.display(),
                reading.declarations
            );
        }
        self.stored_cooks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.store.index_mut().insert_cooked(&path, cooked);
        if let Ok(mut asked) = self.cooked_or_wanted.lock() {
            asked.insert(queue_key(&path));
        }
        Some(reading)
    }

    // **Everything a cook needs, gathered under the write lock and then used without it.**
    //
    // Owned rather than borrowed on purpose: the caller has to let go of the session between
    // [`Session::cooking_materials`] and [`Session::render_a_cooked`], so nothing here may borrow it. The unit is an
    // `Arc` the cache already holds, and the seed is a table built from the index's own macros.

    /// **The rendering of one file** — what the preprocessor produces from it, as text.
    ///
    /// The other half of [`Session::cook`], and the artifact a parser should be reading if the crate is to follow a
    /// compiler's reading rather than its own: `cook` renders a file and **parses the rendering**, then keeps the
    /// declarations it found and throws the text away. This is that text, for a caller that wants the reading
    /// itself — a probe comparing the two readings of a file, and any future consumer that answers from the
    /// rendering instead of from the file's own tokens.
    ///
    /// `None` under exactly the conditions [`Session::cook`] returns `None`: the file is not in the index, or its
    /// closure has not been read, so there is no environment to expand it against.
    ///
    /// # Why it is built the same way `cook` builds it
    ///
    /// The two must agree token for token, or a caller comparing them would be comparing a rendering against a
    /// *different* rendering. They share the steps — the unit, the file's macros, `cook_with` — and this method
    /// exists rather than `cook` returning its text because the text is large (a header's rendering is megabytes)
    /// and every caller of `cook` but this one discards it.
    pub fn rendered_text_of(&self, path: impl AsRef<Path>) -> Option<String> {
        Some(self.rendering_of(path)?.text)
    }

    /// **The rendering of one file, with its spans** — the text and the map that says where each token of it was
    /// written.
    ///
    /// The same artifact as [`Session::rendered_text_of`] with the half that makes it usable for a **view**:
    /// [`crate::FileView::parse_rendering`] needs the spans, because a rendering's offsets are its own and a client
    /// speaks the file's.
    ///
    /// **`&self`, and that is load-bearing.** Every step of it — the lookup, the unit (cached, behind its own lock),
    /// the read, the lex, the cook — reads the session and changes nothing, so a caller can build a rendering under
    /// the **read** lock instead of the write one a query is waiting for. The drain's macro-environment slice used to
    /// be the largest thing left inside the writer after the parse moved out, at 65–190 ms per header.
    pub fn rendering_of(
        &self,
        path: impl AsRef<Path>,
    ) -> Option<crate::preprocess::cooked::RenderedCooked> {
        let path = self.store.index().summary(path.as_ref())?.path.clone();
        let unit = self.translation_unit_of(&path)?;
        let text = self.files.read(&path)?;
        let seed = MacroTable::from_marked(self.store.index().macros());
        let (tokens, _) = cpp_parser::lex(&text, &cpp_parser::LexerConfig::default());
        let unit_definitions = unit.definitions();
        let view = unit.environment_of(&path)?;
        let macros =
            crate::preprocess::cooked::FileMacros::new(&view, &unit_definitions, Some(&seed), true);
        Some(crate::preprocess::cooked::cook_with(&text, &tokens, &macros).render())
    }

    /// **Read one file the way the compiler does, and put that reading in the index.**
    ///
    /// Its key carries the environment the rendering was cooked under, so a header included by two
    /// translation units with different macros gets two readings rather than one that is wrong for
    /// one of them.
    ///
    /// Returns the summary it committed. Costs the cook (a cached rendering is a hash and a lookup) plus the parse of
    /// the rendered text; `tests/rendering_real.rs` measures both halves.
    pub fn index_the_rendering(&mut self, path: impl AsRef<Path>) -> Option<crate::FileSummary> {
        let path = self.store.index().summary(path.as_ref())?.path.clone();
        let unit = self.translation_unit_of(&path)?;
        let environment = unit.environment_of(&path)?;
        let written = self.files.read(&path)?;
        let rendered = match self.known_rendering_of(&path, &written) {
            Some(rendered) => rendered,
            None => std::sync::Arc::new(self.rendering_of(&path)?),
        };
        self.store
            .index_the_rendering(&path, &rendered, Some(&environment))
            .cloned()
    }
    /// **A declaration's fact, read from the *rendering*** — the reading where a directive is resolved.
    ///
    /// For a fact whose [`DeclFact::split_by_a_directive`] is set the summary declines to name a
    /// type: the file's own text states one per branch and the written reading cannot know which is
    /// live.
    ///
    /// **Cost.** The rendering itself is the artifact a cook produces — a hash and a lookup when it is cached — and
    /// the summary of its text is computed here, so a caller pays one parse of the rendered text per split
    /// declaration it asks about. That is why this is asked from a hover on a hand-written `#if` rather than from a
    /// path a keystroke takes.
    ///
    /// [`DeclFact::split_by_a_directive`]: crate::DeclFact::split_by_a_directive
    pub fn fact_from_the_rendering(
        &self,
        path: impl AsRef<Path>,
        name: &str,
        scope: Option<&str>,
    ) -> Option<crate::DeclFact> {
        let path = self.store.index().summary(path.as_ref())?.path.clone();
        // **The cached rendering first.** `rendering_of` builds one — a unit walk, a lex and a cook — and the session
        // already keeps them by content hash; asking for the artifact it holds is the difference between a lookup and
        // 4 ms (`tests/rendering_real.rs`: the second ask was 3.9 ms with this missing, 23.8 → 19.1 before the summary
        // cache existed).
        let text = self.files.read(&path)?;
        let rendered = match self.known_rendering_of(&path, &text) {
            Some(rendered) => rendered,
            None => {
                // Built and **kept**: the key is the one `known_rendering_of` reads, so the next ask is a lookup.
                let built = std::sync::Arc::new(self.rendering_of(&path)?);
                self.remember_a_rendering(&path, crate::cache::content_hash(&text), built.clone());
                built
            }
        };
        // **The summary of the rendered text, cached** — see `rendering_summaries`. A hover on a declaration
        // a directive splits asks this, and before the cache existed every ask paid one parse of the rendering.
        let key = (path.clone(), crate::cache::content_hash(&rendered.text));
        let remembered = self
            .rendering_summaries
            .lock()
            .ok()
            .and_then(|mut known| known.get(&key).cloned());
        let summary = match remembered {
            Some(summary) => summary,
            None => {
                let built = std::sync::Arc::new(crate::summarize(
                    &path,
                    &rendered.text,
                    crate::SummaryKey::new(0, 0),
                ));
                if let Ok(mut known) = self.rendering_summaries.lock() {
                    let weight = built.declarations.len() + built.macros.len();
                    known.insert(key, built.clone(), weight);
                }
                built
            }
        };
        summary
            .declarations
            .iter()
            .find(|fact| {
                fact.name == name && fact.scope.as_deref() == scope && !fact.split_by_a_directive
            })
            .cloned()
    }

    /// Cook every **open** file — what the indexing loop does when its queue drains.
    ///
    /// The moment is the point: a file's environment is only complete once everything it includes has been read, and
    /// "the queue is empty" is exactly that moment. The files are the open ones because those are what a reader is
    /// looking at, and cooking costs a unit walk and a parse per file.
    ///
    /// Returns what each file read as, for a caller that reports it.
    pub fn cook_the_open_files(&mut self) -> Vec<(PathBuf, CookedReading)> {
        let open = self.documents.paths();
        let mut done = Vec::new();
        for path in open {
            if let Some(reading) = self.cook(&path) {
                done.push((path, reading));
            }
        }
        done
    }
}
