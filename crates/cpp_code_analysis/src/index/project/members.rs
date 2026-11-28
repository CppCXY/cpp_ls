//! **Members** — the declarations a class declares, the bases it inherits, and the answer to `object.member`.
//!
//! Moved here from `project/mod.rs` without a change: the member question is the one an editor asks most (a
//! completion, a hover after a `.`, the field a `->` names), and it is the one built from the most — the class's own
//! facts, its base clauses substituted, its aliases resolved, and the standard's own fixed deductions.

use super::*;
use crate::DeclKind;
use crate::FactGuard;
use crate::FileView;
use crate::ScopeId;
use crate::ScopeKind;
use crate::ScopeTree;
use crate::Visibility;
use crate::index;
use crate::sema;

/// Which member a `obj.member` or `ptr->member` at `offset` names.
///
/// The first query in this crate that needs a **type**: `size` in `widget.size` is not looked up among the names
/// in scope, it is looked up *in the type of `widget`*. So the answer is built in three steps, each of which
/// already existed:
///
/// ```text
/// 1. read the shape          — the object expression and the member's spelling (sema::resolve)
/// 2. infer the object's type — its declaration's `type_of`, in this file or through the index
/// 3. look the member up      — as the qualified name `<type>::<member>`, which the index already matches
/// ```
///
/// Step 3 is why this was cheap to add: a declaration fact records the qualified spelling of the scope it was
/// written in, so `Widget::size` is a question the existing lookup answers. What is new is step 2, and it is the
/// beginning of the `infer` layer — deliberately narrow: the object has to be a
/// **name**, because inferring the type of an arbitrary expression is a different and much larger problem.
///
/// # The four answers
///
/// * `Yes` — the member, declared in the class the object's type names.
/// * `Unknown(UnknownType)` — the object's type could not be worked out: the object is not a plain name, or its
///   declaration says nothing about its type, or the type's name resolves to nothing.
/// * `Unknown(NotDeclaredHere)` — the type is known and the member is not in it. Not a definite no, for the
///   reason every other answer here is not: the class may be a base class, a template, or declared in a header
///   nobody indexed.
/// * `Unknown(UnparsableName)` — the offset is not on a member access at all.
pub fn member_across_files(
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<FileView>,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<ProjectDefinition> {
    // One request's worth of memo and table for this single question — see [member_across_files_with] for the
    // callers that ask it many times over one file (the semantic-token pass asks it once per member access).
    member_across_files_with(
        &TypeMemo::default(),
        &sema::declarations::ShapeTable::new(),
        index,
        declaring,
        scopes,
        root,
        path,
        offset,
    )
}

/// [member_across_files] with the memo and the declaration table threaded in.
///
/// A pass over one file asks this question **once per member access**, and each ask used to pay for its own
/// TypeMemo and its own DeclarationShapes — the second is a walk of the whole file, so the semantic-token
/// pass on a 901-line file spent 1 150 ms here (docs/diagnostics-performance.md §3.3).
pub(crate) fn member_across_files_with(
    memo: &TypeMemo,
    shapes: &sema::declarations::ShapeTable,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<FileView>,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<ProjectDefinition> {
    let Some(access) = sema::resolve::member_access_at(root, offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    // `w.` with nothing after the operator is a member access with no member, and a *jump* needs something to jump
    // to. The question this answers is which declaration the written name refers to, and there is no written name —
    // so this is the same "nothing to look up" as a cursor on punctuation. That the shape is readable at all is
    // what the completion query needs, and it is why the tolerance lives in the shape reader rather than here.
    if access.member.is_empty() {
        return Known::Unknown(UnknownReason::UnparsableName);
    }

    // The type of the object, which is what decides which class the member is looked for in.
    let Known::Yes((written, _)) = type_of_expression_memoed(
        memo,
        shapes,
        index,
        declaring,
        scopes,
        root,
        path,
        &access.object,
        0,
    ) else {
        let Known::Unknown(reason) = type_of_expression_memoed(
            memo,
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            &access.object,
            0,
        ) else {
            unreachable!("the first match established that this is an `Unknown`")
        };
        return Known::Unknown(reason);
    };

    let Some(class) = member_access_class(&written) else {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written.to_string())));
    };

    // **No template substitution here**, and the boundary is worth stating: this query answers "where is this
    // member declared", which is a *place* rather than a type. The pairing that finishes a member of a class
    // template is applied by the callers that ask for its type — see [`NamedDeclaration::type_of`] — and applying
    // it here as well would have to rewrite the fact's own spelling, which is what
    // `DeclFact::type_of` is documented to be *as the file wrote it*.
    let found = member_fact(
        shapes,
        index,
        scopes,
        root,
        path,
        &class,
        &access.member,
        None,
    );
    let Known::Yes((fact, file)) = found else {
        let Known::Unknown(reason) = found else {
            unreachable!("the first match established that this is an `Unknown`")
        };
        return Known::Unknown(reason);
    };

    Known::Yes(ProjectDefinition { file, fact })
}

/// **The class a member access is asked of** — the type's own class, or the class it points at.
///
/// `p->size` and `(*p).size` are the same question, and a language server cannot ask the user to write the
/// second one: a pointer's member access reaches through the pointer, and every consumer of this layer wants
/// the class on the other side. A **reference** is already seen through by [`Type::class_name`], because C++
/// does that for free; a **pointer** is not, because reaching a pointee is otherwise a `*` the caller writes.
///
/// One level, which is the language's own rule: a `Widget**` needs two `*`s, and a query that kept descending
/// would answer about a class the expression does not name.
///
/// The class is returned **owned** rather than borrowed, because the class of a pointee belongs to a type that
/// is itself a shared handle — and a borrow of it would have to outlive a local, which is the shape that ends
/// in unsafe lifetime extension. One `String` per member query is the price of not doing that.
/// **The braced initialiser a declaration was written with** — `std::pair pair{1, 2}` → `{1, 2}`.
///
/// `deduction_inputs` looks for an `Initializer` node, which is the shape `= value` and `(args)` produce; a braced
/// initialisation puts its list beside the declarator instead, so it needs its own reader. `None` for every declaration
/// that is not initialised with braces, and for one in another file (this layer holds one tree).
pub(crate) fn the_braced_initialiser_of(
    root: &cpp_parser::CppSyntaxNode,
    named: &NamedDeclaration,
) -> Option<cpp_parser::CppSyntaxNode> {
    let NamedDeclaration::Here(binding) = named else {
        return None;
    };
    let declarator = sema::types::declarator_declaring(root, binding.name_range)?;
    let parent = declarator.parent()?;
    // **The list is wrapped**: `Pair p{1, 2.5}` is `InitDeclarator > [Declarator "p", Initializer "{1, 2.5}"]`, and the
    // `Initializer` holds the `InitListExpr`. One hop, not a search: a subtree search would also find a braced list in an
    // argument (`Pair p(make(1))`'s nested calls), which is not this declaration's initialisation.
    let initializer = parent
        .children()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Initializer)?;
    initializer
        .children()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::InitListExpr)
}

/// **A member of the class the cursor is inside** — the second half of C++'s unqualified name lookup.
///
/// Only a bare identifier is considered: `std::make` is qualified, and a qualified name is not looked up among a class's
/// members. The member list is the query that answers (`members_of` walks the bases and substitutes the class's
/// arguments), which is why a member inherited through a template base comes back with a finished type.
pub(crate) fn a_member_of_the_enclosing_class(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    callee: &cpp_parser::CppSyntaxNode,
    // **What the caller does with it.** The card wants the declaration and can show a type the header wrote with the
    // base's parameter in it; the type engine cannot use one, and a wrong type is worse than a refusal.
    a_finished_type_only: bool,
) -> Option<ProjectDefinition> {
    let name = callee.text().to_string();
    let name = name.trim();
    let bare = !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_');
    if !bare {
        return None;
    }
    let class =
        class_of_the_enclosing_definition(scopes, usize::from(callee.text_range().start()))?;
    let Known::Yes(listed) = members_of(index, scopes, root, path, &class) else {
        return None;
    };
    let member = listed
        .members
        .iter()
        .find(|member| member.fact.name == name)?;
    // **A member of a base that is itself a template is not finished here.** `enable_shared_from_this<Node>`'s
    // `shared_from_this` is `_Tp`, and the member list does not carry the base's arguments — printing `_Tp (*)()` (which
    // is what a call of it answered) is worse than saying the type is not known. The refusal is the same one every other
    // unreadable spelling gets.
    let names_a_parameter = |written: &str| {
        written
            .split(|character: char| !character.is_alphanumeric() && character != '_')
            .any(|word| {
                member
                    .fact
                    .parameters()
                    .iter()
                    .any(|parameter| !parameter.is_empty() && parameter == word)
            })
    };
    if a_finished_type_only
        && (member
            .fact
            .returns
            .as_deref()
            .is_some_and(names_a_parameter)
            || member
                .fact
                .type_of
                .as_deref()
                .is_some_and(names_a_parameter))
    {
        return None;
    }
    // **No class argument pairing here**: `members_of` has already substituted the class's arguments through its
    // members, so the fact's types are the ones a reader of this class sees.
    Some(ProjectDefinition {
        file: member.file.clone(),
        fact: member.fact.clone(),
    })
}

/// **The values of a braced list, flat** — `{1, 2.5}` and `std::pair{1, 2.5}` are the same values.
///
/// A **construction** writes the type's name as the list's first child and the values as *another* list:
/// `InitListExpr > [IdentifierExpr "std::pair", InitListExpr "{1, 2.5}"]`. A declaration's initialiser is the flat list.
/// The deduction wants the values, so the nested list is opened once.
pub(crate) fn the_elements_of_a_braced_list(
    expression: &cpp_parser::CppSyntaxNode,
) -> Vec<cpp_parser::CppSyntaxNode> {
    let mut elements: Vec<cpp_parser::CppSyntaxNode> = Vec::new();
    for child in expression.children().skip(1) {
        match CppSyntaxKind::from(child.kind()) == CppSyntaxKind::InitListExpr {
            true => elements.extend(child.children()),
            false => elements.push(child),
        }
    }
    elements
}

/// **A type a bare expression names** — `std::pair` in `std::pair{1, 2}`, or a written instantiation.
///
/// The question is asked when a braced initialisation's head is not a *name in a scope*: a qualified type written as an
/// expression is an `IdentifierExpr`, which `declaration_of_expression` declines. The index answers whether the spelling
/// is a class, and a spelling that already carries `<…>` is its own type — nothing to look up.
pub(crate) fn a_type_the_expression_names(
    index: &ProjectIndex,
    path: &Path,
    expression: &cpp_parser::CppSyntaxNode,
) -> Option<String> {
    let spelled = expression.text().to_string();
    let spelled = spelled.trim();
    if spelled.is_empty() || spelled.contains(' ') {
        return None;
    }
    if spelled.contains('<') {
        return Some(spelled.to_string());
    }
    let names_a_type = match index.definition(spelled, path) {
        Known::Yes(found) => found.fact.kind == DeclKind::Type,
        // **A name is a type when one of its declarations is one.** `std::string` is an alias (a `Type`) and also has a
        // constructor named `string`, so the name query answers `Ambiguous`; requiring *every* candidate to be a type
        // refused the head of `std::string{"three"}` and, with it, the whole `std::tuple{1, 2.5, std::string{"three"}}`
        // (measured on `basics.cpp`: the tuple deduced to its bare name). A construction needs a class, and a name that
        // any of whose declarations is a type can be one.
        Known::Unknown(UnknownReason::Ambiguous(_)) => index
            .definitions(spelled, path)
            .value()
            .is_some_and(|found| {
                found
                    .found
                    .iter()
                    .any(|declaration| declaration.fact.kind == DeclKind::Type)
            }),
        // **An inline namespace is not part of the name a reader writes.** `std::string` is
        // `using string = basic_string<char>;` inside `std::__cxx11`, so its qualified name is `std::__cxx11::string` and
        // the spelled lookup names nothing — measured on `basics.cpp`, where `std::string{"three"}` answered no type and
        // the `std::tuple{1, 2.5, std::string{"three"}}` around it deduced to its bare name. The retry is the one
        // `members_of` already makes: the last segment, and any candidate that is a type.
        Known::Unknown(_) | Known::No => {
            let bare = spelled.rsplit("::").next().unwrap_or(spelled);
            bare != spelled
                && index.definitions(bare, path).value().is_some_and(|found| {
                    found
                        .found
                        .iter()
                        .any(|declaration| declaration.fact.kind == DeclKind::Type)
                })
        }
    };
    names_a_type.then(|| spelled.to_string())
}

/// **The namespace a namespace alias names** — `views` → `std::ranges::views`.
///
/// A namespace alias's fact is a `Namespace` whose `type_of` is the target's *written* spelling, which may be relative to
/// the scope the alias was declared in (`namespace views = ranges::views;` inside `std`). It is qualified here with that
/// scope when it is unqualified, and `None` is returned for everything that is not an alias — including a real namespace
/// with no members, which is a different answer.
pub(crate) fn a_namespace_aliass_target(
    index: &ProjectIndex,
    path: &Path,
    class: &str,
) -> Option<String> {
    let found = index.definition(class, path).value()?;
    if found.fact.kind != DeclKind::Namespace {
        return None;
    }
    let written = found.fact.type_of.clone()?.to_string();
    if written.contains("::") || written.is_empty() {
        return Some(written);
    }
    match &found.fact.scope {
        Some(scope) => Some(format!("{scope}::{written}")),
        None => Some(written),
    }
}

/// **What a class-template use with no written arguments stands for, from the standard's own guides.**
///
/// `std::tuple{1, 2.5, s}` is `std::tuple<int, double, S>` and `std::pair pair{4, 5}` is `std::pair<int, int>` — the
/// standard fixes both, and neither is readable from libstdc++'s constructors (a constrained variadic template for `tuple`,
/// and a `pair` whose declaring class the walk does not reach). A table is the honest form for a closed set: the names are
/// the standard's, and a class a project calls `pair` is not `std::pair` (the spelling must be `std`-qualified, or the fact
/// must say its scope is `std`).
///
/// The number of elements has to line up with the rule, which is what keeps `std::pair one{1}` from answering a
/// `pair<int>` the language would reject.
pub(crate) fn the_deduction_the_standard_fixes(
    name: &str,
    class_as_written: &str,
    element_types: &[Type],
) -> Option<Vec<Type>> {
    let plain = class_as_written
        .rsplit("::")
        .next()
        .unwrap_or(class_as_written);
    let in_the_library = class_as_written.contains("std::") || name.starts_with("std::");
    if !in_the_library {
        return None;
    }

    // **Every element, one per parameter** — `pair` takes exactly two, `tuple` as many as are written.
    match plain {
        "pair" if element_types.len() == 2 => {
            return Some(element_types.to_vec());
        }
        "tuple" if !element_types.is_empty() => {
            return Some(element_types.to_vec());
        }
        _ => {}
    }

    // **One argument, from the first element** — the containers whose braced form is
    // `Container(std::initializer_list<T>)`.
    let from_the_first = matches!(
        plain,
        "optional"
            | "vector"
            | "list"
            | "deque"
            | "set"
            | "multiset"
            | "unordered_set"
            | "unordered_multiset"
            | "basic_string"
            | "valarray"
            | "initializer_list"
            | "stack"
            | "queue"
            | "priority_queue"
            | "span"
    );
    if from_the_first && element_types.len() == 1 {
        return Some(element_types.to_vec());
    }
    None
}

/// **The class arguments a braced initialisation deduces** — `std::pair pair{1, 2}` is a `pair<int, int>`.
///
/// C++17's deduction for a class template with no written arguments: the initialiser's elements are the constructor's
/// arguments, and the constructor's parameters say what each template parameter stands for. `deduce_the_parameters` is
/// the pairing (a parameter written as one template parameter, reference qualifiers stripped); this walks the class's
/// constructors, keeps the ones that deduce **every** parameter, and answers only when they agree — the same falsifiable
/// rule the overload arm uses.
///
/// `None` when the spelling already names a type (`pair<int, int>`), when it is not a class template, when an element's
/// type is unknown (a lambda's type is unnamed, and guessing one would print an instantiation nobody wrote), or when the
/// candidates disagree.
#[allow(clippy::too_many_arguments)]
pub(crate) fn the_arguments_a_braced_initialisation_deduces(
    memo: &TypeMemo,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<FileView>,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    elements: &[cpp_parser::CppSyntaxNode],
    spelled: &str,
    depth: usize,
) -> Option<Type> {
    // The written spelling must be a bare class name: a template-id has its arguments already.
    if spelled.contains('<') {
        return None;
    }
    let class = parse_type_spelling(spelled);
    let Type::Named { name, arguments } = &class else {
        return None;
    };
    if !arguments.is_empty() {
        return None;
    }

    let element_types: Vec<Type> = elements
        .iter()
        .filter_map(|element| {
            match type_of_expression_memoed(
                memo,
                &sema::declarations::ShapeTable::new(),
                index,
                declaring,
                scopes,
                root,
                path,
                element,
                depth + 1,
            ) {
                Known::Yes((type_of, _)) => Some(type_of),
                _ => None,
            }
        })
        .collect();
    // **Every element, or none.** A list with one unknown element cannot say what the class is, and a partial answer
    // would print a `pair<int, _T2>` nobody wrote.
    if element_types.is_empty() || element_types.len() != elements.len() {
        return None;
    }

    // **The class's template parameters, from the declaration or from its own members.**
    //
    // The index's name query is the direct answer, and it declines a spelling written in an expression in a session that
    // has not indexed the project (`std::pair{1, 2}` — the class is declared in the same file). The member list is the
    // query the deduction walks anyway, and every member of a class template carries the class's parameters
    // (`DeclFact::parameters` is what pairs them with a use's arguments), so the fallback is not a second guess: it is the
    // same field, read from a fact that is in hand.
    //
    // **Two lookups, and no walk, before the standard's own guides are consulted** — see below for why the order is the
    // whole of this change.
    let declared = index
        .definitions(name, path)
        .value()
        .and_then(|found| {
            found
                .found
                .into_iter()
                .find(|declaration| !declaration.fact.parameters().is_empty())
        })
        .or_else(|| index.definition(name, path).value());
    let class_as_written = match &declared {
        Some(declaration) => declaration.fact.qualified_name(),
        None => name.clone(),
    };

    // **The standard's own guides, before the walk that reads a class's members.** The library's constructors are
    // constrained templates (`tuple(_UElements&&...) requires …`), which no pairing of the class's parameters can read;
    // the guide is a fact about the standard rather than about the text, so it needs neither the declaration's
    // parameters nor the member list.
    //
    // **It used to sit below `members_of`, and that is the defect this fixes.** `members_of` is the one call here that
    // costs more than a query: measured on `std::tuple tuple{1, 2.5, std::string{"three"}}` in a nine-line file, listing
    // the members of a class template whose base is written in the pack (`tuple` inherits `_Tuple_impl<0, _Elements...>`,
    // which inherits `_Tuple_impl<1, …>`) took the server **16–20 seconds and 1–2 GB** for one inlay hint, and the
    // request never came back inside the client's deadline. The answer the table holds was available before the walk
    // started: a table that exists so that a walk is unnecessary cannot be read after it.
    if let Some(deduced) = the_deduction_the_standard_fixes(name, &class_as_written, &element_types)
    {
        return Some(Type::Named {
            name: class_as_written,
            arguments: deduced,
        });
    }
    let constructor_name = class_as_written
        .rsplit("::")
        .next()
        .unwrap_or(&class_as_written);

    let Known::Yes(listed) = members_of(index, scopes, root, path, &class_as_written) else {
        return None;
    };
    let parameters = match declared {
        Some(declaration) if !declaration.fact.parameters().is_empty() => {
            declaration.fact.parameters().to_vec()
        }
        _ => listed
            .members
            .iter()
            .find(|member| !member.fact.parameters().is_empty())
            .map(|member| member.fact.parameters().to_vec())?,
    };

    if parameters.is_empty() {
        return None;
    }

    let mut agreed: Option<Vec<Type>> = None;
    for constructor in listed
        .members
        .iter()
        .filter(|member| member.fact.kind == DeclKind::Function)
        .filter(|member| member.fact.name == constructor_name)
    {
        let deduced = deduce_the_parameters(
            &parameters,
            constructor.fact.parameter_list(),
            &element_types,
        );
        // Every parameter must have been replaced: one left standing is not a deduction.
        let every = deduced.iter().all(|argument| {
            !matches!(argument, Type::Named { name, .. }
                if parameters.iter().any(|parameter| parameter == name))
        });
        if !every {
            continue;
        }
        match &agreed {
            Some(previous) if *previous != deduced => return None,
            Some(_) => {}
            None => agreed = Some(deduced),
        }
    }

    agreed.map(|arguments| Type::Named {
        name: name.clone(),
        arguments,
    })
}

/// **What a pointer-like class's arrow reaches** — its `element_type`, with the class's parameters substituted.
///
/// The standard's protocol (`std::pointer_traits` is written in terms of it, and `unique_ptr`, `shared_ptr` and `weak_ptr`
/// all take part): `using element_type = _Tp;`. Pairing `_Tp` with the object type's argument is the step that turns
/// `shared_ptr<Node>` into `Node` — without it the member is `_Tp`, which is not a class, and the lookup answered
/// `Unknown` in the handle (`auto twin = node->twin();`).
///
/// `None` for a class that declares no `element_type` (an ordinary pointer, whose pointee the plain reader already
/// strips) and for a substitution that does not land on a class.
pub(crate) fn the_pointee_of_a_handle(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    arguments: &[Type],
) -> Option<(String, Vec<Type>)> {
    let written: Vec<String> = arguments
        .iter()
        .map(|argument| argument.to_string())
        .collect();
    let Known::Yes((fact, file)) = member_fact(
        &sema::declarations::ShapeTable::new(),
        index,
        scopes,
        root,
        path,
        class,
        "element_type",
        Some(&written),
    ) else {
        return None;
    };
    let written_type = fact.type_of.as_deref()?;
    let bindings = member_bindings(index, scopes, root, path, class, arguments);
    let finished = bindings.applied_to(&parse_type_spelling(written_type));
    // **A nested spelling is followed**, which is the ordinary shape in the standard library: `shared_ptr` inherits
    // `__shared_ptr<_Tp>` and `element_type` is written there as `_Tp`, so the fact found through the base walk says
    // `__shared_ptr<_Tp>::element_type`. Substituted, that is `__shared_ptr<Node>::element_type` — a member of the class
    // `__shared_ptr<Node>`, and following it is what answers `Node`.
    let finished = finish_a_nested_name(
        index,
        scopes,
        root,
        path,
        class,
        &bindings,
        finished,
        file,
        vec![fact.name.to_string()],
    );
    // **A name that no class declares is not a pointee.** `std::shared_ptr`'s `element_type` is written
    // `typename __shared_ptr<_Tp>::element_type`, and a walk that cannot finish it answers the spelling it started with —
    // a class called `__shared_ptr<Resource>::element_type`, which nothing declares. Handing that back hides the caller's
    // own fallback (following `operator->`'s declared return type), which is the step that does resolve it: measured on
    // `test_cpp`'s `smart_pointers.cpp`, the hover asked for members of that invented name and said
    // "`shared_ptr<Resource>` has no member `identifier`".
    match finished.0 {
        Type::Named { name, arguments } => {
            let declared = index.files_declaring(base_type_name(&name), path);
            declared
                .iter()
                .any(|declaration| declaration.fact.kind == DeclKind::Type)
                .then_some((name, arguments))
        }
        _ => None,
    }
}

pub(crate) fn member_access_class(written: &Type) -> Option<String> {
    if let Some(class) = written.class_name() {
        return Some(class.to_string());
    }

    written.pointee()?.class_name().map(str::to_string)
}

/// **A type as the spelling a class question is asked with** — its arguments kept, and nothing else.
///
/// A member query takes a **class spelling**: `members_of("std::vector<int>")` finds the class and substitutes its
/// member types. A type that came from a binding may carry what the *binding* said — `&` from `auto &v = …`, `const`
/// from `const Widget &w` — and those are not part of the class's name: asking for `std::vector<MarkEvent>&` answers
/// "nothing declares a class with that name", which is true and useless.
///
/// Measured on `EmmyLuaCodeStyle`'s `Mark.cpp`: `auto &events = p.GetEvents();` typed `events` as
/// `std::vector<MarkEvent>&`, so `events[Pos]` — a subscript on a class, which is `operator[]` — answered
/// `Unknown(UnknownType("events[Pos]"))` and everything read through it was unknown, including the union member the
/// report was about.
///
/// The recursion is the whole rule: a reference and a qualifier are transparent (C++ says so — a member of a
/// `const Widget&` is a member of `Widget`), and anything else is its own spelling.
pub(crate) fn the_class_spelling(type_of: &Type) -> String {
    match type_of {
        Type::Reference { to, .. } => the_class_spelling(to),
        Type::Qualified { of, .. } => the_class_spelling(of),
        other => other.to_string(),
    }
}

/// **The class a member access is asked of, and the arguments that finish its members' types.**
///
/// The two halves come from one place because they come from one *step*, and there are two callers — the member
/// read as a value (`v.front`) and the member called (`v.back()`) — which had them apart and disagreed for exactly
/// as long as that lasted. Measured: `x.back()` on a `std::string` answered `char&` while `auto y = x.back()`
/// answered `_Elem&`, because the second caller paired the class's parameters with an empty argument list.
///
/// The arguments are the ones the **use** wrote when it wrote any (`std::vector<int>::data`), and otherwise the
/// ones the **alias** carried (`std::string` → `basic_string<char, char_traits<char>, allocator<char>>`): a use
/// that wrote none has not said nothing, it has said "whatever the name it used stands for". See
/// [`resolve_aliases_with_arguments`].
pub(crate) fn member_access_class_and_arguments(
    shapes: &sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    written: &Type,
) -> (String, Vec<Type>) {
    let (class, from_the_alias) =
        resolve_aliases_with_arguments(shapes, index, scopes, root, path, class);

    let arguments = if written.arguments().is_empty() {
        from_the_alias
    } else {
        written.arguments().to_vec()
    };

    (class, arguments)
}

/// **What of a member list may be offered at this cursor** — both rules, in one place, and the count of what the
/// second one kept out.
///
/// # The two rules
///
/// 1. **The names the implementation owns are not offered**, the same rule the name completion applies — and this is
///    where a user meets them first: `line.` on a `std::string` listed 202 members of MSVC's `basic_string`, half of
///    them `_Alty`, `_ALLOC_MASK`, `_Apply_annotation`. See [`is_reserved_to_the_implementation`].
/// 2. **A member the reader cannot name is not offered.** `private` and `protected` are the difference between what
///    a class *has* and what may be written at a cursor, and offering one is a mistake the reader only discovers by
///    compiling. The level travels on the fact ([`DeclFact::access`]) and is checked against the cursor's own
///    classes — the one it is written in and what that derives from, which are the two the language lets a
///    non-public member be named from.
///
/// # Why one function, when there are two callers
///
/// Because they had one rule each, and a rule with two readers is how this area's bugs have all started: the
/// completion layer reads the shape itself (it already has one from the context reader) while
/// [`member_completions_at`] reads it again, and both then need the same two filters. The filters live here, and the
/// second is *counted* rather than dropped in silence so that a `.` which offers nothing can say which of the two
/// reasons it was — "the class has no members" and "every member of it is `private` from here" are different facts
/// about the program, and only one of them has a fix the reader can apply.
pub(crate) fn offerable_members(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
    members: &mut MemberList,
) -> usize {
    members
        .members
        .retain(|member| !is_reserved_to_the_implementation(&member.fact));

    let open_to_the_cursor = classes_the_cursor_is_in(index, scopes, root, path, offset);
    let before = members.members.len();

    members.members.retain(|member| {
        let Some(access) = member.fact.access else {
            return true;
        };
        if access.is_open_to_everyone() {
            return true;
        }

        member
            .fact
            .scope
            .as_deref()
            .is_some_and(|owner| open_to_the_cursor.iter().any(|seen| seen == owner))
    });

    before - members.members.len()
}

/// **The classes a cursor is "inside", for access** — the innermost class its offset is written in, and every class
/// that one derives from.
///
/// # Why those two, and only those two
///
/// C++ lets a class name its own `private` members and its descendants name its `protected` ones. So the "us" of
/// `private:` is the class itself and the "us" of `protected:` is the class and everything below it — and one list
/// answers both, because a member's level only has to be checked against the class it is declared in being *in*
/// this list. A member of a class outside it is invisible, whatever its level: a `protected` member of an unrelated
/// class is as unreachable as a `private` one.
///
/// # What it deliberately does not do
///
/// * **Friends.** `friend class X;` and `friend void f();` make declarations visible to something that is not a
///   descendant, and this model records no friends, so a friend's access is reported as unreachable — a member not
///   offered that the reader could in fact write. That is the direction chosen for the whole feature: a name that
///   is missing is a smaller mistake than a name that cannot be written (see [`crate::completion`]).
/// * **Local classes and lambdas inside a member function.** The cursor's class is found from the *scopes* the
///   offset is in, which a lambda's body shares with the enclosing member — so a lambda in a member function sees
///   what the member function sees, which is what the language says.
///
/// Empty when the cursor is not in a class at all — the ordinary case for a `.cpp` file — and the caller then
/// offers only public members.
pub(crate) fn classes_the_cursor_is_in(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Vec<String> {
    // The innermost class, from the scope the offset is in. `qualification_prefix_of` is what turns a `ScopeId`
    // into the spelling a `DeclFact` records, and only a class-like scope has one that names a class.
    let Some(innermost) = scopes.scope_at(offset) else {
        return Vec::new();
    };

    let mut classes: Vec<String> = scopes
        .scope_chain(innermost)
        .into_iter()
        .filter(|scope| {
            scopes
                .scope(*scope)
                .is_some_and(|data| matches!(data.kind, ScopeKind::Class | ScopeKind::Enum))
        })
        .filter_map(|scope| scopes.qualification_prefix_of(scope))
        .collect();

    // …and outward, one level of bases at a time, the same walk [`members_of`] makes in the other direction. A base
    // that cannot be resolved stops its own branch rather than the whole answer: what is known is still known.
    let mut level: Vec<String> = classes.clone();
    let mut depth = 0usize;

    while !level.is_empty() && depth < MAX_NESTED_TYPE_DEPTH {
        let mut next: Vec<String> = Vec::new();

        for class in &level {
            for base in bases_of(index, scopes, root, path, class)
                .value()
                .unwrap_or_default()
            {
                if !classes.contains(&base) {
                    classes.push(base.clone());
                    next.push(base);
                }
            }
        }

        level = next;
        depth += 1;
    }

    classes
}

/// Every member a type has: its own, and the ones it inherits.
///
/// The query a completion after `.` or `->` is built on, and the second one in this module that needs a **type**.
/// It is the *list* form of [`member_across_files`]: that one is given a member's name and answers where it is
/// declared, this one is given nothing but the type and answers what there is to name at all.
///
/// # The base chain is walked here, and never stored
///
/// The one design decision this query makes, and the reason it is a query rather than a field.
///
/// A summary keeps what each file **says**: `struct D : public B` is stored as the spelling `B`, and `D`'s
/// summary says nothing about `B`'s members. The alternative — resolving the bases when the summary is built and
/// writing `D`'s inherited members onto `D` — is what an index-shaped instinct reaches for, and it is wrong for
/// three reasons that all end in a stale answer with nothing on disk to contradict it:
///
/// ```text
/// 1. B gains and loses members          -> D's text and D's key are unchanged, and D's stored list is now wrong
/// 2. D's base list changes              -> caught, because D's text changed
/// 3. *which* B the name `B` means       -> unchanged in D's text, changed by a macro, an include or an
///                                          `#undef`, so D's stored list is wrong with nothing to catch it
/// ```
///
/// The third is the one that settles it: it leaves `D`'s text identical, so no per-file invalidation can see it.
/// Walking the chain at query time costs one lookup per base per query and cannot go stale, because nothing is
/// kept. `a_member_added_to_a_base_appears_without_reindexing_the_derived_class` is that argument as a test.
///
/// # The three things a list can be, and none of them is "these are all the members"
///
/// * `Yes(list)` — the members this analysis can see. `list.unlisted` names the bases that could not be listed
///   at all, so a consumer can tell a complete answer from a truncated one instead of guessing.
/// * `Unknown(NotDeclaredHere)` — nothing visible declares the type. Not `No`: the index holds a subset of the
///   translation unit, so a type from a header nobody indexed looks exactly like a type that does not exist.
/// * `Unknown(ConditionalCompilation)` — the type is only reachable through a guarded `#include`, so whether it
///   is here at all is not known.
///
/// # What it does with a name two bases declare
///
/// Both entries are listed and both are marked [`ProjectMember::ambiguous`]. Dropping one would be choosing, and
/// choosing is the answer the language refuses to give — the same fact [`member_across_files`] states as
/// `Unknown(Ambiguous)` for a single name, stated here as a property of a listed member, because a list with a
/// hole in it would be a worse answer than a list that says which entries are contested.
///
/// # What it deliberately does not do
///
/// * No `using` declarations and no virtual/override resolution. A `using Base::f;` in a derived class brings a
///   name in without declaring a member of its own, and nothing here models that yet.
/// * No access check. A `private` base's members are listed, because access is not in the facts — see
///   [`DeclFact::bases`] — and filtering on a guess would hide members a consumer can legitimately see.
/// * No instantiation. A template class lists the members it was written with; a base written `Base<int>` is
///   looked up as the class `Base`.
///
/// **The members written in the class itself** — [`members_of`]'s first step, and nothing else.
///
/// [`members_of`] answers the question a *reader* asks ("what can I write after `.`"), and it walks the base chain
/// to do it. Measured on `test_module`'s `import std;` file: `members_of("std::vector")` is **ten bases and 583 ms
/// of walking**, while the class's own list costs 16 ms and comes back empty — so the walk was 97% of the answer
/// for a class it could not name a single member of.
///
/// A *check* that only needs the class's own constructors asked that question and paid for the walk on **every
/// pull** (`assign-type-check`: 1 221 ms of a 1 497 ms diagnostic pass, two classes). This is the same first step,
/// exposed: one `direct_members`, measured at 4–16 ms.
pub fn own_members_of(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<Vec<(PathBuf, DeclFact)>> {
    own_members_of_with(
        &sema::declarations::ShapeTable::new(),
        index,
        scopes,
        root,
        path,
        class,
    )
}

/// [own_members_of], with the file's declaration table threaded in.
pub fn own_members_of_with(
    shapes: &sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<Vec<(PathBuf, DeclFact)>> {
    direct_members(shapes, index, scopes, root, path, class)
}

/// * No conditional region. A member of a class in the buffer comes back with [`FactGuard::Unconditional`]
///   whatever `#if` it is really in — see `fact_from_binding`. A class from the index does carry its regions.
pub fn members_of(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<MemberList> {
    // **One table per call, for a caller that has no request to share it in.** The diagnostics path does — see
    // [`members_of_with`] and `SemanticModel`'s cell — and this wrapper is what keeps every other caller of the
    // public query compiling without a parameter it would have nothing to pass.
    members_of_with(
        &sema::declarations::ShapeTable::new(),
        index,
        scopes,
        root,
        path,
        class,
    )
}

/// [`members_of`], with the file's declaration table threaded in — see [`crate::sema::declarations::ShapeTable`].
///
/// The table is the one thing this query needs from the tree and cannot get from the index, and a caller that
/// asks about many classes of one file must hand in **one** table: building it per class is what made a
/// diagnostics pass quadratic (`docs/diagnostics-performance.md` §2.4 — 1 801 builds, 6.6 s of 7.2 s).
pub fn members_of_with(
    shapes: &sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<MemberList> {
    // **A class's members do not change because a reader typed a character somewhere else.** Every diagnostics pull
    // asks for the same handful of classes — `std::vector`, `std::basic_string`, the file's own types — and each
    // answer costs 4–16 ms (`direct_members`, measured) because it reads the class's facts and walks its bases.
    // Measured on `LuaSource.cpp`: 17 lists, 82 ms of a 157 ms pull, every keystroke. The cache is invalidated by
    // *which files the list was made of*, so an edit to a file that contributes members drops exactly those lists
    // (see [`CachedMemberList`]).
    let key = (class.to_string(), path.to_path_buf());
    if let Some(known) = index.cached_member_list(&key) {
        return known;
    }

    let listed = members_of_inner(shapes, index, scopes, root, path, class);
    // **A bare spelling the index files qualified.** `shared_ptr<int>` is what a type deduced from a `std::` function is
    // written as, and the class is `std::shared_ptr`; a member list asked for the bare name answered `NotDeclaredHere`
    // while the completion at the same cursor offered the members (reported: "`shared_ptr<int>` has no member `reset`"
    // with `reset` in the list next to it). Qualified once, only when the first answer held no class.
    // **An empty answer is also a reason to try the qualified spelling.** A bare class name that resolved to an
    // *ambiguous* declaration answers `Known::Yes` with no members (`direct_members`' own rule for `Ambiguous`), so the
    // retry below — which only ran when the first answer held no class at all — never happened: measured,
    // `members_of("shared_ptr")` answered 0 members while `members_of("std::shared_ptr")` answered 39.
    let listed = match &listed {
        Known::No | Known::Unknown(_) => match class.contains("::") {
            true => listed,
            // **The name, without the arguments, is what the index can qualify.** `allocator_traits<allocator<int>>`
            // is a template id, and `qualified_from_the_index` matches a class *name* — asking it for the id found
            // nothing, so a base the index files as `std::allocator_traits` was `NotDeclaredHere` while the same
            // class with its qualification written out answered. Measured on the conditional fixture: the member
            // list was right for `std::allocator_traits<allocator<int>>` and unknown for the bare spelling the walk
            // uses. The arguments are put back afterwards, which is what the empty-list arm below already did.
            false => {
                let bare = base_type_name(class);
                match qualified_from_the_index(index, path, bare) {
                    Some(qualified) if qualified != bare => {
                        let qualified = match class.find('<') {
                            Some(at) => format!("{qualified}{}", &class[at..]),
                            None => qualified.to_string(),
                        };
                        members_of_inner(shapes, index, scopes, root, path, &qualified)
                    }
                    _ => listed,
                }
            }
        },
        Known::Yes(list) if list.members.is_empty() && !class.contains("::") => {
            match qualified_from_the_index(index, path, base_type_name(class)) {
                Some(qualified) if qualified != class => {
                    // The arguments the caller wrote are kept: `shared_ptr<Resource>` qualifies to
                    // `std::shared_ptr<Resource>`, which is the spelling the members are then substituted with.
                    let qualified = match class.find('<') {
                        Some(at) => format!("{qualified}{}", &class[at..]),
                        None => qualified,
                    };
                    members_of_inner(shapes, index, scopes, root, path, &qualified)
                }
                _ => listed,
            }
        }
        Known::Yes(_) => listed,
    };
    if let Known::Yes(list) = &listed {
        index.remember_member_list(key, list);
    }

    // The diagnostic that found why `std::ios::` offered nothing: whether a member is *present* and at what depth,
    // which is the difference between "the base walk worked" and "the list happens to be long". Off unless asked
    // for, like this crate's other traces.

    listed
}

impl ProjectIndex {
    /// **A member list already built for this class and this file's tree**, if one is held.
    ///
    /// A list whose facts all came from **other** files is the same answer for every file that merely *uses* the class,
    /// so `remember_member_list` files such a list under the empty path too and this lookup falls back to it. The key
    /// keeps the asking path because a class declared in the file being read is resolved against that file's own tree —
    /// a list made *of* this file's facts must not be served to another file, and such a list always has the asking
    /// path among its contributors.
    ///
    /// A list with an `unlisted` base is only as good as the set of files that had been read when it was built, so such
    /// an entry is treated as absent once that set has grown — see [`CachedMemberList`]. A list without a gap never
    /// goes stale by counting.
    pub(crate) fn cached_member_list(&self, key: &(String, PathBuf)) -> Option<Known<MemberList>> {
        let known = self.member_lists.lock().ok()?;
        let read_now = self.summaries.len();
        let cached = known
            .get(key)
            .or_else(|| known.get(&(key.0.clone(), PathBuf::new())))?;
        if !cached.list.unlisted.is_empty() && cached.read_at != read_now {
            return None;
        }

        Some(Known::Yes(cached.list.clone()))
    }

    /// **Remember one member list**, with the files it was made of.
    ///
    /// An **empty** list is not remembered — `direct_members` answers one for a class whose declaration is not in the
    /// index yet, and the contributor set would be empty, so nothing could invalidate it.
    ///
    /// A list with an `unlisted` base **is** remembered, tagged with [`CachedMemberList::read_at`]: the gap is a
    /// statement about the files that have been read, so the entry is valid only while that set does not grow.
    ///
    /// # And a list made of facts that are not cooked is not remembered at all
    ///
    /// A summary is written from the file's **own text** first and revised when the file is **cooked**, and the two
    /// disagree about scope: `_STD_BEGIN` is a name to a reader and `namespace std {` to a compiler, so a class's
    /// members move — and appear — when its header is cooked. Measured on a real project's `<xstring>`:
    /// `std::string* message; message->` offered **one** member before the cook and **58** after, and the
    /// one-member list was then served from this cache for the rest of the session, because none of its contributors
    /// changed and the class was found. A list of uncooked facts is therefore *not* cached: `cooked_reading` answers
    /// for each contributor, and one miss means the facts may still be revised.
    pub(crate) fn remember_member_list(&self, key: (String, PathBuf), list: &MemberList) {
        if list.members.is_empty() {
            return;
        }

        let mut contributors: Vec<PathBuf> = Vec::new();
        for member in &list.members {
            if !contributors.contains(&member.file) {
                contributors.push(member.file.clone());
            }
        }
        // **A list made of not-yet-cooked facts is cached like any other**, and the invalidation rule is what makes
        // that safe: cooking a file calls [`ProjectIndex::forget_member_lists`] for it when its declarations *changed*,
        // so the entry a raw reading produced is dropped the moment a cooked one disagrees with it. Refusing to cache
        // it instead was measured as the whole cost of a cold request: `members_of("std")` — 6 080 members, 37 ms —
        // was recomputed by **every** question that resolved a member of a `std::` type (one cold inlay: ~170 times,
        // 6.3 s), because one of its contributors happened not to be cooked yet.

        let read_at = self.summaries.len();

        if let Ok(mut known) = self.member_lists.lock() {
            if known.len() >= MAX_CACHED_MEMBER_LISTS {
                known.clear();
            }
            // **And under the empty path when no fact came from the asking file**: that list is the index's alone, so
            // every other file that merely uses the class gets the same answer (see `cached_member_list`).
            if !contributors.iter().any(|file| file == &key.1) {
                known.insert(
                    (key.0.clone(), PathBuf::new()),
                    CachedMemberList {
                        contributors: contributors.clone(),
                        list: list.clone(),
                        read_at,
                    },
                );
            }
            known.insert(
                key,
                CachedMemberList {
                    contributors,
                    list: list.clone(),
                    read_at,
                },
            );
        }
    }

    /// **Is the branch this directive is written in one the configuration takes?** — the public form of
    /// [`ProjectIndex::a_guard_is_taken`], for a caller that must not *complain* about a branch nobody compiles.
    ///
    /// [`crate::sema::check::include_not_found`] is that caller: an `#include` under `#if 0`, or under a feature test
    /// this project's `-D`s decide against, names a file for a compiler that never reads the line — so "was not
    /// found" would be a claim about a search that should not have been made. The answer is three-way underneath
    /// (`visibility_at`), and `Unknown` comes back **true**: a guard this layer cannot decide must not silence a
    /// finding, the same rule the declaration lookup follows.
    pub fn the_guard_is_taken(&self, file: &Path, guard: FactGuard, offset: usize) -> bool {
        self.a_guard_is_taken(file, guard, offset, None)
    }

    /// **Is the branch this declaration is written in the one the configuration takes?**
    ///
    /// `Unconditional` is a free yes, and a region is a macro walk (`visibility_at`) — so the verdict is memoized:
    /// the same fact is asked again on every pull that names the declaration, and the uncached filter took a
    /// diagnostic pull from **28 ms to 71 ms** on the real project (`lsp_status`). `Unknown` answers **true** — a
    /// guard this layer cannot decide must not become a lookup that says the name is not declared.
    pub(crate) fn a_guard_is_taken(
        &self,
        file: &Path,
        guard: FactGuard,
        offset: usize,
        answered: Option<&HashMap<(String, u32, usize), Visibility>>,
    ) -> bool {
        let region = match guard {
            FactGuard::Unconditional => return true,
            FactGuard::Region(region) => region,
        };
        // **Already answered by this query's one walk** — see `ProjectIndex::answer_the_guards`, which turns the
        // twenty-five 276-file walks of a colouring pass into one. Absent, the filter walks as it always did.
        if let Some(answers) = answered
            && let Some(answer) = answers.get(&(normalize(file), region, offset))
        {
            return *answer != Visibility::Inactive;
        }
        let key = (file.to_path_buf(), offset, region);
        if let Ok(known) = self.guard_answers.lock()
            && let Some(answer) = known.get(&key)
        {
            return *answer != Visibility::Inactive;
        }
        let visibility = index::environment::visibility_at(self, file, guard, offset);
        if let Ok(mut known) = self.guard_answers.lock() {
            if known.len() >= MAX_GUARD_ANSWERS {
                known.clear();
            }
            known.insert(key, visibility);
        }
        visibility != Visibility::Inactive
    }

    /// **Drop every memoized guard verdict about `path`** — called wherever the index changes for it.
    pub(crate) fn forget_guard_answers(&self, path: &Path) {
        let wanted = normalize(path);
        if let Ok(mut known) = self.guard_answers.lock() {
            known.retain(|(file, _, _), _| normalize(file) != wanted);
        }
    }

    /// **Drop every macro walk whose closure read `path`** — called wherever the index changes for it.
    ///
    /// `known_to_the_index` is [`ProjectIndex::insert_at`]'s question, and the rule is the one that function states for
    /// the visibility walks: a file the index **already knew** can only change the walks that read it, while a file it
    /// has never seen can change how an `#include "x.h"` resolves for a closure that does not contain it, so that one
    /// clears both maps.
    pub(crate) fn forget_macro_walks(&self, path: &Path, known_to_the_index: bool) {
        let wanted = normalize(path);
        if let Ok(mut walks) = self.macro_environments.lock() {
            if known_to_the_index {
                walks.retain(|_, cached| {
                    !cached
                        .contributors
                        .iter()
                        .any(|file| file.as_str() == wanted.as_str())
                });
            } else {
                walks.clear();
            }
        }
        if let Ok(mut names) = self.macro_names.lock() {
            if known_to_the_index {
                names.retain(|_, cached| {
                    !cached
                        .contributors
                        .iter()
                        .any(|file| file.as_str() == wanted.as_str())
                });
            } else {
                names.clear();
            }
        }
        // **And the per-offset states**, which are built by the same walk and invalidated by the same evidence.
        if let Ok(mut states) = self.macro_states_at.lock() {
            if known_to_the_index {
                states.retain(|_, cached| {
                    !cached
                        .contributors
                        .iter()
                        .any(|file| file.as_str() == wanted.as_str())
                });
            } else {
                states.clear();
            }
        }
    }
    /// **Drop every member list one of whose contributors is `path`** — called wherever the index changes for it.
    ///
    /// The comparison is by [`normalize`], for the reason every path comparison in this file is: a summary is filed
    /// under the resolver's spelling (`c:/…`) while an edit arrives with the client's (`C:\…`).
    pub(crate) fn forget_member_lists(&self, path: &Path) {
        let wanted = normalize(path);
        if let Ok(mut known) = self.member_lists.lock() {
            known.retain(|_, cached| {
                !cached
                    .contributors
                    .iter()
                    .any(|file| normalize(file) == wanted)
            });
        }
    }
}

pub(crate) fn members_of_inner(
    shapes: &sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<MemberList> {
    // The spelling as the caller wrote it, kept for the one question the resolved one cannot answer: whether this
    // answer came through an alias at all — see the end of this function.
    let original = base_type_name(class).to_string();
    // **The spelling with its arguments**, kept for the base clauses. The normalization below is about the *class* —
    // which name declares the members — and deliberately drops the arguments; a base clause is about the *type*:
    // `vector<int>`'s base is written `_Vector_base<_Tp, _Alloc>` and substituting it needs to know `_Tp = int`.
    // Measured on the conditional fixture: with the stripped spelling, `conditional_t<_Is_default_allocator<A>::value,
    // …>` kept `A`, so the trait matched no specialization and the wrong branch was taken.
    let with_arguments = class.to_string();
    // The spelling is normalized once, at the entry, for the same reason a base's is: a consumer feeding this from
    // `DeclFact.type_of` hands over what the file wrote — `const Widget&`, `::Widget`, `Base<int>` — and every one
    // of those parts is about the type's shape rather than about which class declares the members. Normalizing
    // here also means `declared_in` is a qualified spelling from the first level on, and levels cannot disagree.
    //
    // **And aliases are followed here as well as one level down**, which the measurement is why for: `direct_members`
    // and `direct_member` resolve the spelling before looking anything up, but the *base walk* below asks `bases_of`
    // about the class as it was written — and an alias has no bases, so `members_of("istream")` walked no bases,
    // recorded no gap, and answered 42 members with `unlisted` **empty**, while the resolved spelling
    // (`std::basic_istream`) answered the same 42 and *named* the gap. Two spellings of one type, one of them
    // claiming a completeness it does not have.
    // **An alias whose target nothing declares is a gap, not a class with no members.** The list comes back with the
    // missing class recorded in `unlisted` — the field that exists to say where an answer stops — so a consumer that
    // must not invent findings (`undefined-field`) stays silent instead of reporting every member of `std::string` as
    // undefined. See [`resolve_aliases_or_unresolved`] for the measurement.
    let (resolved, _arguments, missing) =
        // **With its arguments**, because an alias *template*'s target is substituted by them: `base_type_name` strips
        // them (`ContainerOf<std::vector, int>` → `ContainerOf`), and a stripped alias has no arguments to put into
        // `Container<T, std::allocator<T>>` — so the walk read that target as the class `Container`, answered
        // `unlisted base `Container``, and `ContainerOf<std::vector, int>{}.size()` could not be typed. Measured on
        // `test_cpp`'s `templates.cpp`; the walk itself takes the class *name* after the substitution, so passing the
        // arguments changes nothing for a spelling that is not an alias.
        resolve_aliases_or_unresolved(
            shapes,
            index,
            scopes,
            root,
            path,
            a_type_spelling_with_its_arguments(class),
        );
    if let Some(missing) = missing {
        return Known::Yes(MemberList {
            members: Vec::new(),
            unlisted: vec![UnlistedBase {
                spelling: missing.clone(),
                reason: UnknownReason::NotDeclaredHere(Box::from(missing)),
            }],
        });
    }
    // **An inline namespace is not part of the name a reader writes.** libstdc++ declares the class in `std::__cxx11`
    // (`_GLIBCXX_BEGIN_NAMESPACE_CXX11`) while `std::string`'s alias target is spelled `basic_string<char>` — so once
    // `bits/basic_string.h` is **cooked** (the macro expanded) the scoped lookup for `std::basic_string` answers "not
    // declared" and every `std::string` loses its members: measured on the real project, `std::string` answered 0 with
    // `unlisted: ["std::basic_string"]`, while the *unqualified* lookup found `std::__cxx11::basic_string` at once.
    //
    // The retry is by the **last segment only** — the name the class is declared under, whatever namespaces wrap it —
    // and it runs only when the scoped spelling missed, so a class that is where the spelling says is unaffected.
    // **The class *name*, which is what the index keys.** The walk above is given the spelling with its arguments, so
    // that an alias template's target can be substituted by them — but a member lookup is by name (`Box`, not
    // `Box<int>`), and the arguments the use wrote are applied to the *members* one layer up, in
    // [`crate::index::project::substituted_members`]. Measured: with the arguments kept here, `members_of("Box<int>")`
    // answered `Unknown(NotDeclaredHere("Box<int>"))` and four tests in `tests/types.rs` failed.
    let mut class = base_type_name(&resolved).to_string();
    let own = match direct_members(shapes, index, scopes, root, path, &class) {
        Known::Yes(members) => members,
        Known::Unknown(UnknownReason::NotDeclaredHere(_)) => {
            let last = without_the_namespace(&class);
            if last == class {
                return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class)));
            }
            match direct_members(shapes, index, scopes, root, path, &last) {
                Known::Yes(members) => {
                    class = last;
                    members
                }
                Known::Unknown(reason) => return Known::Unknown(reason),
                Known::No => {
                    return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class)));
                }
            }
        }
        Known::Unknown(reason) => return Known::Unknown(reason),
        // `direct_members` reports a name nothing declares as `Unknown(NotDeclaredHere)` rather than as `No` —
        // the index is a subset of the translation unit — so this arm exists for totality, not for a case.
        Known::No => return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class))),
    };

    // **A namespace alias answers with its target's members.** `namespace views = ranges::views;` is how libstdc++ spells
    // `std::views`, and the index records it as a `Namespace` whose `type_of` is the target — not as a type alias, so
    // `resolve_aliases_or_unresolved` above steps over it. Measured: `std::ranges::views` answered **100** members and
    // `std::views` answered **0**, with nothing in `unlisted` to say why.
    let mut own = own;
    if own.is_empty()
        && let Some(target) = a_namespace_aliass_target(index, path, &class)
        && let Known::Yes(found) = direct_members(shapes, index, scopes, root, path, &target)
        && !found.is_empty()
    {
        own = found;
        class = target;
    }

    let mut list = MemberList::default();
    let mut hidden: HashSet<String> = HashSet::new();
    let mut visited: HashSet<String> = HashSet::from([class.to_string()]);

    // Level 0 is the type's own body. It is not part of the walk below because its members are the ones that
    // *hide*, and a base is only reached after them.
    let mut own: Vec<ProjectMember> = own
        .into_iter()
        .map(|(file, fact)| ProjectMember {
            file,
            fact,
            declared_in: class.to_string(),
            depth: 0,
            ambiguous: false,
        })
        .collect();
    own.sort_by(|one, other| one.fact.name.cmp(&other.fact.name));
    mark_ambiguity(&mut own);
    hidden.extend(recorded_names(&own));
    list.members.extend(own);

    // Then outward, one level of bases at a time — which is the order C++ hides in: a base's member is hidden by
    // a same-named member of anything nearer, so a level that has already contributed a name removes it from
    // every level below.
    //
    // Each base is carried with **the class whose base-clause wrote it**, because that is what decides which scope
    // an unqualified base name is looked up in: see `resolved_in_the_enclosing_scopes`.
    //
    // The class's **own** base list is the one case the walk below cannot report, and it is reported here instead,
    // exactly as a base's is: a class whose bases cannot be read is a truncated list, not a class with no bases.
    // Measured, and it is why the two look different from outside: MSVC's `<istream>` declares `std::basic_istream`
    // **three** times (the class, then `template class _CRTIMP2_PURE_IMPORT basic_istream<char, …>;` twice under
    // `#if defined(_DLL_CPPLIB)`), an ambiguous name answers no base list, and the members inherited from
    // `basic_ios` — `eof`, `fail`, `clear`, … — vanished from the list while `unlisted` stayed **empty**, so the
    // answer did not say it was incomplete.
    // **A template parameter is not a base class.** A base clause is read into *names a class can be looked up by*,
    // and `_Vector_base<_Tp, _Alloc>` produces three of them — the class and both of its arguments — because a base
    // written as `conditional_t<…>` really is one of the names *inside* it (see [`lookup_names`]). The arguments of an
    // ordinary class template are not, and treating them as bases is expensive: `_Tp` is the name of a parameter in
    // hundreds of declarations in a standard-library closure, so the walk asked "who declares `_Tp`" over the whole
    // index — measured on `LuaSource.cpp`, `members_of("std::vector")` called that question twice for **251 ms and
    // 88 ms**, 96% of a member list whose own members cost 1 ms. The rule that removes them is not a heuristic: C++
    // makes a template parameter shadow any name of its class, so a parameter of *this* class can never be its base.
    let parameters = class_template_parameters(shapes, index, scopes, root, path, &class);

    // **The base's written spelling, arguments and all.** `bases_of` answers *names a class can be looked up by* — the
    // arguments are dropped there on purpose (they are not bases, and treating one as a base was measured at 251 ms on
    // `std::vector`) — while a member inherited from a **template** base is written in the base's parameters:
    // `enable_shared_from_this<Node>::shared_from_this` returns `_Tp`, and without the arguments nothing downstream can
    // finish it. `raw_bases` reads the clause itself, and the level keeps the spelling.
    let mut level: Vec<(String, String)> =
        match the_base_clauses_of(shapes, index, scopes, root, path, &class) {
            Known::Yes(bases) => bases
                .into_iter()
                .filter(|written| !names_a_template_parameter(base_type_name(written), &parameters))
                // **The first level's clauses are substituted too.** Every later level already went through
                // [`substitute_a_base_clause`] — `_Vector_base<_Tp, _Alloc>` inside `vector<int>` is written in the
                // level above — but the class's *own* base clauses were used as written. Measured on the conditional
                // fixture: `allocator_traits<allocator<int>> : conditional_t<_Is_default_allocator<A>::value, …>`
                // kept `A`, so the trait was asked about the parameter instead of `allocator<int>`, matched no
                // specialization, and took the primary `false_type` branch — `size_type` came from
                // `_Normal_allocator_traits` where the right answer was `_Default_allocator_traits`'s.
                .map(|written| substitute_a_base_clause(index, path, &with_arguments, &written))
                // **A clause that is not a type is a gap, not a level** — the same rule the walk below applies, and
                // it has to be applied here too: `std::tuple`'s own base is `_Tuple_impl<0, _Elements...>`, so this is
                // the clause that started the unbounded walk. See [`why_a_clause_cannot_be_walked`].
                .filter(
                    |substituted| match why_a_clause_cannot_be_walked(substituted) {
                        Some(reason) => {
                            list.unlisted.push(UnlistedBase {
                                spelling: substituted.clone(),
                                reason,
                            });
                            false
                        }
                        None => true,
                    },
                )
                .map(|substituted| (substituted, class.to_string()))
                .collect(),
            Known::Unknown(reason) => {
                list.unlisted.push(UnlistedBase {
                    spelling: class.to_string(),
                    reason,
                });
                Vec::new()
            }
            Known::No => Vec::new(),
        };
    let mut depth = 1;

    while !level.is_empty() {
        let mut found: Vec<ProjectMember> = Vec::new();
        let mut next: Vec<(String, String)> = Vec::new();

        for (written, owner) in level {
            // **The head is resolved where the base clause was written; the arguments are kept as written.**
            let head = base_type_name(&written).to_string();
            let base = resolved_in_the_enclosing_scopes(index, scopes, path, &owner, &head);
            // The spelling a reader sees and the one the members are substituted by: the resolved head with the
            // arguments the clause wrote.
            let shown = match written.find('<') {
                Some(at) => format!("{base}{}", &written[at..]),
                None => base.clone(),
            };
            if !visited.insert(shown.clone()) {
                continue;
            }
            // **Every level, when the walk is being read.** A member list that never returns is a walk whose *spellings*
            // never repeat, and the spellings are the thing to look at: `_Tuple_impl<0, _Elements...>` is a base that
            // becomes `_Tuple_impl<0 + 1, _Elements...>` one level down, which is a new type every time and therefore an
            // unbounded number of levels. `CPPLS_TRACE_MEMBERS=1`.

            // **A base clause whose head is an *alias template* is not a class.** `conditional_t<…>` is a Type fact
            // with a `type_of` target and no members, so `direct_members("conditional_t")` answered an **empty class**
            // and the clause contributed nothing at all — the `Unknown` arm below, which is where the static resolver
            // is asked, was never reached. Measured on `tests/types.rs`' conditional fixture: the member list of
            // `std::allocator_traits<allocator<int>>` came from `_Normal_allocator_traits` — one of the names inside
            // the clause, picked by the other base path — instead of the branch the trait selects.
            let head_is_an_alias = index
                .files_declaring(&head, path)
                .into_iter()
                .any(|declared| {
                    declared.fact.kind == DeclKind::Type && declared.fact.type_of.is_some()
                });
            if head_is_an_alias {
                let candidates = the_classes_a_base_clause_names(index, path, &written);
                for one in candidates.into_iter().filter(|one| !visited.contains(one)) {
                    next.push((one, owner.clone()));
                }
                continue;
            }

            let members = match direct_members(shapes, index, scopes, root, path, &base) {
                Known::Yes(members) => members,
                // **A clause that is a *type* rather than a class name.** `allocator_traits<_Alloc> : conditional_t<…>`
                // names no class: `conditional_t` is an alias whose target is `typename conditional<…>::type`. The
                // resolution is a static one — facts only, no member list — because the obvious tool,
                // `a_spelling_through_the_aliases`, asks member questions, a member question builds a member list,
                // and a member list walks bases: calling it here was measured as a **stack overflow**.
                //
                // What comes back is a **list**, because `conditional<Cond, A, B>::type` is one of two classes and
                // nothing here can evaluate the condition. Both are walked, and `mark_ambiguity` marks every member
                // the two disagree about — so a member both branches declare with one type is answered, and one only
                // one branch declares is not invented. See `docs/open-questions.md`, the `size_type` entry.
                Known::Unknown(reason) => {
                    let candidates = the_classes_a_base_clause_names(index, path, &written);
                    let fresh: Vec<String> = candidates
                        .into_iter()
                        .filter(|one| !visited.contains(one))
                        .collect();
                    if !fresh.is_empty() {
                        for one in fresh {
                            next.push((one, owner.clone()));
                        }
                        continue;
                    }
                    list.unlisted.push(UnlistedBase {
                        spelling: shown,
                        reason,
                    });
                    continue;
                }
                Known::No => continue,
            };

            // **The base's arguments, applied to its members** — the same substitution the class's own members go
            // through, aliases included. `shown` is a template-id exactly when the clause wrote arguments, and
            // `substituted_members` does nothing for a plain class.
            let listed = MemberList {
                members: members
                    .into_iter()
                    .map(|(file, fact)| ProjectMember {
                        file,
                        fact,
                        declared_in: shown.clone(),
                        depth,
                        ambiguous: false,
                    })
                    .collect(),
                unlisted: Vec::new(),
            };
            let Known::Yes(listed) = substituted_members(index, path, &shown, Known::Yes(listed))
            else {
                continue;
            };
            found.extend(listed.members);

            // **The next level is substituted too**: a base of a base is written in the base's parameters
            // (`_Vector_base<_Tp, _Alloc>` inside `vector<_Tp>`), and those are this level's arguments.
            let further = the_base_clauses_of(shapes, index, scopes, root, path, &base);
            match further {
                Known::Yes(clauses) => next.extend(
                    clauses
                        .into_iter()
                        .map(|clause| substitute_a_base_clause(index, path, &shown, &clause))
                        .filter(|clause| {
                            !names_a_template_parameter(base_type_name(clause), &parameters)
                        })
                        // **The same rule as the first level** — see [`why_a_clause_cannot_be_walked`], which is
                        // where the measurement that makes this necessary is written down.
                        .filter_map(|clause| match why_a_clause_cannot_be_walked(&clause) {
                            Some(reason) => {
                                list.unlisted.push(UnlistedBase {
                                    spelling: clause,
                                    reason,
                                });
                                None
                            }
                            None => Some((clause, base.clone())),
                        }),
                ),
                Known::Unknown(reason) => list.unlisted.push(UnlistedBase {
                    spelling: base,
                    reason,
                }),
                Known::No => {}
            }
        }

        found.retain(|member| !hidden.contains(member.fact.name.as_str()));
        found.sort_by(|one, other| one.fact.name.cmp(&other.fact.name));
        mark_ambiguity(&mut found);
        hidden.extend(recorded_names(&found));
        list.members.extend(found);

        level = next;
        depth += 1;
    }

    // **An alias that resolved to a class with no members at all is a gap, not an empty class.**
    //
    // `std::string` is `typedef basic_string<char> string;` inside libstdc++'s `inline namespace __cxx11`, and the
    // class's facts are keyed `std::__cxx11::basic_string`. The alias walk lands on `std::basic_string` — a name the
    // forward declaration does declare — and `direct_members` finds no members under it, so the list came back
    // **empty and complete** and `undefined-field` reported every member of a `std::string` as undefined (measured:
    // ten findings on `CodeFormatCLib/src/CodeFormat.cpp`). The index cannot see that `__cxx11` is inline, so the
    // honest answer is "the answer stops here" — `unlisted` — which is the field that exists for exactly this.
    if list.members.is_empty() && list.unlisted.is_empty() && class != base_type_name(&original) {
        list.unlisted.push(UnlistedBase {
            spelling: class.to_string(),
            reason: UnknownReason::NotDeclaredHere(Box::from(class.to_string())),
        });
    }

    Known::Yes(list)
}

/// **The base clauses a class was written with** — `std::enable_shared_from_this<Node>`, arguments and all.
///
/// `bases_of` answers the *names* a class can be looked up by, which is the right answer for the walk's lookups and the
/// wrong one for a member's type: a member inherited from a template base is written in that base's parameters, and a
/// name without its arguments cannot say what they are. This reads the clause itself — from the file's own tree when the
/// class is in it, and from the fact the index holds otherwise — and falls back to `bases_of` when neither answers, so the
/// walk is never worse than it was.
pub(crate) fn the_base_clauses_of(
    shapes: &sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<Vec<String>> {
    if let Some(scope) = scopes.scope_with_qualified_name(class)
        && let Some(data) = scopes.scope(scope)
        && let Some(parent) = data.parent
        && let Some(name) = class.rsplit("::").next()
        && let Some(binding) = scopes.scope(parent).and_then(|scope| {
            scope
                .bindings
                .iter()
                .find(|binding| binding.name.identifier_text() == Some(name))
        })
    {
        return Known::Yes(sema::declarations::declared_bases_of_with(
            sema::declarations::shapes_from(shapes, root),
            binding,
        ));
    }
    let base = base_type_name(class).to_string();
    // **The definition, not the forward declaration.** A class template is declared and then defined — MSVC's
    // `<xmemory>` forward-declares `allocator_traits` at line 554 and defines it at 755 — and the two facts are the
    // same entity written twice, with **only the second carrying the base clause**. Taking the first Type fact
    // answered `bases: []` for a class whose whole body is its base, so `size_type` was in no member list:
    // measured on `EmmyLuaCodeStyle`, `members_of("std::allocator_traits")` answered `0 listed, 0 unlisted` and
    // `auto pos = _events.size();` stopped at `_Alty_traits::size_type`.
    //
    // A definition is what the language calls the class ([basic.def]: a declaration is a definition unless it is
    // only a name), so the rule needs no preference beyond the fact's own flag. A name with no definition keeps the
    // first declaration, which is all there is.
    let candidates = index.files_declaring(&base, path);
    let is_a_type = |declared: &VisibleDeclaration<'_>| declared.fact.kind == DeclKind::Type;
    let chosen = candidates
        .iter()
        .position(|declared| is_a_type(declared) && declared.fact.definition)
        .or_else(|| candidates.iter().position(is_a_type));
    if let Some(at) = chosen {
        return Known::Yes(candidates[at].fact.bases().to_vec());
    }
    bases_of(index, scopes, root, path, class)
}

/// **Why a substituted base clause cannot be walked into** — the two shapes substitution produces that are not types.
///
/// The base walk terminates on a **repeated spelling**: a level is skipped when `visited` already holds it. That is a
/// proof of termination only if the spellings a level can produce are finite, and substitution can produce an *unbounded
/// family* of them:
///
/// ```text
/// level 1  std::_Tuple_impl<0, _Elements...>
/// level 2  std::_Tuple_impl<0 + 1, _Tail...>
/// level 3  std::_Tuple_impl<0 + 1 + 1, _Tail...>
/// level 4  std::_Tuple_impl<0 + 1 + 1 + 1, _Tail...>     …
/// ```
///
/// — the log of `members_of("std::tuple")` on `test_cpp/src/containers.cpp`, one `_Head_base<0 + 1 + 1, _Head>` beside
/// each level. The two reasons the walk cannot stop are also the two reasons it should never have started:
///
/// * **`_Elements...` is a pack with nothing to expand it.** The clause names a type per argument, and a class asked
///   about **without** its arguments has none: no level of this walk will ever supply them, so the clause is unwritable
///   at every depth.
/// * **`0 + 1` is an expression where a type belongs.** `_Idx + 1` was substituted as *text*, because this layer does
///   not constant-fold, and each level then writes one more `+ 1` into the spelling. (Substitution joins with spaces, so
///   the operator is spelled ` + `; no *type* argument has one.)
///
/// Both are recorded as a gap — [`UnlistedBase`] is the field that exists to say where an answer stops — instead of
/// being walked. This is not a bound on the work: it is the observation that the clause names nothing, so there is no
/// level to enter. Measured before it: `inlayHint` on a nine-line file containing `std::tuple tuple{1, 2.5, s}` never
/// returned (120 s, the client's deadline) and the server reached **2 GB**; `members_of("std::tuple")` was still walking
/// **ten minutes** in with **843 MB** held and no answer.
fn why_a_clause_cannot_be_walked(clause: &str) -> Option<UnknownReason> {
    if clause.contains("...") {
        return Some(UnknownReason::UnknownType(Box::from(clause)));
    }
    const OPERATORS: [&str; 6] = [" + ", " - ", " * ", " / ", " % ", " << "];
    if OPERATORS.iter().any(|operator| clause.contains(operator)) {
        return Some(UnknownReason::UnknownType(Box::from(clause)));
    }
    None
}

/// **The one file a class's own body is written in** — `None` when nothing the index can see declares it.
///
/// The question a member list asks when several files answer with facts whose `scope` is the class's name. That
/// happens for exactly one reason in C++: a **partial specialization of a class template shares the primary's name**,
/// so `std::vector`'s members are written in `stl_vector.h` and `std::vector<bool>`'s in `stl_bvector.h` while both
/// file every fact under the string `std::vector`. Measured: `vector.begin()` hovered with four declarations, two of
/// them the `vector<bool>` specialization's — the primary's `begin()` and `begin() const` twice.
///
/// The choice is the index's own rule for "which declaration of a type does a use mean"
/// ([`ProjectIndex::definitions`]: a **definition** beats a forward declaration, and among several definitions the one
/// with the **most template parameters** is the primary template). No new rule is invented here; it is the same one
/// [`definition_across_files`] applies, asked for its *file*.
///
/// `None` is not "no body": it is "the index cannot name the class", and the caller then keeps every fact it was given
/// rather than emptying a member list over a spelling the index does not hold.
fn the_file_of_a_class_body(index: &ProjectIndex, path: &Path, class: &str) -> Option<String> {
    let decl = index.definitions(class, path).value()?;
    let mut types: Vec<&ProjectDefinition> = decl
        .found
        .iter()
        .filter(|one| one.fact.kind == DeclKind::Type)
        .collect();
    if types.is_empty() {
        return None;
    }
    // A definition beats a declaration, and the general template beats a specialization.
    types.sort_by_key(|one| {
        (
            !one.fact.definition,
            std::cmp::Reverse(one.fact.parameters().len()),
        )
    });
    Some(normalize(&types[0].file))
}

/// [`direct_members`]' answer, **restricted to the class body the spelling names** — see [`the_file_of_a_class_body`].
/// **A base clause with the current level's arguments substituted into it** — `_Vector_base<_Tp, _Alloc>` asked of
/// `vector<int>` is `_Vector_base<int, allocator<int>>`.
///
/// The parameters come from the declaration of the class the clause was written in, and the arguments from `shown`;
/// when either is missing the clause is returned unchanged, which is the direction that keeps the walk going.
pub(crate) fn substitute_a_base_clause(
    index: &ProjectIndex,
    path: &Path,
    shown: &str,
    clause: &str,
) -> String {
    let Some((head, mut arguments)) = split_template_arguments(shown) else {
        return clause.to_string();
    };

    // **The class is keyed by its bare name.** The spelling here is often qualified — `std::allocator_traits<…>` —
    // and `files_declaring` matches the name a fact records, which is the bare one. A qualified lookup therefore
    // found nothing and the clause came back with its parameters still standing: measured on the conditional
    // fixture, `A` was never replaced by `allocator<int>`, so the trait was asked about the parameter, matched no
    // specialization, and the base was the wrong `conditional` branch.
    let mut candidates = index.files_declaring(&head, path);
    if candidates.is_empty() {
        let bare = head.rsplit("::").next().unwrap_or(&head).to_string();
        candidates = index.files_declaring(&bare, path);
    }
    let Some(declared) = candidates
        .iter()
        .find(|declared| declared.fact.kind == DeclKind::Type)
    else {
        return clause.to_string();
    };
    // **The arguments written are the first parameters, in order** — the same positional rule
    // `substituted_members` applies for a defaulted parameter list.
    let mut names = declared.fact.parameters().to_vec();
    names.truncate(arguments.len());
    arguments.truncate(names.len());
    if names.is_empty() {
        return clause.to_string();
    }

    sema::types::substituted_spelling(clause, &names, &arguments)
}

/// Every declaration written **directly in** the class or namespace `class` names.
///
/// The class's own body first, from the file being edited, and then the index — the same two-layer split
/// [`direct_member`] makes, and for the same reason: a buffer that has never been saved has no summary, and a
/// class the buffer does not mention is only in the index.
///
/// [`Known::Yes`] with an empty list is a real answer — a class with nothing in it — and is deliberately not the
/// same as [`Known::Unknown`], which is what a name nothing declares produces. The two are told apart by asking
/// whether the *name* is declared, which is the one question that distinguishes "nothing written in it" from
/// "nothing here knows what it is".
///
/// A name that is not an identifier — a destructor, an operator, a conversion function — comes back with an empty
/// [`DeclFact::name`], exactly as the index stores it. It is a declaration that exists, so it is not dropped here;
/// a consumer that shows a list filters on the name it can print and reads the spelling from the source, which is
/// where it lives.
/// **The class — or the two classes — a base clause names, when the clause is a type rather than a class name.**
///
/// `allocator_traits<_Alloc> : conditional_t<…>` is the shape: `conditional_t` is an alias template whose target is
/// `typename conditional<Cond, A, B>::type`, and the base is one of `A`/`B`. Two steps are enough for the standard
/// library's spellings — an alias template, then a member type — and the loop is bounded by
/// [`MAX_ALIAS_HOPS`] regardless, because a library can write `using A = typename B<T>::type;` twice.
///
/// # Facts only, and why that is a rule rather than a preference
///
/// This runs **inside** the base walk of a member list. Asking [`a_spelling_through_the_aliases`] here — the function
/// that exists to turn such a spelling into a type — asks member questions, a member question builds a member list,
/// and a member list walks bases: measured as `STATUS_STACK_OVERFLOW` on
/// `tests/types.rs::an_alias_template_used_as_a_base_substitutes_its_arguments`. So this reads only facts:
/// `files_declaring` for a class's template parameters, `declarations_in` for a member's recorded type, and
/// substitutions in between.
///
/// # `conditional`, and why it answers two classes
///
/// `conditional<Cond, A, B>::type` is `A` or `B`, decided by a condition that is a library trait over a template
/// parameter — `_Is_default_allocator<_Alloc>::value` is not something this layer can evaluate, and picking the
/// primary template's `_Ty1` would be a guess that is wrong for every custom allocator. Both are returned instead:
/// the caller walks both, and a member the two disagree about is marked ambiguous, which is the same rule
/// [`the_return_the_overloads_agree_on`] applies to overloads.
pub(crate) fn the_classes_a_base_clause_names(
    index: &ProjectIndex,
    path: &Path,
    clause: &str,
) -> Vec<String> {
    let mut spelling = clause.trim().to_string();

    for _ in 0..MAX_ALIAS_HOPS {
        let without_typename = spelling
            .strip_prefix("typename ")
            .unwrap_or(&spelling)
            .trim()
            .to_string();

        // **A member type** — `Class<…>::member`.
        if let Some((class, member)) = split_at_the_last_scope(&without_typename) {
            let (head, arguments) = match split_template_arguments(&class) {
                Some((head, arguments)) => (head, arguments),
                None => (class.trim().to_string(), Vec::new()),
            };
            let bare = head.rsplit("::").next().unwrap_or(&head).trim();
            if bare == "conditional" && member.trim() == "type" && arguments.len() >= 3 {
                // **The condition decides the branch, when it can be read.** `_Is_default_allocator<_Alloc>` is a
                // class template with a `true_type`/`false_type` base, and its `::value` is which one it inherits —
                // decidable whenever the use's argument matches one of its specializations, which is what
                // [`the_bool_a_trait_has`] asks. When it cannot be decided the two branches are both walked, and
                // only the members they agree on survive (`mark_ambiguity`).
                return match the_bool_a_trait_has(index, path, arguments[0].trim()) {
                    Some(true) => vec![arguments[1].clone()],
                    Some(false) => vec![arguments[2].clone()],
                    None => vec![arguments[1].clone(), arguments[2].clone()],
                };
            }
            let parameters = index
                .files_declaring(&head, path)
                .into_iter()
                .find(|declared| declared.fact.kind == DeclKind::Type)
                .map(|declared| declared.fact.parameters().to_vec())
                .unwrap_or_default();
            let scope = index
                .definition(bare, path)
                .value()
                .map(|found| found.fact.qualified_name())
                .unwrap_or_else(|| head.clone());
            let Some(fact) = index
                .declarations_in(&scope, path)
                .into_iter()
                .find(|one| one.fact.name == member.trim())
            else {
                return Vec::new();
            };
            let Some(target) = fact.fact.type_of.clone() else {
                return Vec::new();
            };
            let substituted = sema::types::substituted_spelling(&target, &parameters, &arguments);
            if substituted == spelling {
                return Vec::new();
            }
            spelling = substituted;
            continue;
        }

        // **An alias template** — `Head<…>` whose fact records a target.
        let Some((head, arguments)) = split_template_arguments(&without_typename) else {
            return Vec::new();
        };
        let declared = index.files_declaring(&head, path);
        if let Some(alias) = declared.iter().find(|declared| {
            declared.fact.kind == DeclKind::Type && declared.fact.type_of.is_some()
        }) {
            let Some(target) = alias.fact.type_of.clone() else {
                return Vec::new();
            };
            let substituted =
                sema::types::substituted_spelling(&target, alias.fact.parameters(), &arguments);
            if substituted == spelling {
                return Vec::new();
            }
            spelling = substituted;
            continue;
        }
        // **The chain ends at a class**, which is the answer — `chosen<int>` is a base, not an alias.
        if declared
            .iter()
            .any(|declared| declared.fact.kind == DeclKind::Type)
        {
            return vec![without_typename];
        }
        return Vec::new();
    }

    Vec::new()
}

/// **The `bool` a trait names** — `_Is_default_allocator<allocator<int>>::value` is `true`, and the answer is the
/// base the matching specialization inherits.
///
/// The standard library decides `conditional_t` branches with traits like this one:
///
/// ```cpp
/// template <class _Ty> struct _Is_default_allocator : false_type {};
/// template <class _Ty> struct _Is_default_allocator<allocator<_Ty>> : true_type {};
/// ```
///
/// Two facts answer it, and both come from the index: [`a_pattern_matches`] picks the declaration whose **pattern**
/// fits the argument the use wrote (the specialization, when the allocator is `allocator<T>`), and the base that
/// declaration inherits says the value. Facts only — this runs inside a base walk, and a member query from here is the
/// cycle recorded in `docs/open-questions.md`.
///
/// `None` when the spelling is not a `::value` of a class template, when no declaration matches, or when the matched
/// declaration does not inherit `true_type`/`false_type`: a trait the layer cannot read is not a `false`.
pub(crate) fn the_bool_a_trait_has(
    index: &ProjectIndex,
    path: &Path,
    spelled: &str,
) -> Option<bool> {
    let (class, member) = split_at_the_last_scope(spelled)?;
    if member.trim() != "value" {
        return None;
    }
    let (head, arguments) = split_template_arguments(&class)?;
    let facts = index.files_declaring(&head, path);
    let declarations: Vec<&VisibleDeclaration<'_>> = facts
        .iter()
        .filter(|declared| declared.fact.kind == DeclKind::Type)
        .collect();

    let matches = |declared: &&&VisibleDeclaration<'_>| {
        declared.fact.pattern().is_some()
            && a_pattern_matches(
                declared.fact.pattern(),
                declared.fact.parameters(),
                Some(&arguments),
            )
    };
    let chosen = declarations
        .iter()
        .find(matches)
        .or_else(|| {
            declarations
                .iter()
                .find(|declared| declared.fact.pattern().is_none())
        })
        .copied()?;

    for base in chosen.fact.bases() {
        match base.rsplit("::").next().unwrap_or(base).trim() {
            "true_type" => return Some(true),
            "false_type" => return Some(false),
            _ => {}
        }
    }
    None
}

pub(crate) fn direct_members(
    shapes: &sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<Vec<(PathBuf, DeclFact)>> {
    // Followed here as well as in `direct_member`, for the same reason it is there: the members of `std::string`
    // are the members of `std::basic_string`, and the member-list query and the single-member query must not
    // disagree about which class a spelling names.
    //
    // **…but the literal spelling is asked first**, because an alias's target can be a spelling no scope has:
    // `typedef struct { int a; int b; } Object;` files its members under **`Object`** — an untagged struct's only name
    // is the typedef's — while `resolve_aliases` turns that spelling into the struct's own *text*
    // (`struct { int a; int b; }`), which names nothing. Measured with `tests/anon_struct_probe.rs`: the facts existed
    // under `Object` (2 postings, 2 visible) and `members_of("Object")` answered an **empty list**. One lookup here
    // costs nothing and keeps the alias path for the shapes it exists for.
    let class = if scopes.scope_with_qualified_name(class).is_some() {
        class.to_string()
    } else {
        resolve_aliases(shapes, index, scopes, root, path, class)
    };
    // **A namespace alias in the spelling's prefix** — `std::views::_Filter` is `std::ranges::views::_Filter`. Type aliases
    // are followed above; a namespace alias is not a type, so it needs this step, and it is *additive*: a spelling that
    // already resolves is unchanged.
    let class = through_a_namespace_alias(index, path, &class);
    let class = &class;

    if let Some(scope) = scopes.scope_with_qualified_name(class)
        && let Some(data) = scopes.scope(scope)
    {
        // **The file's shapes once, not once per member.** See [`fact_from_binding_with`] for the measurement: this
        // list is 151 members of `std::vector` on libstdc++, and the per-member form built the table 151 times.
        let shapes = sema::declarations::shapes_from(shapes, root);
        return Known::Yes(
            data.bindings
                .iter()
                .map(|binding| {
                    (
                        path.to_path_buf(),
                        fact_from_binding_with(shapes, root, class, binding),
                    )
                })
                .collect(),
        );
    }

    // **A class nested inside another one, spelled without its outer class.**
    //
    // `class outer { public: class inner { … }; };` declares a class whose qualified name is `outer::inner`, while
    // every use written inside `outer` spells it `inner` — and so does every parameter list there:
    // `bool operator!=(const inner &other) const`. The lookup above asks the table for the bare spelling, which is
    // not a key, so the answer fell through to the index, where `definition("inner")` is **Ambiguous**: the class is
    // `inner` in scope `outer`, and its constructor is a declaration named `inner` in scope `outer::inner`. An
    // ambiguous class answered `Known::Yes(empty)` — "that class has nothing written in it" — and `undefined-field`
    // therefore reported **every member** of the class: measured, `` `other` has no member `at_end_` `` on
    // `coroutine.cpp`, whose `generator<T>::iterator` is exactly this shape, and a completion on such a class
    // offered its constructor and nothing else.
    //
    // The scope is in the table and keyed correctly; only the spelling differs, so one scan for a class scope whose
    // last segment is this name answers it. Two such classes is a real ambiguity and answers nothing.
    if let Some(scope) = a_class_nested_in_another(scopes, class)
        && let Some(data) = scopes.scope(scope)
    {
        let shapes = sema::declarations::shapes_from(shapes, root);
        let qualified = scopes
            .qualified_name_of(scope)
            .unwrap_or_else(|| class.to_string());
        return Known::Yes(
            data.bindings
                .iter()
                .map(|binding| {
                    (
                        path.to_path_buf(),
                        fact_from_binding_with(shapes, root, &qualified, binding),
                    )
                })
                .collect(),
        );
    }

    let found = index.declarations_in(class, path);
    if !found.is_empty() {
        // **A partial specialization's members are not the primary template's.** Both write `scope: "std::vector"`
        // into their facts — the scope is the class's *qualified name*, and a specialization of a class template has
        // the primary's name — so `declarations_in` answers with the union of two class bodies when a header in the
        // closure declares a specialization. Measured on `test_cpp/src/containers.cpp`, `vector.begin()`: the hover
        // card listed **four** declarations, `begin()`/`begin() const` twice each, the second pair from
        // `stl_bvector.h`'s `vector<bool>` specialization.
        //
        // A class body is written in exactly one place, so the fix is the body the class's own declaration names: see
        // [`the_file_of_a_class_body`], whose rule is the index's existing "a type is the definition, and among several
        // the one with the most template parameters".
        let own_body = the_file_of_a_class_body(index, path, class);
        let found: Vec<_> = match &own_body {
            Some(body) => found
                .into_iter()
                .filter(|declaration| normalize(&declaration.file) == *body)
                .collect(),
            None => found,
        };
        return Known::Yes(
            found
                .into_iter()
                .map(|declaration| (declaration.file.clone(), declaration.fact.clone()))
                .collect(),
        );
    }

    // **The class spelling a use writes is not the key its facts are stored under, and asking again with the key is
    // not a guess.**
    //
    // `declarations_in` matches `fact.scope == class` **exactly**, and the two strings come from different places:
    // the spelling is what the *use* wrote — for `_Repptr._Lock_and_load()`, the object's type as the declaration in
    // this file spells it, `_Locked_pointer` — while a fact's `scope` is the class's **fully qualified** name,
    // `std::_Locked_pointer`, because MSVC's headers open their namespace with the `_STD_BEGIN` macro.
    //
    // Measured: ten `auto` declarations in `<memory>` were refused as `NotDeclaredHere`, seven of them for
    // `_Locked_pointer::_Lock_and_load` — and the fact was there all along with `scope=std::_Locked_pointer`,
    // `qualified=std::_Locked_pointer::_Lock_and_load`, and the lookup asking for a name without the `std::`.
    //
    // So the class is **resolved**, the same way go-to-definition resolves it, and the retry uses the qualified name
    // its own declaration gives. That is the difference between this and a bare-name fallback: a bare name would
    // answer with a `_Locked_pointer` from anywhere, while this asks *which class the spelling names* and then uses
    // the answer's own identity. The level walk above is unaffected — it works on facts, which are already keyed
    // correctly.
    //
    // **The template arguments come off first**, and they have to: a use writes the type of its object, so the class
    // arrives as `_Locked_pointer<_Ty>` — and a *declaration* is never keyed by that spelling. Measured on the same
    // name: `_Locked_pointer` resolves to `std::_Locked_pointer` while `_Locked_pointer<_Ty>` answers
    // `NotDeclaredHere`, which is why a retry that skipped this step changed nothing at all. The arguments are what
    // [`super::member_bindings`] pairs with the class's parameters *after* the class is found; they are not part of
    // its name.
    if let Known::Yes(resolved) = index.definition(without_arguments(class), path) {
        let qualified = resolved.fact.qualified_name();
        if qualified != *class {
            let found = index.declarations_in(&qualified, path);
            if !found.is_empty() {
                return Known::Yes(
                    found
                        .into_iter()
                        .map(|declaration| (declaration.file.clone(), declaration.fact.clone()))
                        .collect(),
                );
            }
        }
    }

    // Nothing is written *in* it, so the name is either an empty class or no class at all. Which one is decided by
    // the declaration itself rather than by the absence of members: a fact whose own qualified name is the
    // spelling asked about is the class, and everything else that matched did so on its bare name.
    //
    // **`Ambiguous` is that same answer**, and it is the ordinary case for the standard library: a class
    // template is forward-declared and then defined, and the two declarations are one entity written twice.
    // `std::allocator_traits` is exactly this — `xmemory:554` forward-declares it and `755` defines it — and
    // the class declares **no members of its own at all**, because every one of them is inherited from
    // `_Normal_allocator_traits`. Answering `Ambiguous` here says "I cannot tell which class you mean", when
    // the truthful answer is "that class, and it has nothing written directly in it": the members are one
    // level out, which is what the base walk above is for.
    match index.definition(class, path) {
        Known::Yes(found) if found.fact.qualified_name() == *class => Known::Yes(Vec::new()),
        // **One class written twice is not two classes.** A class template that is forward-declared and then
        // defined is two declarations and one entity — `std::shared_ptr` is declared in `bits/shared_ptr.h` and
        // defined in `shared_ptr.h` — and the letter of "ambiguous" answered *"that class has nothing written in
        // it"*. That answer was invisible until the base walk found something: a member list that was **empty** was
        // retried with the qualified spelling by [`members_of_with`], but a list with a few *inherited* members in
        // it was not, so a bare `shared_ptr<int>` answered 9 members and no `reset` while `std::shared_ptr<int>`
        // answered 50 — reported by an editor as "`shared_ptr<int>` has no member `reset`".
        //
        // The rule is the one `agreeing_type` already applies to a declaration's type: when every visible
        // declaration of the name agrees on the **qualified** name, that name is the class, and its members are
        // asked for by that name. A name whose declarations live in two different classes stays ambiguous.
        Known::Unknown(UnknownReason::Ambiguous(_)) => {
            if let Known::Yes(found) = index.definitions(class, path) {
                let names: BTreeSet<String> = found
                    .found
                    .iter()
                    .filter(|declaration| declaration.fact.kind == DeclKind::Type)
                    .map(|declaration| declaration.fact.qualified_name())
                    .collect();
                if let [qualified] = names.iter().collect::<Vec<_>>().as_slice() {
                    let found = index.declarations_in(qualified, path);
                    if !found.is_empty() {
                        return Known::Yes(
                            found
                                .into_iter()
                                .map(|declaration| {
                                    (declaration.file.clone(), declaration.fact.clone())
                                })
                                .collect(),
                        );
                    }
                }
            }
            Known::Yes(Vec::new())
        }
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::Yes(_) | Known::No => {
            Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class.as_str())))
        }
    }
}

/// **The scope of a class whose last name segment is `class`**, when exactly one scope has that name and a
/// qualification — see the call site, which is where the measurement is.
///
/// A table with two of them is an ambiguity and answers `None`: guessing between two nested classes named `inner`
/// would make a member list about a class the reader did not write, and a wrong member list is worse than no list.
pub(crate) fn a_class_nested_in_another(scopes: &ScopeTree, class: &str) -> Option<ScopeId> {
    let mut found = None;
    for (index, scope) in scopes.scopes().iter().enumerate() {
        if scope.kind != ScopeKind::Class || scope.name.as_deref() != Some(class) {
            continue;
        }
        let id = ScopeId(index);
        match scopes.qualified_name_of(id) {
            // Keyed by the spelling asked about: the caller's own branch handles it.
            Some(qualified) if qualified == class => return None,
            Some(_) => {}
            None => continue,
        }
        if found.is_some() {
            return None;
        }
        found = Some(id);
    }
    found
}

/// The names in a level that take part in **hiding**: every member that has a name at all.
///
/// **A destructor, an operator and a conversion function used to be left out of this rule** because a fact stored a
/// lookup key rather than a spelling — `~D` has no identifier in it — so their `DeclFact::name` was `""` and there was
/// nothing to compare them by. That is no longer what a fact holds: `binding.name.text()` answers the spelling
/// (`~Derived`, `operator[]`, `operator int`), for the reason `fact_for` records — an empty name made every
/// destructor answer for its own class and made an operator invisible to a member list. So the hiding rule applies to
/// them like anything else, and it is *right* that it does: `~Derived` and `~Base` are different spellings, so a
/// derived destructor hides nothing it should not, while a derived `operator[]` does hide a base's.
///
/// What is still left out is a member with **no** name at all, which a fact now has only where the source has no
/// spelling to read.
pub(crate) fn recorded_names(members: &[ProjectMember]) -> impl Iterator<Item = String> + '_ {
    members
        .iter()
        .map(|member| member.fact.name.to_string())
        .filter(|name| !name.is_empty())
}

/// Flag the members whose name another class at the same level also declares.
///
/// Level-scoped rather than list-scoped, and the difference is the language's: a base's member that a *derived*
/// class redeclares is hidden, so it never reaches this function, while two bases at the same level genuinely
/// leave the name unresolved — the same finding [`member_across_files`] reports as `Unknown(Ambiguous)`.
///
/// **Overloads are not ambiguity.** `void f(); void f(int);` declares one name once, in one class, and a consumer
/// that flagged it would refuse to complete a name the language resolves perfectly well. So what is counted is
/// the number of distinct declaring classes, not the number of declarations.
///
/// A member with no recorded name is skipped for the same reason it takes no part in hiding — see
/// [`recorded_names`] — and skipping it is the conservative direction: claiming two declarations are one contested
/// name would be a definite statement about a name this layer cannot read.
pub(crate) fn mark_ambiguity(members: &mut [ProjectMember]) {
    let mut declaring: HashMap<String, HashSet<String>> = HashMap::new();

    for member in members.iter().filter(|member| !member.fact.name.is_empty()) {
        declaring
            .entry(member.fact.name.to_string())
            .or_default()
            .insert(member.declared_in.clone());
    }

    for member in members.iter_mut() {
        member.ambiguous = !member.fact.name.is_empty()
            && declaring
                .get(member.fact.name.as_str())
                .is_some_and(|classes| classes.len() > 1);
    }
}
