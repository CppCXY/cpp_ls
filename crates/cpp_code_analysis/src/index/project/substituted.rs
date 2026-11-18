//! **Substitution at the use** — a member's type with its class's template arguments written in.
//!
//! Moved here from `project/mod.rs` without a change, together with the standard's own fixed answers (`TableCost`,
//! `names_define_static_array`) that the member and call walks rely on.

use super::*;

/// **What a template-id's own arguments do to the type its declaration wrote.**
///
/// `template <class T> constexpr T pi = T(3.14159);` declares a type spelled **`T`**, and `pi<double>` names that
/// declaration with `double` written for it. Without this, the type of a use of a variable template was the
/// parameter's own name — measured: `double x = pi<double>;` answered `T`, which is not a missing answer but a wrong
/// one, because nothing in the answer says the file did not write it.
///
/// The pairing is positional and both halves already exist: the declaration's template parameter names — through
/// [`NamedDeclaration::parameters_of`], which reads `DeclFact::parameters` for a declaration in another file and walks
/// this tree for one in the file being edited — and the arguments the **use** wrote, through the same
/// `written_template_arguments` the call path uses. Nothing is invented here. When either side is empty the type is
/// returned untouched, which is the ordinary case and the reason this is a pass-through rather than a step in the
/// chain: `v.front` on a `std::vector<int>` is substituted from the *object's* type, which is a different question
/// with a different source of arguments.
pub(crate) fn substituted_at_the_use(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    expression: &cpp_parser::CppSyntaxNode,
    named: &NamedDeclaration,
    type_of: Type,
) -> Type {
    let arguments = written_template_arguments(expression);
    if arguments.is_empty() {
        return type_of;
    }

    let (names, _) = named.parameters_of(
        &crate::sema::declarations::ShapeTable::new(),
        index,
        scopes,
        root,
        path,
    );
    if names.is_empty() {
        return type_of;
    }

    // **And what a parameter the use did not write stands for.** `twice<float>` writes one argument for
    // `template <class T, class U = T>`, so the second meaning is the first argument — see
    // [`crate::TypeSubstitutions::with_defaults`]. The defaults are written in the declaration's own parameter names
    // (`U = T`), so each one is substituted with what the use wrote before it stands for anything; a default that
    // names itself is left alone by `TypeSubstitutions::get`.
    let written_defaults = named.parameter_defaults_of(index, root);
    let written_arguments: Vec<Type> = arguments.clone();
    let mut defaults: Vec<Type> = Vec::with_capacity(written_defaults.len());
    for default in &written_defaults {
        if default.trim().is_empty() {
            defaults.push(Type::builtin(""));
            continue;
        }
        let parsed = parse_type_spelling(default);
        defaults.push(
            parsed.substituted(&crate::sema::types::TypeSubstitutions::new(
                &names,
                &written_arguments,
            )),
        );
    }

    let substitutions =
        crate::sema::types::TypeSubstitutions::with_defaults(&names, &written_arguments, &defaults);
    type_of.substituted(&substitutions)
}
/// **An argument spelling as the pattern comparison should see it**: a decidable compiler builtin, folded.
///
/// `enable_if<true, _Ty>` is a pattern with a literal in it, and the uses that select it are written
/// `enable_if<__is_same(_Ty, _Uty), _Ty>` — so the comparison has to be against what the builtin *answers* rather
/// than against how it is spelled. [`crate::sema::builtins`] decides the ones the written arguments settle
/// (`__is_same(int, int)` is `true`, `__is_pointer(int*)` is `true`) and leaves the rest open
/// (`__is_constructible(T, U)` is a question about a class body, and half of one is not in front of this layer).
///
/// An undecided builtin is returned **unchanged**, so it compares equal to nothing except its own spelling — which
/// is what it did before this existed. The fold can therefore only add the matches that are true: it never removes
/// one that was right, and it never invents one, which is the rule this whole matcher follows.
pub(crate) fn folded_for_matching(argument: &str) -> String {
    crate::sema::builtins::folded(argument)
}
/// **The class and member an alias template's spelling stands for** — and `None` when it is not one.
///
/// This is the step a comment in this file has been referring to as `the_alias_behind` since long before it existed.
/// `template <bool _Test, class _Ty> using enable_if_t = typename enable_if<_Test, _Ty>::type;` is stored as the
/// spelling after its `=`, so `enable_if_t<__is_same(int, int), double>` becomes
/// `typename enable_if<__is_same(int, int), double>::type` once the alias's own parameters are replaced — and what
/// that *names* is the member `type` of the class `enable_if<__is_same(int, int), double>`.
///
/// Two answers, in one pair: the class the member is looked up in, and the member's name. A target with no `::` in it
/// — `template <class _Ty> using ptr = _Ty*;` — is not a member of anything, and that case is answered by the caller
/// parsing the substituted spelling; the member name is empty then, which is how it says so.
pub(crate) fn the_alias_behind(
    index: &ProjectIndex,
    declaring: &Path,
    written: &str,
) -> Option<(String, String)> {
    let (head, rest) = written.split_once('<')?;
    let inside = rest.trim_end().strip_suffix('>')?;
    let Known::Yes(found) = index.definition(head.trim(), declaring) else {
        return None;
    };
    if found.fact.kind != crate::DeclKind::Type {
        return None;
    }

    let target = found.fact.type_of.as_deref()?;
    let arguments = crate::sema::builtins::split_arguments(inside);
    let substituted =
        crate::sema::types::substituted_spelling(target, found.fact.parameters(), &arguments);

    // `typename enable_if<_Test, _Ty>::type` — the last `::` separates the class from the member it names.
    let substituted = substituted
        .trim()
        .trim_start_matches("typename ")
        .trim()
        .trim_start_matches("class ")
        .trim();
    let (named, member) = substituted.rsplit_once("::")?;

    Some((named.trim().to_string(), member.trim().to_string()))
}

/// **`std::vector<int>`'s members, with `T` written through as `int`.**
///
/// A member's fact records the spelling its **class** wrote — `void push_back(T value)`, `T& front()` — and every
/// consumer of the list reads those spellings: completion offers the parameters, hover shows them, `param-type-check`
/// and `argument-count` compare against them. For an instantiated use they are wrong, and the consequence is silence:
/// `std::vector<int>::push_back("text")` is not reported, because `T value` is a parameter neither check can classify.
///
/// The substitution is `sema::types::substituted_spelling` — the same function that makes a member's **return** type
/// come out as `int&` — so this is the arguments reaching the member list, not a second implementation of anything.
///
/// # The parameter names come from the **fact**, and that is a correction
///
/// They used to be read out of the class's file — `template_parameters_before(text, offset)` — which required a
/// reader and the file's **own** text: the fact's offset is the file's, while a `FileView` may be of a *rendering*
/// whose offsets are shifted by everything the cook deleted. `view.written_text().unwrap_or(&view.source)` was the
/// guard, and it is only right when the caller happened to hand over a written view. `Session::members_of` does;
/// `SemanticModel::members_of` — which is what every **check** goes through — hands over the rendering, so
/// `written_text()` was `None`, the guard fell through to the rendering, no `template <…>` was found at the offset,
/// and the substitution was silently skipped.
///
/// Measured on the report this was written for: `std::vector<int> anotherVec; anotherVec.push_back("123131");` on a
/// class declared `void push_back(const value_type& __x);` with `using value_type = _Tp;` — the parameter stayed
/// `const value_type&`, `convertible_to(char*, const value_type&)` answered `Unknown`, and the check said nothing.
/// `DeclFact::parameters` is the class's parameter names, recorded by `declared_template_parameters_with` when the
/// fact was built, so the answer needs no reader at all — and `SemanticModel` and `Session` cannot disagree about it.
pub fn substituted_members(
    index: &ProjectIndex,
    from: &Path,
    class: &str,
    listed: Known<MemberList>,
) -> Known<MemberList> {
    let Known::Yes(mut list) = listed else {
        return listed;
    };
    let Some((base, arguments)) = split_template_arguments(class) else {
        return Known::Yes(list);
    };
    let Some(declared) = index
        .files_declaring(&base, from)
        .into_iter()
        .find(|declared| declared.fact.kind == crate::DeclKind::Type)
    else {
        return Known::Yes(list);
    };
    let names = declared.fact.parameters().to_vec();
    if names.is_empty() {
        return Known::Yes(list);
    }

    // **A declaration with a defaulted parameter is written without it at the use.** `std::vector<int>` writes one
    // argument for `template <class _Tp, class _Alloc = allocator<_Tp>>` — two names — and the default is part of the
    // class's identity, not something to drop: `_Alloc` standing unsubstituted is what made
    // `auto pos = events.size()` answer `allocator_traits<A>::size_type`. The substitution therefore uses the
    // declaration's **defaults** for the parameters the use did not write
    // ([`substituted_spelling_with_defaults`]), which is the same rule `variable_initializers` and the call path use.
    // Measured on the real `<vector>`: without the defaults, `push_back("123131")` on a `std::vector<int>` stayed
    // silent while the one-parameter fixture beside it reported — the arguments were cut to the shorter side and the
    // defaulted parameter was simply left standing.
    let defaults = index.template_parameter_defaults_for(&base, from, &arguments);
    if names.is_empty() {
        return Known::Yes(list);
    }

    // **The class's own aliases, expanded before the arguments go in.** `std::vector` declares its parameter as
    // `void push_back(const value_type& __x);` with `using value_type = _Tp;` in the class — so substituting
    // `_Tp → int` alone leaves the word `value_type`, which nothing in this layer can compare: measured on
    // `std::vector<int> anotherVec; anotherVec.push_back("123131");`, the target stayed `const value_type&`,
    // `convertible_to(const char[7], const value_type&)` answered `Unknown`, and a check that cannot compare says
    // nothing — the report. An alias's target is in its own fact (`DeclFact::type_of` is the target for an alias,
    // see `declared_alias_target`), and the aliases of *this* class are already in the list being substituted,
    // which is why no second lookup is needed.
    //
    // A plain `using value_type = _Tp;` inside a class template records the **class's** parameters as its own
    // (`declared_template_parameters_with` pairs the `template <…>` head with the declaration it introduces), so
    // there is deliberately no `parameters.is_empty()` test here: the alias that matters most has one.
    let mut alias_names: Vec<String> = Vec::new();
    let mut alias_targets: Vec<String> = Vec::new();
    for member in list
        .members
        .iter()
        .filter(|member| member.depth == 0 && member.fact.kind == crate::DeclKind::Type)
    {
        let Some(target) = member.fact.type_of.clone() else {
            continue;
        };
        // **Not the alias itself**: `type_of` is the target, and a member whose target names itself would make
        // `value_type` → `value_type` a rewrite that changes nothing but costs a pass.
        if target == member.fact.name {
            continue;
        }
        alias_names.push(member.fact.name.to_string());
        alias_targets.push(target.to_string());
    }

    // **An alias's own target is not an alias to expand.** `std::shared_ptr`'s `element_type` is written
    // `using element_type = typename __shared_ptr<_Tp>::element_type;`, so the target contains its own name and
    // replacing that name with the target doubles the prefix —
    // `typename __shared_ptr<Resource>::typename __shared_ptr<Resource>::element_type`, measured. The member's own
    // `type_of` *is* the target, so the expansion is a step for the other members only.
    let declares_an_alias = |member: &ProjectMember| {
        member.fact.kind == crate::DeclKind::Type
            && alias_names
                .iter()
                .any(|name| name == member.fact.name.as_str())
    };
    // **One pass is not enough, and the pass's own order is what decides.** A class's aliases name each other:
    // libstdc++'s `basic_string_view` writes `using value_type = _CharT;` and `using const_pointer = const
    // value_type*;`, so expanding `const_pointer` produces a spelling that still holds `value_type` — and
    // `substituted_spelling` walks the alias list once, in its own order, which may already have passed the name the
    // expansion introduced. Measured: `data()`'s answer came out `const value_type*`, `convertible_to` compared it
    // with a parameter written `const char*`, answered `No`, and every `std::from_chars(text.data(), …)` in a real
    // project was reported as *"this passes `text.data()` where `const char*` is declared"*.
    //
    // So the rewrite is repeated until it stops changing, bounded by a handful of passes rather than a `changed` flag:
    // an alias that names itself would otherwise spin, and an alias chain deeper than four is a library experiment
    // rather than a spelling. **The scan that guards the loop is what makes it affordable** — a member's type usually
    // mentions no alias of its class at all, and the pass below is O(names × length), so asking "does this spelling
    // hold any of them" first is the difference between one scan and one pass per member of every standard-library
    // class. Measured after the first version of this loop: a cold start of a four-header project went from 27 s to
    // over 30 s, which is what put the guard here.
    let through_the_aliases = |spelling: &str| -> String {
        let mut spelling = spelling.to_string();
        for _ in 0..MAX_ALIAS_PASSES {
            if !alias_names
                .iter()
                .any(|name| spelling.contains(name.as_str()))
            {
                break;
            }
            let next =
                crate::sema::types::substituted_spelling(&spelling, &alias_names, &alias_targets);
            if next == spelling {
                break;
            }
            spelling = next;
        }
        spelling
    };
    for member in list
        .members
        .iter_mut()
        .filter(|member| !declares_an_alias(member))
    {
        if !alias_names.is_empty() {
            if let Some(spelling) = member.fact.detail_mut().parameter_list.as_mut() {
                *spelling = through_the_aliases(spelling);
            }
            if let Some(spelling) = member.fact.returns.as_mut() {
                *spelling = through_the_aliases(spelling).into();
            }
        }
        // **`type_of` is a `SmolStr`**, so it takes the same rewrite and converts its own result back.
        if !alias_names.is_empty()
            && let Some(spelling) = member.fact.type_of.as_mut()
        {
            *spelling = through_the_aliases(spelling).into();
        }
    }
    for member in &mut list.members {
        if let Some(spelling) = member.fact.detail_mut().parameter_list.as_mut() {
            *spelling = crate::sema::types::substituted_spelling_with_defaults(
                spelling, &names, &defaults, &arguments,
            );
        }
        if let Some(spelling) = member.fact.returns.as_mut() {
            *spelling = crate::sema::types::substituted_spelling_with_defaults(
                spelling, &names, &defaults, &arguments,
            )
            .into();
        }
        if let Some(spelling) = member.fact.type_of.as_mut() {
            *spelling = crate::sema::types::substituted_spelling_with_defaults(
                spelling, &names, &defaults, &arguments,
            )
            .into();
        }
    }
    Known::Yes(list)
}

/// **Split `std::vector<int>` into its name and its arguments** — `("std::vector", ["int"])`.
///
/// `None` when there is no top-level `<…>` at the end: a plain class name has no arguments to substitute, and returning
/// an empty list rather than `None` would make `std::vector<>` and `std::vector` indistinguishable at the call site.
pub(crate) fn split_template_arguments(spelling: &str) -> Option<(String, Vec<String>)> {
    let spelling = spelling.trim();
    let open = spelling.find('<')?;
    let close = spelling.rfind('>')?;
    if close < open || close + 1 != spelling.len() {
        return None;
    }
    let base = spelling[..open].trim().to_string();
    let mut arguments: Vec<String> = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for character in spelling[open + 1..close].chars() {
        match character {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                arguments.push(current.trim().to_string());
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    if !current.trim().is_empty() {
        arguments.push(current.trim().to_string());
    }
    Some((base, arguments))
}

/// **The class of the definition the cursor is in, when the declarator is qualified** — `Source` for `int Source::GetLine(…)`.
///
/// The question this answers is the language's: **a function defined out of class is a member of that class**, so its
/// body's unqualified lookup includes the class's members. The class is usually in another file (`a.cpp` defines
/// what `a.h` declares), so no scope in *this* file's tree can offer them — the members come from the index, and the
/// only thing that has to be read from the tree is **which** class the declarator names.
///
/// # Where the name is written, and why it is read in two places
///
/// The grammar puts the qualified name in one of two places, and both are the same declaration:
///
/// ```text
/// int Source::GetLine(int n) { … }      Declarator > NameExpr "Source::GetLine"     ← written against `(`
/// void Box<int>::grow(…) { … }          DeclSpecifierSeq "void Box<int>::grow"      ← the qualifier has template args
/// ```
///
/// In the first the name is written straight against the parameter list, and since **a type in a declaration is never
/// followed by `(`** the declaration rule knows it is the declarator's own name. In the second `Box` is followed by
/// `<`, which a declaration cannot tell from a return type, so the whole spelling stays in the specifier sequence and
/// is read there (the same two shapes [`crate::sema::scopes`] reads for the scope tree).
///
/// `None` for anything else — a cursor outside a declaration, an ordinary declarator, or a qualifier with more than
/// one level (which would need the nested class's own file, and is left to the scope chain).
pub(crate) fn the_class_a_definition_belongs_to(
    root: &cpp_parser::CppSyntaxNode,
    offset: usize,
) -> Option<String> {
    use cpp_parser::CppAstNode;

    for declaration in root.descendants() {
        let range = declaration.text_range();
        if usize::from(range.start()) > offset || offset >= usize::from(range.end()) {
            continue;
        }
        let Some(declaration) = cpp_parser::CppDeclaration::cast(declaration.clone()) else {
            continue;
        };
        // **One reader for "what is this declaration's own name"**, the same one the scope tree uses — see
        // [`crate::sema::scopes::the_qualified_name_of_a_declaration`]. Two readers of the same two shapes is how the
        // completion and the scope tree came to disagree about an out-of-class definition in the first place.
        let Some((qualifier, _)) =
            crate::sema::scopes::the_qualified_name_of_a_declaration(declaration.syntax())
        else {
            continue;
        };
        // The class is the **last segment of the qualifier** — `Source` for `Source::GetLine`, and `Inner` for
        // `Outer::Inner::deep` (whose innermost class is the one the declaration belongs to). A segment with `::`
        // left in it is a qualifier this file cannot name as a class, which is the answer `None` gives.
        let before = qualifier.trim_end();
        let start = before
            .rfind(|character: char| {
                !(character.is_alphanumeric() || matches!(character, '_' | ':'))
            })
            .map(|boundary| boundary + 1)
            .unwrap_or(0);
        let class = before[start..].trim();
        if !class.is_empty() && !class.contains("::") {
            return Some(class.to_string());
        }
    }
    None
}

/// **A qualified spelling without its namespace** — the name the class is declared under, with any template arguments
/// kept.
///
/// The last `::` **outside angle brackets** is the boundary: `std::basic_string<char>` → `basic_string<char>` (the alias
/// target of `std::string`, which lives in the inline namespace `std::__cxx11`), and `std::vector<LuaSyntaxNode>` →
/// `vector<LuaSyntaxNode>`. Stripping arguments — which an earlier version did by taking `rsplit("::")`'s last piece and
/// then skipping anything with a `<` — lost the substitution and answered the *unparameterized* class.
pub(crate) fn without_the_namespace(class: &str) -> String {
    let mut depth = 0usize;
    let mut boundary = 0usize;
    let bytes = class.as_bytes();
    let mut index = 0usize;
    while index + 1 < bytes.len() {
        match bytes[index] {
            b'<' => depth += 1,
            b'>' => depth = depth.saturating_sub(1),
            b':' if depth == 0 && bytes[index + 1] == b':' => boundary = index + 2,
            _ => {}
        }
        index += 1;
    }
    class[boundary.min(class.len())..].to_string()
}

/// **Is this a template parameter's *name*?** — an identifier, not the keyword that introduced it.
///
/// A fact's `parameters` is a list of names (`["_Ty", "_Alloc"]`), and it used to be able to carry the *keywords*
/// instead — one measured declaration of `std::vector` in MSVC's `<vector>` recorded `["class", "class"]`, because the
/// name was read as "the last word of the parameter" and an **unnamed** parameter's last word is the keyword. That
/// list substitutes nothing and, worse, *won* the choice of which declaration's parameters to use, which is how
/// `std::vector<MarkEvent>::operator[]` answered `_Ty&` after two rounds of fixing the substitution itself.
///
/// The fact is fixed where it is built now (`template_parameters_in` reads the name from the **tokens**, so a keyword
/// cannot become one, and an unnamed parameter is an **empty slot** that keeps the arguments aligned). This remains
/// as the shape check a substitution needs: a list with nothing nameable is a list it cannot use.
pub(crate) fn is_a_parameter_name(name: &str) -> bool {
    !name.is_empty()
        && !matches!(
            name,
            "class" | "typename" | "struct" | "union" | "enum" | "concept"
        )
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
}

/// **Can a substitution be driven by this list?** — at least one name, and every entry either a name or an empty slot.
///
/// The empty entry is a slot rather than a fact: an unnamed parameter (`template <typename, class T>`) cannot be
/// substituted into, because no spelling of a body equals the empty string, and its presence keeps the *later*
/// arguments aligned with their names. So a list that has names beside its slots is exactly a list a substitution can
/// use — and one that is all slots (`template <typename, typename> class vector;`) has nothing to substitute.
pub(crate) fn lists_usable_parameter_names(names: &[String]) -> bool {
    !names.is_empty()
        && names.iter().any(|name| !name.is_empty())
        && names
            .iter()
            .all(|name| name.is_empty() || is_a_parameter_name(name))
}

/// **`element_type` read through the member that names the base** — `using _Mybase = _Ptr_base<_Ty>;` and then
/// `class shared_ptr : public _Mybase`.
///
/// `members_of` walks a base clause, and a base clause that names an **alias** is not a class: measured on a fixture
/// with MSVC's own shape, `members_of("Handle<Item>")` listed `_Mybase: Some("Holder<Item>")`, `element_type` (from the
/// re-exposing `using typename _Mybase::element_type;`, with no type of its own) and `operator->` — and *none* of the
/// base's members. The alias's `type_of` is the base's spelling, already substituted with the use's arguments, so it is
/// the step this walk needs: one level of alias, then the same question.
pub(crate) fn an_element_type_through_the_members(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    parameters: &[String],
    arguments: &[String],
    depth: usize,
) -> Option<String> {
    if depth > 3 {
        return None;
    }
    let Known::Yes(members) = members_of(index, scopes, root, path, class) else {
        if crate::probes::inference() {
            eprintln!("[inference] element_type: no member list for {class}");
        }
        return None;
    };

    // The class's own `element_type`/`value_type`, when the declaration that has the name also has a type.
    for name in ["element_type", "value_type"] {
        let Some(fact) = members
            .members
            .iter()
            .filter(|member| member.fact.name == name)
            .map(|member| &member.fact)
            .find(|fact| fact.type_of.is_some())
        else {
            continue;
        };
        let Some(written) = fact.type_of.as_deref() else {
            continue;
        };
        let substituted = crate::sema::types::substituted_spelling(written, parameters, arguments);
        let target = base_type_name(&substituted).to_string();
        if crate::probes::inference() {
            eprintln!(
                "[inference] element_type: {class}::{name} wrote {written:?} -> {substituted:?} -> {target:?}"
            );
        }
        if !target.is_empty()
            && !parameters.iter().any(|parameter| parameter == &target)
            && target != base_type_name(class)
        {
            return Some(target);
        }
    }

    // A member of kind `Type` whose target names another class — the alias of the base.
    for member in &members.members {
        if member.fact.kind != crate::DeclKind::Type {
            continue;
        }
        let Some(target) = member.fact.type_of.as_deref() else {
            continue;
        };
        if base_type_name(target) == base_type_name(class) {
            continue;
        }
        // **The target is written inside the class's own namespace and carries the class's arguments**: `_Mybase` is
        // `_Ptr_base<_Ty>` in `std`, and the declaration is filed as `std::_Ptr_base`. Asking the bare spelling found
        // nothing on a real project, and asking it *with* arguments found a class with no members — measured, in this
        // order, which is why both parts are removed here and the arguments stay in the substitution.
        let bare_target = without_arguments(target.trim()).to_string();
        let qualified = resolved_in_the_enclosing_scopes(index, scopes, path, class, &bare_target);
        let qualified = resolve_aliases(
            &crate::sema::declarations::ShapeTable::new(),
            index,
            scopes,
            root,
            path,
            &qualified,
        );
        if let Some(found) = an_element_type_through_the_members(
            index,
            scopes,
            root,
            path,
            &qualified,
            parameters,
            arguments,
            depth + 1,
        ) {
            return Some(found);
        }
    }

    // And the declared base clauses, for the ones that do name a class.
    for base in bases_of(index, scopes, root, path, class)
        .value()
        .unwrap_or_default()
    {
        let base = resolved_in_the_enclosing_scopes(index, scopes, path, class, base.trim());
        let base = resolve_aliases(
            &crate::sema::declarations::ShapeTable::new(),
            index,
            scopes,
            root,
            path,
            &base,
        );
        if let Some(found) = an_element_type_through_the_members(
            index,
            scopes,
            root,
            path,
            &base,
            parameters,
            arguments,
            depth + 1,
        ) {
            return Some(found);
        }
    }

    None
}

/// **What the index's own tables cost** — the answer to [`ProjectIndex::table_accounting`].
///
/// The facts live in `summaries` and `cooked` and are counted by [`ProjectIndex::accounting`]; this is the structure
/// around them: the inverted index, the include graph, and the four memos that outlive a request. Every number is an
/// estimate with a stated method — keys, payloads, one container header each, no spare capacity — so the sum is a
/// lower bound, which is the same convention [`crate::MemoryReport`] uses.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TableCost {
    /// The inverted index: two posting maps, the definers, and the sorted spellings.
    pub names: crate::index::names::NameIndexCost,
    /// Include edges across the project — the sum of every file's set of includers.
    pub edges: usize,
    /// The include graph and the module map, in bytes.
    pub edge_bytes: usize,
    /// Cached member lists ([`ProjectIndex::member_lists`]).
    pub member_list_entries: usize,
    /// Members across those lists.
    pub member_lists: usize,
    /// Their estimated bytes.
    pub member_list_bytes: usize,
    /// Guard verdicts remembered ([`ProjectIndex::guard_answers`]).
    pub guard_entries: usize,
    /// Their estimated bytes.
    pub guard_bytes: usize,
    /// Visibility walks remembered, one per root asked.
    pub visibility_entries: usize,
    /// Their estimated bytes.
    pub visibility_bytes: usize,
    /// Conditional-include answers remembered.
    pub condition_entries: usize,
    /// Their estimated bytes.
    pub condition_bytes: usize,
    /// The file paths held twice, by order and by identity.
    pub path_bytes: usize,
}

impl TableCost {
    /// The whole of it, in bytes.
    pub fn total(&self) -> usize {
        self.names.bytes
            + self.edge_bytes
            + self.member_list_bytes
            + self.guard_bytes
            + self.visibility_bytes
            + self.condition_bytes
            + self.path_bytes
    }
}
/// **Does the callee name `std::define_static_array`?** — the C++26 facility whose return type the standard fixes.
///
/// The **qualified** spelling only, for the reason [`names_the_identity_function`] gives: an unqualified
/// `define_static_array(x)` may be a project`s own function, and treating it as the standard`s would be a guess about a
/// name this layer can see declared elsewhere. The standard`s spelling is `std::define_static_array`, and that is what a
/// file writes.
pub(crate) fn names_define_static_array(callee: &cpp_parser::CppSyntaxNode) -> bool {
    let spelled = callee.text().to_string();
    let spelled = spelled.trim();
    let bare = match spelled.find('<') {
        Some(at) => spelled[..at].trim(),
        None => spelled,
    };
    matches!(
        bare,
        "std::define_static_array" | "::std::define_static_array"
    )
}
/// **The return type a function's body gives back** — for the functions whose summary could not answer.
///
/// A summary is built *without the index*, so the `return` expressions it can type are the ones a spelling answers on
/// its own: a literal, a name, a binary expression. `return make_thing();` and `return value.member;` are the two
/// shapes it leaves at `None`, and they are how a great deal of C++14-and-later code writes a deduced return — measured
/// on four shapes: `auto ident(T x) { return x; }` and `auto twice(T x) { return x + x; }` got a return type from the
/// summary, while `auto via(int x) { return g(x); }` and `auto field(W w) { return w.n; }` had none.
///
/// # The two readings, and why the offsets decide which
///
/// The **current file** is the one the caller already has in hand — `root`, `scopes` and `path` are its own, and a
/// declaration inside it is found by the fact's offset directly. The resolver (`declaring`) is for a declaration
/// **another** file holds, and the view it answers may be that file's *rendering* (a cook), whose offsets are not the
/// fact's: a fact records the file's coordinates, so the offset is mapped through [`crate::FileView::reading_offset_of`]
/// first — and `None` there means this layer cannot tell where the declaration sits, which is a refusal rather than a
/// tree read at the wrong place.
///
/// Every `return` must agree, which is what the language requires of a function whose return type is `auto`; two
/// returns that do not agree is a program that does not compile, and this refuses rather than picking one. The spelling
/// is the declaring file's, template parameters and all: the caller substitutes the call's own arguments into it, the
/// same substitution a written return type goes through.
pub(crate) fn the_return_the_body_gives_back(
    memo: &TypeMemo,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    depth: usize,
    candidate: &ProjectDefinition,
) -> Option<String> {
    // **The recursion bound**: typing the body's `return` can ask the same question about another function, and two
    // functions that return each other's calls are a cycle. Past the bound the answer is `None`, which is the answer
    // this layer gives everything it cannot read.
    if depth > MAX_TYPE_DEPTH {
        return None;
    }

    // **Only the file the question is about.** Reading another file's body means parsing that file, and this runs
    // inside a per-expression query — measured on neovim's first twenty-five files, taking the cross-file half out
    // took the same sample from **75.6 s to 26.7 s**, the number it had before the deduction existed. A `return` this
    // layer reads is therefore one in the file the cursor is in; a wrapper in a header stays unknown, which is the
    // answer it gave before.
    if candidate.file != path {
        return None;
    }
    let the_root = root;
    let the_scopes = scopes;
    let the_path = path;
    let in_the_reading = candidate.fact.name_range.start_offset;

    let token = cpp_parser::token_at(the_root, in_the_reading)?;
    let declaration = token.parent_ancestors().find(|node| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::Declaration
    })?;
    use cpp_parser::CppAstNode;
    let body = cpp_parser::CppDeclaration::cast(declaration)?.get_body()?;

    let mut agreed: Option<String> = None;
    for statement in body
        .syntax()
        .descendants()
        .filter_map(cpp_parser::CppReturnStat::cast)
    {
        // **A `return` in a lambda inside the body is the lambda's**, not this function's — the same distinction
        // [`crate::sema::declarations`]'s `the_returns_of` makes. The lambda's own returns would otherwise be read as
        // this function's, and `auto f() { return [](int x) { return x; }; }` would claim to return `int`.
        if statement
            .syntax()
            .ancestors()
            .take_while(|ancestor| ancestor != body.syntax())
            .any(|ancestor| {
                cpp_parser::CppSyntaxKind::from(ancestor.kind())
                    == cpp_parser::CppSyntaxKind::LambdaExpr
            })
        {
            continue;
        }
        let Some(expression) = statement.get_expr() else {
            // **A bare `return;` is `void`** — [dcl.spec.auto] says so, and a function whose body ends that way has no
            // other answer to give. A body with both a bare return and a value return is not a program, and the
            // agreement below is what refuses it.
            match &agreed {
                None => agreed = Some("void".to_string()),
                Some(previous) if previous == "void" => {}
                Some(_) => return None,
            }
            continue;
        };
        let Known::Yes((type_of, _)) = type_of_expression_memoed(
            memo,
            &crate::sema::declarations::ShapeTable::new(),
            index,
            declaring,
            the_scopes,
            the_root,
            the_path,
            expression.syntax(),
            depth + 1,
        ) else {
            return None;
        };
        let written = type_of.to_string();
        match &agreed {
            None => agreed = Some(written),
            Some(previous) if *previous == written => {}
            Some(_) => return None,
        }
    }
    agreed
}
