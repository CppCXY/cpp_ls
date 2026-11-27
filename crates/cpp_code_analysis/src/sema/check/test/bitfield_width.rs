//! **`bitfield-width`** — a bitfield wider than the type it is declared with.
//!
//! The silence half is the contract: a width that fits, a zero width (legal padding), and a type this layer cannot
//! size — a struct, a typedef, an enum — are all left alone.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **A width the type cannot hold.** C11 6.7.2.1p4, and every compiler refuses it.
#[test]
fn a_bitfield_wider_than_its_type_is_reported() {
    let codes = codes_of("struct flags {\n    unsigned int wide : 40;\n};\n");
    assert_reported(
        &codes,
        DiagnosticCode::BitfieldWidth,
        "an `unsigned int` is 32 bits and this asks for 40",
    );

    let by_a_byte = codes_of("struct flags {\n    unsigned char wide : 9;\n};\n");
    assert_reported(
        &by_a_byte,
        DiagnosticCode::BitfieldWidth,
        "a `char` is eight bits and this asks for nine",
    );
}

/// **A width that fits, and the legal zero width**, are silent.
#[test]
fn a_width_that_fits_is_silent() {
    let fits = codes_of(
        "struct flags {\n    unsigned int a : 1;\n    unsigned int b : 31;\n    unsigned int c : 32;\n};\n",
    );
    assert_silent_about(
        &fits,
        DiagnosticCode::BitfieldWidth,
        "1, 31 and 32 all fit an `unsigned int`",
    );

    let zero = codes_of("struct flags {\n    unsigned int padding : 0;\n};\n");
    assert_silent_about(
        &zero,
        DiagnosticCode::BitfieldWidth,
        "a zero-width bitfield is legal: it pads to the next unit",
    );
}

/// **A type this layer cannot size is not judged** — a struct, a typedef, an enum. The size is the target's, and
/// guessing it is the direction that costs a false finding.
#[test]
fn a_type_this_layer_cannot_size_is_not_judged() {
    let named =
        codes_of("struct base { int x; };\nstruct flags {\n    struct base part : 40;\n};\n");
    assert_silent_about(
        &named,
        DiagnosticCode::BitfieldWidth,
        "a struct's size is not this layer's to guess",
    );

    let typedefed =
        codes_of("typedef unsigned int word;\nstruct flags {\n    word wide : 40;\n};\n");
    assert_silent_about(
        &typedefed,
        DiagnosticCode::BitfieldWidth,
        "a typedef is a name, and the size behind it is not visible here",
    );

    let an_enum =
        codes_of("enum colour { RED, GREEN };\nstruct flags {\n    enum colour shade : 40;\n};\n");
    assert_silent_about(
        &an_enum,
        DiagnosticCode::BitfieldWidth,
        "an enumeration's width is the compiler's choice",
    );
}
