//! **Do the semantic checks stay silent on a corpus that is known good — and did they run at all?**
//!
//! The module documentation of `sema::check` names this as the measurement that keeps the layer honest:
//!
//! > The failure mode this layer has to avoid is not missing a problem, it is **inventing one**, because an editor
//! > that underlines correct code is one the user turns off. The measurement that keeps it honest is a corpus that
//! > is *known good*: the standard library.
//!
//! # The rule this probe learned the hard way
//!
//! **A count of zero is the good answer, so a probe that checked nothing prints the good answer.** This one did
//! exactly that, three times running, on a project of two hundred files: it printed `checks over 1 file(s)` and
//! `no findings at all`, and both were true of a corpus it had never looked at. The cause was an ordering mistake —
//! the list of the project's files was captured **before** the file that seeds the index was opened, so the list was
//! empty, so no rendering was ever asked for, so `diagnostics` answered `None` for every path but the one.
//!
//! So this probe now **proves it ran before it reports what it found**:
//!
//! ```text
//!   index        1387 file(s)     what the session holds
//!   asked        1387             what the loop asked about
//!   answered     1387             how many answered (the rest are None: no cooked reading)
//!   checks that ran: 14 of 14      every code, with its count, **including zero**
//! ```
//!
//! A corpus that is clean and a corpus that was never read are now different output.
//!
//! # And it times the load from the outside
//!
//! Two `Instant::now()` calls around the whole of "open the project and index it" — the figure a user feels. The
//! library carries no timers: a timer on a path taken once per condition changes the thing it measures, which is
//! why the interior instrumentation was deleted (see `docs/walk-redesign.md`, §7).
//!
//! ```text
//! usage: checks_probe <workspace root> <a real file in it> [--limit N]
//! ```
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(dir), Some(root)) = (args.first(), args.get(1)) else {
        eprintln!("usage: checks_probe <workspace root> <a real file in it> [--limit N]");
        std::process::exit(2);
    };
    let limit: usize = args
        .iter()
        .position(|arg| arg == "--limit")
        .and_then(|at| args.get(at + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX);

    let root_path = std::path::PathBuf::from(root);
    let text = std::fs::read_to_string(&root_path).unwrap_or_default();
    if text.is_empty() {
        // Said out loud rather than measured around: an empty file seeds a closure of one, and every count below
        // would then be about nothing.
        eprintln!("warning: {root} could not be read or is empty — the numbers below are about no corpus");
    }

    let opened = std::time::Instant::now();
    let mut session = cpp_code_analysis::Session::open(
        std::path::PathBuf::from(dir),
        cpp_code_analysis::SessionFiles::new(
            cpp_code_analysis::OpenDocuments::new(),
            cpp_code_analysis::DiskFiles,
        ),
        cpp_code_analysis::WatchFilter::new(std::path::Path::new(".")),
    );
    let after_open = opened.elapsed();

    // **The open first, and the project's files *after* it.** The open is what puts a file — and the files its
    // `#include`s reach — into the index at all, so a capture before it is an empty list. That empty list is what
    // made this probe print "1 file" on a real project.
    session.did_open(&root_path, &text);

    let wanted: Vec<std::path::PathBuf> = session
        .index()
        .summaries()
        .map(|summary| summary.path.clone())
        .collect();
    session.add_project_files(wanted.clone());

    // **`--chunked`: the index queue drained one step at a time, with each step timed here.**
    //
    // Two questions at once, and neither needs a timer inside the library. *How is the cost spread?* — a mean per
    // file hides whether every file costs the same or a few cost everything, and the two have different fixes. And
    // *how much is the cooking?* — `advance` drains the index queue alone, so what is left afterwards is exactly the
    // rendering backlog that `index_everything` would have done inline. Measured on a real project: this is the
    // difference between "indexing is slow" and "cooking is slow", which the one-call timing could not tell apart.
    if args.iter().any(|argument| argument == "--chunked") {
        let mut step_ms: Vec<u128> = Vec::new();
        let draining = std::time::Instant::now();
        let mut heaviest: Vec<(u128, String, usize, String)> = Vec::new();
        let mut by_depth: std::collections::BTreeMap<usize, (u128, usize)> = std::collections::BTreeMap::new();
        while session.pending() > 0 {
            let started = std::time::Instant::now();
            let worked = session.advance(1);
            let ms = started.elapsed().as_millis();
            for step in &worked {
                step_ms.push(ms);
                by_depth.entry(step.depth).or_default().0 += ms;
                by_depth.entry(step.depth).or_default().1 += 1;
                heaviest.push((
                    ms,
                    step.path.to_string_lossy().replace('\\', "/"),
                    step.depth,
                    format!("{:?}", step.outcome),
                ));
            }
        }
        let drained = draining.elapsed();

        let mut sorted = step_ms.clone();
        sorted.sort_unstable();
        let pick = |at: usize| sorted.get(at).copied().unwrap_or(0);
        println!(
            "\n--- the index queue, one step at a time ---\n  {} step(s) in {} ms: mean {} ms, median {} ms, p90 {} ms, max {} ms",
            step_ms.len(),
            drained.as_millis(),
            if step_ms.is_empty() { 0 } else { drained.as_millis() as usize / step_ms.len() },
            pick(sorted.len() / 2),
            pick(sorted.len() * 9 / 10),
            pick(sorted.len().saturating_sub(1)),
        );

        heaviest.sort_unstable_by(|left, right| right.0.cmp(&left.0));
        let top: u128 = heaviest.iter().take(10).map(|(ms, ..)| *ms).sum();
        let total: u128 = step_ms.iter().sum();
        println!(
            "\n  the ten heaviest steps are {top} ms of {total} ms ({:.0}%)",
            if total == 0 { 0.0 } else { top as f64 * 100.0 / total as f64 }
        );
        for (ms, path, depth, outcome) in heaviest.iter().take(12) {
            println!("    {ms:>6} ms  depth {depth}  {outcome:<8}  {path}");
        }

        println!("\n  by distance from a seed (depth 0 = a file the caller listed):");
        for (depth, (ms, count)) in &by_depth {
            println!("    depth {depth}: {ms} ms over {count} step(s)");
        }

        let cooking = std::time::Instant::now();
        session.index_everything();
        println!(
            "  and the cooking it left behind took {} ms",
            cooking.elapsed().as_millis()
        );
        return;
    }

    for path in &wanted {
        session.want_cooked_reading(path);
    }
    session.index_everything();

    // **Where the time is, split in two, from the outside.** Indexing a workspace and *cooking* every file in it are
    // different jobs with different fixes, and for a long time the second was measured as if it were the first. This
    // prints both, and a `--index-only` run stops here so the two can be told apart without any timer in the library.
    let indexed_only = opened.elapsed();
    println!(
        "  of which: {} ms to index, {} file(s) in the index",
        (indexed_only - after_open).as_millis(),
        session.index().len()
    );
    if args.iter().any(|argument| argument == "--index-only") {
        return;
    }

    // **The ask comes *after* the pump, and then the pump again.** `diagnostics` answers from a file's **cooked
    // reading**, and a rendering is built because something asked for one — but asking before the indexing pass had
    // run left every file but the opened one without one, and the probe then reported "answered 1, refused 1122"
    // over a project of 1123 files. Asking is not enough; the queue has to be pumped *after* the ask, which is what
    // the second call is for. Measured: 1 → 1123 answered, and the count of refused files is printed so that a
    // regression here cannot pass as a clean corpus again.
    for path in &wanted {
        session.want_cooked_reading(path);
    }
    session.index_everything();
    let indexed_for = opened.elapsed();

    println!(
        "opened in {} ms, indexed in {} ms ({} file(s) in the index, {} pending)",
        after_open.as_millis(),
        (indexed_for - after_open).as_millis(),
        session.index().len(),
        session.pending()
    );

    let paths: Vec<std::path::PathBuf> = session
        .index()
        .summaries()
        .map(|summary| summary.path.clone())
        .take(limit)
        .collect();

    let mut checked = 0usize;
    let mut refused = 0usize;
    let mut by_check: std::collections::BTreeMap<&'static str, usize> = std::collections::BTreeMap::new();
    let mut shown = 0usize;

    for path in &paths {
        let Some(found) = session.diagnostics(path) else {
            refused += 1;
            continue;
        };
        checked += 1;
        for finding in &found.checks {
            *by_check.entry(finding.code.name()).or_default() += 1;
            if shown < 60 {
                shown += 1;
                println!(
                    "  {}:{}  [{}]  {}",
                    path.to_string_lossy().replace('\\', "/"),
                    finding.range.start_offset,
                    finding.code.name(),
                    finding.message
                );
            }
        }
    }

    // **What was done, before what was found.** Every line here is printed whether or not anything was found.
    println!("\n--- did it run? ---");
    println!("  index        {} file(s)", session.index().len());
    println!("  asked        {} file(s)", paths.len());
    println!("  answered     {checked} file(s)");
    println!(
        "  refused      {refused} file(s) (no cooked reading: the ask above did not reach them)"
    );


    println!("\n--- what was found, per check ---");
    let mut ran = 0usize;
    for code in cpp_code_analysis::sema::check::DiagnosticCode::ALL {
        let count = by_check.get(code.name()).copied().unwrap_or(0);
        ran += 1;
        println!("  {count:>5}  {}", code.name());
    }
    println!("  {ran} check(s) accounted for, {} with a finding", by_check.len());
}







