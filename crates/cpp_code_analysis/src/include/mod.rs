//! Finding the file an `#include` names.
//!
//! # The rules, and why each one exists
//!
//! * **`#include "x.h"` searches the including file's own directory first.** That is the whole difference
//!   between the two spellings, and it is what lets a header include its own neighbours without the build
//!   system listing every directory.
//! * **`#include <x.h>` does not.** A system header that accidentally picks up a project file of the same
//!   name is a bug that is very hard to see; the standard forbids the search precisely so that it cannot
//!   happen.
//! * **User paths (`-I`) before system paths (`-isystem`).** A project overriding a system header with its
//!   own copy of the same name is a real technique, and it only works if the order is respected.
//! * **`#include_next` continues from where the current file was found**, not from the start. It exists so
//!   that a wrapper header can forward to the header it shadows, and searching from the start would find
//!   the wrapper again.
//!
//! # What a failure is
//!
//! Not an error. An unresolved include is the normal state of a file being written, of a project without a
//! compile database, and of a codebase whose dependencies are not checked out. The analysis continues
//! without the included declarations, which is worse than having them and much better than refusing to
//! analyse the file at all. [`Unresolved`] carries what was searched so that a "cannot find header" message
//! can say where it looked.

use std::path::{Path, PathBuf};

use crate::{
    config::CompilerConfig,
    directive::{Include, IncludeForm},
    paths::{FileId, FileProvider, PathInterner, join_normalized, normalize_path},
};

/// A resolved include.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub file: FileId,
    /// The path as it was found: the candidate that existed.
    pub path: PathBuf,
    /// Which directory it was found in — the entry of the search list that matched.
    ///
    /// Needed for `#include_next`, and worth keeping for a consumer that wants to distinguish a project
    /// header from a system one without re-running the search.
    pub found_in: FoundIn,
}

/// Where in the search order a file was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoundIn {
    /// The including file's own directory.
    LocalDirectory,
    /// Index into [`CompilerConfig::include_paths`].
    SearchPath(usize),
}

/// An include that could not be resolved, and what was tried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unresolved {
    /// The name as written, with its delimiters removed.
    pub name: Box<str>,
    /// Every candidate path that was tried, in order.
    ///
    /// Kept because "cannot find vector" is not actionable and "looked in these four directories" is.
    pub searched: Vec<PathBuf>,
}

/// The outcome of resolving an include.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Resolved(Resolved),
    Unresolved(Unresolved),
}

impl Resolution {
    pub fn resolved(&self) -> Option<&Resolved> {
        match self {
            Resolution::Resolved(resolved) => Some(resolved),
            Resolution::Unresolved(_) => None,
        }
    }

    pub fn is_resolved(&self) -> bool {
        matches!(self, Resolution::Resolved(_))
    }
}

/// Resolves includes against a provider and a configuration.
///
/// Holds a `&mut PathInterner` because resolving is the moment a file's identity is decided: the id has to
/// be minted by whoever keeps the table, and there is no useful resolution that does not produce one.
pub struct IncludeResolver<'a, F: FileProvider> {
    files: &'a F,
    config: &'a CompilerConfig,
}

impl<'a, F: FileProvider> IncludeResolver<'a, F> {
    pub fn new(files: &'a F, config: &'a CompilerConfig) -> Self {
        IncludeResolver { files, config }
    }

    /// Resolve an include written in the file at `including`.
    ///
    /// `including` is the *directory* of the file doing the including, which the caller has because only it
    /// knows how that file was itself found.
    pub fn resolve(
        &self,
        include: &Include,
        including: &Path,
        origin: Option<&FoundIn>,
        interner: &mut PathInterner,
    ) -> Resolution {
        let name = Path::new(&*include.target);
        let mut searched = Vec::new();

        // A macro target (`#include HEADER`) cannot be resolved here: its value comes from expansion, and
        // guessing is not available. Reported as unresolved with nothing searched, which is the honest
        // description — no directory could have helped.
        if include.form == IncludeForm::Macro {
            return Resolution::Unresolved(Unresolved {
                name: include.target.clone(),
                searched,
            });
        }

        // A target that is absolute skips the search entirely. Rare and legal.
        if name.is_absolute() {
            if let Some(path) = self.existing(name) {
                return Resolution::Resolved(self.resolved(
                    path,
                    FoundIn::LocalDirectory,
                    interner,
                ));
            }
            searched.push(name.to_path_buf());
            return Resolution::Unresolved(Unresolved {
                name: include.target.clone(),
                searched,
            });
        }

        // `#include_next` starts *after* the place the current file was found, so that a wrapper header
        // forwards to the header it shadows instead of finding itself.
        let mut candidates: Vec<(PathBuf, FoundIn)> = Vec::new();
        let skip_through = if include.is_next {
            origin.map(|origin| match origin {
                FoundIn::LocalDirectory => 0usize,
                FoundIn::SearchPath(index) => index + 1,
            })
        } else {
            None
        };

        if include.form == IncludeForm::Quote && skip_through.is_none() {
            candidates.push((
                join_normalized(including, name, self.case_insensitive()),
                FoundIn::LocalDirectory,
            ));
        }

        for (index, include_path) in self.config.include_paths.iter().enumerate() {
            if skip_through.is_some_and(|skip| index < skip) {
                continue;
            }
            // The directory is resolved against the working directory, because a relative `-I` is relative
            // to where the compiler ran and not to any file.
            let directory = self
                .config
                .resolve_against_working_directory(&include_path.directory);
            candidates.push((
                join_normalized(&directory, name, self.case_insensitive()),
                FoundIn::SearchPath(index),
            ));
        }

        for (candidate, found_in) in candidates {
            if let Some(path) = self.existing(&candidate) {
                return Resolution::Resolved(self.resolved(path, found_in, interner));
            }
            searched.push(candidate);
        }

        Resolution::Unresolved(Unresolved {
            name: include.target.clone(),
            searched,
        })
    }

    /// The file at this path, as it should be stored, if it is there.
    ///
    /// Returns the *normalized* path so that the id's stored spelling is the same one comparison uses: a
    /// consumer displaying a file's name should not be shown `inc/../inc/a.h`.
    fn existing(&self, candidate: &Path) -> Option<PathBuf> {
        if self.files.exists(candidate) {
            return Some(candidate.to_path_buf());
        }

        // A case-insensitive filesystem was asked for a path with the wrong case. Probing every case
        // combination is not possible, so the practical fallback is to try the other common spelling of the
        // whole path — which is what happens when a project is checked out on Linux and analysed on
        // Windows, the case this exists for.
        if self.case_insensitive() {
            let lowered = normalize_path(candidate, true);
            let lowered = Path::new(&lowered);
            if self.files.exists(lowered) {
                return Some(lowered.to_path_buf());
            }
        }

        None
    }

    fn resolved(&self, path: PathBuf, found_in: FoundIn, interner: &mut PathInterner) -> Resolved {
        let file = interner.intern(&path);
        Resolved {
            file,
            path,
            found_in,
        }
    }

    fn case_insensitive(&self) -> bool {
        self.files.is_case_insensitive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::IncludePath, paths::MemoryFiles};

    /// A project tree: a local header, and two search directories holding the same name.
    fn project() -> MemoryFiles {
        MemoryFiles::new()
            .with_case_insensitive(false)
            .with_file("src/main.cpp", "")
            .with_file("src/local.h", "local")
            .with_file("user/vector", "user's vector")
            .with_file("system/vector", "system's vector")
            .with_file("system/stdio.h", "stdio")
    }

    fn config() -> CompilerConfig {
        CompilerConfig::new()
            .with_include_path("user")
            .with_system_include_path("system")
    }

    fn resolve(
        name: &str,
        form: IncludeForm,
        including: &str,
        case_insensitive: bool,
    ) -> Resolution {
        let files = project().with_case_insensitive(case_insensitive);
        let config = config();
        let resolver = IncludeResolver::new(&files, &config);
        let mut interner = PathInterner::new(case_insensitive);

        let include = Include {
            form,
            target: name.into(),
            is_next: false,
        };

        // `including` is given as a path so the test reads like the file it is describing.
        let mut resolution = resolver.resolve(&include, Path::new(including), None, &mut interner);

        // The config is a local; the resolver borrows it, so the resolution is detached before returning.
        if let Resolution::Resolved(resolved) = &mut resolution {
            resolved.path = PathBuf::from(normalize_path(&resolved.path, case_insensitive));
        }

        resolution
    }

    #[test]
    fn a_quoted_include_prefers_the_files_own_directory() {
        let resolution = resolve("local.h", IncludeForm::Quote, "src", false);

        assert_eq!(
            resolution.resolved().map(|it| it.path.as_path()),
            Some(Path::new("src/local.h"))
        );
        assert_eq!(
            resolution.resolved().map(|it| &it.found_in),
            Some(&FoundIn::LocalDirectory)
        );
    }

    #[test]
    fn a_quoted_include_falls_back_to_the_search_paths() {
        let resolution = resolve("stdio.h", IncludeForm::Quote, "src", false);

        assert_eq!(
            resolution.resolved().map(|it| it.path.as_path()),
            Some(Path::new("system/stdio.h"))
        );
        assert_eq!(
            resolution.resolved().map(|it| &it.found_in),
            Some(&FoundIn::SearchPath(1))
        );
    }

    /// **The one thing the two spellings disagree about.** An angled include must not find a file next to
    /// the including file: a system header picking up a project file of the same name is a bug the standard
    /// exists to prevent.
    #[test]
    fn an_angled_include_does_not_search_the_files_own_directory() {
        let resolution = resolve("local.h", IncludeForm::Angle, "src", false);

        assert!(
            !resolution.is_resolved(),
            "`src/local.h` exists but must not be found: {resolution:?}"
        );
        assert_eq!(
            resolution,
            Resolution::Unresolved(Unresolved {
                name: "local.h".into(),
                searched: vec![
                    PathBuf::from("user/local.h"),
                    PathBuf::from("system/local.h")
                ],
            })
        );
    }

    /// A project directory shadows a system one, which is why the order of the two lists matters.
    #[test]
    fn a_user_path_shadows_a_system_path() {
        let resolution = resolve("vector", IncludeForm::Angle, "src", false);

        assert_eq!(
            resolution.resolved().map(|it| it.path.as_path()),
            Some(Path::new("user/vector"))
        );
        assert_eq!(
            resolution.resolved().map(|it| &it.found_in),
            Some(&FoundIn::SearchPath(0))
        );
    }

    /// A failure reports where it looked, because "cannot find vector" is not actionable.
    #[test]
    fn an_unresolved_include_lists_what_was_searched() {
        let resolution = resolve("missing.h", IncludeForm::Quote, "src", false);

        match resolution {
            Resolution::Unresolved(unresolved) => {
                assert_eq!(&*unresolved.name, "missing.h");
                assert_eq!(
                    unresolved.searched,
                    vec![
                        PathBuf::from("src/missing.h"),
                        PathBuf::from("user/missing.h"),
                        PathBuf::from("system/missing.h"),
                    ]
                );
            }
            Resolution::Resolved(resolved) => panic!("expected a failure, got {resolved:?}"),
        }
    }

    /// A macro target cannot be resolved here — its value comes from expansion — and saying so is better
    /// than searching for a file named `BOOST_VERSION_HEADER`.
    #[test]
    fn a_macro_include_is_unresolved_and_searches_nothing() {
        let resolution = resolve("BOOST_HEADER", IncludeForm::Macro, "src", false);

        assert_eq!(
            resolution,
            Resolution::Unresolved(Unresolved {
                name: "BOOST_HEADER".into(),
                searched: Vec::new(),
            })
        );
    }

    /// `#include_next` continues after the directory the current file was found in, so a wrapper header
    /// forwards to the header it shadows instead of finding itself.
    #[test]
    fn include_next_continues_past_the_current_file() {
        // A wrapper in the user path that wants the system header of the same name.
        let files = MemoryFiles::new()
            .with_file("user/vector", "the wrapper")
            .with_file("system/vector", "the real one");
        let config = config();
        let resolver = IncludeResolver::new(&files, &config);
        let mut interner = PathInterner::new(false);

        let include = Include {
            form: IncludeForm::Angle,
            target: "vector".into(),
            is_next: true,
        };

        // Found in the user path, so the search resumes at the *next* path.
        let resolution = resolver.resolve(
            &include,
            Path::new("src"),
            Some(&FoundIn::SearchPath(0)),
            &mut interner,
        );

        assert_eq!(
            resolution
                .resolved()
                .map(|it| normalize_path(&it.path, false)),
            Some("system/vector".to_string())
        );
    }

    /// Without `#include_next`, the same file resolves to the wrapper — which is the difference the
    /// directive makes and the reason it exists.
    #[test]
    fn an_ordinary_include_finds_the_wrapper() {
        let files = MemoryFiles::new()
            .with_file("user/vector", "the wrapper")
            .with_file("system/vector", "the real one");
        let config = config();
        let resolver = IncludeResolver::new(&files, &config);
        let mut interner = PathInterner::new(false);

        let include = Include {
            form: IncludeForm::Angle,
            target: "vector".into(),
            is_next: false,
        };

        let resolution = resolver.resolve(
            &include,
            Path::new("src"),
            Some(&FoundIn::SearchPath(0)),
            &mut interner,
        );

        assert_eq!(
            resolution
                .resolved()
                .map(|it| normalize_path(&it.path, false)),
            Some("user/vector".to_string())
        );
    }

    /// Two includes of one file produce one id, so the graph has one node for it however it was reached.
    #[test]
    fn two_routes_to_one_file_produce_one_id() {
        let files = project();
        let config = config();
        let resolver = IncludeResolver::new(&files, &config);
        let mut interner = PathInterner::new(false);

        let quoted = Include {
            form: IncludeForm::Quote,
            target: "stdio.h".into(),
            is_next: false,
        };
        let angled = Include {
            form: IncludeForm::Angle,
            target: "stdio.h".into(),
            is_next: false,
        };

        let first = resolver.resolve(&quoted, Path::new("src"), None, &mut interner);
        let second = resolver.resolve(&angled, Path::new("src"), None, &mut interner);

        assert_eq!(
            first.resolved().map(|it| it.file),
            second.resolved().map(|it| it.file)
        );
        assert_eq!(interner.len(), 1);
    }

    /// A relative `-I` is relative to where the compiler ran, not to any file.
    #[test]
    fn a_relative_search_path_resolves_against_the_working_directory() {
        let files = MemoryFiles::new().with_file("build/inc/a.h", "found");
        let config = CompilerConfig::new()
            .with_include_path("inc")
            .with_working_directory("build");
        let resolver = IncludeResolver::new(&files, &config);
        let mut interner = PathInterner::new(false);

        let include = Include {
            form: IncludeForm::Angle,
            target: "a.h".into(),
            is_next: false,
        };

        let resolution = resolver.resolve(&include, Path::new("src"), None, &mut interner);

        assert_eq!(
            resolution
                .resolved()
                .map(|it| normalize_path(&it.path, false)),
            Some("build/inc/a.h".to_string())
        );
    }

    /// An absolute target is used as written rather than searched for.
    ///
    /// Asserted through the *searched* list, because "absolute" and the spelling of a root are
    /// platform-specific — `/opt/inc/a.h` is not a rooted path on Windows — so a test that finds a file by
    /// absolute path is a test of the platform. What distinguishes the two cases is the list: an absolute
    /// target is tried exactly as written, so a failure names one path, while a relative one is tried
    /// against every search directory in turn.
    #[test]
    fn an_absolute_target_is_tried_only_as_written() {
        let files = project();
        let config = config();
        let resolver = IncludeResolver::new(&files, &config);
        let mut interner = PathInterner::new(false);

        // Built from the real root so the test is about absoluteness and not about separators.
        let absolute = std::env::temp_dir().join("definitely-not-here-a1b2c3.h");
        let name: Box<str> = absolute.to_string_lossy().into_owned().into();
        let include = Include {
            form: IncludeForm::Angle,
            target: name.clone(),
            is_next: false,
        };

        let resolution = resolver.resolve(&include, Path::new("src"), None, &mut interner);

        assert_eq!(
            resolution,
            Resolution::Unresolved(Unresolved {
                name,
                searched: vec![absolute],
            }),
            "the target was tried as written and nothing else was"
        );
    }

    /// A relative target *is* searched for, which is the other half of the property above.
    #[test]
    fn a_relative_target_is_searched_for() {
        let resolution = resolve("nope.h", IncludeForm::Angle, "src", false);

        match resolution {
            Resolution::Unresolved(unresolved) => assert_eq!(
                unresolved.searched,
                vec![PathBuf::from("user/nope.h"), PathBuf::from("system/nope.h")],
                "every search directory was tried"
            ),
            Resolution::Resolved(resolved) => panic!("expected a failure, got {resolved:?}"),
        }
    }

    /// A configuration with no search paths still resolves a quoted include next to the file, which is
    /// what makes a project without a compile database partly usable rather than useless.
    #[test]
    fn an_empty_configuration_still_finds_neighbours() {
        let files = MemoryFiles::new().with_file("src/a.h", "found");
        let config = CompilerConfig::new();
        let resolver = IncludeResolver::new(&files, &config);
        let mut interner = PathInterner::new(false);

        let include = Include {
            form: IncludeForm::Quote,
            target: "a.h".into(),
            is_next: false,
        };

        let resolution = resolver.resolve(&include, Path::new("src"), None, &mut interner);
        assert!(resolution.is_resolved(), "{resolution:?}");
    }

    #[test]
    fn an_include_path_with_a_trailing_separator_still_matches() {
        let files = MemoryFiles::new().with_file("user/a.h", "found");
        let config = CompilerConfig {
            include_paths: vec![IncludePath::user("user/")],
            ..CompilerConfig::default()
        };
        let resolver = IncludeResolver::new(&files, &config);
        let mut interner = PathInterner::new(false);

        let include = Include {
            form: IncludeForm::Angle,
            target: "a.h".into(),
            is_next: false,
        };

        let resolution = resolver.resolve(&include, Path::new("src"), None, &mut interner);
        assert!(resolution.is_resolved(), "{resolution:?}");
    }
}

// Where files come from, and who includes whom: compiler settings (`config`), the resolver (this module), the graph
// those resolutions form (`graph`), and the one place that asks the toolchain where its own headers are
// (`toolchain`) — which is what makes `#include <vector>` resolve at all, and therefore what makes a project's own
// files cacheable. `msvc` and `system_headers` are the two halves of the answer on a machine where no compiler can
// be asked: Microsoft's layout (measured in `docs/msvc-notes.md`), and the conventional directories of an operating
// system.
//
// **`paths` is not here any more**: reading a file is the *file* layer's business (`crate::file::paths`), and this
// module keeps a re-export so that every path written before the move still resolves. The distinction is the one
// that matters for a language server: an include search *asks* for a file, and the file layer is what *holds* one.
pub mod config;
pub mod graph;
pub mod msvc;
pub mod system_headers;
pub mod toolchain;

pub use crate::file::paths;
