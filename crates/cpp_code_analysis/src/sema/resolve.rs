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
    Binding, BindingKind, Known, Name, NameKind, Scope, ScopeId, ScopeTree, UnknownReason,
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
pub fn definition_at(scopes: &ScopeTree, root: &CppSyntaxNode, offset: usize) -> Known<Binding> {
    let Some((written, _)) = name_at(root, offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    let name = Name::identifier(written.clone());

    // The scope chain starts at the innermost scope *containing the offset*, and the scopes that merely enclose
    // the position are added as well — see `scopes_to_search` for why both.
    let Some(innermost) = scopes.scope_at(offset) else {
        return Known::No;
    };

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
    Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(written_name_text(&name))))
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

/// The text a [`Name`] was written as, for a message.
fn written_name_text(name: &Name) -> String {
    match &name.kind {
        NameKind::Identifier(text) => text.clone(),
        other => other.text(),
    }
}

#[cfg(test)]
mod tests {
    use super::{definition_at, identifier_written_at, name_at, name_node_at};
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
    fn a_qualified_name_from_outside_its_namespace_is_unknown_rather_than_wrong() {
        // `ns::count` from file scope. Resolving it needs the qualifier walked segment by segment, which this
        // layer does not do yet — and the answer must say so rather than jump to a `count` that happens to be in
        // *some* scope. This test is what will fail when qualified resolution lands, which is the point: the
        // change will be deliberate.
        let source = "namespace ns {\n  int count = 0;\n}\nvoid f() {\n  ns::count = 1;\n}\n";
        let found = definition_of(source, "count = 1;");

        assert!(
            matches!(found, Known::Unknown(_)),
            "a qualifier this layer cannot walk is unknown, not guessed: {found:?}"
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
