//! The on-disk cache: **one file per file**, keyed by a hash of everything that determines a summary.
//!
//! The rules this module implements are the ones fixes; what follows is the part a reader
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
//! It was forgotten, for many rounds — which is why the key also carries a **fingerprint of the readers**
//! ([`READING_FINGERPRINT`]), taken from the versions the build already carries: a rule that depends on remembering is
//! a rule that gets missed, and this one is a constant of the package.
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
pub mod cache_db;
pub mod cache_lru;

use std::path::{Path, PathBuf};

/// The version of everything that produces a summary: the parser's grammar, this crate's summary schema, and the
/// meaning of the fields in it.
///
/// **Bump on any change that could make an old summary wrong.** See the module documentation for why this is part
/// of the key rather than handled by a migration.
///
/// A parser change reaches the key through [`READING_FINGERPRINT`], which carries `cpp_parser`'s version. A change to
/// *this* crate''s own readings leaves no such trace, so it is this number that is bumped — the same lever a
/// dependency bump uses. The number is still the blunt instrument: every entry becomes unreachable.
///
/// # Why this moved to 4
///
/// **A `_BitInt(`-first class-body member was lost from the summary.** `struct bits { _BitInt(13) signed_wide : 13; }`
/// produced no declaration at all: the specifier loop read `_BitInt(13)` as an unexpanded macro and `signed_wide` as
/// its *type*, so the member had no declarator. Every cache written before that fix holds a summary missing those
/// members, and the member list is built from the summary — the stale answer is visibly wrong, not merely old.
///
/// # Why this moved for version 14 of the codec
///
/// A fact's `type_of` is read from the syntax now rather than assembled out of a declaration's text, and the two
/// readers disagree about **what a type is called**: the old one answered `const [[nodiscard]] constexpr size_type`
/// and `Point>` where the new one answers `size_type` and `std::vector<Point>`. The bytes are the same shape, so an
/// old entry still decodes — which is exactly why this number has to move rather than the codec's: an entry that
/// decodes but says something this build would not write is the case this constant exists for.
///
/// # Why this moved to 8
///
/// **A function's facts record which parameters it frees.** `void release(T* p) { delete p; }` now carries a bit for
/// parameter 0, which is what lets `use-after-free` report `release(p); p->x;` when `release` is declared in a header.
/// The field is new, so the *layout* moved too — but the reason this number moves as well is the one that always
/// applies: an entry written before this describes a function without saying what it does to its arguments, and that
/// is a wrong answer rather than an old one.
///
/// # Why this moved to 7
///
/// **A namespace alias records the namespace it names.** `namespace views = ranges::views;` gave the fact an empty
/// `type_of`, so a query for the names written in `std::views` answered nothing — while every adapter a reader types
/// there (`filter`, `transform`, `reverse`, `drop`, `take`) is declared in `std::ranges::views`. The summary's *layout*
/// is unchanged (the target is the existing `type_of` field), so an old entry decodes happily and keeps the empty
/// answer — which is exactly the case this constant exists for.
///
/// # Why this moved to 5
///
/// **A declaration whose name spells its type's last segment recorded `void`.** `std::any any = 1;` — and `Widget
/// Widget;` or `S S;` in any file — was read as an *out-of-line definition* of `any`, because the specifier `std::any`
/// ends with `::` and the declared name. The type was thrown away, the fact said `void`, and `assign-type-check` then
/// reported the initialisation as a conversion "from `int` to `void`". The bytes are unchanged, so an old entry decodes
/// happily and keeps the wrong type — the case this constant is for.
pub const FORMAT_VERSION: u32 = 8;

/// **The fingerprint of the readers a summary was built by**, in the key.
///
/// # Why this is not [`FORMAT_VERSION`]
///
/// A manual number is only as good as the memory of whoever changed the code, and this one was forgotten for many
/// rounds: the grammar moved while `FORMAT_VERSION` stayed at 1, and every entry on disk quietly described an older
/// reader. So the key also carries a number that is computed rather than remembered.
///
/// # Why it is computed from versions, and not from a hash of the sources
///
/// A `build.rs` used to hash this crate's `src` and `cpp_parser/src` and pass the result in as
/// `CPP_READING_FINGERPRINT`. That was the right idea and it could not be **published**: `cargo publish` builds the
/// crate from the `.crate` archive, where `../cpp_parser/src` does not exist, so the verification build panicked
/// before the crate could be uploaded.
///
/// What replaces it is computed from what the archive does contain:
///
/// * `cpp_parser::VERSION` — the parser's sources move with its version, and a released binary is the only one
///   whose cache survives a restart into a different parser;
/// * this crate's own `CARGO_PKG_VERSION` and [`FORMAT_VERSION`] — the numbers that move when a *reading* of this
///   crate changes.
///
/// # The cost, written down rather than hidden
///
/// A grammar edit **between two versions** — a local build, a debug session — is a cache that still answers for the
/// old grammar, and `FORMAT_VERSION` is the lever for that during development, exactly as it is for a dependency
/// bump that changes a reading. A build that wants a different number can raise `FORMAT_VERSION`; nothing is
/// forgotten silently, and no file outside the package is read.
///
/// # It is still computable before the file is parsed
///
/// A constant of the binary, so a lookup can happen before the parse — which is the property the key must keep, and
/// the reason the macro environment could not stay in it.
pub const READING_FINGERPRINT: u64 = {
    let mut hash = FNV_OFFSET_BASIS;
    hash = fnv1a(hash, cpp_parser::VERSION.as_bytes());
    hash = fnv1a(hash, env!("CARGO_PKG_VERSION").as_bytes());
    hash = fnv1a(hash, &FORMAT_VERSION.to_le_bytes());
    hash
};

/// FNV-1a's 64-bit offset basis and prime — fixed by definition, so the number does not move when a compiler is
/// upgraded.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// **FNV-1a, as a `const fn`** — the fingerprint has to be a constant: it is written into the key of every cache
/// file name, and a runtime hash is a runtime failure waiting for a code path with no way to report one.
const fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
        index += 1;
    }
    hash
}

/// How much the summary cache may hold before a sweep starts removing the oldest entries.
///
/// A budget rather than "never delete", because nothing else bounds the directory: every distinct text a file ever
/// had, in every compilation, is an entry of its own — that is what makes a branch switch cheap — and a cache that
/// only grows is a `.cppls` that one day is the largest thing in the checkout.
pub const CACHE_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;

// Sweep `<cache>/summaries`: remove what can never be served, then the oldest until the rest fits `budget_bytes`.
//
// # Why this is needed, and why it is safe to run beside a session
//
// The key contains [`READING_FINGERPRINT`], which is a hash of the readers' source, so **every change to a reader
// makes every entry written before it unreachable** — not wrong, just never asked for again. Nothing removed them.
// A developer of this crate, or a user upgrading it, left a whole project's worth of summaries behind each time.
//
// A sweep only ever removes files a cache miss would rebuild: an unreachable file is never looked up, and a file
// removed for being old is one the next run re-derives. A file that disappears between the directory listing and
// the removal is somebody else's sweep, and is not an error. Writes are `.tmp` then rename
// ([`crate::index::write_summary`]), so a sweep never sees half a summary; a `.tmp` it finds is removed only when
// it is old enough that no writer can still own it.
//
// A missing cache directory is an empty cache, and the sweep does not create it.
//
// "Oldest" is by modification time — a hit does not rewrite an entry, so this is *first written*, not *last used*,

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
    /// **Which macros the reading was taken under** — a hash of the `(name, body)` pairs in force for this file.
    ///
    /// Zero for a reading taken **with no environment**, which is what the first pass produces and what a buffer on
    /// its own gets. A non-zero value is a reading that knew its includes' `#define`s, and it is the answer to the
    /// question the second pass exists for: `_STD` is `::std::` under one environment and a plain name under none.
    ///
    /// Without this part the two would share one entry and one of them would be wrong.
    pub env_hash: u64,
    pub format_version: u32,
    /// The readers themselves — see [`READING_FINGERPRINT`]. Not a parameter of [`SummaryKey::new`], because no
    /// caller has an opinion about it: it is a property of the binary doing the asking.
    pub reading_fingerprint: u64,
}

impl SummaryKey {
    /// The key of a file whose text and compilation context are known, read **without** a macro environment.
    pub fn new(content_hash: u64, context_hash: u64) -> Self {
        SummaryKey {
            content_hash,
            context_hash,
            env_hash: 0,
            format_version: FORMAT_VERSION,
            reading_fingerprint: READING_FINGERPRINT,
        }
    }

    /// The same file, read under an environment — see [`SummaryKey::env_hash`].
    pub fn with_environment(mut self, env_hash: u64) -> Self {
        self.env_hash = env_hash;
        self
    }

    /// The key, as the hex string the file name is built from.
    ///
    /// The parts are hashed **together** rather than concatenated as text: the file name stays one fixed width, so
    /// a directory holds a predictable number of entries and nothing has to be parsed back out.
    pub fn file_stem(self) -> String {
        let mut bytes = Vec::with_capacity(40);
        bytes.extend_from_slice(&self.content_hash.to_le_bytes());
        bytes.extend_from_slice(&self.context_hash.to_le_bytes());
        bytes.extend_from_slice(&self.env_hash.to_le_bytes());
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
    use std::path::{Path, PathBuf};

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
        assert_eq!(
            base,
            SummaryKey::new(1, 2),
            "the same inputs are the same key"
        );

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
        // The component that makes "the parser moved, so the store is void" impossible to forget: it is not a decision
        // someone has to remember to make, it is a number computed from the versions that decide what a summary
        // contains. A zero here would mean the computation broke — the one way this could silently stop working,
        // since a constant component invalidates nothing.
        assert_ne!(
            super::READING_FINGERPRINT,
            0,
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

    #[allow(dead_code)]
    fn scratch(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("cppls-cache-prune-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        directory
    }

    #[allow(dead_code)]
    fn write(directory: &Path, source: &str, fingerprint_shift: u64) -> PathBuf {
        let mut summary =
            crate::index::summarize(Path::new("/p/a.h"), source, SummaryKey::new(0, 0));
        summary.key.reading_fingerprint ^= fingerprint_shift;
        crate::index::write_summary(&summary, directory).expect("the summary is written")
    }

    // A summary another reader wrote is garbage the moment the reader changes; nothing else is touched.

    // Over budget, the oldest go first — and only as many as it takes.

    // A project that has never been cached has nothing to sweep, and asking must not create the directory.

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
