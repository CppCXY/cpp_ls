//! **A parse of a run of tokens, not of a whole text** — the enabling refactor for the split in
//! `docs/parse-architecture.md`.
//!
//! An editor reparses because one file changed. The stream it parses is the **stitched** translation unit — that file's
//! text with every include's text macro-expanded into it — and for `#include <vector>` that is tens of thousands of
//! lines that the keystroke did not touch. So the parser has to be able to parse a **range of that stream's tokens**,
//! and the property that makes the split sound is the one asserted here: a run that starts and ends at a top-level
//! construct's own tokens must produce the shape the whole-stream parse produces for that construct.
//!
//! Two things are *deliberately* different from a whole-text parse, and both are pinned below rather than left to be
//! discovered:
//!
//! * **The tree's node ranges are relative to the run**, because a green tree is built from token lengths. The token
//!   ranges are not: they are the ones the lexer produced, so the anchor is `tokens[0].range.start_offset`.
//! * **The grammar sees end-of-input after the last token.** A cut in the middle of a construct is a truncated
//!   construct, which is a legitimate tree and a broken answer — the caller's job is to cut where it can answer for.

use cpp_parser::{
    CppParser, CppSyntaxElement, CppSyntaxKind, CppSyntaxNode, CppTokenKind, ParserConfig,
    is_trivia,
};

/// A file with several self-contained top-level constructs, and the shapes that used to break the split:
/// a `>>` that closes two template lists, a constructor with a member-initialiser list, a doc comment inside a class
/// body, and a template declaration.
const SOURCE: &str = r#"// A counter, with the comment kept.
namespace outer {
/// The count, and the doc comment that belongs to the member.
class Counter {
public:
    explicit Counter(int start = 0) : at_(start) {}
    int next() { return ++at_; }

private:
    int at_;
};
}

template <typename T>
T twice(T value) {
    return value + value;
}

int use(int n) {
    int total = 0;
    for (int i = 0; i < n; ++i) {
        if (i % 2 == 0) {
            total += twice(i);
        }
    }
    std::vector<std::vector<int>> grid;
    return total + grid.size();
}
"#;

/// The shape of a tree: node kinds and the significant tokens under them, with trivia left out.
///
/// Trivia is left out because its *placement at a node's edges* is the one thing a slice cannot reproduce: the whole
/// stream hands the layout between two constructs to the translation unit, and a run that begins after that layout
/// has none to hand over. The significant tokens and their nesting are the whole of the claim.
fn shape(node: &CppSyntaxNode) -> String {
    let mut out = String::new();
    write_shape(&mut out, node);
    out
}

fn write_shape(out: &mut String, node: &CppSyntaxNode) {
    let kind = CppSyntaxKind::from(node.kind());
    out.push_str(&format!("{kind:?}["));
    for element in node.children_with_tokens() {
        match element {
            CppSyntaxElement::Node(child) => write_shape(out, &child),
            CppSyntaxElement::Token(token) => {
                let kind = CppTokenKind::from(token.kind());
                if is_trivia(kind) {
                    continue;
                }
                out.push_str(&format!(" {kind:?}({:?})", token.text()));
            }
        }
    }
    out.push_str(" ]");
}

/// The byte span a run of tokens spells.
fn span_of(tokens: &[cpp_parser::CppTokenData]) -> std::ops::Range<usize> {
    tokens
        .first()
        .expect("a run is not empty")
        .range
        .start_offset
        ..tokens
            .last()
            .expect("a run is not empty")
            .range
            .end_offset()
}

/// **The range entry point with the whole stream is the same parse** — the control for everything below.
///
/// If this did not hold, a difference in the per-construct test could be the entry point's fault rather than the cut's.
#[test]
fn the_whole_stream_through_the_range_entry_point_is_the_same_parse() {
    let whole = CppParser::parse(SOURCE, ParserConfig::default());
    assert!(
        whole.get_errors().is_empty(),
        "the source itself must parse cleanly, or this test is measuring the grammar: {:?}",
        whole.get_errors()
    );

    let (sliced, audit) =
        CppParser::parse_tokens_with_audit(SOURCE, whole.get_tokens(), ParserConfig::default());

    assert!(
        audit.is_balanced(),
        "the event stream is unbalanced: {audit:?}"
    );
    assert_eq!(
        sliced.get_tokens(),
        whole.get_tokens(),
        "the run is the stream"
    );
    assert_eq!(sliced.to_source_text(), SOURCE, "I1 for the run");
    assert_eq!(
        shape(&sliced.get_red_root()),
        shape(&whole.get_red_root()),
        "a run of every token must be the same tree"
    );
}

/// **A top-level construct parses from its own tokens exactly as it does inside the file.**
///
/// This is the property the split rests on: the include region and the file's own region are cuts of one stream, and a
/// cut at a construct boundary must not change that construct's shape.
#[test]
fn every_top_level_construct_parses_the_same_from_its_own_tokens() {
    let whole = CppParser::parse(SOURCE, ParserConfig::default());
    let tokens = whole.get_tokens();
    let root = whole.get_red_root();

    let mut checked = 0usize;
    for child in root.children() {
        let range = child.text_range();
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        let first = tokens.partition_point(|token| token.range.start_offset < start);
        let last = tokens.partition_point(|token| token.range.start_offset < end);
        if first >= last {
            continue;
        }

        // A node covers exactly its own tokens: if this failed, "the run of a construct" would not be well defined and
        // the comparison below would be about a different text than the child's.
        assert_eq!(
            tokens[first].range.start_offset, start,
            "the construct at {start}..{end} does not start at a token"
        );
        assert_eq!(
            tokens[last - 1].range.end_offset(),
            end,
            "the construct at {start}..{end} does not end at a token"
        );

        let run = &tokens[first..last];
        let (tree, audit) =
            CppParser::parse_tokens_with_audit(SOURCE, run, ParserConfig::default());
        let construct = format!("{:?} at {start}..{end}", CppSyntaxKind::from(child.kind()));

        assert!(
            audit.is_balanced(),
            "{construct}: unbalanced event stream: {audit:?}"
        );
        assert!(
            tree.get_errors().is_empty(),
            "{construct}: a self-contained construct must parse cleanly, got {:?}",
            tree.get_errors()
        );
        assert_eq!(tree.get_tokens(), run, "{construct}: the run is the stream");
        assert_eq!(
            tree.to_source_text(),
            &SOURCE[span_of(run)],
            "{construct}: I1 for the run"
        );

        let sliced_root = tree.get_red_root();
        let children: Vec<CppSyntaxNode> = sliced_root.children().collect();
        assert_eq!(
            children.len(),
            1,
            "{construct}: a run of one construct must give one construct, got {}",
            children.len()
        );
        // **The instrument has teeth**: a shape that ignored node kinds would give the same string for a container and
        // its contents, and then the comparison below could never fail. It must tell the translation unit from the
        // construct inside it.
        assert_ne!(
            shape(&sliced_root),
            shape(&children[0]),
            "{construct}: the shape does not distinguish a node from its child"
        );
        assert_eq!(
            shape(&children[0]),
            shape(&child),
            "{construct}: the same construct must have the same shape from its own tokens"
        );
        checked += 1;
    }

    // The source is supposed to exercise several container shapes; a run over an empty list would pass vacuously.
    assert!(
        checked >= 4,
        "only {checked} top-level construct(s) were checked"
    );
}

/// **Any run of tokens parses losslessly**, however it is cut.
///
/// A cut in the middle of a construct is what an editor sees while it is being typed, and the parser's promise there is
/// the same one it makes for a whole malformed file: terminate, and account for every byte of the run exactly once.
/// What is *not* promised is an error-free tree, so no claim is made about the shape.
#[test]
fn any_run_of_tokens_parses_losslessly() {
    let whole = CppParser::parse(SOURCE, ParserConfig::default());
    let tokens = whole.get_tokens();

    for width in [1usize, 2, 3, 5, 8, 13, 21] {
        for first in (0..tokens.len()).step_by(3) {
            let last = (first + width).min(tokens.len());
            let run = &tokens[first..last];
            let tree = CppParser::parse_tokens(SOURCE, run, ParserConfig::default());

            assert_eq!(
                tree.get_tokens(),
                run,
                "run {first}..{last}: the run is the stream"
            );
            assert_eq!(
                tree.to_source_text(),
                &SOURCE[span_of(run)],
                "run {first}..{last} of width {width} ({} token(s) from {:?}) does not reproduce its bytes",
                run.len(),
                run.first().map(|token| token.kind)
            );
            assert_eq!(
                tree.root_kind(),
                CppSyntaxKind::TranslationUnit,
                "run {first}..{last}: the root must be a translation unit"
            );
        }
    }
}

/// **An empty run is a parse of nothing** — not a panic, and not a tree with a phantom token.
#[test]
fn an_empty_run_is_an_empty_tree() {
    let tree = CppParser::parse_tokens(SOURCE, &[], ParserConfig::default());
    assert_eq!(tree.root_kind(), CppSyntaxKind::TranslationUnit);
    assert_eq!(tree.to_source_text(), "");
    assert!(tree.get_tokens().is_empty());
}

/// **A run reports the grammar's problems about that run, at that run's offsets.**
///
/// The stream was lexed once for the whole text, so a lexical error is a fact about the text that the caller already
/// has. What this tree answers is what the *grammar* made of the run — and its error offsets are the lexer's, which
/// means **absolute**: a consumer that maps them through the same line index as the whole text needs no arithmetic.
#[test]
fn a_run_reports_its_own_grammar_errors_at_absolute_offsets() {
    let whole = CppParser::parse(SOURCE, ParserConfig::default());
    let tokens = whole.get_tokens();

    // A `for` header cut after its first `;`: a statement no production accepts whole.
    let for_at = SOURCE.find("for (").expect("the source has a `for`");
    let cut = SOURCE[for_at..].find(';').expect("the header has a `;`") + for_at;
    let first = tokens.partition_point(|token| token.range.start_offset < for_at);
    let last = tokens.partition_point(|token| token.range.start_offset <= cut);
    let run = &tokens[first..last];
    let span = span_of(run);

    let tree = CppParser::parse_tokens(SOURCE, run, ParserConfig::default());
    assert_eq!(tree.to_source_text(), &SOURCE[span.clone()]);

    let errors = tree.get_errors();
    assert!(
        !errors.is_empty(),
        "a `for` header cut after its first `;` must be reported, not silently accepted"
    );
    for error in errors {
        let (start, end) = error.offsets();
        assert!(
            start >= span.start && end <= span.end,
            "an error at {start}..{end} is outside the run {}..{}: the offsets are not the lexer's",
            span.start,
            span.end
        );
    }
}
