//! **P3: sorting an include block.**
//!
//! clang-format sorts the `#include` lines of a block (`SortIncludes`, `IncludeBlocks: Preserve`): the lines between
//! two blank lines are one block, quoted includes come before angle ones, and each half is sorted by path with the
//! case-sensitivity `SortIncludes.IgnoreCase` asks for. That is the whole of P3 that the corpus needs — the three
//! files the audits skip as "token streams differ" differ exactly here:
//!
//! ```text
//! #include <memory>          #include <memory>
//! #include <string_view>  →  #include <optional>
//! #include <optional>        #include <string>
//! #include <string>          #include <string_view>
//! ```
//!
//! # What this does not do
//!
//! * **A main header is not promoted.** clang-format puts the include naming the file itself first; `format()` is not
//!   given a file name, and every corpus case that matters already sorts that way. When a path is threaded through the
//!   API, this is where the rule goes.
//! * **Blocks are not merged or regrouped** (`IncludeBlocks: Preserve` is what the corpus uses). A blank line ends a
//!   block, and the next one is sorted on its own.
//! * **Only whole `#include` lines are touched**; `#pragma`, `#define` and any other directive stay where they are, byte
//!   for byte.
//!
//! The pass runs on the source text before tokenizing, because sorting moves tokens: it is a source transformation, not
//! a whitespace decision, which is why it lives beside `format` rather than inside `render`.

use crate::config::Style;
use crate::options::StyleOption;

/// The source with each block of `#include` lines sorted, or the source unchanged when sorting is disabled.
pub fn sorted(source: &str, stem: Option<&str>, style: &Style) -> String {
    if !style.flag(StyleOption::SortIncludesEnabled) {
        return source.to_string();
    }
    let ignore_case = style.flag(StyleOption::SortIncludesIgnoreCase);

    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut out = String::with_capacity(source.len());
    let mut index = 0usize;
    while index < lines.len() {
        if !is_an_include(lines[index]) {
            out.push_str(lines[index]);
            index += 1;
            continue;
        }
        // One block: the run of consecutive include lines.
        let start = index;
        while index < lines.len() && is_an_include(lines[index]) {
            index += 1;
        }
        let mut block: Vec<(IncludeKey, &str)> = lines[start..index]
            .iter()
            .map(|line| (IncludeKey::of(line, stem, ignore_case), *line))
            .collect();
        block.sort_by(|left, right| left.0.cmp(&right.0));
        // **A reordered block keeps a terminator on every line**: when the file`s last line has no trailing newline
        // and sorting moves it into the middle, the lines after it would be glued to it (measured with the inline
        // tests: `#include "local.h"#include <vector>`).
        let line_ending = if source.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let count = block.len();
        for (position, (_, line)) in block.into_iter().enumerate() {
            out.push_str(line);
            if position + 1 < count && !line.ends_with(char::from(10)) {
                out.push_str(line_ending);
            }
        }
    }
    out
}

/// The sort key of one include line: its category (quoted before angle), then its path.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct IncludeKey {
    category: u8,
    path: String,
}

impl IncludeKey {
    fn of(line: &str, stem: Option<&str>, ignore_case: bool) -> IncludeKey {
        let rest = line.trim().strip_prefix("#include").unwrap_or("").trim();
        let (category, path) = if let Some(inner) = rest
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
        {
            // **The file`s own header comes first** (`clang-format` promotes it out of its category).
            (
                if is_the_main_header(inner, stem) {
                    0u8
                } else {
                    1u8
                },
                inner,
            )
        } else if let Some(inner) = rest
            .strip_prefix('<')
            .and_then(|rest| rest.strip_suffix('>'))
        {
            (2u8, inner)
        } else {
            (3u8, rest)
        };
        IncludeKey {
            category,
            path: if ignore_case {
                path.to_ascii_lowercase()
            } else {
                path.to_string()
            },
        }
    }
}

/// Is `path` the include of the file being formatted (`Mark.h` for `Mark.cpp`)?
fn is_the_main_header(path: &str, stem: Option<&str>) -> bool {
    let Some(stem) = stem else {
        return false;
    };
    let name = path.rsplit('/').next().unwrap_or(path);
    let (base, extension) = match name.rsplit_once('.') {
        Some((base, extension)) => (base, extension),
        None => (name, ""),
    };
    base == stem && matches!(extension, "h" | "hh" | "hpp" | "hxx" | "inl")
}

/// Is this line an `#include` of a quoted or angle path, and nothing else?
fn is_an_include(line: &str) -> bool {
    let trimmed = line.trim();
    let Some(rest) = trimmed.strip_prefix("#include") else {
        return false;
    };
    let rest = rest.trim();
    (rest.starts_with('"') && rest.ends_with('"') && rest.len() >= 2)
        || (rest.starts_with('<') && rest.ends_with('>') && rest.len() >= 2)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Style;

    fn sorted_with(source: &str) -> String {
        sorted(source, None, &Style::default_style())
    }

    #[test]
    fn angle_includes_are_sorted_and_quoted_ones_come_first() {
        let source =
            "#include <memory>\n#include \"b.h\"\n#include <string_view>\n#include \"a.h\"\n";
        assert_eq!(
            sorted_with(source),
            "#include \"a.h\"\n#include \"b.h\"\n#include <memory>\n#include <string_view>\n"
        );
    }

    #[test]
    fn a_blank_line_ends_a_block() {
        let source = "#include <vector>\n\n#include <array>\n";
        assert_eq!(
            sorted_with(source),
            source,
            "each block is sorted on its own"
        );
    }

    #[test]
    fn other_directives_are_left_alone() {
        let source = "#pragma once\n#include <z>\n#include <a>\n#define X 1\n";
        assert_eq!(
            sorted_with(source),
            "#pragma once\n#include <a>\n#include <z>\n#define X 1\n"
        );
    }

    #[test]
    fn the_files_own_header_comes_first() {
        // `Mark.cpp` includes `Mark.h`; clang-format promotes it above the others even when it sorts later.
        let source = "#include \"LuaParser/Parse/Mark.h\"\n#include \"LuaParser/Parse/LuaParser.h\"\n#include <cstring>\n";
        assert_eq!(
            sorted(source, Some("Mark"), &Style::default_style()),
            source
        );
    }

    #[test]
    fn without_a_stem_there_is_no_main_header() {
        let source = "#include \"b.h\"\n#include \"a.h\"\n";
        assert_eq!(
            sorted(source, None, &Style::default_style()),
            "#include \"a.h\"\n#include \"b.h\"\n"
        );
    }

    #[test]
    fn sorting_can_be_turned_off() {
        let mut style = Style::default_style();
        style.set_from_text("SortIncludes.Enabled", "false");
        assert_eq!(
            sorted("#include <z>\n#include <a>\n", None, &style),
            "#include <z>\n#include <a>\n"
        );
    }
}
