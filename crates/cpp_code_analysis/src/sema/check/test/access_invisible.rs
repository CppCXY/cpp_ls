//! **`access-invisible`** — a private or protected member reached from code that is not the declaring class's own.
//!
//! Both halves of the question have to be in the file being checked: the class's definition, so that the label in force
//! at the member can be read, and the access itself. Everything else is silent, and the silences are most of this file
//! — a public member, a `struct`'s default, the declaring class's own member functions (written inside the class and
//! out of it), a `friend`, and a member of a class whose definition is in a header.

use super::{assert_reported, assert_silent_about, codes_in, codes_of};
use crate::DiagnosticCode;

/// **A private member reached from a free function** is the rule's reason to exist: `class` starts private, and `main`
/// is not `Widget`. **`protected` is not `public`** either — the label in force is the last specifier above the member.
#[test]
fn a_private_or_protected_member_is_reported_from_a_free_function() {
    let private = codes_of(
        "class Widget {\n    int secret;\npublic:\n    int size;\n};\n\n\
         int main() { Widget w; return w.secret; }\n",
    );
    assert_reported(
        &private,
        DiagnosticCode::AccessInvisible,
        "`secret` is private and `main` is not Widget",
    );

    let protected = codes_of(
        "class Widget {\npublic:\n    int size;\nprotected:\n    int hidden;\n};\n\n\
         int main() { Widget w; return w.hidden; }\n",
    );
    assert_reported(
        &protected,
        DiagnosticCode::AccessInvisible,
        "`hidden` is protected, and a free function is outside that too",
    );
}

/// **Another class is not the declaring class**: `peek` is a member function, but of `B`, not of `A`.
#[test]
fn a_member_of_one_class_read_from_another_class_is_reported() {
    let codes = codes_of(
        "class A {\n    int secret;\npublic:\n    int get();\n};\n\
         class B {\npublic:\n    int peek(A& a) { return a.secret; }\n};\n",
    );
    assert_reported(&codes, DiagnosticCode::AccessInvisible, "`B` is not `A`");
}

/// **A public member is public, and a `struct` starts public** — neither has a label above it to report.
#[test]
fn a_public_member_and_a_structs_default_are_silent() {
    let labelled = codes_of(
        "class Widget {\npublic:\n    int size;\n};\n\nint main() { Widget w; return w.size; }\n",
    );
    assert_silent_about(
        &labelled,
        DiagnosticCode::AccessInvisible,
        "`size` is public",
    );

    let defaulted =
        codes_of("struct Point {\n    int x;\n};\n\nint main() { Point p; return p.x; }\n");
    assert_silent_about(
        &defaulted,
        DiagnosticCode::AccessInvisible,
        "a `struct`'s members are public without a label",
    );
}

/// **The declaring class's own code may read its privates**, whether the definition is written in the class or out of
/// it.
#[test]
fn the_own_code_of_the_declaring_class_is_silent() {
    let inside = codes_of(
        "class Widget {\n    int secret;\npublic:\n    int mine() { return secret; }\n};\n",
    );
    assert_silent_about(
        &inside,
        DiagnosticCode::AccessInvisible,
        "`mine` is a member function of `Widget`",
    );

    let out_of_class = codes_of(
        "struct Holder {\n    int peek() const;\nprivate:\n    int secret = 1;\n};\n\
         int Holder::peek() const { return secret; }\n",
    );
    assert_silent_about(
        &out_of_class,
        DiagnosticCode::AccessInvisible,
        "the definition is `Holder`'s own code, written outside its body",
    );
}

/// **A member this file does not declare is not judged** — the class is defined in a header, and this check reads the
/// class written in the file where the access is written.
#[test]
fn a_member_declared_in_a_header_is_silent() {
    let codes = codes_in(
        &[
            (
                "/p/widget.h",
                "class Widget {\n    int secret;\npublic:\n    int size;\n};\n",
            ),
            (
                "/p/use.cpp",
                "#include \"widget.h\"\nint main() { Widget w; return w.secret; }\n",
            ),
        ],
        "/p/use.cpp",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::AccessInvisible,
        "the member's class is not defined in this file",
    );
}

/// **A `friend` may read what is private** — the permission is written in the declaring class's own body.
#[test]
fn a_friends_code_is_silent() {
    let codes = codes_of(
        "class Holder {\n    friend class Borrower;\n    int secret = 1;\n};\n\
         class Borrower {\npublic:\n    int peek(Holder& holder) { return holder.secret; }\n};\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::AccessInvisible,
        "`Borrower` is named as `Holder`'s friend",
    );
}
