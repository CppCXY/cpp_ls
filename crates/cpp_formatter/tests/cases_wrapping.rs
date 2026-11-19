//! **Category: brace placement** — `BreakBeforeBraces` and the `BraceWrapping` table, against clang-format 23.1.5.
//!
//! The fixtures are the shapes every file has, and the expected side is the oracle's text (`clang-format
//! -style=Microsoft`, `-style=LLVM`, `-style=GNU`). The rule being pinned is not "a brace is attached" but **which
//! construct the brace opens**: Microsoft wraps a function's, a class's and a control statement's, attaches a
//! `union`'s, and splits an empty record's.
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

/// **A `case` body's brace is the label's own option** (`AfterCaseLabel`), not the control statement's: Microsoft and
/// LLVM attach it (`case 1: {`), and GNU wraps it.
///
/// GNU's answer is **not asserted**: that style also puts the return type on its own line (`void` / `f ()`) and
/// indents the brace (`IndentBraces`), neither of which is implemented — they are recorded in
/// `docs/formatter-diff.md` rather than approximated here.
#[test]
fn a_case_body_brace_is_attached_where_the_style_says() {
    let input =
        "void f() {\n  switch (x) {\n    case 1: {\n      g();\n      break;\n    }\n  }\n}\n";
    let microsoft = "\
void f()
{
    switch (x)
    {
    case 1: {
        g();
        break;
    }
    }
}
";
    check(
        Style::preset("Microsoft").expect("Microsoft is a preset"),
        input,
        microsoft,
    );
}

/// **An empty body''s placement is its own decision.** `Attach` writes `{}` for everything; Microsoft splits a
/// function''s and a record''s but attaches a `union`''s (`AfterUnion: false`); `Allman` writes `{}` for a function and
/// splits a record. The rule that produces all three: **split only when the construct wraps *and* the style splits
/// it**.
#[test]
fn an_empty_body_is_placed_by_its_style() {
    let input = "void f() {}\nstruct S {};\nunion U {};\n";

    check(
        Style::preset("LLVM").expect("LLVM is a preset"),
        input,
        input,
    );

    check(
        Style::preset("Microsoft").expect("Microsoft is a preset"),
        input,
        "void f()\n{\n}\nstruct S\n{\n};\nunion U {};\n",
    );

    let mut allman = Style::preset("LLVM").expect("LLVM is a preset");
    allman.set_from_text("BreakBeforeBraces", "Allman");
    check(
        allman,
        input,
        "void f() {}\nstruct S\n{\n};\nunion U\n{\n};\n",
    );
}
/// **An empty control body is `{\n}` in every style** — `AllowShortBlocksOnASingleLine: Never` splits it even under
/// `Attach` — while an empty function, lambda or record body is `{}` (measured: LLVM writes `if (x) {` / `}` but
/// `void f() {}` and `struct T {};` in the same file).
#[test]
fn an_empty_control_body_is_split_in_every_style() {
    let input = "void f() {\n  if (x) {}\n  try {} catch (...) {}\n  auto g = []() {};\n  struct T {};\n}\n";

    let llvm = "\
void f() {
  if (x) {
  }
  try {
  } catch (...) {
  }
  auto g = []() {};
  struct T {};
}
";
    check(
        Style::preset("LLVM").expect("LLVM is a preset"),
        input,
        llvm,
    );

    let microsoft = "\
void f()
{
    if (x)
    {
    }
    try
    {
    }
    catch (...)
    {
    }
    auto g = []() {};
    struct T
    {
    };
}
";
    check(
        Style::preset("Microsoft").expect("Microsoft is a preset"),
        input,
        microsoft,
    );
}
/// **A file's own header is promoted above the rest of its block** — `IncludeIsMainRegex`, decided by the name.
#[test]
fn the_files_own_header_sorts_first() {
    let input = "#include \"b.h\"\n#include \"a.h\"\n";
    let style = Style::preset("LLVM").expect("LLVM is a preset");
    let named = cpp_formatter::format_named(input, Some("b"), &style);
    assert_eq!(named, "#include \"b.h\"\n#include \"a.h\"\n");
    // Without the name there is no own header, and the block is sorted as text.
    let unnamed = cpp_formatter::format_named(input, None, &style);
    assert_eq!(unnamed, "#include \"a.h\"\n#include \"b.h\"\n");
}

/// **`Attach` keeps the author's line when there is no column limit.** clang-format reflows only with a limit; with
/// `ColumnLimit: 0` an Allman brace written by the author stays on its own line, and with a limit it is attached.
#[test]
fn attach_joins_only_when_the_style_has_a_column_limit() {
    let input = "void f()\n{\n  g();\n}\n";

    // **The short-function join is turned off here** so that the only rule under test is the brace's: with `All`
    // (LLVM's default) the whole body would join and the brace question would not be observable.
    let mut unlimited = Style::preset("LLVM").expect("LLVM is a preset");
    unlimited.set_from_text("ColumnLimit", "0");
    unlimited.set_from_text("AllowShortFunctionsOnASingleLine", "None");
    check(unlimited, input, input);

    let mut limited = Style::preset("LLVM").expect("LLVM is a preset");
    limited.set_from_text("ColumnLimit", "80");
    limited.set_from_text("AllowShortFunctionsOnASingleLine", "None");
    check(limited, input, "void f() {\n  g();\n}\n");
}
