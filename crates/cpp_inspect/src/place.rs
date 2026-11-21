//! **The position a question is about**, given one of the three ways a report states it.
//!
//! A byte offset, the text to find (which is what a bug report quotes — a needle, often with a `--shift` into it), or a
//! line and column. Resolved once here, so that no command re-implements "find the text": the twenty probes this crate
//! replaces each had their own version of it, and two of them were wrong in different ways.

use clap::Args;

/// Where in the file the question is asked.
#[derive(Debug, Clone, Args)]
pub struct Place {
    /// A byte offset in the file's own text.
    #[arg(long, global = true)]
    pub at: Option<usize>,
    /// Text to find: the offset is its first byte, adjusted by `--occurrence`, `--last` and `--shift`.
    #[arg(long, global = true)]
    pub needle: Option<String>,
    /// Which occurrence of `--needle`, 1-based.
    #[arg(long, global = true, default_value_t = 1)]
    pub occurrence: usize,
    /// The **last** occurrence of `--needle`, instead of the `--occurrence`th.
    #[arg(long, global = true)]
    pub last: bool,
    /// Bytes from the resolved position — `--shift -1` is the byte before it.
    #[arg(
        long,
        global = true,
        default_value_t = 0,
        allow_negative_numbers = true
    )]
    pub shift: isize,
    /// A 1-based line.
    #[arg(long, global = true)]
    pub line: Option<u32>,
    /// A 1-based byte column, with `--line`.
    #[arg(long, global = true)]
    pub col: Option<u32>,
}

impl Place {
    /// **The offset the question is asked at**, in the file's own bytes.
    pub fn offset(&self, text: &str) -> Result<usize, String> {
        let base = match (&self.at, &self.needle, (self.line, self.col)) {
            (Some(at), None, (None, None)) => {
                if *at > text.len() {
                    return Err(format!(
                        "--at {at} is past the end of the file ({} bytes)",
                        text.len()
                    ));
                }
                *at
            }
            (None, Some(needle), (None, None)) => {
                let places: Vec<usize> = text
                    .match_indices(needle.as_str())
                    .map(|(at, _)| at)
                    .collect();
                if places.is_empty() {
                    return Err(format!("--needle {needle:?} does not occur in the file"));
                }
                let chosen = match self.last {
                    true => places.len(),
                    false => self.occurrence,
                };
                if chosen == 0 || chosen > places.len() {
                    return Err(format!(
                        "--occurrence {chosen} is out of range: {needle:?} occurs {} time(s)",
                        places.len()
                    ));
                }
                places[chosen - 1]
            }
            (None, None, (Some(line), col)) => {
                let start = offset_of_line(text, line)
                    .ok_or_else(|| format!("--line {line} is past the end of the file"))?;
                let column = col.unwrap_or(1).saturating_sub(1) as usize;
                let at = start + column;
                if at > text.len() {
                    return Err(format!(
                        "--col {} is past the end of line {line}",
                        col.unwrap_or(1)
                    ));
                }
                at
            }
            _ => {
                return Err(
                    "give one of `--at`, `--needle`, or `--line`/`--col` to say where the question is".to_string(),
                );
            }
        };
        let shifted = base as isize + self.shift;
        usize::try_from(shifted)
            .map_err(|_| format!("--shift {} is before the start of the file", self.shift))
    }
}

/// A 1-based `(line, byte column)` for an offset — the coordinates a report writes.
pub fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rfind('\n')
        .map(|at| offset - at)
        .unwrap_or(offset + 1);
    (line, column)
}

/// The offset a 1-based line starts at.
fn offset_of_line(text: &str, line: u32) -> Option<usize> {
    match line {
        0 => None,
        1 => Some(0),
        line => text
            .match_indices('\n')
            .nth(line as usize - 2)
            .map(|(at, _)| at + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_needle_and_a_line_name_the_same_byte() {
        let text = "void f() {\n    int value = 1;\n}\n";
        let place = Place {
            at: None,
            needle: Some("int value".into()),
            occurrence: 1,
            last: false,
            shift: 4,
            line: None,
            col: None,
        };
        // `value` starts at byte 19: line 2 begins at 11, its indentation is four, `int ` is four more.
        assert_eq!(place.offset(text), Ok(19));
        let by_line = Place {
            at: None,
            needle: None,
            occurrence: 1,
            last: false,
            shift: 0,
            line: Some(2),
            col: Some(9),
        };
        assert_eq!(by_line.offset(text), Ok(19));
        assert_eq!(line_col(text, 19), (2, 9));
    }
}
