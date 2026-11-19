//! **Where a cooked program stream's parse errors are**, by byte offset — the instrument `cpp_dump` does not print.
//!
//! ```text
//! $env:CPPLS_STREAM = "…\ldebug.c.parsed"
//! cargo test -p cpp_parser --test stream_errors_probe -- --ignored --nocapture
//! ```

#[test]
#[ignore = "a probe: prints the offsets and the text around each error of a stream in a file"]
fn where_the_errors_of_a_stream_are() {
    let Ok(path) = std::env::var("CPPLS_STREAM") else {
        eprintln!("skipped: CPPLS_STREAM is not set");
        return;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("skipped: {path} cannot be read");
        return;
    };
    let tree = cpp_parser::CppParser::parse(&text, cpp_parser::ParserConfig::default());
    let parsed = tree.get_errors();
    println!("{} error(s) in {path}", parsed.len());
    for error in parsed.iter().take(20) {
        let range = cpp_parser::source_range(error.range);
        let start = range.start_offset;
        let end = range.end_offset();
        let around_start = start.saturating_sub(120);
        let around_end = (end + 120).min(text.len());
        println!(
            "--- {}..{} [{}] ---\n{}",
            start,
            end,
            error.message,
            text.get(around_start..around_end).unwrap_or("")
        );
    }
}
