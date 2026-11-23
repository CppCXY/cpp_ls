//! **Deductions the language writes without a type** — `auto`, structured bindings, ranges, and initialisers.
//!
//! Moved here from `project/mod.rs` without a change. These are the places where the answer is not written in the
//! program but has to be derived: a range-for's element, `auto x = call()`, a `std::pair`'s halves, the element type of
//! a container, and the type an initialiser gives a declaration that named none.

use super::*;

/// **The element type of the range a `for` head binds** — `for (auto x : v)` answers `int` for a `vector<int>`.
///
/// `None` when the declaration is not a range-based `for`'s, which is the ordinary case. The head is the one shape
/// whose declared variable has **no initializer**: `auto x = v[0]` has one and goes through [`initializer_type`],
/// while `for (auto x : v)` states the range in the loop's own syntax.
pub(crate) fn range_for_element_type(
    memo: &TypeMemo,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    named: &NamedDeclaration,
    depth: usize,
) -> Option<Known<Type>> {
    let NamedDeclaration::Here(binding) = named else {
        return None;
    };

    let declared_at = binding.name_range.start_offset;
    let loop_node = cpp_parser::token_at(root, declared_at)?
        .parent_ancestors()
        .find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::RangeForStat
        })?;

    // **The loop's own variable, and not a declaration that merely sits inside the body.**
    //
    // Reaching a `RangeForStat` by ancestry is not the question: every declaration in the body has one, and this
    // function answers *before* the ordinary deduction is tried, so a body declaration was answered as if it were the
    // loop variable. Measured on `EmmyLuaCodeStyle`'s `LuaCodeFormat.cpp:304` — `auto &result = results.emplace_back();`
    // is the first statement of `for (auto &diagnostic: diagnostics)` — the reported answer was
    // `Unknown(UnknownType("std::vector<LuaDiagnostic>&"))`, the **range's** type, and the initializer was never looked
    // at. The head is everything before the `:`; a declaration after it is a different declaration with an initializer
    // of its own, which is what `deduction_inputs` answers.
    if !is_in_the_head_of(&loop_node, declared_at) {
        return None;
    }

    let range = range_expression_of_the_pattern(&loop_node)?;

    // **A range built out of adaptors has its element computed from the expression**, because the view it makes has no
    // name this layer can spell: `numbers | std::views::filter(f) | std::views::take(2)` yields `int` even though no
    // declaration says so. See [`element_of_a_range_expression`].
    if let Some(element) = element_of_a_range_expression(
        memo,
        index,
        declaring,
        scopes,
        root,
        path,
        &range,
        depth + 1,
    ) {
        return Some(element.map(|(type_of, _)| type_of));
    }

    let Known::Yes((container, _)) = type_of_expression_memoed(
        memo,
        &crate::sema::declarations::ShapeTable::new(),
        index,
        declaring,
        scopes,
        root,
        path,
        &range,
        depth + 1,
    ) else {
        return Some(Known::Unknown(UnknownReason::UnknownType(Box::from(
            "auto",
        ))));
    };

    Some(match range_element_of(as_a_range(&container)) {
        Some(element) => Known::Yes(crate::sema::types::parse_type_spelling(&element)),
        None => Known::Unknown(UnknownReason::UnknownType(Box::from(container.to_string()))),
    })
}

/// **The element a range expression yields, when the expression *builds* the range.**
///
/// The named-container case above is one half; this is the other, and it is what C++20 ranges are written with:
///
/// ```text
/// numbers | std::views::filter(f) | std::views::take(2)   the element of `numbers`
/// map | std::views::keys                                  the key of the map's pair
/// map | std::views::values                                its value
/// rows | std::views::join                                 the element of the inner range
/// std::views::iota(1, 6)                                  the type of `1`
/// std::ranges::subrange(begin, end)                       the element of the range `begin` came from
/// ```
///
/// A view has no name this layer can spell — its type is a class template over a closure type — so the question is asked of
/// the **expression** and never of a `Type`: the left operand says the element, and each adaptor is a function on it. An
/// adaptor that changes the element through a body (`transform`'s lambda) is refused: reading a `return` there needs the
/// same deduction a function body does, and a guess would offer the wrong members in the loop.
///
/// `None` when the expression is not one of these shapes — the caller falls back to typing it as a value.
pub(crate) fn element_of_a_range_expression(
    memo: &TypeMemo,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    expression: &cpp_parser::CppSyntaxNode,
    depth: usize,
) -> Option<Known<(Type, PathBuf)>> {
    use cpp_parser::CppAstNode;

    // **A pipe chain**: the element of the left, adapted by the right.
    if let Some((left, right)) = the_two_operands_of_a_pipe(expression)
        && let Some(left_element) = element_of_a_range_expression(
            memo,
            index,
            declaring,
            scopes,
            root,
            path,
            &left,
            depth + 1,
        )
    {
        return Some(adapted_element(
            &left_element,
            &right,
            index,
            scopes,
            root,
            path,
        ));
    }

    // **The base case: a range expression that is a value.** `numbers`, `collection`, a function call returning a
    // container — the element is the one its type yields, which is the path named containers already took. It has to be
    // here and not only in the caller, because the pipe case recurses to a value.
    if CppSyntaxKind::from(expression.kind()) != CppSyntaxKind::BinaryExpr
        && a_call_to_a_function(expression).is_none()
        && let Known::Yes((container, file)) = type_of_expression_memoed(
            memo,
            &crate::sema::declarations::ShapeTable::new(),
            index,
            declaring,
            scopes,
            root,
            path,
            expression,
            depth + 1,
        )
        && let Some(element) = range_element_of(as_a_range(&container))
    {
        return Some(Known::Yes((
            crate::sema::types::parse_type_spelling(&element),
            file,
        )));
    }

    // **A pipeline a reader gave a name.** `auto ids = items | std::views::filter(pred);` binds the view, and a
    // view's type is a class template over a closure type nothing spells — so every later use of `ids` asked its
    // *type* and found no element, while the same pipe written in a `for` head has always answered one. The
    // initializer is the expression the in-place case reads, so a name whose own type yields no element is looked
    // through once: the same walk on the initializer, with the depth bound shared so a reference cycle stops.
    //
    // **A name only**: `end_of_the_written_name` is defined for the four name shapes, and asking it of a call or a
    // binary expression would place its cursor at the next token — whose declaration is not this expression's.
    if depth <= MAX_TYPE_DEPTH
        && cpp_parser::CppNameExpr::cast(expression.clone()).is_some()
        && let Some(initializer) = the_initializer_a_name_binds(expression, scopes, root)
        && let Some(element) = element_of_a_range_expression(
            memo,
            index,
            declaring,
            scopes,
            root,
            path,
            &initializer,
            depth + 1,
        )
    {
        return Some(element);
    }

    // **`std::views::iota(1, 6)`** — a counted range of the first argument's type.
    if let Some(callee) = a_call_to_a_function(expression) {
        let written = callee.text().to_string();
        let written = written.trim();
        let name = written.rsplit("::").next().unwrap_or(written);
        if name == "iota" {
            let arguments = crate::sema::call_syntax::arguments_of(expression).unwrap_or_default();
            let first = arguments.first()?;
            return Some(
                match type_of_expression_memoed(
                    memo,
                    &crate::sema::declarations::ShapeTable::new(),
                    index,
                    declaring,
                    scopes,
                    root,
                    path,
                    first,
                    depth + 1,
                ) {
                    Known::Yes((type_of, file)) => Known::Yes((type_of, file)),
                    _ => Known::Unknown(UnknownReason::UnknownType(Box::from("std::views::iota"))),
                },
            );
        }
        // **`std::ranges::subrange(begin, end)`** — the element of the range a `begin()` came from.
        if name == "subrange" {
            let arguments = crate::sema::call_syntax::arguments_of(expression).unwrap_or_default();
            let first = arguments.first()?;
            if let Some(element) = element_of_an_iterator_expression(
                memo,
                index,
                declaring,
                scopes,
                root,
                path,
                first,
                depth + 1,
            ) {
                return Some(element);
            }
        }
    }

    None
}

/// **The expression a name is initialized with**, when the name is a variable of this file.
///
/// The one step that lets [`element_of_a_range_expression`] see a pipeline a reader bound first
/// (`auto ids = items | std::views::filter(pred);`): the view's own type has no element to ask, and the initializer
/// is the expression the in-place case already reads. [`deduction_inputs`] is the reader of a declaration's
/// initializer and takes the declaration the way the `auto` path does; a name that resolves to a parameter, to a
/// declaration of another file, or to one with no initializer answers `None` — the fallback the caller has either way.
fn the_initializer_a_name_binds(
    expression: &cpp_parser::CppSyntaxNode,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
) -> Option<cpp_parser::CppSyntaxNode> {
    let at = end_of_the_written_name(expression);
    let Known::Yes(binding) = crate::sema::resolve::definition_at(scopes, root, at) else {
        return None;
    };
    let named = NamedDeclaration::Here(binding);
    deduction_inputs(root, &named).map(|(_, initializer)| initializer)
}

/// **The type a generic lambda's parameter receives from the standard view or algorithm it is handed to.**
///
/// `views::transform([](auto n) { return n * n; })` has no declaration that says what `n` is: the parameter is the
/// placeholder, and the only thing that knows is the call the lambda is an argument of. For the standard adaptors and
/// algorithms the rule is fixed — the callable's **first** parameter receives the range's **element**:
///
/// ```text
/// items | views::filter([](auto v) { … })          v is the element of `items`
/// items | views::transform([](auto v) { … })       the same
/// std::ranges::for_each(items, [](auto v) { … })   the same
/// ```
///
/// Only a spelling the standard fixes is read. The adaptor's name must be written with `views::` and the algorithm's
/// with `ranges::` (a project's own `filter` is not this), the parameter must be the callable's first (a comparator
/// taking two elements is a question this table does not answer), and the range's element must be known — otherwise
/// `None`, which is the answer the caller had before.
///
/// The reference a range yields is treated as an **lvalue** ([`auto_substituted`]'s last argument): a container's
/// element is one, which is what makes `auto&` a `T&`. A view that yields **prvalues** (`iota | transform(…)`) is the
/// case where that is not known to hold — `auto&&` there would be `T&&` — and the value category is not part of
/// [`Type`]; `auto` and `const auto&` are unaffected, because copying and binding to const are the same either way.
#[allow(clippy::too_many_arguments)]
pub(crate) fn the_element_a_lambda_parameter_receives(
    memo: &TypeMemo,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    binding: &crate::Binding,
    written: &str,
    depth: usize,
) -> Option<String> {
    use cpp_parser::CppAstNode;

    if depth > MAX_TYPE_DEPTH {
        return None;
    }

    // **The lambda this name is a parameter of, and which parameter it is.** The element goes to the callable's first
    // argument; any other position is a shape this table does not answer (`sort`'s comparator takes two elements).
    let token = cpp_parser::token_at(root, binding.name_range.start_offset)?;
    let lambda = token
        .parent_ancestors()
        .find_map(cpp_parser::CppLambdaExpr::cast)?;
    let list = lambda.get_params()?;
    let at = binding.name_range.start_offset;
    let mut position = None;
    for (index, parameter) in list.get_params().enumerate() {
        let range = parameter.syntax().text_range();
        if usize::from(range.start()) <= at && at < usize::from(range.end()) {
            if !parameter.is_auto() {
                return None;
            }
            position = Some(index);
            break;
        }
    }
    if position? != 0 {
        return None;
    }

    // **The call the lambda is an argument of**, and the range it is applied to. A one-argument call takes its range
    // from the pipe it is written on (`items | views::filter(pred)`); a two-argument one writes the range first
    // (`std::ranges::for_each(items, f)`).
    let call = lambda
        .syntax()
        .ancestors()
        .skip(1)
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CallExpr)?;
    let callee = call.children().next()?;
    let callee_text = callee.text().to_string();
    let callee_text = callee_text.trim();
    // **The spellings the standard fixes** — the adaptors, whose range is the pipe they are written on, and the
    // algorithms, whose range is the call's first argument. The comparison is the same one [`adapted_element`] makes
    // for `views::transform`/`keys`/`values`/`join`: a suffix of the callee's text, so `std::views::filter` and
    // `views::filter` are one spelling. A project's own `myviews::filter` would match too, which is the same allowance
    // the element table already makes and the reason this is documented as a spelling rather than as a resolved name.
    const ADAPTORS: [&str; 4] = [
        "views::filter",
        "views::transform",
        "views::take_while",
        "views::drop_while",
    ];
    const ALGORITHMS: [&str; 6] = [
        "ranges::for_each",
        "ranges::find_if",
        "ranges::any_of",
        "ranges::all_of",
        "ranges::none_of",
        "ranges::count_if",
    ];
    let is_a_standard_callable = ADAPTORS
        .iter()
        .chain(ALGORITHMS.iter())
        .any(|suffix| callee_text.ends_with(suffix));
    if !is_a_standard_callable {
        return None;
    }
    let arguments = crate::sema::call_syntax::arguments_of(&call)?;
    let lambda_at = arguments
        .iter()
        .position(|argument| argument.text_range() == lambda.syntax().text_range())?;
    let range = match arguments.len() {
        1 => {
            let parent = call.parent()?;
            let (left, right) = the_two_operands_of_a_pipe(&parent)?;
            if right.text_range() != call.text_range() {
                return None;
            }
            left
        }
        2 if lambda_at == 1 => arguments[0].clone(),
        _ => return None,
    };

    let Some(Known::Yes((element, _))) = element_of_a_range_expression(
        memo,
        index,
        declaring,
        scopes,
        root,
        path,
        &range,
        depth + 1,
    ) else {
        return None;
    };
    auto_substituted(written, &element, true)
}

/// **The two operands of `a | b`**, when the expression is one.
///
/// The operator is a **token** between two nodes, so the two children are the operands — the same shape every
/// `BinaryExpr` has.
pub(crate) fn the_two_operands_of_a_pipe(
    expression: &cpp_parser::CppSyntaxNode,
) -> Option<(cpp_parser::CppSyntaxNode, cpp_parser::CppSyntaxNode)> {
    if CppSyntaxKind::from(expression.kind()) != CppSyntaxKind::BinaryExpr {
        return None;
    }
    let is_a_pipe = expression
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| token.text() == "|");
    if !is_a_pipe {
        return None;
    }
    let mut operands = expression.children();
    let left = operands.next()?;
    let right = operands.next()?;
    Some((left, right))
}

/// **The element a view adaptor yields from `element`** — the right-hand side of a pipe, applied.
#[allow(clippy::too_many_arguments)]
pub(crate) fn adapted_element(
    element: &Known<(Type, PathBuf)>,
    adaptor: &cpp_parser::CppSyntaxNode,
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
) -> Known<(Type, PathBuf)> {
    use cpp_parser::CppAstNode;

    let Known::Yes((element, file)) = element else {
        return element.clone();
    };
    let written = adaptor.text().to_string();
    let written = written.trim();

    // **`transform`'s element is the lambda's return type.** It used to be refused — the body is where the answer is, and
    // a guess would offer the wrong members — but the deduction for a lambda's `return` exists
    // (`a_lambdas_return_type`, which a call of the lambda uses), so this is the same answer rather than a guess. A body
    // this layer cannot type (`return ++counter;`) still answers nothing.
    //
    // **A return that projects a member** (`[](auto item) { return item.name; }`) is the second half: the syntax alone
    // cannot state it, because a member's type is the class's declaration and therefore the index's. See
    // [`the_member_a_lambdas_return_names`].
    if written.contains("views::transform") || written.contains("views::transform_view") {
        let lambda = adaptor
            .descendants()
            .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::LambdaExpr);
        let label = lambda.and_then(|lambda| {
            if let Some(label) = crate::sema::declarations::a_lambdas_return_type(
                &lambda,
                std::slice::from_ref(element),
            ) {
                return Some(label);
            }
            let lambda = cpp_parser::CppLambdaExpr::cast(lambda)?;
            the_member_a_lambdas_return_names(&lambda, element, index, scopes, root, path)
        });
        return match label {
            Some(label) => Known::Yes((
                crate::sema::types::parse_type_spelling(&label),
                file.clone(),
            )),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from(
                "std::views::transform",
            ))),
        };
    }

    // **`keys` / `values`** are the two halves of the left element's `pair` — `map | std::views::keys` yields `K`.
    if written.contains("views::keys") {
        return match pair_component(element, 0) {
            Some(component) => Known::Yes((component, file.clone())),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from("std::views::keys"))),
        };
    }
    if written.contains("views::values") {
        return match pair_component(element, 1) {
            Some(component) => Known::Yes((component, file.clone())),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from("std::views::values"))),
        };
    }

    // **`join`** flattens a range of ranges: the element of the element.
    if written.contains("views::join") {
        return match element_of_the_element(element) {
            Some(inner) => Known::Yes((inner, file.clone())),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from("std::views::join"))),
        };
    }

    // **Every other adaptor passes the element through**: `filter`, `reverse`, `take`, `drop`, `take_while`,
    // `drop_while`, `filter_view`, `reverse_view`, `take_view`, `drop_view`, `common`, `all`, `counted`, `elements`…
    Known::Yes((element.clone(), file.clone()))
}

/// **The type a transform's callable returns when its body projects a member** — `[](auto item) { return item.name; }`.
///
/// `a_lambdas_return_type` answers a return the syntax states on its own (`n`, `n * n`, a literal). A member access
/// does not: the object's class and the member's declaration are the *index's* answer, not the expression's. The object
/// must be one of the lambda's **parameters** — the generic one takes the range's element, a written one its own
/// spelling — and the type that comes back is the member's recorded type, substituted through a class template's
/// arguments when the object is one (`std::pair<int, std::string>` yields `std::string` for `second`).
///
/// Conservative by construction: every `return` must project a member of a parameter, the class's member list must
/// answer, and the member must be a **data** member. Anything else is `None`, which leaves the caller's refusal in
/// place rather than offering a member the class does not have.
fn the_member_a_lambdas_return_names(
    lambda: &cpp_parser::CppLambdaExpr,
    element: &Type,
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
) -> Option<String> {
    use cpp_parser::CppAstNode;

    let body = lambda.get_body()?;
    let parameters: Vec<cpp_parser::CppParam> = lambda.get_params()?.get_params().collect();
    let mut agreed: Option<String> = None;
    for statement in body
        .syntax()
        .descendants()
        .filter_map(cpp_parser::CppReturnStat::cast)
    {
        // **A nested lambda's return is its own** — the same rule the syntax-only deduction states.
        if statement
            .syntax()
            .ancestors()
            .take_while(|ancestor| ancestor != body.syntax())
            .any(|ancestor| CppSyntaxKind::from(ancestor.kind()) == CppSyntaxKind::LambdaExpr)
        {
            continue;
        }
        let expression = statement.get_expr()?;
        let access = crate::sema::resolve::member_access_of(expression.syntax())?;
        let object = cpp_parser::CppNameExpr::cast(access.object)?.get_name_text()?;
        let parameter = parameters
            .iter()
            .find(|parameter| parameter.get_name_text().as_deref() == Some(object.as_str()))?;
        let spelled = if parameter.is_auto() {
            element.clone()
        } else {
            crate::sema::types::parse_type_spelling(&parameter.get_type_text())
        };
        // **The class, not the reference the parameter binds** — `const Item&` and `Item` have the same members.
        let class = as_a_range(&spelled).to_string();
        let Known::Yes(listed) = members_of(index, scopes, root, path, &class) else {
            return None;
        };
        // **A class template's arguments reach its members** — `pair<int, std::string>`'s `second` is the `std::string`
        // only after the substitution; a plain class is left alone by the same function.
        let Known::Yes(listed) = substituted_members(index, path, &class, Known::Yes(listed))
        else {
            return None;
        };
        let member = listed.members.iter().find(|member| {
            member.fact.name == access.member && member.fact.kind == crate::DeclKind::Variable
        })?;
        let written = member.fact.type_of.clone()?.to_string();
        match &agreed {
            None => agreed = Some(written),
            Some(previous) if *previous == written => {}
            Some(_) => return None,
        }
    }
    agreed
}

/// **One component of a `pair`-shaped type**, without the `const` a map's key carries.
pub(crate) fn pair_component(element: &Type, position: usize) -> Option<Type> {
    let Type::Named { name, arguments } = element else {
        return None;
    };
    let plain = name.rsplit("::").next().unwrap_or(name);
    if plain != "pair" && plain != "tuple" {
        return None;
    }
    let component = arguments.get(position)?.clone();
    Some(strip_a_leading_const(&component))
}

/// The same type without a leading `const` — `keys` yields the key, and the key of a `map` is `const K` **in the pair**,
/// not in the view's element.
pub(crate) fn strip_a_leading_const(type_of: &Type) -> Type {
    let written = type_of.to_string();
    match written.strip_prefix("const ") {
        Some(rest) => crate::sema::types::parse_type_spelling(rest),
        None => type_of.clone(),
    }
}

/// **The element of the element** — `join`'s rule, for the shapes this layer can read.
pub(crate) fn element_of_the_element(element: &Type) -> Option<Type> {
    let written = element_of(element, 0)?;
    Some(crate::sema::types::parse_type_spelling(&written))
}

/// **The element of the range an iterator expression came from** — `numbers.begin()`, `numbers.begin() + 3`.
///
/// The iterator's own type is a class template this layer cannot read; the range whose `begin()` produced it is written in
/// the expression, and its element is the answer.
pub(crate) fn element_of_an_iterator_expression(
    memo: &TypeMemo,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    expression: &cpp_parser::CppSyntaxNode,
    depth: usize,
) -> Option<Known<(Type, PathBuf)>> {
    // `v.begin()` / `v.end()` — a member call whose object is the range.
    if let Some(access) = crate::sema::resolve::member_access_of(expression) {
        let written = access.member.clone();
        if written == "begin" || written.ends_with(".begin") {
            let Known::Yes((container, file)) = type_of_expression_memoed(
                memo,
                &crate::sema::declarations::ShapeTable::new(),
                index,
                declaring,
                scopes,
                root,
                path,
                &access.object,
                depth + 1,
            ) else {
                return None;
            };
            let element = range_element_of(as_a_range(&container))?;
            return Some(Known::Yes((
                crate::sema::types::parse_type_spelling(&element),
                file,
            )));
        }
    }
    // `begin + 3` — the iterator is the left operand, and the question is the same.
    if CppSyntaxKind::from(expression.kind()) == CppSyntaxKind::BinaryExpr {
        let left = expression.children().next()?;
        return element_of_an_iterator_expression(
            memo,
            index,
            declaring,
            scopes,
            root,
            path,
            &left,
            depth + 1,
        );
    }
    None
}

/// **The callee of a call expression**, when the expression is one.
pub(crate) fn a_call_to_a_function(
    expression: &cpp_parser::CppSyntaxNode,
) -> Option<cpp_parser::CppSyntaxNode> {
    if CppSyntaxKind::from(expression.kind()) != CppSyntaxKind::CallExpr {
        return None;
    }
    expression.children().next()
}

/// **The type a range expression *is*, with the reference and the qualifiers peeled off.**
///
/// `for (auto &x : values)` where `values` is a parameter — `const std::vector<T>&` — is the ordinary spelling in
/// every C++ code base, and a range written as a reference yields exactly what the range itself yields. Peeling it
/// here rather than inside [`range_element_of`] is deliberate: that function also answers "what element does this
/// *object* hold" for `front()` and friends, where the reference is part of the answer — `grid.front()` is a
/// `std::vector<int>&` and must stay one.
pub(crate) fn as_a_range(type_of: &Type) -> &Type {
    match type_of {
        Type::Reference { to, .. } => as_a_range(to),
        Type::Qualified { of, .. } => as_a_range(of),
        other => other,
    }
}

/// **The element a range yields** — the whole element, where [`element_of`] answers one *component* of it.
///
/// The difference is what the two spellings ask: `for (auto x : map)` binds the `pair` itself, while
/// `for (auto [key, value] : map)` binds its two halves.
pub(crate) fn range_element_of(container: &Type) -> Option<String> {
    // **A reference and a qualifier are the binding's business**, and this is where the rule is load-bearing:
    // `const std::map<std::string, std::string, std::less<>>& configMap` is the ordinary spelling of a by-reference
    // parameter, and a reader that matched `Type::Named` alone answered "this container has no element" for every
    // one of them. Measured on `CodeFormatCore/src/Config/LuaStyle.cpp:20` — `auto it = configMap.find(key);` was
    // typed `_Tree_const_iterator<_Scary_val>` (the class's own member, unsubstituted) instead of the element, and
    // `it->second` answered "has no member `second`". [`Type::class_name`] and [`Type::arguments`] are the same two
    // questions the member walk asks, and both see through `Reference` and `Qualified`.
    let Some(name) = container.class_name() else {
        return match container {
            Type::Array { of, .. } => Some(of.to_string()),
            _ => None,
        };
    };
    let arguments = container.arguments();

    let plain = name.rsplit("::").next().unwrap_or(name);

    // **`>= 2`, not `== 2`.** The element of a map is `pair<const K, V>` whatever else the use
    // wrote, and a use writes the comparator as often as not: `std::map<std::string, LuaTokenKind,
    // std::less<>>` has three arguments, and requiring exactly two answered "no element" for the
    // ordinary spelling.
    if matches!(
        plain,
        "map" | "unordered_map" | "multimap" | "unordered_multimap"
    ) && arguments.len() >= 2
    {
        return Some(format!(
            "std::pair<const {}, {}>",
            arguments[0], arguments[1]
        ));
    }

    // The same allowance for the one-argument containers: a `vector<T, Alloc>` and a `set<K, Compare>` are the same
    // element as their one-argument spellings.
    if !arguments.is_empty()
        && matches!(
            plain,
            "vector"
                | "list"
                | "deque"
                | "forward_list"
                | "set"
                | "unordered_set"
                | "multiset"
                | "unordered_multiset"
                | "basic_string"
                | "string"
                | "span"
                | "initializer_list"
                | "valarray"
        )
    {
        return Some(arguments[0].to_string());
    }

    if matches!(plain, "array") && !arguments.is_empty() {
        return Some(arguments[0].to_string());
    }

    None
}

/// The type of what the declaration is initialized with.
///
/// The declaration's own syntax, found by **descending to the binding's range**: a binding's range is its
/// declarator (`n = count()` is the `InitDeclarator`), and the initializer is one of that node's children — the
/// shape the parser gives every initialized declaration, `int x = 0;` and `auto x = f();` alike.
pub(crate) fn initializer_type(
    memo: &TypeMemo,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    named: &NamedDeclaration,
    depth: usize,
) -> Known<(Type, PathBuf)> {
    let Some((_, expression)) = deduction_inputs(root, named) else {
        // Either the declaration is in another file — the initializer is written where the declaration is, and this
        // layer holds one file's syntax — or it has none at all (`auto n;` is ill-formed, and saying so is the
        // honest answer).
        return Known::Unknown(UnknownReason::UnknownType(Box::from("auto")));
    };

    type_of_expression_memoed(
        memo,
        &crate::sema::declarations::ShapeTable::new(),
        index,
        declaring,
        scopes,
        root,
        path,
        &expression,
        depth,
    )
}

/// The spelling `auto` stands for, and the expression that decides it.
///
/// # Why the spelling is rebuilt rather than taken from the declaration's recorded type
///
/// [`crate::declared_type_of`] strips declaration **specifiers**, which is right for a lookup by name (`static
/// const Widget` names `Widget`) and wrong for a type a reader is shown: `const auto& r = x;` has to deduce
/// `const int&`, and the `const` is a specifier the recorded spelling has already dropped. So the two halves are
/// read from the syntax instead — the specifier sequence (`const auto`) and the declarator's own operators (`&`) —
/// and put back together the way the file would have spelled the type it let `auto` stand for.
///
/// The declarator's operators are its text with the name taken out: `& r ` is `&`, `* p ` is `*`, `p ` is nothing.
/// The **expression a `typeof`/`__typeof__` specifier names**, when that is what its payload is.
///
/// `None` for the type-id form (`typeof(struct point)`), which the syntax reader already resolves, and for every
/// declaration that has no `typeof` at all.
pub(crate) fn a_typeof_payload(
    root: &cpp_parser::CppSyntaxNode,
    named: &NamedDeclaration,
) -> Option<cpp_parser::CppSyntaxNode> {
    let NamedDeclaration::Here(binding) = named else {
        return None;
    };
    let declarator = node_covering(root, binding.name_range.start_offset, binding.range)?;
    let specifiers = declarator.ancestors().find_map(|node| {
        node.children().find(|child| {
            cpp_parser::CppSyntaxKind::from(child.kind())
                == cpp_parser::CppSyntaxKind::DeclSpecifierSeq
        })
    })?;
    let typeof_node = specifiers.children().find(|child| {
        let text = child.text().to_string();
        let name = text
            .trim()
            .split('(')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        matches!(
            name.as_str(),
            "typeof"
                | "typeof_unqual"
                | "__typeof__"
                | "__typeof"
                | "__decltype"
                | "decltype"
                | "__typeof_unqual__"
        )
    })?;
    // The payload is the node inside the parentheses. A type-id has a TypeId child and is not this question.
    typeof_node.children().find(|child| {
        !matches!(
            cpp_parser::CppSyntaxKind::from(child.kind()),
            cpp_parser::CppSyntaxKind::TypeId | cpp_parser::CppSyntaxKind::DeclSpecifierSeq
        )
    })
}
/// Does this specifier sequence hold an **unnamed class body** — `struct { … }` with no tag of its own?
///
/// The shape whose type has no spelling, which the query path keys by the declarator's name. See the note on
/// `NamedDeclaration::type_of`.
pub(crate) fn an_unnamed_class_body(
    specifiers: &cpp_parser::CppSyntaxNode,
) -> Option<cpp_parser::CppSyntaxNode> {
    let body = specifiers.children().find(|child| {
        matches!(
            cpp_parser::CppSyntaxKind::from(child.kind()),
            cpp_parser::CppSyntaxKind::ClassDef
                | cpp_parser::CppSyntaxKind::StructDef
                | cpp_parser::CppSyntaxKind::UnionDef
        )
    })?;
    let tag = body.children().any(|child| {
        cpp_parser::CppSyntaxKind::from(child.kind()) == cpp_parser::CppSyntaxKind::NameExpr
    });
    (!tag).then_some(body)
}
pub(crate) fn deduction_inputs(
    root: &cpp_parser::CppSyntaxNode,
    named: &NamedDeclaration,
) -> Option<(String, cpp_parser::CppSyntaxNode)> {
    let NamedDeclaration::Here(binding) = named else {
        return None;
    };

    let declarator = node_covering(root, binding.name_range.start_offset, binding.range)?;
    // **The initializer is the *declarator*'s child, and a binding may range over the `InitDeclarator` that wraps it.**
    // `Box box(7);` is `InitDeclarator > Declarator > [NameExpr, Initializer]`, so a walk that looked for the initializer
    // among the outermost node's children found none and answered "this declaration has no initializer" — which is what
    // made class template argument deduction unreachable. Measured: `techniques::Box box(7)` typed as
    // `techniques::Box`.
    let is_an_initializer = |node: &cpp_parser::CppSyntaxNode| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::Initializer
    };
    let declarator = if declarator.children().any(|child| is_an_initializer(&child)) {
        declarator
    } else {
        declarator.children().find(|child| {
            cpp_parser::CppSyntaxKind::from(child.kind()) == cpp_parser::CppSyntaxKind::Declarator
        })?
    };
    let initializer = declarator
        .children()
        .find(|child| is_an_initializer(child))?;
    let first = initializer.children().next()?;
    // **A paren initialiser holds an *argument list*, not an expression.** `Box box(7);` is
    // `Initializer > ArgumentList > LiteralExpr`, and handing the list itself to the expression walk answers nothing —
    // the `auto` path never noticed because `auto n(7)` is unusual while `Box box(7)` is the shape C++17's class
    // template argument deduction is written for. Measured: `techniques::Box box(7)` typed as `techniques::Box`.
    let expression = if cpp_parser::CppSyntaxKind::from(first.kind())
        == cpp_parser::CppSyntaxKind::ArgumentList
    {
        first.children().next()?
    } else {
        first
    };

    // The specifiers are a sibling of the declarator's parent — `Declaration > [DeclSpecifierSeq, InitDeclarator]`
    // — and they are where the `auto` and its qualifiers are written.
    let specifiers = declarator
        .ancestors()
        .find_map(|node| {
            node.children().find(|child| {
                cpp_parser::CppSyntaxKind::from(child.kind())
                    == cpp_parser::CppSyntaxKind::DeclSpecifierSeq
            })
        })
        .map(|node| node.text().to_string())
        .unwrap_or_default();

    // The name's own text, taken from the file: the declarator's text minus the name is what was written *around*
    // it — the half the recorded type keeps and the specifiers do not. The **declarator** rather than the node the
    // binding ranges over, because that node is the whole `InitDeclarator` (`& r = x`) and its text holds the
    // initializer too.
    let name_range = binding.name_range;
    let name = root
        .text()
        .to_string()
        .get(name_range.start_offset..name_range.end_offset())
        .unwrap_or_default()
        .to_string();
    let operators: String = declarator
        .children()
        .find(|child| {
            cpp_parser::CppSyntaxKind::from(child.kind()) == cpp_parser::CppSyntaxKind::Declarator
        })
        .map(|node| node.text().to_string())
        .unwrap_or_default()
        .replace(&name, "")
        .split_whitespace()
        .collect();

    Some((
        format!("{} {}", specifiers.trim(), operators)
            .trim()
            .to_string(),
        expression,
    ))
}

/// The node whose span is exactly `range`, found along the path to `offset`.
///
/// A binding records a range, and the node it names can be several levels down; walking from the root along the
/// child that contains the offset costs the depth of the tree rather than a search of it.
pub(crate) fn node_covering(
    root: &cpp_parser::CppSyntaxNode,
    offset: usize,
    range: cpp_parser::SourceRange,
) -> Option<cpp_parser::CppSyntaxNode> {
    let matches = |node: &cpp_parser::CppSyntaxNode| {
        usize::from(node.text_range().start()) == range.start_offset
            && usize::from(node.text_range().end()) == range.end_offset()
    };

    let mut node = root.clone();
    loop {
        if matches(&node) {
            return Some(node);
        }

        // **Nodes only, and half-open.** The descent follows the child that contains the offset, and two details
        // decide whether it arrives: a token contains the offset just as its node does (taking it would end the
        // walk one level early), and a node's `end` is *exclusive* — so an offset at the boundary between two
        // children belongs to the one that starts there. Both were wrong once: `auto n = …` answered `None`
        // because the name's offset is exactly where the specifier sequence before it ends.
        let next = node
            .children_with_tokens()
            .find(|element| {
                element.as_node().is_some_and(|child| {
                    let start = usize::from(child.text_range().start());
                    let end = usize::from(child.text_range().end());
                    offset >= start && offset < end
                })
            })
            .and_then(|element| element.into_node())?;

        node = next;
    }
}

/// The written spelling with the initializer's type substituted for `auto`.
///
/// What was written *around* `auto` is what a reader sees and what the language applies: the qualifiers in front
/// (`const`), and the declarator's operators behind (`&`, `*`). The `&&` case is refused here — see
/// [`declared_type`] — and each `*` has to match a pointer in the deduced type, because a `*` that does not is a
/// declaration that does not compile rather than a type to report.
pub(crate) fn auto_substituted(
    written: &str,
    deduced: &Type,
    initializer_is_an_lvalue: bool,
) -> Option<String> {
    // **`decltype(auto)` means "the type this expression has"** — so the deduced type *is* the answer, not a
    // `decltype(…)` written around it. Measured: `decltype(auto) same = widen_gently(1);` answered `decltype(int)`, which
    // is a type no reader writes and no declaration contains.
    if written.trim() == "decltype(auto)" {
        return Some(deduced.to_string().trim().to_string());
    }
    let deduced = deduced.to_string();
    let deduced = deduced.as_str();
    // The `auto` **word**, by position: a spelling can hold those four letters inside a longer name
    // (`automatic`), and splitting on words is what keeps the two apart.
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let bytes = written.as_bytes();
    let mut at = 0usize;
    let mut found = None;

    while at < bytes.len() {
        if !is_word(bytes[at]) {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && is_word(bytes[at]) {
            at += 1;
        }
        // **`__auto_type` is the same placeholder written with GNU's spelling**, and it is one word in this scan — so
        // comparing against `auto` alone left it unreplaced, and the whole deduction answered nothing: measured,
        // `__auto_type value = 41;` went from `__auto_type` to *no type at all*. Pinned by
        // `gnu_auto_type_is_deduced_from_its_initializer`.
        if matches!(&written[start..at], "auto" | "__auto_type") {
            found = Some((start, at));
            break;
        }
    }

    let (start, end) = found?;
    // **A storage or function specifier is not part of the type.** `static constexpr auto members = …` is declared
    // with the *type* of its initializer, and this function keeps what was written before `auto` — so the answer came
    // out `static constexpr info[]`, which the hover card printed beside its own `static constexpr` (twice) and the
    // inlay hint drew after `auto`. Measured on a C++26 reflection example. `const` and `volatile` stay: a
    // cv-qualifier *is* part of the type, which is why they were kept in the first place.
    let prefix: String = written[..start]
        .split_whitespace()
        .filter(|word| {
            !matches!(
                *word,
                "static"
                    | "inline"
                    | "constexpr"
                    | "consteval"
                    | "constinit"
                    | "extern"
                    | "thread_local"
                    | "mutable"
                    | "register"
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let prefix = prefix.as_str();
    let suffix = written[end..].trim();

    let mut base = deduced.trim().to_string();
    for _ in 0..suffix.matches('*').count() {
        base = pointee_type_name(&base)?.to_string();
    }
    // **The reference the declarator wrote, applied the language's way.**
    //
    // A reference in the initializer never survives into `auto`: `auto& v = f();` where `f()` returns `T&` deduces
    // `auto = T`, so `v` is `T&` — and the spelling this function built was `T&&`, a type that names no class and
    // that every later member query then failed on. Measured on a real project (`LSPHandle.cpp`'s
    // `auto &vfs = _server->GetVFS();`): `vfs` was typed **`VirtualFileSystem&&`**, and `vfs.GetVirtualFile(...)`
    // answered `Unknown(NotDeclaredHere("VirtualFileSystem::GetVirtualFile"))` — one class member away from the
    // declaration that is right there. Everything the variable fed (`vFile`, `opSyntaxTree`, `lineIndex`,
    // `totalLine`) inherited the refusal, which is why four unrelated-looking hover cards were wrong at once.
    //
    // `&&` collapses rather than refuting: `T&` stays `T&` and `T`/`T&&` become `T&&`. It used to be refused
    // outright — see the note on [`declared_type`] — which answered nothing for a declaration every C++ compiler
    // reads.
    // **`auto` alone declares a new object, so its type is the initializer's value type** — the reference, and the
    // top-level cv, are dropped. `auto x = std::move(y)` is `T` and not `T&&`: the initializer says where the value
    // comes from, and `x` is a fresh object rather than another name for `y`. Only a declarator that *writes* a
    // reference keeps one, and that is the branch below.
    //
    // Measured on a real project (`CodeFormat.cpp`): `auto diagnostic_results = std::move(_Arg.result.Data);`
    // recorded `std::vector<LuaDiagnosticInfo>&&`, and everything downstream of it failed — a range-`for` over the
    // variable could not type its element, so `diagnostic.Message` answered "the type of `diagnostic` is not known
    // here". The deduced reference only reached this function when the declarator wrote one, which is why the two
    // cases looked alike until a `std::move` initializer was written.
    if !suffix.contains('&') {
        while base.ends_with('&') {
            base.pop();
            base = base.trim_end().to_string();
        }
        // The declarator writes its own qualifiers through `prefix`, so a deduced one would spell `const const` —
        // and it is not part of a new object's type in the first place.
        for qualifier in ["const ", "volatile "] {
            if let Some(rest) = base.strip_prefix(qualifier) {
                base = rest.trim_start().to_string();
            }
        }
    }

    let mut reference = "";
    if suffix.contains('&') {
        let deduced_had_one = base.ends_with("&&") || base.ends_with('&');
        if deduced_had_one {
            let trimmed = base.trim_end_matches('&').trim_end().len();
            base.truncate(trimmed);
            base = base.trim_end().to_string();
        }
        reference = match (
            suffix.contains("&&"),
            deduced_had_one || initializer_is_an_lvalue,
        ) {
            (true, false) => "&&",
            _ => "&",
        };
    }
    let stars = "*".repeat(suffix.matches('*').count());

    let mut type_of = String::new();
    if !prefix.is_empty() {
        type_of.push_str(prefix);
        type_of.push(' ');
    }
    type_of.push_str(base.trim());
    // A `*` and a `&` are glued to what they apply to (`const int*`, `int&`) — the way the file would have spelled
    // the type it let `auto` stand for. The declarator's own operators come back in the order they were written:
    // `auto *&` is a reference to a pointer, and the `*` has already been applied to the base above.
    type_of.push_str(&stars);
    type_of.push_str(reference);

    Some(type_of)
}

/// Is this a return spelling the *body* decides — `auto`, `auto&`, `decltype(auto)`?
pub(crate) fn is_a_placeholder(written: &str) -> bool {
    matches!(
        written.trim(),
        "auto" | "auto&" | "auto&&" | "const auto" | "const auto&" | "decltype(auto)"
    )
}

pub(crate) fn what_a_call_has_in(fact: &DeclFact) -> Option<Type> {
    match fact.kind {
        // `Widget()` is a temporary of `Widget`, so a call of a class has the class. `DeclKind::Type` is exactly
        // "this declaration declares a class-like type", which is the case a call whose callee is a *type* lands in.
        crate::DeclKind::Type => Some(Type::named(fact.qualified_name())),
        // **A placeholder is not a return type.** `auto copy_of() const { return value_; }` records `auto` as the spelling
        // the file wrote, and parsing it answers a type *called* `auto` — which reads like an answer and is not one. `None`
        // here is what lets the caller deduce from the body, which is the same thing the same-file arm already does; the
        // body is read from the declaring file at the fact's offset **translated into that file's rendering**, which is
        // where this path used to land on a neighbouring declaration instead.
        _ => fact
            .returns
            .as_deref()
            .filter(|written| !is_a_placeholder(written))
            .map(parse_type_spelling),
    }
}

/// The declaration an expression names, or why it names none. See [`NamedDeclaration`].
/// Is this word written the way MSVC writes the macro that stands for a namespace?
///
/// `_STD`, `_CSTD`, `_RANGES`, `_STDEXT`, `_CHRONO` — every one of them a leading underscore, which is the half of
/// the convention the standard gives to the implementation. The other half is the all-capitals spelling
/// (`MY_API`, `HUGEP`), and both are what a reader looks at when deciding whether the word in front of a `::` is a
/// namespace or a macro that stands for one.
///
/// A **spelling**, and it is used where nothing better is available: the environment that could settle it is not
/// part of a name, and the alternative — splitting every `::` — would read `std::f` as `f` and answer with whichever
/// `f` it found. See the caller, which is the only place this decides anything.
pub(crate) fn written_like_a_macro(word: &str) -> bool {
    word.starts_with('_')
        || (word.len() > 1
            && word
                .chars()
                .all(|character| !character.is_alphabetic() || character.is_uppercase()))
}

/// **The type of one name a structured binding declared** — `value` in `for (auto [key, value] : map)`.
///
/// `None` when the name is not one: the ordinary case, and the one that must stay cheap. The check is the **binding**
/// the name resolves to, not the syntax around the *use* — `value.size()` is a `NameExpr` with no pattern above it,
/// and looking for one there answered nothing for exactly the position a reader hovers.
///
/// The element type comes from the **range** (or the initializer), decomposed by the standard library's own shapes:
///
/// ```text
///   map<K, V> / unordered_map<K, V>     the element is `pair<const K, V>`, so [key, value] is const K, V
///   vector<T> / list<T> / set<T> …      the element is `T`
///   pair<A, B> / tuple<A, B, …>         the element is the i-th argument
///   T[N] / std::array<T, N>             the element is `T`
/// ```
///
/// Anything else answers `None`: a class that specializes `tuple_size`/`tuple_element` is the language's extension
/// point, and this layer has no way to read a specialization it has not been taught.
pub(crate) fn structured_binding_type(
    memo: &TypeMemo,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    expression: &cpp_parser::CppSyntaxNode,
    depth: usize,
) -> Option<Known<(Type, PathBuf)>> {
    // **The declaration the name resolves to**, and then the pattern it was declared in.
    let at = end_of_the_written_name(expression);
    let Known::Yes(binding) = crate::sema::resolve::definition_at(scopes, root, at) else {
        return None;
    };
    if binding.kind != crate::BindingKind::Variable {
        return None;
    }

    let declared_at = binding.name_range.start_offset;
    let pattern = cpp_parser::token_at(root, declared_at)?
        .parent_ancestors()
        .find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind())
                == cpp_parser::CppSyntaxKind::StructuredBinding
        })?;

    // **Which name of the pattern this is**, by position — the order maps into the components, and the names
    // themselves mean nothing (`[first, second]` and `[a, b]` are the same binding).
    let mut position = None;
    let mut seen = 0usize;
    for child in pattern.children() {
        if cpp_parser::CppSyntaxKind::from(child.kind()) != cpp_parser::CppSyntaxKind::NameExpr {
            continue;
        }
        let range = cpp_parser::source_range(child.text_range());
        if range.start_offset == declared_at {
            position = Some(seen);
            break;
        }
        seen += 1;
    }
    let position = position?;

    // The container: the range of the `for` this pattern is a binding of, or the initializer of a plain declaration.
    let container = match range_expression_of_the_pattern(&pattern) {
        Some(range) => type_of_expression_memoed(
            memo,
            &crate::sema::declarations::ShapeTable::new(),
            index,
            declaring,
            scopes,
            root,
            path,
            &range,
            depth + 1,
        ),
        None => {
            let initializer = initializer_of_the_pattern(&pattern)?;
            type_of_expression_memoed(
                memo,
                &crate::sema::declarations::ShapeTable::new(),
                index,
                declaring,
                scopes,
                root,
                path,
                &initializer,
                depth + 1,
            )
        }
    };
    let Known::Yes((container, file)) = container else {
        return Some(container);
    };

    // **A class this project declares, whose members are `first` and `second`.** `element_of` is a table of the
    // standard library's shapes and knows nothing about a class's members; a pair-like type is exactly a class with those
    // two, and `members_of` has their types — substituted with the container's arguments, so `Pair<int, double>` binds
    // `int` and `double` and not the parameters it was declared with. The order is asked by **name** rather than by
    // position: the member list is sorted, so "the first data member" is not a question it can answer.
    let element = element_of(&container, position).or_else(|| {
        let Type::Named { name, .. } = &container else {
            return None;
        };
        let wanted = match position {
            0 => "first",
            1 => "second",
            _ => return None,
        };
        let Known::Yes(listed) = members_of(index, scopes, root, path, name) else {
            return None;
        };
        // **And the container's arguments substituted through it.** The session's `members_of` does this for a caller that
        // asked with a template-id (`Pair<int, double>`); a caller here holds the list *before* that step, so it runs it —
        // the same function, so the two cannot disagree about what `first` is.
        let Known::Yes(listed) =
            substituted_members(index, path, &container.to_string(), Known::Yes(listed))
        else {
            return None;
        };
        listed
            .members
            .iter()
            .find(|member| {
                member.fact.name == wanted && member.fact.kind == crate::DeclKind::Variable
            })
            .and_then(|member| member.fact.type_of.clone().map(|target| target.to_string()))
    });

    Some(match element {
        Some(element) => Known::Yes((crate::sema::types::parse_type_spelling(&element), file)),
        None => Known::Unknown(UnknownReason::UnknownType(Box::from(container.to_string()))),
    })
}

/// **Is this offset in the head of a range-based `for`?** — everything before its `:`.
///
/// The two halves of `for ( declaration : range )` are on either side of one token, which is why the question is asked
/// of the token rather than of the tree shape: the declaration may be a child of the loop or nested inside a
/// `Declaration`, and the body is simply everything after the colon.
pub(crate) fn is_in_the_head_of(loop_node: &cpp_parser::CppSyntaxNode, offset: usize) -> bool {
    use cpp_parser::{CppSyntaxElement, CppTokenKind};

    let mut colon = None;
    for child in loop_node.children_with_tokens() {
        let CppSyntaxElement::Token(token) = child else {
            continue;
        };
        if CppTokenKind::from(token.kind()) == CppTokenKind::Colon {
            colon = Some(usize::from(token.text_range().start()));
            break;
        }
    }

    // **A loop whose `:` this cannot find is not answered here.** `None` sends the caller to the ordinary deduction,
    // which has the initializer; treating "I could not read this" as "it is the variable" would be the same mistake in
    // the other direction, and one that a body declaration would pay for.
    colon.is_some_and(|colon| offset < colon)
}

/// The expression after the `:` of the range-based `for` this pattern is a binding of.
pub(crate) fn range_expression_of_the_pattern(
    binding: &cpp_parser::CppSyntaxNode,
) -> Option<cpp_parser::CppSyntaxNode> {
    use cpp_parser::CppTokenKind;

    let loop_node = binding.ancestors().find(|node| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::RangeForStat
    })?;

    let mut after_the_colon = false;
    for child in loop_node.children_with_tokens() {
        match child {
            cpp_parser::CppSyntaxElement::Token(token) => {
                if CppTokenKind::from(token.kind()) == CppTokenKind::Colon {
                    after_the_colon = true;
                }
            }
            cpp_parser::CppSyntaxElement::Node(node) => {
                if after_the_colon && node != *binding {
                    return Some(node);
                }
            }
        }
    }
    None
}

/// The initializer of `auto [a, b] = pair;` — the expression after the `=`.
///
/// **The `=` is inside the `InitDeclarator`**, not a child of the `Declaration`: measured, the tree is
/// `Declaration > InitDeclarator "[number, fraction] = pair" > [StructuredBinding, =, NameExpr]`. The first version scanned
/// the declaration and found no `=` there, so every structured binding over a `pair` answered nothing and the name rule below
/// then made up a spelling (`int()` for both names). The declarator is asked first and the declaration is the fallback for a
/// tree that writes the initializer one level up.
pub(crate) fn initializer_of_the_pattern(
    pattern: &cpp_parser::CppSyntaxNode,
) -> Option<cpp_parser::CppSyntaxNode> {
    use cpp_parser::CppTokenKind;

    let holders: Vec<cpp_parser::CppSyntaxNode> = pattern
        .ancestors()
        .take_while(|node| {
            !matches!(
                cpp_parser::CppSyntaxKind::from(node.kind()),
                cpp_parser::CppSyntaxKind::FunctionDef | cpp_parser::CppSyntaxKind::TranslationUnit
            )
        })
        .filter(|node| {
            matches!(
                cpp_parser::CppSyntaxKind::from(node.kind()),
                cpp_parser::CppSyntaxKind::InitDeclarator | cpp_parser::CppSyntaxKind::Declaration
            )
        })
        .collect();

    for holder in holders {
        let mut after_the_equals = false;
        for child in holder.children_with_tokens() {
            match child {
                cpp_parser::CppSyntaxElement::Token(token) => {
                    if CppTokenKind::from(token.kind()) == CppTokenKind::Assign {
                        after_the_equals = true;
                    }
                }
                cpp_parser::CppSyntaxElement::Node(node) if after_the_equals => return Some(node),
                cpp_parser::CppSyntaxElement::Node(_) => {}
            }
        }
    }
    None
}

/// **The type of the `position`-th component** of a container's elements, as a spelling.
///
/// The standard library's shapes, by name: `map`'s element is a `pair`, a sequence's element is its argument, and
/// `tuple`'s is the i-th. A name this does not recognise answers `None` — `tuple_size`/`tuple_element` specializations
/// are a language feature with no reader here.
pub(crate) fn element_of(container: &Type, position: usize) -> Option<String> {
    let Type::Named { name, arguments } = container else {
        // An array's element is its own type: `int[4]` binds `int`.
        return match container {
            Type::Array { of, .. } => Some(of.to_string()),
            _ => None,
        };
    };

    let plain = name.rsplit("::").next().unwrap_or(name);

    // A `map`'s element is `pair<const K, V>`: the key is `const`, and the position picks the side.
    if matches!(
        plain,
        "map" | "unordered_map" | "multimap" | "unordered_multimap"
    ) && arguments.len() == 2
    {
        return match position {
            0 => Some(format!("const {}", arguments[0])),
            _ => Some(arguments[1].to_string()),
        };
    }

    if matches!(plain, "array") && arguments.len() == 2 {
        return Some(arguments[0].to_string());
    }

    // A sequence, a set, or a span: one argument, and the element is it.
    if arguments.len() == 1
        && matches!(
            plain,
            "vector"
                | "list"
                | "deque"
                | "forward_list"
                | "set"
                | "unordered_set"
                | "multiset"
                | "unordered_multiset"
                | "basic_string"
                | "string"
                | "span"
                | "initializer_list"
                | "valarray"
        )
    {
        return Some(arguments[0].to_string());
    }

    // `pair` and `tuple` are the components themselves.
    if matches!(plain, "pair" | "tuple") {
        return arguments
            .get(position)
            .map(|component| component.to_string());
    }

    None
}
pub(crate) fn writes_a_name(written: &str) -> bool {
    let spelling = written.strip_prefix("::").unwrap_or(written);

    // **A whole expression is not a name**, and this is the half that keeps the widening below from swallowing one.
    //
    // `static_cast<T>(y)` has no `::` trouble and its segments pass once the argument list comes off — but the name
    // arm asks its question at the **last identifier**, which is `y` there, so the answer became `int` for a cast
    // that names `T`. Measured by a test that had been passing: `a cast names the type it casts to: int`. The arms
    // below this gate exist for exactly those spellings, and a gate that lets them through answers a different
    // question than the caller asked.
    //
    // What is left after this test is a name: `std::vector<int>` and `A<B>::C<D>` pass, `f(x)`, `a + b`, `{1, 2}`
    // and `static_cast<T>(y)` do not. `,` is in the list because `A<B, C>` is a name and `f(a, b)` is not — and
    // after the argument list comes off, the two are otherwise the same shape.
    // **A braced construction is not a name either.** `B<int>{}` passes every other test here — its one segment is
    // `B` once the `<…>` comes off — so the name arm claimed it and answered `UnknownType("B<int>{}")` before the
    // braced-list arm could type it. `auto x = B{};` worked only because a list with values writes a comma
    // (`std::pair{1, 2.5}`) and the comma was already in the list. Measured on `Builder<int>{}.with(2)` in
    // `templates.cpp`: the member access reported "the type of `Builder<int>{}` is not known".
    if spelling.contains('(')
        || spelling.contains(')')
        || spelling.contains(',')
        || spelling.contains('{')
        || spelling.contains('}')
    {
        return false;
    }

    !spelling.is_empty()
        && spelling
            .split("::")
            // Every segment is one identifier, or a **run of them** separated by single spaces — the macro prefix
            // and the name it stands in front of. A segment with a space in the middle of a word (`_STD  f` with
            // two) is still a run, because `split_whitespace` is what reads it.
            .all(|segment| {
                // **A template-id is still a name, and this gate used to say it was not.**
                //
                // `std::vector<int>` splits on `::` into `std` and `vector<int>`, and the second is not an
                // identifier — so the gate refused, the whole name lookup was **skipped**, and the answer was
                // `UnknownType("std::vector<int>")`. Measured on the report that started this: `auto x =
                // std::vector<int>();` refused while `std::vector<int> v;` worked, because a *declaration's* type
                // is read by another reader — one that understands a template-id — and only the call path came
                // through here.
                //
                // What comes off is the argument list, not the name: `A<B>::C<D>` is two segments and the name of
                // the first is `A`. A spelling that only *looks* like a template-id (`i<n` from a comparison the
                // grammar read as one) is harmless: this is a gate, and the lookup that follows finds nothing for a
                // name nobody declared.
                let bare = segment.split('<').next().unwrap_or(segment);
                let mut words = bare.split_whitespace();
                words.clone().count() > 0 && words.all(is_an_identifier)
            })
}

/// Is this segment a name a declaration could carry?
pub(crate) fn is_an_identifier(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
}

/// **What a function template's parameters stand for, from the types of the arguments the call passed.**
///
/// `template <class _It> auto f(_It p)` called as `f(a_pointer)` says `_It = int*` without writing it. The rule is
/// C++'s, restricted to the one case this layer can be sure of: a function parameter written as **exactly one**
/// template parameter takes that parameter's place, so the argument passed there is what the parameter stands for.
///
/// A parameter written as `vector<_Ty> const&` would need the argument's own template-id taken apart, and that is a
/// different deduction — attempted for none of the parameters if it is attempted for one, because a half-substituted
/// list is a type nobody wrote.
///
/// The result is **positional against the template parameter names**, which is how every consumer of a substitution
/// reads it: a name with no deduced argument stays itself, so `_Alloc` in `template <class _Ty, class _Alloc>` does
/// not silently become `_Ty`'s argument.
pub(crate) fn deduce_the_parameters(
    names: &[String],
    written_list: Option<&str>,
    argument_types: &[Type],
) -> Vec<Type> {
    let mut deduced: Vec<Type> = names.iter().map(Type::named).collect();
    if names.is_empty() || argument_types.is_empty() {
        return deduced;
    }

    let Some(list) = written_list else {
        return deduced;
    };

    for (at, parameter) in written_parameters(list).into_iter().enumerate() {
        // **The parameter's type, which is the parameter minus its name.** `_It p` is `_It` and `int count` is `int`;
        // a parameter written with no name at all (`int`) is one word and is its own type. Getting this wrong is
        // silent: the first version compared the whole `_It p` against the template parameter's `_It`, never matched,
        // and deduced nothing — so the wrong answer it was written to remove (`b = _It`) stayed exactly as it was.
        let words: Vec<&str> = parameter.split_whitespace().collect();
        let written_type = match words.len() {
            0 => continue,
            1 => words[0],
            _ => {
                &parameter[..parameter
                    .rfind(words[words.len() - 1])
                    .unwrap_or(parameter.len())]
            }
        };
        let written_type = written_type.trim();
        if written_type.is_empty() {
            continue;
        }

        // **Exactly one parameter name**, once a reference is taken off — and the reference is the point.
        //
        // Standard library functions are written `template <class _Iter> auto f(_Iter&& _It)`, which is a **forwarding
        // reference**: C++ deduces `_Iter` from the argument, and that is what the syntax is for. A rule that insisted
        // on the bare name matched nothing here, and the failure was quiet — the substitution fell back to the
        // parameter's own name, so `written_arguments` was `["_Iter"]` and every substitution was a no-op. Measured:
        // 54 `auto` declarations over eight MSVC headers, every one of them a `_STD` call whose callee takes `_Iter&&`.
        //
        // What this layer does with the reference is the part `auto` would do anyway: an argument passed to `_Iter&&`
        // gives `_Iter` the argument's own type, and the caller is an `auto` initializer, which strips references and
        // top-level `const` by definition. `_Iter*` — the parameter *contains* the parameter — is still not attempted,
        // because undoing a `*` needs the argument taken apart and a half-done version pairs the wrong argument with
        // the wrong parameter.
        let bare = written_type
            .strip_suffix("&&")
            .or_else(|| written_type.strip_suffix('&'))
            .map(str::trim)
            .unwrap_or(written_type);

        let Some(name_at) = names.iter().position(|name| name == bare) else {
            continue;
        };
        let Some(argument) = argument_types.get(at) else {
            continue;
        };

        deduced[name_at] = argument.clone();
    }

    deduced
}

/// A written parameter list, cut into its parameters at the commas that are **not inside brackets**.
///
/// `(int count, double factor)` is two; `(_Ty (*fn)(int, char))` is one, and a split on every comma would make it
/// three and pair the wrong argument with the wrong parameter — worse than not deducing at all.
pub(crate) fn written_parameters(list: &str) -> Vec<String> {
    let body = list.trim();
    let body = body.strip_prefix('(').unwrap_or(body);
    let body = body.strip_suffix(')').unwrap_or(body);

    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut at = 0usize;
    for (index, character) in body.char_indices() {
        match character {
            '(' | '[' | '<' => depth += 1,
            ')' | ']' | '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(body[at..index].to_string());
                at = index + 1;
            }
            _ => {}
        }
    }
    out.push(body[at..].to_string());

    out
}

/// **The offset at which "which name is this" is asked**: the expression's **last identifier**.
///
/// The offset decides the question, because [`crate::sema::resolve::definition_at`] builds a name's spelling by
/// walking its chain *up to the cursor* — a cursor on `a` in `a::b::c` asks about `a::b`, and one on `c` asks about
/// the whole of it. An expression node is the **whole** chain: the user pointed at an entity, and what identifies
/// that entity is all of it (`std::cin`, not the namespace `std`). So the question is asked at the far end, which
/// for a bare name is the offset it always was.
///
/// The last **identifier** rather than the node's end offset, and the difference is a bug this had for one revision:
/// a node's range covers its trailing trivia (`NameExpr` for `w` in `int w = 1;` ends after the space), so the end
/// offset lands *past* the name and the lookup answers `UnparsableName` — every plain name in the file losing its
/// type at once.
pub(crate) fn end_of_the_written_name(expression: &cpp_parser::CppSyntaxNode) -> usize {
    // **Not the last identifier in the text — the last identifier of the *name*.** The difference is the whole of
    // how a template-id is called: `_Convert_size<size_type>(n)` holds two identifiers, and the later of them is
    // the template **argument**. Taking it asked the scope for `size_type` — an alias, not a function — and the
    // call was then refused with a type that had nothing to do with the callee. Measured on a fixture where the
    // same function called through a header deduced and called in its own file refused.
    //
    // A `TemplateArgumentList` is where those identifiers live, and skipping its subtrees is the whole fix: the
    // name of `A<B>::C<D>` is still `C`, and the arguments on both sides are no longer candidates.
    let mut last = None;

    for element in expression.descendants_with_tokens() {
        let Some(token) = element.into_token() else {
            continue;
        };
        if cpp_parser::CppTokenKind::from(token.kind()) != cpp_parser::CppTokenKind::Identifier {
            continue;
        }
        if token
            .parent_ancestors()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::TemplateArgumentList)
        {
            continue;
        }
        last = Some(usize::from(token.text_range().start()));
    }

    last.unwrap_or_else(|| usize::from(expression.text_range().start()))
}
