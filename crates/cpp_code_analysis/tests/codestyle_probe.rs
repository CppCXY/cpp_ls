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
