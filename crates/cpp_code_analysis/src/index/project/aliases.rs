//! **Aliases** — a spelling resolved through `using`/`typedef`, and a member found through the spelling.
//!
//! Moved here from `project/mod.rs` without a change. A chain is followed at most `MAX_ALIAS_HOPS` times, because a
//! corpus can write `using A = A;` and the honest answer to a reader is a refusal rather than a loop.

use super::*;
use crate::DeclKind;
use crate::ScopeTree;
use crate::sema;

/// **How many aliases one spelling may be followed through** — see [`a_spelling_through_the_aliases`].
///
/// Every step of the walk costs one hop, and a *syntactic* step (resolving the qualifier of a qualified name) costs
/// one as well, so the budget is spent much faster than the number of aliases suggests. Six was the first number and
/// it is past anything a fixture writes — but not past what libstdc++ writes for a container's element type:
///
/// ```text
/// vector::reference             _Alloc_traits::reference
///   _Alloc_traits               __gnu_cxx::__alloc_traits<_Tp_alloc_type>      a qualifier
///   __alloc_traits::reference   value_type&
///     value_type                _Base_type::value_type
///       _Base_type              std::allocator_traits<_Tp_alloc_type>           a qualifier
///         value_type            _Alloc::value_type
///           _Alloc              _Tp_alloc_type
///             _Tp_alloc_type    _Base::_Tp_alloc_type
///               _Base           _Vector_base<_Tp, _Alloc>                     a qualifier
///                 _Tp_alloc_type  __alloc_traits<_Alloc>::template rebind<_Tp>::other
///                   rebind<_Tp>   a nested class                                 a qualifier
///                     other       _Base_type::template rebind_alloc<_Tp>
/// ```
///
/// — thirteen before `allocator<_Tp1>` is even named. Measured on the allocator fixture with the old budget: the walk
/// stopped with half the chain unsubstituted (`Alloc>::template rebind<Tp>::other`), which reads like a substitution
/// bug and is a budget one. Twenty-four is twice what the standard library needs and still a number that stops a
/// cyclic `typedef` from looping.
pub(crate) const MAX_ALIAS_HOPS: usize = 24;

/// **How many times one spelling's aliases are expanded in a row** — see the loop in [substituted_members].
///
/// Four is more than any library chain this project has met (const_pointer → value_type → _CharT is two), and
/// the bound is a stop for an alias that names itself rather than a budget a real class is expected to need.
///
/// [substituted_members]: crate::index::project::substituted_members
pub(crate) const MAX_ALIAS_PASSES: usize = 4;

/// **A spelling with the aliases a class declares followed through** — `pointer` → `_Val*`, and
/// `_Alloc_traits::reference` → `std::size_t&`.
///
/// # Why this is a step of its own
///
/// A member's recorded type is a **name**: `typedef _Val* pointer;` records `_Val*`, while the use writes `pointer`,
/// and the two are the same type only once the class's parameters are paired with the use's arguments — after which
/// the answer is often *another* name (`_Alloc_traits::reference`) that has to be resolved in the same class again.
/// The layer used to answer the name it found, and every consumer downstream then treated a spelling as a type:
///
/// * `map::find(text)->second` — `_Rb_tree_iterator<_Val>::operator->` answers `pointer`, and the answer was used as
///   the class to look `second` up in: `Unknown(NotDeclaredHere("std::_Rb_tree_iterator::second"))`;
/// * `auto lineStartOffset = _lineOffsetVec[line]` — `vector::reference` is `_Alloc_traits::reference`, and the
///   `auto` variable was typed by that alias rather than by `std::size_t&`.
///
/// # The two shapes, and why `::` is the whole of the rule
///
/// ```text
/// pointer                 a member of this class            ask this class for it
/// _Alloc_traits::reference names `reference` in `_Alloc_traits`, which is a member of this class (an alias, a
///                         base's alias, a nested class)     resolve the qualifier here, then ask *it* for `reference`
/// ```
///
/// `depth` counts the hops, and a hop that would answer the spelling it was given is refused — the honest way to stop
/// on a `typedef` that names itself.
///
/// # `visited`: the classes the walk came through, and the one thing a name can be resolved by
///
/// A library declares the same alias name in many classes — libstdc++ has `_Tp_alloc_type` in `vector`,
/// `_Vector_base`, `_Bvector_base` and `_Deque_base` — and an unqualified name that the current class does not declare
/// has no business being looked up by bare name project-wide. The class that **wrote** it is the answer, and it is the
/// class the walk has just left: `__alloc_traits<_Tp_alloc_type>::reference` is `value_type&`, whose `value_type` goes
/// to `allocator_traits<_Tp_alloc_type>::value_type`, whose `_Alloc::value_type` leaves `_Tp_alloc_type` standing —
/// written by `vector`, three classes back. Measured on `Mark.cpp` before this: the bare-name lookup took
/// `_Deque_base`'s alias, so `events[Pos]` answered `_Tp_alloc_type&` and everything read through it was unknown.
///
/// `visited` is `[outermost, …, innermost]`; the current `class` is the innermost of all and is asked first, so a
/// member of the current class always wins over a same-named alias three classes back.
pub(crate) fn a_spelling_through_the_aliases(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &Type,
    visited: &[Type],
    written: &str,
    depth: usize,
) -> String {
    a_spelling_through_the_aliases_in(
        index,
        scopes,
        root,
        path,
        class,
        visited,
        &[],
        written,
        depth,
    )
}

/// **A spelling substituted with every pairing in scope** — the environment (the classes the walk came through,
/// outermost first), then this class's own.
///
/// One substitution is not enough, and that is the whole difficulty: a type written in a nested class names **both**
/// its own parameters (`rebind<_Tp>::other` mentions `_Tp`) and the enclosing class's (`typename _Base_type::template
/// rebind_alloc<_Tp>` mentions `_Base_type`, which is `alloc_traits`'s alias). Substituting only one of the two leaves
/// a name nothing can resolve — measured on the allocator fixture, the answer was
/// `Alloc>::template rebind<Tp>::other`, which reads like a substitution bug and is a *scope* bug.
pub(crate) fn substituted_through(
    spelling: &str,
    parameters: &[String],
    defaults: &[String],
    arguments: &[String],
    environment: &[(String, String)],
) -> String {
    let mut result = spelling.to_string();
    for (name, value) in environment {
        if name.is_empty() || value.is_empty() {
            continue;
        }
        result = sema::types::substituted_spelling(
            &result,
            std::slice::from_ref(name),
            std::slice::from_ref(value),
        );
    }
    sema::types::substituted_spelling_with_defaults(&result, parameters, defaults, arguments)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn a_spelling_through_the_aliases_in(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &Type,
    visited: &[Type],
    environment: &[(String, String)],
    written: &str,
    depth: usize,
) -> String {
    if depth == 0 {
        return written.to_string();
    }
    let (leading, core, trailing) = split_the_operators_of(written);
    let a_name = !core.is_empty()
        && core.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '_' | ':' | '<' | '>' | ',' | ' ')
        });
    if !a_name {
        return written.to_string();
    }
    let Some(class_name) = class.class_name() else {
        return written.to_string();
    };
    let arguments: Vec<String> = class
        .arguments()
        .iter()
        .map(|one| one.to_string())
        .collect();
    let parameters = index.template_parameters_for(class_name, path, &arguments);
    let defaults = index.template_parameter_defaults_for(class_name, path, &arguments);
    // **Every pairing in scope from here on** — the classes the walk came through, and this one. Every substitution
    // below reads it, and every descent passes it on: see [`substituted_through`].
    let mut here = environment.to_vec();
    for (name, value) in parameters.iter().zip(arguments.iter()) {
        if !name.is_empty() && !value.is_empty() {
            here.push((name.clone(), value.clone()));
        }
    }

    // **An unqualified name is a member of this class — or of a class the walk came through.**
    //
    // The order is the rule: the current class first (a member of this class always wins), then the classes the walk
    // has left, innermost first, and only then a bare-name lookup in the whole index — which is the guess that took
    // `_Deque_base`'s `_Tp_alloc_type` for `vector`'s.
    let Some((qualifier, last)) = split_at_the_last_scope(&core) else {
        let mut found = None;
        for candidate in std::iter::once(class).chain(visited.iter().rev()) {
            if let Some(fact) = a_member_the_class_declares(
                index,
                scopes,
                root,
                path,
                &candidate.to_string(),
                &core,
            )
            // **A declaration with no type is not an answer here.** A re-exposed alias declares none of its own
            // (`using typename _Base::value_type;`), and stopping at it would answer nothing where the class it
            // came from had the type: measured, `vector`'s own `tp_alloc_type` is `typename base::tp_alloc_type`
            // with no type recorded, and taking it ended the chain at `Alloc>::template rebind<Tp>::other`.
            && fact.type_of.is_some()
            {
                found = Some((candidate.clone(), fact));
                break;
            }
        }
        // **An alias template declared elsewhere**, which the classes above do not declare. Following it produces a
        // *qualified* spelling, which the branch below resolves: the class selects the specialization its arguments
        // name, and the member is read there.
        if found.is_none()
            && let Some((named, member)) = the_alias_behind(index, path, &core)
        {
            let qualified = format!("{named}::{member}");
            if qualified != core {
                let followed = a_spelling_through_the_aliases(
                    index,
                    scopes,
                    root,
                    path,
                    class,
                    visited,
                    &qualified,
                    depth.saturating_sub(1),
                );
                if followed != core {
                    return format!("{leading}{followed}{trailing}");
                }
            }
        }

        let Some((declared_in, fact)) = found else {
            return written.to_string();
        };
        let Some(target) = fact.type_of.as_deref() else {
            return written.to_string();
        };
        // **The substitution is everything in scope**, which is what makes a name three classes back meaningful:
        // `vector`'s `_Tp_alloc_type` is `_Base::_Tp_alloc_type`, and `_Base` is its parameter — while the `Alloc` a
        // nested class writes was paired two levels up. See [`substituted_through`].
        let target = substituted_through(target, &parameters, &defaults, &arguments, environment);
        if target == core {
            return written.to_string();
        }
        // **The walk continues in the class that declared the name**, not in the one it came from: the target's own
        // names are written in *that* class's scope. The class it came from stays on the visited list, so a name that
        // the declaring class does not have can still be found three classes back.
        let mut deeper = visited.to_vec();
        if declared_in.to_string() != class.to_string() {
            deeper.push(class.clone());
        }
        let followed = a_spelling_through_the_aliases_in(
            index,
            scopes,
            root,
            path,
            &declared_in,
            &deeper,
            &here,
            &target,
            depth - 1,
        );
        if followed != core {
            return format!("{leading}{followed}{trailing}");
        }
        return written.to_string();
    };

    // **A qualified name is a member of whatever the qualifier names** — and the qualifier is itself resolved here,
    // because the library writes it without its namespace (`_Alloc_traits` inside `std`), and **substituted first**,
    // because the qualifier is often a template parameter of the class the member came from:
    // `allocator_traits<_Alloc>` declares `reference` as `typename _Alloc::value_type&`, so `_Alloc` has to become
    // `std::allocator<std::size_t>` before `value_type` can be looked up. Measured on `lineStartOffset`: without
    // this line the chain stops at `_Alloc::value_type&`, with it at `std::size_t&` — the difference between a
    // spelling no reader can use and the type they asked for.
    let qualifier = {
        let substituted =
            substituted_through(&qualifier, &parameters, &defaults, &arguments, environment);
        if substituted.trim().is_empty() {
            qualifier
        } else {
            substituted
        }
    };
    let inner = a_spelling_through_the_aliases_in(
        index,
        scopes,
        root,
        path,
        class,
        visited,
        &here,
        &qualifier,
        depth - 1,
    );
    // **A qualifier that did not resolve is where this stops.** Carrying on with the spelling as if it were a class
    // asks the index for a *bare* name, and the library declares `_Alloc_traits` in a dozen classes: measured, the
    // chain silently continued in `std::basic_string` and answered a type from another class. Stopping answers the
    // last thing that *was* resolved, which is the honest place to hand over.
    // **A qualifier that did not resolve *and is not a class* is where this stops.** The guard used to stop whenever the
    // recursion came back unchanged, which also stopped at a qualifier that was already the answer —
    // `conditional<false, void, T>`, a class template applied to arguments, whose `type` lives in a *specialization*
    // (measured: the answer was the class `conditional` with one member listed, `type`).
    if inner.trim() == qualifier.trim()
        && !matches!(members_of(index, scopes, root, path, &inner), Known::Yes(_))
    {
        return written.to_string();
    }
    // **A name qualified by a *namespace* is not followed.** The walk exists to make a *member* usable: `pointer` in
    // a class is `_Val*`, and `_Alloc_traits::reference` is `std::size_t&` — a spelling no reader wrote. A
    // namespace-qualified name is the opposite case: `std::string` is already the name the source wrote, and
    // replacing it with `basic_string<char, char_traits<char>, allocator<char>>` is what an editor drew as the type
    // of `auto formattedText = f.GetFormatResult(t);` (reported from `EmmyLuaCodeStyle`'s `LuaCodeFormat.cpp:131`,
    // and reproduced over stdio). The test is the index's own kind: a fact named `inner` that is a namespace names a
    // *scope*, not a class in the walk.
    if names_a_namespace(index, path, &inner) {
        return written.to_string();
    }
    let inner_type = parse_type_spelling(&inner);
    let Some(inner_name) = inner_type.class_name() else {
        return written.to_string();
    };
    // **The arguments split where the language splits them.** `split_arguments` counts brackets
    // *and* parentheses, the same rule the pattern comparison uses.
    let inner_arguments: Vec<String> = sema::builtins::split_arguments(
        inner
            .split_once('<')
            .map(|(_, rest)| rest.trim_end().strip_suffix('>').unwrap_or(rest))
            .unwrap_or_default(),
    );
    // **The class's parameters are filed under its qualified name.**
    //
    // A spelling written where the class is in scope is usually the short form — `base<A>` inside `mygnu`, never
    // `mygnu::base<A>` — while the index keys a class's parameter list by the qualified one, so the lookup is made
    // with the name resolved. Measured on the allocator fixture, `base::tp_alloc_type` answered
    // `typename alloc_traits<Alloc>::template rebind<Tp>::other` — a spelling no reader can use, three levels into
    // the chain — and a class whose parameters cannot be found is a class whose members cannot be substituted.
    let inner_class = {
        let qualified = resolve_aliases(
            &sema::declarations::ShapeTable::new(),
            index,
            scopes,
            root,
            path,
            inner_name,
        );
        if qualified.is_empty() || qualified == inner_name {
            inner_name.to_string()
        } else {
            qualified
        }
    };
    let inner_parameters = index.template_parameters_for(&inner_class, path, &inner_arguments);
    let inner_defaults =
        index.template_parameter_defaults_for(&inner_class, path, &inner_arguments);
    // **The member's own name, and the arguments written on *it*** — `A::template rebind<U>::other` names `other`, and
    // the class whose scope it lives in is `A`'s nested `rebind<U>`. The arguments belong to the member and not to the
    // class the spelling qualified with, which is the whole reason `the_member_name` alone was not enough.
    let (member_name, member_arguments) = the_name_and_its_arguments(&last);
    // **The class as the use wrote it** — with its arguments — because that is what selects a
    // partial specialization and what lets the qualifier's own aliases (`_Mybase =
    // _Tree_val<_Traits>`) be substituted.
    let found = a_member_the_class_declares(index, scopes, root, path, &inner, member_name);

    let Some(fact) = found else {
        return written.to_string();
    };

    // **A class is not an alias.** `A::rebind<U>::other` asks for a member of `rebind<U>`, and `rebind` is a **class**
    // of `A` whose own parameters pair with the `<U>` written here. The answer for this level is therefore the nested
    // class's spelling *with its arguments* — which is the scope the next level (`other`) is resolved in, and what
    // `direct_member` reads its arguments from when that next level is asked.
    //
    // The spelling names the **scope** of the nested class (`alloc_traits::rebind<int>`), not the enclosing class with
    // its own arguments: a class spelling cannot carry two argument lists (`direct_member` reads the one at the end,
    // and anything earlier it takes for part of the name). What the enclosing level's arguments mean is carried in the
    // environment instead, which is the reason the walk has one — `other` mentions `Alloc`, which is two levels up.
    //
    // The test is `type_of`, the one the whole module uses to tell the two apart: a class declares no type of its own,
    // and an **alias template** (`using rebind_alloc = typename Alloc::template rebind<Up>::other;`) does — it is a
    // name for a type, and its `<U>` pairs with its own parameter. Reading it as a nested class is what made the walk
    // ask for `value_type` inside `allocator_traits::rebind_alloc<int>`, a class that does not exist.
    //
    // Measured on `Mark.cpp`: without this, the walk stopped at `__gnu_cxx::__alloc_traits<…>::template
    // rebind<MarkEvent>::other` — the spelling the report calls "极其夸张" — because `rebind<MarkEvent>` was never a
    // name any lookup was asked about.
    if fact.kind == DeclKind::Type && fact.type_of.is_none() && !member_arguments.is_empty() {
        let resolved_arguments: Vec<String> = member_arguments
            .iter()
            .map(|argument| {
                substituted_through(argument, &parameters, &defaults, &arguments, environment)
            })
            .collect();
        let nested = format!(
            "{}::{}<{}>",
            a_class_name_without_spaces(&inner_class),
            member_name,
            resolved_arguments.join(", ")
        );
        return format!("{leading}{nested}{trailing}");
    }

    let Some(target) = fact.type_of.as_deref() else {
        return written.to_string();
    };
    // The pattern's slots are what say which written argument each parameter takes.
    let (pattern_parameters, pattern_arguments) = fact
        .pattern()
        .map(|pattern| the_pairing_its_pattern_implies(pattern, &inner_arguments))
        .filter(|(parameters, _)| !parameters.is_empty())
        .unwrap_or_else(|| (inner_parameters.clone(), inner_arguments.clone()));
    // **The class's pairing first, then the member's own.** A member that is itself a template — an alias template's
    // `using rebind_alloc = typename Alloc::template rebind<Up>::other;` — mentions **both** its class's parameter
    // (`Alloc`) and its own (`Up`), and the two are paired with different arguments: the class's with what the use
    // wrote on the class, the member's with what it wrote on the member (`rebind_alloc<MarkEvent>`). Substituting one
    // and not the other leaves a spelling with a name nothing can resolve — measured on the allocator fixture, the
    // chain ended at `Alloc>::template rebind<Tp>::other`. The environment covers a third: a name the *enclosing*
    // class of a nested one wrote.
    let target = substituted_through(
        target,
        &pattern_parameters,
        &inner_defaults,
        &pattern_arguments,
        environment,
    );
    let target = if member_arguments.is_empty() {
        target
    } else {
        sema::types::substituted_spelling_with_defaults(
            &target,
            fact.parameters(),
            fact.parameter_defaults(),
            &member_arguments,
        )
    };
    // **The walk descends into the qualifier's class**, so the class it is leaving joins the visited list — that is
    // what lets `_Tp_alloc_type`, written three classes back, still be found by the class that wrote it.
    let mut deeper = visited.to_vec();
    if inner != class.to_string() {
        deeper.push(class.clone());
    }
    let followed = a_spelling_through_the_aliases_in(
        index,
        scopes,
        root,
        path,
        &inner_type,
        &deeper,
        &here,
        &target,
        depth - 1,
    );
    format!("{leading}{followed}{trailing}")
}

/// **A member this class (or one of its bases) declares, with no arrow fallback.**
///
/// `member_fact` ends by asking about an `operator->` when a name is not found — right for a member query, wrong for the
/// alias walk, which the arrow path itself calls: `pointer` → not found → `operator->` → this walk → `pointer` … is a
/// cycle, and it was measured as a stack overflow in the fixture that pins the arrow's element type. `members_of` walks
/// the bases (which is what `_Ptr_base`'s aliases need) and never asks about an arrow.
///
/// The declaration preferred is the one that *has* a type: a re-exposed alias (`using typename _Base::value_type;`)
/// declares none of its own, and answering that one would be answering nothing.
pub(crate) fn a_member_the_class_declares(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    name: &str,
) -> Option<DeclFact> {
    // **First the class's own member list**, which is what walks the bases (`_Ptr_base`'s `element_type` is one) and
    // which substitutes the class's parameters. A re-exposed alias declares no type of its own, so the declaration
    // answered is the one that *has* one.
    let listed = members_of(index, scopes, root, path, class);
    let name = the_member_name(name);

    // **A member of a specialization that does not match this use is not a member of this use.**
    //
    // `std::allocator_traits<allocator<T>>::value_type` had two answers in the index: the primary template's
    // (`typename _Alloc::value_type`) and the `pmr::polymorphic_allocator` specialization's (`_Tp`), and this loop
    // took the first one with a type — measured on `Mark.cpp`, that substituted `_Tp` with `_Tp_alloc_type` and the
    // whole alias chain ended at `_Tp_alloc_type&`. The pattern is what says which specialization declared a member,
    // so it is asked here the way `direct_member` asks it.
    let arguments: Vec<String> = class
        .split_once('<')
        .map(|(_, rest)| {
            sema::builtins::split_arguments(rest.trim_end().strip_suffix('>').unwrap_or(rest))
        })
        .unwrap_or_default();
    let parameters = index.template_parameters_for(class, path, &arguments);
    if let Some(found) = listed.value().and_then(|members| {
        members
            .members
            .iter()
            .filter(|member| member.fact.name == name)
            .filter(|member| {
                a_pattern_matches(member.fact.pattern(), &parameters, Some(&arguments))
            })
            .map(|member| member.fact.clone())
            .find(|fact| fact.type_of.is_some())
    }) {
        return Some(found);
    }

    // **Then the declaration this *use* is an instance of.** A class template's partial specializations share their
    // primary's scope, and `conditional<false, T, F>::type` is a member of the *specialization*: the member list above
    // is built from the primary, which has no `type` at all. `direct_member` is the function with that rule — the
    // pattern comparison, with the arguments read from the spelling — and it is the one lookup that does **not** end by
    // asking about an `operator->`, which is what keeps this walk out of the arrow's cycle.
    direct_member(
        &sema::declarations::ShapeTable::new(),
        index,
        scopes,
        root,
        path,
        class,
        name,
        None,
    )
    .map(|(fact, _)| fact)
}
/// **The (parameters, arguments) pairing a declaration's `pattern` implies.**
///
/// A partial specialization's pattern names its parameters in *its* slots — `conditional<false, T, F>` has two
/// parameters and three slots — so the use's arguments do not pair with them by position. Each slot that is a *name*
/// binds the parameter of that name to the written argument in the same place; a literal slot (`false`, `true`) binds
/// nothing, which is what makes the specialization apply in the first place.
pub(crate) fn the_pairing_its_pattern_implies(
    pattern: &str,
    written: &[String],
) -> (Vec<String>, Vec<String>) {
    let slots = arguments_in_a_spelling(pattern).unwrap_or_default();
    let mut parameters = Vec::new();
    let mut arguments = Vec::new();
    for (at, slot) in slots.iter().enumerate() {
        let slot = slot.trim();
        let Some(argument) = written.get(at) else {
            continue;
        };
        let a_name = !slot.is_empty()
            && slot
                .chars()
                .all(|character| character.is_alphanumeric() || character == '_')
            && !matches!(slot, "true" | "false");
        if a_name {
            parameters.push(slot.to_string());
            arguments.push(argument.clone());
        }
    }
    (parameters, arguments)
}
/// `A::b` → (`A`, `b`), split at the last `::` **outside angle brackets** — the same boundary
/// [`without_the_namespace`] finds from the other side.
pub(crate) fn split_at_the_last_scope(spelling: &str) -> Option<(String, String)> {
    let bytes = spelling.as_bytes();
    let mut depth = 0usize;
    let mut boundary = None;
    let mut at = 0usize;
    while at + 1 < bytes.len() {
        match bytes[at] {
            b'<' => depth += 1,
            b'>' => depth = depth.saturating_sub(1),
            b':' if depth == 0 && bytes[at + 1] == b':' => {
                boundary = Some(at);
                at += 1;
            }
            _ => {}
        }
        at += 1;
    }

    let boundary = boundary?;
    let qualifier = spelling[..boundary].trim().to_string();
    let last = spelling[boundary + 2..].trim().to_string();
    (!qualifier.is_empty() && !last.is_empty()).then_some((qualifier, last))
}

/// **A member's name with the keywords that may precede it removed** — `template rebind<int>` → `rebind<int>`.
///
/// `A::template rebind<U>::other` writes `template` before a **name**, not before a type: in a dependent context it
/// tells the parser that `rebind<U>` is a template, and it is not part of the member's spelling. A lookup asked for a
/// member literally called `template rebind<U>` finds nothing — measured on `std::vector`'s `reference`, the walk
/// stopped there and answered `__gnu_cxx::__alloc_traits<…>::template rebind<…>::other&`, which is the spelling the
/// report calls "极其夸张". `typename` reaches the same position through a member chain
/// (`typename A::template B<C>::type`), so the two are stripped together.
pub(crate) fn the_member_name(spelling: &str) -> &str {
    let mut rest = spelling.trim();
    loop {
        let Some((keyword, after)) = ["template", "typename", "class", "struct", "enum"]
            .iter()
            .find_map(|keyword| rest.strip_prefix(keyword).map(|after| (*keyword, after)))
        else {
            return rest;
        };
        // A keyword, not a name that merely starts with one: `template<class T>` has no whitespace, and a member
        // called `classy` must keep its spelling.
        if !after.starts_with(char::is_whitespace) && !after.starts_with('<') {
            return rest;
        }
        let _ = keyword;
        rest = after.trim_start();
    }
}

/// **A member's name and the template arguments written on it** — `rebind<MarkEvent>` → (`rebind`, `[MarkEvent]`).
///
/// A name in a qualified spelling can be a *class* or an alias **template** applied to arguments, and both are
/// different questions from the class the qualifier named: `__alloc_traits<A>::rebind<T>::other` names `other` in
/// `__alloc_traits<A>`'s nested `rebind<T>`, and `allocator_traits<A>::rebind_alloc<T>` is an alias template whose own
/// parameter pairs with `T`. `the_member_name` strips the keywords; this is what is left once the arguments are off.
///
/// The arguments are split by [`crate::sema::builtins::split_arguments`], the same rule the pattern comparison and
/// the member lookups use, so `pair<int, int>` is one argument here as it is there.
pub(crate) fn the_name_and_its_arguments(spelling: &str) -> (&str, Vec<String>) {
    let name = the_member_name(spelling);
    let Some((head, rest)) = name.split_once('<') else {
        return (name, Vec::new());
    };
    let inside = rest.trim_end().strip_suffix('>').unwrap_or(rest);
    let arguments = sema::builtins::split_arguments(inside);
    (head.trim(), arguments)
}

/// `const reference&` → (`const `, `reference`, `&`). The middle is what a name lookup is asked about.
pub(crate) fn split_the_operators_of(spelling: &str) -> (String, String, String) {
    let mut leading = String::new();
    let mut rest = spelling.trim();
    for qualifier in ["const", "volatile", "constexpr", "static"] {
        if let Some(after) = rest.strip_prefix(qualifier)
            && after.starts_with(char::is_whitespace)
        {
            leading.push_str(qualifier);
            leading.push(' ');
            rest = after.trim_start();
        }
    }
    // **The elaborated-type keywords are dropped rather than kept**, because they are not part of the type: a lookup
    // asked for a member literally called `typename _Alloc_traits` finds nothing, and the chain then guessed a class
    // by *bare* name and silently followed the wrong one — `std::basic_string`'s `_Alloc_traits` instead of
    // `std::vector`'s, measured on `lineStartOffset`. They are not qualifiers either (`typename T&` and `T&` are the
    // same type), so they belong in neither half of the answer.
    loop {
        let trimmed = rest.trim_start();
        let Some(after) = ["typename", "class", "struct", "union", "enum"]
            .iter()
            .find_map(|keyword| trimmed.strip_prefix(keyword))
        else {
            break;
        };
        if !after.starts_with(char::is_whitespace) {
            break;
        }
        rest = after.trim_start();
    }
    let trimmed = rest.trim_end();
    let mut trailing = String::new();
    let mut core = trimmed;
    for _ in 0..4 {
        if let Some(before) = core
            .strip_suffix("&&")
            .or_else(|| core.strip_suffix('&'))
            .or_else(|| core.strip_suffix('*'))
        {
            let operator = &core[before.len()..];
            trailing.insert_str(0, operator);
            core = before.trim_end();
            continue;
        }
        break;
    }
    (leading, core.to_string(), trailing)
}

pub(crate) fn through_an_operator_arrow(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    member: &str,
    arguments: Option<&[String]>,
) -> Option<(DeclFact, PathBuf)> {
    // **The class without its arguments**, because that is the name its members are filed under, and the arguments
    // are what the arrow's return type is substituted with.
    let target = the_class_an_arrow_points_at(index, scopes, root, path, class, arguments)?;
    direct_member(
        &sema::declarations::ShapeTable::new(),
        index,
        scopes,
        root,
        path,
        &target,
        member,
        None,
    )
}

/// **The class an object's `operator->` reaches**, or `None` when it declares none — or when its answer is a
/// template parameter this call cannot substitute.
pub(crate) fn the_class_an_arrow_points_at(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    arguments: Option<&[String]>,
) -> Option<String> {
    let bare = without_arguments(class).to_string();
    let arguments: Vec<String> = arguments
        .map(<[String]>::to_vec)
        .or_else(|| {
            class.split_once('<').map(|(_, rest)| {
                sema::builtins::split_arguments(rest.trim_end().strip_suffix('>').unwrap_or(rest))
            })
        })
        .unwrap_or_default();
    // **The arguments are handed to the lookup**, so that a partial specialization whose pattern does not match is not
    // the declaration `operator->` is read from — the same rule `direct_member`'s members already follow.
    let (arrow, _) = direct_member(
        &sema::declarations::ShapeTable::new(),
        index,
        scopes,
        root,
        path,
        &bare,
        "operator->",
        Some(&arguments),
    )?;
    // **Before `arrow` is partially moved below** — its parameters say whether an answer is one of them.
    let arrows_parameters = arrow.parameters().to_vec();
    let returns = arrow.returns?;

    if arguments.is_empty() {
        return None;
    }
    let parameters = index.template_parameters_for(&bare, path, &arguments);
    let defaults = index.template_parameter_defaults_for(&bare, path, &arguments);
    // **The arrow's return is a *name* in the class that declares it**, and it has to be followed
    // before it can be a class: `_Tree_iterator::operator->` answers `pointer`, which the
    // iterator's own `typedef _Val* pointer;` makes a `pair<const K, V>*`. The step is the same one
    // a member's recorded type goes through ([`a_spelling_through_the_aliases`]), applied here
    // because this is the second place a *spelling* is about to be used as a type.
    let class_type = parse_type_spelling(class);
    let resolved = a_spelling_through_the_aliases(
        index,
        scopes,
        root,
        path,
        &class_type,
        &[],
        &returns,
        MAX_ALIAS_HOPS,
    );
    let substituted = sema::types::substituted_spelling_with_defaults(
        &resolved,
        &parameters,
        &defaults,
        &arguments,
    );
    let mut target = base_type_name(&substituted).to_string();

    // **The member may be a template of its own**, and then its answer names *its* parameter, not the class's:
    //
    // ```cpp
    // template <class _Ty2 = _Ty, enable_if_t<!is_array_v<_Ty2>, int> = 0>
    // _NODISCARD _Ty2* operator->() const noexcept;
    // ```
    //
    // is how MSVC's `<memory>` writes `shared_ptr::operator->`. A fact records a template's parameter *names* and not
    // their defaults, so `_Ty2` cannot be substituted from here — and asking a member lookup about a class called
    // `_Ty2` is what an editor saw as "`file->` has no members" (`class "_Ty2": 0 member(s)`, measured on
    // EmmyLuaCodeStyle's `LuaFormat.cpp`).
    //
    // What settles it is the convention every pointer-like type in the standard library follows: `element_type` is
    // what the arrow reaches, and it is declared on the class or one of its bases.
    // **When the answer is a name nothing declares, it is a parameter** — the member's own, or the class's own written
    // somewhere the substitution could not reach. `_Ty2` is declared nowhere in a project, and a class called `_Ty2` is
    // a class no reader wrote.
    let names_nothing =
        index.files_declaring(&target, path).is_empty() && !is_a_builtin_name(&target);
    if target.is_empty()
        || target == bare
        || names_nothing
        || arrows_parameters.iter().any(|name| name == &target)
    {
        // **An answer only counts when it names a class the index has.** The walk can arrive at a spelling that is a
        // *trait* rather than a class — `remove_extent_t` after `remove_extent_t<_Ty>` is substituted, an alias template
        // whose target the standard library defines through `typename remove_extent<T>::type` — and asking a member
        // lookup about it answers "no members" with confidence.
        // **A class, not an alias of one.** Both are `DeclKind::Type`; what tells them apart is the same field
        // `operator_returns_of` uses — an alias declares a target in `type_of`, a class declares none. A bare
        // `remove_extent_t` is an alias template, and treating it as a class is exactly the mistake this guards.
        let names_a_class = |name: &str| {
            let bare = base_type_name(name);
            is_a_builtin_name(bare)
                || index
                    .files_declaring(bare, path)
                    .iter()
                    .any(|found| found.fact.kind == DeclKind::Type && found.fact.type_of.is_none())
        };
        let walked = the_element_type_of(index, scopes, root, path, class, &bare, &arguments);
        target = match walked {
            Some(walked) if names_a_class(&walked) => walked,
            // **The pointer-like class's own argument, when its `element_type` leads through a trait this layer cannot
            // follow.** MSVC's `_Ptr_base` says `using element_type = remove_extent_t<_Ty>;`, which substitutes to
            // `remove_extent_t<LuaSource>` — an alias *template* whose target is `typename remove_extent<T>::type`, a
            // trait evaluation rather than a spelling. `base_type_name` then leaves `remove_extent_t`, a class template
            // with no arguments, and no member list can come of it. For a class template that declares a pointer-like
            // arrow, the element is its own argument — that is what `shared_ptr<T>`, `unique_ptr<T>` and every other
            // handle mean, and it is what the standard's `element_type` is for the non-array case.
            _ => match arguments.as_slice() {
                [only] if names_a_class(only) => base_type_name(only).to_string(),
                _ => target,
            },
        };
    }

    (!target.is_empty() && target != bare).then_some(target)
}

/// A name the language spells, not one a declaration introduces — `int` is not a class to look members up in, and it is
/// also not a parameter whose meaning was lost.
pub(crate) fn is_a_builtin_name(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "char"
            | "bool"
            | "void"
            | "float"
            | "double"
            | "long"
            | "short"
            | "signed"
            | "unsigned"
            | "auto"
            | "wchar_t"
            | "char8_t"
            | "char16_t"
            | "char32_t"
            | "size_t"
    )
}
/// **What a pointer-like class's arrow reaches** — its `element_type`, or `value_type` when it has one instead.
///
/// Both are read from the class or a base (a `shared_ptr`'s `element_type` is `_Ptr_base`'s), and both are substituted
/// with the arguments the use wrote, because the declaration spells them with the class's own parameter names.
///
/// # Two callers, one rule
///
/// [`the_arrow_a_member_returns_through`] asks this only when its own walk reached a name nothing declares (a member
/// template's parameter). `completion`'s arrow walk asks it **first**, for any class that declares one, and the
/// difference is a defect measured on a real project: `std::unique_ptr<int> p; p->` offered `get` and `get_deleter`
/// — the smart pointer's *own* members, none of which can be written after that arrow — because `operator->` returns
/// `pointer`, a member alias of `typename _Get_deleter_pointer_type<…>::type`, and that name *is* declared, so the
/// walk followed it into a trait template instead of the class it produces. `element_type` is the standard's own
/// answer to this question (`std::pointer_traits` is written in terms of it) and it is one lookup away.
pub fn element_type_of(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    bare: &str,
    arguments: &[String],
) -> Option<String> {
    the_element_type_of(index, scopes, root, path, class, bare, arguments)
}

pub(crate) fn the_element_type_of(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    bare: &str,
    arguments: &[String],
) -> Option<String> {
    let parameters = index.template_parameters_for(bare, path, arguments);
    // **The walk that follows member aliases as well as base clauses** — see
    // [`an_element_type_through_the_members`]. The class this exists for names its base through an alias
    // (`class shared_ptr : public _Mybase`), so the base's `element_type` is never in the class's own member list.
    let through_the_members = an_element_type_through_the_members(
        index,
        scopes,
        root,
        path,
        class,
        &parameters,
        arguments,
        0,
    );
    if through_the_members.is_some() {
        return through_the_members;
    }
    // **The member *list*, not the single-member query**: `member_fact` ends by falling back to this very function
    // (the arrow of a class that has no `element_type`), so asking it here recursed until the stack ran out — measured,
    // the test that produced this line overflowed it. A list walks the bases and never asks about an arrow.
    let Known::Yes(members) = members_of(index, scopes, root, path, class) else {
        return None;
    };

    for name in ["element_type", "value_type"] {
        let Some(fact) = members
            .members
            .iter()
            .find(|member| member.fact.name == name)
            .map(|member| &member.fact)
        else {
            continue;
        };
        let Some(written) = fact.type_of.as_deref() else {
            continue;
        };
        let substituted = sema::types::substituted_spelling(written, &parameters, arguments);
        let target = base_type_name(&substituted).to_string();

        // A parameter name is not an answer: it means the declaration named something this call cannot substitute.
        if !target.is_empty()
            && target != bare
            && !parameters.iter().any(|parameter| parameter == &target)
        {
            return Some(target);
        }
    }
    None
}
