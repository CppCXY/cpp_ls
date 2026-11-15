//! **Base clauses** — what a class inherits, and what a member lookup walks through.
//!
//! Moved here from `project/mod.rs` without a change. The base list is a *spelling* in most summaries rather than a
//! resolved class, so each name here is pattern-matched against the class it may mean.

use super::*;

pub(crate) fn direct_member(
    shapes: &crate::sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    member: &str,
    arguments: Option<&[String]>,
) -> Option<(DeclFact, PathBuf)> {
    // An alias has no members of its own, so the spelling is followed to the class it names before anything is
    // looked up: `std::string`'s members are `std::basic_string`'s. This is the one place a spelling becomes a
    // class for a **single** member, which is why the step lives here rather than at every caller.
    let class = &resolve_aliases(shapes, index, scopes, root, path, class);

    // A class declared in this file is looked up here first, which is what makes the whole query work on a buffer
    // that has never been written to disk. The declared type is filled in from the tree, because a member of a
    // member is exactly what a nested access asks for next.
    if let Some(scope) = scopes.scope_with_qualified_name(class)
        && let Some(scope_data) = scopes.scope(scope)
        && let Some(binding) = scope_data
            .bindings
            .iter()
            .find(|binding| binding.name.identifier_text() == Some(member))
    {
        return Some((
            fact_from_binding_with(
                crate::sema::declarations::shapes_from(shapes, root),
                root,
                class,
                binding,
            ),
            path.to_path_buf(),
        ));
    }

    // Several declarations of one name **in one class** are overloads, not an ambiguity. The language keeps them in
    // a single overload set and picks by argument types, which this layer does not have — and a cursor still needs
    // one answer, so the first in declaration order is it. That is the rule `definition_at` already uses within a
    // scope, and it is why [`ProjectIndex::declarations_in`] is asked here rather than
    // [`ProjectIndex::definition`], whose "several visible declarations" answer is right for a *name* and wrong
    // for a member: measured on the closure of `<string>`, every real member (`size`, `find`, `substr`) has
    // several declarations, so a member lookup that called that ambiguous reported `NotDeclaredHere` for the whole
    // standard library.
    //
    // A consumer that wants all of them asks [`members_of`], which is the query that exists for it — the split is
    // "a jump takes the first, a list takes them all".
    //
    // # A known wrong answer this rule can give, and the fix it needs
    //
    // **The declarations in one scope are not always overloads of one another.** A class template's *partial
    // specializations* are filed under the same scope as the primary — `atomic<shared_ptr<_Ty>>` and `atomic<_Ty>`
    // both have `qualified_name() == "std::atomic"` — so taking the first name match can take a **specialization's**
    // member for an instantiation that does not match its pattern.
    //
    // Measured, and it is a wrong type rather than a missing one: `std::shared_ptr<int> sp; std::atomic<int>
    // counter; auto n = counter.load();` answered **`shared_ptr<int>`** — the return of
    // `atomic<shared_ptr<_Ty>>::load` — because `<memory>` was indexed before `<atomic>` and its specialization came
    // first. Five declarations named `std::atomic::load` are visible and only one is the primary's.
    //
    // # The rule that is here
    //
    // [`DeclFact::pattern`] is the class's own name as its declaration wrote it — `atomic<_Ty>` for the primary,
    // `atomic<shared_ptr<_Ty>>` for the specialization — and a candidate is kept when its pattern matches the
    // arguments the **use** wrote: a pattern argument that is one of the class's own parameter names is a **slot**
    // and takes whatever the use put there, and anything else has to be equal to it. So `atomic<_Ty>` matches
    // `atomic<int>` and `atomic<shared_ptr<_Ty>>` does not — which is the wrong answer this removes.
    //
    // The arguments arrive as a parameter rather than being read off `class`, because **the class spelling reaching
    // this function has already been stripped to its bare name** — measured: `std::atomic`, never `std::atomic<int>`.
    // The pairing exists one step up, in `member_access_class_and_arguments`, and callers that have it pass it.
    //
    // **Two other fixes were tried and are not here**, so that nobody writes them again. Refusing whenever the
    // candidates disagree removes the wrong answer and also removes `v.begin()` — `vector`'s `const` and non-`const`
    // overloads return `iterator` and `const_iterator`, so they "disagree" too, and that is a call every file makes:
    // measured on a one-line fixture, one `auto` went from an answer to a refusal. Preferring the candidate whose
    // return type is one of the class's own parameters (`atomic<_Ty>::load` returns `_Ty`) keeps that fixture right
    // and does nothing for `begin`, whose overloads both return compound spellings.
    // **The class spelling carries its own arguments, and the declarations are filed under the bare name.**
    //
    // `enable_if<__is_same(int, int), double>` is a class *template applied to arguments*: the index holds the
    // primary and the partial specialization under `enable_if`, so a lookup made with the whole spelling found no
    // declaration at all — and the member then resolved to its own name, which is what a cursor in `A<B>::type` was
    // answered with. The arguments written in the spelling are also exactly what the candidate patterns are compared
    // against, so they are not merely dropped: they are what picks the specialization whose first slot is `true`.
    //
    // A caller that already knows the arguments (from an object's type, say) passes them itself; one that does not —
    // the nested-name walk, which has only a spelling — gets them here.
    let from_the_spelling: Option<Vec<String>> = class.split_once('<').map(|(_, rest)| {
        crate::sema::builtins::split_arguments(rest.trim_end().strip_suffix('>').unwrap_or(rest))
    });
    let (class, arguments) = match (class.split_once('<'), from_the_spelling.as_ref()) {
        (Some((head, _)), Some(written)) => (head.trim(), Some(written.as_slice())),
        _ => (class.as_str(), arguments),
    };

    if let Some(found) = index
        .declarations_in(class, path)
        .into_iter()
        .find(|declaration| {
            declaration.fact.name == member
                && a_pattern_matches(
                    declaration.fact.pattern.as_deref(),
                    &declaration.fact.parameters,
                    arguments,
                )
        })
    {
        return Some((found.fact.clone(), found.file.clone()));
    }

    // Nothing is scoped to that class, so the last thing to try is the qualified spelling itself: a member defined
    // out of line can have been filed under a spelling the class's own scope is not (`C::f` for a definition
    // written outside `C`), and that is exactly what this lookup is for.
    match index.definition(&format!("{class}::{member}"), path) {
        Known::Yes(found) => Some((found.fact, found.file)),
        Known::Unknown(_) | Known::No => None,
    }
}

/// **Does this declaration's class pattern match the arguments the use wrote?**
///
/// See the rule where it is used, in [`direct_member`]. `None` for either side is "cannot be compared" and answers
/// `true`: a class with no pattern is an ordinary class and matches any use of its name, and a use whose arguments
/// could not be read is not evidence against the declaration. That direction is deliberate — this rule removes a
/// **wrong** answer, and a spelling it could not read must not remove a right one.
pub(crate) fn a_pattern_matches(
    pattern: Option<&str>,
    parameters: &[String],
    written: Option<&[String]>,
) -> bool {
    let (Some(pattern), Some(written)) = (pattern, written) else {
        return true;
    };

    // **The whole pattern, once.** `arguments_in_a_spelling` finds the outermost `<…>` itself and splits at the
    // commas that are not inside brackets, so handing it the part between the brackets — which is what this did
    // first — makes it split again at the *inner* `<`: `shared_ptr<_Ty>` produced the slots `["_Ty"]`, `_Ty` is one
    // of the class's own parameters, and the specialization matched `atomic<int>` after all. The wrong answer
    // survived a version whose whole purpose was to remove it, and the shape of the mistake is the one this file
    // keeps making: a reader that takes a spelling apart twice.
    let Some(slots) = arguments_in_a_spelling(pattern) else {
        return true;
    };
    if slots.len() != written.len() {
        return true;
    }

    slots.iter().zip(written).all(|(slot, argument)| {
        // A **slot** is one of the class's own parameters, and takes whatever the use put there; anything else has
        // to **match** it. `_Ty` against `int` matches; `shared_ptr<_Ty>` against `int` does not.
        a_slot_matches(slot, argument, parameters)
    })
}

/// **Does one slot of a specialization's pattern match the argument the use wrote?**
///
/// Text equality is not enough, and that is what this exists for: `_Is_default_allocator<allocator<_Ty>>` is written
/// in `namespace std`, so its slot is `allocator<_Ty>` while the use writes `std::allocator<MarkEvent>` — the same
/// class, spelled with the qualification the use needed. Measured on the standard library's
/// `allocator_traits<_Alloc> : conditional_t<_Is_default_allocator<_Alloc>::value, …>`: the textual comparison never
/// matched, so the trait was never true and the `conditional` never picked a branch.
///
/// The rule for a template id is **the head by its last name segment, then the arguments slot by slot**, which is the
/// same rule [`rank_of`] and the bare-name lookups elsewhere in this file use. A head that is not a template id is
/// compared as text: a bare name in a pattern is a name written in the pattern's own scope, and there is no second
/// spelling of it to reconcile.
pub(crate) fn a_slot_matches(slot: &str, argument: &str, parameters: &[String]) -> bool {
    let slot = slot.trim();
    if parameters.iter().any(|name| name == slot) {
        return true;
    }
    let argument = folded_for_matching(argument);
    let argument = argument.trim();
    if slot == argument {
        return true;
    }

    let (Some((slot_head, slot_arguments)), Some((argument_head, argument_arguments))) = (
        split_template_arguments(slot),
        split_template_arguments(argument),
    ) else {
        return false;
    };
    if slot_head.rsplit("::").next() != argument_head.rsplit("::").next() {
        return false;
    }
    slot_arguments.len() == argument_arguments.len()
        && slot_arguments
            .iter()
            .zip(&argument_arguments)
            .all(|(one, other)| a_slot_matches(one, other, parameters))
}

/// The arguments written between the outermost `<` and `>` of a spelling — `atomic<int>` gives `["int"]`, and a
/// spelling with no angle brackets gives `None`.
///
/// Split at the commas that are **not inside brackets**, which is the rule [`written_parameters`] uses for a
/// parameter list and for the same reason: `pair<int, int>` is one argument.
pub(crate) fn arguments_in_a_spelling(written: &str) -> Option<Vec<String>> {
    let (_, after) = written.split_once('<')?;
    let inside = after.trim_end().strip_suffix('>')?;

    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut at = 0usize;
    for (index, character) in inside.char_indices() {
        match character {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(inside[at..index].to_string());
                at = index + 1;
            }
            _ => {}
        }
    }
    out.push(inside[at..].to_string());

    let out: Vec<String> = out
        .into_iter()
        .map(|argument| argument.trim().to_string())
        .filter(|argument| !argument.is_empty())
        .collect();
    (!out.is_empty()).then_some(out)
}

/// One binding as a [`DeclFact`], under the qualified name of the scope it was written in.
///
/// The single reader of a binding's fact-shaped fields, shared by the single-member query and the member-list
/// query so that `Widget::size`'s type and `Widget`'s member `size` cannot come out differently: both are read
/// out of the tree through [`declared_type_of`](crate::sema::declarations::declared_type_of) and
/// [`declared_bases_of`](crate::sema::declarations::declared_bases_of), and a second call site would be a second
/// answer to "what does this declaration say".
///
/// The guard is [`FactGuard::Unconditional`] because a binding carries none: the region a declaration sits in is
/// a fact about the text, and the layer that sweeps the directives fills it in when the summary is built — see
/// [`build_facts`](crate::sema::declarations::build_facts).
///
/// # The two fields that are not answered on this path
///
/// A fact built from the **buffer's** scope tree has had no directive sweep and has no diagnostic list, so two of
/// its fields are defaults rather than answers: `guard` says `Unconditional` even for a member written inside an
/// `#if`, and `clean` says `true` even in a buffer that does not parse. Both are gaps rather than decisions, and
/// they are stated here rather than left to be discovered: this path is handed a **node** and the tree's
/// diagnostics are not on it, so a caller that needs either field has to ask the summary — which does have them —
/// and a consumer showing a member list has nothing to gain from either, which is why neither was worth
/// threading the tree through every query for. The same applies to the facts [`member_across_files`] returns
/// from this path.
pub(crate) fn fact_from_binding(
    root: &cpp_parser::CppSyntaxNode,
    class: &str,
    binding: &crate::Binding,
) -> DeclFact {
    fact_from_binding_with(
        &crate::sema::declarations::DeclarationShapes::of(root),
        root,
        class,
        binding,
    )
}

/// **One binding as a fact, given the file's shape table** — the form a caller with a *list* of bindings uses.
///
/// Every field below is read out of the tree by a reader that wants a
/// [`DeclarationShapes`](crate::sema::declarations::DeclarationShapes) table, and that table is a pass over the whole
/// tree. Asking `fact_from_binding` per member therefore built the table once per member, which is quadratic in a
/// class: measured on `std::vector` in libstdc++'s `<bits/stl_vector.h>` (151 members), a member list cost **927 ms**
/// and the answers are a lookup each. Passing the table in makes the whole list cost one pass — the same defect, and
/// the same fix, that [`DeclarationShapes`](crate::sema::declarations::DeclarationShapes) records for the index
/// builder.
pub(crate) fn fact_from_binding_with(
    shapes: &crate::sema::declarations::DeclarationShapes,
    root: &cpp_parser::CppSyntaxNode,
    class: &str,
    binding: &crate::Binding,
) -> DeclFact {
    let _ = root;
    DeclFact {
        enumerator: binding.kind == crate::BindingKind::Enumerator,
        // **The spelling, the same one the index stores** — `operator[]`, `~Widget`, `operator int`.
        //
        // This path used to answer an empty name for every declaration without an identifier, on the argument that
        // `~Derived` and `~Base` must not become one name. That argument is about *hiding*, and the spelling answers
        // it: the two destructors have different spellings, so nothing is hidden that should not be. What the empty
        // name cost was measured: a class whose member is `operator[]` answered `members_of → [""]`, so an operator
        // was invisible to a member list — and a completion after `.`, a hover and every member query with it.
        //
        // The name alone is a fact about the declaration and changes no inference.
        name: binding.name.text(),
        // `None` for an empty spelling, which is what a caller listing the names of a **body** passes: a local has
        // no qualified scope at all, and `Some("")` would be a scope that matches nothing while looking like one.
        // Every other caller passes the class or namespace the binding is in.
        scope: (!class.is_empty()).then(|| class.to_string()),
        // Known, and always: this path is handed the class a member belongs to, so there is no scope the file's text
        // could have left open. See [`DeclFact::scope_unknown`].
        // Answered rather than defaulted, and the answer is always `false`: this path exists for **members**, and a
        // member is declared in a class body by construction — the `class` spelling it is keyed by has no meaning
        // inside a function. A local class's members are members of that class, and the caller that asked for them
        // asked by name. See [`DeclFact::local`].
        local: false,
        // `None`, and it is a **gap rather than an answer** — the same kind [`fact_from_binding`]'s note above
        // states for `clean` and `access`: this path is handed a node and no scope tree, so it cannot say which
        // namespaces enclose a body. It is unreachable in practice here, because this path exists for members
        // and a member is never a local; the caller that does need it asks the file's summary, where the reading
        // was made with the scopes in hand. See [`DeclFact::in_namespace`].
        in_namespace: None,
        kind: crate::DeclKind::from_binding_kind(binding.kind),
        // The same bits the index fills — a fact must say the same thing whichever path produced it.
        // **No rendering here**: this path is handed a node and no file text, and the attribute is a sibling of the
        // declaration — a member query never asks for `[[maybe_unused]]`, so the bit is left unset rather than guessed.
        modifiers: crate::sema::declarations::declared_modifiers_with(shapes, binding),
        // **The other answer the index writes**, and for the same reason: a member function's body is where a
        // `delete` lives, so a call site that asks "does this free my argument" must not depend on which path built
        // the fact.
        frees: crate::sema::declarations::freed_parameters_of(root, binding),
        // **Read by the index path too**, when a member query leaves the file: the annotation is a fact like any other,
        // and a hover on a `UPROPERTY` member should find its specifiers wherever the member is declared.
        reflection: None,
        type_of: crate::sema::declarations::declared_type_of_with(shapes, binding).or_else(|| {
            // **A member whose type is an unnamed class body**, named the way the index names it — see
            // [crate::sema::declarations::an_unnamed_class_body_named]. The two paths that build a fact must
            // agree, and this one is what a member query against the open file uses.
            crate::sema::declarations::an_unnamed_class_body_named(
                Some(class),
                &binding.name.text(),
                shapes,
                binding,
            )
        }),
        // Answered here as well, because **members** are exactly where a call happens: `fac.build().size` needs the
        // return type of a member function that the file being edited declares. See [`DeclFact::returns`].
        // **The deduction, when the file wrote no return type.** `declared_returns_of_in` answers only what is written, and
        // `auto` is not — the index's fact deduces through `declared_returns_of`, so this one must too, or the same function
        // has a return type in one answer and none in the other. Measured: `make_sized` printed `std::array<int, Size>` from
        // `facts` and "a function" from the hover.
        // **No table is built for the fallback.** declared_returns_of_in already answers the deduction
        // (deduced_returns_of); what it refuses is the unusable operator spelling, which the _with form still
        // returns — the wrapper declared_returns_of would build a whole declaration table to say the same thing,
        // and that fallback was measured at 1 000 builds for one 901-line diagnostic pass.
        returns: crate::sema::declarations::declared_returns_of_in(shapes, root, binding)
            .or_else(|| crate::sema::declarations::declared_returns_of_with(shapes, binding)),
        // **The same residue the index records**, because a member query is exactly the reader that has the class:
        // `parameters` and the class's file are both in hand here. See [`crate::DeclFact::returns_in_the_class`].
        returns_in_the_class: {
            let returns = crate::sema::declarations::declared_returns_of_in(shapes, root, binding)
                .or_else(|| crate::sema::declarations::declared_returns_of_with(shapes, binding));
            if returns.is_some() {
                None
            } else {
                crate::sema::declarations::the_returns_the_class_has_to_resolve(
                    shapes, root, binding,
                )
            }
        },
        split_by_a_directive: crate::sema::declarations::split_by_a_directive_with(shapes, binding),
        bases: crate::sema::declarations::declared_bases_of_with(shapes, binding),
        // **The same three fields the index fills**, because a fact must say the same thing whichever path produced it.
        //
        // These were empty here, with the argument that "substitution is the caller's step".
        // Substitution still is — what these fields carry is the *names* to substitute and the
        // class's written **pattern**, and a caller that has the use's arguments cannot pair them
        // without them.
        parameters: crate::sema::declarations::declared_template_parameters_with(shapes, binding),
        constraints: Vec::new(),
        // **The clause the declaration writes**, from the same shape table the modifiers come from — the two paths that
        // build a fact must say the same thing (a member queried from the file's own scopes went through this one, and
        // its card showed the declaration without the `requires` the index had).
        requires_clause: crate::sema::declarations::the_requires_clause_with(shapes, binding),
        parameter_defaults: crate::sema::declarations::declared_parameter_defaults_with(
            shapes, binding,
        ),
        pattern: crate::sema::declarations::pattern_of(shapes, binding),
        // …but the **parameter list as written** is here, because this is the path a member list's detail line is
        // built from: `size()` and `size_type size() const` are two rows a reader tells apart by exactly this, and
        // the declaration is in the buffer, so reading it costs one walk up the tree. See
        // [DeclFact::parameter_list].
        parameter_list: match binding.kind {
            crate::BindingKind::Function
            | crate::BindingKind::Constructor
            | crate::BindingKind::Destructor
            | crate::BindingKind::ConversionFunction
            | crate::BindingKind::OperatorFunction
            | crate::BindingKind::LiteralOperator => {
                crate::sema::declarations::declared_parameters_with(shapes, binding)
            }
            _ => None,
        },
        // **Who may name it**, read the same way and for the same reason: a member list built from the buffer is
        // what a completion after `.` offers, and a `private:` member must not be in it. See [DeclFact::access].
        access: shapes.access_at(binding.name_range.start_offset),
        // …and whether an importer may, which is the same kind of question about the same position — see
        // [DeclFact::exported]. A buffer's file is very often the module interface unit being edited.
        exported: shapes.exported_at(binding.name_range.start_offset),
        // **A member is a definition of itself** — this path exists for members, and a class body's members have no
        // separate forward declaration to be confused with.
        definition: true,
        range: binding.range,
        name_range: binding.name_range,
        clean: true,
        guard: FactGuard::Unconditional,
    }
}

/// **Is this base spelling one of the class's own template parameters?** — the filter [`members_of`] applies.
///
/// The head of the spelling is what is compared, because that is the name a class is looked up by: `_Tp` and `_Tp<T>`
/// are the same parameter, while `_Vector_base<_Tp, _Alloc>` is a class whose arguments happen to be parameters. A
/// destructor or a qualified spelling answers `false`, which is the direction that keeps a base.
pub(crate) fn names_a_template_parameter(spelling: &str, parameters: &[String]) -> bool {
    if parameters.is_empty() {
        return false;
    }
    let head = without_arguments(spelling.trim());
    let head = head.strip_prefix("::").unwrap_or(head);
    parameters
        .iter()
        .any(|parameter| parameter == head || parameter == head.rsplit("::").next().unwrap_or(head))
}

/// The base classes `class` was written with, from this file or from the index.
///
/// # Names, not spellings
///
/// What comes back is normalized for **lookup**: `public Base<int>` is the class `Base`. The spelling the file
/// wrote stays in [`DeclFact::bases`], which is a fact about the text; a base here is a name to find a class by,
/// and the template arguments say which *type* is inherited rather than which class declares the members. The
/// members of `Base<int>` are the members of `Base`'s primary template, which is the most useful answer available
/// without instantiating anything — and the same rule [`base_type_name`] applies to a declared type.
///
/// # Why this is `Known` and not a list
///
/// "This class has no bases" and "nothing here says what this class inherits from" are different statements, and
/// an empty `Vec` would merge them. The member **lookup** treats them alike — a base it cannot reach contributes
/// no members either way, so it walks on — while the member **list** reports the gap, because a list is a claim
/// about what a type has.
pub(crate) fn bases_of(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<Vec<String>> {
    bases_of_with(
        &crate::sema::declarations::ShapeTable::new(),
        index,
        scopes,
        root,
        path,
        class,
    )
}

/// [ases_of], with the file's declaration table threaded in — see [crate::sema::declarations::ShapeTable].
pub(crate) fn bases_of_with(
    shapes: &crate::sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<Vec<String>> {
    bases_of_inner(shapes, index, scopes, root, path, class)
}

pub(crate) fn bases_of_inner(
    shapes: &crate::sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<Vec<String>> {
    // In this file: the class scope's parent holds the binding of the class's *name*, which is the declaration the
    // bases were written on. Asking the tree through that binding is the same walk the fact builder makes, so the
    // two cannot disagree about what a class inherits from.
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
        return Known::Yes(lookup_names(
            &crate::sema::declarations::declared_bases_of_with(
                crate::sema::declarations::shapes_from(shapes, root),
                binding,
            ),
        ));
    }

    // **Every declaration of the class, not the first one, and `Ambiguous` is not a failure.**
    //
    // A class template is forward-declared and then defined, and only one of the two carries the base clause:
    // `xmemory:554` writes `struct _NO_SPECIALIZATIONS_CITING(…) allocator_traits;` and `755` writes the same
    // class with `: conditional_t<…>` after it. `definition` answers with the singular, so a class declared
    // twice comes back `Unknown(Ambiguous)` and the walk gave up — and even where it answers `Yes`, the one
    // declaration it picked is as likely to be the forward declaration, whose `bases` is empty, as the
    // definition. The plural [`ProjectIndex::definitions`] is the query that returns both, so the bases are
    // the union of what every declaration was written with, in the order the declarations came in.
    //
    // **And the declarations are looked for by their *bare* name as well as by the qualified one**, which is what
    // makes MSVC's stream classes work at all. Measured, and it cost `std::ios::` every one of its members:
    //
    // ```text
    //   std :: basic_ios  @ iosfwd:5011   bases []                 <- the forward declaration, qualified
    //   <file scope> :: basic_ios @ ios:507 bases ["ios_base"]     <- the definition, filed at file scope
    // ```
    //
    // `ios:507` is the definition and it is filed **unqualified**, because a raw reading of `<ios>` does not expand
    // `_STD_BEGIN` — the macro that opens `namespace std` — so the class lands at file scope in that reading while
    // the cooked reading of the same file places it in `std`. A lookup keyed on `std::basic_ios` therefore finds
    // only the forward declaration, whose base clause is empty, and answers "this class has no bases": `fail`,
    // `eof`, `good` and `rdbuf` vanish from `std::ios::` while `unlisted` stays empty, so the answer does not even
    // claim to be incomplete.
    //
    // A **forward declaration can never contribute a base clause**, so taking the union over every declaration of
    // the name cannot invent one — the same over-approximation, and the same direction, as [`lookup_names`] below.
    match index.definitions(class, path) {
        Known::Yes(found) => {
            let mut bases: Vec<String> = Vec::new();
            for declaration in &found.found {
                for base in lookup_names(&declaration.fact.bases) {
                    if !bases.contains(&base) {
                        bases.push(base);
                    }
                }
            }
            if bases.is_empty()
                && let Some(bare) = class.rsplit("::").next()
            {
                for (_, fact) in index.every_fact_named(bare) {
                    if fact.kind != crate::DeclKind::Type {
                        continue;
                    }
                    for base in lookup_names(&fact.bases) {
                        if !bases.contains(&base) {
                            bases.push(base);
                        }
                    }
                }
            }
            Known::Yes(bases)
        }
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::No,
    }
}

/// Base spellings as names a class can be looked up by, in the order they were written.
///
/// **A base clause is not always a class name**, and the names it *does* mention are what a member lookup has
/// to walk into. MSVC's `<xmemory>` writes
///
/// ```cpp
/// struct _NO_SPECIALIZATIONS_CITING("N5014 …") allocator_traits
///     : conditional_t<_Is_default_allocator<_Alloc>::value, _Default_allocator_traits<_Alloc>,
///           _Normal_allocator_traits<_Alloc>> {};
/// ```
///
/// (line 755), and the class it actually derives from is **one of the two named inside** — which one is a
/// question about `conditional_t`, and answering it is metaprogramming this layer does not do. Taking the
/// clause at its word would walk into `conditional_t`, which is an alias with no bases, and stop one step
/// short of every member the class actually has: measured, `members_of("std::allocator_traits")` returned
/// **zero** members while `pointer` and `difference_type` are in `_Normal_allocator_traits`.
///
/// So every class name the clause mentions is offered, which is an **over-approximation on purpose**: it can
/// only find a member one of the candidates really has, and the direction to err in is "found" rather than
/// "missing". A language server that reports a name as undeclared because it could not evaluate a
/// `conditional_t` is one that reports the standard library as broken.
pub(crate) fn lookup_names(bases: &[String]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();

    for base in bases {
        let parsed = crate::sema::types::parse_type_spelling(base);
        collect_class_names(&parsed, &mut names);
    }

    names
}

/// Every class name a parsed type mentions, outermost first, each kept once.
///
/// The walk is over the **parsed type** rather than its text, so `_Normal_allocator_traits<_Alloc>`
/// contributes `_Normal_allocator_traits` and not `_Alloc`, and a class named inside a nested template
/// argument is found at whatever depth it was written.
pub(crate) fn collect_class_names(ty: &crate::sema::types::Type, into: &mut Vec<String>) {
    if let Some(name) = ty.class_name()
        && !name.is_empty()
        && !into.iter().any(|kept| kept == name)
    {
        into.push(name.to_string());
    }

    for argument in ty.arguments() {
        collect_class_names(argument, into);
    }
}

/// The spelling a base-clause name resolves to, given the class it was written in.
///
/// A base name is looked up **from the scope enclosing the class**, outward — so `_Tree` written in
/// `namespace std { class map : _Tree<…> }` names `std::_Tree`, and a `_Tree` at file scope could not answer it
/// even if there were one. Both base walks used to look for the spelling exactly as written, and MSVC's `<map>`
/// inherits from `_Tree`, declared in `<xtree>` inside `_STD_BEGIN` (= `namespace std {`): the member list reported
/// `UnlistedBase { "_Tree", NotDeclaredHere }` while `std::_Tree` sat in the index with 126 members, four of them
/// `find` — which is what the last two queries of `examples/std_query.rs` were failing on (measured).
///
/// The enclosing scopes are tried innermost first and **the spelling as written is the last candidate**, which is
/// what the rule says (the global name space is the outermost scope) and also what keeps a base named at file
/// scope — `struct Derived : public Base` — answering as it always did.
///
/// A base that already carries a `::` is returned untouched: a qualified name is a claim about where the name
/// lives, and `std::_Tree` written in a class in `std` means that name and no other. A **leading** `::` is such a
/// claim too, and `base_type_name` has already taken it off; the name it leaves is the global one, which is what
/// the last candidate below is.
///
/// The same one step of the same rule is what [`resolve_aliases`] does for an alias's target and documents there;
/// both ask it through [`is_declared`], so "the buffer first, the index second" is not decided twice.
pub(crate) fn resolved_in_the_enclosing_scopes(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    path: &Path,
    owner: &str,
    base: &str,
) -> String {
    if base.contains("::") {
        return base.to_string();
    }

    let mut enclosing = owner;
    while let Some((outer, _)) = enclosing.rsplit_once("::") {
        let candidate = format!("{outer}::{base}");
        if is_declared(index, scopes, path, &candidate) {
            return candidate;
        }
        enclosing = outer;
    }

    base.to_string()
}

/// The class whose scope encloses `offset`, for `this`.
///
/// The nearest class-like scope on the chain, which is the same answer in a member function, in a nested class's
/// member function (the nested class wins, correctly) and in a default member initialiser.
pub(crate) fn enclosing_class(scopes: &crate::ScopeTree, offset: usize) -> Option<String> {
    let innermost = scopes.scope_at(offset)?;

    scopes.scope_chain(innermost).into_iter().find_map(|scope| {
        let data = scopes.scope(scope)?;
        // One variant for class, struct and union: the difference is access, which is a member's property
        // rather than the scope's. See `ScopeKind`.
        (data.kind == crate::ScopeKind::Class)
            .then(|| scopes.qualified_name_of(scope))
            .flatten()
    })
}

/// **The class a member function's body belongs to**, whether or not that class is declared in this file.
///
/// [`enclosing_class`] answers from the **scope chain**, which is this file's own tree — and for the ordinary layout of
/// C++ that is not enough. `void Widget::grow(int by) { … }` written in `widget.cpp` declares no class scope at all:
/// the class is in the header, so the chain runs from the function straight to the file, `this` had no type, and a
/// member list after `this->` answered `Unknown(UnparsableName)` — measured in
/// `tests/completion.rs::a_member_function_defined_in_another_file_sees_its_class`.
///
/// What the chain *does* say is the definition's **qualified name** — `Widget::grow` — and the class is its prefix.
/// That is the same spelling the index keys a class's members under, so the answer reaches the header through the
/// layer that already exists for exactly this: "the finding is about this file, the knowledge is the project's".
///
/// A free function's qualified name has no `::` and so names no class, which is the right answer for `this` there —
/// there is none.
pub(crate) fn class_of_the_enclosing_definition(
    scopes: &crate::ScopeTree,
    offset: usize,
) -> Option<String> {
    if let Some(class) = enclosing_class(scopes, offset) {
        return Some(class);
    }

    let innermost = scopes.scope_at(offset)?;
    let qualified = scopes.qualified_name_of(innermost)?;

    let (class, _member) = qualified.rsplit_once("::")?;

    (!class.is_empty()).then(|| class.to_string())
}

/// The name of the class a written type names, with the parts that do not affect *which* class it is removed.
///
/// `const Widget&` → `Widget`, `std::vector<int>` → `std::vector`, `struct Widget` → `Widget`, `::Widget` →
/// `Widget`. The goal is a spelling that can be looked up as a qualified name, and every one of those parts is
/// about the type's shape or about which name space it is in rather than about its name. `unsigned long` is left
/// alone: there the words *are* the type, and it names no class anyway.
///
/// # Why the leading `::` comes off
///
/// A leading `::` asks about the **global** name space — a real distinction for a *name* lookup, where dropping it
/// would also match `ns::Widget`, and [`matches`] honours it for exactly that reason. It is not a distinction for
/// this walk, because the walk does not ask "which name space"; it asks for the qualified spelling the index keys
/// on, and a global declaration's spelling is its bare name. Keeping the prefix made `::Widget w;` — the type
/// spelling a consumer hands over verbatim from `DeclFact.type_of` — fail to find a class sitting in the buffer.
/// **Where the `<` is that opens a template argument list running to the end of `written`.**
///
/// `None` for a spelling with no such group — `outer<int>::handle_type` ends with `type`, so its `<` belongs to a
/// *qualifier* and is not a list of the type's own. Nesting is counted backwards so `Box<Pair<int, int>>` answers the
/// `Box` opening and not the `Pair` one.
pub(crate) fn the_trailing_argument_list(written: &str) -> Option<usize> {
    if !written.ends_with('>') {
        return None;
    }
    let mut depth = 0usize;
    for (at, character) in written.char_indices().rev() {
        match character {
            '>' => depth += 1,
            '<' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn base_type_name(written: &str) -> &str {
    let mut name = written.trim();

    // **Template arguments, and only the ones at the END of the spelling.**
    //
    // The members of `std::vector<int>` are the members of `std::vector`'s primary template, which is the most useful
    // answer available without instantiating anything. But a **member** of a class template is spelled
    // `outer<int>::handle_type`, and cutting at the first `<` turns that into `outer` — the class, not the member —
    // so every question below was asked about the wrong type. Measured: `outer<int>::handle_type h; h.` offered
    // `outer`'s own typedefs, and a coroutine's `task<int>::handle_type` offered `await_ready`/`take` instead of
    // `resume`/`promise`. The declarators and the leading `::` are taken off first so that the group at the end is
    // really the end (`std::vector<int>*` and `::std::vector<int>` both arrive here).
    loop {
        let before = name;
        // Declarators written after the type: `Widget*`, `Widget&`, `Widget&&`.
        name = name.trim_end_matches(['*', '&']).trim();
        // The global name space, which for a qualified spelling is no prefix at all.
        name = name.strip_prefix("::").unwrap_or(name).trim();
        if let Some(opening) = the_trailing_argument_list(name) {
            name = name[..opening].trim();
        }
        if name == before {
            break;
        }
    }

    // An elaborated specifier: `struct Widget` and `Widget` name one class, and only the second is a spelling the
    // index matches.
    for keyword in ["struct ", "class ", "union ", "enum "] {
        if let Some(rest) = name.strip_prefix(keyword) {
            name = rest.trim();
            break;
        }
    }

    name
}
