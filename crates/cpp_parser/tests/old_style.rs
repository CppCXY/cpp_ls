//! Old-style (K&R) function definitions: the parameters are declared after the parenthesis, not inside it.
//!
//! ```c
//! int main(argc, argv)
//!     int argc;
//!     char *argv[];
//! { return argc; }
//! ```
//!
//! Obsolete in C++ and deprecated in C, but it is how C was written before 1989 and how a lot of C was written
//! after that, and this parser is asked to read C. The file that found the gap is a CMake compiler-id probe —
//! generated into every CMake build tree in existence, so "nobody writes that any more" is not true of the files
//! an editor is pointed at.
//!
//! # The parenthesis cannot decide on its own
//!
//! `(argc, argv)` is read before anything says which list it is, and the tokens are the same either way: `argc`
//! is a perfectly good parameter *type*, so a modern parameter list reads them as two unnamed parameters of type
//! `argc` and `argv` — which is exactly what the tree below shows. What separates the two readings comes
//! **after**: a modern function continues with `{`, `;`, `:`, `requires`… and a *declaration* after a function
//! declarator is only ever an old-style parameter list. So the tree keeps both answers: the `ParameterList` is
//! what the parenthesis said, the `OldStyleParameterList` is what it meant, and a consumer that wants the
//! parameters' types reads the latter. `the_names_are_read_as_parameters_and_the_truth_as_a_second_list` pins
//! that pair, so that teaching the parenthesis to look ahead is a visible change here rather than a silent one.
//!
//! # The `;` that is not there
//!
//! `int main(argc, argv) int argc; char *argv[];` has **no `;` of its own**: the one that ends the last
//! parameter declaration ends the declaration as well. On its own that is not a program in C — a definition
//! needs a body — but it is precisely what a conditional produces:
//!
//! ```c
//! #if defined(__CLASSIC_C__)
//! int main(argc, argv)
//! int argc;
//! char* argv[];
//! #else
//! int main(int argc, char* argv[])
//! #endif
//! { … }
//! ```
//!
//! The head is in one branch and the body in neither: both branches share it. Asking for a second `;` reported
//! `expected ';'` against the `#else`, and one diagnostic on a *preprocessor* line took the whole definition
//! with it — including the branch that had done nothing wrong. `a_head_without_a_body_is_still_one_declaration`
//! and `the_head_and_the_body_may_sit_in_different_conditional_branches` are the two halves of that.

use cpp_parser::{CppParser, CppSyntaxKind, CppSyntaxNode, CppSyntaxTree, ParserConfig};

/// A definition with everything spelled out, and its head written the way old C wrote it.
const DEFINITION: &str =
    "int main(argc, argv)\n    int argc;\n    char *argv[];\n{ return argc; }\n";

/// The same head with no body at all — the shape a conditional branch leaves behind.
const HEAD_WITHOUT_A_BODY: &str = "int main(argc, argv)\nint argc;\nchar *argv[];\n";

/// The head and the body in different branches of one conditional, as CMake generates it.
const SPLIT_BRANCHES: &str = "\
#if defined(__CLASSIC_C__)\n\
int main(argc, argv)\n\
int argc;\n\
char* argv[];\n\
#else\n\
int main(int argc, char* argv[])\n\
#endif\n\
{\n\
  return 0;\n\
}\n";

fn tree(source: &str) -> CppSyntaxTree {
    CppParser::parse(source, ParserConfig::default())
}

/// Parse, and require that the result is clean in **both** senses.
///
/// An empty error list is not enough on its own: a tree can be lossless, well formed and free of diagnostics
/// while describing a different construct than the one that was written — that is the A0 failure mode, and this
/// feature has two places it could happen (the head read as two declarations, the body left at file scope). So
/// the tokens are checked to be all there as well, and the shape tests below ask *what* was read.
fn parses(source: &str) {
    let parsed = tree(source);
    assert_eq!(
        parsed.get_errors(),
        [],
        "{source:?} must parse cleanly, got {:?}",
        parsed.get_errors()
    );

    let unclaimed = parsed.get_red_root().descendants().any(|node| {
        matches!(
            CppSyntaxKind::from(node.kind()),
            CppSyntaxKind::ErrorNode | CppSyntaxKind::MissingNode
        )
    });
    assert!(!unclaimed, "{source:?} leaves an ErrorNode behind");
    assert_eq!(parsed.to_source_text(), source, "{source:?} stays lossless");
}

/// The first node of `kind` in the tree.
fn node(source: &str, kind: CppSyntaxKind) -> CppSyntaxNode {
    tree(source)
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == kind)
        .unwrap_or_else(|| panic!("no {kind:?} in {source:?}"))
}

fn count(source: &str, kind: CppSyntaxKind) -> usize {
    tree(source)
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == kind)
        .count()
}

/// The kinds directly under `node`, so that a shape assertion reads like the tree does.
fn children_of(node: &CppSyntaxNode) -> Vec<CppSyntaxKind> {
    node.children()
        .map(|child| CppSyntaxKind::from(child.kind()))
        .collect()
}

/// The declarations written at file scope.
///
/// Counted this way because a *nested* declaration is a declaration too: an old-style parameter list holds one
/// per parameter, so "how many declarations are in the file" is not the question a shape test is asking.
fn file_scope_declarations(source: &str) -> Vec<CppSyntaxNode> {
    tree(source)
        .get_red_root()
        .children()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declaration)
        .collect()
}

#[test]
fn an_old_style_definition_is_one_declaration_with_its_body() {
    parses(DEFINITION);

    // One declaration, and the body is *inside* it: a body left at file scope would be the A0 shape this
    // feature's second defect produced (`struct S requires C<T> { }` did exactly that to a class).
    let declaration = node(DEFINITION, CppSyntaxKind::Declaration);
    assert_eq!(
        children_of(&declaration),
        vec![
            CppSyntaxKind::DeclSpecifierSeq,
            CppSyntaxKind::InitDeclarator,
            CppSyntaxKind::CompoundStat,
        ],
        "the definition is one declaration, body included"
    );
    assert_eq!(
        declaration
            .parent()
            .map(|parent| CppSyntaxKind::from(parent.kind())),
        Some(CppSyntaxKind::TranslationUnit),
        "and it is the only thing at file scope"
    );

    // The declarator holds the name and the parenthesis; the old-style list sits beside it, in the
    // init-declarator, because that is where the standard puts it — after the declarator, before the body.
    let declarator = node(DEFINITION, CppSyntaxKind::Declarator);
    assert_eq!(
        children_of(&declarator),
        vec![CppSyntaxKind::NameExpr, CppSyntaxKind::ParameterList]
    );
    assert_eq!(
        children_of(&node(DEFINITION, CppSyntaxKind::InitDeclarator)),
        vec![
            CppSyntaxKind::Declarator,
            CppSyntaxKind::OldStyleParameterList,
        ]
    );

    // Two parameter declarations, each read by the ordinary declaration rule — so each carries a specifier
    // sequence, an init-declarator and its own `;`.
    let list = node(DEFINITION, CppSyntaxKind::OldStyleParameterList);
    assert_eq!(
        children_of(&list),
        vec![CppSyntaxKind::Declaration, CppSyntaxKind::Declaration]
    );
    assert_eq!(list.text().to_string(), "int argc;\n    char *argv[];\n");
}

#[test]
fn the_names_are_read_as_parameters_and_the_truth_as_a_second_list() {
    // Neither list is "wrong" — they are two answers to two different questions, and the test pins both so that
    // a later attempt to make the parenthesis look ahead has to change this expectation on purpose.
    parses(DEFINITION);

    assert_eq!(
        node(DEFINITION, CppSyntaxKind::ParameterList)
            .text()
            .to_string(),
        "(argc, argv)\n    ",
        "`argc` and `argv` are read as the parameter list, because a parameter's type may be a name"
    );
    assert_eq!(
        count(DEFINITION, CppSyntaxKind::Parameter),
        2,
        "which makes them two unnamed parameters of type `argc` and `argv`"
    );

    // What the names *are*: `argc` and `argv`. The second list is where a consumer finds that out, and its
    // declarations are ordinary ones, so their names are ordinary declarator names.
    let declarations: Vec<String> = node(DEFINITION, CppSyntaxKind::OldStyleParameterList)
        .children()
        .map(|declaration| declaration.text().to_string())
        .collect();
    assert_eq!(declarations, ["int argc;\n    ", "char *argv[];\n"]);
}

#[test]
fn an_old_style_definition_after_another_declaration_is_still_a_definition() {
    // The head is reached by *trying* the declaration reading, so it must not depend on being the first thing in
    // the file: the token that says "old style" is the declaration that follows, and there is one here either
    // way. (This was broken while the feature was being written, and the symptom was an error on line 1 rather
    // than on the head's own line — which is why the assertion is about the shape and not about the count.)
    let source = format!("int x;\n{DEFINITION}");
    parses(&source);

    assert_eq!(
        file_scope_declarations(&source).len(),
        2,
        "`x`, then the function"
    );
    assert_eq!(count(&source, CppSyntaxKind::CompoundStat), 1, "one body");
    assert_eq!(
        count(&source, CppSyntaxKind::OldStyleParameterList),
        1,
        "and one old-style list — `int x;` is not part of it"
    );
}

#[test]
fn a_head_without_a_body_is_still_one_declaration() {
    // Not a program in C: the head of a function with no body, which only a conditional can produce. The `;`
    // that ended `char *argv[];` ended this declaration too, so there is none left to ask for.
    parses(HEAD_WITHOUT_A_BODY);

    let declaration = node(HEAD_WITHOUT_A_BODY, CppSyntaxKind::Declaration);
    assert_eq!(
        children_of(&declaration),
        vec![
            CppSyntaxKind::DeclSpecifierSeq,
            CppSyntaxKind::InitDeclarator
        ],
        "the declaration is complete at the last parameter's `;`"
    );
    assert_eq!(
        children_of(&declaration.parent().expect("file scope")),
        vec![CppSyntaxKind::Declaration],
        "and nothing is left over beside it"
    );
    assert_eq!(
        count(HEAD_WITHOUT_A_BODY, CppSyntaxKind::CompoundStat),
        0,
        "there is no body to find"
    );
}

#[test]
fn the_head_and_the_body_may_sit_in_different_conditional_branches() {
    parses(SPLIT_BRANCHES);

    assert_eq!(file_scope_declarations(SPLIT_BRANCHES).len(), 2);
    assert_eq!(count(SPLIT_BRANCHES, CppSyntaxKind::CompoundStat), 1);

    // The K&R branch ends at its last `;` — before the `#else`, which is what the error used to be reported
    // against — and holds no body.
    let old_style = tree(SPLIT_BRANCHES)
        .get_red_root()
        .descendants()
        .find(|node| {
            CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declaration
                && node.descendants().any(|child| {
                    CppSyntaxKind::from(child.kind()) == CppSyntaxKind::OldStyleParameterList
                })
        })
        .expect("the old-style branch is a declaration");
    assert_eq!(
        old_style.text().to_string(),
        "int main(argc, argv)\nint argc;\nchar* argv[];\n"
    );

    // The body belongs to the *modern* branch, which is the declaration written after the `#else`.
    let body = node(SPLIT_BRANCHES, CppSyntaxKind::CompoundStat);
    let owner = body.parent().expect("the body has an owner");
    assert_eq!(
        CppSyntaxKind::from(owner.kind()),
        CppSyntaxKind::Declaration
    );
    assert!(
        owner
            .text()
            .to_string()
            .starts_with("int main(int argc, char* argv[])"),
        "the body belongs to the modern head, got {:?}",
        owner.text().to_string()
    );
}

#[test]
fn a_modern_function_declarator_is_not_read_as_an_old_style_one() {
    // The arm is a fallback taken only where nothing else can be meant: every spelling a *modern* function
    // continues with is claimed before it, and none of these may grow an `OldStyleParameterList`.
    for source in [
        "void f(int a) { }",
        "void g();",
        "static void A::f<int>(int);",
        "struct S { void f() const; void g() noexcept; virtual void h() override; };",
        "struct S { S() : a(1) { } int a; };",
        "template <typename T> void f(T t) requires C<T>;",
        "auto l = [](int a) { return a; };",
    ] {
        parses(source);
        assert_eq!(
            count(source, CppSyntaxKind::OldStyleParameterList),
            0,
            "{source:?} is not an old-style definition"
        );
    }
}

#[test]
fn the_list_holds_declarations_and_stops_at_anything_else() {
    // The list is greedy in the way C is: it runs until a token that cannot begin a declaration. A statement
    // after it is left outside — the declarations that *are* in the list are the whole of it.
    let source = "int f(a)\nint a;\nx = 1;\n";

    assert_eq!(
        children_of(&node(source, CppSyntaxKind::OldStyleParameterList)),
        vec![CppSyntaxKind::Declaration],
        "one declaration in the list"
    );
    assert_eq!(
        node(source, CppSyntaxKind::OldStyleParameterList)
            .text()
            .to_string(),
        "int a;\n",
        "and the statement after it is not part of it"
    );
}
