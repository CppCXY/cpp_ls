//! **A designator list, not one designator** — C99 6.7.8's `designator-list`, which C++20 kept.
//!
//! Reported from an editor with a screenshot: `INTEGER_OBJ(input->paste)` — nvim's
//! `#define INTEGER_OBJ(i) ((Object){ .type = kObjectTypeInteger, .data.integer = i })` — reported two
//! "expected primary expression" errors inside its own expansion, and the macro's card showed them. The parser read
//! **one** designator (`.data`) and then expected `=`, so the second `.` was "expected `;` after expression".
//!
//! ```text
//! designator-list: designator                    `.x = 1`
//!                | designator-list designator    `.data.integer = 1`   ← the shape that was missing
//! designator:      `.` identifier | `[` constant-expression `]`
//! ```
//!
//! The second case below is the whole of the fix; the others are the shapes that already worked, kept so that a
//! later change to the loop is a visible change to this list.

use cpp_parser::{CppParser, ParserConfig};

/// Parse `source` as C++ and assert it produces no error.
fn parses(source: &str) {
    let tree = CppParser::parse(source, ParserConfig::default());
    assert!(
        tree.get_errors().is_empty(),
        "{source:?} must parse: {:?}",
        tree.get_errors()
    );
}

#[test]
fn a_designator_list_names_a_subobject_two_levels_down() {
    parses(
        "struct Inner { int integer; };\nstruct Object { int type; Inner data; };\nvoid f(int i) { Object o = (Object){ .type = 1, .data.integer = i }; }\n",
    );
    parses("struct Object { int type; };\nvoid f() { Object o = (Object){ .type = 1 }; }\n");
    // A designator list made of an index and then a field, which is the array half of the same rule.
    parses(
        "struct Inner { int integer; };\nstruct Object { Inner data[2]; };\nvoid f(int i) { Object o = (Object){ .data[0].integer = i }; }\n",
    );
    // …and the C spelling of the same thing, which is where the macro came from.
    parses(
        "typedef struct { int type; struct { int integer; } data; } Object;\nvoid f(int i) { Object o = (Object){ .type = 1, .data.integer = i }; }\n",
    );
}

#[test]
fn a_designated_initializer_stays_one_node() {
    let source = "typedef struct { int type; struct { int integer; } data; } Object;\nvoid f(int i) { Object o = (Object){ .type = 1, .data.integer = i }; }\n";
    let tree = CppParser::parse(source, ParserConfig::default());
    assert_eq!(tree.to_source_text(), source, "the parse stays lossless");
    let designators = tree
        .get_red_root()
        .descendants()
        .filter(|node| format!("{:?}", node.kind()).contains("DesignatedInitExpr"))
        .count();
    assert_eq!(
        designators, 2,
        "one node per element, not one per designator"
    );
}
