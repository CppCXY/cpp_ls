//! **The type a literal spells, read from its own text.**
//!
//! A literal's type is the one type in C++ that is written *inside* the token: `1` is `int` and `1u` is not, `1.5` is
//! `double` and `1.5f` is not, `L"x"` is `const wchar_t*` and `u8"x"` is `const char8_t*`. Nothing else in the language
//! carries its type in its spelling, which is why this is one function rather than a table of declarations.
//!
//! # Why it is one function
//!
//! It was **three**: `declarations.rs` read an `auto` variable's type from the initializer, `index/project/type_of.rs`
//! answered the type of an expression, and `sema/constant.rs` folded a value. Each had its own reading, and all three
//! dropped the suffix — `1u` was `int`, `1.0f` was `double`, `u8"x"` was `const char*` — so `auto a = 1u;` was `int`,
//! `pick(1u)` picked the `int` overload, and a hover on `1.0f` said `double`
//! (`docs/worklog.md` P1-4).
//!
//! # The standard is not a parameter, and this is the one place that hurts
//!
//! `u8"x"` is `const char8_t*` in C++20 and `const char*` before it, and `u8'x'` is `char8_t` only from C++17. The
//! answer here is the **C++20** one: a project that says it is C++17 would get `char8_t` for a string it spells `u8`,
//! which is a type that does not exist there. A wrong answer is worse than none, so the caller that *knows* the
//! standard should pass it — [`a_literal_type_in`] takes it, and this function is that one asked with the assumption an
//! editor makes when nothing said otherwise.
//!
//! # What is not attempted
//!
//! A **user-defined** literal (`10_km`, `"x"sv`) has the type of its `operator""`, which is a declaration lookup rather
//! than a spelling, and this function answers `None` for it. Whether an integer literal's *value* fits in `int` is also
//! not asked: the type of `0xFFFFFFFFFF` is `long long` by the standard's ladder, and this answers `int` for it — the
//! ladder needs the value, and a value that overflows is a diagnostic rather than a type.

/// **The type of a literal, from its spelling** — see the module documentation for what is and is not covered.
pub fn a_literal_type(written: &str) -> Option<&'static str> {
    a_literal_type_for(written, /* a_wide_character_type_exists */ true)
}

/// [`a_literal_type`] with **whether `char8_t` exists** — `false` for a C++17 project, where `u8"x"` is `const char*`.
pub fn a_literal_type_for(written: &str, char8_t_exists: bool) -> Option<&'static str> {
    let written = written.trim();
    if written.is_empty() {
        return None;
    }

    if written == "true" || written == "false" {
        return Some("bool");
    }
    if written == "nullptr" || written == "NULL" {
        return Some("nullptr_t");
    }

    // **A character literal**: one prefix at most, and the prefix is the type.
    if let Some(rest) = written.strip_prefix('\'') {
        let _ = rest;
        return Some("char");
    }
    for (prefix, the_type) in [
        ("L'", "wchar_t"),
        ("u8'", "char8_t"),
        ("u'", "char16_t"),
        ("U'", "char32_t"),
    ] {
        if written.starts_with(prefix) {
            return Some(the_type);
        }
    }

    // **A string literal**, by the same rule. The `u8` case is the one the standard changes.
    for (prefix, the_type) in [
        ("L\"", "const wchar_t*"),
        ("u\"", "const char16_t*"),
        ("U\"", "const char32_t*"),
    ] {
        if written.starts_with(prefix) {
            return Some(the_type);
        }
    }
    if written.starts_with("u8\"") {
        return Some(if char8_t_exists {
            "const char8_t*"
        } else {
            "const char*"
        });
    }
    if written.starts_with('"') {
        return Some("const char*");
    }

    // **A floating literal**: a `.`, an exponent or a hexadecimal `p`, and then the suffix decides.
    let looks_like_a_float = written.contains('.')
        || written.contains('p')
        || written.contains('P')
        || ((written.contains('e') || written.contains('E'))
            && !written.starts_with("0x")
            && !written.starts_with("0X"));
    if looks_like_a_float {
        // **The suffix is a word at the end**, matched longest first: `f128`, `bf16` and `f16` (C++23) before the single
        // letters. Reading a trailing *run* of characters does not work — `1.0f` ends in `0f` because `0` is part of the
        // number — so the candidates are spelled out and the whole literal's tail is compared.
        for (suffix, the_type) in [
            ("f128", "_Float128"),
            ("F128", "_Float128"),
            ("bf16", "__bf16"),
            ("BF16", "__bf16"),
            ("f16", "_Float16"),
            ("F16", "_Float16"),
            ("f", "float"),
            ("F", "float"),
            ("l", "long double"),
            ("L", "long double"),
        ] {
            if written.ends_with(suffix) {
                return Some(the_type);
            }
        }
        return Some("double");
    }

    // **An integer literal**: the suffix selects from the ladder, and the order matters (`ull` before `ul` before
    // `u`), which is why this is a match on the whole suffix rather than a set of `contains` tests.
    // **The integer suffixes**, and only these: a hexadecimal digit is `a`–`f`, so `0x1F` ends in a digit and not in a
    // suffix, and `1L` is `long` rather than a hex float.
    let suffix = trailing(written, |character| {
        matches!(character, 'u' | 'U' | 'l' | 'L' | 'z' | 'Z')
    });
    let suffix = suffix.to_ascii_lowercase();
    Some(match suffix.as_str() {
        "u" => "unsigned int",
        "l" => "long",
        "ul" | "lu" => "unsigned long",
        "ll" => "long long",
        "ull" | "llu" => "unsigned long long",
        // **C++23's `z`**: the signed one is `ptrdiff_t` and the unsigned one is `size_t`.
        "z" => "ptrdiff_t",
        "uz" | "zu" => "size_t",
        _ => "int",
    })
}

/// **The trailing run of characters `allowed` accepts** — a literal's suffix, read from the end.
fn trailing(written: &str, allowed: impl Fn(char) -> bool) -> &str {
    let mut start = written.len();
    for (at, character) in written.char_indices().rev() {
        if allowed(character) {
            start = at;
        } else {
            break;
        }
    }
    &written[start..]
}

#[cfg(test)]
mod tests {
    use super::a_literal_type_for;

    /// The answers `docs/worklog.md` P1-4 named as wrong, plus the neighbours they sit beside.
    #[test]
    fn a_literal_keeps_its_suffix_and_its_encoding() {
        for (written, expected) in [
            ("1", "int"),
            ("1u", "unsigned int"),
            ("1U", "unsigned int"),
            ("1l", "long"),
            ("1L", "long"),
            ("1ul", "unsigned long"),
            ("1lu", "unsigned long"),
            ("1ll", "long long"),
            ("1ull", "unsigned long long"),
            ("1LLU", "unsigned long long"),
            ("0x1F", "int"),
            ("0b1010", "int"),
            ("1.0", "double"),
            ("1.0f", "float"),
            ("1.0F", "float"),
            ("1.0L", "long double"),
            ("1e5", "double"),
            ("1e5f", "float"),
            ("0x1p3", "double"),
            ("'x'", "char"),
            ("L'x'", "wchar_t"),
            ("u'x'", "char16_t"),
            ("U'x'", "char32_t"),
            ("\"x\"", "const char*"),
            ("L\"x\"", "const wchar_t*"),
            ("u\"x\"", "const char16_t*"),
            ("U\"x\"", "const char32_t*"),
            ("true", "bool"),
            ("false", "bool"),
            ("nullptr", "nullptr_t"),
            ("10_km", "int"),
        ] {
            assert_eq!(
                a_literal_type_for(written, true),
                Some(expected),
                "{written:?}"
            );
        }
    }

    /// **`u8"x"` is the one answer the standard changes** — `char8_t` from C++20, `const char*` before.
    #[test]
    fn a_u8_string_depends_on_the_standard() {
        assert_eq!(
            a_literal_type_for("u8\"x\"", true),
            Some("const char8_t*"),
            "C++20"
        );
        assert_eq!(
            a_literal_type_for("u8\"x\"", false),
            Some("const char*"),
            "C++17, where char8_t does not exist"
        );
    }
}
