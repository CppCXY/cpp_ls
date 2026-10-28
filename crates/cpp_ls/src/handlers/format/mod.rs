//! **The format-string feature's one place in the server** — three faces of one question.
//!
//! ```text
//! draw.rs      the colouring: what a part of a literal is, sent as semantic tokens
//! hover.rs     the popup: what a part of a literal means
//! complete.rs  the list: what may be typed at a position inside one
//! mod.rs       which literals are format strings at all — the configuration, translated once
//! ```
//!
//! # Why this directory exists
//!
//! The analysis answers one question ([`cpp_code_analysis::format`]: which literal is a format string, and what each
//! part of it is), and the server has **three** LSP faces for it — `textDocument/semanticTokens/full`, `hover` and
//! `completion`. The handlers are organised by LSP method, which is right for a server: each method has one entry
//! point and one capability. But the *feature* would then be three files in three directories with three copies of
//! "translate `[format]` into the analysis's own configuration", and the third copy is where they disagree.
//!
//! So the translation lives here, once, and each handler asks for the face it needs. The handler modules keep their
//! one-entry-point shape and this module owns the feature.
//!
//! # What a caller has to know
//!
//! `view` must be a view of the file's **own text** ([`cpp_code_analysis::Session::view_of_the_file`]), never a
//! rendering: the answers are in the file's coordinates, and the analysis's own `format_literals` refuses a rendering
//! rather than translating.
//!
//! ```text
//! Session::format_literals(view, &config)     which literals, and the pieces of each — one token walk
//!     ├── draw::draw(builder, &literals)      push_range per piece, with the standard fallback
//!     ├── hover::format_string_hover(…)       the piece under the cursor, explained
//!     └── complete::format_string_complete(…) what may be typed where the cursor is
//! ```

use cpp_code_analysis::format::{ExtraFunction, FormatConfig, FormatKind};
use cpp_code_analysis::{DiskFiles, FormatSection, FormatSyntax, Session};

pub mod complete;
pub mod draw;
pub mod hover;

pub use draw::draw;
pub use hover::format_string_hover;

/// **`[format]`, as the analysis reads it** — the one translation, so that the three faces cannot disagree.
///
/// The defaults are decided here rather than in the section so that `.cppls.toml` stays *data* (see
/// [`cpp_code_analysis::ProjectConfig`]), and `None` therefore means "on": a project that has written no
/// configuration gets the feature, because whether a string is a format string is not a matter of taste.
///
/// A project's own function is dropped when its syntax is not one this build has — [`cpp_code_analysis::load_config`]
/// has already told the user which entry that was, and a name the analysis cannot read a grammar for is one it would
/// have to guess about.
pub fn config_from(section: &FormatSection) -> FormatConfig {
    let grammar = |syntax: FormatSyntax| match syntax {
        FormatSyntax::Std => FormatKind::Std,
        FormatSyntax::Printf => FormatKind::Printf,
        FormatSyntax::Scanf => FormatKind::Scanf,
    };
    FormatConfig {
        std_format: section.std.unwrap_or(true),
        printf: section.printf.unwrap_or(true),
        scanf: section.scanf.unwrap_or(true),
        functions: section
            .functions
            .iter()
            .filter_map(|(name, entry)| {
                Some(ExtraFunction {
                    name: name.clone(),
                    syntax: grammar(entry.syntax()?),
                    argument: entry.argument(),
                })
            })
            .collect(),
    }
}

/// **The configuration in force for this session**, or `None` when the project turned the feature off.
///
/// `None` is the cheap answer every face wants: no colouring, no popup, no list — and it is read here so that a
/// `[format] enable = false` cannot be honoured by two faces and forgotten by the third.
pub fn config_for(session: &Session<DiskFiles>) -> Option<FormatConfig> {
    let section = &session.project_config().config.format;
    if section.enable == Some(false) {
        return None;
    }
    let config = config_from(section);
    (!config.is_off()).then_some(config)
}
