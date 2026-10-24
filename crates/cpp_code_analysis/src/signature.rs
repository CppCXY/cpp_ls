//! Call signatures: **which function is being called, and which of its parameters the cursor is in**.
//!
//! ```text
//! make(|)              →  make(int count, double factor)     the first parameter is active
//! make(1, |)           →  make(int count, double factor)     the second
//! ```
//!
//! # What this is, and what it is not
//!
//! It is the *declaration's own text*: the callee's spelling and the parameter list it wrote, with each
//! parameter's span inside that label so that a client can bold the one the cursor is in.
//!
//! # One call, several declarations — and that is the answer, not a guess
//!
//! A call to an overloaded function has more than one declaration, and the protocol's `SignatureHelp.signatures`
//! is a **list** for exactly that reason: the reader picks, and a client lets them cycle. This module used to
//! answer one signature and refuse when the name had several — which meant that a call to `std::format` (four
//! overloads) answered **nothing at all**: not a wrong signature, an empty popup, on the most ordinary call in
//! modern C++. Sending every declaration the name has claims nothing about which one will be chosen; the
//! alternative was not a better answer, it was no answer.
//!
//! The order is the index's, which is file order and then declaration order — the order the declarations are
//! written in the headers.
//!
//! Types are the **written** spellings, as everywhere else in this crate: `const Widget&` is those three tokens,
//! and an alias is its own name.
//!
//! [`Ambiguous`]: crate::UnknownReason::Ambiguous

use std::ops::Range;
use std::path::{Path, PathBuf};

use cpp_parser::{CppDocComment, CppSyntaxElement, CppSyntaxKind, CppSyntaxNode, CppTokenKind};

use crate::index::project::{Callee, callees_of_a_call};
use crate::inlay::{arguments_of, parameter_list_of};
use crate::sema::scopes::parameters_of;
use crate::{FileView, Known, ProjectIndex};

/// The call being typed: the signature, where each parameter sits in it, and which one the cursor is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSignature {
    /// The signature as written: the callee's spelling followed by its parameter list's own text.
    pub label: String,
    /// Each parameter's span **inside the label**, with the parameter's text — what a client underlines or bolds.
    pub parameters: Vec<(Range<usize>, String)>,
    /// The parameter the cursor is in.
    pub active_parameter: Option<usize>,
    /// The file the declaration is in, and the offset of its own **name** — where a caller that wants to say more
    /// (the documentation above it, for one) has to look.
    pub declared_in: PathBuf,
    pub declared_at: usize,
    /// The comment the declaration is documented by, when the file writes one.
    ///
    /// Left `None` here, because reading it needs a session: [`crate::Session::signatures_at`] fills it in, and that
    /// is the layer that can reach another file's text.
    pub documentation: Option<CppDocComment>,
}

/// **The signature of the call the cursor at `offset` is inside**, when this analysis can name the callee.
///
/// `None` for every position that is not inside a call's arguments — including a cursor *on the callee*
/// (`|make(1)` is a question about `make`, not about the call), and including a callee this layer cannot resolve.
///
/// `view_of` is how a callee declared in another file is read: the file being edited is already parsed and is
/// never asked for, and a header is parsed once for the answer (see [`crate::inlay::parameter_hints`], which has
/// the same shape and the same cost).
/// **The signatures of the call the cursor at `offset` is inside** — one per declaration the callee names.
///
/// Empty for every position that is not inside a call's arguments — including a cursor *on the callee*
/// (`|make(1)` is a question about `make`, not about the call), and including a callee this layer cannot resolve.
///
/// # Why a list, and what it is a list of
///
/// A call to an overloaded function has several answers, and the protocol's `SignatureHelp.signatures` is a list for
/// exactly that reason. This used to answer **one** signature and refuse when the name had more than one
/// declaration — `std::format(` therefore answered nothing at all, four overloads being four declarations. Sending
/// all of them claims nothing: it is the reader who picks, and the alternative was not "one answer" but no answer.
///
/// The order is the index's ([`crate::ProjectIndex::definitions`]), which is file order and then declaration order —
/// the order the declarations are written in the headers, which is the order a compiler's own popup shows.
///
/// `view_of` is how a callee declared in another file is read: the file being edited is already parsed and is never
/// asked for, and **each other file is parsed at most once for one answer**, however many overloads it holds —
/// `<format>` declares four `format`s in one header, and four parses for one popup would be three too many.
pub fn signatures_at<F>(
    index: &ProjectIndex,
    view: &FileView,
    offset: usize,
    mut view_of: F,
) -> Vec<CallSignature>
where
    F: FnMut(&Path) -> Option<FileView>,
{
    let Some(call) = call_around(&view.root, offset) else {
        return Vec::new();
    };
    let Some(callee) = call.children().next() else {
        return Vec::new();
    };

    let Known::Yes(candidates) = callees_of_a_call(
        index,
        &mut view_of,
        &view.scopes,
        &view.root,
        &view.path,
        &call,
    ) else {
        return Vec::new();
    };

    // **The callee's spelling, not the whole call**: a reader who wrote `w.scaled(` already knows the object, and
    // the popup is about which function and which parameter. A `::`-qualified name is kept whole, because there the
    // qualifier is part of the name.
    let callee_text = callee.text().to_string();
    let callee_text = callee_text.trim();
    let member = callee_text
        .rfind("->")
        .map(|at| at + "->".len())
        .into_iter()
        .chain(callee_text.rfind('.').map(|at| at + 1))
        .max();
    let spelling = member.map_or(callee_text, |at| &callee_text[at..]);

    let active = active_parameter(&call, offset);
    let mut parsed: std::collections::HashMap<PathBuf, Option<FileView>> =
        std::collections::HashMap::new();
    let mut signatures = Vec::new();
    // **The parameter spellings the ranking should see**, aligned with `signatures`. For a plain function they are the
    // declaration's own; for a template they are **substituted** once the call's argument types are known, which is
    // what makes `f(T)` and `f(int)` the same candidate for `f(1)`.
    let mut rankable: Vec<(Vec<String>, Vec<String>)> = Vec::new();

    for candidate in candidates {
        // The file being edited is already parsed; every other file is parsed once and kept for the rest of this
        // answer. A file that cannot be read contributes no signature rather than an empty one.
        let declared_in: &FileView = if candidate.file == view.path {
            view
        } else {
            let held = parsed
                .entry(candidate.file.clone())
                .or_insert_with(|| view_of(&candidate.file));
            let Some(held) = held.as_ref() else {
                // **The declaration's *rendering* is not built yet — and the popup is not allowed to wait for it.**
                // A rendering is a unit walk, a preprocess and a render (65–190 ms for a header), so a query queues
                // one and answers `None`; the next request would find it. For a signature popup "the next request"
                // never comes — a client asks once per `(` — so the answer here is built from what the **index**
                // already holds: the declaration's parameter list, which every fact carries.
                //
                // Measured on a C++23 module project: `math::add(` answered **nothing** on the first `(` and
                // everything on the second, which is exactly the shape of "signature help almost never appears".
                //
                // What the fallback gives up is the parameter **spans** (a client bolds the active one): they are
                // offsets into a list read from a tree, and there is no tree here. The label, the count and the
                // active parameter are all known, so the popup is useful rather than absent — and the detail is
                // right, unlike parsing the file's own text at a *rendering* offset, which would name the wrong
                // declaration in any file with a macro above it.
                if let Some(parameters) = &candidate.parameters {
                    let (tidy, _) = crate::inlay::tidy_list_text(parameters);
                    signatures.push(CallSignature {
                        label: format!("{spelling}{tidy}"),
                        parameters: Vec::new(),
                        active_parameter: active,
                        declared_in: candidate.file.clone(),
                        declared_at: candidate.name_offset,
                        documentation: None,
                    });
                    rankable.push((Vec::new(), Vec::new()));
                }
                continue;
            };
            held
        };

        // **What this candidate contributes signatures from.** A declaration with a parameter list is its own
        // answer; a **class name** is a constructor call — `Widget(` — and a class has no parameter list, so its
        // signatures are its **constructors'**, which the index already holds as members named after the class.
        //
        // The fallback used to be `continue`, and that is why a constructor offered nothing at all: the name in a
        // constructor call resolves to the class, `parameter_list_of` answered `None`, and the popup was empty on
        // the most ordinary line of code there is. See `tests/signature.rs::a_constructor_call_offers_its_parameters`.

        let sources: Vec<Callee> = match parameter_list_of(declared_in, candidate.name_offset) {
            Some(_) => vec![candidate.clone()],
            None => constructors_of(declared_in, candidate.name_offset),
        };

        // **A declaration whose list the tree does not have, in the file being edited.** A qualified definition —
        // `void CodeFormat::SetupStyle(LuaStyle &style, …)` — writes its name in the specifier sequence, so
        // `parameter_list_of` finds no declarator at the fact's name offset and the candidate drops out **silently**:
        // measured on a real project, `SetupStyle(style, options);` answered no signature while the fact beside it
        // carried `(LuaStyle &style, const FormattingOptions &options)`. The fact is the same fallback the other-file
        // branch uses, so the label, the count and the active parameter are right even without the parameter spans.
        if sources.is_empty()
            && let Some(parameters) = &candidate.parameters
        {
            let (tidy, _) = crate::inlay::tidy_list_text(parameters);
            signatures.push(CallSignature {
                label: format!("{spelling}{tidy}"),
                parameters: Vec::new(),
                active_parameter: active,
                declared_in: candidate.file.clone(),
                declared_at: candidate.name_offset,
                documentation: None,
            });
            rankable.push((Vec::new(), Vec::new()));
            continue;
        }

        for source in sources {
            let Some(list) = parameter_list_of(declared_in, source.name_offset) else {
                continue;
            };

            // **The label is one line, and the spans follow it.** The declaration's list is laid out for a reader of the
            // file (MSVC wraps three of `basic_string::append`'s nine overloads), and a popup label with a newline in it
            // is a popup with a broken signature — so the same tidying the fact's `parameter_list` gets is applied here,
            // *with its byte map*, because the spans a client bolds are offsets into this label.
            let list_text = list.text().to_string();
            let list_text = list_text.trim_end();
            let list_begin = usize::from(list.text_range().start());
            let raw: Vec<(Range<usize>, String)> = parameters_of(&list)
                .into_iter()
                .map(|(parameter, _)| {
                    let text = parameter.text().to_string();
                    let text = text.trim_end();
                    let start = usize::from(parameter.text_range().start()) - list_begin;
                    (start..start + text.len(), text.to_string())
                })
                .collect();

            let (tidy, moved) = crate::inlay::tidy_list_text(list_text);
            let list_start = spelling.len();
            let label = format!("{spelling}{tidy}");
            let parameters: Vec<(Range<usize>, String)> = raw
                .into_iter()
                .map(|(range, text)| {
                    (
                        list_start + moved[range.start]..list_start + moved[range.end],
                        text,
                    )
                })
                .collect();

            // The declaration's own parameter texts, names dropped, and the template header it owns — the last read
            // from the text before the declaration, which is a heuristic and is stated as one below.
            let plain: Vec<String> = parameters
                .iter()
                .map(|(_, text)| crate::sema::overload::a_type_without_its_name(text))
                .collect();
            let templates = crate::sema::deduction::template_parameters_before(
                &declared_in.source,
                source.name_offset,
            );

            signatures.push(CallSignature {
                label,
                parameters,
                active_parameter: active,
                declared_in: source.file.clone(),
                declared_at: source.name_offset,
                documentation: None,
            });
            rankable.push((plain, templates));
        }
    }

    // **One row per signature**, which is the same rule the member and name lists apply and for the same reason: a
    // list is something a reader *picks* from, and two identical labels are one choice offered twice. It is not
    // hypothetical — a declaration the **raw** reading and the **cooked** reading both found arrives as two
    // candidates with the same name in the same file, and MSVC's headers declare many members twice on purpose
    // (`push_back` for an lvalue and for an rvalue prints two different signatures, which is right; the same
    // declaration found by two readings prints one, twice).
    //
    // The first of a pair is kept, which is the declaration the index's own order reached first.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    signatures.retain(|signature| seen.insert(signature.label.clone()));

    // **Which of these the call's own arguments select.**
    //
    // The list is kept — a client shows them stacked and the reader cycles — but the one the arguments select goes
    // **first**, so a client whose `activeSignature` defaults to the first shows the language's answer rather than
    // whichever overload the index happened to hold first. The pieces are all here already: the call, the candidates,
    // and each candidate's parameter spellings; what the ranking needs is the arguments' types, which is the one thing
    // a `SemanticModel` is for.
    if signatures.len() > 1 {
        let model = crate::sema::model::SemanticModel::new(
            index,
            &view.root,
            &view.scopes,
            &view.source,
            &view.path,
            Box::new(|wanted: &Path| view_of(wanted)),
        );
        // Every node of this file's own text, traversed once: `inlay::arguments_of` is the crate's answer to "where are
        // this call's arguments", and the `SemanticModel` is what types them.
        let arguments = arguments_of(&call).unwrap_or_default();
        let types: Vec<crate::Known<String>> = arguments
            .iter()
            .map(|argument| match model.type_of(argument) {
                crate::Known::Yes((a_type, _)) => crate::Known::Yes(a_type.to_string()),
                crate::Known::No => crate::Known::No,
                crate::Known::Unknown(reason) => crate::Known::Unknown(reason),
            })
            .collect();
        // **Deduce, then substitute, then rank.** A template candidate whose arguments the call fixes is ranked as the
        // function it instantiates; one whose arguments the call does not fix keeps its declared spellings, and the
        // resolver reads those as `Match::Unranked` — which is the quiet direction, and the right one: a template this
        // analysis cannot deduce is a template whose ranking it does not know.
        let ranked: Vec<crate::sema::overload::Candidate> = signatures
            .iter()
            .enumerate()
            .map(|(at, signature)| {
                let (plain, templates) = rankable.get(at).cloned().unwrap_or_default();
                let plain_for_ordering = plain.clone();
                let parameters = match crate::sema::deduction::deduce(&templates, &plain, &types) {
                    Some(arguments) => plain
                        .iter()
                        .map(|spelling| {
                            crate::sema::types::substituted_spelling(
                                spelling, &templates, &arguments,
                            )
                        })
                        .collect(),
                    None => plain,
                };
                let candidate = crate::sema::overload::Candidate::new(
                    parameters,
                    signature.label.trim_end().ends_with("...)"),
                    signature.label.clone(),
                );
                candidate.with_declaration(plain_for_ordering, templates.clone())
            })
            .collect();
        if let crate::sema::overload::Choice::One(chosen) =
            crate::sema::overload::choose(&types, &ranked)
        {
            let chosen = signatures.remove(chosen);
            signatures.insert(0, chosen);
        }
    }
    signatures
}

/// **The constructors of the class a call is constructing** — every member named after the class.
///
/// A constructor call writes a **class name**: `Widget(` is not a function call, and the class has no parameter list
/// to render, which is why a constructor offered no signature at all before this existed. The constructors are in the
/// index as the class's own members — a member named after its class is a constructor, and one whose name begins
/// `operator` is a conversion — so the answer is a fact the cook already produced rather than a second parse of the
/// declaring file.
///
/// **The constructors of the class a call is constructing** — the members named after the class, read from the
/// declaration's **own file**, which this answer already has open.
///
/// A constructor call writes a **class name**: `Widget(` is not a function call, and a class has no parameter list to
/// render — which is why a constructor offered no signature at all before this existed, and why the fix is to answer
/// with the constructors' lists instead.
///
/// The class is found **by its name's offset**, which is what the caller already resolved: the declaration the call's
/// name pointed at is the class, so its node is the one whose `get_name` starts there. Everything else follows from
/// the typed layer — a constructor is a declaration inside the class body whose own name is the class's, and its
/// parameter list is the one `parameter_list_of` already knows how to find.
///
/// Asking the **index** for the member list was the first version of this, and it answered `Yes` with **zero
/// members** for a class whose definition was three lines up in the same file — the same empty-list shape that made
/// `assign_type_check` refuse to judge a class it could not see. Where the declaration is open, reading it is both
/// simpler and more certain.
fn constructors_of(declared_in: &FileView, name_offset: usize) -> Vec<Callee> {
    use cpp_parser::{CppAstNode, CppAstToken, CppClassDef, CppDeclaration, CppKind, CppTokenKind};

    let Some(class) = declared_in
        .root
        .descendants()
        .filter_map(CppClassDef::cast)
        .find(|class| {
            class
                .get_name()
                .is_some_and(|name| usize::from(name.syntax().text_range().start()) == name_offset)
        })
    else {
        return Vec::new();
    };
    let class_name = class.get_name_text().unwrap_or_default().trim().to_string();
    if class_name.is_empty() {
        return Vec::new();
    }

    let mut constructors = Vec::new();
    for declaration in class.descendants::<CppDeclaration>() {
        // The declaration's own name: the **first identifier** it writes, which is the class's for a constructor
        // (`Widget(int size);`) and something else for every other member (`int size;`, `void grow(int);`).
        let Some(name) = declaration
            .syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .find(|token| {
                CppTokenKind::from(CppKind::from(token.kind())) == CppTokenKind::Identifier
            })
        else {
            continue;
        };
        if name.text().to_string() != class_name {
            continue;
        }
        constructors.push(Callee {
            file: declared_in.path.clone(),
            name_offset: usize::from(name.text_range().start()),
            // The caller has this file's tree in hand — that is the whole reason this function can find the
            // constructors at all — so the fallback copy is never the one used.
            parameters: None,
        });
    }

    constructors
}

/// The innermost call whose **arguments** contain `offset`.
/// The callee's own span does not count: a cursor on `make` in `make(|)` is asking about the name, and a popup
/// there would answer a question the user did not ask. Everything from the end of the callee to the closing
/// parenthesis is the argument list — including the offset just past a comma, which is where a cursor sits while
/// the *next* argument is being typed.
fn call_around(root: &CppSyntaxNode, offset: usize) -> Option<CppSyntaxNode> {
    let mut found = None;
    let mut node = root.clone();

    loop {
        if !node_contains(&node, offset) {
            return found;
        }

        if CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CallExpr {
            let arguments_begin = node
                .children()
                .next()
                .map(|callee| usize::from(callee.text_range().end()));

            if arguments_begin.is_some_and(|start| offset >= start) {
                found = Some(node.clone());
            }
        }

        match node
            .children_with_tokens()
            .find(|element| element_contains(element, offset))
            .and_then(|element| element.into_node())
        {
            Some(child) => node = child,
            None => return found,
        }
    }
}

/// Which parameter the cursor is in.
///
/// **Counted in commas, at this call's own level.** The separators a call has are its own direct `,` tokens — a
/// nested call's commas belong to the nested node, so `outer(inner(a, b), |)` counts one separator before the
/// cursor rather than two — and that count is exactly the index of the parameter being typed:
///
/// ```text
/// make(|)          0 commas  → the first parameter    (the state every call begins in)
/// make(1|)         0          → the first, still
/// make(1, |)       1          → the second
/// make(1, 2, |)    2          → past a two-parameter list, which is what a variadic call really is
/// ```
///
/// Counting separators rather than arguments is deliberate: an argument list is malformed *while it is being
/// typed*, which is exactly when a signature is wanted, and `make(1, |)` has one argument and two positions.
///
/// # The two levels a separator can be at
///
/// A list the parser could read is the call's own children, with the commas as its own tokens. One it could
/// **not** read — a trailing comma with nothing after it is the common case, and a macro's arguments are the
/// other — arrives as a single balanced group, and then the commas are that group's own tokens. Both are the
/// argument list's own level, and a nested call's separators belong to the nested node either way: that is what
/// makes the count the index of the parameter being typed.
fn active_parameter(call: &CppSyntaxNode, offset: usize) -> Option<usize> {
    let callee_range = call.children().next().map(|callee| {
        (
            usize::from(callee.text_range().start()),
            usize::from(callee.text_range().end()),
        )
    });
    let mut commas = 0usize;

    fn scan(
        node: &CppSyntaxNode,
        callee_range: Option<(usize, usize)>,
        offset: usize,
        commas: &mut usize,
    ) {
        for element in node.children_with_tokens() {
            if let Some(token) = element.as_token() {
                let start = usize::from(element.text_range().start());
                if CppTokenKind::from(token.kind()) == CppTokenKind::Comma && start < offset {
                    *commas += 1;
                }
                continue;
            }

            let Some(child) = element.into_node() else {
                continue;
            };
            // The callee is not an argument and never a separator.
            let range = (
                usize::from(child.text_range().start()),
                usize::from(child.text_range().end()),
            );
            if Some(range) == callee_range {
                continue;
            }
            // **One level in**, for the group that *is* the argument list: an unread list keeps its separators
            // here rather than as the call's own tokens.
            if CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ArgumentList {
                scan(&child, None, offset, commas);
            }
        }
    }

    scan(call, callee_range, offset, &mut commas);

    Some(commas)
}

/// Is an offset inside a node's span, both ends included?
///
/// The ends are read out here rather than taken as a range because the range type is rowan's, and this crate does
/// not depend on rowan — the same reason `cpp_parser::token_at` exists.
fn node_contains(node: &CppSyntaxNode, offset: usize) -> bool {
    within(
        node.text_range().start().into(),
        node.text_range().end().into(),
        offset,
    )
}

fn element_contains(element: &CppSyntaxElement, offset: usize) -> bool {
    within(
        element.text_range().start().into(),
        element.text_range().end().into(),
        offset,
    )
}

fn within(start: usize, end: usize, offset: usize) -> bool {
    offset >= start && offset <= end
}
