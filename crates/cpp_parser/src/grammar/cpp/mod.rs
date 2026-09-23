mod decls;
mod exprs;
mod modules;
mod stats;
mod types;

use stats::parse_stats;

use crate::{
    kind::{CppSyntaxKind, CppTokenKind},
    parser::{CppParser, MarkerEventContainer},
    parser_error::CppParseError,
};

/// Parse a whole translation unit.
///
/// This is the parser's outermost recovery loop and it must always terminate having consumed
/// every token:
///
/// 1. `parse_stats` parses as many declarations/statements as it can.
/// 2. If it consumed nothing at all, we are looking at a token no production accepts. We wrap it
///    in an `ErrorNode`, report it, and force progress by consuming it.
///
/// Step 2 is what guarantees termination and losslessness at the same time: no input can make the
/// loop spin, and no token can be dropped.
pub fn parse_cpp_unit(p: &mut CppParser) {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::TranslationUnit);

    p.init();
    while p.current_token() != CppTokenKind::Eof {
        let consume_count = p.current_token_index();
        parse_stats(p);

        if p.current_token_index() == consume_count {
            let m = p.mark(CppSyntaxKind::ErrorNode);
            p.bump();
            p.push_error(CppParseError::syntax_error_from(
                &t!("unexpected token"),
                p.current_token_range(),
            ));

            m.complete(p);
        }
    }

    // The translation unit is the one node that must never be left to the recovery machinery: if
    // an inner rule unwound past its marker, the root would be swallowed into an error node.
    debug_assert_eq!(
        p.open_marks(),
        base + 1,
        "a grammar rule unwound past the translation unit marker"
    );
    m.complete(p);
    p.close_marks_above(base);
}

/// Report a token that is expected but absent, without consuming anything.
///
/// Note the deliberate absence of a `bump()` on the failure path: the caller (or the outer
/// recovery loop) decides what to do with the offending token, and silently eating it here is how
/// a parser ends up "succeeding" on input it did not actually understand.
fn expect_token(p: &mut CppParser, token: CppTokenKind) -> Result<(), CppParseError> {
    if p.current_token() == token {
        p.bump();
        Ok(())
    } else {
        Err(CppParseError::syntax_error_from(
            &t!(
                "expected %{token}, but get %{current}",
                token = token,
                current = p.current_token()
            ),
            p.current_token_range(),
        ))
    }
}

/// Is the cursor on a **contextual keyword** — an identifier the grammar reads as a keyword in some positions
/// and as an ordinary name in others?
///
/// C++ has a family of these, and which family member a word is in is a decision the *grammar* makes rather than
/// the lexer: `module`, `import`, `final`, `override`, `requires` and `concept` are all identifiers that the
/// standard gives a meaning to in particular positions, and every one of them is a perfectly good variable name
/// everywhere else:
///
/// ```cpp
/// int module = 1;          a variable, not a module declaration
/// int requires = 1;        a variable, not a constraint
/// int concept = 2;         a variable, not a concept definition
/// ```
///
/// A word in this family must therefore be lexed as an `Identifier` and recognised by its *spelling* here. The
/// alternative — a keyword token, with the name reading refused — is what the two constraint words used to do,
/// and it made `int requires = 1;` unparseable while telling the lexer something the grammar does not believe.
///
/// The caller must also ask what *follows*: a spelling alone never settles the reading, because `requires(x);` is
/// a call and `requires (T t) { }` is a requires-expression.
pub fn is_contextual_keyword(p: &CppParser, text: &str) -> bool {
    p.current_token() == CppTokenKind::Identifier && p.current_token_text() == text
}

/// Is the cursor on `requires`, in either of its readings?
///
/// See [`is_contextual_keyword`]: this says the *word* is there, not which construct it introduces. Which one it
/// is is decided by what follows — see [`decls::starts_a_requires_clause`] and
/// [`exprs::starts_a_requires_expression`].
pub fn at_requires(p: &CppParser) -> bool {
    is_contextual_keyword(p, "requires")
}

/// Is the cursor on `concept`? See [`is_contextual_keyword`].
pub fn at_concept(p: &CppParser) -> bool {
    is_contextual_keyword(p, "concept")
}

/// Consume a contextual keyword, or report that it was expected.
///
/// The sibling of [`expect_token`] for the words that have no token kind of their own. The message names the
/// expected word rather than a token kind, which is the only thing a reader of the diagnostic can act on.
pub fn expect_contextual_keyword(p: &mut CppParser, text: &str) -> Result<(), CppParseError> {
    if is_contextual_keyword(p, text) {
        p.bump();
        Ok(())
    } else {
        Err(CppParseError::syntax_error_from(
            &t!(
                "expected %{token}, but get %{current}",
                token = text,
                current = p.current_token()
            ),
            p.current_token_range(),
        ))
    }
}
