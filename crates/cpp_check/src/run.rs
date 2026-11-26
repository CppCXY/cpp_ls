//! **One session's share of the files** — the pipeline a client's `didOpen` starts, per file, without the client.
//!
//! # What each step is for, and what it costs
//!
//! ```text
//! load(file)         the file's text into the VFS — no document, because nobody has it open
//! catch_up(file)     its summary in the index (and up to eight includes the index has never read)
//! cook(file)         its include closure rendered the way a compiler reads it, and the cooked reading filed
//! pump               the summaries the checks' gate needs, asked for by file rather than popped off the queue
//! diagnostics(file)  the rules, against that reading
//! ```
//!
//! **Measured on neovim (228 sources, release, this machine):** the render is the cost — 44 s of a 60-file run's 52 s,
//! which is one closure render per file either way. Opening each file (`did_open`) only moved that work into
//! `catch_up`, where it built the translation unit the render then used: 55.3 s against 51.9 s, the same machine and
//! the same files. A batch therefore `load`s its files — the attribution is honest and the unit is not built twice —
//! but the render is what it is.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::file::coordinates::ReadingOffset;
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

use crate::cli::Level;
use crate::report::{Report, position_of};

#[allow(clippy::too_many_arguments)]
pub fn check_files(
    root: &Path,
    files: &[PathBuf],
    min: Level,
    only: Option<&BTreeSet<&'static str>>,
    skipped: Option<&BTreeSet<&'static str>>,
    _tracing: bool,
    quiet: bool,
) -> Run {
    let mut session = Session::open(
        root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(root),
    );
    session.add_project_files(files.to_vec());

    let mut run = Run::default();
    for (at, file) in files.iter().enumerate() {
        if !quiet && at > 0 && at % 25 == 0 {
            eprintln!(
                "cpp_check: {} / {} files, {} finding(s)",
                at,
                files.len(),
                run.findings.len()
            );
        }

        let text_started = Instant::now();
        let text = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(_) => {
                run.unreadable.push(file.clone());
                run.tally.unreadable += 1;
                continue;
            }
        };
        run.stages.text += text_started.elapsed();
        // **`load`, not `did_open`** — see the note above: a document is a file the reader has open, and the session
        // answers that by building it a translation unit.
        session.load(file);
        let caught = Instant::now();
        session.catch_up(file);
        let catch_up_time = caught.elapsed();
        run.stages.catch_up += catch_up_time;
        let cooked_at = Instant::now();
        let cooked = session.cook(file);
        let cook_time = cooked_at.elapsed();
        run.stages.cook += cook_time;
        if cooked.is_none() {
            // A file whose closure cannot be rendered is a file the checks have nothing trustworthy to say about. Its
            // parse errors are still the reader's business, and `diagnostics` answers from the raw reading when there
            // is no cooked one — so the file is not dropped here.
            run.no_reading.push(file.clone());
            run.tally.no_reading += 1;
        }
        // **The checks' own gate.** A check reads *absence* out of the index — "nothing declares this member" — so it
        // may only be asked once every header this file includes unconditionally has been read; otherwise an empty
        // index is reported as a missing declaration. `cook` renders the closure but does not read every include's
        // summary, so the pump is run until the gate opens (bounded, because a header that cannot be read at all must
        // not stall the run), and asked for by file: a blind `advance` pops the *project* queue and read hundreds of
        // files this one cannot see before reaching the headers it can — six batches of 256 for six files.
        let pumped = Instant::now();
        session.want_the_closure_cooked(file);
        let mut rounds = 0usize;
        while !session.is_closure_ready(file) && rounds < 64 {
            if session.advance(256).is_empty() {
                break;
            }
            rounds += 1;
        }
        run.stages.pump += pumped.elapsed();
        run.stages.pump_rounds += rounds;
        let diagnosed = Instant::now();
        let Some(diagnostics) = session.diagnostics(file) else {
            run.no_reading.push(file.clone());
            run.tally.no_reading += 1;
            continue;
        };
        run.stages.diagnostics += diagnosed.elapsed();

        run.tally.checked += 1;
        run.tally.parse_errors += diagnostics.errors.len();
        if diagnostics.checks_withheld {
            run.tally.withheld += 1;
        }

        // **The file's own text, for line numbers.** A diagnostic's range is in the *rendering's* coordinates — the
        // text with macros expanded — so it is mapped back through the rendering's spans before a line is counted.
        // Measured on a file with a macro on its first line: the unmapped number was off by more than a hundred lines.
        let viewed = Instant::now();
        let rendered = session.view(file);
        run.stages.view += viewed.elapsed();
        let in_the_file = |offset: usize| -> usize {
            rendered
                .as_ref()
                .and_then(|view| view.file_offset_of(ReadingOffset::new(offset)))
                .map(|offset| offset.get())
                .unwrap_or(offset)
        };

        for error in &diagnostics.errors {
            let start = in_the_file(error.start);
            let (line, column) = position_of(&text, start);
            run.findings.push(Report {
                file: file.clone(),
                line,
                column,
                level: Level::Error,
                code: "syntax-error",
                message: error.message.clone(),
            });
        }
        for finding in &diagnostics.checks {
            let level = Level::of(finding.code.severity());
            if level.rank() < min.rank() {
                continue;
            }
            if let Some(only) = only
                && !only.contains(finding.code.name())
            {
                continue;
            }
            if let Some(skipped) = skipped
                && skipped.contains(finding.code.name())
            {
                continue;
            }
            let start = in_the_file(finding.range.start_offset);
            let (line, column) = position_of(&text, start);
            run.findings.push(Report {
                file: file.clone(),
                line,
                column,
                level,
                code: finding.code.name(),
                message: finding.message.clone(),
            });
        }
    }
    run.stats = session.unit_stats();
    run
}

/// **What one session's share came to** — merged by the caller, which sorts the findings so the report does not
/// depend on which thread found what.
#[derive(Default)]
pub struct Run {
    pub findings: Vec<Report>,
    pub unreadable: Vec<PathBuf>,
    pub no_reading: Vec<PathBuf>,
    pub tally: Tally,
    pub stages: Stages,
    pub stats: cpp_code_analysis::session::UnitStats,
}

/// Where the run's time went — the tool's own instrument, because a batch check's cost is a *policy* question
/// ("read the closure of every translation unit") and only a breakdown can say which part of the policy it is.
#[derive(Default)]
pub struct Stages {
    pub text: std::time::Duration,
    pub catch_up: std::time::Duration,
    pub cook: std::time::Duration,
    pub pump: std::time::Duration,
    pub diagnostics: std::time::Duration,
    pub view: std::time::Duration,
    pub pump_rounds: usize,
}
/// **What a file's run came to** — the answers a check run can give about a file that are not findings, and the reason
/// a summary is not just a finding count: a file with no reading and a file with nothing wrong are different answers.
#[derive(Default)]
pub struct Tally {
    pub checked: usize,
    pub no_reading: usize,
    pub unreadable: usize,
    pub withheld: usize,
    pub parse_errors: usize,
}
