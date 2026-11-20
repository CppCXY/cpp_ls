//! **The shape of a declarator, for the declarations that are hard** — a survey, and the invariants it checks.
//!
//! The C and C++ grammar has exactly one shape for a declarator:
//!
//! ```text
//! declarator        ptr-declarator
//! ptr-declarator    noptr-declarator | ptr-operator ptr-declarator
//! noptr-declarator  declarator-id | ( ptr-declarator ) | noptr-declarator ( params ) | noptr-declarator [ bound ]
//! ```
//!
//! so the suffixes — the parameter list, the array bound — bind to the **same node** as the name they follow, and a
//! parenthesised declarator is a `noptr-declarator` like any other. A tree that puts them anywhere else makes every
//! accessor's question ambiguous: `get_param_list` has to guess whether the list is a child, a sibling, or a
//! grandchild, and each guess is a rule that can disagree with the next reader.
//!
//! This is the report of where our own tree stands, measured on the shapes a real C project writes. It is not an
//! assertion of what the tree *should* be yet — [the survey](#the-survey) prints what it is, and the tests below
//! pin the invariants that already hold so that a change to the grammar cannot break them silently.
//!
//! ```text
//! cargo test --release -p cpp_parser --test declarator_shapes -- --nocapture
//! ```

use cpp_parser::{
    CppAstNode, CppDeclaration, CppParser, CppSyntaxKind, CppSyntaxNode, ParserConfig,
};

/// Parse one declaration and hand back its typed node.
fn declaration(source: &str) -> CppDeclaration {
    let tree = CppParser::parse(source, ParserConfig::default());
    let root = cpp_parser::CppTranslationUnit::cast(tree.get_red_root())
        .expect("the root is a translation unit");
    root.descendants::<CppDeclaration>()
        .next()
        .expect("a declaration")
}

/// **The child-kind chain under a node**, one level deep — what a reader needs to see the shape without a tree dump.
fn child_chain(node: &CppSyntaxNode) -> String {
    let children: Vec<String> = node
        .children()
        .map(|child| {
            let kind = format!("{:?}", CppSyntaxKind::from(child.kind()));
            let text = child.text().to_string();
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let text = if text.len() > 34 {
                format!("{}…", &text[..34])
            } else {
                text
            };
            format!("{kind}[{text}]")
        })
        .collect();
    children.join(" ")
}

/// One row of the survey: the declaration, and the four questions every consumer asks of its declarator.
struct Row {
    written: &'static str,
    /// The chain of nodes between the declaration and the node that names the declarator, as the tree has it.
    shape: String,
    /// What `CppDeclaration::get_param_list` answers, and where that list sits.
    list: String,
    /// What the whole declarator's text is, which is what `get_type_text_of` and every fact's spelling come from.
    declarator: String,
}

fn survey() -> Vec<Row> {
    // The declarations that are hard, and every one of them is real code: the first four are the C declarator
    // examples the standard's own grammar is written to explain, and the rest are the spellings C projects write.
    let cases: &[(&str, &str)] = &[
        ("simple", "void simple(int a);"),
        ("function pointer", "int (*fp)(int);"),
        ("pointer to function pointer", "int (**ppf)(int);"),
        ("function returning pointer", "int *ret(int);"),
        ("parenthesised name", "void (named) (int a, char *b);"),
        (
            "function returning function pointer",
            "int (*maker(int a))(int b);",
        ),
        ("pointer to member function", "void (Widget::*pm)(int);"),
        ("array of function pointers", "int (*table[3])(int);"),
        ("reference to array (C++)", "void takes(int (&arr)[3]);"),
        (
            "unnamed function pointer parameter",
            "void takes2(int (*)(int));",
        ),
        (
            "the classic signal()",
            "void (*signal(int, void (*)(int)))(int);",
        ),
        ("typedef of a function pointer", "typedef int (*Fn)(int);"),
        (
            "using of a function pointer (C++)",
            "using Fn2 = int (*)(int);",
        ),
        ("trailing return (C++)", "auto trailing(int a) -> int;"),
        (
            "function pointer member in a class",
            "struct S { int (*m)(int); };",
        ),
        ("two lists on one declaration", "int (*two(int a))(int b);"),
    ];

    cases
        .iter()
        .map(|(_name, source)| {
            let declaration = declaration(source);
            let declarator = declaration.get_init_declarator();
            let shape = declarator
                .as_ref()
                .map(|init| child_chain(init.syntax()))
                .unwrap_or_else(|| child_chain(declaration.syntax()));
            let list = declaration.get_param_list().map(|list| {
                // Where the list sits, relative to the declaration: the answer to "will a reader find it".
                let parent = list
                    .syntax()
                    .parent()
                    .map(|parent| {
                        format!(
                            "{:?}[{}]",
                            CppSyntaxKind::from(parent.kind()),
                            parent
                                .text()
                                .to_string()
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ")
                        )
                    })
                    .unwrap_or_else(|| "no parent".to_string());
                format!("{} in {parent}", list.syntax().text())
            });
            let declarator_text = declarator
                .as_ref()
                .map(|init| {
                    init.child::<cpp_parser::CppDeclarator>()
                        .map(|declarator| declarator.syntax().text().to_string())
                        .unwrap_or_else(|| "(no declarator)".to_string())
                })
                .unwrap_or_else(|| "(no init declarator)".to_string());
            Row {
                written: source,
                shape,
                list: list.unwrap_or_else(|| "None".to_string()),
                declarator: declarator_text,
            }
        })
        .collect()
}

/// **The survey** — what our tree does with each shape, printed rather than asserted: this is the measurement the
/// question "is the shape reasonable" is answered from.
#[test]
fn the_survey() {
    for row in survey() {
        println!(
            "\n{:<44}\n  shape      {}\n  list       {}\n  declarator {}",
            row.written, row.shape, row.list, row.declarator
        );
    }
}

/// **Which declarators name their parameter list as a child, and which do not** — the measured split.
///
/// The grammar states `noptr-declarator: … | ( ptr-declarator ) | noptr-declarator ( params )`, so the list binds to
/// the same declarator as the name it follows, and an accessor can rely on it without knowing whether the name was
/// written bare, parenthesised, or behind a pointer. A **plain** declarator does; a pointer-declarator form does not —
/// the list is left as a sibling of the declarator, one level up:
///
/// ```text
/// void simple(int a);            Declarator[simple(int a)]          the list is the declarator's child
/// int *ret(int);                 Declarator[*ret(int)]              the list is the declarator's child
/// int (*fp)(int);                Declarator[(*fp)] ParameterList    the list is a SIBLING
/// int (*maker(int a))(int b);    Declarator[(*maker(int a))] ParameterList
/// ```
///
/// The test pins both halves so that a change to the grammar has to come here and say which one it changed. The six
/// shapes on the sibling side are the classic C declarators, and `get_param_list` carries a sibling rule for exactly
/// one of them (`(named)(int)`, where the list really is the name's). See `docs/declarator-shapes.md`.
#[test]
fn the_lists_that_are_children_and_the_ones_that_are_siblings() {
    let child = ["void simple(int a);", "int *ret(int);"];
    for source in child {
        let declaration = declaration(source);
        let declarator = declaration
            .get_init_declarator()
            .and_then(|init| init.child::<cpp_parser::CppDeclarator>())
            .expect("a declarator");
        assert!(
            declarator
                .syntax()
                .children()
                .any(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ParameterList),
            "{source}: the list is `{}`'s child: {}",
            declarator.syntax().text(),
            child_chain(declarator.syntax())
        );
    }

    let sibling = [
        "int (*fp)(int);",
        "int (**ppf)(int);",
        "int (*maker(int a))(int b);",
        "int (*table[3])(int);",
        "void (Widget::*pm)(int);",
        "void (*signal(int, void (*)(int)))(int);",
    ];
    for source in sibling {
        let declaration = declaration(source);
        let init = declaration
            .get_init_declarator()
            .expect("an init declarator");
        let kinds: Vec<CppSyntaxKind> = init
            .syntax()
            .children()
            .map(|child| CppSyntaxKind::from(child.kind()))
            .collect();
        assert_eq!(
            kinds,
            vec![CppSyntaxKind::Declarator, CppSyntaxKind::ParameterList],
            "{source}: the list sits beside the declarator today: {}",
            child_chain(init.syntax())
        );
    }
}

/// **A parenthesised declarator does not name its list as a child** — the shape this survey was asked for.
///
/// `void (named) (int a, char *b);` is the C header spelling (`LUA_API void (lua_setfield) (…)`), and the list is a
/// sibling of the declarator inside the `InitDeclarator`. The test asserts what *is*, so that a change to the grammar
/// has to come here and say so: when the shape is fixed, this test fails and the sibling rule in
/// `CppDeclarator::get_param_list` can go.
#[test]
fn a_parenthesised_declarators_list_is_a_sibling_today() {
    let declaration = declaration("void (named) (int a, char *b);");
    let init = declaration
        .get_init_declarator()
        .expect("an init declarator");
    let kinds: Vec<CppSyntaxKind> = init
        .syntax()
        .children()
        .map(|child| CppSyntaxKind::from(child.kind()))
        .collect();
    assert_eq!(
        kinds,
        vec![CppSyntaxKind::Declarator, CppSyntaxKind::ParameterList],
        "the list sits beside the declarator: {}",
        child_chain(init.syntax())
    );
}

/// **The two lists of `int (*f(int))(int)` are each read by the right node.** The inner one belongs to the function
/// `f`, the outer one to the pointer — and a reader that took the first list it found would give `f` the wrong
/// parameters (that is the boundary `parse_parenthesised_declarator`'s own documentation states).
#[test]
fn the_two_lists_of_a_function_returning_a_function_pointer() {
    let declaration = declaration("int (*maker(int a))(int b);");
    let outer = declaration
        .get_init_declarator()
        .and_then(|init| init.child::<cpp_parser::CppDeclarator>())
        .expect("a declarator");
    // **Today the declaration answers `None`**, and the reason is the survey's finding: `maker`'s own list `(int a)`
    // is two declarators down — inside the parentheses — and the declaration-level accessor only looks at the
    // outermost declarator, whose child is the parenthesised one. That is a wrong answer, not a missing one: `maker`
    // is a function and it has parameters. The test records it as an expected failure of the *shape*, so a change to
    // the grammar or to the accessor has to come here.
    assert!(
        declaration.get_param_list().is_none(),
        "the declaration does NOT answer `maker`'s own list today: {}",
        child_chain(outer.syntax())
    );
    // The function `f`'s own list is inside the parentheses, one declarator down.
    let names_inside: Vec<String> = outer
        .syntax()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ParameterList)
        .map(|node| node.text().to_string())
        .collect();
    assert!(
        names_inside.iter().any(|list| list == "(int a)"),
        "the inner list is in the tree: {names_inside:?}"
    );
}
