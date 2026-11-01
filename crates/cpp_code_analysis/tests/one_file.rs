/// **Every finding in one file, with the source around it** — how a false positive gets a location instead of a guess.
///
/// ```text
/// CPPLS_FILE="…\comutil.h" cargo test -p cpp_code_analysis --test one_file -- --ignored --nocapture
/// ```
#[test]
#[ignore = "reads one real file; run with --ignored and CPPLS_FILE set"]
fn one_file_reports_its_findings_with_context() {
    use cpp_code_analysis::file::coordinates::ReadingOffset;
    use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
    use std::path::PathBuf;

    let Ok(file) = std::env::var("CPPLS_FILE") else {
        eprintln!("skipped: set CPPLS_FILE to a source file");
        return;
    };
    let path = PathBuf::from(file);
    let root = path.parent().expect("a file has a directory").to_path_buf();
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([path.clone()]);
    session.index_everything();
    session.cook(&path);
    let Some(view) = session.view(&path) else {
        panic!("the file is readable");
    };
    let text = std::fs::read_to_string(&path).expect("the file is text");
    let lines: Vec<&str> = text.lines().collect();
    let Some(answer) = session.diagnostics(&path) else {
        panic!("the file is analysed");
    };
    for finding in &answer.checks {
        let at = view.file_offset_of(ReadingOffset::new(finding.range.start_offset));
        let line = at.map(|at| text[..at.get().min(text.len())].matches('\n').count());
        // **The scope chain at the finding** — which class the reader is inside, as the tree sees it.
        if let Some(at) = at
            && let Some(scope) = view.scopes.scope_at(at.get())
        {
            let chain: Vec<String> = view
                .scopes
                .scope_chain(scope)
                .into_iter()
                .filter_map(|id| {
                    view.scopes.scope(id).map(|data| {
                        format!("{:?}{}", data.kind, data.name.clone().unwrap_or_default())
                    })
                })
                .collect();
            println!(
                "  {:?} at {:?} in scopes: {chain:?}",
                finding.code,
                line.map(|line| line + 1)
            );
            // **What the file actually spells at the reported span** — which tells a wrong *range* from a wrong
            // *translation*: when this differs from `finding.name`, the range is the problem.
            {
                let at = at.get();
                let spelled = text.get(
                    at..(at + (finding.range.end_offset() - finding.range.start_offset))
                        .min(text.len()),
                );
                println!("     name={:?} spelled={spelled:?}", finding.name);
            }
        }
        if let Some(line) = line {
            for around in line.saturating_sub(2)..=(line + 2).min(lines.len().saturating_sub(1)) {
                println!("  {}: {}", around + 1, lines[around]);
            }
        }
    }
}
