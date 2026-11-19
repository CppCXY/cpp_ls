//! **What a rendering records for the tokens of a macro invocation** — the map a hover uses to turn a file position
//! into a rendering position.
//!
//! ```text
//! cargo test --release -p cpp_toolchain --test expansion_spans_probe -- --ignored --nocapture
//! ```
//!
//! Reported on Lua 5.5.0's `ldebug.c`: hovering `p` in `ci_func(ci)->p` inside `pcRel(ci->u.l.savedpc, ci_func(ci)->p)`
//! could not be mapped — the rendering's spans for the whole invocation carry `written: None`, so a file position
//! inside it falls back to the expansion's **start**. `pcRel` is defined in `ldebug.c` itself and `ci_func` in
//! `ldebug.h`, and the argument `ci_func(ci)->p` is written in the file — so at least the tokens the file wrote should
//! have a position here.

use cpp_toolchain::macros::MacroTable;
use cpp_toolchain::preprocess::cooked::cook_with;

const SOURCE: &str = "\
#define pcRel(pc, p)	((int)((pc) - (p)->code) - 1)
extern int f;
int g(CallInfo *ci) {
    return pcRel(ci->u.l.savedpc, ci_func(ci)->p);
}
";

#[test]
#[ignore = "a probe: prints the span table of a rendering"]
fn what_a_rendering_records_for_an_invocations_tokens() {
    use cpp_toolchain::macros::{MacroBody, MacroDef, Parameter, ParameterKind};
    use cpp_toolchain::token::Token;

    /// A definition the way the file writes it: single-line body, parameters by name, offsets from `at`.
    fn a_definition(name: &str, parameters: &[&str], body: &str, at: usize) -> MacroDef {
        let (body_tokens, _) = cpp_parser::lex(body, &cpp_parser::LexerConfig::default());
        let tokens: Vec<Token> = body_tokens
            .iter()
            .map(|data| {
                Token::new(
                    data.kind,
                    &body[data.range.start_offset..data.range.end_offset()],
                    data.range,
                )
            })
            .collect();
        MacroDef {
            name: name.into(),
            params: (!parameters.is_empty()).then(|| {
                parameters
                    .iter()
                    .map(|name| Parameter {
                        name: (*name).into(),
                        kind: ParameterKind::Ordinary,
                    })
                    .collect()
            }),
            body: MacroBody {
                tokens,
                stringize: Vec::new(),
                paste: Vec::new(),
            },
            range: cpp_parser::SourceRange::new(at, body.len()),
            name_range: cpp_parser::SourceRange::new(at, name.len()),
            written_in: None,
            value: std::sync::OnceLock::new(),
        }
    }

    let (tokens, _) = cpp_parser::lex(SOURCE, &cpp_parser::LexerConfig::default());
    let mut table = MacroTable::new();
    table.define_shared_at(
        std::sync::Arc::new(a_definition(
            "pcRel",
            &["pc", "p"],
            "((int)((pc) - (p)->code) - 1)",
            0,
        )),
        0,
    );
    table.define_shared_at(
        std::sync::Arc::new(a_definition(
            "ci_func",
            &["ci"],
            "((LClosure *)((ci)->func.p))",
            40,
        )),
        0,
    );
    let rendered = cook_with(SOURCE, &tokens, &table).render();
    println!("[text] {:?}", rendered.text);
    for span in &rendered.spans {
        if true {
            println!(
                "[span] cooked {}..{} written {:?} origin {:?} {:?}",
                span.cooked.start_offset,
                span.cooked.end_offset(),
                span.written.map(|it| (it.start_offset, it.end_offset())),
                span.origin,
                &rendered.text[span.cooked.start_offset..span.cooked.end_offset()]
            );
        }
    }
    println!("[source] {SOURCE:?}");
}
