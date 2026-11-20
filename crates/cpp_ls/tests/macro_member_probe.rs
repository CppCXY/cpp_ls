//! **`MACRO(x)->member`** — what the rendering types, and where the chain stops.
//!
//! ```text
//! $env:CPPLS_C_PROJECT = "C:\Users\zc\Desktop\github\lua_rt\lua_src\lua-5.5.0"
//! cargo test --release -p cpp_ls --test macro_member_probe -- --ignored --nocapture
//! ```
//!
//! Reported from an editor: `ci_func(ci)->p` in Lua 5.5.0's `ldebug.c:101` answers *"the type of `ci_func(ci)` is not
//! known here"*. The written tree has no type for a **macro call** (its callee declares nothing), while the rendering
//! has the expansion — `(&((union GCUnion *)…->cl.l))->p` — which is a chain the engine can walk *if* each step types.
//! This prints every step of that chain, typed on its own, so that the first step which does not says where the work
//! is: the crossing from the written expression into the rendering, or the chain itself.

use std::path::{Path, PathBuf};

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use cpp_parser::CppAstNode as _;

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_C_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\github\lua_rt\lua_src\lua-5.5.0".to_string()),
    )
}

fn project_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.join("src")];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|it| it == "c" || it == "h") {
                files.push(path);
            }
        }
    }
    files
}

/// The **outermost** expression that starts at `at` — the node whose text is the whole spelling from there.
fn outermost_at(root: &cpp_parser::CppSyntaxNode, at: usize) -> Option<cpp_parser::CppSyntaxNode> {
    root.descendants()
        .filter(|node| cpp_parser::CppExpr::can_cast(node.kind().into()))
        .filter(|node| usize::from(node.text_range().start()) == at)
        .max_by_key(|node| usize::from(node.text_range().end()))
}

#[test]
#[ignore = "indexes a real project; run with --ignored"]
fn what_the_expansion_types_and_where_the_chain_stops() {
    let project = project();
    let files = project_files(&project);
    let mut session = Session::open(
        project.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&project),
    );
    session.add_project_files(files.iter().cloned());
    session.index_everything();

    let file = project.join("src/ldebug.c");
    session.want_cooked_reading(&file);
    let _ = session.cook(&file);
    let Some(written) = session.view_of_the_file(&file) else {
        panic!("the written view");
    };
    let Some(cooked) = session.view(&file) else {
        panic!("the cooked view");
    };
    let source = std::fs::read_to_string(&file).expect("the file reads");

    // **Line 101's use of the macro**, not the file's first one: `ci_func(ci)->p` occurs eight times in `ldebug.c`
    // and the first is in `luaG_traceexec`. An earlier probe searched the first and concluded wrongly.
    let at = source
        .match_indices("ci_func(ci)->p")
        .map(|(at, _)| at)
        .find(|at| source[..*at].matches('\n').count() == 100)
        .expect("the reported line")
        + 1;
    let object_start = at - 1;
    let object_end = object_start + "ci_func(ci)".len();
    eprintln!(
        "[probe] the object is {object_start}..{object_end}: {:?}",
        &source[object_start..object_end]
    );

    let Some(node) = cpp_code_analysis::sema::resolve::expression_at(&written.root, at) else {
        panic!("the written expression");
    };
    match session.type_of_the_expression(&written, &node) {
        cpp_code_analysis::Known::Yes(found) => {
            eprintln!("[probe] written: typed {:?}", found.type_of)
        }
        other => eprintln!("[probe] written: not typed {other:?}"),
    }

    let Some(rendering) = session.rendering_of(&file) else {
        panic!("the rendering");
    };
    let start = rendering
        .the_rendering_offset_of(object_start)
        .expect("the object's first token maps");
    eprintln!("[probe] written {object_start} maps to rendering {start}");

    // **The chain, one step at a time**, each step found in the rendering text and typed on its own.
    let text = rendering.text.clone();
    let snippets = [
        "((ci)->func.p)",
        "(ci)->func.p",
        "(((&((ci)->func.p)->val))->value_).gc",
        "(((union GCUnion *)(((((&((ci)->func.p)->val))->value_).gc))))",
        "(((union GCUnion *)(((((&((ci)->func.p)->val))->value_).gc))))->cl",
        "(((union GCUnion *)(((((&((ci)->func.p)->val))->value_).gc))))->cl.l",
        "(&((((union GCUnion *)(((((&((ci)->func.p)->val))->value_).gc))))->cl.l))",
    ];
    for snippet in snippets {
        let Some(found_at) = text.find(snippet) else {
            eprintln!("[probe] {snippet}\n           -> not in the rendering");
            continue;
        };
        let Some(node) = outermost_at(&cooked.root, found_at) else {
            eprintln!("[probe] {snippet}\n           -> no expression starts there");
            continue;
        };
        let typed = match session.type_of_the_expression(&cooked, &node) {
            cpp_code_analysis::Known::Yes(found) => found.type_of,
            other => format!("{other:?}"),
        };
        eprintln!("[probe] {snippet}\n           -> {typed}");
    }

    // **The unions the chain walks through** — `cl` is a member of `GCUnion`, `l` a member of `Closure`.
    for class in [
        "GCUnion",
        "Closure",
        "StackValue",
        "TValue",
        "CallInfo",
        "StkIdRel",
    ] {
        match session.members_of(&cooked, class) {
            cpp_code_analysis::Known::Yes(list) => {
                let names: Vec<&str> = list.members.iter().map(|m| m.fact.name.as_str()).collect();
                eprintln!("[probe] members of {class}: {names:?}");
            }
            other => eprintln!("[probe] members of {class}: {other:?}"),
        }
    }
    // **Why `TValue` has no members**: the written struct body is `TValuefields;` — a macro — and the rendering has
    // the expanded `Value value_; lu_byte tt_;`. Which reading is in the index, and what does each say?
    {
        let lobject = project.join("src/lobject.h");
        match session.view(&lobject) {
            Some(view) => {
                let text = view.source.clone();
                let at = text.find("struct TValue").unwrap_or(0);
                let shown = &text[at..(at + 90).min(text.len())];
                eprintln!("[probe] the rendering of lobject.h has: {shown:?}");
            }
            None => eprintln!("[probe] lobject.h has no cooked view"),
        }
        match session.index().cooked_reading(&lobject) {
            Some(cooked) => {
                let facts: Vec<&str> = cooked
                    .declarations
                    .iter()
                    .filter(|fact| fact.name == "value_" || fact.name == "tt_")
                    .map(|fact| fact.name.as_ref())
                    .collect();
                eprintln!(
                    "[probe] lobject.h's cooked reading has {} declarations, {} of them value_/tt_: {facts:?}",
                    cooked.declarations.len(),
                    facts.len()
                );
            }
            None => eprintln!("[probe] lobject.h has no cooked reading"),
        }
        let summaries: Vec<(String, usize)> = session
            .index()
            .all_summaries()
            .iter()
            .filter(|(path, _)| path.file_name().is_some_and(|name| name == "lobject.h"))
            .map(|(path, summary)| (path.display().to_string(), summary.declarations.len()))
            .collect();
        eprintln!("[probe] summaries of lobject.h: {summaries:?}");
    }
    // **The program reading, which is what the editor commits**: the unit path files every file's share under the
    // program — `commit_a_unit_reading` — and that is where a header's *expanded* member facts come from. The probe
    // above only cooked one file, which is why it could not answer `TValue`'s members.
    {
        let mut session = session;
        session.index_everything();
        if let Some(prepared) = session.prepare_a_unit_reading(&file) {
            let committed = session.commit_a_unit_reading(prepared).is_some();
            eprintln!("[probe] committed a unit reading: {committed}");
        }
        match session
            .index()
            .cooked_reading(&project.join("src/lobject.h"))
        {
            Some(cooked) => {
                let facts: Vec<&str> = cooked
                    .declarations
                    .iter()
                    .filter(|fact| fact.name == "value_" || fact.name == "tt_")
                    .map(|fact| fact.name.as_ref())
                    .collect();
                eprintln!(
                    "[probe] lobject.h's cooked reading now has {} declaration(s), value_/tt_: {facts:?}",
                    cooked.declarations.len()
                );
            }
            None => eprintln!("[probe] lobject.h still has no cooked reading"),
        }
        match session.view(&file) {
            Some(cooked) => {
                let some_start = rendering_start(&cooked);
                eprintln!(
                    "[probe] in the unit session, the writing at the mapped start is {some_start:?}"
                );
                if let Some(at) = some_start
                    && let Some(node) = outermost_at(&cooked.root, at)
                {
                    let typed = match session.type_of_the_expression(&cooked, &node) {
                        cpp_code_analysis::Known::Yes(found) => found.type_of,
                        other => format!("{other:?}"),
                    };
                    eprintln!(
                        "[probe] the expansion {:?} types as {typed}",
                        node.text().to_string()
                    );
                }
            }
            None => eprintln!("[probe] the file has no cooked view"),
        }
        for class in [
            "GCUnion",
            "union GCUnion",
            "Closure",
            "union Closure",
            "LClosure",
        ] {
            match session.members_of(&cooked_view_of(&session, &file), class) {
                cpp_code_analysis::Known::Yes(list) => {
                    eprintln!("[probe] (unit) members of {class}: {}", list.members.len())
                }
                other => eprintln!("[probe] (unit) members of {class}: {other:?}"),
            }
        }
        // **Every member access of the chain**, with the type of its *object*: the engine answers the member by
        // asking the object first, so the first object that does not type is the step that stops the chain.
        if let Some(cooked) = session.view(&file) {
            for node in cooked.root.descendants() {
                let Some(access) = cpp_code_analysis::sema::resolve::member_access_of(&node) else {
                    continue;
                };
                if !matches!(&*access.member, "l" | "cl" | "value_" | "gc" | "p" | "val") {
                    continue;
                }
                let object = access.object.text().to_string().trim().to_string();
                if object.len() > 120 {
                    continue;
                }
                let typed = match session.type_of_the_expression(&cooked, &access.object) {
                    cpp_code_analysis::Known::Yes(found) => found.type_of,
                    other => format!("{other:?}"),
                };
                eprintln!(
                    "[probe] .{}{} on {object:?} (object typed {typed})",
                    if access.arrow { "->" } else { "." },
                    access.member
                );
            }
        }
        match session.view(project.join("src/lobject.h")) {
            Some(view) => match session.members_of(&view, "TValue") {
                cpp_code_analysis::Known::Yes(list) => eprintln!(
                    "[probe] after the unit reading, TValue has {} members: {:?}",
                    list.members.len(),
                    list.members
                        .iter()
                        .map(|m| m.fact.name.as_str())
                        .take(6)
                        .collect::<Vec<_>>()
                ),
                other => eprintln!("[probe] after the unit reading, TValue's members: {other:?}"),
            },
            None => eprintln!("[probe] lobject.h has no view"),
        }
    }
}

/// **The rendering offset of the reported crossing**, found the same way the handler will: the object's written start,
/// mapped through the file's rendering.
fn rendering_start(cooked: &cpp_code_analysis::FileView) -> Option<usize> {
    let text = &cooked.source;
    text.find("luaG_getfuncline((")
        .map(|at| at + "luaG_getfuncline(".len())
}

/// The cooked view of a file, for a probe that asks members with it.
fn cooked_view_of(session: &Session<DiskFiles>, path: &Path) -> cpp_code_analysis::FileView {
    session.view(path).expect("the file has a rendering")
}
