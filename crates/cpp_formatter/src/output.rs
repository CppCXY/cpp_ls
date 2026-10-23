//! **`-output-replacements-xml`**: the formatter's answer as clang-format's replacement list.
//!
//! ```xml
//! <?xml version='1.0'?>
//! <replacements xml:space='preserve' incomplete_format='false'>
//! <replacement offset='3' length='2'> </replacement>
//! </replacements>
//! ```
//!
//! The format is clang-format's (measured against 22.1.3): a header, then one `<replacement>` per edit, each with a byte
//! `offset` into the input, a `length` to remove, and the text to put there as escaped content (a newline is `&#10;`).
//!
//! # One replacement, not many
//!
//! clang-format emits one replacement per whitespace run. This emits **one per file** — the changed middle between the
//! longest common prefix and suffix — because the library's job is "these bytes produce that text", and a client that
//! applies a single edit gets the same answer with less to get wrong. A caller that needs the fine-grained list can
//! diff further; `layout_audit` and `spacing_audit` already do.

/// The replacement list for `formatted` against `source`, as clang-format's XML.
pub fn replacements_xml(source: &str, formatted: &str, path: Option<&std::path::Path>) -> String {
    let mut out = String::from("<?xml version='1.0'?>\n");
    out.push_str("<replacements xml:space='preserve' incomplete_format='false'>\n");
    if let Some((offset, length, text)) = replacement(source, formatted) {
        out.push_str(&format!(
            "<replacement offset='{offset}' length='{length}'>{}</replacement>\n",
            escape(text)
        ));
    }
    out.push_str("</replacements>\n");
    let _ = path;
    out
}

/// The single edit that turns `source` into `formatted`: the middle between the common prefix and suffix, in bytes.
pub fn replacement<'a>(source: &'a str, formatted: &'a str) -> Option<(usize, usize, &'a str)> {
    if source == formatted {
        return None;
    }
    let source_bytes = source.as_bytes();
    let formatted_bytes = formatted.as_bytes();

    let mut prefix = 0usize;
    while prefix < source_bytes.len()
        && prefix < formatted_bytes.len()
        && source_bytes[prefix] == formatted_bytes[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < source_bytes.len() - prefix
        && suffix < formatted_bytes.len() - prefix
        && source_bytes[source_bytes.len() - 1 - suffix] == formatted_bytes[formatted_bytes.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let length = source_bytes.len() - prefix - suffix;
    let text = &formatted[prefix..formatted_bytes.len() - suffix];
    Some((prefix, length, text))
}

/// XML text escaping: the five characters that are not literal, plus newlines as `&#10;` (clang-format's spelling).
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' => out.push_str("&#10;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unchanged_file_has_no_replacements() {
        let xml = replacements_xml("int a;\n", "int a;\n", None);
        assert!(xml.contains("<replacements"), "{xml}");
        assert!(!xml.contains("<replacement "), "{xml}");
    }

    #[test]
    fn one_edit_covers_what_changed() {
        // `int  a` -> `int a`: the removed space is one byte at offset 4 (in `int  a`, the second space).
        let (offset, length, text) = replacement("int  a;\n", "int a;\n").expect("one edit");
        assert_eq!((offset, length, text), (4, 1, ""));
    }

    #[test]
    fn the_xml_is_clang_formats_shape() {
        let xml = replacements_xml("int  a;\n", "int a;\n", None);
        assert!(xml.starts_with("<?xml version='1.0'?>\n"), "{xml}");
        assert!(xml.contains("incomplete_format='false'"), "{xml}");
        assert!(xml.contains("<replacement offset='4' length='1'></replacement>"), "{xml}");
    }

    #[test]
    fn a_newline_is_escaped_the_way_clang_format_writes_it() {
        assert_eq!(escape("a\nb"), "a&#10;b");
        assert_eq!(escape("<&>"), "&lt;&amp;&gt;");
    }
}
