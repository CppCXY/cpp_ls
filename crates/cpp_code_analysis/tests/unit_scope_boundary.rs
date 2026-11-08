//! **A scope may not cross a file boundary** — the rule a stitched unit's parse is built on.
//!
//! The unit indexer hands the parser the first token of every file after the first
//! (`RenderedUnit::file_boundaries`), and the parser is supposed to end a block at that token rather than pair a `{`
//! written in one file with a `}` written in another. MSVC's standard library makes this the difference between a
//! correct index and a wrong one: a header that opens `namespace std {` and does not close it — every one of the
//! `_STD_BEGIN`/`_STD_END` pairs, when the pair is split by a conditional our evaluator decides differently — must not
//! hold the files spliced after it.
//!
//! The failure this pins was measured on the reader's project as **6 732 `_mm*` intrinsics filed under `std`**:
//! `intrin.h` read on its own puts them at global scope, and read inside the std unit the index said `std`. The cause
//! is here, in the parse, and the test is a two-file rendering rather than a program read because a program read takes
//! minutes and this takes microseconds.

use cpp_parser::{CppAstNode, ParserConfig};
use cpp_parser::{CppParser, CppSyntaxNode};

/// The qualified scope a declaration's name sits in, by walking the tree's ancestors.
fn scopes_around(root: &CppSyntaxNode, name_offset: usize) -> Vec<String> {
    let mut scopes = Vec::new();
    let mut found = None;
    for node in root.descendants() {
        let range = cpp_parser::source_range(node.text_range());
        if range.start_offset <= name_offset && name_offset <= range.end_offset() {
            found = Some(node);
        }
    }
    let Some(mut node) = found else {
        return scopes;
    };
    loop {
        if let Some(namespace) = cpp_parser::CppNamespaceDecl::cast(node.clone())
            && let Some(name) = namespace.get_name_text()
        {
            scopes.push(name);
        }
        match node.parent() {
            Some(parent) => node = parent,
            None => break,
        }
    }
    scopes.reverse();
    scopes
}

/// **A `namespace` a file opens and does not close does not hold the next file's declarations.**
#[test]
fn an_unclosed_namespace_does_not_hold_the_next_file() {
    // File one, as MSVC's stream has it: `_STD_BEGIN` expanded, `_STD_END` never reached (the conditional our
    // evaluator decided differently from the compiler's).
    let first = "namespace std {\nint inside_the_namespace;\n";
    let second = "\nint a_global_intrinsic(int);\n";
    let text = format!("{first}{second}");
    // The boundary is the first token of file two, which is `int` of `a_global_intrinsic` — the same offset
    // `RenderedUnit::file_boundaries` reports.
    let boundary = text.find("a_global_intrinsic").expect("the fixture") - "int ".len();

    let tree = CppParser::parse(
        &text,
        ParserConfig::default().with_file_boundaries(vec![boundary]),
    );
    let root = tree.get_red_root();

    let inside = text.find("inside_the_namespace").expect("the fixture");
    assert_eq!(
        scopes_around(&root, inside),
        vec!["std".to_string()],
        "a declaration written between the braces is in the namespace"
    );

    let after = text.find("a_global_intrinsic").expect("the fixture");
    assert!(
        scopes_around(&root, after).is_empty(),
        "the next file's declaration is not in the namespace the previous one left open: {:?}",
        scopes_around(&root, after)
    );
}
