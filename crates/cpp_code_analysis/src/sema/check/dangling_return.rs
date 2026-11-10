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
//!
//! # The result type is the declaration's, not a slice of the source
//!
//! It used to be `context.source[start of the function .. start of its name]`, searched for `&`/`*` before the first
//! `<`. The same three questions are nodes: `CppDeclarator::{is_a_reference, is_a_pointer}` for the two operators, and
//! `CppDeclSpecifierSeq::get_type_name` for the type's name — which is what `view_name_of` classifies, without also
//! seeing a `*` in a template argument.

use cpp_parser::CppAstNode;

use super::borrowed::{Origin, origin_of, view_name_of};
use super::{Check, CheckContext, DiagnosticCode};

pub struct DanglingReturn;

impl Check for DanglingReturn {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::DanglingReturn];

    fn check(context: &mut CheckContext<'_>) {
        // **Every `return` of this file's own text**, and the function it belongs to. The walk outward is what turns a `return`
        // into a question about a *result type*, and it stops at a lambda, whose type is the lambda's.
        let returns: Vec<cpp_parser::CppReturnStat> =
            context.own::<cpp_parser::CppReturnStat>().collect();

        for statement in returns {
            let Some(function) = enclosing_function(statement.syntax()) else {
                continue;
            };
            let Some(body) = function.get_body() else {
                continue;
            };
            let Some(declarator) = function
                .get_init_declarator()
                .and_then(|init| init.get_declarator())
            else {
                continue;
            };

            // **The result type is the declaration's own**: a `&`/`&&` part of the declarator makes a reference, a `*` part a
            // pointer, and the type's *name* is what `view_name_of` classifies.
            let is_reference = declarator.is_a_reference();
            let is_pointer = declarator.is_a_pointer();
            let head = function
                .get_decl_specifiers()
                .map(|specifiers| {
                    specifiers
                        .get_type_name()
                        .map(|name| name.get_qualified_name())
                        .unwrap_or_else(|| specifiers.get_type_text())
                })
                .unwrap_or_default();
            let view = view_name_of(&head);
            if !is_reference && !is_pointer && view.is_none() {
                continue;
            }

            let Some(expression) = statement.get_expr() else {
                continue;
            };
            let Some(borrowed) = origin_of(context, expression.syntax(), body.syntax()) else {
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
                    // **A view of a view is not a dangling view.** The local is itself non-owning, so a copy of it — or
                    // a subrange of it — carries a pointer and a length that outlive the local; what it points into is
                    // whatever the view was made from, which this function never owned. Reported from
                    // `EmmyLuaCodeStyle`'s `LuaSource::Slice`: `std::string_view source = GetSource(); return
                    // source.substr(…)` was underlined as "a local that is destroyed".
                    Origin::Local if borrowed.owner_is_a_view => None,
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
                // A pointer: certain only when the referent is this function's own local and the value is either the local's
                // address or a pointer the local handed out.
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
            let range = cpp_parser::source_range(expression.get_range());
            context.add(DiagnosticCode::DanglingReturn, range, "", message);
        }
    }
}

/// The function declaration a `return` belongs to — `None` for a return inside a lambda, whose result type is the lambda's own.
fn enclosing_function(statement: &cpp_parser::CppSyntaxNode) -> Option<cpp_parser::CppDeclaration> {
    for ancestor in statement.ancestors().skip(1) {
        // **A lambda's `return` belongs to the lambda**: its type is the closure's own, and this rule has nothing to say.
        if cpp_parser::CppLambdaExpr::can_cast(ancestor.kind().into()) {
            return None;
        }
        if let Some(declaration) = cpp_parser::CppDeclaration::cast(ancestor.clone())
            && declaration.get_body().is_some()
        {
            return Some(declaration);
        }
    }
    None
}
