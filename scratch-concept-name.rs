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
    let open = declaration.find("template")?;
    let after = &declaration[open + "template".len()..];
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
        if matches!(*first, "typename" | "class" | "auto" | "template" | "int" | "unsigned")
            || first.contains('=')
        {
            continue;
        }
        // A constrained parameter declares `<concept> <name>`; `std::size_t N` is a non-type parameter and is not this.
        if is_a_name(second) && is_a_name(first) && !first.contains('<') {
            return Some((*first).to_string());
        }
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
        && word.chars().all(|character| character.is_alphanumeric() || character == '_')
}
