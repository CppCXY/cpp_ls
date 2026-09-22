//! A `#define`: the macro's name, its parameters, and its replacement tokens.
//!
//! # Why the body is stored as tokens
//!
//! A macro is text substitution, so storing the body as a `String` and searching it looks simpler —
//! and it cannot work. Expansion has to answer three questions that a string has already thrown the
//! answers to:
//!
//! * **`#x`** — is this `#` the stringize operator, or a `#` that was written in the body? The
//!   operator is the one followed by a *parameter*.
//! * **`a ## b`** — pasting is defined on tokens, not characters: `a ## b` where `a` ends in `+` and
//!   `b` is `=` produces `+=`, one token, which then has to be re-lexed. A string would have to find
//!   the boundary again, and the boundary is not in the text.
//! * **parameter substitution** — a parameter is replaced by the *tokens* of an argument, so
//!   `#define TWICE(x) x x` with `TWICE(a, b)` must not split the argument on the comma.
//!
//! So the body is a `Vec<Token>`, with the two operators and the parameters identifiable in it.

use cpp_parser::{CppTokenKind, SourceRange};

use crate::token::{Token, is_trivia};

/// One declared parameter of a function-like macro.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Parameter {
    pub name: Box<str>,
    pub kind: ParameterKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParameterKind {
    /// An ordinary parameter: `#define F(a) ...`
    Ordinary,
    /// The `...` of `#define F(a, ...) ...`, which is spelled `__VA_ARGS__` in the body.
    ///
    /// Attached to the parameter it follows when the syntax is `#define F(a...)` — a GNU extension
    /// that spells the same thing. Both are represented here as a variadic parameter, because the
    /// difference is spelling and not meaning.
    Variadic,
}

/// The replacement list of a macro, in the form expansion needs it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MacroBody {
    /// Every token of the body, in order, **with** whitespace and **without** comments or line
    /// splices.
    ///
    /// Whitespace is kept because it is load-bearing in exactly one place: it separates two tokens
    /// that would otherwise lex as one. `#define F(x) + x` and `#define F(x) +x` expand to different
    /// token sequences if the argument is empty, and a stringize of the body is sensitive to it too.
    /// Comments and line splices are dropped because translation phase 3 has already removed them by
    /// the time any of this matters — a comment in a macro body is a space, and a splice is nothing.
    pub tokens: Vec<Token>,

    /// Positions in `tokens` of `#` operators that apply to a parameter.
    ///
    /// Only these stringize. A lone `#` in a body that is not followed by a parameter is kept as the
    /// token it is, which matters because `#` is legal in a body and means nothing there.
    pub stringize: Vec<usize>,

    /// Positions in `tokens` of `##` operators.
    pub paste: Vec<usize>,
}

impl MacroBody {
    /// Is the body empty? `#define FOO` defines a macro that expands to nothing, which is legal and
    /// used for feature flags.
    pub fn is_empty(&self) -> bool {
        self.tokens
            .iter()
            .all(|token| is_trivia(token.kind) || token.kind == CppTokenKind::None)
    }

    /// The body's tokens with whitespace removed, for callers that want the spelling.
    pub fn significant(&self) -> impl Iterator<Item = &Token> {
        self.tokens.iter().filter(|token| !is_trivia(token.kind))
    }
}

/// A macro definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroDef {
    pub name: Box<str>,
    /// `None` for an object-like macro, `Some` for a function-like one.
    ///
    /// The distinction is decided by the *absence of whitespace* before the `(`, which is why this is
    /// an `Option` and not an empty `Vec`: `#define A (1)` defines `A` to be `(1)`, while
    /// `#define A(x) x` defines a function-like macro. Both spell a parenthesis after the name.
    pub params: Option<Vec<Parameter>>,

    pub body: MacroBody,

    /// The definition's range: the whole `#define` directive.
    ///
    /// What "reveal this macro" opens the file at.
    pub range: SourceRange,

    /// Where the macro's **name** was written inside that directive.
    ///
    /// Recorded here rather than derived later, because deriving it means searching the directive's text
    /// for the name — and the head's spelling varies (`#  define  NAME`, `#define\tNAME`), so a search is
    /// a second implementation of the same rule, free to disagree with the one that assigned the name.
    /// A go-to-definition puts the cursor here, not at the `#`.
    pub name_range: SourceRange,
}

impl MacroDef {
    pub fn is_function_like(&self) -> bool {
        self.params.is_some()
    }

    pub fn is_variadic(&self) -> bool {
        self.params
            .as_ref()
            .is_some_and(|params| params.iter().any(|p| p.kind == ParameterKind::Variadic))
    }

    /// How many arguments a call has to supply.
    ///
    /// A variadic macro takes at least one fewer than its parameter count, and accepts any number
    /// beyond that — so this is a lower bound, not an exact count.
    pub fn min_arguments(&self) -> usize {
        match &self.params {
            None => 0,
            Some(params) => params
                .iter()
                .filter(|p| p.kind == ParameterKind::Ordinary)
                .count(),
        }
    }

    /// Is `args` a plausible argument count for this macro?
    ///
    /// Used to decline expansion rather than expand wrongly: an arity mismatch means the tokens are
    /// not a call to this macro at all — a variable happens to share its name, or the file is
    /// mid-edit — and expanding anyway would invent code that is not there.
    pub fn accepts_argument_count(&self, count: usize) -> bool {
        match &self.params {
            None => false,
            Some(params) => {
                if self.is_variadic() {
                    count >= self.min_arguments()
                } else {
                    count == params.len()
                }
            }
        }
    }
}

/// A binding, and where it took effect.
#[derive(Debug, Clone)]
struct Binding {
    name: Box<str>,
    /// `None` is an explicit `#undef`, which shadows an earlier definition rather than deleting it.
    definition: Option<MacroDef>,
    /// The offset the directive was written at, so a query can ask about a position.
    at: usize,
}

/// The macros visible at one point in a file.
///
/// A `#define` is never *removed* when it is redefined or `#undef`ed — the earlier binding stays, and
/// the later one shadows it. That is what makes the table usable at an arbitrary offset: "what did
/// `FOO` mean here?" is a question about position, and a map holding only the latest definition could
/// not answer it.
///
/// Nothing here crosses a file boundary yet — turning this into a chain that follows `#include` is the
/// phase in which the include graph exists. For now a table is one file's own directives.
#[derive(Debug, Clone, Default)]
pub struct MacroTable {
    bindings: Vec<Binding>,
}

impl MacroTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a definition, effective from the offset it was written at.
    pub fn define(&mut self, definition: MacroDef) {
        self.bindings.push(Binding {
            at: definition.range.start_offset,
            name: definition.name.clone(),
            definition: Some(definition),
        });
    }

    /// Record an `#undef`, effective from the offset it was written at.
    pub fn undefine(&mut self, name: &str, at: usize) {
        self.bindings.push(Binding {
            name: name.into(),
            definition: None,
            at,
        });
    }

    /// The binding in force at `offset`, or at the end of the file when `offset` is `None`.
    fn binding_at(&self, name: &str, offset: Option<usize>) -> Option<&Binding> {
        self.bindings.iter().rfind(|binding| {
            &*binding.name == name && offset.is_none_or(|offset| binding.at <= offset)
        })
    }

    /// What `name` means at the end of the file.
    pub fn get(&self, name: &str) -> Option<&MacroDef> {
        self.binding_at(name, None)?.definition.as_ref()
    }

    /// What `name` means at `offset`.
    pub fn get_at(&self, name: &str, offset: usize) -> Option<&MacroDef> {
        self.binding_at(name, Some(offset))?.definition.as_ref()
    }

    /// Is `name` defined at the end of the file?
    ///
    /// This is what `#ifdef` asks, and the answer for a name that was never mentioned is `false` —
    /// which is why this is not an error and never will be.
    pub fn is_defined(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn is_defined_at(&self, name: &str, offset: usize) -> bool {
        self.get_at(name, offset).is_some()
    }

    /// Every macro defined at the end of the file, latest binding of each name first.
    pub fn iter(&self) -> impl Iterator<Item = &MacroDef> {
        self.bindings
            .iter()
            .rev()
            .filter_map(|binding| binding.definition.as_ref())
    }

    /// The names currently defined, sorted, with each name appearing once.
    ///
    /// For completion, where a name that was `#undef`ed must not appear and a redefined one must not
    /// appear twice.
    pub fn defined_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .bindings
            .iter()
            .rev()
            .filter(|binding| binding.definition.is_some())
            .map(|binding| &*binding.name)
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    pub fn len(&self) -> usize {
        self.defined_names().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Read a `#define` out of the tokens that follow it.
///
/// `tokens` starts at the macro's name — that is, after the `#` and the `define` keyword — and `range`
/// is the whole directive, which is what "go to definition" on a use should jump to.
///
/// # Why the name may be a keyword
///
/// A macro name is an identifier in the standard's grammar, but the C++ lexer produces *keywords* for
/// the words C++ reserves — and inside a directive they are not keywords at all. `#define true 1` and
/// `#define private public` are legal C++ that real code uses for testing, and the lexer has no way to
/// know it is inside a directive, so it hands over `TrueKeyword` and `PrivateKeyword`. Refusing those
/// would silently lose the definition: the file would still round-trip, the directive would still be in
/// the tree, and the macro would simply not exist.
///
/// So the name is any token that could be spelled as an identifier: an identifier, or a keyword. What
/// it cannot be is punctuation — `#define 1 2` is malformed, and `#define +` likewise.
pub fn parse_define(tokens: &[Token], range: SourceRange) -> Option<MacroDef> {
    let mut index = 0;

    // Leading whitespace before the name is legal: `#  define  FOO 1`.
    skip_trivia(tokens, &mut index);
    let name_token = tokens.get(index)?;
    if !could_be_a_macro_name(name_token.kind) {
        return None;
    }
    let name_range = name_token.range;
    let name = name_token.text.clone();
    index += 1;

    let params = parse_parameters(tokens, &mut index);
    let body = parse_body(tokens, index, &params);

    Some(MacroDef {
        name,
        params,
        body,
        range,
        name_range,
    })
}

/// Could this token be spelled as a macro name?
///
/// A keyword can: `#define true 1` is inside a directive, where `true` is just a word. Punctuation
/// cannot. A literal cannot either — `#define 1 2` has no name to define, and reading `1` as one would
/// put a macro called `1` into the table.
fn could_be_a_macro_name(kind: CppTokenKind) -> bool {
    kind == CppTokenKind::Identifier
        || kind == CppTokenKind::TrueKeyword
        || kind == CppTokenKind::FalseKeyword
        || crate::token::is_keyword_like(kind)
}

/// Read a parameter list starting at `index`, if one is there.
///
/// Returns `None` for an object-like macro — including the `#define A (1)` case, where the parenthesis
/// is separated from the name by whitespace and is therefore part of the body.
fn parse_parameters(tokens: &[Token], index: &mut usize) -> Option<Vec<Parameter>> {
    // The `(` must follow the name *immediately*. This one whitespace check is the whole difference
    // between the two kinds of macro, and getting it wrong turns `#define A (1)` into a function-like
    // macro with the parameter `1`.
    let open = tokens.get(*index)?;
    if open.kind != CppTokenKind::LeftParen {
        return None;
    }

    *index += 1;

    let mut params: Vec<Parameter> = Vec::new();

    loop {
        skip_trivia(tokens, index);
        let Some(token) = tokens.get(*index) else {
            // Unterminated parameter list: mid-edit. Treat the whole thing as object-like rather than
            // inventing parameters out of whatever follows.
            return None;
        };

        match token.kind {
            CppTokenKind::RightParen => {
                *index += 1;
                return Some(params);
            }
            CppTokenKind::Identifier => {
                params.push(Parameter {
                    name: token.text.clone(),
                    kind: ParameterKind::Ordinary,
                });
                *index += 1;
            }
            CppTokenKind::Ellipsis => {
                params.push(Parameter {
                    name: "__VA_ARGS__".into(),
                    kind: ParameterKind::Variadic,
                });
                *index += 1;
            }
            // A parameter name followed directly by `...`: `#define F(a...)`. The name was already
            // pushed on the previous step; this marks it variadic instead of adding another.
            _ => return None,
        }

        skip_trivia(tokens, index);
        match tokens.get(*index).map(|token| token.kind) {
            Some(CppTokenKind::Comma) => {
                *index += 1;
            }
            Some(CppTokenKind::RightParen) => {}
            // A `...` immediately after an identifier, with no comma: GNU's `a...`.
            Some(CppTokenKind::Ellipsis) => {
                if let Some(last) = params.last_mut() {
                    last.kind = ParameterKind::Variadic;
                }
                *index += 1;
            }
            _ => return None,
        }
    }
}

/// Collect the replacement list and note where the two operators are.
fn parse_body(tokens: &[Token], start: usize, params: &Option<Vec<Parameter>>) -> MacroBody {
    let mut body = MacroBody::default();

    let mut index = start;
    while let Some(token) = tokens.get(index) {
        index += 1;

        // Comments and splices are gone by translation phase 3, so they are not part of what the
        // macro expands to. Dropping them here keeps expansion from having to know about them.
        if is_dropped_in_a_body(token.kind) {
            continue;
        }

        match token.kind {
            CppTokenKind::Hash => {
                // Stringize only when a parameter follows. The `#` of `#define F(x) #x` is an
                // operator; the `#` of `#define F(x) a # b` is a token the compiler rejects, and
                // recording it as an operator would make expansion fail differently from a compiler.
                let mut lookahead = index;
                skip_trivia(tokens, &mut lookahead);
                let is_operator = tokens
                    .get(lookahead)
                    .is_some_and(|next| is_parameter(params, next.text()));
                if is_operator {
                    body.stringize.push(body.tokens.len());
                }
                body.tokens.push(token.clone());
            }
            CppTokenKind::HashHash => {
                body.paste.push(body.tokens.len());
                body.tokens.push(token.clone());
            }
            _ => body.tokens.push(token.clone()),
        }
    }

    body
}

/// Is this token removed before a macro body is formed?
///
/// Comments become a space in translation phase 3 and splices vanish in phase 2, so neither survives
/// into what a macro expands to.
fn is_dropped_in_a_body(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::LineComment
            | CppTokenKind::BlockComment
            | CppTokenKind::LineContinuation
            | CppTokenKind::None
            | CppTokenKind::Eof
    )
}

fn is_parameter(params: &Option<Vec<Parameter>>, name: &str) -> bool {
    params.as_ref().is_some_and(|params| {
        params
            .iter()
            .any(|param| &*param.name == name || name == "__VA_ARGS__")
    })
}

fn skip_trivia(tokens: &[Token], index: &mut usize) {
    while tokens
        .get(*index)
        .is_some_and(|token| is_trivia(token.kind))
    {
        *index += 1;
    }
}
