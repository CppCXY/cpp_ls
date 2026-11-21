//! **When a file's text was last seen on disk** — the one fact that makes a cheap rescan possible.
//!
//! A watcher can overflow: the client or the OS says "events were lost" and the only honest answer is to ask the
//! filesystem what moved. Reading every file to find out costs the whole project's bytes (100 MB and up on a large
//! checkout) for a question whose answer is almost always "nothing". The modification time and the size are what a
//! `stat` gives for one path, and comparing them with the values recorded the last time the session read the file
//! answers the same question for a few microseconds per file.
//!
//! What a stamp is *not*: a cache key. Summaries are keyed by content hash, and a file whose mtime moved but whose
//! bytes did not is re-read and then answers from the same summary — the stamp only decides **which** files to ask
//! about, never what the answer is.

use std::path::Path;

/// A file's modification time and size, as of the last time the session read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    /// Nanoseconds since the Unix epoch, or `0` when the platform would not say — a file whose time cannot be read
    /// always compares as "moved", which costs a re-read rather than a missed change.
    modified: u64,
    size: u64,
}

impl FileStamp {
    /// The stamp the file at `path` has **right now**, or `None` when it cannot be read at all.
    pub fn of(path: &Path) -> Option<FileStamp> {
        let metadata = std::fs::metadata(path).ok()?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_nanos().min(u64::MAX as u128) as u64)
            .unwrap_or(0);
        Some(FileStamp {
            modified,
            size: metadata.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stamp moves when the text is rewritten and stays put when it is not — the two answers the rescan acts on.
    ///
    /// The second write is given a **different size** on purpose: a filesystem whose clock is coarse (or one that
    /// runs on a mount where the mtime has a one-second granularity) would otherwise make this test flaky, and the
    /// size is what makes the stamp trustworthy on those.
    #[test]
    fn a_stamp_only_moves_when_the_file_does() {
        let path = std::env::temp_dir().join(format!(
            "cpp-ls-stamp-{}-{:?}.txt",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, "one").expect("a writable temp directory");
        let first = FileStamp::of(&path).expect("the file is there");
        assert_eq!(FileStamp::of(&path), Some(first), "nothing was written");

        std::fs::write(&path, "two!!").expect("the file is still there");
        let second = FileStamp::of(&path).expect("the file is there");
        assert_ne!(second, first, "the size moved, so the stamp must too");

        std::fs::remove_file(&path).expect("a removable file");
        assert_eq!(FileStamp::of(&path), None, "a missing file has no stamp");
    }
}
