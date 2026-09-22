//! Building scopes and bindings from a file's syntax tree.
//!
//! The tests are organised by the question a consumer asks, and the first thing most of them do is render the
//! scope tree as a single line — `KIND{names}(children)` — because the shape is what is easy to get subtly
//! wrong and hard to notice. A scope one level too deep, or a parameter bound in the enclosing block instead
//! of the function, produces a tree that still looks plausible and gives wrong completions everywhere.
//!
//! Scope is what these tests are about, so the renderings show scopes and the names in them, and nothing else.

use cpp_code_analysis::{BindingKind, ScopeKind, SymbolTable, build_scopes};
use cpp_parser::{CppParser, ParserConfig};

/// Parse, check the parse is sound, and build the scopes.
fn scopes(source: &str) -> SymbolTable {
    let tree = CppParser::parse(source, ParserConfig::default());

    assert_eq!(
        tree.to_source_text(),
        source,
        "the parse must stay lossless before anything is read out of it"
    );
    assert_eq!(
        tree.get_errors(),
        [],
        "the input must parse cleanly for its shape to mean anything: {:?}",
        tree.get_errors()
    );

    build_scopes(&tree.get_red_root())
}

/// The scope tree as one line: `KIND{name, name}(children)`.
///
/// A scope with no bindings prints as `KIND` rather than `KIND{}`, so a rendering that lost its bindings is
/// distinguishable from one that never had any.
fn shape(table: &SymbolTable) -> String {
    fn render(table: &SymbolTable, id: cpp_code_analysis::ScopeId, out: &mut String) {
        let scope = table.scope(id).expect("a scope that exists");

        let kind = match scope.kind {
            ScopeKind::TranslationUnit => "File",
            ScopeKind::Namespace => "Ns",
            ScopeKind::Class => "Class",
            ScopeKind::Enum => "Enum",
            ScopeKind::Function => "Fn",
            ScopeKind::Block => "Block",
            ScopeKind::TemplateParameters => "Params",
            ScopeKind::Lambda => "Lambda",
        };

        out.push_str(kind);

        if !scope.bindings.is_empty() {
            let names: Vec<String> = scope
                .bindings
                .iter()
                .map(|binding| binding.name.text())
                .collect();
            out.push('{');
            out.push_str(&names.join(","));
            out.push('}');
        }

        if !scope.children.is_empty() {
            out.push('(');
            for (index, child) in scope.children.iter().enumerate() {
                if index > 0 {
                    out.push(' ');
                }
                render(table, *child, out);
            }
            out.push(')');
        }
    }

    let mut out = String::new();
    if let Some(root) = table.root() {
        render(table, root, &mut out);
    }
    out
}

/// The kind of the binding a scope holds for `name`, or `None`.
fn kind_of(
    table: &SymbolTable,
    scope: cpp_code_analysis::ScopeId,
    name: &str,
) -> Option<BindingKind> {
    table
        .scope(scope)?
        .bindings
        .iter()
        .find(|binding| binding.name.identifier_text() == Some(name))
        .map(|binding| binding.kind)
}

/// All bindings of a name in a scope, as `(kind, name_range)` pairs.
fn bindings_of(
    table: &SymbolTable,
    scope: cpp_code_analysis::ScopeId,
    name: &str,
) -> Vec<(BindingKind, cpp_parser::SourceRange)> {
    table
        .scope(scope)
        .map(|scope| {
            scope
                .bindings
                .iter()
                .filter(|binding| binding.name.identifier_text() == Some(name))
                .map(|binding| (binding.kind, binding.name_range))
                .collect()
        })
        .unwrap_or_default()
}

// ============================================================================
// The shape of a file's scopes
// ============================================================================

/// An empty file still has a file scope.
///
/// A table with no root would make "the file's declarations" undefined rather than empty, and every consumer
/// walking from the root would need a special case for it.
#[test]
fn an_empty_file_has_a_file_scope() {
    let table = scopes("");

    assert_eq!(shape(&table), "File");
    assert_eq!(table.len(), 1);
    assert!(table.root().is_some());
}

/// Top-level declarations land in the file scope, in one scope and not one each.
///
/// The commonest shape there is, and the one a naive walk gets wrong by opening a scope per declaration.
/// The names print in sorted order, not source order.
#[test]
fn top_level_declarations_share_the_file_scope() {
    let table = scopes("int count;\nvoid f();\nclass Widget {};\n");

    assert_eq!(shape(&table), "File{Widget,count,f}");
    assert_eq!(
        table.len(),
        1,
        "an empty class body opens no scope, so there is only the file's"
    );
}

/// A namespace opens a scope, and its contents go in it rather than in the file scope.
///
/// The names inside are printed in the table's order, which is sorted by name rather than by the order they
/// appear in the file: `f` sorts before `x` even though `x` is declared first.
#[test]
fn a_namespace_opens_a_scope() {
    let table = scopes("namespace ns { int x; void f(); }\n");

    assert_eq!(shape(&table), "File{ns}(Ns{f,x})");
}

/// Nested namespaces nest, and each declares its own name in its parent.
#[test]
fn nested_namespaces_nest() {
    let table = scopes("namespace outer { namespace inner { int x; } }\n");

    assert_eq!(shape(&table), "File{outer}(Ns{inner}(Ns{x}))");
}

/// A class opens a scope, and its members go in it.
///
/// The class's own name is declared in the *enclosing* scope, which is the part that is easy to get wrong: a
/// class declared inside its own scope would not be found by anything outside it.
#[test]
fn a_class_declares_its_name_outside_itself() {
    let table = scopes("class Widget { int value_; void size(); };\n");
    let file = table.root().unwrap();

    assert_eq!(
        shape(&table),
        "File{Widget}(Class{size,value_})",
        "the members are inside; the class is outside"
    );
    assert_eq!(kind_of(&table, file, "Widget"), Some(BindingKind::Class));

    let class = table.scope(file).unwrap().children[0];
    assert_eq!(
        kind_of(&table, class, "Widget"),
        None,
        "not a member of itself"
    );
    assert_eq!(
        kind_of(&table, class, "value_"),
        Some(BindingKind::Variable)
    );
    assert_eq!(kind_of(&table, class, "size"), Some(BindingKind::Function));
}

/// A class inside a class nests, and both names are visible where they are declared.
#[test]
fn a_nested_class_nests() {
    let table = scopes("struct Outer { struct Inner { int x; }; };\n");

    assert_eq!(shape(&table), "File{Outer}(Class{Inner}(Class{x}))");
}

/// `class Widget;` declares the name even though no body follows.
///
/// A forward declaration makes `Widget` known as a class, so a table that only read definitions would leave
/// every name introduced by a forward declaration unresolvable — which in real headers is most of them.
#[test]
fn a_forward_declaration_declares_the_name() {
    let table = scopes("class Widget;\nstruct Point;\n");

    assert_eq!(shape(&table), "File{Point,Widget}");
    assert_eq!(
        kind_of(&table, table.root().unwrap(), "Widget"),
        Some(BindingKind::Class)
    );
}

/// An enum opens a scope, and its enumerators are in it.
#[test]
fn an_enum_holds_its_enumerators() {
    let table = scopes("enum class Color { Red, Green = 2 };\n");

    assert_eq!(shape(&table), "File{Color}(Enum{Green,Red})");

    let file = table.root().unwrap();
    let enumeration = table.scope(file).unwrap().children[0];

    assert_eq!(
        kind_of(&table, enumeration, "Red"),
        Some(BindingKind::Enumerator)
    );
    assert_eq!(
        kind_of(&table, file, "Red"),
        None,
        "an enumerator is not in the file scope"
    );
}

/// A function declares its name outside and binds its parameters inside.
///
/// The distinction that makes `for (int i = 0; ...)` and `void f(int i)` behave: a parameter is visible in the
/// body and not outside it, and the body does not nest inside the parameter list.
#[test]
fn a_function_binds_its_parameters_in_its_own_scope() {
    let table = scopes("int add(int a, int b) { int sum = a + b; return sum; }\n");

    assert_eq!(shape(&table), "File{add}(Fn{a,b,sum})");

    let file = table.root().unwrap();
    let function = table.scope(file).unwrap().children[0];

    assert_eq!(kind_of(&table, file, "add"), Some(BindingKind::Function));
    assert_eq!(
        kind_of(&table, file, "a"),
        None,
        "a parameter is not in the file scope"
    );
    assert_eq!(kind_of(&table, function, "a"), Some(BindingKind::Variable));
    assert_eq!(
        kind_of(&table, function, "sum"),
        Some(BindingKind::Variable)
    );
}

/// A parameter is bound once, not once as a parameter and again as a body declaration.
///
/// The body's contents are walked into the function's scope directly, so a parameter list that also reached the
/// body's walk would bind every parameter twice — and a consumer showing overloads would show each one twice.
#[test]
fn a_parameter_is_bound_exactly_once() {
    let table = scopes("void f(int value) { value = 1; }\n");

    let function = table.scope(table.root().unwrap()).unwrap().children[0];
    let bindings = bindings_of(&table, function, "value");

    assert_eq!(bindings.len(), 1, "{:?}", table.scope(function).unwrap());
}

/// A parameter of a declaration with no body is still bound, because the parameter exists either way.
#[test]
fn a_declaration_without_a_body_still_binds_its_parameters() {
    let table = scopes("void f(int a, double b);\n");

    assert_eq!(shape(&table), "File{f}(Fn{a,b})");
}

/// A statement block inside a body opens a nested scope.
///
/// What makes two sibling branches free to declare the same name, and what makes a name declared in a block
/// invisible after it.
#[test]
fn a_block_opens_a_nested_scope() {
    let table = scopes("void f() { { int inner; } }\n");

    assert_eq!(shape(&table), "File{f}(Fn(Block{inner}))");
}

/// A range-based `for` binds its variable in the loop's scope, not in the enclosing block.
///
/// The construct is one of the most common loops in modern C++, and it used to not parse at all — so this test
/// could not be written until the grammar was fixed. What it pins is the same property the C-style loop has:
/// the variable belongs to the loop, and the body is a scope nested inside it.
#[test]
fn a_range_for_opens_a_scope_for_its_variable() {
    let table = scopes("void f() { for (auto x : items) { } }\n");

    assert_eq!(
        shape(&table),
        "File{f}(Fn(Block{x}(Block)))",
        "`x` is in the loop's scope; the body is nested inside it"
    );

    let function = table.scope(table.root().unwrap()).unwrap().children[0];
    let loop_scope = table.scope(function).unwrap().children[0];

    assert_eq!(
        kind_of(&table, loop_scope, "x"),
        Some(BindingKind::Variable),
        "the loop variable is declared where the loop can see it"
    );
    assert_eq!(
        kind_of(&table, table.root().unwrap(), "x"),
        None,
        "and not at file scope"
    );
}

/// A range-based `for` over a structured binding declares every name in the pattern.
///
/// The combination that needed two fixes at once: the range reading itself, and the pattern's names, which are
/// reached through a different path from an ordinary declarator.
#[test]
fn a_range_for_over_a_structured_binding_binds_each_name() {
    let table = scopes("void f() { for (auto [key, value] : map) { } }\n");

    assert_eq!(shape(&table), "File{f}(Fn(Block{key,value}(Block)))");
}

/// A range-based `for`'s range expression is not a declaration.
#[test]
fn a_range_for_over_a_call_binds_only_its_variable() {
    let table = scopes("void f() { for (int x : make_items()) { } }\n");

    assert_eq!(shape(&table), "File{f}(Fn(Block{x}(Block)))");
}

/// A bare expression statement declares nothing, for the statements that parse without a declarator.
///
/// `x;`, `a = b;`, `++i;` and `a + b;` all arrive as a `Declaration` with no declarator at all — the grammar's
/// way of holding a statement whose kind it did not work out — and the scope walker correctly finds no name in
/// them. Asserted as a **negative** so the property is pinned in the layer that can enforce it: whatever the
/// parser's node kind says, a name is only read along the declarator path.
#[test]
fn a_bare_expression_statement_declares_nothing() {
    for body in ["x;", "a = b;", "++i;", "a + b;"] {
        let source = format!("void f() {{ {body} }}\n");
        let table = scopes(&source);
        let function = table.scope(table.root().unwrap()).unwrap().children[0];

        let declared: Vec<String> = table
            .scope(function)
            .unwrap()
            .declared_names()
            .iter()
            .map(|name| name.text())
            .collect();

        assert!(
            declared.is_empty(),
            "{body:?} is a statement, but the walker declared {declared:?}"
        );
    }
}

/// A **call statement** is read as a declaration, and the scope walker therefore declares its argument.
///
/// Recorded as the current behaviour rather than as correct, because the cause is a parser-level ambiguity this
/// layer cannot settle: `use(x);` and `int(x);` have the same token shape, and the grammar reads both as a
/// parenthesized declarator whose name is `x`. The parser is honest about the result — `CppDeclaration` reports
/// a "variable" with **no name**, and `CppDeclarator::get_name_text` reports `None` for the outer declarator —
/// but the inner declarator does hold the identifier, so a scope that walks the declarator finds it.
///
/// The fix belongs in the parser, where the reading is chosen: a bare `name(...)` at statement position is a
/// call, and treating it as a declaration is what puts a name in scope that the statement only uses. Pinned
/// here so that fixing it is a deliberate edit and so the cost is visible: every call statement in every
/// function currently contributes a spurious binding.
#[test]
fn a_call_statement_is_read_as_a_declaration_for_now() {
    let table = scopes("void f() { use(x); }\n");
    let function = table.scope(table.root().unwrap()).unwrap().children[0];

    let declared: Vec<String> = table
        .scope(function)
        .unwrap()
        .declared_names()
        .iter()
        .map(|name| name.text())
        .collect();

    assert_eq!(
        declared,
        vec!["x"],
        "the argument is declared — this is the bug, and fixing it should empty this list"
    );
}

/// A loop's init-declaration goes in the loop's scope, not in the enclosing block.
///
/// The case that decides whether `for (int i = 0; ...)` leaks `i` into the surrounding function — and the
/// reason a scope is opened for the loop rather than for its body.
///
/// Three levels, and each is doing something: the function's own block, the scope the loop opens for its init
/// declaration, and the body's block **inside** that — which is what lets the body see `i` while the rest of
/// the function cannot.
#[test]
fn a_loop_opens_a_scope_for_its_init_declaration() {
    let table = scopes("void f() { for (int i = 0; i < 3; ++i) { int body; } }\n");

    assert_eq!(
        shape(&table),
        "File{f}(Fn(Block{i}(Block{body})))",
        "`i` is in the loop's scope; `body` is in the body's, nested inside it"
    );
}

/// Two sibling branches each get their own scope, so the same name can be declared in both.
///
/// The function's body is one block, and each branch opens a scope inside it — so a name declared in a branch
/// is not visible after the branch, and the same name can appear in both without colliding.
#[test]
fn sibling_branches_have_separate_scopes() {
    let table = scopes("void f() { if (c) { int x; } else { int x; } }\n");

    assert_eq!(
        shape(&table),
        "File{f}(Fn(Block(Block{x} Block{x})))",
        "the function's block, then one scope per branch"
    );

    // And they are different bindings, which is the point: one declaration each, in separate scopes.
    let function = table.scope(table.root().unwrap()).unwrap().children[0];
    let body = table.scope(function).unwrap().children[0];
    let branches = &table.scope(body).unwrap().children;

    assert_eq!(branches.len(), 2, "one scope per branch");
    let first = bindings_of(&table, branches[0], "x");
    let second = bindings_of(&table, branches[1], "x");

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_ne!(
        first[0].1, second[0].1,
        "two declarations of the same name at different offsets"
    );
}

/// A `switch` opens one scope for all its cases.
///
/// One scope and not one per case, because that is what C++ does: a declaration in one `case` is in scope in
/// the next, which is why people write braces inside a `case`.
#[test]
fn a_switch_opens_one_scope_for_all_its_cases() {
    let table = scopes("void f() { switch (n) { case 1: { int a; } case 2: { int b; } } }\n");

    assert_eq!(shape(&table), "File{f}(Fn(Block(Block{a} Block{b})))");
}

/// A label is bound in the scope it appears in.
#[test]
fn a_label_is_bound() {
    let table = scopes("void f() { goto done; done: return; }\n");

    assert_eq!(shape(&table), "File{f}(Fn{done})");

    let function = table.scope(table.root().unwrap()).unwrap().children[0];
    assert_eq!(kind_of(&table, function, "done"), Some(BindingKind::Label));
}

// ============================================================================
// Types, aliases, and using
// ============================================================================

/// A `typedef` declares a name, read from its declarator rather than from the node.
#[test]
fn a_typedef_declares_its_name() {
    let table = scopes("typedef int Integer;\n");

    assert_eq!(shape(&table), "File{Integer}");
    assert_eq!(
        kind_of(&table, table.root().unwrap(), "Integer"),
        Some(BindingKind::Typedef)
    );
}

/// `using Alias = T;` is an alias, and `using ns::f;` is a using-declaration.
///
/// The same node kind for two constructs that mean different things, told apart by whether a type follows the
/// `=`. A consumer offering completions treats them differently — one is a type, the other is whatever `f` was.
#[test]
fn a_using_declaration_is_told_apart_from_an_alias() {
    let table = scopes("using Alias = int;\n");
    assert_eq!(
        kind_of(&table, table.root().unwrap(), "Alias"),
        Some(BindingKind::Alias)
    );

    let table = scopes("using ns::f;\n");
    assert_eq!(
        shape(&table),
        "File{f}",
        "the name is the last component, not the qualifier"
    );
    assert_eq!(
        kind_of(&table, table.root().unwrap(), "f"),
        Some(BindingKind::UsingDeclaration)
    );
}

/// `using namespace std;` is recorded, and binds a namespace rather than a variable.
///
/// It introduces no name of its own, so the alternative to recording it is leaving no trace at all — and then a
/// consumer asking "why is `vector` in scope here" has nothing to point at.
#[test]
fn a_using_directive_is_recorded() {
    let table = scopes("using namespace std;\n");

    assert_eq!(shape(&table), "File{std}");
    assert_eq!(
        kind_of(&table, table.root().unwrap(), "std"),
        Some(BindingKind::UsingDirective)
    );
}

// ============================================================================
// Templates
// ============================================================================

/// A template's parameters go in a scope of their own, and the declaration is inside it.
///
/// What makes `T` visible in the template's body and invisible outside. The class's *name*, though, is declared
/// where the template was written — a class declared inside its own parameter scope would be unreachable.
#[test]
fn a_template_parameter_scope_wraps_the_declaration() {
    let table = scopes("template <typename T, int N> class Array { T data[N]; };\n");

    assert_eq!(
        shape(&table),
        "File{Array}(Params{N,T}(Class{data}))",
        "`Array` is outside the parameter scope; `T` and `N` are in it"
    );
}

/// A template parameter is a `TemplateParameter`, not an alias.
///
/// The difference decides whether `T::value_type` is a name that can be looked up now or one that depends on an
/// argument: storing `T` as an alias would make it look like a known type.
#[test]
fn a_template_parameter_has_its_own_kind() {
    let table = scopes("template <typename T> T id(T x) { return x; }\n");

    let params = table.scope(table.root().unwrap()).unwrap().children[0];

    assert_eq!(
        kind_of(&table, params, "T"),
        Some(BindingKind::TemplateParameter)
    );
    assert_eq!(
        table.scope(params).unwrap().kind,
        ScopeKind::TemplateParameters
    );
}

/// A template parameter is not in the file scope, and the function declared by the template is.
#[test]
fn a_template_parameter_is_not_in_the_file_scope() {
    let table = scopes("template <typename T> T id(T x) { return x; }\n");

    let shape = shape(&table);
    assert!(shape.starts_with("File{id}(Params{T}"), "{shape}");
    assert_eq!(
        kind_of(&table, table.root().unwrap(), "T"),
        None,
        "T belongs to the template"
    );
}

// ============================================================================
// What is deliberately not bound
// ============================================================================

/// A qualified declaration binds nothing in the enclosing scope.
///
/// `int ns::Widget::count = 0;` declares a member of `ns::Widget`, not a name in the file scope. Binding it as
/// plain `count` would put a name in scope that cannot be used unqualified — and it would collide with every
/// other `count` in the file.
#[test]
fn a_qualified_declaration_binds_nothing_here() {
    let table = scopes("int ns::Widget::count = 0;\n");

    assert_eq!(
        shape(&table),
        "File",
        "the name belongs to `ns::Widget`, which this file does not declare"
    );
}

/// A `friend` declaration binds nothing: a friend is not a member.
///
/// `friend class X;` says X's members may reach into this class. Binding `X` as a member would make it appear
/// in member completion, where it is not a member at all.
#[test]
fn a_friend_declaration_binds_nothing() {
    let table = scopes("class Widget { friend class Helper; friend void free_fn(); };\n");

    let class = table.scope(table.root().unwrap()).unwrap().children[0];
    let names: Vec<String> = table
        .scope(class)
        .unwrap()
        .bindings
        .iter()
        .map(|binding| binding.name.text())
        .collect();

    assert!(
        names.is_empty(),
        "a friend declares nothing here, got {names:?}"
    );
}

/// A `static_assert` declares nothing, and does not stop the declarations around it.
#[test]
fn a_static_assert_declares_nothing() {
    let table = scopes("int before;\nstatic_assert(true);\nint after;\n");

    assert_eq!(shape(&table), "File{after,before}");
}

/// A `static_assert` whose argument contains a template-id does not parse yet.
///
/// The gap is in the parser and is narrower than it looks: `static_assert(a > 0)` and `static_assert(a < 0)`
/// both parse, and so does `static_assert(true)`, but `static_assert(sizeof(int) > 0)` does not — the `>` is
/// readable as closing a template argument list, and the expression parser gives up rather than treating it as
/// a comparison. Recorded here so that fixing it is deliberate.
#[test]
fn a_static_assert_with_a_template_id_does_not_parse_yet() {
    let source = "static_assert(sizeof(int) > 0);\n";
    let tree = CppParser::parse(source, ParserConfig::default());

    assert!(
        !tree.get_errors().is_empty(),
        "this now parses — move it into `a_static_assert_declares_nothing`"
    );
    assert_eq!(
        tree.to_source_text(),
        source,
        "and stays lossless regardless"
    );
}

/// A reference is not a binding — only what a declaration introduces is.
///
/// Asserted explicitly because it is a design decision rather than an oversight: collecting uses is a different
/// pass with a different output shape, and doing it here would make a scope's bindings ambiguous between
/// "declared here" and "used here".
#[test]
fn a_use_is_not_a_binding() {
    let table = scopes("void f() { undeclared_name; other(); }\n");

    assert_eq!(
        shape(&table),
        "File{f}(Fn)",
        "nothing is bound for a name that is only used"
    );
}

// ============================================================================
// Malformed input
// ============================================================================

/// A file that does not parse still produces a table, with whatever was readable in it.
///
/// This layer runs while a file is being typed, so a construct the parser recovered from must not cost the
/// declarations around it.
#[test]
fn a_recovered_parse_still_declares_what_it_can() {
    let tree = CppParser::parse(
        "int before;\nstruct { ; \nint after;\n",
        ParserConfig::default(),
    );
    let table = build_scopes(&tree.get_red_root());

    // The parse is recovered, not clean — and the point is that the table is still built.
    assert_eq!(
        tree.to_source_text(),
        "int before;\nstruct { ; \nint after;\n"
    );

    let names: Vec<String> = table
        .scope(table.root().unwrap())
        .unwrap()
        .bindings
        .iter()
        .map(|binding| binding.name.text())
        .collect();

    assert!(
        names.contains(&"before".to_string()),
        "the declarations before the damage are read: {names:?}"
    );
}

/// An anonymous namespace binds no name, and still opens a scope.
///
/// Both halves matter: binding an invented name would put a name in scope that cannot be used, and skipping the
/// scope would put the namespace's contents in the file scope, where they are not.
#[test]
fn an_anonymous_namespace_opens_a_scope_without_binding_a_name() {
    let table = scopes("namespace { int hidden; }\n");

    assert_eq!(shape(&table), "File(Ns{hidden})");
}

/// An unnamed parameter declares nothing, and does not disturb the ones that are named.
#[test]
fn an_unnamed_parameter_declares_nothing() {
    let table = scopes("void f(int, double named) {}\n");

    assert_eq!(shape(&table), "File{f}(Fn{named})");
}

/// A structured binding binds every name in the pattern.
#[test]
fn a_structured_binding_binds_each_name() {
    let table = scopes("void f() { auto [first, second] = pair; }\n");

    assert_eq!(shape(&table), "File{f}(Fn{first,second})");

    let function = table.scope(table.root().unwrap()).unwrap().children[0];
    assert_eq!(
        kind_of(&table, function, "first"),
        Some(BindingKind::Variable)
    );
    assert_eq!(
        kind_of(&table, function, "second"),
        Some(BindingKind::Variable)
    );
}

/// Whatever the input, building scopes terminates without panicking.
///
/// An editor builds this on every keystroke, including while the file is half-typed, so a construct the parser
/// recovered from must not take the process down.
#[test]
fn building_scopes_never_panics() {
    let sources = [
        "",
        ";",
        "{",
        "}",
        "namespace",
        "namespace {",
        "class",
        "class {",
        "template <",
        "template <typename> ",
        "void f(",
        "void f() {",
        "enum {",
        "using",
        "typedef",
        "struct A : B {",
        "int ns::",
        "auto [a, = x;",
        "[[nodiscard]]",
        "void f(int a = {1, 2}) {}",
        "class A { class B { class C { int x; }; }; };",
        "void f() { for (;;) { while (1) { if (x) { } } } }",
    ];

    for source in sources {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(
            tree.to_source_text(),
            source,
            "losslessness must hold even here: {source:?}"
        );

        let table = build_scopes(&tree.get_red_root());

        // Touch the queries, so the closure is part of what is tested rather than elided.
        if let Some(root) = table.root() {
            for offset in 0..source.len().min(64) {
                if let Some(scope) = table.scope_at(offset) {
                    let _ = table.scope_chain(scope);
                    let _ = table.scope(scope).map(|scope| scope.declared_names());
                }
            }
            let _ = table.scope_chain(root);
        }
    }
}

/// The scope covering an offset is the innermost declaration it sits in, on a real file.
///
/// The end-to-end version of the unit test in the symbol layer: a cursor inside a member function's body is in
/// that function's scope and not in the class's.
///
/// Note where the offsets point. A syntax node's range starts at its *first token*, so the class scope begins at
/// the `class` keyword — a cursor on the keyword itself is already inside the class, because that is where the
/// node starts. What is *not* inside is the enclosing file's view of the name, which is a different question
/// from where the node's text is.
#[test]
fn the_scope_at_an_offset_is_the_innermost_on_a_real_file() {
    let source = "class Widget {\n  void size() {\n    int local = 0;\n  }\n};\n";
    let table = scopes(source);

    let keyword_offset = source.find("class").unwrap();
    let body_offset = source.find('{').unwrap() + 1;
    let method_offset = source.find("void size").unwrap() + 5;
    let local_offset = source.find("int local").unwrap() + 5;

    let at = |offset: usize| {
        let scope = table.scope_at(offset).expect("a scope");
        table.scope(scope).unwrap().kind
    };

    assert_eq!(
        at(keyword_offset),
        ScopeKind::Class,
        "the class node starts at its keyword"
    );
    assert_eq!(at(body_offset), ScopeKind::Class, "just inside the body");
    assert_eq!(at(method_offset), ScopeKind::Function, "inside the method");
    assert_eq!(
        at(local_offset),
        ScopeKind::Function,
        "inside the method body"
    );
    assert_eq!(at(method_offset), ScopeKind::Function, "inside the method");
    assert_eq!(
        at(local_offset),
        ScopeKind::Function,
        "inside the method body"
    );
}

/// The bindings a scope reports are only the ones written in it, not the ones in its children.
///
/// A consumer resolving a name walks outward itself, so a scope that included its children's names would make
/// every lookup find things that shadow nothing and are not visible.
#[test]
fn a_scope_reports_only_its_own_bindings() {
    let table = scopes("namespace ns { int inner; }\nint outer;\n");
    let file = table.root().unwrap();

    let names: Vec<String> = table
        .scope(file)
        .unwrap()
        .declared_names()
        .iter()
        .map(|name| name.text())
        .collect();

    assert_eq!(names, vec!["ns", "outer"], "not `inner`");
}

/// The name range is the identifier's, not the whole declarator's.
///
/// The difference between a rename that edits one identifier and one that eats the initialiser, so it is
/// asserted on the source text rather than on the numbers. The declaration range deliberately covers the
/// **declarator** (`counter = 42`) rather than the whole `Declaration`: the type specifiers are shared by every
/// declarator in `int a, b;`, so including them would attribute the same text to both bindings.
#[test]
fn a_bindings_name_range_covers_just_the_name() {
    let source = "int counter = 42;\n";
    let table = scopes(source);
    let file = table.root().unwrap();

    let binding = &table.scope(file).unwrap().bindings[0];

    let slice = |range: cpp_parser::SourceRange| &source[range.start_offset..range.end_offset()];

    assert_eq!(slice(binding.name_range), "counter");
    assert_eq!(slice(binding.range), "counter = 42");
    assert!(
        binding.name_range.length < binding.range.length,
        "the name is a part of the declarator, not the whole of it"
    );
}
