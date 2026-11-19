//! **Where a `{` goes — and `else`/`catch`/`while` after a `}`** — `BreakBeforeBraces` and its `BraceWrapping` table.
//!
//! ```text
//! Attach     void f() {          LLVM, Google
//! Allman     void f()\n{
//! GNU        void f()\n{          (and the brace indented for a control statement — the indent is a later step)
//! Custom     the BraceWrapping.* table decides each construct separately   (Microsoft's style is Custom)
//! ```
//!
//! The table is what makes Microsoft's style what it is: `AfterFunction: true`, `AfterUnion: false`,
//! `AfterControlStatement: Always`, `BeforeElse: true`, `SplitEmptyRecord: true` — five answers for five constructs in
//! one file, which is why a single "attaches" flag cannot be the implementation.

use crate::config::Style;
use crate::options::StyleOption;
use crate::roles::BraceKind;

/// **Does the `{` of this construct open a line?**
///
/// The five preset spellings clang-format accepts that are not `Custom` decide without the table; `Custom` reads it.
/// A value this layer does not know (`Stroustrup`, `WebKit`, `Mozilla`, `Linux`) attaches, and that is **recorded as
/// not implemented** rather than guessed at: their wrapping is a style definition of its own, and a wrong guess would
/// be a wrong line on every file.
pub fn a_brace_starts_a_line(style: &Style, kind: BraceKind) -> bool {
    match style.text(StyleOption::BreakBeforeBraces) {
        "Allman" | "GNU" | "Whitesmiths" => true,
        "Custom" => match kind {
            BraceKind::Function => style.flag(StyleOption::BraceWrappingAfterFunction),
            BraceKind::Class => style.flag(StyleOption::BraceWrappingAfterClass),
            BraceKind::Struct => style.flag(StyleOption::BraceWrappingAfterStruct),
            BraceKind::Union => style.flag(StyleOption::BraceWrappingAfterUnion),
            BraceKind::Enum => style.flag(StyleOption::BraceWrappingAfterEnum),
            BraceKind::Namespace => style.flag(StyleOption::BraceWrappingAfterNamespace),
            BraceKind::ExternBlock => style.flag(StyleOption::BraceWrappingAfterExternBlock),
            BraceKind::Lambda => style.flag(StyleOption::BraceWrappingBeforeLambdaBody),
            // **One option for all five control kinds**: `Always` wraps, `Never` attaches, and `MultiLine` wraps a
            // body whose header spans several lines — which this layer cannot yet tell, so it attaches, the
            // conservative half of the two.
            BraceKind::Control | BraceKind::Try | BraceKind::Catch => {
                style.text(StyleOption::BraceWrappingAfterControlStatement) == "Always"
            }
        },
        _ => false,
    }
}

/// **Does `else` open a line** (rather than stand on the `}` line)?
pub fn else_starts_a_line(style: &Style) -> bool {
    match style.text(StyleOption::BreakBeforeBraces) {
        "Allman" | "GNU" | "Whitesmiths" => true,
        "Custom" => style.flag(StyleOption::BraceWrappingBeforeElse),
        _ => false,
    }
}

/// **Does `catch` open a line?**
pub fn catch_starts_a_line(style: &Style) -> bool {
    match style.text(StyleOption::BreakBeforeBraces) {
        "Allman" | "GNU" | "Whitesmiths" => true,
        "Custom" => style.flag(StyleOption::BraceWrappingBeforeCatch),
        _ => false,
    }
}

/// **Does `while` of a `do … while` open a line?**
pub fn while_starts_a_line(style: &Style) -> bool {
    match style.text(StyleOption::BreakBeforeBraces) {
        "Allman" | "GNU" | "Whitesmiths" => true,
        "Custom" => style.flag(StyleOption::BraceWrappingBeforeWhile),
        _ => false,
    }
}

/// **Does an empty body stay split over three lines?** — `SplitEmptyFunction`, `SplitEmptyRecord`,
/// `SplitEmptyNamespace`.
///
/// Only a `Custom` style asks: under `Attach` an empty body is `{}`, and clang-format's own presets pair the split
/// with a wrapping table that puts the braces on their own lines. Microsoft's style splits all three.
pub fn split_empty_body(style: &Style, kind: BraceKind) -> bool {
    if style.text(StyleOption::BreakBeforeBraces) != "Custom" {
        return false;
    }
    match kind {
        BraceKind::Function | BraceKind::Lambda => {
            style.flag(StyleOption::BraceWrappingSplitEmptyFunction)
        }
        BraceKind::Namespace => style.flag(StyleOption::BraceWrappingSplitEmptyNamespace),
        BraceKind::Class | BraceKind::Struct | BraceKind::Union | BraceKind::Enum => {
            style.flag(StyleOption::BraceWrappingSplitEmptyRecord)
        }
        _ => false,
    }
}
