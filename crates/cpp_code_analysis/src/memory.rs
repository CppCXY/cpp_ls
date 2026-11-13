//! **What the analysis is holding, in bytes** — the number a report like "five gigabytes" needs before anything can
//! be called a leak.
//!
//! # Why this exists, and what it is not
//!
//! The resident set says *that* it happened; it does not say **which table** grew, and a plan built
//! on a total is a plan that will move the total somewhere else. So every structure that can grow
//! is asked for its own size, and [`MemoryReport`] is the answer in one place.
//!
//! The numbers are **estimates with a stated method**: for each entry, `size_of` of the struct plus the bytes of the
//! heap strings it owns. They are not exact (`malloc` overhead, a `HashMap`'s spare capacity and the allocator's own
//! bookkeeping are not counted), and they are consistently *under* the resident set — a discrepancy that is itself
//! data: it is the slack, and it is where a fragmentation or over-capacity problem shows up.
//!
//! # What is counted
//!
//! ```text
//! vfs            the text and line index of every file the session has read — **no budget, see the audit**
//! summaries      the raw reading of every indexed file
//! cooked         the second reading (the compiler's), per cooked file
//! definitions    `MacroDefinitions`: every `#define` read back, keyed by (path, offset)
//! units          the held translation units: events, frames, name index
//! renderings     the LRU cache of rendered closures
//! bookkeeping    dead macros, directive signatures, include searches, units-read keys
//! walk cache     the process-wide contribution table (a static — see the audit)
//! ```
//!
//! [`MemoryReport`] is a value: it can be printed, compared across two runs, or asserted in a test.

/// The size of every table the session holds, as estimated bytes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MemoryReport {
    /// Files the VFS is holding the text of.
    pub vfs_files: usize,
    /// The bytes of those texts.
    pub vfs_text_bytes: usize,
    /// The line indexes that go with them (an offset per line, plus a byte per line).
    pub vfs_line_index_bytes: usize,

    /// Summaries in the index, and their estimated bytes.
    pub summaries: usize,
    pub summary_bytes: usize,
    /// The cooked reading: how many files have one, and their estimated bytes.
    pub cooked_files: usize,
    pub cooked_bytes: usize,

    /// Entries in `MacroDefinitions`, and their estimated bytes.
    pub definitions: usize,
    pub definition_bytes: usize,

    /// Held translation units, and their estimated bytes.
    pub units: usize,
    pub unit_bytes: usize,

    /// The rendering cache: entry count and the bytes of rendered text.
    pub renderings: usize,
    pub rendering_bytes: usize,
    /// **The parse trees kept for reuse**: how many views, and their estimated tree bytes.
    ///
    /// The newest table here and the largest per entry — a rowan tree is 8–24× the text it describes
    /// (`docs/parser-review.md` §2) — bounded by [`crate::cache::cache_lru::VIEW_BYTES`] of *estimated tree*. It buys
    /// a hundredfold on every request that asks a question about a position, which used to parse the rendering again.
    pub views: usize,
    pub view_bytes: usize,

    /// Dead-macro decisions, one vector of offsets per file.
    pub dead_macro_decisions: usize,
    pub dead_macro_bytes: usize,
    /// Directive signatures, one per edited file.
    pub directive_signatures: usize,
    /// Include-search answers (bounded).
    pub include_searches: usize,
    /// Unit keys this session has ever read.
    pub units_read: usize,

    /// The process-wide contribution table: entries and estimated bytes.
    pub walk_cache_entries: usize,
    pub walk_cache_bytes: usize,

    /// **Timelines remembered by closure content** — the dedup table, entries and estimated bytes.
    ///
    /// **Bounded in bytes** (`MAX_WALKS_BY_CONTENT_BYTES`, 64 MB) rather than in entries: a timeline measures
    /// 0.1–6.2 MB, so a count would be a budget of anywhere between six megabytes and two gigabytes.
    pub walks_by_content_entries: usize,
    pub walks_by_content_bytes: usize,
    /// **The part of the two timeline tables that is one allocation, not two.**
    ///
    /// The unit table and the sharing cache hold the *same* `Arc<TranslationUnit>` whenever a timeline was both
    /// held for a root and remembered for its closure's content, which is the common case — so adding
    /// [`MemoryReport::unit_bytes`] to [`MemoryReport::walks_by_content_bytes`] counts one allocation twice. This
    /// field is the overlap, measured by comparing the `Arc` pointers rather than assumed, and the totals below
    /// subtract it.
    ///
    /// Measured on a 103-source project at twenty opened files: `units 40.4 MB + walks 46.6 MB` with every
    /// timeline in both, so the report said **87 MB** where the process held **47**.
    pub shared_timeline_bytes: usize,

    /// **The index's own tables** — what `summary_bytes` and `cooked_bytes` are *about*, rather than the facts.
    ///
    /// The inverted index (`names`), the include graph, and the memos that outlive a request. None of it was in this
    /// report, and on a 103-source project the session said **112.4 MB** while the process held **263 MB** — a gap
    /// that had three candidate explanations and no way to tell them apart. See
    /// [`crate::index::project::ProjectIndex::table_accounting`].
    pub index_names_bytes: usize,
    /// Postings in the inverted index, under names and scopes — one per non-local declaration.
    pub index_name_postings: usize,
    /// Postings under a scope.
    pub index_scope_postings: usize,
    /// Include edges across the project.
    pub index_edges: usize,
    /// The include graph and the module map, in bytes.
    pub index_edge_bytes: usize,
    /// Member lists remembered, and the bytes of what they hold.
    pub index_member_lists: usize,
    pub index_member_list_bytes: usize,
    /// Guard verdicts and conditional-include answers remembered.
    pub index_memo_entries: usize,
    pub index_memo_bytes: usize,
    /// Distinct names in the inverted index.
    pub index_names_entries: usize,
    /// Distinct scopes in it.
    /// **The slices prepared and not yet committed** — [`Session::prefetched`].
    ///
    /// A table the report did not name, and the only place a *summary* can be held without being in the index:
    /// `commit_a_wave` moves entries out as it commits them and clears the map when the queue empties, but a wave
    /// prepares up to four times the files it will commit. On `test_cpp/src/containers.cpp` the pump's own tables
    /// reported 83–86 MB while the process grew to gigabytes and the work was finite (`pending` 26 → 10), so the
    /// bytes had to be somewhere the report did not look.
    pub prefetched: usize,
    pub prefetched_bytes: usize,
    pub index_scope_entries: usize,
}

impl MemoryReport {
    /// **The index's own tables, in bytes** — the part of the report that is not a file's facts.
    pub fn index_table_bytes(&self) -> usize {
        self.index_names_bytes
            + self.index_edge_bytes
            + self.index_member_list_bytes
            + self.index_memo_bytes
    }

    /// Everything the session's own tables hold — the sum of the fields above, without the walk cache, which is
    /// process-wide rather than the session's.
    ///
    /// **Counts a shared timeline once.** `unit_bytes` and `walks_by_content_bytes` overlap by
    /// [`MemoryReport::shared_timeline_bytes`]; the walk table's half is excluded here and the unit table's half
    /// stays, so the sum is the bytes actually held.
    pub fn total(&self) -> usize {
        self.vfs_text_bytes
            + self.vfs_line_index_bytes
            + self.summary_bytes
            + self.cooked_bytes
            + self.definition_bytes
            + self.unit_bytes
            + self.rendering_bytes
            + self.view_bytes
            + self.dead_macro_bytes
            + self.index_table_bytes()
            + self.walks_by_content_bytes
            - self.shared_timeline_bytes
    }

    /// The same total including the tables that are not the session's own — the process-wide contribution table.
    pub fn total_with_the_process(&self) -> usize {
        self.total() + self.walk_cache_bytes
    }
}

impl std::fmt::Display for MemoryReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mb = |bytes: usize| format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0));
        let thousands = |count: usize| {
            if count >= 10_000 {
                format!("{}k", count / 1000)
            } else {
                count.to_string()
            }
        };
        write!(
            formatter,
            "vfs {} files {} ({}) | summaries {} ({}) | cooked {} ({}) | definitions {} ({}) | \
             units {} ({}) | renderings {} ({}) | views {} ({}) | bookkeeping {} dead-macro offsets, {} signatures, \
             {} searches, {} units-read | walk cache {} ({}) | walks by content {} ({}, {} shared with units)",
            self.vfs_files,
            mb(self.vfs_text_bytes),
            mb(self.vfs_line_index_bytes),
            self.summaries,
            mb(self.summary_bytes),
            self.cooked_files,
            mb(self.cooked_bytes),
            thousands(self.definitions),
            mb(self.definition_bytes),
            self.units,
            mb(self.unit_bytes),
            self.renderings,
            mb(self.rendering_bytes),
            self.views,
            mb(self.view_bytes),
            thousands(self.dead_macro_decisions),
            self.directive_signatures,
            self.include_searches,
            self.units_read,
            self.walk_cache_entries,
            mb(self.walk_cache_bytes),
            self.walks_by_content_entries,
            mb(self.walks_by_content_bytes),
            mb(self.shared_timeline_bytes),
        )?;
        write!(
            formatter,
            " | index tables {} ({} name(s) / {} name posting(s), {} scope(s) / {} scope posting(s), \
             {} edge(s) {}, {} member list(s) {}, {} memo(s) {})",
            mb(self.index_table_bytes()),
            thousands(self.index_names_entries),
            thousands(self.index_name_postings),
            thousands(self.index_scope_entries),
            thousands(self.index_scope_postings),
            self.index_edges,
            mb(self.index_edge_bytes),
            self.index_member_lists,
            mb(self.index_member_list_bytes),
            self.index_memo_entries,
            mb(self.index_memo_bytes),
        )
    }
}
