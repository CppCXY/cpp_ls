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

use super::borrowed::{Origin, origin_of, view_name_of};
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
