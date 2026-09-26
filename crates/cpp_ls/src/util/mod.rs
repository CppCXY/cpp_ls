//! # Small shared pieces
//!
//! Each file here is one concept used from more than one layer, which is the only reason a `util` module earns
//! its place: `catch_unwind` is the panic boundary every handler call goes through, `uri` is the one place a URI
//! becomes a path, `position` is the one place a client's line and UTF-16 column becomes a byte offset (and back),
//! and `time_cancel_token` is the timeout-plus-cancellation combinator the indexing progress uses.
//!
//! Two files the Lua skeleton had are **gone on purpose**: `desc.rs` (parsing `---@param` doc strings) and
//! `module_name_convert.rs` (`require` module paths) — both are Lua's language, not this server's. The C++
//! equivalents (doc comments, header/source pairing) get their own module when the hover and completion work
//! starts; see `docs/ls-architecture.md` §5.

mod catch_unwind;
mod position;
mod time_cancel_token;
mod uri;

pub use catch_unwind::catch_unwind;
pub use position::{offset_at_position, position_at_offset, position_in};
pub use time_cancel_token::time_cancel_token;
pub use uri::{path_to_uri, uri_to_file_path};
