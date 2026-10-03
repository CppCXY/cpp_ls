//! **A view of one file, and nothing else** — the question `editor_probe` answers in thirty-six seconds, asked in
//! about one.
//!
//! # Why this exists beside `editor_probe`
//!
//! `editor_probe` is a **census**: it indexes a project, cooks the whole closure, walks every file's type names,
//! times a view five ways and prints a dozen sections. That is the right tool for asking "what does the analysis
//! know", and it is measured at **33.7 seconds** on a closure of 151 files — of which the walk is a `session.view`
//! per file, each one building a macro environment at 103.7 ms. A loop over twelve headers is therefore seven
//! minutes, which is not a loop anybody iterates in.
//!
//! The cost is not in what a *view* needs. It is in the census around it. So this probe asks the one question and
//! stops:
//!
//! ```text
//! how many scopes does this file's own reading produce, how many of them are namespaces,
//! and how many parse errors did that reading report
//! ```
//!
//! which is the measurement every parser change in this crate has been judged by.
//!
//! # Usage
//!
//! ```text
//! view_probe <project-root> <file> [--cheap]
//! ```
//!
//! `--cheap` skips [`Session::index_everything`] and the cook, so the answer is what a session knows after opening
//! one file. Without it the closure is indexed **once** and the file's own macros are built — still a fraction of
//! the census, because nothing is walked but the file named.

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next() else {
        eprintln!("usage: view_probe <project-root> <file> [--cheap]");
        std::process::exit(2);
    };
    let Some(file) = args.next() else {
        eprintln!("usage: view_probe <project-root> <file> [--cheap]");
        std::process::exit(2);
    };
    let cheap = args.any(|arg| arg == "--cheap");

    let started = std::time::Instant::now();

    // **`Session::open` rather than `with_config`**, because the closure is the whole point: opening the project is
    // what discovers the toolchain, and the toolchain is what says where `<yvals_core.h>` is. A session built with
    // a bare configuration resolves nothing and answers in 22 ms — measured, and the number is how this was caught.
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let path = std::path::PathBuf::from(&file);

    // **As a buffer**, like the editor has it: the session reads the file once through its own provider chain and
    // then holds the text, so `session.view` costs a parse rather than a read.
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    session.did_open(&path, &text);

    if !cheap {
        session.index_everything();
    }

    // **The view, which is the question.** `session.view` answers from the plain reading the first time and asks
    // the work loop for the macros, so one `advance` is what turns it into the reading with the closure in hand —
    // and that is exactly the two numbers this probe exists to compare.
    let count = |view: &cpp_code_analysis::FileView| -> (usize, usize) {
        fn walk(
            table: &cpp_code_analysis::ScopeTree,
            id: cpp_code_analysis::ScopeId,
            named: bool,
            out: &mut (usize, usize),
        ) {
            let Some(scope) = table.scope(id) else {
                return;
            };
            let named = named || scope.kind == cpp_code_analysis::ScopeKind::Namespace;
            for _ in &scope.bindings {
                out.0 += 1;
                if named {
                    out.1 += 1;
                }
            }
            for child in &scope.children {
                walk(table, *child, named, out);
            }
        }

        let mut out = (0, 0);
        if let Some(root) = view.scopes.root() {
            walk(&view.scopes, root, false, &mut out);
        }
        out
    };
    let views = std::time::Instant::now();
    let plain = session.view(&path);
    let after_the_plain_view = views.elapsed();

    let plain_count = plain.as_ref().map(count);
    let plain_errors = plain.as_ref().map(|view| view.errors().len());

    session.advance(8);
    let with_macros = session.view(&path);
    let both = views.elapsed();

    let with_macros_count = with_macros.as_ref().map(count);
    let with_macros_errors = with_macros.as_ref().map(|view| view.errors().len());


    // **The errors themselves, not just how many.** A count says a reading stopped; the line and the token say
    // what it stopped on, and that is the whole of the difference between a diagnosis and a number.
    for view in [plain.as_ref(), with_macros.as_ref()].into_iter().flatten() {
        for error in view.errors().iter().take(4) {
            let at = usize::from(error.range.start());
            let line = view.source[..at.min(view.source.len())]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            // **The column as well as the line**, because a line of a header is ninety characters of template
            // arguments and "line 500" is not a location. The text around the token is printed with it: the
            // question a reader has is which `>` of the eight on that line the parser objected to.
            let line_start = view.source[..at.min(view.source.len())].rfind('\n').map_or(0, |nl| nl + 1);
            let column = at - line_start + 1;
            let from = line_start.max(at.saturating_sub(34));
            let to = (at + 34).min(view.source.len());
            println!(
                "   line {line}, column {column}: {} — at `{}`",
                error.message,
                view.source[from..to].replace('\n', " ")
            );
        }
    }

    let show = |label: &str, seen: Option<(usize, usize)>, errors: Option<usize>| match (seen, errors) {
        (Some((all, named)), Some(errors)) => {
            println!("{label:<12} {all:6} bindings, {named:6} inside a namespace, {errors:4} parse errors")
        }
        _ => println!("{label:<12} (not held)"),
    };

    // **What the editor is told**, which is the half the reader actually sees. A parse error that stays in the tree
    // is invisible: the report that started this probe was "you did not report the error, so I did not know", from a
    // file whose view held two bindings and no diagnostic at all. So the diagnostics are printed beside the
    // bindings, from the same session and the same file.
    if let Some(diagnostics) = session.diagnostics(&path) {
        println!(
            "   diagnostics: {:?} | {} error(s), {} note(s), {} check(s)",
            diagnostics.reading,
            diagnostics.errors.len(),
            diagnostics.notes.len(),
            diagnostics.checks.len()
        );
        for error in diagnostics.errors.iter().take(4) {
            let at = error.start;
            let line = text[..at.min(text.len())]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            println!("      line {line}: {}", error.message);
        }
    }
    println!("{file}");
    show("plain", plain_count, plain_errors);
    show("with macros", with_macros_count, with_macros_errors);
    println!(
        "the view took {} ms plain and {} ms with the macros; {} ms in all{}{}",
        after_the_plain_view.as_millis(),
        (both - after_the_plain_view).as_millis(),
        started.elapsed().as_millis(),
        if cheap { ", --cheap" } else { "" },
        // **Printed because it is the thing this probe is a reaction to.** A census that takes 33.7 s cannot be
        // iterated in, and a number nobody sees is a number nobody fixes.
        if started.elapsed().as_millis() > 5_000 {
            "  ← over five seconds: the closure, not the view"
        } else {
            ""
        }
    );
}
