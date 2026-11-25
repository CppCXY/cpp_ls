//! Inlay hints: what the file does not write and a reader still needs.
//!
//! # Two kinds of hint, and both are answerable
//!
//! An inlay hint is text the editor draws **in** the code — not part of the file, and not editable by accident — so the
//! bar for one is higher than for a popup: it sits in the middle of the line the user is reading. Two are worth that.
//!
//! ```text
//! a parameter's name    `scale(3, 0.5)`          →  `scale(count: 3, factor: 0.5)`   the callee's declaration
//!                                                                                     writes the names
//! a deduced type        `auto n = count();`      →  `auto n: int = count();`         the initializer's type
//!                       `for (auto& x : v)`      →  `for (auto& x: std::string& : v)` the range's element
//!                       `auto [a, b] = pair;`    →  `auto [a: int, b: double] = pair` each name's element
//!                       `auto f() { return 1; }` →  `auto f() -> int { return 1; }`   the deduced return
//! ```
//!
//! The first version of this module said the type column was *not* answerable — "the analysis has no type system, nothing
//! deduces `auto`". It grew one (`type_at` deduces from the initializer, a range's element, a structured binding, and
//! since the class-template deduction of `6e6ed31` a braced initialisation's arguments), so that paragraph became a gap
//! instead of a boundary. [`type_hints`] is the second half, and it asks the **same** reader the hover asks: one
//! implementation of "what type is this", because two would eventually disagree in front of a user.
//!
//! What is still not hinted, and why:
//!
//! * **a lambda's deduced return** — its type is unnamed and `deduced_returns_of` starts from a binding a lambda does
//!   not have;
//! * **a type written already** — `int n = 1;` needs no `int:` in front of it, and the hint would be noise.
//!
//! **A generic lambda's parameter is hinted now** — `[](auto &&x)` passed to a function that calls it with `*this`
//! gets `x: Foo&` — and that one breaks the "this layer holds one file" rule the paragraph above used to state: the
//! type comes from the callee's body, in the callee's file, which is why the deduction is a [`Session`] question and
//! this module only asks it ([`lambda_parameter_hints`]).
//!
//! [`Session`]: crate::Session
//!
//! See `docs/worklog.md` for the two that remain.
//!
//! # What makes a hint *useful* rather than noise
//!
//! Every argument of every call in the requested range could carry a name, which is what makes an editor's screen
//! unreadable. Three refusals keep the answer to the ones a reader cannot already see:
//!
//! * **the argument already says it** — `scale(count, factor)` needs no `count:` in front of `count`;
//! * **the callee is not a function this analysis can name** — a function pointer, a template parameter, an
//!   overload set nothing resolves: a hint would be a guess about which parameter list applies;
//! * **the position has no parameter** — a variadic tail, an argument past the declared parameters, an unnamed
//!   parameter. Hints are placed **by position**, so a parameter that has no name breaks the alignment for
//!   everything after it; see `crate::sema::scopes::parameters_of`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cpp_parser::{CppSyntaxKind, CppSyntaxNode, SourceRange};

use crate::index::project::{Callee, callee_of_a_call};
use crate::sema::call_syntax::{arguments_of, parameter_list_of, parameter_names_from_text};
use crate::sema::scopes::parameters_of;
use crate::{FileView, Known, ProjectIndex};
use cpp_parser::CppAstNode;

/// One parameter name, at the argument it belongs to.
///
/// The label is the *caller's* business — this protocol-less layer says which parameter a position is in, and the
/// LSP layer decides that it is drawn as `count:` with a space after it. A hint carrying a rendering would put a
/// wire format's punctuation in the crate that has no wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterHint {
    /// The argument's own start, in the file the request is about.
    pub offset: usize,
    /// The parameter's name, as the callee's declaration spells it.
    pub name: String,
}

/// **A type the file does not write**, at the place it would be written.
///
/// The label is the *caller's* business, like [`ParameterHint`]'s: this layer says which type and where it belongs, and
/// the LSP layer decides that it is drawn after the name with a colon in front of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeHint {
    /// Where the hint is drawn: the end of the name (or of `]` for a structured binding).
    pub offset: usize,
    /// The type, as the analysis spells it: `int`, `const int&`, `std::pair<int, double>`.
    pub label: String,
    /// Which of the three shapes asked for it — a consumer may draw them differently.
    pub kind: TypeHintKind,
}

/// The shapes a [`TypeHint`] can be about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeHintKind {
    /// `auto n = …` — the type the initializer gives it.
    Auto,
    /// `auto [a, b] = …` — the element each name binds.
    StructuredBinding,
    /// `auto f(…) { … }` — the return type the body deduces.
    DeducedReturn,
    /// **A generic lambda's parameter** — `[](auto &&x) {}` passed to a callable that calls it: the type the
    /// argument at the call site gives `x`, which is not visible in this file at all. Drawn like [`Auto`] (`: T`),
    /// and it is a *parameter's* type rather than a variable's.
    ///
    /// [`Auto`]: TypeHintKind::Auto
    LambdaParameter,
}

/// **The types a file does not write, in `range`.**
///
/// `type_at` answers the type of the expression at a byte offset (the session's `type_at`, which is the one reader of
/// that question), and `returns_at` answers the return type a declaration's fact records — a function's *written* `auto`
/// is replaced by the deduced spelling there, which is why this does not need a second inference.
///
/// Three refusals, each the same shape as [`parameter_hints`]'s:
///
/// * **the answer is `auto` or empty** — the deduction declined, and printing `auto` where the file already says it is
///   worse than nothing;
/// * **the position is not in `range`** — a hint outside the viewport is work the client throws away;
/// * **the declaration is not an `auto` one** — a written type is already on the line.
pub fn type_hints<F, G>(
    view: &FileView,
    range: SourceRange,
    mut type_at: F,
    mut returns_at: G,
) -> Vec<TypeHint>
where
    F: FnMut(usize) -> Option<String>,
    G: FnMut(usize) -> Option<String>,
{
    let mut hints: Vec<TypeHint> = Vec::new();

    for node in view.root.descendants() {
        let kind = CppSyntaxKind::from(node.kind());
        match kind {
            // **`auto n = …` and a range-for's `auto`, which is the same node.** The specifier sequence is a sibling of
            // the declaration that holds the declarator, so the question "was this written `auto`" is asked of the
            // declaration rather than of the declarator.
            CppSyntaxKind::InitDeclarator => {
                // A structured binding is its own shape below; the declarator here holds the names — **a level
                // deeper than a child**, measured: `InitDeclarator > Declarator > StructuredBinding`. The children test
                // never fired, so the range-for's `auto` drew the element type on top of the per-name hint and a reader
                // saw `name: const std::string` twice.
                if node.descendants().any(|child| {
                    CppSyntaxKind::from(child.kind()) == CppSyntaxKind::StructuredBinding
                }) {
                    continue;
                }
                if !contains(range, node.text_range().start().into())
                    || !writes_auto_in_the_declaration(&node)
                {
                    continue;
                }
                let Some(name) = the_name_of_a_declarator(&node) else {
                    continue;
                };
                let at = usize::from(name.text_range().end());
                if !contains(range, at) {
                    continue;
                }
                if let Some(label) = type_at(usize::from(name.text_range().start()))
                    && is_a_type_spelling(&label)
                {
                    hints.push(TypeHint {
                        offset: at,
                        label,
                        kind: TypeHintKind::Auto,
                    });
                }
            }
            // **`auto [a, b] = …`** — each name binds one element, and each is asked separately: `a: int, b: double`
            // is what a reader of the line needs, and one label for the whole list would have to invent a notation for
            // "the pair's two halves".
            CppSyntaxKind::StructuredBinding => {
                if !contains(range, node.text_range().start().into()) {
                    continue;
                }
                for name in node.children() {
                    if CppSyntaxKind::from(name.kind()) != CppSyntaxKind::NameExpr {
                        continue;
                    }
                    let at = usize::from(name.text_range().end());
                    if !contains(range, at) {
                        continue;
                    }
                    if let Some(label) = type_at(usize::from(name.text_range().start()))
                        && is_a_type_spelling(&label)
                    {
                        hints.push(TypeHint {
                            offset: at,
                            label,
                            kind: TypeHintKind::StructuredBinding,
                        });
                    }
                }
            }
            // **A lambda with no written return type** — `[](int v) { return v * 10; }` is an `int`, and the
            // deduction for it already exists (`a_lambdas_return_type`, which a *call* of the lambda uses). The hint goes
            // where the trailing `-> T` would be written: just before the body, after the specifiers.
            CppSyntaxKind::LambdaExpr => {
                if !contains(range, node.text_range().start().into()) {
                    continue;
                }
                let Some(body) = node
                    .children()
                    .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::CompoundStat)
                else {
                    continue;
                };
                // **The file's own answer wins.** `[](int v) noexcept -> int { … }` states the type; a hint that repeated
                // it would be noise, and one that disagreed with it would be a lie.
                let written = node.text().to_string();
                let before_the_body = written
                    .find(&body.text().to_string())
                    .map(|at| &written[..at])
                    .unwrap_or("");
                if before_the_body.contains("->") {
                    continue;
                }
                let at = usize::from(body.text_range().start());
                if !contains(range, at) {
                    continue;
                }
                if let Some(label) = crate::sema::declarations::a_lambdas_return_type(&node, &[])
                    && is_a_type_spelling(&label)
                {
                    hints.push(TypeHint {
                        offset: at,
                        label,
                        kind: TypeHintKind::DeducedReturn,
                    });
                }
            }
            // **`auto f(…)`** — the return type the body deduces, drawn after the parameter list where a trailing
            // return type would be written.
            CppSyntaxKind::Declaration | CppSyntaxKind::FunctionDef => {
                if !contains(range, node.text_range().start().into()) {
                    continue;
                }
                // **The declarator, however the declaration wraps it**: `auto one() { … }` is
                // `Declaration > [DeclSpecifierSeq "auto", InitDeclarator > Declarator > [NameExpr, ParameterList]]`,
                // and a direct-child search for `Declarator` found the `InitDeclarator` and stopped. Bounded to the
                // declarator's subtree, a `ParameterList` is a *function* declarator's — an expression's argument list
                // is an `ArgumentList`, a different kind.
                let Some(declarator) = node.children().find(|child| {
                    matches!(
                        CppSyntaxKind::from(child.kind()),
                        CppSyntaxKind::Declarator | CppSyntaxKind::InitDeclarator
                    )
                }) else {
                    continue;
                };
                let Some(list) = declarator.descendants().find(|child| {
                    CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ParameterList
                }) else {
                    continue;
                };
                if !writes_auto_in_the_declaration(&node) {
                    continue;
                }
                let at = usize::from(list.text_range().end());
                if !contains(range, at) {
                    continue;
                }
                let Some(name) = the_name_of_a_declarator(&node) else {
                    continue;
                };
                if let Some(label) = returns_at(usize::from(name.text_range().start()))
                    && is_a_type_spelling(&label)
                {
                    hints.push(TypeHint {
                        offset: at,
                        label,
                        kind: TypeHintKind::DeducedReturn,
                    });
                }
            }
            _ => {}
        }
    }

    hints
}

/// **Was this declaration written with `auto`** — in its specifier sequence, however it is nested?
///
/// The specifiers are a sibling of the declarator's parent (`Declaration > [DeclSpecifierSeq, InitDeclarator]`), and a
/// range-for's are inside the for statement's own declaration; walking the ancestors of the node asked about is what makes
/// both the same question.
fn writes_auto_in_the_declaration(node: &CppSyntaxNode) -> bool {
    // **The node's own specifiers first**: an `InitDeclarator`'s are its parent declaration's, while a `Declaration` or
    // `FunctionDef` that is asked about *is* the declaration — the walk from `ancestors()` alone missed `auto one()`.
    let own = node
        .children()
        .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::DeclSpecifierSeq);
    let enclosing = node.ancestors().take(4).find_map(|ancestor| {
        ancestor
            .children()
            .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::DeclSpecifierSeq)
    });
    own.or(enclosing).into_iter().any(|specifiers| {
        specifiers.descendants().any(|child| {
            let kind = CppSyntaxKind::from(child.kind());
            // **`auto` arrives as a `BuiltinType`** whose token is `AutoKeyword` (the grammar's own table) — the
            // `AutoType` kind exists for another shape. Both are the placeholder and both mean the same; asking only
            // for `AutoType` matched nothing, and every `auto` in the file went unhinted.
            kind == CppSyntaxKind::AutoType
                || (kind == CppSyntaxKind::BuiltinType && child.text().to_string().trim() == "auto")
        })
    })
}

/// The name a declarator declares — its innermost `NameExpr`, which is the identifier a reader sees.
fn the_name_of_a_declarator(node: &CppSyntaxNode) -> Option<CppSyntaxNode> {
    let name = node
        .descendants()
        .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::NameExpr)?;
    if name.text().to_string().trim().is_empty() {
        return None;
    }
    Some(name)
}

/// **Is this spelling a type a reader can use** — not the placeholder, not empty?
///
/// `auto` is the word the file already wrote: printing it in a hint says the analysis declined while looking like an
/// answer. `Type::to_string` of a deduced type never carries it, so this is a guard against a caller whose answer was the
/// placeholder's spelling.
fn is_a_type_spelling(label: &str) -> bool {
    let label = label.trim();
    !label.is_empty() && label != "auto" && !label.contains("auto")
}

/// **The parameter names for the calls in `range`** — what an editor draws as inlay hints.
///
/// `range` is the visible range a client asked about, in the file's own coordinates. Hints are placed at the
/// arguments themselves, and an argument is hinted only when it is inside the range: a hint outside the visible
/// area is work the client throws away.
///
/// # The files this reads
///
/// A call's parameter names are in the **callee's** file, which is usually not the file the cursor is in: `f(1)`
/// in a `.cpp` is declared in a header. The names come from the declaration's *scope* rather than from a text
/// reading of its parameter list, so the declaring file has to be parsed — and `view_of` is how, with the file
/// being edited already in hand so that a same-file call costs nothing. Each declaring file is parsed **once per
/// request** however many calls reach it, which is the whole reason for the map below.
pub fn parameter_hints<F>(
    index: &ProjectIndex,
    view: &FileView,
    range: SourceRange,
    mut view_of: F,
) -> Vec<ParameterHint>
where
    F: FnMut(&Path) -> Option<FileView>,
{
    let mut hints = Vec::new();

    // Declaring files, parsed once each: a file with twenty calls into it is parsed once, not twenty times.
    let mut declaring: HashMap<PathBuf, FileView> = HashMap::new();

    for call in call_expressions(&view.root, range) {
        // **A call with nothing to hint cannot produce a hint, and resolving it is the expensive part.**
        //
        // The containment test below is the one that decides whether any hint is emitted for an argument — and it ran
        // *after* the callee was resolved. Measured on `EmmyLuaCodeStyle` with `CPPLS_TRACE_INLAY=1`: `inlayHint` spent
        // **80 ms** in this loop for **nine** hints, and the fourteen calls that did resolve accounted for ~14 ms of
        // it; the rest was the calls that resolve to nothing and were thrown away one line later. A hint is drawn at
        // an argument, so "is any argument inside the requested range" is the cheapest sufficient test, and it is
        // available before any lookup.
        let Some(arguments) = arguments_of(&call) else {
            continue;
        };
        let asked_about = arguments
            .iter()
            .any(|argument| contains(range, argument.text_range().start().into()));
        if !asked_about {
            continue;
        }

        // **Where a hint's milliseconds go**, when asked. One line per call separates them, and a
        // repeated callee spelling is visible in it.
        // **Printed before the resolution, not only after it.** A call that never finishes never reaches the report
        // below — measured: `inlayHint` on `containers.cpp` sat in one call for sixteen seconds and a gigabyte, and the
        // last line the trace had printed was the call *before* it. This line is what names it.

        // **The parameters of the call's declaration** — a function's own, or, when the callee names a **class**, the
        // constructor's. `Widget(3, 4)` is a temporary of `Widget`: the class's scope holds fields, but the class also
        // *declares constructors*, and a constructor declares parameters. Reported from `EmmyLuaCodeStyle`:
        // `LuaParser(file, std::move(luaLexer.GetTokens()))` showed no hints at all — the constructor branch is what
        // answers there, and the function branch below is unchanged for every other call.
        // **The declaration's own parameters, then a callable object's `operator()`, then a lambda's own list.** The
        // order matters: a *free function* call resolves in the first branch, and only a callee that names no
        // function at all — a variable — reaches the others. The other order would read a function's **return** type
        // and hint its `operator()`'s parameters for a call the function itself declares.
        // **A callee that names no function is a variable, and there are two kinds**: an object of a class that
        // declares `operator()`, and a closure — which has no name for its type and is in no index, so its own
        // parameter list beside the declaration is the only evidence.
        let a_variables_call = || {
            callable_object_parameter_names(index, view, &call, arguments.len())
                .or_else(|| a_lambdas_parameter_names(view, &call, arguments.len()))
        };
        let names =
            constructor_parameter_names(index, view, &call, arguments.len()).or_else(|| {
                let Known::Yes(callee) = callee_of_a_call(
                    &crate::sema::declarations::ShapeTable::new(),
                    index,
                    &mut |_: &Path| None,
                    &view.scopes,
                    &view.root,
                    &view.path,
                    &call,
                ) else {
                    // No declaration, or none this layer can place: the parameter list is unknown, and a hint naming the
                    // wrong parameter is a wrong answer printed into the code.
                    return a_variables_call();
                };

                let names = parameter_names_for(view, &callee, &mut declaring, &mut view_of)
                    .or_else(|| {
                        // **A declaration whose file has no rendering still has names.** The list is in its
                        // *fact*, which the index keeps as text — so the reading is made from the text
                        // instead of from the tree.
                        callee
                            .parameters
                            .as_deref()
                            .and_then(parameter_names_from_text)
                    });

                names.or_else(a_variables_call)
            });
        let Some(names) = names else {
            continue;
        };

        // The arguments were read before the callee: they are what decides whether this call is worth resolving.
        for (argument, name) in arguments.iter().zip(names.iter()) {
            // A parameter with no name breaks the alignment for every parameter *after* it rather than shifting
            // them up: see `parameters_of`. Nothing is hinted for a position nothing declares.
            let Some(name) = name else {
                continue;
            };

            if !contains(range, argument.text_range().start().into()) {
                continue;
            }

            // The argument already spells the parameter, so the hint would repeat the line back at the reader.
            if argument.text().to_string().trim() == name {
                continue;
            }

            hints.push(ParameterHint {
                offset: argument.text_range().start().into(),
                name: name.clone(),
            });
        }
    }

    hints
}

/// **What a call through an *object* hands its arguments** — the parameter names of the class's `operator()`.
///
/// `std::views::filter(numbers, pred)` calls a function-like object: `filter` is a `std::ranges::views::_Filter`
/// variable, so [`callee_of_a_call`] finds a *variable* and there is no function declaration to read a parameter
/// list from — the call had no hints at all. Reported from an editor with the `views` adaptors, where every call is
/// this shape.
///
/// The type of the callee is the class, and the class declares `operator()`; its parameters are the answer, read from
/// the **fact** (`DeclFact::parameter_list`) so a header need not be rendered first. Only an **exact** arity match is
/// accepted: `_Filter` declares `operator()(_Range&&, _Pred&&)` and nothing else, so the one-argument closure form
/// `views::filter(pred)` — which really goes through the adaptor-closure base's pack — must keep its silence rather
/// than label the predicate `__r`.
fn callable_object_parameter_names(
    index: &ProjectIndex,
    view: &FileView,
    call: &CppSyntaxNode,
    passed: usize,
) -> Option<Vec<Option<String>>> {
    let callee = call.children().next()?;
    let Known::Yes((type_of, _)) = crate::index::project::type_of_expression(
        index,
        &mut |_: &Path| None,
        &view.scopes,
        &view.root,
        &view.path,
        &callee,
        0,
    ) else {
        return None;
    };
    // A *free function* has a type too — its return type — and naming it here would read a called object that is
    // not one. The callable shape is a class, which is what `class_name` says.
    let class = type_of.class_name()?;
    let Known::Yes(list) =
        crate::index::project::members_of(index, &view.scopes, &view.root, &view.path, class)
    else {
        return None;
    };
    let mut best: Option<Vec<Option<String>>> = None;
    for member in &list.members {
        if member.fact.name != "operator()" {
            continue;
        }
        let Some(text) = member.fact.parameter_list() else {
            continue;
        };
        let Some(names) = parameter_names_from_text(text) else {
            continue;
        };
        if names.len() != passed {
            continue;
        }
        if best.is_none() {
            best = Some(names);
        }
    }
    best
}

/// **What a call of a variable that holds a lambda hands its arguments** — the lambda's own parameter names.
///
/// ```cpp
/// auto by_value = [counter]() mutable { return ++counter; };
/// by_value();                                     // nothing is written after the name
/// auto initialised = [value = counter + 10]() -> int { return value; };
/// initialised();
/// ```
///
/// The callee names a *variable*, so there is no function declaration and no class with an `operator()` either — a
/// closure type is unnamed and is in no index. The lambda's parameter list is in the tree, in the declaration's own
/// initialiser, and it is the only answer: reported from an editor on a file of lambdas, where **every** call through
/// one of them was unhinted (`by_ref()`, `empty()`, `by_value()`, `template_lambda(4)`, `capture_pack(1, 2, 3)` …).
///
/// The arity has to match, and a **pack** takes any count from its position on: `[](auto... args)` is one parameter and
/// any number of arguments, which is what `capture_pack(1, 2, 3)` passes.
fn a_lambdas_parameter_names(
    view: &FileView,
    call: &CppSyntaxNode,
    passed: usize,
) -> Option<Vec<Option<String>>> {
    let callee = call.children().next()?;
    // A member call is its receiver's question (`w.f(…)`), and a qualified name is not a variable of this file.
    if crate::sema::check::member_access_above(&callee).is_some() {
        return None;
    }
    let at = usize::from(callee.text_range().start());
    let Known::Yes(binding) = crate::sema::resolve::definition_at(&view.scopes, &view.root, at)
    else {
        return None;
    };
    if binding.kind != crate::BindingKind::Variable {
        return None;
    }
    // **The declaration this name is in**, and the lambda its initialiser holds. The declarator is asked for by the
    // binding's own name range, so a declaration of two lambdas on one line answers for the right one.
    let declarator = crate::sema::types::declarator_declaring(&view.root, binding.name_range)?;
    let holder = declarator.parent()?;
    let lambda = cpp_parser::typed_descendants::<cpp_parser::CppLambdaExpr>(&holder).next()?;
    let list = lambda.get_params()?;
    let text = list.syntax().text().to_string();
    let names = parameter_names_from_text(&text)?;
    let a_pack = text.contains("...");
    if names.len() != passed && !(a_pack && names.len() < passed) {
        return None;
    }
    Some(names)
}

/// The parameter names of the declaration a call names, or `None` when they cannot be read.
/// **The parameter names a call of a class hands its arguments** — the constructor's, chosen by the argument count.
///
/// `Widget(3, 4)` names a type, and a type's scope holds its *fields*: reading that scope would label `3` with some
/// member's name, which is why this layer used to refuse a call of a class outright. The right answer is one step
/// further: the class declares **constructors**, each with its own parameters, and the one that can take this many
/// arguments is the one being called. `EmmyLuaCodeStyle`'s `LuaParser(file, std::move(luaLexer.GetTokens()))` is the
/// shape — two arguments and a constructor taking two parameters.
///
/// The list is read from the **fact** (`DeclFact::parameter_list`), because the class is usually declared in a header
/// and the hint must not wait for that header's rendering. When several constructors can take the arguments, the one
/// with the fewest parameters wins: `Widget(Source&, Tokens&)` beats `Widget(Source&, Tokens&, int extra = 0)` for a
/// two-argument call, and a constructor with defaults beyond the call's arity is not chosen over an exact one.
///
/// `None` for a class that writes no constructor at all — aggregate initialization names no parameters, and a field's
/// name is not one.
fn constructor_parameter_names(
    index: &ProjectIndex,
    view: &FileView,
    call: &CppSyntaxNode,
    passed: usize,
) -> Option<Vec<Option<String>>> {
    let callee = call.children().next()?;
    let written = callee.text().to_string();
    let written = written.trim();
    // A qualified spelling names the class by its last segment: `ns::Widget(1, 2)`.
    let name = written.rsplit("::").next().unwrap_or(written).trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
    {
        return None;
    }
    let Known::Yes(found) = index.definitions(name, &view.path) else {
        return None;
    };
    let mut best: Option<Vec<Option<String>>> = None;
    for declaration in &found.found {
        let fact = &declaration.fact;
        // A constructor is filed under its class's name, in the class's own scope.
        if fact.kind != crate::DeclKind::Function || fact.name != name {
            continue;
        }
        let Some(scope) = fact.scope.as_deref() else {
            continue;
        };
        if scope.rsplit("::").next().map(str::trim) != Some(name) {
            continue;
        }
        let Some(list) = fact.parameter_list() else {
            continue;
        };
        let Some(names) = parameter_names_from_text(list) else {
            continue;
        };
        if names.len() < passed {
            continue;
        }
        if best.as_ref().is_none_or(|known| names.len() < known.len()) {
            best = Some(names);
        }
    }
    best
}

fn parameter_names_for<F>(
    view: &FileView,
    callee: &Callee,
    declaring: &mut HashMap<PathBuf, FileView>,
    view_of: &mut F,
) -> Option<Vec<Option<String>>>
where
    F: FnMut(&Path) -> Option<FileView>,
{
    // The file being edited is already parsed, and a same-file call is the common case.
    let declared_in = if callee.file == view.path {
        view
    } else {
        if !declaring.contains_key(&callee.file) {
            declaring.insert(callee.file.clone(), view_of(&callee.file)?);
        }

        declaring.get(&callee.file).expect("just inserted")
    };

    let list = parameter_list_of(declared_in, callee.name_offset)?;
    let names: Vec<Option<String>> = parameters_of(&list)
        .into_iter()
        .map(|(_, declared)| declared.map(|(name, _)| name.text()))
        .collect();

    (!names.is_empty()).then_some(names)
}

/// Every call in the tree whose own start is inside `range`.
///
/// The **start** decides, not the whole extent: a call that begins above the visible area and ends inside it is
/// still the call the reader is looking at, and one that begins inside and ends below is drawn from its first
/// line. What matters is that the arguments — which is where the hints go — are inside, and each is checked
/// against the range on its own.
fn call_expressions(root: &CppSyntaxNode, range: SourceRange) -> Vec<CppSyntaxNode> {
    root.descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CallExpr)
        .filter(|node| contains(range, node.text_range().start().into()))
        .collect()
}

/// Is an offset inside a range?
fn contains(range: SourceRange, offset: usize) -> bool {
    offset >= range.start_offset && offset <= range.end_offset()
}

/// **A generic lambda's parameter type, for each `auto` parameter in `range`.**
///
/// `[](auto &&x) {}` is a template whose parameter has no type until a call gives it one, and this file holds no call
/// — the closure's type is decided at the call site the lambda is an **argument** of. `deduce` answers for one
/// parameter, and `None` is the ordinary answer: see
/// [`Session::a_generic_lambdas_parameter_type`](crate::Session::a_generic_lambdas_parameter_type) for the four rules
/// that decide when a call site can be read at all.
///
/// The hint goes where a parameter's type is written — after the name, inside the `(…)` — and only for a parameter
/// **written `auto`**: one with a type already says it. A parameter with no name (`[](auto) {}`) gets none, because
/// there is nothing to attach a label to.
pub fn lambda_parameter_hints<H>(
    view: &FileView,
    range: SourceRange,
    mut deduce: H,
) -> Vec<TypeHint>
where
    H: FnMut(&CppSyntaxNode) -> Option<String>,
{
    let mut hints = Vec::new();
    for node in view.root.descendants() {
        if CppSyntaxKind::from(node.kind()) != CppSyntaxKind::LambdaExpr
            || !contains(range, node.text_range().start().into())
        {
            continue;
        }
        let Some(list) = node.descendants().find_map(|child| {
            use cpp_parser::CppAstNode as _;
            cpp_parser::CppParamList::cast(child)
        }) else {
            continue;
        };
        for parameter in list.get_params() {
            if !parameter.is_auto() {
                continue;
            }
            // **The last identifier is the name**, and a parameter without one has no place for a label: the tokens
            // before it are the type (`auto`, `auto&&`), and a default value or a pack comes after.
            let Some(name) = cpp_parser::CppAstNode::syntax(&parameter)
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .filter(|token| {
                    cpp_parser::CppTokenKind::from(token.kind())
                        == cpp_parser::CppTokenKind::Identifier
                })
                .last()
            else {
                continue;
            };
            let at = usize::from(name.text_range().end());
            if !contains(range, at) {
                continue;
            }
            if let Some(label) = deduce(cpp_parser::CppAstNode::syntax(&parameter))
                && is_a_type_spelling(&label)
            {
                hints.push(TypeHint {
                    offset: at,
                    label,
                    kind: TypeHintKind::LambdaParameter,
                });
            }
        }
    }
    hints
}
