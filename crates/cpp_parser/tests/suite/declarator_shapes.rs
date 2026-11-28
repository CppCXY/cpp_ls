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
//! cargo test --release -p cpp_parser --test suite declarator_shapes -- --nocapture
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

/// **Every declarator keeps its suffixes to itself** — the invariant the grammar states, now that the tree has it.
///
/// `noptr-declarator: … | ( ptr-declarator ) | noptr-declarator ( params )`, so a parameter list is a **child** of the
/// declarator it follows, and the `InitDeclarator` has exactly one declarator with no list beside it. The six shapes
/// that used to leave it as a sibling — the classic C declarators — are all in this list; see
/// `docs/declarator-shapes.md` for what the tree was before and what each one broke.
#[test]
fn every_declarator_keeps_its_suffixes_to_itself() {
    let shapes = [
        "void simple(int a);",
        "int *ret(int);",
        "int (*fp)(int);",
        "int (**ppf)(int);",
        "void (named) (int a, char *b);",
        "int (*maker(int a))(int b);",
        "void (Widget::*pm)(int);",
        "int (*table[3])(int);",
        "void takes(int (&arr)[3]);",
        "void takes2(int (*)(int));",
        "void (*signal(int, void (*)(int)))(int);",
        "int (*two(int a))(int b);",
    ];
    for source in shapes {
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
            vec![CppSyntaxKind::Declarator],
            "{source}: the list is the declarator's child, and there is no sibling: {}",
            child_chain(init.syntax())
        );
        // …and the declarator's own text is the whole declarator spelling, the list included.
        let declarator = init
            .child::<cpp_parser::CppDeclarator>()
            .expect("a declarator");
        let spelled = declarator.syntax().text().to_string();
        assert!(
            spelled.ends_with(')') && (spelled.contains('(') || spelled.contains('[')),
            "{source}: the declarator spells its own suffixes: {spelled:?}"
        );
    }
}

/// **The parenthesised declarator is one declarator, list included** — `void (named) (int a, char *b);`.
///
/// It was a declarator `(named)` with the list as a **sibling** under the `InitDeclarator` until the grammar was fixed,
/// which is why `get_param_list` carried a "look at my parent" rule for the spelling Lua 5.5.0 writes its whole public
/// API with (`LUA_API void (lua_setfield) (…)`).
#[test]
fn a_parenthesised_declarator_is_one_declarator() {
    let declaration = declaration("void (named) (int a, char *b);");
    let init = declaration
        .get_init_declarator()
        .expect("an init declarator");
    assert_eq!(
        child_chain(init.syntax()),
        "Declarator[(named) (int a, char *b)]",
        "one declarator, its suffixes inside"
    );
}
/// **The two lists of `int (*maker(int a))(int b)` are each in the declarator that spells them.**
///
/// The inner one belongs to the function `maker`, the outer one to the pointer — and a reader that took the first list
/// it found would give `maker` the wrong parameters. The tree separates them by nesting, which is what the analysis's
/// name-anchored walk relies on (`sema::scopes`'s `the_naming_declarator`).
#[test]
fn the_two_lists_of_a_function_returning_a_function_pointer() {
    let declaration = declaration("int (*maker(int a))(int b);");
    let outer = declaration
        .get_init_declarator()
        .and_then(|init| init.child::<cpp_parser::CppDeclarator>())
        .expect("a declarator");
    let lists: Vec<(String, String)> = outer
        .syntax()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::ParameterList)
        .map(|node| {
            let owner = node
                .parent()
                .map(|parent| parent.text().to_string())
                .unwrap_or_default();
            (node.text().to_string(), owner)
        })
        .collect();
    assert!(
        lists
            .iter()
            .any(|(list, owner)| list == "(int a)" && owner.contains("maker")),
        "`maker`'s own list is inside the parentheses: {lists:?}"
    );
    assert!(
        lists
            .iter()
            .any(|(list, owner)| list == "(int b)" && owner.starts_with("(*maker")),
        "and the pointer's list is the outer declarator's: {lists:?}"
    );
}
/// A dump of the initialised-variable declarator, for the type reader's constructor-call guard.
#[test]
fn debug_initialised_variable() {
    let declaration = declaration("std::unique_lock<M> lock(mutex, 1);");
    let declarator = declaration
        .get_init_declarator()
        .and_then(|init| init.child::<cpp_parser::CppDeclarator>());
    fn dump(node: &CppSyntaxNode, depth: usize) {
        println!(
            "{:indent$}{:?} {:?}",
            "",
            CppSyntaxKind::from(node.kind()),
            node.text().to_string(),
            indent = depth * 2
        );
        for child in node.children() {
            dump(&child, depth + 1);
        }
    }
    match declarator {
        Some(declarator) => dump(declarator.syntax(), 0),
        None => println!("no declarator"),
    }
}

/// **A `typedef` in one function is not a type name in the next one.**
///
/// The table of local type names used to be a map from name to *the brace depth it was declared at*, and
/// visibility was `declared_at <= depth`. Two sibling functions share a depth, so `Leaked` stayed a type name for
/// the rest of the file, and the call below was read as a **declaration** of `Leaked` with a parameter list — no
/// `CallExpr`, no diagnostic, and a binding that does not exist. `docs/worklog.md` P1-1.
#[test]
fn a_typedef_in_one_function_is_not_a_type_name_in_its_sibling() {
    let text = "void first()\n{\n    typedef long Leaked;\n}\n\nvoid second()\n{\n    auto Leaked = [] {};\n    \
                Leaked();\n}\n";
    let tree = CppParser::parse(text, ParserConfig::default());
    assert!(tree.get_errors().is_empty(), "{:?}", tree.get_errors());

    let calls = tree
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CallExpr)
        .count();
    assert_eq!(calls, 1, "the callable is called, not declared");
}

/// **The control**: a type the *file* declares is still a type inside every body after it, which is the reading the
/// table exists for — `Widget w(1, 2);` is a variable initialised with two arguments, not a call.
#[test]
fn a_type_declared_at_file_scope_is_still_a_type_inside_a_later_body() {
    let text = "struct Widget { Widget(int, int); };\nvoid f()\n{\n    Widget w(1, 2);\n}\n";
    let tree = CppParser::parse(text, ParserConfig::default());
    assert!(tree.get_errors().is_empty(), "{:?}", tree.get_errors());
    let names: Vec<String> = tree
        .get_red_root()
        .descendants()
        .filter_map(CppDeclaration::cast)
        .filter_map(|declaration| declaration.get_name_text())
        .collect();
    assert!(
        names.iter().any(|name| name == "w"),
        "`w` is a declared variable: {names:?}"
    );
}

/// **An attribute group inside a declarator, and the spellings that put it there.**
///
/// Three shapes, and all three are how a real toolchain writes what the corpus needs:
///
/// ```c
/// typedef void (__attribute__((__stdcall__)) DRVCALLBACK)(HDRVR hdrvr, UINT uMsg, …);   // winuser.h
/// typedef int  (__attribute__((__stdcall__)) *FN)(int);                                 // combaseapi.h
/// int f(void (__attribute__((__cdecl__)) *)(void));                                     // mingw's stdlib.h
/// ```
///
/// Without a reading for them the group was taken for a parameter list, the declaration fell apart at its `;`, and the
/// program of mingw's `ui.c` parsed with errors in `winuser.h`, `objidl.h`, `fobjidl.h` — the last eight of the 1 962
/// the gluing fix and the `__inline` spellings left.
#[test]
fn an_attribute_group_inside_a_declarator_has_a_reading() {
    let cases = [
        "typedef void (__attribute__((__stdcall__)) DRVCALLBACK)(int hdrvr, unsigned int uMsg);",
        "typedef unsigned long (__attribute__((__stdcall__)) PM_OPEN_PROC)(wchar_t *);",
        "typedef int (__attribute__((__stdcall__)) *FN)(int);",
        "int f(void (__attribute__((__cdecl__)) *)(void));",
        "typedef struct S { int a; } S, *PS;",
    ];
    for source in cases {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert!(
            tree.get_errors().is_empty(),
            "{source} parsed with {:?}",
            tree.get_errors()
                .iter()
                .map(|error| error.message.clone())
                .collect::<Vec<_>>()
        );
    }
}

/// **The `inline` spellings a compiler writes are specifiers, not type names.**
///
/// `extern __inline__ size_t __attribute__((__cdecl__)) strnlen_s(…) { … }` is mingw's `string.h` and every GCC
/// intrinsic header. Read as a name, `__inline__` sat in the specifier sequence beside the real type, the declaration
/// failed, and the line was dropped — measured as **1 622 errors** in the program of neovim's `ui.c`, 398 of them in
/// `fxsrintrin.h`, `cmpccxaddintrin.h` and `enqcmdintrin.h`.
#[test]
fn the_inline_spellings_are_specifiers() {
    let cases = [
        "typedef unsigned long size_t;\nextern __inline__ size_t __attribute__((__cdecl__)) strnlen_s(const char * s, size_t n) { return 0; }",
        "extern __inline int f(void) { return 0; }",
        "extern __forceinline__ int g(void) { return 0; }",
    ];
    for source in cases {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert!(
            tree.get_errors().is_empty(),
            "{source} parsed with {:?}",
            tree.get_errors()
                .iter()
                .map(|error| error.message.clone())
                .collect::<Vec<_>>()
        );
    }
}
