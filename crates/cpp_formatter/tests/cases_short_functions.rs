//! **Category: short functions and lambdas** — `AllowShortFunctionsOnASingleLine`, against clang-format 23.1.5.
//!
//! Two shapes, one option, and the fixtures are the oracle's answers verbatim:
//!
//! * a function whose body holds **one statement** and whose whole text fits joins onto one line under `All`, a class
//!   method only under `Inline`, nothing under `None` — and a body with **two** statements never joins;
//! * a body the style refuses to join is **broken**, so a one-line function under `None` comes back as three lines;
//! * a **lambda** body joins on its own, even when the lambda's signature already spans lines, because its width is the
//!   width from its `{` to the end of the line.

use cpp_formatter::config::Style;

const INPUT: &str = "\
void a() {
  g();
}
void b() {
  g();
  h();
}
class C {
  void m() {
    g();
  }
};
void d() {
  g();
} // trailing
void e() {
  f([](int v) {
    return v;
  });
}
";

fn check(style: Style, expected: &str) {
    let path = std::path::Path::new("snippet.cpp");
    let actual = cpp_formatter::format_file(path, INPUT, &style);
    assert_eq!(
        actual, expected,
        "formatting drifted\nexpected:\n{expected}\nactual:\n{actual}"
    );
    let again = cpp_formatter::format_file(path, expected, &style);
    assert_eq!(again, expected, "the expected text is not a fixed point");
}

/// `All` (LLVM's default): every fitting one-statement body joins, including a class method's.
#[test]
fn all_joins_every_fitting_body() {
    let expected = "\
void a() { g(); }
void b() {
  g();
  h();
}
class C {
  void m() { g(); }
};
void d() { g(); } // trailing
void e() {
  f([](int v) { return v; });
}
";
    check(Style::preset("LLVM").expect("LLVM is a preset"), expected);
}

/// `None`: nothing joins, and a body written on one line is **split** — `void d() { g(); }` becomes three lines.
#[test]
fn none_splits_a_one_line_body() {
    let expected = "\
void a() {
  g();
}
void b() {
  g();
  h();
}
class C {
  void m() {
    g();
  }
};
void d() {
  g();
} // trailing
void e() {
  f([](int v) { return v; });
}
";
    let mut style = Style::preset("LLVM").expect("LLVM is a preset");
    style.set_from_text("AllowShortFunctionsOnASingleLine", "None");
    check(style, expected);
}

/// `Inline`: only a class method joins; a free function's body keeps its lines.
#[test]
fn inline_joins_only_class_methods() {
    let expected = "\
void a() {
  g();
}
void b() {
  g();
  h();
}
class C {
  void m() { g(); }
};
void d() {
  g();
} // trailing
void e() {
  f([](int v) { return v; });
}
";
    let mut style = Style::preset("LLVM").expect("LLVM is a preset");
    style.set_from_text("AllowShortFunctionsOnASingleLine", "Inline");
    check(style, expected);
}
