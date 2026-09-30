//! **Put our reading of a program beside a real preprocessor's, and report where they differ.**
//!
//! ```text
//! cargo run --release --example align_preprocessor -- <file.cpp> [--project <dir>] [--ours-only] [--max N]
//! ```
//!
//! This is the tool the plan's §5.0 asks for and the reason it asks for it: *"它把'我们 parse 得对不对'从争论变成数字,
//! 而且数字的来源不是我们自己"* — it turns "did we read this right" from an argument into a number, and the number
//! comes from a compiler rather than from this crate.
//!
//! # What it does
//!
//! ```text
//! the session's configuration   ← compile_commands.json, .cppls.toml, the discovered toolchain
//!         ↓
//! Session::render_the_unit      our reading: every file, stitched in include order, branches taken
//!         ↓  tokens with (file, line)                       ↓  the same file, preprocessed by the compiler
//! cpp_code_analysis::align  ←──────────────────────────────┘
//!         ↓
//! a difference count, by mechanism
//! ```
//!
//! # The two sides are made to agree about everything but the reading
//!
//! A comparison is only worth its number if the two things compared were asked the same question. So the compiler is
//! run with **the configuration this session is using** — the same standard, the same `-D`s, the same include paths,
//! in the same order (`Session::config`, which is the toolchain's directories applied to the project's settings) —
//! and it is run on the same file. What is left to differ is the reading, which is the subject.
//!
//! # The reading of a number
//!
//! ```text
//! matched      tokens both streams spell the same, in order      — the only number that should go up
//! differences  edits the shortest script needs to explain the rest
//! truncated    the comparison gave up (see MAX_EDIT_DISTANCE)    — read this before believing the count
//! ```
//!
//! And then the differences **by mechanism**, which is what makes this a work list rather than a wall of text:
//! `UnexpandedMacro` is the expander, `ConditionDisagreement` is the condition evaluator, `MissingHeader` is the
//! include resolver. Those are milestones; "17 differences" is not.
//!
//! # `--ours-only`
//!
//! Prints our stream and stops: token count, file count, and one line per file with how many tokens it contributed.
//! That is the half of the comparison that needs no compiler, and it is what a machine with no toolchain can still
//! record — so a regression in the reading is visible even where the oracle is not installed.
//!
//! # What it is not
//!
//! **Not a pass/fail gate, yet.** The plan's §5.2 requires the difference count to be monotone, and monotone needs a
//! number that is comparable between runs; the classification is the first step, and a CI check is the next one (a
//! recorded count per file, compared). Until then this prints, and a person reads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cpp_code_analysis::{
    CommandRunner, DiskCommands, DiskFiles, OpenDocuments, Report, SessionFiles, StreamToken, WatchFilter,
};

fn main() {
    let mut arguments = std::env::args().skip(1);
    let mut file: Option<PathBuf> = None;
    let mut root: Option<PathBuf> = None;
    let mut ours_only = false;
    let mut record = false;

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--project" => root = arguments.next().map(PathBuf::from),
            "--ours-only" => ours_only = true,
            "--record" => record = true,
            other => file = Some(PathBuf::from(other)),
        }
    }

    let Some(file) = file else {
        eprintln!(
            "usage: align_preprocessor <file.cpp> [--project <dir>] [--ours-only] [--record]\n\
             \n\
             Compares this analysis's reading of <file.cpp> against a real preprocessor's, and reports the\n\
             differences by mechanism. The compiler comes from the project's compile_commands.json or from\n\
             the toolchain this session discovered; --project defaults to the file's own directory.\n\
             \n\
             --ours-only  print our half and stop: needs no compiler, so a machine without one can still\n\
             \x20            record what the reading produced\n\
             --record     instead of the full report, print the two numbers a regression check compares:\n\
             \x20            one `summary` line and one line per file, in a fixed order"
        );
        std::process::exit(2);
    };

    // **Canonicalized, then un-verbatim'd.** `canonicalize` is what makes sure the file exists and gives an absolute
    // path, and on Windows it answers with a *verbatim* path — `\\?\C:\…` — which is a spelling the index has never
    // seen: the session's summaries are keyed by the path the scan produced, and `\\?\C:\x` and `C:\x` are two
    // different strings. So the prefix comes off, which is safe here because `canonicalize` has already removed every
    // `.` and `..` from the path (the one thing the prefix exists to allow).
    let file = file.canonicalize().unwrap_or_else(|_| file.clone());
    let file = strip_verbatim(&file);
    let root = root.unwrap_or_else(|| {
        file.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    });

    let documents = OpenDocuments::new();
    let mut session = cpp_code_analysis::Session::open(
        root.clone(),
        SessionFiles::new(documents, DiskFiles),
        WatchFilter::new(&root),
    );
    session.index_everything();

    println!("file     {}", file.display());
    println!("project  {}", root.display());

    // **Everything taken from the session before the first `&mut` call**, as owned data. `render_the_unit` takes the
    // session mutably (it may have to walk the closure and put the unit in its cache), so a `&CompilerConfig` held
    // across it would not compile — and holding one is exactly what a report wants.
    let (config, toolchain): (cpp_code_analysis::CompilerConfig, Option<String>) = {
        let mut describes = Vec::new();
        if let Some(toolchain) = session.toolchain() {
            describes.push(format!(
                "{} ({}, {})",
                toolchain.compiler_name(),
                toolchain.source.words(),
                toolchain.version.as_deref().unwrap_or("version not reported")
            ));
        }
        (session.config().clone(), describes.pop())
    };

    println!(
        "compiler {}",
        toolchain.as_deref().unwrap_or("none could be asked")
    );
    println!(
        "config   standard {:?} | target {:?} | dialect {:?} | {} defines | {} include paths",
        config.standard,
        config.target,
        config.dialect(),
        config.defines.len(),
        config.include_paths.len(),
    );

    // --- our side ---------------------------------------------------------------------------------
    let Some(stream) = session.render_the_unit(&file) else {
        eprintln!(
            "\nnothing to read: the index has no summary for {}. Has the file been indexed?",
            file.display()
        );
        std::process::exit(1);
    };

    let ours = our_tokens(&stream);
    println!(
        "\n--- our reading ---\n{} tokens | {} files entered | {} files with tokens | {} missing | {} unbalanced | braces {}",
        stream.len(),
        stream.files.len(),
        stream.files_with_tokens(),
        stream.missing,
        stream.unbalanced.len(),
        stream.braces,
    );

    if ours_only {
        if record {
            record_ours(&ours, &stream);
        } else {
            print_per_file(&ours, &stream.files);
        }
        return;
    }

    // --- the compiler's side ----------------------------------------------------------------------
    let Some(program) = session
        .toolchain()
        .and_then(|toolchain| toolchain.compiler.clone())
    else {
        eprintln!(
            "\nno compiler could be asked, so there is nothing to compare against. \
             `--ours-only` prints the half that needs no oracle."
        );
        std::process::exit(1);
    };

    let arguments = preprocessor_arguments(&file, &config);
    println!("\n--- the compiler's side ---\n{} {}", program.display(), arguments.join(" "));

    // **A warning when the two sides are not the same compiler.** The analysis modelled *this* project's toolchain —
    // that is where its builtin macro table, its search paths and its dialect came from — and the comparison is only
    // about the *reading* if the thing on the other side is that same toolchain. Pointing `g++` at a project whose
    // analysis modelled MSVC produces a difference for every `#if _MSC_VER` in every header, and a report full of
    // those is not a reading of anything. The two can differ legitimately — the analysis may have fallen back to a
    // discovered compiler when the database named none — so this warns rather than refuses.
    if let Some(toolchain) = session.toolchain()
        && let Some(modelled) = toolchain.compiler.as_deref()
        && compiler_family(modelled) != compiler_family(&program)
    {
        eprintln!(
            "warning: the analysis modelled `{}` but this runs `{}`. Differences below may describe the two \
             compilers rather than the two readings.",
            modelled.display(),
            program.display()
        );
    }

    // `CommandRunner::run` takes `&[&str]`: the arguments are owned `String`s here because two of them are built
    // (`-std=…`, `-I…`), and borrowing them for the call is cheaper than building them borrowed.
    let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let environment = compiler_environment(&config);
    let Some(output) = DiskCommands.run(&program, &borrowed, &environment) else {
        eprintln!("\nthe compiler could not be run at all.");
        std::process::exit(1);
    };
    if !output.succeeded {
        // **Not a failure of the comparison but of the question**: a compiler that refused the command line said
        // nothing about the reading, and reporting differences from its diagnostics would report the wrong thing.
        eprintln!(
            "\nthe compiler exited with {:?} and produced {} bytes of output.\n\
             stderr:\n{}",
            output.status,
            output.stdout.len(),
            first_lines(&output.stderr, 20)
        );
        std::process::exit(1);
    }

    let theirs = cpp_code_analysis::tokenize_preprocessed(&output.stdout);
    println!("{} tokens from the preprocessor", theirs.len());
    if theirs.is_empty() && !ours.is_empty() {
        eprintln!(
            "the compiler produced no tokens, which means the invocation was not a preprocess-to-stdout \
             one. Nothing to compare."
        );
        std::process::exit(1);
    }

    // --- the comparison ----------------------------------------------------------------------------
    let report = Report::of(&ours, &theirs);

    // **What a CI check records**: the counts, in a fixed order, or the report a person reads. One or the other,
    // because the two answer different questions and printing both makes the record un-diffable.
    if record {
        record_both(&report, &ours, &stream);
        return;
    }

    println!("\n--- alignment ---\n{}", report.render());

    // Per-file token counts, which is where a difference usually localises: a header that contributed 0 tokens on
    // our side and 4 000 on the compiler's is an include that did not resolve.
    print_per_file(&ours, &stream.files);

    let summary: Vec<String> = report
        .by_reason
        .iter()
        .map(|(reason, count)| format!("{reason:?}={count}"))
        .collect();
    println!(
        "\nsummary  ours={} theirs={} matched={} differences={}{}",
        report.ours,
        report.theirs,
        report.matched,
        report
            .by_reason
            .iter()
            .map(|(_, count)| *count)
            .sum::<usize>(),
        if report.truncated { " TRUNCATED" } else { "" }
    );
    if !summary.is_empty() {
        println!("         {}", summary.join(" "));
    }
}

/// **The numbers a regression check compares**, one per line, in an order that does not depend on the reading.
///
/// The plan's §5.2 requires the difference count to be *monotone* between runs, and a count printed to a terminal
/// cannot be compared with anything. This is that count made comparable: a fixed shape, one fact per line, and
/// **sorted by path rather than by count** — so a run whose numbers are identical produces byte-identical output,
/// and a run that differs produces a readable diff instead of two shuffled lists.
///
/// ```text
/// reading  ours=… files=… missing=… unbalanced=… braces=…      ← what the reading itself was
/// summary  ours=… theirs=… matched=… differences=…            ← what the comparison found
/// reason   <count>  <Reason>
/// file     <tokens>  <path>                                    ← sorted by path, zero lines included
/// ```
///
/// The `reading` line comes first because it is the one that needs no compiler: on a machine with none,
/// `--ours-only --record` records exactly that line plus the `file` lines, and a regression in the reading is
/// visible there. It carries the two M0 counters (`unbalanced`, `repaired`) — the numbers the plan's §5.2 calls the
/// ones that may only go down.
///
/// Deliberately **not** recorded: the differences themselves. A report of forty differences is for a person, and a
/// baseline that changed because a *different* forty were shown is a baseline that fails for no reason.
fn record_both(report: &Report, ours: &[StreamToken], stream: &cpp_code_analysis::RenderedUnit) {
    println!(
        "reading  ours={} files={} missing={} unbalanced={} braces={}",
        stream.len(),
        stream.files.len(),
        stream.missing,
        stream.unbalanced.len(),
        stream.braces,
    );
    println!(
        "summary  ours={} theirs={} matched={} differences={}{}",
        report.ours,
        report.theirs,
        report.matched,
        report
            .by_reason
            .iter()
            .map(|(_, count)| *count)
            .sum::<usize>(),
        if report.truncated { " TRUNCATED" } else { "" }
    );
    let mut by_reason = report.by_reason.clone();
    by_reason.sort_by_key(|(reason, _)| format!("{reason:?}"));
    for (reason, count) in by_reason {
        println!("reason   {count:>7}  {reason:?}");
    }
    record_files(ours, &stream.files);
}

/// [`record_both`] for the half that needs no compiler — `--ours-only --record`.
fn record_ours(ours: &[StreamToken], stream: &cpp_code_analysis::RenderedUnit) {
    println!(
        "reading  ours={} files={} missing={} unbalanced={} braces={}",
        stream.len(),
        stream.files.len(),
        stream.missing,
        stream.unbalanced.len(),
        stream.braces,
    );
    println!("summary  ours={} theirs=- matched=- differences=-", ours.len());
    record_files(ours, &stream.files);
}

/// One `file` line per file, **sorted by path**, including the files that contributed nothing.
///
/// The zero lines are the point: a header that stops contributing tokens is the most common way a reading regresses,
/// and a list that only named the non-empty files would show that as a *missing line* rather than as a changed
/// number.
///
/// **Both sides of the map are normalized**, which is not cosmetic: the seeds come from `RenderedUnit::files` — the
/// session's own spelling of a path — while the keys come from the tokens' `file`, already normalized by `shared`.
/// Two spellings of one path are two entries, so the file would appear twice: once with its real token count and once
/// with zero, which is exactly the regression this list exists to show.
fn record_files(ours: &[StreamToken], files: &[PathBuf]) {
    let mut counts: HashMap<PathBuf, usize> = files
        .iter()
        .map(|path| (cpp_code_analysis::align::normalized(path), 0))
        .collect();
    for token in ours {
        if let Some(file) = token.file.as_deref() {
            *counts.entry(cpp_code_analysis::align::normalized(file)).or_default() += 1;
        }
    }

    let mut listed: Vec<(PathBuf, usize)> = counts.into_iter().collect();
    listed.sort_by(|left, right| left.0.cmp(&right.0));
    for (path, count) in listed {
        println!("file     {count:>7}  {}", path.display());
    }
}

/// **Our stream as comparable tokens**: [`cpp_code_analysis::unit_tokens`] over the filesystem.
///
/// One line of body, because the conversion itself is in the library where it can be tested — see
/// `our_tokens_take_their_line_from_the_file_not_from_the_rendering`. What is left here is which
/// [`cpp_code_analysis::UnitTexts`] to hand it, and that is the caller's decision by design.
fn our_tokens(stream: &cpp_code_analysis::RenderedUnit) -> Vec<StreamToken> {
    cpp_code_analysis::unit_tokens(stream, &cpp_code_analysis::DiskTexts)
}

/// **The command line that asks the compiler the same question this session asked.**
///
/// The standard, the `-D`s, the `-U`s and the include paths come from the session's own configuration, in the
/// configuration's own order — because include order is load-bearing (the first directory that holds a header wins)
/// and a comparison that reordered them would resolve a `#include` to a different file and call the result a
/// difference in the *reading*.
///
/// `-E` (or MSVC's `/E`) is the whole request: preprocess, and write the result to standard output. Line markers are
/// kept — they are on by default in all three compilers and they are what the comparison reads file boundaries out
/// of, so a flag that suppressed them would silently reduce the comparison to a token count.
fn preprocessor_arguments(
    file: &Path,
    config: &cpp_code_analysis::CompilerConfig,
) -> Vec<String> {
    let msvc = config.dialect() == cpp_parser::Dialect::Msvc;
    let mut arguments: Vec<String> = Vec::new();

    if msvc {
        arguments.push("/nologo".into());
        arguments.push("/E".into());
        // `/Zc:preprocessor` is the conforming preprocessor, which is what the header machinery in this crate models;
        // the legacy one differs in ways that would show up as differences in the *reading* rather than in the tool.
        arguments.push("/Zc:preprocessor".into());
        if let Some(standard) = config.standard.as_deref() {
            arguments.push(format!("/std:{standard}"));
        }
        for define in &config.defines {
            arguments.push(match &define.value {
                Some(value) => format!("/D{name}={value}", name = define.name),
                None => format!("/D{}", define.name),
            });
        }
        for name in &config.undefines {
            arguments.push(format!("/U{name}"));
        }
        // `/I` and `-I` are spelled the same for `cl`, and the order is the configuration's.
        for path in &config.include_paths {
            arguments.push(format!("/I{}", path.directory.display()));
        }
        // The system directories are not passed: `cl` finds them through `INCLUDE`, which the caller sets — see
        // `compiler_environment`, which is where that list goes.
    } else {
        arguments.push("-E".into());
        if let Some(standard) = config.standard.as_deref() {
            arguments.push(format!("-std={standard}"));
        }
        if let Some(target) = config.target.as_deref() {
            arguments.push(format!("--target={target}"));
        }
        for define in &config.defines {
            arguments.push(match &define.value {
                Some(value) => format!("-D{name}={value}", name = define.name),
                None => format!("-D{}", define.name),
            });
        }
        for name in &config.undefines {
            arguments.push(format!("-U{name}"));
        }
        for path in &config.include_paths {
            arguments.push(if path.is_system { "-isystem" } else { "-I" }.into());
            arguments.push(path.directory.display().to_string());
        }
    }

    // The file last: a path is the one argument that can look like a flag.
    arguments.push(file.display().to_string());
    arguments
}

/// The environment the compiler needs: `INCLUDE` on MSVC, and nothing elsewhere.
///
/// `cl` has no `-E -v` to be asked where its headers are, so the directories are handed to it the way a developer
/// prompt does. The list is the configuration's own, in its own order, which is the same list the analysis resolves
/// includes against — a comparison that let the compiler find a header the analysis did not would report a
/// difference that belongs to this function rather than to the reading.
///
/// `PATH` is deliberately **not** replaced: the compiler may need to find its own runtime and its own linker
/// directory. Only `INCLUDE` is set, and only when there is a list to set.
fn compiler_environment(
    config: &cpp_code_analysis::CompilerConfig,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    if config.dialect() != cpp_parser::Dialect::Msvc || config.include_paths.is_empty() {
        return Vec::new();
    }

    // A path that cannot be joined (`;` inside one, which Windows allows very few of) leaves the variable unset
    // rather than set to something the compiler would parse wrongly — and an unset `INCLUDE` makes `cl` report a
    // missing header, which is a visible failure rather than a silent difference in the reading.
    let Ok(joined) = std::env::join_paths(config.include_paths.iter().map(|path| &path.directory)) else {
        return Vec::new();
    };

    vec![(std::ffi::OsString::from("INCLUDE"), joined)]
}

/// **Take the Windows verbatim prefix off a canonical path**, so it is spelled the way the rest of the analysis
/// spells it.
///
/// `\\?\C:\dir\file.cpp` and `C:\dir\file.cpp` name one file and compare unequal as strings. The analysis keys its
/// summaries by the path its own scan produced — a plain absolute path — so a caller that hands over the verbatim
/// spelling is asking about a file the index has never heard of, and gets "nothing to read" for a file it just read.
///
/// The prefix is only removed for the shape it is safe to remove: a **drive path** (`\\?\C:\…`). A UNC path
/// (`\\?\UNC\server\share`) means something different without it, so it is left alone — the honest answer there is a
/// path this tool cannot use, not a rewritten one that points somewhere else.
#[cfg(windows)]
fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let Some(rest) = text.strip_prefix(r"\\?\") else {
        return path.to_path_buf();
    };

    // `\\?\C:\…` — a drive letter, a colon, a separator. Anything else keeps its prefix.
    let bytes = rest.as_bytes();
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

/// [`strip_verbatim`] on a platform with no verbatim prefix, where this is the identity.
#[cfg(not(windows))]
fn strip_verbatim(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// One line per file: how many tokens it contributed to our reading.
///
/// This is where a difference usually localises, and it is the reading a machine with no compiler can still record:
/// a header that contributed 0 tokens here and 4 000 on the compiler's side is an include that did not resolve,
/// which is a different work item from a macro that did not expand.
///
/// Both sides of the map are normalized for the reason [`record_files`] gives: the seeds come from the session's own
/// spelling of a path and the counts from the tokens' normalized one, and two spellings of one path produce two
/// entries — one with the real count and one with zero.
fn print_per_file(ours: &[StreamToken], files: &[PathBuf]) {
    let mut counts: HashMap<PathBuf, usize> = files
        .iter()
        .map(|path| (cpp_code_analysis::align::normalized(path), 0))
        .collect();
    for token in ours {
        if let Some(file) = token.file.as_deref() {
            *counts.entry(cpp_code_analysis::align::normalized(file)).or_default() += 1;
        }
    }

    println!("\n--- tokens per file (ours) ---");
    let mut listed: Vec<(PathBuf, usize)> = counts.into_iter().collect();
    listed.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    for (path, count) in listed.iter().take(40) {
        println!("  {count:>7}  {}", path.display());
    }
    if listed.len() > 40 {
        println!("  … {} more files", listed.len() - 40);
    }

    let silent = listed.iter().filter(|(_, count)| *count == 0).count();
    if silent > 0 {
        println!("  ({silent} files the walk entered contributed no token)");
    }
}

/// **Which compiler a program is**, for the one check that matters: is the oracle the same toolchain the analysis
/// modelled?
///
/// Deliberately a **family** and not a name. `clang++` and `clang-cl` are the same program with different front-end
/// switches and the same predefined macros apart from the MSVC-compatibility ones, so comparing file names would
/// report a difference that is really a spelling difference; and `cl.exe` on `PATH` versus a versioned `cl.exe` deep
/// in a Visual Studio installation are the same family and must compare equal.
///
/// The order matters, and it is the one thing easy to get wrong: **`clang-cl` contains `cl`**, so a family test that
/// looked for `cl` first would file it as MSVC — which is the opposite of what it is, for a comparison. Hence
/// `clang` before `cl`, and hence the MSVC test being the *last* resort rather than the first.
///
/// An unrecognised name is returned **verbatim**, which is what makes the comparison total: two different unknown
/// names must not compare equal, or the warning this feeds would stay silent exactly when nobody can check it. The
/// same applies to `clang-cl` versus `clang++` — a distinction this deliberately does *not* make, because those two
/// differ in `_MSC_VER` and nothing else the comparison reads.
fn compiler_family(program: &Path) -> String {
    let stem = program
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    if stem.contains("clang") {
        "clang".to_string()
    } else if stem.contains("msvc") || stem == "cl" || stem.starts_with("cl-") {
        "msvc".to_string()
    } else if stem.contains("gcc") || stem.starts_with("g++") || stem == "cc" || stem == "c++" {
        "gcc".to_string()
    } else {
        stem
    }
}

/// The first `limit` lines of a compiler's message, because a compiler can be verbose about one mistake.
fn first_lines(text: &str, limit: usize) -> String {
    text.lines().take(limit).collect::<Vec<_>>().join("\n")
}
