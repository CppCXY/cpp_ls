//! Turning filesystem events into work: which of them the index cares about, and what each one implies.
//!
//! `docs/index-design.md` gives the watcher three jobs, and this module is all three of them and nothing else:
//!
//! ```text
//! filter      ignore what churns and what the index wrote itself
//! coalesce    many events in one path become one thing that happened
//! respond     say what the index must now do: forget, re-read, or start over
//! ```
//!
//! # What is deliberately not here
//!
//! **No OS watcher, no threads, and no clock.** A `notify`-style adapter produces [`FileEvent`]s and someone has
//! to decide when a burst of them is over; both of those are a shell around this module rather than part of it,
//! and the split is the same one [`crate::index::store`] makes about parsing: the decisions are testable on
//! strings, and the shell has nothing in it worth testing. Adding `notify` to the workspace is a dependency
//! decision this layer does not force.
//!
//! # Modified is the boring one, and that is the point
//!
//! A file whose text changed needs its summary rebuilt, and **nothing else does**. Its includers' summaries do not
//! depend on its contents: a summary is a function of a file's own text, its directory and the configuration —
//! which is what [`crate::SummaryKey`] records, and why the macro environment left the key. So an edited header is
//! one file to re-read, not the fifty that include it. (Their *queries* do change, and queries walk the live
//! index, so the new declarations are visible the moment the header's summary is replaced.)
//!
//! That is worth stating because the opposite is the intuitive answer, and the code that implements the intuitive
//! answer — [`SummaryStore::invalidate`] and its transitive walk — is still here and still right for the two cases
//! that do invalidate a summary without its text changing: a file **appearing** and a file **disappearing**. A
//! summary records where its includes *resolved*, and that is a fact about the filesystem:
//!
//! ```text
//! a.cpp:  #include "generated.h"     ->  resolved: None          (nothing there yet)
//! …      generated.h is created      ->  a.cpp's summary is now wrong, and its text never changed
//! ```
//!
//! A third case belongs with those two and is the one that catches people out: a stored summary is **only ever a
//! candidate**. Its key names the text, the configuration and the directory — everything except which files exist
//! — so a hit is re-checked against the filesystem before it is used. [`SummaryStore::get`] says why, and what
//! that costs.
//!
//! # Why a created file can be repaired exactly
//!
//! To know which files a creation affects, the index needs to know which files *looked* for that path and did not
//! find it. It does not store the candidate lists, and it does not need to: an include's spelling and the
//! directory it was written in determine every candidate it could have tried, and both are known — the spelling
//! is a fact in the summary, and the directories are the file's own plus the configuration's include paths. So the
//! question is answered by enumerating, from each file's side, the paths it would try — the resolver's own
//! `join_normalized` of a directory and a spelling — and looking each one up in the set of paths that appeared.
//! Enumerating from the file's side is also what makes a batch of a thousand creations one pass over the index
//! rather than a thousand.
//!
//! The same test answers the other half, which is the hole `docs/index-design.md` promises this layer closes: a
//! file created **earlier in the search order** than the one that resolution currently points at takes over. The
//! rank of a candidate is the rank the resolver itself computes, so "earlier" is the resolver's own order rather
//! than a guess about it.
//!
//! # This layer is promptness, not correctness
//!
//! Everything above describes what a watcher *should* do about an event — and none of it is what makes the index
//! right. A stored summary whose `resolved` values have gone stale is caught by [`SummaryStore::get`] itself,
//! which re-checks every include against the filesystem before it believes the entry. That check has to exist
//! here regardless, because a watcher can always miss an event: a queue overflows, a network filesystem reports
//! nothing, a file appears while the process was not running, an editor writes in a way nobody anticipated.
//!
//! What this layer buys on top of that is *when* the repair happens. Without it, a file that lost an include
//! keeps answering from a summary that describes the search as it used to be, until something asks about that file
//! again — which for a header the user is not looking at may be never. With it, the answer changes as soon as the
//! event does. That is a real difference for a diagnostics feature and no difference at all for correctness, and
//! keeping the two apart is what stops the watcher from becoming load-bearing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::include::config::CompilerConfig;
use crate::include::paths::{FileProvider, join_normalized, normalize_path};
use crate::index::store::SummaryStore;
use crate::index::ProjectIndex;
use crate::preprocess::directive::IncludeForm;
use crate::summary::{FileSummary, IncludeFact};

/// The name a compile database is conventionally found under, relative to the project root.
const COMPILE_DATABASE: &str = "compile_commands.json";

/// How many changed paths make a batch not worth asking about one at a time.
///
/// A branch switch arrives as thousands of events. Asking the project for its file list and running a
/// [`crate::index::Worklist`] over it is then *cheaper* than thousands of individual lookups — and not much
/// cheaper in correctness terms either way: the files that did not change are cache hits, which the measurement in
/// `examples/measure.rs` puts at a fraction of a millisecond.
const OVERWHELMING_BATCH: usize = 256;

/// What happened to one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// The path is there and was not, as far as this batch knows.
    Created,
    /// The path is there and its contents may have changed.
    Modified,
    /// The path is gone.
    Removed,
}

/// One filesystem event, as an OS adapter would report it.
///
/// A rename is two events — a removal and a creation — and deliberately so: they are the two things the index can
/// act on, and the pair is *free* to handle, because a summary is keyed by content. A file renamed and not
/// otherwise changed has the text the cache was built from, so its new path is a hit and nothing is re-parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEvent {
    pub path: PathBuf,
    pub kind: EventKind,
}

impl FileEvent {
    pub fn created(path: impl Into<PathBuf>) -> Self {
        FileEvent {
            path: path.into(),
            kind: EventKind::Created,
        }
    }

    pub fn modified(path: impl Into<PathBuf>) -> Self {
        FileEvent {
            path: path.into(),
            kind: EventKind::Modified,
        }
    }

    pub fn removed(path: impl Into<PathBuf>) -> Self {
        FileEvent {
            path: path.into(),
            kind: EventKind::Removed,
        }
    }
}

/// Which events a project cares about.
///
/// # What is filtered, and what that filtering is worth
///
/// Two things are excluded for correctness and both are about *this* program writing files:
///
/// * **the cache directory**, because the index writes summaries into it — an unfiltered watcher would take its
///   own writes as input and re-index them for ever;
/// * **`compile_commands.json`**, which is not a source file at all but the signal that the *configuration*
///   changed. See [`ChangeBatch::configuration_changed`].
///
/// Everything else here is an **optimisation**, and it is worth being precise about that: a path nothing includes
/// and the index has never held is a no-op whatever happens to it, so a deny-list of editor temporary files would
/// buy nothing. What the directory list buys is the *batch*: a `git checkout` produces thousands of events under
/// `.git/`, and filtering them where they arrive keeps a thousand no-ops from being sorted through later.
///
/// # What is deliberately not filtered
///
/// **File extensions.** A header is included by *name*, and `<vector>` has no extension at all — so an extension
/// allow-list would drop exactly the files an index most needs. The narrower question "does this file deserve a
/// summary?" is not answered by looking at its name: it is answered by whether the index already holds it, or
/// something includes it. See [`SummaryStore::respond`].
#[derive(Debug, Clone)]
pub struct WatchFilter {
    cache: PathBuf,
    configuration: PathBuf,
    ignored: Vec<PathBuf>,
}

impl WatchFilter {
    /// A filter for a project rooted at `root`.
    pub fn new(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();

        WatchFilter {
            cache: root.join(crate::CACHE_DIRECTORY),
            configuration: root.join(COMPILE_DATABASE),
            ignored: Vec::new(),
        }
    }

    /// Ignore everything under a directory — a build tree, a vendored dependency, a scratch directory.
    ///
    /// There is no way to guess these: `build`, `out`, `target` and `cmake-build-debug` are all conventions, and a
    /// project is entitled to call its build tree `tmp`. So the caller says, and this layer does not invent.
    pub fn ignore(mut self, directory: impl Into<PathBuf>) -> Self {
        self.ignored.push(directory.into());
        self
    }

    /// Where the compile database is, for a caller that read it from somewhere unusual.
    pub fn with_configuration(mut self, path: impl Into<PathBuf>) -> Self {
        self.configuration = path.into();
        self
    }

    /// Is this event one the index should not even look at?
    pub fn is_ignored(&self, path: &Path) -> bool {
        let path = normalize(path);

        if under(&path, &normalize(&self.cache)) {
            return true;
        }

        // `.git` anywhere in the path rather than only at the root: a checkout of a submodule, a worktree, or a
        // nested repository all churn the same way.
        if path.split('/').any(|segment| segment == ".git") {
            return true;
        }

        self.ignored
            .iter()
            .any(|directory| under(&path, &normalize(directory)))
    }

    /// Is this path the compile database?
    pub fn is_configuration(&self, path: &Path) -> bool {
        normalize(path) == normalize(&self.configuration)
    }
}

/// A batch of events, reduced to the paths the index has to think about.
///
/// The batch is where several events on one path become one thing that happened, and the rule is the conservative
/// one: **the last event decides whether the file is there, and any creation in the batch decides that it might be
/// new.** Both halves earn their keep on the shapes an editor actually produces:
///
/// ```text
/// remove + create   a save that writes a temporary file and renames it over the target
///                   -> the file is there, and it may be a file that did not exist before
/// create + modify   a save into a path that was missing -> the same conclusion
/// create + remove   a temporary file that came and went   -> it is not there
/// ```
///
/// # What this does not do
///
/// **It does not decide when the batch ends.** That is a clock question — an editor writes a file several times a
/// second, a `git checkout` is over in a burst — and a clock is exactly what this module does not have. A caller
/// with a timer pushes until it fires and responds once.
#[derive(Debug, Clone)]
pub struct ChangeBatch {
    filter: WatchFilter,
    /// The paths, and what happened to them, in the order they were first seen — so that a response is
    /// deterministic rather than `HashMap`-ordered.
    changes: Vec<(PathBuf, EventKind)>,
    position: HashMap<String, usize>,
    configuration: bool,
}

impl ChangeBatch {
    pub fn new(filter: WatchFilter) -> Self {
        ChangeBatch {
            filter,
            changes: Vec::new(),
            position: HashMap::new(),
            configuration: false,
        }
    }

    /// Add an event. Returns whether the index has to think about it.
    ///
    /// A `false` answer means the event is one of the ignored ones or the configuration — both of which are
    /// *reported* rather than swallowed, which is why the return value matters to a caller deciding whether to do
    /// anything at all.
    pub fn push(&mut self, event: FileEvent) -> bool {
        if self.filter.is_configuration(&event.path) {
            self.configuration = true;
            return false;
        }

        if self.filter.is_ignored(&event.path) {
            return false;
        }

        match self.position.get(&normalize(&event.path)) {
            Some(index) => {
                let (path, kind) = &mut self.changes[*index];
                *kind = merge(*kind, event.kind);
                // The new spelling wins: a rename reports the path the event was about, and that is the one the
                // caller will ask about next.
                *path = event.path;
            }
            None => {
                self.position
                    .insert(normalize(&event.path), self.changes.len());
                self.changes.push((event.path, event.kind));
            }
        }

        true
    }

    /// Every event of a batch, in one call. Returns how many of them the index has to think about.
    pub fn extend(&mut self, events: impl IntoIterator<Item = FileEvent>) -> usize {
        let mut kept = 0;
        for event in events {
            if self.push(event) {
                kept += 1;
            }
        }
        kept
    }

    /// The paths that changed, and what happened to each — one entry per path.
    pub fn changed(&self) -> impl Iterator<Item = (&Path, EventKind)> {
        self.changes
            .iter()
            .map(|(path, kind)| (path.as_path(), *kind))
    }

    pub fn len(&self) -> usize {
        self.changes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && !self.configuration
    }

    /// Did the compile database change?
    ///
    /// A separate answer from "these files changed", because it is a different *size* of change: the configuration
    /// is part of every summary's key, so a single `-I` added to the database makes every summary in the project a
    /// miss. Nothing is wrong with the stored summaries — they describe a compilation that is no longer the one
    /// being asked about — and a caller responds by rebuilding from its file list rather than by asking about the
    /// paths here.
    pub fn configuration_changed(&self) -> bool {
        self.configuration
    }
}

/// What a batch means for the index.
///
/// The two lists are not alternatives and the order between them matters: `forgotten` has to be applied before
/// anything is re-read, so that a query during the work does not find declarations in a file that is gone.
/// [`SummaryStore::respond`] applies it in that order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Response {
    /// Summaries dropped, because the files are gone.
    pub forgotten: Vec<PathBuf>,
    /// Files to read again, in the order to read them.
    pub reindex: Vec<PathBuf>,
    /// Start over instead: the configuration changed, or the batch was too large to be worth asking about one path
    /// at a time. `reindex` is empty when this is set, and the caller rebuilds from its own file list.
    pub everything: bool,
}

impl Response {
    /// Is there nothing to do?
    pub fn is_empty(&self) -> bool {
        self.forgotten.is_empty() && self.reindex.is_empty() && !self.everything
    }
}

impl<'a, F: FileProvider> SummaryStore<'a, F> {
    /// What the index has to do about a batch of events.
    ///
    /// Three answers, and the reasoning for each is the module documentation:
    ///
    /// * **a file changed** — re-read it, if the index holds it. A path the index has never held and that nothing
    ///   includes is not indexed here: that is the project scan's job, not an event's, and doing it here would mean
    ///   parsing every `.md` file the editor touches;
    /// * **a file is gone** — forget it, and re-read the files that resolved to it, because their summaries record
    ///   a search that would now fail;
    /// * **a file appeared** — re-read it if the index holds it, and re-read the files that *searched* for it and
    ///   did not find it, which is every file whose include would now resolve to it (including one whose include
    ///   resolved to something the new file outranks).
    pub fn respond(&mut self, batch: &ChangeBatch) -> Response {
        if batch.configuration_changed() {
            return Response {
                everything: true,
                ..Response::default()
            };
        }

        if batch.len() > OVERWHELMING_BATCH {
            return Response {
                everything: true,
                ..Response::default()
            };
        }

        let mut forgotten = Vec::new();
        let mut reindex: Vec<PathBuf> = Vec::new();
        let mut queued: HashSet<String> = HashSet::new();

        // The creations are collected first and answered in one pass over the index, because that pass costs the
        // same whether it is looking for one path or a thousand. See `naming_files`.
        let created: HashSet<String> = batch
            .changed()
            .filter(|(_, kind)| *kind == EventKind::Created)
            .map(|(path, _)| normalize(path))
            .collect();

        for (path, kind) in batch.changed() {
            match kind {
                EventKind::Removed => {
                    if self.forget(path) {
                        forgotten.push(path.to_path_buf());
                    }
                    // The files whose include resolved *to* this path: their summaries are now describing a search
                    // that would not happen the same way twice.
                    for includer in self.index().includers_of(path) {
                        push_once(&mut reindex, &mut queued, includer);
                    }
                }
                EventKind::Modified => {
                    if self.index().summary(path).is_some() {
                        push_once(&mut reindex, &mut queued, path.to_path_buf());
                    }
                }
                EventKind::Created => {
                    if self.index().summary(path).is_some() {
                        push_once(&mut reindex, &mut queued, path.to_path_buf());
                    }
                    for file in naming_files(self.index(), self.config(), &created) {
                        push_once(&mut reindex, &mut queued, file);
                    }
                }
            }
        }

        // A file that is gone is not read again. This happens when a header and one of the files that included it
        // are deleted in the same batch: the includer is named by the header's removal, and it is not there to be
        // read. Reading it would be a step whose only outcome is `Missing`.
        if !forgotten.is_empty() {
            let gone: std::collections::HashSet<String> =
                forgotten.iter().map(|path| normalize(path)).collect();
            reindex.retain(|path| !gone.contains(&normalize(path)));
        }

        Response {
            forgotten,
            reindex,
            everything: false,
        }
    }
}

/// Add a path to the work list if it is not already there.
fn push_once(work: &mut Vec<PathBuf>, queued: &mut std::collections::HashSet<String>, path: PathBuf) {
    if queued.insert(normalize(&path)) {
        work.push(path);
    }
}

/// The files whose includes could find one of the paths that just appeared.
///
/// The candidates are enumerated **from each file's side** — the directories it would search, joined with each of
/// its spellings — and looked up in the set of paths that appeared. Enumerating this way is what keeps a batch of
/// many creations one pass over the index instead of one pass *per created path*, which is the shape a branch
/// switch has.
fn naming_files(
    index: &ProjectIndex,
    config: &CompilerConfig,
    created: &std::collections::HashSet<String>,
) -> Vec<PathBuf> {
    if created.is_empty() {
        return Vec::new();
    }

    let mut found = Vec::new();

    for summary in index.summaries() {
        if names_one_of(summary, config, created) {
            found.push(summary.path.clone());
        }
    }

    found
}

/// Does this file write an include that would find one of these paths, where it did not find it before?
fn names_one_of(
    summary: &FileSummary,
    config: &CompilerConfig,
    created: &std::collections::HashSet<String>,
) -> bool {
    let including = summary.path.parent().unwrap_or(Path::new("."));

    summary.includes.iter().any(|include| {
        candidates_of(include, including, config)
            .into_iter()
            .any(|(candidate, rank)| {
                if !created.contains(&candidate) {
                    return false;
                }

                match &include.resolved {
                    // Nothing was found, so anything the resolver would try is an improvement.
                    None => true,
                    // Something was found: the new file has to be *earlier* in the search order to take over.
                    Some(resolved) => {
                        // A resolution whose directory cannot be ranked at all — which no search the resolver
                        // performs can produce — counts as "the new file wins", because an extra rebuild is the
                        // cheap mistake and a missed one is the expensive mistake.
                        let held = resolved
                            .parent()
                            .and_then(|directory| rank_of(directory, including, include.form, config));

                        held.is_none_or(|held| rank < held)
                    }
                }
            })
    })
}

/// Every path this include would try, in the order the resolver tries them, each with the resolver's rank.
///
/// Built with the resolver's own [`join_normalized`], so the candidates are the ones it would actually form
/// rather than a second opinion about how to join a directory and a spelling.
///
/// `#include_next` is the one approximation: the resolver starts *after* the directory the including file was
/// found in, and that origin is not stored, so this enumerates from the beginning. That can over-include — naming
/// a file as affected when the search would have skipped that candidate — which costs one rebuild, where the
/// opposite mistake would leave a summary stale. `#include_next` lives in wrapper headers around a system
/// installation, which is not what a project index is usually made of.
fn candidates_of(
    include: &IncludeFact,
    including: &Path,
    config: &CompilerConfig,
) -> Vec<(String, usize)> {
    let spelling = Path::new(&include.spelling);

    // An absolute spelling skips the search entirely, so there is one candidate and it is the spelling.
    if spelling.is_absolute() {
        return vec![(normalize(&join_normalized(Path::new(""), spelling, false)), 0)];
    }

    let mut candidates = Vec::new();

    // The local directory is searched only for a quoted include, which is the whole difference between the two
    // spellings — and it comes first, which is why a file created beside its includer takes over.
    if include.form == IncludeForm::Quote {
        candidates.push((
            normalize(&join_normalized(including, spelling, cfg!(windows))),
            0,
        ));
    }

    for (index, include_path) in config.include_paths.iter().enumerate() {
        let directory = config.resolve_against_working_directory(&include_path.directory);
        candidates.push((
            normalize(&join_normalized(&directory, spelling, cfg!(windows))),
            index + 1,
        ));
    }

    candidates
}

/// Where in an include's candidate list a directory is.
///
/// The numbers are the resolver's own: the including file's own directory first, then the configured paths in
/// order. Getting this wrong would not crash — it would rebuild a file that did not need it, or fail to rebuild
/// one that did, which is the failure this whole module exists to avoid.
fn rank_of(
    directory: &Path,
    including: &Path,
    form: IncludeForm,
    config: &CompilerConfig,
) -> Option<usize> {
    if form == IncludeForm::Quote && normalize(directory) == normalize(including) {
        return Some(0);
    }

    config
        .include_paths
        .iter()
        .position(|include_path| {
            let searched = config.resolve_against_working_directory(&include_path.directory);
            normalize(&searched) == normalize(directory)
        })
        .map(|index| index + 1)
}

/// How two events on one path become one.
///
/// See [`ChangeBatch`] for why a creation is remembered rather than overwritten: an editor's save is a removal and
/// a creation, and the file that comes back may be one that did not exist before.
fn merge(held: EventKind, arriving: EventKind) -> EventKind {
    if arriving == EventKind::Removed {
        return EventKind::Removed;
    }

    if held == EventKind::Created || arriving == EventKind::Created {
        return EventKind::Created;
    }

    EventKind::Modified
}

/// Is `path` the same as, or inside, `directory`?
fn under(path: &str, directory: &str) -> bool {
    path == directory
        || path
            .strip_prefix(directory)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// A path as this module compares them.
fn normalize(path: &Path) -> String {
    // Borrowed from the index rather than invented: a filter that disagrees with the index about when two paths
    // are one path would filter the wrong events, or miss its own writes.
    normalize_path(path, cfg!(windows))
}

#[cfg(test)]
mod tests {
    use super::{ChangeBatch, EventKind, FileEvent, WatchFilter};
    use crate::include::config::CompilerConfig;
    use crate::include::paths::{DiskFiles, MemoryFiles};
    use crate::index::store::SummaryStore;
    use std::path::{Path, PathBuf};

    /// A batch over a filter with the cache and `.git` rules, and nothing else.
    fn batch(root: &str) -> ChangeBatch {
        ChangeBatch::new(WatchFilter::new(root))
    }

    fn kinds(batch: &ChangeBatch) -> Vec<(String, EventKind)> {
        batch
            .changed()
            .map(|(path, kind)| (path.to_string_lossy().replace('\\', "/"), kind))
            .collect()
    }

    // -------------------------------------------------------------------------------------------
    // Filtering
    // -------------------------------------------------------------------------------------------

    #[test]
    fn the_cache_directory_is_ignored() {
        // Not an optimisation: the index writes summaries there, so an unfiltered watcher would take its own
        // writes as input and re-index them for ever.
        let mut batch = batch("/p");

        assert!(!batch.push(FileEvent::created("/p/.cppls/summaries/ab/abcdef.bin")));
        assert!(!batch.push(FileEvent::modified("/p/.cppls/summaries/ab/abcdef.bin.tmp")));
        assert!(batch.is_empty());
    }

    #[test]
    fn a_path_that_merely_starts_like_the_cache_is_not_ignored() {
        // The boundary the rule above is written with a separator for: `.cpplsfoo` is a directory somebody may
        // well have, and filtering it would silently drop a project's files.
        let mut batch = batch("/p");

        assert!(batch.push(FileEvent::modified("/p/.cpplsfoo/widget.h")));
        assert_eq!(batch.len(), 1);
    }

    #[test]
    fn git_internals_are_ignored_wherever_they_are() {
        let mut batch = batch("/p");

        assert!(!batch.push(FileEvent::modified("/p/.git/index.lock")));
        assert!(!batch.push(FileEvent::created("/p/sub/.git/objects/ab/cdef")));
        assert!(!batch.push(FileEvent::modified("/p/.git")));
        assert!(batch.is_empty());
    }

    #[test]
    fn a_directory_the_caller_names_is_ignored() {
        // There is no way to guess this one: a project is entitled to call its build tree `tmp`.
        let mut batch = ChangeBatch::new(WatchFilter::new("/p").ignore("/p/build"));

        assert!(!batch.push(FileEvent::modified("/p/build/generated.h")));
        assert!(batch.push(FileEvent::modified("/p/src/generated.h")));
    }

    #[test]
    fn a_file_with_no_extension_is_not_ignored() {
        // The reason there is no extension allow-list: `#include <vector>` has no extension, so a list of "C++
        // extensions" would drop exactly the files an index most needs.
        let mut batch = batch("/p");

        assert!(batch.push(FileEvent::modified("/usr/include/vector")));
    }

    #[test]
    fn the_compile_database_is_reported_rather_than_indexed() {
        let mut batch = batch("/p");

        assert!(
            !batch.push(FileEvent::modified("/p/compile_commands.json")),
            "it is not a source file and must not be queued as one"
        );
        assert!(batch.configuration_changed());
        assert_eq!(batch.len(), 0);
        assert!(
            !batch.is_empty(),
            "a configuration change is not nothing, even with no paths in the batch"
        );
    }

    // -------------------------------------------------------------------------------------------
    // Coalescing
    // -------------------------------------------------------------------------------------------

    #[test]
    fn several_events_on_one_path_become_one() {
        let mut batch = batch("/p");

        for _ in 0..5 {
            batch.push(FileEvent::modified("/p/widget.h"));
        }

        assert_eq!(
            kinds(&batch),
            [("/p/widget.h".to_string(), EventKind::Modified)]
        );
    }

    #[test]
    fn a_save_that_replaces_a_file_is_a_creation() {
        // The shape an editor produces: write a temporary file, rename it over the target. The file that comes
        // back may be one that did not exist before, so the batch has to remember the creation.
        let mut batch = batch("/p");

        batch.push(FileEvent::removed("/p/widget.h"));
        batch.push(FileEvent::created("/p/widget.h"));

        assert_eq!(
            kinds(&batch),
            [("/p/widget.h".to_string(), EventKind::Created)]
        );
    }

    #[test]
    fn a_temporary_file_that_comes_and_goes_is_gone() {
        let mut batch = batch("/p");

        batch.push(FileEvent::created("/p/widget.h"));
        batch.push(FileEvent::removed("/p/widget.h"));

        assert_eq!(
            kinds(&batch),
            [("/p/widget.h".to_string(), EventKind::Removed)]
        );
    }

    #[test]
    fn a_creation_anywhere_in_the_batch_upgrades_a_modification() {
        let mut batch = batch("/p");

        batch.push(FileEvent::created("/p/new.h"));
        batch.push(FileEvent::modified("/p/new.h"));

        assert_eq!(kinds(&batch), [("/p/new.h".to_string(), EventKind::Created)]);
    }

    #[test]
    fn a_batch_keeps_the_order_it_first_saw_paths_in() {
        // So that a response is deterministic: two runs over the same events do the same work in the same order.
        let mut batch = batch("/p");

        batch.push(FileEvent::modified("/p/b.h"));
        batch.push(FileEvent::modified("/p/a.h"));
        batch.push(FileEvent::modified("/p/b.h"));

        assert_eq!(
            kinds(&batch),
            [
                ("/p/b.h".to_string(), EventKind::Modified),
                ("/p/a.h".to_string(), EventKind::Modified),
            ]
        );
    }

    // -------------------------------------------------------------------------------------------
    // Responding, with files in memory
    //
    // `respond` reads the index and the configuration and touches no filesystem, so these tests are about *which*
    // paths it names. What happens when those paths are actually read is the next section's business.
    // -------------------------------------------------------------------------------------------

    fn memory_store<'a>(
        name: &str,
        files: &'a MemoryFiles,
    ) -> (SummaryStore<'a, MemoryFiles>, PathBuf) {
        let root = std::env::temp_dir().join("cppls-watch-tests").join(name);
        let _ = std::fs::remove_dir_all(&root);

        let store = SummaryStore::with_provider(&root, CompilerConfig::default(), files);
        (store, root)
    }

    /// How a test spells a path, normalized the way a response spells one.
    fn shown(path: &Path) -> String {
        super::normalize(path)
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths.iter().map(|path| shown(path)).collect()
    }

    #[test]
    fn a_modified_header_is_read_again_and_nothing_else_is() {
        // The claim the whole module is built on: a summary does not depend on the contents of what it includes,
        // so an edited header is one file to read, not the fifty that include it. The intuitive answer — walk the
        // reverse edges and rebuild everything below — is what `SummaryStore::invalidate` does, and it is wrong
        // here (it costs fifty reads to learn that nothing changed).
        let files = MemoryFiles::new()
            .with_file("/p/widget.h", "struct Widget { int size; };\n")
            .with_file("/p/a.cpp", "#include \"widget.h\"\n")
            .with_file("/p/b.cpp", "#include \"widget.h\"\n")
            .with_file("/p/c.cpp", "#include \"widget.h\"\n");
        let (mut store, root) = memory_store("modified", &files);

        for path in ["/p/widget.h", "/p/a.cpp", "/p/b.cpp", "/p/c.cpp"] {
            store.get(Path::new(path)).expect("the file reads");
        }

        let mut events = batch("/p");
        events.push(FileEvent::modified("/p/widget.h"));
        let response = store.respond(&events);

        assert_eq!(names(&response.reindex), ["/p/widget.h"]);
        assert!(response.forgotten.is_empty());
        assert!(!response.everything);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_modified_file_the_index_has_never_held_is_not_indexed() {
        // An event is not a project scan. Indexing whatever the editor touches would mean parsing every `.md` and
        // every `.json` in the tree, so a path the index does not hold and nothing includes is a no-op — and a
        // newly written `.cpp` is picked up by the file list, which is the caller's business.
        let files = MemoryFiles::new().with_file("/p/known.cpp", "int x;\n");
        let (mut store, root) = memory_store("unknown-path", &files);
        store.get(Path::new("/p/known.cpp")).expect("the file reads");

        let mut events = batch("/p");
        events.push(FileEvent::modified("/p/README.md"));
        events.push(FileEvent::modified("/p/new.cpp"));
        let response = store.respond(&events);

        assert!(response.is_empty(), "{response:?}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_removed_file_is_forgotten_and_its_includers_are_read_again() {
        let files = MemoryFiles::new()
            .with_file("/p/widget.h", "struct Widget { int size; };\n")
            .with_file("/p/main.cpp", "#include \"widget.h\"\nvoid f() { Widget w; }\n");
        let (mut store, root) = memory_store("removed", &files);

        for path in ["/p/widget.h", "/p/main.cpp"] {
            store.get(Path::new(path)).expect("the file reads");
        }
        assert!(matches!(
            store.index().definition("Widget", Path::new("/p/main.cpp")),
            crate::Known::Yes(_)
        ));

        let mut events = batch("/p");
        events.push(FileEvent::removed("/p/widget.h"));
        let response = store.respond(&events);

        assert_eq!(names(&response.forgotten), ["/p/widget.h"]);
        assert_eq!(
            names(&response.reindex),
            ["/p/main.cpp"],
            "the summary of the file that resolved to it records a search that would now fail"
        );
        assert!(
            matches!(
                store.index().definition("Widget", Path::new("/p/main.cpp")),
                crate::Known::Unknown(_)
            ),
            "and the declaration in the deleted file stops being found before anything is re-read"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_removed_file_nothing_included_is_only_forgotten() {
        let files = MemoryFiles::new().with_file("/p/alone.cpp", "int x;\n");
        let (mut store, root) = memory_store("removed-alone", &files);
        store.get(Path::new("/p/alone.cpp")).expect("the file reads");

        let mut events = batch("/p");
        events.push(FileEvent::removed("/p/alone.cpp"));
        let response = store.respond(&events);

        assert_eq!(names(&response.forgotten), ["/p/alone.cpp"]);
        assert!(response.reindex.is_empty(), "nothing resolved to it");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_created_file_that_nothing_searches_for_changes_nothing() {
        // The rule is exact rather than "any creation rebuilds the project": the created path has to be one a
        // file's include could have been reaching for, which is a question about spellings and search directories.
        let files = MemoryFiles::new()
            .with_file("/p/main.cpp", "#include \"generated.h\"\n")
            .with_file("/p/other.cpp", "#include \"other.h\"\n");
        let (mut store, root) = memory_store("created-irrelevant", &files);

        for path in ["/p/main.cpp", "/p/other.cpp"] {
            store.get(Path::new(path)).expect("the file reads");
        }

        let mut events = batch("/p");
        events.push(FileEvent::created("/p/unrelated.h"));
        events.push(FileEvent::created("/p/deep/nested/generated.h"));
        let response = store.respond(&events);

        assert!(
            response.is_empty(),
            "neither creation is a candidate any include was searching: {response:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_created_file_repairs_the_include_that_was_waiting_for_it() {
        let files = MemoryFiles::new()
            .with_file("/p/main.cpp", "#include \"local/generated.h\"\nvoid f() { }\n")
            .with_file("/p/local/other.h", "int other;\n");
        let (mut store, root) = memory_store("created-relevant", &files);

        store.get(Path::new("/p/main.cpp")).expect("the file reads");
        assert_eq!(
            store.stats().unstored,
            1,
            "the include did not resolve, so the summary was not stored"
        );

        let mut events = batch("/p");
        events.push(FileEvent::created("/p/local/generated.h"));
        let response = store.respond(&events);

        assert_eq!(
            names(&response.reindex),
            ["/p/main.cpp"],
            "the file that wrote that include is the file whose summary is wrong"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_created_file_that_outranks_the_current_resolution_wins() {
        // The hole a key computed from the text cannot see: `#include "x.h"` resolved to a configured directory
        // until somebody created the one beside the file, and the resolver's own ranking is what says the local
        // one is now earlier.
        let files = MemoryFiles::new()
            .with_file("/inc/x.h", "struct FromInclude { int x; };\n")
            .with_file("/p/a.cpp", "#include \"x.h\"\nvoid f() { }\n");
        let config = CompilerConfig::default().with_include_path("/inc");
        let root = std::env::temp_dir().join("cppls-watch-tests").join("outranks");
        let _ = std::fs::remove_dir_all(&root);

        let mut store = SummaryStore::with_provider(&root, config, &files);
        let held = store.get(Path::new("/p/a.cpp")).expect("the file reads").clone();
        assert_eq!(
            held.includes[0].resolved.as_deref(),
            Some(Path::new("/inc/x.h"))
        );

        let mut events = batch("/p");
        events.push(FileEvent::created("/p/x.h"));
        let response = store.respond(&events);
        assert_eq!(names(&response.reindex), ["/p/a.cpp"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_created_file_that_loses_to_the_current_resolution_changes_nothing() {
        // The other half of the ranking, and the one that keeps the rule from rebuilding a project on every file
        // creation: a file created in a directory that is searched *later* than the one that resolved cannot take
        // over.
        let files = MemoryFiles::new()
            .with_file("/first/x.h", "struct First { int x; };\n")
            .with_file("/p/a.cpp", "#include \"x.h\"\nvoid f() { }\n");
        let config = CompilerConfig::default()
            .with_include_path("/first")
            .with_include_path("/second");
        let root = std::env::temp_dir().join("cppls-watch-tests").join("loses");
        let _ = std::fs::remove_dir_all(&root);

        let mut store = SummaryStore::with_provider(&root, config, &files);
        store.get(Path::new("/p/a.cpp")).expect("the file reads");

        let mut events = batch("/p");
        events.push(FileEvent::created("/second/x.h"));
        let response = store.respond(&events);

        assert!(
            response.is_empty(),
            "the search already stopped at /first: {response:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_named_by_two_rules_at_once_is_read_once() {
        // `main.cpp` writes one include that resolved and one that did not. Removing the first names `main.cpp` as
        // an includer; creating the second names it as a file whose search would now be answered. Two rules, one
        // file, one entry.
        let files = MemoryFiles::new()
            .with_file("/p/a.h", "int a;\n")
            .with_file("/p/main.cpp", "#include \"a.h\"\n#include \"b.h\"\n");
        let (mut store, root) = memory_store("dedupe", &files);

        store.get(Path::new("/p/a.h")).expect("the file reads");
        store.get(Path::new("/p/main.cpp")).expect("the file reads");

        let mut events = batch("/p");
        events.push(FileEvent::removed("/p/a.h"));
        events.push(FileEvent::created("/p/b.h"));
        let response = store.respond(&events);

        assert_eq!(names(&response.reindex), ["/p/main.cpp"]);
        assert_eq!(names(&response.forgotten), ["/p/a.h"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_that_is_gone_is_not_read_again() {
        // A header and one of the files that included it, both deleted. The removal of the header names the
        // includer, and the includer is not there: a step that could only come back `Missing` is not work.
        let files = MemoryFiles::new()
            .with_file("/p/widget.h", "struct Widget { int size; };\n")
            .with_file("/p/main.cpp", "#include \"widget.h\"\n");
        let (mut store, root) = memory_store("gone-includer", &files);

        for path in ["/p/widget.h", "/p/main.cpp"] {
            store.get(Path::new(path)).expect("the file reads");
        }

        let mut events = batch("/p");
        events.push(FileEvent::removed("/p/widget.h"));
        events.push(FileEvent::removed("/p/main.cpp"));
        let response = store.respond(&events);

        assert_eq!(
            names(&response.forgotten),
            ["/p/widget.h", "/p/main.cpp"]
        );
        assert!(response.reindex.is_empty(), "{response:?}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_angle_include_does_not_search_the_files_own_directory() {
        // The repair rule follows the resolver, including the rule that makes the two spellings different: an angle
        // include does not look beside the file, so a header created there does *not* answer it. Getting this wrong
        // would rebuild a file on every creation in its directory for nothing.
        let files = MemoryFiles::new()
            .with_file("/p/main.cpp", "#include <generated.h>\n")
            .with_file("/p/other.cpp", "#include \"generated.h\"\n");
        let (mut store, root) = memory_store("angle-form", &files);

        for path in ["/p/main.cpp", "/p/other.cpp"] {
            store.get(Path::new(path)).expect("the file reads");
        }

        let mut events = batch("/p");
        events.push(FileEvent::created("/p/generated.h"));
        let response = store.respond(&events);

        assert_eq!(
            names(&response.reindex),
            ["/p/other.cpp"],
            "only the quoted include was looking beside itself"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_batch_of_creations_is_answered_in_one_pass() {
        // The scan runs once over the index for the whole set of created paths, so a file waiting on the *first* of
        // them is found as reliably as one waiting on the last — and a branch switch, which arrives as a batch of
        // creations, does not cost one scan per created path.
        let files = MemoryFiles::new()
            .with_file("/p/first.cpp", "#include \"one.h\"\n")
            .with_file("/p/last.cpp", "#include \"two.h\"\n")
            .with_file("/p/quiet.cpp", "#include \"three.h\"\n");
        let (mut store, root) = memory_store("batch-of-creations", &files);

        for path in ["/p/first.cpp", "/p/last.cpp", "/p/quiet.cpp"] {
            store.get(Path::new(path)).expect("the file reads");
        }

        let mut events = batch("/p");
        events.push(FileEvent::created("/p/one.h"));
        for index in 0..200 {
            events.push(FileEvent::created(format!("/p/unrelated{index}.h")));
        }
        events.push(FileEvent::created("/p/two.h"));
        let response = store.respond(&events);

        assert_eq!(
            names(&response.reindex),
            ["/p/first.cpp", "/p/last.cpp"],
            "both ends of the batch are considered, and three.h is still missing"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_configuration_change_is_everything() {
        let files = MemoryFiles::new().with_file("/p/a.cpp", "int x;\n");
        let (mut store, root) = memory_store("config-change", &files);
        store.get(Path::new("/p/a.cpp")).expect("the file reads");

        let mut events = batch("/p");
        events.push(FileEvent::modified("/p/compile_commands.json"));
        events.push(FileEvent::modified("/p/a.cpp"));
        let response = store.respond(&events);

        assert!(response.everything);
        assert!(
            response.reindex.is_empty(),
            "the caller rebuilds from its file list: the paths in this batch are not the interesting set"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_batch_too_large_to_ask_about_one_path_at_a_time_is_everything() {
        // A branch switch arrives as thousands of events. Asking the project for its file list is then cheaper
        // than thousands of individual lookups — and no less correct, since the files that did not change are
        // cache hits.
        let files = MemoryFiles::new();
        let (mut store, root) = memory_store("overwhelming", &files);

        let mut events = batch("/p");
        for index in 0..super::OVERWHELMING_BATCH + 1 {
            events.push(FileEvent::modified(format!("/p/file{index}.cpp")));
        }
        let response = store.respond(&events);

        assert!(response.everything);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_checkout_inside_git_does_nothing_at_all() {
        let files = MemoryFiles::new();
        let (mut store, root) = memory_store("git-churn", &files);

        let mut events = batch("/p");
        for path in [
            "/p/.git/index.lock",
            "/p/.git/HEAD",
            "/p/.git/objects/ab/cdef0123",
            "/p/.cppls/summaries/ab/abcdef.bin",
        ] {
            events.push(FileEvent::modified(path));
        }
        let response = store.respond(&events);

        assert!(events.is_empty(), "the batch kept nothing");
        assert!(response.is_empty(), "{response:?}");

        let _ = std::fs::remove_dir_all(&root);
    }

    // -------------------------------------------------------------------------------------------
    // Responding, with files on disk
    //
    // A watcher's central case is a file *appearing*, and `MemoryFiles` borrows immutably — it cannot express
    // that. So these run against the real provider on a real (temporary) project, which is also the provider the
    // watcher will actually drive.
    // -------------------------------------------------------------------------------------------

    /// A project on disk, removed when the test ends — including when it fails.
    struct Project {
        root: PathBuf,
    }

    impl Project {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join("cppls-watch-disk").join(name);
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("the fixture directory");

            Project { root }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.root.join(name)
        }

        fn write(&self, name: &str, text: &str) -> PathBuf {
            let path = self.path(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("the fixture directory");
            }
            std::fs::write(&path, text).expect("the fixture writes");
            path
        }

        fn remove(&self, name: &str) {
            std::fs::remove_file(self.path(name)).expect("the fixture is removed");
        }

        fn store(&self, config: CompilerConfig) -> SummaryStore<'static, DiskFiles> {
            SummaryStore::open(&self.root, config)
        }

        /// The batch a caller would build from this directory's events.
        fn batch(&self) -> ChangeBatch {
            ChangeBatch::new(WatchFilter::new(&self.root))
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Do the work a response asks for, the way a caller would: the named files are the seeds of a work list, and
    /// the work list follows their includes from there.
    fn work(store: &mut SummaryStore<'_, DiskFiles>, seeds: &[PathBuf]) {
        let mut list = store.worklist(seeds.to_vec(), Vec::new());
        while list.step().is_some() {}
    }

    #[test]
    fn a_header_that_appears_is_found_by_the_file_that_was_waiting_for_it() {
        // The end-to-end shape of the whole module: a file that says `#include "generated.h"` while there is no
        // such file, then the file is written, and after the response the declaration in it is reachable.
        let project = Project::new("appears");
        let main = project.write("main.cpp", "#include \"generated.h\"\nvoid f() { }\n");

        let mut store = project.store(CompilerConfig::default());
        let held = store.get(&main).expect("the file reads").clone();
        assert_eq!(held.includes[0].resolved, None);
        assert_eq!(
            store.stats().unstored,
            1,
            "and the summary was deliberately not stored, which is what makes this recoverable"
        );

        let generated = project.write("generated.h", "struct Generated { int x; };\n");

        let mut events = project.batch();
        events.push(FileEvent::created(&generated));
        let response = store.respond(&events);
        assert_eq!(names(&response.reindex), [shown(&main)]);

        // The work the response implies, done the way the three layers compose: the response names the files whose
        // summaries are wrong, and a work list started from them follows the includes — which is how the header
        // that just appeared gets a summary of its own.
        work(&mut store, &response.reindex);

        let rebuilt = store.index().summary(&main).expect("held").clone();
        assert_eq!(
            rebuilt.includes[0].resolved.as_deref(),
            Some(Path::new(&shown(&generated))),
            "the include resolves now, and the summary says where to"
        );
        assert!(
            store.index().summary(&generated).is_some(),
            "and the file it resolves to was indexed by following the include"
        );
        assert!(
            matches!(
                store.index().definition("Generated", &main),
                crate::Known::Yes(_)
            ),
            "so the declaration in it is reachable"
        );
    }

    #[test]
    fn a_header_that_is_deleted_stops_being_found() {
        let project = Project::new("deleted");
        project.write("widget.h", "struct Widget { int size; };\n");
        let main = project.write("main.cpp", "#include \"widget.h\"\nvoid f() { Widget w; }\n");

        let mut store = project.store(CompilerConfig::default());
        store.get(&project.path("widget.h")).expect("the file reads");
        store.get(&main).expect("the file reads");
        assert!(matches!(
            store.index().definition("Widget", &main),
            crate::Known::Yes(_)
        ));

        project.remove("widget.h");

        let mut events = project.batch();
        events.push(FileEvent::removed(project.path("widget.h")));
        let response = store.respond(&events);

        assert_eq!(
            names(&response.forgotten),
            [shown(&project.path("widget.h"))]
        );
        assert_eq!(names(&response.reindex), [shown(&main)]);

        work(&mut store, &response.reindex);

        let rebuilt = store.index().summary(&main).expect("held").clone();
        assert_eq!(
            rebuilt.includes[0].resolved, None,
            "the search fails now, and the summary records the attempt rather than the old answer"
        );
        assert!(
            matches!(
                store.index().definition("Widget", &main),
                crate::Known::Unknown(_)
            ),
            "so the declaration is no longer found"
        );
    }

    #[test]
    fn a_file_renamed_without_being_changed_is_a_cache_hit() {
        // What a content-addressed key buys a rename: nothing has to be re-parsed, because the text is the text.
        //
        // The test also pins a **boundary**: the response does not name the new path, and that is deliberate. The
        // watcher never adds a file to the index except when an include proves it belongs there — a renamed
        // translation unit is the file list's business, and a renamed header is its includers' business (they
        // re-resolve and pull it in by name). Whoever asks, the cache is what makes asking free.
        let project = Project::new("rename");
        let before = project.write("old_name.h", "struct Renamed { int x; };\n");

        let mut store = project.store(CompilerConfig::default());
        store.get(&before).expect("the file reads");
        assert_eq!(store.stats().rebuilt, 1);

        let after = project.write("new_name.h", "struct Renamed { int x; };\n");
        project.remove("old_name.h");

        let mut events = project.batch();
        events.push(FileEvent::removed(&before));
        events.push(FileEvent::created(&after));
        let response = store.respond(&events);

        assert_eq!(names(&response.forgotten), [shown(&before)]);
        assert!(
            response.reindex.is_empty(),
            "nothing in the index names the new path, so nothing is read: {response:?}"
        );

        // The file list catches up, asks about the new path, and pays nothing for it — same text, same
        // directory, therefore the same key as the file that was just forgotten.
        store.get(&after).expect("the file reads");

        assert_eq!(
            store.stats().rebuilt,
            1,
            "the same text under a new name was found, not parsed again"
        );
        assert_eq!(store.stats().reused, 1);
        assert!(
            matches!(
                store.index().definition("Renamed", &after),
                crate::Known::Yes(_)
            ),
            "and the renamed file answers for itself"
        );
    }
}
