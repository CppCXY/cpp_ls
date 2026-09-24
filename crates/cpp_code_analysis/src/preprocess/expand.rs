//! Macro expansion: a *shadow* token stream, with every token knowing where it really came from.
//!
//! # Why the result is not written back into the tree
//!
//! A compiler expands and then parses the result. This does not, and the reason is the losslessness
//! invariant: the syntax tree is the file, byte for byte, and replacing a macro call with its expansion
//! would delete the call site from the tree. So expansion produces a second stream that sits *beside*
//! the tree, and the tree is never touched.
//!
//! # Why every token carries an origin
//!
//! Once tokens come from two places, "where is this?" stops being answerable from the token. A token of
//!
//! ```text
//! int z = MAX(1, 2);
//! ```
//!
//! is written in the file, while the tokens it expands to were written in the `#define` — possibly in
//! another file, possibly years ago. Three features depend on telling them apart, and each of them is
//! wrong without it:
//!
//! * **Go to definition** in an expansion should reach the macro, not the definition of whatever the
//!   expansion mentions.
//! * **Diagnostics** inside an expansion belong at the *call site*, which is the only place the reader
//!   can act. A note can point at the macro body.
//! * **Renaming and refactoring** must not rewrite a macro body when the user asked about one call
//!   site, and vice versa.
//!
//! # The three operations, and why they are not string manipulation
//!
//! * **Substitution** replaces a parameter with the *tokens* of an argument, so an argument containing
//!   a comma stays one argument.
//! * **Stringizing** (`#x`) turns those tokens back into spelling, because that is what it means.
//! * **Pasting** (`a ## b`) joins two tokens and **re-lexes** the result: `+` pasted with `=` is `+=`,
//!   one token, and `>` pasted with `>` is `>>`. A text substitution that concatenated the spelling and
//!   stopped would leave two tokens where the language has one.
//!
//! # Termination
//!
//! Three separate guards, because each catches a different mistake: a depth limit (runaway nesting), a
//! per-macro hide set (the standard's rule, which stops `#define A A`), and a total work budget (a
//! pathological input that is neither). All three decline to expand rather than failing, and say which
//! one fired — see [`Diagnostic`].

use cpp_parser::{CppTokenKind, LexerConfig, SourceRange, is_keyword};

use crate::{
    condition::MacroValues,
    macros::{MacroDef, ParameterKind},
    token::{Token, is_trivia},
};

/// How deep expansions may nest before the expander stops.
pub const MAX_DEPTH: usize = 128;

/// How many tokens one expansion may produce in total.
///
/// A backstop for the case the hide set does not cover: mutually recursive macros whose expansion
/// *grows* — `#define A B B` / `#define B A A` terminates by the hide set, but a longer chain can
/// produce an enormous stream before it does. A budget makes "the editor stops responding" impossible
/// rather than merely unlikely.
pub const MAX_TOKENS: usize = 100_000;

/// Where a token in an expanded stream came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The file — the token is exactly what the user typed.
    Source,
    /// A macro body, and the chain of calls that got here.
    ///
    /// A *chain*, not one call, because an expansion can be nested: `#define A 1` / `#define B A` used as
    /// `B` produces a `1` that was written in `A`'s body, reached through `B`'s. Which of the two a
    /// consumer wants depends on what it is doing — see [`ExpandedToken::diagnostic_range`] and
    /// [`ExpandedToken::navigation_range`] — so both are here rather than one being chosen here.
    ///
    /// Outermost first, so that "the first call the reader can see" is a scan from the front.
    Expanded { invocations: Vec<MacroInvocation> },
    /// Two tokens joined by `##`.
    Pasted { call_site: SourceRange },
    /// A string literal produced by `#`.
    Stringized { call_site: SourceRange },
}

/// One macro call that was expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroInvocation {
    /// The macro's name.
    pub name: Box<str>,
    /// Where the macro was **defined**: the whole directive, for a consumer that wants to show it.
    pub definition: SourceRange,
    /// Where the macro's **name** was written, for a consumer that wants to put a cursor on it.
    ///
    /// Separate from `definition` because the two are asked for by different features and the difference
    /// is visible: "reveal this macro" opens the file at the directive, while a go-to-definition puts the
    /// cursor *on the name* so that a second jump goes wherever the name leads. A range covering the whole
    /// `#define` would put the cursor on the `#`.
    pub name_at: SourceRange,
    /// Where it was called: the name, and the argument list when there is one.
    pub call_site: SourceRange,
}

/// A token in an expanded stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedToken {
    /// The token itself. Its `range` is where its text was *written*, which for an expansion is inside
    /// the macro body and not at the call site.
    pub token: Token,
    pub origin: Origin,
    /// Was there a space before this token where it was written?
    ///
    /// Kept because an expansion's tokens come from two files and the output still has to be a usable
    /// token stream: dropping the separator turns `int m = MAX(x, y)` into `intm=(x,y)`, which is not the
    /// same program. The flag is about *spelling* only — nothing downstream should branch on it.
    pub space_before: bool,
}

/// Would joining these two spellings produce a different token?
///
/// The check that makes the output re-lexable. `max` and `(` are fine next to each other; `m` and `=`
/// are not, because `m=` is not a token but `intm` would be if the left side ended in a letter. Rather
/// than decide it case by case, a separator is inserted whenever both sides are "word-ish": that is
/// conservative, costs a token, and cannot merge two tokens by accident.
fn needs_a_separator(left: &Token, right: &Token) -> bool {
    if left.text().is_empty() || right.text().is_empty() {
        return false;
    }

    finishes_a_word(left.kind) && starts_a_word(right.kind)
}

/// Could this token's spelling run into whatever follows it?
fn finishes_a_word(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Identifier
            | CppTokenKind::IntegerLiteral
            | CppTokenKind::FloatingLiteral
            | CppTokenKind::StringLiteral
            | CppTokenKind::CharLiteral
            | CppTokenKind::UserDefinedLiteral
    ) || is_keyword(kind)
}

/// Could this token's spelling absorb the end of whatever precedes it?
fn starts_a_word(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Identifier
            | CppTokenKind::IntegerLiteral
            | CppTokenKind::FloatingLiteral
            | CppTokenKind::StringLiteral
            | CppTokenKind::CharLiteral
            | CppTokenKind::UserDefinedLiteral
            | CppTokenKind::Plus
            | CppTokenKind::Minus
            | CppTokenKind::Assign
            | CppTokenKind::Less
            | CppTokenKind::Greater
            | CppTokenKind::Ampersand
            | CppTokenKind::Pipe
            | CppTokenKind::Dot
            | CppTokenKind::Scope
    ) || is_keyword(kind)
}

impl ExpandedToken {
    pub fn kind(&self) -> CppTokenKind {
        self.token.kind
    }

    pub fn text(&self) -> &str {
        self.token.text()
    }

    /// Where to report a problem with this token.
    ///
    /// **The outermost call site**, which is the text the reader can see. For a doubly expanded macro the
    /// innermost call is inside a `#define` somewhere above, and pointing a diagnostic at it would send the
    /// reader to a line they were not editing. The scan is over the chain from the outside in, and stops at
    /// the first call site that lies inside the invocation the token belongs to.
    pub fn diagnostic_range(&self) -> SourceRange {
        match &self.origin {
            Origin::Source => self.token.range,
            Origin::Expanded { invocations } => invocations
                .first()
                .map(|invocation| invocation.call_site)
                .unwrap_or(self.token.range),
            Origin::Pasted { call_site } | Origin::Stringized { call_site } => *call_site,
        }
    }

    /// Where to *navigate* from this token: the macro's name.
    ///
    /// **The innermost macro**, which is the one whose body the token was actually written in — and so the
    /// one whose text the token is. Deliberately the opposite end of the chain from
    /// [`diagnostic_range`](Self::diagnostic_range): a reader following a link wants the definition, and a
    /// reader fixing a problem wants their own code.
    ///
    /// The *name* rather than the whole directive, so that the cursor lands somewhere a second jump can
    /// start from. See [`MacroInvocation::name_at`].
    pub fn navigation_range(&self) -> SourceRange {
        match &self.origin {
            Origin::Expanded { invocations } => invocations
                .last()
                .map(|invocation| invocation.name_at)
                .unwrap_or(self.token.range),
            _ => self.token.range,
        }
    }

    /// Was this token produced by expanding a macro, as opposed to written in the file?
    pub fn is_expanded(&self) -> bool {
        !matches!(self.origin, Origin::Source)
    }
}

/// Why the expander declined to expand something.
///
/// Not errors. An unexpanded macro name is still a perfectly good token, and in an editor it is the
/// normal state of a file being typed: the arguments are not finished yet, or the definition is three
/// lines below the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpansionNote {
    /// A function-like macro with no argument list after it.
    ///
    /// `F` alone is not a call, and expanding it would invent an invocation. The name is left as it is.
    NoArgumentList { name: Box<str> },
    /// The argument list was not closed before the end of the input — the normal state of a call being
    /// typed.
    UnterminatedArgumentList { name: Box<str> },
    /// The wrong number of arguments.
    WrongArgumentCount {
        name: Box<str>,
        expected: usize,
        found: usize,
    },
    /// The macro is already being expanded, so expanding it again would not terminate.
    Recursive { name: Box<str> },
    /// Expansions nested deeper than [`MAX_DEPTH`].
    TooDeep { name: Box<str> },
    /// The budget in [`MAX_TOKENS`] ran out.
    BudgetExhausted,
    /// Nothing was found to paste, or the paste produced nothing.
    EmptyPaste,
}

/// A note, and where it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub note: ExpansionNote,
    pub range: SourceRange,
}

/// The result of expanding a token stream.
#[derive(Debug, Clone, Default)]
pub struct Expansion {
    pub tokens: Vec<ExpandedToken>,
    /// Why something was left unexpanded. Empty for a stream with nothing to expand.
    pub diagnostics: Vec<Diagnostic>,
    /// Did the budget run out? When it did, `tokens` is a prefix rather than the whole stream.
    pub exhausted: bool,
}

impl Expansion {
    /// The tokens as plain tokens, for a consumer that does not need the origins.
    pub fn plain(&self) -> Vec<Token> {
        self.tokens.iter().map(|it| it.token.clone()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

/// Expand the macros in a token stream.
///
/// `at` is the offset used to look macros up, so that a `#define` later in the file does not apply to a
/// use before it. Pass `usize::MAX` to use the table's final state.
pub fn expand(tokens: &[Token], macros: &(impl MacroValues + ?Sized)) -> Expansion {
    let mut expander = Expander {
        macros,
        active: Vec::new(),
        invocations: Vec::new(),
        depth: 0,
        budget: MAX_TOKENS,
        out: Vec::new(),
        diagnostics: Vec::new(),
        exhausted: false,
    };

    let marked: Vec<Marked> = tokens.iter().cloned().map(Marked::from).collect();
    let region = span_of(tokens);
    expander.expand_into(&marked, region);
    expander.finish()
}

/// Expand with no limit on the number of tokens, for callers that pass their own bound.
pub fn expand_with_budget(
    tokens: &[Token],
    macros: &(impl MacroValues + ?Sized),
    budget: usize,
) -> Expansion {
    let mut expander = Expander {
        macros,
        active: Vec::new(),
        invocations: Vec::new(),
        depth: 0,
        budget,
        out: Vec::new(),
        diagnostics: Vec::new(),
        exhausted: false,
    };

    let marked: Vec<Marked> = tokens.iter().cloned().map(Marked::from).collect();
    let region = span_of(tokens);
    expander.expand_into(&marked, region);
    expander.finish()
}

struct Expander<'a, M: MacroValues + ?Sized> {
    macros: &'a M,
    /// The macros currently being expanded, innermost last. The standard's "blue paint".
    active: Vec<Box<str>>,
    /// The invocations behind `active`, innermost last.
    ///
    /// Needed because a token from a macro body has a range inside the `#define`, and that range is the
    /// only thing that distinguishes it from a token of the call site: the two live in different parts of
    /// the file, so "is this range inside the call?" answers which of them was written by the user.
    invocations: Vec<MacroInvocation>,
    depth: usize,
    budget: usize,
    out: Vec<ExpandedToken>,
    diagnostics: Vec<Diagnostic>,
    exhausted: bool,
}

impl<M: MacroValues + ?Sized> Expander<'_, M> {
    fn finish(self) -> Expansion {
        Expansion {
            tokens: self.out,
            diagnostics: self.diagnostics,
            exhausted: self.exhausted,
        }
    }

    fn note(&mut self, note: ExpansionNote, range: SourceRange) {
        // A budget-exhausted stream can produce the same note thousands of times, and a consumer only
        // needs to be told once.
        if self.diagnostics.len() < 32 {
            self.diagnostics.push(Diagnostic { note, range });
        }
    }

    /// Was this token written where the user is looking, or in a macro body?
    ///
    /// `region` is the span in the **file** that the run being expanded came from — the call site for an
    /// invocation, and the macro body for what is expanded inside it. A token inside the region was copied
    /// from the source, which includes an argument: `x` in `MAX(x, y)` lies between the parentheses and is
    /// the user's own `x`, so "go to definition" on it must find their variable and not the macro.
    ///
    /// A token outside the region came from the body, and without this it would be reported as
    /// `Origin::Source` — because that is what a body's tokens are when they are read. They are not source
    /// *here*: the user never wrote `((a) > (b) ? (a) : (b))` at this line.
    ///
    /// `Pasted` and `Stringized` are kept as they are: a consumer asking about a joined token wants the
    /// call site it was joined at, and re-labelling it as `Expanded` would throw that away.
    fn origin_of(&self, token: &Token, region: Option<SourceRange>, fallback: &Origin) -> Origin {
        if matches!(fallback, Origin::Pasted { .. } | Origin::Stringized { .. }) {
            return fallback.clone();
        }

        let Some(region) = region else {
            return fallback.clone();
        };

        if token.range.start_offset >= region.start_offset
            && token.range.end_offset() <= region.end_offset()
        {
            return fallback.clone();
        }

        Origin::Expanded {
            invocations: self.invocations.clone(),
        }
    }

    /// Is the separator a consumer renders before this token readable in the *source*?
    ///
    /// Only when the two tokens are adjacent in the same file can the gap between their positions mean
    /// anything. A macro body's tokens have offsets in the `#define`, so comparing them with a call site's
    /// offsets compares two coordinate systems — and the answer would be a space wherever the numbers
    /// happened to line up.
    fn space_before(&self, previous: &Token, token: &Token) -> bool {
        let same_file = match self.invocations.last() {
            None => true,
            Some(invocation) => {
                let site = invocation.call_site;
                let in_site = |range: SourceRange| {
                    range.start_offset >= site.start_offset
                        && range.end_offset() <= site.end_offset()
                };
                in_site(previous.range) == in_site(token.range)
            }
        };

        let separated_in_source =
            same_file && previous.range.end_offset() < token.range.start_offset;

        separated_in_source || needs_a_separator(previous, token)
    }

    fn push(&mut self, token: Token, origin: Origin, region: Option<SourceRange>) -> bool {
        let origin = self.origin_of(&token, region, &origin);

        if self.budget == 0 {
            self.exhausted = true;
            return false;
        }
        self.budget -= 1;

        let space_before = match self.out.last() {
            None => false,
            Some(previous) => self.space_before(&previous.token, &token),
        };

        self.out.push(ExpandedToken {
            token,
            origin,
            space_before,
        });
        true
    }

    /// Expand a run of tokens, appending to `out`.
    ///
    /// `region` is the span in the file those tokens came from, and it is what [`origin_of`](Self::origin_of)
    /// uses to tell a token of the source from a token of a macro body. It is `None` only when the caller
    /// handed over something with no file behind it.
    fn expand_into(&mut self, tokens: &[Marked], region: Option<SourceRange>) {
        let mut index = 0;

        while index < tokens.len() {
            if self.exhausted {
                return;
            }

            let token = &tokens[index].token;

            // Only a *name* can be a macro. A keyword can be one too — `#define true 1` is legal, and
            // the lexer has no way to know it is looking at a directive's name — so both are looked up.
            if !could_be_a_macro_name(token.kind) {
                if !self.push(token.clone(), tokens[index].origin.clone(), region) {
                    return;
                }
                index += 1;
                continue;
            }

            // Only a *definition* expands. A table that knows the name is a macro without holding its body
            // (`Lookup::DefinedWithoutAValue`) cannot say what to paste, and pasting nothing would silently
            // delete the use — the same reasoning that makes `#if NAME` unknown rather than `0`.
            let Some(definition) = self.macros.lookup(token.text()).definition() else {
                // Not a macro. Not a note either: most identifiers are not macros, and reporting that
                // would bury the ones that are.
                if !self.push(token.clone(), tokens[index].origin.clone(), region) {
                    return;
                }
                index += 1;
                continue;
            };

            // The definition has to be cloned because the borrow of the table cannot outlive the
            // recursive calls below, which read the table again.
            let definition = definition.clone();
            let name = token.text.clone();

            if self.active.contains(&name) {
                self.note(ExpansionNote::Recursive { name: name.clone() }, token.range);
                if !self.push(token.clone(), tokens[index].origin.clone(), region) {
                    return;
                }
                index += 1;
                continue;
            }

            if self.depth >= MAX_DEPTH {
                self.note(ExpansionNote::TooDeep { name: name.clone() }, token.range);
                if !self.push(token.clone(), tokens[index].origin.clone(), region) {
                    return;
                }
                index += 1;
                continue;
            }

            match &definition.params {
                // An object-like macro: substitute nothing, expand the body in place.
                None => {
                    let invocation = MacroInvocation {
                        name: name.clone(),
                        definition: definition.range,
                        name_at: definition.name_range,
                        call_site: token.range,
                    };
                    self.expand_body(&definition, &[], tokens, invocation);
                    index += 1;
                }
                // A function-like macro: only a call. The `(` has to be *adjacent*, which is why the
                // check is on the next token rather than on the next significant one.
                Some(_) => {
                    if !next_token_is_a_call(tokens, index) {
                        self.note(
                            ExpansionNote::NoArgumentList { name: name.clone() },
                            token.range,
                        );
                        if !self.push(token.clone(), tokens[index].origin.clone(), region) {
                            return;
                        }
                        index += 1;
                        continue;
                    }

                    let Some(arguments) = split_arguments(tokens, index) else {
                        self.note(
                            ExpansionNote::UnterminatedArgumentList { name: name.clone() },
                            token.range,
                        );
                        if !self.push(token.clone(), tokens[index].origin.clone(), region) {
                            return;
                        }
                        index += 1;
                        continue;
                    };

                    if !definition.accepts_argument_count(arguments.groups.len()) {
                        self.note(
                            ExpansionNote::WrongArgumentCount {
                                name: name.clone(),
                                expected: expected_arguments(&definition),
                                found: arguments.groups.len(),
                            },
                            token.range,
                        );
                        if !self.push(token.clone(), tokens[index].origin.clone(), region) {
                            return;
                        }
                        index += 1;
                        continue;
                    }

                    let call_site = SourceRange::new(
                        token.range.start_offset,
                        arguments.end_offset - token.range.start_offset,
                    );
                    let invocation = MacroInvocation {
                        name: name.clone(),
                        definition: definition.range,
                        name_at: definition.name_range,
                        call_site,
                    };

                    self.expand_body(&definition, &arguments.groups, tokens, invocation);
                    index = arguments.next_index;
                }
            }
        }
    }

    /// Substitute a macro's arguments into its body and expand the result.
    ///
    /// `arguments` are slices of `tokens`, so an argument is a token sequence and not a string.
    fn expand_body(
        &mut self,
        definition: &MacroDef,
        arguments: &[std::ops::Range<usize>],
        tokens: &[Marked],
        invocation: MacroInvocation,
    ) {
        self.active.push(definition.name.clone());
        self.invocations.push(invocation.clone());
        self.depth += 1;

        let plain: Vec<Token> = tokens.iter().map(|marked| marked.token.clone()).collect();
        let substituted = substitute(definition, arguments, &plain, &invocation);

        // The rescan. Doing it through `expand_into` rather than by re-running `expand` on the whole
        // thing is what keeps the origin chain: a token that came from an argument and is *also* a
        // macro gets the inner expansion's origin, which is the one whose call site is on screen.
        //
        // The region is the macro body, so a token of the body is outside it and a token that came from an
        // argument — written at the call site — is inside. That is how the two stay distinguishable all
        // the way down.
        let region = span_of(&plain);
        self.expand_into(&substituted, region);

        self.depth -= 1;
        self.active.pop();
        self.invocations.pop();
    }
}

/// The result of reading one argument list.
struct Arguments {
    /// Each argument as a range into the original token slice, so no tokens are copied.
    groups: Vec<std::ops::Range<usize>>,
    /// The index just past the closing `)`.
    next_index: usize,
    /// The offset just past the closing `)`.
    end_offset: usize,
}

/// Is the token after `index` an opening parenthesis with nothing in between?
///
/// The adjacency is the standard's rule and it is observable: given `#define F(x) x`,
///
/// ```text
/// F(1)     // a call
/// F (1)    // `F` is not a call at all — the tokens are `F`, `(`, `1`, `)`
/// ```
///
/// so a check on the next *significant* token would expand `F (1)` into `(1)` where a compiler leaves
/// `F (1)` alone.
fn next_token_is_a_call(tokens: &[Marked], index: usize) -> bool {
    tokens
        .get(index + 1)
        .is_some_and(|marked| marked.token.kind == CppTokenKind::LeftParen)
}

/// The span a token run occupies in the file, if it occupies one at all.
///
/// A run is contiguous in a file only when it came from one: a substituted body mixes tokens from a
/// `#define` with tokens from a call site, and the span of such a run covers both — which is why the
/// origin check never relies on this alone. It is the *region* the run was built from, and each nested
/// expansion replaces it with its own.
fn span_of(tokens: &[Token]) -> Option<SourceRange> {
    let first = tokens.first()?;
    let last = tokens.last()?;

    Some(SourceRange::new(
        first.range.start_offset,
        last.range.end_offset() - first.range.start_offset,
    ))
}

/// A token and the origin it was produced with, carried through substitution.
///
/// Substitution produces three kinds of token — the body's own, an argument's, and the two the operators
/// make — and the origin of each has to survive into the rescan. A plain `Vec<Token>` cannot: by the time
/// the rescan sees a pasted token it is indistinguishable from a body token, and the call site it was
/// pasted at is lost, which is the one thing a consumer needs in order to report against the right line.
struct Marked {
    token: Token,
    origin: Origin,
}

impl From<Token> for Marked {
    fn from(token: Token) -> Self {
        Marked {
            token,
            origin: Origin::Source,
        }
    }
}

impl FromIterator<Marked> for Vec<Token> {
    /// Keep the tokens and drop the origins, for the callers that only need the spelling.
    fn from_iter<T: IntoIterator<Item = Marked>>(iter: T) -> Self {
        iter.into_iter().map(|marked| marked.token).collect()
    }
}

/// Read an argument list starting at the `(` that follows the macro name at `index`.
///
/// Returns `None` when the list is not closed before the input ends, which is the state of a call being
/// typed and not an error worth reporting.
fn split_arguments(tokens: &[Marked], index: usize) -> Option<Arguments> {
    let open = index + 1;
    let mut depth = 0usize;
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    let mut group_start = open + 1;
    let mut cursor = open;
    // Was there a comma at the top level? It decides whether the group the list ends on is a real
    // argument or an empty tail — see the `)` case.
    let mut saw_a_comma = false;

    while cursor < tokens.len() {
        let token = &tokens[cursor].token;

        match token.kind {
            CppTokenKind::LeftParen => depth += 1,
            CppTokenKind::RightParen => {
                depth -= 1;
                if depth == 0 {
                    // What the list ended on is an argument when something was written there, or when a
                    // comma promised one. `F()` has none; `F(a)` has one; `F(,)` has two empty ones,
                    // because the comma separates two arguments whether or not they are spelled.
                    if saw_a_comma || group_has_content(tokens, group_start..cursor) {
                        groups.push(group_start..cursor);
                    }
                    return Some(Arguments {
                        groups,
                        next_index: cursor + 1,
                        end_offset: token.range.end_offset(),
                    });
                }
            }
            // A comma at depth zero separates arguments. Inside brackets it does not, which is what
            // makes `F((a, b))` one argument.
            CppTokenKind::Comma if depth == 1 => {
                groups.push(group_start..cursor);
                group_start = cursor + 1;
                saw_a_comma = true;
            }
            _ => {}
        }

        cursor += 1;
    }

    None
}

/// Does this range hold anything but layout?
///
/// Whitespace does not make an argument: `F( )` has no arguments, exactly like `F()`. Treating it as one
/// empty argument would make `#define F() 42` uncallable in the only spelling anyone writes it in.
fn group_has_content(tokens: &[Marked], range: std::ops::Range<usize>) -> bool {
    tokens
        .get(range)
        .is_some_and(|slice| slice.iter().any(|marked| !is_trivia(marked.token.kind)))
}

/// How many arguments a macro expects, for a diagnostic.
fn expected_arguments(definition: &MacroDef) -> usize {
    definition
        .params
        .as_ref()
        .map(|params| params.len())
        .unwrap_or(0)
}

/// Build the replacement list: parameters replaced, `#` applied, `##` applied.
///
/// # Why the body's layout is removed first
///
/// `##` is defined on **adjacent** tokens, and `#define CAT(a, b) a ## b` has whitespace on both sides of
/// the operator. Keeping that whitespace means the token before `##` is a space rather than `a`, and the
/// paste joins the wrong pair.
///
/// The obvious fix — look for the next *significant* token — does not work here, because substitution
/// changes how many tokens precede a position: a parameter replaced by two tokens shifts every later
/// index, so a "skip the trivia" offset computed against the body no longer points at the same token.
/// Removing the layout up front makes every operand adjacent by construction, and the output's spelling is
/// reconstructed from the source positions of the tokens that remain.
fn substitute(
    definition: &MacroDef,
    arguments: &[std::ops::Range<usize>],
    tokens: &[Token],
    invocation: &MacroInvocation,
) -> Vec<Marked> {
    let Some(params) = &definition.params else {
        // An object-like macro is its body. The layout around the body goes: it is the space after the
        // macro's name and the newline that ended the directive, and it belongs to the `#define` rather
        // than to what the macro expands to. Keeping it would make `VERSION` expand to `" 3\n"`.
        return significant_tokens(&definition.body.tokens)
            .into_iter()
            .map(Marked::from)
            .collect();
    };

    // Every token that is not layout, in body order. `stringize` and `paste` index into *this*, which is
    // what the positions recorded at parse time mean once the layout is gone.
    let body_tokens = significant_tokens(&definition.body.tokens);
    let stringize_at = remap_positions(&definition.body.stringize, &definition.body.tokens);
    let paste_at = remap_positions(&definition.body.paste, &definition.body.tokens);

    let mut out: Vec<Marked> = Vec::with_capacity(body_tokens.len());

    let mut index = 0;
    while index < body_tokens.len() {
        let token = &body_tokens[index];

        // `#` before a parameter: the argument's *spelling*, as a string literal.
        if stringize_at.contains(&index)
            && token.kind == CppTokenKind::Hash
            && let Some(name_index) = (index + 1..body_tokens.len()).next()
            && is_parameter(params, body_tokens[name_index].text())
        {
            let name = body_tokens[name_index].text();
            let argument = argument_for(params, arguments, tokens, name);
            out.push(Marked {
                token: stringize(&argument, invocation.call_site),
                origin: Origin::Stringized {
                    call_site: invocation.call_site,
                },
            });
            // Skip the `#` and the parameter name.
            index = name_index + 1;
            continue;
        }

        // `##`: paste the token before it with the token after it, which may be a parameter.
        if paste_at.contains(&index) {
            let right_index = (index + 1..body_tokens.len()).next();
            let Some(right_index) = right_index else {
                // A `##` at the end of a body: nothing to paste. Dropping it is what GCC does.
                index += 1;
                continue;
            };

            // **The** token before `##`, which is the last one emitted. Everything before it stays where
            // it is: `#define CAT(a, b) x a ## b` pastes `a` with `b` and leaves `x` alone, and an
            // argument that expanded to several tokens contributes only its last one.
            let left = out.pop();
            let right = if is_parameter(params, body_tokens[right_index].text()) {
                argument_for(params, arguments, tokens, body_tokens[right_index].text())
                    .into_iter()
                    .next()
                    .map(Marked::from)
            } else {
                Some(Marked::from(body_tokens[right_index].clone()))
            };

            match (left, right) {
                (Some(left), Some(right)) => {
                    // The first token is the one the operator made; a paste that did not re-lex leaves
                    // both halves, and only the first is the result.
                    let mut pasted = paste(&left.token, &right.token);
                    if let Some(first) = pasted.first_mut() {
                        first.origin = Origin::Pasted {
                            call_site: invocation.call_site,
                        };
                    }
                    out.extend(pasted);
                }
                // One side was empty — `a ## ## b`, or a parameter called with nothing. There is
                // nothing to paste, and the remaining side stands on its own.
                (Some(left), None) => out.push(left),
                (None, Some(right)) => out.push(right),
                (None, None) => {}
            }

            index = right_index + 1;
            continue;
        }

        // An ordinary parameter: the argument's tokens.
        if is_parameter(params, token.text()) {
            let argument = argument_for(params, arguments, tokens, token.text());
            out.extend(argument.into_iter().map(Marked::from));

            // A variadic parameter often has no argument at all — `LOG("x")` for
            // `#define LOG(f, ...)`. Nothing is substituted, which is why `__VA_ARGS__` disappearing
            // is normal rather than a bug.
            index += 1;
            continue;
        }

        out.push(Marked::from(token.clone()));
        index += 1;
    }

    out
}

/// Translate positions in the full body into positions in its layout-free form.
///
/// The parser records where the `#` and `##` operators are by index into the body *as written*, and
/// [`substitute`] works on a body with the layout removed. This is the one place that translates between
/// the two, so that a recorded position and the token it means can never drift apart.
fn remap_positions(positions: &[usize], body: &[Token]) -> Vec<usize> {
    positions
        .iter()
        .filter_map(|position| {
            // How many non-trivia tokens come before this one: that is its new index.
            let significant_before = body
                .get(..*position)
                .map(|before| before.iter().filter(|token| !is_trivia(token.kind)).count())
                .unwrap_or(0);

            // A position that pointed at trivia was never an operator.
            body.get(*position)
                .filter(|token| !is_trivia(token.kind))
                .map(|_| significant_before)
        })
        .collect()
}

/// Drop every trivia token from a run.
///
/// Used on macro bodies, where layout is never semantic and where keeping it would break `##` — see
/// [`substitute`]. Arguments are trimmed at the ends only, because `#x` is defined on the spelling and a
/// space *between* two argument tokens is part of it.
fn significant_tokens(tokens: &[Token]) -> Vec<Token> {
    tokens
        .iter()
        .filter(|token| !is_trivia(token.kind))
        .cloned()
        .collect()
}

/// Drop the trivia at the two ends of a token run.
fn trim_trivia(tokens: Vec<Token>) -> Vec<Token> {
    let start = tokens
        .iter()
        .position(|token| !is_trivia(token.kind))
        .unwrap_or(tokens.len());
    let end = tokens
        .iter()
        .rposition(|token| !is_trivia(token.kind))
        .map(|index| index + 1)
        .unwrap_or(start);

    tokens[start.min(end)..end].to_vec()
}

/// The argument bound to a parameter name, with the layout around it removed.
///
/// Trimming is not cosmetic: `CAT(+, =)` has an argument whose tokens are `+`, and without the trim they
/// would be `" "`, `"+"`, `" "` — so the paste would join a space with an `=` and produce nothing.
/// Whitespace between *two* argument tokens is kept, because `#x` is defined on the spelling.
fn argument_for(
    params: &[crate::macros::Parameter],
    arguments: &[std::ops::Range<usize>],
    tokens: &[Token],
    name: &str,
) -> Vec<Token> {
    trim_trivia(argument_range_for(params, arguments, tokens, name))
}

/// The tokens bound to a parameter, layout included.
fn argument_range_for(
    params: &[crate::macros::Parameter],
    arguments: &[std::ops::Range<usize>],
    tokens: &[Token],
    name: &str,
) -> Vec<Token> {
    let Some(position) = params.iter().position(|param| &*param.name == name) else {
        // `__VA_ARGS__` in a macro that is not variadic, or a name that only looks like a parameter.
        return Vec::new();
    };

    // A variadic parameter collects everything from its position onwards, which is what `...` means.
    let is_variadic = params[position].kind == ParameterKind::Variadic;
    let range = if is_variadic {
        let Some(first) = arguments.get(position) else {
            return Vec::new();
        };
        let last_end = arguments.last().map(|range| range.end).unwrap_or(first.end);
        first.start..last_end
    } else {
        match arguments.get(position) {
            Some(range) => range.clone(),
            None => return Vec::new(),
        }
    };

    let Some(slice) = tokens.get(range) else {
        return Vec::new();
    };

    // With more than one argument the pieces have to be re-joined, because they are separate ranges of
    // the original slice and the commas between them belong to `__VA_ARGS__`.
    if is_variadic && arguments.len() > position + 1 {
        let mut joined = Vec::new();
        for (offset, argument) in arguments[position..].iter().enumerate() {
            if offset > 0 {
                joined.push(synthetic_comma(tokens, argument.start));
            }
            if let Some(piece) = tokens.get(argument.clone()) {
                joined.extend_from_slice(piece);
            }
        }
        return joined;
    }

    slice.to_vec()
}

/// A `,` token to stand between two argument pieces that were split apart.
fn synthetic_comma(tokens: &[Token], near: usize) -> Token {
    let range = tokens
        .get(near)
        .map(|token| SourceRange::new(token.range.start_offset, 1))
        .unwrap_or_else(|| SourceRange::new(0, 1));

    Token::new(CppTokenKind::Comma, ",", range)
}

/// `#x`: the argument's tokens as a string literal.
///
/// The spelling is the argument's own text, with whitespace between tokens collapsed to one space,
/// because that is what a preprocessor does and what makes `#x` for `x` = `a   +   b` produce `"a + b"`.
fn stringize(argument: &[Token], call_site: SourceRange) -> Token {
    let mut text = String::with_capacity(argument.len() * 4 + 2);
    text.push('"');

    let mut needs_a_space = false;
    for token in argument.iter().filter(|token| !is_trivia(token.kind)) {
        if needs_a_space {
            text.push(' ');
        }
        // Within the string, `"` and `\` have to be escaped or the literal does not re-lex.
        for character in token.text().chars() {
            match character {
                '"' => text.push_str("\\\""),
                '\\' => text.push_str("\\\\"),
                other => text.push(other),
            }
        }
        needs_a_space = true;
    }

    text.push('"');

    Token::new(CppTokenKind::StringLiteral, text, call_site)
}

/// `a ## b`: join two tokens into one, then read the result back as a token.
///
/// The re-lex is the point. `+` pasted with `=` is `+=`, and a caller that concatenated the spelling
/// would hand the parser two tokens where the language has one.
///
/// The joined token's own range is the *left* token's, which is a position in the macro body rather
/// than anywhere the text exists. That is deliberate: the origin recorded for the pasted token is
/// [`Origin::Pasted`], and its call site is what a consumer reports against.
fn paste(left: &Token, right: &Token) -> Vec<Marked> {
    let joined = format!("{}{}", left.text(), right.text());
    let range = SourceRange::new(left.range.start_offset, joined.len());

    let lexed = lex_fragment(&joined, range);

    match lexed {
        // Exactly one token: the paste produced a real token.
        Some(mut tokens) if tokens.len() == 1 => {
            let mut token = tokens.remove(0);
            token.range = range;
            vec![Marked::from(token)]
        }
        // Nothing readable, or more than one token. `##` producing an invalid token is undefined in the
        // standard; keeping the two halves is the least surprising thing to do, and it keeps the stream
        // lossless in the sense that no text was invented or dropped.
        _ => vec![Marked::from(left.clone()), Marked::from(right.clone())],
    }
}

/// Lex a fragment, with no file behind it.
///
/// Returns `None` when nothing was produced. Used only by `##`, whose result has no position in the
/// file — the range handed in is the left token's, which is a lie the call site corrects.
fn lex_fragment(text: &str, _range: SourceRange) -> Option<Vec<Token>> {
    let mut errors = Vec::new();
    let mut lexer = cpp_parser::CppLexer::new(text, LexerConfig::default(), &mut errors);
    let data = lexer.tokenize();

    if data.is_empty() {
        return None;
    }

    Some(
        data.into_iter()
            .map(|token| {
                Token::new(
                    token.kind,
                    &text[token.range.start_offset..token.range.end_offset()],
                    token.range,
                )
            })
            .collect(),
    )
}

/// Is this name a parameter of the macro?
fn is_parameter(params: &[crate::macros::Parameter], name: &str) -> bool {
    params.iter().any(|param| &*param.name == name)
}

/// Is this token a name that could be a macro, as opposed to punctuation or a literal?
pub fn could_be_a_macro_name(kind: CppTokenKind) -> bool {
    kind == CppTokenKind::Identifier || is_keyword(kind)
}
