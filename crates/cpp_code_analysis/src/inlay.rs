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
//! * **a lambda parameter** — `[](auto x)` is a template, and the type exists only at a call site (this layer holds one
//!   file and one tree);
//! * **a lambda's deduced return** — its type is unnamed and `deduced_returns_of` starts from a binding a lambda does
//!   not have;
//! * **a type written already** — `int n = 1;` needs no `int:` in front of it, and the hint would be noise.
//!
//! See `docs/buglist.md` for the two that remain.
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
use crate::sema::scopes::parameters_of;
use crate::{FileView, Known, ProjectIndex};

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
        let trace = crate::probes::inlay();
        let started = std::time::Instant::now();
        // **The parameters of the call's declaration** — a function's own, or, when the callee names a **class**, the
        // constructor's. `Widget(3, 4)` is a temporary of `Widget`: the class's scope holds fields, but the class also
        // *declares constructors*, and a constructor declares parameters. Reported from `EmmyLuaCodeStyle`:
        // `LuaParser(file, std::move(luaLexer.GetTokens()))` showed no hints at all — the constructor branch is what
        // answers there, and the function branch below is unchanged for every other call.
        let names =
            constructor_parameter_names(index, view, &call, arguments.len()).or_else(|| {
                let Known::Yes(callee) = callee_of_a_call(
                    index,
                    &mut |_: &Path| None,
                    &view.scopes,
                    &view.root,
                    &view.path,
                    &call,
                ) else {
                    // No declaration, or none this layer can place: the parameter list is unknown, and a hint naming the
                    // wrong parameter is a wrong answer printed into the code.
                    return None;
                };
                let resolved = started.elapsed();

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
                if trace {
                    eprintln!(
                        "[inlay] {} -> {:?}: callee {:?}, names {:?}, {} name(s)",
                        call.text()
                            .to_string()
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .chars()
                            .take(40)
                            .collect::<String>(),
                        callee.file.display(),
                        resolved,
                        started.elapsed() - resolved,
                        names.as_ref().map(Vec::len).unwrap_or(0)
                    );
                }
                names
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
        let Some(list) = fact.parameter_list.as_deref() else {
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

/// **The parameter names of a declaration read from its own text**, for a file with no rendering.
///
/// `parameter_list` is what the index stores — `(const char* __format, ...)`, parentheses included — so the names
/// are read by **parsing one synthetic declaration** around it: `void __hint(const char* __format, ...);`. The
/// tree then tells a name from a type, which is the one thing splitting on commas gets wrong: `int (*f)(int)`,
/// `const Widget& w`, a default argument containing a comma (`map<int, string> m = {}`), and `...` all come out
/// right because they are not text to this function.
///
/// `None` for a list with no parameter (a `()`), which is also the answer for a declaration whose fact was written
/// before the field existed.
pub(crate) fn parameter_names_from_text(parameter_list: &str) -> Option<Vec<Option<String>>> {
    let text = format!("void __hint{parameter_list};");
    let tree = cpp_parser::CppParser::parse(&text, cpp_parser::ParserConfig::default());
    let root = tree.get_red_root();

    let declarator = root
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declarator)?;
    let list = cpp_parser::first_child_of_kind(&declarator, &[CppSyntaxKind::ParameterList])?;

    let names: Vec<Option<String>> = parameters_of(&list)
        .into_iter()
        .map(|(_, declared)| declared.map(|(name, _)| name.text()))
        .collect();

    (!names.is_empty()).then_some(names)
}

/// The declaration's **own** parameter list: the one belonging to the declarator the named at `offset` is a name
/// of.
///
/// The innermost `Declarator` ancestor, and *its* `ParameterList` child — not the nearest parameter list up the
/// tree. The difference is the whole correctness of the reading: a variable declared inside a function body has
/// the enclosing function's parameter list above it, and a function returning a function pointer
/// (`void (*f(int a))(int b)`) has two in the same declaration. The declarator the name is a name *of* is the one
/// whose list says what the name's parameters are, and it is found the way the scope builder finds a declaration's
/// name (`sema::scopes::declared_name`) — the same relationship, read from the other end.
pub(crate) fn parameter_list_of(view: &FileView, offset: usize) -> Option<CppSyntaxNode> {
    let token = cpp_parser::token_at(&view.root, offset)?;

    let declarator = token
        .parent_ancestors()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declarator)?;

    cpp_parser::first_child_of_kind(&declarator, &[CppSyntaxKind::ParameterList])
}

/// The **text** of that list, as the file writes it — `(_Ty* _First, size_type _Count)`, parentheses included.
///
/// One function for the two readers that want the spelling rather than the nodes: a declaration's fact
/// ([`crate::sema::declarations::parameter_list_at`], which is what a completion shows beside a name) and anything
/// that has to print a signature without walking it.
pub(crate) fn parameter_list_text(declarator: &CppSyntaxNode) -> Option<String> {
    let list = cpp_parser::first_child_of_kind(declarator, &[CppSyntaxKind::ParameterList])?;

    Some(tidy_list_text(&list.text().to_string()).0)
}

/// **A list as one line, and where every byte of the original went.**
///
/// # The layout is taken out, and the spelling is not
///
/// A node's text is the *source* text, which is laid out for a reader of the file rather than for a one-line
/// summary: measured on MSVC's `<istream>`, `basic_istream<_Elem, _Traits>&& _Istr` arrives with a `\r\n` and four
/// spaces of indentation in the middle, and MSVC's `<xstring>` wraps three of `basic_string::append`'s nine
/// overloads the same way. A **cooked** reading arrives with the rendering's own separator instead — one space
/// between every token, so `format_string<_Types...>` is spelled `format_string < _Types ... >`.
///
/// So whitespace runs become one space, and the space inside the brackets this list is made of goes: after `(` and
/// before `)` and `,`. Those three are safe to decide lexically — a parameter list's own parentheses and commas are
/// never comparison operators — while `<`, `>` and `...` are deliberately left alone, because `(bool _B = 1 < 2)`
/// is a parameter list too and a rule that pulled `1 <2` together would be inventing a different expression.
///
/// What is *not* done is the rest of the rendering's spacing (`_Elem * ()`, `_NODISCARD _CONSTEXPR20 size_type`):
/// that is how every spelling in a fact is written, and a parameter list that read differently from the return type
/// beside it would be two conventions in one line.
///
/// # Why the map comes back with it
///
/// Because the other reader is a **signature**, whose parameters are byte spans *inside* the label it builds
/// ([`crate::signature::CallSignature::parameters`]) — a client bolds them. Tidying the text without moving the
/// spans would put the highlight on the wrong bytes, which is worse than the newline it removed. `map[i]` is where
/// byte `i` of `text` ended up, with one entry per byte plus one for the end, so a span `a..b` becomes
/// `map[a]..map[b]` whatever the two ends are.
pub(crate) fn tidy_list_text(text: &str) -> (String, Vec<usize>) {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut map = vec![0usize; text.len() + 1];
    let mut wrote_a_word = false;
    let mut at = 0usize;

    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            // Whitespace lands where the next word will start, before the separator is decided: no span begins or
            // ends in it — a span is a whole parameter, and each is trimmed.
            map[at] = out.len();
            at += 1;
            continue;
        }

        let mut end = at;
        while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
            end += 1;
        }
        let word = &text[at..end];

        if wrote_a_word {
            let after_an_open = out.ends_with('(') || out.ends_with('[');
            let before_a_close =
                word.starts_with(')') || word.starts_with(']') || word.starts_with(',');
            if !after_an_open && !before_a_close {
                out.push(' ');
            }
        }

        for (offset, slot) in map[at..end].iter_mut().enumerate() {
            *slot = out.len() + offset;
        }
        out.push_str(word);
        wrote_a_word = true;
        at = end;
    }

    map[text.len()] = out.len();
    (out, map)
}

/// The argument expressions of a call, in order.
///
/// The call's children **after the callee**, which is the shape the grammar keeps for every call it can read —
/// `f(1)`, `ns::g(1)`, `f<int>(2)`, `(f)(3)`, `f(4)(5)`, `W{}.go(6)`: the template arguments and the qualifier are
/// inside the callee's own node, so the arguments are siblings of it and the commas between them are tokens rather
/// than children.
///
/// `None` for the one shape that is not a list of expressions: a call whose arguments could not be read — a
/// macro's arguments, which the parser keeps as a balanced token group in an `ArgumentList` child. There is no
/// expression there to put a name against, and a hint on a raw token would be a guess about where an argument
/// begins.
pub(crate) fn arguments_of(call: &CppSyntaxNode) -> Option<Vec<CppSyntaxNode>> {
    let mut children = call.children();
    children.next()?; // the callee

    let arguments: Vec<CppSyntaxNode> = children.collect();

    let unread = arguments
        .iter()
        .any(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ArgumentList);

    (!arguments.is_empty() && !unread).then_some(arguments)
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
