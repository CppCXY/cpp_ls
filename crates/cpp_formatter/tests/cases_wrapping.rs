//! **Category: brace placement** — `BreakBeforeBraces` and the `BraceWrapping` table, against clang-format 15.0.1.
//!
//! The fixtures are the shapes every file has, and the expected side is the oracle's text (Visual Studio's own
//! `clang-format.exe`, `-style=Microsoft` and `-style=LLVM`). The rule being pinned is not "a brace is attached" but
//! **which construct the brace opens**: Microsoft wraps a function's, a class's and a control statement's, attaches
//! a `union`'s, and splits an empty record's.
//!
//! Two behaviours are deliberately excluded and documented where they live: `Stroustrup`/`WebKit`/`Mozilla`/`Linux`
//! are not implemented (they attach, and `wrapping.rs` says so), and an empty body is `{}` unless the style's
//! `SplitEmpty*` asks for the split.

use cpp_formatter::config::Style;

fn check(style: Style, input: &str, expected: &str) {
    let path = std::path::Path::new("snippet.cpp");
    let actual = cpp_formatter::format_file(path, input, &style);
    let actual = actual.trim_end_matches('\n');
    let expected = expected.trim_end_matches('\n');
    assert_eq!(
        actual, expected,
        "formatting drifted\ninput:\n{input}\nexpected:\n{expected}\nactual:\n{actual}"
    );
    let again = cpp_formatter::format_file(path, expected, &style);
    assert_eq!(again, expected, "the expected text is not a fixed point");
}

/// **Microsoft's table, on one file**: namespace, class, union, enum, function, `if`/`else`, `do … while` and an
/// empty record — each wrapped or attached by its own rule.
#[test]
fn microsoft_wraps_by_construct() {
    let input = "\
namespace ns {
class Widget {
public:
    void run();
};
union U {
    int i;
};
enum class Kind {
    A,
    B
};
void Widget::run() {
    if (x) {
        y();
    } else {
        z();
    }
    do {
        v();
    } while (u);
    struct S {
    };
}
}
";
    let expected = "\
namespace ns
{
class Widget
{
  public:
    void run();
};
union U {
    int i;
};
enum class Kind
{
    A,
    B
};
void Widget::run()
{
    if (x)
    {
        y();
    }
    else
    {
        z();
    }
    do
    {
        v();
    } while (u);
    struct S
    {
    };
}
} // namespace ns
";
    check(
        Style::preset("Microsoft").expect("Microsoft is a preset"),
        input,
        expected,
    );
}

/// **`Attach` keeps the author's line when there is no column limit.** clang-format reflows only with a limit; with
/// `ColumnLimit: 0` an Allman brace written by the author stays on its own line, and with a limit it is attached.
#[test]
fn attach_joins_only_when_the_style_has_a_column_limit() {
    let input = "void f()\n{\n  g();\n}\n";

    let mut unlimited = Style::preset("LLVM").expect("LLVM is a preset");
    unlimited.set_from_text("ColumnLimit", "0");
    check(unlimited, input, input);

    let mut limited = Style::preset("LLVM").expect("LLVM is a preset");
    limited.set_from_text("ColumnLimit", "80");
    check(limited, input, "void f() {\n  g();\n}\n");
}
