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
pub mod table;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cpp_parser::{CppSyntaxNode, SourceRange};

pub use context::{CompletionContext, context_at};
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
            insert: fact.name.clone(),
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
fn detail_of(fact: &DeclFact) -> Option<String> {
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
}

impl Default for CompletionSet {
    fn default() -> CompletionSet {
        CompletionSet {
            items: Vec::new(),
            scope: String::new(),
            prefix: String::new(),
            replace: SourceRange::new(0, 0),
            truncated: false,
        }
    }
}

/// An item and the score that orders it.
///
/// The score is **not** a field of [`CompletionItem`]: a consumer renders items, and a number it must not re-sort
/// by is a field it would eventually misuse. Keeping the two together here means the order is decided once, in one
/// place, and thrown away at the same moment.
#[derive(Debug, Clone)]
struct Scored {
    score: Score,
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
) -> CompletionSet {
    match context_at(root, offset) {
        CompletionContext::Nothing => CompletionSet {
            replace: SourceRange::new(offset, 0),
            ..CompletionSet::default()
        },
        CompletionContext::Member(access) => members(index, scopes, root, path, &access, offset).set,
        CompletionContext::Qualified(position) => {
            names(index, scopes, root, path, &position, offset, false)
        }
        CompletionContext::Name(position) => {
            names(index, scopes, root, path, &position, offset, true)
        }
        CompletionContext::DirectiveName { written, range } => directives(&written, range),
        CompletionContext::DirectiveArgument { written, range } => {
            let position = crate::sema::resolve::NamePosition {
                scope: String::new(),
                written,
                range,
            };

            // `#define NAME`, `#undef NAME`, `#ifdef NAME`: the argument is a *name*, and what may be written is
            // what the program already declares. No keywords and no snippets — a macro's name is an identifier, and
            // a control-flow statement is not one — which is why this goes through the name query only.
            names(index, scopes, root, path, &position, offset, false)
        }
        CompletionContext::Include {
            written,
            closed,
            range,
            ..
        } => headers_for(headers, &written, closed, range),
    }
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
    offset: usize,
) -> String {
    match context_at(root, offset) {
        CompletionContext::Member(access) => members(index, scopes, root, path, &access, offset).why,
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
            items: Vec::new(),
        },
        why,
    };

    let written = match crate::index::project::type_of_expression(index, &mut |_: &Path| None, scopes, root, path, &access.object, 0) {
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
    let Some(class) = written.class_name() else {
        return refused(format!(
            "the type of `{}` is `{written}`, which names no class to list members of",
            access.object.text().to_string().trim()
        ));
    };

    let found = match crate::index::project::members_of(index, scopes, root, path, class) {
        Known::Yes(found) => found,
        Known::Unknown(reason) => {
            return MemberAnswer {
                set: CompletionSet {
                    scope: class.to_string(),
                    prefix,
                    replace,
                    truncated: false,
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
        let hidden =
            crate::index::project::offerable_members(index, scopes, root, path, offset, &mut members);
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
        item.kind = member_kind(member, class);
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

        scored.push(Scored {
            score: member_score(member, class),
            item,
        });
    }

    let (items, truncated) = ordered(scored);
    let offered = items.len();

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

    MemberAnswer {
        set: CompletionSet {
            scope: class.to_string(),
            prefix,
            replace,
            truncated,
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

                scored.push(Scored {
                    score: name_score(offered),
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
            if !starts_with(snippet.trigger, &listed_prefix) {
                continue;
            }
            scored.push(Scored {
                score: Score::new(Score::SNIPPET, 0),
                item: CompletionItem::snippet(snippet.trigger, snippet.detail, snippet.body),
            });
        }

        for keyword in table::keywords_for(statements, in_a_class) {
            // A snippet that writes the same word already stands for it: two items with one label and different
            // effects is a choice to make by accident.
            if table::a_snippet_writes_this(keyword.spelling)
                || !starts_with(keyword.spelling, &listed_prefix)
            {
                continue;
            }
            scored.push(Scored {
                score: Score::new(Score::KEYWORD, 0),
                item: CompletionItem::word(keyword.spelling, ItemKind::Keyword, keyword.detail),
            });
        }

        // The built-in type names that are not keywords (`size_t`, `char16_t`). They are names rather than
        // keywords because that is how a compiler reads them, and they are here because a file that never includes
        // `<cstddef>` still writes `size_t`.
        for (name, detail) in table::BUILTIN_TYPES {
            if !starts_with(name, &listed_prefix) {
                continue;
            }
            scored.push(Scored {
                score: Score::new(Score::KEYWORD, 0),
                item: CompletionItem::word(name, ItemKind::Class, detail),
            });
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
    }
}

/// The directives themselves, after a `#`.
fn directives(written: &str, range: SourceRange) -> CompletionSet {
    let items: Vec<CompletionItem> = table::DIRECTIVES
        .iter()
        .filter(|directive| starts_with(directive.name, written))
        .map(|directive| {
            let mut item = CompletionItem::word(directive.name, ItemKind::Keyword, directive.detail);
            item.replace = range;
            item
        })
        .collect();

    CompletionSet {
        scope: String::new(),
        prefix: written.to_string(),
        replace: range,
        truncated: false,
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
            .then_with(|| one.item.label.cmp(&other.item.label))
    });

    let truncated = scored.len() > ITEM_BUDGET;
    scored.truncate(ITEM_BUDGET);

    (scored.into_iter().map(|scored| scored.item).collect(), truncated)
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
        + if offered.fact.name.starts_with('~') { 20 } else { 0 };

    Score::new(tier, penalty)
}

/// The score of a member of an accessed type.
fn member_score(member: &ProjectMember, class: &str) -> Score {
    // An inherited member is reachable but is not what the class itself declares, so it comes after the class's
    // own — the order C++ looks in, and the order a reader expects to see. The base walk produced the depth.
    let tier = Score::LOCAL + 10 * member.depth as i32;

    let penalty = Score::kind_penalty(member.fact.kind)
        + if member.fact.name == class { 6 } else { 0 }
        + if member.fact.name.starts_with('~') { 20 } else { 0 };

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

/// Does this spelling begin with what has been typed, ignoring case?
///
/// Case-insensitive because C++ spells types `Widget` and a reader types `wid`; a **prefix** match rather than a
/// fuzzy one because the client does the fuzzy matching, and a server that dropped a candidate the client would
/// have ranked would be answering a different question than the one that was asked.
fn starts_with(spelling: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }

    let mut written = spelling.chars();
    prefix
        .chars()
        .all(|wanted| written.next().is_some_and(|have| have.eq_ignore_ascii_case(&wanted)))
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

    #[test]
    fn a_prefix_match_ignores_case_and_does_not_go_fuzzy() {
        assert!(starts_with("Widget", "wid"));
        assert!(starts_with("Widget", ""));
        assert!(starts_with("Widget", "Widget"));
        assert!(!starts_with("Widget", "idget"), "not a substring match");
        assert!(!starts_with("Widget", "wdgt"), "not a subsequence match");
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
            pattern: None,
            parameter_list: parameter_list.map(str::to_string),
            range: cpp_parser::SourceRange::new(0, 0),
            name_range: cpp_parser::SourceRange::new(0, 0),
            clean: true,
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

