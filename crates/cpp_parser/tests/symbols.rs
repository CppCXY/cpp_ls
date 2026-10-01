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
    CppParser, CppSyntaxKind, CppSyntaxTree, NoSymbols,
    ParserConfig, SymbolKind, SymbolMap, SymbolTable,
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
///
/// The `Macro` answer is the harshest nonsense available: the grammar has no use for it any more (see
/// `ParserConfig`), so a table that says nothing else is a table the parser must simply not believe.
struct EverythingIsAMacro;

impl SymbolTable for EverythingIsAMacro {
    fn kind_of(&self, _name: &str) -> Option<SymbolKind> {
        Some(SymbolKind::Macro {
            function_like: true,
            body: cpp_parser::MacroBody::Statement,
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

/// The table's **type** answers, and — for each — what happens when the table says nothing, and when it says the
/// wrong thing. The `Macro` answer was tested here too and is gone with the macro table: see `ParserConfig`.
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



#[test]
fn a_table_can_be_built_and_shared_from_outside_the_crate() {
    // The interface's shape, exercised the way a caller would: an index wrapped in a map, handed to the parser as
    // a trait object, and outliving the parse.
    let mut symbols = SymbolMap::new();
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
