//! # `file:` URIs ⇄ paths — one implementation for the whole crate
//!
//! The LSP speaks in URIs and every analysis API speaks in paths, so this conversion is on the path of every
//! handler. It lives here, once, for the same reason `catch_unwind` does: two copies of it is how the two halves
//! of a server come to disagree about what `%20` or `/C:/` means.
//!
//! The Lua server this skeleton came from took the same two functions from its analysis crate
//! (`emmylua_code_analysis::{uri_to_file_path, file_path_to_uri}`); ours has no business in `cpp_code_analysis`
//! (the analysis layer never sees a URI), so they are here.

use std::path::{Path, PathBuf};

use lsp_types::Uri;
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};

/// The path a `file:` URI names, or `None` for a URI this server has no business reading.
///
/// Deliberately **not** a general URI parser: a `file:` URI is the only scheme a C++ project's sources arrive
/// under, and anything else (an `untitled:` buffer, a `vscode-notebook-cell:`) has no path — answering `None`
/// there is what keeps the caller from inventing one.
pub fn uri_to_file_path(uri: &Uri) -> Option<PathBuf> {
    let text = uri.as_str();

    // `file:///C:/x/y.cpp` — the authority is empty for a local path, and what follows is percent-encoded.
    let rest = text.strip_prefix("file://")?;
    // An authority with a host (`file://server/share`) is a UNC path; keep it whole, including the leading `/`.
    let path_text = match rest.find('/') {
        Some(0) => &rest[1..],
        Some(_) => rest,
        None => return None,
    };

    let decoded = percent_decode_str(path_text).decode_utf8().ok()?;
    // `/C:/x` is how Windows paths arrive: the leading slash is the URI's, not the path's.
    let trimmed = match decoded.strip_prefix('/') {
        Some(rest) if looks_like_a_windows_drive(rest) => rest,
        _ => decoded.as_ref(),
    };

    Some(PathBuf::from(trimmed.replace('/', std::path::MAIN_SEPARATOR_STR)))
}

/// The `file:` URI for a path. The inverse of [`uri_to_file_path`] for everything this server produces: the
/// paths it answers with come from the index, which holds paths the filesystem gave it.
pub fn path_to_uri(path: &Path) -> Option<Uri> {
    let text = path.to_str()?;
    let text = text.replace(std::path::MAIN_SEPARATOR, "/");
    // A Windows path needs its `/C:/…` form, and a UNC path (`//server/share`) must *not* gain a third slash.
    let body = if text.starts_with("//") {
        text
    } else if looks_like_a_windows_drive(&text) {
        format!("/{text}")
    } else {
        text
    };

    let encoded = utf8_percent_encode(&body, NON_ALPHANUMERIC).to_string();
    // `/` and `:` are the two characters the percent-encoder must not touch: they are the URI's own syntax.
    let encoded = encoded.replace("%2F", "/").replace("%3A", ":");
    encoded.parse::<Uri>().ok()
}

/// Does this text open with a drive letter and a colon — `C:/…`?
fn looks_like_a_windows_drive(text: &str) -> bool {
    let mut characters = text.chars();
    matches!(
        (characters.next(), characters.next()),
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_windows_path_survives_the_round_trip() {
        let uri: Uri = "file:///C:/work/a%20b/x.cpp".parse().unwrap();
        let path = uri_to_file_path(&uri).unwrap();
        assert!(path.ends_with("x.cpp"));
        assert!(path.to_str().unwrap().contains("a b"));

        let back = path_to_uri(&path).unwrap();
        assert_eq!(back.as_str(), "file:///C:/work/a%20b/x.cpp");
    }

    #[test]
    fn a_uri_without_a_path_has_no_answer() {
        let uri: Uri = "untitled:Untitled-1".parse().unwrap();
        assert!(uri_to_file_path(&uri).is_none());
    }
}
