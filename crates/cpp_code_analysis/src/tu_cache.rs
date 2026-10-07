//! **A translation unit's walk, kept across runs.**
//!
//! The one-walk `TranslationUnit` removed the repetition *inside* a run: every file's macro environment is a view
//! of one timeline instead of a walk of its own, and the census measured what that was worth (255 files of the
//! Windows SDK corpus: seeding 147 s → 0.51 s, 2 489 142 macro entries → 42 939). What it did not remove is the
//! repetition *between* runs: open a header, type a character, switch file, and the unit is walked again.
//!
//! This is the layer that keeps it. The model is clangd's **preamble**: the expensive prefix of a file is built
//! once, stored, and reused while it still describes the same text — the key being the *content* of what was read,
//! not its timestamp, and the reuse being refused the moment any of it moves.
//!
//! # The validity key, and why it is the whole design
//!
//! clang's dependency scanner removed a shared `FileManager` across module builds
//! ([b0770d4](https://github.com/llvm/llvm-project/commit/b00de4dd4156874fd5c163e9cecd69a54e45e083)), and the
//! reason is the one that decides this file: **a shared cache must have a key that cannot be stale.** A cache of
//! macro state is the worst kind to get wrong — it does not produce a diagnostic, it produces a *reading*, and
//! every rule downstream then answers about a header that is not the one on disk.
//!
//! So an entry carries:
//!
//! * the reader's fingerprint and both format versions, read back by the decoder
//!   ([`crate::summary_codec::decode_translation_unit`]), so a file written by another binary is a rejected read;
//! * the **closure** — every file the walk entered, with the hash of its contents — and
//!   [`TranslationUnitCache::get`] refuses the entry unless every one of them still hashes the same.
//!
//! The closure is re-read to check it, which is not free: for the SDK corpus that is 13 MB of text hashed in
//! ~60 ms. That is the price of the key, and it is three orders of magnitude below the walk it replaces — clangd
//! pays the same kind of price and caches the `stat`s themselves for it (`PreambleFileStatusCache`).
//!
//! # What is *not* in the key
//!
//! The caller passes the rest of it — [`TranslationUnitCache::get`] takes a `key` number — because only the caller
//! knows what a *compilation* is. In this crate that number is [`crate::index::store::SummaryStore`]'s
//! `context_hash`: the include paths, the `-D`s, the standard, the target and the directory the file sits in. Two
//! compilations of the same file that differ in any of those are two different units, and the entry names differ
//! with them.

use std::path::{Path, PathBuf};

use crate::FileProvider;
use crate::index::store::SummaryStore;
use crate::summary::TranslationUnit;

/// The directory name a project's translation units are cached under, inside the store's cache directory.
pub const TRANSLATION_UNITS_DIRECTORY: &str = "tu";

/// **What the root file's entry is checked against** — the one file whose key is not simply "its whole text".
///
/// Every other file in a closure is only ever *read* by the walk, so an entry for it is valid exactly while its
/// bytes are; the root is the file a person is typing into, and hashing all of it means the entry is refused on
/// every keystroke. This is the rule that makes a body edit free.
///
/// # Why the whole preamble, and why the tail must hold no `#`
///
/// [`RootKey::Preamble`] is the root's bytes up to and including its last preprocessing directive
/// (`directive::preamble_end`). Two facts make that a *sufficient* key, and both are checked at read time:
///
/// ```text
///   the prefix is byte-identical   →  every directive of the file is the same text at the same offset, so the
///                                     walk reads exactly what it read before out of this file
///   the tail holds no `#` at all   →  there is no directive after the bound now, so nothing the walk reads has
///                                     appeared below it either
/// ```
///
/// and the second is why this is sound rather than merely convenient: a `#define` typed into a function body is a
/// directive after the bound, and a check that only compared the prefix would serve a stale timeline for it. A
/// `#` anywhere in the tail — even in a string or a comment, where it is not a directive — refuses the entry, so
/// the answer is conservative in the direction this crate always takes: a miss costs one walk, a wrong hit costs
/// every answer downstream.
///
/// It is also why the bound is the **last** directive rather than clangd's (the first declaration, with the
/// `#define`s before it): the last one is a superset, so there is nothing after it to re-apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKey {
    /// The whole text. The rule for every file that is not a root, and the safe answer for a caller with no text in
    /// hand — it can only ever be *less* generous than [`RootKey::Preamble`], never wrong.
    WholeText,
    /// The root's bytes up to and including its last preprocessing directive.
    Preamble { end: usize, hash: u64 },
}

impl RootKey {
    /// The key for a file whose text is in hand: its preamble, computed from that text.
    ///
    /// A file with no directives has an empty preamble, which is `end = 0` and the hash of the empty string — the
    /// honest answer, and one that is *refused* by the tail rule only if the file holds no `#` at all. A `.cpp`
    /// with no directives and no `#` anywhere is therefore stored under a key of nothing, which is exactly right:
    /// nothing in it can be an input to a timeline.
    pub fn of(source: &str) -> RootKey {
        let end = crate::preprocess::directive::preamble_end(source);
        let end = end.min(source.len());
        RootKey::Preamble {
            end,
            hash: crate::content_hash(&source[..end]),
        }
    }

    /// **The hash this key requires of `text`**, or `None` when the text no longer answers to the key at all.
    ///
    /// One method rather than two so that [`TranslationUnitCache::get`] and [`TranslationUnitCache::put`] cannot
    /// disagree about what a key means: the entry records what this returns, and the read compares against it.
    fn hash_of(self, text: &str) -> Option<u64> {
        match self {
            RootKey::WholeText => Some(crate::content_hash(text)),
            RootKey::Preamble { end, hash } => {
                // The three conditions of the type's documentation, in the order that fails fastest: the file has
                // not been truncated, nothing below the bound could be a directive, and the bytes above it are the
                // ones the entry was built from.
                (text.len() >= end && !text[end..].contains('#') && crate::content_hash(&text[..end]) == hash)
                    .then_some(hash)
            }
        }
    }
}

/// A directory of translation units, one entry per (file, compilation).
pub struct TranslationUnitCache {
    directory: PathBuf,
}

impl TranslationUnitCache {
    /// A cache under `directory`, which is created on the first write.
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        TranslationUnitCache {
            directory: directory.into(),
        }
    }

    /// The cache a store's units belong in — beside the summaries, under one cache directory.
    pub fn of_store<F: FileProvider>(store: &SummaryStore<F>) -> Self {
        TranslationUnitCache::new(store.cache_directory().join(TRANSLATION_UNITS_DIRECTORY))
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Where one compilation's unit for `root` lives.
    ///
    /// Named by the root's path **and** the caller's key: a file compiled two ways has two units, and the second
    /// must not overwrite the first. The name is a hash rather than a path so that a cache directory stays flat
    /// and flat filesystems stay happy — nothing reads the name back, so nothing is lost by it being unreadable.
    pub fn entry_path(&self, root: &Path, key: u64) -> PathBuf {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(crate::paths::normalize_path(root, cfg!(windows)).as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&key.to_le_bytes());

        self.directory
            .join(format!("{:016x}.tu", crate::fnv1a64(&bytes)))
    }

    /// The unit for `root`, when the entry is there and **every file it was built from still hashes the same**.
    ///
    /// `None` is the ordinary answer on the first run and after any edit inside the closure, and it is not an
    /// error: the caller walks the unit and calls [`TranslationUnitCache::put`]. A read that fails for any other
    /// reason — a truncated entry, one written by another binary, a permission problem — is also `None`, because
    /// the reading it would produce is one the caller cannot check.
    ///
    /// `root_key` says what the **root's** entry is checked against, and it is the caller's because the caller is
    /// the one that knows whether the file is being edited: see [`RootKey`], which is the whole of why a keystroke
    /// in a file's body no longer costs the walk of its closure.
    pub fn get<F: FileProvider>(
        &self,
        root: &Path,
        key: u64,
        files: &F,
        root_key: RootKey,
    ) -> Option<TranslationUnit> {
        let bytes = std::fs::read(self.entry_path(root, key)).ok()?;
        let (unit, closure) = crate::summary_codec::decode_translation_unit(&bytes).ok()?;

        // The root's own entry is the one the rule above is about; every other file is compared whole. Compared by
        // the **normalized** spelling, because the walk records `FileSummary::path` while the caller spells the
        // path it asked with, and the two differ in case and separators on Windows.
        let wanted = crate::paths::normalize_path(root, cfg!(windows));

        for (path, hash) in &closure {
            let text = files.read(path)?;
            let is_the_root = crate::paths::normalize_path(path, cfg!(windows)) == wanted;

            let holds = if is_the_root {
                root_key.hash_of(&text) == Some(*hash)
            } else {
                crate::content_hash(&text) == *hash
            };

            if !holds {
                return None;
            }
        }

        Some(unit)
    }

    /// Keep a unit, with the closure it was built from.
    ///
    /// The closure is the walk's own answer — `unit.files()` — and its hashes are taken from the provider **now**,
    /// which is what makes the next read's check meaningful: an entry claims what the files said when it was
    /// written. The root's entry records only its preamble when the caller says to, so that the claim is about the
    /// part of it a walk can read.
    pub fn put<F: FileProvider>(
        &self,
        root: &Path,
        key: u64,
        unit: &TranslationUnit,
        files: &F,
        root_key: RootKey,
    ) -> std::io::Result<()> {
        let wanted = crate::paths::normalize_path(root, cfg!(windows));
        let mut closure = Vec::new();

        for path in unit.files() {
            // A file the provider cannot read is not in the closure: the walk had its summary, so the unit is
            // still good for this run, and an entry that cannot be checked must not claim it can.
            let Some(text) = files.read(path) else {
                continue;
            };

            let is_the_root = crate::paths::normalize_path(path, cfg!(windows)) == wanted;
            let hash = if is_the_root {
                // The root's entry records what the key says it should, and falls back to the whole file when the
                // key does not apply — a `#` below the bound, or a caller with no text in hand. `get` applies the
                // same rule, so an entry written either way is read by the rule that wrote it.
                root_key
                    .hash_of(&text)
                    .unwrap_or_else(|| crate::content_hash(&text))
            } else {
                crate::content_hash(&text)
            };

            closure.push((path.to_path_buf(), hash));
        }

        std::fs::create_dir_all(&self.directory)?;
        std::fs::write(
            self.entry_path(root, key),
            crate::summary_codec::encode_translation_unit(unit, &closure),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryFiles;

    /// A cache under a directory of its own, so two tests never share an entry.
    fn cache_for(what: &'static [u8]) -> (TranslationUnitCache, PathBuf) {
        let directory = std::env::temp_dir().join(format!(
            "cppls-tu-cache-{}-{}",
            std::process::id(),
            crate::fnv1a64(what)
        ));
        let _ = std::fs::remove_dir_all(&directory);
        (TranslationUnitCache::new(&directory), directory)
    }

    /// A unit whose frames are exactly `files`, the first being the root — the shape `put`'s root rule reads.
    ///
    /// Nothing here is a real walk: the tests below are about the *key*, and a timeline with the right frames and
    /// no events says everything a key check needs.
    fn a_unit_over(files: &[&str]) -> TranslationUnit {
        let frames = files
            .iter()
            .enumerate()
            .map(|(index, file)| crate::summary::TuFrame {
                file: PathBuf::from(file),
                parent: (index > 0).then_some(0),
                from_in_parent: index * 10,
                entry_seq: 0,
                tout: files.len() as u32,
                visit_once: false,
            })
            .collect();

        TranslationUnit::from_parts(Vec::new(), frames, 0, 0)
    }

    /// The cache's own rules, without a corpus: an entry round-trips, and a moved file refuses it.
    ///
    /// What is *not* asserted here is that the decoded unit answers like the original — that is
    /// `tests/translation_unit.rs`'s job, on a timeline built by a real walk.
    #[test]
    fn an_entry_is_refused_when_a_file_in_the_closure_moved() {
        let root = Path::new("/p/main.cpp");
        let mut files = MemoryFiles::new();
        files.insert("/p/main.cpp", "#include \"a.h\"\n");
        files.insert("/p/a.h", "#define A 1\n");

        let (cache, directory) = cache_for(b"an_entry_is_refused");

        let unit = TranslationUnit::from_parts(Vec::new(), Vec::new(), 0, 0);
        // A unit with no frames has an empty closure, so this entry is *vacuously* valid: the test is about the
        // check, and an empty closure is the one case where there is nothing to check.
        let key = RootKey::WholeText;
        cache.put(root, 7, &unit, &files, key).expect("the entry writes");
        assert!(
            cache.get(root, 7, &files, key).is_some(),
            "an entry with nothing to verify is served"
        );

        // A key that was never written is a file that does not exist, not a wrong answer.
        assert!(cache.get(root, 8, &files, key).is_none());

        // …and a corrupt entry is refused rather than half-read.
        std::fs::write(cache.entry_path(root, 7), b"not a timeline at all").expect("the file writes");
        assert!(cache.get(root, 7, &files, key).is_none());

        let _ = std::fs::remove_dir_all(&directory);
    }

    /// **The rule that matters: typing in the body of a file does not invalidate its own unit, and a directive
    /// typed below the bound does.**
    ///
    /// The failing case this exists for was every keystroke in the file being edited: the root is a frame of its
    /// own unit, its whole text was hashed, and so the entry was refused on each character. See [`RootKey`].
    #[test]
    fn a_body_edit_keeps_the_unit_and_a_directive_below_the_bound_refuses_it() {
        let root = Path::new("/p/main.cpp");
        let header = "#pragma once\n#define A 1\n";
        let body = "#include \"a.h\"\nint main() { return ZERO; }\n";

        let mut files = MemoryFiles::new();
        files.insert("/p/a.h", header);
        files.insert("/p/main.cpp", body);

        let (cache, directory) = cache_for(b"a_body_edit_keeps_the_unit");
        // The root has to be a **frame** for the root rule to have anything to apply to: `put` walks `unit.files()`
        // and only the entry whose path is the root is keyed on the preamble.
        let unit = a_unit_over(&["/p/main.cpp", "/p/a.h"]);

        let key = RootKey::of(body);
        cache.put(root, 7, &unit, &files, key).expect("the entry writes");
        assert!(
            cache.get(root, 7, &files, key).is_some(),
            "the entry that was just written is served"
        );

        // **A character typed into the body.** Same preamble, no `#` below it — the unit is still the unit, and
        // this is the whole point: it used to be refused here.
        let typed = "#include \"a.h\"\nint main() { return ZERO + 1; }\n";
        files.insert("/p/main.cpp", typed);
        assert!(
            cache.get(root, 7, &files, RootKey::of(typed)).is_some(),
            "typing in a body does not change what a walk reads out of the file"
        );

        // **A directive typed below the bound.** Nothing above it moved, so a check that only compared the prefix
        // would serve the old timeline — and the new `#define` is exactly what the walk would have read.
        let with_a_directive = "#include \"a.h\"\nint main() { return ZERO; }\n#define LATE 2\n";
        files.insert("/p/main.cpp", with_a_directive);
        assert!(
            cache.get(root, 7, &files, RootKey::of(with_a_directive)).is_none(),
            "a `#define` below the bound is a directive the walk would have read"
        );

        // …and a `#` that is not a directive refuses it too, which is the conservative direction: a miss costs one
        // walk, a wrong hit costs every answer downstream.
        let a_hash_in_the_body = "#include \"a.h\"\nint main() { return ZERO; } // #\n";
        files.insert("/p/main.cpp", a_hash_in_the_body);
        assert!(
            cache.get(root, 7, &files, RootKey::of(a_hash_in_the_body)).is_none(),
            "a `#` the scan cannot prove is not a directive refuses the entry"
        );

        // An edit **above** the bound moves the prefix, which is what the key is.
        let moved_include = "#include \"a.h\"\n#include \"b.h\"\nint main() { return ZERO; }\n";
        files.insert("/p/main.cpp", moved_include);
        assert!(
            cache.get(root, 7, &files, RootKey::of(moved_include)).is_none(),
            "an edit above the bound is an edit to the preamble"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A key computed from one text does not answer for another, and `WholeText` is never more generous than
    /// `Preamble` — the property that makes it the safe fallback for a caller with no text in hand.
    #[test]
    fn a_key_is_about_the_text_it_was_taken_from() {
        let text = "#include <vector>\nint main() {}\n";
        assert!(RootKey::of(text).hash_of(text).is_some());

        let other = "#include <string>\nint main() {}\n";
        assert!(
            RootKey::of(other).hash_of(text).is_none(),
            "a key taken from another text does not answer for this one"
        );

        assert!(
            RootKey::WholeText.hash_of(text).is_some(),
            "the whole-text rule applies to any text; it is the *entry* that then has to match it"
        );
    }
}
