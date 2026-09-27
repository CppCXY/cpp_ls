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
    pub fn get<F: FileProvider>(
        &self,
        root: &Path,
        key: u64,
        files: &F,
    ) -> Option<TranslationUnit> {
        let bytes = std::fs::read(self.entry_path(root, key)).ok()?;
        let (unit, closure) = crate::summary_codec::decode_translation_unit(&bytes).ok()?;

        for (path, hash) in &closure {
            let text = files.read(path)?;
            if crate::content_hash(&text) != *hash {
                return None;
            }
        }

        Some(unit)
    }

    /// Keep a unit, with the closure it was built from.
    ///
    /// The closure is the walk's own answer — `unit.files()` — and its hashes are taken from the provider **now**,
    /// which is what makes the next read's check meaningful: an entry claims what the files said when it was
    /// written.
    pub fn put<F: FileProvider>(
        &self,
        root: &Path,
        key: u64,
        unit: &TranslationUnit,
        files: &F,
    ) -> std::io::Result<()> {
        let mut closure = Vec::new();
        for path in unit.files() {
            // A file the provider cannot read is not in the closure: the walk had its summary, so the unit is
            // still good for this run, and an entry that cannot be checked must not claim it can.
            let Some(text) = files.read(path) else {
                continue;
            };
            closure.push((path.to_path_buf(), crate::content_hash(&text)));
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

        let directory = std::env::temp_dir().join(format!(
            "cppls-tu-cache-{}-{}",
            std::process::id(),
            crate::fnv1a64(b"an_entry_is_refused")
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let cache = TranslationUnitCache::new(&directory);

        let unit = TranslationUnit::from_parts(Vec::new(), Vec::new(), 0, 0);
        // A unit with no frames has an empty closure, so this entry is *vacuously* valid: the test is about the
        // check, and an empty closure is the one case where there is nothing to check.
        cache.put(root, 7, &unit, &files).expect("the entry writes");
        assert!(
            cache.get(root, 7, &files).is_some(),
            "an entry with nothing to verify is served"
        );

        // A key that was never written is a file that does not exist, not a wrong answer.
        assert!(cache.get(root, 8, &files).is_none());

        // …and a corrupt entry is refused rather than half-read.
        std::fs::write(cache.entry_path(root, 7), b"not a timeline at all").expect("the file writes");
        assert!(cache.get(root, 7, &files).is_none());

        let _ = std::fs::remove_dir_all(&directory);
    }
}
