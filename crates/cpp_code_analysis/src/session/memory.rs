//! **The view cache** — remembering a parsed rendering, and keeping the session inside its budget.
//!
//! Moved here from `session/mod.rs` without a change. A view is the largest thing a session holds and the one most
//! worth reusing, so what is remembered, what is evicted, and what the session reports about its own memory all live
//! here.

use super::*;
use crate::FileSummary;
use crate::MemoryReport;
use crate::file;
use crate::index;

impl<F: FileProvider + Clone> Session<F> {
    /// **A view already parsed from exactly this input**, or `None`.
    ///
    /// A hit is a handful of `Arc` bumps and one red root — see [`Session::view`] for what the key is made of and why
    /// none of it can serve a stale parse, and [`crate::file::view::ParsedView`] for why the cached half has no root.
    pub(crate) fn a_view_already_parsed(&self, key: &ViewKey) -> Option<FileView> {
        let mut known = self.the_view_cache(key).lock().ok()?;
        let parsed = known.get(key)?;
        Some(FileView::from_parsed((**parsed).clone()))
    }

    /// **Which cache a view belongs in** — the rendering views apart from the buffer views. See
    /// [`Session::rendering_views`] for the measurement that split them.
    pub(crate) fn the_view_cache(
        &self,
        key: &ViewKey,
    ) -> &std::sync::Mutex<cache::cache_lru::Cache<ViewKey, std::sync::Arc<file::view::ParsedView>>>
    {
        if key.rendering.is_some() {
            &self.rendering_views
        } else {
            &self.views
        }
    }

    /// Remember `view` under `key`, weighed by what it actually costs — see [`a_view_weighs`].
    pub(crate) fn remember_a_view(&self, key: ViewKey, view: &FileView) {
        let weight = a_view_weighs(view);
        if let Ok(mut known) = self.the_view_cache(&key).lock() {
            let parsed = view.clone().into_parsed();
            known.insert(key, std::sync::Arc::new(parsed), weight);
        }
    }

    /// **What every table the session owns is holding, in bytes** — see [`crate::MemoryReport`].
    ///
    /// [`Session::cache_sizes`] answers "how many entries", which is the wrong unit for the
    /// question a five-gigabyte report asks: an entry is a `#define` or a rendering, and the two
    /// differ by six orders of magnitude. **How many walk contributions this session's store
    /// holds** — a diagnostic, and the number the sharing test asserts. The table itself is the
    /// store's; see [`crate::index::walk_cache::WalkCache`].
    /// **Turn the walk cache's cross-walk sharing on or off**, overriding the `CPPLS_WALK_SHARE` probe.
    ///
    /// An A/B and a test hook, not a setting: the sharing is off by default because a contribution replayed at another
    /// walk's position drops include edges — and the test that has to exercise the machinery itself asks for it here.
    pub fn set_sharing_contributions(&mut self, on: bool) {
        self.share_contributions = on;
    }

    pub fn walk_contributions_held(&self) -> usize {
        self.store.contributions().held()
    }

    /// **Keep the index's summaries under a byte budget** — P2's byte budget, applied after a drain.
    ///
    /// The largest summaries go first, because that is what a cap is for: the standard library's headers are 130 KB
    /// each and they are the same files on every machine, while a project's own sources are small. What it never
    /// touches:
    ///
    /// * **a file the reader has open** — a `didChange` would find its summary gone and the answer would be "not read
    ///   yet" for the file somebody is typing in;
    /// * **a file refined in this session** — its reading is what the refinement bought, and forgetting the summary
    ///   under it would keep the facts and lose the text they were mapped against;
    /// * **a file still in the queue** — the queue is about to read it, and a summary dropped now is one built twice.
    ///
    /// # And the fourth rule is why this rarely fires
    ///
    /// A file in `parsed_since_the_last_pass` is protected too, because the second pass needs its summary to re-read it.
    /// Under the demand-driven contract that backlog is **never empty** — a batch no longer drains it (it asks for
    /// nothing), and only a request does — so on a project of any size every indexed file is protected by one rule or
    /// another and the budget cannot fire. That is a real tension between P2's two halves, and the honest resolution is
    /// a policy for *which queued files may be forgotten* (the ones the pass would skip anyway: no bodied macro
    /// mentioned, no guarded declaration) rather than a looser filter here.
    ///
    /// A forgotten summary is re-read on demand ([`Session::needs_catching_up`]), which is the path a cache miss already
    /// takes, so the only cost is the re-read.
    pub fn enforce_the_summary_budget(&mut self, budget: usize) -> usize {
        let (_, bytes, _, _) = self.store.index().accounting();
        if bytes <= budget {
            return 0;
        }
        // The candidates: everything with a summary, with its size, largest first.
        let mut candidates: Vec<(PathBuf, usize)> = self
            .store
            .index()
            .summaries()
            .map(|summary| (summary.path.clone(), FileSummary::heap_bytes(summary)))
            .collect();
        candidates.sort_unstable_by_key(|candidate| std::cmp::Reverse(candidate.1));

        let open: std::collections::HashSet<String> = self
            .documents
            .paths()
            .iter()
            .map(|path| queue_key(path))
            .collect();
        let refined: std::collections::HashSet<String> = self
            .refined_readings
            .lock()
            .map(|refined| refined.iter().cloned().collect())
            .unwrap_or_default();

        let mut over = bytes.saturating_sub(budget);
        let mut forgotten = 0usize;
        for (path, size) in candidates {
            if over == 0 {
                break;
            }
            if open.contains(&queue_key(&path))
                || refined.contains(&queue_key(&path))
                || self.queue.is_queued(&path)
                || self
                    .parsed_since_the_last_pass
                    .iter()
                    .any(|queued| queued == &path)
            {
                continue;
            }
            if self.store.index_mut().forget(&path) {
                over = over.saturating_sub(size);
                forgotten += 1;
            }
        }
        forgotten
    }

    pub fn memory_report(&self) -> MemoryReport {
        let (vfs_files, vfs_text_bytes, vfs_line_index_bytes) = self.vfs.accounting();
        let (summaries, summary_bytes, cooked_files, cooked_bytes) =
            self.store.index().accounting();
        let (definitions, definition_bytes) = self
            .definitions
            .lock()
            .map(|definitions| definitions.accounting())
            .unwrap_or((0, 0));
        // The table keeps its own byte total, so the report does not walk every unit's events on every ask.
        let (units, unit_bytes) = self.held_units().accounting();
        let (renderings, rendering_bytes) = self
            .renderings
            .lock()
            .map(|known| {
                let bytes = known
                    .values()
                    .map(|rendering| rendering.text.len())
                    .sum::<usize>();
                (known.len(), bytes)
            })
            .unwrap_or((0, 0));
        let (dead_macro_files, dead_macro_offsets) = self.dead_macro_decisions();
        // **Both populations**, and the split is why the number is read from two caches: a rendering view is a
        // whole unit's tree and a buffer view is a page, and a report that added only one of them would
        // understate what a reader with a big file open is paying for.
        let counted = |cache: &std::sync::Mutex<
            cache::cache_lru::Cache<ViewKey, std::sync::Arc<file::view::ParsedView>>,
        >| {
            cache
                .lock()
                .map(|known| (known.len(), known.weight()))
                .unwrap_or((0, 0))
        };
        let (buffer_views, buffer_view_bytes) = counted(&self.views);
        let (rendering_views, rendering_view_bytes) = counted(&self.rendering_views);
        let (views, view_bytes) = (
            buffer_views + rendering_views,
            buffer_view_bytes + rendering_view_bytes,
        );
        let (walk_cache_entries, walk_cache_bytes) = self.store.contributions().accounting();
        let (walks_by_content_entries, walks_by_content_bytes) = self
            .walked_by_content
            .lock()
            .map(|held| (held.len(), held.weight()))
            .unwrap_or((0, 0));
        // **Which timelines are one allocation counted twice.** The unit table and the sharing cache hold the same
        // `Arc` whenever a timeline was both held for a root and remembered for its content — the common case — so
        // adding their byte counts would report the same megabytes twice. Measured rather than assumed: the pointers
        // are compared, and the walk table's half of every overlap is what the totals subtract.
        let shared_timeline_bytes = {
            let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
            for unit in self.held_units().timelines() {
                seen.insert(std::sync::Arc::as_ptr(unit) as *const () as usize);
            }
            let mut shared = 0usize;
            if let Ok(held) = self.walked_by_content.lock() {
                for unit in held.values() {
                    if !seen.insert(std::sync::Arc::as_ptr(unit) as *const () as usize) {
                        shared += unit.heap_bytes();
                    }
                }
            }
            shared
        };
        let tables = self.store.index().table_accounting();

        // **The prepared-but-uncommitted slices**, which no other field here covers: a `Prepared` holds a
        // built summary (and maybe a rendering) and `commit_a_wave` clears the map only when the queue empties.
        let (prefetched, prefetched_bytes) = (
            self.prefetched.len(),
            self.prefetched
                .values()
                .map(index::store::Prepared::heap_bytes)
                .sum::<usize>(),
        );
        MemoryReport {
            prefetched,
            prefetched_bytes,
            vfs_files,
            vfs_text_bytes,
            vfs_line_index_bytes,
            summaries,
            summary_bytes,
            cooked_files,
            cooked_bytes,
            definitions,
            definition_bytes,
            units,
            unit_bytes,
            renderings,
            rendering_bytes,
            views,
            view_bytes,
            dead_macro_decisions: dead_macro_files,
            dead_macro_bytes: dead_macro_offsets * std::mem::size_of::<usize>(),
            directive_signatures: self.directive_signatures.len(),
            include_searches: self.store.include_search_count().unwrap_or(0),
            units_read: self.units_read.len(),
            walk_cache_entries,
            walk_cache_bytes,
            walks_by_content_entries,
            walks_by_content_bytes,
            shared_timeline_bytes,
            index_names_bytes: tables.names.bytes,
            index_names_entries: tables.names.name_entries,
            index_scope_entries: tables.names.scope_entries,
            index_name_postings: tables.names.name_postings,
            index_scope_postings: tables.names.scope_postings,
            index_edges: tables.edges,
            index_edge_bytes: tables.edge_bytes,
            index_member_lists: tables.member_list_entries,
            index_member_list_bytes: tables.member_list_bytes,
            index_memo_entries: tables.guard_entries
                + tables.visibility_entries
                + tables.condition_entries,
            index_memo_bytes: tables.guard_bytes + tables.visibility_bytes + tables.condition_bytes,
        }
    }
}
