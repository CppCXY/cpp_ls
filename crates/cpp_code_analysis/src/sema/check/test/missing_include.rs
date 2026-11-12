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

/// **A template-id inside a template is a dependent name, not a missing include.**
///
/// Measured on `EmmyLuaCodeStyle`: after the first sweep this was the last finding —
/// `associated_executor.hpp`'s `constraint<is_convertible<ExecutionContext&, execution_context&>::value>` was told to
/// include an unrelated header that happens to declare an `is_convertible`. Inside a template the name may be a
/// dependent base's, an ADL-only name, or one the instantiation supplies; the rule reads declaration *spellings* and
/// has no scopes for the use, so silence is the only honest answer.
///
/// The negative half is what keeps the rule useful: a plain type in a template's parameter list is **not** this shape.
#[test]
fn a_template_id_inside_a_template_is_silent() {
    let dependent = codes_in(
        &[
            ("/p/convertible.h", "class is_convertible {};\n"),
            (
                "/p/use.h",
                "template <typename T>\nvoid f(typename constraint<is_convertible<T&, int&>::value>::type = 0);\n",
            ),
        ],
        "/p/use.h",
    );
    assert_silent_about(
        &dependent,
        DiagnosticCode::MissingInclude,
        "a dependent template-id is not a name a header can be named for",
    );

    let ordinary = codes_in(
        &[
            ("/p/widget.h", "class Widget {};\n"),
            ("/p/uses.h", "template <class T>\nvoid g(Widget w) {}\n"),
        ],
        "/p/uses.h",
    );
    assert_reported(
        &ordinary,
        DiagnosticCode::MissingInclude,
        "a plain type in a template's parameter list is still this rule's question",
    );
}

/// **A header is a fragment of its includers order** — the reported shape, from `EmmyLuaCodeStyle`:
///
/// ```cpp
/// // LuaStyleEnum.h
/// enum class FunctionSingleArgSpace { … };
/// // LuaStyleStruct.h            — no include of the enum anywhere in this file
/// struct SpaceBeforeSingleArg { FunctionSingleArgSpace table = FunctionSingleArgSpace::Always; };
/// // LuaStyle.h
/// #include "LuaStyleEnum.h"
/// #include "LuaStyleStruct.h"
/// ```
///
/// The project compiles as written, and the header is never compiled on its own. The finding was reported anyway —
/// "declared in `luastyleenum.h`, which this file does not include" — which argues with a style, not with a program.
/// The rule now asks the files that include the header; one of them missing the enum header is what a finding needs.
#[test]
fn a_header_inside_its_includers_order_is_silent() {
    let fragment = codes_in(
        &[
            ("/p/style_enum.h", "enum class Space { Always };\n"),
            (
                "/p/style_struct.h",
                "struct Before { Space table = Space::Always; };\n",
            ),
            (
                "/p/style.h",
                "#include \"style_enum.h\"\n#include \"style_struct.h\"\n",
            ),
        ],
        "/p/style_struct.h",
    );
    assert_silent_about(
        &fragment,
        DiagnosticCode::MissingInclude,
        "the only includer reaches the enum header before the fragment",
    );

    // **The same fragment, included by a file that never reaches the enum** — that translation unit does not compile,
    // and it is the finding the rule is for.
    let broken = codes_in(
        &[
            ("/p/style_enum.h", "enum class Space { Always };\n"),
            (
                "/p/style_struct.h",
                "struct Before { Space table = Space::Always; };\n",
            ),
            ("/p/only_struct.cpp", "#include \"style_struct.h\"\n"),
        ],
        "/p/style_struct.h",
    );
    assert_reported(
        &broken,
        DiagnosticCode::MissingInclude,
        "an includer that does not reach the enum is the broken translation unit",
    );
}
