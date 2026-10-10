use cpp_code_analysis::{MemoryFiles, Session, SessionFiles, OpenDocuments, WatchFilter, CompilerConfig};

fn probe(label: &str, source: &str, want: &str) {
    let path = format!("/p/{label}.hpp");
    let files = MemoryFiles::new()
        .with_file(path.clone(), source)
        .with_file("/p/main.cpp", &format!("#include \"{label}.hpp\"\n"));
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config("/p", providers, WatchFilter::new("/p"), CompilerConfig::default());
    session.add_project_files([std::path::PathBuf::from("/p/main.cpp")]);
    session.index_everything();

    let summary = session.index().summary(std::path::Path::new(&path)).expect("indexed");
    let names: Vec<&str> = summary
        .declarations
        .iter()
        .map(|fact| fact.name.as_str())
        .collect();
    println!(
        "{label:24} declarations {:>4} | {want} present: {} | first 6 {:?}",
        summary.declarations.len(),
        names.contains(&want),
        names.iter().take(6).collect::<Vec<_>>()
    );
}

fn main() {
    // The real declaration from xstring:2343, plus one member so the body is non-empty.
    probe("real-shape", "template <class _Elem, class _Traits = char_traits<_Elem>, class _Alloc = allocator<_Elem>>\nclass basic_string {\npublic:\n    void f();\n};\n", "basic_string");
    // The same with a full template-argument default that names two parameters.
    probe("two-arg-default", "template <class A, class B>\nstruct Pair {};\ntemplate <class _Elem, class _Traits = char_traits<_Elem>, class _Alloc = allocator<_Elem>>\nclass basic_string {\npublic:\n    void f();\n};\n", "basic_string");
    // Defaults but no body member at all.
    probe("empty-body", "template <class _Elem, class _Traits = char_traits<_Elem>>\nclass basic_string_view {};\n", "basic_string_view");
    // Exactly basic_string_view's real shape.
    probe("view-shape", "template <class _Elem, class _Traits>\nclass basic_string_view {\npublic:\n    void g();\n};\n", "basic_string_view");
}
