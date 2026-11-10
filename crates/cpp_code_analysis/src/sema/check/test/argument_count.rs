//! **`argument-count`** — more arguments than the callee takes, or fewer than it needs.
//!
//! The rule's direction of error is "quiet when unsure": a call is legal if **any** declaration of that signature can
//! take it, and a declaration whose parameters this layer cannot read is not judged. Everything in this file is about
//! the shapes where a naive arity count would be wrong — a default argument, a pack, an overload set, a call through
//! a member, a constructor — and each one is silent for a reason the test states.

use super::{assert_reported, assert_silent_about, codes_in, codes_of};
use crate::DiagnosticCode;

/// The plain case, both directions: this is the rule's whole reason to exist.
#[test]
fn a_call_with_too_many_or_too_few_arguments_is_reported() {
    let too_many = codes_of("void f(int a, int b) {}\nvoid g() { f(1, 2, 3); }\n");
    assert_reported(&too_many, DiagnosticCode::ArgumentCount, "`f` takes two");

    let too_few = codes_of("void f(int a, int b) {}\nvoid g() { f(1); }\n");
    assert_reported(&too_few, DiagnosticCode::ArgumentCount, "`f` needs two");

    let exact = codes_of("void f(int a, int b) {}\nvoid g() { f(1, 2); }\n");
    assert_silent_about(&exact, DiagnosticCode::ArgumentCount, "two for two");
}

/// **A default argument is written once, in the declaration** — and the definition may not repeat it.
///
/// `LuaExpectedError(std::string_view message, LuaTokenKind expectedToken = 0);` in a header, and the definition
/// without the default in the source: the definition alone says two arguments are required, and eight calls were
/// underlined on `EmmyLuaCodeStyle` until the candidates were merged per signature at the most permissive arity.
#[test]
fn a_default_argument_makes_the_parameter_optional() {
    let codes = codes_in(
        &[
            (
                "/p/api.h",
                "void report(int a, int b = 0);\nint twice(int a, int b = 1);\n",
            ),
            (
                "/p/use.cpp",
                "#include \"api.h\"\n\
                 void report(int a, int b) { (void)a; (void)b; }\n\
                 int twice(int a, int b) { return a * b; }\n\
                 void g() { report(1); (void)twice(2); report(1, 2); }\n",
            ),
        ],
        "/p/use.cpp",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::ArgumentCount,
        "both parameters are defaulted in the header",
    );
}

/// **A pack takes any number of arguments, and an ellipsis anywhere in the parameter makes it one.**
///
/// `Args&&... args` is the spelling a template writes, and it used to be counted as one required parameter.
#[test]
fn a_pack_and_a_c_style_ellipsis_take_any_number() {
    let pack = codes_of(
        "template <class... Args>\nvoid f(int first, Args&&... rest) {}\nvoid g() { f(1, 2, 3, 4); }\n",
    );
    assert_silent_about(
        &pack,
        DiagnosticCode::ArgumentCount,
        "a pack takes the rest",
    );

    let c_style =
        codes_of("#include <cstdarg>\nvoid f(int count, ...);\nvoid g() { f(1, 2, 3); }\n");
    assert_silent_about(
        &c_style,
        DiagnosticCode::ArgumentCount,
        "`...` takes the rest",
    );
}

/// **One overload that takes the arguments keeps the check silent** — and when none does, it is reported.
#[test]
fn an_overload_set_is_silent_when_one_candidate_fits() {
    let fits = codes_of(
        "struct W {\n    void resize(int n);\n    void resize(int n, int extra);\n};\n\
         void g(W& w) { w.resize(1); w.resize(1, 2); }\n",
    );
    assert_silent_about(
        &fits,
        DiagnosticCode::ArgumentCount,
        "each call fits one overload",
    );

    let none = codes_of(
        "struct W {\n    void resize(int n);\n    void resize(int n, int extra);\n};\n\
         void g(W& w) { w.resize(1, 2, 3); }\n",
    );
    assert_reported(
        &none,
        DiagnosticCode::ArgumentCount,
        "no overload takes three",
    );
}

/// **A call of a class is a constructor call**, and the constructors' arities are what it is judged against.
/// **A call of a class is not judged by this rule today** — a gap, stated rather than hidden.
///
/// `W()` with only `W(int a)` declared is one argument short, and the rule reports nothing: the callee it resolves is
/// the *class*, whose arity is its constructors', and this rule reads functions. The signature layer and the inlay
/// hints answer for constructors (see `signature::constructors_in_the_index` and `inlay`'s constructor branch), so the
/// gap is here only, and this fixture is what will say so when it closes: change the first case to
/// `assert_reported` on the day constructor calls are judged.
#[test]
fn a_call_of_a_class_is_not_judged_today() {
    let empty = codes_of("struct W {\n    W(int a);\n};\nW make() { return W(); }\n");
    assert_silent_about(
        &empty,
        DiagnosticCode::ArgumentCount,
        "the gap: `W` declares one constructor taking one, and this rule does not ask it",
    );

    // **A call of the class through a member function is judged, and that half works** — the rule's function path.
    let member =
        codes_of("struct W {\n    void resize(int n);\n};\nvoid g(W& w) { w.resize(); }\n");
    assert_reported(&member, DiagnosticCode::ArgumentCount, "`resize` takes one");
}

/// **A call this layer cannot resolve is not judged at all** — a function pointer's arity is its type's, and the
/// rule does not read types that way.
#[test]
fn an_unresolvable_callee_is_not_judged() {
    let codes = codes_of("void g() { unknown(1, 2, 3); }\n");
    assert_silent_about(
        &codes,
        DiagnosticCode::ArgumentCount,
        "nothing declares `unknown`, so nothing says how many it takes",
    );
}
