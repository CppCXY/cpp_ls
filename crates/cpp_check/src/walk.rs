//! **Which files a run checks** — the project's translation units, or the headers too, and never a build tree.
//!
//! The walk is the tool's own rather than the session's scan: the session's file list is what the *editor* owns, and it
//! is shaped by the compile database (a header is not a translation unit). A check run wants the source files a build
//! would compile, and `--headers` adds the headers that no build compiles on their own.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
const SOURCES: &[&str] = &["c", "cc", "cpp", "cxx", "c++", "m", "mm"];

/// The extensions a header is written with.
const HEADERS: &[&str] = &[
    "h", "hh", "hpp", "hxx", "h++", "inl", "ipp", "tpp", "ixx", "cppm",
];

/// Directories no check run should walk into: build output, VCS data, the analysis's own cache.
const SKIPPED: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".cppls",
    "target",
    "build",
    "out",
    "node_modules",
    ".vs",
    ".vscode",
    ".idea",
];

/// **The files to check** — the project's translation units, in a stable order.
///
/// The walk is the tool's own rather than the session's scan: the session's file list is what the *editor* owns, and it
/// is shaped by the compile database (a header is not a translation unit). A check run wants the source files a build
/// would compile, and `--headers` adds the headers that no build compiles on their own.
pub fn discover(root: &Path, headers: bool, exclude: &[String]) -> Vec<PathBuf> {
    let mut extensions: BTreeSet<&str> = SOURCES.iter().copied().collect();
    if headers {
        extensions.extend(HEADERS.iter().copied());
    }

    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                // **A path substring skips a subtree**, because the directories a project wants ignored are not all
                // named the same: `build`, `cmake-build-debug`, `third_party/godot-cpp`.
                if SKIPPED.iter().any(|skipped| *skipped == name)
                    || exclude
                        .iter()
                        .any(|excluded| path.to_string_lossy().contains(excluded.as_str()))
                {
                    continue;
                }
                stack.push(path);
                continue;
            }
            let extension = path
                .extension()
                .map(|extension| extension.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if extensions.contains(extension.as_str()) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}
