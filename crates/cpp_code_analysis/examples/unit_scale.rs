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

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

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

        let read = Instant::now();
        let reading = session.read_the_unit(source);
        let read_seconds = read.elapsed().as_secs_f64();

        rendered_total += rendered;
        read_total += read_seconds;

        let (files, tokens) = match &stream {
            Some(stream) => (stream.files.len(), stream.len()),
            None => (0, 0),
        };
        let placed = reading.as_ref().map(|reading| reading.files).unwrap_or(0);
        println!(
            "{:>3}/{:<3} {:<28} files {files:>4}  tokens {tokens:>7}  render {rendered:>7.2}s  read {read_seconds:>7.2}s  filed {placed}",
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
