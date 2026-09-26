//! The order to index a project in, so that the answer a user is waiting for arrives first.
//!
//! [`SummaryStore`] indexes one file when it is asked about it. Nothing so far decides *which* file to ask about
//! next, and on a cold cache that decision is the whole difference between a language server that answers a
//! go-to-definition after one file and one that answers it after ten thousand. `docs/index-design.md` fixes the
//! order:
//!
//! ```text
//! 1. the files the user is looking at — the editor's open documents
//! 2. everything those files include, transitively — what a question about them needs
//! 3. the rest of the project, and everything *it* includes
//! ```
//!
//! # Why this cannot be a list
//!
//! The obvious shape would be a function returning `Vec<PathBuf>` in the right order, and it is impossible: the
//! includes of a file the index has never seen are **not knowable** before that file is indexed. The list has to
//! grow as the work is done — each summary names the files it includes, and those names are what the next round is
//! made of. So this is a driver with a `step`, not a plan with a `len`, and that is also what makes it usable from
//! an editor: one `step` is one file, so a caller can do one per idle tick, a background thread can loop until
//! [`Worklist::step`] returns `None`, and a caller with a budget can simply stop — the list keeps its state, and
//! every query about a file that has not been reached yet answers `Unknown` rather than waiting.
//!
//! # Why two queues rather than one
//!
//! Both halves do the same thing — breadth-first from their seeds, following includes — so one queue seeded with
//! the open files and then the project would look simpler. It is wrong, and the failure is silent: with one queue,
//! a project list of ten thousand files is *in front of* the includes of the one file the user has open, so the
//! closure that phase 2 exists to produce arrives last. Two queues are what keeps the phases honest, and a test
//! with a longer project list than the closure pins it.
//!
//! # Why the project half follows includes too
//!
//! Because the caller's list is usually only the translation units. A `compile_commands.json` names the `.cpp`
//! files and says nothing about headers, so with no discovery the index would contain no headers at all except
//! those an open file happens to reach. Following includes from the project roots is what makes a
//! compile-database-driven index contain anything to navigate into.
//!
//! The consequence is worth stating plainly: **the work list is the transitive closure of what it is given.**
//! A root that includes `<vector>` on a machine with system include paths will pull in those headers, and the
//! caller bounds that by bounding the seeds (or by stopping), not by a rule here — the crate has no opinion about
//! which directories are worth indexing.
//!
//! # What a step's outcome is, and where it comes from
//!
//! [`SummaryStore::stats`] counts cumulatively, so a step's outcome is read from how far the counters moved while
//! it ran. That is why [`crate::index::StoreStats`] is `Copy` and comparable: it is the store's own account of
//! what it did, and a second copy of it here would be a second thing to keep in step.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::include::paths::{FileProvider, normalize_path};
use crate::index::store::{StoreStats, SummaryStore};

/// The order to work a project in, growing as it goes. See the module documentation.
pub struct Worklist<'s, F: FileProvider> {
    store: &'s mut SummaryStore<F>,
    /// The open files and everything they reach, breadth-first. Worked before [`Worklist::tail`] entirely.
    open: VecDeque<(PathBuf, usize)>,
    /// The caller's project list and everything it reaches. See the module documentation for why this is a second
    /// queue rather than more of the first.
    tail: VecDeque<(PathBuf, usize)>,
    /// Every path either queue has ever held, by normalized spelling, so that a diamond include graph or a cycle
    /// works a file once.
    seen: HashSet<String>,
}

impl<'s, F: FileProvider> Worklist<'s, F> {
    /// A work list over the files the user is looking at and the rest of the project.
    ///
    /// Neither list has to be sorted, complete, or free of duplicates. A path in both is worked **once**, in the
    /// open half — which is the answer to "I have the file open *and* it is in the project": it is the file the
    /// user is looking at, and that is the reason it goes first.
    pub fn new(
        store: &'s mut SummaryStore<F>,
        open: impl IntoIterator<Item = PathBuf>,
        project: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        let mut seen = HashSet::new();
        let mut queue = |paths: Vec<PathBuf>| -> VecDeque<(PathBuf, usize)> {
            paths
                .into_iter()
                .filter(|path| seen.insert(key(path)))
                .map(|path| (path, 0))
                .collect()
        };

        let open = queue(open.into_iter().collect());
        let tail = queue(project.into_iter().collect());

        Worklist {
            store,
            open,
            tail,
            seen,
        }
    }

    /// Index the next file in the order, and say what happened.
    ///
    /// `None` when the list is finished. A file that cannot be read is a step like any other: it is reported as
    /// [`StepOutcome::Missing`] and the list moves on, because one deleted file in a project is not a reason to
    /// stop indexing the rest of it.
    ///
    /// A file already worked by this list is never worked twice, however many paths reach it — a header included
    /// by twenty files, or a cycle, both cost one step each.
    pub fn step(&mut self) -> Option<Step> {
        let (path, priority, depth) = match self.open.pop_front() {
            Some((path, depth)) => (path, Priority::Open, depth),
            None => {
                let (path, depth) = self.tail.pop_front()?;
                (path, Priority::Rest, depth)
            }
        };

        let before = self.store.stats();
        let includes = self
            .store
            .get(&path)
            .map(|summary| {
                summary
                    .includes
                    .iter()
                    .filter_map(|include| include.resolved.clone())
                    .collect::<Vec<PathBuf>>()
            })
            .unwrap_or_default();
        let after = self.store.stats();

        let outcome = outcome_of(before, after);

        // Everything this file includes joins the same half of the list the file came from, one level further out.
        // An include that resolved to nothing contributes nothing: there is no file to work.
        let next: Vec<(PathBuf, usize)> = includes
            .into_iter()
            .filter(|include| self.seen.insert(key(include)))
            .map(|include| (include, depth + 1))
            .collect();

        match priority {
            Priority::Open => self.open.extend(next),
            Priority::Rest => self.tail.extend(next),
        }

        Some(Step {
            path,
            priority,
            depth,
            outcome,
        })
    }

    /// How many files are queued and not yet worked.
    ///
    /// It **grows** as the list runs, which is not a bug to hide from a progress bar: the number of files a
    /// project needs is a fact about the project that nobody knows until its includes have been read. A caller
    /// showing progress shows this number, or a count of steps taken, and not a fraction of a total it cannot
    /// have.
    pub fn pending(&self) -> usize {
        self.open.len() + self.tail.len()
    }

    /// Is there nothing left to work?
    pub fn is_empty(&self) -> bool {
        self.pending() == 0
    }

    /// How many distinct files the list has ever held.
    pub fn reached(&self) -> usize {
        self.seen.len()
    }
}

/// Which half of the order a step belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// A file the caller said the user is looking at, or something one of those includes. The reason this half
    /// exists: a question about an open file should not wait behind the rest of the project.
    Open,
    /// The rest of the project the caller listed, or something one of those includes.
    Rest,
}

/// One file's turn in the work list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The file that was worked, as the list spelled it — a caller's own path for a seed, and the resolved include
    /// for a file that was discovered.
    pub path: PathBuf,
    pub priority: Priority,
    /// How many includes away from a seed this file was: `0` for a file the caller listed, `1` for one it
    /// includes, and so on.
    pub depth: usize,
    pub outcome: StepOutcome,
}

/// What one step did with its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// The disk had the summary, so nothing was parsed.
    Reused,
    /// The file was read and summarised, and the summary was stored.
    Built,
    /// The file was summarised and deliberately not stored, because it writes an `#include` that does not
    /// resolve — a fact about the filesystem that a key computed from the text cannot check. See
    /// [`crate::index::store`]. Implies that the file *was* built.
    Unstored,
    /// The file could not be read: deleted, or not a file. The list moves on; a caller that wants it retried can
    /// build a new list, which is the only cheap way to ask again.
    Missing,
}

/// A path as [`Worklist`] compares them — the same normalization the store and the index use.
fn key(path: &Path) -> String {
    normalize_path(path, cfg!(windows))
}

/// What one file's turn did, read from the store's counters.
///
/// The counters are cumulative and comparable on purpose: the difference they moved by *is* a step's outcome, and
/// asking the store for it separately would be a second account of the same fact. The three arms of
/// [`SummaryStore::get`] each move at least one counter — a hit moves `reused`, a build moves `rebuilt`, and a
/// build that is not stored moves that and `unstored` — so counters that did not move at all mean the file was
/// never read, which is exactly `Missing`.
///
/// Public rather than private to [`Worklist`] because a caller with its own queue reports the same outcomes:
/// [`crate::Session`] holds a list that notifications re-seed, which a one-shot [`Worklist`] cannot express, and
/// two implementations of "what just happened" would drift.
pub fn outcome_of(before: StoreStats, after: StoreStats) -> StepOutcome {
    if after == before {
        StepOutcome::Missing
    } else if after.reused > before.reused {
        StepOutcome::Reused
    } else if after.unstored > before.unstored {
        StepOutcome::Unstored
    } else {
        StepOutcome::Built
    }
}

impl<F: FileProvider> SummaryStore<F> {
    /// The order to index a project in: see [`Worklist`].
    ///
    /// `open` is what the user is looking at — the editor's open documents, or the one file a query started from.
    /// `project` is the rest, which for a caller with a compile database is its translation units and for a caller
    /// with a directory listing is everything it found. Passing an empty `open` is the ordinary batch case: the
    /// list is then just a bounded breadth-first walk of the project.
    pub fn worklist<'s>(
        &'s mut self,
        open: impl IntoIterator<Item = PathBuf>,
        project: impl IntoIterator<Item = PathBuf>,
    ) -> Worklist<'s, F> {
        Worklist::new(self, open, project)
    }
}

#[cfg(test)]
mod tests {
    use super::{Priority, StepOutcome, Worklist};
    use crate::include::config::CompilerConfig;
    use crate::include::paths::MemoryFiles;
    use crate::index::store::SummaryStore;
    use std::path::{Path, PathBuf};

    /// A store over a few files in memory, with a cache directory of its own.
    fn store(name: &str, files: &MemoryFiles) -> (SummaryStore<MemoryFiles>, std::path::PathBuf) {
        let root = std::env::temp_dir().join("cppls-worklist-tests").join(name);
        let _ = std::fs::remove_dir_all(&root);

        let store = SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
        (store, root)
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    /// Work a list to the end, keeping the steps in order.
    fn run(list: &mut Worklist<'_, MemoryFiles>) -> Vec<super::Step> {
        let mut steps = Vec::new();
        while let Some(step) = list.step() {
            steps.push(step);
        }
        steps
    }

    #[test]
    fn the_open_files_come_first_then_what_they_include_then_the_rest() {
        // The whole point of the order, and the test reads the *order of reads* rather than the result: a
        // provider that was asked for these files in this sequence is a store that did the work in this sequence.
        let files = MemoryFiles::new()
            .with_file("/p/main.cpp", "#include \"a.h\"\nvoid f() { }\n")
            .with_file("/p/a.h", "#include \"b.h\"\n")
            .with_file("/p/b.h", "struct B { int x; };\n")
            .with_file("/p/z.cpp", "int z;\n")
            .with_file("/p/y.cpp", "int y;\n");
        let (mut store, root) = store("order", &files);

        let mut list = store.worklist(paths(&["/p/main.cpp"]), paths(&["/p/z.cpp", "/p/y.cpp"]));
        let steps = run(&mut list);

        let worked: Vec<String> = steps
            .iter()
            .map(|step| step.path.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(
            worked,
            ["/p/main.cpp", "/p/a.h", "/p/b.h", "/p/z.cpp", "/p/y.cpp"],
            "the open file, its includes breadth-first, and only then the project"
        );

        // And the same order is what the provider saw, which is the claim without the list's own bookkeeping.
        assert_eq!(files.reads(), worked);

        assert_eq!(steps[0].priority, Priority::Open);
        assert_eq!(steps[0].depth, 0);
        assert_eq!(
            (steps[1].priority, steps[1].depth),
            (Priority::Open, 1),
            "a.h is one include away"
        );
        assert_eq!((steps[2].priority, steps[2].depth), (Priority::Open, 2));
        assert_eq!(steps[3].priority, Priority::Rest);
        assert_eq!(steps[3].depth, 0, "z.cpp is a project root");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_closure_is_not_pushed_behind_a_long_project_list() {
        // The bug a single queue would have: `open`'s includes are discovered *while* the list runs, so on one
        // queue they would be appended after every project root the caller listed — and the file the user has
        // open would be the only thing indexed before the whole project. Two queues are what this pins, and it
        // needs a project longer than the closure to see the difference.
        let mut builder = MemoryFiles::new()
            .with_file("/p/main.cpp", "#include \"a.h\"\n")
            .with_file("/p/a.h", "#include \"b.h\"\n")
            .with_file("/p/b.h", "struct B { int x; };\n");
        let project: Vec<String> = (0..50).map(|index| format!("/p/file{index}.cpp")).collect();
        for path in &project {
            builder = builder.with_file(path, "int x;\n");
        }
        let (mut store, root) = store("long-project", &builder);

        let mut list = store.worklist(
            paths(&["/p/main.cpp"]),
            project.iter().map(PathBuf::from).collect::<Vec<_>>(),
        );

        let steps = run(&mut list);
        let first_rest = steps
            .iter()
            .position(|step| step.priority == Priority::Rest)
            .expect("the project half is worked");

        assert_eq!(
            first_rest, 3,
            "all three files of the closure are done before the first project file: {:?}",
            steps
                .iter()
                .map(|step| step.path.display().to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(steps.len(), 53, "and nothing is worked twice");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_in_both_lists_is_worked_once_and_in_the_open_half() {
        // "I have it open *and* it is in the project" is the ordinary case, and the answer is that it is the file
        // the user is looking at — so it goes in the half that exists for exactly that reason.
        let files = MemoryFiles::new().with_file("/p/main.cpp", "int x;\n");
        let (mut store, root) = store("both-lists", &files);

        let mut list = store.worklist(paths(&["/p/main.cpp"]), paths(&["/p/main.cpp"]));
        let steps = run(&mut list);

        assert_eq!(steps.len(), 1, "one file, one step: {:?}", steps);
        assert_eq!(steps[0].priority, Priority::Open);
        assert_eq!(files.reads(), ["/p/main.cpp"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_diamond_of_includes_works_the_shared_file_once() {
        let files = MemoryFiles::new()
            .with_file("/p/main.cpp", "#include \"a.h\"\n#include \"b.h\"\n")
            .with_file("/p/a.h", "#include \"common.h\"\n")
            .with_file("/p/b.h", "#include \"common.h\"\n")
            .with_file("/p/common.h", "int shared;\n");
        let (mut store, root) = store("diamond", &files);

        let mut list = store.worklist(paths(&["/p/main.cpp"]), paths(&[]));
        let steps = run(&mut list);

        let worked: Vec<String> = steps
            .iter()
            .map(|step| step.path.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(
            worked,
            ["/p/main.cpp", "/p/a.h", "/p/b.h", "/p/common.h"],
            "the second path to common.h is not a second step"
        );
        assert_eq!(list.reached(), 4);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_cycle_of_includes_terminates() {
        // Two headers that include each other: the file that would be queued a second time has already been seen,
        // so the list ends rather than growing for ever.
        let files = MemoryFiles::new()
            .with_file("/p/a.h", "#include \"b.h\"\nstruct A { int x; };\n")
            .with_file("/p/b.h", "#include \"a.h\"\nstruct B { int y; };\n");
        let (mut store, root) = store("cycle", &files);

        let mut list = store.worklist(paths(&["/p/a.h"]), paths(&[]));
        let steps = run(&mut list);

        assert_eq!(steps.len(), 2);
        assert!(list.is_empty());
        assert_eq!(list.pending(), 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_project_root_discovers_the_headers_it_includes() {
        // The compile-database case: the caller knows the translation units and nothing about the headers, which
        // is what the project half following includes is for.
        let files = MemoryFiles::new()
            .with_file("/p/tu.cpp", "#include \"deep/middle.h\"\nvoid f() { }\n")
            .with_file("/p/deep/middle.h", "#include \"leaf.h\"\n")
            .with_file("/p/deep/leaf.h", "struct Leaf { int x; };\n");
        let (mut store, root) = store("discovery", &files);

        let mut list = store.worklist(paths(&[]), paths(&["/p/tu.cpp"]));
        let steps = run(&mut list);

        let worked: Vec<String> = steps
            .iter()
            .map(|step| step.path.to_string_lossy().replace('\\', "/"))
            .collect();
        assert_eq!(worked, ["/p/tu.cpp", "/p/deep/middle.h", "/p/deep/leaf.h"]);
        assert!(steps.iter().all(|step| step.priority == Priority::Rest));

        // And the index it built is usable across files, which is the reason to walk a graph at all.
        assert!(
            matches!(
                store.index().definition("Leaf", Path::new("/p/tu.cpp")),
                crate::Known::Yes(_)
            ),
            "the discovered header is visible from the unit that includes it"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_second_run_over_the_same_project_reads_nothing_new() {
        // A work list is also the way to warm a cache, so running it twice has to be cheap the second time: every
        // file is a hit, and none of them is parsed. `StepOutcome` is what says so per file.
        let files = MemoryFiles::new()
            .with_file("/p/main.cpp", "#include \"a.h\"\nvoid f() { }\n")
            .with_file("/p/a.h", "struct A { int x; };\n");
        let (mut store, root) = store("twice", &files);

        let first = run(&mut store.worklist(paths(&["/p/main.cpp"]), paths(&[])));
        assert_eq!(
            first.iter().map(|step| step.outcome).collect::<Vec<_>>(),
            [StepOutcome::Built, StepOutcome::Built]
        );

        let mut again = SummaryStore::with_provider(&root, CompilerConfig::default(), files.clone());
        let second = run(&mut again.worklist(paths(&["/p/main.cpp"]), paths(&[])));

        assert_eq!(
            second.iter().map(|step| step.outcome).collect::<Vec<_>>(),
            [StepOutcome::Reused, StepOutcome::Reused],
            "a warm cache means the parser is not asked"
        );
        assert_eq!(again.stats().rebuilt, 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_that_cannot_be_read_is_a_step_like_any_other() {
        // A deleted file in a project is not a reason to stop indexing it. The step is reported and the list
        // moves on — and the file is not queued again, because a second look would ask the same question.
        let files = MemoryFiles::new().with_file("/p/here.cpp", "int x;\n");
        let (mut store, root) = store("missing", &files);

        let mut list = store.worklist(paths(&["/p/gone.cpp"]), paths(&["/p/here.cpp"]));
        let steps = run(&mut list);

        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].outcome, StepOutcome::Missing);
        assert_eq!(steps[0].path, Path::new("/p/gone.cpp"));
        assert_eq!(steps[1].outcome, StepOutcome::Built);
        assert_eq!(steps[1].path, Path::new("/p/here.cpp"));
        assert_eq!(
            list.reached(),
            2,
            "the missing file is held, not dropped and retried"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_whose_include_does_not_resolve_is_still_a_step() {
        // The include resolves to nothing, so there is no file to queue — and the file itself is summarised and
        // deliberately not stored, which the step reports rather than hiding.
        let files = MemoryFiles::new()
            .with_file("/p/main.cpp", "#include \"nope.h\"\nvoid f() { }\n");
        let (mut store, root) = store("unresolved", &files);

        let mut list = store.worklist(paths(&["/p/main.cpp"]), paths(&[]));
        let steps = run(&mut list);

        assert_eq!(steps.len(), 1, "nothing to queue for an unresolved include");
        assert_eq!(steps[0].outcome, StepOutcome::Unstored);
        assert_eq!(store.stats().unstored, 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pending_grows_as_the_list_discovers_files() {
        // A progress bar cannot know the total, and the honest thing is to say so with a number that moves:
        // the list starts with what it was given and grows as includes are read.
        let files = MemoryFiles::new()
            .with_file("/p/main.cpp", "#include \"a.h\"\n")
            .with_file("/p/a.h", "#include \"b.h\"\n")
            .with_file("/p/b.h", "int x;\n");
        let (mut store, root) = store("pending", &files);

        let mut list = store.worklist(paths(&["/p/main.cpp"]), paths(&["/p/z.cpp"]));
        assert_eq!(list.pending(), 2, "one open file and one project file");
        assert_eq!(list.reached(), 2);

        list.step().expect("the open file is worked");
        assert_eq!(
            list.pending(),
            2,
            "one file left plus the include it just discovered"
        );
        assert_eq!(list.reached(), 3, "and a.h is now known");

        list.step().expect("a.h is worked");
        assert_eq!(list.pending(), 2, "b.h was discovered, z.cpp is still there");

        run(&mut list);
        assert_eq!(list.pending(), 0);
        assert!(list.is_empty());
        assert_eq!(list.reached(), 4);

        let _ = std::fs::remove_dir_all(&root);
    }
}
