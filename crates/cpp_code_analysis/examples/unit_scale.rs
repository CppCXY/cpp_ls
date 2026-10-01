//! **What a whole project costs to read as programs** — one `read_the_unit` per source, timed.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example unit_scale -- <dir>
//! ```
//!
//! # Why this exists
//!
//! The plan's §5.1 M4 (渲染缓存) is written against a number: *one* `read_the_unit` of a real `<format>` project is
//! 27 seconds, and a session that has to do that once **per source file** spends minutes on a project a reader is
//! waiting on. Every other probe in this directory measures one unit, so none of them can show that total — and the
//! total is what a reader feels.
//!
//! What it reports per unit is deliberately not just the time:
//!
//! ```text
//! files, tokens   how big the program was
//! render          the stitch, from the walk's timeline to one stream
//! read            render, parse, and index the program
//! ```
//!
//! A cache that makes the second render of a unit free shows up as a `render` of nearly nothing on the second and
//! later units that share the same headers; a cache that does not exist shows every unit paying the full price.

use std::path::PathBuf;
use std::time::Instant;

use cpp_code_analysis::{
    DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    stages::Stage,
};

fn main() {
    let root: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );

    let indexing = Instant::now();
    let indexed = session.index_everything();
    println!(
        "indexed {indexed} files in {:.2}s",
        indexing.elapsed().as_secs_f64()
    );

    // Every source the project names, in the order the pump would take them.
    let sources: Vec<PathBuf> = session.project_files().to_vec();
    println!("{} source files to read", sources.len());

    let whole = Instant::now();
    let mut rendered_total = 0.0f64;
    let mut read_total = 0.0f64;

    for (index, source) in sources.iter().enumerate() {
        let render = Instant::now();
        let stream = session.render_the_unit(source);
        let rendered = render.elapsed().as_secs_f64();

        // **The second render of the same unit**, which is what a keystroke path pays. It must be nearly free:
        // the plan's M4 is exactly this number, and a cache keyed on the inputs is what makes it so.
        let again = Instant::now();
        let _ = session.render_the_unit(source);
        let second = again.elapsed().as_secs_f64();

        // **Where this unit's read time went**, as the difference of two readings of the crate's global counters.
        // The totals at the end of a run cannot say which unit paid what, and a stage that runs hundreds of times
        // inside one unit is invisible without this.
        let before_stages = cpp_code_analysis::stages::StageTimes::read();
        let read = Instant::now();
        let reading = session.read_the_unit(source);
        let read_seconds = read.elapsed().as_secs_f64();
        let spent = cpp_code_analysis::stages::StageTimes::read().since(before_stages);
        if std::env::var_os("CPPLS_STAGES").is_some() {
            let mut ranked: Vec<(Stage, std::time::Duration)> = [
                Stage::Facts,
                Stage::Scopes,
                Stage::Scan,
                Stage::Parse,
                Stage::Sweep,
                Stage::RenderParse,
                Stage::RenderSweep,
                Stage::UnitRender,
                Stage::UnitFiles,
                Stage::Includes,
                Stage::Guards,
                Stage::Settling,
                Stage::Lookup,
                Stage::Read,
                Stage::TypeOf,
                Stage::Drop,
            ]
            .into_iter()
            .map(|stage| (stage, spent.of(stage)))
            .filter(|(_, duration)| !duration.is_zero())
            .collect();
            ranked.sort_unstable_by(|left, right| right.1.cmp(&left.1));
            let shown = ranked
                .into_iter()
                .take(5)
                .map(|(stage, duration)| format!("{} {:.2}s", stage.name(), duration.as_secs_f64()))
                .collect::<Vec<_>>()
                .join(", ");
            println!("        spent: {shown}");
        }

        // **The second read of the same unit** 鈥?what a keystroke on an already-read file pays.
        let reread = Instant::now();
        let _ = session.read_the_unit(source);
        let reread_seconds = reread.elapsed().as_secs_f64();

        rendered_total += rendered;
        read_total += read_seconds;

        // `CPPLS_DUMP=<path>` writes the first unit's rendered program, so the parser can be measured on exactly
        // the text the sweep is given rather than on a source file that merely resembles it.
        if let Some(target) = std::env::var_os("CPPLS_DUMP")
            && index == 0
            && let Some(stream) = &stream
        {
            std::fs::write(&target, &stream.text).expect("the dump path is writable");
            println!("        dumped {} bytes to {}", stream.text.len(), target.to_string_lossy());
        }

        let (files, tokens) = match &stream {
            Some(stream) => (stream.files.len(), stream.len()),
            None => (0, 0),
        };
        let placed = reading.as_ref().map(|reading| reading.files).unwrap_or(0);
        println!(
            "{:>3}/{:<3} {:<28} files {files:>4}  tokens {tokens:>7}  render {rendered:>6.2}s  2nd {second:>6.3}s  read {read_seconds:>7.2}s  2ndread {reread_seconds:>6.3}s  filed {placed}",
            index + 1,
            sources.len(),
            source
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
    }

    // Where the read's own time went, by stage. The crate counts it globally, so this is the whole run.
    if std::env::var_os("CPPLS_STAGES").is_some() {
        use cpp_code_analysis::stages::StageTimes;
        println!("\n--- how often each stage ran ---");
        for stage in [
            Stage::Facts,
            Stage::Scopes,
            Stage::Scan,
            Stage::Parse,
            Stage::Sweep,
            Stage::RenderParse,
            Stage::RenderSweep,
            Stage::UnitRender,
            Stage::UnitFiles,
            Stage::Includes,
        ] {
            println!(
                "  {:>14}  {:>8} entries",
                stage.name(),
                StageTimes::entries(stage)
            );
        }

        println!(
            "\n--- where the time went ---\n{}",
            cpp_code_analysis::stages::StageTimes::read().report()
        );
    }

    println!(
        "\ntotal {:.1}s: render {rendered_total:.1}s, read {read_total:.1}s over {} units",
        whole.elapsed().as_secs_f64(),
        sources.len()
    );
}
