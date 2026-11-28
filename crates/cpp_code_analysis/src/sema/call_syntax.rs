//! **The syntax of a call and of a declaration's parameter list** — read once, by the four layers that need it.
//!
//! A call's arguments, a declaration's `( … )`, its text as one line: three questions with nothing semantic in them,
//! and every layer above the tree asks at least one of them. The index asks what a call was given (to choose an
//! overload), the declaration reader asks where a function's parameters are, the signature popup asks for the list as
//! one line with the byte spans a client bolds, and the inlay layer asks for the names to put against the arguments.
//!
//! They live here, below all four, because the alternative is what this file replaces: five `pub(crate)` helpers in
//! `inlay` — a *feature* module — imported by the index, by `sema::declarations`, and by the completion layer. A
//! feature module that the index depends on is a dependency pointing the wrong way, and it is invisible while the two
//! share a crate. The questions are about the syntax tree, the tree is `cpp_parser`'s, and this is the one place in
//! this crate that reads it for its shape alone.
//!
//! Nothing here decides anything: each function answers `None` for the shapes a reader cannot place (a macro's
//! arguments, a list with no parameter), and every caller turns that into its own kind of silence.

use cpp_parser::{CppAstNode, CppSyntaxKind, CppSyntaxNode};

use crate::FileView;
use crate::sema::scopes::parameters_of;

/// **The parameter names of a declaration read from its own text**, for a file with no rendering.
///
/// `parameter_list` is what the index stores — `(const char* __format, ...)`, parentheses included — so the names
/// are read by **parsing one synthetic declaration** around it: `void __hint(const char* __format, ...);`. The
/// tree then tells a name from a type, which is the one thing splitting on commas gets wrong: `int (*f)(int)`,
/// `const Widget& w`, a default argument containing a comma (`map<int, string> m = {}`), and `...` all come out
/// right because they are not text to this function.
///
/// `None` for a list with no parameter (a `()`), which is also the answer for a declaration whose fact was written
/// before the field existed.
pub(crate) fn parameter_names_from_text(parameter_list: &str) -> Option<Vec<Option<String>>> {
    let text = format!("void __hint{parameter_list};");
    let tree = cpp_parser::CppParser::parse(&text, cpp_parser::ParserConfig::default());
    let root = tree.get_red_root();

    let declarator = root
        .descendants()
        .find(|node| cpp_parser::CppDeclarator::can_cast(node.kind().into()))?;
    let list = cpp_parser::first_child_of_kind(&declarator, &[CppSyntaxKind::ParameterList])?;

    let names: Vec<Option<String>> = parameters_of(&list)
        .into_iter()
        .map(|(_, declared)| declared.map(|(name, _)| name.text()))
        .collect();

    (!names.is_empty()).then_some(names)
}

/// The declaration's **own** parameter list: the one belonging to the declarator the named at `offset` is a name
/// of.
///
/// The innermost `Declarator` ancestor, and *its* `ParameterList` child — not the nearest parameter list up the
/// tree. The difference is the whole correctness of the reading: a variable declared inside a function body has
/// the enclosing function's parameter list above it, and a function returning a function pointer
/// (`void (*f(int a))(int b)`) has two in the same declaration. The declarator the name is a name *of* is the one
/// whose list says what the name's parameters are, and it is found the way the scope builder finds a declaration's
/// name (`sema::scopes::declared_name`) — the same relationship, read from the other end.
pub(crate) fn parameter_list_of(view: &FileView, offset: usize) -> Option<CppSyntaxNode> {
    let token = cpp_parser::token_at(&view.root, offset)?;

    let declarator = token
        .parent_ancestors()
        .find(|node| cpp_parser::CppDeclarator::can_cast(node.kind().into()))?;

    // **The innermost declarator that answers, then the ones around it.** A name inside a parenthesised declarator
    // (`void (foo) (int a);`) is in a declarator with no list of its own — the list is beside the declarator one
    // level up — so the search rises until one answers. See [`the_param_list_rising`].
    Some(the_param_list_rising(&declarator)?.syntax().clone())
}

/// **The parameter list of the declaration this declarator is part of**, found by rising through the declarators
/// around it.
///
/// The *innermost* declarator that answers wins, which is the relationship [`parameter_list_of`]'s note states: for
/// `void (*f(int a))(int b)` the list that belongs to the name `f` is the innermost one. A **parenthesised**
/// declarator has no list of its own — `void (foo) (int a);` puts it beside the declarator one level up — so the walk
/// rises until one answers, and it cannot borrow a *function's* list for a local variable: a local's declarator has no
/// declarator ancestor above it (the body is a sibling of the declarator, not a parent).
fn the_param_list_rising(declarator: &CppSyntaxNode) -> Option<cpp_parser::CppParamList> {
    let mut current = Some(declarator.clone());
    while let Some(node) = current {
        let typed = cpp_parser::CppDeclarator::cast(node.clone())?;
        // **The list first, then the guard**: `int *f(int a)` is a function returning a pointer, and its list is a
        // direct child of the declarator that carries the `*` — asking the pointer question first would refuse every
        // function whose return type ends in `*` or `&`.
        if let Some(list) = typed.get_param_list() {
            return Some(list);
        }
        // **A declarator with a `*` or `&` inside it is not a function's own name.** `void (*cb)(int a)` is a
        // *function-pointer type*: the list beside it belongs to the pointed-to type, not to `cb`, so the rise stops
        // here. Measured: without this, `cb(1)` was hinted with the type's own `a` — the test
        // `a_call_through_a_function_pointer_is_not_hinted` is exactly that shape.
        if typed.is_a_pointer() || typed.is_a_reference() {
            return None;
        }
        current = node
            .parent()
            .filter(|parent| cpp_parser::CppDeclarator::can_cast(parent.kind().into()));
    }
    None
}

/// The **text** of that list, as the file writes it — `(_Ty* _First, size_type _Count)`, parentheses included.
///
/// One function for the two readers that want the spelling rather than the nodes: a declaration's fact
/// ([`crate::sema::declarations::parameter_list_at`], which is what a completion shows beside a name) and anything
/// that has to print a signature without walking it.
pub(crate) fn parameter_list_text(declarator: &CppSyntaxNode) -> Option<String> {
    // **The declarator's reading, rising through the declarators around it** — the direct-child search agrees for
    // every ordinary function and finds nothing for a parenthesised declarator, whose list is a *sibling*:
    // `LUA_API void (lua_setfield) (lua_State *L, …)` parses as `InitDeclarator (Declarator "(lua_setfield)",
    // ParameterList …)`. Measured before this: `lua_setfield` was a `Function` fact with no `parameter_list`, so its
    // card printed `lua_setfield(…)` and no inlay hint was placed — Lua writes its whole public API that way.
    let list = the_param_list_rising(declarator)?;

    Some(tidy_list_text(&list.syntax().text().to_string()).0)
}

/// **A list as one line, and where every byte of the original went.**
///
/// # The layout is taken out, and the spelling is not
///
/// A node's text is the *source* text, which is laid out for a reader of the file rather than for a one-line
/// summary: measured on MSVC's `<istream>`, `basic_istream<_Elem, _Traits>&& _Istr` arrives with a `\r\n` and four
/// spaces of indentation in the middle, and MSVC's `<xstring>` wraps three of `basic_string::append`'s nine
/// overloads the same way. A **cooked** reading arrives with the rendering's own separator instead — one space
/// between every token, so `format_string<_Types...>` is spelled `format_string < _Types ... >`.
///
/// So whitespace runs become one space, and the space inside the brackets this list is made of goes: after `(` and
/// before `)` and `,`. Those three are safe to decide lexically — a parameter list's own parentheses and commas are
/// never comparison operators — while `<`, `>` and `...` are deliberately left alone, because `(bool _B = 1 < 2)`
/// is a parameter list too and a rule that pulled `1 <2` together would be inventing a different expression.
///
/// What is *not* done is the rest of the rendering's spacing (`_Elem * ()`, `_NODISCARD _CONSTEXPR20 size_type`):
/// that is how every spelling in a fact is written, and a parameter list that read differently from the return type
/// beside it would be two conventions in one line.
///
/// # Why the map comes back with it
///
/// Because the other reader is a **signature**, whose parameters are byte spans *inside* the label it builds
/// ([`crate::signature::CallSignature::parameters`]) — a client bolds them. Tidying the text without moving the
/// spans would put the highlight on the wrong bytes, which is worse than the newline it removed. `map[i]` is where
/// byte `i` of `text` ended up, with one entry per byte plus one for the end, so a span `a..b` becomes
/// `map[a]..map[b]` whatever the two ends are.
pub(crate) fn tidy_list_text(text: &str) -> (String, Vec<usize>) {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut map = vec![0usize; text.len() + 1];
    let mut wrote_a_word = false;
    let mut at = 0usize;

    while at < bytes.len() {
        // **A comment is not part of the list.** A summary's parameter list is the *file's* text, and a declaration
        // written across lines with a comment beside each parameter keeps those comments in it:
        //
        // ```c
        // static Decimal *decimal_new(
        //   sqlite3_context *pCtx,       /* Report error here, if not null */
        //   sqlite3_value *pIn,          /* Construct the decimal object from this */
        // ```
        //
        // `parameters_in` then split on the comma *inside* the comment, and the spelling of one parameter became
        // "`if not null */ sqlite3_value *`" — measured on sqlite, six `param-type-check` findings about a type no
        // file writes. The bytes are dropped from the text and land on the position of the next word, so a span
        // inside a comment maps into the word that follows it rather than into nothing.
        if bytes[at] == b'/'
            && at + 1 < bytes.len()
            && (bytes[at + 1] == b'*' || bytes[at + 1] == b'/')
        {
            let line_comment = bytes[at + 1] == b'/';
            let mut end = at + 2;
            while end < bytes.len() {
                if line_comment && bytes[end] == b'\n' {
                    break;
                }
                if !line_comment
                    && bytes[end] == b'*'
                    && end + 1 < bytes.len()
                    && bytes[end + 1] == b'/'
                {
                    end += 2;
                    break;
                }
                end += 1;
            }
            for slot in &mut map[at..end.min(bytes.len())] {
                *slot = out.len();
            }
            at = end;
            continue;
        }

        if bytes[at].is_ascii_whitespace() {
            // Whitespace lands where the next word will start, before the separator is decided: no span begins or
            // ends in it — a span is a whole parameter, and each is trimmed.
            map[at] = out.len();
            at += 1;
            continue;
        }

        let mut end = at;
        while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
            end += 1;
        }
        let word = &text[at..end];

        if wrote_a_word {
            let after_an_open = out.ends_with('(') || out.ends_with('[');
            let before_a_close =
                word.starts_with(')') || word.starts_with(']') || word.starts_with(',');
            if !after_an_open && !before_a_close {
                out.push(' ');
            }
        }

        for (offset, slot) in map[at..end].iter_mut().enumerate() {
            *slot = out.len() + offset;
        }
        out.push_str(word);
        wrote_a_word = true;
        at = end;
    }

    map[text.len()] = out.len();
    (out, map)
}

/// The argument expressions of a call, in order.
///
/// The call's children **after the callee**, which is the shape the grammar keeps for every call it can read —
/// `f(1)`, `ns::g(1)`, `f<int>(2)`, `(f)(3)`, `f(4)(5)`, `W{}.go(6)`: the template arguments and the qualifier are
/// inside the callee's own node, so the arguments are siblings of it and the commas between them are tokens rather
/// than children.
///
/// `None` for the one shape that is not a list of expressions: a call whose arguments could not be read — a
/// macro's arguments, which the parser keeps as a balanced token group in an `ArgumentList` child. There is no
/// expression there to put a name against, and a hint on a raw token would be a guess about where an argument
/// begins.
pub(crate) fn arguments_of(call: &CppSyntaxNode) -> Option<Vec<CppSyntaxNode>> {
    let mut children = call.children();
    children.next()?; // the callee

    let arguments: Vec<CppSyntaxNode> = children.collect();

    let unread = arguments
        .iter()
        .any(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ArgumentList);

    (!arguments.is_empty() && !unread).then_some(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A comment between two parameters is not part of the list.** A summary`s parameter list is the file`s own
    /// text, and sqlite writes one declaration per parameter with a comment beside each:
    ///
    /// ```c
    /// static Decimal *decimal_new(
    ///   sqlite3_context *pCtx,       /* Report error here, if not null */
    ///   sqlite3_value *pIn,          /* Construct the decimal object from this */
    /// ```
    ///
    /// The comma inside the first comment split the list, and a parameter`s spelling became
    /// `if not null */ sqlite3_value *` — measured on sqlite, six `param-type-check` findings about a type no file
    /// writes.
    #[test]
    fn a_comment_inside_a_parameter_list_is_dropped() {
        let (text, _map) = tidy_list_text(
            "(sqlite3_context *pCtx,       /* Report error here, if not null */\n  sqlite3_value *pIn)",
        );
        assert_eq!(text, "(sqlite3_context *pCtx, sqlite3_value *pIn)");
        assert!(
            !text.contains("*/") && !text.contains("if not null"),
            "no comment text survives: {text}"
        );
    }

    /// A line comment ends at the newline, and the parameter after it is still one word.
    #[test]
    fn a_line_comment_ends_at_the_newline() {
        let (text, _map) = tidy_list_text("(int a, // the first\n int b)");
        assert_eq!(text, "(int a, int b)");
    }
}
