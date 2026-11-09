//! **A comprehensive table of what GCC, Clang and MSVC add to C++**, one construct per row.
//!
//! # Why a table per compiler, and why this one is long
//!
//! `extensions_gnu.rs`, `extensions_clang.rs` and `extensions_msvc.rs` each started with the handful of constructs that
//! were reported. A sweep of the three toolchains' own headers (`tests/extension_sweep.rs`) then showed that the gaps
//! cluster by *vendor family*, not by file: MSVC's C++/CX and C++/CLI keywords, GCC's statement expressions and traits
//! builtins, Clang's nullability and block spellings. So this file is the checklist: every row is a construct a real
//! header writes, and the test prints **every** row that fails rather than the first, so one run says how far behind the
//! grammar is in one number.
//!
//! The tables are documentation as much as tests: a row that passes is a statement about what this parser reads.

mod common;

use common::assert_all_parse;

/// GCC's own additions — C and C++ both, because a `.h` compiled as C++ still writes them.
const GCC: &[(&str, &str)] = &[
    (
        "__attribute__ before a declaration",
        "__attribute__((always_inline)) void f();",
    ),
    (
        "__attribute__ after a declarator",
        "int f() __attribute__((pure));",
    ),
    (
        "__attribute__ with several arguments",
        "__attribute__((format(printf, 1, 2))) void f(const char*, ...);",
    ),
    (
        "__attribute__ on a struct",
        "struct __attribute__((packed)) S { int n; };",
    ),
    ("__typeof__", "void f() { __typeof__(1) n = 1; (void)n; }"),
    ("typeof", "void f() { typeof(1) n = 1; (void)n; }"),
    (
        "__typeof__ of an array",
        "void f() { __typeof__(int[4]) a; (void)a; }",
    ),
    (
        "typeof_unqual",
        "void f() { typeof_unqual(1) n = 1; (void)n; }",
    ),
    ("__auto_type", "void f() { __auto_type n = 1; (void)n; }"),
    ("__extension__", "__extension__ typedef int T;"),
    (
        "__extension__ in an expression",
        "void f() { int n = __extension__ 1; (void)n; }",
    ),
    ("an asm block", "void f() { __asm__ volatile (\"nop\"); }"),
    (
        "an asm label on a declaration",
        "int f() __asm__(\"f_alias\");",
    ),
    ("__int128", "__int128 big = 0; (void)big;"),
    ("unsigned __int128", "unsigned __int128 u = 0; (void)u;"),
    ("_Float128", "_Float128 q = 0; (void)q;"),
    ("__restrict__", "void f(int* __restrict__ a);"),
    ("__restrict", "void f(int* __restrict a);"),
    (
        "a case range",
        "void f(int n) { switch (n) { case 1 ... 5: break; default: break; } }",
    ),
    (
        "__builtin_expect",
        "void f(int n) { if (__builtin_expect(n, 1)) { return; } }",
    ),
    (
        "__builtin_unreachable",
        "void f() { __builtin_unreachable(); }",
    ),
    (
        "__builtin_offsetof",
        "struct S { int x; }; unsigned long f() { return __builtin_offsetof(S, x); }",
    ),
    (
        "__builtin_types_compatible_p",
        "int f() { return __builtin_types_compatible_p(int, int); }",
    ),
    (
        "__builtin_choose_expr",
        "int f() { return __builtin_choose_expr(1, 2, 3); }",
    ),
    (
        "__sync builtins",
        "void f(int* p) { (void)__sync_fetch_and_add(p, 1); }",
    ),
    ("#include_next", "#include_next <stdio.h>\n"),
    (
        "a statement expression",
        "void f() { int n = ({ int x = 1; x; }); (void)n; }",
    ),
    (
        "a statement expression as a statement",
        "void f() { ({ int x = 1; (void)x; }); }",
    ),
    (
        "a designated initializer",
        "struct S { int x; int y; }; struct S s = { .x = 1, .y = 2 };",
    ),
    ("an array designator", "int a[3] = { [0] = 1, [2] = 3 };"),
    ("__thread", "__thread int t = 0;"),
    (
        "a vector_size attribute",
        "typedef int v4 __attribute__((vector_size(16)));",
    ),
    (
        "__real__ and __imag__",
        "void f() { double d = __real__ 1.0 + __imag__ 1.0; (void)d; }",
    ),
    ("__alignof__", "int f() { return (int)__alignof__(int); }"),
    (
        "__underlying_type",
        "void f() { __underlying_type(int) n = 1; (void)n; }",
    ),
    (
        "a traits builtin as a type",
        "void f() { __is_trivially_constructible(int, int) n = 1; (void)n; }",
    ),
    (
        "a label and a goto",
        "void f() { __label__ l; goto l; l: ; }",
    ),
    (
        "a computed goto",
        "void f() { void* p = &&l; goto *p; l: ; }",
    ),
    (
        "#pragma GCC diagnostic",
        "#pragma GCC diagnostic push\n#pragma GCC diagnostic ignored \"-Wall\"\nint n;\n#pragma GCC diagnostic pop\n",
    ),
    (
        "#pragma GCC optimize",
        "#pragma GCC optimize(\"O2\")\nint n;\n",
    ),
];

/// Clang's additions, including the ones Apple's headers are written with.
const CLANG: &[(&str, &str)] = &[
    ("_Nullable", "void f(int* _Nullable p);"),
    ("_Nonnull", "void f(int* _Nonnull p);"),
    ("_Null_unspecified", "void f(int* _Null_unspecified p);"),
    ("__nullable", "void f(int* __nullable p);"),
    (
        "a block declaration and literal",
        "void f() { void (^block)(int) = ^(int n) { (void)n; }; (void)block; }",
    ),
    ("a block typedef", "typedef void (^handler_t)(void);"),
    ("__block", "void f() { __block int n = 1; (void)n; }"),
    (
        "__has_feature",
        "#if __has_feature(cxx_concepts)\nint n;\n#endif\n",
    ),
    (
        "__has_extension",
        "#if __has_extension(cxx_concepts)\nint n;\n#endif\n",
    ),
    (
        "__has_include",
        "#if __has_include(<vector>)\nint n;\n#endif\n",
    ),
    (
        "__has_builtin",
        "#if __has_builtin(__builtin_expect)\nint n;\n#endif\n",
    ),
    ("a clang attribute", "[[clang::fallthrough]] void f();"),
    (
        "an attribute with a clang spelling",
        "__attribute__((no_sanitize(\"address\"))) void f();",
    ),
    (
        "overloadable",
        "__attribute__((overloadable)) int f(int n);",
    ),
    (
        "availability",
        "__attribute__((availability(macos, introduced=10.12))) void f();",
    ),
    ("swift_name", "__attribute__((swift_name(\"x\"))) void f();"),
    ("annotate", "__attribute__((annotate(\"x\"))) void f();"),
    ("address_space", "__attribute__((address_space(1))) int* p;"),
    (
        "a vector type",
        "typedef int int4 __attribute__((ext_vector_type(4)));",
    ),
    ("__auto_type", "void f() { __auto_type n = 1; (void)n; }"),
    (
        "__typeof_unqual__",
        "void f() { __typeof_unqual__(1) n = 1; (void)n; }",
    ),
    (
        "__builtin_assume",
        "void f(int n) { __builtin_assume(n > 0); }",
    ),
    (
        "__builtin_complex",
        "void f() { double d = __builtin_complex(1.0, 2.0); (void)d; }",
    ),
    (
        "__integer_pack",
        "template <int...> struct seq { }; using s = seq<__integer_pack(4)...>;",
    ),
    ("_BitInt", "void f() { _BitInt(32) n = 0; (void)n; }"),
    ("_Atomic", "void f() { _Atomic(int) n = 0; (void)n; }"),
    (
        "#pragma clang diagnostic",
        "#pragma clang diagnostic push\n#pragma clang diagnostic ignored \"-Wall\"\nint n;\n#pragma clang diagnostic pop\n",
    ),
    (
        "#pragma unroll",
        "#pragma unroll 4\nfor (int i = 0; i < 4; ++i) { }\n",
    ),
    ("_Pragma", "_Pragma(\"clang diagnostic push\")\nint n;\n"),
];

/// MSVC's additions — the classic Windows dialect, then C++/CLI and C++/CX, which MSVC's **own** headers write.
const MSVC: &[(&str, &str)] = &[
    ("__declspec", "__declspec(dllexport) void f();"),
    (
        "__declspec on a class",
        "class __declspec(dllexport) S { };",
    ),
    (
        "__declspec(align)",
        "__declspec(align(16)) struct S { int n; };",
    ),
    (
        "__declspec(uuid)",
        "__declspec(uuid(\"00000000-0000-0000-0000-000000000000\")) class S { };",
    ),
    ("__declspec(selectany)", "__declspec(selectany) int x = 1;"),
    ("__declspec(thread)", "__declspec(thread) int t = 0;"),
    (
        "__declspec(property)",
        "__declspec(property(get=g, put=s)) int x;",
    ),
    (
        "__declspec(deprecated)",
        "__declspec(deprecated(\"x\")) void f();",
    ),
    ("__cdecl", "void __cdecl f();"),
    ("__stdcall", "void __stdcall f();"),
    ("__fastcall", "void __fastcall f();"),
    ("__thiscall", "struct S { void __thiscall f(); };"),
    ("__vectorcall", "void __vectorcall f();"),
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
    ("__unaligned", "void f(int __unaligned* p);"),
    ("__w64", "__w64 int x = 0;"),
    ("__try/__except", "void f() { __try { } __except (1) { } }"),
    ("__try/__finally", "void f() { __try { } __finally { } }"),
    ("__leave", "void f() { __try { __leave; } __finally { } }"),
    (
        "__interface",
        "__interface I : IUnknown { virtual void f() = 0; };",
    ),
    ("__pragma", "__pragma(warning(push)) void f();"),
    ("__assume", "void f(int n) { __assume(n > 0); }"),
    ("__debugbreak", "void f() { __debugbreak(); }"),
    ("__nop", "void f() { __nop(); }"),
    ("__rdtsc", "unsigned long long f() { return __rdtsc(); }"),
    (
        "#pragma warning",
        "#pragma warning(push)\n#pragma warning(disable: 4996)\nint n;\n#pragma warning(pop)\n",
    ),
    (
        "#pragma pack",
        "#pragma pack(push, 1)\nstruct S { int n; };\n#pragma pack(pop)\n",
    ),
    (
        "#pragma comment",
        "#pragma comment(lib, \"user32.lib\")\nint n;\n",
    ),
    ("#pragma intrinsic", "#pragma intrinsic(memcpy)\nint n;\n"),
    // C++/CLI — the managed dialect, and still written by `marshal_*.h`.
    ("gcnew", "void* f() { return gcnew System::Object(); }"),
    (
        "gcnew without parentheses",
        "void f() { throw gcnew System::Exception; }",
    ),
    (
        "a handle type",
        "void f() { System::Object^ o = nullptr; (void)o; }",
    ),
    ("a handle in a parameter", "void f(System::String^ s);"),
    (
        "a handle in a template argument",
        "template <typename T> struct A { }; void f() { A<System::Object^> a; (void)a; }",
    ),
    ("a tracking reference", "void f(System::Object% r);"),
    (
        "safe_cast",
        "void f() { int n = safe_cast<int>(1); (void)n; }",
    ),
    ("ref class", "ref class R { };"),
    ("ref struct", "ref struct R { };"),
    ("value class", "value class V { };"),
    (
        "interface class",
        "interface class I { virtual void f() = 0; };",
    ),
    ("delegate", "delegate void D(int n);"),
    (
        "event",
        "delegate void D(int n); ref class R { event D^ e; };",
    ),
    (
        "property",
        "ref class R { property int P { int get() { return 1; } } };",
    ),
    // C++/CX — WinRT, which `vccorlib.h` and `collection.h` are written in.
    (
        "ref new",
        "void f() { throw ref new OutOfMemoryException; }",
    ),
    ("a hat type", "void f() { Object^ o = nullptr; (void)o; }"),
    ("a percent reference", "void f(Object% r);"),
    ("a default namespace", "long f(const ::default::char16* p);"),
    ("a ref class keyword sequence", "ref class R sealed { };"),
    (
        "safe_cast on a handle",
        "void f() { Object^ o = safe_cast<Object^>(nullptr); (void)o; }",
    ),
];

#[test]
fn every_gcc_extension_parses_without_an_error() {
    assert_all_parse("GCC", GCC);
}

#[test]
fn every_clang_extension_parses_without_an_error() {
    assert_all_parse("Clang", CLANG);
}

#[test]
fn every_msvc_extension_parses_without_an_error() {
    assert_all_parse("MSVC", MSVC);
}
