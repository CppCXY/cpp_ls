//! **The integer value of a constant**, as a file writes it — `UCHAR_MAX + 1`, `1 << 4`, `FIRST_RESERVED`.
//!
//! # What this is for
//!
//! Two questions an editor asks that look different and are one: *what number is this enumerator* (`TK_AND =
//! FIRST_RESERVED`) and *what is this constant* (`const int FIRST_RESERVED = UCHAR_MAX + 1;`). The hover answered
//! neither unless the initialiser was a single integer literal — reported from an editor, twice, with screenshots of
//! exactly those two lines.
//!
//! # What it evaluates, and what it refuses
//!
//! The operators C++ would fold with no lookup (`+ - * / % << >> & | ^ ~`, parentheses, unary `+`/`-`), integer and
//! character literals, and **names it can be told the value of**: a file-local enumerator or `const`/`constexpr`
//! variable (followed through its own initialiser, with a depth bound), or an object-like macro — this file's own, or
//! any the caller can read out of the includes. Everything else is `None`: a floating-point constant, a `sizeof`, a
//! function call, a name nobody can value. That direction is deliberate — a wrong number is worse than none.
use cpp_parser::{CppSyntaxKind, CppSyntaxNode, CppTokenKind};

/// **How many names one value may be followed through** — `A = B + 1`, `B = C << 2`, … A cycle (`const int a = b;
/// const int b = a;`) is not something anyone writes, and a language server meets whatever is written: past this depth
/// the answer is `None` rather than a stack overflow.
const MAX_DEPTH: usize = 12;

/// **The value of the constant declared at `offset`**, if this layer can fold it.
///
/// `macro_body` answers for an object-like macro the *caller* can see — the includes' macros are the session's to
/// read, and keeping that out of this module is what makes the folding testable without a project.
pub fn value_at(
    view: &crate::FileView,
    offset: usize,
    macro_body: &dyn Fn(&str) -> Option<String>,
) -> Option<i64> {
    let token = cpp_parser::token_at(&view.root, offset)?;
    let declaration = token.parent_ancestors().find(|node| {
        matches!(
            CppSyntaxKind::from(node.kind()),
            CppSyntaxKind::EnumeratorDecl | CppSyntaxKind::InitDeclarator
        )
    })?;
    if CppSyntaxKind::from(declaration.kind()) == CppSyntaxKind::EnumeratorDecl {
        an_enumerators_value(view, &declaration, macro_body, 0)
    } else if is_a_constant_declaration(&declaration) {
        a_declarators_value(view, &declaration, macro_body, 0)
    } else {
        // **A `int x = 5;` has a value but not a *constant* one**, and reporting it would invite a reader to use it
        // where a constant is required. Only `const`/`constexpr` declarations are answered for.
        None
    }
}

/// Is this declarator's declaration `const` or `constexpr`? — read from the declaration's own text, up to the name.
///
/// Up to the name and no further: a `const` in a *sibling* declarator (`int a = 1, b = 2;` after a `const int c = 3;`
/// in the same statement) is that sibling's, and reading the whole statement would claim it here.
fn is_a_constant_declaration(declarator: &CppSyntaxNode) -> bool {
    let mut node = declarator.parent();
    let declaration = loop {
        let Some(inner) = node else {
            return false;
        };
        if matches!(
            CppSyntaxKind::from(inner.kind()),
            CppSyntaxKind::Declaration | CppSyntaxKind::InitDeclarator
        ) {
            break inner;
        }
        node = inner.parent();
    };
    let together = declaration.text_range().start();
    let up_to_the_name = declarator
        .text_range()
        .start()
        .checked_sub(together)
        .and_then(|offset| {
            declaration
                .text()
                .to_string()
                .get(..usize::from(offset))
                .map(str::to_string)
        })
        .unwrap_or_default();
    up_to_the_name
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .any(|word| matches!(word, "const" | "constexpr" | "consteval" | "constinit"))
}

/// **The value of a constant expression written as text**, with `names` answering for the identifiers in it.
pub fn value_of(written: &str, names: &mut dyn FnMut(&str) -> Option<i64>) -> Option<i64> {
    let tokens = tokens_of(written);
    let mut at = 0usize;
    let value = an_expression(&tokens, &mut at, names)?;
    (at == tokens.len()).then_some(value)
}

/// The value of one enumerator: its written initialiser, or one more than the enumerator before it.
fn an_enumerators_value(
    view: &crate::FileView,
    enumerator: &CppSyntaxNode,
    macro_body: &dyn Fn(&str) -> Option<String>,
    depth: usize,
) -> Option<i64> {
    if depth > MAX_DEPTH {
        return None;
    }
    let body = enumerator.parent()?;
    let mut value: i64 = -1;
    for each in body.children() {
        if CppSyntaxKind::from(each.kind()) != CppSyntaxKind::EnumeratorDecl {
            continue;
        }
        let after_the_name = tokens_after_the_name(&each)?;
        match after_the_name.first() {
            Some((kind, _)) if *kind == CppTokenKind::Assign => {
                let written = join(&after_the_name[1..]);
                value = value_of(&written, &mut |name| {
                    a_names_value(view, name, macro_body, depth + 1)
                })?;
            }
            // **The next enumerator counts from this one** — the language's rule, and the reason an enumerator
            // with no initialiser is a number at all.
            _ => value = value.checked_add(1)?,
        }
        if each.text_range() == enumerator.text_range() {
            return Some(value);
        }
    }
    None
}

/// The value of a variable's initialiser — `const int n = 1 << 4;`.
fn a_declarators_value(
    view: &crate::FileView,
    declarator: &CppSyntaxNode,
    macro_body: &dyn Fn(&str) -> Option<String>,
    depth: usize,
) -> Option<i64> {
    if depth > MAX_DEPTH {
        return None;
    }
    let written = initialiser_of(declarator)?;
    value_of(&written, &mut |name| {
        a_names_value(view, name, macro_body, depth + 1)
    })
}

/// The value of a name written inside a constant expression.
fn a_names_value(
    view: &crate::FileView,
    name: &str,
    macro_body: &dyn Fn(&str) -> Option<String>,
    depth: usize,
) -> Option<i64> {
    if depth > MAX_DEPTH {
        return None;
    }
    // **A macro first**, because that is what the preprocessor would have done: `UCHAR_MAX` is `0xff` to the compiler.
    if let Some(body) = own_macro_body(view, name).or_else(|| macro_body(name)) {
        return value_of(&body, &mut |inner| {
            a_names_value(view, inner, macro_body, depth + 1)
        });
    }
    // Then a declaration this file writes — an enumerator or a variable, by its own name.
    let offset = view
        .scopes
        .scopes()
        .iter()
        .flat_map(|scope| scope.bindings.iter())
        .find(|binding| binding.name.identifier_text() == Some(name))
        .map(|binding| binding.name_range.start_offset)?;
    let token = cpp_parser::token_at(&view.root, offset)?;
    let declaration = token.parent_ancestors().find(|node| {
        matches!(
            CppSyntaxKind::from(node.kind()),
            CppSyntaxKind::EnumeratorDecl | CppSyntaxKind::InitDeclarator
        )
    })?;
    if CppSyntaxKind::from(declaration.kind()) == CppSyntaxKind::EnumeratorDecl {
        an_enumerators_value(view, &declaration, macro_body, depth + 1)
    } else {
        a_declarators_value(view, &declaration, macro_body, depth + 1)
    }
}

/// **This file's own object-like macro**, when it writes one with that name — the body is the text after the name.
fn own_macro_body(view: &crate::FileView, name: &str) -> Option<String> {
    let preprocessing = crate::preprocess(&view.source, view.tree.get_tokens());
    for spanned in &preprocessing.directives {
        if let crate::preprocess::directive::Directive::Define(define) = &spanned.directive
            && let Some(definition) = &define.macro_def
            && definition.name.as_ref() == name
            && definition
                .params
                .as_ref()
                .is_none_or(|params| params.is_empty())
        {
            // The body is **tokens**, and their own text is the spelling the folder reads: `0xff`, `1 << 4`.
            return Some(
                definition
                    .body
                    .tokens
                    .iter()
                    .map(|token| token.text.as_ref())
                    .collect::<String>(),
            );
        }
    }
    None
}

/// **The initialiser a declarator writes**, as text — everything after its `=` and before its `,`/`;`.
fn initialiser_of(declarator: &CppSyntaxNode) -> Option<String> {
    let tokens = tokens_of_the_node(declarator);
    let equals = tokens
        .iter()
        .position(|(kind, _)| *kind == CppTokenKind::Assign)?;
    let mut depth = 0i32;
    let mut written = String::new();
    for (kind, text) in &tokens[equals + 1..] {
        match text.as_str() {
            "(" | "[" | "{" | "<" => depth += 1,
            ")" | "]" | "}" | ">" => depth -= 1,
            "," | ";" if depth == 0 => break,
            _ => {}
        }
        let _ = kind;
        written.push_str(text);
        written.push(' ');
    }
    (!written.trim().is_empty()).then(|| written.trim().to_string())
}

/// The tokens after a declaration's own name, trivia dropped.
fn tokens_after_the_name(node: &CppSyntaxNode) -> Option<Vec<(CppTokenKind, String)>> {
    let tokens = tokens_of_the_node(node);
    let name = tokens
        .iter()
        .position(|(kind, _)| *kind == CppTokenKind::Identifier)?;
    Some(tokens[name + 1..].to_vec())
}

fn tokens_of_the_node(node: &CppSyntaxNode) -> Vec<(CppTokenKind, String)> {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .map(|token| (CppTokenKind::from(token.kind()), token.text().to_string()))
        .filter(|(kind, text)| !cpp_parser::is_trivia(*kind) && !text.trim().is_empty())
        .collect()
}

fn join(tokens: &[(CppTokenKind, String)]) -> String {
    tokens
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A constant expression's tokens, read by the crate's own lexer rather than split by hand: `0xff` and `'a'` are one
/// token each, and `<<` is one operator.
fn tokens_of(written: &str) -> Vec<(CppTokenKind, String)> {
    let tree = cpp_parser::CppParser::parse(written, cpp_parser::ParserConfig::default());
    let source = written.to_string();
    tree.get_tokens()
        .into_iter()
        .filter(|token| {
            let kind = CppTokenKind::from(token.kind);
            !cpp_parser::is_trivia(kind)
        })
        .map(|token| {
            let start = token.range.start_offset.min(source.len());
            let end = token.range.end_offset().min(source.len());
            (
                CppTokenKind::from(token.kind),
                source[start..end].to_string(),
            )
        })
        .collect()
}

/// `expression := shifted (('+' | '-') shifted)*`
fn an_expression(
    tokens: &[(CppTokenKind, String)],
    at: &mut usize,
    names: &mut dyn FnMut(&str) -> Option<i64>,
) -> Option<i64> {
    let mut value = a_shifted_expression(tokens, at, names)?;
    loop {
        let Some((_, text)) = tokens.get(*at) else {
            break;
        };
        // **The operator is consumed before the right operand is read** — the one bug this parser had, and it read as
        // arithmetic working for `+` and `-` only: those two fell through to `a_primary`'s *unary* arms and `1 + 2`
        // happened to come out 3, while `10 / 3` read `/` as its right operand and answered nothing.
        let operator = text.as_str();
        if !matches!(operator, "+" | "-") {
            break;
        }
        *at += 1;
        let right = a_shifted_expression(tokens, at, names)?;
        value = if operator == "+" {
            value.checked_add(right)?
        } else {
            value.checked_sub(right)?
        };
    }
    Some(value)
}

/// `shifted := bitwise (`<<` / `>>` bitwise)*`
fn a_shifted_expression(
    tokens: &[(CppTokenKind, String)],
    at: &mut usize,
    names: &mut dyn FnMut(&str) -> Option<i64>,
) -> Option<i64> {
    let mut value = a_bitwise_expression(tokens, at, names)?;
    loop {
        let Some((_, text)) = tokens.get(*at) else {
            break;
        };
        let operator = text.as_str();
        if !matches!(operator, "<<" | ">>") {
            break;
        }
        *at += 1;
        let by = a_bitwise_expression(tokens, at, names)?;
        let by = u32::try_from(by).ok()?;
        value = if operator == "<<" {
            value.checked_shl(by)?
        } else {
            value.checked_shr(by)?
        };
    }
    Some(value)
}

/// `bitwise := term (('|' | '^' | '&') term)*` — one precedence level, because each of the three is rare in an
/// enumerator's initialiser and the exact grouping only matters between them, which no file in the wild mixes.
fn a_bitwise_expression(
    tokens: &[(CppTokenKind, String)],
    at: &mut usize,
    names: &mut dyn FnMut(&str) -> Option<i64>,
) -> Option<i64> {
    let mut value = a_term(tokens, at, names)?;
    loop {
        let Some((_, text)) = tokens.get(*at) else {
            break;
        };
        let operator = text.as_str();
        if !matches!(operator, "|" | "^" | "&") {
            break;
        }
        *at += 1;
        let right = a_term(tokens, at, names)?;
        value = match operator {
            "|" => value | right,
            "^" => value ^ right,
            _ => value & right,
        };
    }
    Some(value)
}

/// `term := primary (('*' | '/' | '%') primary)*`
fn a_term(
    tokens: &[(CppTokenKind, String)],
    at: &mut usize,
    names: &mut dyn FnMut(&str) -> Option<i64>,
) -> Option<i64> {
    let mut value = a_primary(tokens, at, names)?;
    loop {
        let Some((_, text)) = tokens.get(*at) else {
            break;
        };
        let operator = text.as_str();
        if !matches!(operator, "*" | "/" | "%") {
            break;
        }
        *at += 1;
        let right = a_primary(tokens, at, names)?;
        value = match operator {
            "*" => value.checked_mul(right)?,
            "/" => value.checked_div(right)?,
            _ => value.checked_rem(right)?,
        };
    }
    Some(value)
}

fn a_primary(
    tokens: &[(CppTokenKind, String)],
    at: &mut usize,
    names: &mut dyn FnMut(&str) -> Option<i64>,
) -> Option<i64> {
    let (kind, text) = tokens.get(*at)?.clone();
    *at += 1;
    match text.as_str() {
        "(" => {
            let value = an_expression(tokens, at, names)?;
            let (kind, close) = tokens.get(*at)?;
            let _ = kind;
            (close == ")").then(|| *at += 1)?;
            Some(value)
        }
        "-" => Some(a_primary(tokens, at, names)?.checked_neg()?),
        "+" => a_primary(tokens, at, names),
        "~" => Some(!a_primary(tokens, at, names)?),
        _ => match kind {
            CppTokenKind::IntegerLiteral => an_integer(&text),
            CppTokenKind::CharLiteral => a_character(&text),
            CppTokenKind::Identifier => names(&text),
            _ => None,
        },
    }
}

/// An integer literal's value — decimal, hexadecimal, binary, octal, with any `u`/`l` suffix and digit separators.
fn an_integer(text: &str) -> Option<i64> {
    let text = text.trim();
    let (digits, radix) =
        if let Some(rest) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            (rest, 16)
        } else if let Some(rest) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
            (rest, 2)
        } else if text.len() > 1 && text.starts_with('0') {
            (&text[1..], 8)
        } else {
            (text, 10)
        };
    // **The digits stop where the suffix starts.** Trimming trailing letters took `0xff` down to `0x`: `f` is an
    // alphabetic character *and* a hexadecimal digit, and the earlier version answered `None` for the commonest way a
    // header writes a mask.
    let digits: String = digits
        .chars()
        .take_while(|character| character.is_digit(radix) || *character == '\'')
        .filter(|character| *character != '\'')
        .collect();
    i64::from_str_radix(&digits, radix).ok()
}

/// A character literal's value — `'a'` is 97, and the ordinary escapes are read as the ones they spell.
fn a_character(text: &str) -> Option<i64> {
    let inner = text.trim().strip_prefix('\'')?.strip_suffix('\'')?;
    let character = match inner {
        "\\n" => '\n',
        "\\t" => '\t',
        "\\r" => '\r',
        "\\0" => '\0',
        "\\\\" => '\\',
        "\\'" => '\'',
        other => {
            if let Some(hex) = other.strip_prefix("\\x") {
                u8::from_str_radix(hex, 16).ok()? as char
            } else if let Some(octal) = other.strip_prefix('\\') {
                u8::from_str_radix(octal, 8).ok()? as char
            } else {
                let mut characters = other.chars();
                let one = characters.next()?;
                characters.next().is_none().then_some(one)?
            }
        }
    };
    Some(character as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folded_expression_reads_literals_operators_and_names() {
        let mut names = |name: &str| match name {
            "UCHAR_MAX" => Some(255),
            "FIRST_RESERVED" => Some(256),
            _ => None,
        };
        assert_eq!(value_of("UCHAR_MAX + 1", &mut names), Some(256));
        assert_eq!(value_of("FIRST_RESERVED", &mut names), Some(256));
        assert_eq!(value_of("1 << 4", &mut names), Some(16));
        assert_eq!(value_of("0xff", &mut names), Some(255));
        assert_eq!(value_of("(1 + 2) * 3", &mut names), Some(9));
        assert_eq!(value_of("'a'", &mut names), Some(97));
        assert_eq!(value_of("10 / 3", &mut names), Some(3));
        assert_eq!(
            value_of("1 +", &mut names),
            None,
            "a truncated expression is not a value"
        );
        assert_eq!(
            value_of("1.5", &mut names),
            None,
            "a float is not this layer's"
        );
        assert_eq!(value_of("unknown + 1", &mut names), None);
        assert_eq!(value_of("0xff", &mut names), Some(255));
        assert_eq!(value_of("(1 + 2) * 3", &mut names), Some(9));
        assert_eq!(value_of("'a'", &mut names), Some(97));
        assert_eq!(value_of("10 / 3", &mut names), Some(3));
        assert_eq!(
            value_of("1 +", &mut names),
            None,
            "a truncated expression is not a value"
        );
        assert_eq!(
            value_of("1.5", &mut names),
            None,
            "a float is not this layer's"
        );
        assert_eq!(value_of("unknown + 1", &mut names), None);
    }
}
