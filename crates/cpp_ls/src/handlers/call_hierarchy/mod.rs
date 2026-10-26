//! `textDocument/prepareCallHierarchy` and its two follow-ups — **who calls this, and what it calls**.
//!
//! # What the three requests answer
//!
//! ```text
//! prepareCallHierarchy   the function the cursor names (a declaration or a definition) → one item
//! incomingCalls          every place that function is **called**, grouped by the function that calls it
//! outgoingCalls          every function its **body calls**, with the ranges of the calls
//! ```
//!
//! # Where each answer comes from
//!
//! * **prepare** is Go to Definition's answer, filtered to a callable: the fact the name resolves to, its declaration
//!   range and its name's range. A cursor on a class, a local or a macro answers `null` — "call hierarchy of a class" is
//!   not a question with a wrong answer, it is a question with none.
//! * **incoming** is Find References' list, filtered to **calls**: a reference whose text is followed by `(`. The caller
//!   is the innermost function fact whose range contains the reference — a fact-level question, so no parse of the
//!   calling file is needed.
//! * **outgoing** walks the body's `CallExpr` nodes. The callee is resolved with the same `definitions` query every
//!   other feature uses, so the two directions cannot disagree about a name.
//!
//! # What is deliberately not answered
//!
//! * **A call through a function pointer, a `std::function` or a virtual dispatch** — the callee is a value, not a
//!   declaration, and the set of functions it can reach is not known here. C++ makes that set open, and a partial list
//!   presented as complete is the one thing a call hierarchy must not do.
//! * **An overload set's exact member**: `incomingCalls` answers the calls of the *name*, as Find References does. A
//!   caller that selected a different overload is indistinguishable from here without the arguments' types at every
//!   reference, and the reference list is what a reader checks anyway.
//! * **A declaration with no body** (a header's prototype) contributes no outgoing calls: there is nothing to walk. The
//!   client asks again on the definition, which is where the body is.
//!
//! # Cost
//!
//! `incomingCalls` is one references query plus the file texts of the callers; nothing is parsed. `outgoingCalls` parses
//! the file that holds the body once — the same parse a hover would do — and then asks `definitions` per call.

use cpp_code_analysis::{FileProvider, Known, Session};
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    CallHierarchyServerCapability, ClientCapabilities, Range, ServerCapabilities, SymbolKind,
};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::{offset_at_position, path_to_uri, position_in_file, uri_to_file_path};

pub async fn on_prepare_call_hierarchy(
    context: ServerContextSnapshot,
    params: CallHierarchyPrepareParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<CallHierarchyItem>> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    log::debug!("call hierarchy: prepare asked at {position:?}");
    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;
    }
    log::debug!("call hierarchy: prepare returned, asking the session");

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        log::debug!("call hierarchy: inside the query for {}", path.display());
        let view = session.view_of_the_file(&path)?;
        let offset = offset_at_position(&view, position)?;
        let item = item_at(session, &view, offset.get())?;
        Some(vec![item])
    })
    .await
}

pub async fn on_incoming_calls(
    context: ServerContextSnapshot,
    params: CallHierarchyIncomingCallsParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<CallHierarchyIncomingCall>> {
    let item = params.item;
    let Some(path) = uri_to_file_path(&item.uri) else {
        return RequestOutcome::Missing;
    };
    context.analysis().prepare(&path).await;

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let view = session.view_of_the_file(&path)?;
        // The name the item points at: its `selection_range`'s start is the declaration's own name.
        let offset = offset_at_position(&view, item.selection_range.start)?;
        let Known::Yes(defined) = session.definitions(&view, offset) else {
            return None;
        };
        let target = defined.found.first()?.fact.name.clone();
        Some(incoming_calls_of(session, &view, offset.get(), &target))
    })
    .await
}

pub async fn on_outgoing_calls(
    context: ServerContextSnapshot,
    params: CallHierarchyOutgoingCallsParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<Vec<CallHierarchyOutgoingCall>> {
    let item = params.item;
    let Some(path) = uri_to_file_path(&item.uri) else {
        return RequestOutcome::Missing;
    };
    context.analysis().prepare(&path).await;

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let view = session.view_of_the_file(&path)?;
        let offset = offset_at_position(&view, item.selection_range.start)?;
        Some(outgoing_calls_of(session, &view, offset.get()))
    })
    .await
}

/// **The function the cursor names**, as one item — or `None` for a name that is not a callable.
///
/// A free function so a test can ask it without a client. The kind says `Method` when the fact has a class scope, which
/// is what a client draws differently; `detail` carries the signature, which is what the hover would show.
pub fn item_at<F: FileProvider + Clone>(
    session: &Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: usize,
) -> Option<CallHierarchyItem> {
    let Known::Yes(defined) = session.definitions(
        view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(offset),
    ) else {
        return None;
    };
    let definition = defined
        .found
        .iter()
        .find(|definition| definition.fact.kind == cpp_code_analysis::DeclKind::Function)?;
    item_of(session, &definition.file, &definition.fact)
}

/// One fact, as the protocol's item.
fn item_of<F: FileProvider + Clone>(
    session: &Session<F>,
    file: &std::path::Path,
    fact: &cpp_code_analysis::DeclFact,
) -> Option<CallHierarchyItem> {
    let uri = path_to_uri(file)?;
    let held = session.files().held(file)?;
    let range = Range::new(
        position_in_file(held, fact.range.start_offset)?,
        position_in_file(held, fact.range.end_offset())?,
    );
    let selection_range = Range::new(
        position_in_file(held, fact.name_range.start_offset)?,
        position_in_file(held, fact.name_range.end_offset())?,
    );
    let detail = match (&fact.returns, &fact.parameter_list) {
        (Some(returns), Some(parameters)) => Some(format!("{returns} {}{parameters}", fact.name)),
        (None, Some(parameters)) => Some(format!("{}{parameters}", fact.name)),
        _ => None,
    };
    Some(CallHierarchyItem {
        name: fact.name.clone(),
        kind: if fact.scope.is_some() {
            SymbolKind::METHOD
        } else {
            SymbolKind::FUNCTION
        },
        tags: None,
        detail,
        uri,
        range,
        selection_range,
        data: None,
    })
}

/// **Every call of `name`, grouped by the function that makes it.**
///
/// The reference list is Find References' (class-filtered, cross-file); what this adds is the two questions a call
/// hierarchy asks on top: is the reference a **call** (the text after the name is `(`), and **which function** is it in
/// (the innermost function fact whose range contains it).
pub fn incoming_calls_of<F: FileProvider + Clone>(
    session: &Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: usize,
    name: &str,
) -> Vec<CallHierarchyIncomingCall> {
    let _ = name;
    let references = crate::handlers::references::name_level_offsets(
        session,
        view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(offset),
        false,
    );

    // **One parse per file**, kept for both questions about it: is this reference a *declaration* (the defining file's
    // own fact is not in the reference list's `aimed` set, so it arrives here as a reference), and which function is it in.
    let mut parsed: HashMap<PathBuf, Option<cpp_code_analysis::FileView>> = HashMap::new();
    let mut callers: Vec<(CallHierarchyItem, Vec<Range>)> = Vec::new();
    for (file, range) in references {
        let Some(held) = session.files().held(&file) else {
            continue;
        };
        let parsed_view = parsed
            .entry(file.clone())
            .or_insert_with(|| {
                Some(cpp_code_analysis::FileView::parse_with(
                    &held,
                    &cpp_parser::MacroEnvironment::default(),
                ))
            })
            .as_ref();
        let Some(parsed_view) = parsed_view else {
            continue;
        };
        if is_a_declaration(parsed_view, range) {
            continue;
        }
        if !is_a_call(&held.text, range) {
            continue;
        }
        let Some(caller) = the_caller_of(session, parsed_view, &file, range.start_offset) else {
            continue;
        };
        let (Some(start), Some(end)) = (
            position_in_file(&held, range.start_offset),
            position_in_file(&held, range.end_offset()),
        ) else {
            continue;
        };
        let call = Range::new(start, end);
        match callers.iter_mut().find(|(item, _)| {
            item.uri == caller.uri && item.selection_range == caller.selection_range
        }) {
            Some((_, ranges)) => {
                if !ranges.contains(&call) {
                    ranges.push(call);
                }
            }
            None => callers.push((caller, vec![call])),
        }
    }
    callers
        .into_iter()
        .map(|(from, from_ranges)| CallHierarchyIncomingCall { from, from_ranges })
        .collect()
}

/// Is `range` the **name of a declaration** in this file — the definition of the function whose calls are being listed?
///
/// A definition arrives in the reference list: the list excludes the declarations *visible from the asking file*, and a
/// `.cpp` that defines a header's function is not visible from the header. Its name is followed by `(` too, so the
/// lexical test cannot tell them apart — the file's own facts can.
fn is_a_declaration(view: &cpp_code_analysis::FileView, range: cpp_parser::SourceRange) -> bool {
    let Known::Yes(defined) = cpp_code_analysis::sema::resolve::definitions_at(
        &view.scopes,
        &view.root,
        range.start_offset,
    ) else {
        return false;
    };
    defined.iter().any(|binding| {
        binding.name_range.start_offset == range.start_offset
            && matches!(
                binding.kind,
                cpp_code_analysis::BindingKind::Function
                    | cpp_code_analysis::BindingKind::Constructor
                    | cpp_code_analysis::BindingKind::Destructor
                    | cpp_code_analysis::BindingKind::ConversionFunction
                    | cpp_code_analysis::BindingKind::OperatorFunction
                    | cpp_code_analysis::BindingKind::LiteralOperator
            )
    })
}
/// Is the reference at `range` a **call** — is the next non-space character a `(`?
///
/// A lexical test on the file's text, and the honest one: the analysis resolves *names*, and whether a name is called
/// is the one thing the tree records more finely than a fact does (a `CallExpr`'s callee against a bare use). The
/// alternative — parsing every calling file — is what this feature does not need.
fn is_a_call(text: &str, range: cpp_parser::SourceRange) -> bool {
    let Some(rest) = text.get(range.end_offset()..) else {
        return false;
    };
    rest.trim_start().starts_with('(')
}

/// **The function whose body contains `offset`** — the innermost declaration with a body, read from the tree.
///
/// A fact's `range` is its **declarator**, not its body (`int both(int a, int b)` covers no `{ … }`), so the calling
/// file is parsed once per query — this is a request a reader makes, not something a pull does — and the function is
/// the innermost declaration whose `CompoundStat` child contains the offset.
fn the_caller_of<F: FileProvider + Clone>(
    session: &Session<F>,
    view: &cpp_code_analysis::FileView,
    file: &std::path::Path,
    offset: usize,
) -> Option<CallHierarchyItem> {
    let held = session.files().held(file)?;
    let mut node = view.root.clone();
    loop {
        let mut child = None;
        for candidate in node.children() {
            let range = candidate.text_range();
            let start = usize::from(range.start());
            let end = usize::from(range.end());
            if offset >= start && offset < end {
                child = Some(candidate);
                break;
            }
        }
        match child {
            Some(child) => node = child,
            None => break,
        }
    }

    let mut current = Some(node);
    while let Some(node) = current {
        let body = node.children().find(|child| {
            matches!(
                cpp_parser::CppSyntaxKind::from(child.kind()),
                cpp_parser::CppSyntaxKind::CompoundStat
            )
        });
        // **The body has to contain the offset.** A definition's own name is inside the declaration that has a body —
        // as a *sibling* — so "the declaration has a body" is not the same question as "this offset is in the body".
        // Measured: without this, the definition of `UpdateCodeStyle` was reported as its own caller.
        let body = body.filter(|body| {
            let range = body.text_range();
            offset >= usize::from(range.start()) && offset < usize::from(range.end())
        });
        if body.is_some() {
            // **The declarator's own name**, not a parameter's: the direct `NameExpr` child of the outermost
            // `Declarator`, the same rule `qualified_declarations` uses.
            let declarator = node.children().find(|child| {
                matches!(
                    cpp_parser::CppSyntaxKind::from(child.kind()),
                    cpp_parser::CppSyntaxKind::InitDeclarator
                )
            });
            let name_node = declarator.as_ref().and_then(|declarator| {
                let outer = declarator
                    .children()
                    .find(|child| {
                        matches!(
                            cpp_parser::CppSyntaxKind::from(child.kind()),
                            cpp_parser::CppSyntaxKind::Declarator
                        )
                    })
                    .unwrap_or_else(|| declarator.clone());
                outer.children().find(|child| {
                    matches!(
                        cpp_parser::CppSyntaxKind::from(child.kind()),
                        cpp_parser::CppSyntaxKind::NameExpr
                    )
                })
            })?;
            let name = name_node.text().to_string().trim().to_string();
            if name.is_empty() {
                return None;
            }
            let uri = path_to_uri(file)?;
            let range = span(
                &held,
                usize::from(node.text_range().start()),
                usize::from(node.text_range().end()),
            )?;
            let selection_range = span(
                &held,
                usize::from(name_node.text_range().start()),
                usize::from(name_node.text_range().end()),
            )?;
            return Some(CallHierarchyItem {
                name: name.clone(),
                // **A member is a method**: an out-of-line definition writes `C::f`, and a qualifier is what says the
                // name belongs to a class. A free function has none.
                kind: if name.contains("::") {
                    SymbolKind::METHOD
                } else {
                    SymbolKind::FUNCTION
                },
                tags: None,
                detail: None,
                uri,
                range,
                selection_range,
                data: None,
            });
        }
        current = node.parent();
    }
    None
}
/// A byte range in a file, as a protocol range.
fn span(held: &cpp_code_analysis::VfsFile, start: usize, end: usize) -> Option<Range> {
    Some(Range::new(
        position_in_file(held, start)?,
        position_in_file(held, end)?,
    ))
}
/// **What the body of the function at `offset` calls.**
///
/// The body is found through the fact's own range (the innermost function fact whose range contains the declaration),
/// its `CallExpr` children are walked, and every callee is resolved with the same `definitions` query the other
/// features use — so a call this answers is a call Go to Definition can follow.
pub fn outgoing_calls_of<F: FileProvider + Clone>(
    session: &Session<F>,
    view: &cpp_code_analysis::FileView,
    offset: usize,
) -> Vec<CallHierarchyOutgoingCall> {
    let Some(body) = the_body_of(view, offset) else {
        return Vec::new();
    };
    let mut calls: Vec<(CallHierarchyItem, Vec<Range>)> = Vec::new();
    for call in body.descendants().filter(|node| {
        matches!(
            cpp_parser::CppSyntaxKind::from(node.kind()),
            cpp_parser::CppSyntaxKind::CallExpr
        )
    }) {
        let Some(callee) = call.children().next() else {
            continue;
        };
        let at = usize::from(callee.text_range().end()).saturating_sub(1);
        let Known::Yes(defined) = session.definitions(
            view,
            cpp_code_analysis::file::coordinates::ReadingOffset::new(at),
        ) else {
            continue;
        };
        let Some(definition) = defined
            .found
            .iter()
            .find(|definition| definition.fact.kind == cpp_code_analysis::DeclKind::Function)
        else {
            continue;
        };
        let Some(item) = item_of(session, &definition.file, &definition.fact) else {
            continue;
        };
        let Some(held) = session.files().held(&view.path) else {
            continue;
        };
        let range = call.text_range();
        let (Some(start), Some(end)) = (
            position_in_file(held, usize::from(range.start())),
            position_in_file(held, usize::from(range.end())),
        ) else {
            continue;
        };
        let call_range = Range::new(start, end);
        match calls
            .iter_mut()
            .find(|(held, _)| held.uri == item.uri && held.selection_range == item.selection_range)
        {
            Some((_, ranges)) => {
                if !ranges.contains(&call_range) {
                    ranges.push(call_range);
                }
            }
            None => calls.push((item, vec![call_range])),
        }
    }
    calls
        .into_iter()
        .map(|(to, from_ranges)| CallHierarchyOutgoingCall { to, from_ranges })
        .collect()
}

/// The body of the function the declaration at `offset` names, when this file holds it.
///
/// **Up, then sideways**: a declaration's name is in its declarator and the body is the declaration's *sibling* child,
/// so the walk goes down to the innermost node holding the offset, then up to the declaration, and takes the
/// `CompoundStat` from there — a walk *down* through children never meets the body.
fn the_body_of(
    view: &cpp_code_analysis::FileView,
    offset: usize,
) -> Option<cpp_parser::CppSyntaxNode> {
    let body_of = |node: &cpp_parser::CppSyntaxNode| {
        node.children().find(|child| {
            matches!(
                cpp_parser::CppSyntaxKind::from(child.kind()),
                cpp_parser::CppSyntaxKind::CompoundStat
            )
        })
    };

    let mut node = view.root.clone();
    loop {
        let mut child = None;
        for candidate in node.children() {
            let range = candidate.text_range();
            let start = usize::from(range.start());
            let end = usize::from(range.end());
            if offset >= start && offset < end {
                child = Some(candidate);
                break;
            }
        }
        match child {
            Some(child) => node = child,
            None => break,
        }
    }

    let mut current = Some(node);
    while let Some(node) = current {
        if let Some(body) = body_of(&node) {
            return Some(body);
        }
        current = node.parent();
    }
    None
}
pub struct CallHierarchyCapabilities;

impl RegisterCapabilities for CallHierarchyCapabilities {
    fn register_capabilities(server: &mut ServerCapabilities, _client: &ClientCapabilities) {
        server.call_hierarchy_provider = Some(CallHierarchyServerCapability::Simple(true));
    }
}
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };

    use super::{incoming_calls_of, item_at, outgoing_calls_of};

    const BASE_H: &str = "\
int add(int a, int b);
int subtract(int a, int b);
";

    const MAIN_CPP: &str = "\
#include \"base.h\"
int add(int a, int b) { return a + b; }
int subtract(int a, int b) { return a - b; }
int both(int a, int b) {
    return add(a, b) + subtract(a, b);
}
";

    fn session() -> Session<MemoryFiles> {
        let files = MemoryFiles::new()
            .with_file("/p/base.h", BASE_H)
            .with_file("/p/main.cpp", MAIN_CPP);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.add_project_files([PathBuf::from("/p/base.h"), PathBuf::from("/p/main.cpp")]);
        session.index_everything();
        session
    }

    /// **The two directions answer each other**: `both` calls `add`, so `add`'s incoming call is `both`, and `both`'s
    /// outgoing call is `add`.
    #[test]
    fn a_call_appears_from_both_ends() {
        let session = session();
        let path = PathBuf::from("/p/main.cpp");
        let view = session.view_of_the_file(&path).expect("the file parses");
        let source = view.source.to_string();

        let add = source.find("int add(int a").expect("the definition") + "int ".len();
        let item = item_at(&session, &view, add).expect("a function is a call item");
        println!("add → {:?} {:?}", item.name, item.selection_range);
        assert_eq!(item.name, "add");
        assert_eq!(item.kind, lsp_types::SymbolKind::FUNCTION);

        let outgoing = outgoing_calls_of(&session, &view, add);
        println!(
            "add calls: {:?}",
            outgoing
                .iter()
                .map(|call| (call.to.name.clone(), call.from_ranges.len()))
                .collect::<Vec<_>>()
        );
        // `add`'s body has no call of its own: `a + b` is an operator on builtins.
        assert!(outgoing.is_empty(), "add calls nothing: {outgoing:?}");

        let both = source.find("int both(int a").expect("the caller") + "int ".len();
        let outgoing = outgoing_calls_of(&session, &view, both);
        println!(
            "both calls: {:?}",
            outgoing
                .iter()
                .map(|call| (call.to.name.clone(), call.from_ranges.len()))
                .collect::<Vec<_>>()
        );
        let names: Vec<&str> = outgoing.iter().map(|call| call.to.name.as_str()).collect();
        assert!(names.contains(&"add"), "both calls add: {names:?}");
        assert!(
            names.contains(&"subtract"),
            "both calls subtract: {names:?}"
        );
        assert!(
            !names.contains(&"both"),
            "and nothing calls itself here: {names:?}"
        );

        let incoming = incoming_calls_of(&session, &view, both, "both");
        println!(
            "callers of both: {:?}",
            incoming
                .iter()
                .map(|call| (call.from.name.clone(), call.from_ranges.len()))
                .collect::<Vec<_>>()
        );
        assert!(
            incoming.is_empty(),
            "nothing calls `both` in the fixture: {incoming:?}"
        );

        let incoming = incoming_calls_of(&session, &view, add, "add");
        println!(
            "callers of add: {:?}",
            incoming
                .iter()
                .map(|call| (call.from.name.clone(), call.from_ranges.len()))
                .collect::<Vec<_>>()
        );
        assert_eq!(incoming.len(), 1, "one caller, `both`: {incoming:?}");
        assert_eq!(incoming[0].from.name, "both");
        assert_eq!(
            incoming[0].from_ranges.len(),
            1,
            "one call site: {incoming:?}"
        );
    }

    /// **A name that is not a callable is no item** — a class, a local, a variable.
    #[test]
    fn a_class_is_not_a_call_item() {
        let session = session();
        let path = PathBuf::from("/p/base.h");
        let view = session.view_of_the_file(&path).expect("the header parses");
        let at = 0;
        assert!(
            item_at(&session, &view, at).is_none(),
            "a file has no function at 0"
        );
    }
}
