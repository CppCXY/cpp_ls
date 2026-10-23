//! **`// clang-format off` … `// clang-format on` and `DisableFormat`.**
//!
//! A file may say that a span of it is not to be reformatted. clang-format honours two forms:
//!
//! ```text
//! // clang-format off        from here …                          (P3)
//! // clang-format on         … to here
//! /* clang-format off */     the block-comment spelling is accepted too
//! ```
//!
//! and a style may switch the whole file off (`DisableFormat: true`).
//!
//! # What "off" means here
//!
//! The bytes inside the span are copied from the source, unchanged — indentation included. The formatter still *sees*
//! the tokens (a brace inside the span still opens a block, so the indentation after `on` is right), it just does not
//! rewrite their whitespace. That is the same treatment a preprocessor line gets, and for the same reason: the user
//! said this region is not the formatter's business.
//!
//! # Why the spans are byte ranges
//!
//! A line range would have to be kept in step with the formatter's own line changes. A byte range is a property of the
//! input, and the renderer already walks the input's gaps and tokens by offset, so asking "is this offset off?" is a
//! lookup rather than a mapping.

use crate::config::Style;
use crate::options::StyleOption;
use crate::tokens::Stream;

/// The byte spans of `source` that must be copied verbatim: from the line carrying an `off` marker through the line
/// carrying the matching `on` marker (or to the end of the file when no `on` follows).
pub fn disabled_spans(source: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start: Option<usize> = None;
    let mut offset = 0usize;
    for line in source.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let marker = marker_of(line);
        match marker {
            Some(Marker::Off) if start.is_none() => start = Some(line_start),
            Some(Marker::On) => {
                if let Some(begin) = start.take() {
                    spans.push((begin, offset));
                }
            }
            _ => {}
        }
    }
    // An `off` with no `on`: everything to the end.
    if let Some(begin) = start {
        spans.push((begin, source.len()));
    }
    spans
}

/// Is `offset` inside one of the spans?
pub fn is_disabled(spans: &[(usize, usize)], offset: usize) -> bool {
    spans.iter().any(|(begin, end)| offset >= *begin && offset < *end)
}

/// One flag per token and per gap: does it fall inside a disabled span?
///
/// Indexed like the stream's tokens (a gap `index` is the whitespace in front of token `index`), so the renderer can
/// ask one question per iteration.
pub fn per_token(stream: &Stream<'_>, spans: &[(usize, usize)]) -> Vec<bool> {
    let mut flags = vec![false; stream.tokens.len()];
    for index in 0..stream.tokens.len() {
        let gap = &stream.gaps[index];
        // The gap belongs to the span when its *end* is inside it (a gap that starts the span includes the newline
        // before the `off` line, and rewriting that newline would move the marker).
        flags[index] = is_disabled(spans, gap.start.max(gap.end.saturating_sub(1)))
            || is_disabled(spans, stream.tokens[index].start);
    }
    flags
}

/// `DisableFormat: true` — the whole file is off.
pub fn is_fully_disabled(style: &Style) -> bool {
    style.flag(StyleOption::DisableFormat)
}

enum Marker {
    Off,
    On,
}

/// The marker a line carries, if any. The spellings are clang-format's: the words after `clang-format` may be in any
/// order (`// clang-format off` and `// clang-format: off` are both accepted there), and a block comment counts too.
fn marker_of(line: &str) -> Option<Marker> {
    let text = line.trim();
    let body = if let Some(rest) = text.strip_prefix("//") {
        rest
    } else if let Some(rest) = text.strip_prefix("/*") {
        rest.trim_end_matches("*/")
    } else {
        return None;
    };
    let body = body.trim().trim_start_matches(':').trim();
    let mut words = body.split_whitespace();
    if words.next()? != "clang-format" {
        return None;
    }
    let rest: Vec<&str> = words.collect();
    let off = rest.iter().any(|word| *word == "off");
    let on = rest.iter().any(|word| *word == "on");
    match (off, on) {
        (true, _) => Some(Marker::Off),
        (_, true) => Some(Marker::On),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_runs_from_off_to_on_inclusive() {
        let source = "int a;\n// clang-format off\nint   b   = 1;\n// clang-format on\nint c;\n";
        let spans = disabled_spans(source);
        assert_eq!(spans.len(), 1);
        let (begin, end) = spans[0];
        assert!(source[begin..end].starts_with("// clang-format off"));
        assert!(source[begin..end].trim_end().ends_with("// clang-format on"));
        assert!(!is_disabled(&spans, 0), "the first line is still ours");
        assert!(is_disabled(&spans, begin + 1));
    }

    #[test]
    fn an_unterminated_span_runs_to_the_end() {
        let source = "int a;\n// clang-format off\nint   b   = 1;\n";
        let spans = disabled_spans(source);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].1, source.len());
    }

    #[test]
    fn the_block_comment_spelling_counts() {
        let source = "/* clang-format off */\nint   b;\n/* clang-format on */\n";
        assert_eq!(disabled_spans(source).len(), 1);
    }

    #[test]
    fn an_ordinary_comment_is_not_a_marker() {
        let source = "// clang-format is a tool\nint a;\n// off we go\n";
        assert!(disabled_spans(source).is_empty());
    }
}
