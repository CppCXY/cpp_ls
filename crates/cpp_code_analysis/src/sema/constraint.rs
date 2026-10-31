//! **A condition written in a template's signature, decided** — the `enable_if` idiom and the traits it asks about.
//!
//! ```cpp
//! template <typename T>
//! typename std::enable_if<std::is_integral<T>::value, std::string>::type describe(T);
//! ```
//!
//! Two declarations of `describe` differ only in that condition, so *which* one a reader's `describe(1.5)` calls is the
//! question `textDocument/definition` has to answer — and until this module existed the answer was the first declaration
//! the scope happened to bind, for either call.
//!
//! # What it decides, and what it refuses
//!
//! The condition is a **constant expression over types**, and this is not a compiler: it knows the fundamental traits by
//! name ([`Trait`]), the operators `!`, `&&` and `||`, and the three spellings a trait's answer is written with
//! (`::value`, `{}`, `_v`). Everything else — a user's own trait, a `requires` clause, a trait of a template parameter
//! that nothing has bound — answers [`None`], meaning *this layer cannot tell*, and a caller must keep the candidate
//! rather than delete it. Deleting on "unknown" is how a reader loses the overload their program actually chose.

/// **The fundamental traits this layer can answer** — the ones whose answer follows from the spelling alone.
///
/// Deliberately short: each is a property of a builtin type that needs no declaration to decide, and adding a name here
/// is claiming to know what the standard library's own definition says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trait {
    IsIntegral,
    IsFloatingPoint,
    IsArithmetic,
    IsPointer,
    IsVoid,
    IsSigned,
    IsUnsigned,
}

impl Trait {
    fn named(name: &str) -> Option<Trait> {
        Some(match name {
            "is_integral" => Trait::IsIntegral,
            "is_floating_point" => Trait::IsFloatingPoint,
            "is_arithmetic" => Trait::IsArithmetic,
            "is_pointer" => Trait::IsPointer,
            "is_void" => Trait::IsVoid,
            "is_signed" => Trait::IsSigned,
            "is_unsigned" => Trait::IsUnsigned,
            _ => return None,
        })
    }

    /// `None` when the spelling is not a type this layer knows — an unbound template parameter, a class, a `decltype` —
    /// because a trait of an unknown type is *cannot tell*, and answering `false` would delete a candidate.
    fn of(self, spelling: &str) -> Option<bool> {
        let spelling = spelling.trim();
        let bare = spelling
            .strip_prefix("const ")
            .map(str::trim)
            .unwrap_or(spelling);
        let pointers = bare.ends_with('*');
        let scalars = bare.trim_end_matches('*').trim();
        let integral = matches!(
            scalars,
            "char"
                | "signed char"
                | "unsigned char"
                | "short"
                | "unsigned short"
                | "int"
                | "unsigned int"
                | "unsigned"
                | "long"
                | "unsigned long"
                | "long long"
                | "unsigned long long"
                | "bool"
                | "wchar_t"
                | "char8_t"
                | "char16_t"
                | "char32_t"
                | "size_t"
                | "ptrdiff_t"
                | "std::size_t"
        );
        let floating = matches!(scalars, "float" | "double" | "long double");
        let signed =
            !bare.starts_with("unsigned") && !matches!(scalars, "bool" | "unsigned") && integral;
        // **A name this layer cannot classify is not evidence of a class.** The convention that names a template
        // parameter is one capital letter (T, U, V) is what separates "a type nobody bound" from "a type the
        // program named" — the first is *cannot tell*, the second is a class, because every builtin is in the list
        // above. decltype(...) and uto are not types until something decides them, so they are cannot-tell too.
        let a_parameter_by_convention = scalars.len() == 1
            && scalars
                .chars()
                .all(|character| character.is_ascii_uppercase());
        if a_parameter_by_convention || scalars.starts_with("decltype") || scalars == "auto" {
            return None;
        }
        Some(match self {
            Trait::IsIntegral => integral && !pointers,
            Trait::IsFloatingPoint => floating && !pointers,
            Trait::IsArithmetic => (integral || floating) && !pointers,
            Trait::IsPointer => pointers,
            Trait::IsVoid => scalars == "void" && !pointers,
            Trait::IsSigned => signed,
            Trait::IsUnsigned => integral && !signed,
        })
    }
}

/// **Decide a condition**, with the template parameters bound to the types a call deduced.
///
/// `bindings` is `[("T", "int")]` for `describe(41)`: the call's argument types are what the template parameter stands
/// for, in order. `None` means *cannot tell* — an unbound parameter, a trait this layer does not know, a clause that is
/// not a condition it reads.
pub fn satisfies(condition: &str, bindings: &[(String, String)]) -> Option<bool> {
    let condition = condition.trim();
    if condition.is_empty() {
        return None;
    }

    // The operators, from weakest to strongest, split at the *top level* so a nested `is_same<a, b>` is not cut in two.
    for operator in ["||", "&&"] {
        let parts = top_level_split(condition, operator);
        if parts.len() > 1 {
            let mut decided = Vec::with_capacity(parts.len());
            for part in parts {
                decided.push(satisfies(&part, bindings)?);
            }
            return Some(if operator == "||" {
                decided.iter().any(|one| *one)
            } else {
                decided.iter().all(|one| *one)
            });
        }
    }
    if let Some(rest) = condition.strip_prefix('!') {
        return Some(!satisfies(rest, bindings)?);
    }
    if condition == "true" {
        return Some(true);
    }
    if condition == "false" {
        return Some(false);
    }

    // **`std::is_same<A, B>`** — the one trait with two arguments, and the one every `!is_same<T, char>` needs.
    if let Some(arguments) = a_trait_call_with_name(condition, "is_same") {
        let mut parts = top_level_split(arguments, ",").into_iter();
        let left = substitute(&parts.next()?, bindings);
        let right = substitute(&parts.next()?, bindings);
        if left.contains('<') || right.contains('<') {
            return None;
        }
        return Some(left.trim() == right.trim());
    }

    // **`std::is_same<A, B>`** — the one trait with two arguments, and the one every `!is_same<T, char>` needs.
    if let Some(arguments) = a_trait_call_with_name(condition, "is_same") {
        let mut parts = top_level_split(arguments, ",").into_iter();
        let left = substitute(&parts.next()?, bindings);
        let right = substitute(&parts.next()?, bindings);
        if left.contains('<') || right.contains('<') {
            return None;
        }
        return Some(left.trim() == right.trim());
    }

    // **`std::is_integral<T>::value`**, `std::is_integral<T>{}`, `std::is_integral_v<T>` — one trait, three spellings.
    let trait_call = a_trait_call(condition)?;
    let argument = substitute(&trait_call.argument, bindings);
    if argument.contains('<') || argument.is_empty() {
        return None;
    }
    Trait::named(&trait_call.name)?.of(&argument)
}

/// **A condition spliced out of a declaration** — the text between `enable_if<` and its closing `>`, or the expression a
/// `requires` clause states.
///
/// ```cpp
/// typename std::enable_if<std::is_integral<T>::value, std::string>::type describe(T);   // the C++11 spelling
/// template <typename T> requires std::is_integral_v<T> std::string describe(T);          // C++20's own
///                       ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
/// ```
///
/// `None` when the declaration has no condition at all, which is the ordinary function. A **requires-expression**
/// (`requires { value.size(); }`) is also `None`: it is a block of statements asking whether each one is well formed, not a
/// constant expression this layer can decide, and answering it as if it were would drop the wrong candidate.
pub fn the_condition_of(declaration: &str) -> Option<&str> {
    the_enable_if_argument(declaration)
}

/// The first argument of the first `enable_if<…>` — the condition, in both `enable_if` and `enable_if_t`.
fn the_enable_if_argument(declaration: &str) -> Option<&str> {
    let at = declaration.find("enable_if")?;
    let after = &declaration[at + "enable_if".len()..];
    let open = after.find('<')?;
    let mut depth = 0isize;
    for (index, character) in after[open..].char_indices() {
        match character {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    let inside = &after[open + 1..open + index];
                    // The first argument is the condition; `enable_if<cond, T>` and `enable_if_t<cond, T>` both put it
                    // first, and `enable_if<cond>` has nothing else.
                    return Some(inside.split(',').next().unwrap_or_default().trim());
                }
            }
            _ => {}
        }
    }
    None
}

/// **The constraint a `RequiresClause` node states** — the node's own text without its keyword.
///
/// ```cpp
/// template <typename T> requires std::is_integral_v<T>
///                       ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
/// ```
///
/// Read from the **node**, not from the declaration's text: the clause and the declaration that follows it are separated
/// by nothing but a newline, so a text scan cannot tell `requires is_integral_v<T>` from `requires is_integral_v<T>
/// std::string describe(T)`. The grammar knows, and this is that knowledge.
///
/// `None` when the node is a **requires-expression** (`requires { … }`), which asks whether statements compile rather than
/// stating a condition this layer could decide.
pub fn a_requires_clause(clause: &str) -> Option<&str> {
    let text = clause.trim_start();
    let rest = text.strip_prefix("requires")?.trim_start();
    if rest.starts_with('{') {
        return None;
    }
    (!rest.is_empty()).then_some(rest)
}

/// The argument list of `[std::]`name`<…>`, when the condition *is* that trait.
fn a_trait_call_with_name<'a>(condition: &'a str, wanted: &str) -> Option<&'a str> {
    let call = a_trait_call(condition)?;
    (call.name == wanted).then_some(call.argument)
}
struct TraitCall<'a> {
    name: String,
    argument: &'a str,
}

/// `[std::]is_X<T>[::value | {} | _v]` — the three spellings, and nothing else.
fn a_trait_call(condition: &str) -> Option<TraitCall<'_>> {
    let condition = condition.trim();
    let condition = condition.strip_suffix("::value").unwrap_or(condition);
    let condition = condition.strip_suffix("{}").unwrap_or(condition);
    let condition = condition.strip_suffix("()").unwrap_or(condition);
    let condition = condition.trim();
    let open = condition.find('<')?;
    let name = condition[..open].trim();
    let name = name.strip_suffix("_v").unwrap_or(name);
    let name = name.rsplit("::").next().unwrap_or(name).trim();
    let close = condition.rfind('>')?;
    Some(TraitCall {
        name: name.to_string(),
        argument: condition[open + 1..close].trim(),
    })
}

/// Replace a bound template parameter with its type, as a word.
fn substitute(spelling: &str, bindings: &[(String, String)]) -> String {
    let mut out = spelling.to_string();
    for (parameter, concrete) in bindings {
        out = out
            .split(|character: char| !character.is_alphanumeric() && character != '_')
            .collect::<Vec<_>>()
            .join("\0");
        let replaced = out
            .split('\0')
            .map(|word| {
                if word == parameter {
                    concrete.as_str()
                } else {
                    word
                }
            })
            .collect::<Vec<_>>()
            .join("");
        out = replaced;
    }
    out
}

/// Split at `operator` outside any angle brackets or parentheses.
fn top_level_split(text: &str, operator: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0isize;
    let mut start = 0usize;
    let bytes = text.as_bytes();
    let mut at = 0usize;
    while at < bytes.len() {
        match bytes[at] as char {
            '<' | '(' => depth += 1,
            '>' | ')' => depth -= 1,
            _ => {}
        }
        if depth == 0 && text[at..].starts_with(operator) {
            parts.push(text[start..at].trim().to_string());
            at += operator.len();
            start = at;
            continue;
        }
        at += 1;
    }
    parts.push(text[start..].trim().to_string());
    parts
}

/// **The concept a declaration's template parameter names**, if it names one.
///
/// ```cpp
/// template <integral T> std::string describe(T);          // `integral` here
/// template <typename T> requires integral<T> …            // and here
/// ```
///
/// A constraint that is a *name* has no expression in the declaration to evaluate, so this answers the name and the caller
/// finds the definition. `None` for the ordinary `typename T` / `class T` / `auto`, which name no concept.
pub fn a_concept_name(declaration: &str) -> Option<String> {
    // The `requires NAME<…>` form first: the keyword makes the name unambiguous.
    if let Some(at) = declaration.find("requires") {
        let after = declaration[at + "requires".len()..].trim_start();
        if let Some(open) = after.find('<') {
            let name = after[..open].trim();
            if is_a_name(name) {
                return Some(name.to_string());
            }
        }
    }

    // **The template parameter list** — `template <integral T>`: a parameter whose first word is a name and whose second
    // is the parameter it constrains. `typename T`, `class U` and `auto` are the ones that are *not* this.
    let at = declaration.find("template")?;
    let after = &declaration[at + "template".len()..];
    let open = after.find('<')?;
    let mut depth = 0isize;
    let mut close = None;
    for (index, character) in after[open..].char_indices() {
        match character {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + index);
                    break;
                }
            }
            _ => {}
        }
    }
    let inside = &after[open + 1..close?];
    for parameter in inside.split(',') {
        let words: Vec<&str> = parameter.split_whitespace().collect();
        let [first, second, ..] = words.as_slice() else {
            continue;
        };
        if matches!(*first, "typename" | "class" | "auto" | "template")
            || first.contains('=')
            || !is_a_name(first)
            || !is_a_name(second)
        {
            continue;
        }
        return Some((*first).to_string());
    }
    None
}

/// An identifier as this layer spells one: letters, digits and `_`, starting with a letter or `_`.
fn is_a_name(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .next()
            .is_some_and(|first| first.is_alphabetic() || first == '_')
        && word
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
}
