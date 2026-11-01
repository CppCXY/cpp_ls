/// **A sweep for false findings over a standard library's headers** — valid code, so every finding is a defect of this layer's.
///
/// Ignored by default because it reads a hundred megabytes. Run it with the include directory:
///
/// ```text
/// CPPLS_HEADERS="C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.35.32215\include" \
///   cargo test -p cpp_code_analysis --test headers_sweep -- --ignored --nocapture
/// ```
#[test]
#[ignore = "reads a standard library's headers; run with --ignored and CPPLS_HEADERS set"]
fn the_headers_of_a_standard_library_produce_no_findings() {
    use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
    use std::path::PathBuf;

    let Ok(root) = std::env::var("CPPLS_HEADERS") else {
        eprintln!("skipped: set CPPLS_HEADERS to a standard library's include directory");
        return;
    };
    let root = PathBuf::from(root);
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let mut files: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("the directory is readable")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_file())
        .take(60)
        .collect();
    files.sort();
    session.add_project_files(files.clone());
    let indexing = std::time::Instant::now();
    session.index_everything();
    println!(
        "\n=== {} header(s) indexed in {:?}",
        files.len(),
        indexing.elapsed()
    );

    let mut by_code: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut shown = 0usize;
    let mut judged = 0usize;
    let diagnostics = std::time::Instant::now();
    for path in &files {
        session.cook(path);
        let Some(answer) = session.diagnostics(path) else {
            continue;
        };
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        for finding in &answer.checks {
            judged += 1;
            *by_code.entry(format!("{:?}", finding.code)).or_default() += 1;
            if shown < 400
                && !matches!(
                    finding.code,
                    cpp_code_analysis::sema::check::DiagnosticCode::CStyleCast
                        | cpp_code_analysis::sema::check::DiagnosticCode::UseNullptr
                        | cpp_code_analysis::sema::check::DiagnosticCode::ElseAfterReturn
                        | cpp_code_analysis::sema::check::DiagnosticCode::MacroParentheses
                )
            {
                println!("  {name}: {:?}  {}", finding.code, finding.message);
            }
            shown += 1;
        }
    }
    println!(
        "\n=== {judged} finding(s) by code: {by_code:?} in {:?}",
        diagnostics.elapsed()
    );
}
