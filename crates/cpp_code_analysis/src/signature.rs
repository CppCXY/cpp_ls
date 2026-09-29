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

use crate::index::project::callees_of_a_call;
use crate::inlay::parameter_list_of;
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

    let Known::Yes(candidates) = callees_of_a_call(index, &view.scopes, &view.root, &view.path, &call)
    else {
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
    let mut parsed: std::collections::HashMap<PathBuf, Option<FileView>> = std::collections::HashMap::new();
    let mut signatures = Vec::new();

    for candidate in candidates {
        // The file being edited is already parsed; every other file is parsed once and kept for the rest of this
        // answer. A file that cannot be read — or a declaration whose declarator has no parameter list, which is
        // every class name and every variable — contributes no signature rather than an empty one.
        let declared_in: &FileView = if candidate.file == view.path {
            view
        } else {
            let held = parsed
                .entry(candidate.file.clone())
                .or_insert_with(|| view_of(&candidate.file));
            let Some(held) = held.as_ref() else {
                continue;
            };
            held
        };

        let Some(list) = parameter_list_of(declared_in, candidate.name_offset) else {
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

        signatures.push(CallSignature {
            label,
            parameters,
            active_parameter: active,
            declared_in: candidate.file.clone(),
            declared_at: candidate.name_offset,
            documentation: None,
        });
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

    signatures
}

/// The innermost call whose **arguments** contain `offset`.
///
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
    within(node.text_range().start().into(), node.text_range().end().into(), offset)
}

fn element_contains(element: &CppSyntaxElement, offset: usize) -> bool {
    within(element.text_range().start().into(), element.text_range().end().into(), offset)
}

fn within(start: usize, end: usize, offset: usize) -> bool {
    offset >= start && offset <= end
}
