//! **Three reported usability defects on `containers.cpp`, asked the way the editor asks them.**
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "C:\Users\zc\Desktop\test_cpp"
//! cargo test --release -p cpp_ls --test usability_probe -- --ignored --nocapture
//! ```
//!
//! # What was reported
//!
//! ```text
//! 135  std::cout << Value{1}.describe() …   `Value` has no semantic token (TextMate draws it `variable.other.unknown`)
//! 132  list.remove(1);                     `remove` is drawn as a *macro*
//! 118  return std::to_string(value);       no hover card
//! ```
//!
//! Each is a different layer — the classifier, the classifier again, the hover — and the probe prints the answer of
//! both layers at the same offset so a report can be attributed instead of guessed at: `hover_text` is exactly what the
//! handler builds, and `classified_names` is exactly what the semantic-token handler maps onto its legend.

use std::path::PathBuf;
use std::time::Instant;

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use cpp_ls::handlers::hover::hover_text;

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\test_cpp".to_string()),
    )
}

#[test]
#[ignore = "a probe: opens a real project and prints instead of asserting"]
fn the_three_reported_places() {
    let root = project();
    let file = root.join("src/containers.cpp");
    let text = std::fs::read_to_string(&file).expect("the reported file is here");
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.index_everything();
    session.did_open(&file, &text);
    let started = Instant::now();
    let mut slices = 0usize;
    while session.pending_work() > 0 && slices < 4000 {
        session.advance(64);
        slices += 1;
    }
    println!(
        "pumped {slices} slice(s) in {:?}, {} work item(s) left\n",
        started.elapsed(),
        session.pending_work()
    );

    // **What is left, and of what kind.** The queue is two queues — files still to *read* and files to re-read for the
    // compiler's view — and the reports that follow are about names in headers, which only exists once the header has
    // been read through its macros. A count that never reaches zero is the reason a header stays at its raw reading.
    println!(
        "queue: {} to read, {} in all; units {:?}",
        session.pending(),
        session.pending_work(),
        session.unit_stats()
    );
    let variant = root.join(
        "C:/Users/zc/Desktop/mingw/mingw64/lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++/variant",
    );
    let _ = variant;
    for named in [
        "C:/Users/zc/Desktop/mingw/mingw64/lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++/variant",
        "C:/Users/zc/Desktop/mingw/mingw64/lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++/bits/stl_bvector.h",
    ] {
        let path = std::path::Path::new(named);
        println!(
            "{:<12} summary {}, cooked reading {}",
            std::path::Path::new(named)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            session.index().summary(path).is_some(),
            session.index().cooked_declarations(path).is_some()
        );
    }

    let view = session
        .view_of_the_file(&file)
        .expect("the reported file has a view");
    let names = session.classified_names(&view);

    // The places as the report wrote them: the needle, and how far into it the name is.
    let places: [(&str, usize, &str); 12] = [
        ("std::to_string(value)", 5, "hover was missing"),
        ("Value{1}.describe()", 0, "`Value` should be a type"),
        ("list.remove(1);", 5, "`remove` is drawn as a macro"),
        ("variant.index()", 8, "`variant` has no member `index`"),
        (
            "std::holds_alternative<double>(variant)",
            5,
            "no hover for `holds_alternative`",
        ),
        ("left.begin()", 5, "should be the `const` `begin` only"),
        (
            "vector.begin()",
            7,
            "four overloads, two of them duplicates",
        ),
        (
            "std::size_t operator()",
            11,
            "the `operator` keyword, not the function",
        ),
        (
            "std::hash<std::string>{}",
            5,
            "for comparison: a type used as a constructor",
        ),
        (
            "list.sort();",
            5,
            "for comparison: a member that is drawn right",
        ),
        ("struct Value", 7, "for comparison: the declaration"),
        ("Value{std::string{\"three\"}}", 0, "the second `Value`"),
    ];

    for (needle, shift, _why) in places {
        let Some(at) = text.find(needle).map(|at| at + shift) else {
            println!("{needle:?} is not in the file");
            continue;
        };
        let word = text[at..]
            .split(|character: char| !(character.is_alphanumeric() || character == '_'))
            .next()
            .unwrap_or_default();
        let classified = names
            .iter()
            .find(|name| {
                let range = name.range;
                (range.start_offset..range.start_offset + range.length).contains(&at)
            })
            .map(|name| format!("{:?} ({:?})", name.kind, name.provenance))
            .unwrap_or_else(|| "no classified name at this offset".to_string());
        let hover = hover_text(&session, &view, ReadingOffset::new(at), FileOffset::new(at))
            .map(|card| {
                let whole = card.replace('\n', "⏎");
                if whole.chars().count() > 260 {
                    format!("{}…", whole.chars().take(260).collect::<String>())
                } else {
                    whole
                }
            })
            .unwrap_or_else(|| "NONE".to_string());
        println!("{at:>5} {word:<14} semantic: {classified:<44}\n      hover: {hover}");
    }

    // **What the scope tree says the cursor is on**, for the two `operator` tokens the report named — the keyword card
    // and the function card differ by nothing a reader can see, so the layer that decides is printed.
    for needle in ["bool operator()", "std::size_t operator()"] {
        if let Some(at) = text
            .find(needle)
            .and_then(|at| text[at..].find("operator").map(|inside| at + inside))
        {
            println!(
                "{needle:?} at {at}: declared name {:?}, word {:?}",
                session.the_name_declared_at(&view, at),
                &text[at..(at + 10).min(text.len())]
            );
        }
    }

    // **The window around `Value{1}.describe()`**, so the row above can be read against what the file says there: a
    // name missing from the classifier is missing from this list, not mis-kinded in it.
    println!("\nclassified names in 4300..1445:");
    for name in names
        .iter()
        .filter(|name| (4300..4450).contains(&name.range.start_offset))
    {
        println!(
            "  {:>5} +{:<3} {:?} ({:?}) declaration {}",
            name.range.start_offset,
            name.range.length,
            name.kind,
            name.provenance,
            name.declaration
        );
    }
    println!("\nthe text there: {:?}", &text[4320..4440.min(text.len())]);
}
