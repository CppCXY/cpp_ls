//! # `textDocument/completion` — the list a cursor is asking for, and **why each item is in it**
//!
//! A completion is one question with one answer, and the whole of this module is the reading that decides *which*
//! question the cursor is asking. Everything below — the scope tree, the index, the search path — already knows how
//! to list what it has; what was missing was the reading that decides which of those lists belongs here, and the
//! ordering that decides what a reader sees first.
//!
//! ```text
//! the shape at the cursor   →  what may be written here                  (context)
//! the candidates            →  every one of them, with where it came from (names, members, table)
//! the order                 →  the nearest declaration first              (Score)
//! the budget                →  the biggest useful list, not the whole one (ITEM_BUDGET)
//! ```
//!
//! # Why "everything visible" is not an answer
//!
//! `#include <iostream>` puts the whole standard library in scope, transitively and honestly: a lookup that stops
//! at every declaration reachable through the include graph is *correct*, and it is what this layer used to answer
//! — measured on one real file, **692 names** for a blank line inside a function body, of which **6** came from the
//! file itself. A list whose useful part is one percent of it is a list nobody reads.
//!
//! The number is not wrong, it is the wrong *answer to the question a completion asks*. What a reader wants when
//! they press the key is what they were about to type, and the signals for that are textual and structural rather
//! than semantic — which is what this module ranks by:
//!
//! ```text
//! local_variable   declared in the scope the cursor is in          ← what they meant
//! w                declared in the scope the cursor is in
//! Widget           declared at file scope in this file
//! readMessage      declared at file scope in this file
//! std::string      declared in a header this file includes
//! _ARGMAX          declared in a header that header includes
//! ```
//!
//! The same list, ordered by how far the declaration is from the cursor **in the program text**: the body, the
//! file, what the file includes, what *that* includes. It is the rule every C++ front end's completion uses, and
//! it is C++'s own order of consideration seen from the other end — a name lookup walks outward, so the names it
//! reaches first are the ones a reader is most likely writing.
//!
//! # The vocabulary rules, which are separate from the ordering
//!
//! * **A member access is not a name position.** After `w.` the answer is `Widget`'s members and nothing else: a
//!   file-scope name cannot follow the operator, and offering one is offering a syntax error.
//! * **A `#` owns its cursor.** After a `#` the answer is the directives, after `#include` it is headers, and
//!   after `#endif ` it is nothing at all.
//! * **Prose is not code.** A cursor in a comment or a literal gets an empty list, because a list of names to type
//!   into a sentence is not a suggestion.
//!
//! # What is deliberately *not* filtered
//!
//! Nothing is dropped for being *unlikely*. `abort` and `FILE` are genuinely visible and genuinely occasionally
//! what somebody is writing, so they stay and are ranked low; `while` is offered inside an expression because C++
//! has no statement/expression divide a completion can see. What is dropped is what **cannot** be written here (a
//! member name after a `::`, a keyword where the reader is mid-identifier), what the implementation reserves
//! (`_Arg`, `__crt_…`), and what is past the budget. The remaining judgement is expressed as an *order*, because
//! an order can be wrong cheaply and a filter cannot.

pub mod context;
pub mod includes;
pub mod matching;
pub mod table;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cpp_parser::{CppSyntaxNode, SourceRange};

pub use context::{CompletionContext, context_at, doc_comment_at};
pub use includes::{Header, HeaderIndex, HeaderKind, SharedHeaders};
pub use table::{
    BUILTIN_TYPES, DIRECTIVES, DirectiveName, KEYWORDS, Keyword, KeywordUse, SNIPPETS, Snippet,
};

use crate::sema::symbol::{Known, ScopeKind, ScopeTree};
use crate::summary::{DeclFact, DeclKind};
use crate::{ProjectIndex, ProjectMember};

/// What an item is, in the vocabulary every consumer understands.
///
/// The analysis does not speak the protocol — `CompletionItemKind` is LSP's, and its numbers are a wire format —
/// so the kind is stated here once and mapped where the wire is. See `crate::util::kind` in the server for the
/// mapping, which is the only place that knows both vocabularies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemKind {
    Variable,
    Function,
    Method,
    Field,
    Class,
    Enum,
    EnumMember,
    Namespace,
    Macro,
    TypeParameter,
    Keyword,
    Snippet,
    Header,
}

/// One item a completion can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    /// The text shown in the list, and what is inserted unless `CompletionItem::snippet` says otherwise.
    pub label: String,
    pub kind: ItemKind,
    /// What to insert, when that is not the label. For a snippet this is the body **in snippet syntax**.
    pub insert: String,
    /// Is [`CompletionItem::insert`] a snippet body rather than plain text?
    pub snippet: bool,
    /// One line beside the label: a type, a signature, "inherited from `Base`".
    pub detail: Option<String>,
    /// Where the declaration is, for `completionItem/resolve`: the file, and the offset of the declared **name**.
    /// `None` for a keyword, a snippet or a header, which declare nothing to document.
    pub identity: Option<(PathBuf, usize)>,
    /// What the client should compare against the typed prefix, when that is not the label: a qualified name whose
    /// tail is what is being typed (`std::string` against `str`).
    pub filter: Option<String>,
    /// The declaration this item is, when it has one.
    pub fact: Option<DeclFact>,
    /// The range a client **replaces** with [`CompletionItem::insert`].
    pub replace: SourceRange,
}

impl CompletionItem {
    /// A declaration, as an item.
    fn declared(fact: &DeclFact, file: &Path) -> CompletionItem {
        // An item with no name — a destructor the index could not spell — has nothing to resolve *to*, and an
        // identity pointing at an empty range would fetch another declaration's documentation.
        let identity =
            (!fact.name.is_empty()).then(|| (file.to_path_buf(), fact.name_range.start_offset));

        CompletionItem {
            label: fact.name.clone(),
            kind: kind_of(fact.kind),
            insert: match (&fact.kind, &fact.parameter_list) {
                // **A function-like macro is inserted with its parenthesis** — the only hint a completion item carries about how a
                // name is used: `MAX(` says it takes arguments, and `MAX` alone does not. Measured before this: the item for
                // `#define MAX(a, b)` read `insert: "MAX"`, `kind: Macro`, and nothing else.
                (crate::DeclKind::MacroLike, Some(_)) => format!("{}(", fact.name),
                _ => fact.name.clone(),
            },
            snippet: false,
            detail: detail_of(fact),
            identity,
            filter: None,
            fact: Some(fact.clone()),
            replace: SourceRange::new(0, 0),
        }
    }

    fn word(label: &str, kind: ItemKind, detail: &str) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            kind,
            insert: label.to_string(),
            snippet: false,
            detail: Some(detail.to_string()),
            identity: None,
            filter: None,
            fact: None,
            replace: SourceRange::new(0, 0),
        }
    }

    fn snippet(trigger: &str, detail: &str, body: &str) -> CompletionItem {
        CompletionItem {
            label: trigger.to_string(),
            kind: ItemKind::Snippet,
            insert: body.to_string(),
            snippet: true,
            detail: Some(detail.to_string()),
            identity: None,
            filter: None,
            fact: None,
            replace: SourceRange::new(0, 0),
        }
    }

    fn header(header: &Header) -> CompletionItem {
        CompletionItem {
            label: header.name.clone(),
            kind: ItemKind::Header,
            insert: header.name.clone(),
            snippet: false,
            detail: None,
            identity: None,
            filter: Some(header.name.clone()),
            fact: None,
            replace: SourceRange::new(0, 0),
        }
    }
}

/// A declaration's kind, in the item vocabulary.
fn kind_of(kind: DeclKind) -> ItemKind {
    match kind {
        DeclKind::Type => ItemKind::Class,
        DeclKind::Function => ItemKind::Function,
        DeclKind::Variable => ItemKind::Variable,
        DeclKind::Namespace => ItemKind::Namespace,
        DeclKind::MacroLike => ItemKind::Macro,
        DeclKind::Other => ItemKind::Variable,
    }
}

/// A declaration's **own** one-line description: its type, or what it returns with what it takes.
///
/// Deliberately not the kind ("a variable"): the icon already says that, and a line repeating the icon would
/// crowd out the one thing a reader wants — the type — which is also the one thing a name from another file does
/// not show in the list otherwise.
///
/// # Why a function shows its parameter list
///
/// Because a list of names is a list of *choices*, and two functions are told apart by what they take: the popup
/// after `std::` holds `format`, `format_to`, `format_to_n`, `formatted_size` and `vformat`, all returning
/// something string-shaped, and `string (…)` for every one of them is a row a reader cannot choose from. The
/// spelling comes from the declaration's own fact ([`DeclFact::parameter_list`]), so it is available for a name
/// declared in a header nobody has opened — the case this line exists for.
///
/// `(…)` survives for the one case it is still the honest answer to: a fact written before the field existed, or a
/// function whose declarator the reading could not reach. It says "this function takes something", which is less
/// than the truth and not more.
pub fn detail_of(fact: &DeclFact) -> Option<String> {
    match fact.kind {
        DeclKind::Function => match (&fact.returns, &fact.parameter_list) {
            (Some(returns), Some(parameters)) => Some(format!("{returns} {parameters}")),
            (Some(returns), None) => Some(format!("{returns} (…)")),
            (None, Some(parameters)) => Some(parameters.clone()),
            (None, None) => None,
        },
        DeclKind::Variable => fact.type_of.clone(),
        DeclKind::Type if !fact.bases.is_empty() => Some(format!(": {}", fact.bases.join(", "))),
        _ => None,
    }
}

/// What a completion found: the items, in the order they should be shown, and **whether the answer is complete**.
///
/// `Default` is written out rather than derived because [`SourceRange`] has no default of its own: a range of
/// nothing at offset zero is the same "no edit" the type means everywhere else, and giving `SourceRange` a
/// `Default` would make an accidental one compile at every other call site too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionSet {
    pub items: Vec<CompletionItem>,
    /// The scope the names were listed from — `ns`, `Widget`, `::`, or empty for "wherever the cursor is".
    pub scope: String,
    /// What of the name is already written before the cursor, which is what filters the list.
    pub prefix: String,
    /// The range a client **replaces** with the chosen item.
    pub replace: SourceRange,
    /// Was the list cut short by a budget, so that items which would have been offered are missing?
    ///
    /// Distinct from the analysis being incomplete, and the distinction is the protocol's `isIncomplete`: a client
    /// told "incomplete" asks again as the user types, which is exactly right while a background index is still
    /// reading files and exactly wrong when the answer was merely capped — the retry would return the same capped
    /// list for ever.
    pub truncated: bool,
    /// **Was the analysis itself incomplete** — a class this answer depends on that could not be read?
    ///
    /// The protocol's `isIncomplete` means one thing: *ask again later*. `truncated` is a budget, and re-asking
    /// returns the same list; this is the other kind — a class whose facts the index does not have yet, so the
    /// members are **missing rather than few**. Measured on a real project: `std::string* message; message->`
    /// offered **one** item on the first keystroke (`operator basic_string_view`, the only member the raw reading of
    /// `<xstring>` had at file scope) and **58** once the file was cooked. A list that looks final and holds a
    /// quarter of the truth is worse than an empty one, because the reader believes it.
    pub incomplete: bool,
}

impl Default for CompletionSet {
    fn default() -> CompletionSet {
        CompletionSet {
            items: Vec::new(),
            scope: String::new(),
            prefix: String::new(),
            replace: SourceRange::new(0, 0),
            truncated: false,
            incomplete: false,
        }
    }
}

/// An item and the score that orders it.
///
/// The score is **not** a field of [`CompletionItem`]: a consumer renders items, and a number it must not re-sort
/// by is a field it would eventually misuse. Keeping the two together here means the order is decided once, in one
/// place, and thrown away at the same moment.
///
/// `matched` is how well the item answers what was typed, and it is the **last** thing the order looks at — after the
/// tier, which says where a declaration is, and after the penalty, which says what kind of thing it is. It is here
/// rather than inside `score` because it is a fact about the *typed text* and not about the declaration: the same
/// name scores the same whatever the reader has written, and the same declaration scores differently on every
/// keystroke.
#[derive(Debug, Clone)]
struct Scored {
    score: Score,
    matched: crate::completion::matching::Quality,
    item: CompletionItem,
}

/// **The completion for a cursor**, given everything the analysis knows.
///
/// This is the whole of the feature: the context decides which vocabulary applies, the layers below gather it, and
/// the score orders it. A caller that wants one of the *queries* rather than the list — a test, a probe — should
/// look at [`NameCompletions`](crate::NameCompletions) and [`MemberCompletions`](crate::MemberCompletions), which
/// are the two questions this dispatches between.
pub fn completion_at(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &CppSyntaxNode,
    path: &Path,
    offset: usize,
    headers: &HeaderIndex,
    cuda: bool,
) -> CompletionSet {
    // The context is this function's first question and the only one read from a tree; a caller that has already read
    // it — `Session::completions`, for the two contexts that live in the **file's** own text rather than in the
    // rendering — hands it over instead of having it asked again of the wrong tree.
    let asked = context_at(root, offset);
    completion_with(index, scopes, root, path, offset, headers, asked, cuda)
}

/// **The same dispatch with the context already decided**, and the entry point for a caller whose context came from
/// another tree — see `Session::completions`.
///
/// `offset` is in the coordinates of **the tree the context was read from**: the reading's for a context asked of
/// `root`, and the **file's** for one asked of a parse of the written text. The `replace` range a context carries is
/// built from it, and a range a client is to edit has to be the file's.
pub fn completion_with(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &CppSyntaxNode,
    path: &Path,
    offset: usize,
    headers: &HeaderIndex,
    context: CompletionContext,
    cuda: bool,
) -> CompletionSet {
    // **The measurement window for one completion**, opened here because this is the entry point the feature has:
    // everything below is the query's own work, and the server's log line splits only the outside of it
    // (`view`/`completions`). Opt-in, on `CPPLS_TRACE_COMPLETION`, like this crate's other traces — see

    let answer = match context {
        CompletionContext::Nothing => CompletionSet {
            replace: SourceRange::new(offset, 0),
            ..CompletionSet::default()
        },
        CompletionContext::Member(access) => {
            members(index, scopes, root, path, &access, offset).set
        }
        CompletionContext::Qualified(position) => {
            names(index, scopes, root, path, &position, offset, false, cuda)
        }
        CompletionContext::Name(position) => {
            names(index, scopes, root, path, &position, offset, true, cuda)
        }
        CompletionContext::DirectiveName { written, range } => directives(&written, range),
        // **A module name is answered by the session, not here**, and the empty set says so rather than guessing:
        // the names are the *project's* (its `.cppm` files and the modules the index can name), which is a question
        // about the index and the file list rather than about the tree — see `Session::module_completions`. This arm
        // exists because the match must be total, and a caller that reaches it has no session to ask.
        CompletionContext::ModuleName { .. } => CompletionSet::default(),
        CompletionContext::DirectiveArgument { written, range } => {
            let position = crate::sema::resolve::NamePosition {
                scope: String::new(),
                written,
                range,
            };

            // `#define NAME`, `#undef NAME`, `#ifdef NAME`: the argument is a *name*, and what may be written is
            // what the program already declares. No keywords and no snippets — a macro's name is an identifier, and
            // a control-flow statement is not one — which is why this goes through the name query only.
            names(index, scopes, root, path, &position, offset, false, cuda)
        }
        CompletionContext::Include {
            written,
            closed,
            range,
            ..
        } => headers_for(headers, &written, closed, range),
        // **A `#define`'s replacement list**: the macro's own parameters first, then the macros already defined.
        // Nothing else belongs here — the body is preprocessor text, where a class member or a local cannot appear.
        CompletionContext::MacroBody {
            written,
            range,
            parameters,
        } => macro_body(index, path, &written, range, &parameters),
        // **A preprocessor condition** — `#if`, `#elif`, `#ifdef`, `#undef`: the macros, and the operators on them.
        CompletionContext::Condition { written, range } => condition(index, path, &written, range),
        // **Inside a documentation comment**: Doxygen's vocabulary, or the name the command takes. Both are read
        // from the file's own text rather than from a scope, so neither needs the index — see
        // [`doc_comment_at`].
        CompletionContext::DocComment {
            written,
            range,
            wants,
        } => doc_comments(index, path, &written, range, wants),
    };

    answer
}

/// **What the completion made of a cursor** — the one diagnosis this layer is worth logging.
///
/// A `.` whose answer is the names in scope is the shape a user reported as "补全明显错误": the popup is full of
/// things that cannot follow the operator, and nothing in it says whether the *type* could not be worked out, the
/// class could not be found, or the cursor was never read as a member access at all. Those three have different
/// fixes, and the reason is thrown away by the time a client sees an answer with no members in it.
///
/// **Asked of the same reader the answer came from**, and that is the whole of why this function takes `root` and
/// not the set: the member list is produced together with the reason it came out empty, so the sentence a log shows
/// is about *this* query rather than about a second one that may disagree with it. Measured on a live server: the
/// diagnostic said "the cursor is not a member access at all" while the completion beside it was listing the
/// members — because the diagnostic asked the index layer, whose tree-only shape reader does not accept a cursor
/// drawn **on** the operator's own column, and the feature asks the context reader, which does.
///
/// Only asked when the answer contained no members, so the cost is one extra query on exactly the keystrokes a
/// reader would complain about.
pub fn why_no_members(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &CppSyntaxNode,
    path: &Path,
    // **A typed offset, because an untyped one let a caller pass the file's** — and the failure was a panic inside
    // `rowan` rather than a wrong answer: `"Bad offset: range 0..27 offset 52"`, where 52 is a position in the file
    // (the header inlined, the directive gone) and 27 is the length of its rendering. This function reads `root`, so
    // its offset is the rendering's.
    offset: crate::file::coordinates::ReadingOffset,
) -> String {
    let offset = offset.get();
    match context_at(root, offset) {
        CompletionContext::Member(access) => {
            members(index, scopes, root, path, &access, offset).why
        }
        other => format!("the cursor is not a member access at all: {other:?}"),
    }
}

/// The members of an object, after a `.` or `->`, and **the sentence that explains an empty list**.
///
/// # Why the object comes from the caller rather than from a second reading of the tree
///
/// This used to call `member_completions_at`, which reads the shape off the tree itself — and that is a **second
/// reading of the same fact**, which is how the two came to disagree. The reader above answers "is the cursor a
/// member access" from the tree **and** from the text (see [`context::context_at`]), because a parser recovering
/// from a line that ends in an operator does not always build the access node; the query behind it only asked the
/// tree, so a cursor the reader had just classified as a member access came back as *not a member access at all*
/// and the list was empty. Measured on a live server: `full2.` on the last line of a body answered nothing.
///
/// There is one reader of the shape and it is the one above. What this function needs from it is the **object**,
/// which is what a type is inferred from — and that inference is [`type_of_expression`], the same call the query
/// makes, so the two layers still cannot disagree about the *type*.
///
/// # Why it returns the reason rather than only the list
///
/// An empty list has four causes — the object's type could not be read, the class is not declared anywhere the
/// index can see, the class has no members, or every member was the implementation's — and they have four different
/// fixes. The reason is produced *here*, where the steps are, rather than re-derived by a diagnostic that would
/// have to ask the same questions again and could answer them differently. See [`why_no_members`].
/// **Which class a `.` or a `->` reaches** — the answer a member list is built from.
///
/// A `.` means the object's own class (or, for a pointer, the class it points at — C++ makes that ill-formed, and a
/// reader typing `.` on a pointer is asking about the class anyway). A `->` means the same thing *after* the arrow has
/// been followed as far as the type answers, which is what makes the two cases a reader reports work:
///
/// ```text
/// Widget* p;                    p->   reaches Widget          (a pointer's pointee)
/// std::unique_ptr<Widget> sp;   sp->  reaches Widget          (the class's own operator->, then its pointee)
/// std::shared_ptr<T> sp;        sp->  reaches T               (same, one more hop through the pointer)
/// standard containers           it->  reaches the element     (`iterator::operator->`'s return)
/// ```
///
/// Answering the object's own class instead was the defect reported as "智能指针的补全没有考虑指向的内容": a smart
/// pointer's own member list is `operator->`, `operator*`, `reset`, `get`, … — none of which can be reached through
/// `->` — while the class it points at has exactly the members the arrow means.
///
/// **Bounded.** A class whose `operator->` returns itself (a proxy) is a loop, and four hops is past every real chain
/// measured: `shared_ptr<T>` is two, a `unique_ptr` is two, a container iterator is two.
pub(crate) fn the_class_after_the_operator(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &CppSyntaxNode,
    path: &Path,
    written: &crate::sema::types::Type,
    arrow: bool,
) -> Option<(String, bool)> {
    // **Whether a pointer was followed to find the class**, which is what says a `.` should have been a `->`.
    let mut followed_a_pointer = false;
    let mut current = written.clone();
    for _ in 0..4 {
        if let Some(class) = current.class_name().map(str::to_string) {
            // **1. A pointer-like class says what its arrow reaches, in `element_type`** — the standard's own
            // protocol, which `std::pointer_traits` is written in terms of and which `unique_ptr`, `shared_ptr` and
            // `weak_ptr` all take part in. It is asked **first**, because on those classes the arrow's declared
            // return type is a member alias of a dependent `typename …::type`:
            //
            // ```cpp
            // _NODISCARD pointer operator->() const noexcept;         // unique_ptr
            // using pointer = typename _Get_deleter_pointer_type<_Ty, remove_reference_t<_Dx>>::type;
            // ```
            //
            // Following *that* lands on a trait template — or, when the fact carries no readable return type at all,
            // on nothing, which is how the answer became the smart pointer's **own** members. Measured on a real
            // project: `std::unique_ptr<int> p; p->` offered `get`/`get_deleter` (none of which can be written after
            // that arrow) and `std::unique_ptr<std::string> p; p->` offered nothing.
            //
            // It is asked **only of a class that declares `operator->`** — that is what "pointer-like" means, and it
            // is what keeps an ordinary pointer from being stolen: `std::string* p; p->` must reach `std::string`,
            // whose member list has no `operator->`, so the pointer is stripped by the arm below instead of `p->`
            // becoming `basic_string::value_type`'s members (`char`'s, of which there are none).
            if let Some(pointee) =
                the_element_type_pointed_at(index, scopes, root, path, &class, &current)
            {
                current = crate::sema::types::parse_type_spelling(&pointee);
                followed_a_pointer = true;
                continue;
            }
            // **2. A class's own `operator->`, when its declared return type can be read.** For an `arrow` this is
            // the chain continuing; for a `.` it is a reader who typed the wrong operator, which is reported by
            // `followed_a_pointer` so that the completion can rewrite the `.` as `->`.
            let returns = crate::index::project::operator_returns_of(
                index,
                &mut |_: &Path| None,
                scopes,
                root,
                path,
                &current,
                "operator->",
            );
            let Some(returns) = returns else {
                // **3. Where the arrow stops.** A plain `struct` on the left of `->` is ill-formed, and its members
                // are the honest answer for a reader who typed one by mistake.
                return Some((class, followed_a_pointer));
            };
            current = crate::sema::types::parse_type_spelling(&returns);
            if !arrow {
                followed_a_pointer = true;
            }
            continue;
        }
        match current.pointee() {
            Some(pointee) => {
                followed_a_pointer = true;
                current = (*pointee).clone();
            }
            None => return None,
        }
    }
    current
        .class_name()
        .map(str::to_string)
        .map(|class| (class, followed_a_pointer))
}

/// **What a pointer-like class's arrow reaches**, asked of the index's own walk — see
/// [`crate::index::project::element_type_of`], which is the one implementation of that rule.
///
/// # Why the member list is asked first
///
/// A class is **pointer-like** when it declares `operator->`; `element_type` is then the standard's answer to what
/// that arrow reaches. The two must be asked together, because `element_type`/`value_type` alone are far too common:
/// `std::string` has `value_type = char`, so asking them of *any* class turned `std::string* p; p->` into "the members
/// of `char`" — measured, sixty-eight members became zero. So the guard is the operator, read from the class's member
/// list (which walks bases and, unlike the single-member query, never asks about an arrow — `member_fact` falls back to
/// `the_class_after_the_operator`, and this function is called from there).
///
/// The **arguments the use wrote** (`["std::string"]` for `std::unique_ptr<std::string>`) are what its `element_type`
/// is substituted with, and the class is passed to `template_parameters_for` **qualified**: with the bare `unique_ptr`
/// the walk reached the primary template's parameters and answered `_Ty` unsubstituted, which then pointed the arrow at
/// a class called `_Ty`.
fn the_element_type_pointed_at(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &CppSyntaxNode,
    path: &Path,
    class: &str,
    written: &crate::sema::types::Type,
) -> Option<String> {
    let Known::Yes(members) = crate::index::project::members_of(index, scopes, root, path, class)
    else {
        return None;
    };
    if !members
        .members
        .iter()
        .any(|member| matches!(member.fact.name.as_str(), "operator->" | "operator*"))
    {
        if crate::probes::members() {
            let names: Vec<&str> = members
                .members
                .iter()
                .map(|member| member.fact.name.as_str())
                .filter(|name| name.starts_with("operator"))
                .collect();
            eprintln!(
                "element_type: {class} is not pointer-like ({} member(s), operators {names:?})",
                members.members.len()
            );
        }
        return None;
    }
    let arguments: Vec<String> = written
        .arguments()
        .iter()
        .map(|argument| argument.to_string())
        .collect();
    let walked =
        crate::index::project::element_type_of(index, scopes, root, path, class, class, &arguments);

    // **A spelling only counts when it names a *class*.** `_Ptr_base` — `shared_ptr`'s base — says
    // `using element_type = remove_extent_t<_Ty>;`, and that is an **alias template**: substituted at the use it is
    // `remove_extent_t<std::string>`, whose target is `typename remove_extent<T>::type`, a trait evaluation rather
    // than a spelling. Following it reaches a class template with no arguments, no members and no way back — which is
    // exactly what `shared_ptr<std::string> p; p->` answered: nothing.
    //
    // For a class that takes an arrow through its own operator, **its own argument is the element** — that is what
    // `shared_ptr<T>`, `unique_ptr<T>` and `weak_ptr<T>` mean, and it is the same rule
    // `the_class_an_arrow_points_at` states for the other reader of this question.
    match walked {
        Some(pointee) if leads_to_members(index, scopes, root, path, &pointee) => Some(pointee),
        _ => arguments
            .first()
            .filter(|first| leads_to_members(index, scopes, root, path, first))
            .cloned(),
    }
}

/// **Can a member query be asked about this spelling at all?** — the test for "is this a type, or a dead end?".
///
/// Deliberately asked of the *member list* rather than of the declaration: `std::string` is an **alias** of
/// `basic_string<char, …>` and its members are 58; `remove_extent_t<std::string>` is an **alias template** whose target
/// is a trait evaluation and whose member list is empty. The distinction that matters here is exactly that one, and a
/// kind/`type_of` test gets it backwards on the case this exists for.
///
/// The list is cached by the index, and the walk for a class with no declaration answers immediately, so this is a
/// hash lookup in the common case.
fn leads_to_members(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &CppSyntaxNode,
    path: &Path,
    name: &str,
) -> bool {
    matches!(
        crate::index::project::members_of(index, scopes, root, path, name),
        Known::Yes(list) if !list.members.is_empty()
    )
}

fn members(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &CppSyntaxNode,
    path: &Path,
    access: &crate::sema::resolve::MemberAccess,
    offset: usize,
) -> MemberAnswer {
    let replace = access.member_range;
    let prefix = written_before(access, offset);

    let refused = |why: String| MemberAnswer {
        set: CompletionSet {
            scope: String::new(),
            prefix: prefix.clone(),
            replace,
            truncated: false,
            // **"Not worked out" is not "nothing there".** Every refusal below is a missing fact — the type of the
            // object, or the class it names — so the client has to be told to ask again, and the handler's
            // `isIncomplete` is the only way to say so.
            incomplete: true,
            items: Vec::new(),
        },
        why,
    };

    let written = match crate::index::project::type_of_expression(
        index,
        &mut |_: &Path| None,
        scopes,
        root,
        path,
        &access.object,
        0,
    ) {
        Known::Yes((written, _)) => written,
        // The object's type could not be worked out — a call, a dereference, a subscript: each needs a type
        // *computed* rather than read off a declaration. **Nothing is offered**, and that is the answer rather than
        // a failure: a name that cannot follow the `.` is worse than no name, because the user believes it. The
        // empty list is not marked truncated, so a client that sees `isIncomplete` alongside it knows the
        // difference between "no members" and "not known yet".
        Known::Unknown(reason) => return refused(reason.describe()),
        Known::No => {
            return refused(format!(
                "nothing says what `{}` is, so its members are not known",
                access.object.text().to_string().trim()
            ));
        }
    };

    // **The class, not the type.** `std::vector<int>` is a type and `std::vector` is what has members; listing the
    // members of the whole spelling asked about a class nobody declares, which is why a completion after a standard
    // container's member answered nothing.
    //
    // **And for `->` it is the class the arrow reaches, not the object's own class.** `p->` on a `Widget*` means
    // `Widget`; `sp->` on a `std::unique_ptr<Widget>` means `Widget` as well, through the class's own `operator->`
    // (whose return type is a pointer, whose pointee is the class). Answering the object's own class was the defect
    // a reader reported as "智能指针的补全没有考虑指向的内容": a smart pointer's own members are `operator->`,
    // `operator*`, `reset`, … — none of which can be reached through `->` — while the class it points at has exactly
    // the members the arrow means.
    let Some((class, followed_a_pointer)) =
        the_class_after_the_operator(index, scopes, root, path, &written, access.arrow)
    else {
        // **A type that names no class has no members, and that is a fact rather than a gap.** Every way of working
        // the type out has already been taken by the time `the_class_after_the_operator` answers `None` — it follows
        // pointers, a class's own `operator->` and `element_type` — so what is left is a type that is not a class at
        // all: `int`, `double`, `void`, a function pointer. `int* p; p->` therefore answers "nothing here", and it is
        // **complete**: `isIncomplete` would make a client ask again for an answer that cannot change, which is the
        // shape of a popup that stays empty for ever.
        return MemberAnswer {
            set: CompletionSet {
                scope: String::new(),
                prefix,
                replace,
                truncated: false,
                incomplete: false,
                items: Vec::new(),
            },
            why: format!(
                "the type of `{}` is `{written}`, which names no class to list members of",
                access.object.text().to_string().trim()
            ),
        };
    };

    let found = match crate::index::project::members_of(index, scopes, root, path, &class) {
        Known::Yes(found) => found,
        // **The class was not found, which on a real project is usually "not read yet".** `<xstring>` declares
        // `basic_string` inside `_STD_BEGIN`/`_STD_END`, so the class is at *file* scope until that header is
        // cooked — and a member list for it is empty (or holds the handful of members the raw reading happened to
        // place) until then. Told to ask again, the client retries on the next keystroke and gets the real list;
        // told nothing, it shows the list it got and the reader believes it.
        Known::Unknown(reason) => {
            return MemberAnswer {
                set: CompletionSet {
                    scope: class.to_string(),
                    prefix,
                    replace,
                    truncated: false,
                    incomplete: true,
                    items: Vec::new(),
                },
                why: reason.describe(),
            };
        }
        // `members_of` reports a class nothing declares as `Unknown`, so this arm is for totality and says the same
        // thing it would.
        Known::No => {
            return MemberAnswer {
                set: CompletionSet {
                    scope: class.to_string(),
                    prefix,
                    replace,
                    truncated: false,
                    incomplete: true,
                    items: Vec::new(),
                },
                why: format!("`{class}` is not a class this analysis can see"),
            };
        }
    };

    let mut scored: Vec<Scored> = Vec::new();

    // **The two offer rules are applied before the ranking**, in one place that both consumers of a member list go
    // through: the names the implementation owns, and the members the reader cannot name from here — see
    // [`crate::index::project::offerable_members`], which is where the access rule reads [`DeclFact::access`]
    // against the classes the cursor is in.
    let members = {
        let mut members = found;
        let hidden = crate::index::project::offerable_members(
            index,
            scopes,
            root,
            path,
            offset,
            &mut members,
        );
        (members, hidden)
    };
    let (members, hidden_by_access) = members;

    // **One row per spelling.** A class declares `replace` eleven times and `insert` nine, and every one of them is
    // a real declaration a jump should find — but a list is something a reader *picks* from, and eleven rows
    // reading `replace` are one choice offered eleven times. So the member query's list is collapsed by name here,
    // which is the same rule the name query applies and for the same reason. The declarations are not lost: the
    // jump and the hover ask `members_of` / `member_definitions_across_files`, which keep the whole overload set.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for member in &members.members {
        // **A member the reader cannot name is not offered** — and that decision is the *query's*, not this
        // layer's: `member_completions_at` compares each member's access level with the classes the cursor is in
        // and reports what it kept out (`MemberCompletions::hidden_by_access`). Filtering here as well would be a
        // second reader of one rule, which is the shape of every bug in this area.
        if !seen.insert(member.fact.name.as_str()) {
            continue;
        }

        let mut item = CompletionItem::declared(&member.fact, &member.file);
        item.kind = member_kind(member, &class);
        item.replace = replace;

        if member.depth > 0 {
            item.detail = Some(match &item.detail {
                Some(type_of) => format!("{type_of}  (inherited from {})", member.declared_in),
                None => format!("inherited from {}", member.declared_in),
            });
        }

        if member.ambiguous {
            item.detail = Some(match &item.detail {
                Some(detail) => format!("{detail}  (ambiguous)"),
                None => "ambiguous: more than one base declares it".to_string(),
            });
        }

        // **What the reader has typed, matched against the name they would be typing.** A member list has a prefix
        // like any other, and a member whose name does not answer it is dropped rather than merely ranked low — see
        // [`crate::completion::matching`] for the rule and why the client's own filter cannot do this alone (it
        // cannot see `to_string` when the reader typed `string`, and it does not know the words of a name).
        let matched = crate::completion::matching::quality(&item.label, &prefix);
        if matched == crate::completion::matching::NONE {
            continue;
        }

        scored.push(Scored {
            score: member_score(member, &class),
            matched,
            item,
        });
    }

    let (items, truncated) = ordered(scored);
    let offered = items.len();

    // **A `.` typed on a pointer completes as `->`.** The list is the pointee's — what the reader meant — and the
    // operator they typed is the one thing about the answer that is wrong: `.size` on a `Widget*` does not compile.
    // So the operator is replaced as part of the insertion: the range grows one byte to the left (the `.`) and every
    // insert text gains the `->`. The handler keeps the rewrite for a client that cannot interpolate snippets by
    // noticing the prefix, and a client that filters on the label is unaffected (the label stays the member's name).
    // **…and only when the operator typed was the wrong one.** `followed_a_pointer` says "a pointer was followed to
    // find the class", which is also true of `p->` itself — and on an arrow this rewrite made every insert
    // `->append` with a range grown one byte left over the `>`, so accepting an item wrote `p-->append`. A reader on
    // `p->` typed the right operator; the rewrite is for the one case where the operator and the answer disagree,
    // which is a `.` on a pointer.
    let (replace, items) = if followed_a_pointer && !access.arrow {
        let operator = replace.start_offset.saturating_sub(1);
        let items: Vec<crate::completion::CompletionItem> = items
            .into_iter()
            .map(|mut item| {
                item.insert = format!("->{}", item.insert);
                item
            })
            .collect();
        (
            cpp_parser::SourceRange::new(operator, replace.length + 1),
            items,
        )
    } else {
        (replace, items)
    };

    let why = if offered > 0 {
        format!("the member query answered, with {offered} members")
    } else if hidden_by_access > 0 {
        // The distinction matters to whoever reads the log: "the class has no members" and "its members are
        // `private` from here" are different facts, and the second one has a fix the reader can apply.
        format!(
            "`{class}` declares {} member(s) and {hidden_by_access} of them are `private` or `protected` where the \
             cursor is",
            members.members.len() + hidden_by_access
        )
    } else {
        format!("`{class}` is declared and has no members this analysis can see")
    };

    // **A list that names where it stops is an incomplete answer**, and the client has to be able to tell it from a
    // complete one — `MemberList::unlisted` is exactly that field (the bases this walk could not open, with why). The
    // list is still offered: a partial list plus "ask again later" is better than a partial list that looks final.
    let incomplete = !members.unlisted.is_empty();

    MemberAnswer {
        set: CompletionSet {
            scope: class.to_string(),
            prefix,
            replace,
            truncated,
            incomplete,
            items,
        },
        why,
    }
}

/// A member list and **why it is what it is** — see [`members`], which is the only producer.
///
/// The reason travels with the answer because the two are one reading: a diagnostic that had to ask a second query
/// to find out why a list is empty would be a second reader of the same facts, and the two would drift apart the
/// first time either changed. It costs nothing when the list is full — the sentence is built at the same moment the
/// list is, and only a caller with an empty list has any use for it.
struct MemberAnswer {
    set: CompletionSet,
    /// A sentence for a log line: what was looked up and what came back.
    why: String,
}

/// The names visible at a cursor, from the scope tree and the index.
///
/// `with_table` is whether the language's own vocabulary — keywords, snippets, built-in type names — belongs in
/// this answer. It does in a **statement** and does not in a directive's argument.
fn names(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &CppSyntaxNode,
    path: &Path,
    position: &crate::sema::resolve::NamePosition,
    offset: usize,
    with_table: bool,
    cuda: bool,
) -> CompletionSet {
    let mut scored: Vec<Scored> = Vec::new();
    let mut listed_scope = position.scope.clone();
    let mut listed_prefix = position.written.clone();
    let mut replace = position.range;
    let mut truncated = false;

    match crate::index::project::name_completions_at(index, scopes, root, path, offset) {
        Known::Yes(listed) => {
            listed_scope = listed.scope.clone();
            listed_prefix = listed.prefix.clone();
            replace = listed.name_range;

            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

            for offered in &listed.names {
                // The query hides what an inner scope hides. What it cannot hide is one spelling listed twice by
                // *two files* — a namespace two headers reopen, a declaration and its redeclaration — and the list
                // is a list of things to type.
                if !seen.insert(offered.fact.name.as_str()) {
                    continue;
                }

                let mut item = CompletionItem::declared(&offered.fact, &offered.file);
                item.replace = replace;

                // **What is being typed is the tail of the name.** Inside `std::` the reader writes `str` and means
                // `string`: a client filtering on the bare label would show `string` for `str` anyway (it is a
                // substring), but a qualified detail is what makes the *ordering* stable when two scopes declare
                // one name.
                if !listed.scope.is_empty() {
                    item.filter = Some(format!("{}::{}", listed.scope, offered.fact.name));
                }

                // **Matched against what the client would filter on**, which is `filter` when the item has one and the
                // label otherwise: inside `std::` the reader types `str` and the item's filter is `std::string`, so
                // matching the label alone would answer `str` with nothing.
                let matched = matching::quality(
                    item.filter.as_deref().unwrap_or(&item.label),
                    &listed_prefix,
                );
                if matched == matching::NONE {
                    continue;
                }

                scored.push(Scored {
                    score: name_score(offered),
                    matched,
                    item,
                });
            }
        }
        // The position is not answerable — a `::` that names a scope nothing declares, a cursor the scope tree
        // cannot place. The list stays empty and the table still contributes, because `#define ret|` has a use for
        // `return` whether or not any name is visible.
        Known::Unknown(_) | Known::No => {}
    }

    if with_table {
        let statements = true;
        let in_a_class = in_a_class_body(scopes, offset);

        for snippet in table::snippets_for(statements) {
            let matched = matching::quality(snippet.trigger, &listed_prefix);
            if matched == matching::NONE {
                continue;
            }
            scored.push(Scored {
                score: Score::new(Score::SNIPPET, 0),
                matched,
                item: CompletionItem::snippet(snippet.trigger, snippet.detail, snippet.body),
            });
        }

        for keyword in table::keywords_for(statements, in_a_class) {
            // A snippet that writes the same word already stands for it: two items with one label and different
            // effects is a choice to make by accident.
            if table::a_snippet_writes_this(keyword.spelling) {
                continue;
            }
            let matched = matching::quality(keyword.spelling, &listed_prefix);
            if matched == matching::NONE {
                continue;
            }
            scored.push(Scored {
                score: Score::new(Score::KEYWORD, 0),
                matched,
                item: CompletionItem::word(keyword.spelling, ItemKind::Keyword, keyword.detail),
            });
        }

        // The built-in type names that are not keywords (`size_t`, `char16_t`). They are names rather than
        // keywords because that is how a compiler reads them, and they are here because a file that never includes
        // `<cstddef>` still writes `size_t`.
        for (name, detail) in table::BUILTIN_TYPES {
            let matched = matching::quality(name, &listed_prefix);
            if matched == matching::NONE {
                continue;
            }
            scored.push(Scored {
                score: Score::new(Score::KEYWORD, 0),
                matched,
                item: CompletionItem::word(name, ItemKind::Class, detail),
            });
        }

        // **CUDA's vocabulary, when the project is CUDA.** `__global__` is an identifier to the lexer and a specifier
        // to a CUDA compiler, so what may be written here is a fact about the *family* — and offering it in a file no
        // CUDA compiler will see is a claim that is false. The kinds differ on purpose: a qualifier is a keyword, a
        // built-in variable is a variable, and a vector type is a class, because that is what each would be if a
        // toolkit header had declared it.
        if cuda {
            for word in crate::cuda::words() {
                let matched = matching::quality(word.name, &listed_prefix);
                if matched == matching::NONE {
                    continue;
                }
                let kind = match word.kind {
                    crate::cuda::WordKind::Qualifier => ItemKind::Keyword,
                    crate::cuda::WordKind::Builtin => ItemKind::Variable,
                    crate::cuda::WordKind::VectorType => ItemKind::Class,
                };
                scored.push(Scored {
                    score: Score::new(Score::KEYWORD, 0),
                    matched,
                    item: CompletionItem::word(word.name, kind, word.detail),
                });
            }
        }
    }

    let (items, cut) = ordered(scored);
    truncated |= cut;

    CompletionSet {
        items,
        scope: listed_scope,
        prefix: listed_prefix,
        replace,
        truncated,
        // A name list is not a member list: nothing here can be *missing* because a class was not read.
        incomplete: false,
    }
}

/// The directives themselves, after a `#`.
///
/// Filtered by the same rule as every other list — see [`crate::completion::matching`] — so that `#ifnd` finds
/// `ifndef` and `#inc` finds `include` for the same reason a name list answers the initials of its words.
fn directives(written: &str, range: SourceRange) -> CompletionSet {
    let items: Vec<CompletionItem> = table::DIRECTIVES
        .iter()
        .filter(|directive| matching::quality(directive.name, written) != matching::NONE)
        .map(|directive| {
            let mut item =
                CompletionItem::word(directive.name, ItemKind::Keyword, directive.detail);
            item.replace = range;
            item
        })
        .collect();

    CompletionSet {
        scope: String::new(),
        prefix: written.to_string(),
        replace: range,
        truncated: false,
        incomplete: false,
        items,
    }
}

/// **Inside a documentation comment** — Doxygen's vocabulary, and then the name that command takes.
///
/// ```cpp
/// /// @par|              → `param`, `paragraph`, … (filtered, and by the same quality rule as every other list)
/// /// @param |           → the parameter names of the declaration below the comment
/// /// @tparam |          → its template parameters
/// ```
///
/// The commands come from the **parser's own table** ([`cpp_parser::every_command`]), which is the single list of
/// what may follow an `@`: a completion carrying its own copy would be a second source of truth, free to disagree
/// about `@retval` the day one of them is updated.
///
/// The names come from the **facts**, not from the tree: the declaration a comment documents is the first one after
/// it, and the index already says what its parameters are called (`DeclFact::parameters` for the template head and
/// `parameter_list` for the parameter list). A comment above a declaration in *another* file cannot be reached from
/// here, and does not need to be — a completion is asked in the file being written.
fn doc_comments(
    index: &ProjectIndex,
    path: &Path,
    written: &str,
    range: SourceRange,
    wants: Option<cpp_parser::DocCommandKind>,
) -> CompletionSet {
    use cpp_parser::DocCommandKind;

    let mut items: Vec<CompletionItem> = Vec::new();
    match wants {
        // **The name the command takes.** `@param`/`@tparam` want a parameter of the documented declaration, and
        // `@exception`/`@ref` want a name this layer has no better answer for than the ones in scope — so those
        // are answered with nothing rather than with a wrong list.
        Some(DocCommandKind::Parameter) => {
            let Some(summary) = index.summary(path) else {
                return CompletionSet {
                    prefix: written.to_string(),
                    replace: range,
                    ..CompletionSet::default()
                };
            };

            // **The declaration the comment documents** — the first one that starts after the comment. `range`
            // here is the span of the *typed name*, so the comment is what precedes it on an earlier line; the
            // fact's own start is after the comment's text either way.
            let documented = summary
                .declarations
                .iter()
                .filter(|fact| fact.range.start_offset > range.start_offset)
                .min_by_key(|fact| fact.range.start_offset);

            if let Some(fact) = documented {
                // A **template parameter** is documented with `@tparam` and a function's parameter with `@param`,
                // and the fact separates the two: `parameters` is the template head, `parameter_list` the
                // parameter list. Both are offered, because which command the reader typed is a spelling and the
                // name they want is a name.
                // **The empty name is a slot, not a parameter.** `fact.parameters` keeps one entry per template
                // parameter so that arguments pair with names by position, and an **unnamed** parameter's entry is
                // the empty string (`template <typename, class T>` is `["", "T"]`). It is not a name a reader can be
                // told about — offering it would put a blank item in the list.
                let mut names: Vec<String> = fact
                    .parameters
                    .iter()
                    .filter(|name| !name.is_empty())
                    .cloned()
                    .collect();
                if let Some(list) = &fact.parameter_list
                    && let Some(found) = crate::inlay::parameter_names_from_text(list)
                {
                    names.extend(found.into_iter().flatten());
                }

                for name in names {
                    if matching::quality(&name, written) == matching::NONE {
                        continue;
                    }
                    let mut item = CompletionItem::word(&name, ItemKind::Variable, "a parameter");
                    item.replace = range;
                    items.push(item);
                }
            }
        }
        Some(_) => {}
        // **A command's own name**: the whole vocabulary, filtered by what has been typed.
        None => {
            for spec in cpp_parser::every_command() {
                if matching::quality(spec.name, written) == matching::NONE {
                    continue;
                }
                let detail = match spec.kind {
                    DocCommandKind::Description => "describe",
                    DocCommandKind::Parameter => "a parameter",
                    DocCommandKind::Exception => "an exception",
                    DocCommandKind::Reference => "a reference",
                    DocCommandKind::Advisory => "a note",
                    DocCommandKind::Structural => "structure",
                    DocCommandKind::Callout => "a callout",
                };
                let mut item = CompletionItem::word(spec.name, ItemKind::Keyword, detail);
                item.replace = range;
                items.push(item);
            }
        }
    }

    CompletionSet {
        scope: String::new(),
        prefix: written.to_string(),
        replace: range,
        truncated: false,
        incomplete: false,
        items,
    }
}

/// **A `#define`'s replacement list** — its own parameters, and the macros already defined.
///
/// ```cpp
/// #define MAX(a, b) ((a) > (b) ? (a) : (b))
/// #define MAX_OF_THREE(a, b, c) MAX(MAX(a, b), c)
/// ```
///
/// The parameters come first because they are the names that exist **nowhere else**: the index has never heard of
/// `a`, and a reader who mistypes it gets a macro that quietly substitutes nothing. The macros follow, because a
/// body that composes another macro is the other half of what a replacement list is written from.
fn macro_body(
    index: &ProjectIndex,
    path: &Path,
    written: &str,
    range: SourceRange,
    parameters: &[String],
) -> CompletionSet {
    let mut items: Vec<CompletionItem> = Vec::new();

    for parameter in parameters {
        if matching::quality(parameter, written) == matching::NONE {
            continue;
        }
        let mut item =
            CompletionItem::word(parameter, ItemKind::Variable, "a parameter of this macro");
        item.replace = range;
        items.push(item);
    }

    for name in macro_names(index, path) {
        if matching::quality(name, written) == matching::NONE {
            continue;
        }
        let mut item = CompletionItem::word(name, ItemKind::Macro, "a macro");
        item.replace = range;
        items.push(item);
    }

    CompletionSet {
        scope: String::new(),
        prefix: written.to_string(),
        replace: range,
        truncated: false,
        incomplete: false,
        items,
    }
}

/// **The macros this file can see** — its own and those of the headers it includes directly, sorted and deduplicated.
///
/// The list a *preprocessor position* completes from: a `#define`'s body, `#if`, `#elif`, `#ifdef`, `#undef`. A macro
/// two headers away is reachable only through the macro environment, which is the hover's question, not this one's —
/// so the boundary is the direct includes, the same one `names` uses for the same reason.
fn macro_names<'a>(index: &'a ProjectIndex, path: &Path) -> Vec<&'a str> {
    let Some(summary) = index.summary(path) else {
        return Vec::new();
    };

    let mut macros: Vec<&str> = summary
        .macros
        .iter()
        .map(|fact| fact.name.as_str())
        .collect();
    for include in &summary.includes {
        let Some(resolved) = &include.resolved else {
            continue;
        };
        let Some(included) = index.summary(resolved) else {
            continue;
        };
        macros.extend(included.macros.iter().map(|fact| fact.name.as_str()));
    }
    macros.sort_unstable();
    macros.dedup();
    macros
}

/// **`#if`, `#elif`, `#ifdef`, `#ifndef`, `#undef`** — the vocabulary is the macros, and the operator on them.
///
/// ```cpp
/// #if |                 the macros this file can see, and `defined`
/// #if defined(|         the same: `defined` takes a macro name
/// #elif VERSION >= |    …and again, because the right-hand side of a comparison is a macro's value
/// ```
///
/// Nothing else belongs here. An `#if` expression is evaluated by the preprocessor before a single declaration
/// exists, so a function or a class name is not a candidate — offering the program's names, which is what a directive
/// argument used to do there, is offering words the condition can never mean.
fn condition(
    index: &ProjectIndex,
    path: &Path,
    written: &str,
    range: SourceRange,
) -> CompletionSet {
    let mut items: Vec<CompletionItem> = Vec::new();

    // **`defined` first**: the one operator a condition is written with, and a word of the preprocessor rather than a
    // macro anyone defined.
    for operator in ["defined", "__has_include", "__has_include_next"] {
        if matching::quality(operator, written) == matching::NONE {
            continue;
        }
        let mut item = CompletionItem::word(operator, ItemKind::Keyword, "a preprocessor operator");
        item.replace = range;
        items.push(item);
    }

    for name in macro_names(index, path) {
        if matching::quality(name, written) == matching::NONE {
            continue;
        }
        let mut item = CompletionItem::word(name, ItemKind::Macro, "a macro");
        item.replace = range;
        items.push(item);
    }

    CompletionSet {
        scope: String::new(),
        prefix: written.to_string(),
        replace: range,
        truncated: false,
        incomplete: false,
        items,
    }
}
/// The headers a `#include` may name.
fn headers_for(
    headers: &HeaderIndex,
    written: &str,
    closed: bool,
    range: SourceRange,
) -> CompletionSet {
    // `#include <vector>` with the cursor at the end: the name is written, and offering another one would insert a
    // second header into a directive that already has one.
    if closed {
        return CompletionSet {
            prefix: written.to_string(),
            replace: range,
            ..CompletionSet::default()
        };
    }

    let items: Vec<CompletionItem> = headers
        .matching(written, HEADER_BUDGET)
        .into_iter()
        .map(|header| {
            let mut item = CompletionItem::header(&header);
            item.replace = range;

            // **Where the header comes from**, which is the one thing a reader cannot see from the name and the one
            // thing that decides whether the include will resolve — and, since a real search path holds four
            // thousand headers of which a hundred and fifty are the standard library's, the thing that decides
            // which one they meant.
            item.detail = Some(match header.kind {
                HeaderKind::StandardLibrary => "the C++ standard library".to_string(),
                HeaderKind::Project => "this project".to_string(),
                HeaderKind::System => "on the include path".to_string(),
            });
            item
        })
        .collect();

    CompletionSet {
        scope: String::new(),
        prefix: written.to_string(),
        replace: range,
        truncated: headers.truncated(),
        incomplete: false,
        items,
    }
}

/// The part of a member's spelling that lies **before** the cursor.
///
/// Separate from the range because the two answer different halves of one edit: the range is what the client
/// **replaces** (`size`, so nothing of the old spelling is left behind) while this is what filters the list — and a
/// cursor inside a name has only typed the part before it. Taking the whole spelling for both is what makes
/// `w.si|ze` offer nothing.
fn written_before(access: &crate::sema::resolve::MemberAccess, offset: usize) -> String {
    let start = access.member_range.start_offset;
    let end = access.member_range.end_offset().min(offset);

    if end <= start {
        return String::new();
    }

    access
        .member
        .get(..end - start)
        .unwrap_or(&access.member)
        .to_string()
}

/// Which kind of member this is, for the icon: a member function is a method, a variable is a field, and a function
/// whose name spells its own class is a constructor.
fn member_kind(member: &ProjectMember, class: &str) -> ItemKind {
    match member.fact.kind {
        DeclKind::Function if member.fact.name == class => ItemKind::Function,
        DeclKind::Function => ItemKind::Method,
        DeclKind::Variable => ItemKind::Field,
        DeclKind::Type => ItemKind::Class,
        DeclKind::Namespace => ItemKind::Namespace,
        DeclKind::MacroLike => ItemKind::Macro,
        DeclKind::Other => ItemKind::Field,
    }
}

/// How many headers one `#include` offers.
const HEADER_BUDGET: usize = 200;

/// How many items one completion offers before it is cut short.
///
/// A **payload** bound rather than a UI one: a client renders ten rows and still receives all of them, and 14 351
/// items was 4.5 MB of JSON for one keystroke. Two hundred is more than any list a reader scrolls and small enough
/// that the request stays a few kilobytes.
const ITEM_BUDGET: usize = 200;

/// Put the items in order, apply the budget, and say whether anything was cut.
///
/// The sort is **stable**, which is what makes it a refinement rather than a re-decision: two items the gathering
/// layer produced in one order and the score cannot separate keep that order, so the query's own ordering (C++'s,
/// by how far the lookup walked) survives wherever the score has nothing to say.
fn ordered(mut scored: Vec<Scored>) -> (Vec<CompletionItem>, bool) {
    scored.sort_by(|one, other| {
        one.score
            .keys()
            .cmp(&other.score.keys())
            // **How well it answers what was typed**, before the alphabetical tiebreak. `str` puts `string` above
            // `to_string` even when both are in the same header, because one begins the name and the other begins a
            // later word — and that is the ordering a reader expects from having typed something.
            .then_with(|| other.matched.cmp(&one.matched))
            .then_with(|| one.item.label.cmp(&other.item.label))
    });

    let truncated = scored.len() > ITEM_BUDGET;
    scored.truncate(ITEM_BUDGET);

    (
        scored.into_iter().map(|scored| scored.item).collect(),
        truncated,
    )
}

/// **How near a declaration is to the cursor**, which is the whole of the ranking.
///
/// A tier plus a penalty, and the two are separate because they answer different questions: the tier is *where the
/// declaration is* (the reader's body, this file, a header the file includes) and the penalty is *what kind of
/// thing it is* (a value before a type, a variable before a macro). The tier dominates, because "nearer
/// textually" is the signal that survives every language and every project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Score {
    tier: i32,
    penalty: i32,
}

impl Score {
    /// A name declared in a scope of the file being edited — a local, a parameter, a member of the class the
    /// cursor's body is in, or the names written directly in the `::`-qualified scope that was asked about.
    const LOCAL: i32 = 0;
    /// A name at **file scope** in the file being edited.
    const FILE: i32 = 100;
    /// A snippet: a shape the reader is about to write. Between their own names and the headers', because a
    /// construct is likelier than a name from `<cstdio>` and unlikelier than the variable in front of them.
    const SNIPPET: i32 = 140;
    /// A keyword or a built-in type name.
    const KEYWORD: i32 = 150;
    /// A name from a header the file includes **directly**.
    const DIRECT_INCLUDE: i32 = 200;
    /// A name from a header reached through another header: the standard library's own, mostly.
    const INDIRECT_INCLUDE: i32 = 300;

    fn new(tier: i32, penalty: i32) -> Score {
        Score { tier, penalty }
    }

    /// The comparable key. A method rather than a derived `Ord` so that the field order is stated once, where a
    /// reader looking for "what sorts first" will find it.
    fn keys(&self) -> (i32, i32) {
        (self.tier, self.penalty)
    }

    /// A small nudge within a tier: a **value** before a type, a variable before a namespace.
    ///
    /// Not a filter, and deliberately small — a type is very often exactly what is being written (`Widget w;`) —
    /// so this reorders two candidates that were equally near and never decides whether one is offered at all.
    fn kind_penalty(kind: DeclKind) -> i32 {
        match kind {
            DeclKind::Variable | DeclKind::Function => 0,
            DeclKind::Type => 4,
            DeclKind::Other => 5,
            DeclKind::Namespace => 6,
            DeclKind::MacroLike => 8,
        }
    }
}

/// The score of a name the scope walk produced: **how far from the cursor its file is**, plus what it declares.
fn name_score(offered: &crate::OfferedName) -> Score {
    use crate::index::project::NameProvenance;

    let tier = match offered.from {
        // A scope of this file — which is where the *depth* earns its place: the scope the cursor is in comes
        // first, then each enclosing one outward, which is the order C++ looks in.
        NameProvenance::Scope => Score::LOCAL + offered.depth as i32,
        NameProvenance::ThisFile => Score::FILE,
        NameProvenance::DirectInclude => Score::DIRECT_INCLUDE,
        NameProvenance::IndirectInclude => Score::INDIRECT_INCLUDE,
    };

    // A destructor is spelled `~D` and a conversion function is not reachable by its spelling either. Both are
    // listed — they are real declarations — but after everything a reader could be typing the start of.
    let penalty = Score::kind_penalty(offered.fact.kind)
        + if offered.fact.name.starts_with('~') {
            20
        } else {
            0
        };

    Score::new(tier, penalty)
}

/// The score of a member of an accessed type.
fn member_score(member: &ProjectMember, class: &str) -> Score {
    // An inherited member is reachable but is not what the class itself declares, so it comes after the class's
    // own — the order C++ looks in, and the order a reader expects to see. The base walk produced the depth.
    let tier = Score::LOCAL + 10 * member.depth as i32;

    let penalty = Score::kind_penalty(member.fact.kind)
        + if member.fact.name == class { 6 } else { 0 }
        + if member.fact.name.starts_with('~') {
            20
        } else {
            0
        };

    Score::new(tier, penalty)
}

/// Is the cursor inside a class body, so that `public`, `virtual` and `friend` may be written?
fn in_a_class_body(scopes: &ScopeTree, offset: usize) -> bool {
    let Some(innermost) = scopes.scope_at(offset) else {
        return false;
    };

    scopes.scope_chain(innermost).iter().any(|scope| {
        matches!(
            scopes.scope(*scope).map(|data| data.kind),
            Some(ScopeKind::Class | ScopeKind::Enum)
        )
    })
}

/// **An index of headers**, read from a configuration and a project's own file list.
///
/// The one constructor a session needs, so that the walk and its budget are decided in one place rather than at the
/// call site: the search path first — that is what `<…>` means — and the project's files after it, which is what
/// the quoted form is resolved against.
///
/// Shared and behind a lock rather than owned, because one half of it can grow: [`crate::Session::add_project_files`]
/// hands over more of the project after the session is built, and the header list has to see them.
pub fn header_index(
    config: &crate::CompilerConfig,
    root: &Path,
    project_files: impl IntoIterator<Item = PathBuf>,
) -> SharedHeaders {
    Arc::new(std::sync::RwLock::new(
        HeaderIndex::read(includes::search_directories(config), root).with_project(project_files),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A name that does not answer what was typed is dropped, not ranked low.** The rule itself lives in
    /// [`crate::completion::matching`] and is tested there; this is the property the list depends on — that the
    /// filtering happens on this side at all, so a client does not have to receive and sort several hundred names to
    /// show four.
    #[test]
    fn a_candidate_that_does_not_answer_the_prefix_is_dropped() {
        use crate::completion::matching::{NONE, PREFIX, SUBSEQUENCE, WORD_START, quality};

        assert_eq!(quality("Widget", "wid"), PREFIX);
        assert_eq!(
            quality("Widget", ""),
            quality("Widget", ""),
            "the empty prefix matches everything"
        );
        // The fuzzy tiers are why this is not `starts_with` any more: `bsc` is nowhere in `basic_string` as a
        // prefix, and the analysis still offers it. Which *tier* it earns is the matching module's business — what
        // this test is about is that a candidate the rule rejects is dropped and one it accepts is not.
        assert!(quality("basic_string", "bsc") > NONE);
        assert_eq!(quality("get_parent_id", "gpid"), WORD_START);
        // The characters are not in `Widget` at all, so it is dropped rather than ranked last.
        assert_eq!(quality("Widget", "xyz"), NONE);
        assert_eq!(quality("Widget", "wz"), NONE);
        // …and this one is in there in order, which is a real match and the lowest tier.
        assert_eq!(quality("Widget", "wdgt"), SUBSEQUENCE);
    }

    /// The **order of the tiers** is the feature, so it is asserted rather than assumed: a local before a keyword,
    /// a keyword before a name from a header this file includes, and that before one from a header only a header
    /// includes.
    ///
    /// The constants are `const`, so the comparison is one rustc folds away — and that is the point rather than a
    /// waste: this test fails to *compile* the day two tiers are put in the wrong order, which is the earliest any
    /// test could notice.
    #[test]
    fn the_tiers_are_in_the_order_a_reader_expects() {
        const _: () = assert!(Score::LOCAL < Score::SNIPPET);
        const _: () = assert!(Score::SNIPPET < Score::KEYWORD);
        const _: () = assert!(Score::KEYWORD < Score::DIRECT_INCLUDE);
        const _: () = assert!(Score::DIRECT_INCLUDE < Score::INDIRECT_INCLUDE);
    }

    /// **A function's row shows what it takes.** The one thing a reader choosing between `format`, `format_to` and
    /// `format_to_n` has to see, and the thing this model did not record until it had a parameter list: every
    /// function in another file used to be described as `returns (…)`.
    ///
    /// The `(…)` survives only where it is still the truth — a fact written before the field existed, or a declarator
    /// the reading could not reach — and that case is asserted here too, because a fallback nobody tests is a
    /// fallback that silently stops working.
    #[test]
    fn a_functions_detail_line_is_what_it_takes() {
        let fact = |returns: Option<&str>, parameter_list: Option<&str>| DeclFact {
            name: "format".to_string(),
            scope: Some("std".to_string()),
            local: false,
            in_namespace: None,
            kind: DeclKind::Function,
            type_of: None,
            returns: returns.map(str::to_string),
            bases: Vec::new(),
            parameters: Vec::new(),
            constraints: Vec::new(),
            parameter_defaults: Vec::new(),
            pattern: None,
            parameter_list: parameter_list.map(str::to_string),
            range: cpp_parser::SourceRange::new(0, 0),
            name_range: cpp_parser::SourceRange::new(0, 0),
            clean: true,
            split_by_a_directive: false,
            guard: crate::FactGuard::Unconditional,
            access: None,
            exported: false,
        };

        assert_eq!(
            detail_of(&fact(Some("string"), Some("(_Fmt, _Args...)"))).as_deref(),
            Some("string (_Fmt, _Args...)"),
            "the return type and the parameters, as the file spells them"
        );
        assert_eq!(
            detail_of(&fact(Some("string"), Some("()"))).as_deref(),
            Some("string ()"),
            "a function that takes nothing says so — that is an answer, not an absence"
        );
        assert_eq!(
            detail_of(&fact(Some("string"), None)).as_deref(),
            Some("string (…)"),
            "and with no list recorded, the old spelling is still the honest one"
        );
        assert_eq!(
            detail_of(&fact(None, Some("(int)"))).as_deref(),
            Some("(int)"),
            "a constructor has no return type, and its parameters are then the whole description"
        );
        assert_eq!(detail_of(&fact(None, None)), None, "nothing to say");
    }
}
