//! Finding what is under a cursor, and what it refers to.
//!
//! This is the first *query* over the scope tree rather than a producer of it. Everything below answers "what did
//! the file write"; this answers "what is this name", which is a different kind of question and has a different
//! failure mode: a lookup that cannot be answered must say **why**, not return the nearest plausible binding.
//!
//! # What this asks, and what it deliberately does not
//!
//! ```text
//! asked:      which declaration does the name under this offset refer to, within this file?
//!             — including a `::`-qualified one, whose qualifier is resolved as a scope
//! not asked:  base classes, argument-dependent lookup, overload selection, `using` of a base member,
//!             anything reached through an `#include`
//! ```
//!
//! Those are not oversights, they are the next layers — and the reason they are named here is that a consumer
//! must be able to tell "not found" from "not looked for". Every answer is therefore a [`Known`]:
//! [`Known::Yes`] with the declaration, [`Known::No`] when the name is definitely not declared in this file's
//! reachable scopes, and [`Known::Unknown`] with the specific reason when the question was not answered.
//!
//! # Why the answer is three-valued here more than anywhere else
//!
//! A "go to definition" that jumps to the wrong place is worse than one that does nothing, because the user
//! believes it. C++ makes the wrong place easy to reach: a member `f` and a free function `f` differ by the name
//! of a class that may not even be in this file, and a name that is merely *not here* is usually declared in a
//! header the analysis has not read. So the two "no" answers are kept apart — [`UnknownReason::NotDeclaredHere`]
//! says the name is somewhere else, which is a fact about the file, while [`Known::No`] would say it is nowhere.

use cpp_parser::{CppSyntaxKind, CppSyntaxNode, SourceRange};

use crate::sema::symbol::{
    Binding, BindingKind, Known, Name, Scope, ScopeId, ScopeTree, UnknownReason,
};

/// The name written at `offset`, if the offset is inside one.
///
/// The *innermost* name node containing the offset, which is the one the user is pointing at: in `ns::Widget`,
/// an offset inside `Widget` gives `Widget` and not `ns::Widget`, because that is what a symbol search and a
/// rename both act on.
///
/// A `NameExpr` is the node a name is written as — see the parser's `parse_name` — and this walks the tree down to
/// the smallest one holding the offset. The descent is by containment rather than by a token search, so it does
/// not care how the offsets relate to tokens and whitespace.
pub fn name_node_at(root: &CppSyntaxNode, offset: usize) -> Option<CppSyntaxNode> {
    let mut found = None;
    let mut node = root.clone();

    loop {
        if !contains(&node, offset) {
            return found;
        }

        if is_a_name_node(&node) {
            found = Some(node.clone());
        }

        // The child that holds the offset. `children_with_tokens` rather than `children`, so that the walk
        // reaches a token as well as a node — without it an offset pointing exactly at an identifier would find
        // no child containing it and the descent would stop one level too high.
        let next = node
            .children_with_tokens()
            .find(|element| contains_element(element, offset));

        match next {
            Some(element) => match element.into_node() {
                Some(child) => node = child,
                // A token: the descent is over, and whatever `NameExpr` was last seen is the answer.
                None => return found,
            },
            None => return found,
        }
    }
}

/// Is this node a name a cursor can be on?
///
/// Two kinds, because the grammar has two and they are the same thing in different positions:
///
/// * [`CppSyntaxKind::NameExpr`] — a name in *declaration* position, and in a qualified name, produced by
///   `parse_name`;
/// * [`CppSyntaxKind::IdentifierExpr`] — a name in *expression* position, produced by `parse_primary_expr`.
///
/// Both are needed and neither is optional: a definition jump is asked from a use (`count = 1`, an
/// `IdentifierExpr`) as often as from the declaration it lands on (`int count`, a `NameExpr`). Reading only the
/// first is how the first version of this answered "unreadable name" for every use site in the file.
fn is_a_name_node(node: &CppSyntaxNode) -> bool {
    matches!(
        CppSyntaxKind::from(node.kind()),
        CppSyntaxKind::NameExpr | CppSyntaxKind::IdentifierExpr | CppSyntaxKind::IndexExpr
    )
}

/// The identifier the name node writes **at** `offset`: `Widget` for `ns::Widget`.
///
/// `offset` decides *which* identifier, and it has to be passed rather than assumed to be the last: a name node
/// can hold several. `ns::Widget` is one `NameExpr` holding `ns`, `::` and `Widget`, so "the last identifier"
/// happens to be right there — and is wrong for `ns::Widget<int>`, where a template argument list follows the
/// name, and for a cursor that is on the qualifier rather than on the name.
pub fn identifier_written_at(name: &CppSyntaxNode, offset: usize) -> Option<String> {
    let token = identifier_token_at(name, offset)?;
    Some(token.text().to_string())
}

/// The identifier token of a name node that holds `offset`.
fn identifier_token_at(name: &CppSyntaxNode, offset: usize) -> Option<cpp_parser::CppSyntaxToken> {
    name.children_with_tokens()
        .filter_map(|child| child.into_token())
        .find(|token| {
            cpp_parser::CppTokenKind::from(token.kind()) == cpp_parser::CppTokenKind::Identifier
                && contains_token(token, offset)
        })
}

/// The name the cursor at `offset` is on, with the range it occupies.
///
/// `None` when the offset is not inside a name — on punctuation, on a keyword, or past the end of the file. That
/// is the ordinary answer for most cursor positions, and a caller showing a hover has to treat it as "nothing to
/// say" rather than as a failure.
pub fn name_at(root: &CppSyntaxNode, offset: usize) -> Option<(String, SourceRange)> {
    let node = name_node_at(root, offset)?;

    // The range of the written identifier, which is what a selection and a rename act on. Taken from the token
    // rather than from the node: `ns::Widget`'s node covers the qualifier too, and highlighting the whole of it
    // when the user pointed at `Widget` is visibly wrong.
    let token = identifier_token_at(&node, offset)?;

    Some((
        token.text().to_string(),
        cpp_parser::source_range(token.text_range()),
    ))
}

/// The name the cursor is on, including when the cursor is inside a **preprocessor directive**.
///
/// [`name_at`] answers for the syntax the grammar builds name nodes for, and a directive is not part of it:
/// `#define FOO(x) …` is a run of tokens inside a `PreprocessorDirective`, so a cursor on `FOO` finds no name node
/// and [`name_at`] answers `None`. That is the wrong answer for the question a user asks by pointing at a macro's
/// name — "where is this used", "rename this" and "go to its definition" all start from the `#define`.
///
/// The fallback is the token under the cursor: if it is an `Identifier`, its spelling **is** the name. Nothing is
/// inferred from it, which is what keeps this honest: a `#define`'s name and a use of it are the same spelling by
/// construction, and a spelling nothing defines is rejected by whichever query reads it (there is no definition to
/// find, and no references to list).
///
/// The token search is a walk of the tree at the offset, not a scan of the file: this runs on every request that
/// has a cursor.
pub fn name_at_including_directives(
    root: &CppSyntaxNode,
    offset: usize,
) -> Option<(String, SourceRange)> {
    if let Some(found) = name_at(root, offset) {
        return Some(found);
    }

    let token = root
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| contains_token(token, offset))?;

    (cpp_parser::CppTokenKind::from(token.kind()) == cpp_parser::CppTokenKind::Identifier)
        .then(|| {
            (
                token.text().to_string(),
                cpp_parser::source_range(token.text_range()),
            )
        })
}

/// The name the cursor is on as the file **writes** it, which is what a lookup has to be given: `ns::Widget` for
/// an offset on `Widget` in `ns::Widget w;`, and `Widget` for one on a bare name.
///
/// # Why the qualifier has to be carried
///
/// `ns::Widget` is not a name to look up, it is a *scope* to find and then a name to look up in it — a different
/// question with a different answer, and one the index can answer too, because a declaration fact records the
/// qualified spelling of the scope it was written in. Dropping the qualifier would look for `Widget` among all the
/// names in scope, which is where a wrong jump comes from: `a::Widget` and `b::Widget` are both "a `Widget` in
/// scope" and only one of them is the one the user pointed at.
///
/// # The two spellings that are not a chain
///
/// A leading `::` is part of the spelling and is kept: `::Widget` asks about the global name space, and the
/// lookup honours it. Template arguments after the name are ignored, because they qualify the entity rather than
/// name it — the cursor on `Widget` in `ns::Widget<int>` asks about `ns::Widget`.
pub fn qualified_name_at(root: &CppSyntaxNode, offset: usize) -> Option<(String, SourceRange)> {
    let node = name_node_at(root, offset)?;
    let token = identifier_token_at(&node, offset)?;

    let mut written = String::new();
    let mut global = false;

    // The chain is written as flat tokens of one name node — see `identifier_written_at` — so this walks them in
    // order and stops at the identifier the cursor is on, which is what makes a cursor on a *qualifier* mean the
    // qualifier: `a::b::c` with the cursor on `b` asks about `a::b`.
    for element in node.children_with_tokens() {
        let Some(child) = element.into_token() else {
            continue;
        };

        match cpp_parser::CppTokenKind::from(child.kind()) {
            cpp_parser::CppTokenKind::Identifier => {
                if !written.is_empty() {
                    written.push_str("::");
                }
                written.push_str(child.text());

                if contains_token(&child, offset) {
                    return Some((
                        spell(global, &written),
                        cpp_parser::source_range(token.text_range()),
                    ));
                }
            }
            // A `::` before any identifier is a leading one, which is what asks about the global name space.
            cpp_parser::CppTokenKind::Scope if written.is_empty() => global = true,
            _ => {}
        }
    }

    // The cursor's identifier was not among the tokens, which cannot happen for a node that `identifier_token_at`
    // found it in — but returning the identifier alone is a better answer than `None` if it ever does.
    Some((
        spell(global, token.text()),
        cpp_parser::source_range(token.text_range()),
    ))
}

/// The name as written, with the leading `::` of a global name put back.
fn spell(global: bool, written: &str) -> String {
    if global {
        format!("::{written}")
    } else {
        written.to_string()
    }
}

/// A name **being written** at a cursor, split into the scope to list and what is already typed.
///
/// The state a completion asks from, and the reason it is not [`qualified_name_at`]: that function answers "which
/// name does this cursor point at", which requires a name to be *there*. Here the interesting states are the ones
/// with no name at all:
///
/// ```text
/// ns::            the scope is `ns`, and nothing of the next name is typed
/// ns::Wid         the scope is `ns`, and `Wid` is typed
/// ::Widget        the scope is the **global** name space, spelled `::`
/// loc             no scope at all: the names visible where the cursor is
/// ```
///
/// # The three fields, and why the range is one of them
///
/// `scope` is a **lookup key** rather than a spelling to display: the qualified name a scope is found by, with the
/// leading `::` of a global name kept because it means "the global name space" rather than "no qualifier" — the
/// same convention [`qualified_name_at`] and `decls::matches` use. Empty means what it says: no qualifier was
/// written, so the answer is every name visible from the cursor.
///
/// `written` is what of the *next* segment is already typed (`Wid` for `ns::Wid`), which a consumer filters by and
/// [`NamePosition::range`] is what it **replaces**. The two are separate for the reason `MemberCompletions`
/// documents: a cursor in the middle of a name — `ns::Wi|d` — filters by `Wi` while the replacement covers all of
/// `Wid`, and a consumer that used one for the other would offer nothing while the user is plainly typing.
///
/// The range is **empty at the cursor** when the segment has only just started (`ns::`), which is the state the
/// whole reader exists for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamePosition {
    /// The scope to list names from: `ns`, `ns::Widget`, `::` for the global name space, or empty for "wherever
    /// the cursor is".
    pub scope: String,
    /// What of the name is written **before the cursor**.
    pub written: String,
    /// The range a consumer replaces with the chosen name.
    pub range: SourceRange,
}

/// Is the cursor at or inside the names being written at `offset`?
pub fn name_position_at(root: &CppSyntaxNode, offset: usize) -> Option<NamePosition> {
    let node = name_node_around(root, offset)?;

    let mut segments: Vec<String> = Vec::new();
    let mut written = String::new();
    let mut global = false;
    let mut range = None;

    // The chain is written as flat tokens of one node — see [`identifier_written_at`] — so this walks them in
    // order and stops at the cursor. Trivia is stepped over rather than treated as an end: `ns :: Wid` is the same
    // name as `ns::Wid`, and the grammar is what said so when it built one node out of them.
    for token in node.children_with_tokens().filter_map(|child| child.into_token()) {
        let token_range = cpp_parser::source_range(token.text_range());

        if cpp_parser::is_trivia(cpp_parser::CppTokenKind::from(token.kind())) {
            continue;
        }
        // Everything from here on is **after** the cursor, so it is not part of what is being written — and it is
        // not part of the scope either. This is what keeps a recovery from answering: `ns::` at the end of a body
        // has the closing `}` inside the same node, and a reader that walked past it would report a name that is
        // not there.
        if token_range.start_offset >= offset {
            break;
        }

        match cpp_parser::CppTokenKind::from(token.kind()) {
            cpp_parser::CppTokenKind::Identifier => {
                if token_range.end_offset() <= offset {
                    written.push_str(token.text());
                    range = Some(token_range);
                    continue;
                }

                // The cursor is inside this identifier: the part before it is what is typed.
                written.push_str(&token.text()[..offset - token_range.start_offset]);
                range = Some(token_range);
                break;
            }
            cpp_parser::CppTokenKind::Scope => {
                if written.is_empty() && segments.is_empty() {
                    // A leading `::` — the global name space, which is a scope and not an empty qualifier.
                    global = true;
                } else {
                    segments.push(std::mem::take(&mut written));
                }
                // A fresh segment: the chosen name goes where the `::` ends, at the cursor.
                range = None;
            }
            // Anything else — a `(`, an operator, a token a recovery put here — ends the name.
            _ => break,
        }
    }

    Some(NamePosition {
        scope: spell(global, &segments.join("::")),
        written,
        range: range.unwrap_or(SourceRange {
            start_offset: offset,
            length: 0,
        }),
    })
}

/// The name node a cursor is **in or at the end of**, innermost first.
///
/// [`name_node_at`] asks this question of a cursor that is *on* a name and answers with the name node it lands in.
/// Completion asks it one keystroke earlier, when the cursor is at the end of what has been typed — `loc|` — or
/// past a `::` with nothing after it, so an offset exactly at the end of a node has to count as being on it. The
/// containment test is therefore inclusive at both ends, which is the same one [`crate::ScopeTree::scope_at`] uses
/// and for the same reason.
///
/// `IndexExpr` is deliberately **not** a name node here, even though [`is_a_name_node`] counts it: `w.size` is a
/// member access, and the query for that shape is [`member_access_at`]. What this reads is the other position — a
/// name, or a qualifier ending in `::` — and a reader that claimed both would answer a member completion with the
/// names in scope.
fn name_node_around(root: &CppSyntaxNode, offset: usize) -> Option<CppSyntaxNode> {
    let mut found = None;
    let mut node = root.clone();

    loop {
        let range = node.text_range();
        if !(usize::from(range.start()) <= offset && offset <= usize::from(range.end())) {
            return found;
        }

        if matches!(
            CppSyntaxKind::from(node.kind()),
            CppSyntaxKind::NameExpr | CppSyntaxKind::IdentifierExpr
        ) {
            found = Some(node.clone());
        }

        let next = node
            .children_with_tokens()
            .find(|element| contains_element(element, offset) || ends_at(element, offset));

        match next.and_then(|element| element.into_node()) {
            Some(child) => node = child,
            None => return found,
        }
    }
}

/// Does this element end exactly at `offset`, one past its last token?
fn ends_at(element: &cpp_parser::CppSyntaxElement, offset: usize) -> bool {
    let range = element.text_range();
    usize::from(range.end()) == offset && usize::from(range.start()) < offset
}

/// A member access the cursor is in: `widget` and `size` of `widget.size`.
///
/// The first query in this crate that has to ask about a **type** rather than a name. `size` is not looked up
/// anywhere: it is looked up *in the type of `widget`*, so the answer needs the object, its type, and the member
/// — and this is the half that reads the shape off the syntax.
///
/// # The member may not be written yet
///
/// `w.` is how a member access is asked about most often — the keystroke that *is* the question — and the parser
/// reads it as an access with nothing after the operator rather than as a different construct. So
/// [`MemberAccess::member`] is empty in that state and [`MemberAccess::member_range`] is an empty range just past
/// the operator, which is where a client inserts the chosen name. A caller with nothing to do about a nameless
/// access has to say so itself: [`crate::index::project::member_across_files`] does, because there is no member to
/// jump to, while the completion query is asking exactly for this state.
#[derive(Debug, Clone)]
pub struct MemberAccess {
    /// The expression left of the `.` or `->`: the thing whose type decides what the member is.
    pub object: CppSyntaxNode,
    /// The member's spelling, as written — **empty** when only the operator has been typed.
    pub member: String,
    /// The member's range, for a selection or a rename. Empty and past the operator when nothing is written.
    pub member_range: SourceRange,
}

/// The member access the cursor at `offset` is in, if it is in one.
///
/// The **innermost** one, which is what makes `a.b.c` answer about `c`: the tree nests `(a.b).c`, and a cursor on
/// the last name is asking about the member of the member.
///
/// `None` for every other position — a plain name, an operator, a declaration — which is the ordinary answer for
/// most cursor positions. A cursor immediately after a `.` or `->` **is** in a member access — see
/// [`MemberAccess`] — with an empty member.
pub fn member_access_at(root: &CppSyntaxNode, offset: usize) -> Option<MemberAccess> {
    let node = member_node_at(root, offset)?;
    let access = member_access_of(&node)?;

    // A cursor on the *object* of `a.b` is asking about `a`, which is the name query's question rather than this
    // one's — so the offset decides whether this is an answer at all, and the node only decides what the shape is.
    // For a nameless access the range is empty and sits just past the operator, so this accepts exactly the one
    // offset that means "here is where the member goes" and not the whole line it is on.
    contains_range(access.member_range, SourceRange::new(offset, 0)).then_some(access)
}

/// The shape of a node that **is** a member access: its object, its member, and where the member was written.
///
/// Separate from [`member_access_at`] because the question comes in two forms: a *cursor* is on a member of some
/// node (`offset` decides which), while an *expression* that is being typed is a node of its own. The second form
/// is what makes inference recursive — `a.b.size` needs the shape of `a.b` with nobody's cursor on it.
///
/// `None` for a node that is not a member access: `arr[0]` is the same node kind with brackets instead of a dot.
/// An access whose member is not written yet is **not** `None` — see [`MemberAccess`].
pub fn member_access_of(node: &CppSyntaxNode) -> Option<MemberAccess> {
    let elements: Vec<cpp_parser::CppSyntaxElement> = node.children_with_tokens().collect();

    // The operator, found by its text rather than by a token kind: the grammar gives `.` and `->` kinds of their
    // own, and a shape reader that had to know their names would be a second place to update if either changed.
    let operator = elements.iter().position(|element| {
        element
            .as_token()
            .is_some_and(|token| token.text() == "." || token.text() == "->")
    })?;
    let operator_range = elements[operator].as_token()?.text_range();

    // The object is the first **node** before the operator. A node rather than a token, because that is what a
    // type has to be inferred *from*: `(*p)`, `make()` and `a.b` are all nodes, and the inference decides which
    // shapes it can type.
    let object = elements[..operator]
        .iter()
        .find_map(|element| element.as_node().cloned())?;

    // The member is the identifier **after** the operator — the last one in text order, because the object of a
    // nested access is itself a member access and its identifiers come first.
    let after_operator: Vec<cpp_parser::CppSyntaxToken> = node
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| {
            cpp_parser::CppTokenKind::from(token.kind()) == cpp_parser::CppTokenKind::Identifier
                && token.text_range().start() >= operator_range.end()
        })
        .collect();

    // Nothing after the operator is a real state and not a failure: `w.` is a member access with the member still
    // to be written. The empty range sits at the operator's end, which is exactly where the name goes — so a
    // caller that inserts, replaces or drops text has one range to work with in both states.
    let Some(member_token) = after_operator.into_iter().next_back() else {
        return Some(MemberAccess {
            object,
            member: String::new(),
            member_range: SourceRange::new(usize::from(operator_range.end()), 0),
        });
    };

    Some(MemberAccess {
        object,
        member: member_token.text().to_string(),
        member_range: cpp_parser::source_range(member_token.text_range()),
    })
}

/// The innermost member-access node holding `offset`.
///
/// Three kinds, because the grammar reaches a member access in three ways and only one of them is named for it:
/// `MemberExpr` and `ArrowExpr` exist, and the parser also produces an **`IndexExpr`** for `w.size` — the same
/// node it uses for `arr[0]`, with a `.` where the brackets would be. What makes a node a member access is
/// therefore the *operator*, which is why the caller checks for it by text rather than trusting the kind.
fn member_node_at(root: &CppSyntaxNode, offset: usize) -> Option<CppSyntaxNode> {
    let mut found = None;
    let mut node = root.clone();

    loop {
        if !contains(&node, offset) {
            return found;
        }

        if matches!(
            CppSyntaxKind::from(node.kind()),
            CppSyntaxKind::MemberExpr | CppSyntaxKind::ArrowExpr | CppSyntaxKind::IndexExpr
        ) {
            found = Some(node.clone());
        }

        match node
            .children_with_tokens()
            .find(|element| contains_element(element, offset))
            .and_then(|element| element.into_node())
        {
            Some(child) => node = child,
            None => return found,
        }
    }
}

/// Which declaration the name written at `offset` refers to.
///
/// The go-to-definition question, answered within one file's scope tree. See the module documentation for what
/// this does not look at, and why every answer is a [`Known`].
///
/// # Which declaration wins
///
/// **The first one in the nearest scope that declares the name.** Not "the best match", because choosing between
/// overloads is a different question that needs argument types, and not "all of them", because a consumer with
/// one cursor position and several declarations has to pick anyway — better here, where the order is declaration
/// order and is documented, than in every caller.
///
/// So `void f(); void f(int);` jumps to the first, and a local `x` shadowing a member `x` jumps to the local,
/// which is what a reader expects of each.
///
/// # A qualified name is a different question
///
/// `ns::Widget` is not looked for among the names in scope: the qualifier is a **scope** to find
/// ([`ScopeTree::scope_with_qualified_name`]) and `Widget` is then a name to look up *in it*. Doing it the other
/// way — dropping the qualifier and looking for `Widget` — is not a smaller version of the right answer, it is a
/// wrong one: `a::Widget` and `b::Widget` are both "a `Widget` in scope", and only one of them is the one the
/// cursor is on.
///
/// A **relative** qualifier is tried against each enclosing scope in turn, innermost first, which is the shape of
/// C++'s own rule (the first component is found by ordinary lookup, and the rest descends from it). Where it
/// differs is stated rather than hidden: the language looks up only the *first component* outward and then
/// descends without further outward search, while this tries the whole qualifier per enclosing scope — which can
/// find a declaration the language would not, and cannot miss one it would.
///
/// Nothing found locally is reported as [`UnknownReason::NotDeclaredHere`] carrying the **qualified** spelling,
/// which is what lets the cross-file layer answer: a declaration fact records the qualified name of the scope it
/// was written in, so `ns::Widget` is a spelling an index can be asked about.
pub fn definition_at(scopes: &ScopeTree, root: &CppSyntaxNode, offset: usize) -> Known<Binding> {
    let Some((written, _)) = qualified_name_at(root, offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    // The scope chain starts at the innermost scope *containing the offset*, and the scopes that merely enclose
    // the position are added as well — see `scopes_to_search` for why both.
    let Some(innermost) = scopes.scope_at(offset) else {
        return Known::No;
    };

    if written.contains("::") {
        return qualified_definition_at(scopes, innermost, &written);
    }

    let name = Name::identifier(written.clone());

    // The scopes a `using namespace` directive brings into view, resolved **once** before the walk: a directive
    // names a namespace, and finding which scope that namespace is has nothing to do with where the search has
    // got to. Searching per scope would also have to decide what to do when the same directive is seen twice.
    let mut pending = using_directive_targets(scopes);
    pending.extend(scopes_to_search(scopes, innermost));



    let mut visited = Vec::new();
    let mut searched_any_scope = false;


    while let Some(scope) = pending.pop() {
        if visited.contains(&scope) {
            continue;
        }
        visited.push(scope);
        searched_any_scope = true;

        let Some(scope_data) = scopes.scope(scope) else {
            continue;
        };

        if let Some(binding) = first_binding_of(scope_data, &name) {
            return Known::Yes(binding.clone());
        }
    }

    if !searched_any_scope {
        return Known::No;
    }

    // The name is not in any scope reachable from here, which within one file is the end of the road: it is
    // declared in another file, and no include has been followed. `No` would be a claim this layer cannot make.
    Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(written)))
}

/// Which declaration a `::`-qualified name written at a position refers to, within one file.
///
/// `written` is the spelling as the file writes it, with a leading `::` for the global name space. See
/// [`definition_at`] for the rule and for where it is deliberately wider than the language.
fn qualified_definition_at(scopes: &ScopeTree, innermost: ScopeId, written: &str) -> Known<Binding> {
    // A leading `::` asks about the global name space, which is the file scope and nothing else — so there is
    // exactly one spelling to try rather than one per enclosing scope.
    let (global, spelling) = match written.strip_prefix("::") {
        Some(rest) => (true, rest),
        None => (false, written),
    };

    let segments: Vec<&str> = spelling.split("::").collect();
    let (last, qualifier) = match segments.split_last() {
        Some((last, rest)) if !rest.is_empty() => (*last, rest.join("::")),
        // A spelling with no qualifier is not this function's question, and `a::` cannot be written.
        _ => return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(written))),
    };

    // The scopes to anchor the qualifier in, innermost first: the scopes that contain the cursor, or the file
    // scope alone when the spelling asked for the global one.
    let anchors: Vec<Option<String>> = if global {
        vec![None]
    } else {
        scopes
            .scope_chain(innermost)
            .into_iter()
            .map(|scope| scopes.qualified_name_of(scope))
            .collect()
    };

    let name = Name::identifier(last.to_string());

    for anchor in anchors {
        let candidate = match anchor {
            Some(prefix) => format!("{prefix}::{qualifier}"),
            None => qualifier.clone(),
        };

        let Some(scope) = scopes.scope_with_qualified_name(&candidate) else {
            continue;
        };
        let Some(scope_data) = scopes.scope(scope) else {
            continue;
        };

        if let Some(binding) = first_binding_of(scope_data, &name) {
            return Known::Yes(binding.clone());
        }
    }

    // Nothing here declares it, so the qualified spelling goes to the layer that can look outside this file.
    Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(written)))
}

/// The namespace scopes that a `using namespace <name>;` directive makes visible, if this file writes one.
///
/// A directive is recorded as a binding of the namespace's name — see [`BindingKind::UsingDirective`] — so what
/// this has to do is find the **namespace scope** that name refers to, which is a different thing from the
/// binding: the binding says where the directive is, the scope says what it makes visible.
///
/// The target is looked for as a scope whose *own* name is the one written, which is what a namespace scope
/// records. That deliberately does not follow a chain of directives: `using namespace a; using namespace b;`
/// where `b` is inside `a` is legal and does bring `a`'s names in, and following it would need a worklist with
/// its own cycle handling — the recursion here is bounded by the number of scopes because a scope is only ever
/// added to `pending` once it is found.
fn using_directive_targets(scopes: &ScopeTree) -> Vec<ScopeId> {
    // The names the file's `using namespace` directives name — `ns` in `using namespace ns;`. Read from the
    // bindings rather than from a syntax walk, because the directive is already recorded as a binding of
    // exactly that name; see [`BindingKind::UsingDirective`].
    let named: Vec<String> = scopes
        .scopes()
        .iter()
        .flat_map(|scope| scope.bindings.iter())
        .filter(|binding| binding.kind == BindingKind::UsingDirective)
        .map(|binding| binding.name.text())
        .collect();

    if named.is_empty() {
        return Vec::new();
    }

    // The namespace scope each of those names refers to: a scope that records *itself* as introducing the name,
    // which is what a namespace scope does. Matching on the scope's own name rather than on a binding is what
    // keeps the directive's own binding — which is in the enclosing scope and declares nothing — out of the
    // answer.
    //
    // # What this over-approximates
    //
    // Every namespace any directive in the file names, rather than only the ones whose directive is in a scope
    // the cursor can see. So a `using namespace` inside an unrelated function makes its namespace's names
    // visible to a search elsewhere in the file. That is deliberate for now and recorded here as a *known*
    // over-approximation rather than a bug: restricting it means threading the cursor's scope chain into this
    // function, and the cost of the leniency is one wrong jump in a file that has two directives for different
    // namespaces — while the cost of getting it wrong the other way is a name that is plainly visible being
    // reported as undeclared.
    scopes
        .scopes()
        .iter()
        .enumerate()
        .filter(|(_, scope)| {
            scope.kind == crate::ScopeKind::Namespace
                && scope
                    .name
                    .as_deref()
                    .is_some_and(|name| named.iter().any(|named| named == name))
        })
        .map(|(index, _)| ScopeId(index))
        .collect()
}

/// The scopes to try, as a stack whose **last** element is the innermost.
///
/// Two parts, and both are needed:
///
/// * the **scope chain** from the innermost scope, which is C++ ordinary lookup;
/// * the scopes that *contain the offset*, outward, because a name is often written outside the scope its
///   declaration will open — the cursor on `x` in `int x = 1;` sits in the enclosing scope, since the variable's
///   own scope does not exist yet.
///
/// The second is what makes a definition jump work *from inside the declaration itself*, which is where a cursor
/// most often is when the question is asked.
///
/// # Why the order is spelled out
///
/// The caller pops, so the ordering here decides **which declaration wins** — and getting it backwards is not a
/// crash, it is a jump to the file-scope `count` where a local one shadows it. The first version put the chain
/// on the stack first and the enclosing scopes on top, which made the *outermost* scope win every time: the
/// shadowing test caught it, and nothing else would have.
fn scopes_to_search(scopes: &ScopeTree, innermost: ScopeId) -> Vec<ScopeId> {
    // Outermost first, so that after the reversal below the innermost of these is popped first.
    let mut enclosing: Vec<ScopeId> = scopes
        .scopes()
        .iter()
        .enumerate()
        .filter_map(|(index, scope)| {
            let range = scope.range?;
            contains_range(range, scopes.scope(innermost)?.range?).then_some(ScopeId(index))
        })
        .collect();
    enclosing.sort_by_key(|id| {
        std::cmp::Reverse(
            scopes
                .scope(*id)
                .and_then(|scope| scope.range)
                .map(|range| range.length),
        )
    });

    // Innermost first, then reversed onto the end of the stack — see the ordering note above.
    let mut chain = scopes.scope_chain(innermost);
    chain.reverse();

    enclosing.extend(chain);
    enclosing
}

/// The first binding of `name` in `scope`, in declaration order.
fn first_binding_of<'a>(scope: &'a Scope, name: &Name) -> Option<&'a Binding> {
    scope
        .bindings_of(name)
        // A using-*directive* binds the namespace's name to record that the line exists, not to declare
        // anything — see [`BindingKind::UsingDirective`] — so it is not a definition to jump to.
        .find(|binding| binding.kind != BindingKind::UsingDirective)
}

/// Is `offset` inside this node's range?
fn contains(node: &CppSyntaxNode, offset: usize) -> bool {
    let range = node.text_range();
    offset >= usize::from(range.start()) && offset < usize::from(range.end())
}

/// Is `offset` inside this element's range?
fn contains_element(element: &cpp_parser::CppSyntaxElement, offset: usize) -> bool {
    let range = element.text_range();
    offset >= usize::from(range.start()) && offset < usize::from(range.end())
}

/// Is offset inside this token?
fn contains_token(token: &cpp_parser::CppSyntaxToken, offset: usize) -> bool {
    let range = token.text_range();
    offset >= usize::from(range.start()) && offset < usize::from(range.end())
}

/// Does `outer` contain `inner`?
fn contains_range(outer: SourceRange, inner: SourceRange) -> bool {
    outer.start_offset <= inner.start_offset && outer.end_offset() >= inner.end_offset()
}

#[cfg(test)]
mod tests {
    use super::{definition_at, identifier_written_at, name_at, name_node_at, qualified_name_at};
    use crate::sema::scopes::build_scopes;
    use crate::sema::symbol::{BindingKind, Known, UnknownReason};
    use cpp_parser::{CppParser, ParserConfig};

    fn tree(source: &str) -> cpp_parser::CppSyntaxTree {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(tree.get_errors(), [], "the input must parse cleanly: {source:?}");
        tree
    }

    /// The offset of the **last** `needle`, or a panic naming the source.
    ///
    /// Text rather than a hand-written offset, because an offset is the thing that goes stale when a fixture
    /// changes. The *last* occurrence, because nearly every fixture below declares a name before using it and the
    /// question being asked is about the use — with `find` the cursor landed on the declaration, and
    /// `int count = 0;` reported the declaration as its own definition without the lookup running at all.
    fn at(source: &str, needle: &str) -> usize {
        source
            .rfind(needle)
            .unwrap_or_else(|| panic!("{needle:?} is not in {source:?}"))
    }

    fn definition_of(source: &str, needle: &str) -> Known<crate::Binding> {
        let parsed = tree(source);
        let root = parsed.get_red_root();
        let scopes = build_scopes(&root);

        definition_at(&scopes, &root, at(source, needle))
    }

    #[test]
    fn the_name_under_the_cursor_is_the_innermost_one() {
        let source = "ns::Widget value;\n";
        let parsed = tree(source);
        let root = parsed.get_red_root();

        let (name, range) = name_at(&root, at(source, "Widget")).expect("Widget is a name");
        assert_eq!(name, "Widget");
        assert_eq!(
            &source[range.start_offset..range.end_offset()],
            "Widget",
            "the range is the identifier, not the whole qualified name"
        );

        let (qualifier, _) = name_at(&root, at(source, "ns")).expect("ns is a name");
        assert_eq!(qualifier, "ns");
    }

    #[test]
    fn punctuation_and_the_end_of_the_file_are_not_names() {
        let source = "int x = 1;\n";
        let parsed = tree(source);
        let root = parsed.get_red_root();

        assert!(name_at(&root, at(source, "=")).is_none(), "an operator");
        assert!(name_at(&root, at(source, "1")).is_none(), "a literal");
        assert!(name_at(&root, source.len() + 10).is_none(), "past the end");
    }

    #[test]
    fn a_qualified_name_is_read_one_segment_at_a_time() {
        let source = "ns::Widget value;\n";
        let parsed = tree(source);
        let root = parsed.get_red_root();

        let widget = name_node_at(&root, at(source, "Widget")).expect("a name node");
        assert_eq!(identifier_written_at(&widget, at(source, "Widget")).as_deref(), Some("Widget"));
    }

    #[test]
    fn a_local_definition_is_found_from_a_use() {
        let source = "void f() {\n  int count = 0;\n  count = 1;\n}\n";
        let found = definition_of(source, "count = 1;");

        let Known::Yes(binding) = found else {
            panic!("the local must resolve: {found:?}");
        };
        assert_eq!(binding.kind, BindingKind::Variable);
        assert_eq!(binding.name.identifier_text(), Some("count"));
    }

    #[test]
    fn the_innermost_declaration_of_a_name_wins() {
        // The local shadows the file-scope one, which is what a reader expects a jump to do.
        let source = "int count = 0;\nvoid f() {\n  int count = 1;\n  count = 2;\n}\n";
        let found = definition_of(source, "count = 2;");

        let Known::Yes(binding) = found else {
            panic!("the local must resolve: {found:?}");
        };
        assert!(
            binding.range.start_offset > at(source, "void f"),
            "the jump goes to the local, not to the file-scope declaration"
        );
    }

    #[test]
    fn a_definition_is_found_from_inside_the_declaration_itself() {
        // The cursor on the name in `int count = 0;` — the position a user is in when they have just typed it.
        let source = "void f() {\n  int count = 0;\n}\n";
        let found = definition_of(source, "count = 0;");

        let Known::Yes(binding) = found else {
            panic!("the declaration must resolve to itself: {found:?}");
        };
        assert_eq!(binding.name.identifier_text(), Some("count"));
    }

    #[test]
    fn a_member_is_found_from_a_use_inside_the_class() {
        let source = "struct Widget {\n  int size;\n  int get() { return size; }\n};\n";
        let found = definition_of(source, "size; }");

        let Known::Yes(binding) = found else {
            panic!("the member must resolve: {found:?}");
        };
        assert_eq!(binding.name.identifier_text(), Some("size"));
        assert_eq!(binding.kind, BindingKind::Variable);
    }

    #[test]
    fn a_namespace_member_is_found_from_a_use_inside_the_namespace() {
        // The use is written *inside* the namespace, which is where unqualified access to its members works.
        // The same name written at file scope would not be valid C++ at all — unqualified `count` is not visible
        // there — so a test written that way would be asserting the wrong answer. That case is
        // `a_using_directive_makes_a_namespace_reachable` below, and the fully-qualified one is deliberately not
        // supported yet; see the module documentation.
        let source = "namespace ns {\n  int count = 0;\n  void f() {\n    count = 1;\n  }\n}\n";
        let found = definition_of(source, "count = 1;");

        let Known::Yes(binding) = found else {
            panic!("the namespace member must resolve: {found:?}");
        };
        assert_eq!(binding.name.identifier_text(), Some("count"));
        assert!(
            binding.range.start_offset < at(source, "void f"),
            "the jump goes to the declaration above the function"
        );
    }

    #[test]
    fn the_written_name_is_the_whole_qualifier_up_to_the_cursor() {
        // What the lookup is given, asserted directly: this function decides which question every other test in
        // this file is asking, and getting it wrong is not a crash but a different question with a different
        // answer — `Widget` instead of `ns::Widget` is exactly the jump to the wrong entity.
        let source = "namespace ns {\n  struct Widget { int x; };\n}\n\
                      ::Global g;\n\
                      ns::Widget<int> w;\n";
        let root = tree(source).get_red_root();

        let at_widget = |needle: &str| {
            let offset = at(source, needle);
            qualified_name_at(&root, offset).map(|(written, _)| written)
        };

        assert_eq!(
            at_widget("Widget<int> w;").as_deref(),
            Some("ns::Widget"),
            "the template argument list is not part of the name"
        );
        assert_eq!(
            at_widget("ns::Widget<int>").as_deref(),
            Some("ns"),
            "a cursor on the qualifier asks about the qualifier"
        );
        assert_eq!(
            at_widget("Global g;").as_deref(),
            Some("::Global"),
            "a leading `::` is part of the spelling: it asks about the global name space"
        );
        assert_eq!(
            at_widget("x; };").as_deref(),
            Some("x"),
            "and a bare name is still a bare name"
        );
    }

    #[test]
    fn a_qualified_name_resolves_into_its_namespace() {
        // This test used to assert the opposite, on purpose: it was written to fail the day qualified resolution
        // landed, so that the change could not happen by accident. It landed, and this is what it bought —
        // `ns::count` from file scope used to be `Unknown`, and it is now the declaration it names.
        let source = "namespace ns {\n  int count = 0;\n}\nvoid f() {\n  ns::count = 1;\n}\n";
        let found = definition_of(source, "count = 1;");

        let Known::Yes(binding) = found else {
            panic!("the qualifier names a namespace this file declares: {found:?}");
        };
        assert_eq!(binding.name.identifier_text(), Some("count"));
        assert!(
            binding.range.start_offset < at(source, "void f"),
            "and the jump goes into the namespace, not to the use"
        );
    }

    #[test]
    fn a_qualifier_picks_the_right_namespace_when_the_name_is_in_two_of_them() {
        // The reason qualification is not a detail: `Widget` is declared in both, so an implementation that dropped
        // the qualifier and looked for `Widget` would answer with whichever came first in the file. That is a wrong
        // jump rather than a missing one, which is the class of answer this project refuses to give.
        let source = "namespace a {\n  struct Widget { int from_a; };\n}\n\
                      namespace b {\n  struct Widget { int from_b; };\n}\n\
                      void f() {\n  b::Widget w;\n}\n";
        let found = definition_of(source, "Widget w;");

        let Known::Yes(binding) = found else {
            panic!("`b::Widget` is the one in `b`");
        };
        assert!(
            binding.range.start_offset > at(source, "namespace b"),
            "the jump lands in `b`, not in `a`: {:?}",
            binding.range
        );
    }

    #[test]
    fn a_nested_qualifier_walks_every_segment() {
        let source = "namespace a {\n  namespace b {\n    int count = 0;\n  }\n}\n\
                      void f() {\n  a::b::count = 1;\n}\n";
        let found = definition_of(source, "count = 1;");

        let Known::Yes(binding) = found else {
            panic!("both segments name scopes this file declares: {found:?}");
        };
        assert_eq!(binding.name.identifier_text(), Some("count"));
    }

    #[test]
    fn a_relative_qualifier_is_anchored_in_the_enclosing_scope() {
        // `b::count` written *inside* `a` means `a::b::count`: the qualifier is relative to where it is written,
        // which is why the search tries each enclosing scope rather than only the whole spelling.
        let source = "namespace a {\n  namespace b {\n    int count = 0;\n  }\n  void f() {\n    b::count = 1;\n  }\n}\n";
        let found = definition_of(source, "count = 1;");

        let Known::Yes(binding) = found else {
            panic!("a relative qualifier is resolved from the enclosing scope: {found:?}");
        };
        assert_eq!(binding.name.identifier_text(), Some("count"));
    }

    #[test]
    fn a_cursor_on_the_qualifier_asks_about_the_qualifier() {
        // `ns::count` with the cursor on `ns` is a question about `ns`, not about `count`: the spelling the file
        // writes up to the cursor is `ns`. Getting this wrong sends a user to the wrong entity from a position
        // they can plainly see is the namespace.
        let source = "namespace ns {\n  int count = 0;\n}\nvoid f() {\n  ns::count = 1;\n}\n";
        let found = definition_of(source, "ns::count");

        let Known::Yes(binding) = found else {
            panic!("the qualifier is a name this file declares: {found:?}");
        };
        assert_eq!(binding.name.identifier_text(), Some("ns"));
    }

    #[test]
    fn a_qualifier_that_names_nothing_is_unknown_rather_than_guessed() {
        let source = "namespace ns {\n  int count = 0;\n}\nvoid f() {\n  nope::count = 1;\n}\n";
        let found = definition_of(source, "count = 1;");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "and the reason must carry the qualified spelling, which is what the cross-file layer looks up: \
             {found:?}"
        );

        let Known::Unknown(UnknownReason::NotDeclaredHere(written)) = found else {
            unreachable!("just checked");
        };
        assert_eq!(&*written, "nope::count");
    }

    #[test]
    fn an_out_of_line_definition_resolves_to_the_declaration_in_the_class() {
        // The everyday C++ pair: the declaration in the class, the definition in a `.cpp`. Both are `C::value`,
        // and the jump a user wants is from either to the declaration.
        let source = "struct C {\n  int value() const;\n};\nint C::value() const {\n  return 0;\n}\n";
        let found = definition_of(source, "value() const {");

        let Known::Yes(binding) = found else {
            panic!("`C::value` names the member declared in `C`: {found:?}");
        };
        assert_eq!(binding.name.identifier_text(), Some("value"));
        assert!(
            binding.range.start_offset < at(source, "int C::value"),
            "and the jump goes to the declaration, not to the definition it is standing on"
        );
    }

    #[test]
    fn a_using_directive_makes_a_namespace_reachable() {
        let source = "namespace ns {\n  int count = 0;\n}\nusing namespace ns;\nvoid f() {\n  count = 1;\n}\n";
        let found = definition_of(source, "count = 1;");

        let Known::Yes(binding) = found else {
            panic!("`using namespace ns` makes `count` visible: {found:?}");
        };
        assert_eq!(binding.name.identifier_text(), Some("count"));
    }

    #[test]
    fn a_name_declared_nowhere_in_the_file_is_unknown_rather_than_absent() {
        // The honest answer: within one file the name is not here, and it is almost certainly in a header that
        // has not been read. `No` would say it is nowhere, which this layer cannot know.
        let source = "#include <vector>\nstd::vector<int> values;\nvoid f() {\n  values.push_back(1);\n}\n";
        let found = definition_of(source, "push_back");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::NotDeclaredHere(_))),
            "a name from a header is unknown, not absent: {found:?}"
        );

        if let Known::Unknown(reason) = found {
            assert!(
                reason.describe().contains("push_back"),
                "the message names what could not be resolved: {}",
                reason.describe()
            );
        }
    }

    #[test]
    fn a_qualified_name_that_cannot_be_resolved_is_unknown() {
        let source = "void f() {\n  ns::Widget value;\n}\n";
        let found = definition_of(source, "Widget");

        assert!(
            matches!(found, Known::Unknown(_)),
            "a qualifier this file cannot see means the answer is elsewhere: {found:?}"
        );
    }

    #[test]
    fn a_binding_that_is_not_a_definition_is_skipped() {
        // `using namespace ns;` is recorded as a binding of `ns` so that a consumer can find the line — but it
        // declares nothing, and jumping to it from a *use* of `ns` would be jumping to the directive rather than
        // to the namespace. The namespace's own declaration is the answer.
        let source = "namespace ns {\n  int count = 0;\n}\nusing namespace ns;\n";
        let found = definition_of(source, "ns;");

        let Known::Yes(binding) = found else {
            panic!("the namespace itself is declared in this file: {found:?}");
        };
        assert_eq!(binding.kind, BindingKind::Namespace);
    }

    #[test]
    fn a_non_identifier_name_reports_that_it_could_not_be_read() {
        // `operator+` has no identifier to look up. The answer is a reason rather than a panic, because the
        // cursor lands on operator names in ordinary editing.
        let source = "struct S {\n  S operator+(const S&);\n};\n";
        let found = definition_of(source, "operator+");

        assert!(
            matches!(found, Known::Unknown(UnknownReason::UnparsableName)),
            "an operator name is not an identifier: {found:?}"
        );
    }
}
