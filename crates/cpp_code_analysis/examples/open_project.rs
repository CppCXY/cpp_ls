//! Probe: open a project the way a language server does, and time the first answer.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example open_project
//! ```
//!
//! `examples/std_query.rs` measures the *queries*: an index is built all at once and every member access is asked
//! against it. This measures the **driver** — the layer that turns "the user opened a project" into answers:
//!
//! ```text
//! Session::open          discover the toolchain, read compile_commands.json, list the sources
//! did_open(main.cpp)     the buffer is the text, unsaved or not
//! advance(n)             n files, in the order a question about the open file needs them
//! view + a query         what a client's request becomes
//! ```
//!
//! # The number this probe exists for
//!
//! **How much has to be read before the first honest answer.** A language server that indexes a project before
//! answering anything is unusable on a large one, and a session answers `Unknown` until the file that holds the
//! declaration has been read — so "files read before `std::string` resolved, and how long that took" is the
//! product-relevant figure. It is printed as the lazy loop runs, one chunk at a time, which is also the shape a
//! server uses: a bounded amount of work per idle tick.
//!
//! The second half of the probe is the **buffer**: `widget.h` is on disk with one member and open in the editor
//! with another, and the members the analysis reports have to be the buffer's. That is the difference between an
//! analysis that works while a user types and one that works after they save, and it is a property of this layer
//! rather than of any single query.

use std::path::{Path, PathBuf};
use std::time::Instant;

use cpp_code_analysis::{
    DiskFiles, FileEvent, FileView, Known, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// What the file on disk says. The buffer the editor opens adds one function — an edit nobody has saved.
const MAIN_ON_DISK: &str = "\
#include \"widget.h\"\n\
#include <string>\n\
#include <vector>\n\
#include <map>\n\
void f(std::string s, std::string* p, std::string arr[4]) {\n\
  s.size();\n\
  s.substr(1);\n\
  s.empty();\n\
  std::vector<int> v;\n\
  v.push_back(1);\n\
  v.size();\n\
  std::map<int, int> m;\n\
  m.find(1);\n\
  m.begin();\n\
  (*p).size();\n\
  arr[0].empty();\n\
}\n";

/// What the buffer says: the file plus one function that uses the header's class.
const MAIN_IN_THE_BUFFER: &str = "\
#include \"widget.h\"\n\
#include <string>\n\
#include <vector>\n\
#include <map>\n\
void f(std::string s, std::string* p, std::string arr[4]) {\n\
  s.size();\n\
  s.substr(1);\n\
  s.empty();\n\
  std::vector<int> v;\n\
  v.push_back(1);\n\
  v.size();\n\
  std::map<int, int> m;\n\
  m.find(1);\n\
  m.begin();\n\
  (*p).size();\n\
  arr[0].empty();\n\
}\n\
int use(Widget w) { return 0; }\n";

/// The member accesses to ask about, in the order the text writes them. The same eleven as
/// `examples/std_query.rs`, asked through the driver instead of through the query functions: a cursor position in,
/// a completion list out.
const QUERIES: &[&str] = &[
    "s.size",
    "s.substr",
    "s.empty",
    "v.push_back",
    "v.size",
    "m.find",
    "m.begin",
    "(*p).size",
    "arr[0].empty",
];

fn main() {
    let root = std::env::temp_dir().join("cppls-open-project");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("the probe directory");

    let main = root.join("main.cpp");
    let widget = root.join("widget.h");
    std::fs::write(&main, MAIN_ON_DISK).expect("the fixture writes");
    std::fs::write(&widget, "struct Widget { int on_disk; };\n").expect("the fixture writes");
    write_compile_database(&root);

    let documents = OpenDocuments::new();
    let files = SessionFiles::new(documents.clone(), DiskFiles);

    // --- opening the project ----------------------------------------------------------------------
    let started = Instant::now();
    let mut session = Session::open(&root, &files, WatchFilter::new(&root));
    let opened = started.elapsed();

    println!("project: {}", root.display());
    match session.toolchain() {
        Some(toolchain) => println!(
            "  toolchain: {} ({}) — {} system include directories",
            toolchain.compiler.display(),
            toolchain.version.as_deref().unwrap_or("version not reported"),
            toolchain.system_include_paths.len()
        ),
        None => println!("  toolchain: none found — `<string>` will not resolve"),
    }
    match session.compile_database() {
        Some(database) => println!(
            "  compile database: {} entries ({} malformed), flags from the first",
            database.len(),
            database.malformed
        ),
        None => println!("  compile database: none"),
    }
    println!(
        "  opened in {opened:?} — {} project files queued ({}), {} defines",
        session.pending(),
        names(session.project_files()),
        session.config().defines.len()
    );

    // --- the user opens a file --------------------------------------------------------------------
    session.did_open(&main, MAIN_IN_THE_BUFFER);
    session.did_open(&widget, "struct Widget { int in_buffer; };\n");

    let view = session.view(&main).expect("the buffer is the text");
    assert!(view.open, "the view must have read the buffer");

    // The cursor a user creates by typing `s.` — the keystroke that *asks* the question, and the answer needs the
    // standard library's own headers. Placed after `s.size`'s `e`, which is the same request one keystroke later.
    let cursor = view
        .source
        .find("s.size")
        .map(|at| at + "s.size".len() - 1)
        .expect("the access is in the buffer");

    // --- how much has to be read before the first answer ------------------------------------------
    println!("\nlazy: `s.size`'s member resolved after…");
    let started = Instant::now();
    let mut read = 0usize;
    let mut first_answer = None;

    while !session.is_idle() {
        // One chunk is the shape a server uses — a bounded amount of work per idle tick — and the count below is
        // therefore an upper bound: the answer arrived somewhere inside the chunk that found it.
        read += session.advance(16).len();

        if let Known::Yes(found) = session.member_completions(&view, cursor)
            && let Some(member) = found
                .members
                .own()
                .find(|member| member.fact.name == "size")
        {
            first_answer = Some((read, member.file.clone()));
            break;
        }
    }

    match &first_answer {
        Some((files, file)) => println!(
            "  {files} files, {:?} — declared in {}",
            started.elapsed(),
            short(file)
        ),
        None => {
            // A failure here has two shapes and the probe has to tell them apart: the cursor's object has no type
            // the analysis can work out, or the class was found and has no such member. Both are printed, because
            // "unknown" from a member completion can mean either.
            println!(
                "  never (the whole project was read) — at the cursor: {}",
                match session.member_completions(&view, cursor) {
                    Known::Yes(found) => format!("the type is `{}`, with no `size`", found.class),
                    Known::Unknown(reason) => format!("unknown: {}", reason.describe()),
                    Known::No => "no".to_string(),
                }
            );
        }
    }

    // --- and the rest of it -----------------------------------------------------------------------
    let started = Instant::now();
    let everything = session.index_everything();
    let rest = started.elapsed();
    let stats = session.stats();

    println!(
        "\nall: {everything} files in {rest:?} — {} parsed, {} from disk, {} not stored (hit rate {})",
        stats.rebuilt,
        stats.reused,
        stats.unstored,
        match stats.hit_rate() {
            Some(rate) => format!("{:.0}%", rate * 100.0),
            None => "no lookups".to_string(),
        }
    );
    println!(
        "  index: {} files, {} declarations",
        session.index().len(),
        session
            .index()
            .summaries()
            .map(|summary| summary.declarations.len())
            .sum::<usize>()
    );

    // --- the next session: what the cache is for --------------------------------------------------
    //
    // The same project, opened again, with the summaries the first session wrote. This is the second run of the
    // day, a restart, or the next CI job — and the buffers are the same handles, which is what a client re-sending
    // its open documents looks like from here.
    let started = Instant::now();
    let mut warm = Session::open(&root, &files, WatchFilter::new(&root));
    let warm_opened = started.elapsed();

    let started = Instant::now();
    let mut warm_read = 0usize;
    let mut warm_answer = None;

    while !warm.is_idle() {
        warm_read += warm.advance(16).len();

        if let Known::Yes(found) = warm.member_completions(&view, cursor)
            && found.members.own().any(|member| member.fact.name == "size")
        {
            warm_answer = Some(warm_read);
            break;
        }
    }

    let warm_took = started.elapsed();
    let warm_parsed = warm.index_everything();
    let stats = warm.stats();

    println!(
        "\nwarm: opened in {warm_opened:?}, `s.size` after {} files in {warm_took:?}",
        match warm_answer {
            Some(files) => files.to_string(),
            None => "never".to_string(),
        }
    );
    println!(
        "  the rest of the project: {warm_parsed} files — {} parsed, {} from disk (hit rate {})",
        stats.rebuilt,
        stats.reused,
        match stats.hit_rate() {
            Some(rate) => format!("{:.0}%", rate * 100.0),
            None => "no lookups".to_string(),
        }
    );

    // --- the buffer is the text -------------------------------------------------------------------
    let view = session.view(&main).expect("the buffer is the text");
    println!("\nthe buffer, not the disk:");

    match session.definition(&view, view.source.find("Widget w").expect("the use is there") + 2) {
        Known::Yes(found) => println!(
            "  Widget            -> {} line {} (the file is on disk too)",
            short(&found.file),
            line_of(&view, found.fact.name_range)
        ),
        Known::Unknown(reason) => println!("  Widget            -> unknown: {}", reason.describe()),
        Known::No => println!("  Widget            -> no"),
    }

    let widget_view = session.view(&widget).expect("the buffer is the text");
    println!(
        "  Widget's members  -> {:?}   (the disk says `on_disk`)",
        member_names(&session, &widget_view, "Widget")
    );

    // Closing the document is the client saying "this path is the filesystem's again".
    session.did_close(&widget);
    session.index_everything();
    println!(
        "  after did_close   -> {:?}",
        member_names(&session, &view, "Widget")
    );

    // --- the queries ------------------------------------------------------------------------------
    println!("\nqueries through the driver (a cursor in, a member list out):");
    let mut answered = 0usize;

    for query in QUERIES {
        let Some(offset) = view.source.rfind(query).map(|at| at + query.len() - 1) else {
            continue;
        };

        match session.member_completions(&view, offset) {
            Known::Yes(found) => {
                let hit = found
                    .members
                    .own()
                    .find(|member| member.fact.name == query.rsplit('.').next().unwrap_or(query));
                match hit {
                    Some(member) => {
                        answered += 1;
                        println!(
                            "  {query:<14} -> {}  {}::{}",
                            short(&member.file),
                            member.fact.scope.as_deref().unwrap_or("<file scope>"),
                            member.fact.name
                        );
                    }
                    None => println!(
                        "  {query:<14} -> the type is `{}` and it has no such member",
                        found.class
                    ),
                }
            }
            Known::Unknown(reason) => println!("  {query:<14} -> unknown: {}", reason.describe()),
            Known::No => println!("  {query:<14} -> no"),
        }
    }

    // --- what the client reports changed ----------------------------------------------------------
    let response = session.changed([FileEvent::modified(&widget)]);
    println!(
        "\nthe client reports `widget.h` changed: {} to re-read, {} forgotten, everything = {}",
        response.reindex.len(),
        response.forgotten.len(),
        response.everything
    );
    session.advance(1);
    println!("  pending after one step: {}", session.pending());

    println!("\nanswered {answered}/{}", QUERIES.len());
}

/// A compile database that names the project's one translation unit and one file that is not here.
///
/// Two things are being shown: the flags are the project's own (`-DFROM_THE_DATABASE`, `-std=c++20`), and an entry
/// naming a file this checkout does not have is not queued — a checked-in database writes the absolute paths of the
/// machine that produced it.
fn write_compile_database(root: &Path) {
    let spelled = root.to_string_lossy().replace('\\', "/");
    let json = format!(
        "[\n  {{\"directory\": \"{spelled}\", \"file\": \"{spelled}/main.cpp\", \
         \"arguments\": [\"g++\", \"-DFROM_THE_DATABASE\", \"-std=c++20\", \"-c\", \"{spelled}/main.cpp\"]}},\n  \
         {{\"directory\": \"{spelled}\", \"file\": \"{spelled}/elsewhere.cpp\", \
         \"arguments\": [\"g++\", \"-c\", \"{spelled}/elsewhere.cpp\"]}}\n]\n"
    );

    std::fs::write(root.join("compile_commands.json"), json).expect("the database writes");
}

/// The members of a type as the session reports them, for an assertion a human can read.
fn member_names(session: &Session<'_, DiskFiles>, view: &FileView, class: &str) -> Vec<String> {
    match session.members_of(view, class) {
        Known::Yes(members) => members
            .own()
            .map(|member| member.fact.name.clone())
            .collect(),
        Known::Unknown(reason) => vec![format!("unknown: {}", reason.describe())],
        Known::No => vec!["none".to_string()],
    }
}

/// The file's name, which is what identifies a header in a one-line answer.
fn short(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// The 1-based line a range starts on, for a pointer a human can check against the file.
fn line_of(view: &FileView, range: cpp_parser::SourceRange) -> usize {
    view.source[..range.start_offset.min(view.source.len())]
        .lines()
        .count()
}

/// The project's files, as names rather than as paths.
fn names(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| short(path))
        .collect::<Vec<_>>()
        .join(", ")
}
