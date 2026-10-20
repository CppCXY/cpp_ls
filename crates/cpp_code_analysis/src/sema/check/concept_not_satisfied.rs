//! **`concept-not-satisfied`** — an argument whose type does not satisfy the concept its parameter is constrained by.
//!
//! ```cpp
//! template <std::integral T>
//! T twice(T value);
//!
//! twice("text");      // `const char*` is not `std::integral`
//! ```
//!
//! # What this decides, and what it deliberately leaves alone
//!
//! Concept satisfaction in general is the language's most expensive question: a `requires`-expression can name any
//! expression, and deciding it means instantiating templates. What this check does is the part that is **decidable
//! from the type alone** and that a reader meets constantly — the standard library's *type* concepts, whose meaning
//! is a property of the type and nothing else:
//!
//! ```text
//!   std::integral, std::signed_integral, std::unsigned_integral    an integer builtin (not a pointer, not a class)
//!   std::floating_point                                            float, double, long double
//!   std::same_as<A, B>                                             the same type
//!   std::convertible_to<From, To>                                  convertible, through the crate's one answer
//! ```
//!
//! Everything else is **silence**: a user-written concept, a `requires` clause, a constraint this file cannot read, a
//! type the analysis cannot work out. That is not a gap to be apologised for but the rule every check here follows —
//! a finding is a claim, and a claim about a concept nobody evaluated would be a guess dressed as an error.
//!
//! # Why a finding needs the argument and the constraint to be in one file
//!
//! The constraint is read from the declaration's **`template <…>` head**, and the parameter it constrains is matched
//! by name against the parameter list's spellings. Both are read from the tree of the file the declaration is in. A
//! callee from another file is reachable — the index holds its parameter list — but not its template head, and a
//! parameter list without the head cannot say *which* parameter is constrained. So a call into a header is not
//! checked here; `param-type-check` covers the argument's type against such a declaration, and that is the question
//! this check is not.

use cpp_parser::{CppAstNode, CppCallExpr};

use super::{Check, CheckContext, DiagnosticCode};

/// **`concept-not-satisfied`** — see the module documentation.
pub struct ConceptNotSatisfied;

impl Check for ConceptNotSatisfied {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::ConceptNotSatisfied];

    fn check(context: &mut CheckContext<'_>) {
        // **This file's constrained templates**, by name: the name, and for each parameter position the concept that
        // constrains it. A function declared twice is an overload set and is skipped for the reason `param-type-check`
        // documents — the constraint of *which* overload is not a question a name can answer.
        let mut declared: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut constrained: std::collections::HashMap<String, Vec<(usize, Concept)>> =
            std::collections::HashMap::new();

        for function in context
            .own_syntax_nodes()
            .filter(|node| super::is_a_function_definition(node))
            .collect::<Vec<_>>()
        {
            let Some(name) = super::name_declared_by(&function) else {
                continue;
            };
            *declared.entry(name.clone()).or_insert(0) += 1;
            if let Some(list) = constrained_parameters(context, &function) {
                constrained.insert(name, list);
            }
        }

        for call in context.own::<CppCallExpr>().collect::<Vec<_>>() {
            let Some(callee) = call.get_callee() else {
                continue;
            };
            let Some(name) = super::spelling_of(callee.syntax()) else {
                continue;
            };
            // **One declaration, or nothing is said.** Two declarations of a name are an overload set, and which
            // overload's constraint applies is overload resolution — not a question a check answers by position.
            if declared.get(&name) != Some(&1) {
                continue;
            }
            let Some(constraints) = constrained.get(&name) else {
                continue;
            };

            for (at, argument) in call.get_args().iter().enumerate() {
                let Some((_, concept)) = constraints.iter().find(|(index, _)| *index == at) else {
                    continue;
                };
                let crate::Known::Yes((passed, _)) = context.type_of(argument.syntax()) else {
                    continue;
                };
                if concept.is_satisfied_by(&passed) != Some(false) {
                    continue;
                }

                context.add(
                    DiagnosticCode::ConceptNotSatisfied,
                    cpp_parser::source_range(argument.syntax().text_range()),
                    name.clone(),
                    t!(
                        "`%{passed}` does not satisfy `%{concept}`, which constrains the parameter this argument is \
                         passed for",
                        passed = passed.to_string(),
                        concept = concept.spelling()
                    ),
                );
            }
        }
    }
}

/// **The concept that constrains each parameter** of one function, by parameter position.
///
/// `None` for a function with no `template <…>` head, or one whose constrained parameter no parameter uses — both are
/// the ordinary case, and both mean "nothing to check" rather than "unconstrained".
fn constrained_parameters(
    context: &CheckContext<'_>,
    function: &cpp_parser::CppSyntaxNode,
) -> Option<Vec<(usize, Concept)>> {
    let head = template_head(context, function)?;
    let constrained = constrained_type_parameters(&head);
    if constrained.is_empty() {
        return None;
    }

    let parameters = super::parameter_spellings_of(function);
    let mut found = Vec::new();
    for (at, spelling) in parameters.iter().enumerate() {
        for (parameter, concept) in &constrained {
            if mentions_the_word(spelling, parameter) {
                found.push((at, concept.clone()));
            }
        }
    }

    (!found.is_empty()).then_some(found)
}

/// The text between `template` and the matching `>`, from the declaration's own text.
fn template_head(
    context: &CheckContext<'_>,
    function: &cpp_parser::CppSyntaxNode,
) -> Option<String> {
    let text = cpp_parser::source_range(function.text_range());
    let written = context.source.get(text.start_offset..text.end_offset())?;

    let start = written.find("template")?;
    let after = &written[start + "template".len()..];
    let open = after.find('<')? + 1;
    let mut depth = 1usize;
    for (at, character) in after[open..].char_indices() {
        match character {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(after[open..open + at].to_string());
                }
            }
            _ => {}
        }
    }

    None
}

/// `[(T, std::integral)]` for `template <std::integral T, class U, std::same_as<U> V>`.
///
/// A parameter list is split on the commas **outside** angle brackets, because `std::same_as<U>` holds one of its
/// own — the mistake that reads `same_as<U` and `V>` as two constraints.
fn constrained_type_parameters(head: &str) -> Vec<(String, Concept)> {
    let mut found = Vec::new();
    for parameter in split_at_top_level_commas(head) {
        let parameter = parameter.trim();
        if parameter.is_empty() {
            continue;
        }

        // The name is the last identifier; everything before it is the constraint. `class T` and `typename T` are
        // unconstrained and answer `None`.
        let Some((constraint, name)) = parameter.rsplit_once(char::is_whitespace) else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|character| character.is_alphanumeric() || character == '_') {
            continue;
        }
        let constraint = constraint.trim();
        if constraint.is_empty() || constraint == "class" || constraint == "typename" {
            continue;
        }

        if let Some(concept) = Concept::from_constraint(constraint) {
            found.push((name.to_string(), concept));
        }
    }

    found
}

fn split_at_top_level_commas(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0isize;
    let mut start = 0usize;
    for (at, character) in text.char_indices() {
        match character {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(text[start..at].to_string());
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].to_string());
    parts
}

/// Does this parameter spelling name the type parameter `T`? — as a word, so that `Total` is not `T`.
fn mentions_the_word(spelling: &str, word: &str) -> bool {
    let mut rest = spelling;
    while let Some(at) = rest.find(word) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + word.len()..].chars().next();
        let before_is_a_word = before.is_some_and(|character| {
            character.is_alphanumeric() || character == '_'
        });
        let after_is_a_word =
            after.is_some_and(|character| character.is_alphanumeric() || character == '_');
        if !before_is_a_word && !after_is_a_word {
            return true;
        }
        rest = &rest[at + word.len()..];
    }
    false
}

/// **The concepts this check can decide** — a closed set, and that is the point: a name outside it is a concept this
/// layer knows nothing about, and answering `None` is what keeps the check silent about it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Concept {
    /// `std::integral`, `std::signed_integral`, `std::unsigned_integral`.
    Integral,
    /// `std::floating_point`.
    FloatingPoint,
    /// `std::same_as<A, B>`.
    SameAs(String),
    /// `std::convertible_to<From, To>`.
    ConvertibleTo(String),
}

impl Concept {
    /// **Which concept a constraint names**, from the head's own spelling — with or without `std::`.
    fn from_constraint(constraint: &str) -> Option<Concept> {
        let unqualified = constraint.rsplit("::").next().unwrap_or(constraint);
        let (name, arguments) = match unqualified.split_once('<') {
            Some((name, rest)) => (name.trim(), Some(rest.trim_end_matches('>'))),
            None => (unqualified.trim(), None),
        };

        match name {
            "integral" | "signed_integral" | "unsigned_integral" => Some(Concept::Integral),
            "floating_point" => Some(Concept::FloatingPoint),
            "same_as" => {
                let arguments = split_at_top_level_commas(arguments?);
                Some(Concept::SameAs(arguments.first()?.trim().to_string()))
            }
            "convertible_to" => {
                let arguments = split_at_top_level_commas(arguments?);
                Some(Concept::ConvertibleTo(arguments.get(1)?.trim().to_string()))
            }
            _ => None,
        }
    }

    fn spelling(&self) -> String {
        match self {
            Concept::Integral => "std::integral".to_string(),
            Concept::FloatingPoint => "std::floating_point".to_string(),
            Concept::SameAs(other) => format!("std::same_as<{other}>"),
            Concept::ConvertibleTo(other) => format!("std::convertible_to<{other}>"),
        }
    }

    /// `Some(false)` only when the type **definitely** does not satisfy it; `Some(true)` and `None` both keep the
    /// check quiet, and `None` is what every type the analysis cannot work out answers.
    fn is_satisfied_by(&self, passed: &crate::sema::types::Type) -> Option<bool> {
        match self {
            Concept::Integral => Some(is_an_integer(passed)),
            Concept::FloatingPoint => Some(is_a_floating_point(passed)),
            Concept::SameAs(declared) => {
                let declared = crate::sema::types::parse_type_spelling(declared);
                match passed.convertible_to(&declared) {
                    crate::Known::No => Some(false),
                    crate::Known::Yes(()) => Some(passed.to_string() == declared.to_string()),
                    crate::Known::Unknown(_) => None,
                }
            }
            Concept::ConvertibleTo(target) => {
                let target = crate::sema::types::parse_type_spelling(target);
                match passed.convertible_to(&target) {
                    // The class case goes through the shared facility, exactly as `param-type-check` asks it — a
                    // class nobody has read answers `Unknown` there and stays quiet here.
                    crate::Known::No => Some(false),
                    crate::Known::Yes(()) => Some(true),
                    crate::Known::Unknown(_) => Some(match crate::sema::convert::conversion(
                        passed,
                        &target.to_string(),
                        |_| None,
                    ) {
                        crate::sema::convert::Conversion::Refused => false,
                        _ => true,
                    }),
                }
            }
        }
    }
}

/// **Is this type one of the language's integer types?**
///
/// `std::integral` is true for the builtin integer types and their standard spellings, and false for a pointer, a
/// reference, an enumeration and a class — which is what this answers, by the spelling the type has.
fn is_an_integer(passed: &crate::sema::types::Type) -> bool {
    let Some(spelling) = plain_type_spelling(passed) else {
        return false;
    };

    matches!(
        spelling.as_str(),
        "bool"
            | "char"
            | "signed char"
            | "unsigned char"
            | "wchar_t"
            | "char8_t"
            | "char16_t"
            | "char32_t"
            | "short"
            | "short int"
            | "signed short"
            | "signed short int"
            | "unsigned short"
            | "unsigned short int"
            | "int"
            | "signed"
            | "signed int"
            | "unsigned"
            | "unsigned int"
            | "long"
            | "long int"
            | "signed long"
            | "signed long int"
            | "unsigned long"
            | "unsigned long int"
            | "long long"
            | "long long int"
            | "signed long long"
            | "signed long long int"
            | "unsigned long long"
            | "unsigned long long int"
            | "size_t"
            | "ptrdiff_t"
            | "intptr_t"
            | "uintptr_t"
            | "int8_t"
            | "int16_t"
            | "int32_t"
            | "int64_t"
            | "uint8_t"
            | "uint16_t"
            | "uint32_t"
            | "uint64_t"
    )
}

fn is_a_floating_point(passed: &crate::sema::types::Type) -> bool {
    let Some(spelling) = plain_type_spelling(passed) else {
        return false;
    };
    matches!(spelling.as_str(), "float" | "double" | "long double")
}

/// **The type's spelling when it is the type itself** — `None` for a pointer, a reference or an array, which are the
/// three shapes `std::integral` is false for whatever they point at.
fn plain_type_spelling(passed: &crate::sema::types::Type) -> Option<String> {
    let spelling = passed.to_string();
    let spelling = spelling.trim();
    if spelling.contains('*')
        || spelling.contains('&')
        || spelling.contains('[')
        || spelling.starts_with("const ")
    {
        return None;
    }
    Some(spelling.to_string())
}
