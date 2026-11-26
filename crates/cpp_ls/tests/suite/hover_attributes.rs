//! **The specifiers a declaration carries but its fact cannot hold** — the hover card's declaration block.
//!
//! `__attribute__((cold)) int f();`, `[[nodiscard]] int f();` and `NESTED int f();` parse with the attribute or the macro
//! invocation **beside** the declaration rather than inside it, so the fact — and the card generated from it — dropped
//! them. Reported from a CUDA project, where the macros that carry the CUDA annotations are exactly this shape, and the
//! expected card is the one a compiler's diagnostics print: the attributes above, the specifiers in front of the type.

use crate::common;
use std::path::PathBuf;

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

/// The card for the declaration `needle` names, from a one-file project.
fn the_card(source: &str, needle: &str) -> String {
    let root: PathBuf = common::a_temp_directory("hover-attributes");
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
    // **On the name**, not on the attribute: the attribute has a card of its own, and the one this asks about is the
    // declaration it decorates.
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

/// **A GNU attribute is drawn above the declaration**, the way a compiler's diagnostics draw it.
#[test]
fn a_leading_attribute_is_drawn_above_the_declaration() {
    let card = the_card(
        "__attribute__((cold)) int cold(int x) { return x; }\n",
        "int cold(",
    );
    assert!(
        card.contains("__attribute__((cold))\nint cold(int x)"),
        "the attribute is a line of its own above the signature: {card}"
    );
    assert!(
        !card.contains("Attributes:"),
        "and not a paragraph after it: {card}"
    );
}

/// **A standard attribute is drawn above too** — the same sibling node, a different spelling.
#[test]
fn a_standard_attribute_is_drawn_above_the_declaration() {
    let card = the_card(
        "[[nodiscard]] int checked(int x) { return x; }\n",
        "int checked(",
    );
    assert!(card.contains("[[nodiscard]]\nint checked(int x)"), "{card}");
}

/// **An MSVC `__declspec` is drawn above**, because a Windows project writes its export macro this way.
#[test]
fn a_declspec_is_drawn_above_the_declaration() {
    let card = the_card(
        "__declspec(dllexport) int exported(int x) { return x; }\n",
        "int exported(",
    );
    assert!(
        card.contains("__declspec(dllexport)\nint exported(int x)"),
        "{card}"
    );
}

/// **A macro is expanded, and its attributes and specifiers are drawn where they belong** — the reported shape:
///
/// ```text
/// __attribute__((always_inline)) __attribute__((cold))
/// inline int f_flat(int x)
/// ```
#[test]
fn a_macro_expands_into_the_attributes_and_the_specifiers() {
    let card = the_card(
        "#define AI __inline__ __attribute__((always_inline))\n\
         #define FLAT AI __attribute__((cold))\n\
         FLAT int f_flat(int x) { return x; }\n",
        "int f_flat(",
    );
    assert!(
        card.contains(
            "__attribute__((always_inline)) __attribute__((cold))\ninline int f_flat(int x)"
        ),
        "the attributes above, the specifier in front of the type: {card}"
    );
    assert!(
        !card.contains("FLAT"),
        "the macro's own name is not what the declaration means: {card}"
    );
}

/// **A macro with no expansion stays a word** — `__global__` has no `#define` in the file, and the card shows it as the
/// specifier the source wrote rather than inventing an expansion.
#[test]
fn a_macro_without_a_definition_stays_in_the_signature() {
    let card = the_card("__global__ void kernel(int x) { }\n", "void kernel(");
    assert!(card.contains("__global__ void kernel(int x)"), "{card}");
}

/// **A declaration with nothing written before it keeps the plain block** — no empty attribute line.
#[test]
fn a_declaration_without_attributes_has_one_line() {
    let card = the_card("int plain(int x) { return x; }\n", "int plain(");
    assert!(
        card.contains("```cpp\nint plain(int x)\n```"),
        "the block is the declaration and nothing else: {card}"
    );
}

/// **A macro that ends a scope is not the annotation of the declaration below it.**
///
/// `stringfwd.h` writes `_GLIBCXX_END_NAMESPACE_CXX11` — a macro that expands to `}` — above `std::string`'s
/// declaration, with a blank line and the doc comment between them. The declaration's previous sibling is a node whose
/// text is the macro **and** that doc comment, and the card drew the macro in front of the signature:
/// `_GLIBCXX_END_NAMESPACE_CXX11 using string = basic_string<char>;`. The macro is left **undefined** here on purpose:
/// that is the shape the real header has when the file is read on its own, and it is why the expansion cannot be what
/// rules the node out.
#[test]
fn a_macro_that_ends_a_scope_is_not_an_annotation() {
    let card = the_card(
        "namespace std {\n\
         inline namespace cxx11 {\n\
         class basic_string {};\n\
         }\n\
         END_NAMESPACE\n\
         \n\
         /// A string of @c char\n\
         typedef basic_string<char> string;\n",
        "typedef basic_string<char> string",
    );
    assert!(
        !card.contains("END_NAMESPACE"),
        "a macro two lines up, past a blank line and the doc comment, is a statement of its own: {card}"
    );
    assert!(card.contains("basic_string<char>"), "{card}");
}
