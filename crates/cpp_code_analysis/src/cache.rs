//! The on-disk cache: **one file per file**, keyed by a hash of everything that determines a summary.
//!
//! The rules this module implements are the ones `docs/index-design.md` fixes; what follows is the part a reader
//! of the code needs to know to change it safely.
//!
//! # Why one file per summary
//!
//! A single blob for the whole project would have to be rewritten whenever any file changes — for a large project
//! that is hundreds of megabytes of I/O per keystroke, and a crash while writing loses everything. One file per
//! summary makes invalidation local, writes independent, and a torn write cost exactly one file.
//!
//! # What the key is made of, and why each part is in it
//!
//! ```text
//! content_hash     what the file says          — a change here changes the summary
//! context_hash     compiler settings, -D, include paths, and the file's own directory; see below
//! macro_env_hash   the macros visible when the file is entered — the hard one; see below
//! format_version   the producer               — a parser or schema change makes every summary wrong
//! ```
//!
//! `context_hash` is the configuration **and the directory the file sits in**, which are one question rather than
//! two: `#include "widget.h"` means *the one beside me*, so the same text in two directories describes two
//! different compilations and resolves to two different files. The directory was missing from the key at first, and
//! the hole was invisible until a test put the same text in two directories: the two files shared one entry, and
//! the second one was handed the first one's resolved includes — a jump to a header it does not include.
//!
//! The part of the key that is *not* in it is which file it is: nothing here names a path, only the directory the
//! path is in. That is what makes a branch switch cheap — see [`content_hash`].
//!
//! `format_version` is deliberately the blunt instrument: the grammar is still moving, and a summary written by a
//! different parser is not "probably still fine", it is a **silent** wrong answer. Bumping the number is the one
//! invalidation that must never be forgotten, so it lives in the key rather than in a migration step.
//!
//! `macro_env_hash` is the subtle one. It is *not* the set of macro values in scope — that would change with every
//! `#define` in every header and make the cache useless. It is the **set of content hashes of the files that
//! define macros on the way in**, with include-guarded repeats collapsed: that set changes exactly when something
//! that could change the macros changes, and `detect_guard` is what keeps it from growing a duplicate per
//! inclusion path.
//!
//! # Why the hash is written here rather than taken from `std`
//!
//! `DefaultHasher` is explicitly allowed to change between Rust releases, and a cache key that changes when the
//! compiler is upgraded is a cache that silently misses forever (or worse, hits with a different meaning if the
//! value is ever persisted). FNV-1a is fixed by definition, is a few lines, and is strong enough for this job: the
//! key guards a **local** cache, and a collision costs a stale summary for one file — a wrong answer a re-parse
//! fixes, not a security boundary.

use std::path::{Path, PathBuf};

/// The version of everything that produces a summary: the parser's grammar, this crate's summary schema, and the
/// meaning of the fields in it.
///
/// **Bump on any change that could make an old summary wrong.** See the module documentation for why this is part
/// of the key rather than handled by a migration.
pub const FORMAT_VERSION: u32 = 1;

/// The directory name this crate keeps its cache in, under the project root.
///
/// A dot-directory inside the project rather than the operating system's cache directory: it is what the user
/// asked for, it travels with the repository checkout (so CI gets the same warm cache as a developer), and
/// deleting it is an obvious, local operation.
pub const CACHE_DIRECTORY: &str = ".cppls";

/// Everything that determines what a summary of one file contains.
///
/// Built by the caller, which is the only place that knows the compiler configuration, the macro environment and
/// the directory the file is compiled in; this type's job is to turn them into one stable key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SummaryKey {
    pub content_hash: u64,
    pub context_hash: u64,
    pub macro_env_hash: u64,
    pub format_version: u32,
}

impl SummaryKey {
    /// The key of a file whose text and compilation context are known.
    pub fn new(content_hash: u64, context_hash: u64, macro_env_hash: u64) -> Self {
        SummaryKey {
            content_hash,
            context_hash,
            macro_env_hash,
            format_version: FORMAT_VERSION,
        }
    }

    /// The key, as the hex string the file name is built from.
    ///
    /// The four parts are hashed **together** rather than concatenated as text: the file name stays one fixed
    /// width, so a directory holds a predictable number of entries and nothing has to be parsed back out.
    pub fn file_stem(self) -> String {
        let mut bytes = Vec::with_capacity(32);
        bytes.extend_from_slice(&self.content_hash.to_le_bytes());
        bytes.extend_from_slice(&self.context_hash.to_le_bytes());
        bytes.extend_from_slice(&self.macro_env_hash.to_le_bytes());
        bytes.extend_from_slice(&self.format_version.to_le_bytes());
        format!("{:016x}", fnv1a64(&bytes))
    }

    /// Where the summary for this key lives, under `project_root`.
    ///
    /// Two levels deep — `<root>/.cppls/summaries/<first two hex digits>/<key>.bin` — because a flat directory with
    /// tens of thousands of entries is slow to list and unpleasant to debug; the prefix directory is the usual
    /// sharding, and it is derived from the key so nothing has to be looked up.
    pub fn path_under(self, project_root: &Path) -> PathBuf {
        let stem = self.file_stem();
        project_root
            .join(CACHE_DIRECTORY)
            .join("summaries")
            .join(&stem[..2])
            .join(format!("{stem}.bin"))
    }
}

/// A stable 64-bit FNV-1a hash of `bytes`.
///
/// See the module documentation for why the standard library's hasher is not used. Exposed because the same
/// function has to be used for a file's content hash, the configuration hash and the macro-environment hash — three
/// places that must agree, and the only way to make them agree is to have one implementation.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// The hash of a file's text, as the cache sees it.
///
/// Nothing about *where* the file is enters here, which is what makes a branch switch cheap: a file that comes
/// back to a content the cache has seen — after `git checkout`, a revert, or a rename within its directory — is
/// found without being parsed. The directory is a separate part of the key, because the text alone does not
/// determine what its includes resolve to.
pub fn content_hash(text: &str) -> u64 {
    fnv1a64(text.as_bytes())
}

/// The macros that were in force when a file was entered, as the cache keys on them.
///
/// # Why this is a set of files rather than a set of macros
///
/// The obvious key would be "the name and value of every macro defined on the way in", and it cannot be used: a
/// header that defines one more macro would change the key of every file below it *and* of every file below
/// those, so one keystroke in one header invalidates the whole project — which is exactly what the cache exists
/// to avoid.
///
/// What is stored instead is the set of **content hashes of the files that define macros** along the way. That
/// set changes when something that could change the macros changes, and it changes for one file at a time: a
/// header edited in a comment has a new content hash, so the files below it are rebuilt, while a file that
/// defines nothing new is not in the set at all and does not propagate anything.
///
/// # What it deliberately does not include
///
/// A file with no `#define` contributes nothing, even though it may `#include` something that does. Its own
/// includes are added by the walk, so the macros it brings in are accounted for by the file that wrote them —
/// adding the includer as well would make every file in a chain part of every key below it.
///
/// # Cycles
///
/// A file is added at most once, so `a.h` including `b.h` including `a.h` terminates. That is not a special
/// case: an include guard means the compiler reads the second visit's *nothing*, so the set of files that
/// contributed macros is the same either way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MacroEnvironment {
    /// The content hashes of the files that define macros, deduplicated.
    defining_files: std::collections::BTreeSet<u64>,
}

impl MacroEnvironment {
    pub fn new() -> Self {
        MacroEnvironment::default()
    }

    /// Add a file that defines macros, if it defines any.
    ///
    /// `defines_any_macro` is a fact about the file — whether any of its facts is a macro — and a file that
    /// defines none is deliberately not recorded. See the type's documentation.
    pub fn add(&mut self, content_hash: u64, defines_any_macro: bool) {
        if defines_any_macro {
            self.defining_files.insert(content_hash);
        }
    }

    /// How many files contributed.
    pub fn len(&self) -> usize {
        self.defining_files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defining_files.is_empty()
    }

    /// The hash a [`SummaryKey`] records.
    ///
    /// Built from the **sorted** set, so that the same files in a different include order give the same key: two
    /// orders that see the same macro definitions produce the same summary, and making them differ would throw
    /// away a cache hit for a difference that does not change any answer.
    ///
    /// `BTreeSet` is what makes that ordering free rather than something to remember.
    pub fn hash(&self) -> u64 {
        let mut bytes = Vec::with_capacity(self.defining_files.len() * 8);

        for hash in &self.defining_files {
            bytes.extend_from_slice(&hash.to_le_bytes());
        }

        // A domain separator, so that a file whose content hash happens to equal the hash of an empty set cannot
        // be confused with an environment that has no defining files.
        bytes.extend_from_slice(b"cppls-macro-env");
        fnv1a64(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{CACHE_DIRECTORY, FORMAT_VERSION, MacroEnvironment, SummaryKey, content_hash, fnv1a64};
    use std::path::Path;

    #[test]
    fn the_hash_is_the_fixed_fnv_vectors() {
        // The published FNV-1a 64 test vectors. They are the point of hand-writing the function: if a future change
        // to it alters these numbers, every existing cache entry becomes unreachable, and that has to be a
        // deliberate change rather than an accident nobody notices.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn every_part_of_the_key_changes_the_key() {
        let base = SummaryKey::new(1, 2, 3);
        assert_eq!(
            base,
            SummaryKey::new(1, 2, 3),
            "the same inputs are the same key"
        );

        let variants = [
            SummaryKey::new(9, 2, 3),
            SummaryKey::new(1, 9, 3),
            SummaryKey::new(1, 2, 9),
            SummaryKey {
                format_version: FORMAT_VERSION + 1,
                ..base
            },
        ];
        for variant in variants {
            assert_ne!(
                variant.file_stem(),
                base.file_stem(),
                "a different key must name a different file"
            );
        }
    }

    #[test]
    fn the_same_content_hashes_the_same_wherever_it_is() {
        // The property that makes a branch switch cheap: a summary is keyed by *what the file says*, so a file that
        // comes back to a content the cache has seen — after `git checkout`, a revert, or a rename — is found
        // without being parsed. Nothing in the key mentions a path.
        assert_eq!(content_hash("int x;\n"), content_hash("int x;\n"));
        assert_ne!(content_hash("int x;\n"), content_hash("int y;\n"));

        let key = SummaryKey::new(content_hash("int x;\n"), 7, 11);
        let from_one_root = key.path_under(Path::new("/one/project"));
        let from_another = key.path_under(Path::new("D:/elsewhere/checkout"));
        assert_eq!(
            from_one_root.file_name(),
            from_another.file_name(),
            "the file name is the content's, the directory is the project's"
        );
    }

    #[test]
    fn the_layout_is_sharded_and_inside_the_project() {
        let key = SummaryKey::new(0, 0, 0);
        let path = key.path_under(Path::new("/p"));
        let text = path.to_string_lossy().replace('\\', "/");

        assert!(
            text.starts_with(&format!("/p/{CACHE_DIRECTORY}/summaries/")),
            "the cache is a dot-directory in the project: {text}"
        );
        let stem = key.file_stem();
        assert!(text.ends_with(&format!("/{stem}.bin")), "{text}");
        assert!(
            text.contains(&format!("/summaries/{}/", &stem[..2])),
            "the first two hex digits shard the directory: {text}"
        );
        assert_eq!(
            stem.len(),
            16,
            "one fixed-width name, nothing to parse back"
        );
    }

    #[test]
    fn a_file_that_defines_nothing_is_not_part_of_the_environment() {
        // The property the whole design rests on: a header that merely *includes* things does not become part of
        // every key below it, so editing it in a way that changes no macro changes no summary.
        let mut environment = MacroEnvironment::new();
        environment.add(content_hash("// just a comment\n"), false);

        assert!(environment.is_empty());
        assert_eq!(environment.len(), 0);
    }

    #[test]
    fn a_file_that_defines_a_macro_is_part_of_it() {
        let mut environment = MacroEnvironment::new();
        environment.add(content_hash("#define A 1\n"), true);

        assert_eq!(environment.len(), 1);
        assert!(!environment.is_empty());
    }

    #[test]
    fn the_same_files_in_any_order_hash_the_same() {
        // Two include orders that see the same macro definitions produce the same summary — making them differ
        // would throw away a cache hit for a difference that changes no answer.
        let one = content_hash("#define A 1\n");
        let two = content_hash("#define B 2\n");

        let mut forwards = MacroEnvironment::new();
        forwards.add(one, true);
        forwards.add(two, true);

        let mut backwards = MacroEnvironment::new();
        backwards.add(two, true);
        backwards.add(one, true);

        assert_eq!(forwards.hash(), backwards.hash());
    }

    #[test]
    fn a_different_set_of_files_is_a_different_hash() {
        let one = content_hash("#define A 1\n");
        let two = content_hash("#define A 2\n");

        let mut first = MacroEnvironment::new();
        first.add(one, true);

        let mut second = MacroEnvironment::new();
        second.add(two, true);

        assert_ne!(
            first.hash(),
            second.hash(),
            "one character in one header must change the key of everything below it"
        );
        assert_ne!(first.hash(), MacroEnvironment::new().hash());
    }

    #[test]
    fn adding_the_same_file_twice_counts_once() {
        // What makes a cycle terminate, and what keeps an include-guarded header included twice from changing
        // the key: the second visit contributes nothing new.
        let mut environment = MacroEnvironment::new();
        let hash = content_hash("#define A 1\n");

        environment.add(hash, true);
        environment.add(hash, true);

        assert_eq!(environment.len(), 1);
    }
}
