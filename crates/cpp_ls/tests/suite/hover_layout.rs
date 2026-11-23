//! **The layout a class's hover shows** — `sizeof`, the alignment, and where every data member sits.
//!
//! The card is the one a reader asks for and no reading answers: `sizeof` gives the first number and nothing in an
//! editor gives the offsets, which is exactly what rust-analyzer puts on a struct and this server did not.

use std::path::PathBuf;

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::{
    CompilerConfig, DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// The card for the declaration `needle` names, from a one-file project.
fn the_card(source: &str, needle: &str) -> String {
    let root: PathBuf = crate::common::a_temp_directory("hover-layout");
    let file = root.join("a.cpp");
    std::fs::write(&file, source).expect("the fixture writes");

    // **A stated configuration, not a discovered compiler.** The card's layout rules are the x64 ones, and the
    // bitfield rules are the GNU/Itanium ones; a session that discovers whichever compiler is installed on the test
    // machine would answer differently per machine. `CompilerConfig::default()` is the GNU dialect this test pins.
    let mut session = Session::with_config(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        CompilerConfig::default(),
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

/// **A `_BitInt` struct with bitfields is priced, not withheld**: the card from the report says 6 bytes and names the
/// bit ranges, because the width is in the file and the type's size is a function of N.
#[test]
fn a_bitint_bitfield_structs_card_has_its_bits() {
    let card = the_card(
        "\
struct bits {
    unsigned _BitInt(3) three : 3;
    _BitInt(13) signed_wide : 13;
    _BitInt(5) plain;
    unsigned _BitInt(4) other : 2;
};
",
        "struct bits",
    );
    assert!(
        card.contains("size 6 byte(s), alignment 2"),
        "measured with gcc: {card}"
    );
    assert!(
        card.contains("offset   2, 2 byte(s) (bits 16..29)"),
        "the 13-bit field's unit is at byte 2: {card}"
    );
    assert!(
        card.contains("offset   5, 1 byte(s) (bits 40..42)"),
        "the last field takes two bits of byte 5: {card}"
    );
    assert!(
        !card.contains("not computed"),
        "the layout is answered: {card}"
    );
}

/// **A struct's card says what it costs**: 4 + pad + 8 is 16 bytes, and the `double` is at offset 8 — not 4.
#[test]
fn a_structs_card_has_its_size_alignment_and_offsets() {
    let card = the_card(
        "struct Widget {\n    int x;\n    double y;\n};\n",
        "struct Widget",
    );
    assert!(
        card.contains("**Layout**"),
        "the block is on the card: {card}"
    );
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
    assert!(
        card.contains("size 8 byte(s), alignment 8, a union"),
        "{card}"
    );
    assert!(
        card.contains("offset   0, 4 byte(s)") && card.contains("offset   0, 8 byte(s)"),
        "both members start at the object's first byte: {card}"
    );
}

/// **A bare `unsigned` is four bytes** — `struct dim3 { unsigned x, y, z; }` could not be laid out at all, so its card
/// named all three members as unsized. Reported from a CUDA project, where `dim3` is the launch-configuration type.
#[test]
fn a_member_written_unsigned_is_sized() {
    let card = the_card(
        "struct Dim {\n    unsigned x, y, z;\n    Dim(unsigned a = 1);\n};\n",
        "struct Dim",
    );
    assert!(
        card.contains("size 12 byte(s), alignment 4"),
        "three `unsigned`s are twelve bytes: {card}"
    );
    assert!(
        card.contains("offset   0, 4 byte(s)")
            && card.contains("offset   4, 4 byte(s)")
            && card.contains("offset   8, 4 byte(s)"),
        "each member is four bytes after the last: {card}"
    );
}

/// **A bitfield is placed, not withheld** — its width is read from the file, and the storage-unit rule puts the next
/// member where the compiler does.
#[test]
fn a_bitfield_is_placed_by_its_storage_unit() {
    let card = the_card(
        "struct Flags {\n    unsigned a : 3;\n    int tail;\n};\n",
        "struct Flags",
    );
    assert!(
        card.contains("size 8 byte(s), alignment 4"),
        "`a` takes a four-byte unit and `tail` follows it: {card}"
    );
    assert!(
        card.contains("offset   0, 4 byte(s) (bits 0..3)"),
        "the bit range is shown: {card}"
    );
    assert!(
        card.contains("offset   4, 4 byte(s)"),
        "the next member starts after the unit: {card}"
    );
}
