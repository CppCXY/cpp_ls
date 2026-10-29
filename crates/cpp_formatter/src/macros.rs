//! **`AlignEscapedNewlines`**: the backslashes of a multi-line macro.
//!
//! ```cpp
//! #define MISALIGNED(a, b)                 \
//!     do {                                 \
//!         (void)(a);                       \
//!         (void)(b);                       \
//!     } while (0)
//! ```
//!
//! clang-format rewrites that to the columns below, which is what `AlignEscapedNewlines: Right` means:
//!
//! ```cpp
//! #define MISALIGNED(a, b) \
//!     do {                 \
//!         (void)(a);       \
//!         (void)(b);       \
//!     } while (0)
//! ```
//!
//! Measured against 22.1.3: the backslash of a run of escaped lines sits at `max(width of the code) + 1`, where the code
//! is the line without its backslash and trailing spaces. `Left` puts exactly one space before each backslash, and
//! `DontAlign` leaves the line as it was.
//!
//! # Why this is a pass over the output and not part of the renderer
//!
//! A `#define` body is not tokenised as code by this formatter (the gaps inside a directive are copied verbatim), so the
//! alignment cannot be decided while writing the tokens — but it can be decided afterwards, and that keeps the
//! renderer's invariant ("a token's text is never re-serialised") intact: only spaces before a `\` are touched.

use crate::config::Style;
use crate::options::StyleOption;

/// Align the backslashes that continue a macro body, in the way the style asks.
pub fn align_escaped_newlines(output: &str, style: &Style) -> String {
    let policy = style.text(StyleOption::AlignEscapedNewlines);
    if policy == "DontAlign" {
        return output.to_string();
    }
    let mut lines: Vec<String> = output.split_inclusive('\n').map(str::to_string).collect();
    let mut index = 0usize;
    while index < lines.len() {
        if !escapes(&lines[index]) {
            index += 1;
            continue;
        }
        // One run: every consecutive line that ends with a backslash.
        let start = index;
        let mut end = index;
        while end < lines.len() && escapes(&lines[end]) {
            end += 1;
        }
        let width = (start..end)
            .map(|line| code_width(&lines[line]))
            .max()
            .unwrap_or(0);
        for line in lines[start..end].iter_mut() {
            let target = match policy {
                "Left" => code_width(line) + 1,
                _ => width + 1,
            };
            *line = aligned(line, target);
        }
        index = end;
    }
    lines.concat()
}

/// Does this line end with a backslash (the macro continuation)?
fn escapes(line: &str) -> bool {
    line.trim_end_matches(['\n', '\r'])
        .trim_end_matches([' ', '\t'])
        .ends_with('\\')
}

/// The width of the code before the backslash, trailing spaces removed.
fn code_width(line: &str) -> usize {
    let body = line.trim_end_matches(['\n', '\r']);
    let body = body.trim_end_matches([' ', '\t']);
    let without_backslash = body.strip_suffix('\\').unwrap_or(body);
    without_backslash
        .trim_end_matches([' ', '\t'])
        .chars()
        .count()
}

/// The line with its code, spaces up to `target`, the backslash, and the line ending it had.
fn aligned(line: &str, target: usize) -> String {
    let ending = if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    let body = line.trim_end_matches(['\n', '\r']);
    let body = body.trim_end_matches([' ', '\t']);
    let code = body.strip_suffix('\\').unwrap_or(body);
    let code = code.trim_end_matches([' ', '\t']);
    let spaces = target.saturating_sub(code.chars().count());
    format!("{code}{}\\{ending}", " ".repeat(spaces))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style_with(policy: &str) -> Style {
        let mut style = Style::default_style();
        style.set_from_text("AlignEscapedNewlines", policy);
        style
    }

    #[test]
    fn a_run_of_escaped_lines_lines_their_backslashes_up() {
        let input = "#define M(a, b)                 \\\n    do {                     \\\n    } while (0)\n";
        let output = align_escaped_newlines(input, &style_with("Right"));
        let lines: Vec<&str> = output.lines().collect();
        assert!(lines[0].ends_with("\\"), "{output:?}");
        assert_eq!(
            lines[0].find('\\'),
            lines[1].find('\\'),
            "the backslashes share a column: {output:?}"
        );
    }

    #[test]
    fn dont_align_leaves_the_text_alone() {
        let input = "#define M \\\n    body\n";
        assert_eq!(
            align_escaped_newlines(input, &style_with("DontAlign")),
            input
        );
    }

    #[test]
    fn left_puts_one_space_before_each_backslash() {
        let input = "#define M        \\\n    body   \\\n    x\n";
        let output = align_escaped_newlines(input, &style_with("Left"));
        assert!(output.contains("#define M \\\n"), "{output:?}");
        assert!(output.contains("    body \\\n"), "{output:?}");
    }

    #[test]
    fn a_line_without_a_backslash_ends_the_run() {
        let input = "#define A \\\n    one\n#define B \\\n    two\n";
        let output = align_escaped_newlines(input, &style_with("Right"));
        assert_eq!(output, input);
    }
}
