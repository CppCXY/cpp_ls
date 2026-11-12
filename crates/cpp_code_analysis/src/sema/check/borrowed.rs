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
//!
//! # The expression is a `CppExpr`, not a child list
//!
//! Every arm used to be a `CppSyntaxKind` match with positional child reads — `children().next()` for the operand,
//! `children().skip(1)` for a call's arguments, a token scan for `&`/`*`, a token scan for "is this a string literal". The
//! sum type names the same shapes, and its accessors answer them: `CppUnaryExpr::{get_operator_text, get_operand}`,
//! `CppCallExpr::{get_callee, get_args}`, `CppMemberExpr::get_object`, `CppLiteralExpr::get_literal_token`,
//! `CppInitListExpr::{get_constructed_type, get_elements}`. A parameter's `&` is its declarator's (`is_a_reference`), and a
//! local's `static` is its specifier sequence's (`has_keyword`) — neither is a slice of the source.

use cpp_parser::{CppAstNode, CppAstToken, CppExpr, CppTokenKind};

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
    /// **Does the binding itself own any storage?** A `string_view` local or parameter owns no bytes: handing out a
    /// copy of it, or a subrange of it, copies a pointer and a length, so destroying the binding destroys nothing —
    /// `dangling-return` must not call it "a local that is destroyed", and `dangling-field` must not call a
    /// `string_view` parameter "destroyed" when its view is copied into another view. A **parameter's** pointer is the
    /// same shape: the characters a `const char*` names belong to whoever passed them. (A *local* pointer is not, and
    /// the two functions that compute this are separate for that reason — see [`the_binding_owns_nothing`].) Measured
    /// on `EmmyLuaCodeStyle`: a view local (`LuaSource::Slice`, `std::string_view source = GetSource(); … return
    /// source.substr(…)`) whose correct code was underlined, and later `spell::Word`'s
    /// `Word(TextRange, std::string_view item) : Item(item) {}` reported as a dangling field.
    pub(crate) owner_owns_nothing: bool,
}

/// **A view type, by the name every library spells it with.**
///
/// `std::string_view` written as `string_view` after a `using namespace`, `absl::string_view`, `llvm::StringRef`,
/// `std::span<T>`: the last segment is the name, and it is the whole test. A project with its own type of one of these
/// names and owning semantics would be reported wrongly — which is the same trade Clang's own lifetime analysis makes,
/// and the one worth making, because these are the names the standard library, libc++ and Abseil use.
pub(crate) fn view_name_of(return_type: &str) -> Option<String> {
    // **The template arguments are not part of the name**: `std::span<int>` is a `span`. Cutting them here keeps every caller
    // from having to, and it is the same rule the `&`/`const` stripping below applies.
    let return_type = return_type.split('<').next().unwrap_or(return_type);
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
/// is asked of the parameter's declarator). A caller with no body — an in-class member initialiser — passes the
/// initialiser's own node, and every name in it is then a member or a global.
pub(crate) fn origin_of(
    context: &CheckContext<'_>,
    expression: &cpp_parser::CppSyntaxNode,
    function_body: &cpp_parser::CppSyntaxNode,
) -> Option<Borrowed> {
    let Some(expression_node) = CppExpr::cast(expression.clone()) else {
        return Some(unknown(expression.text().to_string()));
    };
    match &expression_node {
        CppExpr::ParenExpr(paren) => origin_of(context, paren.get_expr()?.syntax(), function_body),
        CppExpr::NameExpr(name) => {
            let description = name.syntax().text().to_string();
            let offset = usize::from(name.get_last_name_token()?.get_range().start());
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
                // **A parameter is bound in the function scope and written before the body begins.** Asked before the
                // "outside the body" arm, because a parameter *is* outside the body — that ordering made every parameter a
                // `Global`, which is silent in both rules that read this and hid the by-value case `dangling-field` exists for.
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
                // **A local and a by-value parameter are the two bindings this can be about.** Both are copies the
                // function owns — unless what they hold is a view or a pointer, which owns no bytes at all.
                owner_owns_nothing: match origin {
                    Origin::Local => the_binding_owns_nothing(context, &binding),
                    Origin::ByValueParameter => a_parameter_owns_nothing(context, &binding),
                    _ => false,
                },
            })
        }
        // `x.member` is a subobject of what `x` is: the local dies with its member. `v[i]` is an element of `v`, which is the
        // same relationship — and the two node types are the two shapes.
        CppExpr::MemberExpr(member) => {
            let mut borrowed = origin_of(context, member.get_object()?.syntax(), function_body)?;
            borrowed.description = member.syntax().text().to_string();
            Some(borrowed)
        }
        CppExpr::IndexExpr(index) => {
            // `a.b` and `a[i]` are the same node: a subobject or an element is the same lifetime relationship, and the operand
            // is the object either way.
            let mut borrowed = origin_of(context, index.get_object()?.syntax(), function_body)?;
            borrowed.description = index.syntax().text().to_string();
            Some(borrowed)
        }
        CppExpr::ThisExpr(_) => Some(Borrowed {
            kind: Origin::Member,
            description: "this".to_string(),
            through: None,
            address_of: false,
            owner_owns_nothing: false,
        }),
        CppExpr::UnaryExpr(unary) => {
            let operator = unary.get_operator_text();
            if operator.as_deref() == Some("&") {
                // `&x` — the address of the operand, which carries its origin.
                let mut borrowed =
                    origin_of(context, unary.get_operand()?.syntax(), function_body)?;
                borrowed.address_of = true;
                borrowed.description = unary.syntax().text().to_string();
                return Some(borrowed);
            }
            if operator.as_deref() == Some("*") {
                // `*p` — a target that only points-to analysis could name.
                return Some(unknown(unary.syntax().text().to_string()));
            }
            Some(temporary(unary.syntax()))
        }
        CppExpr::CallExpr(call) => {
            let callee_name = call
                .get_callee()
                .and_then(|callee| the_last_name_of(callee.syntax()));
            // **A member call is not its receiver, unless the callee is known to hand out a view of it.**
            //
            // This used to be every member call: "any other member call returns a value that borrows its object the
            // same way". That is true of a handful of standard members and false of an ordinary method, and the false
            // case is the one that reaches a reader — measured on `EmmyLuaCodeStyle`:
            //
            // ```cpp
            // LuaStyle &LuaCodeFormat::GetStyle(...) {
            //     std::shared_ptr<LuaEditorConfig> editorConfig = nullptr;
            //     return editorConfig->Generate(fileUri);   // reported as "`editorConfig` is a local, and this
            // }                                             //  function returns a reference to it"
            // ```
            //
            // `Generate` returns a reference to an object the local *points at*, not to the local; whether that object
            // outlives the call is a question about `Generate`'s body and its owner, which this layer cannot answer. The
            // list below is the standard members whose contract **is** "a view of me" — each is documented to return a
            // reference or a pointer into its own object — so the lifetime question really is its receiver's.
            const BORROWING_MEMBERS: &[&str] =
                &["c_str", "data", "front", "back", "at", "operator[]"];
            if let Some(callee) = call.get_callee()
                && let Some(receiver) = a_member_access_object(&callee)
                && callee_name
                    .as_deref()
                    .is_some_and(|name| BORROWING_MEMBERS.contains(&name))
            {
                let mut borrowed = origin_of(context, receiver.syntax(), function_body)?;
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
                    return origin_of(context, call.get_args().first()?.syntax(), function_body);
                }
                // A view constructed from an argument borrows that argument: `std::string_view(s)`.
                if view_name_of(name).is_some() {
                    return match call.get_args().first() {
                        Some(argument) => origin_of(context, argument.syntax(), function_body),
                        None => Some(temporary(call.syntax())),
                    };
                }
                // An owning type constructed here is a temporary, and it dies at the end of the statement.
                if is_an_owner(name) {
                    return Some(temporary(call.syntax()));
                }
            }
            // **A call this analysis cannot see is not evidence.** `return get();` may return a reference to a global.
            Some(unknown(call.syntax().text().to_string()))
        }
        CppExpr::InitListExpr(list) => {
            // **A braced list with a head constructs that type** — `std::string_view{ x }` is a temporary view, not `x`. A
            // headless `{ x }` is `x` initialised, which carries `x`'s origin.
            if list.get_constructed_type().is_some() {
                return Some(temporary(list.syntax()));
            }
            match list.get_elements().next() {
                Some(element) => origin_of(context, element.syntax(), function_body),
                None => Some(temporary(list.syntax())),
            }
        }
        CppExpr::LiteralExpr(literal) => {
            // A string literal is an array with static storage duration; every other literal is a prvalue.
            let is_string = literal
                .get_literal_token()
                .is_some_and(|token| token.get_token_kind() == CppTokenKind::StringLiteral);
            Some(Borrowed {
                kind: if is_string {
                    Origin::StringLiteral
                } else {
                    Origin::Temporary
                },
                description: literal.syntax().text().to_string(),
                through: None,
                address_of: false,
                owner_owns_nothing: false,
            })
        }
        _ => Some(unknown(expression.text().to_string())),
    }
}

/// **A parameter's own declaration says whether it is by value or by reference** — and the two differ for a lifetime rule:
/// `S(std::string s)` hands the constructor a copy that dies at its end, while `S(const std::string& s)` hands it something
/// the caller owns.
///
/// Asked of the **parameter node** whose span covers the binding (`CppDeclarator::is_a_reference`), not of the source text
/// before the name: `int (&)[3]` is a reference and a by-value `std::string` is not, and a `&` written in a default argument
/// belongs to no parameter.
fn a_parameter_is_a_reference(context: &CheckContext<'_>, binding: &crate::Binding) -> bool {
    let start = binding.range.start_offset;
    let end = binding.range.end_offset();
    let parameter = context
        .own::<cpp_parser::CppParam>()
        .filter(|parameter| {
            let range = parameter.syntax().text_range();
            usize::from(range.start()) <= start && end <= usize::from(range.end())
        })
        .max_by_key(|parameter| usize::from(parameter.syntax().text_range().start()));
    parameter
        .and_then(|parameter| parameter.get_declarator())
        .is_some_and(|declarator| declarator.is_a_reference())
}

/// **Is the declaration that binds this name itself a view?**
///
/// A `std::string_view` local owns no bytes: a copy of it, and a subrange of it, are a pointer and a length, so
/// destroying the local destroys nothing and handing one out is not a defect. `dangling-return` reports a local
/// *owner* handed out as a view (`std::string s; return s;`), and this is the test that keeps it from reporting the
/// other shape — measured on `EmmyLuaCodeStyle`'s `LuaSource::Slice`, which is
/// `std::string_view source = GetSource(); … return source.substr(…)`.
///
/// **A pointer is not this question**, deliberately: `const char* p = local.c_str(); return p;` hands out storage the
/// *local* owns, which is exactly what `dangling-return` is for. [`a_parameter_owns_nothing`] is where a pointer counts,
/// because a parameter's pointer names the caller's bytes rather than a local's.
///
/// The declaration is found the way [`declared_static`] finds it: the innermost declaration containing the name.
fn the_binding_owns_nothing(context: &CheckContext<'_>, binding: &crate::Binding) -> bool {
    let name_start = binding.name_range.start_offset;
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
    let Some(specifiers) = declaration.get_decl_specifiers() else {
        return false;
    };
    let written = specifiers
        .get_type_name()
        .map(|name| name.get_qualified_name())
        .unwrap_or_else(|| specifiers.get_type_text());
    view_name_of(&written).is_some()
}

/// **Does a by-value parameter own any storage?** — the same question for the other binding that is a copy.
///
/// Asked of the **parameter node**, which is where a parameter's type lives (`CppParam::get_type_text` composes it from
/// the specifiers and the declarator's `*`/`&`/`[]` parts). A `std::string_view item` parameter owns no bytes and a
/// `const char* p` owns none either: what they name belongs to the caller, so copying one into a view member is not a
/// borrow of the parameter. Measured on `EmmyLuaCodeStyle`'s `spell::Word`, whose correct
/// `Word(TextRange range, std::string_view item) : Item(item) {}` was reported as "a parameter that is destroyed".
fn a_parameter_owns_nothing(context: &CheckContext<'_>, binding: &crate::Binding) -> bool {
    let start = binding.range.start_offset;
    let end = binding.range.end_offset();
    let parameter = context
        .own::<cpp_parser::CppParam>()
        .filter(|parameter| {
            let range = parameter.syntax().text_range();
            usize::from(range.start()) <= start && end <= usize::from(range.end())
        })
        .max_by_key(|parameter| usize::from(parameter.syntax().text_range().start()));
    let Some(parameter) = parameter else {
        return false;
    };
    let written = parameter.get_type_text();
    // A pointer's trailing `*` is the declarator's (`int* p`, `const char *p`), and `view_name_of` classifies the rest.
    written.trim_end().ends_with('*') || view_name_of(&written).is_some()
}

fn unknown(description: String) -> Borrowed {
    Borrowed {
        kind: Origin::Unknown,
        description,
        through: None,
        address_of: false,
        owner_owns_nothing: false,
    }
}

fn temporary(expression: &cpp_parser::CppSyntaxNode) -> Borrowed {
    Borrowed {
        kind: Origin::Temporary,
        description: expression.text().to_string(),
        through: None,
        address_of: false,
        owner_owns_nothing: false,
    }
}

/// Is the declaration that binds this name written with `static` or `thread_local`? A local with static storage duration
/// outlives every call, so handing out a reference to it is not a defect.
fn declared_static(context: &CheckContext<'_>, binding: &crate::Binding) -> bool {
    let name_start = binding.name_range.start_offset;
    // **The innermost declaration that contains the name** — the local's own statement, not the function around it.
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
    declaration.get_decl_specifiers().is_some_and(|specifiers| {
        specifiers.has_keyword(CppTokenKind::StaticKeyword)
            || specifiers.has_keyword(CppTokenKind::ThreadLocalKeyword)
    })
}

/// **The object of a member access** — `s` for `s.c_str`, whether the parser wrote a `MemberExpr` or an `IndexExpr`.
fn a_member_access_object(expression: &CppExpr) -> Option<CppExpr> {
    match expression {
        CppExpr::MemberExpr(member) => member.get_object(),
        CppExpr::IndexExpr(index) if index.is_a_member_access() => index.get_object(),
        _ => None,
    }
}

/// **The last name a callee writes** — `ns::free` is `free`, and `p->release` is `release`.
fn the_last_name_of(expression: &cpp_parser::CppSyntaxNode) -> Option<String> {
    match CppExpr::cast(expression.clone())? {
        CppExpr::NameExpr(name) => name
            .get_last_name_token()
            .map(|token| token.get_name_text().to_string()),
        CppExpr::MemberExpr(member) => member
            .get_member_name()
            .map(|token| token.get_name_text().to_string()),
        CppExpr::IndexExpr(index) => index
            .get_member_name()
            .map(|token| token.get_name_text().to_string()),
        _ => None,
    }
}
