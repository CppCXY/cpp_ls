//! **What one find-references costs, on a real project** — the measurement a reference-count code lens needs before
//! it can be designed.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test reference_cost_probe -- --ignored --nocapture
//! ```
//!
//! The question is arithmetic: a lens draws one number per function, so a file with *n* functions asks *n* questions.
//! If one question is milliseconds, the naive design works; if it is hundreds, the numbers have to be computed for the
//! whole project at once instead.

use std::path::PathBuf;

use cpp_code_analysis::file::coordinates::ReadingOffset;
use cpp_code_analysis::{
    DiskFiles, OpenDocuments, ReferenceBudget, Session, SessionFiles, WatchFilter,
};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_CODESTYLE_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\github\EmmyLuaCodeStyle"));
    named
        .join("CodeFormatCore")
        .join("src")
        .is_dir()
        .then_some(named)
}

#[test]
#[ignore = "a probe: needs the external project"]
fn what_one_find_references_costs() {
    let Some(root) = project() else {
        eprintln!("[cost] the project is not here, nothing measured");
        return;
    };
    let file = root.join("CodeFormatCore/src/Config/EditorconfigPattern.cpp");
    let source = std::fs::read_to_string(&file).expect("the file");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, &source);
    session.catch_up(&file);
    let _ = session.cook(&file);
    session.want_cooked_reading(&file);
    for _ in 0..8 {
        session.index_everything();
        if session.pending_work() == 0 {
            break;
        }
    }

    let view = session.view_of_the_file(&file).expect("a view");
    let names = ["Compile", "Lex", "Match", "GetPattern"];
    let mut total = 0u128;
    for name in names {
        let Some(at) = source.find(&format!("EditorconfigPattern::{name}(")) else {
            continue;
        };
        let at = at + "EditorconfigPattern::".len();
        let started = std::time::Instant::now();
        let found =
            session.symbol_references(&view, ReadingOffset::new(at), ReferenceBudget::default());
        let elapsed = started.elapsed();
        total += elapsed.as_micros();
        let count = match &found {
            cpp_code_analysis::Known::Yes(found) => found
                .files
                .iter()
                .map(|file| file.references.len())
                .sum::<usize>(),
            _ => 0,
        };
        eprintln!(
            "[cost] {name:>10}: {:>7.2} ms, {count} reference(s)",
            elapsed.as_secs_f64() * 1000.0
        );
    }
    eprintln!(
        "[cost] {} query(ies), {:.2} ms each on average",
        names.len(),
        total as f64 / 1000.0 / names.len() as f64
    );
}
