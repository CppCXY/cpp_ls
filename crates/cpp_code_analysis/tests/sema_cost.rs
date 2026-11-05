//! **What the deduction's walks cost**, on a file built to ask them many times.
//!
//! Every function below writes an `auto`-returning body with a lambda of its own, an `if constexpr` with two returns, and locals —
//! so the parameter, local, return and condition walks run once per name and once per return over subtrees that are not small. The
//! number is a **measurement, not a check**: the test is `#[ignore]`d and prints.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test sema_cost -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

fn stress() -> String {
    let mut source = String::new();
    for index in 0..240 {
        source.push_str(&format!(
            "auto compute_{index}(int value) {{\n\
             \x20   auto transform = [](int inner) {{ return inner; }};\n\
             \x20   int local = value;\n\
             \x20   if constexpr (sizeof(int) == 4) {{\n\
             \x20       return local;\n\
             \x20   }} else {{\n\
             \x20       int shadow = local;\n\
             \x20       return shadow;\n\
             \x20   }}\n\
             }}\n"
        ));
    }
    source
}

#[test]
#[ignore = "a measurement, not a check"]
fn what_the_deduction_costs() {
    let source = stress();
    let files = MemoryFiles::new().with_file("/p/stress.cpp", source.as_str());
    let mut session = Session::with_config(
        "/p",
        SessionFiles::new(OpenDocuments::new(), files),
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([PathBuf::from("/p/stress.cpp")]);

    let started = std::time::Instant::now();
    session.index_everything();
    let indexing = started.elapsed();

    let summary = session
        .index()
        .summary(std::path::Path::new("/p/stress.cpp"))
        .expect("the file is indexed");
    let deduced = summary
        .declarations
        .iter()
        .filter(|fact| fact.returns.is_some())
        .count();
    println!(
        "index_everything: {} ms, {} declaration(s), {} with a deduced return",
        indexing.as_millis(),
        summary.declarations.len(),
        deduced
    );
}
