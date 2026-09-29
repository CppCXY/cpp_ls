//! **What the editor's three requests actually get** on a real project: completion, hover and the signature.
//!
//! ```text
//! cargo run --release --example editor_probe -- <dir> [<file.cpp>]
//! ```
//!
//! The file is opened **as a buffer** with a few lines appended — no file on disk is touched — so the probe asks
//! about `#include <format>`, an `auto` variable and a `std::format(...)` call in the very project whose answers
//! were complained about:
//!
//! * `std::` — does the list contain `format`, `string`, `cout`? How many names does it offer at all?
//! * `std::string::` — a class template reached through an alias: a reader looking for `size()` types this every day.
//! * hover on an `auto` variable, and on a member whose type is written with the library's own macros.
//! * the signature of a call to a function declared in a header.
//!
//! Every answer is printed **with what the index knows beside it** (`declarations_in("std")`, is the header in the
//! closure, is `std::format` a declaration), because "completion is empty" and "the declaration is not there" are
//! different defects with the same symptom.

use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};
use std::path::PathBuf;

/// The lines appended to the file being probed: one question each.
const QUESTIONS: &str = r#"
void probe_auto() {
    std::string x;
    auto y = x.back();
    std::string::size_type n = 0;
    std::format("{}", n);
    std::string::iterator it;
}

void probe_member() {
    std::string s;
    s.append("x");
}
"#;

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("a directory"));
    let file = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("main.cpp"));

    let on_disk = std::fs::read_to_string(&file).expect("the file the probe was pointed at");
    // **The include the user's complaint is about**, added to the *buffer* rather than to their file. Prepended, so
    // the rest of the file reads exactly as it does on disk.
    let text = format!("#include <format>\n{on_disk}{QUESTIONS}");

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    // **As a buffer**, which is what the editor has: the probe must not edit the user's file to ask its questions.
    session.did_open(&file, &text);
    session.index_everything();

    let Some(view) = session.view(&file) else {
        println!("{} is not held", file.display());
        return;
    };

    // ---------------------------------------------------------------------------------------------
    // Is the header even in the program? `#include <format>` is the user's own line, and everything below is
    // moot if the closure never reached it — the two failures look identical from the completion list.
    // ---------------------------------------------------------------------------------------------
    let formats: Vec<PathBuf> = session
        .index()
        .summaries()
        .map(|summary| summary.path.clone())
        .filter(|path| path.to_string_lossy().contains("format"))
        .collect();
    println!("--- the program ---");
    // **What configuration the answers below are answers about**, before any of them: the language standard in
    // force, and whether the analysis chose it or the project stated it. Every `#if` in every header is a question
    // about this, and a project with no build configuration at all has nothing else to say it with.
    println!(
        "standard = {:?} | toolchain {:?} | note {:?}",
        session.config().standard.as_deref(),
        session.toolchain().and_then(|found| found.standard.as_deref()),
        session.toolchain().and_then(|found| found.note.as_deref()),
    );
    println!(
        "declarations_in(\"std\") = {} | files whose path mentions `format`: {}",
        session.index().declarations_in("std", &file).len(),
        formats.len()
    );
    // **Why a name that is in a file in the closure is not in the index**: a header reached through `#include` has a
    // *raw* summary the moment it is parsed, and a *cooked* one only when something read it the way a compiler does —
    // and `std::format` is only `std::` after `_STD_BEGIN` (a macro in `yvals_core.h`) has been expanded.
    for path in &formats {
        let raw = session
            .index()
            .summary(path)
            .map(|summary| {
                summary
                    .declarations
                    .iter()
                    .filter(|fact| fact.name.contains("format"))
                    .map(|fact| format!("{}::{}", fact.scope.clone().unwrap_or_default(), fact.name))
                    .take(6)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let cooked = session
            .index()
            .cooked_declarations(path)
            .map(|facts| {
                facts
                    .iter()
                    .filter(|fact| fact.name.contains("format"))
                    .map(|fact| format!("{}::{}", fact.scope.clone().unwrap_or_default(), fact.name))
                    .take(6)
                    .collect::<Vec<_>>()
            });
        println!(
            "   {} | raw {} declarations, `format`-ish {:?} | cooked {:?}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            session.index().summary(path).map(|summary| summary.declarations.len()).unwrap_or(0),
            raw,
            cooked.as_ref().map(|facts| facts.len())
        );
        if let Some(facts) = cooked {
            println!("      cooked names: {facts:?}");
        }
    }
    for name in ["std::format", "std::vformat", "std::string", "std::cout"] {
        println!("   definition({name:?}) = {:?}", session.index().definition(name, &file));
    }

    // ---------------------------------------------------------------------------------------------
    // Completion after `std::`, and after `std::string::`.
    // ---------------------------------------------------------------------------------------------
    println!("\n--- completion ---");
    let after = |needle: &str, skip: usize| -> Option<usize> {
        text.find(needle).map(|at| at + skip)
    };

    for (what, needle, skip) in [
        ("std::", "std::format", "std::".len()),
        ("std::string::", "std::string::iterator", "std::string::".len()),
    ] {
        let Some(offset) = after(needle, skip) else {
            println!("{what:<14} (the probe's own line is not in the file)");
            continue;
        };
        let found = session.completions(&view, offset);
        let labels: Vec<&str> = found.items.iter().map(|item| item.label.as_str()).collect();
        println!(
            "{what:<14} scope {:?} | {} item(s), truncated {}",
            found.scope,
            found.items.len(),
            found.truncated
        );
        let wanted: Vec<&str> = match what {
            "std::" => vec!["format", "vformat", "string", "cout", "getline", "basic_string", "size_t"],
            _ => vec!["size", "length", "data", "begin", "push_back", "iterator"],
        };
        for name in wanted {
            match found.items.iter().find(|item| item.label == name) {
                Some(item) => println!("      {name:<14} offered | detail {:?}", item.detail),
                None => println!(
                    "      {name:<14} **NOT OFFERED** (and the index {} it)",
                    match session.index().definition(&format!("std::{name}"), &file) {
                        Known::Yes(_) => "has",
                        _ => "has not",
                    }
                ),
            }
        }
        let sample: Vec<&str> = labels.iter().take(8).copied().collect();
        println!("      first: {sample:?}");
    }

    // ---------------------------------------------------------------------------------------------
    // Hover: the type at the cursor. `auto y = x.back()` is the case the user named, and `back` itself is the case
    // where the answer is a *spelling* — what the library wrote, macros and all.
    // ---------------------------------------------------------------------------------------------
    println!("\n--- hover (type_at) ---");
    for (what, needle, skip) in [
        ("the `y` of `auto y`", "auto y", "auto ".len()),
        ("the `x` of `x.back()`", "auto y = x.", "auto y = ".len()),
        ("`back` in the call", "auto y = x.back", "auto y = x.".len()),
        ("`size_type` in `std::string::size_type`", "std::string::size_type", "std::string::".len()),
    ] {
        let Some(offset) = after(needle, skip) else { continue };
        let answer = session.type_at(&view, offset);
        println!("{what:<40} {answer:?}");
    }

    // ---------------------------------------------------------------------------------------------
    // **The same two questions after the declaring header has been read the way a compiler reads it.**
    //
    // `auto y = x.back()` answers `_NODISCARD _CONSTEXPR20 reference` today, and that spelling can be wrong in two
    // different ways with two different fixes: the library's own macros standing where specifiers go (a *reading*
    // problem — the cooked rendering has them expanded), or a member alias (`reference` is `typedef _Ty& reference`)
    // that the substitution step did not follow (a *type* problem). Cooking the header separates them: if the answer
    // becomes `char&`, the fix is to have the cooked reading; if it stays `reference`, the alias step is what needs
    // the work.
    // ---------------------------------------------------------------------------------------------
    if let Some(back) = after("auto y = x.back", "auto y = x.".len())
        && let Known::Yes(declaring) = session.type_at(&view, back)
    {
        println!("\n--- after cooking {} ---", declaring.file.display());
        session.want_cooked_reading(&declaring.file);
        let mut rounds = 0usize;
        while (session.pending() > 0 || session.pending_cooking() > 0) && rounds < 10_000 {
            session.advance(64);
            rounds += 1;
        }

        if let Some(y) = after("auto y", "auto ".len()) {
            println!("the `y` of `auto y`   {:?}", session.type_at(&view, y));
        }
        println!("`back` in the call    {:?}", session.type_at(&view, back));

        // **What the two readings of that header say about `back`**, because "the cook did not change the answer"
        // has two causes: the cooked reading does not exist (the cook did not run), or it exists and spells the
        // return type the same way. Only the second one means the macros are not the problem.
        let raw = session
            .index()
            .summary(&declaring.file)
            .map(|summary| summary.declarations.iter().filter(|fact| fact.name == "back").count());
        let cooked = session
            .index()
            .cooked_declarations(&declaring.file)
            .map(|facts| facts.iter().filter(|fact| fact.name == "back").count());
        println!("declarations named `back`: raw {raw:?}, cooked {cooked:?}");
        if let Some(facts) = session.index().cooked_declarations(&declaring.file)
            && let Some(fact) = facts.iter().find(|fact| fact.name == "back")
        {
            println!("the cooked fact: returns {:?} type_of {:?}", fact.returns, fact.type_of);
        }
        if let Some(summary) = session.index().summary(&declaring.file)
            && let Some(fact) = summary.declarations.iter().find(|fact| fact.name == "back")
        {
            println!("the raw fact:    returns {:?} type_of {:?}", fact.returns, fact.type_of);
        }
    }

    // ---------------------------------------------------------------------------------------------
    // **What a hover on an overloaded name has to work with**: the name question answers `Ambiguous`, and the
    // plural question is what the popup is built from — one line per declaration, from the facts alone.
    // ---------------------------------------------------------------------------------------------
    println!("\n--- hover on an overloaded name ---");
    if let Some(offset) = after("std::format(\"{}\"", "std::".len()) {
        println!(
            "definition(std::format) = {:?}",
            session.definition(&view, offset).value().map(|found| found.fact.qualified_name())
        );
        match session.definitions(&view, offset) {
            Known::Yes(found) => {
                println!("definitions: {} declaration(s)", found.found.len());
                for declaration in &found.found {
                    println!(
                        "   {} {}{}",
                        declaration.fact.returns.clone().unwrap_or_default(),
                        declaration.fact.name,
                        declaration.fact.parameter_list.clone().unwrap_or_else(|| "(…)".to_string()),
                    );
                }
            }
            other => println!("definitions: {other:?}"),
        }
    }

    // ---------------------------------------------------------------------------------------------
    // The signature: what a reader gets while typing the arguments of a call to a header's function. **All** of
    // them, because an overload set is a list in the protocol and one signature per declaration here — a popup with
    // one of four overloads, or with none, is what a reader complained about.
    // ---------------------------------------------------------------------------------------------
    println!("\n--- signature ---");
    if let Some(offset) = after("std::format(\"{}\"", "std::format(".len() + 1) {
        let signatures = session.signatures_at(&view, offset);
        if signatures.is_empty() {
            println!("**no signature** for a call inside `std::format(...)`");
        }
        for signature in &signatures {
            println!("label: {}", signature.label);
            println!(
                "  parameters: {:?} | active {:?} | declared in {}",
                signature.parameters.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>(),
                signature.active_parameter,
                signature.declared_in.display()
            );
        }
    }

    // …and the same question about a **member** call: `s.append(` is where an overload set is the difference between
    // a popup that lists what the reader can write and one that shows whichever overload happened to be picked.
    if let Some(offset) = after("s.append(\"x\"", "s.append(".len() + 1) {
        let signatures = session.signatures_at(&view, offset);
        println!("s.append( → {} signature(s)", signatures.len());
        for signature in signatures.iter().take(12) {
            println!("   {}", signature.label);
        }
    }
}
