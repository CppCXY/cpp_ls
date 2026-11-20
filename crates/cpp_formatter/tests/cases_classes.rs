//! **Category: classes** — inline cases, no files, no clang-format at test time.
//!
//! Each case is a pair of strings: the input as written, and the text this formatter must produce for it. The pairs
//! were taken from clang-format 22.1.3 for the same style that `style()` documents, so the expected side is the
//! oracle's answer rather than whatever this implementation happened to print.

use cpp_formatter::config::Style;

/// Format `input` with this project's style and compare against the oracle's answer, then re-format the answer.
///
/// The second half is the property a user feels: formatting text that is already formatted has to leave it alone.
#[track_caller]
fn check(input: &str, expected: &str) {
    let style = style();
    let path = std::path::Path::new("snippet.cpp");
    let actual = cpp_formatter::format_file(path, input, &style);
    // The trailing newline is the *source's* (the formatter preserves its absence), so the cases compare the
    // content and leave the file's final byte to `trim_end_matches` on both sides.
    let actual = actual.trim_end_matches('\n');
    let expected = expected.trim_end_matches('\n');
    assert_eq!(
        actual, expected,
        "formatting drifted\ninput:\n{input}\nexpected:\n{expected}\nactual:\n{actual}"
    );
    let again = cpp_formatter::format_file(path, expected, &style);
    assert_eq!(again, expected, "the expected text is not a fixed point");
}

/// The style the expected strings were produced with.
fn style() -> Style {
    let mut style = Style::preset("LLVM").expect("LLVM is a preset");
    style.set_from_text("IndentWidth", "4");
    style.set_from_text("ContinuationIndentWidth", "8");
    style.set_from_text("ColumnLimit", "0");
    style
}

/// `07_members_block`
#[test]
fn classes_07_members_block() {
    check(
        r#"class A {
public:
    int  x;

private:
    int  y;
};"#,
        r#"class A {
  public:
    int x;

  private:
    int y;
};"#,
    );
}

/// `08_ctor_block`
#[test]
fn classes_08_ctor_block() {
    check(
        r#"class B {
public:
    B() : x_(0) {}

    int x_;
};"#,
        r#"class B {
  public:
    B() : x_(0) {}

    int x_;
};"#,
    );
}

/// `09_inherit_block`
#[test]
fn classes_09_inherit_block() {
    check(
        r#"class C : public A {
public:
    void f() override;
};"#,
        r#"class C : public A {
  public:
    void f() override;
};"#,
    );
}

/// `10_enum_block`
#[test]
fn classes_10_enum_block() {
    check(
        r#"enum class E {
    A,
    B,
};"#,
        r#"enum class E {
    A,
    B,
};"#,
    );
}

/// `11_operator_block`
#[test]
fn classes_11_operator_block() {
    check(
        r#"struct S {
    S &operator=(const S &) = default;
};"#,
        r#"struct S {
    S &operator=(const S &) = default;
};"#,
    );
}

/// `12_template_member`
#[test]
fn classes_12_template_member() {
    check(
        r#"class D {
public:
    template <typename T>
    void f(T v);
};"#,
        r#"class D {
  public:
    template <typename T>
    void f(T v);
};"#,
    );
}

/// **`= default` is not a `default:` label, and a template head is a line of its own** — both measured on the
/// nlohmann json corpus under `Google` (where the two styles' `AccessModifierOffset`/`IndentWidth` made the label
/// mistake visible: every member after `C() = default;` came out two columns too deep).
#[test]
fn classes_20_an_equality_default_is_not_a_label() {
    let mut google = Style::preset("Google").expect("Google is a preset");
    google.set_from_text("ColumnLimit", "80");
    let path = std::path::Path::new("snippet.cpp");
    let input = "class C {\n public:\n  C(int p) : x_(p) {}\n  C() = default;\n  int x_;\n};\n";
    assert_eq!(cpp_formatter::format_file(path, input, &google), input);
    let template = "template <typename T>\nclass lexer_base {\n public:\n  int x;\n};\n";
    assert_eq!(
        cpp_formatter::format_file(path, template, &google),
        template
    );
}

/// **A template head that spans lines aligns its arguments under the `<`**, and the declaration after it starts a new
/// line at the block's column — clang-format 23.1.5 (`LLVM`) writes this fixture byte for byte. Two rules were missing:
/// `template <typename …>` / `template <class …>` did not mark the angle at all, and the "the line after a template's
/// `>` is a new declaration" test looked at the line's start instead of the declaration's.
#[test]
fn classes_21_a_template_head_that_spans_lines() {
    let llvm = Style::preset("LLVM").expect("LLVM is a preset");
    let path = std::path::Path::new("snippet.cpp");
    let input = "template <typename BasicJsonType, typename InputAdapterType,\n          bool Collect = false,\n          typename LexerType = lexer<BasicJsonType, InputAdapterType>>\nclass json_sax_dom_parser : public json_sax_dom_error_state {\npublic:\n  void f();\n};\n";
    assert_eq!(cpp_formatter::format_file(path, input, &llvm), input);
}

/// **A template head's continuation lines, a `!`-prefixed template argument and a directive before the head**: the
/// nested `std::enable_if<!…, int>` list aligns under *its* `<` (34), and the declaration after the head starts at the
/// block's column even though a `#ifdef` line stands before the head. clang-format 23.1.5 (`LLVM`) writes the fixture
/// byte for byte.
#[test]
fn classes_22_a_directive_before_a_template_head() {
    let llvm = Style::preset("LLVM").expect("LLVM is a preset");
    let path = std::path::Path::new("snippet.cpp");
    let input = "#ifdef JSON_HAS_CPP_17\ntemplate <typename BasicJsonType, typename T,\n          typename std::enable_if<!nlohmann::detail::is_basic_json<T>::value,\n                                  int>::type = 0>\nvoid from_json(const BasicJsonType &j, std::optional<T> &opt) {\n  int x = 1;\n}\n#endif\n";
    assert_eq!(cpp_formatter::format_file(path, input, &llvm), input);
}

/// **A `template <` whose `<` ends the line**: clang-format puts the argument list one `ContinuationIndentWidth` past
/// the statement (6 for a member at 2), not at the statement's own column. Measured on nlohmann json's `json_sax.hpp`;
/// the oracle writes this fixture byte for byte.
#[test]
fn classes_23_a_template_angle_that_ends_the_line() {
    let llvm = Style::preset("LLVM").expect("LLVM is a preset");
    let path = std::path::Path::new("snippet.cpp");
    let input = "class C {\n  template <\n      typename ObjectType, typename StringType,\n      bool = is_detected<detect_emplace_new_key, ObjectType, StringType>::value>\n  struct detect_indexable_keys : std::false_type {};\n};\n";
    assert_eq!(cpp_formatter::format_file(path, input, &llvm), input);
}

/// **A record keyword at the end of a line continues the declaration**: `class` then the name on the next line is one
/// `ContinuationIndentWidth` in, while a `{` after the keyword still sits at the block's column. clang-format 23.1.5
/// writes the fixture byte for byte.
#[test]
fn classes_24_a_record_name_on_its_own_line() {
    let llvm = Style::preset("LLVM").expect("LLVM is a preset");
    let path = std::path::Path::new("snippet.cpp");
    let input = "template <typename BasicJsonType>\nclass\n    iter_impl // NOLINT(cppcoreguidelines-special-member-functions,hicpp-special-member-functions)\n{\n  int x;\n};\n";
    assert_eq!(cpp_formatter::format_file(path, input, &llvm), input);
}

/// **An enumerator's value on the next line is a continuation**: `syntax_error =` then `101,` sits one
/// `ContinuationIndentWidth` in, not at the body's own column (nlohmann json's `exceptions.hpp`, where every exception
/// id is written that way). The enum alignment must not pull it back to the item column.
#[test]
fn classes_25_an_enumerator_value_on_its_own_line() {
    let llvm = Style::preset("LLVM").expect("LLVM is a preset");
    let path = std::path::Path::new("snippet.cpp");
    let input = "enum class exception_id : int {\n  // parse_error\n  syntax_error =\n      101, ///< unexpected token or invalid literal while parsing JSON\n  patch_not_an_array =\n      104, ///< a JSON Patch document is not an array of objects\n};\n";
    assert_eq!(cpp_formatter::format_file(path, input, &llvm), input);
}
