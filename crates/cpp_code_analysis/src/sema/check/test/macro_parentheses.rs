//! **`macro-parentheses`** — a macro's replacement list needs its own parentheses, or a caller's expression is spliced
//! in without its precedence.
//!
//! The rule reads the replacement list as **text** and asks one question: is there an operator that binds looser than a
//! primary expression, outside any nested parentheses? Its silence is where the care is, because most macros are not
//! expressions — a single token, a braced initialiser, a call, a declaration, an X-macro fragment (`##`), a statement,
//! or the already-parenthesised answer. Each of those splices nothing into a precedence-sensitive place.

use super::{assert_reported, assert_silent_about, codes_of};
use crate::DiagnosticCode;

/// **The shape the rule exists for**: an unparenthesised arithmetic replacement list.
#[test]
fn an_unparenthesised_expression_body_is_reported() {
    let twice = codes_of("#define TWICE(x) x * 2\nint f(int n) { return TWICE(n + 1); }\n");
    assert_reported(
        &twice,
        DiagnosticCode::MacroParentheses,
        "`x * 2` lets a caller's `n + 1` lose its precedence",
    );

    let sum = codes_of("#define SUM(a, b) a + b\nint f(int a, int b) { return SUM(a, b); }\n");
    assert_reported(
        &sum,
        DiagnosticCode::MacroParentheses,
        "`a + b` is the same defect with another operator",
    );
}

/// **A replacement list that starts with `(` is already the answer** — the caller's expression lands inside a group.
#[test]
fn a_body_that_is_already_parenthesised_is_not_reported() {
    let doubled = codes_of("#define TWICE(x) ((x) * 2)\nint f(int n) { return TWICE(n + 1); }\n");
    assert_silent_about(
        &doubled,
        DiagnosticCode::MacroParentheses,
        "the whole replacement list is one parenthesised expression",
    );

    let maximum = codes_of(
        "#define MAX(a, b) ((a) > (b) ? (a) : (b))\nint f(int a, int b) { return MAX(a, b); }\n",
    );
    assert_silent_about(
        &maximum,
        DiagnosticCode::MacroParentheses,
        "the answer the rule would recommend is not reported",
    );
}

/// **A body that is not an expression is not this rule's shape**: one token has no operator to lose, `#v` is the
/// preprocessor's own stringify, and a declaration's `=` belongs to its declarator rather than to an expression.
#[test]
fn a_body_that_is_not_an_expression_is_not_reported() {
    let one_token = codes_of("#define ANSWER 42\nint f() { return ANSWER; }\n");
    assert_silent_about(
        &one_token,
        DiagnosticCode::MacroParentheses,
        "a single token has no precedence to lose",
    );

    let stringified = codes_of("#define STRINGIFY(v) #v\nint f() { return 0; }\n");
    assert_silent_about(
        &stringified,
        DiagnosticCode::MacroParentheses,
        "`#v` is a lexer operator, not an expression",
    );

    let negative = codes_of("#define MINUS_ONE -1\nint f() { return MINUS_ONE; }\n");
    assert_silent_about(
        &negative,
        DiagnosticCode::MacroParentheses,
        "a leading `-` is unary, so there is no operator binding looser than its operand",
    );

    let declaration =
        codes_of("#define DECLARE(name, v) static int name = (v)\nint f() { return 0; }\n");
    assert_silent_about(
        &declaration,
        DiagnosticCode::MacroParentheses,
        "the body begins a declaration, whose caller's expression is already grouped",
    );
}

/// **A call groups whatever the caller splices in**, so a body that is one call is not a precedence problem.
#[test]
fn a_body_that_is_one_call_is_not_reported() {
    let codes = codes_of(
        "#define CALL(f, v) f(v)\nint g(int v) { return v; }\nint h(int v) { return CALL(g, v); }\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::MacroParentheses,
        "the argument is already inside the call's own parentheses",
    );
}

/// **`##` and `->` are not precedence-sensitive**: a paste makes one token, and an arrow is a suffix operator rather
/// than a binary `-`.
#[test]
fn a_paste_and_a_pointer_member_are_not_reported() {
    let paste = codes_of("#define FRAGMENT(name) COLOUR_##name = 1,\nint f() { return 0; }\n");
    assert_silent_about(
        &paste,
        DiagnosticCode::MacroParentheses,
        "an X-macro fragment is a token list, not an expression",
    );

    let arrow = codes_of("#define FIELD(t) t->value\nint f() { return 0; }\n");
    assert_silent_about(
        &arrow,
        DiagnosticCode::MacroParentheses,
        "`->` is a suffix, and the `-` before it is not a binary operator",
    );
}

/// **An empty replacement list and a statement macro are not expressions**: a header guard has nothing to
/// parenthesise, and a `do { … } while (0)` is a statement the caller cannot splice into.
#[test]
fn an_empty_body_and_a_statement_macro_are_not_reported() {
    let guard = codes_of("#ifndef API_H\n#define API_H\nint f();\n#endif\n");
    assert_silent_about(
        &guard,
        DiagnosticCode::MacroParentheses,
        "the guard's replacement list is empty",
    );

    let statement = codes_of(
        "#define SHOW(x) do { (void)(x); } while (0)\nint f(int n) { SHOW(n); return 0; }\n",
    );
    assert_silent_about(
        &statement,
        DiagnosticCode::MacroParentheses,
        "the body is a statement, and its operators are inside its own braces",
    );
}

/// **A macro that expands to a type is not this rule's shape**, however many `<` and `>` its spelling has.
///
/// Measured on asio's `async_result.hpp`: seven macros expanding to a template-id type —
/// `typename ::asio::detail::async_result_helper<ct, sig>::return_type` — were reported as "a caller's expression is
/// spliced in without its precedence". No caller's expression ever reaches a type: a template-id's `<` is not a
/// comparison, and a `typename`-headed body is a type fragment.
#[test]
fn a_macro_that_expands_to_a_type_is_silent() {
    let codes = codes_of(
        "# define RESULT(ct, sig) \\\n  typename ::asio::detail::async_result_helper< \\\n    ct, sig>::return_type\n\
         # define HANDLER(sig) ::asio::handler<sig>\nint f(int n) { return n + 1; }\n",
    );
    assert_silent_about(
        &codes,
        DiagnosticCode::MacroParentheses,
        "a type fragment has no precedence to get wrong",
    );

    // …and the rule still reports the shape it exists for.
    let still = codes_of("#define TWICE(x) x * 2\nint f(int n) { return TWICE(n + 1); }\n");
    assert_reported(
        &still,
        DiagnosticCode::MacroParentheses,
        "an expression body without its own parentheses is the finding",
    );
}
