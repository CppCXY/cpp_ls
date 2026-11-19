//! **How much there is to grey out, and what it costs to send** — a measurement, not the feature.
//!
//! ```text
//! cargo test --release -p cpp_toolchain --test inactive_regions_probe -- --ignored --nocapture
//! ```
//!
//! `CookedStream.inactive` already holds the spans of the branches this configuration did not compile (the
//! preprocessor records them while it decides; `merge_span` joins one dead branch's pieces). What a
//! `textDocument/inactiveRegions` notification would cost is the count and the byte size of those spans as LSP
//! ranges, which is what this prints on a real project.

use cpp_toolchain::macros::MacroTable;
use cpp_toolchain::preprocess::cooked::cook_with;

#[test]
#[ignore = "a probe: cooks a real project's files and prints the inactive spans"]
fn how_many_inactive_regions_a_real_project_has() {
    let root = std::path::PathBuf::from(std::env::var("CPPLS_SOURCES").unwrap_or_else(|_| {
        r"C:\Users\zc\Desktop\github\lua_rt\lua_src\lua-5.5.0\src".to_string()
    }));
    let Ok(entries) = std::fs::read_dir(&root) else {
        eprintln!("skipped: {} is not here", root.display());
        return;
    };
    let nothing_defined = MacroTable::new();
    let mut files = 0usize;
    let mut total_regions = 0usize;
    let mut total_bytes = 0usize;
    let mut worst: Vec<(usize, usize, String)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !matches!(path.extension().and_then(|it| it.to_str()), Some("c" | "h")) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (tokens, _) = cpp_parser::lex(&text, &cpp_parser::LexerConfig::default());
        let cooked = cook_with(&text, &tokens, &nothing_defined);
        files += 1;
        total_regions += cooked.inactive.len();
        let bytes: usize = cooked
            .inactive
            .iter()
            .map(|span| span.length as usize)
            .sum();
        total_bytes += bytes;
        worst.push((
            cooked.inactive.len(),
            bytes,
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default(),
        ));
    }
    worst.sort_by_key(|(regions, _, _)| std::cmp::Reverse(*regions));
    println!(
        "[inactive] {files} file(s), {total_regions} region(s), {total_bytes} byte(s) of inactive text"
    );
    // **The payload**, as the JSON a notification would carry: `{"start":{"line","character"},"end":…}` per range is
    // about 90 bytes at these line numbers, and a file's whole answer is one notification.
    println!(
        "[inactive] about {} byte(s) of JSON per project, {} per file",
        total_regions * 90,
        if files == 0 {
            0
        } else {
            total_regions * 90 / files
        }
    );
    for (regions, bytes, name) in worst.into_iter().take(6) {
        println!("[inactive] {name}: {regions} region(s), {bytes} byte(s)");
    }
}
