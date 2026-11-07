//! **`export extern "C++" template <class _Elem> class basic_ios;`** — the line that makes the standard library's
//! `namespace std` unclosable.
//!
//! Measured on the stitched stream of `import std;` (4.85 MB, 224 files): a file's own rendering
//! (`<iosfwd>`-shaped, 24 557 bytes) parses with **5 errors**, the first at `export extern "C++" template <class _Ty>
//! class basic_ios;`, and the `}` that closes `namespace std` is swallowed by the recovery — so every file after it is
//! parsed **inside** `std`. Ten such namespaces accumulate by the time the intrinsics are reached, and all 6 732 of them
//! are filed under `std`.
//!
//! The construct is ordinary C++20: `export` (module linkage) in front of `extern "C++"` (language linkage) in front of
//! a declaration. Both spellings are tested here, with and without `export`, because a language-linkage declaration with
//! no block is a *declaration*, not a statement.

use cpp_parser::{CppAstNode, CppNamespaceDecl, CppParser, ParserConfig};

fn parse(text: &str) -> cpp_parser::CppSyntaxTree {
    CppParser::parse(text, ParserConfig::default())
}

/// Is `name` written at file scope — no namespace above it?
fn at_file_scope(tree: &cpp_parser::CppSyntaxTree, text: &str, name: &str) -> bool {
    let root = tree.get_red_root();
    let at = text.find(name).expect("the fixture names it");
    let mut found = None;
    for node in root.descendants() {
        let range = cpp_parser::source_range(node.text_range());
        if range.start_offset <= at && at <= range.end_offset() {
            found = Some(node);
        }
    }
    let mut node = found;
    while let Some(current) = node {
        if CppNamespaceDecl::cast(current.clone()).is_some() {
            return false;
        }
        node = current.parent();
    }
    true
}

/// **A language-linkage declaration in front of a class**, without `export`.
#[test]
fn extern_cxx_before_a_class_is_a_declaration() {
    let text = "namespace std { extern \"C++\" template <class _Elem> class basic_ios; } int after_this;\n";
    let tree = parse(text);
    assert!(
        tree.get_errors().is_empty(),
        "no error: {:?}",
        tree.get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range).start_offset)
            .collect::<Vec<_>>()
    );
    assert!(at_file_scope(&tree, text, "after_this"), "the namespace closed");
}

/// **And with `export` in front**, which is what the standard module writes.
#[test]
fn export_before_a_language_linkage_declaration_is_a_declaration() {
    let text =
        "namespace std { export extern \"C++\" template <class _Elem> class basic_ios; } int after_this;\n";
    let tree = parse(text);
    assert!(
        tree.get_errors().is_empty(),
        "no error: {:?}",
        tree.get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range).start_offset)
            .collect::<Vec<_>>()
    );
    assert!(at_file_scope(&tree, text, "after_this"), "the namespace closed");
}

/// **The measured file's own shape** — several in a row, the way `<iosfwd>` writes them.
#[test]
fn a_run_of_exported_language_linkage_declarations_keeps_the_namespace_closed() {
    let text = "namespace std { export using streamoff = long long; export template <class _Statetype> class fpos; \
                export extern \"C++\" class ios_base; \
                export extern \"C++\" template <class _Elem, class _Traits = char_traits<_Elem>> class basic_ios; \
                export extern \"C++\" template <class _Elem, class _Traits = char_traits<_Elem>> class basic_streambuf; \
                } int after_this;\n";
    let tree = parse(text);
    assert!(
        tree.get_errors().is_empty(),
        "no error: {:?}",
        tree.get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range).start_offset)
            .collect::<Vec<_>>()
    );
    assert!(at_file_scope(&tree, text, "after_this"), "the namespace closed");
}

/// **An attribute between the class key and the name** — `class [[deprecated("…")]] wstring_convert { … };`.
///
/// The standard library writes this on every deprecated class:
///
/// ```cpp
/// export template <class _Codecvt, class _Elem = wchar_t, class _Traits = char_traits<_Elem>>
/// class [[deprecated("warning STL4017: …")]] wstring_convert { … };
/// ```
///
/// Measured on the stitched stream of `import std;`: the file that holds such a class parses with **13 errors**, and the
/// `}` that closes `namespace std` goes with the recovery — so every file after it is read inside `std`. This and one
/// more like it were the last two namespaces open at EOF.
#[test]
fn an_attribute_after_the_class_key_is_an_attribute() {
    let text = "namespace std { class [[deprecated(\"old\")]] Widget { public: int size; }; } int after_this;\n";
    let tree = parse(text);
    assert!(
        tree.get_errors().is_empty(),
        "no error: {:?}",
        tree.get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range).start_offset)
            .collect::<Vec<_>>()
    );
    assert!(at_file_scope(&tree, text, "after_this"), "the namespace closed");
}

/// **The template the standard library actually writes** — a templated class with the attribute on the class key.
#[test]
fn an_exported_template_class_with_an_attribute_keeps_the_namespace_closed() {
    let text = "namespace std { export template <class _Elem, unsigned long _Mymax = 0x10ffff> \
                class [[deprecated(\"warning STL4017: \")]] codecvt_utf8 { public: explicit codecvt_utf8(size_t _Refs = 0); \
                ~codecvt_utf8() noexcept {} }; } int after_this;\n";
    let tree = parse(text);
    assert!(
        tree.get_errors().is_empty(),
        "no error: {:?}",
        tree.get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range).start_offset)
            .collect::<Vec<_>>()
    );
    assert!(at_file_scope(&tree, text, "after_this"), "the namespace closed");
}

/// **`_Pragma("…")` at declaration position** — how the standard library pushes and pops warnings in the middle of a
/// header's declarations:
///
/// ```cpp
/// namespace std { _Pragma("warning(push)") _Pragma("warning(disable : 4996)") export template <…> class [[deprecated(…)]] wstring_convert { … };
/// ```
///
/// The error of the measured file begins exactly at the `_Pragma`, and the class after it never gets a declaration node.
#[test]
fn a_pragma_operator_at_declaration_position_is_a_declaration() {
    let text = "namespace std { _Pragma(\"warning(push)\") _Pragma(\"warning(disable : 4996)\") class Widget { public: int size; }; } int after_this;\n";
    let tree = parse(text);
    assert!(
        tree.get_errors().is_empty(),
        "no error: {:?}",
        tree.get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range).start_offset)
            .collect::<Vec<_>>()
    );
    assert!(at_file_scope(&tree, text, "after_this"), "the namespace closed");
}

/// **The measured line, verbatim** — string-literal concatenation inside the attribute, `export template`, `_Pragma`s.
///
/// **Ignored while it fails**, and it is the next target rather than a regression: the combination is `_Pragma("…")`
/// followed by `export`, which the parser has no rule for at all — `_Pragma` is an ordinary identifier to it, so it is
/// read as a call expression and the namespace body is left in a state where `export` is refused.
#[ignore = "known: `_Pragma(...)` at declaration position is not a rule yet"]
#[test]
fn the_deprecated_template_class_of_the_standard_library_keeps_the_namespace_closed() {
    let text = "namespace std { _Pragma(\"warning(push)\") _Pragma(\"warning(disable : 4996)\") export template <class _Codecvt, class _Elem = wchar_t, class _Traits = char_traits<_Elem>> class [[deprecated(\"warning STL4017: \" \"std::wbuffer_convert, std::wstring_convert, and the <codecvt> header are deprecated.\")]] wstring_convert { public: wstring_convert() : _State() {} }; } int after_this;\n";
    let tree = parse(text);
    assert!(
        tree.get_errors().is_empty(),
        "no error: {:?}",
        tree.get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range).start_offset)
            .collect::<Vec<_>>()
    );
    assert!(at_file_scope(&tree, text, "after_this"), "the namespace closed");
}

/// **Which of the pieces breaks which.** Each variant is one construct at namespace scope followed by the `export`ed
/// class, and the first one whose parse reports an error names the interaction. Ignored alongside the measurement it
/// explains: the failing variants are exactly the ones with `_Pragma` before `export`.
#[ignore = "known: `_Pragma(...)` at declaration position is not a rule yet"]
#[test]
fn which_piece_of_the_deprecated_line_breaks_it() {
    let variants = [
        ("pragma then class", "namespace std { _Pragma(\"a\") class W { }; } int after_this;"),
        ("pragma then export class", "namespace std { _Pragma(\"a\") export class W { }; } int after_this;"),
        ("pragma then export template class", "namespace std { _Pragma(\"a\") export template <class T> class W { }; } int after_this;"),
        ("export template class with attribute", "namespace std { export template <class T> class [[deprecated(\"x\")]] W { }; } int after_this;"),
        ("pragma then export template class with attribute", "namespace std { _Pragma(\"a\") export template <class T> class [[deprecated(\"x\")]] W { }; } int after_this;"),
        ("two pragmas then export template class", "namespace std { _Pragma(\"a\") _Pragma(\"b\") export template <class T> class W { }; } int after_this;"),
    ];
    let mut broken = Vec::new();
    for (name, text) in variants {
        let tree = parse(text);
        if !tree.get_errors().is_empty() || !at_file_scope(&tree, text, "after_this") {
            broken.push((
                name,
                tree.get_errors()
                    .iter()
                    .map(|error| cpp_parser::source_range(error.range).start_offset)
                    .collect::<Vec<_>>(),
            ));
        }
    }
    assert!(broken.is_empty(), "broken variants: {broken:?}");
}
