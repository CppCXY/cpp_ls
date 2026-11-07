//! **A probe, not a test**: is the unit's own parse putting `_mm_abs_epi16` inside `namespace std`?
//!
//! ```text
//! $env:CPPLS_DUMP_RENDERING_DIR = "$env:TEMP\unit-dump"   # written by `module_hover_probe`'s run
//! cargo test --release -p cpp_code_analysis --test unit_stream_probe -- --ignored --nocapture
//! ```
//!
//! `module_hover_probe` dumps the streams it indexes (`CPPLS_DUMP_RENDERING_DIR`). This reads the dump back and asks the
//! parser the one question the facts disagree about: at the offset of a misfiled intrinsic, which namespaces does the
//! tree say it is inside? If `std` is among them, the parse is the defect; if not, the scope walker or the fact
//! attribution is, and the two are different repairs.

use cpp_parser::{CppNamespaceDecl, CppParser, ParserConfig};

#[test]
#[ignore]
fn where_the_parser_puts_the_intrinsics() {
    let Some(directory) = std::env::var_os("CPPLS_DUMP_RENDERING_DIR") else {
        println!("skipped: CPPLS_DUMP_RENDERING_DIR is not set");
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    let text = std::fs::read_to_string(directory.join("unit.stream")).expect("the dump reads");
    let boundaries: Vec<usize> = std::fs::read_to_string(directory.join("unit.boundaries"))
        .expect("the boundaries read")
        .lines()
        .filter_map(|line| line.trim().parse::<usize>().ok())
        .collect();
    let files: Vec<String> = std::fs::read_to_string(directory.join("unit.files"))
        .expect("the file list reads")
        .lines()
        .map(|line| line.to_string())
        .collect();
    println!(
        "stream {} bytes, {} boundaries, {} files",
        text.len(),
        boundaries.len(),
        files.len()
    );

    let started = std::time::Instant::now();
    let tree = CppParser::parse(
        &text,
        ParserConfig::default().with_file_boundaries(boundaries.clone()),
    );
    let root = tree.get_red_root();
    println!(
        "parsed in {:.0} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );

    for name in ["_mm_abs_epi16", "vector<bool, _Alloc>", "format"] {
        let Some(at) = text.find(name) else {
            println!("`{name}`: not in the stream");
            continue;
        };
        // The file this offset is in, by the same boundaries the parse was given.
        let slice = boundaries.partition_point(|boundary| *boundary <= at);
        let file = files.get(slice).map(String::as_str).unwrap_or("?");
        let namespaces = namespaces_at(&root, at);
        println!(
            "`{name}` at {at} in {}: namespaces {:?}",
            file.rsplit('/').next().unwrap_or(file),
            namespaces
        );
        // **Which opening each of those namespaces is**, and how much text it spans: an opening that reaches far past
        // its own file is the one the boundary rule failed to end, and the file it starts in is the culprit.
        for (range, _) in namespace_ranges_at(&root, at) {
            let opened_in = boundaries.partition_point(|boundary| *boundary <= range.start_offset);
            let opened_file = files
                .get(opened_in)
                .map(|file| file.rsplit('/').next().unwrap_or(file).to_string())
                .unwrap_or_default();
            let crossings = boundaries
                .iter()
                .filter(|boundary| {
                    *boundary > &range.start_offset && *boundary < &range.end_offset()
                })
                .count();
            println!(
                "    opens {} in {} — ends {}, {} boundary(ies) crossed",
                range.start_offset,
                opened_file,
                if range.end_offset() == text.len() {
                    "EOF".to_string()
                } else {
                    range.end_offset().to_string()
                },
                crossings
            );
        }
        // **The invariant**: a declaration in a header is inside `std` **once**. A depth of two or more is a namespace
        // whose `}` the parse lost, and every one of them files the next file's declarations into a namespace they do
        // not belong to — the 6 732 intrinsics under `std` that flood `std::` completion.
        assert!(
            namespaces.len() <= 1,
            "`{name}` is inside {} nested namespaces ({namespaces:?}) — one of them never closed",
            namespaces.len()
        );
    }
}

/// The range of each enclosing namespace, outermost first.
fn namespace_ranges_at(
    root: &cpp_parser::CppSyntaxNode,
    at: usize,
) -> Vec<(cpp_parser::SourceRange, usize)> {
    use cpp_parser::CppAstNode;
    let mut found = None;
    for node in root.descendants() {
        let range = cpp_parser::source_range(node.text_range());
        if range.start_offset <= at && at <= range.end_offset() {
            found = Some(node);
        }
    }
    let mut ranges = Vec::new();
    let mut node = found;
    while let Some(current) = node {
        if let Some(namespace) = CppNamespaceDecl::cast(current.clone()) {
            let range = cpp_parser::source_range(current.text_range());
            ranges.push((range, range.start_offset));
        }
        node = current.parent();
    }
    ranges.reverse();
    ranges
}

/// The file an offset is in, by the same boundaries the parse was given.
fn file_of(files: &[String], boundaries: &[usize], at: usize) -> String {
    let slice = boundaries.partition_point(|boundary| *boundary <= at);
    let file = files.get(slice).cloned().unwrap_or_default();
    file.rsplit('/').next().unwrap_or(&file).to_string()
}

/// The namespace names enclosing `at`, outermost first, read from the tree.
fn namespaces_at(root: &cpp_parser::CppSyntaxNode, at: usize) -> Vec<String> {
    use cpp_parser::CppAstNode;
    let mut found = None;
    for node in root.descendants() {
        let range = cpp_parser::source_range(node.text_range());
        if range.start_offset <= at && at <= range.end_offset() {
            found = Some(node);
        }
    }
    let mut names = Vec::new();
    let mut node = found;
    while let Some(current) = node {
        if let Some(namespace) = CppNamespaceDecl::cast(current.clone()) {
            if let Some(name) = namespace.get_name_text() {
                names.push(name);
            }
        }
        node = current.parent();
    }
    names.reverse();
    names
}
