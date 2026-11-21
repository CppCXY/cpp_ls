//! **Edits and the watch filter** — what a client's `didOpen`/`didChange`/`didClose` does to the state.
//!
//! Moved here from `session/mod.rs` without a change. An edit is not applied to the index; it *invalidates* what the
//! index holds, and the dependents that have to be re-read are queued here.

//! What changed — edits, opens, closes, and the filesystem news that is not an edit.
//!
//! The two halves of a reconcile live here rather than in one call so that the disk can be asked **outside the
//! writer**: see [`Session::prepare_a_reconcile`] for why, and [`ReconcileAnswer`] for what crosses between them.

use super::*;

/// **What a reconcile found on disk** — the answer [`Session::prepare_a_reconcile`] hands to
/// [`Session::commit_a_reconcile`], and the only thing that crosses between the two phases.
///
/// Paths rather than events: the events are a wire-format detail of [`Session::changed`], and a caller that wants to
/// log "N files moved" would otherwise have to count them back out of an enum.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileAnswer {
    /// Files whose modification time or size moved since the session read them.
    pub modified: Vec<PathBuf>,
    /// Files the project listed and the disk no longer has, or the listing no longer names.
    pub removed: Vec<PathBuf>,
    /// Files the listing found that the project did not have.
    pub new: Vec<PathBuf>,
}

impl ReconcileAnswer {
    /// Is there nothing to apply? The ordinary answer, and the one a caller logs as "nothing moved".
    pub fn is_empty(&self) -> bool {
        self.modified.is_empty() && self.removed.is_empty() && self.new.is_empty()
    }

    /// How many files the answer is about, for a log line.
    pub fn len(&self) -> usize {
        self.modified.len() + self.removed.len() + self.new.len()
    }
}

impl<F: FileProvider + Clone> Session<F> {
    /// **What a name is at one position in one file** — see [`crate::MacroAt`] for why this exists and what the
    /// offset is for.
    ///
    /// Reads the unit the way [`Session::read_the_unit`] does, minus the indexing: this is a question *about* a file,
    /// and asking it must not change the answer to anything else.
    ///
    /// # Which unit a file belongs to
    ///
    /// A file in the project is its own unit. One that is not — a system header, a file the caller named by hand — is
    /// a member of some *other* file's unit, and which one is answered by the project's source list: the first source
    /// whose walk reaches it. `None` when nothing in the project reaches it, which is the honest "this session
    /// cannot speak about that file" rather than a `false` that would read as "not defined".
    pub fn macro_at(&mut self, file: &Path, name: &str, offset: usize) -> Option<crate::MacroAt> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if self.project.iter().any(|root| root == file) {
            candidates.push(file.to_path_buf());
        }
        candidates.extend(self.project.iter().filter(|root| *root != file).cloned());

        for root in candidates {
            let Some(unit) = self.translation_unit_of(&root) else {
                continue;
            };
            if let Some(answer) = unit.macro_at(file, name, offset) {
                return Some(answer);
            }
        }

        None
    }

    pub fn stats(&self) -> StoreStats {
        self.store.stats()
    }

    /// **How many diagnostics pulls this session answered from the memo and how many ran the rules** — see
    /// [`Session::pull_hits`].
    ///
    /// The two numbers a regression can assert without a clock: a pull that must be remembered is `pull_hits` going
    /// up and `pull_runs` staying where it was.
    pub fn pull_counts(&self) -> (u64, u64) {
        (
            self.pull_hits.load(std::sync::atomic::Ordering::Relaxed),
            self.pull_runs.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// **How many guarded `#define`s the second pass has decided about** — `(files, offsets)`.
    ///
    /// The one output of the second pass that a caller cannot get from an answer, and the one that decides whether
    /// the pass's unit walks are buying anything: a walk that records nothing and rebuilds nothing is a walk that can
    /// be skipped. See [`crate::SummaryStore::dead_macro_decisions`].
    pub fn dead_macro_decisions(&self) -> (usize, usize) {
        self.store.dead_macro_decisions()
    }

    /// **Where this session writes the summaries it can reuse**, so that a caller can say what a *second* run costs.
    ///
    /// Exposed because "the second time is fast" is a claim about a directory, and a caller measuring the first time
    /// has to be able to say which directory it means: a benchmark that reports a warm number as a cold one is how a
    /// five-second stall ships as a feature. Deleting what is here is a safe thing for a caller to do — every entry
    /// is derived from a file's text and is rebuilt on demand — and it is the only way to measure the cold path on a
    /// machine that has run before.
    pub fn cache_directory(&self) -> &Path {
        self.store.cache_directory()
    }

    /// The sources the project scan listed, which is the seed of the rest of the work list.
    pub fn project_files(&self) -> &[PathBuf] {
        &self.project
    }

    pub fn contains_project_file(&self, path: &Path) -> bool {
        self.project_paths.contains_key(&queue_key(path))
    }

    /// Add files the caller knows about to the project list — a build system's translation units, a test's
    /// fixtures, a directory listing somebody else did better.
    ///
    /// They join the **rest** half of the queue and the list a configuration change re-seeds from, exactly as the
    /// scan's files do. Returns how many were queued, which is not always how many were passed: a file already in
    /// the list is one file.
    ///
    /// This is also how a session over an in-memory project gets a project list at all: the scan reads a
    /// directory, and a set of buffers has none. See [`Session::with_config`].
    pub fn add_project_files(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> usize {
        let mut added = 0;

        for path in paths {
            if let std::collections::hash_map::Entry::Vacant(entry) =
                self.project_paths.entry(queue_key(&path))
            {
                entry.insert(path.clone());
                self.project.push(path.clone());
                added += 1;
            }

            self.queue.add(path, Priority::Rest, 0);
        }

        // **The header list sees the new files.** A header of this project is offered in a `#include` completion,
        // and "which files are the project" is exactly the list that just grew — so the half of the header index
        // that is derived from it is derived again. The search-path half is *not* re-read: that is a filesystem
        // walk, and nothing that happened here can have changed it.
        if added > 0
            && let Ok(mut headers) = self.headers.write()
        {
            *headers = headers.clone().with_project(self.project.clone());
        }

        added
    }

    // ---------------------------------------------------------------------------------------------
    // What changed
    // ---------------------------------------------------------------------------------------------

    /// The client opened a document, or said what its buffer now contains.
    ///
    /// The buffer becomes the text for that path, the summary built from the *old* text is dropped, and the file
    /// goes to the front of the open half of the queue. Dropping the summary is the honest half of this: the
    /// analysis could re-read the buffer here, and until it does, a query about a name in this file must answer
    /// "not read yet" rather than answer from text the user has already changed.
    ///
    /// The files it **includes** are marked for cooking one drain later, not here, and the reason is arithmetic: at
    /// this moment the index has nothing to follow — the file's own summary is the one that was just dropped — so
    /// its closure is not known yet. See `advance`, where the closure of every open file is marked once the index
    /// queue is empty.
    pub fn did_open(&mut self, path: impl AsRef<Path>, text: &str) {
        self.buffer_changed(path.as_ref(), text);
    }

    /// The client changed an open document's buffer.
    ///
    /// The same work as [`Session::did_open`] — one method exists per protocol event because a server matches on
    /// them, not because the analysis does anything different — and the reason it is not a *cheaper* path is worth
    /// stating: a change can add a declaration, remove one, or add an `#include` that changes what the file sees,
    /// so there is nothing to update in place. The summary is dropped and the file is re-read; the work is one
    /// parse, and the parse is what the cache cannot answer because the text has never been seen.
    pub fn did_change(&mut self, path: impl AsRef<Path>, text: &str) {
        self.buffer_changed(path.as_ref(), text);
    }

    /// The client saved a document — with the text, when it sent one.
    ///
    /// A save changes nothing about what the analysis reads: the buffer was already the text, and the disk now
    /// agrees. `None` is therefore no work at all, which is why the text is an `Option` rather than the analysis
    /// re-reading the file to compare it. A client that sends the text gets the same treatment as a change, because
    /// a save can carry an edit a change event did not.
    pub fn did_save(&mut self, path: impl AsRef<Path>, text: Option<&str>) {
        if let Some(text) = text {
            self.buffer_changed(path.as_ref(), text);
        }
    }

    /// The client closed a document: the file is the filesystem's again.
    ///
    /// The summary is dropped, because the one in the index describes the buffer and the disk may never have seen
    /// it — a language server that kept answering from a closed, unsaved buffer would report declarations that do
    /// not exist anywhere. The file goes to the **rest** half of the queue: the user has stopped looking at it, and
    /// everything else they are looking at now comes first.
    pub fn did_close(&mut self, path: impl AsRef<Path>) {
        self.changed_input();
        let path = path.as_ref();
        // **The focused document goes with it**, so the next program read is not for a file nobody is looking at.
        if let Ok(mut focused) = self.focused_document.lock()
            && focused.as_deref() == Some(path)
        {
            *focused = None;
        }
        self.forget_refinement(path);
        self.documents.close(path);
        // The text the analysis was holding was the *buffer's*, and the disk may never have seen it. Dropping the
        // entry is what makes the next question read the file again — a closed buffer is not a text this session
        // knows any more, and a view built on it would answer about an edit nobody saved.
        self.vfs.close(path);
        self.store.forget(path);
        self.forget_the_read_ahead();
        self.directive_signatures.remove(&queue_key(path));
        self.units
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.units_read.clear();
        self.queue.again(path.to_path_buf(), Priority::Rest, 0);
    }

    /// Turn client or native filesystem notifications into index work.
    /// The batch is built here from the same [`WatchFilter`] the session was opened with, so the cache directory,
    /// `.git` and any ignored build tree are filtered once, in one place.
    ///
    /// What comes back is [`SummaryStore::respond`]'s answer — what was forgotten, what has to be re-read, and
    /// whether the configuration changed — and the session applies it to its queue before returning it, so a caller
    /// that ignores the return value still gets the work done. A configuration change re-seeds from the project
    /// scan and the open buffers, in that order's reverse: what the user is looking at is read first.
    pub fn changed(&mut self, events: impl IntoIterator<Item = FileEvent>) -> Response {
        let mut batch = ChangeBatch::new(self.filter.clone());
        batch.extend(events);
        self.respond(&batch)
    }

    /// **Whatever moved on disk since the last read, without reading anything.**
    ///
    /// This is the answer to a watcher that **overflowed**: the events are lost, and the honest question is not "what
    /// did the OS report" but "what does the filesystem say now". Re-opening the workspace answers it too, and that is
    /// what the server used to do — a full session rebuild with a compiler run, a whole-project index and a
    /// whole-program read, which on a Linux kernel tree is ten seconds of work per overflow and, in the session this
    /// was measured on, **four of them in fifty seconds** (332 overflow reports). The rescan it was named after costs a
    /// `stat` per project file and a directory walk:
    ///
    /// ```text
    /// the project's files, listed again     the compile database or the root's walk (the same one the session opened with)
    /// a file whose stamp moved              re-read it                                        (Session::index_one records the stamp)
    /// a file the listing no longer has      forget it, and re-read what resolved to it
    /// a file the listing has that we do not  add it to the project and read it
    /// ```
    ///
    /// A file the analysis is holding as a **buffer** is skipped: the client's text is the text, and the client is
    /// the authority for its own edits.
    ///
    /// # Why `listing` is a parameter
    ///
    /// Because the listing is what costs: on a 3 000-file project, `list_the_project` re-reads
    /// `compile_commands.json` (3 000 entries of JSON) and asks `exists` for every one of them, which measured **1.3 s**
    /// against the 300 ms a stamp-only look takes — and a reconcile runs under the write lock, so that is a request's
    /// latency. A **database** project's list cannot change without the database changing (a file the database does not
    /// name is not a project file; a header joins the index through an `#include`, not through this list), so a lost-event
    /// report — the common case — asks for the stamps only. A **directory** that appeared, vanished or moved is the
    /// case that needs the listing: files may have arrived wholesale, and only a walk can see the one nobody includes
    /// yet. A project with no database is walked either way: its list *is* the tree.
    ///
    /// Returns the same [`Response`] [`Session::changed`] does, so a caller can say how much moved — an answer that
    /// reconciles nothing is the ordinary one, and saying so is what keeps a rescan out of the log's warnings.
    ///
    /// The work is split so that the disk is asked **before** the caller takes the writer
    /// ([`Session::prepare_a_reconcile`] and [`Session::commit_a_reconcile`]): the stats are the expensive half —
    /// measured at 400–850 ms for 3 000 files on Windows, dominated by the listings — and holding the write lock for
    /// them delays every request in flight for that long. This form does both halves in one call, for a caller that
    /// already holds the writer.
    pub fn reconcile(&mut self, listing: bool) -> Response {
        let answer = self.prepare_a_reconcile(listing);
        self.commit_a_reconcile(answer)
    }

    /// **Phase one of a reconcile: what the disk has to be asked**, read without changing anything.
    ///
    /// Two `stat`s per project file when a listing is needed (the listing checks each database entry exists, and then
    /// the stamp is read) and one when it is not. That is the cost a caller keeps out of the writer: read this under
    /// the read lock (queries are unaffected), then take the writer only for [`Session::commit_a_reconcile`], which
    /// touches nothing but the session.
    pub fn prepare_a_reconcile(&self, listing: bool) -> ReconcileAnswer {
        let relist = listing || self.discovery.database.is_none();
        let listed = if relist {
            self.list_the_project()
        } else {
            self.project.clone()
        };
        let listed_keys: std::collections::HashSet<String> =
            listed.iter().map(|path| queue_key(path)).collect();

        let mut answer = ReconcileAnswer::default();

        // **Gone**: the project no longer names it, or the file is not on disk any more. Both are one answer — the
        // index must forget the summary and re-read what resolved to it — and the difference is only who said so.
        for path in &self.project {
            let key = queue_key(path);
            let stamp = crate::file::FileStamp::of(path);
            if stamp.is_none() || !listed_keys.contains(&key) {
                answer.removed.push(path.clone());
                continue;
            }
            if self.documents.is_open(path) {
                continue;
            }
            if let Some(stamp) = stamp
                && self.stamps.get(&key).is_none_or(|known| *known != stamp)
            {
                answer.modified.push(path.clone());
            }
        }

        // **New**: a file the listing has and the project did not — a header somebody just added to the tree.
        answer.new = listed
            .into_iter()
            .filter(|path| !self.project_paths.contains_key(&queue_key(path)))
            .collect();
        answer
    }

    /// **Phase three of a reconcile: the facts go into the session**, under the writer and with no disk work.
    ///
    /// The answer [`Session::prepare_a_reconcile`] produced is applied here — the project list follows the disk, the
    /// stamps of the files that moved are dropped so the next read records a new one, and the events go through
    /// [`Session::changed`] like any other change.
    pub fn commit_a_reconcile(&mut self, answer: ReconcileAnswer) -> Response {
        let ReconcileAnswer {
            modified,
            removed,
            new,
        } = answer;

        let mut events: Vec<FileEvent> = Vec::with_capacity(modified.len() + removed.len());
        events.extend(removed.iter().cloned().map(FileEvent::removed));
        events.extend(modified.iter().cloned().map(FileEvent::modified));

        if !new.is_empty() {
            self.add_project_files(new.clone());
        }

        // The removed files leave the project list too: a listing that keeps a deleted file would ask about it on
        // every reconcile, and a stat of a missing path is the one answer that never changes.
        for path in &removed {
            let key = queue_key(path);
            if self.project_paths.remove(&key).is_some() {
                self.project.retain(|held| queue_key(held) != key);
            }
            self.stamps.remove(&key);
        }
        for path in &modified {
            self.stamps.remove(&queue_key(path));
        }

        // **The new files are part of the answer**: `changed` reports what the *store* has to re-read, and a file the
        // index has never held is not the store's — it is the queue's. Reporting it here is what lets a caller say
        // how much the reconcile found, and a reconcile that quietly added a file would be silent about the one thing
        // a lost-event report is most likely to have missed.
        let mut response = self.changed(events);
        response.reindex.extend(new);
        response
    }

    /// **The project's files, listed again** — the same compile database or walk the session was opened with.
    ///
    /// Used by [`Session::reconcile`] and by nothing else: the listing is the *input* to a rescan, and every other
    /// question in this module is about files whose names the session already has.
    pub(crate) fn list_the_project(&self) -> Vec<PathBuf> {
        let config = &self.discovery.config.config;
        let mut extra_extensions = config.workspace.source_extensions.clone();
        if self.config.cuda() {
            extra_extensions.extend(["cu".to_string(), "cuh".to_string()]);
        }
        let database = self
            .discovery
            .database
            .as_ref()
            .map(|database| &database.commands);
        let mut project = super::project_files(
            &self.files,
            &self.filter,
            &self.root,
            database,
            &extra_extensions,
            config.index.max_files.unwrap_or(super::MAX_PROJECT_FILES),
        );
        for file in super::library_files_to_read(&config.libraries, &self.root) {
            if !project.contains(&file) {
                project.push(file);
            }
        }
        project
    }

    /// The effective watch filter, including project configuration and cache exclusions.
    pub fn watch_filter(&self) -> WatchFilter {
        self.filter.clone()
    }

    /// [`Session::changed`] for a caller that did its own coalescing.
    pub fn respond(&mut self, batch: &ChangeBatch) -> Response {
        if !batch.is_empty() {
            self.changed_input();
        }
        // Anything read ahead was read before this batch said the world moved.
        self.forget_the_read_ahead();

        // **What the batch's files said, before the store takes their summaries away**: a watched change is the same
        // event as an edit as far as a dependent's reading is concerned, and the reverse walk needs the edges and the
        // macro facts that are still there.
        for (path, _) in batch.changed() {
            // What the file says **now**: the buffer if one is open, the disk otherwise, and nothing when it is gone.
            let text = self.files.read(path);
            let moved = self.directives_moved(path, text.as_deref());

            if moved.environment {
                self.invalidate_dependents(path);
            }
            // …and the definitions this session read **out of** that file: their keys are offsets in it, so an edit
            // that moved them would leave entries a later fact could reach and never wrote.
            self.held_definitions().forget(path);
            // …and the **translation units**: a unit is a reading of its whole closure, so a file whose *directives*
            // moved anywhere in it makes every unit built over it a reading of a timeline that is no longer there.
            // Cleared whole, for the reason `Session::translation_unit_of` gives: the honest short answer to "did
            // something inside it move" is "assume so", and the disk entry checks file by file when it is asked
            // again. A change that left every directive where it was leaves the timeline as it was.
            if moved.layout {
                self.units
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clear();
                self.units_read.clear();
            }
        }

        let response = self.store.respond(batch);
        for path in &response.forgotten {
            self.forget_refinement(path);
            if !self.documents.is_open(path) {
                self.vfs.close(path);
            }
        }

        if response.everything {
            // The configuration is part of every summary's key, so every stored summary is now a miss: the work is
            // the whole project again, open documents first — and `respond` has already been through the paths in
            // the batch, so this is not a repeat of them but the list to rebuild from.
            //
            // `requeue` rather than `add`: these files have been worked, and `add` exists to refuse exactly that.
            let project = self.project.clone();
            for path in project {
                self.queue.requeue(path, Priority::Rest, 0);
            }
            for path in self.documents.paths() {
                self.queue.again(path, Priority::Open, 0);
            }
        } else {
            for path in &response.reindex {
                self.forget_refinement(path);
                self.store.forget(path);
                if !self.documents.is_open(path) {
                    self.vfs.close(path);
                }
                // A file the user has open is the file they are waiting for; `respond` does not know about
                // buffers, and this is the one thing the session adds to its answer.
                let priority = if self.documents.is_open(path) {
                    Priority::Open
                } else {
                    Priority::Rest
                };
                self.queue.again(path.clone(), priority, 0);
            }
        }

        response
    }

    /// The two events that replace a path's text, which differ in the protocol and not here.
    ///
    /// The VFS is told **in the same breath** as the overlay, and that order matters: the schema is that a file's
    /// text and its line index are never out of step, so the entry takes the new text (and builds its index) before
    /// anything can ask a question about the file — and the summary is dropped with it, because the old one
    /// describes text that no longer exists.
    pub(crate) fn buffer_changed(&mut self, path: &Path, text: &str) {
        // **The file the reader is looking at**, which is the only document a whole-program read is worth doing for:
        // every `did_open`/`did_change`/`did_save` names it, and a background tab names nothing.
        if let Ok(mut focused) = self.focused_document.lock() {
            *focused = Some(path.to_path_buf());
        }
        self.changed_input();

        // **Every stored-refinement miss is about a text, and a text just moved.**
        if let Ok(mut misses) = self.refinement_misses.lock() {
            misses.clear();
        }
        // **The clock the one indivisible piece of work is deferred by.** Recorded first, so that nothing this edit
        // starts can be mistaken for the idle period that follows it — see [`Session::a_unit_may_be_read_now`] and
        // [`UNTIL_A_UNIT_MAY_BE_READ`].
        self.last_edit = std::time::Instant::now();
        // **Judged before the old text is replaced**: the first edit of a file has no signature on record, and the
        // text the VFS is holding is what it said until this moment.
        if !self.directive_signatures.contains_key(&queue_key(path))
            && let Some(held) = self.vfs.held(path)
        {
            let before = crate::preprocess::directive::directive_signature(&held.text);
            self.directive_signatures.insert(queue_key(path), before);
        }
        let moved = self.directives_moved(path, Some(text));
        self.forget_the_read_ahead();

        self.documents.open(path, text);
        self.vfs.insert(path, text, true);

        // **Who read what this file used to say**, asked while it still says it: `forget` below takes the summary
        // with it, and the answer is "every file that includes this one, at any depth" — see
        // [`Session::invalidate_dependents`]. Only when the file's directives changed: a file that says the same
        // things to its includers says them to the same readings.
        if moved.environment {
            self.invalidate_dependents(path);
        }

        // Drops the summary **and the cooked reading**: what this file was read *as* is no longer what it is, and a
        // declaration whose range points into the text the user just replaced is a wrong answer rather than a
        // missing one. The reading is rebuilt when the queue drains — see [`Session::cooking`].
        self.forget_refinement(path);
        self.store.forget(path);
        self.held_definitions().forget(path);
        // **And the file is askable again**, because the reason it was asked for — its text — has moved; the closure
        // loop must queue it once more, or an edited header would keep a stale reading for the rest of the session.
        if let Ok(mut asked) = self.cooked_or_wanted.lock() {
            asked.remove(&queue_key(path));
        }
        // **And the rendering this text was asked for**, so the next query asks for the new one.
        if let Ok(mut asked) = self.renderings_asked.lock() {
            asked.retain(|(known, _)| known != path);
        }
        // **The translation unit of *this* file, and only this file's.** A unit is a timeline over a closure, so an
        // edit that moves a directive has changed the one rooted here; it has not changed the one rooted at a file
        // that merely *includes* this one, whose timeline is still the same sequence of directives at the same
        // offsets. Clearing the whole table was the first version of this and it threw away up to sixteen units to
        // invalidate one — including, on the next keystroke, the file's own.
        //
        // The disk entry needs no such care: it is keyed per file and checked against the closure's hashes, so it
        // refuses itself the moment anything it was built from moves (`crate::tu_cache`).
        if moved.layout {
            self.held_units().remove(&queue_key(path));
            // **And the program this file is in, for the same reason and no other.** A body edit changes the file's own
            // text, but not the program's *shape*: the closure's directives, includes and conditions are where they
            // were, so the facts a program read filed for every **other** file of the closure are still true — and the
            // edited file's own facts are rebuilt by the per-file cook (`Session::want_the_closure_cooked`). Re-reading
            // the program after every pause in typing would be **5.24 s of background CPU for an answer that did not
            // change** (measured: 180 files, 806 008 tokens, `tests/keystroke_probe.rs`).
            //
            // A *directive* edit is the other case: every offset the timeline recorded moved, and the rendering of the
            // whole program changes with it, so this file's program is read again at the next quiet moment.
            self.units_read.remove(&queue_key(path));
        }
        // **The one place a refusal is cleared**, and it is cleared because the reason for it — the text — has
        // changed. See [`Session::want_cooked_reading`], which used to clear it on every request and turned a cook
        // that cannot succeed into a loop.
        if let Ok(mut refused) = self.cooking_refused.lock() {
            refused.remove(&queue_key(path));
        }
        self.cooking.want(path);

        self.queue.again(path.to_path_buf(), Priority::Open, 0);
    }

    /// **Did this text move any directive of the file?** — the two comparisons an edit is judged by, and the record
    /// the next one is judged against.
    ///
    /// `None` for the text is a file that is gone, and it moved everything. A file with no signature on record has
    /// never been compared, so it is assumed to have moved: the conservative answer, which is one re-read.
    pub(crate) fn directives_moved(&mut self, path: &Path, text: Option<&str>) -> DirectivesMoved {
        let key = queue_key(path);

        let Some(text) = text else {
            self.directive_signatures.remove(&key);
            return DirectivesMoved {
                environment: true,
                layout: true,
            };
        };

        let now = crate::preprocess::directive::directive_signature(text);
        match self.directive_signatures.insert(key, now) {
            Some(before) => DirectivesMoved {
                environment: before.environment != now.environment,
                layout: before.layout != now.layout,
            },
            None => DirectivesMoved {
                environment: true,
                layout: true,
            },
        }
    }

    /// **Drop the cooked readings a change to `path` invalidates, and want them built again.**
    ///
    /// A file's cooked reading is a reading of its *environment* as well as of its text: `#define BEGIN_NS namespace
    /// ns {` is a fact about `ns.h`, and every file that includes `ns.h` — at any depth — read as whatever that
    /// macro said when its own reading was built. So a change here invalidates the reading of **every file that can
    /// see it** ([`ProjectIndex::dependents_of`]).
    ///
    /// # Why it is no longer "the open files that include this one"
    ///
    /// Because that was the set of files that *had* a reading. The session now cooks the whole project
    /// ([`Session::want_the_project_cooked`]), so a header's dependents hold readings too — and a reading left
    /// behind after its environment changed is a **wrong** answer, not a missing one. Dropping rather than marking
    /// is the same rule as before: between the change and the next drain, "not found" is honest, while a declaration
    /// whose range describes text that is no longer there is not.
    ///
    /// # When it is called
    ///
    /// Only for a file whose **directives** changed ([`Session::directives_moved`]). The environment a dependent reads
    /// in is what its includes' directives brought into force: a `#define` (added, removed, or with a new body), an
    /// `#undef`, an `#include`, a condition. An edit that changes none of those — a function body, a comment, a
    /// declaration — says the same thing to every includer, so their readings stay. The gate used to be "the file
    /// defined macros *before* the edit", which is the wrong question twice over: it read the text the user had
    /// already replaced (so the edit that *added* the first `#define` to a header was never noticed), and it ignored
    /// everything but `#define` (an `#include` added to a macro-free header changes what every includer sees).
    ///
    /// Conservative in the safe direction: the cost of being wrong is one re-cook, and the cost of being wrong the
    /// other way is a stale answer.
    pub(crate) fn invalidate_dependents(&mut self, path: &Path) {
        let dependents = self.store.index().dependents_of(path);
        for dependent in dependents {
            // **And the timeline it was read out of.** A cooked reading is not the only thing a dependent holds
            // that was made out of this file: it also has a **translation unit**, and the in-memory one is checked
            // against nothing at all. The disk entry is keyed on the content of its closure, so this edit refuses
            // it on its own — which is why the defect hid here: the cache looked invalidation-proof, and the table
            // in front of it was not.
            //
            // Measured: with the readings dropped and the timelines left behind, `ns.h`'s macro changed from
            // `namespace one` to `namespace two`, the file was re-cooked **out of the timeline that still said
            // `one`**, and `two::Widget` came back `NotDeclaredHere` — with every step of the invalidation
            // working. The re-cook is what needs the new timeline, so this is where it has to go.
            self.held_units().remove(&queue_key(&dependent));

            // **And the summary itself, which is the part this loop did not used to own.** A dependent's summary is
            // now a reading of *this* file's replacement lists — `Widget` is in `std` because `open.h` says
            // `#define _STD_BEGIN namespace std {`, and the parser read that out of the rendering — so a directive
            // that moved makes every dependent's summary a statement about a body that no longer exists.
            //
            // Dropping it is the *same* answer the cooked reading gets, for the same reason: between the edit and
            // the next drain, "not found" is honest while `std::Widget` pointing at a body that now opens `studio`
            // is not. Nothing is lost for ever — the file is queued, read, and the re-read pass renders it again
            // from the new timeline, which is exactly the pipeline that built it.
            //
            // Measured: without this, `std::Widget` kept answering after `_STD_BEGIN` became `namespace studio {`,
            // with every other step of the invalidation working — and the assertion that used to stand for this
            // (`macro_readings` non-empty) was a note nothing read, so the capability had never been wired at all.
            self.forget_refinement(&dependent);
            let was_indexed = self.store.forget(&dependent);
            if was_indexed {
                self.queue.again(dependent.clone(), Priority::Rest, 0);
            }
            // **And the dependent is askable again**, for the reason [`Session::buffer_changed`] gives about the
            // edited file: its reading was just invalidated, so the closure loop must be free to queue it — an
            // asked-once set that outlived an invalidation would leave the file on the old macro for the session.
            if let Ok(mut asked) = self.cooked_or_wanted.lock() {
                asked.remove(&queue_key(&dependent));
            }
            if let Ok(mut asked) = self.renderings_asked.lock() {
                asked.retain(|(known, _)| known != &dependent);
            }

            // **The named rendering too.** `renderings` is keyed by the *dependent's* own text, which did not
            // change — so the entry left behind is a rendering of a world whose macros have moved, and the next
            // ask would find it instead of rendering again.
            let dependent_key = a_rendering_key(&dependent, 0).0;
            if let Ok(mut known) = self.renderings.lock() {
                known.retain(|(held, _)| a_rendering_key(held, 0).0 != dependent_key);
            }
        }
    }
}
