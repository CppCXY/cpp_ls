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
        // **This file's constrained templates**, by name: the name, and for each parameter position the concept that
        // constrains it. A function declared twice is an overload set and is skipped for the reason `param-type-check`
        // documents — the constraint of *which* overload is not a question a name can answer.
        let mut declared: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut constrained: std::collections::HashMap<String, Vec<(usize, Concept)>> =
            std::collections::HashMap::new();

        // **A declaration as well as a definition.** A constrained template is *declared* in a header and
        // defined somewhere else, so requiring a body would leave the ordinary shape unchecked — the first test of
        // this check used a definition and the second a declaration, and only the second failed.
        for function in context
            .own_syntax_nodes()
            .filter(|node| {
                super::name_declared_by(node).is_some()
                    && !super::parameter_spellings_of(node).is_empty()
            })
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
            //
            // The constraint is read from this file's tree when the declaration is here, and from the **summary**
            // when it is not: `DeclFact::parameters` and `DeclFact::constraints` are the template head, split into the
            // two things a call site needs, and `parameter_list` is the declaration's own spelling — which is where a
            // header's `template <std::integral T> T twice(T)` is reachable from a source file that only includes it.
            let same_file = (declared.get(&name) == Some(&1))
                .then(|| constrained.get(&name))
                .flatten()
                .cloned();
            let Some(constraints) = same_file.or_else(|| from_the_index(context, &name)) else {
                continue;
            };

            for (at, argument) in call.get_args().iter().enumerate() {
                let Some((_, concept)) = constraints.iter().find(|(index, _)| *index == at) else {
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
                Some(Concept::ConvertibleTo(arguments.first()?.trim().to_string()))
            }
            "derived_from" => {
                let arguments = split_at_top_level_commas(arguments?);
                Some(Concept::DerivedFrom(arguments.first()?.trim().to_string()))
            }
            "default_initializable" | "move_constructible" | "copy_constructible" | "movable" | "copyable"
            | "semiregular" | "regular" => Some(Concept::NeedsAnObjectType(match name {
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
            Concept::DerivedFrom(base) => {
                // `derived_from<D, B>` also requires `is_base_of_v<B, D>`, and a pointer or a builtin is not a
                // class at all — the two answers before the index is asked.
                if passed.to_string().contains('*') || plain_type_spelling(passed).is_none() {
                    return Some(false);
                }
                derives_from(context, &passed.to_string(), base)
            }
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

/// **Is `derived` a class derived from `base`?** — through the index, which is where a class's bases are recorded.
///
/// `None` when the walk cannot answer: a class nobody has described, a base that is itself a template parameter, a
/// chain deeper than the bound. `Some(false)` only when every class on the way was read and none of them is the base.
fn derives_from(context: &CheckContext<'_>, derived: &str, base: &str) -> Option<bool> {
    // **A builtin is not a class**, and saying so is what turns `handle(1)` into a finding rather than into a
    // walk of the index that finds nothing and answers "not known".
    let as_a_type = crate::sema::types::parse_type_spelling(derived);
    if is_an_integer(&as_a_type)
        || is_a_floating_point(&as_a_type)
        || matches!(derived.trim(), "void" | "auto" | "nullptr_t" | "std::nullptr_t")
    {
        return Some(false);
    }

    let index = context.model.index();
    let base = base.trim();
    let base = base.rsplit("::").next().unwrap_or(base);

    let mut waiting = vec![derived.to_string()];
    let mut seen = std::collections::HashSet::new();
    let mut answered = true;

    while let Some(class) = waiting.pop() {
        let class = class.rsplit("::").next().unwrap_or(&class).to_string();
        if !seen.insert(class.clone()) {
            continue;
        }
        if class == base {
            return Some(true);
        }

        let mut facts = index
            .files_declaring(&class, context.path)
            .into_iter()
            .filter(|found| found.fact.kind == crate::DeclKind::Type);
        let Some(found) = facts.next() else {
            // A class nobody has read: the answer is not "no", it is "not known".
            answered = false;
            continue;
        };
        if facts.next().is_some() {
            answered = false;
            continue;
        }

        for written in &found.fact.bases {
            // `public: std::iostream` and `private Widget` are both written with an access keyword, and a virtual
            // base with `virtual`; none of it changes which class the name is.
            let written = written.trim();
            let name = written
                .split_whitespace()
                .next_back()
                .unwrap_or(written)
                .trim_end_matches('<');
            waiting.push(name.to_string());
        }
    }

    answered.then_some(false)
}

/// **The constrained parameter positions of a declaration in another file** — from the summary, which is where a
/// template's head is once the file has been read.
///
/// `None` when the index holds no such declaration, holds two, or holds one whose constraints are all empty — the
/// three cases where the honest answer is "this is not a question the index can answer".
fn from_the_index(
    context: &CheckContext<'_>,
    name: &str,
) -> Option<Vec<(usize, Concept)>> {
    let index = context.model.index();
    let found = index.files_declaring(name, context.path);
    if found.len() != 1 {
        return None;
    }
    let fact = &found[0].fact;
    if fact.kind != crate::DeclKind::Function {
        return None;
    }

    let constrained: Vec<(&str, Concept)> = fact
        .parameters
        .iter()
        .zip(fact.constraints.iter())
        .filter(|(_, constraint)| !constraint.is_empty())
        .filter_map(|(parameter, constraint)| {
            Concept::from_constraint(constraint).map(|concept| (parameter.as_str(), concept))
        })
        .collect();
    if constrained.is_empty() {
        return None;
    }

    let spellings = crate::sema::overload::parameters_in(fact.parameter_list.as_deref()?).0;
    let mut positions = Vec::new();
    for (at, spelling) in spellings.iter().enumerate() {
        for (parameter, concept) in &constrained {
            if mentions_the_word(spelling, parameter) {
                positions.push((at, concept.clone()));
            }
        }
    }

    (!positions.is_empty()).then_some(positions)
}