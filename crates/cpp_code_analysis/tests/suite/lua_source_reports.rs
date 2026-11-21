//! Reported from an editor, five defects on Lua 5.5.0's own sources. Three of them shared a cause: **which reading
//! answers**, not the expansion itself.
//!
//! ```text
//! 1. hover on `PF_VAHID` says "Expands to PF_VAHID"          the card's expansion was made from the wrong offsets
//! 2. `previous` is coloured as a type                          semantic tokens
//! 3. hover on a parameter says `LUA_API int level`             the type line takes the function's specifiers
//! 4. `ci_func(ci)->p` has no type, so `p` cannot be resolved   the rendering exists and is right; the hover asks the
//!                                                             file's own text first, and the mapping into the
//!                                                             rendering was unavailable
//! 5. `lua_State`'s layout is "not computed"                    the layout writer cannot size a member
//! ```
//!
//! This file pins the parts that were fixed and the measurements the rest are owed: see
//! `docs/open-questions.md`, "Lua 5.5.0's own sources".

use std::path::PathBuf;

use cpp_code_analysis::Known;
use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::file::paths::DiskFiles;
use cpp_code_analysis::index::watch::WatchFilter;
use cpp_code_analysis::session::{OpenDocuments, Session, SessionFiles};

/// A session over a fixture, indexed and cooked.
fn a_session(name: &str, files: &[(&str, &str)], the_file: &str) -> (Session<DiskFiles>, PathBuf) {
    let root: PathBuf = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    let mut written = Vec::new();
    for (name, text) in files {
        let path = root.join(name);
        std::fs::write(&path, text).expect("the fixture writes");
        written.push(path);
    }
    let file = root.join(the_file);
    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(written);
    session.want_cooked_reading(&file);
    let _ = session.index_everything();
    (session, file)
}

/// **A macro defined in an included header expands to its body in the hover** — not to its own name.
///
/// `Session::the_expansion_of_a_macro` builds a synthetic source: the hovered file's own `#define`s, then the
/// invocation, and cooks it against the composed environment. The environment's bindings are keyed by **positions in
/// the hovered file** (the offset of the `#include` that reached each definition), while the synthetic source starts at
/// zero — so a definition from a header was not yet in force there and the name came back as its own expansion.
///
/// Measured on Lua 5.5.0: `#define PF_VAHID 1` in `lobject.h:586`, used in `lcode.c:1933` as `cast_byte(~PF_VAHID)`;
/// the card said "a macro whose value is `1`" and "Expands to `PF_VAHID`".
#[test]
fn a_macros_expansion_is_read_at_the_use_and_not_at_offset_zero() {
    let (session, file) = a_session(
        "cppls-a-macros-expansion",
        &[
            (
                "object.h",
                "#pragma once\n#define FLAG 1  /* the flag a compiler expands */\n",
            ),
            (
                "main.c",
                "#include \"object.h\"\n\nint f(void) {\n    return FLAG;\n}\n",
            ),
        ],
        "main.c",
    );
    let text = std::fs::read_to_string(&file).expect("the fixture reads");
    let view = session.view_of_the_file(&file).expect("the written text");
    let at = text.find("FLAG;").expect("the use");
    let expansion = session.the_expansion_of_a_macro(&view, FileOffset::new(at));
    assert_eq!(
        expansion.as_deref(),
        Some("1"),
        "the header's `#define FLAG 1` is in force at the use"
    );
    let _ = std::fs::remove_dir_all(file.parent().unwrap_or(&file));
}

/// **The comma operator is its right operand's type** — C++'s rule, and the shape a rendering is full of because
/// `check_exp(c, e)` is `(lua_assert(c), (e))`.
///
/// Measured on Lua 5.5.0's `ldebug.c:101`: `ci_func(ci)` expands to a comma expression, its type was "not known", and
/// `->p` on it had none — so hovering `p` said the type of `ci_func(ci)` was not known.
#[test]
fn a_comma_expression_is_its_right_operands_type() {
    let source = "\
int f() {
    int a = 1;
    int b = 2;
    auto c = (a, b);
    return c;
}
double g() {
    int a = 1;
    double d = 2.5;
    auto e = (a, d);
    return e;
}
";
    let (session, file) = a_session("cppls-comma-expression", &[("a.cpp", source)], "a.cpp");
    let view = session.view_of_the_file(&file).expect("the written text");
    for (written, expected) in [("(a, b)", "int"), ("(a, d)", "double")] {
        let expression = view
            .root
            .descendants()
            .filter(|node| node.text() == written)
            .max_by_key(|node| node.ancestors().count())
            .expect("the comma expression");
        let Known::Yes(found) = session.type_of_the_expression(&view, &expression) else {
            panic!("`{written}` has no type");
        };
        assert_eq!(found.type_of.to_string(), expected, "{written}");
    }
    let _ = std::fs::remove_dir_all(file.parent().unwrap_or(&file));
}
