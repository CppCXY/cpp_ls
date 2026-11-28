//! **The analysis's checks, run over a whole workspace** — CI's half of the same engine an editor reads.
//!
//! ```text
//! $ cpp_check                       # the current directory, findings at `warning` and above
//! $ cpp_check E:\neovim             # a project
//! $ cpp_check --min-severity=hint   # style hints too
//! $ cpp_check --json                # machine-readable
//! $ cpp_check --code=use-after-free,dangling-return
//! $ cpp_check E:\neovim --file src/nvim/cursor.c --file src/nvim/api/vim.c
//! $ cpp_check E:\neovim --files-from changed.txt
//! ```
//!
//! **A named file set is the practical mode.** C++ locality is strong, and every file checked pays for its own include
//! closure (~0.8 s of render on neovim); a hook or a person knows the four files a change touched. `--file` and
//! `--files-from` say so and skip the walk — the project (`ROOT`) still supplies the compile database and flags, so the
//! files are read the way the build reads them.
//!
//! # What it is, and what it is not
//!
//! It is the **same run** a reader gets in an editor: one [`Session`] over the project (the compile database, the
//! toolchain's include search, `.cppls.toml`), each file opened and read the way a compiler reads it, and
//! [`Session::diagnostics`] — so every finding here is a finding the editor would show on the same line, and a rule
//! fixed in the server is fixed here.
//!
//! It is **not** a compiler. The compiler is the authority on whether a program builds, and this tool does not
//! pretend otherwise: run it for the rules a compiler does not have — a pure virtual nothing overrides anywhere in
//! the workspace, a name nothing uses, an include that is missing, a reference to something that dies — and for the
//! files a build never sees. `docs/cpp-check.md` has the split, measured.
//!
//! # The reading step is the cost, and it is per file
//!
//! A check that resolves names against a file whose macros are unexpanded is a check about a program nobody compiles,
//! so every file is **cooked** before it is asked about: `did_open` puts the text in the VFS, `catch_up` reads its
//! summary, `cook` renders the closure and files the cooked reading `diagnostics` answers from. The three are the
//! server's own steps for a file a client opened, in the same order.
//!
//! `index_everything` looks like the batch version of that and is not: on a real project it spins — measured on
//! neovim, **seventeen seconds of CPU in sixteen minutes** with the session making no progress, the
//! "pending 0, cooking N on every round" failure its own comments describe. The server never drains that queue in one
//! call either; it pumps in slices under a budget, and so does this.
//!
//! # Why it exits non-zero
//!
//! A check run is a gate: `--min-severity` says which findings are fatal, and a run with any of them exits `1`. The
//! default (`warning`) deliberately excludes hints, because a hint is a suggestion and a build that fails on a
//! suggestion is a build somebody will turn the gate off for.

mod cli;
mod report;
mod rules;
mod run;
mod walk;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use clap::Parser;

use crate::cli::Args;
use crate::report::{Fate, Report, report_json, report_text};
use crate::rules::{codes, rule_names};
use crate::run::{Stages, Tally, check_files};

fn main() {
    let args = Args::parse();
    // **Not `canonicalize`**: on Windows that answers `\\?\C:\…`, and the analysis keys its index, its renderings and
    // its diagnostics on the spelling a client writes — `C:\…`. Measured here: every file "could not be read" with the
    // verbatim prefix, because `Session::diagnostics` looked for a summary under a path nobody had filed.
    let root = match std::path::absolute(&args.root) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("cpp_check: cannot open {}: {error}", args.root.display());
            std::process::exit(2);
        }
    };

    let files = match cli::files_to_check(&root, &args) {
        Ok(files) => files,
        Err(problem) => {
            eprintln!("cpp_check: {problem}");
            std::process::exit(2);
        }
    };
    if files.is_empty() {
        eprintln!("cpp_check: no files to check");
        std::process::exit(2);
    }
    // **What the session is told the project is.** A run that reads the whole project needs the *project* rather than
    // the named files: `missing-include` finds `OnlyHere` only if the header that declares it was read, and a header is
    // not one of the files a check run names. Headers are included for that reason, and the walk is the same one
    // `files_to_check` uses when nothing is named.
    let project = if args.index {
        walk::discover(&root, true, &args.exclude)
    } else {
        files.clone()
    };

    // **The names the filters allow**, resolved once: an unknown code is a typo, not an empty filter, and a check run
    // that silently reports nothing because of one is worse than one that refuses.
    let only = match codes(&args.code) {
        Ok(codes) => codes,
        Err(unknown) => {
            eprintln!("cpp_check: --code names no rule: {unknown}");
            eprintln!("cpp_check: the rules are: {}", rule_names().join(", "));
            std::process::exit(2);
        }
    };
    let skipped = match codes(&args.skip_code) {
        Ok(codes) => codes,
        Err(unknown) => {
            eprintln!("cpp_check: --skip-code names no rule: {unknown}");
            eprintln!("cpp_check: the rules are: {}", rule_names().join(", "));
            std::process::exit(2);
        }
    };

    let clock = cli::Clock::start();
    // **One session, one pass.** Files are checked in path order, and a check reads the index — so the answer to a
    // file early in the run is built from less than the answer to a file late in it. That is the price of not
    // indexing the whole project first (`index_everything` is the call that would, and on a real project it does not
    // finish: measured on neovim, seventeen seconds of CPU in sixteen minutes with the session making no progress).
    // Parallel sessions were tried and **withdrawn**: each one indexes only what it reads, so the same 20 files gave
    // 4, 5 and 34 findings at `--jobs=1/4/8` — a report that depends on the split is not a report.
    let mut runs = vec![check_files(
        &root,
        &files,
        args.min_severity,
        only.as_ref(),
        skipped.as_ref(),
        args.time || (!args.quiet && !args.json),
        args.quiet || args.json,
        &project,
        args.index,
    )];
    let mut findings: Vec<Report> = Vec::new();
    let mut unreadable: Vec<PathBuf> = Vec::new();
    let mut no_reading: Vec<PathBuf> = Vec::new();
    let mut tally = Tally::default();
    let mut stages = Stages::default();
    let mut stats = cpp_code_analysis::session::UnitStats {
        from_memory: 0,
        from_disk: 0,
        walked: 0,
    };
    for run in &mut runs {
        findings.append(&mut run.findings);
        unreadable.append(&mut run.unreadable);
        no_reading.append(&mut run.no_reading);
        tally.checked += run.tally.checked;
        tally.withheld += run.tally.withheld;
        tally.parse_errors += run.tally.parse_errors;
        stages.text += run.stages.text;
        stages.catch_up += run.stages.catch_up;
        stages.cook += run.stages.cook;
        stages.pump += run.stages.pump;
        stages.diagnostics += run.stages.diagnostics;
        stages.view += run.stages.view;
        stages.pump_rounds += run.stages.pump_rounds;
        stats.from_memory += run.stats.from_memory;
        stats.from_disk += run.stats.from_disk;
        stats.walked += run.stats.walked;
    }
    findings.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line.cmp(&right.line))
            .then(left.column.cmp(&right.column))
            .then(left.code.cmp(right.code))
    });

    let seconds = clock.seconds();
    if args.time && !args.json {
        eprintln!(
            "cpp_check: units from memory {}, from disk {}, walked {}",
            stats.from_memory, stats.from_disk, stats.walked
        );
        eprintln!(
            "cpp_check: {:.1} s = text {:.1} + catch_up {:.1} + cook {:.1} + pump {:.1} ({stages} rounds) + diagnostics {:.1} + view {:.1}",
            seconds,
            stages.text.as_secs_f64(),
            stages.catch_up.as_secs_f64(),
            stages.cook.as_secs_f64(),
            stages.pump.as_secs_f64(),
            stages.diagnostics.as_secs_f64(),
            stages.view.as_secs_f64(),
            stages = stages.pump_rounds,
        );
    }
    let told = Fate {
        unreadable,
        no_reading,
        withheld: tally.withheld,
        parse_errors: tally.parse_errors,
        checked: tally.checked,
        seconds,
    };
    if args.json {
        report_json(&root, &files, &findings, &told);
    } else {
        report_text(&root, &files, &findings, &told);
    }

    // **The gate.** A finding at or above the minimum severity fails the run; the summary above already said what it
    // was, so the exit code carries nothing a reader has to interpret.
    let fatal = findings
        .iter()
        .filter(|finding| finding.level.rank() >= args.min_severity.rank())
        .count();
    std::process::exit(if fatal == 0 { 0 } else { 1 });
}
