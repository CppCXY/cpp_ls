//! Inlay hints: what the file does not write and a reader still needs.
//!
//! # What can be hinted, and what cannot
//!
//! An inlay hint is a piece of text the editor draws *in* the code — not part of the file, and not editable by
//! accident — so the bar for one is higher than for a popup: it sits in the middle of the line the user is
//! reading. Two hints are worth that, and only one of them is answerable here.
//!
//! ```text
//! a parameter's name    `scale(3, 0.5)`          →  `scale(count: 3, factor: 0.5)`     answerable: the
//!                                                                                       callee's declaration
//!                                                                                       writes the names
//! a declaration's type  `auto n = count();`      →  `auto n: int = count();`           not answerable: the
//!                       `for (auto& x : v)`       →  `for (auto& x : std::vector<..>)`  analysis has no type
//!                                                                                       system — nothing
//!                                                                                       deduces `auto`, and
//!                                                                                       `vector<int>` is not
//!                                                                                       an instantiated type
//! ```
//!
//! So this module produces parameter hints and nothing else, and the type column is not a gap to be filled in later
//! by guessing: a type hint that was wrong would be a lie printed into the user's code.
//!
//! # What makes a hint *useful* rather than noise
//!
//! Every argument of every call in the requested range could carry a name, which is what makes an editor's screen
//! unreadable. Three refusals keep the answer to the ones a reader cannot already see:
//!
//! * **the argument already says it** — `scale(count, factor)` needs no `count:` in front of `count`;
//! * **the callee is not a function this analysis can name** — a function pointer, a template parameter, an
//!   overload set nothing resolves: a hint would be a guess about which parameter list applies;
//! * **the position has no parameter** — a variadic tail, an argument past the declared parameters, an unnamed
//!   parameter. Hints are placed **by position**, so a parameter that has no name breaks the alignment for
//!   everything after it; see `crate::sema::scopes::parameters_of`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cpp_parser::{CppSyntaxKind, CppSyntaxNode, SourceRange};

use crate::index::project::{Callee, callee_of_a_call};
use crate::sema::scopes::parameters_of;
use crate::{FileView, Known, ProjectIndex};

/// One parameter name, at the argument it belongs to.
///
/// The label is the *caller's* business — this protocol-less layer says which parameter a position is in, and the
/// LSP layer decides that it is drawn as `count:` with a space after it. A hint carrying a rendering would put a
/// wire format's punctuation in the crate that has no wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterHint {
    /// The argument's own start, in the file the request is about.
    pub offset: usize,
    /// The parameter's name, as the callee's declaration spells it.
    pub name: String,
}

/// **The parameter names for the calls in `range`** — what an editor draws as inlay hints.
///
/// `range` is the visible range a client asked about, in the file's own coordinates. Hints are placed at the
/// arguments themselves, and an argument is hinted only when it is inside the range: a hint outside the visible
/// area is work the client throws away.
///
/// # The files this reads
///
/// A call's parameter names are in the **callee's** file, which is usually not the file the cursor is in: `f(1)`
/// in a `.cpp` is declared in a header. The names come from the declaration's *scope* rather than from a text
/// reading of its parameter list, so the declaring file has to be parsed — and `view_of` is how, with the file
/// being edited already in hand so that a same-file call costs nothing. Each declaring file is parsed **once per
/// request** however many calls reach it, which is the whole reason for the map below.
pub fn parameter_hints<F>(
    index: &ProjectIndex,
    view: &FileView,
    range: SourceRange,
    mut view_of: F,
) -> Vec<ParameterHint>
where
    F: FnMut(&Path) -> Option<FileView>,
{
    let mut hints = Vec::new();

    // Declaring files, parsed once each: a file with twenty calls into it is parsed once, not twenty times.
    let mut declaring: HashMap<PathBuf, FileView> = HashMap::new();

    for call in call_expressions(&view.root, range) {
        let Known::Yes(callee) = callee_of_a_call(index, &view.scopes, &view.root, &view.path, &call)
        else {
            // No declaration, or none this layer can place: the parameter list is unknown, and a hint naming the
            // wrong parameter is a wrong answer printed into the code.
            continue;
        };

        let Some(names) = parameter_names_for(view, &callee, &mut declaring, &mut view_of) else {
            continue;
        };

        let Some(arguments) = arguments_of(&call) else {
            continue;
        };

        for (argument, name) in arguments.iter().zip(names.iter()) {
            // A parameter with no name breaks the alignment for every parameter *after* it rather than shifting
            // them up: see `parameters_of`. Nothing is hinted for a position nothing declares.
            let Some(name) = name else {
                continue;
            };

            if !contains(range, argument.text_range().start().into()) {
                continue;
            }

            // The argument already spells the parameter, so the hint would repeat the line back at the reader.
            if argument.text().to_string().trim() == name {
                continue;
            }

            hints.push(ParameterHint {
                offset: argument.text_range().start().into(),
                name: name.clone(),
            });
        }
    }

    hints
}

/// The parameter names of the declaration a call names, or `None` when they cannot be read.
fn parameter_names_for<F>(
    view: &FileView,
    callee: &Callee,
    declaring: &mut HashMap<PathBuf, FileView>,
    view_of: &mut F,
) -> Option<Vec<Option<String>>>
where
    F: FnMut(&Path) -> Option<FileView>,
{
    // The file being edited is already parsed, and a same-file call is the common case.
    let declared_in = if callee.file == view.path {
        view
    } else {
        if !declaring.contains_key(&callee.file) {
            declaring.insert(callee.file.clone(), view_of(&callee.file)?);
        }

        declaring.get(&callee.file).expect("just inserted")
    };

    let list = parameter_list_of(declared_in, callee.name_offset)?;
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
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declarator)?;

    cpp_parser::first_child_of_kind(&declarator, &[CppSyntaxKind::ParameterList])
}

/// The **text** of that list, as the file writes it — `(_Ty* _First, size_type _Count)`, parentheses included.
///
/// One function for the two readers that want the spelling rather than the nodes: a declaration's fact
/// ([`crate::sema::declarations::parameter_list_at`], which is what a completion shows beside a name) and anything
/// that has to print a signature without walking it.
///
/// # The layout is taken out, and the spelling is not
///
/// A node's text is the *source* text, which is laid out for a reader of the file rather than for a one-line
/// summary: measured on MSVC's `<istream>`, `basic_istream<_Elem, _Traits>&& _Istr` arrives with a `\r\n` and four
/// spaces of indentation in the middle, and a **cooked** reading arrives with the rendering's own separator — one
/// space between every token, so `format_string<_Types...>` is spelled `format_string < _Types ... >`.
///
/// So whitespace runs become one space, and the space inside the brackets this list is made of goes: after `(` and
/// before `)` and `,`. Those three are safe to decide lexically — a parameter list's own parentheses and commas are
/// never comparison operators — while `<`, `>` and `...` are deliberately left alone, because `(bool _B = 1 < 2)`
/// is a parameter list too and a rule that pulled `1 <2` together would be inventing a different expression.
///
/// What is *not* done is the rest of the rendering's spacing (`_Elem * ()`, `_NODISCARD _CONSTEXPR20 size_type`):
/// that is how every spelling in a fact is written, and a parameter list that read differently from the return type
/// beside it would be two conventions in one line.
pub(crate) fn parameter_list_text(declarator: &CppSyntaxNode) -> Option<String> {
    let list = cpp_parser::first_child_of_kind(declarator, &[CppSyntaxKind::ParameterList])?;
    let text = list.text().to_string();

    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;

    for word in text.split_whitespace() {
        // A space is written only where it is not inside the brackets: after `(` and before `)` and `,`.
        let after_an_open = out.ends_with('(') || out.ends_with('[');
        let before_a_close = word.starts_with(')') || word.starts_with(']') || word.starts_with(',');

        if pending_space && !after_an_open && !before_a_close {
            out.push(' ');
        }
        out.push_str(word);
        pending_space = true;
    }

    Some(out)
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

/// Every call in the tree whose own start is inside `range`.
///
/// The **start** decides, not the whole extent: a call that begins above the visible area and ends inside it is
/// still the call the reader is looking at, and one that begins inside and ends below is drawn from its first
/// line. What matters is that the arguments — which is where the hints go — are inside, and each is checked
/// against the range on its own.
fn call_expressions(root: &CppSyntaxNode, range: SourceRange) -> Vec<CppSyntaxNode> {
    root.descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::CallExpr)
        .filter(|node| contains(range, node.text_range().start().into()))
        .collect()
}

/// Is an offset inside a range?
fn contains(range: SourceRange, offset: usize) -> bool {
    offset >= range.start_offset && offset <= range.end_offset()
}
