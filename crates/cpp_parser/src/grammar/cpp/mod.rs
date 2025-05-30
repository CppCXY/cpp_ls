mod exprs;
mod stats;
mod test;

use stats::{parse_stat, parse_stats};

use crate::{
    kind::{CppSyntaxKind, CppTokenKind},
    parser::{CppParser, MarkerEventContainer},
    parser_error::CppParseError,
};

use super::ParseResult;

#[allow(unused)]
pub fn parse_cpp_unit(p: &mut CppParser) {
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

    m.complete(p);
}

fn parse_compound_stat(p: &mut CppParser) -> ParseResult {
    let m = p.mark(CppSyntaxKind::CompoundStat);

    let left_brace_founded = if_token_bump(p, CppTokenKind::LeftBrace);

    if left_brace_founded {
        parse_stats(p);
        expect_token(p, CppTokenKind::RightBrace)?;
    } else {
        parse_stat(p)?;
    }

    Ok(m.complete(p))
}

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

fn if_token_bump(p: &mut CppParser, token: CppTokenKind) -> bool {
    if p.current_token() == token {
        p.bump();
        true
    } else {
        false
    }
}
