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
//!   corpus is expected to be non-zero here until `Definition` answers values (`docs/module-cache.md` §8).
//!
//! # The two runs
//!
//! [`the_fold_and_the_walk_agree_on_a_fixture`] is the repeatable gate: a project small enough to reason about, run on
//! every test invocation, and asserted to have **no** mismatches. [`the_fold_and_the_walk_agree_on_a_real_project`] is
//! the probe: `EmmyLuaCodeStyle`, roots named by `CPPLS_PROBE_DIRS`, and the first `CPPLS_PROBE_ROOTS` `.cpp` files
//! walked.

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
    }

    fn mismatches(&self) -> usize {
        self.only_walked
            + self.only_composed
            + self.body_differs
            + self.kind_differs
            + self.missing_files
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
    seed: &dyn crate::MacroValues,
    known: &[PathBuf],
) -> UnitComposition {
    // **A resolver for the includes the summary could not resolve**, from the paths the walk reached: a spelling
    // with no cached path is matched against the last two segments of a known file. This is a probe's stand-in for
    // the session's resolver, and it exists to test the diagnosis in docs/module-cache.md §6a: the fold reaches
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
        tally.mismatches(),
        0,
        "the fold and the walk disagree: {tally:#?}"
    );
}

/// **The probe.** `EmmyLuaCodeStyle` (or `CPPLS_REAL_PROJECT`), the project's own directories, one unit per root.
#[test]
#[ignore = "a probe: walks a real project and compares instead of asserting"]
fn the_fold_and_the_walk_agree_on_a_real_project() {
    let root = PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT").unwrap_or_else(|_| r"E:\EmmyLuaCodeStyle".to_string()),
    );
    let directories: Vec<String> = std::env::var("CPPLS_PROBE_DIRS")
        .unwrap_or_else(|_| {
            "CodeFormat,CodeFormatCLib,CodeFormatCore,CodeFormatLib,CodeFormatServer,LuaParser,Util"
                .to_string()
        })
        .split(',')
        .map(|directory| directory.trim().to_string())
        .collect();
    let mut sources: Vec<PathBuf> = Vec::new();
    for directory in &directories {
        let mut pending = vec![root.join(directory)];
        while let Some(next) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&next) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "cpp") {
                    sources.push(path);
                }
            }
        }
    }
    sources.sort();
    println!("project {} ({} source(s))", root.display(), sources.len());

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), crate::file::paths::DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(sources.clone());
    session.index_everything();

    let seed_names = seed_names_of(&session);
    let roots: usize = std::env::var("CPPLS_PROBE_ROOTS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);
    for root_source in sources.iter().take(roots) {
        let started = std::time::Instant::now();
        let Some(walked) = session.walked_bindings(root_source) else {
            println!("{}: the walk declined", root_source.display());
            continue;
        };
        let known: Vec<PathBuf> = walked.iter().map(|(path, _)| path.clone()).collect();
        let composition = composition(&session, root_source, session.index().macros(), &known);
        let mut tally = Tally::default();
        let mut per_file: Vec<(PathBuf, Tally)> = Vec::new();
        for (path, bindings) in &walked {
            let Some(file) = composition.file(path) else {
                tally.missing_files += 1;
                continue;
            };
            let one = compare_file(file, bindings, &seed_names);
            if one.only_walked + one.only_composed > 0 {
                per_file.push((path.clone(), one.clone()));
            }
            tally.merge(one);
        }
        per_file.sort_by_key(|(_, one)| std::cmp::Reverse(one.only_walked));
        for (path, one) in per_file.iter().take(8) {
            println!(
                "  worst {}: only walk {}, only fold {}, unknown {}, names {}",
                path.display(),
                one.only_walked,
                one.only_composed,
                composition.file(path).map_or(0, |file| file.unknown_guards),
                one.names
            );
            for example in one.examples.iter().take(3) {
                println!("    {example}");
            }
            if let Some(file) = composition.file(path) {
                let _ = file;
            }
        }
        println!(
            "{}: {} file(s), {} name(s) compared, {} mismatch(es), {} unknown guard(s) | only walk {}, only fold {}, \
             body {}, kind {}, missing {} | skipped own {} seed {} | {:?}",
            root_source
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            tally.files,
            tally.names,
            tally.mismatches(),
            composition.unknown_guards,
            tally.only_walked,
            tally.only_composed,
            tally.body_differs,
            tally.kind_differs,
            tally.missing_files,
            tally.skipped_own,
            tally.skipped_seed,
            started.elapsed(),
        );
        for example in tally.examples.iter().take(10) {
            println!("  {example}");
        }
        let mut undecided: Vec<&ComposedFile> = composition
            .files
            .iter()
            .filter(|file| file.unknown_guards > 0)
            .collect();
        undecided.sort_by_key(|file| std::cmp::Reverse(file.unknown_guards));
        for file in undecided.iter().take(5) {
            println!(
                "  undecided {}: {} directive(s)",
                file.path.display(),
                file.unknown_guards
            );
        }
        // **The environment a shared header is entered with**, for the two names the last diagnosis needed: a header
        // that defines a name and a downstream header that reads it are one question about the fold's propagation.
        for probe_name in ["_GLIBCXX_HOSTED", "__cplusplus"] {
            println!(
                "  probe {probe_name}: seed has {}",
                seed_names.contains(probe_name)
            );
            let shared = composition.files.iter().find(|file| {
                let name = file.path.to_string_lossy().to_lowercase();
                name.ends_with("bits/version.h") || name.ends_with("c++config.h")
            });
            if let Some(file) = shared {
                println!(
                    "    {}: entry {} {:?} / exit {} {:?} / unknown {}",
                    file.path.display(),
                    file.at_entry.get(probe_name).is_some(),
                    file.at_entry.get(probe_name).and_then(|found| found.body()),
                    file.at_exit.get(probe_name).is_some(),
                    file.at_exit.get(probe_name).and_then(|found| found.body()),
                    file.unknown_guards
                );
            }
        }
        println!(
            "  composition: {} file(s), unresolved includes {}",
            composition.files.len(),
            composition.unresolved_includes
        );
        // **The set difference, which is the question left**: the walk reached files the fold never composed, and
        // neither a guard nor a look-up failed on the fold's side — so the edges themselves differ. Printing the
        // first ten names the shape without asking the reader to guess it.
        let composed_paths: std::collections::HashSet<&PathBuf> =
            composition.files.iter().map(|file| &file.path).collect();
        let mut went_elsewhere: Vec<&PathBuf> = walked
            .iter()
            .map(|(path, _)| path)
            .filter(|path| !composed_paths.contains(*path))
            .collect();
        went_elsewhere.sort();
        println!(
            "  walk-only files: {}; e.g. {:?}",
            went_elsewhere.len(),
            went_elsewhere
                .iter()
                .take(10)
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
        );
        for wanted in ["_mingw.h", "wchar.h", "time.h", "bits/chrono.h", "cstdlib"] {
            let mut found = false;
            let mut skeletons = 0;
            for file in &composition.files {
                let name = file.path.to_string_lossy().to_lowercase();
                if name.ends_with(wanted) {
                    found = true;
                    skeletons += 1;
                }
            }
            println!("  contains {wanted}: {found} ({skeletons})");
            let _ = skeletons;
        }
        for (path, offset, what, visibility, condition) in
            composition.skipped_includes.iter().take(12)
        {
            println!(
                "  skipped include [{visibility}] {}:{offset} {what} under {condition}",
                path.display()
            );
        }
        for (path, offset, what) in composition.unknown.iter().take(20) {
            println!("  undecided {}:{offset}: {what}", path.display());
        }
    }
}
