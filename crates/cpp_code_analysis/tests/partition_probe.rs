/// A probe, not a test: what an implementation unit sees of its module's **partitions**.
#[test]
#[ignore = "a probe: opens the real partition project and prints"]
fn what_an_implementation_unit_sees_of_its_partitions() {
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
    session.add_project_files([root.join("src").join("partition_demo.cpp")]);
    for file in ["src/partition_demo.cpp", "src/main.cpp"] {
        let path = root.join(file);
        let text = std::fs::read_to_string(&path).expect("the file");
        session.did_open(&path, &text);
        session.catch_up(&path);
        let _ = session.index_everything();
        eprintln!("=== {file}");
        let visible = session.index().visible_files(&path);
        eprintln!("visible files: {}", visible.len());
        for needle in [
            "geometry.cppm",
            "geometry_shapes.cppm",
            "geometry_area.cppm",
        ] {
            let hit = visible.iter().any(|(entry, _)| entry.contains(needle));
            eprintln!("  visible {needle}: {hit}");
        }
        for name in ["partition_demo_area", "Rect", "rect_area"] {
            match session.index().definition(name, &path) {
                cpp_code_analysis::Known::Yes(found) => eprintln!(
                    "  definition {name}: {}:{}",
                    found.file.display(),
                    found.fact.name_range.start_offset
                ),
                other => eprintln!("  definition {name}: {other:?}"),
            }
        }
    }
}
