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
pub mod presets;
pub mod render;
pub mod rows;
pub mod includes;
pub mod layout;
pub mod options;
pub mod roles;
pub mod space;
pub mod tokens;
pub mod trailing;

pub use config::{ConfigError, Style};

/// Format `source` with `style`, keeping the source's line structure.
pub fn format(source: &str, style: &Style) -> String {
    render::format(source, None, style)
}

/// Format `source` as the file at `path`, which is what lets the include sorter put the file's own header first
/// (`clang-format`'s main-header rule; see `includes.rs`).
pub fn format_file(path: &std::path::Path, source: &str, style: &Style) -> String {
    render::format(source, path.file_stem().and_then(|stem| stem.to_str()), style)
}

/// Format `source` with the style that `text` (a `.clang-format`) describes.
pub fn format_with_config(source: &str, config: &str) -> Result<String, ConfigError> {
    let style = config::parse(config, None)?;
    Ok(format(source, &style))
}