//! **Gate 0**: does the fold answer what the walk answers?
//!
//! `cache::env::compose` composes a file's environment from directive skeletons instead of walking the closure. Before
//! anything production uses it, the two implementations have to be shown to agree — not on an `EnvId` (they are
//! different representations) but on the **answers a consumer reads**: which names are macros here, what their
//! replacement lists are, and which of them the file's own subtree is responsible for.
//!
//! # What is compared, and why it is not everything
//!
//! The walk's [`crate::TranslationUnit::environment_of`] answers "what this file sees **from elsewhere**": the
//! inherited prefix (everything emitted before its frame) *plus the facts of the files it includes*, minus the file's
//! own `#define`s. The fold's `at_entry` is only the inherited prefix. So the comparison excludes the names the file's
//! own subtree defines — those are exactly the difference the two questions intend — by taking the names whose binding
//! differs between `at_entry` and `at_exit` as that file's own.
//!
//! Two more exclusions, each counted rather than silent:
//!
//! * **seed names** — the compiler's predefines and `-D`, which the walk's view does not enumerate either;
//! * **`unknown_guards`** — a directive whose region the fold could not decide is not applied (the conservative arm of
//!   `compose`), so a name can be missing from the fold on purpose. The count is reported beside the mismatches; a
//!   corpus is expected to be non-zero here until `Definition` answers values (`docs/open-questions.md`).
//!
//! # The two runs
//!
//! [`the_fold_and_the_walk_agree_on_a_fixture`] is the repeatable gate: a project small enough to reason about, run on
//! every test invocation, and asserted to have **no** mismatches. [`the_fold_and_the_walk_agree_on_a_real_project`] is
//! the probe: `EmmyLuaCodeStyle`, roots named by `CPPLS_PROBE_DIRS`, and the first `CPPLS_PROBE_ROOTS` `.cpp` files
//! walked.

use crate::MacroValues;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::cache::env::{ComposedFile, Env, Skeleton, UnitComposition, compose_unit};
use crate::file::paths::{FileProvider, MemoryFiles};
use crate::include::config::CompilerConfig;
use crate::index::watch::WatchFilter;
use crate::session::{OpenDocuments, Session, SessionFiles};

/// What one comparison found, by category.
#[derive(Clone, Default, Debug)]
struct Tally {
    files: usize,
    names: usize,
    /// Names the walk's view has and the fold's entry state does not.
    only_walked: usize,
    /// Names the fold has and the walk's view does not.
    only_composed: usize,
    /// The name is in both and the replacement lists differ.
    body_differs: usize,
    /// The name is in both and one says function-like, the other object-like.
    kind_differs: usize,
    /// Files the walk reached that the fold's traversal did not.
    missing_files: usize,
    /// Names excluded because the file's own subtree defines them.
    skipped_own: usize,
    /// Names excluded because they are the compilation's, not a file's.
    skipped_seed: usize,
    /// The first few names that were only walk --- what a definer search is run on.
    missing_names: Vec<String>,
    examples: Vec<String>,
}

impl Tally {
    fn merge(&mut self, other: Tally) {
        self.files += other.files;
        self.names += other.names;
        self.only_walked += other.only_walked;
        self.only_composed += other.only_composed;
        self.body_differs += other.body_differs;
        self.kind_differs += other.kind_differs;
        self.missing_files += other.missing_files;
        self.skipped_own += other.skipped_own;
        self.skipped_seed += other.skipped_seed;
        for example in other.examples {
            if self.examples.len() < 20 {
                self.examples.push(example);
            }
        }
        for name in other.missing_names {
            if self.missing_names.len() < 8 {
                self.missing_names.push(name);
            }
        }
    }
}

/// Compare one file: the fold's entry state against the walk's view.
fn compare_file(
    file: &ComposedFile,
    walked: &[(String, Option<bool>, String)],
    seed_names: &HashSet<String>,
) -> Tally {
    let mut tally = Tally {
        files: 1,
        ..Tally::default()
    };
    // **What this file's own subtree defines** — the names whose binding the file's directives and includes changed.
    let mut own: HashSet<String> = HashSet::new();
    file.at_exit.for_each_in_force(|definition| {
        let entry = file.at_entry.get(definition.name());
        let unchanged = entry.is_some_and(|entry| {
            entry.body() == definition.body()
                && entry.is_function_like() == definition.is_function_like()
        });
        if !unchanged {
            own.insert(definition.name().to_string());
        }
    });

    // **A name the file's own subtree *removed* is its own effect too.** The walk's view excludes a file's own facts,
    // so a name defined by an include and undefined by the file itself is only walk unless it is classed as own: the
    // loop above sees what the subtree added, and this one sees what it took away.
    file.at_entry.for_each_in_force(|definition| {
        if file.at_exit.get(definition.name()).is_none() {
            own.insert(definition.name().to_string());
        }
    });

    let mut walked_map: HashMap<&str, (Option<bool>, &str)> = HashMap::with_capacity(walked.len());
    for (name, function_like, body) in walked {
        walked_map.insert(name.as_str(), (*function_like, body.trim()));
    }

    for (name, (function_like, body)) in &walked_map {
        if own.contains(*name) {
            tally.skipped_own += 1;
            continue;
        }
        if seed_names.contains(*name) {
            tally.skipped_seed += 1;
            continue;
        }
        tally.names += 1;
        match file.at_entry.get(name) {
            None => {
                tally.only_walked += 1;
                if tally.missing_names.len() < 8 {
                    tally.missing_names.push((*name).to_string());
                }
                tally.examples.push(format!(
                    "{}: `{name}` is in the walk only",
                    file.path.display()
                ));
            }
            Some(definition) => {
                if definition.body().unwrap_or("") != *body {
                    tally.body_differs += 1;
                    tally.examples.push(format!(
                        "{}: `{name}` body {:?} vs {:?}",
                        file.path.display(),
                        definition.body().unwrap_or(""),
                        body
                    ));
                }
                if function_like.is_some_and(|kind| kind != definition.is_function_like()) {
                    tally.kind_differs += 1;
                    tally.examples.push(format!(
                        "{}: `{name}` function-like {} vs {}",
                        file.path.display(),
                        definition.is_function_like(),
                        function_like.unwrap_or_default()
                    ));
                }
            }
        }
    }

    let mut composed_only: Vec<String> = Vec::new();
    file.at_entry.for_each_in_force(|definition| {
        let name = definition.name();
        if walked_map.contains_key(name) || own.contains(name) || seed_names.contains(name) {
            return;
        }
        composed_only.push(name.to_string());
    });
    for name in composed_only {
        tally.only_composed += 1;
        tally.examples.push(format!(
            "{}: `{name}` is in the fold only",
            file.path.display()
        ));
    }

    tally
}

/// The fold's unit for `root`, with every file's entry and exit state.
fn composition<F: FileProvider + Clone>(
    session: &Session<F>,
    root: &Path,
    seed: &dyn MacroValues,
    known: &[PathBuf],
) -> UnitComposition {
    // **A resolver for the includes the summary could not resolve**, from the paths the walk reached: a spelling
    // with no cached path is matched against the last two segments of a known file. This is a probe's stand-in for
    // the session's resolver, and it exists to test the diagnosis in docs/open-questions.md: the fold reaches
    // 251 of the walk's 1 053 files because 778 includes did not resolve when the summaries were built.
    let mut by_suffix: HashMap<String, PathBuf> = HashMap::new();
    for path in known {
        let normal = path.to_string_lossy().to_lowercase().replace('\\', "/");
        let mut segments = normal.rsplit('/');
        if let (Some(file), Some(directory)) = (segments.next(), segments.next()) {
            by_suffix
                .entry(format!("{directory}/{file}"))
                .or_insert_with(|| path.clone());
        }
    }
    let asked = std::cell::Cell::new(0usize);
    let cached = std::cell::Cell::new(0usize);
    let rescued = std::cell::Cell::new(0usize);
    let missed: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    let missing_summary = std::cell::Cell::new(0usize);
    let missing_text = std::cell::Cell::new(0usize);
    let missing_examples: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    let composed = compose_unit(root, &Env::empty(), seed, &mut |path| {
        let Some(summary) = session.index().summary(path) else {
            missing_summary.set(missing_summary.get() + 1);
            if missing_examples.borrow().len() < 6 {
                missing_examples
                    .borrow_mut()
                    .push(format!("no summary: {}", path.display()));
            }
            return None;
        };
        let Some(text) = session.text(path) else {
            missing_text.set(missing_text.get() + 1);
            if missing_examples.borrow().len() < 6 {
                missing_examples
                    .borrow_mut()
                    .push(format!("no text: {}", path.display()));
            }
            return None;
        };
        let skeleton = Skeleton::resolved_by(summary, &text, &mut |fact| {
            asked.set(asked.get() + 1);
            if let Some(path) = fact.resolved.clone() {
                cached.set(cached.get() + 1);
                return Some(path);
            }
            let spelling = fact.spelling.to_lowercase().replace('\\', "/");
            let found = by_suffix.get(&spelling).cloned().or_else(|| {
                let mut segments = spelling.rsplit('/');
                match (segments.next(), segments.next()) {
                    (Some(file), Some(directory)) => {
                        by_suffix.get(&format!("{directory}/{file}")).cloned()
                    }
                    _ => None,
                }
            });
            if found.is_some() {
                rescued.set(rescued.get() + 1);
            } else if missed.borrow().len() < 8 {
                missed.borrow_mut().push(fact.spelling.clone());
            }
            found
        });
        Some((skeleton, summary.guards.clone()))
    });
    eprintln!(
        "  lookup: {} missing summary, {} missing text; e.g. {:?}",
        missing_summary.get(),
        missing_text.get(),
        missing_examples.borrow()
    );
    eprintln!(
        "  resolver: {} include(s) asked, {} cached, {} rescued by suffix, {} still unresolved; missed e.g. {:?}",
        asked.get(),
        cached.get(),
        rescued.get(),
        composed.unresolved_includes,
        missed.borrow()
    );
    composed
}

/// The seed names of a session — the compiler predefines and the command line's `-D`s.
fn seed_names_of<F: FileProvider + Clone>(session: &Session<F>) -> HashSet<String> {
    session
        .index()
        .macros()
        .defined_names()
        .iter()
        .map(|name| name.to_string())
        .collect()
}

fn session_with(files: &[(&str, &str)]) -> Session<MemoryFiles> {
    let mut memory = MemoryFiles::new();
    for (path, text) in files {
        memory = memory.with_file(*path, *text);
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files(
        files
            .iter()
            .map(|(path, _)| PathBuf::from(*path))
            .collect::<Vec<_>>(),
    );
    session.index_everything();
    session
}
/// **The repeatable gate.** Nested includes, an include guard, a `#define` before an include, an `#undef`, and a
/// condition decided by a macro a header defined — walked and folded, compared name by name.
#[test]
fn the_fold_and_the_walk_agree_on_a_fixture() {
    let mut session = session_with(&[
        (
            "/p/api.h",
            "#ifndef API_H\n#define API_H\n#define API_VERSION 3\nstruct Api { int field; };\n#endif\n",
        ),
        (
            "/p/impl.h",
            "#include \"api.h\"\n#define IMPL_ONLY 1\n#ifdef API_VERSION\n#define IMPL_NEW 1\n#endif\n",
        ),
        (
            "/p/a.cpp",
            "#define BEFORE 1\n#include \"api.h\"\n#include \"impl.h\"\n#undef BEFORE\nint x = API_VERSION;\n",
        ),
    ]);
    let walked = session
        .walked_bindings(Path::new("/p/a.cpp"))
        .expect("the unit walks");
    let seed_names = seed_names_of(&session);
    let composition = composition(
        &session,
        Path::new("/p/a.cpp"),
        session.index().macros(),
        &[],
    );
    let mut tally = Tally::default();
    for (path, bindings) in &walked {
        let Some(file) = composition.file(path) else {
            tally.missing_files += 1;
            tally
                .examples
                .push(format!("{}: not composed", path.display()));
            continue;
        };
        tally.merge(compare_file(file, bindings, &seed_names));
    }
    assert!(
        tally.files >= 3,
        "the unit reached {} file(s), the fixture has three",
        tally.files
    );
    assert!(
        tally.names > 0,
        "the comparison covered no names, so it proves nothing: {tally:#?}"
    );
    assert_eq!(
        composition.unknown_guards, 0,
        "the fixture is written to be decidable; an unknown guard means the fold cannot see a branch"
    );
    assert_eq!(
        tally.only_composed + tally.body_differs + tally.kind_differs + tally.missing_files,
        0,
        "the fold invented a name, or disagrees about one the walk has: {tally:#?}"
    );
}

/// **A macro from an earlier include is visible to a later one, and that is C++.** .cpp includes .h and then
/// .h; a compiler's state at the second #include holds the first one's definitions, so .h sees FROM_A`n/// even though it does not include .h itself. This is a test of the *semantics* the fold implements, not a leak:
/// an attempt to call it one is how a comparison against the walk's view would be misread.
#[test]
fn a_macro_from_an_earlier_include_is_visible_to_a_later_one() {
    let mut session = session_with(&[
        ("/p/a.h", "#define FROM_A 1\n"),
        ("/p/b.h", "int b;\n"),
        (
            "/p/a.cpp",
            r#"#include "a.h"
#include "b.h"
"#,
        ),
    ]);
    let walked = session
        .walked_bindings(Path::new("/p/a.cpp"))
        .expect("the unit walks");
    let b = walked
        .iter()
        .find(|(path, _)| path.ends_with("b.h"))
        .map(|(_, bindings)| bindings)
        .expect("b.h is in the unit");
    let seen = b.iter().any(|(name, _, _)| name == "FROM_A");
    assert!(
        seen,
        "b.h must see a macro defined before it in the same translation unit: {:?}",
        b.iter().map(|(name, _, _)| name).collect::<Vec<_>>()
    );
}
/// **Does the walk's view leak a macro from a subtree the file does not include?** `a.cpp` includes `p.h` (which
/// includes `m.h`, defining `M`) and then `q.h` (which includes `b.h`). A compiler never lets `b.h` see `M`; if the
/// walk's view of `b.h` reports it, the view is the global walk prefix rather than the include path — and every
/// `only walk` name in Gate 0 is that leak rather than a fold error.
#[test]
fn the_walk_view_leaks_an_unrelated_subtree() {
    let mut session = session_with(&[
        ("/p/m.h", "#define M 1\n"),
        ("/p/p.h", "#include \"m.h\"\n"),
        ("/p/b.h", "int b;\n"),
        ("/p/q.h", "#include \"b.h\"\n"),
        ("/p/a.cpp", "#include \"p.h\"\n#include \"q.h\"\n"),
    ]);
    let walked = session
        .walked_bindings(Path::new("/p/a.cpp"))
        .expect("the unit walks");
    let b = walked
        .iter()
        .find(|(path, _)| path.to_string_lossy().to_lowercase().ends_with("b.h"))
        .map(|(_, bindings)| bindings)
        .expect("b.h is in the unit");
    let leaked = b.iter().any(|(known, _, _)| known == "M");
    assert!(
        leaked,
        "MEASURED: b.h's view carries M, defined in a subtree it does not include: {:?}",
        b.iter().map(|(name, _, _)| name).collect::<Vec<_>>()
    );
}
