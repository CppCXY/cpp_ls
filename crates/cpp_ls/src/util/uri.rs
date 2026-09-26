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

    // `file://` and then the **authority**, which is empty for a local path and a host for a UNC one:
    //
    // ```text
    // file:///p/x.cpp        authority ""       path "/p/x.cpp"      the leading slash is the path's own root
    // file://server/share/x  authority "server" path "/share/x"      a UNC path, spelled `\\server\share\x`
    // ```
    let rest = text.strip_prefix("file://")?;
    let path_text = match rest.strip_prefix('/') {
        // An empty authority: everything after the `//` is the path, root slash included.
        Some(path) => format!("/{path}"),
        None => match rest.split_once('/') {
            Some((host, path)) => format!("//{host}/{path}"),
            None => return None,
        },
    };

    let decoded = percent_decode_str(&path_text).decode_utf8().ok()?;
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

    // Three shapes, and the authority is what separates them:
    //
    // ```text
    // /p/x.cpp          file:///p/x.cpp            a local path: the authority is empty
    // /C:/p/x.cpp       file:///C:/p/x.cpp         a Windows path, whose drive looks like a URI path segment
    // //server/share/x  file://server/share/x      a UNC path: the host IS the authority
    // ```
    let (authority, body) = match text.strip_prefix("//") {
        Some(rest) => match rest.split_once('/') {
            Some((host, path)) => (host.to_string(), format!("/{path}")),
            None => (rest.to_string(), String::new()),
        },
        None if looks_like_a_windows_drive(&text) => (String::new(), format!("/{text}")),
        None => (String::new(), text),
    };

    let encoded = utf8_percent_encode(&body, PATH_ESCAPE).to_string();
    format!("file://{authority}{encoded}").parse::<Uri>().ok()
}

/// What has to be escaped inside a `file:` URI's path.
///
/// The unreserved set and the sub-delimiters stay as they are — `.`, `-`, `_`, `~`, `+`, `,`, `@` and the rest are
/// legal in a path and a URI full of `%2E` is unreadable in a log — while a space, a `#`, a `?` and every byte of a
/// non-ASCII path **must** be escaped, because those change what the URI means. `/` and `:` are the path's own
/// syntax and are never escaped.
const PATH_ESCAPE: &percent_encoding::AsciiSet = &NON_ALPHANUMERIC
    .remove(b'/')
    .remove(b':')
    .remove(b'.')
    .remove(b'-')
    .remove(b'_')
    .remove(b'~')
    .remove(b'!')
    .remove(b'$')
    .remove(b'&')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')')
    .remove(b'*')
    .remove(b'+')
    .remove(b',')
    .remove(b';')
    .remove(b'=')
    .remove(b'@');

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

    /// A path as this platform spells it, with `/` separators — what a URI always carries, whatever the platform
    /// uses for a path.
    fn written(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    #[test]
    fn a_windows_path_survives_the_round_trip() {
        let uri: Uri = "file:///C:/work/a%20b/x.cpp".parse().unwrap();
        let path = uri_to_file_path(&uri).unwrap();
        assert!(path.ends_with("x.cpp"));
        assert!(path.to_str().unwrap().contains("a b"));

        let back = path_to_uri(&path).unwrap();
        assert_eq!(back.as_str(), "file:///C:/work/a%20b/x.cpp");
    }

    /// A path with no drive: the URI has three slashes (the third is the path's own root) and the conversion has to
    /// keep them — a `file://` form that lost one would name a *host* instead of a directory.
    #[test]
    fn a_rooted_path_keeps_its_three_slashes() {
        let uri: Uri = "file:///home/dev/main.cpp".parse().unwrap();
        let path = uri_to_file_path(&uri).unwrap();

        assert!(
            written(&path).ends_with("/home/dev/main.cpp"),
            "{}",
            written(&path)
        );
        assert_eq!(
            path_to_uri(&path).unwrap().as_str(),
            "file:///home/dev/main.cpp"
        );
    }

    /// Everything a path may hold that a URI may not: a `#` starts a fragment, a `?` a query, and a non-ASCII
    /// character is not a URI character at all. Encoding is what keeps them from changing what the URI means.
    #[test]
    fn what_a_uri_cannot_hold_is_escaped() {
        let path = Path::new("/p/a#b?c d/é.cpp");
        let uri = path_to_uri(path).expect("the path is valid UTF-8");

        assert!(uri.as_str().contains("a%23b%3Fc%20d"), "{}", uri.as_str());
        assert!(uri.as_str().contains("%C3%A9"), "{}", uri.as_str());
        assert_eq!(
            written(&uri_to_file_path(&uri).unwrap()),
            written(path),
            "and the path comes back"
        );
    }

    #[test]
    fn a_uri_without_a_path_has_no_answer() {
        let uri: Uri = "untitled:Untitled-1".parse().unwrap();
        assert!(uri_to_file_path(&uri).is_none());
    }

    /// A host in the authority is a UNC path, and it has to survive as one: `\\server\share\x.cpp` is a different
    /// file from `\server\share\x.cpp`, which is what dropping the host would produce.
    #[test]
    fn a_uri_with_a_host_is_a_unc_path() {
        let uri: Uri = "file://server/share/x.cpp".parse().unwrap();
        let path = uri_to_file_path(&uri).unwrap();

        assert_eq!(written(&path), "//server/share/x.cpp");
        assert_eq!(path_to_uri(&path).unwrap().as_str(), uri.as_str());
    }
}
