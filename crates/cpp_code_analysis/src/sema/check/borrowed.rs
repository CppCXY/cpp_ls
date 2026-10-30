//! **Where a borrowed value comes from** — the origin analysis the lifetime rules share.
//!
//! ```text
//!   dangling-return   return s;            is `s` a local, a parameter, a member, a temporary?
//!   dangling-field    S(std::string s) : v(s)   the same question, about a member's initialiser
//! ```
//!
//! The answer is asked of the **scope tree** rather than guessed from the spelling: a name is looked up where it is
//! written, and the scope that binds it says whether it is a local, a parameter, a member or a namespace-scope
//! declaration. What the tree cannot say — whether the expression *is* a temporary — is read from the syntax: a call
//! to an owning type constructs one, `x.c_str()` hands out a pointer into `x`, `&x` is the address of `x`, and
//! `std::move(x)` is `x` seen as an rvalue.
//!
//! # What is `Unknown`, and why that is the important answer
//!
//! A call to a function nobody can see (`return get();`), a dereference, a cast: each may return a reference to
//! something long-lived, so nothing is claimed about it. Both rules report only what they can **prove**, and this
//! module is where that decision is made once rather than in each of them.

use cpp_parser::CppAstNode;
use cpp_parser::CppSyntaxKind;

use super::CheckContext;

/// **Where a borrowed value comes from**, for the answers that change what a rule says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    /// A local of this function — the one origin that is always a defect when it is handed out past the call.
    Local,
    /// A local with static storage duration, which outlives every call.
    StaticLocal,
    /// A **by-value** parameter: a copy that dies when the function (or constructor) returns.
    ByValueParameter,
    /// A **reference** parameter: the value lives as long as the argument, which is the caller's obligation.
    ReferenceParameter,
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

/// One borrowed value, as far as a lifetime rule may describe it.
pub(crate) struct Borrowed {
    pub(crate) kind: Origin,
    /// What a reader sees: the local's spelling, or the expression for a subobject.
    pub(crate) description: String,
    /// The member function the borrow came through, for `c_str`/`data`.
    pub(crate) through: Option<&'static str>,
    /// Is this the **address** of the referent (`&x`)?
    pub(crate) address_of: bool,
}

/// **A view type, by the name every library spells it with.**
///
/// `std::string_view` written as `string_view` after a `using namespace`, `absl::string_view`, `llvm::StringRef`,
/// `std::span<T>`: the last segment is the name, and it is the whole test. A project with its own type of one of these
/// names and owning semantics would be reported wrongly — which is the same trade Clang's own lifetime analysis makes,
/// and the one worth making, because these are the names the standard library, libc++ and Abseil use.
pub(crate) fn view_name_of(return_type: &str) -> Option<String> {
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

/// **The origin of `expression`**, as far as a lifetime rule may claim one.
///
/// `function_body` is the region whose locals are "local": a name bound in a function or block scope **inside** it is
/// this function's own; a name bound before it is a parameter (and whether that parameter is by value or by reference
/// is read from the declaration's own text). A caller with no body — an in-class member initialiser — passes the
/// initialiser's own node, and every name in it is then a member or a global.
pub(crate) fn origin_of(
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
            let before_the_body =
                binding.name_range.start_offset < usize::from(function_body.text_range().start());
            let origin = match scope.kind {
                crate::ScopeKind::Class => Origin::Member,
                // **A parameter is bound in the function scope and written before the body begins.** Asked before
                // the "outside the body" arm, because a parameter *is* outside the body — that ordering made every
                // parameter a `Global`, which is silent in both rules that read this and hid the by-value case that
                // `dangling-field` exists for.
                crate::ScopeKind::Function if before_the_body => {
                    if a_parameter_is_a_reference(context, &binding) {
                        Origin::ReferenceParameter
                    } else {
                        Origin::ByValueParameter
                    }
                }
                _ if !inside_the_body => Origin::Global,
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
            // which is the same relationship. Both arrive as one node with the base as the first child.
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
            Some(temporary(expression))
        }
        CppSyntaxKind::CallExpr => {
            let callee = expression.children().next()?;
            let arguments: Vec<cpp_parser::CppSyntaxNode> = expression.children().skip(1).collect();
            let callee_name = the_last_identifier_of(&callee);
            // **`x.c_str()` and `x.data()` hand out a pointer into `x`.** Any other member call returns a value that
            // borrows its object the same way: `v.front()` is a reference into `v`, which is reported through `v`'s
            // origin.
            if is_a_member_access(&callee) {
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

/// **A parameter's own declaration says whether it is by value or by reference** — and the two differ for a lifetime
/// rule: `S(std::string s)` hands the constructor a copy that dies at its end, while `S(const std::string& s)` hands it
/// something the caller owns. The `&` is asked about **before the name**, so a type whose own spelling has one
/// (`int (&)[3]`) still reads as a reference and a by-value `std::string` does not.
fn a_parameter_is_a_reference(context: &CheckContext<'_>, binding: &crate::Binding) -> bool {
    let range = binding.range;
    let Some(text) = context.source.get(range.start_offset..range.end_offset()) else {
        return false;
    };
    let name_start = binding
        .name_range
        .start_offset
        .saturating_sub(range.start_offset)
        .min(text.len());
    text[..name_start].contains('&')
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

fn kind(node: &cpp_parser::CppSyntaxNode) -> CppSyntaxKind {
    CppSyntaxKind::from(node.kind())
}
