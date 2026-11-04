//! **What the parser costs** — lex and parse throughput, what a tree weighs, and what the server pays per request.
//!
//! ```text
//! $env:CPPLS_PROJECT_ROOT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_parser --test parse_cost -- --ignored --nocapture
//! ```
//!
//! Three questions, because they have three different answers and only the third is the one a reader feels:
//!
//! 1. **Throughput** on real files — lexing alone, then lex + parse, in MB/s.
//! 2. **Weight** — how many tokens and nodes a file becomes, and what the green tree's allocations add up to. An
//!    immutable rowan tree stores each token's text in its own allocation, so "the file is 3 MB" says nothing about
//!    what the tree costs.
//! 3. **Per request** — what the analysis actually pays: a rendering (a whole closure, preprocessed) is parsed on
//!    every `Session::view`, and the second `view` of one file is what says whether that is cached.

use cpp_parser::{CppParser, CppSyntaxKind, ParserConfig};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_PROJECT_ROOT").map(PathBuf::from)?;
    named.is_dir().then_some(named)
}

fn sources(root: &Path, limit: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // The build outputs and the vendored third party are not what a reader types into.
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if !matches!(name.as_ref(), "build" | "3rd" | ".git" | "target" | "out") {
                    stack.push(path);
                }
                continue;
            }
            let text = path.to_string_lossy();
            // **The standard library is the hard corpus**, and it spells its own headers without an extension:
            // `bits/shared_ptr.h` has one, `bits/stl_vector.h` does not, and a `meow.tcc` is a template body.
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let extensionless = !name.contains('.');
            if text.ends_with(".cpp")
                || text.ends_with(".h")
                || text.ends_with(".hpp")
                || text.ends_with(".hxx")
                || text.ends_with(".tcc")
                || text.ends_with(".inl")
                || extensionless
            {
                found.push(path);
                if found.len() >= limit * 4 {
                    return found;
                }
            }
        }
    }
    found.sort();
    found
}

fn mbps(bytes: usize, took: std::time::Duration) -> f64 {
    let seconds = took.as_secs_f64();
    if seconds == 0.0 {
        return 0.0;
    }
    bytes as f64 / (1024.0 * 1024.0) / seconds
}

/// **What one tree is made of**, counted by walking the green tree — every node and token the immutable side holds.
fn weigh(root: &cpp_parser::CppSyntaxTree) -> (usize, usize, usize) {
    let mut nodes = 0usize;
    let mut tokens = 0usize;
    let mut token_bytes = 0usize;
    let mut stack = vec![root.get_green_root().clone()];
    while let Some(node) = stack.pop() {
        nodes += 1;
        for child in node.children() {
            match child {
                rowan::NodeOrToken::Node(node) => stack.push(node.to_owned()),
                rowan::NodeOrToken::Token(token) => {
                    tokens += 1;
                    token_bytes += token.text().len();
                }
            }
        }
    }
    (nodes, tokens, token_bytes)
}

#[test]
#[ignore = "reads a real project; run with --ignored and CPPLS_PROJECT_ROOT"]
fn what_parsing_costs() {
    let Some(root) = project() else {
        eprintln!("skipped: set CPPLS_PROJECT_ROOT");
        return;
    };

    // **1. Throughput**, over the files a reader actually edits.
    let files = sources(&root, 60);
    let mut total_bytes = 0usize;
    let mut total_parse = std::time::Duration::ZERO;
    let mut worst: Vec<(u128, String, usize)> = Vec::new();
    for path in files.iter().take(60) {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        if text.len() < 1024 {
            continue;
        }
        total_bytes += text.len();
        let started = Instant::now();
        let tree = CppParser::parse(&text, ParserConfig::default());
        let took = started.elapsed();
        total_parse += took;
        let errors = tree.get_errors().len();
        worst.push((
            took.as_micros(),
            format!("{} ({errors} error(s))", path.display()),
            text.len(),
        ));
    }
    worst.sort_by_key(|(micros, _, _)| std::cmp::Reverse(*micros));
    println!(
        "parsed {} file(s), {:.1} MB in {:?} — {:.2} MB/s",
        worst.len(),
        total_bytes as f64 / (1024.0 * 1024.0),
        total_parse,
        mbps(total_bytes, total_parse)
    );
    for (micros, which, bytes) in worst.iter().take(5) {
        println!(
            "  {:>7.1} ms  {:>6.1} MB/s  {}",
            *micros as f64 / 1000.0,
            mbps(*bytes, std::time::Duration::from_micros(*micros as u64)),
            which
        );
    }

    // **How much of the real code it actually accepts**, and whether it gives the bytes back unchanged. Two numbers
    // that matter more than any benchmark: a parser that is fast and wrong is not fast, and a "lossless" tree that
    // loses a byte has broken the invariant every layer above it relies on.
    let mut checked = 0usize;
    let mut with_errors = 0usize;
    let mut total_errors = 0usize;
    let mut not_lossless: Vec<String> = Vec::new();
    for path in files.iter().take(120) {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        if text.len() < 256 {
            continue;
        }
        checked += 1;
        let tree = CppParser::parse(&text, ParserConfig::default());
        let errors = tree.get_errors().len();
        if errors > 0 {
            with_errors += 1;
            total_errors += errors;
        }
        if tree.to_source_text() != text {
            not_lossless.push(path.display().to_string());
        }
    }
    println!(
        "\nreal code: {checked} file(s), {with_errors} with syntax error(s) ({total_errors} in total), \
         {} not lossless",
        not_lossless.len()
    );
    for path in not_lossless.iter().take(5) {
        println!("  lost bytes: {path}");

        // **Where the bytes go, when a file is not lossless.** The invariant every layer above relies on is
        // `to_source_text() == the text`, and a violation is a defect rather than a statistic — so the first
        // differing byte and its neighbourhood are printed, which is enough to name the construct.
        if let Ok(text) = std::fs::read_to_string(&path) {
            let tree = CppParser::parse(&text, ParserConfig::default());
            let rebuilt = tree.to_source_text();
            if rebuilt != text {
                // **By line**, because a byte offset in the middle of a header says nothing a reader can act on.
                let file_lines: Vec<&str> = text.lines().collect();
                let tree_lines: Vec<&str> = rebuilt.lines().collect();
                let at = file_lines
                    .iter()
                    .zip(tree_lines.iter())
                    .position(|(left, right)| left != right)
                    .unwrap_or(file_lines.len().min(tree_lines.len()));
                println!(
                    "    the file is {} bytes in {} line(s), the tree gives {} bytes in {} line(s) — first \
                     difference at line {}",
                    text.len(),
                    file_lines.len(),
                    rebuilt.len(),
                    tree_lines.len(),
                    at + 1
                );
                for (label, lines) in [("file", &file_lines), ("tree", &tree_lines)] {
                    for offset in 0..4 {
                        let line = at.saturating_sub(1) + offset;
                        if let Some(text) = lines.get(line) {
                            println!(
                                "    {label} {}: {}",
                                line + 1,
                                text.escape_debug().to_string()
                            );
                        }
                    }
                }
            }
        }
    }

    // **2. Weight** — the largest file, and what its tree is made of.
    let biggest = files
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok().map(|text| (path, text)))
        .max_by_key(|(_, text)| text.len());
    if let Some((path, text)) = biggest {
        let tree = CppParser::parse(&text, ParserConfig::default());
        let (nodes, tokens, token_bytes) = weigh(&tree);
        // The immutable side is one allocation per token (its bytes) and one per node (its children), each with an
        // `Arc` header and allocator padding. The estimate below says what that adds up to at 16 bytes of overhead
        // per allocation, which is the Windows heap's granularity.
        let token_allocations = tokens * (24 + 16 + ((token_bytes / tokens.max(1)).max(8)));
        let node_allocations = nodes * (24 + 16);
        println!(
            "\n{} — {:.2} MB, {nodes} node(s), {tokens} token(s), {} bytes of token text",
            path.display(),
            text.len() as f64 / (1024.0 * 1024.0),
            token_bytes
        );
        println!(
            "  green tree ≈ {:.1} MB ({:.1}× the file): token allocations ≈ {:.1} MB, node allocations ≈ {:.1} MB",
            (token_allocations + node_allocations) as f64 / (1024.0 * 1024.0),
            (token_allocations + node_allocations) as f64 / text.len().max(1) as f64,
            token_allocations as f64 / (1024.0 * 1024.0),
            node_allocations as f64 / (1024.0 * 1024.0),
        );

        // **The asymptotic rate**, which the per-file numbers hide: a translation unit opened once is small, and a
        // *rendering* is a whole closure. Concatenating real sources gives a few megabytes of real code without
        // needing a preprocessor in the probe.
        let mut big = String::new();
        for path in files.iter() {
            if big.len() > 4 * 1024 * 1024 {
                break;
            }
            if let Ok(text) = std::fs::read_to_string(path) {
                big.push_str(&text);
                big.push('\n');
            }
        }
        let started = Instant::now();
        let lexed = cpp_parser::lex(&big, &cpp_parser::LexerConfig::default());
        let lexing_took = started.elapsed();
        println!(
            "lexing alone: {} token(s) in {:?} — {:.2} MB/s",
            lexed.0.len(),
            lexing_took,
            mbps(big.len(), lexing_took)
        );
        let started = Instant::now();
        let big_tree = CppParser::parse(&big, ParserConfig::default());
        let parse_took = started.elapsed();
        let (big_nodes, big_tokens, _) = weigh(&big_tree);
        println!(
            "large unit: {:.2} MB, {big_nodes} node(s), {big_tokens} token(s), parsed in {:?} — {:.2} MB/s",
            big.len() as f64 / (1024.0 * 1024.0),
            parse_took,
            mbps(big.len(), parse_took)
        );

        // **Does rowan's own interning pay for itself?** `ParserConfig` has carried a `NodeCache` since it was
        // written and nothing has ever passed one, so identical subtrees — `int` declared ten thousand times in a
        // standard header — are ten thousand allocations. This is the experiment that says whether that matters.
        {
            let mut cache = rowan::NodeCache::default();
            let config = ParserConfig::new(cpp_parser::CppLanguageLevel::Cpp20, Some(&mut cache));
            let started = Instant::now();
            let cached = CppParser::parse(&big, config);
            let took = started.elapsed();
            let (cached_nodes, cached_tokens, _) = weigh(&cached);
            // The weight is the same either way — the same number of *distinct* elements is walked — so what an
            // interned tree saves is allocations, and what it costs is the cache. Time is the honest measure.
            println!(
                "  with a NodeCache: {:?} — {:.2} MB/s, {cached_nodes} node(s), {cached_tokens} token(s)",
                took,
                mbps(big.len(), took)
            );
        }

        // **How much of that is the tree and how much is the grammar.** The event stream is the grammar's whole
        // output, so building a second tree from it — with the same builder the parser uses — is the rowan share of
        // the cost, measured rather than estimated.
        let (_, events) = CppParser::parse_with_events(&big, ParserConfig::default());
        let started = Instant::now();
        let mut builder = cpp_parser::CppTreeBuilder::new(&big, events, None);
        builder.build();
        let green = builder.finish();
        let building_took = started.elapsed();
        println!(
            "  tree building alone: {:?} — {:.1}% of the parse, {:.2} MB/s",
            building_took,
            building_took.as_secs_f64() / parse_took.as_secs_f64() * 100.0,
            mbps(big.len(), building_took)
        );
        let _ = green;

        // **Does the cost stay linear?** A parser with a speculation that re-walks what it already read shows up as a
        // falling MB/s here and nowhere else — the shape of the two quadratic defects this grammar has already had
        // (the closed-marker scan and the annotation follower walk). One construct, four sizes.
        println!("\nscaling, one construct repeated:");
        for count in [2000usize, 4000, 8000, 16000] {
            let mut text = String::with_capacity(count * 22);
            for at in 0..count {
                text.push_str(&format!("int f{at}(int a);\n"));
            }
            let started = Instant::now();
            let _ = CppParser::parse(&text, ParserConfig::default());
            let took = started.elapsed();
            println!(
                "  {count:>6} declaration(s) {:>7.2} MB: {:>8.2?} — {:>7.2} MB/s",
                text.len() as f64 / (1024.0 * 1024.0),
                took,
                mbps(text.len(), took)
            );
        }
        // And the same for a template-heavy shape, where the parser speculates: `T<U> x;` is a declaration only if
        // the `<` opens a template argument list, and every one of them is tried.
        println!("scaling, one template declaration repeated:");
        for count in [2000usize, 4000, 8000, 16000] {
            let mut text = String::with_capacity(count * 40);
            for at in 0..count {
                text.push_str(&format!(
                    "template <class T> void g{at}(Box<T> &b) {{ b.get(); }}\n"
                ));
            }
            let started = Instant::now();
            let _ = CppParser::parse(&text, ParserConfig::default());
            let took = started.elapsed();
            println!(
                "  {count:>6} declaration(s) {:>7.2} MB: {:>8.2?} — {:>7.2} MB/s",
                text.len() as f64 / (1024.0 * 1024.0),
                took,
                mbps(text.len(), took)
            );
        }
        let started = Instant::now();
        let red = big_tree.get_red_root();
        let mut count = 0usize;
        for _node in red.descendants() {
            count += 1;
        }
        let walk_took = started.elapsed();
        println!(
            "red walk over {count} node(s): {:?} — {:.1} ns/node",
            walk_took,
            walk_took.as_nanos() as f64 / count.max(1) as f64
        );

        // **The red tree is built, not stored.** What a consumer that walks it pays, per walk.
        let started = Instant::now();
        let red = tree.get_red_root();
        let mut seen = 0usize;
        for node in red.descendants() {
            if cpp_parser::CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ErrorNode {
                seen += 1;
            }
        }
        println!(
            "  red walk (descendants): {:?} for {nodes} node(s), {seen} error node(s)",
            started.elapsed()
        );
    }
}
