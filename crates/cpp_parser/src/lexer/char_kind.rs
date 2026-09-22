//! C++ character classification.
//!
//! C++23 adopted the Unicode `XID_Start` / `XID_Continue` properties for identifiers, with the
//! C++20 wording allowing a subset. `unicode-ident` implements exactly those properties — the same
//! crate `proc-macro2` and `syn` use — so the lexer and Rust agree on what an identifier is.
//!
//! On top of the standard properties the lexer accepts two widely implemented extensions:
//!
//! * `$` in identifiers (GCC, Clang, MSVC all accept it), gated by
//!   [`LexerConfig::dollar_in_identifier`](crate::lexer::LexerConfig::dollar_in_identifier).
//! * Universal character names (`\uXXXX`, `\UXXXXXXXX`) inside identifiers, which is standard but
//!   almost always forgotten. These need the raw source, so they are handled in the lexer rather
//!   than here.

/// Can `ch` start an identifier? (`XID_Start`, plus `_`)
pub fn is_name_start(ch: char) -> bool {
    ch == '_' || unicode_ident::is_xid_start(ch)
}

/// Can `ch` continue an identifier? (`XID_Continue`, plus `_`)
pub fn is_name_continue(ch: char) -> bool {
    ch == '_' || unicode_ident::is_xid_continue(ch)
}

/// Same as [`is_name_start`], but for the `$` extension.
pub fn is_name_start_with_dollar(ch: char, dollar_allowed: bool) -> bool {
    is_name_start(ch) || (dollar_allowed && ch == '$')
}

/// Same as [`is_name_continue`], but for the `$` extension.
pub fn is_name_continue_with_dollar(ch: char, dollar_allowed: bool) -> bool {
    is_name_continue(ch) || (dollar_allowed && ch == '$')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_identifiers_are_identifiers() {
        for ch in ['a', 'z', 'A', 'Z', '_', '0', '9'] {
            assert!(
                is_name_continue(ch),
                "{ch:?} must be able to continue an identifier"
            );
        }
        for ch in ['0', '9'] {
            assert!(
                !is_name_start(ch),
                "{ch:?} must not be able to start an identifier"
            );
        }

        // `$` is an extension, not part of the standard properties, so the plain predicates must
        // reject it and only the gated variants may accept it.
        assert!(!is_name_start('$') && !is_name_continue('$'));
    }

    #[test]
    fn unicode_identifiers_follow_xid() {
        // C++23 allows these; C++20 technically did not, but every real compiler does.
        for ch in ['é', '中', 'λ', 'Ж'] {
            assert!(
                is_name_start(ch),
                "{ch:?} is XID_Start and must be accepted"
            );
            assert!(is_name_continue(ch));
        }

        // Emoji and punctuation are neither XID_Start nor XID_Continue.
        for ch in ['😀', '@', '#', '-', '.', '+'] {
            assert!(!is_name_start(ch), "{ch:?} must not start an identifier");
            assert!(!is_name_continue(ch), "{ch:?} must not continue one");
        }
    }

    #[test]
    fn dollar_is_gated() {
        assert!(is_name_start_with_dollar('$', true));
        assert!(!is_name_start_with_dollar('$', false));
    }
}
