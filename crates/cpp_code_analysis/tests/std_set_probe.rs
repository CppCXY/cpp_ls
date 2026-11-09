/// A probe, not a test: why `std::map` is invisible from an `import std;` file.
#[test]
#[ignore = "a probe: opens the real module project and prints"]
fn what_the_module_closure_holds() {
    let root = std::path::PathBuf::from(r"C:\Users\zc\Desktop\test_module");
    if !root.is_dir() {
        eprintln!("no project at {}", root.display());
        return;
    }
    let files = cpp_code_analysis::SessionFiles::new(
        cpp_code_analysis::OpenDocuments::new(),
        cpp_code_analysis::DiskFiles,
    );
    let mut session =
        cpp_code_analysis::Session::open(&root, files, cpp_code_analysis::WatchFilter::new(&root));
    let file = root.join("src").join("std_module_demo.cpp");
    session.add_project_files([file.clone()]);
    let text = std::fs::read_to_string(&file).expect("the file");
    session.did_open(&file, &text);
    session.catch_up(&file);
    let read = session.read_the_modules_a_file_imports(&file);
    eprintln!("read {read} interface unit(s)");
    match session
        .index()
        .interface_units()
        .into_iter()
        .find(|(name, _)| name == "std")
    {
        Some((_, unit)) => eprintln!("`std` interface: {unit}"),
        None => eprintln!("`std` has no interface unit"),
    }
    match session.index().unit_closure_of("std") {
        Some(files) => {
            eprintln!("closure of `std`: {} file(s)", files.len());
            for needle in ["map", "iostream", "stl_map.h", "vector", "format"] {
                let hit = files
                    .iter()
                    .any(|path| path.ends_with(needle) || path.contains(needle));
                eprintln!("  {needle}: {hit}");
            }
        }
        None => eprintln!("no closure recorded for `std`"),
    }
    // **And after an index**, which is what the server's pump does around it: does the closure survive?
    let _ = session.index_everything();
    match session.index().unit_closure_of("std") {
        Some(files) => eprintln!("closure of `std` after the index: {} file(s)", files.len()),
        None => eprintln!("no closure recorded for `std` after the index"),
    }
    let after = session.index().visible_files(&file);
    eprintln!(
        "visible files from the demo after the index: {}",
        after.len()
    );
    for needle in ["stl_map.h", "iostream"] {
        let hit = after.iter().any(|(path, _)| path.contains(needle));
        eprintln!("  visible {needle} after the index: {hit}");
    }
    // **The question `inspect define` asks**, in this same session.
    for name in ["std::map", "std::cout", "std::vector", "std::string"] {
        match session.index().definition(name, &file) {
            cpp_code_analysis::Known::Yes(found) => eprintln!(
                "definition {name}: Yes {}:{}",
                found.file.display(),
                found.fact.name_range.start_offset
            ),
            other => eprintln!("definition {name}: {other:?}"),
        }
    }
    // **The spellings**, which is the other half: a closure entry the index files under a different spelling is a
    // file that answers nothing.
    if let Some(files) = session.index().unit_closure_of("std") {
        for needle in ["stl_map.h", "iostream", "stl_vector.h"] {
            if let Some(entry) = files.iter().find(|path| path.contains(needle)) {
                eprintln!("closure spelling {needle}: {entry}");
            }
        }
    }
    for (path, _) in session.index().visible_files(&file) {
        if path.contains("stl_map.h") || path.contains("stl_vector.h") {
            let summary = session
                .index()
                .summary(std::path::Path::new(&path))
                .is_some();
            eprintln!("visible spelling {path} — summary: {summary}");
        }
    }
    // **Bare against qualified, and the member list** — which of the three lookups disagrees.
    for name in ["map", "map<std::string, int>", "cout"] {
        match session.index().definitions(name, &file) {
            cpp_code_analysis::Known::Yes(found) => {
                eprintln!("definitions {name}: {} declaration(s)", found.found.len())
            }
            other => eprintln!("definitions {name}: {other:?}"),
        }
    }
    // **What the one candidate says about itself** — the qualified spelling, the export flag and the guard are
    // the three filters that can drop a fact that is in the index and in a visible file.
    if let cpp_code_analysis::Known::Yes(found) = session.index().definitions("map", &file) {
        for definition in &found.found {
            let fact = &definition.fact;
            eprintln!(
                "map candidate: qualified {:?} scope {:?} exported {} guard {:?}",
                fact.qualified_name(),
                fact.scope,
                fact.exported,
                fact.guard
            );
        }
    }
    let iostream = std::path::PathBuf::from(
        r"c:/users/zc/desktop/mingw/mingw64/lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++/iostream",
    );
    match session.index().summary(&iostream) {
        Some(summary) => {
            for fact in summary
                .declarations
                .iter()
                .filter(|fact| fact.name == "cout")
            {
                eprintln!(
                    "cout fact: scope {:?} exported {} guard {:?} type {:?}",
                    fact.scope, fact.exported, fact.guard, fact.type_of
                );
            }
        }
        None => eprintln!("iostream has no summary in this session"),
    }
    let view = session.view_of_the_file(&file);
    if let Some(view) = &view {
        match &session.members_of(view, "std::map") {
            cpp_code_analysis::Known::Yes(list) => {
                eprintln!("members_of std::map: {} member(s)", list.members.len())
            }
            other => eprintln!("members_of std::map: {other:?}"),
        }
    }
    let visible = session.index().visible_files(&file);
    eprintln!("visible files from the demo: {}", visible.len());
    let names: Vec<&str> = visible.iter().map(|(path, _)| path.as_str()).collect();
    for needle in ["stl_map.h", "iostream", "stl_vector.h", "format"] {
        let hit = names.iter().any(|path| path.contains(needle));
        eprintln!("  visible {needle}: {hit}");
    }
}
