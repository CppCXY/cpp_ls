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
    Integral,
    FloatingPoint,
    Arithmetic,
    Pointer,
    Void,
    Signed,
    Unsigned,
    Reference,
    Const,
    Volatile,
    Array,
    Function,
}

impl Trait {
    fn named(name: &str) -> Option<Trait> {
        Some(match name {
            "is_integral" => Trait::Integral,
            "is_floating_point" => Trait::FloatingPoint,
            "is_arithmetic" => Trait::Arithmetic,
            "is_pointer" => Trait::Pointer,
            "is_void" => Trait::Void,
            "is_signed" => Trait::Signed,
            "is_unsigned" => Trait::Unsigned,
            "is_reference" => Trait::Reference,
            "is_const" => Trait::Const,
            "is_volatile" => Trait::Volatile,
            "is_array" => Trait::Array,
            "is_function" => Trait::Function,
            _ => return None,
        })
    }

    /// `None` when the spelling is not a type this layer knows — an unbound template parameter, a `decltype` — because a
    /// trait of an unknown type is *cannot tell*, and answering `false` would delete a candidate.
    ///
    /// # What the spelling alone can answer
    ///
    /// ```text
    /// the scalar          int, double, void …        is_integral, is_floating_point, is_arithmetic, is_void
    /// the sign            unsigned int, int          is_signed, is_unsigned
    /// the decorators      int*   int&   const int    is_pointer, is_reference, is_const, is_volatile
    ///                     int[4] int(int)             is_array, is_function
    /// ```
    ///
    /// The decorators are read off the same pieces the scalar is read from, so `const int*` is a pointer that is **not**
    /// const and `const int&` is a reference that **is** — in that order, which is what the language means by each.
    ///
    /// **`is_class` and `is_enum` are not here**, and the refusal is the honest answer rather than an omission: a name is a
    /// class, an enum or a typedef, and only a symbol table says which. `is_class<std::string>` answered `false` would delete
    /// the overload the program chose; answered `true` it would delete the other. So would `is_base_of` and
    /// `is_convertible`, which are relations between two types rather than a property of one spelling.
    fn of(self, spelling: &str) -> Option<bool> {
        let spelling = spelling.trim();
        // **A reference is a trailing `&`**, one or two, and the rest of the spelling is what it refers to.
        let reference = spelling.ends_with('&');
        let without_reference = spelling.trim_end_matches('&').trim();
        // **`const` and `volatile` on the *topmost* type** — not on what it points at or refers to. `const int` is const;
        // `const int*` is a pointer to a const int and is **not** const itself, and `const int&` is a reference to one.
        // The outermost qualifier is either at the front with nothing wrapping it after it, or written after the `*`
        // (`int* const`). Read from the spelling as it was given, before the decorators are peeled.
        let topmost = without_reference;
        let qualified_const = (topmost.starts_with("const ") && !topmost.contains('*'))
            || topmost.ends_with(" const");
        let qualified_volatile = (topmost.starts_with("volatile ") && !topmost.contains('*'))
            || topmost.ends_with(" volatile");
        // **The qualifiers are then peeled** so the scalar underneath can be read: `const volatile int` is an `int`.
        let mut bare = without_reference;
        loop {
            if let Some(rest) = bare.strip_prefix("const ") {
                bare = rest.trim();
                continue;
            }
            if let Some(rest) = bare.strip_prefix("volatile ") {
                bare = rest.trim();
                continue;
            }
            break;
        }
        let mut pointers_start = bare;
        while let Some(rest) = pointers_start.strip_suffix('*') {
            pointers_start = rest.trim();
        }
        let pointers = bare.ends_with('*');
        let scalars = pointers_start.trim();
        // **A declaration's own shape**: an array writes `[`, a function type writes `(` and no object type does.
        let array = scalars.contains('[');
        let function = !array && scalars.contains('(');
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
            !scalars.starts_with("unsigned") && !matches!(scalars, "bool" | "unsigned") && integral;
        // **A name this layer cannot classify is not evidence of anything.** The convention that names a template
        // parameter is one capital letter (T, U, V) is what separates "a type nobody bound" from "a type the program
        // named" — the first is *cannot tell*. `decltype(…)` and `auto` are not types until something decides them, and
        // a name whose own spelling this cannot place is left to the traits that need no scalar at all (a name with `&`
        // *is* a reference whatever it names).
        let a_parameter_by_convention = scalars.len() == 1
            && scalars
                .chars()
                .all(|character| character.is_ascii_uppercase());
        let unknown =
            a_parameter_by_convention || scalars.starts_with("decltype") || scalars == "auto";

        // The decorators answer for **any** type, known or not: `std::string&` is a reference whichever class `string` is.
        match self {
            Trait::Reference => return Some(reference),
            Trait::Const => {
                if unknown {
                    return None;
                }
                return Some(qualified_const);
            }
            Trait::Volatile => {
                if unknown {
                    return None;
                }
                return Some(qualified_volatile);
            }
            Trait::Array => {
                if unknown {
                    return None;
                }
                return Some(array);
            }
            Trait::Function => {
                if unknown {
                    return None;
                }
                return Some(function);
            }
            _ => {}
        }

        if unknown {
            return None;
        }
        Some(match self {
            Trait::Integral => integral && !pointers && !reference,
            Trait::FloatingPoint => floating && !pointers && !reference,
            Trait::Arithmetic => (integral || floating) && !pointers && !reference,
            Trait::Pointer => pointers && !reference,
            Trait::Void => scalars == "void" && !pointers && !reference,
            Trait::Signed => signed,
            Trait::Unsigned => integral && !signed,
            // Answered above, before the scalar was needed.
            Trait::Reference | Trait::Const | Trait::Volatile | Trait::Array | Trait::Function => {
                unreachable!("answered before the scalar")
            }
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
    let argument = substitute(trait_call.argument, bindings);
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

/// Replace a bound template parameter with its type, as a **word** — `std::is_integral_v<T>` with `T = double` answers
/// `std::is_integral_v<double>`.
///
/// The word boundary is the whole of it: the first version split the spelling into words, joined them with a separator,
/// replaced, and joined them again **without restoring the punctuation** — so `std::is_integral_v<T>` became
/// `stdis_integral_vdouble`, and a condition that named a concept answered "cannot tell" for a reason that had nothing to do
/// with concepts.
fn substitute(spelling: &str, bindings: &[(String, String)]) -> String {
    let mut out = String::with_capacity(spelling.len());
    let mut at = 0usize;
    let characters: Vec<(usize, char)> = spelling.char_indices().collect();
    while at < characters.len() {
        let (start, character) = characters[at];
        if !character.is_alphanumeric() && character != '_' {
            out.push(character);
            at += 1;
            continue;
        }
        let mut end = at;
        while end < characters.len()
            && (characters[end].1.is_alphanumeric() || characters[end].1 == '_')
        {
            end += 1;
        }
        let word = &spelling[start
            ..characters
                .get(end)
                .map_or(spelling.len(), |(offset, _)| *offset)];
        match bindings.iter().find(|(parameter, _)| parameter == word) {
            Some((_, concrete)) => out.push_str(concrete),
            None => out.push_str(word),
        }
        at = end;
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

/// **Decide a condition, expanding the concepts it names** — [`satisfies`] with a way to look a concept up.
///
/// `expand` answers `(parameter, body)` for a concept name: `signed_integral` → `("T", "integral<T> && is_signed_v<T>")`.
/// The body is then evaluated with the parameter bound to the argument the concept was applied to, and a body that names
/// another concept recurses the same way. The depth is bounded because a concept may name itself (`concept X = X<T>`), and
/// a walk that trusted the text would never end.
pub fn satisfies_with(
    condition: &str,
    bindings: &[(String, String)],
    expand: &mut dyn FnMut(&str) -> Option<(String, String)>,
) -> Option<bool> {
    satisfies_inside(condition, bindings, expand, 0)
}

/// How many concepts deep one condition is expanded. A concept that composes three others is the deepest shape in the
/// standard library's own headers, and the bound exists for the self-referential spelling rather than for a real one.
const MAX_CONCEPT_DEPTH: usize = 8;

fn satisfies_inside(
    condition: &str,
    bindings: &[(String, String)],
    expand: &mut dyn FnMut(&str) -> Option<(String, String)>,
    depth: usize,
) -> Option<bool> {
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
                let verdict = satisfies_inside(&part, bindings, expand, depth);
                decided.push(verdict?);
            }
            return Some(if operator == "||" {
                decided.iter().any(|one| *one)
            } else {
                decided.iter().all(|one| *one)
            });
        }
    }
    if let Some(rest) = condition.strip_prefix('!') {
        return Some(!satisfies_inside(rest, bindings, expand, depth)?);
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

    // **`std::is_integral<T>::value`**, `{}`, `_v`, `()` — one trait, four spellings.
    let trait_call = a_trait_call(condition)?;
    let argument = substitute(trait_call.argument, bindings);
    if argument.contains('<') || argument.is_empty() {
        return None;
    }
    if let Some(trait_) = Trait::named(&trait_call.name) {
        return trait_.of(&argument);
    }

    // **A concept the caller knows** — `integral<T>` inside another concept's body. Expanded with its own parameter bound
    // to this argument, then decided.
    if depth >= MAX_CONCEPT_DEPTH {
        return None;
    }
    let (parameter, body) = match expand(&trait_call.name) {
        Some(found) => found,
        None => {
            return None;
        }
    };
    let substituted = substitute(&body, &[(parameter, argument)]);
    satisfies_inside(&substituted, bindings, expand, depth + 1)
}
