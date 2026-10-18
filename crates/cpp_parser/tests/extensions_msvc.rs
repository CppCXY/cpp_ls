//! **MSVC extensions** — what `cl.exe` accepts and a Windows project's headers are written with.

mod common;

use common::assert_all_parse;

const CONSTRUCTS: &[(&str, &str)] = &[
    ("__declspec", "__declspec(dllexport) void f();"),
    ("__declspec on a class", "class __declspec(dllexport) S { };"),
    ("__cdecl", "void __cdecl f();"),
    ("__stdcall", "void __stdcall f();"),
    ("__fastcall", "void __fastcall f();"),
    ("__forceinline", "__forceinline void f();"),
    ("__int64", "__int64 big = 0;"),
    ("__int8 and friends", "__int8 a = 0; __int16 b = 0; __int32 c = 0;"),
    ("__m128", "__m128 v = { 0 };"),
    ("__uuidof", "void f(void* p) { auto id = __uuidof(p); (void)id; }"),
    ("__super", "struct B { void f(); }; struct D : B { void g() { __super::f(); } };"),
    ("a __ptr32 pointer", "void f(int __ptr32* p);"),
];

#[test]
fn every_msvc_extension_parses_without_an_error() {
    assert_all_parse("MSVC", CONSTRUCTS);
}

/// **The MSVC constructs the grammar does not parse**: structured exception handling and `__interface`.
///
/// ```text
///   __try { } __except (1) { }     expected `;` after expression
///   __try { } __finally { }        expected `;` after expression
///   __interface I : IUnknown { };  expected `;` after expression
/// ```
#[test]
#[ignore = "`__try`/`__except`, `__try`/`__finally` and `__interface` do not parse"]
fn the_msvc_gaps_are_named() {
    assert_all_parse(
        "MSVC gaps",
        &[
            ("__try/__except", "void f() { __try { } __except (1) { } }"),
            ("__try/__finally", "void f() { __try { } __finally { } }"),
            ("__interface", "__interface I : IUnknown { virtual void f() = 0; };"),
        ],
    );
}