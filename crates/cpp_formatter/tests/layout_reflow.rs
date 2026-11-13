//! **The layout pass**: a break before every logical line, then the indentation the formatter decides.
//!
//! [`cpp_formatter::format`] keeps the author's line structure, which is right for a file being edited and wrong for
//! text that has none — a macro's expansion is one line of half a kilobyte. [`cpp_formatter::reflow`] is the same
//! formatter on text whose line breaks are clang-format's own, and these cases pin what that means.

use cpp_formatter::{Style, format, reflow};

/// The style a file with no `.clang-format` gets: LLVM with the project's four-space indent.
fn style() -> Style {
    Style::fallback_style()
}

/// **A one-line block comes out as a block**, with each statement and brace on its own line.
#[test]
fn a_one_line_function_is_laid_out_as_a_block() {
    let source = "void f() { if (x) { g(1); } }";
    let laid_out = reflow(source, &style());
    assert_eq!(
        laid_out,
        "void f() {\n    if (x) {\n        g(1);\n    }\n}",
        "each logical line starts at its own indent: {laid_out:?}"
    );
}

/// **A statement that shares a line with the one before it is moved down**, which is the whole difference between
/// `format` and `reflow`.
#[test]
fn a_second_statement_is_moved_to_its_own_line() {
    let source = "int a = 1; int b = 2;";
    assert_eq!(format(source, &style()).trim(), source, "`format` keeps the line");
    assert_eq!(
        reflow(source, &style()),
        "int a = 1;\nint b = 2;",
        "`reflow` breaks where clang-format breaks"
    );
}

/// **A long logical line is broken at the points the breaker already chooses**, and indented as a continuation.
#[test]
fn a_long_statement_is_broken_at_a_comma() {
    let source = "void f() { g(100, 200, 300, 400, 500, 600, 700, 800, 900, 1000, 1100, 1200, 1300, 1400, 1500); }";
    let laid_out = reflow(source, &style());
    for line in laid_out.lines() {
        assert!(
            line.chars().count() <= 80,
            "no line runs past the column limit: {line:?} in\n{laid_out}"
        );
    }
    assert!(laid_out.lines().count() > 3, "the call is broken: {laid_out}");
}

/// **A directive is never broken**: a newline inside a `#define` body would end the macro, and the continuation lines
/// the author wrote are the macro's own.
#[test]
fn a_directive_keeps_its_lines() {
    let source = "#define MAX(a, b) do { \\\n    (a) > (b) ? (a) : (b); \\\n} while (0)\n";
    let laid_out = reflow(source, &style());
    assert_eq!(
        laid_out.matches('\n').count(),
        3,
        "no break was added inside the directive: {laid_out:?}"
    );
    assert!(
        laid_out.contains("(a) > (b)"),
        "the macro's own body survives: {laid_out:?}"
    );
}

/// **Laying out an already laid-out text changes nothing** — the pass is idempotent, which is what makes it safe to
/// apply to a card that is rebuilt on every hover.
#[test]
fn the_layout_is_idempotent() {
    let source = "struct Widget { int x; double y; }; void f() { for (int i = 0; i < 10; i++) { g(i); } }";
    let once = reflow(source, &style());
    let twice = reflow(&once, &style());
    assert_eq!(once, twice, "the second pass is a no-op");
}
/// **The brace style is the layout's decision**: `Attach` keeps `{`, `else` and `while` on the line of what they
/// follow, and `Allman` starts a line at every brace.
#[test]
fn the_brace_style_decides_where_a_brace_goes() {
    let source = "void f() { if (x) { g(); } else { h(); } do { i(); } while (y); }";
    let attached = reflow(source, &style());
    assert!(
        attached.contains("void f() {"),
        "`Attach` keeps the function's brace on its line: {attached}"
    );
    assert!(
        attached.contains("} else {"),
        "and the `else` on the line of the close brace: {attached}"
    );
    assert!(
        attached.contains("} while (y);"),
        "and a `do`'s `while` on the line of its close brace: {attached}"
    );

    let mut allman = Style::fallback_style();
    allman.set_from_text("BreakBeforeBraces", "Allman");
    let broken = reflow(source, &allman);
    assert!(
        broken.contains("void f()\n{"),
        "`Allman` starts a line at the brace: {broken}"
    );
}