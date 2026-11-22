//! **The questions that cross a file boundary** — where an `#include` points, what a name resolves to when the
//! declaration may be in any file, and the macro that no single file's summary answers.
//!
//! Moved here from `project/mod.rs` without a change. Each answer is a search over the summaries the index holds, and
//! each has to say *why* it stopped: `Known::Unknown` with a reason is a promise to the caller that the analysis looked
//! and could not know, which is different from a name that is not declared anywhere.

use super::*;

/// The `#include` whose **directive** covers `offset`, if there is one and it resolved.
///
/// # What it answers, and what it deliberately does not
///
/// * `Yes(target)` — the cursor is on an `#include` line and the resolver found the file. The spelling is the one
///   written, which is what a client shows; the path is where the file is, which is what it opens.
/// * `Unknown(NotDeclaredHere)` — the line is an `#include` and **nothing was found** for it: a header outside every
///   search path, or one whose file was deleted since. Not `No`, and the distinction is the one the rest of this
///   module keeps: "the index cannot see it" is a different claim from "there is no such thing".
/// * `Unknown(UnparsableName)` — the cursor is not on an `#include` at all. Either nothing is here, or what is here
///   is a different kind of name (a macro include's target is [`ProjectIndex::macro_definition`]'s question).
/// * `Unknown(ConditionalCompilation)` — the `#include` is inside an `#if` this layer cannot evaluate, so whether
///   the file is part of the translation unit is not known.
///
/// The directive's own range is what the cursor is matched against rather than the header-name token, and the two
/// differ on purpose: a reader who points at `#include` or at the `<` means the same thing as one who points at
/// `vector`, and the tree has no node for the second half of the line when the lexer never folded it.
pub fn header_at(
    index: &ProjectIndex,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<HeaderTarget> {
    // The tree answers *which line*, because only the tree knows where the cursor is; the summary answers *where it
    // points*, because resolution happened when the file was read and re-running the search here would be a second
    // implementation of the resolver.
    let Some((spelling, name_range)) = crate::sema::resolve::header_name_at(root, offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    let Some(summary) = index.summary(path) else {
        // The file's own reading is not in the index, so nothing here knows where its includes resolved. The
        // spelling is what the caller pointed at, and saying it is better than an empty reason.
        return Known::Unknown(UnknownReason::UnresolvedInclude(Box::from(
            spelling.as_str(),
        )));
    };

    // The fact whose **directive** covers the name: `range` is the whole `#include …` line, and the name sits
    // inside it. A file has few includes, so this is a scan of a handful of ranges.
    let Some(fact) = summary.includes.iter().find(|include| {
        include.range.start_offset <= name_range.start_offset
            && include.range.end_offset() >= name_range.end_offset()
    }) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    match fact.guard {
        FactGuard::Unconditional => {}
        FactGuard::Region(_) => {
            // The directive is inside an `#if`. Whether it is part of the translation unit is a question this
            // layer answers elsewhere (`index::environment::visibility_at`) and this query does not ask, so the
            // honest answer is the one every other conditional answer in this module gives.
            return Known::Unknown(UnknownReason::ConditionalCompilation);
        }
    }

    // **A macro include's target is a macro.** `#include HEADER` names a name, and what it expands to is the
    // preprocessor's answer — which is the same question `#define` answers, so it goes to the same place rather
    // than being guessed at here.
    if fact.form == IncludeForm::Macro {
        return Known::Unknown(UnknownReason::UnresolvedInclude(Box::from(
            fact.spelling.as_str(),
        )));
    }

    match &fact.resolved {
        Some(resolved) => Known::Yes(HeaderTarget {
            spelling: fact.spelling.clone(),
            form: fact.form,
            range: fact.range,
            resolved: resolved.clone(),
        }),
        // **The search ran and found nothing** — the header is on no include path this project configured, or the
        // file it named has been deleted since. `UnresolvedInclude` is exactly this reason, and it is the one the
        // rest of the crate already uses for a declaration reachable only through an include nobody could find.
        None => Known::Unknown(UnknownReason::UnresolvedInclude(Box::from(
            fact.spelling.as_str(),
        ))),
    }
}

/// Which declaration something refers to, using **both** layers.
///
/// The entry point a feature should call, and the reason it exists rather than each caller composing the two:
/// C++ resolves a name in a fixed order, and getting that order wrong is a jump to the wrong file rather than an
/// error. The order is:
///
/// ```text
/// 1. this file's scopes        — a local shadows a header's declaration, always
/// 2. this file's own top-level — a declaration written here is not the header's
/// 3. the headers it includes   — reached through the include graph
/// ```
///
/// # What it takes, and why each half is a reference
///
/// `scopes` and `root` are the file's own analysis, which [`crate::build_scopes`] and the parser produce — the
/// caller has them because it just parsed the file. The index holds the *other* files. Neither is derivable from
/// the other: the index has no scopes for the open buffer, and the scopes know nothing outside it.
///
/// # The two reasons that reach step 3, and the one that does not
///
/// [`UnknownReason::NotDeclaredHere`] means the name is not in this file, which is exactly what step 3 is for.
/// [`UnknownReason::Ambiguous`] does **not** mean that: the name *is* here, more than once, and looking in the
/// headers would be answering a different question. So it stops.
///
/// Everything else stops too — a name that could not be read, a conditional the analysis cannot evaluate — for
/// the same reason: the failure is not "not here".
pub fn definition_across_files(
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<ProjectDefinition> {
    // **An unqualified name inside its class's own method is that class's member**, and the plural query cannot say
    // so: it lists every declaration of the spelling (a references list narrows that itself, see
    // `handlers::references::narrowed_to_the_cursors_class`). `void C::g() { … }` cannot name another class's member
    // without qualifying it, so the class the definition is written for decides the singular question — and when it
    // declares nothing of that spelling, the answer is that the class does not declare it rather than another
    // class's member of the same name.
    let found = definitions_across_files(index, declaring, scopes, root, path, offset);
    // **The rule is for an *unqualified* name.** `nlohmann::json::object` in the body of
    // `WorkspaceDiagnosticReport::Serialize` is a qualified name: it names the class it wants, and what the enclosing
    // definition's class declares has no bearing on it. Measured: reading it as this class's member turned the correct
    // plural answer (`basic_json::object`) into `NotDeclaredHere` — the class does not declare a member spelled
    // `nlohmann::json::object`.
    let written = crate::sema::resolve::qualified_name_at(root, offset)
        .map(|(written, _)| written)
        .unwrap_or_default();
    let found = if !written.contains("::")
        && crate::sema::resolve::member_access_at(root, offset).is_none()
        && let Some(class) = the_class_a_definition_is_written_for(root, offset)
        && index.scope_names_a_class(&class, path)
        && let Known::Yes(list) = members_of(index, scopes, root, path, &class)
        && let Known::Unknown(UnknownReason::NotDeclaredHere(..)) | Known::Yes(_) = &found
    {
        let named: Vec<ProjectDefinition> = list
            .members
            .into_iter()
            .filter(|member| member.fact.name.as_str() == written.as_str())
            .map(|member| ProjectDefinition {
                file: member.file,
                fact: member.fact,
            })
            .collect();
        if named.is_empty() {
            Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(written)))
        } else {
            Known::Yes(ProjectDefinitions {
                found: named,
                conditional: 0,
            })
        }
    } else {
        found
    };
    match found {
        Known::Yes(mut found) => {
            // **A synonym is not a second entity, and a definition beats a forward declaration.** Both rules were in
            // `ProjectIndex::definition` and neither was here, so every query on this path answered `Ambiguous` for a
            // name a module re-exports: measured on `test_module`'s `import std;` file, `std::cout` had two candidates
            // — `std.cc:1588 using std::cout;` (kind `Other`) and `iostream:65 extern ostream cout;` — and `std::map`
            // had three: the same kind of synonym, the **definition** in `stl_map.h`, and a forward declaration in
            // `stl_multimap.h`. See `docs/worklog.md`.
            if found
                .found
                .iter()
                .any(|declaration| declaration.fact.kind != DeclKind::Other)
            {
                found
                    .found
                    .retain(|declaration| declaration.fact.kind != DeclKind::Other);
            }
            if found.found.len() > 1 {
                let definitions: Vec<&ProjectDefinition> = found
                    .found
                    .iter()
                    .filter(|declaration| declaration.fact.definition)
                    .collect();
                if let [only] = definitions.as_slice() {
                    return Known::Yes((*only).clone());
                }
                // **Several *definitions* of one type are a class template and its specializations**, and the general
                // one is the one with the most template parameters: `std::vector` has the primary in `stl_vector.h`
                // (two) and the `vector<bool>` specialization in `stl_bvector.h` (one), and a reader jumping on
                // `std::vector` means the primary. Restricted to `Type`, because a **function** overload set is
                // several definitions too and "the most parameters" would pick one for a call nobody made.
                if definitions.len() > 1
                    && definitions
                        .iter()
                        .all(|declaration| declaration.fact.kind == DeclKind::Type)
                    && let Some(general) = definitions
                        .iter()
                        .max_by_key(|declaration| declaration.fact.parameters().len())
                {
                    return Known::Yes((*general).clone());
                }
            }
            match found.found.len() {
                1 => Known::Yes(found.found.remove(0)),
                _ => Known::Unknown(UnknownReason::Ambiguous(Box::from(
                    // The spelling the cursor wrote, read back from the tree rather than taken from the list: a reader
                    // asking "which `find` is this" wants the name they pointed at, not the qualified name of whichever
                    // overload happens to sort first.
                    crate::sema::resolve::qualified_name_at(root, offset)
                        .map(|(written, _)| written)
                        .unwrap_or_default(),
                ))),
            }
        }
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::No,
    }
}

/// **[`member_across_files`] as a list** — every declaration of the member, in the class the object's type names.
///
/// # Why this is a different question from "where is this name declared"
///
/// A member written after a `.` is not looked up among the names in scope: it is looked up **in the type of the
/// object**, and the difference is measured on one real file — `line.empty()` asked by name answers with **twelve**
/// declarations of `empty` (every `empty` in the standard library, from classes the reader never mentioned) while
/// asked of the object's type it is `basic_string`'s two. The name query is a heuristic that happens to work often
/// enough to be worth keeping as a fallback; this is the reading.
///
/// The set is [`members_of`]'s, filtered by name: that walk already follows bases and already hides what a nearer
/// level hides, which is exactly the set of declarations a name in this position can refer to — and it is the same
/// walk the completion after `.` shows, so a jump and the list a reader was just looking at cannot disagree.
pub fn member_definitions_across_files(
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<ProjectDefinitions> {
    let memo = TypeMemo::default();
    let Some(access) = crate::sema::resolve::member_access_at(root, offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    if access.member.is_empty() {
        return Known::Unknown(UnknownReason::UnparsableName);
    }

    let Known::Yes((object_type, _)) = type_of_expression_memoed(
        &memo,
        &crate::sema::declarations::ShapeTable::new(),
        index,
        declaring,
        scopes,
        root,
        path,
        &access.object,
        0,
    ) else {
        let Known::Unknown(reason) = type_of_expression_memoed(
            &memo,
            &crate::sema::declarations::ShapeTable::new(),
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

    // **The class, not the type**: `std::vector<int>` is a type and `std::vector` is what has members — and a query
    // handed the whole spelling asks about a class nobody declares, which is one of the shapes that made a member
    // access on a standard container answer nothing. A reference is seen through here because C++ sees through it
    // (a member of a `Widget&` is a member of `Widget`); a pointer is **not**, because reaching the pointee is a `*`
    // the caller writes and this query cannot take for it.
    let Some(class) = member_access_class(&object_type) else {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(
            object_type.to_string(),
        )));
    };

    let members = match members_of(index, scopes, root, path, &class) {
        Known::Yes(members) => members,
        Known::Unknown(reason) => return Known::Unknown(reason),
        Known::No => {
            return Known::Unknown(UnknownReason::UnknownType(Box::from(
                object_type.to_string(),
            )));
        }
    };

    let found: Vec<ProjectDefinition> = members
        .members
        .iter()
        .filter(|member| member.fact.name == access.member)
        .map(|member| ProjectDefinition {
            file: member.file.clone(),
            fact: member.fact.clone(),
        })
        .collect();

    if found.is_empty() {
        // **Not a definite no**: the class may be a template this layer does not instantiate, or inherit from one
        return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(format!(
            "{class}::{}",
            access.member
        ))));
    }

    Known::Yes(ProjectDefinitions {
        found,
        // A member behind a conditional `#if` inside a class body is a fact of its file like any other, and
        // `members_of` offers what it found; the count is about *includes*, which is a question this path never
        // asks. Zero rather than a guess.
        conditional: 0,
    })
}

/// **[`definition_across_files`] as a list** — see [`ProjectIndex::definitions`] for what the list is.
///
/// The two answers are one query: this one, and the single-answer form restricted to the names that have exactly
/// one declaration. A consumer that can show several locations should ask this one, because "ambiguous" is not an
/// answer a client can use.
/// **What a declaration answer is qualified by** — the qualifier the cursor wrote, or the scope the declaration
/// itself is written in.
///
/// The two are different questions and both are needed. `ns::Widget` says which `Widget` the reader means, and that
/// is the qualifier's answer. A cursor on a **bare** name says nothing, and this used to answer `None` — which made
/// the fact's qualified name the bare spelling, so a reference search started from a method's own declaration
/// compared `length_squared` against `math::Point::length_squared` and dropped every occurrence: **0 references**,
/// and a code lens reading `0 references`, for every method in the project. Measured on the `cpp_modules` fixture,
/// which is where the report came from.
///
/// The declaration's own scope is the answer the index gives the same fact (`DeclFact::scope`), so a binding-built
/// answer and an indexed one agree about what the symbol is *called* — the identity a reference list, a rename and
/// a code lens all key on.
fn the_scope_of_the_answer(
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    offset: usize,
    binding: &crate::Binding,
) -> Option<String> {
    crate::sema::resolve::qualified_name_at(root, offset)
        .and_then(|(written, _)| {
            written
                .rsplit_once("::")
                .map(|(scope, _)| scope.trim_start_matches("::").to_string())
                .filter(|scope| !scope.is_empty())
        })
        .or_else(|| scopes.qualified_name_of(binding.scope))
        .filter(|scope| !scope.is_empty())
}

pub fn definitions_across_files(
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<ProjectDefinitions> {
    // **A cursor on a member asks about the object's type, not about the name.** `line.empty()` is
    // `std::basic_string::empty`, and asking by name instead reaches every `empty` in the library — measured, 12
    // declarations against `basic_string`'s 2. So the member question goes first, and the name query answers only
    // when the member question *cannot be asked*:
    //
    // * the object's type is unknown — a heuristic answer beats a dead end, and this is what answered before the
    //   member path existed;
    // * a type that **is** known and does not have the member is not that case: answering with another class's
    //   member of the same name would be a jump to a place the language does not name, which the handler's own
    //   rule ("a wrong location is worse than none") forbids.
    // **Whether the cursor is on a member access**, decided once: the member question below asks it, and the name
    // query needs the same fact for a different reason — see the `NotDeclaredHere` arm at the end.
    let naming_a_member = crate::sema::resolve::member_access_at(root, offset).is_some();
    if naming_a_member {
        match member_definitions_across_files(index, declaring, scopes, root, path, offset) {
            Known::Yes(found) => return Known::Yes(found),
            // The member question could not be asked (the object's type is unknown) **or** was answered with
            // nothing, and both fall through to the name query — which is what answered before the member path
            // existed. The second case is a deliberate trade rather than an oversight: a member list is never a
            // claim of completeness ([`MemberList::unlisted`]), so "this class does not have that member" and "this
            // class's bases could not be read" arrive here looking the same, and a name-based list is a jump that
            // usually lands right rather than no jump at all. Measured: `std::cin.eof()` is inherited through
            // `basic_ios`, and while `std::basic_istream` is declared three times the base walk cannot open it —
            // the name query still has `eof` among its candidates.
            Known::Unknown(_) | Known::No => {}
        }
    }

    match crate::sema::resolve::definitions_at(scopes, root, offset) {
        // **An overload set, which the condition in a signature can narrow.** The scope tree keeps both declarations —
        // measured: the translation unit holds `describe` twice — while the *singular* query takes the first and stops,
        // so `describe(1.5)` answered the `enable_if<is_integral<T>>` overload because it is written first. See
        // [`the_overload_a_call_names`].
        Known::Yes(candidates) if candidates.len() > 1 => {
            let chosen = the_overload_a_call_names(
                index,
                declaring,
                scopes,
                root,
                path,
                offset,
                &candidates,
            )
            .unwrap_or_else(|| candidates[0].clone());
            let scope = the_scope_of_the_answer(scopes, root, offset, &chosen);
            let local = scopes.declares_a_local(chosen.scope);
            return Known::Yes(ProjectDefinitions::one(ProjectDefinition::from_binding(
                path, chosen, scope, local,
            )));
        }
        Known::Yes(mut candidates) => {
            let binding = candidates.remove(0);
            let scope = the_scope_of_the_answer(scopes, root, offset, &binding);

            // A definition answer can be a *local*: the cursor is inside the same body the name was declared in,
            // which is exactly when `definition_at` resolves it here rather than sending it to the index. Saying
            // so matters because the caller may hand this answer on to a consumer asking whether the name is
            // reachable elsewhere. Asked before the binding is moved, and from the tree that can answer it.
            let local = scopes.declares_a_local(binding.scope);

            return Known::Yes(ProjectDefinitions::one(ProjectDefinition::from_binding(
                path, binding, scope, local,
            )));
        }
        Known::Unknown(UnknownReason::NotDeclaredHere(name)) => {
            // The single-file layer has already established the spelling, so the project layer is asked about
            // exactly that name rather than re-reading the cursor — and it answers with the list, which is the
            // whole point of this function.
            //
            // **Which list depends on whether the cursor names a member**, and the two differ by a scope rule
            // rather than by a preference. A member access whose object's type is unknown still wants the member
            // (`std::cin.eof()` is the measured case: the base walk cannot open `basic_istream`, and `eof` is
            // reachable only by name), so it asks for every scope. So does a cursor inside a definition written
            // `void C::g() { … }`, whose body may name `C`'s members with no object and no qualifier at all —
            // see [`the_class_a_definition_is_written_for`]. Everything else is an ordinary name lookup, and
            // an ordinary name does not reach into a class: measured on a two-file project, go-to-definition on
            // `printf` in a `.cpp` answered **three** declarations — a class's member, its out-of-class definition,
            // and `stdio.h`'s `printf` — while the only one the file can name is the last.
            // **A qualified name whose class does not declare it itself** — `Derived::instances`, where `instances`
            // is a static member of `Base`. The scope tree has no binding for it (the derived class declares nothing
            // of that name), while the member walk [`members_of`] goes through the bases. A reader writing the derived
            // class's name is asking for its base's member: measured on `test_cpp/src/classes.cpp:141`,
            // `Derived::instances()` had no hover while `Base::instances()` and `derived.name()` both did.
            if let Some((qualifier, last)) = name.rsplit_once("::")
                && !qualifier.is_empty()
                && !last.is_empty()
                // **A *class*, and not a namespace.** `members_of` answers a class's member list, and it reads the
                // members of the file that declares the class; a namespace is every file that opens it, and there is
                // no one file whose member list is `std`'s. Measured on a project importing `std`: `std::println`
                // hovered with **two** declarations, both from `<ostream>` — the declarations of whichever file the
                // member list was built from — while the index holds all six (four more in `<print>`, under `#ifdef
                // __cpp_lib_print`). Without this guard the branch answers before the name lookup (the fallthrough
                // below) is ever asked, which is why the visibility walk never even ran for this name.
                && index.scope_names_a_class(qualifier, path)
                && let Known::Yes(list) = members_of(index, scopes, root, path, qualifier)
            {
                let inherited: Vec<ProjectDefinition> = list
                    .members
                    .into_iter()
                    .filter(|member| member.fact.name == last)
                    .map(|member| ProjectDefinition {
                        file: member.file,
                        fact: member.fact,
                    })
                    .collect();
                if !inherited.is_empty() {
                    return Known::Yes(ProjectDefinitions {
                        found: inherited,
                        conditional: 0,
                    });
                }
            }
            // **A qualified name the scope tree cannot place, but the index holds in full.** `nlohmann::json::object`
            // is the measured case: `json` is `using json = basic_json<>;`, so the *class* the member lives in is not
            // the qualifier a reader wrote — the branch above asks `members_of(nlohmann::json)`, which finds no class
            // of that name, and the fallback then asks for a name in scope, which a class member is not. The index
            // already resolves the spelling through the alias (`basic_json::object`, scope `nlohmann::basic_json`), so
            // the question is asked of it directly before saying the name is not declared.
            if !(naming_a_member || inside_a_definition_written_with_a_qualifier(root, offset))
                && name.contains("::")
                && let Known::Yes(found) = index.definition(&name, path)
            {
                return Known::Yes(ProjectDefinitions::one(found));
            }
            return if naming_a_member || inside_a_definition_written_with_a_qualifier(root, offset)
            {
                index.definitions(&name, path)
            } else {
                index.definitions_in_scope(&name, path)
            };
        }
        Known::Unknown(reason) => return Known::Unknown(reason),
        Known::No => {}
    }

    // `No` from the single-file layer means the offset is not in a name the scopes could place at all — on
    // punctuation, or past the end — so there is nothing to look up anywhere.
    Known::Unknown(UnknownReason::UnparsableName)
}
///
/// A concept's body is written in terms of its parameter, so expanding `integral<double>` means substituting `double` for
/// that name. Read from the concept's own declaration rather than guessed from the use site.
pub(crate) fn the_concept_parameters(
    root: &cpp_parser::CppSyntaxNode,
    name: &str,
) -> Option<String> {
    for declaration in root.descendants() {
        if cpp_parser::CppSyntaxKind::from(declaration.kind())
            != cpp_parser::CppSyntaxKind::ConceptDecl
        {
            continue;
        }
        let text = declaration.text().to_string();
        let Some(at) = text.find('=') else {
            continue;
        };
        let declared = text[..at].split_whitespace().last().unwrap_or_default();
        if declared != name {
            continue;
        }
        // **The `template <…>` is a *sibling* of the `ConceptDecl`**, not part of it: measured, the tree is
        // `Declaration > [TemplateDecl, ConceptDecl]` and the concept node's own text is `concept integral = …`. So the
        // parameter list is read from the enclosing declaration.
        let with_the_parameters = declaration
            .parent()
            .map(|parent| parent.text().to_string())
            .unwrap_or(text);
        return template_parameters_in(&with_the_parameters)
            .into_iter()
            .next();
    }
    None
}

/// **A concept's right-hand side**, found by name: `template <typename T> concept integral = std::is_integral_v<T>;`
/// answers `std::is_integral_v<T>`.
///
/// Searched in the tree the caller holds — the file being edited and, when a `.cpp` is asking, the header its class is
/// declared in, which is the same walk the condition itself takes. Only the *definition* is read: a concept that composes
/// other concepts (`concept signed_integral = integral<T> && is_signed_v<T>`) answers a name this layer cannot expand, so
/// the verdict is `None` and the candidate stays.
pub(crate) fn the_concept_definition(
    root: &cpp_parser::CppSyntaxNode,
    name: &str,
) -> Option<String> {
    for declaration in root.descendants() {
        if cpp_parser::CppSyntaxKind::from(declaration.kind())
            != cpp_parser::CppSyntaxKind::ConceptDecl
        {
            continue;
        }
        let text = declaration.text().to_string();
        let Some(at) = text.find('=') else {
            continue;
        };
        // `concept integral = …` — the name is the word before the `=`.
        let declared = text[..at].split_whitespace().last().unwrap_or_default();
        if declared != name {
            continue;
        }
        let body = text[at + 1..].trim().trim_end_matches(';').trim();
        if !body.is_empty() {
            return Some(body.to_string());
        }
    }
    None
}

/// **The overload a call actually names**, when the condition in its signature can decide it.
///
/// ```cpp
/// template <typename T>
/// typename std::enable_if<std::is_integral<T>::value, std::string>::type describe(T);
/// template <typename T>
/// typename std::enable_if<std::is_floating_point<T>::value, std::string>::type describe(T);
///
/// describe(1.5);      // the second, and only the condition says so
/// ```
///
/// The scope tree keeps **both** declarations — measured, the translation unit holds `describe` twice — and until this
/// function existed the answer was the first of them for either call, because the singular query takes the first binding
/// of the name and stops. What separates the two is the `enable_if` in the *return type*, so the narrowing is:
///
/// * a candidate whose condition is **false** for the call's argument types is dropped;
/// * a candidate with no condition, or with one this layer cannot decide, is **kept** — deleting on "cannot tell" is how
///   a reader loses the overload their program chose;
/// * the answer is used only when exactly one candidate survives.
///
/// The argument types are bound to the declaration's template parameters **in order** (`describe(T)` with
/// `describe(1.5)` binds `T = double`), which is the one deduction this needs and the one it can do exactly.
pub(crate) fn the_overload_a_call_names(
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
    candidates: &[crate::Binding],
) -> Option<crate::Binding> {
    // **The call the cursor's name is the callee of.** A name that is not called has no arguments, so there is nothing to
    // decide a condition against and the set stands as it is.
    let name = crate::sema::resolve::expression_at(root, offset)?;
    let call = name.ancestors().find(|node| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::CallExpr
    })?;
    // **The arguments are the call's children after the callee**, and not always wrapped in an `ArgumentList`: measured,
    // `describe(1.5)` is `CallExpr > [IdentifierExpr, LiteralExpr]`. Asking only for the wrapper found none and answered
    // "this name is not called", which is why the narrowing never ran.
    let arguments: Vec<cpp_parser::CppSyntaxNode> = match call.children().find(|node| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::ArgumentList
    }) {
        Some(list) => list.children().collect(),
        None => call.children().skip(1).collect(),
    };
    let argument_types: Vec<Option<String>> = arguments
        .iter()
        .map(|argument| {
            match type_of_expression(index, declaring, scopes, root, path, argument, 0) {
                Known::Yes((found, _)) => Some(found.to_string()),
                _ => None,
            }
        })
        .collect();
    if argument_types.is_empty() {
        return None;
    }

    let mut survivors = Vec::new();
    for candidate in candidates {
        let Some(the_candidate) =
            node_covering(root, candidate.name_range.start_offset, candidate.range)
        else {
            survivors.push(candidate.clone());
            continue;
        };
        // **The whole declaration, not the declarator.** A binding ranges over the declarator — `describe(T value)` —
        // and the `enable_if` is in the *return type*, which is a specifier of the enclosing declaration. Measured: the
        // binding's text was `describe(T value)` for both overloads, so the condition was never found and nothing narrowed.
        let declaration_node = the_candidate
            .ancestors()
            .find(|node| {
                cpp_parser::CppSyntaxKind::from(node.kind())
                    == cpp_parser::CppSyntaxKind::Declaration
            })
            .unwrap_or_else(|| the_candidate.clone());
        let declaration = declaration_node.text().to_string();
        // **A `requires` clause is read from its own node**, because the clause and the declaration that follows it are
        // separated by nothing but a newline: a text scan cannot tell `requires is_integral_v<T>` from
        // `requires is_integral_v<T> std::string describe(T)`. `enable_if` is a name inside the type, so it is found in
        // the text and needs no node.
        let requires_clause = declaration_node
            .descendants()
            .find(|node| {
                cpp_parser::CppSyntaxKind::from(node.kind())
                    == cpp_parser::CppSyntaxKind::RequiresClause
            })
            .map(|node| node.text().to_string())
            .and_then(|text| crate::sema::constraint::a_requires_clause(&text).map(str::to_string));
        let condition_owned = requires_clause.as_deref().map(str::to_string).or_else(|| {
            crate::sema::constraint::the_condition_of(&declaration).map(str::to_string)
        });
        // **A constraint written as a *concept*'s name** — `template <integral T>` — holds no expression to evaluate, so
        // the concept's own definition is found and its right-hand side evaluated with the same bindings. Measured: a
        // pair of overloads gated by `integral`/`floating` stayed an unresolved set and the call answered the first.
        let condition_owned = condition_owned.or_else(|| {
            let name = crate::sema::constraint::a_concept_name(&declaration)?;
            the_concept_definition(root, &name)
        });
        let Some(condition) = condition_owned.as_deref() else {
            // An ordinary function, or a template enabled unconditionally.
            survivors.push(candidate.clone());
            continue;
        };
        let parameters = template_parameters_in(&declaration);
        let bound: Vec<(String, String)> = parameters
            .into_iter()
            .zip(argument_types.iter())
            .filter_map(|(parameter, found)| Some((parameter, found.clone()?)))
            .collect();
        // **A concept the condition names is expanded and decided**, recursively: `signed_integral<T>` is
        // `integral<T> && is_signed_v<T>`, and `integral<T>` is another concept. Without this, a composed concept answered
        // "cannot tell" and both candidates stayed — measured, `describe(1.5)` resolved to the `signed_integral` overload.
        let verdict = crate::sema::constraint::satisfies_with(condition, &bound, &mut |name| {
            the_concept_definition(root, name).and_then(|body| {
                the_concept_parameters(root, name).map(|parameter| (parameter, body))
            })
        });
        match verdict {
            Some(false) => {}
            _ => survivors.push(candidate.clone()),
        }
    }

    (survivors.len() == 1).then(|| survivors.remove(0))
}

/// The names a declaration's `template <…>` introduces, in order — `template <typename T, class U>` answers `["T", "U"]`.
pub(crate) fn template_parameters_in(declaration: &str) -> Vec<String> {
    let Some(at) = declaration.find("template") else {
        return Vec::new();
    };
    let after = &declaration[at + "template".len()..];
    let Some(open) = after.find('<') else {
        return Vec::new();
    };
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
    let Some(close) = close else {
        return Vec::new();
    };
    after[open + 1..close]
        .split(',')
        .filter_map(|parameter| {
            // `typename T`, `class U`, `typename... Args` — the name is the last word, and a pack keeps its `...`.
            parameter
                .split_whitespace()
                .last()
                .map(|word| word.trim_end_matches("...").trim().to_string())
                .filter(|word| !word.is_empty())
        })
        .collect()
}

/// **The class a member function definition is written for** — `Widget` in `int Widget::area() { … }`, and
/// `lsp::WorkspaceDiagnosticReport` in `nlohmann::json lsp::WorkspaceDiagnosticReport::Serialize() { … }`.
///
/// A member function defined out of class has its class **in another file**, while the scope tree is built from the
/// text of *this* one: `void C::g() { n = 1; }` opens a function scope whose qualifier names no scope here, so `n` is
/// not in any scope the walk can reach. It may well be `C::n`, which is why this function exists: the class's own
/// member list is the first place an unqualified name in that body is looked up, the same rule `this->n` follows.
///
/// # What it reads, and the direction it can be wrong in
///
/// The identifier immediately before the parameter list's `(`: `void C::g(…)` and `auto C::g(…) -> std::string`
/// define a qualified name, while `std::string g(…)` returns a qualified type and defines an unqualified one. The
/// window is bounded by the last whitespace before the `(`, which is what tells those two apart. Written as a
/// spelling rather than a walk of the grammar's own shapes — the declarator of a definition holds only its parameter
/// list, and the name it declares sits in the specifier sequence, which is the seam
/// `sema::scopes::declarator_as` documents. A miss answers `None` and the caller's ordinary scope rule applies; it
/// cannot return a class the program never named.
///
/// `None` for a free function, for a definition written without a qualifier, and for an offset outside every body.
pub(crate) fn the_class_a_definition_is_written_for(
    root: &cpp_parser::CppSyntaxNode,
    offset: usize,
) -> Option<String> {
    let mut node = cpp_parser::token_at(root, offset).and_then(|token| token.parent());

    while let Some(current) = node {
        let is_a_definition = cpp_parser::CppSyntaxKind::from(current.kind())
            == cpp_parser::CppSyntaxKind::Declaration
            && current.children().any(|child| {
                cpp_parser::CppSyntaxKind::from(child.kind())
                    == cpp_parser::CppSyntaxKind::CompoundStat
            });
        if is_a_definition {
            let text = current.text().to_string();
            let before_the_parameters = text.split('(').next().unwrap_or_default();
            let name = before_the_parameters
                .split_whitespace()
                .next_back()
                .unwrap_or_default();
            if let Some((qualifier, _)) = name.rsplit_once("::")
                && !qualifier.is_empty()
            {
                return Some(qualifier.to_string());
            }
        }

        node = current.parent();
    }

    None
}

/// **Does the cursor sit inside a definition written `void C::g() { … }`?**
///
/// The plural query asks it to decide whether a bare name may reach into a class at all — a `.cpp` that defines a
/// member writes that member's class nowhere else. The singular query narrows further and asks the class itself; see
/// [`the_class_a_definition_is_written_for`].
pub(crate) fn inside_a_definition_written_with_a_qualifier(
    root: &cpp_parser::CppSyntaxNode,
    offset: usize,
) -> bool {
    the_class_a_definition_is_written_for(root, offset).is_some()
}
/// The answer to a cross-file question about a macro name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMacro {
    pub file: PathBuf,
    /// The `#define` or the `#undef` that settles what the name is at the point asked about.
    pub fact: MacroFact,
}

/// Where the macro name written at `offset` is defined — or that it is not a macro there.
///
/// The entry point for "go to definition" on a macro, and the counterpart of [`definition_across_files`]. It needs
/// no scope tree and no second layer, because a macro query is not a name lookup at all: it is a question about
/// **translation order** — which of the `#define`s and `#undef`s written before this point is the last one — and
/// the index already stores every one of them with the offset it was written at.
///
/// [`UnknownReason::UnparsableName`] when the offset is not on a name at all, which is the ordinary answer for
/// most cursor positions.
pub fn macro_across_files(
    index: &ProjectIndex,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<ProjectMacro> {
    match crate::sema::resolve::name_at(root, offset) {
        Some((name, _)) => index.macro_definition(&name, path, offset),
        None => Known::Unknown(UnknownReason::UnparsableName),
    }
}
