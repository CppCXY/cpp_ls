//! **What one file costs when a reader opens it, and what the answers are** — the probe for the editor reports on a
//! real project, without indexing the whole thing first.
//!
//! ```text
//! $env:CPPLS_PROJECT_ROOT = "E:\EmmyLuaCodeStyle"
//! $env:CPPLS_FILE = "…\LSPHandle.cpp"
//! $env:CPPLS_NEEDLES = "lineIndex->|opSyntaxTree|totalLine"
//! cargo test --release -p cpp_code_analysis --test one_file_probe -- --ignored --nocapture
//! ```
//!
//! Each phase is timed and the session's own account of its tables is printed, because the report that started this
//! was "hover is very slow and I do not know what it is waiting for": the answer has to separate *the pump* from *the
//! question*.

use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use std::path::PathBuf;
use std::time::Instant;

fn ms(took: std::time::Duration) -> f64 {
    took.as_secs_f64() * 1000.0
}

#[test]
#[ignore = "opens a real project; run with --ignored and CPPLS_PROJECT_ROOT/CPPLS_FILE"]
fn what_one_file_costs() {
    let root = PathBuf::from(
        std::env::var("CPPLS_PROJECT_ROOT").expect("CPPLS_PROJECT_ROOT names the project"),
    );
    let file = PathBuf::from(std::env::var("CPPLS_FILE").expect("CPPLS_FILE names a file"));
    let needles: Vec<String> = std::env::var("CPPLS_NEEDLES")
        .unwrap_or_else(|_| ".".to_string())
        .split('|')
        .map(str::to_string)
        .collect();
    let text = std::fs::read_to_string(&file).expect("the file is readable");

    let opened = Instant::now();
    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    println!(
        "Session::open                {:>9.1} ms",
        ms(opened.elapsed())
    );

    // **The project, the way the pump indexes it** — and *only* the index. `index_everything` also drains the cooking
    // queue, which renders every file's closure, and on this project that is 9.7 GB and no answer in twenty minutes.
    // A server cooks lazily; so does this.
    if std::env::var_os("CPPLS_INDEX_THE_PROJECT").is_some() {
        let indexing = Instant::now();
        let all = session.project_files().to_vec();
        session.add_project_files(all.clone());
        let mut slices = 0usize;
        while session.pending() > 0 && slices < 4000 {
            if session.advance(64).is_empty() {
                break;
            }
            slices += 1;
        }
        println!(
            "the project indexed          {:>9.1} ms   {} file(s), {} slice(s)",
            ms(indexing.elapsed()),
            all.len(),
            slices
        );
    }

    // **What the hover handler does, in order** — and nothing else. `view_of_the_file` is an *ask*: it queues the
    // cook and answers from what is there, which is the state a real hover sees on the first keystroke.
    let asked = Instant::now();
    session.did_open(&file, &text);
    println!(
        "did_open                     {:>9.1} ms",
        ms(asked.elapsed())
    );

    let catching = Instant::now();
    session.catch_up(&file);
    println!(
        "catch_up                     {:>9.1} ms",
        ms(catching.elapsed())
    );

    let cooking = Instant::now();
    let cooked = session.cook(&file);
    println!(
        "cook                         {:>9.1} ms   {}",
        ms(cooking.elapsed()),
        if cooked.is_some() {
            "cooked"
        } else {
            "nothing"
        }
    );

    let viewing = Instant::now();
    let view = session.view(&file);
    println!(
        "view                         {:>9.1} ms   {}",
        ms(viewing.elapsed()),
        if view.is_some() {
            "a reading"
        } else {
            "nothing"
        }
    );
    // **What that view was parsed from, and whether asking again parses it again.** A `view` is a `FileView`, whose
    // tree is built from the *rendering* — the whole closure, preprocessed — and nothing caches the tree.
    if let Some(rendered) = session.rendering_of(&file) {
        println!(
            "the rendering                {:>9.2} MB, {} token(s)",
            rendered.text.len() as f64 / (1024.0 * 1024.0),
            rendered.spans.len()
        );
    }
    let again = Instant::now();
    let _ = session.view(&file);
    println!("view, asked again            {:>9.1} ms", ms(again.elapsed()));

    let Some(view) = view else {
        println!("no view, so no question can be asked");
        return;
    };

    // **The questions, one per needle**, each timed — this is what a hover waits for.
    for needle in &needles {
        println!("\n--- {needle}");
        let mut from = 0;
        let mut seen = 0;
        while let Some(found) = text[from..].find(needle.as_str()) {
            let start = from + found;
            from = start + needle.len();
            seen += 1;
            if seen > 4 {
                break;
            }
            let line = text[..start].lines().count();
            // **The cursor sits inside the needle, in *file* coordinates, and is mapped into the reading** — a
            // rendering is not the file (comments and whitespace are gone), so a file offset is not a `ReadingOffset`.
            let at = &text[start..];
            let within = at
                .char_indices()
                .take_while(|(_, ch)| ch.is_alphanumeric() || *ch == '_' || *ch == '>')
                .count();
            let in_the_file = FileOffset::new(if within == 0 {
                start
            } else {
                start + within / 2
            });
            let snippet: String = text[start..]
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(70)
                .collect();
            let Some(offset) = view.reading_offset_of(in_the_file) else {
                println!("{line}: …{snippet}");
                println!("   the file offset {in_the_file:?} is not in the reading");
                continue;
            };
            println!("{line}: …{snippet}");

            let definitions = Instant::now();
            match session.definitions(&view, offset) {
                cpp_code_analysis::Known::Yes(found) => {
                    println!(
                        "   definitions {:>8.1} ms   {:?}",
                        ms(definitions.elapsed()),
                        found
                            .found
                            .iter()
                            .map(|one| format!(
                                "{} ({})",
                                one.fact.name,
                                one.file.file_name().unwrap_or_default().to_string_lossy()
                            ))
                            .collect::<Vec<_>>()
                    );
                }
                other => println!(
                    "   definitions {:>8.1} ms   {other:?}",
                    ms(definitions.elapsed())
                ),
            }

            let typed = Instant::now();
            println!(
                "   type_at     {:>8.1} ms   {:?}",
                ms(typed.elapsed()),
                session.type_at(&view, offset)
            );

            // The member list after the needle, when it ends with an operator.
            if needle.ends_with("->") || needle.ends_with('.') {
                let members = Instant::now();
                match session.member_completions(&view, in_the_file) {
                    cpp_code_analysis::Known::Yes(list) => println!(
                        "   members     {:>8.1} ms   class {:?}, {} member(s): {:?}",
                        ms(members.elapsed()),
                        list.class,
                        list.members.members.len(),
                        list.members
                            .members
                            .iter()
                            .map(|member| member.fact.name.as_str())
                            .take(10)
                            .collect::<Vec<_>>()
                    ),
                    other => println!(
                        "   members     {:>8.1} ms   {other:?}",
                        ms(members.elapsed())
                    ),
                }
            }
        }
    }

    // **What the index holds for a name**, and the member list of a class: the two questions every inference bug in
    // this probe ends in. `CPPLS_NAME=<name>` and `CPPLS_CLASS=<class>`.
    if let Ok(name) = std::env::var("CPPLS_NAME") {
        for summary in session.index().summaries() {
            for fact in &summary.declarations {
                if fact.name == name {
                    println!(
                        "fact {name}: {} kind {:?} scope {:?} returns {:?} type_of {:?} parameters {:?}",
                        summary.path.display(),
                        fact.kind,
                        fact.scope,
                        fact.returns,
                        fact.type_of,
                        fact.parameters
                    );
                }
            }
        }
    }
    if let Ok(class) = std::env::var("CPPLS_CLASS") {
        match session.members_of(&view, &class) {
            cpp_code_analysis::Known::Yes(list) => {
                println!("members of {class}: {}", list.members.len());
                for member in list.members.iter().take(12) {
                    println!(
                        "   {:?} {:?} scope {:?}",
                        member.fact.kind, member.fact.name, member.fact.scope
                    );
                }
            }
            other => println!("members of {class}: {other:?}"),
        }
    }

    let report = session.memory_report();
    println!(
        "\naccounted {:.1} MB\n{report}",
        report.total() as f64 / (1024.0 * 1024.0)
    );
}
