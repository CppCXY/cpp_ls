//! The driver: one open project, with the layers below joined and a queue that notifications re-seed.
//!
//! Everything below this module is a *capability*: [`discover`](crate::project::discover) can find a toolchain,
//! [`SummaryStore`] can cache a file's facts, [`Worklist`](crate::Worklist) knows the order that answers a user's
//! question first, and the queries in [`crate::index`] answer about a name, a member or a cursor. Nothing joined
//! them, which is why calls "opening a project" the shortest piece of product work left:
//!
//! ```text
//! open a project     discover the toolchain, read compile_commands.json, list the sources
//! say what changed   didOpen / didChange / didClose / didSave, and the client's own file events
//! index some of it   one file per call, open files and what they include before the rest
//! answer a question  parse the buffer the cursor is in, then ask the index about it
//! ```
//!
//! # Who owns the providers
//!
//! The session does. A [`Session`] reads through a [`SessionFiles`] — the editor's open buffers in front of the
//! filesystem — and keeps it, together with the [`SummaryStore`] that reads through the same chain:
//!
//! ```text
//! let documents = OpenDocuments::new();
//! let files = SessionFiles::new(documents.clone(), DiskFiles);
//! let mut session = Session::open(root, files, WatchFilter::new(root));
//!
//! documents.open(path, buffer_text);   // ← the handle is a second owner, not a borrow
//! ```
//!
//! The clone on the second line is the whole of the arrangement, and it is not a trick: every provider in this
//! crate is a **handle** rather than data — `OpenDocuments` is a map behind a lock that an `Arc` shares, `DiskFiles`
//! is a unit struct — so the session's copy and the caller's copy read the same buffers and duplicate nothing. An
//! edit reaches the analysis while the store reads through that chain, and no borrow is involved on either side:
//! what the two owners share has interior mutability and both read it through `&self`.
//!
//! The version before this one *borrowed* the provider for the session's whole life, which forced every caller to
//! declare a provider that outlived the session. A test can do that; a language server cannot, because it builds
//! its session once when the workspace opens and keeps it until the editor exits — the borrow would have had to be
//! leaked, or the session boxed behind a self-referential struct, to fit in a function that returns.
//!
//! # What it deliberately is not
//!
//! * **No LSP types.** Positions here are byte offsets, and the answers are this crate's own. A server maps
//!   `Position` onto an offset (`cpp_parser::LineIndex::get_offset` is that mapping) and the answers onto protocol
//!   responses; keeping the protocol out means this layer is testable without a client.
//! * **No OS watcher, no threads, no clock.** The client is the event source — LSP's `didOpen`/`didChange`/
//!   `didSave`/`didClose`/`workspace/didChangeWatchedFiles` are notifications, so `notify` is not needed and a
//!   debounce clock is the client's. [`Session::advance`] does a bounded amount of work per call and returns, so a
//!   caller decides whether that is an idle tick, a budget, or a loop.
//! * **No conclusion about a file that has not been read.** See below.
//!
//! # Lazy indexing makes "not here" mean two things, and this is where that is handled
//!
//! A query about a name that nothing has indexed answers
//! [`UnknownReason::NotDeclaredHere`](crate::UnknownReason) — the same answer a
//! fully indexed project gives for a name that genuinely is not there. That is deliberate and it is the honest
//! answer ([`ProjectIndex::definition`] explains why the index never claims "nowhere"), and it is *different* from
//! what a user needs: a consumer that reports absence must not report it while the file's includes are still being
//! read. [`Session::pending`] is how it tells the two apart, and the rule for the layer above is:
//!
//! ```text
//! pending() > 0   →  say nothing (and do not report an unresolved name as an error)
//! pending() == 0  →  NotDeclaredHere is now a finding about the project
//! ```
//!
//! # One configuration for the project
//!
//! [`Session::open`] reads `compile_commands.json` if there is one, and the flags it finds are **the project's**:
//! the first entry's, applied to every file. Real projects compile different targets with different `-D`s, so this
//! is an approximation, and the place it would be fixed is [`SummaryStore`], which holds one configuration for
//! every file it caches — the key already records the whole compilation context, so per-file configurations are
//! representable and simply not implemented. The store is what carries the single configuration today.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::cache::SummaryKey;
use crate::file::coordinates::{FileOffset, ReadingOffset};
use crate::file::paths::{DiskFiles, FileProvider, OverlayFiles, normalize_path};
use crate::file::vfs::{Vfs, VfsFile};
use crate::file::view::{FileView, ParsedView, TokensOf};
use crate::include::config::{CompileCommands, CompilerConfig, project_config_from_flags};
use crate::include::toolchain::{self, Environment, Toolchain};
use crate::index::project::{
    MemberCompletions, MemberList, NameCompletions, ProjectDefinition, ProjectDefinitions,
    ProjectIndex, ProjectMacro, substituted_members,
};
use crate::index::references::{MacroReferences, ReferenceBudget, macro_references};
use crate::index::store::{StoreStats, SummaryStore};
use crate::index::watch::{ChangeBatch, FileEvent, Response, WatchFilter};
use crate::index::worklist::StepOutcome;
use crate::index::worklist::{Priority, Step, outcome_of};
use crate::index::{
    FileIndexer, definition_across_files, definitions_across_files, macro_across_files,
    member_completions_at, members_of,
};
use crate::inlay::ParameterHint;
use crate::macros::MacroTable;
use crate::project::{ConfigReport, ProjectDiscovery};
use crate::sema::modules::{ImportOutcome, ModuleScanner};
use crate::summary::{FileSummary, OutlineSymbol, UnitReading};
use crate::symbol::{Known, UnknownReason};
use crate::{PathInterner, cache};

/// **What the compiler predefines, for one file's language.**
///
/// The toolchain's predefines are probed **once, with a C++ source** — `predefined.cpp`, and its `-dM` output carries
/// `__cplusplus` and `_MSVC_LANG`. A `.c` file is not a C++ file, and every C/C++ split in a system header takes the
/// C++ branch while its seed says otherwise: `#ifdef __cplusplus` around a declaration, and MSVC's
/// `extern "C++" { template < size_t _Size > inline errno_t __cdecl strcat_s ( char ( & _Destination ) [ _Size ] … ) }`
/// in `string.h`.
///
/// Measured on Lua 5.5.0 (`C:\…\lua_src\lua-5.5.0`, no compilation database, so the toolchain is discovered): the
/// program reading of `ldebug.c` — a **C** file — contained that C++ block, and the parse reported **15**
/// `expected primary expression` errors, the first inside it; the file's own cook, whose closure is the same, was
/// clean once the seed answered for C.
///
/// The C++ standard's own rule is the one applied: a translation unit compiled as C has no `__cplusplus`, so a header
/// that asks is told no. `_MSVC_LANG` goes with it — MSVC's equivalent question — and the `__cpp_*` feature macros
/// stay: every header gates them on `__cplusplus` first, which is now false.
pub(crate) fn the_seed_for(
    config: &CompilerConfig,
    macros: &crate::Marked,
    file: &Path,
) -> MacroTable {
    let mut seed = MacroTable::from_marked(macros);
    if config.language_for(file).family() == Some(cpp_parser::features::Family::C) {
        // **An `#undef` at offset 0, inserted after the definition** — the table's lookup reads the last binding at
        // or before the offset, so this is what makes the name not a macro for the whole file.
        seed.undefine("__cplusplus", 0);
        seed.undefine("_MSVC_LANG", 0);
    }
    seed
}

/// **[`the_seed_for`], as the `Marked` a unit walk starts from.**
///
/// The walk decides *which branches exist* before any text is rendered: `#ifdef __cplusplus` inside a system header is
/// answered while walking, so the seed the walk is given is what puts MSVC's `extern "C++" { template … }` blocks into
/// a C program's stream. Measured on Lua 5.5.0's `ldebug.c`: 24 of those blocks and **15** parse errors in the program
/// reading, none in the file's own cook.
pub(crate) fn the_marked_seed_for(
    config: &CompilerConfig,
    macros: &crate::Marked,
    root: &Path,
) -> crate::Marked {
    let mut seed = macros.clone();
    if config.language_for(root).family() == Some(cpp_parser::features::Family::C) {
        seed.undefine("__cplusplus");
        seed.undefine("_MSVC_LANG");
    }
    seed
}

/// How many files one drain **cooks**.
/// The same shape as the indexing slice, and for the same reason: cooking a file is a unit walk, a parse of the
/// rendering and an index of it, so a slice keeps the writer lock short enough that the server answers between two
/// of them. The rest stays in the list and the next drain takes the next few — see `Session::pending`, which is
/// what tells a caller there is work left.
// **How many files one cooking slice takes.** The drain itself is the pump's ([`Session::take_cooking_materials`]
// is what it calls), and this is the count that call passes — kept here because the number is a property of cooking
// rather than of a language server: one file is a render and a parse of it, measured at 9–269 ms on MSVC's headers.
#[allow(dead_code)]
/// The sharing cache's file name inside the cache directory — see [`Session::save_the_sharing`].
const SHARING_FILE: &str = "sharing.bin";
const COOK_SLICE: usize = 4;

/// **Where one `diagnostics` call spent its time**, on `CPPLS_TRACE_DIAGNOSTICS=1`.
///
/// The call has four parts and they have four different fixes: the **cooked lookup** (a hash), the **view** (the
/// file's own tree — a parse), the **module notes**, and the **checks**. Measured on MSVC's `<vector>`: the whole
/// call is **3.5 s on every request**, and the tree alone is 21 ms — so the split is not what the code around it
/// suggests, which is the whole reason this exists. Nested timings are printed as they end, one line each.
pub struct DiagTrace(Option<(&'static str, std::time::Instant)>);

impl DiagTrace {
    pub fn new(what: &'static str) -> DiagTrace {
        DiagTrace(crate::probes::diagnostics().then(|| (what, std::time::Instant::now())))
    }
}

impl Drop for DiagTrace {
    fn drop(&mut self) {
        if let Some((what, started)) = self.0 {
            eprintln!(
                "      [diagnostics] {what:<28} {:>7} ms",
                started.elapsed().as_millis()
            );
        }
    }
}

/// **How many files one drain re-reads**, once the closure is in hand.
///
/// The second of the two things a drain does that are proportional to the project, and the one that was measured
/// holding the writer for **10.9 s**: a file is re-read here when its reading may have changed now that its includes
/// are in the index, and after a cold start that is every file in the closure. Eight is small on purpose — the work
/// per file is a unit lookup and a re-index, and the queue the rest waits in is drained by the next call.
///
/// **Four is measured, not chosen.** The whole point of the number is the writer budget, so it was set by watching
/// that budget move: at **8** a slice held the writer for 1105 ms and at **1** for 150 ms, which is linear in the
/// count and says the re-read is the whole of what this constant controls. Four is the middle — the second pass
/// costs four times what one file does and the queue still drains at sixteen files a second.
///
/// What it does **not** control is a second cost in the same closure: even at 1, one slice in this project held the
/// writer for **1003 ms**. That is `units_for_the_pass` — the unit walk, which is one indivisible step because a
/// translation unit is built for a closure rather than for a file. It is the next thing to cut, and the budget
/// warning is what will say when it has been.
const RE_READ_SLICE: usize = 16;

/// **How much summary memory a session keeps before it forgets the biggest ones** — P2's byte budget.
///
/// The density it has to be a budget *of*, measured on the standard library (`tests/modules_probe.rs`, 401 files):
/// **52 MB of summaries and 25 MB of cooked readings** — 130 KB and 112 KB per file, because the STL's headers are the
/// heavy end of the distribution. At that density 8 000 such files hold a megabyte; a hundred-thousand-file project
/// holds **13 GB**, which is the case this exists for, and a 400-file closure holds 52 MB, which is not worth evicting
/// anything for. So the budget is set where it only fires on a project nobody could hold: **1 GiB of summaries**.
///
/// Forgetting is safe by construction: a summary that goes is re-read on demand ([`Session::needs_catching_up`]), which
/// is the same path a cache miss takes. What is *not* free is forgetting the wrong file — see
/// [`Session::enforce_the_summary_budget`] for the three kinds it never touches.
const SUMMARY_BUDGET_BYTES: usize = 1 << 30;

/// **The most items a project may ask a completion for** — the ceiling `[completion] max_items` is clamped to.
///
/// Twenty thousand items is a megabyte of JSON per keystroke and more rows than any client renders; the default is
/// [`crate::completion::ITEM_BUDGET`]. A budget of zero is refused for a different reason: it answers "there is nothing
/// here", which is a lie with no upside.
const MAX_COMPLETION_ITEMS: usize = 20_000;

/// **How many files a slice needs before its second pass shares one timeline instead of walking one per candidate.**
///
/// The second pass re-reads a slice's files and every file that defines a macro one of them mentions. A candidate
/// with no timeline falls to `a_timeline_of_its_own`, which walks that file's whole closure — and a slice can reach
/// far more candidates than it has files: measured on the modules fixture, **6 files reached 44 candidates and the
/// re-read cost 46.8 s**, which was the whole `import std;` read.
///
/// The shared timeline is not free of consequence: a reading built through it has the *slice root's* environment, not
/// the candidate's own, so it cannot be stored as that file's refinement. Four is where a fixture stops being a
/// fixture: below it, one walk per candidate is the cheaper side of that trade and the refinement stays cacheable.
///
/// # The per-candidate walk was tried again, and it is not the way
///
/// The argument for it is that its reading *is* cacheable — the environment is the candidate's own closure — so a
/// first start pays the walks and later ones serve them. Measured, with this constant forced to `usize::MAX` so no
/// slice ever shares, on the same fixture and the same cache:
///
/// ```text
/// shared (WALKS_WORTH_SHARING = 4):   cold 2.5 s     warm 1.1 s
/// own walks (no sharing at all):      cold 38.6 s    warm 66–104 s in three runs
/// ```
///
/// So the half of the argument about caching does not hold: the walks are paid again on every start, and a warm start
/// that walks is *forty to a hundred times* slower than one that shares. Whatever makes those readings unservable (the
/// closure key moving, or `commit_the_re_read` never storing them), the shared timeline is the cheaper default and
/// this constant is the lever that keeps it.
const WALKS_WORTH_SHARING: usize = 4;
/// **How many macro environments one drain builds.**
///
/// One, where [`COOK_SLICE`] is four, and the difference is **measured**: a cook is a unit walk over one file's
/// closure with the results indexed, while building a macro environment takes a copy of every file's text in the
/// closure — 103.7 ms for a 151-file closure against a view's 163.2 µs. A slice of four of those would be most of a
/// second in one drain, which is the thing the slice exists to prevent.
///
/// The queue is what makes a slice this small acceptable: a query that asked for an environment still answers from
/// the plain reading, so nothing is waiting on this — it is a reading being improved, not a request being served.
const MACRO_SLICE: usize = 1;

/// **How long a session waits after an edit before it reads a whole program.**
///
/// The one indivisible piece of work in this session: a unit read is a walk, a cook of every file in include order,
/// a parse of the whole program and a sweep of every declaration in it. Its preparing half takes the *read* lock, and
/// a keystroke takes the write one, so a reader typing during it waits for the whole program to be parsed.
///
/// So it is deferred by an idle period, and the number is a person's typing rhythm rather than a benchmark: at 1 s, a
/// reader who is mid-sentence defers it indefinitely (each keystroke pushes it out), and a reader who has stopped
/// gets the whole closure queryable a second later. What a keystroke can then wait for is the *walk*, which holds the
/// read lock for 44 ms on the measured program — the cook and the parse come after it and hold no lock at all.
const UNTIL_A_UNIT_MAY_BE_READ: std::time::Duration = std::time::Duration::from_secs(1);

/// **How often a root's whole-program read is attempted before the session gives up on it.**
///
/// Each attempt is a *gather* — the walk, the texts — and a read declines when the closure cannot be walked (a
/// dependency the session has no text for). The first decline used to be final: the root was marked read, and measured
/// on a real project three of its own roots were never read because one attempt declined while the index was still
/// filling. Three attempts cover that without letting a genuinely unbuildable root be chosen on every wake — and a
/// fourth would not learn anything the third did not.
const PROGRAM_READ_ATTEMPTS: u8 = 3;

// **There were three size ceilings here, and all three were answering a question nobody had measured.**
//
// `THE_LARGEST_PROGRAM_READ_AT_ONCE` (8 MB of source) and `PROGRAM_READ_BUDGET_BYTES / PROGRAM_READ_BYTES_PER_TOKEN`
// (1 342 177 tokens) refused to read a program at all, and the file kept the per-file reading; the byte estimate
// `PROGRAM_READ_BYTES_PER_SOURCE_BYTE = 48` priced one before the walk. Their justification was "5 GB of private
// bytes", read off a counter that only ever **added** — no free was subtracted — so it measured churn.
//
// Measured with a counter that counts both sides (`cpp_code_analysis::alloc`, and
// `tests/cold_start_memory_probe.rs` which drives a whole session with it): a cold index of `test_cpp` holds 147 MB,
// seventeen open files with the pump drained hold 250 MB (peak 281), and seventeen whole-program reads take it to
// 478 MB (peak 528) — about 13 MB each, and 2.7–4.4 s each for 340 000–594 000 tokens. Churn does not inflate the
// resident set at all: forty reads of a 218 KB header churn 1 723 MB in 32 million allocations and the working set
// never leaves **11 MB**. The 4.5 GB was one request path — `docs/open-questions.md` has the measurement — and no
// program is too large to read.

/// How many files [`Session::want_the_closure_cooked`] reaches for **one level below the direct includes**.
///
/// A payload bound, like [`COOK_SLICE`]: the rule above it is "a header's own headers are where the standard
/// library keeps what a reader asks for", and the cost of applying it without a bound is the 11.7 s the whole
/// closure took. Sixty-four covers the layer under every MSVC header that is a thin wrapper over another
/// (`<string>` → `<xstring>`, `<vector>` → `<xmemory>`, `<map>` → `<xtree>`); a translation unit that includes
/// the whole library wants [`Session::want_everything_cooked`], which is the caller's decision and not a default.
/// **How many files the second level beyond a file`s includes may cook** — see [`Session::want_the_closure_cooked`].
///
/// The first level (`COOK_ONE_LEVEL_FURTHER`, 64) reaches the headers a file includes; this one reaches the headers
/// *those* include, which is where a class a header uses is declared: MSVC`s `<string>` is a thin layer over
/// `<xstring>`, and `basic_string` is in `xstring`. Measured: cooking `xstring` took **2.17 s**, and without it
/// `std::string` answered `NotDeclaredHere` in a file that includes the header that includes the header that
/// declares it.
///
/// Smaller than the first level because it is further from the file being edited and is reached only when the first
/// level does not already answer.
const COOK_TWO_LEVELS_FURTHER: usize = 24;

const COOK_ONE_LEVEL_FURTHER: usize = 64;

/// How many data members a class may have before the hover stops pricing its layout.
///
/// See the note in [`Session::the_layout_of`]: a layout is a visibility walk per member, and a header's registry
/// struct (neovim's `file_buffer` has eighty members) cost **7.2 s** of hover with no bound.
const LAYOUT_MEMBER_BUDGET: usize = 64;
/// How many files a project scan will list before it stops.
///
/// A bound on a directory walk whose size is a fact about a machine rather than about the project: a checkout with
/// a generated tree in it can be arbitrarily large, and a session that spent a minute listing it would delay the
/// first answer for a reason the user cannot see. Ten thousand translation units and headers is far past any
/// project that has been measured here, and the number is reported rather than silent — [`Session::project_files`]
/// is what a caller reads to see what the scan found.
const MAX_PROJECT_FILES: usize = 10_000;

/// The extensions a project scan treats as sources.
///
/// # Why a list here, when the event filter deliberately has none
///
/// `WatchFilter` refuses an extension allow-list for events, because `#include <vector>` has no extension and a
/// filter keyed on names would drop exactly the files an index most needs. A **scan** is a different question: it
/// is looking for the files a project *owns*, and it has no include to follow — so the alternative to a list is
/// guessing that every file in the tree is a source. Both halves of that distinction are the point:
///
/// ```text
/// an include   the file was NAMED — open it whatever it is called
/// a scan       the file was FOUND  — take it when its name says it is a source
/// ```
///
/// A header with no extension is still indexed: it arrives through an include, which is how a compiler finds it
/// too.
const SOURCE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cxx", "c++", "h", "hh", "hpp", "hxx", "h++", "ipp", "inl", "tpp", "tcc",
    "cppm", "ixx",
];

/// The files a session reads through: the editor's buffers in front of a fallback provider.
pub type SessionFiles<F = DiskFiles> = OverlayFiles<OpenDocuments, F>;

/// The documents an editor has open, as a provider.
///
/// # Why this exists next to [`crate::OverlayFiles`]
///
/// `OverlayFiles` is the *shape* — one provider in front of another — and it is generic over both halves. This is
/// the front half an editor needs: a set of buffers that changes while the analysis reads through it, keyed by
/// normalized path, and readable through `&self` because the handle is shared rather than borrowed — the session
/// holds one, and every caller that kept this handle holds another.
///
/// # What an open document means
///
/// The text in here is **the text**, and the file on disk is not consulted for it. That is the whole difference
/// between an analysis that works while a user types and one that works after they save: a buffer's includes
/// resolve against what has been typed, a buffer that has never been saved has no file at all, and a save is a
/// notification that the two now agree. An unsaved buffer is indexed as itself — see [`Session::advance`] — and its
/// summary is filed under the hash of *its* text, so the cache entry is correct the moment that text reaches a
/// disk and merely unused until it does.
///
/// # Why a lock rather than `&mut self`
///
/// Because two owners write here and they are not the same owner. A session writes through
/// [`Session::did_open`], which holds `&mut Session` and is the editor's ordinary notification path; a caller that
/// kept this handle writes without the session at all. An editor that had to borrow its session mutably to record
/// one keystroke would serialise every request behind that keystroke. A read-mostly lock is what lets an edit land
/// *between* two queries instead — this handle is written while a session reads through it. The lock is never held
/// across one.
#[derive(Debug, Clone, Default)]
pub struct OpenDocuments {
    buffers: Arc<RwLock<HashMap<String, String>>>,
    revision: Arc<std::sync::atomic::AtomicU64>,
}

impl OpenDocuments {
    pub fn new() -> Self {
        OpenDocuments::default()
    }

    /// A document is now open, with this text — LSP's `didOpen`, or any later text for the same path.
    ///
    /// One method for both because the analysis has one question about it: which text is this path's. The protocol
    /// distinguishes the two events; nothing here does.
    ///
    /// `&self` rather than `&mut self`, because the buffer map is behind a lock: a caller holding the handle can
    /// write a buffer while a session reads through it, which is the whole reason the handle exists.
    pub fn open(&self, path: impl AsRef<Path>, text: impl Into<String>) {
        let mut buffers = self
            .buffers
            .write()
            .expect("the buffer map is not poisoned");
        let key = normalize_path(path.as_ref(), cfg!(windows));
        let text = text.into();
        if buffers.get(&key) != Some(&text) {
            buffers.insert(key, text);
            self.revision
                .fetch_add(1, std::sync::atomic::Ordering::Release);
        }
    }

    /// The document is closed: the path is the filesystem's again.
    ///
    /// Returns whether there was a buffer to drop. The text is **not** kept: after a close, the disk is the answer,
    /// and holding the last buffer would answer with an edit that may never have been saved.
    pub fn close(&self, path: impl AsRef<Path>) -> bool {
        let mut buffers = self
            .buffers
            .write()
            .expect("the buffer map is not poisoned");
        let removed = buffers
            .remove(&normalize_path(path.as_ref(), cfg!(windows)))
            .is_some();
        if removed {
            self.revision
                .fetch_add(1, std::sync::atomic::Ordering::Release);
        }
        removed
    }

    fn revision(&self) -> u64 {
        self.revision.load(std::sync::atomic::Ordering::Acquire)
    }

    /// The buffer for a path, if it is open.
    pub fn text(&self, path: impl AsRef<Path>) -> Option<String> {
        self.buffers
            .read()
            .expect("the buffer map is not poisoned")
            .get(&normalize_path(path.as_ref(), cfg!(windows)))
            .cloned()
    }

    pub fn is_open(&self, path: impl AsRef<Path>) -> bool {
        self.buffers
            .read()
            .expect("the buffer map is not poisoned")
            .contains_key(&normalize_path(path.as_ref(), cfg!(windows)))
    }

    /// Every open document, in an unspecified order.
    pub fn paths(&self) -> Vec<PathBuf> {
        self.buffers
            .read()
            .expect("the buffer map is not poisoned")
            .keys()
            .map(PathBuf::from)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.buffers
            .read()
            .expect("the buffer map is not poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl FileProvider for OpenDocuments {
    fn read(&self, path: &Path) -> Option<String> {
        self.text(path)
    }

    fn exists(&self, path: &Path) -> bool {
        self.is_open(path)
    }

    fn is_open_buffer(&self, path: &Path) -> bool {
        self.is_open(path)
    }
}

/// **What a session's translation units cost it** — the unit-side counterpart of [`crate::StoreStats`].
///
/// The three numbers are the three paths [`Session::translation_unit_of`] can take, and they answer the only
/// question worth asking about this cache: **is a keystroke paying for a walk?**
///
/// ```text
///   from_memory   this session already held the unit            — no read, no decode, no walk
///   from_disk     decoded from the cache, closure check included — a read and a hash per file
///   walked        the whole closure read and a timeline built    — and written for the next run
/// ```
///
/// Before the root of a unit was keyed on its preamble it moved on every keystroke, and no other
/// number in this crate would have shown it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnitStats {
    pub from_memory: usize,
    pub from_disk: usize,
    pub walked: usize,
}

/// Which of [`Session::translation_unit_of`]'s three paths answered — the argument to `Session::note_a_unit`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Which {
    FromMemory,
    FromDisk,
    Walked,
}

/// One open project: the configuration, the facts read so far, and the order the rest will be read in.
///
/// The provider chain is owned rather than borrowed, which is what lets a long-lived caller — a language server —
/// hold a session in a field. See the module documentation: the caller keeps a handle to the same chain by cloning
/// it before the session takes it, and a clone of a provider is a second handle on the same files.
/// **A file's configuration and its language, worked out once** — the value [`Session::config_for_file`] memoizes.
///
/// The language is kept beside the configuration because deriving it is not free and it was being derived per query:
/// [`CompilerConfig::language_for`] looks the file up in the compilation database *again* and re-parses `-std=` out of
/// the command line, for an answer that cannot change while the session lives.
#[derive(Debug)]
pub(crate) struct PerFileConfig {
    config: std::sync::Arc<CompilerConfig>,
    language: cpp_parser::features::LanguageConfig,
    cuda: bool,
}

/// **What a cached view was parsed from**, and the whole reason a stale one cannot be served.
///
/// A `FileView` is a function of exactly these seven things, and each of them moves for a reason:
///
/// ```text
/// path       one view per file, whatever spelling the caller used
/// text       the content hash of the text parsed — a rendering's, or the file's own for a buffer view
/// spans      how many rendering spans went with it: part of the identity of the offset mapping
/// id         the VFS's identity for the file, which a delete-and-recreate moves
/// cuda       the parse's vendor switch
/// language   the file's own language level (`-std=`), which changes what a name may mean
/// open       whether the text is a buffer rather than the file on disk
/// ```
///
/// The **text** is the load-bearing one, and it is why this needs no invalidation hooks: a rendering whose text
/// changed for any reason — a header edited, a macro moved, the closure grown — is a different key rather than a
/// stale hit. The rendering cache can rely on `retain(dependent)` for the same job because it is keyed by the
/// *dependent's* text; this one is keyed by what was actually parsed, which is strictly narrower.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ViewKey {
    path: PathBuf,
    /// **Which rendering this is**, by object — see [`RenderingId`]. `None` for a buffer view, which has none.
    ///
    /// By object rather than by the hash of its text, and the difference is the cost of a hit on a large file: hashing
    /// a 1 MB rendering is about a millisecond and would be paid on every hit, while comparing an `Arc` pointer is one
    /// word.
    rendering: Option<RenderingId>,
    /// The file's own text, by hash — what a **buffer view** was parsed from.
    text: u64,
    id: crate::file::paths::FileId,
    cuda: bool,
    language: Option<cpp_parser::features::LanguageConfig>,
    open: bool,
    /// **The macro environment a buffer view was parsed with**, when there was one. `None` is the empty environment,
    /// which is one value however often it is made.
    ///
    /// Held rather than identified by a raw address: an id that outlived its allocation could name a *different*
    /// environment that happened to be allocated where the old one was. The environment is an `Arc`, so holding it is
    /// one reference count — and the identity is `Arc::ptr_eq`, which is exactly "the same environment object".
    environment: Option<EnvironmentId>,
}

/// **The key the rendering cache uses** — the normalized spelling, because a caller may hold a canonicalized one.
fn a_rendering_key(path: &Path, text_hash: u64) -> (PathBuf, u64) {
    (
        PathBuf::from(crate::file::paths::normalize_path(path, cfg!(windows))),
        text_hash,
    )
}

fn buffer_view_key(
    file: &VfsFile,
    cuda: bool,
    environment: Option<Arc<cpp_parser::MacroEnvironment>>,
) -> ViewKey {
    ViewKey {
        path: file.path.clone(),
        rendering: None,
        text: crate::cache::content_hash(&file.text),
        id: file.id,
        cuda,
        language: None,
        open: file.open,
        environment: environment.map(EnvironmentId),
    }
}

/// The identity of a rendering — **by object, held**, see [`ViewKey::rendering`].
#[derive(Debug, Clone)]
struct RenderingId(std::sync::Arc<crate::preprocess::cooked::RenderedCooked>);

impl PartialEq for RenderingId {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for RenderingId {}

impl std::hash::Hash for RenderingId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::sync::Arc::as_ptr(&self.0).hash(state);
    }
}

/// The identity of a macro environment — **by object, held**, see [`ViewKey::environment`].
#[derive(Debug, Clone)]
struct EnvironmentId(std::sync::Arc<cpp_parser::MacroEnvironment>);

impl PartialEq for EnvironmentId {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for EnvironmentId {}

impl std::hash::Hash for EnvironmentId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::sync::Arc::as_ptr(&self.0).hash(state);
    }
}

/// What a session is holding — see [`Session::cache_sizes`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheSizes {
    /// Cached renderings, by `(path, content hash)`. **Unbounded** — see [`Session::cache_sizes`].
    pub renderings: usize,
    /// The text of those renderings, in bytes.
    pub rendering_bytes: usize,
    /// Translation units held (bounded by `MAX_UNITS`).
    pub units: usize,
    /// Summaries in the index.
    pub summaries: usize,
    /// Declaration facts across them.
    pub facts: usize,
    /// Macro facts across them.
    pub macros: usize,
    /// **Cooked readings held**: files, and the declarations in them.
    pub cooked_files: usize,
    pub cooked_declarations: usize,
    /// **Parse trees held for reuse**: how many views, and their estimated tree bytes — see [`Session::view`].
    pub views: usize,
    pub view_bytes: usize,
}

/// **What one parsed view costs**, in the estimate the view cache's budget is stated in.
///
/// A rowan tree weighs 8–24× the text it describes, measured on two corpora (`docs/parser-review.md` §2): one
/// allocation per token with the token's bytes copied into it, one per node with its children. 8 is the low end and a
/// source file is the case that hits it, so `text × 8` plus a word per token is the estimate — deliberately an
/// under-estimate, because over-stating it would evict views that are cheap to keep and the budget is what matters,
/// not the last megabyte.
///
/// **Why not the rendering weight**: that is `text + spans × 16`, which is what the *rendering* cache is bounded by.
/// The two caches hold different things and a view is the larger one.
fn a_view_weighs(view: &FileView) -> usize {
    view.source.len().saturating_mul(8) + view.tree.get_tokens().len().saturating_mul(16)
}

#[derive(Clone, Debug)]
pub struct SessionRevision {
    session: Arc<()>,
    changes: u64,
    documents: u64,
}

pub struct Session<F: FileProvider = DiskFiles> {
    root: PathBuf,
    identity: Arc<()>,
    changes: u64,
    config: CompilerConfig,
    /// What `discover` found, or `None` when no compiler answered. Kept for a human reading a log line, and for
    /// 's measurements.
    toolchain: Option<Toolchain>,
    /// **What this analysis thinks the project is**: the configuration file, the compile database and where it was
    /// found, and every problem. Kept whole rather than reduced to the parts the analysis uses,
    /// because the first question anybody asks about a wrong answer is "what did it think this project was" — and
    /// the sections a language server reads ([`crate::DiagnosticsSection`], [`crate::HoverSection`]) live in the
    /// same struct, since one file is parsed once, in one place.
    discovery: ProjectDiscovery,
    /// The providers. Owned; the store holds a second handle to the same chain.
    files: SessionFiles<F>,
    /// The buffer half of `files`, as a handle this session can write through.
    documents: OpenDocuments,
    /// **The files this session is holding** — their text and their line indexes, kept in step with each other.
    ///
    /// The text source for everything a *cursor* query needs, and the reason a view costs a pointer rather than a
    /// scan: see [`crate::file::vfs`]. It reads through the same provider chain the store does, so a buffer and a
    /// file are the same question to both.
    vfs: Vfs<SessionFiles<F>>,
    store: SummaryStore<SessionFiles<F>>,
    /// Which paths the client's events and the project scan are about. Kept because both questions — "is this
    /// event interesting" and "is this file part of the project" — have one answer, and the cache directory is the
    /// case that makes that concrete: it is not project source and it is not an event anybody wants.
    filter: WatchFilter,

    /// **Whether the walk cache's cross-walk contribution sharing is used.** Off unless the A/B probe
    /// (`CPPLS_WALK_SHARE`) asks for it: the sharing replays a file's contribution at a position the compiler would
    /// not, and that drops include edges — measured on `import std;`, `<limits>` lost `<cwchar>` and `<xstddef>`.
    share_contributions: bool,

    /// What the project scan found, so that a configuration change can re-seed from it.
    project: Vec<PathBuf>,
    project_paths: HashMap<String, PathBuf>,
    queue: Work,
    /// **What the disk said when each file was last read** — the whole of [`Session::reconcile`]'s cheap half.
    ///
    /// Keyed like everything else that names a file ([`queue_key`]) and recorded by [`Session::index_one`], which is
    /// the one place a file's summary enters the index. A file the analysis is holding as a **buffer** has no entry:
    /// the client's text is the text, and the disk's mtime says nothing about it.
    stamps: HashMap<String, crate::file::FileStamp>,
    /// The files this session has **parsed** since the last body pass — the candidates for
    /// [`SummaryStore::re_read_where_a_body_decides`], which may only run once the closure is in hand.
    ///
    /// Accumulated across `advance` calls rather than taken from one: a closure is read in chunks (64 files at a
    /// time), and a file parsed in the first chunk is exactly as much a candidate as one parsed in the last. Taking
    /// only the chunk that happened to drain the queue is what left MSVC's `<xstring>` and `<vector>` — both read in
    /// the first chunk, both scoped by a macro body defined in a later one — reading at file scope for ever.
    parsed_since_the_last_pass: Vec<PathBuf>,
    /// **The files somebody has asked to have refined** — phase 0 of `docs/indexing-design.md`.
    ///
    /// The second pass used to drain the whole backlog at startup: on `EmmyLuaCodeStyle` that was 1745 files and
    /// **59–120 s**, for answers about files the reader had not opened. A start now refines nothing; `catch_up` marks
    /// the file a request or an open is about, and the pump refines only what it finds here. The backlog stays for the
    /// files nobody asked about — they are refined the first time somebody does.
    ///
    /// **Behind a lock of its own**, because the thing that asks is a *request*: it holds the session for reading, and
    /// taking the write lock to record a `HashSet` insert is the wait this whole phase exists to remove.
    refined_on_request: std::sync::Mutex<std::collections::HashSet<PathBuf>>,

    /// **The stored-refinement questions that answered "not stored", by file and content hash.**
    ///
    /// `prepare_wave` asks the disk cache whether the slice's files have a stored refinement; answering that for
    /// one file walks its **whole include closure** (`the_closure_fingerprint`, up to 8192 summaries). The answer
    /// cannot change while the text does not — no refinement is storable any more, so a miss is permanent for that
    /// content. Measured on `test_cpp/src/containers.cpp` (663-file closure): **20 MB in the wave stage on every
    /// round** with `prepared 0`, i.e. sixteen closure walks a round that could never answer anything else.
    /// Keyed by the index's content hash, because the file need not be held by the VFS when the slice is planned.
    refinement_misses: std::sync::Mutex<std::collections::HashSet<(String, u64)>>,
    /// **The files whose reading in the index is a refined one** — what [`Session::is_refined`] answers, and what
    /// phase 1 needs to know when an answer is still the shape-only reading.
    refined_readings: std::sync::Mutex<std::collections::HashSet<String>>,
    /// **The open files whose cooked reading is missing or stale** — what [`Session::cook`] is called for when the
    /// queue drains.
    ///
    /// A queue rather than "all open files, every time", for two reasons that are both about cost: a drain happens
    /// after every keystroke, and re-cooking a file whose text did not change is a unit walk and a parse for an
    /// answer the index already holds. A file joins this when a buffer changes ([`Session::did_change`] and
    /// friends) and when it is opened, because those are the two moments its reading can be out of date — a file
    /// whose summary came from the **disk cache** was never parsed in this run, so nothing else would notice that it
    /// has no cooked reading at all.
    cooking: Cooking,
    /// **Every `#define` this session has read back**, by the file and offset it was written at — see
    /// [`crate::MacroDefinitions`].
    ///
    /// One per session rather than one per cook, because the expensive part is reading a definition out of a file's
    /// text and the same definition is in force for hundreds of files: project-wide cooking turned a per-cook cache
    /// into the difference between 4.5 s and 12.7 s for 255 files (measured; the census's own note records the same
    /// effect at 40 s). Invalidated **per path** when a file changes, since the keys are offsets in that file.
    /// **The macro definition cache, behind its own lock** — see [`Session::held_definitions`].
    definitions: std::sync::Mutex<crate::MacroDefinitions>,
    /// **The project's translation units that have been read as programs** — see [Session::read_the_unit].
    ///
    /// Cleared wherever [Session::units] is: a unit read is a reading of the same timeline, so anything that
    /// moves inside it makes this list a claim about a reading that no longer describes its closure.
    units_read: std::collections::HashSet<String>,
    /// **Translation units the session has been told to read as programs** that are not open documents — the module
    /// interface units, in practice.
    ///
    /// `import std;` names a module, not a file, so nothing in the project includes `bits/std.cc`: it arrives through
    /// [`Session::queue_the_modules_a_file_imports`], and from then on it is one of the project's units. A compiler
    /// reads it as a translation unit — the whole closure, rendered in *its* environment — which is the only reading
    /// in which the library's own scopes are right: read raw, `shared_ptr_base.h`'s `_Sp_counted_ptr_inplace` never
    /// closes (its class head is written three times under `#if`/`#else`) and every class after it is filed as its
    /// nested class.
    ///
    /// [`Session::the_next_unit_to_read`] considers these after the open documents, so the pump reads a module once,
    /// at the same budget as any other unit read, and never before a reader's own file.
    programs_wanted: std::collections::HashSet<String>,
    /// **How often a root's program was attempted and declined** — see [`Session::commit_a_program`]. A root that
    /// cannot be walked yet is retried, but not for ever: three attempts and it keeps the reading the per-file path
    /// gave it.
    units_attempted: std::collections::HashMap<String, u8>,
    /// **The findings one file's last pull produced, and the state they were produced from.**
    ///
    /// A pull builds a `SemanticModel` (it borrows the tree and holds a non-`Send` cache of header views, so it cannot
    /// outlive the call) and runs every rule over it — measured **143.4 ms** on `EmmyLuaCodeStyle`, 97 % of it type
    /// inference. The findings themselves are plain data and a function of everything the pull reads, so the *answers*
    /// are what is remembered; the key names the state ([`Session::the_state_of_a_pull`]).
    checks_memo: std::sync::Mutex<Option<(u64, Vec<crate::sema::check::Finding>)>>,
    /// **How many pulls were answered from [`Session::checks_memo`] and how many ran the rules** — the two numbers
    /// that say whether the memo is worth its key, and the ones a regression can assert without a clock.
    ///
    /// `pull_runs` is the cost: one model and every enabled rule, measured at 143 ms on a real project. `pull_hits`
    /// is the saving. The pair is what turns "the memo misses after an unrelated edit" into a number a test can fail
    /// on, which is why they exist rather than a log line.
    pull_hits: std::sync::atomic::AtomicU64,
    pull_runs: std::sync::atomic::AtomicU64,
    /// **When this session last took an edit** — the clock the one indivisible piece of work in it is deferred by.
    ///
    /// A unit read ([`Session::prepare_a_unit_reading`]) is a walk, a render and a **parse of a whole program** in one
    /// piece: 5 s measured on a 159-file closure. It is `&self` work and it runs beside queries, but a *keystroke*
    /// takes the write lock and would wait for it — so nothing in a session may start one while a reader is typing.
    /// [`Session::a_unit_may_be_read_now`] is that rule, and this is the instant it is measured from.
    last_edit: std::time::Instant,
    /// **The last program this session indexed, and what it was** — one entry, keyed by the stream's own bytes.
    ///
    /// A unit read is a walk, a render, a **parse of the whole program** and a sweep of every declaration in it, and
    /// the reading is a pure function of the stream: the same bytes give the same facts. Sources that include the
    /// same headers therefore produce the **same** stream — measured on twenty sources each including `<future>`, all
    /// twenty render to 617 607 tokens, byte for byte — and re-parsing it nineteen more times is work nobody asked
    /// for.
    ///
    /// # What the measurement was
    ///
    /// Twenty sources, one shared header set, on this machine:
    ///
    /// ```text
    ///                  ms        share of the run
    /// [indexing]     131 322       71.9%     ← the render's parse and sweep, per unit
    ///   render-parse  59 190       32.4%
    ///   render-sweep  72 133       39.5%
    /// [units]         47 963       26.2%
    ///   unit-render   45 489       24.9%     ← the render itself, also per unit
    /// ```
    ///
    /// 179 seconds for twenty units is 300-odd for forty, which is the number this exists to remove. The cache is
    /// one entry rather than a map because the sources of one project include the same headers **in runs**: a build
    /// walks them in directory order, and the entry that helps is the one just computed.
    ///
    /// Keyed by [`crate::cache::content_hash`] of the stream so nothing large is kept twice, and dropped wherever the
    /// index or the configuration moves, because a program's reading is only valid for the configuration that read it.
    ///
    /// # Why it is behind a lock
    ///
    /// Because the read that fills it is the expensive half of a unit read — a render, a parse and a sweep — and that
    /// half has to be payable **outside the write lock** ([`Session::prepare_a_unit_reading`]) if a whole program is
    /// ever to be read as one. Same reason, and same shape, as [`Session::units`].
    unit_index: std::sync::Mutex<Option<(u64, crate::IndexedUnit)>>,
    /// **The last unit this session rendered, and what its inputs were** — one entry, keyed by the *content* of
    /// every file the walk read.
    ///
    /// A render is a walk, a cook of every file in include order, and a stitched stream of megabytes; the plan's
    /// §5.1 M4 exists because the second one costs exactly as much as the first. What a reader does is ask for the
    /// same unit again and again — every keystroke that lands on a name, every drain — so the second answer is the
    /// one worth having cheap.
    ///
    /// # The key is the inputs, not the root path
    ///
    /// Keyed on the root alone it would answer with a stream built from text the files no longer have: a file the
    /// reader has open and has **not saved** comes from the overlay, and an edit changes the program without changing
    /// its name. So the key is a hash of `(path, content hash)` for every file the walk reached, which is exactly
    /// what the render is a function of. **One entry**, because the interesting repetition is the same unit asked
    /// for twice in a row, and a map would trade a render for a memory of every stream the session ever built.
    ///
    /// Behind a lock for the same reason [`Session::unit_index`] is.
    rendered_unit: std::sync::Mutex<Option<(u64, crate::RenderedUnit)>>,
    /// **How many items one completion may answer with** — `[completion] max_items` in `.cppls.toml`, clamped.
    ///
    /// A **payload** bound rather than a UI one: the client renders ten rows and receives all of them, and an
    /// untruncated `std::` would be 12 867 items and megabytes of JSON per keystroke. What makes the bound harmless is
    /// that a cut list reports itself (`isIncomplete: true`), so the client asks again on the next keystroke and the
    /// prefix filter — applied in the index before anything is cloned — narrows the answer exactly. Measured on the
    /// reader's project: `std::` 1000 items in 53 ms (truncated), `std::s` 167 in 7 ms, `std::st` 55, `std::str` 38,
    /// `std::string` 6 — exact from one letter on.

    /// **Which key a root's rendering was filed under** — `(root, context) → units_key`, for this session.
    ///
    /// The walk exists to produce that key and costs **70.7 ms** on the standard library's 256 files. A rendering
    /// records the walk's own file list (`RenderedUnit::walked`), so the key can be reproduced from the rendering
    /// alone — and validated against those files' texts, so an edit between two asks is a miss rather than a stale
    /// answer.
    unit_roots: std::sync::Mutex<std::collections::HashMap<(PathBuf, u64), u64>>,
    /// **What each edited file's directives said the last time it changed**, by the path the queue keys on.
    ///
    /// The comparison an edit is judged by: an edit that leaves a file's directives where and what they were cannot
    /// have changed the environment any file reads it in, or the timeline of any unit it is part of — so it drops
    /// neither. See [`crate::preprocess::directive::DirectiveSignature`].
    directive_signatures:
        std::collections::HashMap<String, crate::preprocess::directive::DirectiveSignature>,
    /// **Files already read and parsed, waiting for the queue to reach them** — see [`Session::advance`].
    ///
    /// Bounded by what one `advance` asked for, and emptied by anything that could make a prepared answer describe a
    /// text the file no longer has: an edit, a close, a filesystem event. It is a read-ahead, never a source of
    /// truth: an entry that is not here is simply read when its turn comes.
    prefetched: HashMap<String, crate::index::store::Prepared>,
    /// **The order the prepared slices were made in**, oldest first — what a budget drops from.
    ///
    /// The map has no order, and the order is the whole question a bound asks: the entries made *last* are the ones
    /// furthest ahead of the queue, so they are the ones to let go when the read-ahead is over its budget. A key here
    /// that is no longer in `prefetched` is stale and skipped.
    prefetched_order: std::collections::VecDeque<String>,
    /// **The translation unit each cooked file was read in**, kept across cooks.
    ///
    /// The one-walk timeline ([`crate::TranslationUnit`]) is what makes a *file's* environment a position in a walk
    /// rather than a walk of its own — but [`Session::cook`] was building one **per cooked file**, so cooking the
    /// 138 files of one real project's closure walked that closure 138 times. The compiler's answer to the same
    /// repetition is a preamble, and the crate already has it on disk ([`crate::TranslationUnitCache`], keyed on the
    /// content of every file the walk entered); this is the in-session half, so that a second cook of the same
    /// translation unit does not even decode the entry again.
    ///
    /// Cleared whole by any change: a unit is a reading of its closure, and the shortest correct invalidation is
    /// "nothing in it may have moved".
    ///
    /// **Bounded** ([`MAX_UNITS`]): a unit holds a whole closure's timeline, and a project with a thousand sources
    /// would otherwise keep one per source it was ever asked about.
    /// **The units table, behind its own lock rather than behind `&mut self`.** See [`UnitTable`].
    ///
    /// It was a plain field, which made [`Session::translation_unit_of`] take `&mut self` — and a unit build is the
    /// single most expensive thing a drain does: measured from the server's write-budget warning, **1003 ms**, on
    /// the first slice after a cold start. `&mut self` is what put it *inside* the writer, where every request in
    /// flight waits for it. A lock of its own lets the unit walk happen under the **read** lock instead, beside
    /// whatever a person is typing, which is the whole point.
    units: std::sync::Mutex<UnitTable>,
    /// **A timeline already walked in this run, by what makes it the same timeline.**
    ///
    /// A `TranslationUnit` is a function of its closure's **content** and the compilation context — which is exactly
    /// what [`crate::tu_cache`]'s on-disk key says, and why a warm run reports `reused 1494 / rebuilt 235`: the disk
    /// already shares what is shareable *between* runs. What it cannot do is share **within** one, because nothing is
    /// on it yet.
    ///
    /// Measured on `EmmyLuaCodeStyle`: 2110 roots, **786 distinct closures**, and **977 walks (46%) that a shared
    /// result would remove**. The largest single group was 638 roots with the same (empty) closure. Those repeats are
    /// walked one after another, each re-evaluating every guard of every file it reaches — 68 s of a 186 s index in
    /// `walk-macro-guard` alone.
    ///
    /// The key is the context plus a hash of the closure's files **by content**, so two roots whose closures hold the
    /// same text share one timeline whatever the paths are, and a root whose closure differs by one byte of one
    /// header is walked on its own. See [`Session::closure_identity`].
    ///
    /// A `Mutex` of its own rather than the units table's: the two are read at different points of the same call, and
    /// holding one across the other is how a re-entrant lock becomes a deadlock.
    ///
    /// **Bounded in bytes** ([`MAX_WALKS_BY_CONTENT_BYTES`]), not in entries: a timeline measures 0.1–6.2 MB, so a
    /// count would be a budget that says nothing about what it holds. Least recently used goes first; a unit larger
    /// than the whole budget is still kept, because the walk that just produced it is the one a query is about to
    /// use and rebuilding it is the most expensive thing this session can do.
    walked_by_content: std::sync::Mutex<
        cache::cache_lru::Cache<(u64, u64), std::sync::Arc<crate::TranslationUnit>>,
    >,
    /// **The headers the include search path can name**, read once — what `#include` is completed from.
    ///
    /// Built with the session rather than on the first completion, and the reason is the budget rather than the
    /// cost: this walks the `-I` directories with a depth bound and a file count, so the work is bounded and
    /// happens once, while doing it lazily would put a filesystem walk on the first keystroke after a `#include`.
    ///
    /// Behind a lock because the **project's** half of it grows: [`crate::Session::add_project_files`] re-derives
    /// that half when a build system hands over more translation units, and a completion may be asked for before,
    /// during or after. The search-path half never changes after this field is built.
    headers: crate::SharedHeaders,
    /// **The macros a file's closure defines, remembered by the content they were read from.**
    ///
    /// [`Session::view_with_macros`] walks the file's whole closure and takes a copy of every file's text to build
    /// one of these, and that is **measured at 106.7 ms** beside 167.8 µs for a view without it — 635×, on a
    /// closure of 151 files. Which is affordable once and not per query, so it is kept.
    ///
    /// The key carries the **content hash** rather than the path: a buffer being typed in has a new text on every
    /// keystroke and the same path, and an environment built from the old text would be a wrong answer rather than
    /// a stale one. Two files with the same content share an entry, which is right — the environment is a reading
    /// of that text and its closure.
    ///
    /// Behind a lock because the queries that want it hold a read on the session, and an environment is only ever
    /// added (§[`Session::view_with_macros`] takes `&self`).
    macro_environments: std::sync::Mutex<
        cache::cache_lru::Cache<(PathBuf, u64), std::sync::Arc<cpp_parser::MacroEnvironment>>,
    >,
    /// **Files whose macros something has asked for and not yet got.**
    ///
    /// The other half of the arrangement [`Session::macro_environments`] describes: a query cannot pay 103.7 ms,
    /// so it asks for the environment and reads the plain view, and the **work loop** builds it — the same shape
    /// [`Session::want_cooked_reading`] and `Cooking` have, for the same reason. Behind a lock because the asking
    /// happens in a query (`&self`) and the building happens in the loop (`&mut self`).
    macro_work: std::sync::Mutex<Cooking>,
    /// **The run's parsed macro definitions** — see `ParsedDefinitions`: a body parses once per run, not once per
    /// file that sees it. Keyed by the definition's own text, so it needs no invalidation: the same text is the same
    /// definition, and a file that changed contributes a different one.
    parsed_definitions: std::sync::Mutex<crate::ParsedDefinitions>,
    /// **A file's `CompilerConfig`, worked out once** — see [`Session::config_for_file`].
    ///
    /// [`CompilerConfig::for_file`] clones the whole configuration, applies the project's `remove_args`/`extra_args`
    /// and looks the file up in the compilation database, and it was being called by every `view` — **after** the
    /// parse became a cache hit, it was the whole cost of one (measured: 0.28 ms per hit on an 11 KB file, against
    /// 0.3 ms for the parse it was hiding behind).
    ///
    /// **No invalidation, and the reason is structural**: a session's configuration is built once by `Session::open`
    /// and nothing reassigns it. A configuration that changes is a *new session* (the server replaces the analysis
    /// state), which is the same rule the rest of this type follows.
    config_for_file: std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<PerFileConfig>>>,
    /// **The renderings built for `view`**, remembered against the content they were read from.
    ///
    /// The same arrangement as [`Session::macro_environments`] and for the same reason: a rendering is a unit walk
    /// and a preprocess, a query cannot pay it, and the file it is asked about does not change between two
    /// keystrokes. Keyed by the **content hash** rather than the path, because a buffer being typed in has a new
    /// text on every keystroke and a rendering of the old text would be a wrong answer rather than a stale one.
    /// **Surfaces already scanned**, shared by every effect walk — see [`crate::effect::Surfaces`].
    surfaces: std::sync::Mutex<crate::effect::Surfaces>,
    /// **Effects already computed**, shared by every walk — the other half of what makes a walk hash lookups.
    effects: std::sync::Mutex<HashMap<u64, crate::effect::Effect>>,
    renderings: std::sync::Mutex<
        cache::cache_lru::Cache<
            (PathBuf, u64),
            std::sync::Arc<crate::preprocess::cooked::RenderedCooked>,
        >,
    >,
    /// **Views already built**, by what they were parsed from — the answer to "a pull is a parse per request".
    ///
    /// [`Session::view`] parses a file's **rendering**, which for a header-heavy file is its whole closure; measured
    /// on a 0.74 MB unit at the parser's 6 MB/s, that is **120 ms per call**, and a hover, a pull, an inlay hint and
    /// a semantic-token request each call it. The tree is the expensive half and nothing was remembering it: the
    /// rendering cache holds the rendered *text*, and the parse was made again every time.
    ///
    /// # Why this key cannot serve a stale view
    ///
    /// A view is a function of: the text parsed (the rendering's), the rendering's spans (the offsets between the
    /// rendering and the file), the parse's own configuration (`cuda`, the language level), whether the buffer is
    /// open, and the file's identity. All seven are in [`ViewKey`], and the text is keyed by its **content hash** —
    /// the same rule the rendering cache uses, and stricter here, because a rendering whose text changed for any
    /// reason (a header edited, a macro moved) is a different key rather than a stale hit. A config change moves the
    /// language, an open/close moves `open`, a re-read of the file moves the text. Nothing else can change what a
    /// parse of that input means.
    ///
    /// `FileView` is cheap to hand out because its tree and its scopes are behind `Arc`s: a hit is a handful of
    /// reference-count bumps, which is what makes a cache worth having at all (a clone that copied the token stream
    /// would cost twelve bytes per token — megabytes for the units this exists for).
    ///
    /// The value is a [`crate::file::view::ParsedView`] and not a [`FileView`]: a view holds rowan's red root, which is
    /// neither `Send` nor `Sync`, and a session is shared across the server's threads. The root is one allocation,
    /// made per hit — see `FileView::from_parsed`.
    views: std::sync::Mutex<
        cache::cache_lru::Cache<ViewKey, std::sync::Arc<crate::file::view::ParsedView>>,
    >,

    /// **The parse trees of whole renderings, in a cache of their own** — see [`Cache::rendering_views`].
    ///
    /// A rendering view and a buffer view are different populations: one is a whole unit's closure (measured at
    /// ~75 MB of tree for `test_cpp/src/containers.cpp`), the other the few kilobytes a reader typed. Sharing one
    /// eight-entry cache let ten buffer views evict the rendering, so **every** request after them re-parsed the
    /// unit: private bytes went 190 MB → 7 500 MB over twelve rounds while the session's own tables stayed near
    /// 40 MB. The dispatch is on the key: `ViewKey::rendering` is `Some` exactly for a rendering.
    rendering_views: std::sync::Mutex<
        cache::cache_lru::Cache<ViewKey, std::sync::Arc<crate::file::view::ParsedView>>,
    >,
    /// **The summary of a rendering**, by `(path, content hash of the rendered text)`.
    ///
    /// A rendering is what the preprocessor resolved, so a declaration a directive splits has one answer there —
    /// `reference` for libstdc++'s `emplace_back` — where the file's own text has two. Reading it needs the *summary*
    /// of the rendered text, which is a parse: measured at **20.2 ms, and 19.1 ms again** on the second ask
    /// (`tests/rendering_real.rs`), because the rendering is cached by the cook path and its summary was not.
    rendering_summaries: std::sync::Mutex<
        cache::cache_lru::Cache<(PathBuf, u64), std::sync::Arc<crate::FileSummary>>,
    >,
    ///
    /// `cooking_materials` answers `None` for a file whose unit is provisional, whose summary is not in the index, or
    /// whose render declines — and the caller that wanted it (`want_the_closure_cooked`, once per drain) wants it
    /// again. Measured on a real session, that is a loop: the server's own log repeated
    ///
    /// ```text
    /// index slice: prepared 0, committed 0, write lock held 0 ms, 4 to cook, 1 left
    /// index slice: prepared 0, committed 0, write lock held 0 ms, 4 to cook, 2 left
    /// index slice: prepared 0, committed 0, write lock held 0 ms, 2 to cook, 0 left
    /// ```
    ///
    /// every second for as long as the window stayed open, at 28 % of a core and 916 MB.
    ///
    /// A refusal is not a conclusion about the file, only about *this* moment: [`Session::want_cooked_reading`] — a
    /// query that names the file — clears it and queues again, so nothing is lost but the retry.
    cooking_refused: std::sync::Mutex<std::collections::HashSet<String>>,
    /// **The files this session has already asked to cook** — so a closure is wanted once, not once per drain.
    ///
    /// `already_read` is the *index's* answer, and the index keeps a **bounded** map ([`crate::index::MAX_COOKED_FILES`]):
    /// a reading that is evicted reads as "nobody cooked this" again, and [`Session::want_the_closure_cooked`] — called
    /// for every open document on every idle drain — queued it again. Measured on `test_cpp` with twenty files open and
    /// **no request at all**: 486 cook commits for **179 distinct files** in 33 s, the cooked map 124 → 179, session
    /// tables 63 → 114 MB, and private bytes 600 → **900 MB** before a 1 GB cap killed the process. The work was not the
    /// project: it was the same headers, cooked over and over.
    ///
    /// A file that has been *asked for* is asked for once per text. A query that needs the reading after an eviction
    /// asks for it itself ([`Session::want_cooked_reading`], [`Session::view`]), and that is the asking side this loop
    /// is not. Cleared for a file in [`Session::buffer_changed`], which is the one place its text moves.
    cooked_or_wanted: std::sync::Mutex<std::collections::HashSet<String>>,
    /// **The renderings a query has already asked for, by text** --- so an evicted reading is not asked for again.
    ///
    /// [Session::cooked_or_wanted] is the pump's half of this rule and is keyed by file, because it is asked once
    /// per open document per drain. This is the query's half: iew (and iew_of_the_file) miss whenever the
    /// rendering cache does not hold the reading, and asking again on every miss was the measured shape of a server
    /// that never goes idle --- the pump cooked the same header once per drain, because a cache that had evicted it
    /// could not tell the asker it had ever been cooked. Keyed by (path, content hash) because the asker is looking
    /// at one text; cleared wherever a file's text moves, like the set above.
    renderings_asked: std::sync::Mutex<std::collections::HashSet<(std::path::PathBuf, u64)>>,
    /// **The document the reader is looking at** — the one a whole-program read is for.
    ///
    /// Set by every `did_open`/`did_change`/`did_save`, because that is the file a request has just named. It used to
    /// be *every* open document: twenty files open was twenty programs read, measured as **19 whole-program reads in a
    /// twenty-second idle window** on `test_cpp`, each re-parsing the same 116 headers. A program read is the most
    /// expensive thing this session does, and a background tab is not a reason to do it — the same trade
    /// `the_program_to_read` already states for files nobody has open.
    focused_document: std::sync::Mutex<Option<std::path::PathBuf>>,
    /// **What the client said each open document is** — `didOpen`'s `languageId`, reduced to a family.
    ///
    /// The client knows something no configuration in this session does: the *editor's* language mode for the file
    /// the reader is looking at. A `.h` is the case that matters — it is included by whichever language includes it,
    /// so `CompilerConfig::language_for` deliberately refuses to guess from the extension, and a header nobody has
    /// configured was read as **not told**. `DiagnosticCode::is_cpp_only`'s rule is "not told is not C", so a C++
    /// rule then fires in a C project's headers: measured on redis 8.x, **73 `cstyle-cast` findings, every one of
    /// them in a `.h`** (`docs/c-support.md` §8).
    ///
    /// Only the C answer is kept: it is the one that turns rules *off*, and a client saying `cpp` changes nothing
    /// this session would otherwise do.
    client_says_c: std::sync::Mutex<std::collections::HashSet<String>>,
    /// **How many cooks the disk answered** --- a cook that rendered, parsed and indexed nothing, because the file's
    /// record was already on the disk with its rendering. The number the per-file cache is judged by.
    stored_cooks: std::sync::atomic::AtomicUsize,
    /// **What this session's units have cost it** — see [`UnitStats`], which says what the three numbers mean.
    ///
    /// Atomics rather than one `Mutex<UnitStats>`: `translation_unit_of` takes `&self` and runs on the pump's read
    /// side, where the pump and a request can be inside it at once. Nothing here decides an answer, so a relaxed
    /// order is all a counter needs.
    unit_from_memory: std::sync::atomic::AtomicUsize,
    unit_from_disk: std::sync::atomic::AtomicUsize,
    unit_walked: std::sync::atomic::AtomicUsize,
    /// **How many times each root's timeline has been *walked*** — the diagnostic that answers "who keeps asking".
    ///
    /// `UnitStats::walked` is the total, and a total cannot tell one root walked two hundred times from two hundred
    /// roots walked once. The difference is the whole of the next optimisation, and this is the measurement that
    /// decides which one it is — see [`Session::walks_by_root`].
    walks_by_root: std::sync::Mutex<std::collections::HashMap<String, u64>>,
}

/// **One `import` a file writes**, as a link or a jump can use it — see [`Session::module_imports_of`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleImport {
    /// The module's name as written: `math`, `std.compat`.
    pub name: String,
    /// The **name's** own span in the file — what a document link covers, not the whole declaration.
    pub range: cpp_parser::SourceRange,
    /// The interface unit the name reaches, when the analysis can point at one.
    pub resolved: Option<PathBuf>,
}

/// **The macros the C and C++ *standards* define** — every compiler defines these, whatever the project's build system
/// says, and their values follow from the language the file is read as.
///
/// `ext
/// __STDC__            1                     both families
/// __STDC_HOSTED__     1                     both families (a freestanding implementation says 0 and this cannot know)
/// __STDC_VERSION__    201710L / 202311L     C99 and later, by the file's standard
/// __cplusplus          201703L / 202302L    every C++ standard
/// `
///
/// `None` for a name that is not one of them, and for `__STDC_VERSION__` in C89 — where it is deliberately undefined,
/// which is exactly the question `#ifdef __STDC_VERSION__` asks.
/// The **last directive that settles `name`** at or before `offset`, as its own text.
///
/// ```text
/// #define MAX(a, b) ((a) > (b) ? (a) : (b))      → the whole line, `\`-continuations joined
/// #define COUNT  10                            → `#define COUNT  10`
/// #undef  COUNT                               → `#undef COUNT` (the name is settled, and by an un-definition)
/// ```
///
/// A text scan rather than a parse: it is asked about the file the reader is in, at the moment they hover, and a
/// directive is the one construct whose meaning is entirely in its first line. `None` when no directive mentions the
/// name, or the last one *after* `offset` — a `#define` written below the point asked about does not apply there.
fn the_directive_defining(name: &str, text: &str, offset: usize) -> Option<String> {
    let mut settled: Option<String> = None;
    let mut at = 0usize;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        if start > offset {
            break;
        }
        let trimmed = line.trim_start();
        let rest = if let Some(rest) = trimmed.strip_prefix("#define") {
            rest.trim_start()
        } else if let Some(rest) = trimmed.strip_prefix("#undef") {
            rest.trim_start()
        } else {
            continue;
        };
        // The name is the first identifier of what follows, and the directive is `#define NAME` or `#define NAME(…)`.
        let end = rest
            .find(|character: char| !(character.is_alphanumeric() || character == '_'))
            .unwrap_or(rest.len());
        if &rest[..end] != name {
            continue;
        }
        // A definition only counts if it is written *before* the point asked about — or is the one the cursor is on.
        if start > offset {
            continue;
        }
        let mut directive = trimmed.trim_end().to_string();
        // `\`-continuations: a body written on the next line is part of the same directive.
        while directive.ends_with('\\') {
            let next_start = at;
            let Some(next) = text[next_start..].split_inclusive('\n').next() else {
                break;
            };
            at += next.len();
            directive.pop();
            directive.push_str(next.trim_end());
            if next.trim_end().ends_with('\\') {
                continue;
            }
            break;
        }
        settled = Some(directive.trim_end().to_string());
    }
    settled
}
fn the_standards_own_macro(
    name: &str,
    language: &cpp_parser::features::LanguageConfig,
) -> Option<String> {
    use cpp_parser::features::{Family, Standard};
    let family = language.family();
    let c_standard = language
        .standards()
        .filter(|standard| standard.family() == Family::C)
        .max();
    let cpp_standard = language
        .standards()
        .filter(|standard| standard.family() == Family::Cpp)
        .max();
    match name {
        "__STDC__" if family != Some(Family::Cpp) => Some("1".to_string()),
        "__STDC_HOSTED__" if family != Some(Family::Cpp) => Some("1".to_string()),
        "__STDC_VERSION__" if family != Some(Family::Cpp) => match c_standard? {
            Standard::C89 => None,
            Standard::C99 => Some("199901L".to_string()),
            Standard::C11 => Some("201112L".to_string()),
            Standard::C17 => Some("201710L".to_string()),
            Standard::C23 => Some("202311L".to_string()),
            // A C++ standard in the C slot cannot happen (max per family), and answering nothing is right.
            _ => None,
        },
        "__cplusplus" if family != Some(Family::C) => match cpp_standard? {
            Standard::Cpp11 => Some("201103L".to_string()),
            Standard::Cpp14 => Some("201402L".to_string()),
            Standard::Cpp17 => Some("201703L".to_string()),
            Standard::Cpp20 => Some("202002L".to_string()),
            Standard::Cpp23 => Some("202302L".to_string()),
            // **C++26 reports a draft value**, as every compiler does before the standard is published; GCC 14 and
            // Clang 18 both answer 202400L for -std=c++26.
            Standard::Cpp26 => Some("202400L".to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// **The standard's macros whose value depends on where they are written** — `__LINE__`, `__FILE__`, `__DATE__`,
/// `__TIME__`, and C++11's `__COUNTER__`.
///
/// They are macros in every translation unit, exactly as `__cplusplus` is, but neither table this session has can say
/// so: `-dM` does not print them (measured — `g++ -dM -E -x c++ NUL` answers `#define __cplusplus 201703L` and none of
/// these), and [`the_standards_own_macro`] answers *values*, which these have none of that a reading could fold:
/// `__LINE__` is the line it stands on. What a caller needs for a colour is the one bit — "this name is a macro" — so
/// this is its own list rather than a fake value in the other one.
fn a_macro_whose_value_depends_on_the_use(name: &str) -> bool {
    matches!(
        name,
        "__LINE__" | "__FILE__" | "__DATE__" | "__TIME__" | "__COUNTER__"
    )
}
impl Session<DiskFiles> {
    /// Open a project the way a language server does: ask the machine what it compiles with.
    ///
    /// The four steps, in the order their results depend on each other:
    ///
    /// ```text
    /// 0. .cppls.toml, if the project has one  — exclusions, source extensions, and how it says it is built
    /// 1. compile_commands.json                — the flags and the file list
    /// 2. discover(files, …)                   — which compiler, and where its own headers are
    /// 3. the scan                             — the sources the project owns
    /// ```
    ///
    /// **The compiler is run**, once, with `-E -v -x c++ -` to print its search list. That is a process and
    /// therefore tens of milliseconds, paid once at open; a project with no compiler on the machine is not an
    /// error, and [`Session::toolchain`] answers `None` — the analysis then resolves the project's own headers and
    /// reports `<vector>` as unresolved, which is the honest answer rather than a silently empty index.
    ///
    /// The file list is the **queue's seed**, not a list to consult later: opening a project is the caller saying
    /// "index this", so [`Session::pending`] counts the project from the first call and [`Session::advance`] has
    /// something to do before anything is opened.
    pub fn open(
        root: impl Into<PathBuf>,
        files: SessionFiles<DiskFiles>,
        filter: WatchFilter,
    ) -> Session<DiskFiles> {
        Session::open_with_config_file(root.into(), files, filter, None)
    }

    /// **Write this session's summaries to another file in the cache directory** — the server's is
    /// [`crate::cache::cache_db::DATABASE_FILE`], and a tool that indexes a project *beside* a running editor must not
    /// share it: each writer's `generation` is its own, and a collection on one side would evict rows the other is
    /// using. `cpp_inspect` writes `inspect.db`.
    ///
    /// Only before this session answers anything — see [`crate::SummaryStore::set_cache_file`].
    ///
    /// The **directory** stays the project's, because it travels with the checkout and the rows are as expensive to
    /// rebuild from the tool as from the server.
    pub fn with_cache_file(mut self, file_name: &'static str) -> Self {
        self.store.set_cache_file(file_name);
        self
    }

    /// [`Session::open`] for a caller that knows where the configuration file is (`--config`).
    ///
    /// A named file that is not there is reported rather than ignored — an instruction is not a convention — and
    /// everything else proceeds as if no configuration had been given.
    pub fn open_with_config_file(
        root: PathBuf,
        files: SessionFiles<DiskFiles>,
        filter: WatchFilter,
        config_file: Option<&Path>,
    ) -> Session<DiskFiles> {
        // **What the project is** — its own file and its build — worked out before anything is
        // asked of a compiler, because the first half decides *which* compiler gets asked (`named_compilers`) and
        // what flags it is asked about.
        let discovery = crate::project::discover(&files, &root, config_file);
        let project_config = discovery.config.clone();

        // **The project's own language wins.** `locale = "zh-CN"` in `.cppls.toml` is a statement about this
        // repository, and the editor's setting (`initialize.locale`) is a statement about the editor — so the file is
        // applied here, after the client's has already been applied. Applying it in the *analysis* rather than in the
        // server is what keeps the two in one order: whoever opens a project gets that project's messages, including
        // the messages produced while it is being indexed.
        if let Some(locale) = project_config.config.locale.as_deref() {
            cpp_parser::set_locale(locale);
        }

        let filter = apply_project_config(filter, &project_config);

        let for_file = discovery
            .database
            .as_ref()
            .and_then(|database| database.commands.commands.first())
            .map(|command| command.file.clone())
            .unwrap_or_else(|| root.clone());

        // **Which flags won, and then what the project does to them.** `[compile].args` is the compilation when a
        // person wrote it, and the database's first entry when the project's build says so. Either way the result
        // goes through one pass — `remove_args` drops,
        // `extra_args` appends — so that the three keys have one implementation and one order, whichever list they
        // start from.
        // **The project-wide answer, which a database can only approximate.** Its first entry is *a* compilation
        // rather than the project's — with one entry per translation unit there is no such thing — and this is the
        // one place that has to live with that: the defines here are what the **cook** seeds every file's macro
        // environment with, and a per-file define cannot reach that yet. The include paths, which are what decide
        // *which* header a file resolves, are per file where they are used — see `CompilerConfig::for_file`, which
        // the include resolver asks.
        let base = project_config_from_flags(
            &discovery.flags(),
            discovery.working_directory(),
            &project_config.config.compile,
        );

        // **The project root, so that an include nothing could place can be looked for in the tree.**
        //
        // The one search that is about where files *are* rather than where a compiler looked, and the reason it is
        // needed at all is that a Visual Studio solution cannot export a compile database and a checkout nobody has
        // configured has none. `[compile].fuzzy_includes` decides whether it runs; the root is recorded either way,
        // because a flag that is off must not make the rest of the configuration differ.
        let base = CompilerConfig {
            root: Some(root.to_path_buf()),
            // **On by default where it is the only thing that can work.** The flag is tri-state: `true` and `false`
            // are a project saying so, and *absent* means "as the situation decides" — on when the project has no
            // compile database and off when it has one. A database states where the compiler looked, so a guess can
            // add nothing there; without one, the alternative is a closure of fourteen files instead of the program,
            // which is what a Visual Studio solution and an unconfigured checkout both produce.
            fuzzy_includes: project_config
                .config
                .compile
                .fuzzy_includes
                .unwrap_or(discovery.database.is_none()),
            targets: crate::project::targets_for(&project_config.config.compile),
            // **The libraries a target may depend on** — `[[library]]`, resolved at the moment the file's config is
            // applied (see `CompilerConfig::for_file`), because that is where a file and its target meet.
            libraries: project_config.config.libraries.clone(),
            database: discovery
                .database
                .as_ref()
                .map(|database| std::sync::Arc::new(database.commands.clone())),
            remove_args: project_config.config.compile.remove_args.clone(),
            extra_args: project_config.config.compile.extra_args.clone(),
            ..base
        };

        // **The standard the project states in its own file**, which is the person's word and so wins: the key
        // exists because `-std=` is the one flag nearly every project sets and nearly every target restates.
        let base = match (&project_config.config.compile.standard, &base.standard) {
            (Some(stated), _) => base.with_standard(stated.clone()),
            (None, _) => base,
        };

        // **The standard the project's own build states**, read out of the compilation database's command line by
        // `discovery.standard()`, applied only when nothing above it stated one: what the build ran is closer to the
        // truth than a compiler's default, and a project's own `args` are closer still.
        let base = match (base.standard.clone(), discovery.standard()) {
            (None, Some(standard)) => base.with_standard(standard),
            _ => base,
        };

        // The compilers the *project* named, in their order (`discovery.named_compilers`): a project that says
        // `clang++` gets clang's headers, and a project whose build names a compiler gets that one — before
        // `CXX`, before the platform's own, and before anything on `PATH`.
        let environment = Environment::current();
        let layout = crate::include::msvc::WindowsLayout::current();
        let named = discovery.named_compilers();

        // **The compiler's answers are cached under the project's own cache**, beside the summaries they are the
        // environment of: a warm open replays them instead of running a compiler (`Session::open` measured 141 ms,
        // almost all of it the invocations). See [`toolchain::CachedCommands`].
        let toolchain_cache = project_config
            .config
            .index
            .cache_dir
            .clone()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.join(".cppls"))
            .join("toolchain");
        let commands = toolchain::CachedCommands::new(toolchain_cache);
        let toolchain = toolchain::discover_with(
            &files,
            &commands,
            &named,
            // **What the project says this file is built as**: the database's entry for it, or — when the database
            // states nothing — the standard the configuration already resolved (`[compile] args = ["-std=c++17"]`,
            // the build's `-std=`). The compiler has to be asked about *that* standard: its
            // `__cplusplus` is what every `#if` in every header is a question about, and a table printed for some
            // other language version is a table that silently says `<format>` is empty.
            toolchain::BuildStatement {
                commands: discovery
                    .database
                    .as_ref()
                    .map(|database| &database.commands),
                standard: base.standard.as_deref(),
            },
            &for_file,
            &environment,
            &layout,
        );

        let config = match &toolchain {
            Some(found) => found.config(&base),
            None => base,
        };

        // **And what the project says about CUDA**, which is the one layer discovery cannot work out on a machine
        // where `nvcc` is not installed: the toolkit's location, the host compiler's name, extra includes. Applied
        // after the toolchain so that `enable` can override a discovery that found no `nvcc` — see
        // `crate::project::cuda::CudaSection`.
        let config = match &project_config.config.compile.cuda {
            Some(cuda) => cuda.applied_to(config, &root),
            None => config,
        };

        // **And what it says about Unreal Engine** — where the engine is installed, so that `#include "CoreMinimal.h"`
        // and `#include "GameFramework/Actor.h"` resolve the way a build resolves them, plus the macros an engine build
        // defines and one `<MODULE>_API` per module the project's `*.uproject` names. Applied after the toolchain and
        // the database, so that a define the build itself stated wins. See `crate::project::ue::UnrealSection`.
        let config = match &project_config.config.unreal {
            Some(unreal) => unreal.applied_to(config, &root),
            None => config,
        };

        // **Which compiler this is**, asked of the compiler rather than guessed from the flags: GCC and Clang
        // predefine `__GNUC__`, MSVC predefines `_MSC_VER`, and they arrive in the table the same call that gave
        // the search paths (see `toolchain::discover`). It matters to the *parser*, which has to know what
        // `__int128` means to the compiler reading the file — see `CompilerConfig::dialect`.
        //
        // No toolchain, no answer: the configuration keeps its default, and a caller that knows the target
        // (`with_config`) sets it itself.
        let config = match toolchain.as_ref().and_then(|found| found.dialect) {
            Some(dialect) => config.with_dialect(dialect),
            None => config,
        };

        let session = Session::assemble(root, files, filter, config, toolchain, discovery);
        // (the file-cache sweep is gone with the file cache: `cache.redb` collects its own generations —
        // `SummaryStore::collect_the_cache`.)
        session
    }
}

/// **Everything a cook needs, gathered under the write lock and then used without it.**
///
/// Owned rather than borrowed on purpose: the caller has to let go of the session between
/// `Session::cooking_materials` and `Session::render_a_cooked`, so nothing here may borrow it. The unit is an
/// `Arc` the cache already holds, and the seed is a table built from the index's own macros.
/// **A file, rendered and parsed, waiting to be filed** — and the hash of the text it was made from.
///
/// The hash is the whole reason this is a struct rather than the pair it used to be. A cook is **two halves under
/// two locks** ([`Session::render_a_cooked`] and [`Session::commit_a_cooked`]) and the file can be edited between
/// them — at which point the reading describes text nobody has any more. Committing it files a stale reading *under
/// the new text*, and nothing re-cooks a file that has a cooked reading: measured, a pull right after an edit
/// answered `reading Cooked, 0 error(s)` for a file whose `;` had just been deleted. `text_hash` is what lets the
/// commit say "this is not the text any more" and drop it.
pub struct RenderedCook {
    /// `cache::content_hash` of the text the rendering was made from.
    pub text_hash: u64,
    pub indexed: crate::IndexedRendering,
    pub reading: CookedReading,
    revision: SessionRevision,
}

pub struct CookingMaterials {
    /// The index's own spelling of the path, which is what every other layer keys on.
    pub path: PathBuf,
    /// The summary key the rendered reading is filed under.
    pub key: SummaryKey,
    /// **The timeline whose view answers positionally**, when one covers the file — `FileMacros` over its
    /// definitions, which is what the reading has always used where a unit exists.
    pub unit: Option<std::sync::Arc<crate::TranslationUnit>>,
    /// **The composed starting state**, for a file no timeline covers: `Over(configuration, seed)`.
    ///
    /// Two fields rather than one because they are not equivalent: a `MacroView` is a **position** in a walk,
    /// so it answers `#define`s that arrive mid-file and directives that split a declaration; a materialised
    /// `Configuration` is the state a file *starts* with. The walk stays preferred wherever it exists.
    pub configuration: Option<crate::preprocess::cooked::Configuration>,
    /// The file's own text.
    pub text: String,
    /// **The hash of that text**, which is what the commit compares the file's text against — see
    /// [RenderedCook].
    pub text_hash: u64,
    /// What the compilation defines, before the file's own `#define`s.
    pub seed: MacroTable,
    /// The names the file's raw reading already declares, so the render's addition can be counted.
    pub already_declared: std::collections::HashSet<String>,
    revision: SessionRevision,
}

pub struct DetachedCook<F: FileProvider> {
    files: SessionFiles<F>,
    config: CompilerConfig,
    materials: CookingMaterials,
}

impl<F: FileProvider + Clone> DetachedCook<F> {
    pub fn render(self) -> Option<RenderedCook> {
        Session::<F>::render_cook(&self.files, &self.config, &self.materials)
    }
}

/// **What a wave prepared, and the units the pass that follows it will need.**
///
/// Both halves are the same kind of thing: work that is a pure function of the files and the index, done under the
/// read lock so that the write lock only has to *insert*. The units belong here for a measured reason — a unit walk
/// held the writer for **1003 ms** on the first slice after a cold start, which was the largest single thing a drain
/// did — and they can be built in the preparing half because `Session::units_for_the_pass` takes `&self`: the two
/// caches a walk needs (the units table and the macro definitions) have locks of their own rather than needing
/// `&mut self`.
#[derive(Default)]
pub struct PreparedWave {
    /// The summaries `Session::commit_a_wave` is about to put into the index.
    pub prepared: Vec<(PathBuf, crate::index::store::Prepared)>,
    /// The timelines the second pass reads its environments out of, for the files this wave will parse.
    pub units: Vec<std::sync::Arc<crate::TranslationUnit>>,
    /// **Refined readings the cache already holds for the slice this wave will drain.**
    ///
    /// The second pass refines a file against its own closure, and its answer is a function of that closure — so a
    /// stored one is served instead of being rebuilt, and the files it covers need no timeline at all. This is what
    /// makes a second start cheap: the pass costs a lookup rather than a parse per file.
    pub refinements: Vec<crate::FileSummary>,
    revision: Option<SessionRevision>,
}

pub struct FileReadMaterials<F: FileProvider> {
    path: PathBuf,
    revision: SessionRevision,
    input: crate::index::store::DetachedRead<F>,
    view: Option<BufferViewMaterials>,
}

pub struct PreparedFile {
    path: PathBuf,
    revision: SessionRevision,
    prepared: crate::index::store::Prepared,
    view: Option<PreparedBufferView>,
}

#[derive(Clone)]
struct BufferViewMaterials {
    file: VfsFile,
    cuda: bool,
    /// The language the file is compiled as — see [`FileView::parse_with_cuda`].
    language: Option<cpp_parser::features::LanguageConfig>,
    environment: Option<Arc<cpp_parser::MacroEnvironment>>,
}

struct PreparedBufferView {
    key: ViewKey,
    parsed: ParsedView,
    weight: usize,
}

impl BufferViewMaterials {
    fn parse(self) -> PreparedBufferView {
        let key = buffer_view_key(&self.file, self.cuda, self.environment.clone());
        let macros = self.environment.unwrap_or_default();
        let view = FileView::parse_with_cuda(&self.file, macros.as_ref(), self.cuda, self.language);
        let weight = a_view_weighs(&view);
        PreparedBufferView {
            key,
            parsed: view.into_parsed(),
            weight,
        }
    }
}

impl<F: FileProvider> FileReadMaterials<F> {
    pub fn read(self) -> PreparedFile {
        let (prepared, view) = match self.view {
            Some(view)
                if view.file.text.len() >= 16 * 1024
                    && std::thread::available_parallelism().is_ok_and(|count| count.get() > 1) =>
            {
                std::thread::scope(|scope| {
                    let worker_view = view.clone();
                    let worker = std::thread::Builder::new()
                        .name("cpp-ls-buffer-view".to_string())
                        .stack_size(16 * 1024 * 1024)
                        .spawn_scoped(scope, move || worker_view.parse());
                    match worker {
                        Ok(worker) => {
                            let prepared = self.input.read();
                            let view = worker
                                .join()
                                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
                            (prepared, Some(view))
                        }
                        Err(error) => {
                            log::warn!("could not start a buffer-view worker: {error}");
                            (self.input.read(), Some(view.parse()))
                        }
                    }
                })
            }
            Some(view) => (self.input.read(), Some(view.parse())),
            None => (self.input.read(), None),
        };
        PreparedFile {
            path: self.path,
            revision: self.revision,
            prepared,
            view,
        }
    }
}

/// **What a drained slice still has to do, decided but not paid for.**
///
/// The write lock's last remaining expenses — other than the inserts themselves — were the second pass
/// ([`SummaryStore::prepare_the_re_read`], 150 ms per file measured) and one macro environment (65–190 ms). Neither
/// changes the session when it is *worked out*; both were inside the writer only because the code that decided to do
/// them was. So the decision is made here, cheaply, under the write lock, and the work is handed to
/// [`Session::prepare_the_drain`], which takes `&self`.
///
/// A plan that is prepared and never committed is dropped, exactly like a [`PreparedWave`] that is never committed:
/// the files it named are re-read when something asks about them again.
#[derive(Default)]
pub struct Drain {
    /// The files the second pass will re-read, claimed from the backlog by this plan — see
    /// [`Session::parsed_since_the_last_pass`].
    files: Vec<PathBuf>,
    /// The timelines that pass reads its environments out of, built for exactly this slice by
    /// [`Session::prepare_a_wave`].
    units: Vec<std::sync::Arc<crate::TranslationUnit>>,
    /// The macro environments a query asked for and has not got — up to `MACRO_SLICE` of them.
    environments: Vec<PathBuf>,
    revision: Option<SessionRevision>,
}

impl Drain {
    /// Is there nothing to do? A caller that would take a lock only for this can skip it.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.environments.is_empty()
    }
}

/// **What [`Session::prepare_the_drain`] made** — the re-reads built and the environments rendered.
///
/// **A whole program is not in here**, and that is the point of [`UnitReadMaterials`]: it costs 4.9 s, and a drain is
/// planned under the writer. The pump takes it as its own three phases, with every lock dropped for the middle one.
#[derive(Default)]
pub struct PreparedDrain {
    /// The second pass, planned: `None` when the slice's backlog was empty.
    re_read: Option<crate::index::store::ReRead>,
    /// The rendered environments, keyed the way [`Session::known_rendering_of`] looks them up: the file, and the
    /// hash of the text it was rendered from.
    rendered: Vec<(
        (PathBuf, u64),
        std::sync::Arc<crate::preprocess::cooked::RenderedCooked>,
    )>,
    revision: Option<SessionRevision>,
    retry_files: Vec<PathBuf>,
    retry_environments: Vec<PathBuf>,
}

/// **A unit read whose expensive half is done** — see [`Session::prepare_a_unit_reading`].
pub struct PreparedUnitReading {
    root: PathBuf,
    revision: SessionRevision,
    /// The hash of the rendering these facts were read from — what the memo and the database file them under.
    program: u64,
    indexed: crate::IndexedUnit,
    /// **Every file the unit was rendered from** — the module's closure, which is what an importer of it sees.
    ///
    /// Carried out of the read rather than looked up afterwards because the rendering is consumed there: a **cold**
    /// read cooks it inside `read_the_program`, so the only layer that ever holds it is the caller of this function —
    /// and a warm read has it earlier, at gather time. Recording it in both places is what keeps the answer from
    /// depending on whether the cache was warm (measured: a cold run still answered 34 completions where a warm one
    /// answered 38).
    walked: Vec<PathBuf>,
}

/// **What a program read needs, gathered under the session lock and read without it.**
///
/// The three parts of a unit read, and the measurement that decides where each one is paid
/// (`tests/unit_read_probe.rs`, a 159-file / 7.07 MB program, 5.09 s in total):
///
/// ```text
/// gather   the walk, the closure's texts, two cache reads        44 ms      needs &Session — this type comes from it
/// read     the cook (1.25 s), the parse and the sweep (3.66 s)   ~4.9 s     needs none of it — `read_the_program`
/// commit   one insert_cooked per file, and queue.mark_worked     a few ms   needs &mut Session
/// ```
///
/// So the caller may drop **every** lock between [`Session::gather_a_unit_read`] and
/// [`Session::commit_a_unit_reading`], which is the difference between a whole-program reading that a keystroke waits
/// five seconds for and one it does not wait for at all. The session's own schedule
/// ([`Session::a_unit_may_be_read_now`]) only allowed programs under a megabyte while this split did not exist; the
/// budget is about a pathological closure now, not about a person's typing speed.
pub struct UnitReadMaterials<F: FileProvider> {
    root: PathBuf,
    revision: SessionRevision,
    /// The compilation context, which is part of the key a summary is filed under.
    key: SummaryKey,
    /// The key the rendering is filed under: the content hash of every file the walk entered, folded.
    units_key: u64,
    /// The walked timeline — **absent when a stored rendering made the walk unnecessary**.
    ///
    /// The key a rendering is filed under folds the walk's file list, and a rendering now records that list
    /// (`RenderedUnit::walked`), so a session that has already walked a program can find the rendering again without
    /// re-walking it. What is missing then is the timeline, and it is only needed to **cook** — a served rendering is
    /// already cooked.
    unit: Option<std::sync::Arc<crate::TranslationUnit>>,
    /// The text of every file the walk entered, by path — what the cook reads.
    sources: HashMap<PathBuf, String>,
    /// What the compilation itself defines ([`crate::Marked`]), which no file in the closure knows.
    macros: crate::Marked,
    config: CompilerConfig,
    files: F,
    /// The store's own database, for the two rows a program read reads and writes.
    database: Option<std::sync::Arc<cache::cache_db::SummaryCache>>,
    /// The rendering's hash, when the rendering was already in hand.
    program: Option<u64>,
    /// The rendering, when this session or the database already had it.
    rendered: Option<crate::RenderedUnit>,
    /// The facts, when they were already read.
    known: Option<crate::IndexedUnit>,
}

impl<F: FileProvider> UnitReadMaterials<F> {
    /// The root this program is read for, by the index's spelling.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// **The key the rendering is filed under** — what [`Session::prepare_a_unit_reading`] remembers per root.
    pub fn units_key(&self) -> u64 {
        self.units_key
    }

    /// **Read the program** — the cook, the parse and the sweep, and no session at all.
    ///
    /// The order is the one the measurements put in: facts if they are known, then the rendering (a cook, or the one
    /// gathered), then the facts again by the rendering's own hash, then the one parse. Both rows are written back,
    /// so the next session decodes instead of computing — 63 ms against 3.66 s on the measured program.
    pub fn read_the_program(self) -> Option<PreparedUnitReading> {
        if let Some(indexed) = self.known {
            return Some(PreparedUnitReading {
                root: self.root,
                revision: self.revision,
                program: self.program?,
                indexed,
                walked: self
                    .rendered
                    .as_ref()
                    .map(|r| r.walked.clone())
                    .unwrap_or_default(),
            });
        }

        let stream = match self.rendered {
            Some(rendered) => rendered,
            None => {
                // **A cook needs the walk**, and the fast path (`Session::unit_roots`) is the one that has no timeline.
                // Materials without a unit are only ever built *with* a rendering, so this is unreachable rather than
                // an answer, and saying so is better than cooking a program from texts nobody walked.
                let unit = self.unit.as_ref()?;
                let definitions = unit.definitions();
                let seed = the_seed_for(&self.config, &self.macros, &self.root);
                // **The include search, so `__has_include` is answerable.** Without it the operator answers
                // `Unknown`, which is honest but decides nothing, and `#if __has_include(<span>)` is a shape real
                // headers are full of. See [`Session::render_the_unit`].
                let search = crate::preprocess::cooked::Search::new(&self.files, &self.config);
                let cooking = std::time::Instant::now();
                #[cfg(feature = "perf-probe")]
                crate::perf::mark("before the cook");
                let mut stream = unit.cook_the_unit(
                    &self.sources,
                    &definitions,
                    Some(&seed),
                    true,
                    Some(&search),
                );
                #[cfg(feature = "perf-probe")]
                crate::perf::mark("cook");
                if crate::probes::units() {
                    eprintln!(
                        "[unit] cook  {:>6.1} ms  ({} file(s), {} bytes)",
                        cooking.elapsed().as_secs_f64() * 1000.0,
                        stream.files.len(),
                        stream.text.len()
                    );
                }
                // **And the walk's own file list, which is what its key folds.** `files` names only the files that
                // contributed tokens; a header of nothing but `#define`s writes none, and a rendering that did not
                // record it could never reproduce the key it is filed under — see `RenderedUnit::walked`.
                // **Deduplicated**, because `files()` yields one entry per *inclusion* and the key folds one entry per
                // path (`the_units_inputs` collects into a map). A file included twice is one input.
                stream.walked = unit
                    .files()
                    .map(|path| path.to_path_buf())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                // **Kept under the key, provisionally or not — the key is what makes that safe.** It folds every
                // `(path, content)` the walk entered, so a rendering of a closure that was still filling has a key
                // the finished closure cannot repeat: the row is never hit again, it is merely wasted.
                if let Some(database) = &self.database {
                    let _ = database.put_unit(self.units_key, &stream);
                }
                stream
            }
        };

        let program = crate::cache::content_hash(&stream.text);
        if let Some(database) = &self.database
            && let Ok(Some(known)) = database.indexed_unit(program)
        {
            return Some(PreparedUnitReading {
                root: self.root,
                revision: self.revision,
                program,
                indexed: known,
                walked: stream.walked.clone(),
            });
        }

        let indexer = FileIndexer::new(&self.files, &self.config);
        let indexed = indexer.index_unit_rendering(&self.root, &stream, self.key);
        if let Some(database) = &self.database {
            let _ = database.put_indexed_unit(program, &indexed);
        }

        Some(PreparedUnitReading {
            root: self.root,
            revision: self.revision,
            program,
            indexed,
            walked: stream.walked.clone(),
        })
    }
}

/// **How the effect chain reads files**: the session`s own provider, and the include graph the index resolved.
struct SessionReads<'a, F: FileProvider> {
    files: &'a F,
    index: &'a crate::ProjectIndex,
}

impl<F: FileProvider> crate::effect::Reads for SessionReads<'_, F> {
    fn read(&self, path: &std::path::Path) -> Option<String> {
        self.files.read(path)
    }

    fn resolve(&self, from: &std::path::Path, spelling: &str) -> Option<PathBuf> {
        // **The resolution the index already made.** A summary records where each `#include` resolved to, so the
        // chain does not re-run the search: a second search would be a different answer to the same question, which
        // is the reason `IncludeFact::resolved` is stored at all.
        let wanted = spelling.trim_matches(|c| c == '"' || c == '<' || c == '>');
        let summary = self.index.summary(from)?;
        summary
            .includes
            .iter()
            .find(|include| include.spelling.trim() == wanted)
            .and_then(|include| include.resolved.clone())
    }
}

/// **What a cooking slice claimed and produced** --- the two kinds of reading, re-exported because the server's
/// pump gathers and commits them from outside this module. See [cook::AClaimedSlice].
pub use cook::{AClaimedCook, AClaimedSlice};
mod calls;
pub mod completion;
mod constrained;
mod cook;
/// **What a file's words are**, for the highlighter: the query layer's own vocabulary, next to the request that
/// wants it.
pub mod cuda;
/// **A missing member's definition as text** --- the code action behind a diagnostic, which is a session question
/// because the class it belongs to may be declared in another file.
pub mod definition;
mod diagnostics;
/// **What a documentation comment refers to** --- the names a hover draws as links.
pub mod doc;
mod edits;
pub use edits::ReconcileAnswer;
/// **The regions a file can hide** --- folds, read from the tree and the directives.
pub mod folding;
/// **The format string** --- which {...} and %... of a literal is what, for the hover that explains it.
pub mod format;
mod indexing;
/// **The types a file does not write** --- inlay hints, from the reader the hover uses.
pub mod inlay;
mod lifecycle;
mod memory;
mod navigation;
mod pump;
mod queries;
/// **What each name in a file is** --- the classification the highlighter is drawn from.
pub mod semantic;
/// **Call signatures** --- which function is being called, and which of its parameters the cursor is in.
pub mod signature;
mod units;
mod views;

pub use queries::WrittenAttribute;

/// Which reading answered a file's diagnostics — see [`Session::diagnostics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticReading {
    /// The text a **compiler** parses: the file's bytes with its translation unit's macros expanded and the branches
    /// nobody takes left out.
    ///
    /// **There was a `Raw` variant beside this one and it is gone.** It meant "the file's own bytes, unexpanded —
    /// what a reader editing the file sees, macros and untaken branches included", and it was reachable: a file the
    /// index held no cooked reading for was answered from its own text. That answer was not a lesser one, it was an
    /// **untrue** one — an untaken branch is not compiled, so an error reported inside one is an error about a
    /// program that does not exist. Measured on the fixture below: the same `#if 0` branch produced 1 error as the
    /// file's text and 0 once cooked.
    ///
    /// A caller that needs diagnostics for an unexpanded file has nothing to read: it asks
    /// [`Session::diagnostics`], gets `None`, and comes back — the same deferral [`Session::view`] answers with, and
    /// for the same reason. The variant is deleted rather than left reachable-but-unused because a variant nothing
    /// produces is an invitation to match on it.
    Cooked,
}

/// The type of the expression at a cursor — see [`Session::type_at`].
///
/// Three fields rather than one, because a consumer that shows the type needs to say *about what*: the expression's
/// own text is what the user recognises, and the file is where the declaration the type was read from lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpressionType {
    /// The expression's own text, as the file writes it — `this`, `*p`, `make(1)`, `w.size`.
    pub expression: String,
    /// The type, spelled the way the declaration it was read from spells it. Not a canonical type: an alias is an
    /// alias name, and `const Widget&` is those three tokens, because that is what the file says.
    pub type_of: String,
    /// The file that declaration is in.
    pub file: PathBuf,
}

/// One thing to report, with **file** offsets and the message as the parser wrote it.
///
/// Offsets rather than a line and a column because the line index belongs to the layer that holds the text
/// ([`Session::view`]) — and a range, because an error that covers one character and one that covers a declaration
/// are different claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiagnostic {
    pub start: usize,
    pub end: usize,
    pub message: String,
    /// **Which rule produced this**, as the enum the analysis defines — see
    /// [`crate::sema::check::DiagnosticCode`].
    ///
    /// Carried rather than derived from the message, because a client needs a *stable* name: `Diagnostic::code`
    /// is what an editor shows and what a settings file turns a rule off by, and a consumer that parsed the
    /// sentence would be reading prose as an identifier.
    pub code: crate::sema::check::DiagnosticCode,
}

/// **Something the analysis knows that is not an error** — see [`Session::notes_about_the_modules`].
///
/// The same offsets as [`FileDiagnostic`] and a different kind of claim: an import whose module this project does not
/// contain is not a mistake in the file, it is a fact about where the module lives. A consumer shows it as
/// information, and a type that merged the two would have to carry a severity field whose only honest values are
/// "error, for the errors" and "information, for this" — which is the same as having two types, with one of them
/// harder to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleNote {
    pub message: String,
    pub start: usize,
    pub end: usize,
}

/// **The interface unit a module name or partition names**, as a jump needs it — see
/// [`Session::module_unit_at`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleUnitTarget {
    /// The interface unit that declares the module (or the partition).
    pub file: PathBuf,
    /// The name as the language spells it: `geometry:shapes` for a partition, `geometry` for a module.
    pub name: String,
}

/// What a diagnostics channel should say about one file — see [`Session::diagnostics`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiagnostics {
    /// Which reading answered. Nothing about the list depends on it; a caller that explains itself to a user
    /// ("this file is read as the compiler reads it") needs it.
    pub reading: DiagnosticReading,
    /// How many errors the reading found that **cannot be shown here** — text written in another file's macro body,
    /// or a token whose spelling exists nowhere. Only a cooked reading can have any, and a caller that reports
    /// [`FileDiagnostics::errors`] as the whole truth should mention this number when it is not zero.
    pub unplaced: usize,
    pub errors: Vec<FileDiagnostic>,
    /// **Things the analysis knows about this file that are not errors** — see
    /// [`Session::notes_about_the_modules`].
    ///
    /// Here rather than in a call of its own because both answers come out of the same tree, and a consumer that
    /// asked twice would pay for the file's parse twice. Empty is the ordinary case.
    pub notes: Vec<ModuleNote>,
    /// **What the analysis says is wrong with the file** — see [`crate::sema::check`].
    ///
    /// The third channel, and the one that is neither the parser's nor a note: a construct the grammar accepts
    /// and the language does not. Every entry is a claim the analysis can stand behind — a check reports only
    /// `Known::Yes` or `Known::No`, and never an absence of an answer — so a consumer may show all of them
    /// without hedging, which is the property the layer exists to have.
    pub checks: Vec<crate::sema::check::Finding>,
    /// **Were the checks withheld because this file's closure is not read yet?**
    ///
    /// A check reads the *index*, and an index still being built answers absence as if it were a fact — see
    /// [`Session::is_closure_ready`] and the gate in `cpp_ls::handlers::diagnostic`. The gate used to be asked by the
    /// server **after** this call had already run every rule, so a pull whose findings could not be published paid
    /// for them anyway: measured on a 4 MB translation unit, four rules resolving types is seconds. The question is
    /// asked here now, before [`crate::sema::check::run`], and this flag is what tells a caller *why*
    /// [`FileDiagnostics::checks`] is empty — which is not the same answer as "nothing is wrong".
    pub checks_withheld: bool,
}

/// **What reading a file the way a compiler reads it produced** — see [`Session::cook`].
///
/// Counts rather than a summary, because the summary has already gone where it belongs: the index holds the
/// declarations and the errors (that is the point of the call), and what a caller — a status line, a test, a log —
/// wants to know is whether the reading found anything the file's own text did not, what it had to say about it, and
/// whether every range could be placed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CookedReading {
    /// How many declarations the cooked reading found.
    pub declarations: usize,
    /// How many of them the file's own text does not declare — the names that exist only after expansion.
    pub only_after_expansion: usize,
    /// How many errors the parse of the rendering reported **and this file can show** — see
    /// [`crate::IndexedRendering::diagnostics`].
    pub diagnostics: usize,
    /// How many it reported that this file cannot show, because the text they are about is not written here (a
    /// grammar error inside another file's macro body, or a pasted token that exists nowhere).
    pub unplaced: usize,
    /// How the ranges mapped back into the file; see [`crate::MapReport`].
    pub mapped: crate::MapReport,
}

/// How many translation units the session keeps in memory.
///
/// A unit is a timeline over a whole include closure, so it is the session's largest single object; the disk cache
/// holds the rest ([`crate::TranslationUnitCache`]) and a unit that falls out of here is one decode away. **Eight**
/// is enough for the units a reader is working in — the open file it is looking at and the headers beside it — and
/// small enough that a project of a thousand sources does not hold a thousand timelines.
///
/// It was sixteen, and the measurement that halved it is at [`MAX_UNIT_BYTES`]: a session that opened twenty files
/// held sixteen timelines for 40.4 MB and re-asked for them **twice**.
///
/// **A count is not a budget.** Measured on `EmmyLuaCodeStyle`, a unit is **6.2 MB** after the
/// second pass — eight of them are 50 MB, and a header-heavier project would have eight units
/// of 25 MB without this constant changing. So the table is bounded by **bytes** as well:
/// [`MAX_UNIT_BYTES`].
const MAX_UNITS: usize = 8;

/// **How many bytes of translation units the session keeps** — the budget the count above cannot express.
///
/// An entry larger than the whole budget is still kept (the walk that just produced it is the one the caller is
/// about to use), and the eviction order prefers the smallest closure first so the units that answer the most
/// questions survive.
///
/// # Why 128 MB became 64, measured
///
/// The table is the session's largest single object and it was sized when the **second pass** walked units for every
/// file it re-read — a phase-2 change removed that caller, and what uses the table now is a reader cooking one file
/// at a time. Measured with `tests/memory_probe.rs` on `EmmyLuaCodeStyle`, twenty files opened, cooked and cooked
/// again:
///
/// ```text
/// sixteen timelines held                 40.4 MB
/// twenty files cooked a second time        2 hits, 23 walks   (the rendering cache answers the rest)
/// ```
///
/// So the tail of sixteen is not earning 40 MB. Eight keeps the reader's working set — the file it is looking at and
/// the headers beside it — and halves the bill; a timeline that falls out is a 38 ms walk, and the rendering cache
/// in front of it is what a re-cook usually hits anyway.
const MAX_UNIT_BYTES: usize = 64 * 1024 * 1024;

/// **How many timelines the closure-dedup table keeps.** The byte budget is the real bound; this is the backstop
/// against a project of thousands of tiny closures — where 512 entries are a budget of their own, on a project whose
/// closures are a few kilobytes each. 32 is what a reader's recent history needs; the block below has the measurement
/// that took both numbers down.
const MAX_WALKS_BY_CONTENT_ENTRIES: usize = 32;

/// **How many bytes of closure timelines the dedup table keeps** — see [`Session::walked_by_content`].
///
/// Before this bound the table was a `HashMap` that never dropped anything: measured on a cold start it held
/// **384 entries / 222 MB** after the second pass, and every edit of a root adds a new content identity.
///
/// # Why 64 MB became 16 MB, measured
///
/// The cache's justification was a mass indexer: "2110 roots, 786 distinct closures, **46 % of walks removable**".
/// That caller no longer exists — phase 2 stopped building units for the second pass — and what remains is a reader
/// cooking one file at a time, where a hit needs two roots whose *closure content* is identical. Measured with
/// `tests/memory_probe.rs` on two real projects:
///
/// ```text
/// EmmyLuaCodeStyle   20 files opened and cooked   20 walks, 0 from memory   46.6 MB held
/// clice             20 files opened and cooked   20 walks, 0 from memory    0.6 MB held
/// ```
///
/// So on both projects the table's hit rate is **zero** and its bill on the first is 46.6 MB, of which 40.4 MB is
/// the same allocation the unit table also holds. A cap of 16 MB keeps what the cache is good at — the sibling roots
/// that do share a closure — and bounds the waste at a quarter of what it was. The *useful* table here is the unit
/// table, which holds a root's own timeline for its own cooks and pulls; this one is speculation.
const MAX_WALKS_BY_CONTENT_BYTES: usize = 16 * 1024 * 1024;

/// **How many files' configurations a session memoizes** — see [`Session::config_for_file`].
///
/// A reader meets a few dozen files; 256 is far past that and small enough that the memo is a rounding error in the
/// memory report. Reaching it clears the whole memo rather than evicting one entry.
const CONFIG_FOR_FILE_ENTRIES: usize = 256;

/// How many rounds [`Session::read_the_modules_a_file_imports`] may read module interface units in.
///
/// A round reads every module an already-read file imports and could not name a file for, so the number of rounds is
/// the **depth** of the module graph rather than its size — one for `import std;`, two for a module that imports a
/// module that imports a module. Sixteen is far past anything a real project reaches and bounds a graph that is
/// merely wrong: an interface unit that imports a module whose interface unit imports it back is a cycle, and a cycle
/// terminates on its own (a path already read is never read again), so this is the guard for the case the cycle rule
/// does not cover — a module graph that keeps producing *new* files, which only a scan that resolves module names to
/// the wrong files can do.
const MAX_MODULES_READ_AT_ONCE: usize = 16;

/// The in-memory translation units, least recently used out first.
#[derive(Default)]
pub(crate) struct UnitTable {
    held: std::collections::HashMap<String, HeldUnit>,
    clock: u64,
    /// The sum of `HeldUnit::weight` — what the table holds, in the same estimate the memory report prints.
    bytes: usize,
}

impl UnitTable {
    /// The units this session is holding, borrowed — for a caller asking *about* them rather than for one by name.
    /// The cook is such a caller: "does any held unit contain this header" is a question about all of them and about
    /// none of them in particular.
    fn values(&self) -> impl Iterator<Item = &HeldUnit> {
        self.held.values()
    }

    fn get(&mut self, key: &str) -> Option<(bool, std::sync::Arc<crate::TranslationUnit>)> {
        self.clock += 1;
        let held = self.held.get_mut(key)?;
        held.used = self.clock;
        Some((held.provisional, held.unit.clone()))
    }

    fn insert(
        &mut self,
        key: String,
        unit: std::sync::Arc<crate::TranslationUnit>,
        provisional: bool,
    ) {
        self.clock += 1;

        // **Both budgets, and the byte one is the real one.** A count of sixteen says nothing when a unit is 0.1 MB
        // in one project and 25 MB in another; the count stays as the backstop that keeps a project of tiny
        // closures from pinning hundreds of timelines.
        let weight = unit.heap_bytes();
        if let Some(replaced) = self.held.remove(&key) {
            self.bytes = self.bytes.saturating_sub(replaced.weight);
        }
        while self.held.len() + 1 > MAX_UNITS || self.bytes + weight > MAX_UNIT_BYTES {
            if self.held.is_empty() {
                // One unit larger than the whole budget is still the one the caller just asked for.
                break;
            }
            // **The smallest closure goes first, and only then the oldest.** A unit is a timeline over its whole
            // closure, so a project root's unit answers `environment_of` for hundreds of headers while a header's
            // own answers for a handful — and the second pass asks "is this file covered" for every file it
            // re-reads. LRU kept whichever was walked last and threw the big ones away: measured on a cold start,
            // the tail walked **365 units for 63 slices** (~6 per slice of 16 files), because every sibling needed
            // its own timeline while the root that covered all of them had been evicted.
            let smallest = self
                .held
                .iter()
                .min_by_key(|(_, held)| (held.weight, held.used))
                .map(|(key, _)| key.clone());
            let Some(smallest) = smallest else {
                break;
            };
            if let Some(removed) = self.held.remove(&smallest) {
                self.bytes = self.bytes.saturating_sub(removed.weight);
            }
        }

        self.bytes = self.bytes.saturating_add(weight);
        self.held.insert(
            key,
            HeldUnit {
                used: self.clock,
                provisional,
                unit,
                weight,
            },
        );
    }

    /// **Forget one file's unit** — what an edit to that file invalidates, and nothing else.
    ///
    /// `clear` was the only removal for most of this table's life, and it is the wrong width: see
    /// [`Session::buffer_changed`]. `clear` stays for the one case that really is whole-table — a closed buffer,
    /// whose text every unit in the table may have been built from.
    fn remove(&mut self, key: &str) {
        if let Some(removed) = self.held.remove(key) {
            self.bytes = self.bytes.saturating_sub(removed.weight);
        }
    }

    fn clear(&mut self) {
        self.held.clear();
        self.bytes = 0;
    }

    /// What the table holds: `(entries, bytes)`.
    fn accounting(&self) -> (usize, usize) {
        (self.held.len(), self.bytes)
    }

    /// **The timelines themselves, for the one caller that has to know which of them is shared.**
    ///
    /// [`UnitTable`] and `walked_by_content` hold the *same* `Arc<TranslationUnit>` whenever a timeline was both held
    /// for a root and remembered for sharing, which is the common case — so a report that adds
    /// `accounting().1` to the sharing cache's weight counts one allocation twice. This is how that is measured
    /// instead of assumed: the caller compares pointers. See [`Session::memory_report`].
    fn timelines(&self) -> impl Iterator<Item = &std::sync::Arc<crate::TranslationUnit>> {
        self.held.values().map(|held| &held.unit)
    }
}

/// One held timeline, and **whether it was built while the program was still being read**.
///
/// # The defect this flag exists for
///
/// A walk enters every file its closure offers and stops at one the closure does not hold. Usually that is the
/// right reading *and the permanent one*: a system header nobody indexed is **outside the analysis**, which is what
/// [`crate::TranslationUnit::walk`]'s early return says and what was measured when it was written (treating it as
/// "may define anything" collapses the corpus's conditional evidence from millions of facts to tens of thousands).
///
/// But while an index is running, the same absence means **not yet** — the file is in the queue and will be read —
/// and a timeline built without it is a timeline of a *smaller program*. Nothing in the timeline says which of the
/// two it is, so storing it asserts the wrong one. Measured, with `examples/keystroke.rs`'s fixture family:
///
/// ```text
///   one step of indexing            WALK "/p/api.h" frames=["/p/api.h"]      the closure held two files
///   …written to the disk cache      an entry whose recorded closure is the one frame it entered
///   every later session             served it — the key cannot tell a short closure from a complete one
///   the environment it answered     without `#define BEGIN_NS namespace one {`
///   so the reading of the file      `struct Widget` at file scope, and `one::Widget` NotDeclaredHere
/// ```
///
/// # What provisional buys, and what it costs
///
/// A provisional timeline is used for **this** run, exactly as before — the index is being built, and a reading of
/// most of the program beats none. It is not written to the disk cache, and it is dropped as soon as the queue is
/// empty, so the next walk is over the whole closure and *that* one is stored.
///
/// The cost is one re-walk per index burst. The alternative — never caching while anything is queued — is worse,
/// because a big project's first minute is exactly when the cache is worth most.
struct HeldUnit {
    /// The table's own clock, for eviction.
    used: u64,
    /// Built while a file the closure needed was still queued. See this type's documentation.
    provisional: bool,
    unit: std::sync::Arc<crate::TranslationUnit>,
    /// What this unit costs, in the estimate [`crate::TranslationUnit::heap_bytes`] gives — the number the table's
    /// byte budget is deducted from. Stored rather than recomputed on eviction: the estimate walks every event.
    weight: usize,
}

/// Which of a file's two directive comparisons an edit failed — see [`Session::directives_moved`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct DirectivesMoved {
    /// What the file tells its includers changed.
    environment: bool,
    /// Where the file's directives sit changed, so a timeline over it is out of date.
    layout: bool,
}

/// The files waiting to be read **the way a compiler reads them**, in the order they will be.
///
/// A queue with a membership set, the same shape [`Work`] has and for the same reason: a file may be wanted twice
/// (an open file that is also a dependent of something that changed) and must be cooked once, and the *whole
/// project* is marked at every drain once the open files are done — a scan whose cost has to be linear in the
/// number of files rather than quadratic in it. The order is the point of the queue: a reader is waiting for the
/// file they are looking at, and for the header their edit invalidated, before they are waiting for the rest.
#[derive(Debug, Default)]
struct Cooking {
    /// The order, front first.
    wanted: VecDeque<PathBuf>,
    /// Membership, by the queue's own key — the normalization every other queue in this crate compares by.
    held: HashSet<String>,
}

impl Cooking {
    /// Want `path` cooked, **at the front**: something is about to look at it, and its answer is the next thing
    /// asked for.
    fn want(&mut self, path: &Path) -> bool {
        if !self.held.insert(queue_key(path)) {
            return false;
        }

        // The spelling is kept as the caller gave it: the cook resolves it against the index's own spelling anyway
        // (`Session::cook` asks the index for the path it filed the file under), and rewriting it here would be a
        // second opinion about identity.
        self.wanted.push_front(path.to_path_buf());
        true
    }

    /// Is `path` already waiting to be cooked? The **membership set** [`Cooking::want`] inserts into, asked without
    /// taking anything — see [`Session::is_ready_for_a_request_about`].
    fn wants(&self, path: &Path) -> bool {
        self.held.contains(&queue_key(path))
    }

    fn forget(&mut self, path: &Path) {
        let key = queue_key(path);
        if self.held.remove(&key) {
            self.wanted.retain(|queued| queue_key(queued) != key);
        }
    }

    /// Take up to `how_many` files to cook, in order.
    fn take(&mut self, how_many: usize) -> Vec<PathBuf> {
        let mut taken = Vec::new();

        for _ in 0..how_many {
            let Some(path) = self.wanted.pop_front() else {
                break;
            };
            self.held.remove(&queue_key(&path));
            taken.push(path);
        }

        taken
    }

    fn len(&self) -> usize {
        self.wanted.len()
    }

    /// **Drop the whole queue, and say how many files it held** — see [`Session::abandon_the_cooking_queue`].
    ///
    /// Both halves: the order and the membership set, which is what `want` consults before pushing. Clearing only one
    /// would leave the queue claiming a file it no longer holds.
    fn abandon(&mut self) -> usize {
        let held = self.wanted.len();
        self.wanted.clear();
        self.held.clear();
        held
    }
}

/// The queue, and what each path in it is doing there.
///
/// Two deques because the order has two halves — open files and what they reach, then the rest of the project —
/// and because a notification can move a path from one half to the other. The map is what keeps one file from being
/// worked twice: a header `#include`d by forty files is one entry, and an entry left behind by an upgrade is
/// skipped rather than read again.
#[derive(Debug, Default)]
struct Work {
    /// Files the user is looking at, and (through `add`'s inheritance of the half) what they include.
    open: VecDeque<(PathBuf, usize)>,
    /// The project scan's files, and what they include.
    rest: VecDeque<(PathBuf, usize)>,
    /// Where each path stands, by normalized spelling.
    standing: HashMap<String, Standing>,
    /// How many *files* are queued — the count [`Work::pending`] reports, maintained rather than recomputed.
    queued: usize,
}

/// What the queue knows about one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// In one of the deques, in this half.
    Queued(Priority),
    /// Worked. A second entry for it — left behind by an upgrade, or discovered by a second include — is skipped.
    Worked,
}

impl Work {
    /// Queue a path if it is not already queued or worked.
    ///
    /// A path in the **rest** half that is now wanted in the open half is upgraded rather than ignored: that is the
    /// case the two halves exist for — a header the project scan listed before the user opened it — and ignoring it
    /// would leave the file the user is looking at behind everything else in the project.
    fn add(&mut self, path: PathBuf, priority: Priority, depth: usize) -> bool {
        let key = queue_key(&path);

        match self.standing.get(&key) {
            None => {
                self.standing.insert(key, Standing::Queued(priority));
                self.push(path, priority, depth);
                self.queued += 1;
                true
            }
            Some(Standing::Queued(Priority::Rest)) if priority == Priority::Open => {
                self.standing.insert(key, Standing::Queued(Priority::Open));
                self.push(path, priority, depth);
                // The count does not move: the file was already counted once, and the entry it left in the rest
                // half is skipped when it surfaces.
                true
            }
            Some(_) => false,
        }
    }

    /// Queue a path whose **answer** has changed — an edit, or a file the client says changed.
    ///
    /// Different from [`Work::add`] in both directions: it is queued even if it was already worked, and it goes to
    /// the **front** of its half. Both are about the same thing — the user is waiting for this one — and the cost
    /// of being wrong is one redundant step, because a file whose text did not actually change is a cache hit.
    fn again(&mut self, path: PathBuf, priority: Priority, depth: usize) {
        self.rearm(path, priority, depth, true);
    }

    /// Queue a path whose standing is void for a reason that is not urgent — every key in the project, after the
    /// configuration changed.
    ///
    /// The same re-arming as [`Work::again`] and the back of the half instead of the front: there is no file among
    /// ten thousand of them that the user is waiting for more than the others, and pushing each one to the front
    /// would leave the list in the reverse of the order it was scanned in.
    fn requeue(&mut self, path: PathBuf, priority: Priority, depth: usize) {
        self.rearm(path, priority, depth, false);
    }

    /// Queue a path and forget what the queue knew about it, at either end of its half.
    fn rearm(&mut self, path: PathBuf, priority: Priority, depth: usize, urgent: bool) {
        let key = queue_key(&path);

        // An entry still in a deque keeps its count; it will be skipped when it surfaces, because this insert
        // replaces its standing. A path that was worked, or never seen, is a new file to read.
        if !matches!(self.standing.get(&key), Some(Standing::Queued(_))) {
            self.queued += 1;
        }

        self.standing.insert(key, Standing::Queued(priority));

        match (priority, urgent) {
            (Priority::Open, true) => self.open.push_front((path, depth)),
            (Priority::Open, false) => self.open.push_back((path, depth)),
            (Priority::Rest, true) => self.rest.push_front((path, depth)),
            (Priority::Rest, false) => self.rest.push_back((path, depth)),
        }
    }

    /// **Forget a path whose answer is already in hand** — the queue entry a synchronous re-read makes unnecessary.
    ///
    /// Returns whether there was anything to forget, which is also how a caller learns that the file *was* stale:
    /// a path in the queue is a path whose summary was dropped and not yet rebuilt, so "the queue had it" and "the
    /// index is one edit behind" are the same statement.
    ///
    /// The path becomes [`Standing::Worked`] rather than being left unknown, because it *has* been worked — by
    /// whoever read it synchronously — and a later `add` for it (an include discovered by another file) must not
    /// queue a second read of a file the store already answered for.
    fn forget(&mut self, path: &Path) -> bool {
        if !self.is_queued(path) {
            return false;
        }

        self.standing.insert(queue_key(path), Standing::Worked);
        self.queued -= 1;
        true
    }

    /// **Is `path` waiting to be read?** — the test [`Work::forget`] is built on, asked on its own because a caller
    /// that only wants to know whether there is work to do must be able to ask it without doing any.
    ///
    /// One implementation, two callers: [`Work::forget`] decides what to forget with it and
    /// [`Session::needs_catching_up`] decides whether to take the write lock with it.
    fn is_queued(&self, path: &Path) -> bool {
        matches!(
            self.standing.get(&queue_key(path)),
            Some(Standing::Queued(_))
        )
    }

    /// The next file to work, and the half it came from.
    ///
    /// Skips the entries an upgrade left behind: a path whose standing is in the other half, or has already been
    /// worked, is not work.
    fn pop(&mut self) -> Option<(PathBuf, Priority, usize)> {
        loop {
            let (path, priority, depth) = match self.open.pop_front() {
                Some((path, depth)) => (path, Priority::Open, depth),
                None => {
                    let (path, depth) = self.rest.pop_front()?;
                    (path, Priority::Rest, depth)
                }
            };

            let key = queue_key(&path);
            match self.standing.get(&key) {
                Some(Standing::Queued(held)) if *held == priority => {
                    self.standing.insert(key, Standing::Worked);
                    self.queued -= 1;
                    return Some((path, priority, depth));
                }
                _ => continue,
            }
        }
    }

    fn push(&mut self, path: PathBuf, priority: Priority, depth: usize) {
        match priority {
            Priority::Open => self.open.push_back((path, depth)),
            Priority::Rest => self.rest.push_back((path, depth)),
        }
    }

    fn pending(&self) -> usize {
        self.queued
    }

    /// Has this path already been worked?
    fn is_worked(&self, path: &Path) -> bool {
        matches!(self.standing.get(&queue_key(path)), Some(Standing::Worked))
    }

    /// **Mark a file read without popping it** — what a translation-unit read does for every file it covered.
    ///
    /// A unit read parses one program and files the facts of every file in it, so the per-file queue must not read
    /// those files again. The queue entry (if any) is left where it is and skipped by `pop`, which asks `standing`
    /// rather than the deque; the count is what a caller sees, so it comes down here.
    ///
    /// A path the queue has never heard of is recorded as worked rather than ignored: a later `add` then leaves it
    /// alone, which is the honest answer — the unit read covered it.
    fn mark_worked(&mut self, path: &Path) {
        let key = queue_key(path);
        match self.standing.get(&key) {
            Some(Standing::Queued(_)) => {
                self.standing.insert(key, Standing::Worked);
                self.queued -= 1;
            }
            Some(Standing::Worked) => {}
            None => {
                self.standing.insert(key, Standing::Worked);
            }
        }
    }

    /// The next `how_many` files [`Work::pop`] would answer with, **without taking them** — in the order it would.
    ///
    /// The queue as it stands now: a file a step discovers later can outrank these, which is why the caller treats
    /// the answer as a hint about what to prepare and pops for real.
    fn peek(&self, how_many: usize) -> Vec<PathBuf> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut ahead = Vec::new();

        let halves = [(&self.open, Priority::Open), (&self.rest, Priority::Rest)];
        for (half, priority) in halves {
            for (path, _) in half {
                if ahead.len() >= how_many {
                    return ahead;
                }

                let key = queue_key(path);
                let queued_here = matches!(
                    self.standing.get(&key),
                    Some(Standing::Queued(held)) if *held == priority
                );
                if queued_here && seen.insert(key) {
                    ahead.push(path.clone());
                }
            }
        }

        ahead
    }
}

/// **The namespaces enclosing `at`, as the file itself spells them** — see
/// [`Session::definition_of_a_written_type`], which asks this for a declaration that has no scope of its own.
///
/// The chain is walked outward from the innermost scope and then reversed, so the answer reads outermost first:
/// a declaration in `namespace a { namespace b { void f() { … } } }` is looked up from `a::b`, then `a`, which
/// is the order C++ looks in and the order [`crate::ProjectIndex::definition_where_written`] already walks once
/// it has a starting point. `None` when nothing encloses the offset but the file: a declaration at file scope
/// is qualified by nothing, and an empty string would be a scope no name is in.
fn enclosing_namespace_at(view: &FileView, at: usize) -> Option<String> {
    let scope = view.scopes.scope_at(at)?;

    let mut segments: Vec<&str> = Vec::new();
    for id in view.scopes.scope_chain(scope) {
        let Some(data) = view.scopes.scope(id) else {
            continue;
        };
        if data.kind == crate::ScopeKind::Namespace
            && let Some(name) = data.name.as_deref()
        {
            segments.push(name);
        }
    }

    if segments.is_empty() {
        return None;
    }

    segments.reverse();
    Some(segments.join("::"))
}

/// A path as the queue compares them — the same normalization the store and the index use.
fn queue_key(path: &Path) -> String {
    normalize_path(path, cfg!(windows))
}

/// **Does this session know the whole of what its compilation defines?**
///
/// The licence for one claim — *a name none of the witnesses mentions is not defined* — which is what turns
/// `#ifdef NAME` from `Unknown` into a branch, and with it every conditional region in every header the project
/// reaches. See the note at the call site ([`Session::assemble`]) for what it is worth measured.
///
/// # Why the compiler's own answer is the licence
///
/// A condition is answered against three witnesses: the compiler's predefined names, the command line's `-D`s, and
/// the file's own `#define`s. The second and third are in files — the build description and the source — and the
/// first exists **nowhere**: `_MSC_VER`, `__cplusplus` and five hundred more are built into the compiler, and
/// `-dM`/`/PD` is the only way to see them. So a session that has that table knows everything there is to know
/// about the command line *except what a build description nobody can read said*, and one that does not have it
/// knows neither half.
///
/// Empty is the signal, and it is not "this compiler predefines nothing": [`Toolchain::builtin_macros`] says so —
/// a compiler that could not be asked (no `/Zc:preprocessor`, no resources for the language it is running in) and
/// the last-resort toolchain built from the system's conventional header directories both arrive here with an
/// empty table and a [`Toolchain::note`] explaining it.
///
/// A compile database is **not** part of the test, although it was the whole of it before: it is a source of
/// flags, not a witness to the built-ins, and tying the claim to it made every project with no build system read
/// MSVC's standard library as if nothing in it were compiled. See [`Session::assemble`].
fn the_compilation_is_known(toolchain: Option<&Toolchain>) -> bool {
    toolchain.is_some_and(|found| !found.builtin_macros.is_empty())
}

/// **The files of every library this project depends on**, restricted to what a dependency is used through.
///
/// Headers (`.h`, `.hpp`, `.hxx`, `.h++`, `.inl`, `.ipp`) under each library's include directories, and module
/// interfaces (`.cppm`) under its module directories. **No `.cpp`**: see `LibrarySection` for what that costs and why
/// the cost is the right one — a dependency's implementation is not part of what this project compiles, and indexing it
/// would multiply the index by every dependency's sources on every cold start.
///
/// A missing directory is skipped rather than reported: a library whose path does not exist is a configuration problem
/// the discovery layer already reports, and a session built from a provider that cannot list directories (an in-memory
/// project) has nothing to walk.
fn library_files_to_read(
    libraries: &[crate::project::LibrarySection],
    workspace: &std::path::Path,
) -> Vec<PathBuf> {
    const HEADER_EXTENSIONS: &[&str] = &["h", "hpp", "hxx", "h++", "inl", "ipp"];
    /// How deep a dependency's public headers are walked. Deep enough for `include/pkg/detail/x.h`, bounded so that a
    /// library whose path is a whole disk cannot turn a start-up into a walk of it.
    const MAX_DEPTH: usize = 12;
    const MAX_FILES: usize = 20_000;

    let mut found: Vec<PathBuf> = Vec::new();
    for library in libraries {
        let mut roots: Vec<PathBuf> = library.include_directories(workspace);
        roots.extend(library.module_directories(workspace));
        let mut stack: Vec<(PathBuf, usize)> = roots.into_iter().map(|root| (root, 0)).collect();
        while let Some((directory, depth)) = stack.pop() {
            if depth > MAX_DEPTH || found.len() >= MAX_FILES {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    stack.push((path, depth + 1));
                    continue;
                }
                let extension = path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .map(|extension| extension.to_lowercase());
                let wanted = match extension.as_deref() {
                    Some("cppm") => true,
                    Some(other) => HEADER_EXTENSIONS.contains(&other),
                    None => false,
                };
                if wanted {
                    found.push(path);
                }
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// **Where an `#include` for this file belongs**, as a byte offset into the written text: right after the last
/// top-level `#include` line, or the top of the file when there is none.
///
/// The same rule the code action uses ([`FixContext::after_the_includes`](../../cpp_ls) in the server), expressed in the
/// offsets a `TextEdit` needs.
fn after_the_includes(text: &str) -> usize {
    let mut end = 0usize;
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("#include") {
            end = offset + line.len();
        }
        offset += line.len();
    }
    end
}

/// **Where an `import` belongs**: after the module declaration or the last top-level import, in the file's offsets.
fn after_the_module_imports(text: &str) -> usize {
    let mut end = 0usize;
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let imports = trimmed.starts_with("import ") && trimmed.trim_end().ends_with(';');
        let declares_a_module = (trimmed.starts_with("module ")
            || trimmed.starts_with("export module "))
            && trimmed.trim_end().ends_with(';');
        if imports || declares_a_module {
            end = offset + line.len();
        }
        offset += line.len();
    }
    end
}

/// Fold a project's configuration into the filter a session reads through.
///
/// The exclusions are applied **on top of** whatever the caller passed, because the two lists are different claims that
/// are both true at once: the caller's is the editor's ("these files are not what I am working on"), the project's is
/// the file's ("these files are not source"). The cache directory goes in here too, so that the rule keeping a watcher
/// from re-indexing the cache's own writes follows the configuration rather than the default — one place, and the
/// store reads the same name from the same report.
fn apply_project_config(filter: WatchFilter, report: &ConfigReport) -> WatchFilter {
    let mut filter = filter;

    for pattern in &report.config.workspace.exclude {
        if let Ok(pattern) = crate::PathPattern::new(pattern) {
            filter = filter.ignore_pattern(pattern);
        }
    }

    if let Some(cache_dir) = &report.config.index.cache_dir {
        filter = filter.with_cache_directory(cache_dir);
    }

    // **A UE project's build trees are not source.** `Intermediate` holds the generated headers — which the analysis
    // must still *read* when a source includes one, and must never analyse as a translation unit — and `Binaries`,
    // `Saved` and `DerivedDataCache` hold outputs. Measured on a real UE checkout: `Intermediate/Build` is most of the
    // file count under the project, and indexing it produces findings about generated code nobody edits.
    //
    // Excluded by **pattern** rather than by a special case in the walk, because that is what this filter is for and
    // because a project that keeps its generated code somewhere else can say so with `[workspace] exclude`.
    if report
        .config
        .unreal
        .as_ref()
        .is_some_and(|unreal| unreal.is_on())
    {
        for pattern in [
            "Intermediate/**",
            "**/Intermediate/**",
            "Binaries/**",
            "Saved/**",
            "DerivedDataCache/**",
        ] {
            if let Ok(pattern) = crate::PathPattern::new(pattern) {
                filter = filter.ignore_pattern(pattern);
            }
        }
    }

    filter
}

/// The sources a project owns: the compile database's files, or a scan of the root.
///
/// The database when it names files that are actually here — a checked-in `compile_commands.json` is written on
/// somebody else's machine, so its absolute paths may name nothing locally, and a list of files that do not exist
/// is worse than a scan. The scan is a fallback and not a merge: a database that resolves is the project's own
/// statement of which files are compiled, and adding every other file under the root to it would index test
/// fixtures and dead code.
///
/// # The scan reads the filesystem, not the provider
///
/// Deliberate, and the one place in this module where the two disagree. A project scan is a question about a
/// **directory** — what is in it — and a provider interface that answered it would be a filesystem listing by
/// another name: `MemoryFiles` has no directories, and a buffer set has no "everything else". A caller with
/// synthetic files says so with [`Session::with_config`] plus its own seeds; a session over the real disk, which
/// is the only kind [`Session::open`] builds, gets the walk.
fn project_files(
    files: &impl FileProvider,
    filter: &WatchFilter,
    root: &Path,
    database: Option<&CompileCommands>,
    extra_extensions: &[String],
    max_files: usize,
) -> Vec<PathBuf> {
    let mut found = Vec::new();

    if let Some(database) = database {
        for command in &database.commands {
            // A relative path in a database is relative to the entry's `directory`, which is where the build ran.
            let path = match (&command.directory, command.file.is_relative()) {
                (Some(directory), true) => directory.join(&command.file),
                _ => command.file.clone(),
            };

            if files.exists(&path) && !filter.is_ignored(&path) {
                found.push(path);
            }
        }
    }

    if found.is_empty() {
        scan(root, filter, &mut found, extra_extensions, max_files);
    }

    // Sorted and deduplicated: the walk's order is the filesystem's, and an index whose contents depend on which
    // directory entry the OS returned first is one whose behaviour cannot be compared between two runs.
    found.sort();
    found.dedup();
    found
}

/// Every source under `root`, bounded and with symlinked directories left alone.
///
/// The bound is the caller's (`index.max_files`, or [`MAX_PROJECT_FILES`]). Symlinks are not followed **as
/// directories**: a link back up the tree is an infinite walk, and the cheap rule that rules it out — the entry's
/// own type, which does not resolve the link — is also the one that keeps a project from indexing the same files
/// through two paths.
fn scan(
    root: &Path,
    filter: &WatchFilter,
    found: &mut Vec<PathBuf>,
    extensions: &[String],
    max_files: usize,
) {
    let mut pending = vec![root.to_path_buf()];

    while let Some(directory) = pending.pop() {
        if found.len() >= max_files {
            return;
        }

        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };

        for entry in entries.flatten() {
            if found.len() >= max_files {
                return;
            }

            let path = entry.path();
            if filter.is_ignored(&path) {
                continue;
            }

            let Ok(kind) = entry.file_type() else {
                continue;
            };

            if kind.is_symlink() {
                continue;
            }

            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && is_a_source_file(&path, extensions) {
                found.push(path);
            }
        }
    }
}

/// Does this path's name say it is a source? See [`SOURCE_EXTENSIONS`] for why a scan may ask this and an include
/// may not.
///
/// `extra` is the project's own list (`workspace.source_extensions` in `.cppls.toml`), **added** to the engine's:
/// a project that writes `.cuh` is saying "and also these", and a project that wanted to *replace* the list would
/// be one analysing C++ without `.cpp`.
fn is_a_source_file(path: &Path, extra: &[String]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            let lower = extension.to_ascii_lowercase();
            SOURCE_EXTENSIONS.contains(&lower.as_str())
                || extra
                    .iter()
                    .any(|listed| listed.trim_start_matches('.').eq_ignore_ascii_case(&lower))
        })
}
#[cfg(test)]
mod tests;
/// How many rendering summaries a session remembers, and how much they may weigh.
///
/// The weight is `declarations + macros`, which is what a summary is made of, so the bound is "a handful of the
/// largest headers": a rendering summary of `bits/basic_string.h` is about 1 100 declarations and one of
/// `stl_bvector.h` about 440. Sixteen of the largest is a few tens of thousands of entries.
const RENDERING_SUMMARY_ENTRIES: usize = 16;
const RENDERING_SUMMARY_BYTES: usize = 40_000;
