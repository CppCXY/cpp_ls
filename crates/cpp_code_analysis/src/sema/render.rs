//! # Rendering a declaration
//!
//! **The hover card's code block is generated, never sliced.** A declaration's own text is the wrong thing to show: it
//! carries the vendor's macro noise (`& _CLR_OR_THIS_CALL read(…)`), the line breaks the file happened to use, and the
//! spelling of one particular declaration rather than the shape of the thing. So every character here comes from the
//! analysis's model — [`Type`]s printed by their own writer, parameters parsed out of the recorded list, the template
//! head built from the declaration's parameters — and this module is the **only** place the hover's code block is
//! composed.
//!
//! # What a reconstructed line looks like
//!
//! ```text
//!   template <class _Tp, class _Alloc = std::allocator<_Tp>>
//!   void push_back(const _Tp& __x)
//!
//!   template <class _Kty, class _Ty>
//!   class map
//!
//!   using size_type = std::size_t;
//!   static constexpr int kWidth = …;
//!   enum class Color
//! ```
//!
//! The shape is rust-analyzer's: the item alone, one line where it fits, the template head above it, and no body. What
//! differs is what C++ a reader needs and Rust has no equivalent for — the trailing `const`/`&`/`&&` that tells two
//! overloads apart, `= …` for a defaulted argument, and the template head's kinds (`class`/`typename`/a non-type type),
//! which the syntax supplies when the declaring file is at hand.
//!
//! # What is *not* rendered
//!
//! A **body**, an **initializer** (beyond a constant whose value the analysis folded), and a **(doc) comment**: the first
//! two are not the interface, and the third is documentation, which [`crate::sema::documentation`] formats elsewhere.
//! Interpolation is the rule for everything else: a type the index could not resolve is printed as the name that was
//! written, because that is the model's honest answer and not a copy of the line.

use cpp_parser::CppAstNode;
use cpp_parser::{CppDeclaration, CppTemplateDecl};

use crate::sema::types::parse_type_spelling;
use crate::{DeclFact, DeclKind, DeclModifiers};

/// **One parameter of a function, rebuilt** — its type as the model prints it, its own name, and whether it has a
/// default argument.
///
/// The default's **value** is not kept: it is an expression in the declaration's text, and a signature is about what can
/// be written at a call, which is "this parameter may be left off". [`Parameter::default`] is `Some` exactly then, and
/// the *rendering* is `= …` (or the name, when the declaration wrote one, because `npos` is what a reader recognises).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub ty: String,
    pub name: Option<String>,
    pub default: Option<String>,
    pub variadic: bool,
}

impl Parameter {
    fn said(&self) -> String {
        if self.variadic && self.ty.is_empty() {
            return "...".to_string();
        }
        let mut out = self.ty.clone();
        if let Some(name) = &self.name {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(name);
        }
        if let Some(default) = &self.default {
            out.push_str(" = ");
            out.push_str(default);
        }
        out
    }
}

/// **A template parameter, as the head writes it** — `class T`, `typename T = int`, `int N`, `class... Args`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateParameter {
    /// `class`, `typename`, or the written type of a non-type parameter (`int`, `std::size_t`).
    pub keyword: String,
    pub name: String,
    pub default: Option<String>,
    pub pack: bool,
    /// A non-type parameter's type is a type; a type parameter's keyword is not.
    pub is_type: bool,
}

/// **The declaration's line, or lines, rebuilt** — the code block a hover shows.
///
/// `syntax` is the declaration's own node when the declaring file is at hand. It supplies the three things a fact does
/// not record: whether the class was written `class`, `struct` or `union` (and whether an enum is scoped), the kinds of
/// a template head's parameters, and the access of a base. Without it the rendering is still complete for everything
/// whose facts carry the answer, and a class whose keyword is unknown is rendered by name alone rather than guessed at.
///
/// `value` is a constant the analysis folded — an enumerator's number, a `constexpr` initializer — and is shown only
/// where it is part of what the name *is*.
pub fn an_item(
    fact: &DeclFact,
    syntax: Option<&CppDeclaration>,
    value: Option<i64>,
) -> Vec<String> {
    let mut lines = Vec::new();
    // **A reflection annotation is part of the declaration**, not decoration around it: `UCLASS(Blueprintable)` is what
    // makes an Unreal class a class as far as the engine is concerned, and the fact records the macro and its arguments.
    // Rendered as `NAME(arg, arg)` from those parts — generated, like everything else in this block.
    if let Some(reflection) = &fact.reflection {
        lines.push(reflection.said());
    }
    if let Some(head) = a_template_head(fact, syntax) {
        lines.push(head);
    }
    lines.push(a_line(fact, syntax, value));
    lines
}

/// **The declaration's own line**, without a template head — what an overload list prints per declaration.
pub fn a_line(fact: &DeclFact, syntax: Option<&CppDeclaration>, value: Option<i64>) -> String {
    match fact.kind {
        DeclKind::Function => a_function(fact),
        DeclKind::Type if fact.type_of.is_some() => {
            format!(
                "using {} = {}{};",
                fact.name,
                the_written_qualifiers(syntax),
                a_type(fact.type_of.as_deref().unwrap_or(""))
            )
        }
        DeclKind::Type => a_class(fact, syntax),
        DeclKind::Variable => a_variable(fact, value, syntax),
        DeclKind::Namespace => match &fact.type_of {
            Some(target) => format!("namespace {} = {target}", fact.name),
            None => format!("namespace {}", fact.name),
        },
        DeclKind::MacroLike | DeclKind::Other => a_variable(fact, value, syntax),
    }
}

/// **The `template <…>` line**, when this declaration is itself the template.
///
/// A **member** of a class template carries the class's parameter names in [`DeclFact::parameters`] — the member-list
/// builder puts them there so a use's arguments can be paired — and they are not the member's own: printing them said
/// "`std::pair::second` — a variable template over `_T1`, `_T2`". So the head is rendered only when the declaration is
/// the template, which the fact says by having **no scope** (a template at file or namespace scope) and the syntax says
/// exactly (a `TemplateDecl` child).
///
/// The kinds (`class` vs `typename` vs a non-type type) and the defaults are the syntax's where it is present; the fact's
/// `parameters`/`parameter_defaults` cover the rest, with `class` as the keyword — the standard spelling for a type
/// parameter, and the only one a fact cannot distinguish.
fn a_template_head(fact: &DeclFact, syntax: Option<&CppDeclaration>) -> Option<String> {
    if let Some(syntax) = syntax
        && let Some(head) = syntax.child::<CppTemplateDecl>()
        && let Some(list) = head.get_param_list()
    {
        let parameters: Vec<TemplateParameter> = list
            .get_params()
            .map(|parameter| a_template_parameter(&parameter))
            .collect();
        if !parameters.is_empty() {
            return Some(a_head(&parameters));
        }
    }
    // **The fact's own parameters, and only when the declaration is the template.** A member of a class template has
    // them too, which is why the scope decides — see this function's documentation.
    if fact.scope.is_some() || fact.parameters.is_empty() {
        return None;
    }
    let parameters: Vec<TemplateParameter> = fact
        .parameters
        .iter()
        .enumerate()
        .filter(|(_, name)| !name.is_empty())
        .map(|(at, name)| TemplateParameter {
            keyword: "class".to_string(),
            name: name.clone(),
            default: fact
                .parameter_defaults
                .get(at)
                .filter(|default| !default.trim().is_empty())
                .map(|default| a_default(default)),
            pack: false,
            is_type: true,
        })
        .collect();
    (!parameters.is_empty()).then(|| a_head(&parameters))
}

fn a_head(parameters: &[TemplateParameter]) -> String {
    let said: Vec<String> = parameters.iter().map(a_template_parameter_said).collect();
    format!("template <{}>", said.join(", "))
}

fn a_template_parameter_said(parameter: &TemplateParameter) -> String {
    let mut out = parameter.keyword.clone();
    if parameter.pack {
        out.push_str("...");
    }
    if !parameter.name.is_empty() {
        out.push(' ');
        out.push_str(&parameter.name);
    }
    if let Some(default) = &parameter.default {
        out.push_str(" = ");
        out.push_str(default);
    }
    out
}

/// One template parameter, read from the syntax — the only place its **kind** is written.
fn a_template_parameter(parameter: &cpp_parser::CppTemplateParam) -> TemplateParameter {
    let written = parameter.syntax().text().to_string();
    let name = parameter
        .get_name()
        .map(|name| name.get_name_text().to_string())
        .unwrap_or_default();
    // **The default, at a top-level `=`.** `class A = std::array<int, 3>` has a bracket pair and a comma in its
    // default; splitting at the first `=` byte works for it and would not for a parameter whose *name* is followed by
    // `=` inside a template argument.
    let (declared, default) = match split_at_a_top_level_default(&written) {
        Some((declared, default)) => (declared, Some(a_default(default))),
        None => (written.as_str(), None),
    };
    let declared = declared.trim_end();
    let pack = declared.contains("...");
    let is_type = parameter.is_type_param();
    let keyword = if is_type {
        // `class` and `typename` mean the same thing; the declaration's own word is kept when it has one.
        if declared.trim_start().starts_with("typename") {
            "typename".to_string()
        } else {
            "class".to_string()
        }
    } else {
        // A non-type parameter: what stands before the name is its type, rendered canonically.
        let before_the_name = match declared.find(&name) {
            Some(at) if !name.is_empty() => &declared[..at],
            _ => declared,
        };
        // **The pack's dots are the head's, not the type's.** `char... Digits` writes `...` before the name, and
        // `pack` puts them back once — keeping them in the keyword rendered `char......`.
        a_type(before_the_name.trim_end().trim_end_matches('.').trim_end())
    };
    TemplateParameter {
        keyword,
        name,
        default,
        pack,
        is_type,
    }
}

/// **A function or method**, rebuilt: `static constexpr int f(const T& value) const & noexcept`.
fn a_function(fact: &DeclFact) -> String {
    let mut out = String::new();
    for (bit, word) in [
        (DeclModifiers::STATIC, "static "),
        (DeclModifiers::INLINE, "inline "),
        (DeclModifiers::CONSTEXPR, "constexpr "),
        (DeclModifiers::CONSTEVAL, "consteval "),
        (DeclModifiers::CONSTINIT, "constinit "),
        (DeclModifiers::VIRTUAL, "virtual "),
        (DeclModifiers::EXPLICIT, "explicit "),
    ] {
        if fact.modifiers.bits() & bit.bits() != 0 {
            out.push_str(word);
        }
    }
    if let Some(returns) = fact
        .returns
        .as_deref()
        .filter(|returns| !returns.trim().is_empty())
    {
        out.push_str(&a_type(returns));
        out.push(' ');
    }
    out.push_str(&fact.name);
    match fact.parameter_list.as_deref() {
        Some(list) => {
            let parameters: Vec<String> = parameters(list).iter().map(Parameter::said).collect();
            out.push('(');
            out.push_str(&parameters.join(", "));
            out.push(')');
        }
        // **A function with no recorded list is still a function**: the parentheses and an ellipsis say "there are
        // parameters this build did not read", rather than printing a name that looks like a variable.
        None => out.push_str("(…)"),
    }
    out.push_str(&a_trailing(fact));
    out
}

/// **What a function writes after its parameter list** — the half that tells two overloads apart.
fn a_trailing(fact: &DeclFact) -> String {
    let mut out = String::new();
    for (bit, word) in [
        (DeclModifiers::CONST, " const"),
        (DeclModifiers::VOLATILE, " volatile"),
        (DeclModifiers::RVALUE_REFERENCE, " &&"),
        (DeclModifiers::LVALUE_REFERENCE, " &"),
        (DeclModifiers::OVERRIDE, " override"),
        (DeclModifiers::FINAL, " final"),
    ] {
        if fact.modifiers.bits() & bit.bits() != 0 {
            out.push_str(word);
        }
    }
    if fact.modifiers.bits() & DeclModifiers::DELETED.bits() != 0 {
        out.push_str(" = delete");
    } else if fact.modifiers.bits() & DeclModifiers::DEFAULTED.bits() != 0 {
        out.push_str(" = default");
    }
    if let Some(clause) = fact
        .requires_clause
        .as_deref()
        .filter(|clause| !clause.trim().is_empty())
    {
        // **The clause is stored as the file wrote it, keyword included** — `requires (x > 0)`,
        // `requires requires { fn(1); }` — so the renderer must not add a second keyword: measured, every constrained
        // declaration's card read `requires requires (sizeof(T) > 2)`, and the nested form read
        // `requires requires requires { fn(1); }`. A clause stored without the keyword (a hand-built fact in a test)
        // still gets one, which is why the check is on the text and not on a flag.
        out.push(' ');
        if !clause.trim_start().starts_with("requires") {
            out.push_str("requires ");
        }
        out.push_str(clause.trim());
    }
    out
}

/// **A class, struct, union or enum** — the keyword from the syntax, and the bases by name.
fn a_class(fact: &DeclFact, syntax: Option<&CppDeclaration>) -> String {
    let keyword = syntax.and_then(a_class_keyword);
    let mut out = String::new();
    // **No keyword, no guess.** A fact records `Type` and not which word introduced it, and printing `class` for a
    // `struct` would be a guess dressed as a fact. Without the declaring file the name alone is what is known.
    if let Some(keyword) = keyword {
        out.push_str(&keyword);
        out.push(' ');
    }
    out.push_str(&fact.name);
    let bases: Vec<String> = fact
        .bases
        .iter()
        .filter(|base| !base.trim().is_empty())
        .map(|base| a_type(base))
        .collect();
    if !bases.is_empty() {
        out.push_str(" : ");
        out.push_str(&bases.join(", "));
    }
    out
}

/// The keyword a class-like definition was written with, from its own syntax — `class shared_ptr`, `struct S`,
/// `union U`, `enum class Color`.
fn a_class_keyword(declaration: &CppDeclaration) -> Option<String> {
    // **The node kind is the keyword.** A class-like definition has one of three syntax kinds, and the parser chose it
    // from the word the file wrote — so this is the fact, not a guess.
    if let Some(class) = declaration.get_class_def() {
        return Some(
            match (class.is_struct(), class.is_union()) {
                (true, _) => "struct",
                (_, true) => "union",
                _ => "class",
            }
            .to_string(),
        );
    }
    if let Some(enumeration) = declaration.get_enum_def() {
        return Some(match enumeration.is_scoped() {
            true => "enum class".to_string(),
            false => "enum".to_string(),
        });
    }
    None
}

/// **A variable, a constant or an enumerator.**
///
/// The initializer is not part of the interface, so it is not shown; the **value** is when the analysis folded one,
/// because for an enumerator (`Red = 3`) or a `constexpr` constant the number *is* what the name means.
fn a_variable(fact: &DeclFact, value: Option<i64>, syntax: Option<&CppDeclaration>) -> String {
    let mut out = String::new();
    for (bit, word) in [
        (DeclModifiers::STATIC, "static "),
        (DeclModifiers::INLINE, "inline "),
        (DeclModifiers::CONSTEXPR, "constexpr "),
        (DeclModifiers::CONSTEVAL, "consteval "),
    ] {
        if fact.modifiers.bits() & bit.bits() != 0 {
            out.push_str(word);
        }
    }
    if let Some(ty) = fact.type_of.as_deref().filter(|ty| !ty.trim().is_empty()) {
        // **The qualifiers the fact drops.** `declared_type_of` strips `const` and `volatile` on purpose — it is a
        // spelling to look a class up by, and a qualifier changes nothing about which class that is — so
        // `static const Widget* first` recorded `Widget*`. A card that hides `const` tells the reader the variable may be
        // written through, which is the one thing it may not. The syntax still has them, so they are read from the
        // declaration's own specifier sequence; a `Foo* const p` writes its `const` in the declarator, where it
        // qualifies the pointer and not the type named here.
        out.push_str(the_written_qualifiers(syntax));
        out.push_str(&a_type(ty));
        out.push(' ');
    }
    out.push_str(&fact.name);
    if let Some(value) = value {
        out.push_str(&format!(" = {value}"));
    }
    out
}

/// **`const ` and `volatile ` as the declaration wrote them**, from its specifier sequence — `""` when there are none or
/// the declaration's syntax is not at hand.
fn the_written_qualifiers(syntax: Option<&CppDeclaration>) -> &'static str {
    let Some(specifiers) = syntax.and_then(|declaration| declaration.get_decl_specifiers()) else {
        return "";
    };
    use cpp_parser::CppTokenKind;
    let volatile = specifiers.has_keyword(CppTokenKind::VolatileKeyword);
    let constant = specifiers.has_keyword(CppTokenKind::ConstKeyword);
    match (constant, volatile) {
        (true, true) => "const volatile ",
        (true, false) => "const ",
        (false, true) => "volatile ",
        (false, false) => "",
    }
}

/// **A type, as the model writes it** — parsed from the recorded spelling and printed by [`Type`]'s own writer, which is
/// what turns `std::vector<std::string> &` into `std::vector<std::string>&` and drops the `typename` disambiguator.
///
/// The spelling is the *input*; the output is generated. A spelling this layer cannot parse is printed back as a name,
/// which is the model's answer for a type it does not model — not a copy of the declaration.
pub fn a_type(written: &str) -> String {
    let written = written.trim();
    if written.is_empty() {
        return String::new();
    }
    parse_type_spelling(written).to_string()
}

/// **The parameters of a recorded `parameter_list`**, rebuilt — types canonical, names kept, defaults summarised.
///
/// The split is at a **top-level comma**: `std::pair<int, int>` is one parameter, `void (*cb)(int, int)` is one too. A
/// list whose parentheses do not balance answers what it could read, which is the conservative direction for a popup.
pub fn parameters(written: &str) -> Vec<Parameter> {
    let inner = written.trim();
    let inner = inner.strip_prefix('(').unwrap_or(inner);
    let inner = inner.strip_suffix(')').unwrap_or(inner);
    split_at_top_level_commas(inner)
        .into_iter()
        .map(|parameter| a_parameter(parameter.trim()))
        .collect()
}

fn a_parameter(written: &str) -> Parameter {
    if written.is_empty() {
        return Parameter {
            ty: String::new(),
            name: None,
            default: None,
            variadic: false,
        };
    }
    if written == "..." || written == "…" {
        return Parameter {
            ty: String::new(),
            name: None,
            default: None,
            variadic: true,
        };
    }
    // **The default, at a top-level `=`.** `operator=` and `==` are not defaults, and neither is a `=` inside a
    // template argument or a parenthesised expression.
    let (declared, default) = match split_at_a_top_level_default(written) {
        Some((declared, default)) => (declared, Some(a_default(default))),
        None => (written, None),
    };
    let declared = declared.trim();
    let (ty, name) = a_type_and_its_name(declared);
    Parameter {
        ty,
        name,
        default,
        variadic: false,
    }
}

/// **The last identifier of a parameter is its name**, unless it is a keyword or part of the type — `const char* const`
/// has none, `const _Tp& __x` has `__x`, and `int (*cb)(int)` has `cb`.
fn a_type_and_its_name(written: &str) -> (String, Option<String>) {
    // A function pointer's name is inside its parentheses: `int (*cb)(int)`.
    if let Some(name) = a_name_in_parentheses(written) {
        return (a_type(written), Some(name));
    }
    let words: Vec<(usize, &str)> = identifier_runs(written);
    let Some((at, last)) = words.last().copied() else {
        return (a_type(written), None);
    };
    if is_a_keyword(last) {
        return (a_type(written), None);
    }
    // **The name is a *declared* name**, so what follows it is nothing but punctuation the declarator may carry —
    // an array extent, a trailing qualifier — and what precedes it is a type, not another name (`ns::Widget` is a type
    // and `Widget` is its last segment, not a parameter name).
    let after = written[at + last.len()..].trim();
    let before = &written[..at];
    // **A space counts, and it is the ordinary case**: `int n` has `int ` before `n`, and a test that trimmed it away
    // found no name in the commonest parameter there is.
    let follows_a_type = before.ends_with(char::is_whitespace)
        || before.trim_end().ends_with(['*', '&', '>', ']', ')']);
    let is_not_qualified = !before.trim_end().ends_with("::");
    let is_a_declarator_end = after.is_empty() || after.starts_with('[');
    if follows_a_type && is_not_qualified && is_a_declarator_end {
        let ty = written[..at].trim();
        (a_type(ty), Some(last.to_string()))
    } else {
        (a_type(written), None)
    }
}

/// The name of a function-pointer parameter — the identifier inside `(* … )`.
fn a_name_in_parentheses(written: &str) -> Option<String> {
    let open = written.find("(*")?;
    let rest = &written[open + 2..];
    let close = rest.find(')')?;
    let inside = rest[..close].trim();
    let name = inside.trim_start_matches(['&', '*']).trim();
    (!name.is_empty()
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_'))
    .then(|| name.to_string())
}

/// Every run of identifier characters, with its byte offset.
fn identifier_runs(written: &str) -> Vec<(usize, &str)> {
    let mut runs = Vec::new();
    let mut start = None;
    for (at, character) in written.char_indices() {
        match (character.is_alphanumeric() || character == '_', start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                runs.push((from, &written[from..at]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        runs.push((from, &written[from..]));
    }
    runs
}

/// The words that cannot be a declared name — a parameter written `const char* const` has no name, and its last word
/// is a qualifier.
fn is_a_keyword(word: &str) -> bool {
    matches!(
        word,
        "const"
            | "volatile"
            | "constexpr"
            | "consteval"
            | "constinit"
            | "static"
            | "inline"
            | "mutable"
            | "unsigned"
            | "signed"
            | "long"
            | "short"
            | "int"
            | "char"
            | "float"
            | "double"
            | "void"
            | "bool"
            | "auto"
            | "struct"
            | "class"
            | "union"
            | "enum"
            | "typename"
    )
}

/// **A default argument, as the signature shows it** — the name or number the declaration wrote when it is a single
/// word (`npos`, `0`, `true`, `nullptr`), and `…` for an expression whose value is the initializer's business, not the
/// interface's.
fn a_default(written: &str) -> String {
    let written = written.trim();
    if written.is_empty() {
        return "…".to_string();
    }
    let single_word = written
        .chars()
        .all(|character| character.is_alphanumeric() || matches!(character, '_' | '.' | '+' | '-'));
    if single_word {
        return written.to_string();
    }
    "…".to_string()
}

/// **Split at top-level commas**, counting `<>`, `()` and `[]` as nesting — `std::map<int, int>, int` is two parameters
/// and `void (*)(int, int)` is one.
fn split_at_top_level_commas(written: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (at, character) in written.char_indices() {
        match character {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&written[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&written[start..]);
    parts
}

/// The default argument's `=`, at depth zero and not part of `==`, `<=`, `>=` or `!=`.
fn split_at_a_top_level_default(written: &str) -> Option<(&str, &str)> {
    let bytes = written.as_bytes();
    let mut depth = 0i32;
    for (at, byte) in bytes.iter().enumerate() {
        match byte {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' | b')' | b']' => depth -= 1,
            b'=' if depth == 0 => {
                let before = written[..at].trim_end();
                let ends_with_operator = before.ends_with(['=', '!', '<', '>']);
                let starts_with_equals = bytes.get(at + 1) == Some(&b'=');
                if !ends_with_operator && !starts_with_equals {
                    return Some((&written[..at], &written[at + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpp_parser::{CppParser, ParserConfig};

    /// **The fact and the node of one declaration in `source`** — what a hover has when the declaring file is at hand.
    fn rendered(source: &str, name: &str) -> Vec<String> {
        let tree = CppParser::parse(source, ParserConfig::default());
        assert_eq!(tree.get_errors(), [], "the fixture must parse cleanly");
        let root = tree.get_red_root();
        let scopes = crate::sema::scopes::build_scopes(&root, &crate::NoMacroBodies);
        let (facts, _) = crate::sema::declarations::build_facts(
            &scopes,
            &crate::preprocess(source, tree.get_tokens()),
            &root,
            &[],
        );
        let fact = facts
            .iter()
            .find(|fact| fact.name == name)
            .unwrap_or_else(|| panic!("{name} must be a fact: {facts:?}"));
        // **The node at the name**, which is how the session finds it — a template's declaration starts at
        // `template`, and a class's fact starts at `class`, so a range match on the fact's start finds neither.
        let declaration =
            cpp_parser::token_at(&root, fact.name_range.start_offset).and_then(|token| {
                token
                    .parent_ancestors()
                    .find_map(cpp_parser::CppDeclaration::cast)
            });
        an_item(fact, declaration.as_ref(), None)
    }

    /// **A function is rebuilt**, not quoted: canonical types, the parameters' names kept, defaults said, and the
    /// trailing qualifiers that tell two overloads apart.
    #[test]
    fn a_function_is_rebuilt_from_its_facts() {
        let lines = rendered(
            "int total(const Widget& w, int scale = 2, ...) const &;\n",
            "total",
        );
        assert_eq!(
            lines,
            ["int total(const Widget& w, int scale = 2, ...) const &"]
        );
    }

    /// **A template head is the declaration's own**, with the kind and default the syntax writes.
    #[test]
    fn a_template_head_keeps_its_kinds_and_defaults() {
        let lines = rendered(
            "template <typename T, typename Alloc = std::allocator<T>>\nclass Box : public Base<T> { };\n",
            "Box",
        );
        assert_eq!(lines[0], "template <typename T, typename Alloc = …>");
        assert_eq!(lines[1], "class Box : Base<T>");
    }

    /// **A non-type parameter and a pack** are the two shapes a fact cannot describe — the syntax is what knows.
    #[test]
    fn a_non_type_parameter_and_a_pack_are_read_from_the_syntax() {
        let lines = rendered(
            "template <int N, char... Digits>\nconstexpr int count();\n",
            "count",
        );
        assert_eq!(lines[0], "template <int N, char... Digits>");
        assert_eq!(lines[1], "constexpr int count()");
    }

    /// **A class says which word introduced it**, because the node's kind is that word.
    #[test]
    fn a_class_says_its_own_keyword() {
        assert_eq!(rendered("struct S { };\n", "S")[0], "struct S");
        assert_eq!(rendered("union U { int a; };\n", "U")[0], "union U");
        assert_eq!(
            rendered("enum class Color { Red };\n", "Color")[0],
            "enum class Color"
        );
    }

    /// **An alias is a `using` line**, whatever the file wrote it as — the target rendered by the type writer.
    #[test]
    fn an_alias_is_a_using_line() {
        assert_eq!(
            rendered("using Size = unsigned long;\n", "Size"),
            ["using Size = unsigned long;"]
        );
        assert_eq!(
            rendered("typedef const char* Name;\n", "Name"),
            ["using Name = const char*;"]
        );
    }

    /// **A variable keeps its qualifiers and its type**, and shows a folded constant's value.
    #[test]
    fn a_variable_keeps_its_qualifiers() {
        assert_eq!(
            rendered("static const Widget* first;\n", "first"),
            ["static const Widget* first"]
        );
    }

    /// **A parameter list is parsed**, not sliced: names are dropped from types, defaults summarised, packs said once.
    #[test]
    fn a_parameter_list_is_parsed() {
        let parameters = parameters("(const std::map<int, int>& table, int n = npos, ...)");
        assert_eq!(
            parameters,
            [
                Parameter {
                    ty: "const std::map<int, int>&".to_string(),
                    name: Some("table".to_string()),
                    default: None,
                    variadic: false,
                },
                Parameter {
                    ty: "int".to_string(),
                    name: Some("n".to_string()),
                    default: Some("npos".to_string()),
                    variadic: false,
                },
                Parameter {
                    ty: String::new(),
                    name: None,
                    default: None,
                    variadic: true,
                },
            ]
        );
    }

    /// **A type is printed by the model**: `T &` is `T&`, `T* const` loses the qualifier that does not name a class,
    /// and the `typename` disambiguator is not part of a type.
    /// **A constrained function's card does not repeat the keyword** — the fact stores the clause as written.
    ///
    /// Measured before: `requires requires (sizeof(T) > 2)` for a simple constraint, and
    /// `requires requires requires { fn(1); }` for a nested requirement — the renderer added `requires ` in front of a
    /// clause that already began with it.
    #[test]
    fn a_constrained_function_does_not_repeat_its_keyword() {
        let lines = rendered(
            "template <typename T>\nvoid f(int x) requires (sizeof(T) > 2) {}\n",
            "f",
        );
        assert_eq!(
            lines,
            [
                "template <typename T>",
                "void f(int x) requires (sizeof(T) > 2)"
            ],
            "one keyword, as the file writes it"
        );

        let lines = rendered(
            "template <typename Fn>\nvoid g(Fn &&fn) requires requires { fn(1); } { fn(1); }\n",
            "g",
        );
        assert_eq!(
            lines,
            [
                "template <typename Fn>",
                "void g(Fn&& fn) requires requires { fn(1); }"
            ],
            "the nested requirement keeps both, and no third"
        );
    }

    #[test]
    fn a_type_is_printed_by_the_model() {
        assert_eq!(
            a_type("std::vector<std::string> &"),
            "std::vector<std::string>&"
        );
        assert_eq!(a_type("const char * const"), "const char*");
        assert_eq!(a_type("typename Alloc::value_type"), "Alloc::value_type");
    }
}
