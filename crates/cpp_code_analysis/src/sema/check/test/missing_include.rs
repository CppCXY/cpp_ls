//! **`missing-include`** — a type a declaration spells, declared in a header this file never includes.
//!
//! The rule reads one file's **declarations**: the type a variable or a field was declared with, and the types in a
//! parameter list. Everything here is about the shapes that look like a missing include and are not one — a name the
//! file declares itself, a name its own scopes bind, a header it already reaches, and a qualified spelling, which is a
//! namespace question rather than an include. A finding needs a project file, because the header it names must exist.

use super::{assert_reported, assert_silent_about, codes_in, codes_of};
use crate::DiagnosticCode;

/// **The plain case, in both positions the rule reads**: a variable's declared type, and a parameter's.
#[test]
fn a_type_used_without_its_header_is_reported() {
    let a_variable = codes_in(
        &[
            ("/p/widget.h", "class Widget { int size; };\n"),
            ("/p/use.cpp", "Widget w;\n"),
        ],
        "/p/use.cpp",
    );
    assert_reported(
        &a_variable,
        DiagnosticCode::MissingInclude,
        "`Widget` is declared in widget.h, which use.cpp never includes",
    );

    let a_parameter = codes_in(
        &[
            ("/p/nothere.h", "class NotHere {};\n"),
            ("/p/uses.cpp", "void f(NotHere* value) {}\n"),
        ],
        "/p/uses.cpp",
    );
    assert_reported(
        &a_parameter,
        DiagnosticCode::MissingInclude,
        "a type in a parameter list is a position this rule reads",
    );
}

/// **The `#include` is the fix, so a file that already writes it has no finding** — and neither has a file that
/// declares the type itself, which is the other way a name can be visible.
#[test]
fn a_type_the_file_can_already_see_is_not_missing_an_include() {
    let included = codes_in(
        &[
            ("/p/widget.h", "class Widget { int size; };\n"),
            (
                "/p/use.cpp",
                "#include \"widget.h\"\nvoid f(Widget* w) {}\n",
            ),
        ],
        "/p/use.cpp",
    );
    assert_silent_about(
        &included,
        DiagnosticCode::MissingInclude,
        "widget.h is already in this file's include closure",
    );

    let declared_here = codes_of("struct Widget { int size; };\nvoid f(Widget* w) {}\n");
    assert_silent_about(
        &declared_here,
        DiagnosticCode::MissingInclude,
        "`Widget` is declared in this file, so nothing is missing from its includes",
    );
}

/// **A qualified spelling is a namespace question**: `gui::Widget` names the namespace `gui` as much as the type, and
/// an `#include` line cannot express that.
#[test]
fn a_qualified_spelling_is_a_namespace_question() {
    let qualified = codes_in(
        &[
            ("/p/gui.h", "namespace gui { class Widget {}; }\n"),
            ("/p/use.cpp", "void f(gui::Widget* w) {}\n"),
        ],
        "/p/use.cpp",
    );
    assert_silent_about(
        &qualified,
        DiagnosticCode::MissingInclude,
        "`gui::Widget` is written with a `::`, which this rule leaves to the namespace question",
    );
}

/// **A name the file's own scopes bind is not a name from elsewhere** — a template parameter is the case that was
/// measured, and a local variable is the other one: a body's locals are the scope tree's business.
#[test]
fn a_name_the_file_binds_is_not_from_elsewhere() {
    let a_template_parameter = codes_in(
        &[
            ("/p/container.h", "class Container {};\n"),
            (
                "/p/db.h",
                "template <class K, class V, class Container>\n\
                 class DBase {\n\
                 public:\n\
                     V* find(const K& key);\n\
                 private:\n\
                     Container _hash;\n\
                 };\n",
            ),
        ],
        "/p/db.h",
    );
    assert_silent_about(
        &a_template_parameter,
        DiagnosticCode::MissingInclude,
        "`Container` is this class's own template parameter, which the scope tree binds here",
    );

    let a_local = codes_in(
        &[
            ("/p/widget.h", "class Widget { int size; };\n"),
            ("/p/use.cpp", "void f() { Widget w; }\n"),
        ],
        "/p/use.cpp",
    );
    assert_silent_about(
        &a_local,
        DiagnosticCode::MissingInclude,
        "a declaration inside a body is local, and its type is not this rule's question",
    );
}

/// **A declaration in a source file is not a header to include.**
///
/// Measured on `EmmyLuaCodeStyle`: four asio headers were told to include `gtest_unittest.cc`, because a template
/// parameter named `T` is also a *type fact* in a test file. A source file is compiled rather than included, so its
/// declarations can never be made visible by naming it — and the message this rule prints proposes exactly that.
#[test]
fn a_type_declared_in_a_source_file_is_not_offered() {
    let codes = codes_in(
        &[
            ("/p/detail/gtest_unittest.cc", "class T {};\n"),
            ("/p/use.hpp", "void f(T value) {}\n"),
        ],
        "/p/use.hpp",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::MissingInclude,
        "a `.cc` is a translation unit, not a header to include",
    );
}
