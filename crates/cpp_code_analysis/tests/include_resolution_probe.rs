//! **What one header's `#include` resolved to** — the question the walk's exponential re-entry turned on.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test -p cpp_code_analysis --test include_resolution_probe -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use cpp_code_analysis::file::paths::DiskFiles;
use cpp_code_analysis::include::config::CompilerConfig;
use cpp_code_analysis::index::watch::WatchFilter;
use cpp_code_analysis::session::{OpenDocuments, Session, SessionFiles};

#[test]
#[ignore = "a probe: prints the includes of uriparser's uri.h and their resolutions"]
fn how_uri_h_resolves_its_include() {
    let root = PathBuf::from(
        std::env::var("CPPLS_REAL_PROJECT").unwrap_or_else(|_| r"E:\EmmyLuaCodeStyle".to_string()),
    );
    let uri = root.join("3rd/uriparser/include/uriparser/uri.h");
    let mut session = Session::with_config(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        CompilerConfig::default(),
    );
    session.add_project_files([uri.clone()]);
    session.index_everything();
    let Some(summary) = session.index().summary(&uri) else {
        println!("no summary for {}", uri.display());
        return;
    };
    println!("uri.h has {} include(s)", summary.includes.len());
    for include in &summary.includes {
        println!(
            "  spelling {:?} resolved {:?} (guarded {:?})",
            include.spelling, include.resolved, include.guard
        );
    }
    let config = root.join("3rd/uriparser/include/uriparser/uridefsconfig.h");
    if let Some(summary) = session.index().summary(&config) {
        println!(
            "uridefsconfig.h has {} include(s), own guard {:?}, visit_once {}",
            summary.includes.len(),
            summary.guards.own_guard,
            summary.guards.visit_once
        );
    } else {
        println!("no summary for {}", config.display());
    }
}
