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
//! # Where the constraint is read from
//!
//! From the declaration's **`template <…>` head**, in one of two places: this file's tree when the declaration is
//! here, and the **summary** when it is not — `DeclFact::constraints` is the head's other half, stored beside the
//! parameter names for exactly this question. The constrained index is then found by matching the type parameter's
//! name against the parameter list's spellings, so `T value` and `const T& value` are both "the parameter `T`
//! constrains".
//!
//! Two declarations of a name are an **overload set** and are skipped: which overload's constraint applies is
//! overload resolution, and a check that guessed would be reporting on a declaration the call may not use.

use cpp_parser::{CppAstNode, CppCallExpr};

use super::{Check, CheckContext, DiagnosticCode};

/// **`concept-not-satisfied`** — see the module documentation.
pub struct ConceptNotSatisfied;

impl Check for ConceptNotSatisfied {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::ConceptNotSatisfied];

    fn check(context: &mut CheckContext<'_>) {
        // **The declarations, as facts.** This file's are in its summary, and everything else is in the index — one
        // question, one shape, and no walk of two trees that could disagree.
        //
        // `DeclFact::constraints` is aligned with the *parameters*, so the answer is `constraints[at]` and nothing
        // has to be re-derived here: the pairing of a template parameter with the function parameter it constrains
        // was decided where the tree was (see `declared_constraints_with`).
        //
        // One declaration per name: a name declared twice is an overload set, and which overload's constraint
        // applies is overload resolution — not a question a check answers by position.
        for call in context.own::<CppCallExpr>().collect::<Vec<_>>() {
            let Some(callee) = call.get_callee() else {
                continue;
            };
            let Some(name) = super::spelling_of(callee.syntax()) else {
                continue;
            };
            // Cloned because the finding below needs `&mut context`: the list is one short string per
            // parameter, and only a constrained declaration ever has a non-empty one.
            let Some(constraints) =
                constraints_of_a_declaration(context, &name).map(<[String]>::to_vec)
            else {
                continue;
            };

            for (at, argument) in call.get_args().iter().enumerate() {
                let Some(concept) = constraints
                    .get(at)
                    .filter(|written| !written.is_empty())
                    .and_then(|written| Concept::from_constraint(written))
                else {
                    continue;
                };
                let crate::Known::Yes((passed, _)) = context.type_of(argument.syntax()) else {
                    continue;
                };
                if concept.is_satisfied_by(&passed, context) != Some(false) {
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

/// **The constraints of the one declaration of `name`**, from this file's summary or from the index.
///
/// `None` for a name this file declares twice, for one the index holds twice, and for one with no constrained
/// parameter — the three cases where the honest answer is that no constraint applies to a call.
fn constraints_of_a_declaration<'a>(
    context: &'a CheckContext<'_>,
    name: &str,
) -> Option<&'a [String]> {
    let mut own: Option<&[String]> = None;
    let mut own_declarations = 0;
    for fact in &context.summary.declarations {
        if fact.name != name || fact.parameter_list.is_none() {
            continue;
        }
        own_declarations += 1;
        own = Some(&fact.constraints);
    }
    match own_declarations {
        0 => {}
        1 => return own,
        _ => return None,
    }

    let index = context.model.index();
    let declared = index.files_declaring(name, context.path);
    let mut elsewhere: Option<&[String]> = None;
    let mut elsewhere_declarations = 0;
    for found in &declared {
        if found.fact.parameter_list.is_none() {
            continue;
        }
        elsewhere_declarations += 1;
        elsewhere = Some(&found.fact.constraints);
    }
    match elsewhere_declarations {
        1 => elsewhere,
        _ => None,
    }
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
    /// `std::derived_from<B>` — decided through the index, which is where a class's bases are.
    DerivedFrom(String),
    /// **The object family**: `std::default_initializable`, `move_constructible`, `copy_constructible`,
    /// `movable`, `copyable`, `semiregular`, `regular`.
    ///
    /// A class's answer needs its constructors and its assignment operators, which this layer does not read — so the
    /// only type it can answer *definitely* about is a **reference**, which satisfies none of them (`copyable<int&>`
    /// is false for the same reason `std::copyable` asks for an object type). A value type answers `None`.
    NeedsAnObjectType(&'static str),
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
                Some(Concept::ConvertibleTo(
                    arguments.first()?.trim().to_string(),
                ))
            }
            "derived_from" => {
                let arguments = split_at_top_level_commas(arguments?);
                Some(Concept::DerivedFrom(arguments.first()?.trim().to_string()))
            }
            "default_initializable"
            | "move_constructible"
            | "copy_constructible"
            | "movable"
            | "copyable"
            | "semiregular"
            | "regular" => Some(Concept::NeedsAnObjectType(match name {
                "default_initializable" => "std::default_initializable",
                "move_constructible" => "std::move_constructible",
                "copy_constructible" => "std::copy_constructible",
                "movable" => "std::movable",
                "copyable" => "std::copyable",
                "semiregular" => "std::semiregular",
                _ => "std::regular",
            })),
            _ => None,
        }
    }

    fn spelling(&self) -> String {
        match self {
            Concept::Integral => "std::integral".to_string(),
            Concept::FloatingPoint => "std::floating_point".to_string(),
            Concept::SameAs(other) => format!("std::same_as<{other}>"),
            Concept::ConvertibleTo(other) => format!("std::convertible_to<{other}>"),
            Concept::DerivedFrom(other) => format!("std::derived_from<{other}>"),
            Concept::NeedsAnObjectType(spelling) => spelling.to_string(),
        }
    }

    /// `Some(false)` only when the type **definitely** does not satisfy it; `Some(true)` and `None` both keep the
    /// check quiet, and `None` is what every type the analysis cannot work out answers.
    fn is_satisfied_by(
        &self,
        passed: &crate::sema::types::Type,
        context: &CheckContext<'_>,
    ) -> Option<bool> {
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
            Concept::DerivedFrom(base) => derives_from(context, passed, base),
            Concept::NeedsAnObjectType(_) => {
                // **A reference satisfies none of them**, and that is the whole of what this answers: a value type
                // needs its constructors, which this layer does not read.
                Some(!passed.to_string().contains('&'))
            }
            Concept::ConvertibleTo(target) => {
                let target = crate::sema::types::parse_type_spelling(target);
                match passed.convertible_to(&target) {
                    // The class case goes through the shared facility, exactly as `param-type-check` asks it — a
                    // class nobody has read answers `Unknown` there and stays quiet here.
                    crate::Known::No => Some(false),
                    crate::Known::Yes(()) => Some(true),
                    crate::Known::Unknown(_) => Some(!matches!(
                        crate::sema::convert::conversion(passed, &target.to_string(), |_| None),
                        crate::sema::convert::Conversion::Refused
                    )),
                }
            }
        }
    }
}

/// **The spelling of a type that is a plain name** — `Some("int")` for `Type::Builtin`, `Some("size_t")` for a
/// `Type::Named` with no arguments; `None` for a pointer, a reference, an array, a function or a pack.
///
/// The **tree's own reading**, not a scan for `*` and `&`: `Type` has a variant for each of those, and asking the
/// string whether it contains an asterisk is a second reader of the same fact — one that disagrees with the first
/// the day a type is spelled `decltype(p)` or `int (*)(int)`.
fn plain_type_spelling(passed: &crate::sema::types::Type) -> Option<String> {
    match passed {
        crate::sema::types::Type::Builtin { spelling } => Some(spelling.clone()),
        crate::sema::types::Type::Named { name, arguments } if arguments.is_empty() => {
            Some(name.clone())
        }
        _ => None,
    }
}

/// **Is this type one of the language's integer types?**
///
/// `std::integral` is true for the builtin integer types and their standard spellings, and false for a pointer, a
/// reference, an enumeration and a class — which is what this answers, from the type's own shape and then from the
/// names the standard gives the same types (`size_t`, `int64_t`).
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
/// Splits `a, b` into its parts, ignoring the commas inside `<>`, `()` and `[]`.
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

/// **Is `derived` a class derived from `base`?** — through the index, which is where a class's bases are recorded.
///
/// `None` when the walk cannot answer: a class nobody has described, a chain deeper than the bound. `Some(false)`
/// only when every class on the way was read and none of them is the base — which is what makes a builtin or a
/// pointer a definite `false` without asking anything.
fn derives_from(
    context: &CheckContext<'_>,
    derived: &crate::sema::types::Type,
    base: &str,
) -> Option<bool> {
    use crate::sema::types::Type;

    let Type::Named { name, arguments } = derived else {
        // A builtin, a pointer, a reference, an array: none of them is a class, and `derived_from` needs one.
        return Some(false);
    };
    if !arguments.is_empty() {
        // `derived_from<B> vector<int>` asks whether the *instantiation* derives from `B`; the index is keyed by the
        // template's own name, and guessing here would be a claim about a type nobody instantiated.
        return None;
    }

    let index = context.model.index();
    let base = base.trim();
    let base = base.rsplit("::").next().unwrap_or(base);

    let mut waiting = vec![name.clone()];
    let mut seen = std::collections::HashSet::new();
    let mut answered = true;

    while let Some(class) = waiting.pop() {
        if !seen.insert(class.clone()) {
            continue;
        }
        if class.rsplit("::").next().unwrap_or(&class) == base {
            return Some(true);
        }

        let declared = index.files_declaring(&class, context.path);
        let mut found = None;
        let mut declarations = 0;
        for visible in &declared {
            if visible.fact.kind != crate::DeclKind::Type {
                continue;
            }
            declarations += 1;
            found = Some(&visible.fact);
        }
        let (true, 1) = (answered, declarations) else {
            answered = false;
            continue;
        };
        let Some(fact) = found else {
            answered = false;
            continue;
        };

        for written in &fact.bases {
            // `public: std::basic_ios` and `private Widget` are both written with an access keyword; none of it
            // changes which class the name is.
            let written = written.trim();
            let name = written.split_whitespace().next_back().unwrap_or(written);
            waiting.push(name.trim_end_matches('<').to_string());
        }
    }

    answered.then_some(false)
}
