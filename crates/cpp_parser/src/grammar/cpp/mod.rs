mod decls;
mod exprs;
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
