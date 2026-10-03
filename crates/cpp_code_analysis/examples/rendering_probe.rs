//! **The two readings of a file, side by side — and the whole list in one session.**
//!
//! ```text
//! rendering_probe <project-root> <file> [<file> …]
//! ```
//!
//! # Why it takes a list
//!
//! The first version took one file and started a session per run, and a loop over fourteen headers therefore paid
//! the project's setup, its index and its toolchain discovery fourteen times. Measured: **~1 s per run**, and one
//! header (`<xmemory>`) where the walk from the file did not visit the file — so a loop of fourteen was minutes,
//! which is not a loop anybody iterates in.
//!
//! One session, one index, then a cook per file. The setup is paid once and each additional file costs its own
//! cook and nothing else.
//!
//! # What it measures, and what the question is
//!
//! Two readings of one file are built in this crate and both are parsed: the file's own text, and the **rendering**
//! a preprocessor produces from it. A compiler has one — its preprocessor's output is what its parser sees — so the
//! pair is a divergence, and every macro-aware rule in the grammar exists because of the first one.
//!
//! ```text
//! declarations   how many the reading produces
//! errors         how many the parse reported
//! _STD-ish       how many names still carry a macro prefix — the crutch's reason to exist
//! time           what each reading costs
//! ```
//!
//! If the rendering reads **more**, with **fewer** errors, and with **no** macro-prefixed names, the raw reading is
//! the crutch and the divergence is what it costs.

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use std::collections::BTreeSet;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next() else {
        eprintln!("usage: rendering_probe <project-root> <file> [<file> …]");
        std::process::exit(2);
    };
    let rest: Vec<String> = args.collect();
    let names = rest.iter().any(|arg| arg == "--names");
    let dump = rest
        .iter()
        .position(|arg| arg == "--dump")
        .and_then(|at| rest.get(at + 1))
        .cloned();
    let limit: usize = rest
        .iter()
        .position(|arg| arg == "--limit")
        .and_then(|at| rest.get(at + 1))
        .and_then(|count| count.parse().ok())
        .unwrap_or(8);
    let files: Vec<std::path::PathBuf> = rest
        .iter()
        .enumerate()
        .filter(|(at, arg)| {
            // **A switch, or the value that belongs to one.** Everything else names a file, and getting this wrong
            // is not cosmetic: a `--dump <path>` whose path was taken for an input file made the probe dump the
            // rendering for the real file and then dump an **empty** one for the path it had just written, so the
            // text it was asked for was overwritten by nothing before anything could read it.
            !arg.starts_with("--")
                && !matches!(
                    rest.get(at.wrapping_sub(1)).map(String::as_str),
                    Some("--limit") | Some("--dump")
                )
        })
        .map(|(_, arg)| std::path::PathBuf::from(arg))
        .collect();
    if files.is_empty() {
        eprintln!("usage: rendering_probe <project-root> <file> [<file> …]");
        std::process::exit(2);
    }

    let started = std::time::Instant::now();
    let mut session = Session::open(
        std::path::PathBuf::from(&root),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    // **Every file is a project file and a buffer.** A project file so the session owns it and will walk it; a
    // buffer so the text the reading uses is the text on disk, read once here rather than per cook.
    session.add_project_files(files.iter().cloned());
    let mut texts = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        session.did_open(file, &text);
        texts.push(text);
    }
    session.index_everything();
    let setup = started.elapsed();

    println!(
        "{:<14} {:>16} {:>16}   {}",
        "file", "raw (decl/err)", "rendering (decl/err)", "notes"
    );

    let (mut raw_declarations, mut rendered_declarations) = (0usize, 0usize);
    for (file, text) in files.iter().zip(&texts) {
        // **The raw reading**: the file's own text, parsed as it is written.
        let (raw, raw_summary) = {
            let at = std::time::Instant::now();
            let tree = cpp_parser::CppParser::parse(text, cpp_parser::ParserConfig::default());
            let errors = tree.get_errors().len();
            let summary =
                cpp_code_analysis::summarize(file, text, cpp_code_analysis::SummaryKey::new(0, 0));
            let prefixed = summary
                .declarations
                .iter()
                .filter(|fact| fact.qualified_name().contains("_STD"))
                .count();
            (
                (summary.declarations.len(), errors, prefixed, at.elapsed()),
                summary,
            )
        };

        // **The rendering**: the preprocessor's reading, which is what a compiler's parser is handed.
        let cooked = {
            let at = std::time::Instant::now();
            let reading = session.cook(file);
            (
                reading.as_ref().map(|reading| reading.declarations),
                reading.as_ref().map(|reading| reading.diagnostics),
                at.elapsed(),
            )
        };

        raw_declarations += raw.0;
        rendered_declarations += cooked.0.unwrap_or(0);

        let raw_cell = format!("{}/{}/{}", raw.0, raw.1, raw.2);
        let cooked_cell = match cooked {
            (Some(declarations), Some(errors), _) => format!("{declarations}/{errors}/0"),
            _ => "— not cooked".to_string(),
        };
        println!(
            "{:<14} {:>16} {:>16}   raw {} / cook {}",
            file.file_name().unwrap_or_default().to_string_lossy(),
            raw_cell,
            cooked_cell,
            format_args!("{:?}", raw.3),
            format_args!("{:?}", cooked.2)
        );

        // **The rendering itself, when asked for**, because a count and a name list say *that* a reading differs
        // and only the text says *why*: a declaration the rendering lacks is either one a branch dropped, one the
        // expansion mangled, or one the raw reading invented, and those are told apart by looking at what the
        // preprocessor produced. `--dump <file>` writes it beside the probe's own output.
        if let Some(into) = dump.as_deref() {
            if let Some(reading) = session.rendered_text_of(file) {
                let _ = std::fs::write(into, reading);
                println!("    wrote the rendering to {into}");
            }
        }

        // **Which names each reading has and the other does not**, when asked for.
        //
        // A count says the readings differ; the names say **how**, and the question a reader of the difference has
        // is not "who has more" but "is the difference a loss or a gain" — the rendering dropping a declaration the
        // file writes, or the raw reading inventing one out of text a macro wrote. `--names` prints both sides so
        // that question is answered by looking rather than by arguing from totals.
        if names {
            let qualified = |declarations: &[cpp_code_analysis::DeclFact]| -> BTreeSet<String> {
                declarations
                    .iter()
                    .map(cpp_code_analysis::DeclFact::qualified_name)
                    .collect()
            };

            let (raw_names, rendered_names) = {
                let raw_names = qualified(&raw_summary.declarations);
                let rendered_names = session
                    .index()
                    .cooked_declarations(file)
                    .map(qualified)
                    .unwrap_or_default();
                (raw_names, rendered_names)
            };

            let only_rendered: Vec<&str> = rendered_names
                .difference(&raw_names)
                .take(limit)
                .map(String::as_str)
                .collect();
            let only_raw: Vec<&str> = raw_names
                .difference(&rendered_names)
                .take(limit)
                .map(String::as_str)
                .collect();

            println!("    the rendering alone has ({}): {only_rendered:?}", rendered_names.len());
            println!("    the raw reading alone has ({}): {only_raw:?}", raw_names.len());

            // **The same comparison by the LAST segment**, which is the one that separates a lost declaration from
            // a differently-qualified one. A file whose macro never opened its namespace has every declaration at
            // file scope — `_Alignas_storage_unit` — while the rendering has the same declaration as
            // `std::_Alignas_storage_unit`, so a comparison by the whole qualified name reads one entity as two and
            // reports a loss where there is a **difference in scope**. Measured on `<memory>`: 477 names the
            // rendering "alone" had and 518 the raw reading "alone" had, and the lists are the same declarations.
            let by_last = |names: &BTreeSet<String>| -> BTreeSet<String> {
                names
                    .iter()
                    .map(|name| {
                        name.rsplit_once("::")
                            .map_or(name.as_str(), |(_, last)| last)
                            .to_string()
                    })
                    .collect()
            };
            let (raw_last, rendered_last) = (by_last(&raw_names), by_last(&rendered_names));
            let only_rendered: Vec<&str> = rendered_last
                .difference(&raw_last)
                .take(limit)
                .map(String::as_str)
                .collect();
            let only_raw: Vec<&str> = raw_last
                .difference(&rendered_last)
                .take(limit)
                .map(String::as_str)
                .collect();
            println!(
                "    by last segment — rendering {} / raw {} — the rendering alone: {only_rendered:?}",
                rendered_last.len(),
                raw_last.len()
            );
            println!("    by last segment — the raw reading alone: {only_raw:?}");
        }
    }

    println!(
        "\ntotals: raw {raw_declarations} declarations, rendering {rendered_declarations}   \
         (setup {setup:?}, all {} ms)",
        started.elapsed().as_millis()
    );
}
