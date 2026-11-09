//! **MSVC extensions** — what `cl.exe` accepts and a Windows project's headers are written with.

mod common;

use common::assert_all_parse;

const CONSTRUCTS: &[(&str, &str)] = &[
    ("__declspec", "__declspec(dllexport) void f();"),
    (
        "__declspec on a class",
        "class __declspec(dllexport) S { };",
    ),
    ("__cdecl", "void __cdecl f();"),
    ("__stdcall", "void __stdcall f();"),
    ("__fastcall", "void __fastcall f();"),
    ("__forceinline", "__forceinline void f();"),
    ("__int64", "__int64 big = 0;"),
    (
        "__int8 and friends",
        "__int8 a = 0; __int16 b = 0; __int32 c = 0;",
    ),
    ("__m128", "__m128 v = { 0 };"),
    (
        "__uuidof",
        "void f(void* p) { auto id = __uuidof(p); (void)id; }",
    ),
    (
        "__super",
        "struct B { void f(); }; struct D : B { void g() { __super::f(); } };",
    ),
    ("a __ptr32 pointer", "void f(int __ptr32* p);"),
];

#[test]
fn every_msvc_extension_parses_without_an_error() {
    assert_all_parse("MSVC", CONSTRUCTS);
}

/// **The MSVC constructs that used to be gaps** — structured exception handling and `__interface`.
///
/// ```text
///   __try { } __except (1) { }     a block and a handler, like `try`/`catch` — the filter is an expression
///   __try { } __finally { }        the other handler, with no filter at all
///   __interface I : IUnknown { };  a class key spelled as an identifier: name, base clause, body
/// ```
///
/// All three are written by every Windows driver and by MSVC's own headers. `__try`, `__except` and `__interface`
/// are identifiers rather than keywords, so each is recognised by its spelling — the reading `catch` already gets.
#[test]
fn the_msvc_gaps_are_named() {
    assert_all_parse(
        "MSVC gaps",
        &[
            ("__try/__except", "void f() { __try { } __except (1) { } }"),
            ("__try/__finally", "void f() { __try { } __finally { } }"),
            (
                "__interface",
                "__interface I : IUnknown { virtual void f() = 0; };",
            ),
        ],
    );
}

/// **A definition whose head and first parameter are both `::`-qualified** — MSVC's `vccorlib.h` writes every `Guid`
/// member that way:
///
/// ```cpp
/// inline bool ::Platform::Guid::Equals(::Platform::Guid __guidArg) { … }
/// ```
///
/// The specifier sequence joined the whole qualified name into the type (`starts_an_out_of_class_member` only knew a
/// leading *identifier*), so the declarator named nothing and the `(` was read as a direct-initialiser — which failed
/// at the parameter's own `::` and cost the declaration. Measured before: 11 errors in `vccorlib.h` on the sweep, and
/// after: none (`docs/buglist.md` section 13).
///
/// The check is on the **shape of the declaration**, not on the error count alone: a declaration read as a
/// type-plus-initialiser can be error-free in the shapes where the initializer parses, and the parameter list is what a
/// reader loses. (`CppDeclaration::get_name_text` answers the *first* segment of a qualified declarator — `Platform`
/// here — which is a property of that accessor and not of this fix; the analysis layer's fact says `Equals` in
/// `::Platform::Guid`, measured with `inspect facts`.)
#[test]
fn a_definition_whose_head_is_globally_qualified_keeps_its_parameter_list() {
    use cpp_parser::CppAstNode;

    let text = "inline bool ::Platform::Guid::Equals(::Platform::Guid g) { return true; }\n";
    let tree = cpp_parser::CppParser::parse(text, cpp_parser::ParserConfig::default());
    assert!(
        tree.get_errors().is_empty(),
        "the definition parses: {:?}",
        tree.get_errors()
    );
    let declarations: Vec<String> = tree
        .get_red_root()
        .descendants()
        .filter_map(cpp_parser::CppDeclaration::cast)
        .map(|declaration| declaration.syntax().text().to_string())
        .collect();
    assert!(
        declarations
            .iter()
            .any(|text| text.contains("Equals(::Platform::Guid g)")),
        "the parameter list belongs to the declaration: {declarations:?}"
    );
}
