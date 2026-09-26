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
//! context_hash     compiler settings and the file's own directory; see below
//! format_version   the producer               — a parser or schema change makes every summary wrong
//! reading_fingerprint  the readers themselves — measured, not remembered; see [`READING_FINGERPRINT`]
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
//! It was forgotten, for many rounds — which is why the key also carries a **measurement** of the readers
//! ([`READING_FINGERPRINT`], computed by `build.rs`): a rule that depends on remembering is a rule that gets
//! missed, and this one is cheap to compute and impossible to miss.
//!
//! # Why the macro environment is *not* in the key
//!
//! A fourth part used to be here: the set of content hashes of the files that define macros on the way in. It was
//! the one part that could not be computed from the text and the path — the file's own `#define`s and its own
//! `#include`s are products of the parse — and that had a consequence the design did not intend: **a lookup could
//! not happen until after the parse**, so the disk cache never saved the thing it exists to save. It saved writes.
//!
//! It is also not needed, and the evidence is a chain of three facts about the producer:
//!
//! * [`crate::build_scopes`] takes only the syntax tree, so declarations are collected from the whole file;
//! * [`crate::build_facts`] takes the preprocessing state to find **where the directives are**, not to decide
//!   which branch was taken — every fact records the `#if` it was written in as a [`crate::FactGuard`] and lets
//!   the consumer decide, which is the whole reason guards are stored rather than applied;
//! * `#include MACRO` — the one directive whose meaning depends on expansion — is **never** resolved
//!   (`crate::include`'s resolver returns `Unresolved` for it, and says so), so a file that writes one is not
//!   stored at all.
//!
//! So a summary is a function of the text, the directory, the configuration and which candidate headers exist —
//! not of which macros were in force. **If that ever stops being true** — macro expansion inside declarations, or
//! pruning the untaken branch of an `#if` — the environment has to come back here *and* [`FORMAT_VERSION`] has to
//! be bumped in the same change, because summaries already on disk would be silently stale rather than merely
//! unreachable.
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
///
/// A source change is caught automatically by [`READING_FINGERPRINT`], so this is for the changes that leave no
/// trace in the sources this workspace owns: a dependency bump that changes a reading, or a build whose inputs
/// differ some other way. The number is still the blunt instrument — every entry becomes unreachable.
pub const FORMAT_VERSION: u32 = 1;

/// The fingerprint of the code that **reads** a file into a summary, computed by `build.rs` from the text of the
/// grammar and of this crate's semantic layer.
///
/// # Why this is not [`FORMAT_VERSION`]
///
/// The two answer different questions. `FORMAT_VERSION` is a **decision** — "I changed something, throw the store
/// away" — and it is only as good as the memory of whoever changed it. This is a **measurement** of the readers
/// themselves, so it cannot be forgotten, and it is what the module documentation means by "a summary written by a
/// different parser is a silent wrong answer": the parser moved for many rounds while `FORMAT_VERSION` stayed at 1,
/// and every entry on disk was quietly describing an older reader.
///
/// # It is still computable before the file is parsed
///
/// A constant of the binary, so a lookup can happen before the parse — which is the property the key must keep,
/// and the reason the macro environment could not stay in it. Nothing here reads a source file at run time: the
/// hash is computed while *building* (see `build.rs`), and a change to any of those sources changes the constant,
/// so the whole store becomes unreachable and the next query re-indexes.
///
/// # What it does not cover
///
/// Anything outside the hashed directories — a workspace dependency's source, a compiler flag. Those remain
/// `FORMAT_VERSION`'s job.
pub const READING_FINGERPRINT: u64 = parse_fingerprint(env!("CPP_READING_FINGERPRINT"));

/// The hexadecimal fingerprint `build.rs` emitted, as a number.
///
/// A `const fn` rather than `u64::from_str_radix` because the value has to be a constant: it is written into the
/// key of every cache file name, and a runtime parse would be a runtime error in a code path that has no way to
/// report one.
const fn parse_fingerprint(text: &str) -> u64 {
    let bytes = text.as_bytes();
    let mut value = 0u64;
    let mut index = 0;

    while index < bytes.len() {
        let digit = match bytes[index] {
            b'0'..=b'9' => bytes[index] - b'0',
            b'a'..=b'f' => bytes[index] - b'a' + 10,
            b'A'..=b'F' => bytes[index] - b'A' + 10,
            // `_` separators are allowed so the constant can be regrouped by hand; anything else is a build script
            // that emitted something other than a number.
            b'_' => {
                index += 1;
                continue;
            }
            _ => panic!("CPP_READING_FINGERPRINT is not hexadecimal"),
        };

        value = value * 16 + digit as u64;
        index += 1;
    }

    value
}

/// The directory name this crate keeps its cache in, under the project root.
///
/// A dot-directory inside the project rather than the operating system's cache directory: it is what the user
/// asked for, it travels with the repository checkout (so CI gets the same warm cache as a developer), and
/// deleting it is an obvious, local operation.
pub const CACHE_DIRECTORY: &str = ".cppls";

/// Everything that determines what a summary of one file contains.
///
/// Built by the caller, which is the only place that knows the compiler configuration and the directory the file
/// is compiled in; this type's job is to turn them into one stable key.
///
/// There is no macro-environment part, and the module documentation gives the evidence for why. The practical
/// consequence is the one that matters: **every part of this key is computable from the text and the path**, so a
/// lookup can happen before the file has been parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SummaryKey {
    pub content_hash: u64,
    pub context_hash: u64,
    pub format_version: u32,
    /// The readers themselves — see [`READING_FINGERPRINT`]. Not a parameter of [`SummaryKey::new`], because no
    /// caller has an opinion about it: it is a property of the binary doing the asking.
    pub reading_fingerprint: u64,
}

impl SummaryKey {
    /// The key of a file whose text and compilation context are known.
    pub fn new(content_hash: u64, context_hash: u64) -> Self {
        SummaryKey {
            content_hash,
            context_hash,
            format_version: FORMAT_VERSION,
            reading_fingerprint: READING_FINGERPRINT,
        }
    }

    /// The key, as the hex string the file name is built from.
    ///
    /// The parts are hashed **together** rather than concatenated as text: the file name stays one fixed width, so
    /// a directory holds a predictable number of entries and nothing has to be parsed back out.
    pub fn file_stem(self) -> String {
        let mut bytes = Vec::with_capacity(32);
        bytes.extend_from_slice(&self.content_hash.to_le_bytes());
        bytes.extend_from_slice(&self.context_hash.to_le_bytes());
        bytes.extend_from_slice(&self.format_version.to_le_bytes());
        bytes.extend_from_slice(&self.reading_fingerprint.to_le_bytes());
        format!("{:016x}", fnv1a64(&bytes))
    }

    /// Where the summary for this key lives, under a project's cache directory.
    ///
    /// Two levels deep — `<cache>/summaries/<first two hex digits>/<key>.bin` — because a flat directory with
    /// tens of thousands of entries is slow to list and unpleasant to debug; the prefix directory is the usual
    /// sharding, and it is derived from the key so nothing has to be looked up.
    ///
    /// The cache **directory** rather than the project root, because the directory is configurable
    /// (`index.cache_dir` in `.cppls.toml`) and the two callers that must agree about it — the store writing and
    /// the watcher deciding what to ignore — would otherwise each join their own idea of the name to the root.
    pub fn path_under(self, cache_directory: &Path) -> PathBuf {
        let stem = self.file_stem();
        cache_directory
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

#[cfg(test)]
mod tests {
    use super::{CACHE_DIRECTORY, FORMAT_VERSION, SummaryKey, content_hash, fnv1a64};
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
        let base = SummaryKey::new(1, 2);
        assert_eq!(base, SummaryKey::new(1, 2), "the same inputs are the same key");

        let variants = [
            SummaryKey::new(9, 2),
            SummaryKey::new(1, 9),
            SummaryKey {
                format_version: FORMAT_VERSION + 1,
                ..base
            },
            SummaryKey {
                reading_fingerprint: base.reading_fingerprint ^ 1,
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
    fn the_key_carries_a_measurement_of_the_readers() {
        // The component that makes "the parser moved, so the store is void" impossible to forget: it is not a
        // decision someone has to remember to make, it is a number computed from the sources that decide what a
        // summary contains (`build.rs`). A zero here would mean the build script found nothing to hash — which is
        // the one way this could silently stop working, since a constant component invalidates nothing.
        assert_ne!(
            super::READING_FINGERPRINT, 0,
            "the fingerprint is the hash of the grammar and of this crate's semantic layer"
        );
        assert_eq!(
            SummaryKey::new(0, 0).reading_fingerprint,
            super::READING_FINGERPRINT,
            "every key this binary builds carries it, and no caller has an opinion about it"
        );
    }

    #[test]
    fn the_same_content_hashes_the_same_wherever_it_is() {
        // The property that makes a branch switch cheap: a summary is keyed by *what the file says*, so a file that
        // comes back to a content the cache has seen — after `git checkout`, a revert, or a rename — is found
        // without being parsed. Nothing in the key mentions a path.
        assert_eq!(content_hash("int x;\n"), content_hash("int x;\n"));
        assert_ne!(content_hash("int x;\n"), content_hash("int y;\n"));

        let key = SummaryKey::new(content_hash("int x;\n"), 7);
        let from_one_root = key.path_under(Path::new("/one/project/.cppls"));
        let from_another = key.path_under(Path::new("D:/elsewhere/checkout/.cppls"));
        assert_eq!(
            from_one_root.file_name(),
            from_another.file_name(),
            "the file name is the content's, the directory is the project's"
        );
    }

    #[test]
    fn the_layout_is_sharded_and_inside_the_project() {
        let key = SummaryKey::new(0, 0);
        let path = key.path_under(&Path::new("/p").join(CACHE_DIRECTORY));
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
}
