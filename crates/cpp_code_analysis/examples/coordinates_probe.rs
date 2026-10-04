//! **The two coordinate systems, measured on a real file** — the round trip a client's position makes.
//!
//! ```text
//! coordinates_probe <project-root> <file> <line> <column> [<line> <column> …]
//! ```
//!
//! # What it prints, and why each number is worth having
//!
//! A view may be of a **rendering** — the file with its directives resolved and its macros replaced — and then
//! every offset inside it is the rendering's, while the client's line and column are the file's. Four steps connect
//! the two, and a mistake in any one of them shows up as an answer about the *wrong place* rather than as an error:
//!
//! ```text
//! file offset      the line and column resolved against the file's own text
//! reading offset   where that is in the rendering
//! back again       and what the rendering's offset maps back to
//! the text there   the spelling at each, which is what says whether the pair agrees
//! ```
//!
//! Measured on a real report: hovering `std::cout` on line 17 of a 31-line file answered about `x`, declared on
//! line 21 — so the position resolved somewhere other than where the cursor was. The last line of this output is
//! what settles that: if the file offset does not point at the text the client is looking at, the mapping is wrong,
//! and no amount of looking at the analysis will find it.
//!
//! # What this does **not** do, and it matters when reading its answers
//!
//! It renders the file it is given and **cooks nothing else** — so every header is read through its raw summary,
//! while a live session cooks the open file's own closure at its first idle drain. A type that comes from a header
//! can therefore read as `Unknown` here and be answered there: measured, `auto x = std::vector<int>();` in a
//! 31-line file refused here and deduced in the same project read with its closure cooked (319 `auto`, 191 deduced).
//!
//! Reach for `types_probe --cook` with the headers listed when the question is about the analysis rather than about
//! coordinates. This probe is for the mapping, and for that question it is the right instrument.

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().expect("a project root");
    let file = std::path::PathBuf::from(args.next().expect("a file"));
    let asked: Vec<usize> = args.filter_map(|value| value.parse().ok()).collect();

    let mut session = Session::open(
        std::path::PathBuf::from(&root),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let text = std::fs::read_to_string(&file).expect("the file");
    session.add_project_files([file.clone()]);
    session.did_open(&file, &text);
    session.index_everything();
    // **Drained to idle, which is where the closure is cooked.** `want_the_closure_cooked` runs at every idle drain
    // and reaches for the file's **direct includes** — so a probe that stops at `index_everything` reads every
    // *header* through its raw summary, and a raw summary files `std::vector` at file scope because `_STD_BEGIN` is
    // a macro the raw text does not expand. The product drains; a probe that does not is measuring a different
    // program. Measured: without this step `std::vector<int>()` refuses, and with it the same expression deduces.
    for _ in 0..2000 {
        if session.advance(1).is_empty() {
            break;
        }
    }
    // **Built, not waited for.** `Session::view` answers from the file's own tokens until a rendering exists, and a
    // probe that took that answer would be measuring the fallback rather than the reading a client is served.
    let view = session
        .view_of_the_rendering(&file)
        .expect("the file is held and renders");
    let _ = session.view(&file);

    // **What the index actually filed, under the two spellings a lookup can use.** This is the question the whole
    // report turns on, and it is not the question `summarize` answers: a summary built standalone reads the file's
    // own text, while the index's copy is built *with the include closure's macro knowledge* — so `_STD_BEGIN` opens
    // `std` in one and is an ordinary name in the other. `std::vector` is findable only in the first.
    for scope in ["std::vector", "vector", "std::string", "string"] {
        let found = session.index().declarations_in(scope, &file);
        println!(
            "  the index's `{scope}` -> {} declaration(s){}",
            found.len(),
            found
                .first()
                .map(|first| format!(", e.g. `{}`", first.fact.qualified_name()))
                .unwrap_or_default()
        );
    }

    println!("{}", file.display());
    println!(
        "  the view is {} of {} bytes; the file is {} bytes",
        if view.written_text().is_some() { "a rendering" } else { "the file's own tokens" },
        view.source.len(),
        text.len()
    );

    let mut pairs = asked.chunks(2);
    while let Some(pair) = pairs.next() {
        let (Some(&line), Some(&column)) = (pair.first(), pair.get(1)) else {
            break;
        };
        let Some(in_the_file) = view.file_offset_at(line, column) else {
            println!("  {line}:{column}  ** the file has no such position **");
            continue;
        };
        let at_the_file = view
            .written_text()
            .unwrap_or(&view.source)
            .get(in_the_file..)
            .map(|rest| rest.chars().take(28).collect::<String>())
            .unwrap_or_default();

        match view.reading_offset_of(in_the_file) {
            Some(in_the_reading) => {
                let at_the_reading = view
                    .source
                    .get(in_the_reading..)
                    .map(|rest| rest.chars().take(28).collect::<String>())
                    .unwrap_or_default();
                println!(
                    "  {line}:{column}  file offset {in_the_file} (`{at_the_file}`)  ->  reading offset \
                     {in_the_reading} (`{at_the_reading}`)"
                );

                // **What the analysis answers about that offset**, which is the number the report was really about:
                // the mapping can be right and the *name* found there still be the wrong one, and the two failures
                // look identical from a screenshot. `definition_at` is the same call the hover handler makes first.
                let name = cpp_code_analysis::sema::resolve::name_at(&view.root, in_the_reading);
                println!("        name_at -> {name:?}");
                match session.definition(&view, in_the_reading) {
                    cpp_code_analysis::Known::Yes(found) => {
                        let at = found.fact.name_range.start_offset;
                        // **Where the fact says it is, read against BOTH rulers.** The hover's "Declared in
                        // `file:line:col`" asks the *file's* line index about this offset — so if the offset is the
                        // rendering's, the answer is a line number from a different document: measured on a real
                        // report, a class on line 6 of a file with five `#include`s above it was reported on line 1,
                        // with the column right, which is the signature of exactly that mistake.
                        let against_the_file = view
                            .written_text()
                            .unwrap_or(&view.source)
                            .get(at..)
                            .map(|rest| rest.chars().take(24).collect::<String>())
                            .unwrap_or_default();
                        let against_the_reading = view
                            .source
                            .get(at..)
                            .map(|rest| rest.chars().take(24).collect::<String>())
                            .unwrap_or_default();
                        println!(
                            "        definition -> `{}` name_range {}..{} range {}..{}",
                            found.fact.qualified_name(),
                            at,
                            found.fact.name_range.end_offset(),
                            found.fact.range.start_offset,
                            found.fact.range.end_offset()
                        );
                        let at_the_range = found.fact.range.start_offset;
                        let file_at_the_range = view
                            .written_text()
                            .unwrap_or(&view.source)
                            .get(at_the_range..)
                            .map(|rest| rest.chars().take(28).collect::<String>())
                            .unwrap_or_default();
                        let reading_at_the_range = view
                            .source
                            .get(at_the_range..)
                            .map(|rest| rest.chars().take(28).collect::<String>())
                            .unwrap_or_default();
                        // **The popup's first line comes from `range`, and it is read against the file.** A
                        // declaration in the file being viewed carries the *rendering's* offsets, so that read lands
                        // wherever the directives had taken lines out — measured on a real report, the first line of
                        // a popup was `<iostream>`'s tail, `tream>`.
                        println!("          range: the file says    `{file_at_the_range}`");
                        println!("          range: the reading says `{reading_at_the_range}`");
                        println!("          name_range: the file says    `{against_the_file}`");
                        println!("          name_range: the reading says `{against_the_reading}`");
                    }
                    other => println!("        definition -> {other:?}"),
                }
                // **What the name's type is**, which is the question an `auto` variable raises and the one a reader
                // reads the popup for. Separate from `definition` because the two can disagree: a call can be found
                // and its return type still unknown, which is what an `auto` refusal is.
                match session.type_at(&view, in_the_reading) {
                    cpp_code_analysis::Known::Yes(type_of) => {
                        println!("        type -> `{}`", type_of.type_of)
                    }
                    other => println!("        type -> {other:?}"),
                }
            }
            None => println!(
                "  {line}:{column}  file offset {in_the_file} (`{at_the_file}`)  ->  ** not in the reading **"
            ),
        }
    }
}
