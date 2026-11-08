//! **A probe, not a test**: two wrong answers a reader reported on the CodeStyle project.
//!
//! ```text
//! $env:CPPLS_CODESTYLE_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_code_analysis --test codestyle_probe -- --ignored --nocapture
//! ```
//!
//! 1. `CodeFormatCLib/src/CodeFormat.cpp`: `LuaParser p(file, std::move(luaLexer.GetTokens()));` then
//!    `p.HasError()` — the editor says `p` "is a LuaParser(file, std::move)" and that the class is out of the index.
//!    If the reading is right, the fact for `p` has `type_of: "LuaParser"` and a `member` lookup finds `HasError`.
//! 2. `CodeFormatCLib/src/CodeFormatCLib.cpp`: `for (auto &diagnostic : diagnostic_results)` — the editor cannot give
//!    `diagnostic` a type, so `diagnostic.Start` resolves nowhere. The question is what the fact for
//!    `diagnostic_results` says, and whether `auto` in a range-for can be deduced from it.
//!
//! A probe because neither answer is a unit test's business: both need the project's real include closure, which is
//! minutes of reading on a fixture and seconds here.

use std::path::PathBuf;
use std::time::Instant;

use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_CODESTYLE_PROJECT")
            .unwrap_or_else(|_| r"E:\EmmyLuaCodeStyle".to_owned()),
    )
}

/// Every declaration the reading holds for `name` in `file`: `(kind, scope, type_of, returns, parameters, local)`.
fn facts_for(session: &Session<DiskFiles>, file: &std::path::Path, name: &str) -> Vec<String> {
    let Some(summary) = session.index().summary(file) else {
        return vec!["<no summary>".to_owned()];
    };
    summary
        .declarations
        .iter()
        .filter(|fact| fact.name == name)
        .map(|fact| {
            format!(
                "{:?} scope={:?} type_of={:?} returns={:?} params={:?} local={} at {}",
                fact.kind,
                fact.scope,
                fact.type_of,
                fact.returns,
                fact.parameters,
                fact.local,
                fact.range.start_offset
            )
        })
        .collect()
}

#[test]
#[ignore = "a probe, not a test: it reads the CodeStyle project's real closure"]
fn two_wrong_answers_on_the_codestyle_project() {
    let root = project();
    let main = root
        .join("CodeFormatCLib")
        .join("src")
        .join("CodeFormat.cpp");
    let c_lib = root
        .join("CodeFormatCLib")
        .join("src")
        .join("CodeFormatCLib.cpp");
    assert!(
        main.is_file(),
        "the project's CodeFormat.cpp: {}",
        main.display()
    );
    assert!(
        c_lib.is_file(),
        "the project's CodeFormatCLib.cpp: {}",
        c_lib.display()
    );

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.index_everything();
    println!("indexed in one pass");

    // **The two files as a reader has them**: open, which is the reading a hover asks.
    for path in [&main, &c_lib] {
        let text = std::fs::read_to_string(path).expect("the file reads");
        session.did_open(path, &text);
    }
    session.index_everything();

    println!("\n== 1. `LuaParser p(file, std::move(…));` ==");
    for fact in facts_for(&session, &main, "p") {
        println!("  p: {fact}");
    }
    println!(
        "  definition(\"LuaParser\") = {:?}",
        session.index().definition("LuaParser", &main)
    );
    if let Known::Yes(all) = session.index().definitions("LuaParser", &main) {
        for one in &all.found {
            println!(
                "  candidate: file {} kind {:?} scope {:?}",
                one.file.file_name().unwrap_or_default().to_string_lossy(),
                one.fact.kind,
                one.fact.scope
            );
        }
    }
    println!(
        "  definitions(\"HasError\") = {:?}",
        session.index().definitions("HasError", &main)
    );
    if let Some(view) = session.view_of_the_file(&main)
        && let Known::Yes(members) = session.members_of(&view, "LuaParser")
    {
        let has = members
            .members
            .iter()
            .any(|member| member.fact.name == "HasError");
        println!(
            "  members_of(LuaParser): {} member(s), HasError among them: {has}",
            members.members.len()
        );
    }

    println!("\n== 2. `for (auto &diagnostic : diagnostic_results)` ==");
    for fact in facts_for(&session, &c_lib, "diagnostic_results") {
        println!("  diagnostic_results: {fact}");
    }
    for fact in facts_for(&session, &c_lib, "diagnostic") {
        println!("  diagnostic: {fact}");
    }
    println!(
        "  definition(\"diagnostic_results\") = {:?}",
        session.index().definition("diagnostic_results", &c_lib)
    );

    // **What the *expression engine* says about the object**, which is what the hover prints. The fact said
    // `LuaParser`; if the engine says `LuaParser(file, std::move)` then the fact and the engine disagree, and the
    // engine is reading the declarator's spelling as if `p(file, …)` were a function declaration.
    if let Some(view) = session.view_of_the_file(&main) {
        let text = std::fs::read_to_string(&main).expect("the file reads");
        let at = text.find("p.HasError").expect("the use is written");
        println!(
            "  type_at(the `p` of `p.HasError`) = {:?}",
            session.type_at(
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(at)
            )
        );
    }
    let c_lib_text = std::fs::read_to_string(&c_lib).expect("the file reads");
    if let Some(view) = session.view_of_the_file(&c_lib) {
        let text = c_lib_text.as_str();
        for wanted in [
            "result.Data",
            "std::move(result.Data)",
            "codeFormat.Diagnostic(uri, code)",
            "diagnostic_results",
        ] {
            if let Some(at) = text.find(wanted) {
                println!(
                    "  type_at({wanted:?}) = {:?}",
                    session.type_at(
                        &view,
                        cpp_code_analysis::file::coordinates::ReadingOffset::new(at)
                    )
                );
            }
        }
        // **The member expression itself**, whose type the `std::move` call needs as its argument type: the cursor
        // sits on `Data`, which is the member-access node's own name. `type_at("result.Data")` above lands on the
        // object, because the offset is the start of the expression and `type_at` answers the *innermost* node there.
        if let Some(from) = text.find("std::move(result.Data)") {
            let at = from + "std::move(result.".len();
            println!(
                "  type_at(the `Data` of `result.Data`) = {:?}",
                session.type_at(
                    &view,
                    cpp_code_analysis::file::coordinates::ReadingOffset::new(at)
                )
            );
        }
        // **The candidates the call has to choose between**, with the three fields the choice reads. `std::move` is
        // an overload set, so this is also the evidence for what a deduction step would be handed.
        if let Known::Yes(all) = session.index().definitions("std::move", &c_lib) {
            for one in all.found.iter().take(6) {
                println!(
                    "  std::move candidate: params {:?} list {:?} returns {:?}",
                    one.fact.parameters, one.fact.parameter_list, one.fact.returns
                );
            }
        }
        let text = std::fs::read_to_string(&c_lib).expect("the file reads");
        let at = text
            .find("diagnostic.Message")
            .expect("the member use is written");
        println!(
            "  type_at(the `diagnostic` of `diagnostic.Message`) = {:?}",
            session.type_at(
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(at)
            )
        );
    }
}

#[test]
#[ignore = "a probe, not a test: it measures what a keystroke costs on the project's real closure"]
fn what_a_keystroke_costs_on_the_real_closure() {
    // **The measurement `docs/buglist.md` #2 needs, in the one setup that resolves the closure.** The other probes
    // disagree with each other about what a file's rendering is (`one_file_probe` prints 1 643 tokens for a file that
    // includes `<fstream>`), because whether the include closure resolves depends on the session having walked the
    // project — so this uses the setup above, which does resolve it, and prints the size of the stream it measures.
    let root = project();
    // **The file is the caller's**, because the report is about one file: `LSPHandle.cpp` is where the reader sees
    // hover slow and completion empty, and it is not the file this probe started on.
    let file = std::env::var("CPPLS_FILE")
        .map(PathBuf::from)
        .map(|file| {
            if file.is_absolute() {
                file
            } else {
                root.join(file)
            }
        })
        .unwrap_or_else(|_| {
            root.join("CodeFormatCLib")
                .join("src")
                .join("CodeFormat.cpp")
        });
    let text = std::fs::read_to_string(&file).expect("the file reads");

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let indexing = Instant::now();
    session.index_everything();
    session.did_open(&file, &text);
    session.index_everything();
    session.catch_up(&file);
    println!("the project read in {:?}", indexing.elapsed());

    if let Some(rendered) = session.rendering_of(&file) {
        println!(
            "the rendering            {:.2} MB, {} token(s)",
            rendered.text.len() as f64 / (1024.0 * 1024.0),
            rendered.spans.len(),
        );
    } else {
        println!(
            "the rendering            none — the closure did not resolve, so nothing below is measured"
        );
        return;
    }

    let started = Instant::now();
    let view = session.view_of_the_file(&file);
    println!(
        "the first view           {:?} ({})",
        started.elapsed(),
        if view.is_some() {
            "a reading"
        } else {
            "nothing"
        }
    );

    for round in 0..5 {
        let mut edited = text.clone();
        edited.push_str(&format!("// typed {round}\n"));
        session.did_change(&file, &edited);

        let started = Instant::now();
        let steps = session.advance(1);
        let pump = started.elapsed();
        let read_as_the_program = steps
            .iter()
            .find(|step| step.path == file)
            .is_some_and(|step| step.with_the_environment);

        let started = Instant::now();
        session.catch_up(&file);
        let catch = started.elapsed();

        let started = Instant::now();
        let view = session.view_of_the_file(&file);
        let viewed = started.elapsed();

        // **The screenshot's question**: the diagnostics a client pulls right after a keystroke.
        let started = Instant::now();
        let reported = session.diagnostics(&file).map(|found| found.errors.len());
        let diagnosed = started.elapsed();

        println!(
            "body keystroke {round}       pump {pump:>9.1?} | catch_up {catch:>9.1?} | view {viewed:>9.1?} ({}) \
             | diagnostics {diagnosed:>9.1?} ({reported:?}) | read as the program {read_as_the_program}",
            if view.is_some() {
                "a reading"
            } else {
                "nothing"
            }
        );
    }
}

#[test]
#[ignore = "a probe, not a test: it prints the parse tree of one declaration"]
fn the_tree_of_a_constructor_call_declaration() {
    // **The shape the engine misreads**, on its own: `LuaParser p(file, std::move(x));` — is the parameter list part
    // of the declarator, or a sibling of the declaration?
    let source = "void f() {\n  LuaParser p(file, std::move(x));\n  p.HasError();\n}\n";
    let tree = cpp_parser::CppParser::parse(source, cpp_parser::ParserConfig::default());
    fn walk(node: &cpp_parser::CppSyntaxNode, depth: usize) {
        let text = node.text().to_string();
        let text = text.trim().replace('\n', " ");
        let shown: String = text.chars().take(40).collect();
        println!(
            "{:indent$}{:?} {:?}",
            "",
            cpp_parser::CppSyntaxKind::from(node.kind()),
            shown,
            indent = depth * 2
        );
        for child in node.children() {
            walk(&child, depth + 1);
        }
    }
    walk(&tree.get_red_root(), 0);
}
/// **What the analysis says about `LSPHandle` in `LSPHandle.cpp`** — the reader's report is that the name is read as
/// the constructor rather than the class, so this prints the answers that say which: the declaration it picks, every
/// declaration the name has, and the type of the qualifier at a use.
#[test]
#[ignore = "a probe, not a test: it prints the analysis's answer about a name in the real project"]
fn what_the_analysis_says_about_lsp_handle() {
    let root = project();
    let file = root
        .join("CodeFormatServer")
        .join("src")
        .join("LSP")
        .join("LSPHandle.cpp");
    let text = std::fs::read_to_string(&file).expect("the file reads");

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.index_everything();
    session.did_open(&file, &text);
    session.index_everything();
    session.catch_up(&file);

    println!(
        "definition(LSPHandle) = {:?}",
        session.index().definition("LSPHandle", &file)
    );
    println!("-- every fact named `LSPHandle`, wherever it is:");
    for (where_, one) in session.index().every_fact_named("LSPHandle") {
        println!(
            "  {:?} scope {:?} type_of {:?} in {} at {}",
            one.kind,
            one.scope,
            one.type_of,
            where_.file_name().unwrap_or_default().to_string_lossy(),
            one.range.start_offset
        );
    }
    if let Known::Yes(all) = session.index().definitions("LSPHandle", &file) {
        for one in &all.found {
            println!(
                "  candidate: {:?} scope {:?} type_of {:?} returns {:?} parameters {:?} in {}",
                one.fact.kind,
                one.fact.scope,
                one.fact.type_of,
                one.fact.returns,
                one.fact.parameters,
                one.file.file_name().unwrap_or_default().to_string_lossy()
            );
        }
    }

    let Some(view) = session.view_of_the_file(&file) else {
        println!("no view of LSPHandle.cpp");
        return;
    };
    let mut from = 0;
    let mut seen = 0;
    while let Some(found) = text[from..].find("LSPHandle::") {
        let start = from + found;
        from = start + "LSPHandle::".len();
        seen += 1;
        if seen > 3 {
            break;
        }
        let at = start + "LSPHandle".len() - 1;
        println!(
            "\nLSPHandle:: at {start}: type_at = {:?}",
            session.type_at(
                &view,
                cpp_code_analysis::file::coordinates::ReadingOffset::new(at)
            )
        );
        let end = start + "LSPHandle::".len();
        let set = session.completions(
            &view,
            cpp_code_analysis::file::coordinates::FileOffset::new(end),
        );
        println!(
            "  completions at {end}: {} item(s) {:?}",
            set.items.len(),
            set.items
                .iter()
                .map(|item| item.label.as_str())
                .take(8)
                .collect::<Vec<_>>()
        );
        let members = session.member_completions(
            &view,
            cpp_code_analysis::file::coordinates::FileOffset::new(end),
        );
        println!("  member_completions: {members:?}");
    }
}
