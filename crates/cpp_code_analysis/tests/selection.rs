//! The chain of ranges an editor walks when the user expands a selection.
//!
//! Two properties make it a *chain* rather than a list of ranges, and both are pinned here: every range contains the
//! cursor, and each one **strictly** contains the one before it. A chain that repeats a range is a keystroke that
//! does nothing, and a chain with a range that does not contain the cursor is the client being asked to select
//! something other than what the user is looking at.

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// The chain at an offset in a file, as `(start, text)` pairs — the text is what a reader would see selected.
fn chain_at(source: &str, offset: usize) -> Vec<(usize, String)> {
    let files = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.load("/p/a.cpp");
    let view = session.view("/p/a.cpp").expect("the file is held");

    view.selection_chain(offset)
        .into_iter()
        .map(|range| {
            (
                range.start_offset,
                source[range.start_offset..range.end_offset()].to_string(),
            )
        })
        .collect()
}

/// **From the word under the cursor outwards**, one strictly larger range at a time, ending at the whole file.
///
/// The ladder of a real cursor: a member's name, the member access, the expression statement, the body, the function
/// definition, and the file itself. Nothing in it is skipped and nothing is repeated.
#[test]
fn a_selection_chain_widens_one_step_at_a_time() {
    let source = "struct Widget {\n    int size;\n};\nint f(Widget& w) {\n    return w.size;\n}\n";
    let at_the_member = source.find("w.size").expect("the fixture uses it") + 2;

    let chain = chain_at(source, at_the_member);
    // Trimmed, because a node's range carries the trivia that follows it — the newline after a statement is part of
    // the statement's own tokens, not of the next one. What a client selects is the same text either way, and
    // asserting on the trimmed form keeps this test about the **rungs** rather than about rowan's trivia rule.
    let texts: Vec<&str> = chain
        .iter()
        .map(|(_, text)| text.trim_end())
        .collect();

    assert_eq!(
        texts.first().copied(),
        Some("size"),
        "the word under the cursor comes first: {chain:?}"
    );
    assert_eq!(
        texts.last().copied(),
        Some(source.trim_end()),
        "and the whole file last: {chain:?}"
    );
    assert!(
        texts.contains(&"w.size"),
        "the member access is one rung: {chain:?}"
    );
    assert!(
        texts.contains(&"return w.size;"),
        "so is the statement: {chain:?}"
    );
    assert!(
        texts.contains(&"int f(Widget& w) {\n    return w.size;\n}"),
        "and the function definition: {chain:?}"
    );
}

/// **Every rung contains the cursor, and every rung is strictly bigger than the one before it.**
///
/// The first is what makes the answer a selection the user recognises; the second is what makes each expansion a
/// keystroke that does something. A recovered tree has nodes whose range equals their parent's, which is where a
/// repeated rung would come from — and a range that does not contain the cursor is what a *guessed* ancestor would
/// be.
#[test]
fn every_rung_contains_the_cursor_and_grows() {
    let source = "namespace ns {\nstruct Widget {\n    int size;\n};\n}\nint f() {\n    ns::Widget w;\n    w.size = 1;\n}\n";

    // Every offset in the file, which is the only way to be sure the two properties hold for the ones a test would
    // not think to pick: the cursor lands on whitespace, on punctuation, at the very end.
    for offset in 0..source.len() {
        let chain = chain_at(source, offset);
        assert!(
            !chain.is_empty(),
            "every offset has at least the file: {offset} in {source:?}"
        );

        for (start, text) in &chain {
            assert!(
                offset >= *start && offset < start + text.len(),
                "the rung at {offset} does not contain the cursor: {chain:?}"
            );
        }

        for pair in chain.windows(2) {
            let (smaller_start, smaller) = &pair[0];
            let (bigger_start, bigger) = &pair[1];
            assert!(
                *bigger_start <= *smaller_start && bigger.len() >= smaller.len(),
                "the rungs have to grow: {chain:?} at {offset}"
            );
            assert!(
                !(bigger_start == smaller_start && bigger.len() == smaller.len()),
                "and strictly, or the expansion does nothing: {chain:?} at {offset}"
            );
        }
    }
}

/// **A cursor in whitespace starts at what surrounds it**, not at the whitespace.
///
/// There is no word under the cursor there, so the first useful rung is the enclosing construct — selecting the
/// blank run between two members would be a selection the user cannot see.
#[test]
fn a_cursor_in_whitespace_starts_at_the_enclosing_construct() {
    let source = "struct Widget {\n    int size;\n\n    int other;\n};\n";
    let blank_line = source.find("\n\n").expect("the fixture has one") + 1;

    let chain = chain_at(source, blank_line);
    let first = &chain[0].1;

    assert!(
        !first.trim().is_empty(),
        "the first rung is not whitespace: {chain:?}"
    );
    assert!(
        first.contains("int other;") || first.contains("int size;"),
        "it is the declaration the blank line is between or the class that holds them: {chain:?}"
    );
}
