//! Building scopes and bindings from a file's syntax tree.
//!
//! The tests are organised by the question a consumer asks, and the first thing most of them do is render the
//! scope tree as a single line — `KIND{names}(children)` — because the shape is what is easy to get subtly
//! wrong and hard to notice. A scope one level too deep, or a parameter bound in the enclosing block instead
//! of the function, produces a tree that still looks plausible and gives wrong completions everywhere.
//!
//! Scope is what these tests are about, so the renderings show scopes and the names in them, and nothing else.

use cpp_code_analysis::{BindingKind, ScopeKind, ScopeTree, build_scopes};
use cpp_parser::{CppParser, CppSyntaxKind, ParserConfig};

/// Parse, check the parse is sound, and build the scopes.
fn scopes(source: &str) -> ScopeTree {
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
fn shape(table: &ScopeTree) -> String {
    fn render(table: &ScopeTree, id: cpp_code_analysis::ScopeId, out: &mut String) {
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
    table: &ScopeTree,
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

/// A scope's qualified name, for the scope of kind `want` that introduces `name`.
///
/// `name` is `None` for the scopes that introduce nothing. `None` back means either no such scope or no
/// qualified name for it, which is why the callers that care about the difference pass a name that exists.
fn qualified_of(table: &ScopeTree, want: ScopeKind, name: Option<&str>) -> Option<String> {
    let id = table
        .scopes()
        .iter()
        .position(|scope| scope.kind == want && scope.name.as_deref() == name)?;

    table.qualified_name_of(cpp_code_analysis::ScopeId(id))
}

/// All bindings of a name in a scope, as `(kind, name_range)` pairs.
fn bindings_of(
    table: &ScopeTree,
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

/// A class whose body contains a destructor is still named by its own name.
///
/// The bug this pins: `~Widget` is spelled with a `~` token, and whether a name is a destructor was decided by
/// looking for that token with `descendants_with_tokens` over the whole **declaration**. For `struct Widget { ~Widget(); };`
/// the declaration is the `StructDef`, whose descendants include the destructor inside the body — so the *class*
/// became `~Widget`. The class's scope was named `~Widget`, every member written after the destructor landed in
/// it, and `Widget` itself was never bound at all.
///
/// Every class in real C++ has a destructor, so this was not an edge case: it silently renamed the class and
/// moved its members.
#[test]
fn a_class_with_a_destructor_in_its_body_keeps_its_own_name() {
    let table = scopes("struct Widget {\n  ~Widget();\n  int size;\n};\n");
    let file = table.root().unwrap();

    assert_eq!(
        shape(&table),
        "File{Widget}(Class{size})",
        "the class is `Widget` and `size` is its member"
    );
    assert_eq!(kind_of(&table, file, "Widget"), Some(BindingKind::Class));
    assert_eq!(
        qualified_of(&table, ScopeKind::Class, Some("Widget")).as_deref(),
        Some("Widget"),
        "and the scope it opens is named after it, not after the destructor"
    );
}

/// An `operator` declaration in a class body does not make the class an operator either.
///
/// The same shape of bug as the destructor above, one token over: the search for the `operator` keyword has to
/// look in the **name node**, and a class body holding `operator+` put that keyword in the declaration's
/// descendants. Pinned separately because the two searches are separate calls and fixing one does not fix the
/// other.
#[test]
fn a_class_with_an_operator_in_its_body_keeps_its_own_name() {
    let table = scopes("struct Widget {\n  Widget& operator+(const Widget&);\n  int size;\n};\n");

    assert_eq!(
        shape(&table),
        "File{Widget}(Class{size})",
        "the class is `Widget`; the class body does not make it an operator"
    );
}

/// A declaration whose declarator is **not** wrapped in an `InitDeclarator` declares nothing here yet.
///
/// The boundary, written down rather than left to be discovered — see "现在答不了什么" in
/// `docs/index-design.md`. It is a gap in this walker, not a decision: `~Widget();` is
/// `Declaration[Declarator[NameExpr(~ Widget), ParameterList]]`, with no `DeclSpecifierSeq` and no
/// `InitDeclarator`, and [`is_unnamed_declaration`] asks `CppDeclaration::get_name_text` — which reads the first
/// `init-declarator` — so the declaration looks unnamed and is dropped.
///
/// What it costs today: a destructor or a `= delete`d special member is not a binding, so it is missing from a
/// member list and a jump to it from a call site cannot land. What landing it needs: a rule for "a bare
/// declarator names the entity" that does **not** also accept the shapes `is_unnamed_declaration` exists to
/// reject — see `a_call_statement_declares_nothing` and `a_real_declaration_is_still_declared` just above, which
/// are the two sides that rule has to keep apart.
///
/// The test is written to fail the day it lands, on purpose: the fix is then a deliberate edit here.
#[test]
fn a_destructor_without_a_specifier_declares_nothing_yet() {
    let table = scopes("struct Widget {\n  ~Widget();\n  int size;\n};\n");

    assert_eq!(
        shape(&table),
        "File{Widget}(Class{size})",
        "`~Widget` is not bound; when it is, this becomes `Class{{size,~Widget}}` and the test has to be updated"
    );
}

/// A `virtual` destructor **is** bound, and its name is not an identifier.
///
/// The other side of the boundary above, and the case that keeps the empty-name handling in the member-list
/// query from being dead code: the specifier sequence is what lets this declaration through the unnamed test, and
/// the name it binds is `~Widget` — a [`NameKind::Destructor`], whose `identifier_text()` is `None`. So a fact
/// built from it stores an empty name, and any rule that compared member names would compare two empty strings.
///
/// [`NameKind::Destructor`]: cpp_code_analysis::NameKind::Destructor
#[test]
fn a_virtual_destructor_is_bound_under_its_tilde_name() {
    let table = scopes("struct Widget {\n  virtual ~Widget();\n  int size;\n};\n");

    assert_eq!(shape(&table), "File{Widget}(Class{size,~Widget})");

    let class = table.scope(table.root().unwrap()).unwrap().children[0];
    let bound = table
        .scope(class)
        .unwrap()
        .bindings
        .iter()
        .find(|binding| binding.name.identifier_text().is_none())
        .expect("the destructor is bound");

    assert_eq!(bound.name.text(), "~Widget");
    assert_eq!(bound.kind, BindingKind::Destructor);
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

/// A bare expression statement declares nothing — including the statements the grammar reads as declarations.
///
/// The most vexing parse leaves a statement-shaped hole in the declaration grammar: `use(x);` arrives as a
/// `Declaration` whose declarator names nothing, because `use` was taken for a type and `(x)` for a
/// parenthesised declarator. What the analysis *can* say is whether a name was declared, and none was.
///
/// Without this the cost is not subtle: **every call statement in every function** contributed a binding for
/// its first argument, so a scope reported names the code only uses. That is why the list is asserted to be
/// empty rather than the shapes being pinned.
///
/// `use(x);` is one of these statements — it parses, and is misread, so it belongs here rather than among the
/// calls that parse correctly. See `direct_initialisation_of_a_user_type_does_not_parse_yet` for the same
/// ambiguity from the other side.
#[test]
fn a_call_statement_declares_nothing() {
    for body in [
        "use(x);",
        "f();",
        "g(a, b);",
        "x;",
        "a = b;",
        "++i;",
        "a + b;",
        "delete p;",
        "return;",
    ] {
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

/// The rule that rejects a call's argument does not reject a declaration that has a name.
///
/// The other half of the same rule, and the half that would break silently: a gate that rejected too much
/// would leave every function body empty of declarations, which no single test above would notice.
#[test]
fn a_real_declaration_is_still_declared() {
    let table = scopes(
        "void f() {\n\
         int local;\n\
         int initialised = 1;\n\
         const char* name = \"x\";\n\
         auto deduced = 2;\n\
         Widget widget{1, 2};\n\
         int direct(1, 2);\n\
         std::vector<int> values;\n\
         Foo* pointer = new Foo();\n\
         }\n",
    );

    assert_eq!(
        shape(&table),
        "File{f}(Fn{deduced,direct,initialised,local,name,pointer,values,widget})",
        "every one of these declares a name"
    );
}

/// Direct-initialisation — `Type name(args);` — declares a variable.
///
/// One of the most common declarations in C++, and it used to fail outright. A `(` after a declared name is a
/// **parameter list** or the parentheses of a **direct-initialised variable**, and the two are told apart by
/// what they can hold: `int a(b)` has a parameter, because `b` parses as a type, while `int a(1)` does not,
/// because `1` is not one.
///
/// A keyword type is the case that needs no lookup at all. A **user type** needs the file's own declarations,
/// which is the other half of the same rule and has its own test below.
#[test]
fn direct_initialisation_declares_a_variable() {
    for source in [
        "void f() { int a(1); }\n",
        "void f() { double d(1.5); }\n",
        "void f() { char c('x'); }\n",
        "void f() { unsigned int u(1); }\n",
    ] {
        let tree = CppParser::parse(source, ParserConfig::default());

        assert_eq!(
            tree.get_errors(),
            [],
            "{source:?} must parse cleanly, got {:?}",
            tree.get_errors()
        );
        assert_eq!(
            tree.to_source_text(),
            source,
            "{source:?} must stay lossless"
        );

        assert!(
            tree.get_red_root()
                .descendants()
                .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Initializer),
            "{source:?} declares a variable with an initializer"
        );
    }
}

/// A **call** keeps its parentheses, because a bare name in front of `(` is not a declared type.
///
/// The other half of the rule above, and the half a naive fix breaks: reading every `(...)` after a name as an
/// initializer makes `g(1, 2);` into a declaration that declares nothing, so the statement loses its callee
/// *and* its arguments — and a name enters scope that was never declared. That is the direction to avoid, so a
/// name nothing in the file declares to be a type stays a callee.
///
/// `g();` is deliberately absent: an empty argument list is the most vexing parse's other half, and `T x()` is
/// a function declaration, so a call with no arguments is read as one — which is what C++ does with it too.
#[test]
fn a_call_keeps_its_argument_list() {
    for source in [
        "void f() { g(1); }\n",
        "void f() { g(1, 2); }\n",
        "void f() { obj.method(1); }\n",
        "void f() { p->method(x); }\n",
        // A file-scope call is not valid C++, but the reading must still not invent a variable out of it.
        "Max(a, 1);\n",
    ] {
        let tree = CppParser::parse(source, ParserConfig::default());

        assert_eq!(
            tree.get_errors(),
            [],
            "{source:?} must parse cleanly, got {:?}",
            tree.get_errors()
        );
        assert!(
            tree.get_red_root()
                .descendants()
                .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CallExpr),
            "{source:?} is a call, not a declaration"
        );
    }
}

/// Direct-initialisation of a **user-defined** type parses, because the file says the name is a type.
///
/// `Widget w(1, 2);` and `g(1, 2);` are the same tokens with different meanings, and which is right depends on
/// whether `Widget` names a type. C++ settles it by looking the name up, and that is exactly why `int x(2)` is
/// unambiguous (`int` is a keyword) while `Widget w(1, 2);` is not.
///
/// The parser has no symbol table, but it does have the file, so it records the names a class-like head, a
/// `typedef` or a `using` alias introduces and asks that table. A name it finds is read as a declaration; a name
/// it does not is left to the expression reading, because the two mistakes are not equally bad — reading a *call*
/// as a declaration loses its callee and its arguments and puts a name in scope that was never declared, while
/// reading a *declaration* as a call merely leaves the variable unbound.
///
/// This is deliberately a **file-local** judgement: a type from an included header is not in the table, so
/// `std::string s("x");` stays an expression. Recording what a file declares is not the same claim as resolving
/// names across files, and the query layer is where that second claim belongs.
#[test]
fn direct_initialisation_of_a_user_type_declares_a_variable() {
    for source in [
        "struct Widget {};\nvoid f() { Widget w(1, 2); }\n",
        "class Widget {};\nvoid f() { Widget w(1, 2); }\n",
        "union Widget {};\nvoid f() { Widget w(1, 2); }\n",
        "enum class Widget {};\nvoid f() { Widget w(1, 2); }\n",
        "using Widget = int;\nvoid f() { Widget w(1, 2); }\n",
        "typedef int Integer;\nvoid f() { Integer i(1); }\n",
        "struct Outer { struct Inner {}; void m() { Inner i(1); } };\n",
        "struct Widget {};\nWidget make();\n",
    ] {
        let tree = CppParser::parse(source, ParserConfig::default());

        assert_eq!(
            tree.get_errors(),
            [],
            "{source:?} must parse cleanly, got {:?}",
            tree.get_errors()
        );
        assert_eq!(
            tree.to_source_text(),
            source,
            "{source:?} must stay lossless"
        );
    }

    // The variables themselves, which is what the scope layer reads off the tree.
    let table = scopes(
        "struct Widget {};\nvoid f() {\n  Widget w(1, 2);\n  Widget v;\n}\nstruct Outer { Inner i(1); };\n",
    );

    assert_eq!(
        shape(&table),
        "File{Outer,Widget,f}(Fn{v,w} Class{i})",
        "both direct-initialised variables are declared"
    );
}

/// A user type written **before** its declaration is a type for the *parser*, which reads the statement by its
/// shape rather than by the table.
///
/// The table is still filled in as the file is read, and still cannot answer for a name declared further down —
/// but the parser no longer needs it to: `Widget w(1, 2);` is a declaration because it has a name and an
/// argument list, and no call has that shape. The declaration reading no longer depends on the order in which
/// the file is written, which is what a file being edited needs.
#[test]
fn a_user_type_declared_later_is_still_a_declaration() {
    let source = "void f() { Widget w(1, 2); }\nstruct Widget {};\n";
    let tree = CppParser::parse(source, ParserConfig::default());

    assert!(
        tree.get_red_root()
            .descendants()
            .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Initializer),
        "the declarator's name is the evidence, and it is there whether or not the type is"
    );
    assert_eq!(tree.to_source_text(), source, "and the file stays lossless");
}

/// A user type the file never declares at all is still a declaration.
///
/// This used to be the documented cost of a file-local table: nothing says `Widget` is a type, so
/// `Widget w(1, 2);` kept the reading that lost the least. It is a declaration now, because the *shape* says so —
/// a name followed by an argument list is not a call — and the trade moved to the other side: `A(B);` is read as
/// a call, and `MyType` used in a cast has to be declared somewhere in the file.
#[test]
fn an_undeclared_type_name_is_still_a_declaration() {
    for source in [
        "void f() { Widget w(1, 2); }\n",
        "void f() { std::string s(\"x\"); }\n",
        "void f() { Foo bar(1); }\n",
    ] {
        let tree = CppParser::parse(source, ParserConfig::default());

        assert!(
            tree.get_red_root()
                .descendants()
                .any(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Initializer),
            "{source:?} names a declarator and initialises it"
        );
        assert_eq!(tree.to_source_text(), source, "{source:?} stays lossless");
    }
}

/// `sizeof` of a **builtin** type parses, as it always should have.
///
/// `sizeof(std::vector<int>)` and `sizeof(Foo)` worked — the first as a type, the second as an expression, since
/// a bare name is one. Only `sizeof(int)` failed, because `int` cannot start an expression and the
/// parenthesised-type reading was missing. It is there now, and this test pins that the two readings of `sizeof`
/// both work.
#[test]
fn sizeof_a_builtin_type_parses() {
    for source in [
        "void f() { sizeof(int); }\n",
        "void f() { sizeof(unsigned char); }\n",
    ] {
        let tree = CppParser::parse(source, ParserConfig::default());

        assert!(
            tree.get_errors().is_empty(),
            "{source:?} should parse, got {:?}",
            tree.get_errors()
        );
        assert_eq!(tree.to_source_text(), source, "and stays lossless");
    }

    // The neighbouring spellings that do parse, asserted so the gap cannot be described more broadly than it is.
    for source in [
        "void f() { sizeof(std::vector<int>); }\n",
        "void f() { sizeof(Foo); }\n",
        "void f() { sizeof x; }\n",
    ] {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(tree.get_errors(), [], "{source:?} parses");
    }
}

/// Assignment and compound assignment parse, in every spelling, and the statement forms built on them.
///
/// These used to fail with `expected `;` after expression`, and the cause was not in the statement rules at all
/// but in the **binary operator table**: no assignment operator was in it, so `a = b` parsed as the bare
/// expression `a` and stopped at the `=`. The expression reading then reported a missing `;`, and the
/// declaration/expression fallback could not help because the *expression* reading was the one that failed.
///
/// Worth a test of its own rather than a line in another: assignments are in nearly every line of real C++, so
/// their absence made the parser report a syntax error on ordinary code — the false positives that get an
/// editor-facing tool switched off.
#[test]
fn assignment_operators_parse() {
    for body in [
        "a = b;",
        "i += 1;",
        "i -= 1;",
        "i *= 2;",
        "i /= 2;",
        "i %= 2;",
        "flags &= mask;",
        "flags |= mask;",
        "flags ^= mask;",
        "value <<= 2;",
        "value >>= 2;",
        "a = b = c;",
        "arr[0] = 1;",
        "arr[0] += 1;",
        "p->field = 1;",
        "p->next = q;",
        "obj.field = obj.other;",
        "*p = 5;",
        "x = y + z * w;",
    ] {
        let source = format!("void f() {{ {body} }}\n");
        let tree = CppParser::parse(&source, ParserConfig::default());

        assert_eq!(
            tree.get_errors(),
            [],
            "{body:?} must parse cleanly, got {:?}",
            tree.get_errors()
        );
        assert_eq!(tree.to_source_text(), source, "{body:?} must stay lossless");
    }
}

/// Assignment is right-associative and everything else is left-associative.
///
/// The shape rather than the outcome: `*p = *q = 5` is `*p = (*q = 5)`, and reading it left-associatively would
/// give `(*p = *q) = 5`, which is not a thing anyone writes and is not what the source says. Pinned because the
/// tree round-trips either way — a wrong associativity is invisible to every losslessness check.
///
/// The operands are dereferences on purpose. `a = b = c;` cannot be used: at statement position it is read as a
/// **declaration** of `a`, because `a` may be a type name and `= b = c` then looks like an initializer. `*p`
/// cannot be a type, so this statement is unambiguously an expression and the shape under test is the
/// expression's own.
#[test]
fn assignment_is_right_associative() {
    let source = "void f() { *p = *q = 5; }\n";
    let tree = CppParser::parse(source, ParserConfig::default());

    assert_eq!(tree.get_errors(), [], "must parse cleanly");

    let outer = tree
        .get_red_root()
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::BinaryExpr)
        .expect("a binary expression");

    assert_eq!(
        outer.text().to_string(),
        "*p = *q = 5",
        "the outer assignment covers the whole chain"
    );

    let inner = outer
        .children()
        .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::BinaryExpr)
        .expect("the right-hand side is itself an assignment");

    assert_eq!(
        inner.text().to_string(),
        "*q = 5",
        "which is what right-associative means here"
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

/// **Every** name a `typedef` lists, not the first.
///
/// `typedef WCHAR *PWCHAR, *LPWCH, *PWCH;` introduces three type names in one declaration, and that is how every C
/// header writes its pointer aliases — `winnt.h` hundreds of times. Binding only the first left the others
/// undeclared: a consumer asking for `LPWCH` found nothing, and every fact about it was missing from the index.
#[test]
fn a_typedef_declares_every_name_it_lists() {
    let table = scopes("typedef WCHAR *PWCHAR, *LPWCH, *PWCH;\n");

    assert_eq!(shape(&table), "File{LPWCH,PWCH,PWCHAR}");
    for name in ["PWCHAR", "LPWCH", "PWCH"] {
        assert_eq!(
            kind_of(&table, table.root().unwrap(), name),
            Some(BindingKind::Typedef),
            "`{name}` is one of the declaration's names"
        );
    }
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

/// A `static_assert` whose argument is a comparison *does* parse, template-id or not.
///
/// This was a documented gap in the parser: `static_assert(a > 0)` and `static_assert(true)` parsed, but
/// `static_assert(sizeof(int) > 0)` did not — the `>` was readable as closing a template argument list, and the
/// expression parser gave up rather than treating it as a comparison. The `sizeof` of a builtin type is read
/// now, and with it the comparison.
#[test]
fn a_static_assert_with_a_comparison_parses() {
    let source = "static_assert(sizeof(int) > 0);\n";
    let tree = CppParser::parse(source, ParserConfig::default());

    assert!(
        tree.get_errors().is_empty(),
        "the comparison should parse, got {:?}",
        tree.get_errors()
    );
    assert_eq!(
        tree.to_source_text(),
        source,
        "and stays lossless regardless"
    );

    // And it declares nothing, like every other `static_assert`.
    assert_eq!(shape(&scopes(source)), "File");
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

// ---------------------------------------------------------------------------------------------
// Qualified names
//
// A scope records the name it introduces, which is the only way `ns::C::f` can be produced: the *binding* of
// `f` says `f` is in the class scope, and the class scope is the only thing that knows it is called `C`.
// ---------------------------------------------------------------------------------------------

/// The name a scope introduces is what makes a qualified name reachable, segment by segment.
#[test]
fn a_scope_records_the_name_it_introduces() {
    let table = scopes("namespace ns { struct C { int member; }; }\n");

    assert_eq!(
        qualified_of(&table, ScopeKind::Namespace, Some("ns")).as_deref(),
        Some("ns")
    );
    assert_eq!(
        qualified_of(&table, ScopeKind::Class, Some("C")).as_deref(),
        Some("ns::C"),
        "the class's qualified name includes the namespace it was written in"
    );
}

/// A nested `namespace a::b` and the spelled-out form are the same thing, and must produce the same name.
///
/// The two are one construct in C++11 and later, and a consumer that stored only the *written* spelling would
/// answer differently for them — which is the kind of difference that only shows up in a rename.
#[test]
fn a_nested_namespace_spelling_is_the_same_name() {
    let compact = scopes("namespace a::b { struct C { int member; }; }\n");
    let spelled = scopes("namespace a { namespace b { struct C { int member; }; } }\n");

    for table in [&compact, &spelled] {
        assert_eq!(
            qualified_of(table, ScopeKind::Class, Some("C")).as_deref(),
            Some("a::b::C"),
            "both spellings name the same class"
        );
    }
}

/// A scope that introduces no name contributes nothing, and stops nothing.
///
/// This is what makes `void ns::f() {` and `void f() {` agree: the function body is transparent either way,
/// and only the namespace the function was written in is part of the answer. Getting this wrong would give
/// `ns::f`'s locals the qualified name `ns::f::local`, which is not a name anything can be referred to by.
#[test]
fn an_unnamed_scope_is_transparent() {
    let table = scopes("namespace ns { void f() { int local = 0; } }\n");

    let function = table
        .scopes()
        .iter()
        .position(|scope| scope.kind == ScopeKind::Function)
        .expect("the function body has a scope");
    let function = cpp_code_analysis::ScopeId(function);

    assert_eq!(
        table.qualified_name_of(function).as_deref(),
        Some("ns"),
        "the function body contributes no segment but does not cut the namespace off"
    );
    assert_eq!(
        table.scope(function).expect("the scope").name,
        None,
        "and it really is unnamed rather than named `f`"
    );
}

/// An unnamed class is a dead end: a member of it is reachable by no qualified name at all.
#[test]
fn an_unnamed_class_cuts_the_qualified_name_off() {
    let table = scopes("namespace ns { struct { int member; } value; }\n");

    let class = table
        .scopes()
        .iter()
        .position(|scope| scope.kind == ScopeKind::Class)
        .expect("the class body has a scope");

    assert_eq!(
        table.qualified_name_of(cpp_code_analysis::ScopeId(class)),
        None,
        "`ns::member` is not a name, and neither is `member`"
    );
}

/// A member's qualified name comes from its class scope, not from the binding it sits beside.
#[test]
fn a_member_is_found_under_its_class() {
    let table = scopes("namespace ns { struct C { void method(); int field; }; }\n");

    let class = table
        .scopes()
        .iter()
        .position(|scope| scope.kind == ScopeKind::Class)
        .expect("the class body has a scope");
    let class = cpp_code_analysis::ScopeId(class);

    let members: Vec<String> = table
        .scope(class)
        .expect("the scope")
        .bindings
        .iter()
        .map(|binding| {
            format!(
                "{}::{}",
                table.qualified_name_of(class).expect("the class is named"),
                binding.name.text()
            )
        })
        .collect();

    assert_eq!(members, ["ns::C::field", "ns::C::method"]);
}

/// A scope with no name of its own has no qualified name, rather than an empty one.
///
/// The distinction matters to a consumer: `Some("")` would be a name that matches nothing while looking like
/// an answer, where `None` says "there is no qualified name here".
#[test]
fn the_file_scope_has_no_qualified_name() {
    let table = scopes("int x;\n");

    let root = table.root().expect("a non-empty file has a file scope");
    assert_eq!(table.scope(root).expect("the scope").name, None);
    assert_eq!(table.qualified_name_of(root), None);
}
