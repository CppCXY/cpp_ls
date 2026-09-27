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
    ///
    /// **Shared**, because one definition reaches many tables: the corpus's own definitions are handed to the
    /// cooker once per file, and an owned `MacroDef` made each of those a deep clone of its parameter list and its
    /// body tokens. With an `Arc` the second table to see a definition pays a refcount bump — see
    /// `ParsedDefinitions`, which parses each definition's text exactly once for a whole run.
    definition: Option<std::sync::Arc<MacroDef>>,
    /// The offset the directive was written at, so a query can ask about a position.
    at: usize,
}

/// **What a cook reads its macros from** — the definitions a file starts with, asked by name and offset.
///
/// A trait rather than `&MacroTable` because the answer comes from three places, and they must be *asked* the
/// same way: a file's own directives while it is being cooked, the compilation's builtins (`-dM`), and — for a
/// file inside a walked translation unit — the unit's timeline, which materialises nothing per file.
///
/// # Why the answer is by offset, and why that is the whole difficulty
///
/// A `#define` is in force from where it was written, and "where" is a position in **the file being cooked**.
/// One definition reaches hundreds of files from a different offset in each, which is why the shared layer
/// cannot be a table of definitions alone: what is shared is the *definition*, and the offset is computed by
/// whoever knows which file is asking. See `TranslationUnit::definitions` and `FileMacros`.
///
/// Both questions return a borrow rather than an `Arc`: the layers own their definitions, and the hot path asks
/// this once per identifier token.
pub trait MacroBindings {
    /// The definition `name` is in force with at `offset`, or `None` when nothing says.
    fn definition_at(&self, name: &str, offset: usize) -> Option<&MacroDef>;

    /// The definition `name` is in force with at the end of the file.
    ///
    /// A question of its own rather than `definition_at(name, usize::MAX)`: a caller that has read the whole file
    /// (a `#if` evaluated after it, a completion list) means "the last one", and a shared layer may answer it
    /// without resolving any offset at all.
    fn definition(&self, name: &str) -> Option<&MacroDef>;
}

impl MacroBindings for MacroTable {
    fn definition_at(&self, name: &str, offset: usize) -> Option<&MacroDef> {
        self.get_at(name, offset)
    }

    fn definition(&self, name: &str) -> Option<&MacroDef> {
        self.get(name)
    }
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
    /// `name → the indices of its bindings`, in insertion order.
    ///
    /// What makes a lookup proportional to the **name's own history** instead of to the table. `binding_at` used to
    /// scan every binding in reverse, and a cooked file's starting table holds the whole include closure's
    /// definitions — tens of thousands of bindings — while the expander asks about a name for every identifier
    /// token and for every name it meets inside a body. That is the same shape as the two fixes before it (the
    /// class-head scan's window and `rollback`'s `retain`): a linear scan over state that grows with the file, in
    /// a loop that runs per token.
    ///
    /// The index is **not** a second source of truth: `bindings` is the table, this only says where a name's
    /// entries are, and every mutation of one goes through the two methods below.
    by_name: std::collections::HashMap<Box<str>, Vec<u32>>,
}

impl MacroTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember where a name's newest binding is.
    fn index(&mut self, name: &str) {
        let index = (self.bindings.len() - 1) as u32;
        self.by_name
            .entry(Box::from(name))
            .or_default()
            .push(index);
    }

    /// Record a definition, effective from the offset it was written at.
    pub fn define(&mut self, definition: MacroDef) {
        self.define_shared(std::sync::Arc::new(definition));
    }

    /// [`MacroTable::define`] for a caller that already holds the definition **shared** — the second and every
    /// later table to see the same `#define` pays a refcount bump instead of a clone of its tokens.
    pub fn define_shared(&mut self, definition: std::sync::Arc<MacroDef>) {
        let at = definition.range.start_offset;
        self.define_shared_at(definition, at);
    }

    /// [`MacroTable::define_shared`] with the offset the caller wants the binding in force **from**.
    ///
    /// The offset is the *binding's*, which is a different thing from the definition's own range: one `#define` is
    /// in force from a different place in every file that sees it, while the definition itself — its name, its
    /// parameters, its body tokens — is the same text everywhere. Separating the two is what lets a run parse each
    /// distinct definition **once** and hand the same `Arc` to hundreds of tables; the range inside the shared
    /// definition is the reconstructed `#define` line of whichever file parsed it first, which is what it already
    /// was before it was shared (`configuration_from_environment_with` builds that line rather than pointing into
    /// a file), so no consumer loses a position it had.
    pub fn define_shared_at(&mut self, definition: std::sync::Arc<MacroDef>, at: usize) {
        self.bindings.push(Binding {
            at,
            name: definition.name.clone(),
            definition: Some(std::sync::Arc::clone(&definition)),
        });
        self.index(&definition.name);
    }

    /// Record an `#undef`, effective from the offset it was written at.
    pub fn undefine(&mut self, name: &str, at: usize) {
        self.bindings.push(Binding {
            name: name.into(),
            definition: None,
            at,
        });
        self.index(name);
    }

    /// Append every binding of `other`, **sharing** its definitions and keeping each one's offset.
    ///
    /// What a caller building a table out of another one wants (`initial` = the compiler's builtins, then the
    /// environment's definitions): the definitions are the same text in both tables, so copying them is work with
    /// no purpose — and it is measurable work, because a `MacroDef` owns its parameter list and its body tokens.
    /// A census that copied the configuration into the cook's starting table this way spent 5.5 s of a 24 s run
    /// doing it, one deep clone per definition per file.
    pub fn extend_from(&mut self, other: &MacroTable) {
        let first = self.bindings.len() as u32;
        self.bindings.extend(other.bindings.iter().map(|binding| Binding {
            name: binding.name.clone(),
            definition: binding.definition.clone(),
            at: binding.at,
        }));

        // The names come along, shifted: `other`'s indices are its own, and a binding's index is what the lookup
        // uses. A rebuild would also be correct and would throw away the names already in the map.
        for (name, indices) in &other.by_name {
            self.by_name
                .entry(name.clone())
                .or_default()
                .extend(indices.iter().map(|index| index + first));
        }
    }

    /// The binding in force at `offset`, or at the end of the file when `offset` is `None`.
    ///
    /// **The name's own history, not the table.** The reverse scan this replaces compared every binding in the
    /// table against the name, once per query, and the queries come once per token — the third time this shape has
    /// turned up in a census (see the field's note).
    fn binding_at(&self, name: &str, offset: Option<usize>) -> Option<&Binding> {
        let indices = self.by_name.get(name)?;
        indices
            .iter()
            .rev()
            .map(|index| &self.bindings[*index as usize])
            .find(|binding| offset.is_none_or(|offset| binding.at <= offset))
    }

    /// What `name` means at the end of the file.
    pub fn get(&self, name: &str) -> Option<&MacroDef> {
        self.binding_at(name, None)?.definition.as_deref()
    }

    /// What `name` means at `offset`.
    pub fn get_at(&self, name: &str, offset: usize) -> Option<&MacroDef> {
        self.binding_at(name, Some(offset))?.definition.as_deref()
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
            .filter_map(|binding| binding.definition.as_deref())
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
