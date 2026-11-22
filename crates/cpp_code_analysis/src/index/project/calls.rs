//! **Calls** — the declaration a call names, the overload its arguments pick, and the type it returns.
//!
//! Moved here from `project/mod.rs` without a change. A call is the question with the most evidence and the most
//! ambiguity: the callee may be a function, a type, a member, a macro-shaped factory or a range algorithm, the
//! arguments may select one overload out of many, and the answer must be `Known::Unknown` rather than a guess when
//! they do not settle it.

use super::*;

pub(crate) fn declaration_of_expression(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    expression: &cpp_parser::CppSyntaxNode,
) -> Known<NamedDeclaration> {
    let offset = end_of_the_written_name(expression);
    let written = expression.text().to_string();
    let written = written.trim();

    let asked = crate::sema::resolve::definition_at(scopes, root, offset);

    match asked {
        Known::Yes(binding) => Known::Yes(NamedDeclaration::Here(binding)),
        Known::Unknown(UnknownReason::NotDeclaredHere(name)) => {
            // **C++'s unqualified member lookup comes first, and the class may be in another file.** Inside a member
            // function a name is looked up in the class and its bases before the enclosing namespaces; the file's own
            // scope tree has no scope for a class declared in a header, so the lookup above answered "not here" and
            // the index-wide query is what answered next — and that query is `Ambiguous` when several classes declare
            // the name. `EmmyLuaCodeStyle`'s `_tokens` is a member of `LuaLexer`, `LuaParser` **and** `LuaSyntaxTree`,
            // so `type_at("_tokens")` in `LuaParser.cpp` answered `Unknown(Ambiguous("_tokens"))` and
            // `_tokens.size()` could not resolve `size` at all. The class is named by the definition's own declarator
            // (`LuaParser::Parse`), and [`a_member_of_the_enclosing_class`] is the query that knows it — the same one
            // the hover uses.
            //
            // A finished type only: a member of a template base whose arguments the member list could not read is
            // refused rather than answered with the parameter's spelling.
            if let Some(member) =
                a_member_of_the_enclosing_class(index, scopes, root, path, expression, true)
            {
                return Known::Yes(NamedDeclaration::Indexed(
                    member.fact,
                    member.file,
                    TypeBindings::default(),
                ));
            }
            // **A name whose spelling carries a macro prefix is two spellings, and one of them is real.** `_STD`
            // is `::std::` to a compiler and an ordinary identifier to a file that has not read `<yvals.h>`, so
            // the expression reads the pair as one name — `_STD f`, `_CSTD memmove` — and the lookup then asks the
            // index for a name nobody declares. Measured, this is what most of the `auto` declarations in MSVC's
            // headers were refused for: 476 of them reached this arm and stopped.
            //
            // The part **after** the prefix is the name, and it is tried second rather than first: a file whose
            // macro the analysis *can* see has its full spelling bound in the scope tree, and that case never
            // reaches here at all. What is left is the prefix, and dropping it is what a reader does with the
            // line in front of them.
            //
            // **Two separators, because the name arrives in two spellings.** A macro prefix and what follows it are
            // written `_STD f` — two words — and the reader above builds its qualified spelling with `::`, so the
            // name this arm is handed is **`_STD::f`**. Measured, that is exactly what it was: the space-split never
            // matched, and `auto a = _STD f();` stayed refused while `auto a = f();` gave `int`.
            //
            // Splitting a `::` is only safe when the **first segment is written like a macro** (`_STD`, `_CSTD`,
            // `_RANGES` — the leading underscore MSVC uses for every one of them), because `std::f` is a real
            // qualified name and dropping `std` to find some other `f` would be a wrong answer rather than a
            // missing one. That is the same discriminator the expression grammar uses for the same question, and it
            // is a spelling here for the same reason: the environment that could settle it is not part of a name's
            // spelling, and the segment in front of a `::` is what a reader would look at.
            let mut candidates: Vec<&str> = vec![name.as_ref()];

            // **A leading `::` is not part of the name.** It says "start at the global namespace", and for a name
            // that is already qualified — which is every name this arm is handed — that is a no-op: `::std::f` and
            // `std::f` are one entity. The index stores the second spelling, so a lookup with the first finds nothing.
            //
            // This is what kept every `_STD`-prefixed call in the standard library refused, and the count said so
            // without saying where: 54 `auto` declarations over eight MSVC headers, every one of them
            // `UnknownType("::std::…")`. The comment above notes that the spelling arrives as `_STD::f` — and for
            // `_STD`, which is `::std::`, that is `::std::f`. The fallback below is asked about the segment **before**
            // the first `::`, which for a leading `::` is the empty string: never macro-like, so it never fired.
            if let Some(without) = name.strip_prefix("::") {
                candidates.push(without);
            }

            if let Some((_, without_the_prefix)) = name.split_once(' ') {
                candidates.push(without_the_prefix.trim());
            }
            if let Some((first, rest)) = name.split_once("::")
                && written_like_a_macro(first)
            {
                candidates.push(rest);
            }

            // **Whether the index said "more than one" rather than "nothing"**, which is the difference between an
            // ambiguity a reader can act on and a name nothing declares. See the final arm below.
            let mut was_ambiguous = false;
            for candidate in candidates {
                let found: Option<(crate::DeclFact, PathBuf)> =
                    match index.definition(candidate, path) {
                        Known::Yes(found) => Some((found.fact, found.file)),
                        // **Several declarations of one name, and one type between them.** A name query answers
                        // `Ambiguous` when more than one declaration is visible, and that is right for "where is this
                        // declared" — but the question here is *what type it has*, and a type is not ambiguous when
                        // every declaration spells it the same way. MSVC's `<iostream>` declares `cin` twice (once
                        // plain, once as `_EXPORT_STD extern "C++" … istream cin;`), so `std::cin` was `Ambiguous` and
                        // every use of it lost its type: `std::cin.read(…)`, `std::cin.eof()`, a completion after
                        // `std::cin.` — measured, 8 offsets in one file. Candidates that **disagree**, and functions
                        // (which have no type as a name), keep the `Unknown` that the name query gave.
                        Known::Unknown(UnknownReason::Ambiguous(_)) => {
                            was_ambiguous = true;
                            agreeing_type(index, path, candidate)
                        }
                        Known::Unknown(_) | Known::No => None,
                    };

                if let Some((fact, file)) = found {
                    return Known::Yes(NamedDeclaration::Indexed(
                        fact,
                        file,
                        TypeBindings::default(),
                    ));
                }
            }

            // **Several declarations this file can see, and no type they all agree on.** The reason is kept rather
            // than flattened into `UnknownType`: `Ambiguous` already has a sentence a reader can act on ("declared
            // more than once in what this file can see, and nothing here chooses between the declarations"), and it is
            // what the editor report of `auto enenn = std::format(…)` needed — the popup said nothing at all, and the
            // reason it had nothing to say was exactly this. Measured on that file: `std::format` has four overloads
            // visible (narrow and wide, with and without a locale) and none of them is selected by the call.
            if was_ambiguous {
                Known::Unknown(UnknownReason::Ambiguous(Box::from(written)))
            } else {
                Known::Unknown(UnknownReason::UnknownType(Box::from(written)))
            }
        }
        // A name this layer cannot place at all is not a type it can read. `No` means the offset is not on a name;
        // any other `Unknown` is already the most specific answer available and is passed through.
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
    }
}

/// The type of a **call**: `make().size` needs this, and so does `fac.build().size`.
///
/// # The two callees, and the one lookup behind them
///
/// ```text
/// make()          a name: what it returns
/// Widget()        a name, and the same tokens — a temporary of the class it names
/// fac.build()     a member access: what the member returns
/// ns::make()      a qualified name: what it returns
/// ```
///
/// The first two are one case because only the *declaration* separates them, and the last two go through the
/// lookups that already exist — the member lookup for a member call, the name lookup for everything else. What
/// this adds is the last step: the callee's declaration says what a call of it has ([`what_a_call_has_in`]).
///
/// Everything else stays [`UnknownReason::UnknownType`]: a call of a function pointer, of a lambda, of a template
/// parameter whose type is not known here. Each of those needs a type *computed* rather than read off a
/// declaration, which is the same boundary the rest of this function has.
/// **What the lambda the callee names returns**, when the callee is a name initialised with one.
///
/// ```cpp
/// auto doubler = [](int value) { return value * 2; };
/// auto result = doubler(3);      // what `result` is
/// **What a call through a function pointer has** — the return type of the function it points at.
///
/// `int (*fp)(int)` is a `Pointer` to a `Function`, and calling `fp(3)` has what that function returns. `None` for every other
/// type, which is the ordinary case: a name whose type is not a function pointer is not called through one.
pub(crate) fn a_function_pointer_returns(a_type: &Type) -> Option<Type> {
    let Type::Pointer { to } = a_type else {
        return None;
    };
    match to.as_ref() {
        Type::Function { returns, .. } => Some(returns.as_ref().clone()),
        // A pointer to a *pointer* to a function is not this: C++ would need the intermediate dereference, and answering
        // the deep return type would make `(**fpp)(1)` and `fpp(1)` the same call.
        _ => None,
    }
}

/// ```text
///
/// A lambda's type is unnamed, so `type_of` on the name answers nothing and the call has no return type to read. The syntax
/// does hold the answer: the declarator that carries the name also carries the lambda.
///
/// **Any binding kind is accepted**, because a name whose value can be called is recorded as a `Function` in this scope tree —
/// measured, `auto doubler = […];` gives `kind=Function` with the variable's own name range — and the question here is not what
/// kind it is, but whether a lambda is what it was initialised with.
pub(crate) fn a_lambdas_body_the_callee_names(
    root: &cpp_parser::CppSyntaxNode,
    scopes: &crate::ScopeTree,
    callee: &cpp_parser::CppSyntaxNode,
    argument_types: &[Type],
) -> Option<String> {
    let inside = callee.text_range();
    let Known::Yes(binding) =
        crate::sema::resolve::definition_at(scopes, root, inside.start().into())
    else {
        return None;
    };
    // **The declarator that holds the name**, found by the range the binding already carries — no name comparison, which
    // would confuse two names of the same spelling in sibling scopes.
    let name = binding.name_range;
    let declarator = root.descendants().find(|node| {
        let range = node.text_range();
        CppSyntaxKind::from(node.kind()) == CppSyntaxKind::InitDeclarator
            && usize::from(range.start()) <= name.start_offset
            && name.end_offset() <= usize::from(range.end())
    })?;
    let lambda = declarator
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::LambdaExpr)?;
    crate::sema::declarations::a_lambdas_return_type(&lambda, argument_types)
}

pub(crate) fn type_of_a_call(
    memo: &TypeMemo,
    shapes: &crate::sema::declarations::ShapeTable,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    call: &cpp_parser::CppSyntaxNode,
    // **The recursion bound, carried through this call** — the return type of a function whose body returns a *call*
    // is typed by typing that call (see [`the_return_the_body_gives_back`]), which is one more question of the same
    // kind. Without the bound, `auto f() { return g(); } auto g() { return f(); }` is an infinite walk.
    depth: usize,
) -> Known<(Type, PathBuf)> {
    let written = call.text().to_string();
    let written = written.trim();

    let Some(callee) = call.children().next() else {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    };

    // **The template arguments the call wrote**, which is the half of a function-template call that was being read
    // and thrown away: `_Convert_size<size_type>(_Length)` names the type it returns, and nothing was pairing that
    // name with the declaration's parameters. Read from the **syntax** rather than from the text, for the reason
    // `read_template_arguments` gives — an argument's text can contain commas a splitter would get wrong.

    // **An iterator-producing member call is a view of its container's element** — `v.begin()`, `m.find(k)`.
    //
    // The standard library's iterator types are class templates this layer cannot instantiate, so their own
    // members are not readable; what a reader writes next is `it->first`, `it->second` and `*it`, and all three are
    // questions about the **element**. Answering the element type is what makes them work — and the declared
    // `iterator` type, whose members are `operator*` and four typedefs, is both harder to compute and less useful.
    if let Some(element) =
        iterator_element_type(memo, index, declaring, scopes, root, path, &callee)
    {
        return element;
    }

    // **The template arguments the call wrote, from the *call* and not only from the callee.** A member call's `‹…›`
    // may be a sibling of the member-access node rather than a descendant of it — measured on a real project:
    // `_server->GetService<CommandService>()` found no written arguments, so the member's `returns` came back with
    // its template parameter still standing (`Service *`) and nothing after the arrow could be typed. The arguments
    // are searched for **before the argument list**, so that `f(g<int>(1))` cannot lend its inner `<int>` to `f`.
    let written_arguments = {
        let from_the_callee = written_template_arguments(&callee);
        if from_the_callee.is_empty() {
            call.children()
                .take_while(|child| {
                    CppSyntaxKind::from(child.kind()) != CppSyntaxKind::ArgumentList
                })
                .filter_map(|child| {
                    // **The child may *be* the list** — `descendants()` does not include the node it is called on,
                    // and a member call's template arguments sit beside the member access rather than inside it.
                    if CppSyntaxKind::from(child.kind()) == CppSyntaxKind::TemplateArgumentList {
                        return Some(child);
                    }
                    child.descendants().find(|node| {
                        CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TemplateArgumentList
                    })
                })
                .next()
                .map(|list| crate::sema::types::read_template_arguments(&list))
                .unwrap_or_default()
        } else {
            from_the_callee
        }
    };

    // **The types of the arguments the call passed**, which is the other way a function template is told what its
    // parameters stand for — and the way the standard library uses almost everywhere: `_Get_unwrapped(_First)`
    // never writes `<int*>`, it passes one. Computed only when nothing was written, which is the only case it is
    // used in, because typing every argument of every call is not free.
    let argument_types: Vec<Type> = if written_arguments.is_empty() {
        crate::sema::call_syntax::arguments_of(call)
            .unwrap_or_default()
            .iter()
            .filter_map(|argument| {
                match type_of_expression_memoed(
                    memo, shapes, index, declaring, scopes, root, path, argument, 0,
                ) {
                    Known::Yes((type_of, _)) => Some(type_of),
                    _ => None,
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    // **`std::move` and `std::forward` are the identity on the type a reader can see.**
    //
    // Their return type is `remove_reference_t<T>&&` — a reference to the argument's own class — and every question a
    // reader asks next is about that class: `std::move(x).size()`, or `auto y = std::move(x); y.push_back(…)`. The
    // standard library declares them as templates whose answer needs deduction from the argument, which is what the
    // `argument_types` above already computed, so the useful answer is that type.
    //
    // Measured on a real project (`EmmyLuaCodeStyle`'s `CodeFormatCLib.cpp`): `auto diagnostic_results =
    // std::move(result.Data);` answered `UnknownType("std::move")` — the *callee's spelling* as if it were a type —
    // so a range-`for` over the variable could not type its element and `diagnostic.Message` reported "the type of
    // `diagnostic` is not known here". The `&&` is not part of the answer for the same reason `auto` drops it: it
    // carries nothing this layer can query, and a class name is what the next question needs.
    if names_the_identity_function(&callee)
        && let Some(type_of) = argument_types.first()
    {
        return Known::Yes((type_of.clone(), path.to_path_buf()));
    }

    // **A call through a function pointer** — `int (*fp)(int) = negate; fp(3)`. The callee is a variable whose type is a
    // *pointer to a function*, so what the call has is that function's return type. Measured: `auto through_pointer = fp(3)`
    // answered *"the type of `fp(3)` is not known here"*.
    if let Known::Yes((callee_type, file)) = type_of_expression_memoed(
        memo, shapes, index, declaring, scopes, root, path, &callee, 0,
    ) && let Some(returns) = a_function_pointer_returns(&callee_type)
    {
        return Known::Yes((returns, file));
    }

    // **A call of a lambda** — `auto f = [](int v) { return v * 2; }; f(3)`. The name has no type that says anything (a
    // lambda's type is unnamed), so the answer comes from the body of the lambda it was initialised with — the same deduction
    // an `auto`-returning function goes through. Measured: `auto result = doubler(3)` answered *"the type of `doubler(3)` is
    // not known here"* while the body plainly returns an `int`.
    if let Some(returns) = a_lambdas_body_the_callee_names(root, scopes, &callee, &argument_types) {
        return Known::Yes((
            crate::sema::types::parse_type_spelling(&returns),
            path.to_path_buf(),
        ));
    }

    // **A call of an object** — `f(1)` where `f`'s type declares `operator()`. The callee is not a function
    // declaration at all, so the lookup below would answer `No`; what the call has is what the operator returns.
    //
    // **Asked after the callee's declaration, and only when that is not a function.** This model has no function
    // type: the type of a *member function's name* is what the member **returns** — `vFile.GetSyntaxTree` types as
    // `std::optional<LuaSyntaxTree>` — so asking "does this callee have an `operator()`?" of a perfectly declared
    // member call answered **`std::optional`'s `operator()`**, and `auto op = vFile.GetSyntaxTree(vfs);` deduced
    // `value_type`. Measured on a real project: four unrelated-looking hover cards were this one arm, because the
    // wrong answer propagated to everything the variable fed.
    let resolved = declaration_of_a_callee(
        memo, shapes, index, declaring, scopes, root, path, &callee, written,
    );
    let names_a_function = match &resolved {
        Known::Yes(named) => named.fact_kind() == crate::DeclKind::Function,
        _ => false,
    };
    if !names_a_function
        && let Known::Yes((callee_type, file)) = type_of_expression_memoed(
            memo, shapes, index, declaring, scopes, root, path, &callee, 0,
        )
        && matches!(callee_type, Type::Named { .. })
        && let Some(returns) = operator_returns_of_with_arguments(
            index,
            declaring,
            scopes,
            root,
            path,
            &callee_type,
            "operator()",
            &argument_types,
            crate::sema::call_syntax::arguments_of(call).map(|arguments| arguments.len()),
        )
    {
        return Known::Yes((crate::sema::types::parse_type_spelling(&returns), file));
    }

    // **The standard library's factories, whose return type the standard fixes.** `make_shared<T>(args…)` is a
    // `shared_ptr<T>` and `make_unique<T>(args…)` a `unique_ptr<T>` — but the implementation is free to return a wrapper:
    // libstdc++ writes `shared_ptr<_NonArray<_Tp>>`, and substitution produced `shared_ptr<_NonArray<Node>>`, whose
    // `element_type` is the wrapper. A member reached through the arrow then looked in `std::shared_ptr` and answered
    // `Unknown`, measured on `auto twin = node->twin();`. The table is two families and says only what [util.smartptr]
    // says, so it holds for any library.
    if let Some(pointer) = the_factory_the_call_names(&callee)
        && let Some(argument) = written_arguments.first()
    {
        return Known::Yes((
            crate::sema::types::parse_type_spelling(&format!("{pointer}<{argument}>")),
            path.to_path_buf(),
        ));
    }

    // **`std::define_static_array(r)`** (C++26, P2996): the elements of a range as a static array — the standard says
    // `const T(&)[N]`, where `T` is the range's element type. libstdc++'s declaration is
    // `template<ranges::input_range R> consteval auto define_static_array(R&& r)` with a body built out of reflection
    // intrinsics, so **no fact can answer** this one: the return type is `auto` and the returns are arrays made by
    // machinery this layer does not model. Measured on `C:\Users\zc\Desktop\cpp\src\reflect.hpp`, where
    // `auto members = std::define_static_array(…);` had no type at all — and so no inlay hint and a hover that could
    // only say "not known".
    //
    // The **bound** is the number of elements, which is a value this layer does not compute, so the array is the
    // unknown-bound one — `const T(&)[]`. That is a type C++ spells, and it is exactly what is known here: a reference
    // to an array of `T`.
    // **The argument is typed first, and that is the whole difference**: the range helper below deliberately answers
    // only for *values*, and the argument here is a call (`std::meta::nonstatic_data_members_of(…)`), whose type the
    // ordinary expression typer does know. `vector<std::meta::info>` yields the element `std::meta::info`.
    if names_define_static_array(&callee)
        && let Some(first) =
            crate::sema::call_syntax::arguments_of(call).and_then(|nodes| nodes.into_iter().next())
        && let Known::Yes((argument, _)) = type_of_expression_memoed(
            memo, shapes, index, declaring, scopes, root, path, &first, 0,
        )
        && let Some(element) = range_element_of(as_a_range(&argument))
    {
        let element = crate::sema::types::parse_type_spelling(&element);
        let element = Arc::new(Type::Qualified {
            of: Arc::new(element),
            qualifiers: crate::sema::types::Qualifiers::CONST,
        });
        let array = Arc::new(Type::Array {
            of: element,
            extent: None,
        });
        return Known::Yes((
            Type::Reference {
                to: array,
                rvalue: false,
            },
            path.to_path_buf(),
        ));
    }

    // **The ranges algorithms, whose return type the standard fixes.** The implementation's `iterator` is a class
    // template over a closure type, so the index cannot spell it; what a reader does with the result is dereference it or
    // count with it, and those are the two answers below. See [`a_ranges_algorithm_the_call_names`].
    if let Some(algorithm) = a_ranges_algorithm_the_call_names(&callee) {
        let arguments = crate::sema::call_syntax::arguments_of(call).unwrap_or_default();
        match algorithm.as_str() {
            "find" | "find_if" | "find_if_not" | "max" | "min" | "max_element" | "min_element"
            | "lower_bound" | "upper_bound" | "equal_range" | "search" => {
                if let Some(element) = arguments.first().and_then(|first| {
                    element_of_a_range_expression(
                        memo, index, declaring, scopes, root, path, first, 0,
                    )
                }) {
                    return element;
                }
            }
            "count" | "count_if" | "distance" | "ssize" => {
                return Known::Yes((
                    crate::sema::types::parse_type_spelling("std::ptrdiff_t"),
                    path.to_path_buf(),
                ));
            }
            "size" => {
                return Known::Yes((
                    crate::sema::types::parse_type_spelling("std::size_t"),
                    path.to_path_buf(),
                ));
            }
            "subrange" => {
                if let Some(first) = arguments.first()
                    && let Some(element) = element_of_an_iterator_expression(
                        memo, index, declaring, scopes, root, path, first, 0,
                    )
                {
                    return element;
                }
            }
            _ => {}
        }
    }

    // **A call whose callee names a class is a temporary of that class** — `std::shared_ptr<int>(p, deleter)`,
    // `Widget(1)`. The constructor is not a function the spelling names, so every arm above answers nothing, and the
    // class (with its written template arguments, which `parse_type_spelling` reads) was in the spelling all along.
    // Measured: `std::shared_ptr<int>(shared, nullptr).use_count()` answered *"the type of … is not known here"*, and the
    // hover on the member then had no class to look it up in.
    if let Some(spelling) = a_callee_that_names_a_type(index, path, &callee) {
        return Known::Yes((
            crate::sema::types::parse_type_spelling(&spelling),
            path.to_path_buf(),
        ));
    }

    let resolved = declaration_of_a_callee(
        memo, shapes, index, declaring, scopes, root, path, &callee, written,
    );
    let known_so_far = match resolved {
        Known::Yes(named) => match named.what_a_call_has(
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            &written_arguments,
            &argument_types,
        ) {
            Some(type_of) => return Known::Yes((type_of, named.file(path))),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        },
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
    };

    // **A call of an overload set, where the call's own arguments choose.** `std::to_string(count)` has five declarations
    // visible and one takes an `int`; `std::get<int>(either)` has three the `variant` can reach, and the survivors spell the
    // written `int`. The name query answers `Ambiguous` — right for "where is this declared", wrong for "what does this call
    // return", and the argument types are exactly the thing that decides. See
    // [`the_return_the_overloads_agree_on`] for the two questions it asks and what it refuses.
    if let Some(returns) = the_return_the_overloads_agree_on(
        index,
        path,
        &the_names_the_call_may_use(&callee),
        &written_arguments,
        &argument_types,
        crate::sema::call_syntax::arguments_of(call)
            .map(|arguments| arguments.len())
            .unwrap_or_default(),
        memo,
        declaring,
        scopes,
        root,
        depth,
    ) {
        return Known::Yes((
            crate::sema::types::parse_type_spelling(&returns),
            path.to_path_buf(),
        ));
    }

    known_so_far
}

/// **The smart pointer a factory function returns** — `make_shared` → `shared_ptr`, `make_unique` → `unique_ptr`.
///
/// Asked of the **last** segment of the callee`s spelling, because the standard library's are written `std::make_shared`
/// and a project may write `using std::make_shared;` and then call it bare. A name that is only *similar* is not in the
/// table, and the ordinary arms answer for it.
pub(crate) fn the_factory_the_call_names(
    callee: &cpp_parser::CppSyntaxNode,
) -> Option<&'static str> {
    let spelled = callee.text().to_string();
    let spelled = spelled.trim();
    // The template arguments are the type the factory makes, not part of its name.
    let bare = match spelled.find('<') {
        Some(at) => spelled[..at].trim(),
        None => spelled,
    };
    let last = bare.rsplit("::").next().unwrap_or(bare);
    match last {
        "make_shared" => Some("shared_ptr"),
        "make_unique" | "make_unique_for_overwrite" => Some("unique_ptr"),
        _ => None,
    }
}

/// **Is the callee `std::move` or `std::forward`?** — the two standard calls that hand back their argument's type.
///
/// Asked of the **qualified** spelling only. An unqualified `move(x)` may be a project's own function with a meaning of
/// its own, and treating it as the standard's would be a guess about a name this layer can see declared elsewhere; the
/// standard's spelling is `std::move`, and that is what a file writes unless it has said `using std::move;` first.
pub(crate) fn names_the_identity_function(callee: &cpp_parser::CppSyntaxNode) -> bool {
    let spelled = callee.text().to_string();
    let spelled = spelled.trim();
    // The template arguments are not part of the name, and `std::move<int>` is not a spelling anyone writes.
    let bare = match spelled.find('<') {
        Some(at) => spelled[..at].trim(),
        None => spelled,
    };
    matches!(
        bare,
        "std::move" | "std::forward" | "::std::move" | "::std::forward"
    )
}

/// **The callee's spelling when it names a class** — the temporary a call of a type constructs.
///
/// A template-id is kept as written, because its arguments are part of the type: `std::shared_ptr<int>`. The name is
/// looked up without them (`std::shared_ptr`), and every declaration of it must be a type — the ordinary case for a class
/// template, whose first declaration is a forward one, so the singular query answers `Ambiguous` and the plural one says
/// whether they agree about being a class.
/// **The name before a template-id's arguments** — `make_sized<3>` → `make_sized`, and `None` for anything else.
///
/// The rule is C++'s own spelling and nothing more: a name-like prefix, a `<`, and a `>` that ends the text. A member
/// access and an operator are not touched — the caller asks this only of a callee whose ordinary lookup answered nothing.
pub(crate) fn the_name_before_its_template_arguments(
    callee: &cpp_parser::CppSyntaxNode,
) -> Option<String> {
    let kind = CppSyntaxKind::from(callee.kind());
    if !matches!(
        kind,
        CppSyntaxKind::NameExpr | CppSyntaxKind::IdentifierExpr
    ) {
        return None;
    }
    let text = callee.text().to_string();
    let text = text.trim();
    let open = text.find('<')?;
    if open == 0 || !text.ends_with('>') {
        return None;
    }
    let name = &text[..open];
    let names_something = name
        .chars()
        .all(|character| character.is_alphanumeric() || character == '_' || character == ':');
    names_something.then(|| name.to_string())
}

/// **The ranges algorithm a callee names** — `std::ranges::find`, `ranges::find`.
///
/// The name is read from the spelling the call wrote, and only the `std::ranges` family is recognised: a project's own
/// `find` is a function whose declaration the ordinary path reads, and answering it from a table would be a guess.
pub(crate) fn a_ranges_algorithm_the_call_names(
    callee: &cpp_parser::CppSyntaxNode,
) -> Option<String> {
    let written = callee.text().to_string();
    let written = written.trim();
    let last = written.rsplit("::").next()?;
    // **A call written in the ranges namespace's own spelling** — `std::ranges::find`, `ranges::find`. A bare `find`
    // is left to the ordinary path: a project declares one as often as not, and the table would take it.
    let in_the_ranges_namespace = written.contains("ranges::");
    let is_an_algorithm = matches!(
        last,
        "find"
            | "find_if"
            | "find_if_not"
            | "max"
            | "min"
            | "max_element"
            | "min_element"
            | "count"
            | "count_if"
            | "distance"
            | "size"
            | "ssize"
            | "subrange"
            | "lower_bound"
            | "upper_bound"
            | "equal_range"
            | "search"
    );
    (in_the_ranges_namespace && is_an_algorithm).then(|| last.to_string())
}

pub(crate) fn a_callee_that_names_a_type(
    index: &ProjectIndex,
    path: &Path,
    callee: &cpp_parser::CppSyntaxNode,
) -> Option<String> {
    let spelled = callee.text().to_string();
    let spelled = spelled.trim();
    if spelled.is_empty() || spelled.contains(' ') {
        return None;
    }
    let bare = match spelled.find('<') {
        Some(at) => spelled[..at].trim(),
        None => spelled,
    };
    let names_a_type = match index.definition(bare, path) {
        Known::Yes(found) => found.fact.kind == crate::DeclKind::Type,
        Known::Unknown(UnknownReason::Ambiguous(_)) => {
            index.definitions(bare, path).value().is_some_and(|found| {
                !found.found.is_empty()
                    && found
                        .found
                        .iter()
                        .all(|declaration| declaration.fact.kind == crate::DeclKind::Type)
            })
        }
        Known::Unknown(_) | Known::No => false,
    };
    names_a_type.then(|| spelled.to_string())
}

/// **The spellings a call's callee may be declared under** — its own text without the template arguments, and its last
/// `::` segment. `std::to_string(count)`'s callee is `std::to_string` and `to_string`, two names the index may hold.
pub(crate) fn the_names_the_call_may_use(callee: &cpp_parser::CppSyntaxNode) -> Vec<String> {
    let spelled = callee.text().to_string();
    let spelled = spelled.trim();
    // **The template arguments are not part of the name**: `get<int>` is `get`, and `<int>` is what the call *wrote*.
    let bare = match spelled.find('<') {
        Some(at) => spelled[..at].trim(),
        None => spelled,
    };
    let last = bare.rsplit("::").next().unwrap_or(bare);
    let mut names = vec![bare.to_string()];
    if last != bare {
        names.push(last.to_string());
    }
    names
}

/// **What an overload set returns, when every candidate the call could reach agrees.**
///
/// A name declared more than once answers `Ambiguous` to the name query — right for "where is this declared" and wrong for
/// "what does this call return", because the call's own arguments choose. Two questions narrow the set, and neither is a
/// guess:
///
/// * **can this candidate be called this way** — the arity fits and every parameter takes the argument in its position. Only
///   a *definite* refusal drops a candidate (`sema::convert::conversion`'s `Refused`); one this layer cannot compare keeps
///   its place, because dropping it would be the same guess in the other direction;
/// * **do the survivors say the same type** — their return spellings, with the call's **written** template arguments
///   substituted in, read as `auto` would read them (no top-level reference, no `const`).
///
/// `std::to_string(count)` has five overloads and the one taking an `int` answers `std::string`; `std::get<int>(either)` has
/// two left once the unexpanded spelling is dropped, and both say `int` — with and without a reference, which `auto` then
/// removes. A spelling that still holds a template-id after substitution (`variant_alternative_t<int, …>`) names a trait this
/// cannot evaluate, so it is **dropped** rather than compared: it would disagree with every concrete answer for a reason that
/// is not about the type. A set that still disagrees answers `None` — choosing between them is overload *ranking*, which this
/// layer does not do.
pub(crate) fn the_return_the_overloads_agree_on(
    index: &ProjectIndex,
    path: &Path,
    names: &[String],
    written_arguments: &[Type],
    argument_types: &[Type],
    how_many_arguments: usize,
    memo: &TypeMemo,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    depth: usize,
) -> Option<String> {
    for name in names {
        let Known::Yes(found) = index.definitions(name, path) else {
            continue;
        };
        let mut agreed: Option<String> = None;
        let mut any = false;
        for candidate in found
            .found
            .iter()
            .filter(|declaration| declaration.fact.kind == crate::DeclKind::Function)
        {
            // **A deleted function is not a candidate** — [dcl.fct.def.delete]: it participates in overload resolution
            // only to make the call ill-formed. `std::make_unique` declares
            // `template <class T, class... Args> void make_unique(Args&&...) = delete;` to stop `make_unique<T[]>(…)`,
            // and keeping it made the set disagree with the two real overloads around it.
            if candidate
                .fact
                .modifiers
                .contains(crate::summary::DeclModifiers::DELETED)
            {
                continue;
            }
            // **A function the summary has no return type for may still have one in its body.** The summary types a
            // `return` expression syntactically, where a call cannot be resolved — so `auto make() { return
            // make_thing(); }` and `auto field(W w) { return w.member; }` have no `returns` at all, and every call to
            // them was untyped. Here the declaring file's own view and scopes are in reach, so the expression is
            // typed with the machinery a use in that file gets. See [`the_return_the_body_gives_back`].
            let written: String = match candidate.fact.returns.as_deref() {
                Some(written) => written.to_string(),
                None => match the_return_the_body_gives_back(
                    memo, index, declaring, scopes, root, path, depth, candidate,
                ) {
                    Some(written) => written,
                    None => continue,
                },
            };
            if !a_candidate_that_can_be_called_this_way(
                candidate.fact.parameter_list(),
                argument_types,
                how_many_arguments,
            ) {
                continue;
            }
            let substituted =
                if written_arguments.is_empty() || candidate.fact.parameters().is_empty() {
                    written.clone()
                } else {
                    let arguments: Vec<String> = written_arguments
                        .iter()
                        .map(|one| one.to_string())
                        .collect();
                    crate::sema::types::substituted_spelling(
                        &written,
                        candidate.fact.parameters(),
                        &arguments,
                    )
                };
            // **A spelling that still names one of the candidate's own parameters could not be substituted** —
            // `variant_alternative_t<int, variant<_Types...>>` keeps `_Types` — and comparing it with a concrete
            // answer would make every set holding such an overload disagree for a reason that is not about the type.
            let still_unsubstituted =
                candidate.fact.parameters().iter().any(|parameter| {
                    !parameter.is_empty() && substituted.contains(parameter.as_str())
                });
            if still_unsubstituted {
                continue;
            }
            let read_as_auto_reads_it = read_the_way_auto_does(&substituted);
            if read_as_auto_reads_it.is_empty() {
                continue;
            }
            any = true;
            match &agreed {
                None => agreed = Some(read_as_auto_reads_it),
                Some(previous) if *previous == read_as_auto_reads_it => {}
                Some(_) => return None,
            }
        }
        if any && agreed.is_some() {
            return agreed;
        }
    }
    None
}

/// **Can a call with this many arguments, of these types, reach this parameter list?**
///
/// Arity first — a **parameter pack** accepts any count from its position on — and then each parameter in turn, when the
/// arguments could be typed at all. A call that wrote its template arguments (`std::get<int>(either)`) has no
/// `argument_types` to compare, and its arity is still known: the counts are checked, the conversions are not.
pub(crate) fn a_candidate_that_can_be_called_this_way(
    parameter_list: Option<&str>,
    argument_types: &[Type],
    how_many_arguments: usize,
) -> bool {
    let Some(parameter_list) = parameter_list else {
        // **A declaration with no recorded list has no vote.** It cannot be refused either, so it is skipped rather
        // than kept: `std::make_unique` has a `void`-returning declaration with no parameter list recorded, and keeping
        // it made every set holding such a declaration disagree with the concrete answers around it.
        return false;
    };
    let parameters = crate::sema::overload::parameters_in(parameter_list).0;
    // **A pack is recognised from the written list**, because `parameters_in` strips the `...`: `(_Types&&... _Args)`
    // comes back as one parameter, `_Types&&`, and reading that as an exact arity dropped every variadic candidate
    // — measured, `std::make_unique<Closing>()` answered nothing while two of its three overloads agree on the type.
    let a_pack = parameter_list.contains("...");
    if a_pack {
        let fixed = parameters.len().saturating_sub(1);
        if how_many_arguments < fixed {
            return false;
        }
    } else if parameters.len() != how_many_arguments {
        return false;
    }
    if argument_types.len() != how_many_arguments {
        // The arguments could not be typed (the call wrote its template arguments instead), and the counts are all there
        // is to check.
        return true;
    }
    parameters
        .iter()
        .zip(argument_types)
        .all(|(parameter, argument)| {
            !crate::sema::convert::conversion(argument, parameter, |_| None).is_refused()
        })
}

/// **The type `auto` would give this spelling** — no top-level reference and no `const`, which is what `auto x = f();` keeps.
pub(crate) fn read_the_way_auto_does(written: &str) -> String {
    let mut spelling = written.trim();
    while let Some(rest) = spelling.strip_prefix("const ") {
        spelling = rest.trim();
    }
    while let Some(rest) = spelling.strip_suffix('&') {
        spelling = rest.trim();
    }
    spelling.to_string()
}

/// The types a callee's own `<…>` names, when it wrote one.
///
/// An empty list is what both "no argument list" and "an argument list this could not read" give, and the caller's
/// only use for it is a positional substitution — substituting nothing is what it already does when there is nothing
/// to substitute, so the two need not be told apart.
/// **The element type an iterator-producing member call yields** — `v.begin()`, `m.find(k)`, `m.lower_bound(k)`.
///
/// `None` for every other call, which is the ordinary case and stays cheap: one member lookup and a name test.
pub(crate) fn iterator_element_type(
    memo: &TypeMemo,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    callee: &cpp_parser::CppSyntaxNode,
) -> Option<Known<(Type, PathBuf)>> {
    let member = crate::sema::resolve::member_access_at(root, end_of_the_written_name(callee))?;
    if !matches!(
        member.member.as_str(),
        "begin"
            | "cbegin"
            | "rbegin"
            | "end"
            | "cend"
            | "rend"
            | "find"
            | "lower_bound"
            | "upper_bound"
    ) {
        return None;
    }

    let Known::Yes((object, file)) = type_of_expression_memoed(
        memo,
        &crate::sema::declarations::ShapeTable::new(),
        index,
        declaring,
        scopes,
        root,
        path,
        &member.object,
        0,
    ) else {
        return None;
    };

    let element = range_element_of(&object)?;
    Some(Known::Yes((
        crate::sema::types::parse_type_spelling(&element),
        file,
    )))
}

pub(crate) fn written_template_arguments(callee: &cpp_parser::CppSyntaxNode) -> Vec<Type> {
    let Some(list) = callee
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TemplateArgumentList)
    else {
        return Vec::new();
    };

    crate::sema::types::read_template_arguments(&list)
}

/// **The scope a bare class name was declared in**, as `scope::name`, from the index.
///
/// The companion of the note in [`member_fact`]: a member query arrives with the spelling the *use* wrote, and an
/// unqualified one names a class whose declaration is filed under a scope the use did not spell out. This is the
/// step that recovers it — by bare name, over the files visible from `from`, taking the first declaration that has
/// a scope at all.
///
/// `None` when nothing declares the name, or when the only declaration has no scope (a class at file scope is
/// already spelled correctly, and qualifying it would look for `::Widget`).
pub(crate) fn qualified_from_the_index(
    index: &ProjectIndex,
    from: &Path,
    class: &str,
) -> Option<String> {
    // **A spelling that already writes its namespace is the index's own.** Qualifying it again produced
    // `std::std::unique_ptr` — measured on `classes.cpp`'s `clone->describe()`, whose receiver is already
    // `std::unique_ptr<Base>` — and every member query after it was about a class nothing declares.
    if class.contains("::") {
        return None;
    }
    // **A *type* called that, not the first thing called that.** `files_declaring` is a name lookup, and a class's
    // name is also its constructors': `std::shared_ptr` has a dozen facts named `shared_ptr` whose scope is
    // `std::shared_ptr` itself — qualifying from one of those gives `std::shared_ptr::shared_ptr`, a class nothing
    // declares, and every member query after it answers nothing. Measured: this function is what turned
    // `shared_ptr<Resource>` into a class with **0 members** while `std::shared_ptr<Resource>` answered **39**, and
    // the hover on `locked->identifier()` said "has no member" because of it. The question here is a *class* spelling,
    // so the declaration has to be the class.
    let declared = index.files_declaring(class, from);
    let found = declared
        .into_iter()
        .find(|declared| declared.fact.kind == crate::DeclKind::Type)?;
    let scope = found.fact.scope.as_deref()?;
    (!scope.is_empty()).then(|| format!("{scope}::{class}"))
}

/// The declaration a **callee expression** names — `make` in `make()`, `fac.build` in `fac.build()`.
///
/// The two lookups a call needs, in one place: a member access goes through the member lookup (the object's type,
/// then the member in it), everything else through the name lookup. The callers differ only in what they then ask
/// of the declaration — [`type_of_a_call`] wants what a call of it *has*, a parameter hint wants the parameters
/// it declares — and neither is a reason for a second copy of the resolution.
///
/// `written` is the **call's** spelling rather than the callee's, and it is what the `Unknown` answers carry: a
/// consumer reporting "not known" should name the code the user wrote, not the half of it the lookup started at.
pub(crate) fn declaration_of_a_callee(
    memo: &TypeMemo,
    shapes: &crate::sema::declarations::ShapeTable,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    callee: &cpp_parser::CppSyntaxNode,
    written: &str,
) -> Known<NamedDeclaration> {
    // A member call: the object's type, then the member's declaration.
    if let Some(access) = crate::sema::resolve::member_access_of(callee) {
        let object = match type_of_expression_memoed(
            memo,
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            &access.object,
            0,
        ) {
            Known::Yes((type_of, _)) => type_of,
            Known::Unknown(reason) => return Known::Unknown(reason),
            Known::No => return Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };

        // **The class the arrow reaches, not the object's own class.** `lineIndex->GetTotalLine()` on a
        // `std::shared_ptr<LineIndex>` asks `LineIndex` for `GetTotalLine`; asking the smart pointer itself answered
        // `Unknown(NotDeclaredHere("std::shared_ptr::GetTotalLine"))` — measured on a real project, and the reason a
        // hover on `auto totalLine = lineIndex->GetTotalLine();` said the type "could not be typed". One
        // implementation of the rule: [`crate::completion::the_class_after_the_operator`], which pointer
        // completion has used since the smart pointers landed.
        let class = if access.arrow
            && let Some((reached, _)) = crate::completion::the_class_after_the_operator(
                index, scopes, root, path, &object, true,
            ) {
            reached
        } else {
            let Some(own) = member_access_class(&object) else {
                return Known::Unknown(UnknownReason::UnknownType(Box::from(object.to_string())));
            };
            own
        };
        // The class and the arguments **together**, which is the step this caller used to do half of: it paired
        // the class's parameters with the arguments the *use* wrote, and a use that wrote none (`std::string`)
        // therefore had none — while the same question asked of the same object from `type_of_expression` had
        // `char&`. One question, one reader.
        let (class, arguments) =
            member_access_class_and_arguments(shapes, index, scopes, root, path, &class, &object);
        let bindings = member_bindings(index, scopes, root, path, &class, &arguments);

        if crate::probes::inference() {
            eprintln!(
                "[inference] callee: object {object:?} -> class {class:?}, arguments {arguments:?}, bindings empty {}",
                bindings.is_empty()
            );
        }
        // The arguments as **spellings**, for the pattern rule — see [`direct_member`].
        let written_arguments: Vec<String> = arguments
            .iter()
            .map(|argument| argument.to_string())
            .collect();
        // **The arguments choose among a member's overloads**, the same ranking a free function's call goes through
        // below. `struct S { int get(int); double get(const char*); };` — `s.get("z")` answered `int`, the first
        // declaration, because a member lookup's rule is "a jump takes the first" and a call is not a jump. The
        // candidates come from [`member_candidates`] (one posting query for the name), and the ranking is
        // [`the_one_the_arguments_select`], which is the one `std::format`'s overloads already go through.
        //
        // **The overload count is asked first, and the argument types second.** Typing the arguments is a walk of
        // each of them, and a name with one declaration — most names — has nothing to rank: the query is over as soon
        // as the postings say so.
        let declared = member_candidates(
            shapes,
            index,
            scopes,
            root,
            path,
            &class,
            &access.member,
            Some(&written_arguments),
        );
        // **A `const` object can only call a `const` member**: a candidate it could not call is not a candidate.
        let callable: Vec<(DeclFact, PathBuf)> = declared
            .into_iter()
            .filter(|(fact, _)| {
                !object.is_const() || fact.modifiers.contains(crate::DeclModifiers::CONST)
            })
            .collect();
        if callable.len() > 1
            && let Some(call) = the_call_around(callee)
            && let Some(types) =
                the_types_of_the_arguments(memo, index, declaring, scopes, root, path, &call)
            && let Some(chosen) = the_one_the_arguments_select(&types, &callable, &[])
            && let Some((fact, file)) = callable.get(chosen)
        {
            if crate::probes::inference() {
                eprintln!(
                    "[inference] callee: {} candidate(s), the arguments chose {chosen}",
                    callable.len()
                );
            }
            return Known::Yes(NamedDeclaration::Indexed(
                fact.clone(),
                file.clone(),
                bindings,
            ));
        }
        return match member_fact(
            shapes,
            index,
            scopes,
            root,
            path,
            &class,
            &access.member,
            Some(&written_arguments),
        ) {
            Known::Yes((fact, file)) => Known::Yes(NamedDeclaration::Indexed(fact, file, bindings)),
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // Everything else is a name — `make()`, `Widget()`, `ns::make()` — and the name lookup is the one every other
    // **Several declarations of this name in this file, and a call that selects one of them.** `definition_at` answers
    // the *first* declaration of a name in one scope — the rule for a name, documented beside it — and for a call that
    // is wrong: `std::format("x {}", 1)` and `std::format(L"x {}", 1)` reach different overloads of one name, and only
    // the arguments say which. Both halves (this one and the index's, below) go through the same ranking.
    if let Some(call) = the_call_around(callee)
        && let Known::Yes(bindings) =
            crate::sema::resolve::definitions_at(scopes, root, end_of_the_written_name(callee))
        && bindings.len() > 1
        && let Some(types) =
            the_types_of_the_arguments(memo, index, declaring, scopes, root, path, &call)
    {
        let candidates: Vec<(DeclFact, PathBuf)> = bindings
            .iter()
            .map(|binding| {
                (
                    fact_from_binding_with(
                        crate::sema::declarations::shapes_from(shapes, root),
                        root,
                        "",
                        binding,
                    ),
                    path.to_path_buf(),
                )
            })
            .collect();
        let explicit_arguments: Vec<String> = call
            .children()
            .next()
            .map(|callee| written_template_arguments(&callee))
            .unwrap_or_default()
            .iter()
            .map(Type::to_string)
            .collect();
        if let Some(chosen) = the_one_the_arguments_select(&types, &candidates, &explicit_arguments)
            && let Some(binding) = bindings.get(chosen)
        {
            return Known::Yes(NamedDeclaration::Here(binding.clone()));
        }
    }

    // **C++'s unqualified member lookup, asked where the language puts it.**
    //
    // Inside a member function a name is looked up in the class and its bases **before** the enclosing namespaces —
    // `shared_from_this()` in a class deriving `std::enable_shared_from_this<Node>` is
    // `std::enable_shared_from_this<Node>::shared_from_this`, which no scope in the file declares. It has to come before
    // the index-wide name query, because that query resolves the base's declaration in its own header and answers the
    // **written** fact (`T *`), while the member list substitutes the class's arguments through it (`Node *`).
    //
    // Two conditions keep this from taking an answer that was already better: the file's own scopes must answer
    // **nothing** (a local variable, or a member declared here, keeps the scope tree's answer), and the class's member
    // must be finished (a member of a template base whose arguments the member list could not read is refused).
    if matches!(
        crate::sema::resolve::definition_at(scopes, root, end_of_the_written_name(callee)),
        Known::No | Known::Unknown(UnknownReason::NotDeclaredHere(_))
    ) && let Some(member) =
        a_member_of_the_enclosing_class(index, scopes, root, path, callee, true)
    {
        return Known::Yes(NamedDeclaration::Indexed(
            member.fact,
            member.file,
            crate::sema::types::TypeBindings::new(Vec::new(), Vec::new()),
        ));
    }

    // **A callee written as a template-id names the function before its `<`.** `make_sized<3>()` is a call of
    // `make_sized`, and the written arguments are read by `type_of_a_call`, which passes them to `what_a_call_has` for
    // substitution — but the name query was asked about `make_sized<3>`, which is no declaration's name, so the call had
    // no type at all (measured: `make_sized<3>().size()` reported the type as unknown while `std::array<int, 3>`'s members
    // were listable). The retry is one index query, and only when the ordinary lookup found nothing.
    let known = declaration_of_expression(index, scopes, root, path, callee);
    let known = match &known {
        Known::No
        | Known::Unknown(UnknownReason::NotDeclaredHere(_))
        | Known::Unknown(UnknownReason::Ambiguous(_)) => {
            match the_name_before_its_template_arguments(callee) {
                // **The scope tree first, by offset.** It is the reader that resolves a namespaced name —
                // `test_cpp::make_sized` is not reachable by `index.definition("make_sized")` — and it places the name
                // whose template-id the file wrote.
                Some(bare) => {
                    let start = usize::from(callee.text_range().start());
                    match crate::sema::resolve::definition_at(scopes, root, start + bare.len()) {
                        Known::Yes(binding) => Known::Yes(NamedDeclaration::Here(binding)),
                        _ => match index.definition(&bare, path) {
                            Known::Yes(found) => Known::Yes(NamedDeclaration::Indexed(
                                found.fact,
                                found.file,
                                crate::sema::types::TypeBindings::new(Vec::new(), Vec::new()),
                            )),
                            Known::Unknown(reason) => Known::Unknown(reason),
                            Known::No => Known::No,
                        },
                    }
                }
                // **Nothing was retried, so nothing is replaced.** An `Ambiguous` name stays ambiguous: the call's own
                // arguments are what picks one, and that question is asked below.
                None => known,
            }
        }
        _ => known,
    };

    // **A name with several declarations, and a call that selects one of them.** `std::format("…", value)` is four
    // overloads — narrow and wide, with and without a locale — and the name lookup is right to refuse to pick. What
    // was missing is that the *call* is the tie-breaker: the arguments' types are exactly what a compiler uses, and
    // the ranking for them already exists (it is what the signature popup and the hover's `→` marker use). Without
    // this, `auto enenn = std::format(…)` had no initializer type and therefore no type at all.
    let Known::Unknown(UnknownReason::Ambiguous(name)) = &known else {
        return known;
    };
    // **The call this callee belongs to, however many expression nodes sit between them** — `std::format` parses as
    // a name inside the call, and a member callee as a member access inside it, so the search is up the parents
    // rather than one `parent()`.
    let Some(call) = the_call_around(callee) else {
        return known;
    };
    match the_declaration_the_arguments_pick(
        memo,
        index,
        declaring,
        scopes,
        root,
        path,
        &call,
        name.as_ref(),
    ) {
        Some((fact, file)) => Known::Yes(NamedDeclaration::Indexed(
            fact,
            file,
            TypeBindings::default(),
        )),
        None => known,
    }
}

/// Where a call's callee is declared — what a parameter hint is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Callee {
    /// The file the declaration is written in.
    pub file: PathBuf,
    /// The offset of the declared **name** in that file.
    pub name_offset: usize,
    /// **The declaration's parameter list as the index already holds it**, parentheses included — the fallback a
    /// signature popup uses when that file has no *rendering* to read the list out of.
    ///
    /// The two are not the same source: `name_offset` is an offset into the file's **cooked rendering**, so a caller
    /// that has only the file's own text cannot read a parameter list at it — and a query may not build a rendering
    /// (65–190 ms for a standard-library header). Measured on a real C++23 module project: `math::add(` answered
    /// **no signatures at all** on the first request and only after a second `(` had queued and built the interface
    /// unit's rendering — which is what "the signature popup almost never appears" looks like from the outside.
    ///
    /// `None` for a declaration whose fact was built before the field existed — the popup then says nothing, which
    /// is the old behaviour rather than a wrong label.
    pub parameters: Option<String>,
}

/// **Every declaration a call's callee names** — one for an ordinary function, all of them for an overload set.
///
/// [`callee_of_a_call`] is the singular question ("where is *the* function this calls") and it is the right one for
/// a parameter *hint*, which labels one declaration's parameters. It is the wrong one for signature help: a call to
/// an overloaded function has several answers, the protocol's `SignatureHelp` carries a **list**, and declining with
/// `Ambiguous` — which is what this analysis did — leaves a reader typing `std::format(` with nothing at all.
/// Measured on the project this was written against: `std::format` is four overloads, so the popup was empty.
///
/// # What is plural, and what is not
///
/// A **name** goes through the plural lookup ([`ProjectIndex::definitions`]), which is the query that exists for
/// exactly this: it answers with every declaration of the name the cursor can see, in file order, each carrying the
/// fact — including the parameter list, which is what a signature is rendered from.
///
/// A **member call** (`s.push_back(`) goes through [`members_of`], the class's whole member list, filtered by the
/// name written — **bases included**, which is what a member call needs and what `direct_member` alone does not
/// give: `std::string::push_back` is two overloads, and a class that inherits an overloaded member inherits all of
/// it. The object's type is worked out exactly as the singular path works it out, because that is the question the
/// two share.
pub(crate) fn callees_of_a_call(
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    call: &cpp_parser::CppSyntaxNode,
) -> Known<Vec<Callee>> {
    let memo = TypeMemo::default();
    let shapes = crate::sema::declarations::ShapeTable::new();
    let written = call.text().to_string();
    let written = written.trim();

    let Some(callee) = call.children().next() else {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    };

    if let Some(access) = crate::sema::resolve::member_access_of(&callee) {
        let members = member_callees(&memo, index, declaring, scopes, root, path, &access);

        // A member whose set could not be listed — the object's type is unknown, the class is not declared
        // anywhere the index can see — falls back to the singular path, which reports *why* rather than an empty
        // list. An empty list and an unanswerable question are different answers.
        return match members {
            Known::Yes(found) if !found.is_empty() => Known::Yes(found),
            other => match other {
                Known::Unknown(reason) => Known::Unknown(reason),
                _ => match callee_of_a_call(&shapes, index, declaring, scopes, root, path, call) {
                    Known::Yes(one) => Known::Yes(vec![one]),
                    Known::Unknown(reason) => Known::Unknown(reason),
                    Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
                },
            },
        };
    }

    // Everything else is a name — `make`, `ns::make`, `std::format`. The qualified spelling a name resolves to is
    // what the index is asked about, and it is the answer's *failure* shapes that carry it: a name this file cannot
    // place is either absent from the index or declared there more than once, and both are questions for the plural
    // lookup.
    let offset = end_of_the_written_name(&callee);

    match crate::sema::resolve::definitions_at(scopes, root, offset) {
        // Declared in this buffer: every overload of it, and the file being edited is where they are. **All** of
        // them, which is the whole point of this function: `void scale(int); double scale(double, int);` in one
        // file is one name with two declarations in one scope, and a signature list that showed one of them would
        // be the old behaviour with extra steps.
        Known::Yes(bindings) if !bindings.is_empty() => Known::Yes(
            bindings
                .iter()
                .map(|binding| Callee {
                    file: path.to_path_buf(),
                    name_offset: binding.name_range.start_offset,
                    // The buffer is parsed in front of the caller, so its own list is read from the tree — the
                    // fallback exists for a declaration in a file nobody has cooked.
                    parameters: None,
                })
                .collect(),
        ),
        Known::Yes(_) => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        Known::Unknown(UnknownReason::NotDeclaredHere(name))
        | Known::Unknown(UnknownReason::Ambiguous(name)) => match index.definitions(&name, path) {
            Known::Yes(found) => Known::Yes(
                found
                    .found
                    .iter()
                    .map(|declaration| Callee {
                        file: declaration.file.clone(),
                        name_offset: declaration.fact.name_range.start_offset,
                        parameters: declaration.fact.parameter_list().map(str::to_string),
                    })
                    .collect(),
            ),
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        },
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
    }
}

/// **The `CallExpr` a callee belongs to**, looking up the parents a few levels — see its first caller.
pub(crate) fn the_call_around(
    callee: &cpp_parser::CppSyntaxNode,
) -> Option<cpp_parser::CppSyntaxNode> {
    let mut node = callee.parent();
    for _ in 0..4 {
        let inner = node?;
        if cpp_parser::CppSyntaxKind::from(inner.kind()) == cpp_parser::CppSyntaxKind::CallExpr {
            return Some(inner);
        }
        node = inner.parent();
    }
    None
}
/// **Which declaration a call's own arguments select**, when the callee's name has more than one.
///
/// The engine used to stop at "several declarations" for a free function, and that is where `auto enenn =
/// std::format(FULL_NAME_FORMAT, myName.getFullName(0));` lost its type: `std::format` has four overloads visible from
/// that file (narrow and wide, with and without a locale) and nothing chose between them, so `auto` had no initializer
/// type to deduce from. The ranking machinery already existed — it is what the signature popup and the hover's `→`
/// marker use ([`crate::sema::overload::choose`]) — and what was missing was asking it here.
///
/// This is the **index** half: the declarations are in other files and the name is qualified. The same-file half is in
/// [`declaration_of_a_callee`], which has bindings rather than facts, and both go through
/// [`the_one_the_arguments_select`] so that the two decide alike.
pub(crate) fn the_declaration_the_arguments_pick(
    memo: &TypeMemo,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    call: &cpp_parser::CppSyntaxNode,
    name: &str,
) -> Option<(DeclFact, PathBuf)> {
    // **The spelling a call writes is not the spelling the index stores.** `std::make_unique<Resource>(1)` carries its
    // template arguments in the name the ambiguity reports, and `definitions("std::make_unique<Resource>")` finds
    // nothing — measured — so the selection never ran at all. The arguments are the call's, not the declaration's name.
    let name = the_name_without_its_arguments(name);
    let Known::Yes(definitions) = index.definitions(name, path) else {
        return None;
    };
    if definitions.found.len() < 2 {
        return None;
    }
    let candidates: Vec<(DeclFact, PathBuf)> = definitions
        .found
        .iter()
        .map(|definition| (definition.fact.clone(), definition.file.clone()))
        .collect();
    let types = the_types_of_the_arguments(memo, index, declaring, scopes, root, path, call)?;
    let explicit_arguments: Vec<String> = call
        .children()
        .next()
        .map(|callee| written_template_arguments(&callee))
        .unwrap_or_default()
        .iter()
        .map(Type::to_string)
        .collect();
    let chosen = the_one_the_arguments_select(&types, &candidates, &explicit_arguments)?;
    candidates.get(chosen).cloned()
}

/// **A call's callee without the template arguments the call wrote** — the spelling the index keys a name on.
///
/// `std::make_unique<Resource>` → `std::make_unique`, and `A<B<C>>::f` stays whole: the arguments are stripped only
/// when the first `<`'s matching `>` is the **last** character, which is exactly the shape a call's argument list
/// leaves. Anything else is part of the name (`A<B<C>>::f` has a `::` after its arguments), and cutting there would
/// name a different entity.
pub(crate) fn the_name_without_its_arguments(written: &str) -> &str {
    let Some(open) = written.find('<') else {
        return written;
    };
    let mut depth = 0i32;
    for (at, character) in written[open..].char_indices() {
        match character {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    let close = open + at;
                    return if close + character.len_utf8() == written.len() {
                        &written[..open]
                    } else {
                        written
                    };
                }
            }
            _ => {}
        }
    }
    written
}
/// **The types of a call's arguments**, as spellings — `Unknown` for an argument whose type the engine cannot read.
pub(crate) fn the_types_of_the_arguments(
    memo: &TypeMemo,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    call: &cpp_parser::CppSyntaxNode,
) -> Option<Vec<Known<String>>> {
    let arguments = crate::sema::call_syntax::arguments_of(call)?;
    Some(
        arguments
            .iter()
            .map(|argument| {
                match type_of_expression_memoed(
                    memo,
                    &crate::sema::declarations::ShapeTable::new(),
                    index,
                    declaring,
                    scopes,
                    root,
                    path,
                    argument,
                    0,
                ) {
                    Known::Yes((type_of, _)) => Known::Yes(type_of.to_string()),
                    Known::No => Known::No,
                    Known::Unknown(reason) => Known::Unknown(reason),
                }
            })
            .collect(),
    )
}

/// **Which of these declarations the arguments select**, as an index into `candidates`.
///
/// # The tie the ranker leaves, and the one it is allowed to break
///
/// `std::format`'s narrow and wide overloads are spelled `format(const format_string<_Types...> _Fmt, _Types&&...
/// _Args)` and the same with `wformat_string`; against a call's arguments the ranker answers `Ambiguous`, because the
/// character width of the format string is a property of the *class* those parameters have, not of their spelling. The
/// tie-break here is the one a reader makes: **the argument's character width selects the family** — a narrow string
/// cannot construct a `wformat_string` and a wide one cannot construct a `format_string` — so when exactly one
/// candidate's *first* parameter is written for the width the call writes, that candidate is the answer. If several
/// are (or none is definite), the tie stands and the caller keeps its `Ambiguous`: a tie between two candidates of the
/// same width is a real one.
pub(crate) fn the_one_the_arguments_select(
    types: &[Known<String>],
    candidates: &[(DeclFact, PathBuf)],
    explicit_arguments: &[String],
) -> Option<usize> {
    // **A deleted declaration cannot be the meaning of a call this layer reports.** `std::make_unique` has a third
    // overload `= delete`d so that arrays are rejected ([memory.syn]); it ties with the real one on every argument, and
    // keeping it in the running is what made `auto unique = std::make_unique<Resource>(1);` ambiguous and left the
    // variable with no type. `usable` holds the original indices, in the order the ranking sees them.
    let usable: Vec<usize> = (0..candidates.len())
        .filter(|at| {
            !candidates[*at]
                .0
                .modifiers
                .contains(crate::DeclModifiers::DELETED)
        })
        .collect();
    if usable.is_empty() {
        // Every candidate is deleted: the call is ill-formed whichever one it means, and "no answer" is the honest
        // report of that.
        return None;
    }
    let built: Vec<crate::sema::overload::Candidate> = usable
        .iter()
        .enumerate()
        .map(|(at, &index)| {
            let (fact, _) = &candidates[index];
            let (declared, variadic) = fact
                .parameter_list()
                .map(crate::sema::overload::parameters_in)
                .unwrap_or_default();
            let names = fact.parameters().to_vec();
            let declared_for_ordering = declared.clone();
            // **The deduction starts from what the call wrote.** `std::make_unique<Resource>(1)` states `_Tp` as
            // `Resource`; without that seed the candidate's every parameter stayed unbound and it ranked `Unranked`.
            let substituted = match crate::sema::deduction::deduce_with_written(
                &names,
                &declared,
                types,
                explicit_arguments,
            ) {
                Some(deduced) => (
                    declared
                        .iter()
                        .map(|spelling| {
                            crate::sema::types::substituted_spelling(spelling, &names, &deduced)
                        })
                        .collect::<Vec<String>>(),
                    true,
                ),
                None => (declared, !names.is_empty()),
            };
            let candidate = crate::sema::overload::Candidate::new(
                substituted.0,
                variadic,
                format!("declaration {at}"),
            );
            if substituted.1 {
                candidate.with_declaration(declared_for_ordering, names.to_vec())
            } else {
                candidate
            }
        })
        .collect();

    if crate::probes::inference() {
        eprintln!(
            "[inference] pick among {} candidates ({} usable), argument types {types:?} written {explicit_arguments:?}",
            candidates.len(),
            usable.len()
        );
        for (at, (fact, _)) in candidates.iter().enumerate() {
            eprintln!(
                "[inference]   candidate {at}: bits={} deleted={} params={:?}",
                fact.modifiers.bits(),
                fact.modifiers.contains(crate::DeclModifiers::DELETED),
                fact.parameter_list()
            );
        }
        for (at, (fact, _)) in candidates.iter().enumerate() {
            eprintln!(
                "[inference]   candidate {at}: scope {:?} returns {:?} params {:?}",
                fact.scope,
                fact.returns,
                fact.parameter_list()
            );
        }
    }
    let choice = crate::sema::overload::choose(types, &built);
    if crate::probes::inference() {
        eprintln!("[inference]   choice: {choice:?}");
    }
    match choice {
        // The rank is a position in `built`, which is `usable`'s order — the original index is what the caller wants.
        crate::sema::overload::Choice::One(at) => usable.get(at).copied(),
        crate::sema::overload::Choice::Ambiguous => {
            let written = types.iter().find_map(|argument| match argument {
                Known::Yes(spelling) => the_character_width(spelling),
                _ => None,
            })?;
            let definite: Vec<usize> = usable
                .iter()
                .filter(|at| {
                    let (fact, _) = &candidates[**at];
                    fact.parameter_list()
                        .and_then(|list| {
                            crate::sema::overload::parameters_in(list)
                                .0
                                .into_iter()
                                .next()
                        })
                        .and_then(|first| the_character_width(&first))
                        == Some(written)
                })
                .copied()
                .collect();
            match definite[..] {
                [only] => Some(only),
                _ => None,
            }
        }
        crate::sema::overload::Choice::None => None,
    }
}
/// **Is this spelling written for wide characters, narrow ones, or neither?** — the tie-break
/// [`the_declaration_the_arguments_pick`] uses, and nothing else does.
///
/// `wchar_t` contains `char` and `wstring` contains `string`, so the wide spellings are asked first. `None` for a
/// spelling that says neither (`int`, `pair<A, B>`), which keeps such a candidate in the running.
pub(crate) fn the_character_width(spelling: &str) -> Option<bool> {
    if spelling.contains("wchar_t")
        || spelling.contains("wstring")
        || spelling.contains("wformat")
        || spelling.contains("wint_t")
    {
        return Some(true);
    }
    if spelling.contains("char") || spelling.contains("string") {
        return Some(false);
    }
    None
}
/// **Every declaration of one member name in the object's class** — the member-call half of [`callees_of_a_call`].
///
/// The class comes from the object's **type**, which is the same step [`declaration_of_a_callee`] makes: the two
/// questions differ only in what they then ask for (one member, or all of them), and a second way of working out
/// what `s` is would be a second answer to it.
///
/// `None`-typed objects, unknown types and classes nothing declares are reported as they are —
/// [`UnknownReason`] — rather than as an empty list, for the reason the plural name lookup gives: "there are no
/// members" and "this analysis cannot say" are different answers and a caller shows them differently.
pub(crate) fn member_callees(
    memo: &TypeMemo,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    access: &crate::sema::resolve::MemberAccess,
) -> Known<Vec<Callee>> {
    let object = match type_of_expression_memoed(
        memo,
        &crate::sema::declarations::ShapeTable::new(),
        index,
        declaring,
        scopes,
        root,
        path,
        &access.object,
        0,
    ) {
        Known::Yes((type_of, _)) => type_of,
        Known::Unknown(reason) => return Known::Unknown(reason),
        Known::No => {
            return Known::Unknown(UnknownReason::UnknownType(Box::from(
                access.object.text().to_string(),
            )));
        }
    };

    let Some(class) = member_access_class(&object) else {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(object.to_string())));
    };

    // **The member's name is looked up in every class it could come from**, which is what `members_of` answers and
    // what an overloaded member needs: `push_back` is declared twice in `basic_string` itself, and a base's
    // overloads are the derived class's overloads unless something hides them.
    let found = match members_of(index, scopes, root, path, &class) {
        Known::Yes(found) => found,
        Known::Unknown(reason) => return Known::Unknown(reason),
        Known::No => return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class))),
    };

    let wanted = &*access.member;
    let candidates: Vec<Callee> = found
        .members
        .iter()
        .filter(|member| member.fact.name == wanted)
        .map(|member| Callee {
            file: member.file.clone(),
            name_offset: member.fact.name_range.start_offset,
            parameters: member.fact.parameter_list().map(str::to_string),
        })
        .collect();

    Known::Yes(candidates)
}

/// The declaration a call names, as **a place in a file**: the file it is in, and the offset of its name.
///
/// [`type_of_a_call`]'s question asked for a different answer: "what does a call of this have" needs the
/// declaration's return type, while a parameter hint needs the parameters it declares — and both need the
/// declaration found the same way.
///
/// Nothing is refused here for not being a function. `Widget(1, 2)` and `fp(1)` are calls whose declaration is a
/// class or a variable, and whether either has a parameter list is a question about the *declaration's own
/// declarator*, which the caller asks: a class name has no declarator at all, and a function pointer's parameters
/// belong to its type rather than to the name. Answering "here is the declaration" and letting that reading say
/// "no parameters" keeps one rule instead of two.
///
/// # Why the *name's* offset rather than the declaration's first token
///
/// A caller looking for the parameters finds them in the declarator the name is a name *of*, so it has to start at
/// the name: a `const`, a `static` or a return type is outside the declarator, and a declaration may hold more
/// than one declarator (`void (*f(int a))(int b)`).
pub(crate) fn callee_of_a_call(
    shapes: &crate::sema::declarations::ShapeTable,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    call: &cpp_parser::CppSyntaxNode,
) -> Known<Callee> {
    let memo = TypeMemo::default();
    let written = call.text().to_string();
    let written = written.trim();

    let Some(callee) = call.children().next() else {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    };

    match declaration_of_a_callee(
        &memo, shapes, index, declaring, scopes, root, path, &callee, written,
    ) {
        Known::Yes(named) => Known::Yes(Callee {
            file: named.file(path),
            name_offset: named.name_offset(),
            parameters: named.parameter_list(),
        }),
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
    }
}

/// The declaration of `member` in the class `class` names, and the file it is in.
///
/// The lookup is the qualified name `<class>::<member>`, which is why nothing here needs to know what a class
/// *is*: the file being edited is asked first through its scope tree, and the index second — the same two-layer
/// split [`definition_across_files`] makes, and for the same reason.
/// **The member a class's `operator->` reaches**, when the class declares one — see the call site in
/// [`member_fact_inner`].
///
/// One level, like the pointer rule: an object whose `operator->` returns another handle is a shape this layer does
/// not follow twice, and a second level would be a guess about which `operator->` the language picked.
/// **A member's written answer, read as a type in the class the member belongs to.**
///
/// `std::vector<T>::operator[]` is declared `reference operator[](size_type)`, and `reference` is `T&` — an alias
/// *inside the class*. Two things have to happen before that is a type a member lookup can ask about:
///
/// 1. the class's parameters are substituted with the arguments the use wrote (`reference` → `_Ty&` → `MarkEvent&`);
/// 2. the alias is read **in this class**, not by its bare name in the index.
///
/// The second is what a real project needs and a fixture does not show: `files_declaring("reference", …)` answers with
/// every class's `reference` — `basic_string`'s, `vector`'s, `_Vector_val`'s — and the first visible one is usually not
/// the one whose member was asked. Measured on EmmyLuaCodeStyle's `Mark.cpp`, where `events[Pos].U` on a
/// `std::vector<MarkEvent>` answered `Unknown(NotDeclaredHere("_Ty"))`: the alias was followed to a *different* class's
/// `reference`, whose `_Ty` nothing substitutes.
pub(crate) fn a_members_answer_as_a_type(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class_type: &Type,
    returns: &str,
) -> String {
    // **An alias has no parameters of its own; its target has them.** `std::string` is
    // `using string = basic_string<char, char_traits<char>, allocator<char>>`, so `template_parameters_for("std::string")`
    // answers `[]` — and the members whose spellings this resolves are `basic_string`'s (`reference` is `value_type&`,
    // `value_type` is `_Elem`). Measured on the server: `s[0]` on a `std::string&` answered **`_Elem&`**, and the trace
    // showed the two alias steps with `(class "std::string", parameters [], arguments [])`. One step to the target is
    // enough — the recursion ends at a class, whose declaration carries no `type_of` — and it is the same step
    // [`operator_returns_of_with_arguments`] takes for an alias's operators.
    let mut class_type = class_type.clone();
    for _ in 0..4 {
        let Some(name) = class_type.class_name().map(str::to_string) else {
            return returns.to_string();
        };
        let Some(target) = index
            .files_declaring(&name, path)
            .into_iter()
            .find(|declared| {
                declared.fact.kind == crate::DeclKind::Type && declared.fact.type_of.is_some()
            })
            .and_then(|declared| declared.fact.type_of.clone())
        else {
            break;
        };
        let resolved = crate::sema::types::parse_type_spelling(&target);
        let Some(resolved_name) = resolved.class_name() else {
            break;
        };
        // **The target is written unqualified**, and the class it names is looked up by the index's own spelling:
        // `basic_string` in `<string>` is `std::basic_string`, and asking a member question about the bare name found
        // no class at all — the substitution then answered `reference`, the alias's *name*, which is no type either.
        let qualified = crate::index::project::qualified_from_the_index(index, path, resolved_name)
            .unwrap_or_else(|| resolved_name.to_string());
        if qualified == name {
            break;
        }
        let arguments: Vec<String> = resolved
            .arguments()
            .iter()
            .map(|argument| argument.to_string())
            .collect();
        // **The qualifier is written on the use, so it is put back on the target** — see the same step in
        // [`operator_returns_of_with_arguments`]: a `const std::string&` resolved to `basic_string<char, …>`
        // unqualified and the member choice then treated the object as non-const.
        let resolved = crate::sema::types::parse_type_spelling(&match arguments.is_empty() {
            true => qualified,
            false => format!("{qualified}<{}>", arguments.join(", ")),
        });
        class_type = match class_type.is_const() && !resolved.is_const() {
            true => crate::sema::types::Type::Qualified {
                of: std::sync::Arc::new(resolved),
                qualifiers: crate::sema::types::Qualifiers::CONST,
            },
            false => resolved,
        };
    }
    let Some(class) = class_type.class_name() else {
        return returns.to_string();
    };
    let arguments: Vec<String> = class_type
        .arguments()
        .iter()
        .map(|argument| argument.to_string())
        .collect();
    let parameters = index.template_parameters_for(class, path, &arguments);
    // **A use that writes fewer arguments than the declaration has parameters**, which is the ordinary case:
    // `std::vector<int>` names one of two. The defaults are what the rest stand for — see
    // [`crate::substituted_spelling_with_defaults`], and `lineStartOffset` for the chain that needed it.
    let defaults = index.template_parameter_defaults_for(class, path, &arguments);
    let substituted = crate::sema::types::substituted_spelling_with_defaults(
        returns,
        &parameters,
        &defaults,
        &arguments,
    );

    // **The operators and qualifiers the spelling around the name carries**: `const reference&` is `const T&`, and
    // replacing only the name must keep both ends.
    let (leading, core, trailing) = split_the_operators_of(&substituted);
    if core.is_empty()
        || !core
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
    {
        return substituted;
    }

    // **The class's own declaration of that name**, which is the only one whose parameters are the ones in hand.
    let Known::Yes((fact, _)) = member_fact(
        &crate::sema::declarations::ShapeTable::new(),
        index,
        scopes,
        root,
        path,
        &the_class_spelling(&class_type),
        &core,
        None,
    ) else {
        return substituted;
    };
    let Some(target) = fact.type_of.as_deref() else {
        return substituted;
    };
    let target = crate::sema::types::substituted_spelling_with_defaults(
        target,
        &parameters,
        &defaults,
        &arguments,
    );
    // **And the target is a *name* too**, so it is followed before it is answered: `pointer` is `_Val*` in the class
    // that declares it, and `_Alloc_traits::reference` is whatever that alias names. Answering the spelling instead
    // is what made `map::find(text)->second` look `second` up in `_Rb_tree_iterator` rather than in its `pair`.
    let target = a_spelling_through_the_aliases(
        index,
        scopes,
        root,
        path,
        &class_type,
        &[],
        &target,
        MAX_ALIAS_HOPS,
    );
    // **The substitution runs again, because the aliases put parameter names back.** `std::string`'s `operator[]`
    // returns `reference`, which is `value_type&`, and `value_type` is `_Elem` — so the first substitution cannot
    // reach it (no parameter is written in `reference`) and the alias walk hands back `_Elem&`: a spelling no reader
    // can use. Measured through the server, `s[0]` on a `std::string&` answered **`_Elem&`** where a compiler answers
    // `char&`; the second pass is one call and turns it into `char&`.
    let target = crate::sema::types::substituted_spelling_with_defaults(
        &target,
        &parameters,
        &defaults,
        &arguments,
    );
    format!("{leading}{target}{trailing}")
}
