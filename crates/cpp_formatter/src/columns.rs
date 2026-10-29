//! **The column model: what a column number means.**
//!
//! Every decision this crate makes about *where* something sits — an aligned trailing comment, a
//! continuation line, a column limit — is a decision in columns, and a column is not a byte and not
//! a `char`: a tab moves to the next tab stop, a CJK character occupies two cells in a terminal and
//! in clang-format's own model, a combining mark occupies none.
//!
//! So there is one model, here, and everything that needs a column asks it.

use crate::config::Style;
use crate::options::StyleOption;

/// The column model of one style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Columns {
    tab_width: usize,
}

impl Columns {
    pub fn new(style: &Style) -> Self {
        Self {
            tab_width: style.count(StyleOption::TabWidth).max(1),
        }
    }

    pub fn tab_width(&self) -> usize {
        self.tab_width
    }

    /// **How wide `text` is when it starts at `from`**, and where it ends.
    pub fn advance(&self, from: usize, text: &str) -> usize {
        let mut column = from;
        for character in text.chars() {
            column += match character {
                '\t' => self.tab_width - (column % self.tab_width),
                // A line break resets the column: a caller that hands over a gap with newlines in it gets the column
                // of the last line, which is what "where does this end" means.
                '\n' => {
                    column = 0;
                    0
                }
                '\r' => 0,
                _ => width_of(character),
            };
        }
        column
    }

    /// The width of `text` on its own line.
    pub fn width(&self, text: &str) -> usize {
        self.advance(0, text)
    }
}

/// How many columns one character occupies.
fn width_of(character: char) -> usize {
    let code = character as u32;
    if is_combining(code) {
        0
    } else if is_wide(code) {
        2
    } else {
        1
    }
}

/// Zero width: combining marks, and the invisible separators a comment can carry.
fn is_combining(code: u32) -> bool {
    matches!(code,
        0x0300..=0x036F
        | 0x0483..=0x0489
        | 0x0591..=0x05BD
        | 0x0610..=0x061A
        | 0x064B..=0x065F
        | 0x0670
        | 0x06D6..=0x06DC
        | 0x0900..=0x0903
        | 0x093A..=0x094F
        | 0x0E31
        | 0x0E34..=0x0E3A
        | 0x200B..=0x200F
        | 0x20D0..=0x20F0
        | 0xFE00..=0xFE0F
        | 0xFE20..=0xFE2F
        | 0xFEFF
        | 0x1AB0..=0x1AFF
        | 0x1DC0..=0x1DFF
        | 0xE0100..=0xE01EF
    )
}

/// Two columns: the East Asian `W` and `F` ranges that appear in source code and in comments.
fn is_wide(code: u32) -> bool {
    matches!(code,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xA960..=0xA97F
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE10..=0xFE19
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1_7000..=0x1_87FF
        | 0x1_F004..=0x1_F9FF
        | 0x2_0000..=0x3_FFFD
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Style;

    fn llvm() -> Columns {
        Columns::new(&Style::default_style())
    }

    #[test]
    fn ascii_is_one_column_per_character() {
        let columns = llvm();
        assert_eq!(columns.width("abc"), 3);
        assert_eq!(columns.advance(7, "abc"), 10);
    }

    #[test]
    fn a_tab_reaches_the_next_tab_stop() {
        let columns = llvm();
        assert_eq!(columns.tab_width(), 8);
        assert_eq!(columns.advance(0, "\t"), 8);
        assert_eq!(columns.advance(3, "\t"), 8);
        assert_eq!(columns.advance(8, "\t"), 16);
        assert_eq!(columns.advance(1, "a\tb"), 9);
    }

    #[test]
    fn a_cjk_character_is_two_columns() {
        let columns = llvm();
        assert_eq!(columns.width("中"), 2);
        assert_eq!(columns.width("中文"), 4);
        assert_eq!(columns.width("a中b"), 4);
        assert_eq!(columns.width("，。"), 4);
    }

    #[test]
    fn a_combining_mark_is_no_column() {
        let columns = llvm();
        assert_eq!(columns.width("e\u{0301}"), 1);
        assert_eq!(columns.width("\u{200B}"), 0);
        assert_eq!(columns.width("\u{FE0F}"), 0);
    }

    #[test]
    fn an_emoji_is_two_columns_and_a_line_break_resets() {
        let columns = llvm();
        assert_eq!(columns.width("🙂"), 2);
        assert_eq!(columns.advance(0, "ab\ncd"), 2);
        assert_eq!(columns.advance(5, "\n"), 0);
    }

    #[test]
    fn the_tab_width_comes_from_the_style() {
        let mut style = Style::default_style();
        style.set_from_text("TabWidth", "4");
        let columns = Columns::new(&style);
        assert_eq!(columns.tab_width(), 4);
        assert_eq!(columns.advance(0, "\t"), 4);
        assert_eq!(columns.advance(5, "\t"), 8);
    }
}
