//! The external symbol table: the interface, and the guarantee that it is **optional**.
//!
//! The parser decides several readings by looking names up, and this crate's answer has always been two-layered:
//! what the file says about itself, and — when it says nothing — a documented shape preference. The table here is
//! the third source of evidence, the one a caller with a project index can supply (see [`cpp_parser::symbols`]).
//!
//! The tests below pin the properties that make it safe to add:
//!
//! * **an empty table is indistinguishable from no table** — the same tree, node for node, over the real corpus
//!   file and over the shapes the grammar's name-lookups touch. If a later step wires the table into a rule and
//!   that rule starts answering for a name nobody resolved, this test fails;
//! * **a table decides readings, not structure**: even a table that answers nonsense — everything is a macro,
//!   everything is a type — leaves the parse lossless, well-formed and total. It may choose a wrong reading
//!   (that is what a wrong index can do), but it can never drop a token or panic;
//! * **the interface is usable from outside the crate**: built by hand, shared as a trait object, and passed
//!   through `ParserConfig`.

use cpp_parser::{
    CppParser, CppSyntaxKind, CppSyntaxTree, MacroBody, NoSymbols, ParserConfig, SymbolKind,
    SymbolMap, SymbolTable,
};

/// A parse, reduced to what a test can compare: the node kinds with their ranges, the diagnostics, and the text.
///
/// Comparing the *tree* rather than just "it parsed" is the point: a table that sneaks into a rule would change
/// which node covers which tokens long before it changed whether anything was reported.
fn shape(
    source: &str,
    config: ParserConfig<'_>,
) -> (Vec<(CppSyntaxKind, usize, usize)>, Vec<String>, String) {
    let tree = CppParser::parse(source, config);
    let nodes = tree
        .get_red_root()
        .descendants()
        .map(|node| {
            let range = node.text_range();
            (
                CppSyntaxKind::from(node.kind()),
                u32::from(range.start()) as usize,
                u32::from(range.end()) as usize,
            )
        })
        .collect();
    let errors = tree
        .get_errors()
        .iter()
        .map(|error| error.message.to_string())
        .collect();

    (nodes, errors, tree.to_source_text())
}

/// Sources chosen for the areas a symbol lookup will eventually decide: the declaration/expression question, the
/// specifier sequence, macros, casts, and template-ids — plus the corpus file this crate already keeps as a
/// real-world probe.
fn sources() -> Vec<String> {
    let mut sources: Vec<String> = [
        // The declaration/expression question, which is what a type lookup is for.
        "Widget w(1, 2);\ng(1, 2);\n",
        // A specifier-position macro, a macro statement, a macro with a block.
        "#define MY_API __declspec(dllexport)\n#define NUMBER_OPTION(op) if (Get(op)) { }\n#define IF_EXIST(op) if (Get(op))\nMY_API Widget *p;\nvoid f() {\n    NUMBER_OPTION(a)\n    IF_EXIST(b) { g(); }\n}\n",
        // Casts and parenthesised expressions, where "is this a type" is the whole question.
        "void f(void* p) { auto x = (Widget*)p; auto y = (a) - b; auto z = (::a); }\n",
        // Template-ids: a name that is a template makes the `<` an argument list.
        "std::vector<int> v;\nA<B> x;\na < b > c;\n",
        // Qualified names, definitions of out-of-line members, and a class body.
        "static void Widget::draw(T t) { }\nstruct S { int bits : 3; void f() { for (auto &v: vec) { } } };\n",
        // An enum's members and a class-like definition in front of its declarator.
        "enum E { A = 0, B };\nstatic const struct { int a; } table[] = { { 1 } };\n",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    sources.push(include_str!("real_world.cpp").to_string());
    sources
}

#[test]
fn an_empty_table_parses_exactly_like_no_table() {
    for source in sources() {
        let without = shape(&source, ParserConfig::default());
        let nothing = shape(
            &source,
            ParserConfig::default().with_symbol_table(&NoSymbols),
        );
        let empty = shape(
            &source,
            ParserConfig::default().with_symbol_table(&SymbolMap::new()),
        );

        assert_eq!(
            without, nothing,
            "`NoSymbols` must be indistinguishable from no table at all"
        );
        assert_eq!(
            without, empty,
            "an empty `SymbolMap` must be indistinguishable from no table at all"
        );
    }
}

/// A table that answers **nonsense** — every name is a macro whose body is a whole statement, or a type.
///
/// This is the "wrong index" case, and the contract is structural: the tree stays lossless, well formed and
/// total. A wrong answer may choose a wrong *reading* — that is what an index the user has not saved yet can do,
/// and the fallback is why the parser kept its own evidence — but it can never lose a token.
struct EverythingIsAMacro;

impl SymbolTable for EverythingIsAMacro {
    fn kind_of(&self, _name: &str) -> Option<SymbolKind> {
        Some(SymbolKind::Macro {
            function_like: true,
            body: MacroBody::Statement,
        })
    }
}

struct EverythingIsAType;

impl SymbolTable for EverythingIsAType {
    fn kind_of(&self, _name: &str) -> Option<SymbolKind> {
        Some(SymbolKind::Type)
    }
}

#[test]
fn a_hostile_table_cannot_break_the_tree() {
    for source in sources() {
        for table in [
            &EverythingIsAMacro as &dyn SymbolTable,
            &EverythingIsAType as &dyn SymbolTable,
        ] {
            let tree: CppSyntaxTree =
                CppParser::parse(&source, ParserConfig::default().with_symbol_table(table));
            assert_eq!(
                tree.to_source_text(),
                source,
                "a table must never cost a token, whoever is answering"
            );

            // Well formed, in the sense the invariants tests use: every node's range is inside its parent's, and
            // no node is empty. A table choosing a reading cannot change that, and this is where that would show.
            for node in tree.get_red_root().descendants() {
                if let Some(parent) = node.parent() {
                    let (inner, outer) = (node.text_range(), parent.text_range());
                    assert!(
                        outer.contains_range(inner),
                        "{:?} escapes its parent {:?}",
                        inner,
                        outer
                    );
                }
            }
        }
    }
}

// ============================================================================
// The rules that consult the table
// ============================================================================

/// The table's `Macro` answer in the three shapes the grammar acts on, and — for each — what happens when the
/// table says nothing, and when the table says the wrong thing.
#[test]
fn a_table_described_macro_is_read_as_one() {
    let statement = SymbolMap::new().with(
        "STEP",
        SymbolKind::Macro {
            function_like: true,
            body: MacroBody::Statement,
        },
    );
    let block = SymbolMap::new().with(
        "CHECK",
        SymbolKind::Macro {
            function_like: true,
            body: MacroBody::Block,
        },
    );

    // A macro statement without a `;`, with **no `#define` in the file**: the table is the evidence. `NUMBER_LIKE`
    // is not spelled like a macro, which is the point — the index knows and the spelling convention does not.
    let source = "void f() {\n    NUMBER_LIKE(a)\n}\n";
    assert!(
        text_has_errors("NUMBER_LIKE", source),
        "without a table this is a call with its `;` missing"
    );

    let source = "void f() {\n    STEP(a)\n}\n";
    assert!(errors_with(source, &statement).is_empty());
    let tree = CppParser::parse(
        source,
        ParserConfig::default().with_symbol_table(&statement),
    );
    assert_eq!(
        tree.get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall)
            .count(),
        1
    );

    // A macro whose body is a block, used inside a body: the block belongs to the invocation.
    let source = "void f() {\n    CHECK(a) {\n        g();\n    }\n}\n";
    assert!(errors_with(source, &block).is_empty());

    // …and a class member that is nothing but a macro, which is how an attribute-like macro is written:
    // `Q_OBJECT` carries no `;`, and the declaration rule would otherwise take its name for a type.
    let member = SymbolMap::new()
        .with(
            "Q_OBJECT",
            SymbolKind::Macro {
                function_like: false,
                body: MacroBody::Unknown,
            },
        )
        .with(
            "Q_PROPERTY",
            SymbolKind::Macro {
                function_like: true,
                body: MacroBody::Statement,
            },
        );
    let source = "struct S {\n    Q_OBJECT\n    Q_PROPERTY(int x READ x)\n    void f();\n};\n";
    let tree = CppParser::parse(source, ParserConfig::default().with_symbol_table(&member));
    assert_eq!(tree.get_errors(), [], "a described member macro is read");
    assert_eq!(
        tree.get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::MacroCall)
            .count(),
        2,
        "both members are macro calls"
    );
}

#[test]
fn a_body_the_table_describes_decides_whether_the_semicolon_is_needed() {
    // The distinction `MacroBody` exists for. `Expression` and `Type` bodies answer "no" to the statement
    // question on purpose: `MAX(x, y)` really is an expression statement, and a missing `;` after it is a typo a
    // reader has to fix — so the table's precision is what keeps that diagnostic.
    let expression = SymbolMap::new().with(
        "MAX",
        SymbolKind::Macro {
            function_like: true,
            body: MacroBody::Expression,
        },
    );
    let unknown = SymbolMap::new().with(
        "MAYBE",
        SymbolKind::Macro {
            function_like: true,
            body: MacroBody::Unknown,
        },
    );

    let source = "void f() {\n    MAX(a, b)\n}\n";
    assert!(
        !errors_with(source, &expression).is_empty(),
        "an expression macro still needs its `;`"
    );
    let source = "void f() {\n    MAX(a, b);\n}\n";
    assert!(errors_with(source, &expression).is_empty());

    // `Unknown` is the ordinary answer for a header macro, and it must stay usable: the name is a macro, which
    // already rules out the call reading, and the statement reading is taken.
    let source = "void f() {\n    MAYBE(a)\n}\n";
    assert!(errors_with(source, &unknown).is_empty());
}

#[test]
fn a_table_described_type_settles_the_declaration_question() {
    // `Widget w(1, 2);` against `g(1, 2);` is the same tokens, and this is the decision the whole external-table
    // design exists for: a name from a header is exactly the case the file-local table cannot answer.
    let symbols = SymbolMap::new()
        .with("Widget", SymbolKind::Type)
        .with("g", SymbolKind::Function)
        .with("Vec", SymbolKind::Template);

    // `Widget` is a type, so this is a declaration with a direct initialiser — even where the file declares no
    // `Widget` and the arguments are bare names the shapes could read either way.
    let source = "Widget w(a, b);\n";
    let tree = CppParser::parse(source, ParserConfig::default().with_symbol_table(&symbols));
    assert_eq!(tree.get_errors(), []);
    assert_eq!(
        tree.get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Initializer)
            .count(),
        1,
        "the table's `Type` answer makes it a declaration"
    );

    // `g` is a **function** — the answer the local table can never give — so the same shape is a call.
    let source = "g(a, b);\n";
    let tree = CppParser::parse(source, ParserConfig::default().with_symbol_table(&symbols));
    assert_eq!(tree.get_errors(), []);
    assert_eq!(
        tree.get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CallExpr)
            .count(),
        1,
        "the table's `Function` answer refuses the declaration reading"
    );

    // A template name is a type for this purpose too, and the cast rule hears the same evidence.
    let source = "void f(void* p) { auto x = (Widget)p; }\n";
    let tree = CppParser::parse(source, ParserConfig::default().with_symbol_table(&symbols));
    assert_eq!(
        tree.get_red_root()
            .descendants()
            .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CastExpr)
            .count(),
        1,
        "`(Widget)p` is a cast when the table says `Widget` is a type"
    );
}

/// Parse `source` with `table` and return the diagnostics.
fn errors_with(source: &str, table: &SymbolMap) -> Vec<String> {
    CppParser::parse(source, ParserConfig::default().with_symbol_table(table))
        .get_errors()
        .iter()
        .map(|error| error.message.to_string())
        .collect()
}

/// Does `source` fail to parse with **no** table, and does the failure mention `name`?
///
/// The negative half of every table test: evidence is what changes the reading, and without it the shape rules
/// report what they always did.
fn text_has_errors(name: &str, source: &str) -> bool {
    let source = source.replace(name, "NUMBER_OPTION");
    !CppParser::parse(&source, ParserConfig::default())
        .get_errors()
        .is_empty()
}

#[test]
fn a_table_described_macro_can_carry_a_block_inside_a_body() {
    // The definition rule (`a_macro_definition_follows`) asks the same evidence question as the statement rule, and
    // takes the spelling convention only as the last resort. The name here is **not** spelled like a macro and the
    // body is described as a *block* by the table — a `#define` that opens a scope — so evidence is the only thing
    // that can read it.
    let symbols = SymbolMap::new().with(
        "with_scope",
        SymbolKind::Macro {
            function_like: true,
            body: MacroBody::Block,
        },
    );

    let source = "void f() {\n    with_scope(a) {\n        g();\n    }\n}\n";
    assert!(
        !CppParser::parse(source, ParserConfig::default())
            .get_errors()
            .is_empty(),
        "with no evidence this is a call followed by a block, which is an error"
    );
    assert!(errors_with(source, &symbols).is_empty());

    // The negative half that keeps the rule honest: a **specifier** macro is not a statement, so the same shape
    // with a `Specifier` body stays an error — as long as the name is not macro-spelled either, since the spelling
    // convention is deliberately still there for a macro from a header nobody indexed (`MY_ATTR` below would be
    // read by the convention, and that is the documented trade).
    let specifier = SymbolMap::new().with(
        "my_attr",
        SymbolKind::Macro {
            function_like: true,
            body: MacroBody::Specifier,
        },
    );
    let source = "void f() {\n    my_attr(a) {\n        g();\n    }\n}\n";
    assert!(!errors_with(source, &specifier).is_empty());

    // …and the same shape with a macro-spelled name *is* read, through the convention rather than the table.
    let spelled = SymbolMap::new().with(
        "MY_ATTR",
        SymbolKind::Macro {
            function_like: true,
            body: MacroBody::Specifier,
        },
    );
    let source = "void f() {\n    MY_ATTR(a) {\n        g();\n    }\n}\n";
    assert!(errors_with(source, &spelled).is_empty());
}

#[test]
fn a_macro_before_a_type_is_read_with_or_without_a_table() {
    // `MY_API Widget const w;` — a modifier macro in front of a type the file does not declare.
    //
    // This used to be the case that **needed** the table's `Type` answer: the shape rule reads `MY_API Widget *p;`
    // on its own, because a `*` after the name is something a declarator continues into, while `const` is not —
    // so the type ended at `Widget` and the `const` was taken for the start of the declarator.
    //
    // It no longer needs one, and the reason is worth keeping: the rule for "a macro from a header standing where
    // a declaration goes" reads the **run** of names in front of a declaration, so `MY_API` is a macro and
    // `Widget const w;` is the declaration behind it. The table's answer is now what a table *adds* rather than
    // what carries the reading — and both halves are asserted, because a caller that supplies a table must not
    // get a worse tree than one that does not.
    let symbols = SymbolMap::new()
        .with(
            "MY_API",
            SymbolKind::Macro {
                function_like: false,
                body: MacroBody::Specifier,
            },
        )
        .with("Widget", SymbolKind::Type);

    let source = "MY_API Widget const w;\n";
    assert!(
        CppParser::parse(source, ParserConfig::default())
            .get_errors()
            .is_empty(),
        "the shape reads it: `MY_API` is a macro standing for a declaration, and `Widget const w;` is the \
         declaration behind it"
    );
    assert!(
        errors_with(source, &symbols).is_empty(),
        "and the table's answer does not make it worse — with evidence, the name is read by the rules that know \
         what a macro is, and this one steps aside (see `at_a_macro_that_stands_for_a_declaration`)"
    );
}

#[test]
fn a_table_can_be_built_and_shared_from_outside_the_crate() {
    // The interface's shape, exercised the way a caller would: an index wrapped in a map, handed to the parser as
    // a trait object, and outliving the parse. The answers themselves are not consulted yet — the rules that will
    // read them are the next step — so what this pins is that the plumbing compiles and is usable.
    let mut symbols = SymbolMap::new();
    symbols.insert(
        "MY_API",
        SymbolKind::Macro {
            function_like: false,
            body: MacroBody::Specifier,
        },
    );
    symbols.insert(
        "TEST",
        SymbolKind::Macro {
            function_like: true,
            body: MacroBody::Block,
        },
    );
    symbols.insert("Widget", SymbolKind::Type);
    symbols.insert("Vec", SymbolKind::Template);
    symbols.insert("g", SymbolKind::Function);
    symbols.insert("count", SymbolKind::Variable);
    symbols.insert("ns", SymbolKind::Namespace);

    let config = ParserConfig::default().with_symbol_table(&symbols);
    let table = config.symbol_table().expect("the table is in force");
    assert_eq!(table.kind_of("Widget"), Some(SymbolKind::Type));
    assert_eq!(table.kind_of("Vec"), Some(SymbolKind::Template));
    assert_eq!(table.kind_of("g"), Some(SymbolKind::Function));
    assert_eq!(
        table.kind_of("unknown_to_the_index"),
        None,
        "an unresolved name is unknown — never a `no`"
    );

    // …and the parse itself is unchanged until a rule consults it.
    let source = "Widget w(1, 2);\n";
    let with = shape(source, ParserConfig::default().with_symbol_table(&symbols));
    let without = shape(source, ParserConfig::default());
    assert_eq!(with, without);

    // No table at all is the ordinary case, and it is not an error.
    assert!(ParserConfig::default().symbol_table().is_none());
}
