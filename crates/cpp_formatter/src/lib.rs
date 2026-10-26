//! **A C++ formatter whose configuration is clang-format's.** See `docs/cpp-format.md`.
//!
//! The crate is built in the order the design document measures, and each module says which step of that order it
//! is:
//!
//! ```text
//! config    `.clang-format` and the seven presets, as a flattened key/value style
//! tokens    the token stream with its trivia split into "gaps" (the only regions a formatter may rewrite)
//! roles     what a token *is* — template angle, declarator, block brace — read from the syntax tree
//! space     the width between two tokens that stay on one line
//! render    the source with those widths substituted, and nothing else touched
//! ```
//!
//! What is **not** here yet, and is the next step (`layout.rs`): where the line breaks go and how deep each line
//! sits. Until then [`format`] keeps the input's line structure, which makes the tool safe to run on a file and
//! lets the spacing decisions be audited against clang-format on their own (`examples/spacing_audit.rs`).

pub mod breaks;
pub mod columns;
pub mod config;
pub mod includes;
pub mod layout;
pub mod macros;
pub mod options;
pub mod output;
pub mod presets;
pub mod regions;
pub mod render;
pub mod roles;
pub mod rows;
pub mod space;
pub mod tokens;
pub mod trailing;

pub use config::{ConfigError, Style};

/// Format `source` with `style`, keeping the source's line structure.
pub fn format(source: &str, style: &Style) -> String {
    // **`DisableFormat: true` is the whole file being off**: the bytes go back out unchanged.
    if regions::is_fully_disabled(style) {
        return source.to_string();
    }
    finish(render::format(source, None, style), style)
}

/// **The passes that run over the finished text**, in order: the trailing comments, then the escaped newlines.
///
/// They live here rather than in `render` because both need the *lines* the renderer produced, not the token stream:
/// a macro body is verbatim to the renderer, and aligning a comment needs the line it sits on. `format` and
/// `format_file` both end with this call, so the two entry points cannot drift apart (they did: a bare `format` used to
/// skip both passes, which the inline tests caught).
fn finish(rendered: String, style: &Style) -> String {
    let aligned_comments = trailing::align(&rendered, style);
    macros::align_escaped_newlines(&aligned_comments, style)
}

/// Format `source` as the file at `path`, which is what lets the include sorter put the file's own header first
/// (`clang-format`'s main-header rule; see `includes.rs`).
pub fn format_file(path: &std::path::Path, source: &str, style: &Style) -> String {
    // **`DisableFormat: true` is the whole file being off**: the bytes go back out unchanged.
    if regions::is_fully_disabled(style) {
        return source.to_string();
    }
    finish(
        render::format(
            source,
            path.file_stem().and_then(|stem| stem.to_str()),
            style,
        ),
        style,
    )
}

/// **Format only the lines `first..=last`** (0-based), the library half of clang-format's `-lines`.
///
/// The whole file is formatted — a fragment cannot know its indentation — and then only the edits that fall inside the
/// requested lines are kept. That is the same choice `textDocument/rangeFormatting` makes on the server side, and it is
/// deliberate: formatting a fragment on its own would start every line at column zero.
pub fn format_range(source: &str, style: &Style, first: usize, last: usize) -> String {
    let formatted = format(source, style);
    if formatted == source {
        return formatted;
    }
    let original: Vec<&str> = source.split_inclusive('\n').collect();
    let new: Vec<&str> = formatted.split_inclusive('\n').collect();
    let mut out = String::with_capacity(source.len());
    for (index, line) in original.iter().enumerate() {
        if index >= first && index <= last {
            out.push_str(new.get(index).copied().unwrap_or(line));
        } else {
            out.push_str(line);
        }
    }
    out
}
/// Format `source` with the style that `text` (a `.clang-format`) describes.
pub fn format_with_config(source: &str, config: &str) -> Result<String, ConfigError> {
    let style = config::parse(config, None)?;
    Ok(format(source, &style))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_leaves_the_lines_outside_it_alone() {
        let source = "int  a = 1;\nint  b = 2;\n";
        let out = format_range(source, &Style::default_style(), 1, 1);
        assert_eq!(
            out.lines().nth(0),
            Some("int  a = 1;"),
            "line 0 is not in the range"
        );
        assert_eq!(out.lines().nth(1), Some("int b = 2;"), "line 1 is");
    }

    #[test]
    fn a_whole_file_range_is_the_whole_file() {
        let source = "int  a = 1;\n";
        let whole = format(source, &Style::default_style());
        assert_eq!(format_range(source, &Style::default_style(), 0, 1), whole);
    }
}
