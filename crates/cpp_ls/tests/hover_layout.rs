//! **The layout a class's hover shows** — `sizeof`, the alignment, and where every data member sits.
//!
//! The card is the one a reader asks for and no reading answers: `sizeof` gives the first number and nothing in an
//! editor gives the offsets, which is exactly what rust-analyzer puts on a struct and this server did not.

use std::path::PathBuf;

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

/// The card for the declaration `needle` names, from a one-file project.
fn the_card(source: &str, needle: &str) -> String {
    let root: PathBuf = std::env::temp_dir().join(format!(
        "cppls-hover-layout-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let file = root.join("a.cpp");
    std::fs::write(&file, source).expect("the fixture writes");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, source);
    session.catch_up(&file);
    let _ = session.cook(&file);
    let _ = session.index_everything();

    let view = session.view_of_the_file(&file).expect("the written text");
    // **On the name**, not the keyword: `struct` has a card of its own about the keyword, and the one this asks about
    // is the type it names.
    let start = source.find(needle).expect("the fixture writes the needle");
    let at = start + needle.rfind(' ').map_or(0, |space| space + 1);
    let hover = cpp_ls::handlers::hover::hover(
        &session,
        &view,
        ReadingOffset::new(at),
        FileOffset::new(at),
    )
    .expect("the declaration has a card");
    let text = match hover.contents {
        lsp_types::HoverContents::Markup(markup) => markup.value,
        other => format!("{other:?}"),
    };
    let _ = std::fs::remove_dir_all(&root);
    text
}

/// **A struct's card says what it costs**: 4 + pad + 8 is 16 bytes, and the `double` is at offset 8 — not 4.
#[test]
fn a_structs_card_has_its_size_alignment_and_offsets() {
    let card = the_card(
        "struct Widget {\n    int x;\n    double y;\n};\n",
        "struct Widget",
    );
    assert!(card.contains("**Layout**"), "the block is on the card: {card}");
    assert!(
        card.contains("size 16 byte(s), alignment 8"),
        "the two numbers `sizeof` and `alignof` give: {card}"
    );
    assert!(
        card.contains("x") && card.contains("offset   0, 4 byte(s)"),
        "the first member is at 0: {card}"
    );
    assert!(
        card.contains("offset   8, 8 byte(s)"),
        "the `double` is at 8, which is the number no reading gives: {card}"
    );
}

/// **A union is named as one**, and every member is at 0.
#[test]
fn a_unions_card_says_so_and_puts_every_member_at_zero() {
    let card = the_card(
        "union Value {\n    int i;\n    double d;\n};\n",
        "union Value",
    );
    assert!(card.contains("size 8 byte(s), alignment 8, a union"), "{card}");
    assert!(
        card.contains("offset   0, 4 byte(s)") && card.contains("offset   0, 8 byte(s)"),
        "both members start at the object's first byte: {card}"
    );
}

/// **A class whose layout cannot be computed says which member stopped it**, rather than showing numbers that are
/// wrong about every member after it.
#[test]
fn a_bitfield_withholds_the_numbers_and_names_the_member() {
    let card = the_card(
        "struct Flags {\n    unsigned a : 3;\n    int tail;\n};\n",
        "struct Flags",
    );
    assert!(
        card.contains("not computed: `a`"),
        "the member is named: {card}"
    );
    assert!(
        !card.contains("offset"),
        "and no offset is shown for a layout that cannot be trusted: {card}"
    );
}
