//! What folds, what does not, and why the difference matters while a file is being typed.
//!
//! Every rule here is asked of a file's **own** tokens and directives, so these tests are the whole contract: what a
//! client will be told it may hide, and — the half that is easy to get wrong — what it will not.

use cpp_code_analysis::folding::{FoldKind, folding_ranges};
use cpp_parser::{CppTokenData, LexerConfig, lex};

fn folds_of(source: &str) -> Vec<(FoldKind, usize, String)> {
    let (tokens, _): (Vec<CppTokenData>, _) = lex(source, &LexerConfig::default());

    folding_ranges(source, &tokens)
        .into_iter()
        .map(|fold| {
            (
                fold.kind,
                fold.range.start_offset,
                source[fold.range.start_offset..fold.range.end_offset()].to_string(),
            )
        })
        .collect()
}

fn kinds_of(source: &str) -> Vec<FoldKind> {
    folds_of(source)
        .into_iter()
        .map(|(kind, _, _)| kind)
        .collect()
}

/// **Every brace pair that spans more than one line**, nested ones included, and nothing else.
///
/// A one-line body is not a fold: hiding it would hide the whole declaration. And the nesting is the stack's doing
/// rather than the grammar's, which is what lets this work on a file the parser recovered from.
#[test]
fn brace_pairs_span_more_than_one_line() {
    let source = "struct Widget {\n    int size;\n    void grow() {\n        ++size;\n    }\n};\n\
                  struct Empty {};\n\
                  int one_line() { return 1; }\n";

    assert_eq!(
        kinds_of(source),
        vec![FoldKind::Code, FoldKind::Code],
        "the class and the member function, and neither of the one-line bodies"
    );

    let found = folds_of(source);
    assert!(
        found[0].2.starts_with('{') && found[0].2.ends_with('}'),
        "the fold is the brace pair itself, which is what a client draws a fold marker against: {found:?}"
    );
    assert!(
        found[0].2.contains("void grow()"),
        "the class's pair covers its members: {found:?}"
    );
    assert!(
        found[1].2.contains("++size"),
        "and the member function's covers its body: {found:?}"
    );
}

/// **An unclosed brace is not a fold** — the file is being typed, not broken.
///
/// The tempting wrong answer is "from this `{` to the end of the file", which folds away everything the user is
/// working on. The same applies to a stray `}`: it closes nothing, so it opens nothing.
#[test]
fn an_unbalanced_brace_folds_nothing() {
    let source = "void f() {\n    int x;\n";

    assert_eq!(
        kinds_of(source),
        Vec::<FoldKind>::new(),
        "a region that has not been written yet is not a region"
    );

    let stray = "int after;\n}\n";
    assert_eq!(
        kinds_of(stray),
        Vec::<FoldKind>::new(),
        "and a closing brace with nothing to close is not one either"
    );

    // The balanced part of a half-written file still folds, which is what makes this useful while typing.
    let half_typed = "struct Good {\n    int x;\n};\nstruct BeingTyped {\n    int y;\n";
    assert_eq!(
        kinds_of(half_typed),
        vec![FoldKind::Code],
        "the class that is finished folds; the one that is not, does not"
    );
}

/// **Comment runs**, including the one that is multi-line on its own.
///
/// A blank line ends a run — two comment blocks are two blocks — and a single `//` line is not a fold, because
/// folding it hides nothing.
#[test]
fn comment_runs_fold_and_a_blank_line_ends_one() {
    let source = "// one\n// two\n// three\nint after;\n";
    assert_eq!(kinds_of(source), vec![FoldKind::Comment]);
    assert_eq!(
        folds_of(source)[0].2,
        "// one\n// two\n// three",
        "the run is folded as one region"
    );

    let separated = "// one\n\n// two\n// three\n";
    assert_eq!(
        kinds_of(separated),
        vec![FoldKind::Comment],
        "the blank line splits them: only the second run is longer than a line"
    );

    let multi_line = "/*\n * a block comment\n */\nint after;\n";
    assert_eq!(
        kinds_of(multi_line),
        vec![FoldKind::Comment],
        "one token, five lines, one fold"
    );

    let single = "// just this\nint after;\n";
    assert_eq!(
        kinds_of(single),
        Vec::<FoldKind>::new(),
        "one line is not a region"
    );
}

/// **`#if` regions and runs of `#include`s**, from the directives rather than from the tokens.
///
/// The region fold is what a C++ reader wants most: the branch that is not theirs, out of the way. Its `#else` is
/// inside it rather than a fold of its own — one region is what a client draws.
#[test]
fn conditionals_and_include_runs_fold() {
    let source = "#include <string>\n#include <vector>\nint after;\n";
    assert_eq!(kinds_of(source), vec![FoldKind::Imports]);
    assert_eq!(folds_of(source)[0].2, "#include <string>\n#include <vector>");

    let one_include = "#include <string>\n\n#include <vector>\n";
    assert_eq!(
        kinds_of(one_include),
        Vec::<FoldKind>::new(),
        "a blank line between them ends the run, and a single include is one line"
    );

    let conditional = "#if defined(_WIN32)\nint win;\n#else\nint other;\n#endif\nint after;\n";
    assert_eq!(kinds_of(conditional), vec![FoldKind::Region]);
    let folded = &folds_of(conditional)[0].2;
    assert!(folded.starts_with("#if defined(_WIN32)"), "{folded:?}");
    assert!(folded.ends_with("#endif"), "the whole region: {folded:?}");

    let unterminated = "#if defined(_WIN32)\nint win;\n";
    assert_eq!(
        kinds_of(unterminated),
        Vec::<FoldKind>::new(),
        "an `#if` with no `#endif` is a file being typed"
    );
}

/// **Everything comes back in source order**, whichever rule found it.
///
/// A client draws the folds in the order it receives them, so an order that grouped them by rule would draw a
/// comment fold above a class that starts before it.
#[test]
fn folds_are_in_source_order() {
    let source = "#include <string>\n#include <vector>\n\
                  // a note\n// and another\n\
                  struct Widget {\n    int size;\n};\n\
                  #if X\nint win;\n#endif\n";
    let kinds = kinds_of(source);

    assert_eq!(
        kinds,
        vec![
            FoldKind::Imports,
            FoldKind::Comment,
            FoldKind::Code,
            FoldKind::Region
        ],
        "in the order the file writes them"
    );

    let found = folds_of(source);
    let starts: Vec<usize> = found.iter().map(|(_, start, _)| *start).collect();
    assert!(
        starts.windows(2).all(|pair| pair[0] < pair[1]),
        "and their offsets really are ordered: {starts:?}"
    );
}
