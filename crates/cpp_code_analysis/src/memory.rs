//! **What the analysis is holding, in bytes** — the number a report like "five gigabytes" needs before anything can
//! be called a leak.
//!
//! # Why this exists, and what it is not
//!
//! `docs/memory-audit.md` starts from a measured fact: an editor session on `EmmyLuaCodeStyle` reached **5 GB**. The
//! resident set says *that* it happened; it does not say **which table** grew, and a plan built on a total is a plan
//! that will move the total somewhere else. So every structure that can grow is asked for its own size, and
//! [`MemoryReport`] is the answer in one place.
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
    /// The one table in this report that is **unbounded**: one entry per distinct closure a walk has produced, never
    /// evicted. See `docs/memory-audit.md`; on the 1 273-file corpus it is the largest single holder after a cold
    /// start.
    pub walks_by_content_entries: usize,
    pub walks_by_content_bytes: usize,
}

impl MemoryReport {
    /// Everything the session's own tables hold — the sum of the fields above, without the walk cache, which is
    /// process-wide rather than the session's.
    pub fn total(&self) -> usize {
        self.vfs_text_bytes
            + self.vfs_line_index_bytes
            + self.summary_bytes
            + self.cooked_bytes
            + self.definition_bytes
            + self.unit_bytes
            + self.rendering_bytes
            + self.dead_macro_bytes
    }

    /// The same total including the tables that are not the session's own (the walk cache is process-wide) and the
    /// closure-dedup table.
    pub fn total_with_the_process(&self) -> usize {
        self.total() + self.walk_cache_bytes + self.walks_by_content_bytes
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
             units {} ({}) | renderings {} ({}) | bookkeeping {} dead-macro offsets, {} signatures, \
             {} searches, {} units-read | walk cache {} ({}) | walks by content {} ({})",
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
            thousands(self.dead_macro_decisions),
            self.directive_signatures,
            self.include_searches,
            self.units_read,
            self.walk_cache_entries,
            mb(self.walk_cache_bytes),
            self.walks_by_content_entries,
            mb(self.walks_by_content_bytes),
        )
    }
}
