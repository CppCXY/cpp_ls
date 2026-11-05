//! **What one rule costs on a file built to ask it many times** — a measurement, not a check.
//!
//! ```text
//! CPPLS_TIME_CHECKS=1 cargo test --release -p cpp_code_analysis --test check_timing -- --ignored --nocapture
//! ```
//!
//! The per-rule timing comes from the crate's own probe (`CPPLS_TIME_CHECKS`), which prints one line per rule that takes at least
//! a millisecond. The variable is exported rather than set in the test: the probe is an `OnceLock`, so it is read once, at the
//! first rule — an environment a test sets afterwards would be ignored.

use std::path::PathBuf;

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// **A file with many C-style casts** — five per function, in the shapes a real project writes: a scalar conversion, a pointer
/// cast, an address-of operand, and a cast of an expression.
fn cast_heavy(functions: usize) -> String {
    let mut source = String::new();
    for index in 0..functions {
        source.push_str(&format!(
            "int f{index}(int a, long b, double c) {{\n\
             \x20   int x = (int)b;\n\
             \x20   long y = (long)c;\n\
             \x20   char* p = (char*)&a;\n\
             \x20   void* q = (void*)p;\n\
             \x20   return (int)(x + y) + (int)p[0];\n\
             }}\n"
        ));
    }
    source
}

#[test]
#[ignore = "a measurement, not a check"]
fn what_one_rule_costs_on_a_cast_heavy_file() {
    let source = cast_heavy(4_000);
    let path = "/p/casts.cpp";
    let files = MemoryFiles::new().with_file(path, source.as_str());
    let mut session = Session::with_config(
        "/p",
        SessionFiles::new(OpenDocuments::new(), files),
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([PathBuf::from(path)]);
    session.want_cooked_reading(std::path::Path::new(path));

    let started = std::time::Instant::now();
    session.index_everything();
    let indexing = started.elapsed();

    let started = std::time::Instant::now();
    let found = session
        .diagnostics(std::path::Path::new(path))
        .expect("the file has a reading");
    let checking = started.elapsed();

    println!(
        "{} bytes: index {indexing:?}, diagnostics {checking:?}, {} finding(s)",
        source.len(),
        found.errors.len()
    );
    let mut codes: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for error in &found.errors {
        *codes.entry(error.code.name()).or_default() += 1;
    }
    println!("findings by rule: {codes:?}");
}
