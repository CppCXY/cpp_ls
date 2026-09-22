//! The typed AST layer.
//!
//! These tests do two things at once: they check that the accessors find what they claim, and they
//! pin down that the layer is *usable* — that a caller can get from a source string to a declaration's
//! name without touching the untyped tree.
//!
//! Every accessor returns `Option`, so the tests also run over broken input: an editor parses files
//! mid-edit, and an accessor that panicked on a half-written declaration would turn a typo into a
//! crash.

use cpp_parser::{
    CppAstNode, CppAstToken, CppDeclaration, CppExpr, CppParser, CppStat, CppSyntaxKind,
    CppTranslationUnit, ParserConfig,
};

/// Parse and hand back the typed root.
fn unit(source: &str) -> (CppTranslationUnit, cpp_parser::CppSyntaxTree) {
    let tree = CppParser::parse(source, ParserConfig::default());
    let root = CppTranslationUnit::cast(tree.get_red_root()).expect("root is a translation unit");
    (root, tree)
}

/// The names of the top-level declarations.
fn decl_names(source: &str) -> Vec<String> {
    let (root, _) = unit(source);
    root.get_declarations()
        .map(|decl| decl.get_name_text().unwrap_or_else(|| "<anon>".to_string()))
        .collect()
}

#[test]
fn finds_top_level_declarations() {
    let source = concat!(
        "int counter;\n",
        "void reset();\n",
        "struct Point { int x; int y; };\n",
        "namespace ns { int inner; }\n",
    );

    let (root, _) = unit(source);
    let kinds: Vec<&str> = root.get_declarations().map(|it| it.kind_name()).collect();

    assert_eq!(kinds, vec!["variable", "function", "class"]);
    // A namespace is a `NamespaceDecl`, not a `Declaration`, so it is not in that list.
    assert_eq!(root.get_namespaces().count(), 1);
    assert_eq!(
        decl_names(source),
        vec!["counter", "reset", "Point"]
    );
}

#[test]
fn variable_declaration_exposes_type_and_initializer() {
    let (root, _) = unit("int counter = 42;\n");
    let decl = root.get_declarations().next().unwrap();

    assert_eq!(decl.get_name_text().as_deref(), Some("counter"));
    assert_eq!(
        decl.get_decl_specifiers().unwrap().get_type_text(),
        "int",
        "the specifier sequence should report the written type"
    );

    let initializer = decl.get_initializer().expect("initializer");
    let value = initializer.get_expr().expect("initializer expression");
    let literal = match value {
        CppExpr::LiteralExpr(literal) => literal,
        other => panic!("expected a literal, got {other:?}"),
    };
    assert_eq!(literal.get_literal_text().as_deref(), Some("42"));
    assert!(!root.get_declarations().next().unwrap().is_function_def());
}

#[test]
fn function_definition_exposes_params_and_body() {
    let (root, _) = unit("int add(int a, int b) { return a + b; }\n");
    let decl = root.get_declarations().next().unwrap();

    assert!(decl.is_function_def(), "should be recognised as a definition");
    assert_eq!(decl.get_name_text().as_deref(), Some("add"));

    let declarator = decl.get_declarator().expect("declarator");
    assert!(declarator.is_function());
    let params = declarator.get_param_list().expect("parameter list");
    let names: Vec<String> = params
        .get_params()
        .filter_map(|param| param.get_name().map(|it| it.get_name_text().to_string()))
        .collect();
    assert_eq!(names, vec!["a", "b"]);

    let body = decl.get_body().expect("body");
    let stats: Vec<CppStat> = body.get_stats().collect();
    assert!(
        stats.iter().any(|it| matches!(it, CppStat::ReturnStat(_))),
        "body should contain the return statement"
    );
}

#[test]
fn function_definition_with_trailing_return_type() {
    let (root, _) = unit("auto begin() -> int* { return nullptr; }\n");
    let decl = root.get_declarations().next().unwrap();

    assert!(decl.is_function_def());
    let declarator = decl.get_declarator().unwrap();
    let trailing = declarator
        .get_trailing_return_type()
        .expect("trailing return type");
    // The pointer belongs to the return type, and the `->` does not.
    assert_eq!(trailing.get_type_text(), "int*");
}

#[test]
fn class_definition_exposes_members_and_access() {
    let source = "class Foo : public Base {\npublic:\n    void method();\nprivate:\n    int value_;\n};\n";
    let (root, _) = unit(source);
    let decl = root.get_declarations().next().unwrap();

    let class = decl.get_class_def().expect("class definition");
    assert_eq!(class.get_name_text().as_deref(), Some("Foo"));
    assert!(!class.is_struct());

    let bases: Vec<String> = class
        .get_base_specifiers()
        .filter_map(|it| it.get_name().map(|n| n.get_qualified_name()))
        .collect();
    assert_eq!(bases, vec!["Base"]);

    let body = class.get_body().expect("class body");
    let members: Vec<String> = body
        .get_members()
        .filter_map(|it| it.get_name_text())
        .collect();
    assert_eq!(members, vec!["method", "value_"]);

    let access: Vec<&'static str> = body
        .get_access_specifiers()
        .iter()
        .map(|it| match CppSyntaxKind::from(it.syntax().kind()) {
            CppSyntaxKind::PublicAccess => "public",
            CppSyntaxKind::PrivateAccess => "private",
            _ => "protected",
        })
        .collect();
    assert_eq!(access, vec!["public", "private"]);
}

#[test]
fn enum_definition_exposes_enumerators() {
    let (root, _) = unit("enum class Color : unsigned char { Red, Green = 2 };\n");
    let decl = root.get_declarations().next().unwrap();
    let enum_def = decl.get_enum_def().expect("enum definition");

    assert_eq!(enum_def.get_name().map(|it| it.get_name_text().to_string()).as_deref(), Some("Color"));
    assert!(enum_def.is_scoped(), "`enum class` is a scoped enum");

    let names: Vec<String> = enum_def
        .get_enumerators()
        .filter_map(|it| it.get_name().map(|n| n.get_name_text().to_string()))
        .collect();
    assert_eq!(names, vec!["Red", "Green"]);
}

#[test]
fn modules_are_typed() {
    let (root, _) = unit("export module my.mod:part;\n");

    let module = root.get_module_decl().expect("module declaration");
    assert!(module.is_interface(), "`export module` is an interface unit");
    assert_eq!(
        module.get_name().map(|it| it.get_name_text()).as_deref(),
        Some("my.mod")
    );
    let partition = module.get_partition().expect("partition");
    assert_eq!(
        partition
            .get_name()
            .map(|it| it.get_name_text())
            .as_deref(),
        Some("part")
    );
}

#[test]
fn imports_cover_all_three_forms() {
    let source = "import std;\nexport import :part;\nimport <iostream>;\n";
    let (root, _) = unit(source);

    let imports: Vec<_> = root
        .get_declarations()
        .filter_map(|decl| match CppStat::cast(decl.syntax().clone()) {
            Some(CppStat::ImportDecl(node)) => Some(node),
            _ => None,
        })
        .collect();

    // The import declarations are statement-level nodes, so reach them through `CppStat`.
    let tree_imports: Vec<_> = root
        .syntax()
        .descendants()
        .filter_map(cpp_parser::CppImportDecl::cast)
        .collect();
    assert_eq!(tree_imports.len(), 3, "expected three imports, got {imports:?}");

    assert_eq!(
        tree_imports[0].get_name().map(|it| it.get_name_text()).as_deref(),
        Some("std")
    );
    assert!(tree_imports[1].is_reexport());
    assert!(tree_imports[2].is_header_unit());
    assert_eq!(
        tree_imports[2]
            .get_header_name()
            .map(|it| it.get_name_text())
            .as_deref(),
        Some("iostream")
    );
}

#[test]
fn expressions_are_walkable() {
    let (root, _) = unit("int x = a + b * c;\n");
    let decl = root.get_declarations().next().unwrap();
    let initializer = decl.get_initializer().unwrap();
    let expr = initializer.get_expr().unwrap();

    // `a + b * c` parses as `a + (b * c)`.
    let binary = match expr {
        CppExpr::BinaryExpr(node) => node,
        other => panic!("expected a binary expression, got {other:?}"),
    };
    assert_eq!(binary.get_operator_text().as_deref(), Some("+"));
    assert!(matches!(binary.get_lhs(), Some(CppExpr::NameExpr(_))));
    let rhs = binary.get_rhs().expect("rhs");
    assert!(
        matches!(rhs, CppExpr::BinaryExpr(_)),
        "`b * c` should bind tighter than `+`"
    );
}

#[test]
fn call_expression_exposes_callee_and_args() {
    let (root, _) = unit("void f() { g(1, 2); }\n");
    let call = root
        .syntax()
        .descendants()
        .filter_map(cpp_parser::CppCallExpr::cast)
        .next()
        .expect("call expression");

    let callee = call.get_callee().expect("callee");
    assert!(matches!(callee, CppExpr::NameExpr(_)));
    assert_eq!(call.get_args().len(), 2);
}

#[test]
fn names_report_qualification() {
    let (root, _) = unit("std::vector<int> v;\n");
    let decl = root.get_declarations().next().unwrap();
    let name = decl
        .get_decl_specifiers()
        .and_then(|it| it.get_type_name())
        .expect("type name");

    assert!(name.is_qualified());
    assert_eq!(name.get_qualified_name(), "std::vector<int>");
}

#[test]
fn preprocessor_directives_are_typed() {
    let (root, _) = unit("#include <vector>\n#define N 3\n");
    let directives: Vec<_> = root
        .syntax()
        .descendants()
        .filter_map(cpp_parser::CppPreprocessorDirective::cast)
        .collect();

    assert_eq!(directives.len(), 2);
    assert_eq!(directives[0].get_directive_name().as_deref(), Some("include"));
    assert_eq!(
        directives[0].get_header_name_text().as_deref(),
        Some("vector")
    );
    assert_eq!(directives[1].get_directive_name().as_deref(), Some("define"));
}

#[test]
fn comments_report_their_kind() {
    let source = "// ordinary\n\n/// documented\n/** block doc */\nint x;\n";
    let (root, tree) = unit(source);

    let comments: Vec<_> = tree
        .get_red_root()
        .descendants_with_tokens()
        .filter_map(|it| it.into_token())
        .filter_map(cpp_parser::CppCommentToken::cast)
        .collect();

    // One token per comment *marker*, now that the doc layer has taken over the grouping: the three
    // comments are three openers, and the text between them is doc tokens rather than a comment token.
    let openers: Vec<_> = comments
        .iter()
        .filter(|comment| {
            matches!(
                comment.get_comment_kind(),
                cpp_parser::CppCommentKind::Line | cpp_parser::CppCommentKind::Block
            )
        })
        .collect();
    assert!(
        !openers.is_empty(),
        "the comment markers must still be in the tree: {:#?}",
        tree.get_red_root()
    );

    // What a consumer asks is which *documentation blocks* a file has, and the doc layer answers it.
    let doc_comments: Vec<_> = tree
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::DocComment)
        .collect();

    assert_eq!(
        doc_comments.len(),
        2,
        "the blank line separates the header from the documented declaration: {:#?}",
        tree.get_red_root()
    );
    assert_eq!(root.get_declarations().count(), 1);
}

/// The layer has to survive input that does not parse, because an editor sees it constantly.
#[test]
fn accessors_never_panic_on_broken_input() {
    let broken = [
        "",
        "int",
        "int x =",
        "void f(",
        "class {",
        "void f() { if (",
        "struct S { public:",
        "namespace",
        "template <",
        "export module",
        "import",
        "int x = a + ;",
        "#include",
    ];

    for source in broken {
        let (root, _) = unit(source);

        // Walk everything and touch every accessor a consumer plausibly would.
        for decl in root.get_declarations() {
            let _ = decl.kind_name();
            let _ = decl.get_name_text();
            let _ = decl.get_body();
            let _ = decl.get_class_def();
            let _ = decl.get_enum_def();
            let _ = decl.get_namespace_decl();
            let _ = decl.get_initializer();
            let _ = decl.is_function_def();
            let _ = decl.is_exported();

            if let Some(specs) = decl.get_decl_specifiers() {
                let _ = specs.get_type_text();
                let _ = specs.is_const();
                let _ = specs.is_static();
            }
            if let Some(declarator) = decl.get_declarator() {
                let _ = declarator.get_name_text();
                let _ = declarator.get_declarator_text();
                let _ = declarator.is_function();
                let _ = declarator.is_array();
                let _ = declarator.get_trailing_return_type();
            }
            if let Some(body) = decl.get_body() {
                for stat in body.get_stats() {
                    let _ = stat.syntax().text().to_string();
                }
            }
        }

        // The same for expressions, which is where a half-written file hurts most.
        for expr in root.syntax().descendants().filter_map(CppExpr::cast) {
            match expr {
                CppExpr::BinaryExpr(node) => {
                    let _ = node.get_lhs();
                    let _ = node.get_rhs();
                    let _ = node.get_operator_text();
                }
                CppExpr::CallExpr(node) => {
                    let _ = node.get_callee();
                    let _ = node.get_args();
                }
                CppExpr::UnaryExpr(node) => {
                    let _ = node.is_prefix();
                    let _ = node.get_operand();
                }
                CppExpr::NameExpr(node) => {
                    let _ = node.get_qualified_name();
                    let _ = node.is_qualified();
                }
                _ => {}
            }
        }
    }
}

/// `CppAst` dispatches on a node without the caller knowing what it is.
#[test]
fn cpp_ast_dispatches_on_node_kind() {
    use cpp_parser::CppAst;

    let (root, tree) = unit("int x = 1;\n");

    let mut seen_declaration = false;
    let mut seen_expr = false;
    for node in tree.get_red_root().descendants() {
        match CppAst::cast(node) {
            Some(CppAst::Declaration(_)) => seen_declaration = true,
            Some(CppAst::Expr(_)) => seen_expr = true,
            _ => {}
        }
    }

    assert!(seen_declaration, "should find a declaration");
    assert!(seen_expr, "should find the initializer expression");
    assert_eq!(root.get_declarations().count(), 1);
}

/// A declaration's accessors are the same whichever way it is reached.
#[test]
fn accessors_agree_across_entry_points() {
    let (root, tree) = unit("int counter = 1;\n");

    let by_child = root.get_declarations().next().unwrap();
    let by_descendant = tree
        .get_red_root()
        .descendants()
        .find_map(CppDeclaration::cast)
        .unwrap();

    assert_eq!(
        by_child.get_name_text(),
        by_descendant.get_name_text(),
        "the same declaration must look the same however it is found"
    );
    assert_eq!(by_child.syntax().text_range(), by_descendant.syntax().text_range());
}

/// A module unit written the way a real one is, with a namespace around it.
///
/// The other tests each pin down one construct. This one uses the layer for its actual purpose —
/// getting the declarations out of a file — and it is the test that catches a regression in the
/// *interaction* between constructs, which per-construct tests cannot see. `real_world.cpp` is
/// written the way a person writes C++ rather than the way a grammar is written: blank lines,
/// blank-line-separated sections, and a namespace wrapping everything.
#[test]
fn a_realistic_module_unit_yields_its_declarations() {
    let source = include_str!("real_world.cpp");
    let tree = CppParser::parse(source, ParserConfig::default());

    assert_eq!(
        tree.get_errors(),
        [],
        "the parser reports valid code as broken; the errors are {:?}",
        tree.get_errors()
    );
    assert_eq!(
        tree.to_source_text(),
        source,
        "the tree must reproduce the file exactly"
    );

    let root = CppTranslationUnit::cast(tree.get_red_root()).unwrap();

    // The unit itself is a module: `module;` ... `export module shapes;`
    let module = root.get_module_decl().expect("module declaration");
    assert_eq!(
        module.get_name().map(|it| it.get_name_text()),
        Some("shapes".to_string())
    );

    // `export int helper();` is a top-level declaration, outside the namespace. The `export using
    // Point = ...;` before it is a `UsingDecl`, which `CppDeclaration::cast` accepts (it declares a
    // name) even though `get_declarations` never yields one — so both are here, and the alias
    // reports itself as one rather than as a nameless variable.
    let top_level: Vec<String> = root
        .get_declarations()
        .map(|decl| format!("{} {}", decl.kind_name(), decl.get_name_text().unwrap_or_default()))
        .collect();
    assert_eq!(top_level, vec!["alias Point", "function helper"]);

    let namespace = root.get_namespaces().next().expect("namespace shapes");
    assert_eq!(
        namespace.get_name().and_then(|it| it.get_name_text()),
        Some("shapes".to_string())
    );

    let members: Vec<String> = namespace
        .get_declarations()
        .map(|stat| match stat {
            CppStat::Declaration(decl) => {
                format!("{} {}", decl.kind_name(), decl.get_name_text().unwrap_or_default())
            }
            other => format!("{other:?}"),
        })
        .collect();

    assert_eq!(
        members,
        vec![
            "variable kMax",
            "enum Kind",
            "class Point",
            "class Grid",
            "class Shape",
            "function distance",
        ]
    );
}

// ============================================================================
// Structured bindings
// ============================================================================

/// The names of the structured bindings in a source, in order.
fn binding_names(source: &str) -> Vec<Vec<String>> {
    let (root, _) = unit(source);

    root.get_declarations()
        .filter_map(|declaration| declaration.get_init_declarator())
        .filter_map(|init| init.get_structured_binding())
        .map(|binding| binding.get_name_texts())
        .collect()
}

/// A binding pattern is its own node, and the names in it are the ones a consumer indexes by.
///
/// The alternative the tree could have had — one declarator named `[a, b]` — would make "what does
/// `auto [a, b] = pair;` declare?" answer with a string that is not a name at all.
#[test]
fn a_structured_binding_reports_its_names() {
    assert_eq!(
        binding_names("auto [a, b] = pair;\n"),
        vec![vec!["a".to_string(), "b".to_string()]]
    );
}

/// The reference forms put the pattern after an abstract declarator, which is a different position in
/// the grammar — and the one that needed its own entry point in the declarator rule.
#[test]
fn a_structured_binding_survives_a_reference_prefix() {
    for source in [
        "auto& [k, v] = map;\n",
        "auto&& [k, v] = map;\n",
        "const auto& [k, v] = map;\n",
    ] {
        assert_eq!(
            binding_names(source),
            vec![vec!["k".to_string(), "v".to_string()]],
            "{source:?}"
        );
    }
}

/// A pack expansion binds the names it names; the `...` is a token, not a name.
#[test]
fn a_pack_expansion_binding_reports_its_name() {
    assert_eq!(
        binding_names("auto [... xs] = rest;\n"),
        vec![vec!["xs".to_string()]]
    );
    assert_eq!(
        binding_names("auto [a, ...] = rest;\n"),
        vec![vec!["a".to_string()]]
    );
}

/// An ordinary declarator is not a structured binding, and says so rather than answering with an
/// empty pattern.
#[test]
fn an_ordinary_declarator_is_not_a_structured_binding() {
    let (root, _) = unit("int arr[3];\nauto [a, b] = pair;\n");

    let kinds: Vec<bool> = root
        .get_declarations()
        .filter_map(|declaration| declaration.get_init_declarator())
        .map(|init| init.is_structured_binding())
        .collect();

    assert_eq!(kinds, vec![false, true]);
}

// ============================================================================
// consteval if
// ============================================================================

/// `if consteval` parses, and its body is a real block rather than an error.
///
/// The form takes no condition, which is what makes it worth a test: a parser that asks for one
/// reports `expected (` against the `{` that is really the body, and C++23 code then looks broken.
#[test]
fn consteval_if_has_a_body_and_no_condition() {
    for source in [
        "void f() { if consteval { g(); } }\n",
        "void f() { if !consteval { g(); } }\n",
        "void f() { if consteval { g(); } else { h(); } }\n",
    ] {
        let (root, tree) = unit(source);
        assert_eq!(tree.get_errors(), [], "{source:?} must parse cleanly");

        let if_stat = root
            .get_declarations()
            .filter_map(|declaration| declaration.get_body())
            .flat_map(|body| body.get_stats())
            .find_map(|stat| match stat {
                CppStat::IfStat(if_stat) => Some(if_stat),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{source:?} has an if statement"));

        let text = if_stat.syntax().text().to_string();
        assert!(
            text.contains("consteval"),
            "the keyword is part of the statement: {text:?}"
        );
    }
}

/// An attribute list consumes **both** of its closing brackets.
///
/// The off-by-one this pins was invisible in the error list and showed up as damage to the *following*
/// token instead: `[[nodiscard]] int f();` left the last `]` unconsumed, so the enclosing rule met `]`
/// where it wanted `int` and produced an `ErrorNode` — a stray syntax error in perfectly ordinary code,
/// pointing at a token that had nothing wrong with it.
///
/// Written as a property over many placements rather than one case, because the same rule parses
/// attributes everywhere they can appear and the bug was in the rule, not in any one caller.
///
/// # Placements that are not covered here
///
/// Attributes are allowed in more positions than these, and a few of them do not parse yet — see
/// `known_unparsed_attribute_positions` below. They are absent from this list deliberately: a failing
/// case in a passing test says nothing about the bug being pinned, and these gaps are a separate piece
/// of work from the bracket accounting.
#[test]
fn attribute_lists_consume_both_closing_brackets() {
    let sources = [
        "[[nodiscard]] int f();\n",
        "[[noreturn]] void g();\n",
        "class C [[deprecated]] { int x; };\n",
        "enum class E [[deprecated]] { A };\n",
        "struct S { [[maybe_unused]] int x; };\n",
        "[[nodiscard, deprecated]] int i();\n",
        "[[gnu::always_inline]] inline void j();\n",
        "void k([[maybe_unused]] int x);\n",
        "[[nodiscard]] int l() { return 1; }\n",
        "int m [[gnu::aligned(16)]];\n",
        "struct A { [[nodiscard]] virtual int q() const; };\n",
        "export module mod [[deprecated]];\n",
        "import std [[deprecated]];\n",
    ];

    for source in sources {
        let (_, tree) = unit(source);

        assert_eq!(tree.get_errors(), [], "{source:?} must parse cleanly");
        assert_eq!(tree.to_source_text(), source, "{source:?} must stay lossless");

        // Every `[[` in the source is matched by a `]]` inside an `AttributeList`, so none is left over for
        // an enclosing rule to trip over.
        let attribute_text: String = tree
            .get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::AttributeList)
            .map(|node| node.text().to_string())
            .collect();

        let opened = source.matches("[[").count();
        let closed = attribute_text.matches("]]").count();
        assert_eq!(
            closed, opened,
            "{source:?}: {opened} attribute list(s) opened but {closed} closed — got {attribute_text:?}"
        );
    }
}

/// Attribute positions that do **not** parse yet, recorded so the gap is visible rather than rediscovered.
///
/// All four are standard positions, and all four fail the same way: the declaration parses far enough to
/// report something, then the attribute is met where a declarator or a `;` was expected, so the error
/// points at the attribute rather than at anything wrong with the code. They are unrelated to the bracket
/// accounting above — a leading attribute in the same declaration parses — and fixing them means teaching
/// the declarator, class-head, and alias rules to consume an attribute list, which is a change to those
/// rules rather than to this one.
///
/// The test asserts the *current* behaviour so that fixing the gap is a deliberate edit here rather than a
/// silent change, which is the same reason the parser's other limitation lists exist.
#[test]
fn known_unparsed_attribute_positions() {
    let still_broken = [
        // Attribute after the declarator, before the `;`.
        "int h() [[carries_dependency]];\n",
        // Attribute between the `namespace` keyword and its name.
        "namespace [[deprecated]] n { int x; }\n",
        // Attribute on an alias declaration.
        "using T [[deprecated]] = int;\n",
        // Attribute on a template declaration, between the parameter list and the type.
        "template <typename T> [[nodiscard]] T p();\n",
    ];

    for source in still_broken {
        let (_, tree) = unit(source);

        assert!(
            !tree.get_errors().is_empty(),
            "{source:?} now parses cleanly — move it into `attribute_lists_consume_both_closing_brackets`"
        );
        assert_eq!(
            tree.to_source_text(),
            source,
            "{source:?} must stay lossless even while it does not parse"
        );
    }
}

/// A nested attribute list balances too, and the brackets inside it are counted rather than the rule
/// stopping at the first `]`.
#[test]
fn a_nested_attribute_list_balances_its_brackets() {
    let source = "[[outer([[inner]])]] int f();\n";
    let (_, tree) = unit(source);

    assert_eq!(tree.get_errors(), [], "must parse cleanly");
    assert_eq!(tree.to_source_text(), source);

    let text: String = tree
        .get_red_root()
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::AttributeList)
        .map(|node| node.text().to_string())
        .collect();

    assert_eq!(text.matches("]]").count(), 2, "both lists closed: {text:?}");
}

/// An attribute list opened and never closed is reported, and the rest of the file does not become
/// declarations.
///
/// Recorded as the behaviour rather than as a defect, because the alternative is worse. `[[nodiscard` at
/// the start of a line is genuinely ambiguous — `[[` also opens a **lambda capture list**, and `[[x](int
/// y){...}]` and `[[nodiscard]]` agree on everything up to the first identifier. So there is no point at
/// which a recovery could say where the attribute was meant to end, and a wrong guess turns one half-typed
/// `[[` into a cascade of spurious errors on code that is fine.
///
/// The two things that do matter are asserted instead: the parse stays lossless, and an error is reported.
/// The message is deliberately *not* pinned — which rule ends up reporting the ambiguity is an
/// implementation detail, and the useful property is that the input is not silently accepted.
#[test]
fn an_unterminated_attribute_list_is_reported_and_consumes_the_rest() {
    let source = "[[nodiscard\nint f();\nint g;\n";
    let (root, tree) = unit(source);

    assert_eq!(tree.to_source_text(), source, "still lossless");
    assert!(
        !tree.get_errors().is_empty(),
        "the input is not silently accepted"
    );

    // Nothing after the `[[` is a declaration, because all of it was taken by the construct that opened
    // there. This is the part worth pinning: a broken attribute must not leave the *following* code
    // looking well-formed when it was never parsed as code at all.
    let declarations: Vec<String> = root
        .get_declarations()
        .map(|decl| decl.get_name_text().unwrap_or_default())
        .collect();

    assert!(
        declarations.is_empty(),
        "the rest of the file is inside the unclosed construct: {declarations:?}"
    );
}
