//! **The expansion budget counts program tokens, not the spaces between them.**
//!
//! The budget exists to stop a macro chain whose expansion *grows* (`#define A B B` / `#define B A A` and longer
//! relatives). Whitespace is not part of that question, and counting it made the budget a limit on the file's
//! *layout*: every macro body carries the spaces of the line it was written on.
//!
//! Measured on `C:\Users\zc\Desktop\lua-5.5.0`: `src/lvm.c` is 17 758 tokens and expands to 2.6× — but with the
//! bodies' trivia the 100 000-token budget ran out after **46 717** significant tokens, the rendering was a **prefix**
//! ending mid-`s2v(...)`, and the seven parse errors published for the file were all about the cut. The fixture below
//! is that shape without the file: 60 000 program tokens whose separation is 60 000 more trivia tokens, so the old
//! accounting spent the budget at 100 000 pushes and stopped inside the list.

use cpp_toolchain::macros::MacroTable;
use cpp_toolchain::preprocess::cooked::cook_with;

#[test]
fn a_files_layout_does_not_spend_the_expansion_budget() {
    const USES: usize = 60_000;
    let mut source = String::from("#define A 1\n");
    for _ in 0..USES {
        source.push_str("A ");
    }

    let (tokens, _) = cpp_parser::lex(&source, &cpp_parser::LexerConfig::default());
    let cooked = cook_with(&source, &tokens, &MacroTable::new());

    assert!(
        !cooked.exhausted,
        "{USES} program tokens are well inside the 100 000-token budget; the {USES} spaces between them are layout"
    );
    assert_eq!(
        cooked.tokens.len(),
        USES,
        "and every one of them is in the stream"
    );
}

/// **A macro argument that contains a macro is expanded once, not twice, against the budget.**
///
/// The expander expands an argument that mentions a macro **speculatively**, to hand the call a plain token run —
/// and then the body's rescan expands those tokens again. Charging the budget for both passes made it a limit on the
/// expander's *work* rather than on the stream, and a real file paid it: `lvm.c` pushed 100 000 tokens for 47 497 of
/// output, the rendering was cut mid-expansion, and the parse errors published for the file were the cut's. The
/// fixture here is that shape in one call: 60 000 program tokens whose argument mentions `A`, so the old accounting
/// spent 120 000 pushes and stopped.
#[test]
fn a_macro_in_an_argument_is_charged_once() {
    const USES: usize = 60_000;
    let mut source = String::from("#define A 1\n#define ID(x) x\nID(");
    for _ in 0..USES {
        source.push_str("A ");
    }
    source.push_str(")\n");

    let (tokens, _) = cpp_parser::lex(&source, &cpp_parser::LexerConfig::default());
    let cooked = cook_with(&source, &tokens, &MacroTable::new());

    assert!(
        !cooked.exhausted,
        "{USES} program tokens of output are inside the budget; the speculative pass is not output"
    );
    assert_eq!(cooked.tokens.len(), USES);
}
