//! **The one check every per-standard syntax test uses.**
//!
//! The parser is lossless and recovers from errors, so "it parses" is not "it understands" — but a construct that
//! produces a **parse error** is a construct whose text the analysis cannot rely on, and that is the first question to
//! ask of any standard or vendor extension. Each test file states its own table and calls this; a failure reports
//! **every** construct it failed rather than the first, so one run says how far behind the grammar is in one number.

use cpp_parser::{CppParser, ParserConfig};

/// Parse each `(name, source)` pair and fail with the whole list of those that produced an error.
pub fn assert_all_parse(standard: &str, constructs: &[(&str, &str)]) {
    let mut failed: Vec<(&str, String)> = Vec::new();
    for (name, source) in constructs {
        let tree = CppParser::parse(source, ParserConfig::default());
        let errors = tree.get_errors();
        if !errors.is_empty() {
            failed.push((name, format!("{errors:?}")));
        }
    }
    assert!(
        failed.is_empty(),
        "{standard}: {} of {} construct(s) do not parse:\n{}",
        failed.len(),
        constructs.len(),
        failed
            .iter()
            .map(|(name, errors)| format!("  {name}: {errors}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
