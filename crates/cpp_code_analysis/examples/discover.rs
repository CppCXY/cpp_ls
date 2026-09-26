//! What this server thinks a project is, and where each part of that came from.
//!
//! The first question anybody asks about a wrong answer is "what did it think this project was", and until this
//! example existed the answer was spread across a log line ("toolchain: none found"), a `Debug` dump of a
//! configuration, and a directory listing. This prints the whole of it in one place, in the order the decisions
//! depend on each other:
//!
//! ```text
//! the project's own file   .cppls.toml — what a person said, and everything wrong with it
//! the build description    compile_commands.json — where it was found and what it holds
//! the toolchain            which compiler answered, and what it said about its own headers
//! the assembled answer     include paths, defines, standard, dialect — the configuration queries are keyed on
//! the project's files      what the scan (or the database) says the project is made of
//! ```
//!
//! ```bash
//! cargo run -q -p cpp_code_analysis --example discover -- <project root> [--config <file>]
//! ```
//!
//! It is also the instrument the toolchain work is measured with: the numbers it prints — how many include paths
//! came from where, whether a compiler answered at all — are the ones `docs/ls-architecture.md` §5 records.

use std::path::{Path, PathBuf};

use cpp_code_analysis::{
    DiskFiles, Environment, OpenDocuments, SessionFiles, WatchFilter, load_config,
};

fn main() {
    let mut arguments = std::env::args().skip(1);
    let mut root = None;
    let mut config_file = None;

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--config" => config_file = arguments.next().map(PathBuf::from),
            _ => root = Some(PathBuf::from(argument)),
        }
    }

    let root = root.unwrap_or_else(|| {
        eprintln!("usage: discover <project root> [--config <file>]");
        std::process::exit(2);
    });

    if !root.is_dir() {
        eprintln!("{} is not a directory", root.display());
        std::process::exit(2);
    }

    println!("project: {}", root.display());

    // --- the project's own file ------------------------------------------------------------------
    let documents = OpenDocuments::new();
    let files = SessionFiles::new(documents.clone(), DiskFiles);
    let report = load_config(&files, &root, config_file.as_deref());

    println!("\n--- .cppls.toml ---");
    match &report.path {
        Some(path) => println!("read from      {}", path.display()),
        None => println!("read from      nowhere (the project has no configuration file)"),
    }
    if !report.problems.is_empty() {
        println!("problems:");
        for problem in &report.problems {
            println!("  [{:?}] {}", problem.severity, problem.message);
        }
    }

    let config = &report.config;
    println!("exclude        {:?}", config.workspace.exclude);
    println!("extensions     {:?}", config.workspace.source_extensions);
    println!("database       {:?}", config.compile.database);
    println!("compiler       {:?}", config.compile.named_compiler());
    println!("args           {:?}", config.compile.args);
    println!("extra_args     {:?}", config.compile.extra_args);
    println!("remove_args    {:?}", config.compile.remove_args);
    println!("include rules  {:?}", config.compile.include);
    println!("cache_dir      {:?}", config.index.cache_dir);
    println!("max_files      {:?}", config.index.max_files);
    println!("diagnostics    {:?}", config.diagnostics);
    println!("hover          {:?}", config.hover);

    // --- the toolchain the machine offers ---------------------------------------------------------
    println!("\n--- the toolchain ---");
    let environment = Environment::current();
    println!("CXX            {:?}", environment.cxx);
    println!("CC             {:?}", environment.cc);
    println!("PATH entries   {}", environment.path.len());

    // --- the session, which is what assembles all of it ------------------------------------------
    let session = cpp_code_analysis::Session::open_with_config_file(
        root.clone(),
        SessionFiles::new(documents.clone(), DiskFiles),
        WatchFilter::new(&root),
        config_file.as_deref(),
    );

    println!("\n--- the compile database ---");
    match session.discovery().database.as_ref() {
        Some(database) => {
            println!("path           {}", database.path.display());
            println!("found by       {}", database.origin.words());
            println!("entries        {}", database.commands.len());
            println!("malformed      {}", database.commands.malformed);
        }
        None => println!("entries        none (no database was read)"),
    }

    println!("\n--- CMake ---");
    match session.discovery().cmake.as_ref() {
        Some(cmake) => {
            println!("cache          {}", cmake.path.display());
            for (key, value) in cmake.keys() {
                println!("  {key:26} {value}");
            }
        }
        None => println!("cache          none (no CMakeCache.txt for this project was found)"),
    }

    println!("\n--- what could not be worked out ---");
    let problems = &session.discovery().problems;
    if problems.is_empty() && session.project_config().problems.is_empty() {
        println!("nothing");
    }
    for problem in session
        .project_config()
        .problems
        .iter()
        .map(|problem| (problem.severity, problem.message.as_str()))
        .chain(
            problems
                .iter()
                .map(|problem| (problem.severity, problem.message.as_str())),
        )
    {
        println!("[{:?}] {}", problem.0, problem.1);
    }

    println!("\n--- the compiler that answered ---");
    match session.toolchain() {
        Some(toolchain) => {
            println!("compiler       {}", toolchain.compiler_name());
            println!("found by       {}", toolchain.source.words());
            if let Some(note) = &toolchain.note {
                println!("note           {note}");
            }
            println!(
                "version        {}",
                toolchain.version.as_deref().unwrap_or("(not reported)")
            );
            println!("include paths  {}", toolchain.system_include_paths.len());
            println!("builtin macros {}", toolchain.builtin_macros.len());
            for directory in toolchain.system_include_paths.iter().take(5) {
                println!("               {}", directory.display());
            }
            if toolchain.system_include_paths.len() > 5 {
                println!(
                    "               … {} more",
                    toolchain.system_include_paths.len() - 5
                );
            }
        }
        None => println!("compiler       none — nothing could be asked, so no system headers are known"),
    }

    println!("\n--- the configuration queries are keyed on ---");
    let config = session.config();
    println!("standard       {:?}", config.standard);
    println!("target         {:?}", config.target);
    println!("dialect        {:?}", config.dialect());
    println!("working dir    {:?}", config.working_directory);
    println!("defines        {}", config.defines.len());
    for define in config.defines.iter().take(10) {
        println!(
            "               {}{}",
            define.name,
            define
                .value
                .as_deref()
                .map(|value| format!("={value}"))
                .unwrap_or_default()
        );
    }
    println!("undefines      {}", config.undefines.len());
    println!("include paths  {}", config.include_paths.len());
    for path in config.include_paths.iter().take(10) {
        println!(
            "               {}{}",
            path.directory.display(),
            if path.is_system { "  (system)" } else { "" }
        );
    }
    if config.include_paths.len() > 10 {
        println!("               … {} more", config.include_paths.len() - 10);
    }

    println!("\n--- the project's files ---");
    let files = session.project_files();
    println!("files          {}", files.len());
    for path in files.iter().take(10) {
        println!("               {}", short(path, &root));
    }
    if files.len() > 10 {
        println!("               … {} more", files.len() - 10);
    }

    println!("\n--- what the session would say about itself ---");
    println!("indexed        {}", session.index().len());
    println!("pending        {}", session.pending());
}

/// A path relative to the project, which is what makes a report readable.
fn short(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.display().to_string())
}

