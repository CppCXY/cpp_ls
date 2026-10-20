//! # Small shared pieces
//!
//! Each file here is one concept used from more than one layer, which is the only reason a `util` module earns
//! its place: `catch_unwind` is the panic boundary every handler call goes through, `uri` is the one place a URI
//! becomes a path, `position` is the one place a client's line and UTF-16 column becomes a byte offset (and back),
//! `kind` is the one place a declaration's kind becomes the protocol's icon, and `time_cancel_token` is the
//! timeout-plus-cancellation combinator the indexing progress uses.
//!
//! Two files the Lua skeleton had are **gone on purpose**: `desc.rs` (parsing `---@param` doc strings) and
//! `module_name_convert.rs` (`require` module paths) — both are Lua's language, not this server's. The C++
//! equivalents (doc comments, header/source pairing) get their own module when the hover and completion work
//! starts;

mod catch_unwind;
mod kind;
mod position;
mod time_cancel_token;
mod uri;

mod messages;

pub use catch_unwind::catch_unwind;
pub use messages::set_locale;
pub use kind::symbol_kind;
pub use position::{
    file_offset_at_position, line_body, offset_at_position, position_at_offset, position_in_file,
    utf16_column,
};
// `position_in_file` is the one a handler wants (a file the VFS is holding); `position_in` is the piece it is built
// from, and what a handler with a view's own text and line index uses.
pub use time_cancel_token::time_cancel_token;
pub use uri::{path_to_uri, uri_to_file_path};

/// Is `path` inside `root`, compared the way this server compares paths elsewhere?
///
/// Byte-for-byte only after normalising: Windows compares case-insensitively and spells separators both ways, and the
/// two sides of a comparison like this come from different places — an index's file list and a configuration's root — so
/// a raw `starts_with` answers "outside" for a file the user just wrote. That is the same rule the index's own path
/// keys follow, stated once here so a write boundary and a workspace boundary cannot disagree.
pub fn is_inside(path: &std::path::Path, root: &std::path::Path) -> bool {
    let flatten = |path: &std::path::Path| {
        let text = path.to_string_lossy().replace('\\', "/");
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    };
    let path = flatten(path);
    let root = flatten(root);
    path == root || path.starts_with(&format!("{root}/"))
}
