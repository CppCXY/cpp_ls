//! Where a system's headers are, when no compiler can be asked.
//!
//! The last resort, and it is one on purpose: a compiler knows its own search list — the target triple and the
//! version are in the paths it prints — and nothing here can reconstruct that from a directory layout. So this
//! module answers only the question a *conventional* layout can answer: **is there a standard library on this
//! machine at all, and where does this operating system keep it?**
//!
//! ```text
//! Windows          Microsoft's layout, from `msvc` — the toolset's `include` and the Windows SDK's parts
//! Linux            /usr/include, /usr/local/include, /usr/include/<triplet>, /usr/include/c++/<version>
//! macOS            the SDK `xcrun` names, then /usr/include and the command line tools' C++ headers
//! ```
//!
//! # What a caller is buying, and what it is not
//!
//! Headers resolve: `#include <vector>` finds a file and its declarations are indexed, which is the difference
//! between a project that is half-usable and one that is not usable at all. What is *not* there is the compiler's
//! macro table — and that is not a detail: `#ifdef _WIN32`, `#if _MSC_VER >= 1930` and every `__cplusplus` test
//! then answer `Unknown`, which the analysis reports rather than guesses (see [`crate::Known`]). A caller that
//! took a guess for a fact would turn "I did not find a compiler" into a wrong answer about which code is compiled
//! — the one failure this layer exists to avoid.
//!
//! # Why the list is short
//!
//! Because every entry is a claim about a machine this was not measured on. Two entries that come from the
//! platform's own convention (`/usr/include`, and the toolset layout on Windows) are worth having; a list of
//! fifteen directory names copied from a wiki is not, because a wrong include path makes a header resolve to the
//! wrong file — and that is worse than not resolving it. So each entry below is either the POSIX convention or
//! measured in `docs/msvc-notes.md`.

use std::path::{Path, PathBuf};

use super::msvc::{self, WindowsLayout};
use super::toolchain::CommandRunner;

/// How many versions of a compiler's C++ headers are considered.
///
/// `/usr/include/c++/12`, `/usr/include/c++/13` … a machine that has upgraded its toolchain keeps the old headers,
/// and the newest is the one a compiler would pick. The bound is what keeps a directory listing with fifty entries
/// from turning into fifty include paths.
const MAX_VERSIONS: usize = 4;

/// A guessed set of system header directories, and the machine it was guessed for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemHeaders {
    pub directories: Vec<PathBuf>,
    /// What the guess was based on, for the report a user reads: `"Microsoft Visual Studio 2022"`,
    /// `"/usr/include"`.
    pub based_on: String,
}

/// The system header directories this machine has, with nothing run but `xcrun` on macOS.
///
/// `None` when nothing conventional exists — a machine with no standard library installed, which is a real state
/// and one the analysis reports as "no system headers" rather than papering over.
pub fn discover(runner: &impl CommandRunner, layout: &WindowsLayout) -> Option<SystemHeaders> {
    if cfg!(windows) {
        return from_windows(runner, layout);
    }

    from_unix(runner)
}

/// Microsoft's layout: the toolset's `include` and the SDK's parts, with no compiler asked and no macros known.
///
/// This is still worth doing on a machine with Visual Studio installed and `cl` unrunnable (the measured case: the
/// compiler needs `INCLUDE`, which is exactly what is built here — so usually the compiler *can* be asked, and
/// this path is for a machine where it cannot). `vswhere` is still run: it is a system tool asking the installer a
/// question, not a compiler being asked about itself, and it costs ~24 ms.
fn from_windows(runner: &impl CommandRunner, layout: &WindowsLayout) -> Option<SystemHeaders> {
    let found = msvc::discover(runner, layout)?;
    if found.include.is_empty() {
        return None;
    }

    let toolset = found.toolset.version.clone();
    let sdk = found
        .sdk
        .as_ref()
        .map(|sdk| format!(", Windows SDK {}", sdk.version))
        .unwrap_or_default();

    Some(SystemHeaders {
        based_on: format!("the MSVC toolset {toolset}{sdk}"),
        directories: found.include,
    })
}

/// The conventions of a Unix-like system, plus where a compiler keeps its C++ headers.
fn from_unix(runner: &impl CommandRunner) -> Option<SystemHeaders> {
    let mut directories = Vec::new();
    let mut based_on = Vec::new();

    for conventional in ["/usr/local/include", "/usr/include"] {
        let path = PathBuf::from(conventional);
        if path.is_dir() {
            directories.push(path);
            based_on.push(conventional.to_string());
        }
    }

    // The C++ headers live under a version directory (`/usr/include/c++/13`) or under a target triple first
    // (`/usr/include/x86_64-linux-gnu/c++/13`). Both are conventions of GCC's packaging, and the newest of each is
    // the one a compiler would pick — so the triples are walked in a sorted order to keep two runs identical.
    for cxx_root in cxx_header_roots() {
        directories.extend(newest_versions(&cxx_root, MAX_VERSIONS));
        based_on.push(cxx_root.display().to_string());
    }

    // The SDK on macOS, which is a system tool's answer rather than a guess: `xcrun` is part of the command line
    // tools, and asking it is the same move as asking a compiler about itself.
    if let Some(sdk) = macos_sdk(runner) {
        directories.push(sdk.clone());
        based_on.push(sdk.display().to_string());
    }

    directories.dedup();
    if directories.is_empty() {
        return None;
    }

    Some(SystemHeaders {
        directories,
        based_on: based_on.join(", "),
    })
}

/// The directories that hold a compiler's C++ headers, by the conventions of a Unix packaging.
fn cxx_header_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/usr/include/c++")];

    // `/usr/include/<triplet>/c++`, which is where Debian's packaging puts a target's own C++ headers.
    if let Ok(entries) = std::fs::read_dir("/usr/include") {
        let mut triples: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path().join("c++"))
            .filter(|path| path.is_dir())
            .collect();
        triples.sort();
        roots.extend(triples);
    }

    // And the one a hand-installed compiler uses, under its own lib directory.
    if let Ok(entries) = std::fs::read_dir("/usr/lib") {
        let mut llvm: Vec<PathBuf> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.starts_with("llvm").then(|| entry.path())?
                    .join("lib")
                    .join("clang")
                    .exists()
                    .then(|| entry.path().join("lib").join("clang"))
            })
            .collect();
        llvm.sort();

        for root in llvm {
            if let Ok(versions) = std::fs::read_dir(&root) {
                let mut versions: Vec<PathBuf> = versions
                    .flatten()
                    .map(|entry| entry.path().join("include"))
                    .filter(|path| path.is_dir())
                    .collect();
                versions.sort();
                roots.extend(versions);
            }
        }
    }

    roots
}

/// The newest `MAX_VERSIONS` subdirectories of a versioned header root, newest first.
fn newest_versions(root: &Path, take: usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };

    let mut versions: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.chars().next().is_some_and(|first| first.is_ascii_digit()))
        })
        .collect();

    // Newest first, by the numeric value of the whole name (so `13` comes after `9`).
    versions.sort_by_key(|path| {
        std::cmp::Reverse(
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.parse::<u64>().ok())
                .unwrap_or(0),
        )
    });
    versions.truncate(take);
    versions
}

/// The macOS SDK's include directory, asked of `xcrun`.
fn macos_sdk(runner: &impl CommandRunner) -> Option<PathBuf> {
    let output = runner.run(Path::new("xcrun"), &["--show-sdk-path"], &[])?;
    if !output.succeeded {
        return None;
    }

    let sdk = PathBuf::from(output.stdout.trim());
    let include = sdk.join("usr").join("include");
    include.is_dir().then_some(include)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_without_a_conventional_layout_has_no_guess() {
        // The honest answer on a system where nothing is where the conventions say: no headers, which the analysis
        // reports as unresolved includes rather than filling in a directory that does not exist.
        let nowhere = WindowsLayout {
            program_files: vec![PathBuf::from("/nowhere")],
            windows_sdk_dir: None,
            vc_tools_install_dir: None,
        };

        let runner = NoCommands;

        if cfg!(windows) {
            assert!(from_windows(&runner, &nowhere).is_none());
        }
        if !cfg!(windows) {
            // A Unix machine may well have `/usr/include`; what this asserts is that nothing is *invented*: every
            // directory in the answer exists.
            if let Some(found) = from_unix(&runner) {
                for directory in &found.directories {
                    assert!(directory.is_dir(), "{}", directory.display());
                }
                assert!(!found.based_on.is_empty());
            }
        }
    }

    #[test]
    fn only_versioned_directories_are_taken_and_the_newest_first() {
        let root = std::env::temp_dir().join("cppls-system-headers");
        let _ = std::fs::remove_dir_all(&root);
        for version in ["9", "12", "13", "not-a-version"] {
            std::fs::create_dir_all(root.join(version)).expect("the fixture directory");
        }

        let found = newest_versions(&root, 2);
        let names: Vec<String> = found
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert_eq!(names, ["13", "12"], "newest first, and nothing else");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A runner that refuses everything, for the paths that must not run anything.
    struct NoCommands;

    impl CommandRunner for NoCommands {
        fn run(
            &self,
            _: &Path,
            _: &[&str],
            _: &[(std::ffi::OsString, std::ffi::OsString)],
        ) -> Option<super::super::toolchain::Output> {
            None
        }
    }
}
