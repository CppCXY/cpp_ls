//! `readability-implicit-bool-conversion`, from clang-tidy: **`if (count)` is a conversion, and it says something other than
//! what it looks like.**
//!
//! ```cpp
//! if (count)        { … }    // this check says: if (count != 0)
//! if (count == 1)   { … }    // …and here is why: the two read alike and mean different things when count is 2
//! ```
//!
//! The rule exists because an integer in a condition is the one place a C++ program silently changes type: `if (n)` asks
//! "is `n` non-zero", which is not the question `if (n == 1)` asks, and the two spellings differ by two characters. Naming
//! the comparison costs nothing at the call site and makes the question explicit.
//!
//! # Why this one needs the analysis
//!
//! Because the spelling does not say it. `if (ready)` is right when `ready` is a `bool` and wrong when it is an `int`, and
//! the two are one identifier apart — only the **type** of the expression tells them apart, which is what
//! [`CheckContext::type_of`] answers from the scope tree and the index.
//!
//! **Pointers are not reported.** `if (ptr)` is the same implicit conversion, and it is the idiom every C and C++ codebase
//! uses for a nullable pointer; a rule that flagged it would be off in every project within a day. The rule reports an
//! **arithmetic** operand that is not `bool`, which is the case where the conversion is doing real work the reader cannot
//! see.
//!
//! # What is not covered yet
//!
//! `while`, `do`/`while`, `for`'s condition, `!`, `&&`/`||` operands and the conditional operator's condition. The walk is
//! `if` and `while` for now; each of the others is the same two lines.

use cpp_parser::CppAstNode;

use super::{Check, CheckContext, DiagnosticCode};

pub struct ImplicitBoolConversion;

impl Check for ImplicitBoolConversion {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::ImplicitBoolConversion];

    fn check(context: &mut CheckContext<'_>) {
        let mut conditions: Vec<(usize, usize)> = Vec::new();
        let mut note = |condition: &cpp_parser::CppParenExpr| {
            // The expression inside the parentheses is what has a type; the parentheses are not an expression.
            let Some(inner) = condition.syntax().children().next() else {
                return;
            };
            let crate::Known::Yes((type_of, _)) = context.type_of(&inner) else {
                return;
            };
            // **Arithmetic and not `bool`.** `bool` is the type the conversion would produce, so it is not a conversion;
            // a pointer is the idiom a rule must leave alone, and a class with an `operator bool` is the C++ way to say
            // "this is testable", which is another question entirely.
            if !type_of.is_arithmetic() || type_of.to_string() == "bool" {
                return;
            }
            let range = inner.text_range();
            conditions.push((
                usize::from(range.start()),
                usize::from(range.end()) - usize::from(range.start()),
            ));
        };

        for statement in context.own::<cpp_parser::CppIfStat>().collect::<Vec<_>>() {
            if let Some(condition) = statement.get_condition() {
                note(&condition);
            }
        }
        for statement in context
            .own::<cpp_parser::CppWhileStat>()
            .collect::<Vec<_>>()
        {
            if let Some(condition) = statement.get_condition() {
                note(&condition);
            }
        }

        for (offset, length) in conditions {
            let written = context
                .source
                .get(offset..offset + length)
                .unwrap_or_default()
                .trim()
                .to_string();
            context.add(
                DiagnosticCode::ImplicitBoolConversion,
                cpp_parser::SourceRange::new(offset, length),
                written.clone(),
                format!(
                    "`{written}` is converted to `bool` here — write the comparison the reader is being asked to make: \
                     `{written} != 0`"
                ),
            );
        }
    }
}
