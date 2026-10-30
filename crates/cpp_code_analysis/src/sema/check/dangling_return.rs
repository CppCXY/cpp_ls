//! **`dangling-return`** — a reference, a pointer or a view of something that dies when the function returns.
//!
//! ```cpp
//! int& f() { int x = 0; return x; }                        // `x` is the function's own local
//! std::string_view g() { std::string s; return s; }        // the view outlives the string
//! const std::string& h() { return std::string("x"); }      // the temporary dies at the semicolon
//! const char* k() { std::string s; return s.c_str(); }     // the pointer points into the local
//! int* m() { int x = 0; return &x; }                       // the address of a local
//! ```
//!
//! Every one of them is undefined behaviour by the standard, and every one is a defect a compiler's own
//! `-Wreturn-local-addr` family reports — which is why this rule only reports **what it can prove**:
//!
//! ```text
//!   reported   a local of this function, a temporary, or a pointer the local handed out
//!   silent     a parameter, `*this`'s member, a static or global, a target points-to would be needed for
//! ```
//!
//! # Why a parameter is silent, and why that is not a hole
//!
//! `std::string_view f(const std::string& s) { return s; }` dangles only if the caller passes something that dies —
//! and that is a fact about the *caller*, not about this function. The function's own contract is "the result lives
//! as long as the argument", which is Clang's `[[clang::lifetimebound]]`. Reporting the declaration would report
//! every wrapper function that is written correctly.
//!
//! # Why a pointer is only reported when its target is certain
//!
//! `int* f() { return p; }` is not a defect — `p` may point at a global, at the heap, or at a buffer that outlives
//! the call. The two pointer shapes that **are** certain are the address of a local (`&x`) and a pointer a local
//! handed out (`s.c_str()`, `s.data()`): in both, the referent is the local itself. A value whose target would need
//! points-to analysis is not reported.
//!
//! # What it does not see, and says so
//!
//! A **view** is recognised by its name (`string_view`, `basic_string_view`, `span`, `initializer_list`, `StringRef`)
//! rather than by its definition: a project with its own type of that name and owning semantics would be reported
//! wrongly, and these are the names the standard library, libc++ and Abseil use. A call to a function nobody can see
//! is an unknown origin — a *missing* finding, never a wrong one. A `return` inside a lambda belongs to the lambda,
//! and this rule says nothing about it.

use cpp_parser::CppAstNode;
use cpp_parser::CppSyntaxKind;

use super::{Check, CheckContext, DiagnosticCode};

pub struct DanglingReturn;

impl Check for DanglingReturn {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::DanglingReturn];

    fn check(context: &mut CheckContext<'_>) {
        // **Every `return` of this file's own text**, and the function it belongs to. The walk outward is what turns
        // a `return` into a question about a *result type*, and it stops at a lambda, whose type is the lambda's.
        let returns: Vec<cpp_parser::CppSyntaxNode> = context
            .own::<cpp_parser::CppReturnStat>()
            .map(|statement| statement.syntax().clone())
            .collect();

        for statement in returns {
            let Some(function) = enclosing_function(&statement) else {
                continue;
            };
            let Some(body) = function
                .children()
                .find(|child| kind(child) == CppSyntaxKind::CompoundStat)
            else {
                continue;
            };
            let Some(declarator) = function
                .children()
                .find(|child| kind(child) == CppSyntaxKind::InitDeclarator)
                .and_then(|init| {
                    init.children()
                        .find(|child| kind(child) == CppSyntaxKind::Declarator)
                })
            else {
                continue;
            };
            let Some(name) = the_function_name(&declarator) else {
                continue;
            };

            // **The result type is what the declaration writes before the name**, read as text: `int&`,
            // `const std::string&`, `std::string_view`, `const char*`. The `&`/`*` are asked about **before the first
            // `<`**, so `std::vector<int*>` is a returned vector rather than a returned pointer.
            let Some(prefix) = context.source.get(
                usize::from(function.text_range().start())..usize::from(name.text_range().start()),
            ) else {
                continue;
            };
            let head = prefix.split('<').next().unwrap_or_default();
            let is_reference = head.contains('&');
            let is_pointer = head.contains('*');
            let view = view_name_of(head);
            if !is_reference && !is_pointer && view.is_none() {
                continue;
            }

            let Some(expression) = statement.children().next() else {
                continue;
            };
            let Some(borrowed) = origin_of(context, &expression, &body) else {
                continue;
            };

            let message = if is_reference {
                match borrowed.kind {
                    Origin::Local => Some(format!(
                        "`{name}` is a local, and this function returns a reference to it — it is destroyed when \
                         the function returns",
                        name = borrowed.description
                    )),
                    Origin::Temporary => Some(
                        "this returns a reference to a temporary, which is destroyed at the end of the statement \
                         the return is in"
                            .to_string(),
                    ),
                    _ => None,
                }
            } else if let Some(view) = view.as_deref() {
                match borrowed.kind {
                    Origin::Local => Some(format!(
                        "the returned `{view}` refers to `{name}`, a local that is destroyed when the function \
                         returns",
                        name = borrowed.description
                    )),
                    Origin::Temporary => Some(format!(
                        "the returned `{view}` refers to a temporary, which is destroyed at the end of the \
                         statement the return is in"
                    )),
                    _ => None,
                }
            } else {
                // A pointer: certain only when the referent is this function's own local and the value is either the
                // local's address or a pointer the local handed out.
                match borrowed.kind {
                    Origin::Local if borrowed.through.is_some() => Some(format!(
                        "`{through}()` returns a pointer into `{name}`, which is destroyed when the function returns",
                        through = borrowed.through.unwrap_or_default(),
                        name = borrowed.description
                    )),
                    Origin::Local if borrowed.address_of => Some(format!(
                        "this returns the address of `{name}`, a local that is destroyed when the function returns",
                        name = borrowed.description
                    )),
                    _ => None,
                }
            };

            let Some(message) = message else {
                continue;
            };
            let range = cpp_parser::source_range(expression.text_range());
            context.add(DiagnosticCode::DanglingReturn, range, "", message);
        }
    }
}

/// **Where a returned value comes from**, for the answers that change what the rule says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// A local of this function — the one origin that is always a defect when it is handed out.
    Local,
    /// A local with static storage duration, which outlives every call.
    StaticLocal,
    /// A parameter: the value lives as long as the argument, and judging that is the caller's rule.
    Parameter,
    /// `*this`'s member: the object outlives the call by the caller's own obligation.
    Member,
    /// A namespace-scope name.
    Global,
    /// A prvalue — a temporary, destroyed at the end of the full-expression.
    Temporary,
    /// A string literal, whose array has static storage duration.
    StringLiteral,
    /// Nothing this analysis can name, which is the answer that stays silent.
    Unknown,
}

struct Borrowed {
    kind: Origin,
    /// What a reader sees: the local's spelling, or the expression for a subobject.
    description: String,
    /// The member function the borrow came through, for `c_str`/`data`.
    through: Option<&'static str>,
    /// Is this the **address** of the referent (`&x`)?
    address_of: bool,
}

/// The function declaration a `return` belongs to — `None` for a return inside a lambda, whose result type is the
/// lambda's own.
fn enclosing_function(statement: &cpp_parser::CppSyntaxNode) -> Option<cpp_parser::CppSyntaxNode> {
    let mut current = statement.parent();
    while let Some(ancestor) = current {
        match kind(&ancestor) {
            CppSyntaxKind::LambdaExpr => return None,
            CppSyntaxKind::Declaration
                if ancestor
                    .children()
                    .any(|child| kind(&child) == CppSyntaxKind::CompoundStat) =>
            {
                return Some(ancestor);
            }
            _ => {}
        }
        current = ancestor.parent();
    }
    None
}

/// The declarator's own name — the `NameExpr` written directly inside it, not the one inside a parameter.
fn the_function_name(declarator: &cpp_parser::CppSyntaxNode) -> Option<cpp_parser::CppSyntaxNode> {
    declarator
        .children()
        .find(|child| kind(child) == CppSyntaxKind::NameExpr)
}

fn kind(node: &cpp_parser::CppSyntaxNode) -> CppSyntaxKind {
    CppSyntaxKind::from(node.kind())
}

/// **A view type, by the name every library spells it with.**
fn view_name_of(return_type: &str) -> Option<String> {
    let mut head = return_type.trim().trim_end_matches('&').trim();
    head = head
        .trim_start_matches("const ")
        .trim_start_matches("volatile ")
        .trim();
    let base = head.rsplit("::").next()?.trim();
    let base = base.trim_end_matches('*').trim();
    match base {
        "string_view" | "basic_string_view" | "span" | "initializer_list" | "StringRef" => {
            Some(base.to_string())
        }
        _ => None,
    }
}

/// Is this spelling an owning standard type? A constructor call of one of these produces a **temporary**, which is
/// what makes `return std::string("x");` a dangling reference rather than an unknown call.
fn is_an_owner(spelling: &str) -> bool {
    matches!(
        spelling,
        "string"
            | "wstring"
            | "basic_string"
            | "vector"
            | "deque"
            | "list"
            | "array"
            | "map"
            | "set"
            | "unordered_map"
            | "unordered_set"
            | "optional"
            | "variant"
            | "tuple"
            | "pair"
            | "unique_ptr"
            | "shared_ptr"
            | "function"
    )
}

/// **The origin of `expression`**, as far as this rule may claim one.
fn origin_of(
    context: &CheckContext<'_>,
    expression: &cpp_parser::CppSyntaxNode,
    function_body: &cpp_parser::CppSyntaxNode,
) -> Option<Borrowed> {
    match kind(expression) {
        CppSyntaxKind::ParenExpr => {
            let inner = expression.children().next()?;
            origin_of(context, &inner, function_body)
        }
        CppSyntaxKind::IdentifierExpr | CppSyntaxKind::NameExpr => {
            let description = expression.text().to_string();
            let offset = the_last_identifier_offset(expression)?;
            let crate::Known::Yes(binding) =
                crate::sema::resolve::definition_at(context.scopes(), context.tree(), offset)
            else {
                return Some(unknown(description));
            };
            let Some(scope) = context.scopes().scope(binding.scope) else {
                return Some(unknown(description));
            };
            let inside_the_body = binding.name_range.start_offset
                >= usize::from(function_body.text_range().start())
                && binding.name_range.end_offset() <= usize::from(function_body.text_range().end());
            // **The order is the answer.** A member is a member wherever it is written; a name declared outside this
            // body is not this function's to hand out; a name written before the body begins is a parameter; and only
            // what is left can be a local — static or automatic.
            let origin = match scope.kind {
                crate::ScopeKind::Class => Origin::Member,
                _ if !inside_the_body => Origin::Global,
                _ if binding.name_range.start_offset
                    < usize::from(function_body.text_range().start()) =>
                {
                    Origin::Parameter
                }
                _ if declared_static(context, &binding) => Origin::StaticLocal,
                _ => Origin::Local,
            };
            Some(Borrowed {
                kind: origin,
                description,
                through: None,
                address_of: false,
            })
        }
        CppSyntaxKind::MemberExpr | CppSyntaxKind::IndexExpr => {
            // `x.member` is a subobject of what `x` is: the local dies with its member. `v[i]` is an element of `v`,
            // which is the same relationship. Both arrive as one node with the base as the first child, and the
            // `.`/`->` token is what tells a member from an index.
            let base = expression.children().next()?;
            let mut borrowed = origin_of(context, &base, function_body)?;
            borrowed.description = expression.text().to_string();
            Some(borrowed)
        }
        CppSyntaxKind::ThisExpr => Some(Borrowed {
            kind: Origin::Member,
            description: "this".to_string(),
            through: None,
            address_of: false,
        }),
        CppSyntaxKind::UnaryExpr => {
            let operator = expression
                .children_with_tokens()
                .filter_map(|element| element.into_token())
                .next()
                .map(|token| cpp_parser::CppTokenKind::from(token.kind()));
            if operator == Some(cpp_parser::CppTokenKind::Ampersand) {
                // `&x` — the address of the operand, which carries its origin.
                let mut borrowed =
                    origin_of(context, &expression.children().next()?, function_body)?;
                borrowed.address_of = true;
                borrowed.description = expression.text().to_string();
                return Some(borrowed);
            }
            if operator == Some(cpp_parser::CppTokenKind::Star) {
                // `*p` — a target that only points-to analysis could name.
                return Some(unknown(expression.text().to_string()));
            }
            Some(Borrowed {
                kind: Origin::Temporary,
                description: expression.text().to_string(),
                through: None,
                address_of: false,
            })
        }
        CppSyntaxKind::CallExpr => {
            let callee = expression.children().next()?;
            let arguments: Vec<cpp_parser::CppSyntaxNode> = expression.children().skip(1).collect();
            let callee_name = the_last_identifier_of(&callee);
            let member_access = is_a_member_access(&callee);
            // **`x.c_str()` and `x.data()` hand out a pointer into `x`.** Any other member call returns a value that
            // borrows its object the same way: `v.front()` is a reference into `v`, which is reported through `v`'s
            // origin.
            if member_access {
                let receiver = callee.children().next()?;
                let mut borrowed = origin_of(context, &receiver, function_body)?;
                borrowed.through = match callee_name.as_deref() {
                    Some("c_str") => Some("c_str"),
                    Some("data") => Some("data"),
                    _ => None,
                };
                return Some(borrowed);
            }
            if let Some(name) = callee_name.as_deref() {
                // `std::move(x)` and `std::forward<T>(x)` are their argument, seen as an rvalue.
                if matches!(name, "move" | "forward") {
                    return origin_of(context, arguments.first()?, function_body);
                }
                // A view constructed from an argument borrows that argument: `std::string_view(s)`.
                if view_name_of(name).is_some() {
                    return match arguments.first() {
                        Some(argument) => origin_of(context, argument, function_body),
                        None => Some(temporary(expression)),
                    };
                }
                // An owning type constructed here is a temporary, and it dies at the end of the statement.
                if is_an_owner(name) {
                    return Some(temporary(expression));
                }
            }
            // **A call this analysis cannot see is not evidence.** `return get();` may return a reference to a global.
            Some(unknown(expression.text().to_string()))
        }
        CppSyntaxKind::InitListExpr => {
            let children: Vec<cpp_parser::CppSyntaxNode> = expression.children().collect();
            match children.len() {
                // `{ x }` is `x` initialised, which carries `x`'s origin.
                1 => origin_of(context, &children[0], function_body),
                _ => Some(temporary(expression)),
            }
        }
        CppSyntaxKind::LiteralExpr => {
            // A string literal is an array with static storage duration; every other literal is a prvalue.
            let is_string = expression
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .any(|token| {
                    cpp_parser::CppTokenKind::from(token.kind())
                        == cpp_parser::CppTokenKind::StringLiteral
                });
            Some(Borrowed {
                kind: if is_string {
                    Origin::StringLiteral
                } else {
                    Origin::Temporary
                },
                description: expression.text().to_string(),
                through: None,
                address_of: false,
            })
        }
        _ => Some(unknown(expression.text().to_string())),
    }
}

fn unknown(description: String) -> Borrowed {
    Borrowed {
        kind: Origin::Unknown,
        description,
        through: None,
        address_of: false,
    }
}

fn temporary(expression: &cpp_parser::CppSyntaxNode) -> Borrowed {
    Borrowed {
        kind: Origin::Temporary,
        description: expression.text().to_string(),
        through: None,
        address_of: false,
    }
}

/// Is this expression a member access (`a.b`, `a->b`) rather than an index (`a[i]`)? The parser gives both one
/// node, and the `.`/`->` token is what tells them apart.
fn is_a_member_access(expression: &cpp_parser::CppSyntaxNode) -> bool {
    expression
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| {
            matches!(
                cpp_parser::CppTokenKind::from(token.kind()),
                cpp_parser::CppTokenKind::Dot | cpp_parser::CppTokenKind::Arrow
            )
        })
}

/// Is the declaration that binds this name written with `static` or `thread_local`? A local with static storage
/// duration outlives every call, so handing out a reference to it is not a defect.
fn declared_static(context: &CheckContext<'_>, binding: &crate::Binding) -> bool {
    let name_start = binding.name_range.start_offset;
    // **The innermost declaration that contains the name** — the local's own statement, not the function around it.
    // `own` walks in document order, and the function's declaration starts earlier and contains this name too, so the
    // answer is the one that starts **last**.
    let declaration = context
        .own::<cpp_parser::CppDeclaration>()
        .filter(|declaration| {
            let range = declaration.syntax().text_range();
            usize::from(range.start()) <= name_start && name_start < usize::from(range.end())
        })
        .max_by_key(|declaration| usize::from(declaration.syntax().text_range().start()));
    let Some(declaration) = declaration else {
        return false;
    };
    let start = usize::from(declaration.syntax().text_range().start());
    let Some(text) = context.source.get(start..name_start) else {
        return false;
    };
    text.split(|character: char| !character.is_alphanumeric() && character != '_')
        .any(|word| word == "static" || word == "thread_local")
}

/// The offset of the last identifier in an expression — `ns::v` answers for `v`, and `v` for `v`.
fn the_last_identifier_offset(expression: &cpp_parser::CppSyntaxNode) -> Option<usize> {
    expression
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| {
            cpp_parser::CppTokenKind::from(token.kind()) == cpp_parser::CppTokenKind::Identifier
        })
        .last()
        .map(|token| usize::from(token.text_range().start()))
}

/// The last identifier's spelling — what a call is to, as far as a name can say.
fn the_last_identifier_of(expression: &cpp_parser::CppSyntaxNode) -> Option<String> {
    expression
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| {
            cpp_parser::CppTokenKind::from(token.kind()) == cpp_parser::CppTokenKind::Identifier
        })
        .last()
        .map(|token| token.text().to_string())
}
