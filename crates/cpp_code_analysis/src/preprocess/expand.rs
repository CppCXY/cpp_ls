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
    /// [`ExpandedToken::navigation_at`] — so both are here rather than one being chosen here.
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
    /// Where it was called: the name, and the argument list where there is one.
    pub call_site: SourceRange,
    /// **Which file `definition` and `name_at` are positions in** — see [`crate::macros::MacroFile`].
    ///
    /// `Some(Here)` when the macro is defined in the file being cooked, `Some(Frame(f))` when it came from
    /// another file of a walked unit (the unit turns `f` into a path), and `None` when the definition was
    /// reconstructed from text that carried no position — where `definition`/`name_at` are offsets in that
    /// reconstruction and a consumer must not show them.
    ///
    /// `call_site` is **always** in the file being cooked: an invocation is written where the reader is.
    pub written_in: Option<crate::macros::MacroFile>,
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

    /// Where to *navigate* from this token: the macro's name, **and which file that name is in**.
    ///
    /// **The innermost macro** whose definition is a position in a file, which is the one whose body the token
    /// was actually written in — and so the one whose text the token is. Deliberately the opposite end of the
    /// chain from [`diagnostic_range`](Self::diagnostic_range): a reader following a link wants the definition,
    /// and a reader fixing a problem wants their own code.
    ///
    /// The *name* rather than the whole directive, so that the cursor lands somewhere a second jump can start
    /// from. See [`MacroInvocation::name_at`].
    ///
    /// # Why the answer is a pair, and why it can be `None`
    ///
    /// A definition that came from another file of a walked unit has real positions, in that file
    /// ([`crate::macros::MacroFile::Frame`]) — the unit turns the frame into a path, and a consumer that jumped to the range
    /// alone would open the *wrong* file. And a definition reconstructed from text that carried no position has
    /// no file at all (`written_in: None`): `None` here is "there is nowhere to go", which is what the caller
    /// must show, rather than a range that happens to be inside the reconstruction.
    ///
    /// The chain is scanned from the inside out, so a token an inherited macro pasted is navigated to the
    /// *innermost definition that can be pointed at* rather than to a hop with no file.
    pub fn navigation_at(&self) -> Option<(crate::macros::MacroFile, SourceRange)> {
        match &self.origin {
            Origin::Expanded { invocations } => invocations.iter().rev().find_map(|invocation| {
                invocation
                    .written_in
                    .map(|file| (file, invocation.name_at))
            }),
            // A token the file wrote: its own name is where a reader would look, in the file they are reading.
            _ => Some((crate::macros::MacroFile::Here, self.token.range)),
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
    /// `F` with no `(` after it (layout aside) is not a call, and expanding it would invent an invocation. The name is\n    /// left as it is.
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
    expander.expand_into(&marked, region, None);
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
    expander.expand_into(&marked, region, None);
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
    /// # Why a body's tokens say so instead of being measured
    ///
    /// The test this used to rely on alone — "inside the region is the source, outside is the body" — compares
    /// offsets, and offsets only compare when they are in the same text. For a definition a **file** wrote they
    /// are: the body's ranges are in the `#define` and the call site is in the file, so a body token is outside
    /// the region. For a definition that arrived from another file through the unit's evidence they are not: it
    /// is parsed from the `#define` line this crate *reconstructs*
    /// (`cpp_code_analysis::preprocess::cooked::definition_text`), so a body token's range is an offset in that
    /// string — 7 inside a 13-byte reconstruction — and the comparison can succeed by coincidence against a call
    /// site in a 20 000-byte header. What that produced was a token labelled `Origin::Source` carrying a range
    /// that is a position in no file at all, which is worse than a missing answer: a diagnostic lands in the
    /// wrong place and a "go to definition" goes nowhere.
    ///
    /// So a body token is **marked** as a body token ([`Marked::from_a_body`]) and this fills in the chain it
    /// belongs to. The range test stays for everything else, where it is sound and is what keeps an argument's
    /// own tokens labelled as the source's.
    ///
    /// `Pasted` and `Stringized` are kept as they are: a consumer asking about a joined token wants the
    /// call site it was joined at, and re-labelling it as `Expanded` would throw that away.
    fn origin_of(&self, token: &Token, region: Option<SourceRange>, fallback: &Origin) -> Origin {
        if matches!(fallback, Origin::Pasted { .. } | Origin::Stringized { .. }) {
            return fallback.clone();
        }

        // A token the expansion of an argument already produced carries the chain it was produced under.
        if matches!(fallback, Origin::Expanded { invocations } if !invocations.is_empty()) {
            return fallback.clone();
        }

        if matches!(fallback, Origin::Expanded { invocations } if invocations.is_empty()) {
            return Origin::Expanded {
                invocations: self.invocations.clone(),
            };
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
    fn expand_into(
        &mut self,
        tokens: &[Marked],
        region: Option<SourceRange>,
        tail: Option<&Tail<'_>>,
    ) -> usize {
        let mut index = 0;
        // How many tokens of `tail` a call that began in `tokens` took as its arguments.
        let mut consumed = 0usize;

        while index < tokens.len() {
            if self.exhausted {
                return consumed;
            }

            let token = &tokens[index].token;

            // Only a *name* can be a macro. A keyword can be one too — `#define true 1` is legal, and
            // the lexer has no way to know it is looking at a directive's name — so both are looked up.
            if !could_be_a_macro_name(token.kind) {
                if !self.push(token.clone(), tokens[index].origin.clone(), region) {
                    return consumed;
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
                    return consumed;
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
                    return consumed;
                }
                index += 1;
                continue;
            }

            if self.depth >= MAX_DEPTH {
                self.note(ExpansionNote::TooDeep { name: name.clone() }, token.range);
                if !self.push(token.clone(), tokens[index].origin.clone(), region) {
                    return consumed;
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
                        written_in: definition.written_in,
                    };
                    // What follows the name is what the rescan of its body may still need: a body that ends in
                    // a function-like macro's name (`#define __MACHINEX64 __MACHINEZ`) is called by the tokens
                    // **after** it, and those are not in the body.
                    let after = Tail {
                        tokens: &tokens[index + 1..],
                        outer: tail,
                    };
                    let used = self.expand_body(&definition, &[], tokens, invocation, Some(&after));
                    index += 1;
                    self.advance_past(tokens.len(), &mut index, used, &mut consumed);
                }
                // A function-like macro: only a call. The standard allows layout between the name and the
                // `(` — `F (1)` is a call — and the `(` may be in whatever follows the replacement list this
                // name ended.
                Some(_) => {
                    let open = match tokens[index + 1..]
                        .iter()
                        .position(|marked| !is_trivia(marked.token.kind))
                    {
                        Some(offset) => (tokens[index + 1 + offset].token.kind == CppTokenKind::LeftParen)
                            .then_some(index + 1 + offset),
                        None => None,
                    };
                    let reaches_into_the_tail = tokens[index + 1..]
                        .iter()
                        .all(|marked| is_trivia(marked.token.kind))
                        && tail.is_some_and(Tail::starts_a_call);

                    if open.is_none() && !reaches_into_the_tail {
                        self.note(
                            ExpansionNote::NoArgumentList { name: name.clone() },
                            token.range,
                        );
                        if !self.push(token.clone(), tokens[index].origin.clone(), region) {
                            return consumed;
                        }
                        index += 1;
                        continue;
                    }

                    // A call that starts here and ends in the tail is read from one flat run, so the argument
                    // reader and the substitution see the same indices.
                    let flat: Vec<Marked>;
                    let (source, in_this_run, open) = match open {
                        Some(open) => (tokens, true, open),
                        None => {
                            let mut run = tokens[index..].to_vec();
                            if let Some(tail) = tail {
                                tail.copy_into(&mut run);
                            }
                            let open = run
                                .iter()
                                .skip(1)
                                .position(|marked| !is_trivia(marked.token.kind))
                                .map_or(1, |offset| offset + 1);
                            flat = run;
                            (flat.as_slice(), false, open)
                        }
                    };

                    let Some(arguments) = split_arguments(source, open) else {
                        self.note(
                            ExpansionNote::UnterminatedArgumentList { name: name.clone() },
                            token.range,
                        );
                        if !self.push(token.clone(), tokens[index].origin.clone(), region) {
                            return consumed;
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
                            return consumed;
                        }
                        index += 1;
                        continue;
                    }

                    // A name that ended a replacement list was written in a `#define`, and its arguments were
                    // written at the use: the two are offsets in different texts, so the call is placed where the
                    // reader can see it — from the `(`.
                    let call_from = if in_this_run {
                        token.range.start_offset
                    } else {
                        source[open].token.range.start_offset
                    };
                    let call_site = SourceRange::new(call_from, arguments.end_offset - call_from);
                    let invocation = MacroInvocation {
                        name: name.clone(),
                        definition: definition.range,
                        name_at: definition.name_range,
                        call_site,
                        written_in: definition.written_in,
                    };

                    if in_this_run {
                        let after = Tail {
                            tokens: &source[arguments.next_index..],
                            outer: tail,
                        };
                        let used =
                            self.expand_body(&definition, &arguments.groups, source, invocation, Some(&after));
                        index = arguments.next_index;
                        self.advance_past(tokens.len(), &mut index, used, &mut consumed);
                    } else {
                        // `source` is the name, the rest of this run, and the tail up to the call's end: what it
                        // took of the tail is everything past this run's own tokens.
                        let own = tokens.len() - index;
                        let after = Tail {
                            tokens: &source[arguments.next_index..],
                            outer: None,
                        };
                        let used =
                            self.expand_body(&definition, &arguments.groups, source, invocation, Some(&after));
                        consumed = arguments.next_index - own + used;
                        index = tokens.len();
                    }
                }
            }
        }

        consumed
    }

    /// Each argument of a call, macro-expanded in isolation — `None` where there is nothing in it to expand.
    ///
    /// Indexed by parameter position. An argument with no macro name in it is left to be substituted as written,
    /// which is both what the standard says for it and the common case.
    fn expand_arguments(
        &mut self,
        definition: &MacroDef,
        arguments: &[std::ops::Range<usize>],
        plain: &[Token],
        region: Option<SourceRange>,
    ) -> Vec<Option<Vec<Marked>>> {
        let Some(params) = &definition.params else {
            return Vec::new();
        };

        let mut expanded = Vec::with_capacity(params.len());
        for param in params {
            let raw = trim_trivia(argument_range_for(params, arguments, plain, &param.name));
            let has_a_macro = raw.iter().any(|token| {
                could_be_a_macro_name(token.kind) && self.macros.lookup(token.text()).definition().is_some()
            });
            if !has_a_macro {
                expanded.push(None);
                continue;
            }

            let marked: Vec<Marked> = raw.into_iter().map(Marked::from).collect();
            let saved = std::mem::take(&mut self.out);
            self.expand_into(&marked, region, None);
            let produced = std::mem::replace(&mut self.out, saved);

            let mut tokens: Vec<Marked> = produced
                .into_iter()
                .map(|token| Marked {
                    token: token.token,
                    origin: token.origin,
                })
                .collect();
            while tokens.last().is_some_and(|marked| is_trivia(marked.token.kind)) {
                tokens.pop();
            }
            let leading = tokens
                .iter()
                .take_while(|marked| is_trivia(marked.token.kind))
                .count();
            tokens.drain(..leading);
            expanded.push(Some(tokens));
        }
        expanded
    }

    /// Move past the tokens a nested expansion took from what followed it.
    ///
    /// `index` is the first token after the call; `used` of the tokens from there on were consumed by the
    /// expansion's own rescan. When that is more than this run has left, the rest came from the run's own tail.
    fn advance_past(&self, length: usize, index: &mut usize, used: usize, consumed: &mut usize) {
        let available = length - *index;
        if used <= available {
            *index += used;
        } else {
            *index = length;
            *consumed = used - available;
        }
    }

    /// Substitute a macro's arguments into its body and expand the result.
    ///
    /// `arguments` are slices of `tokens`, so an argument is a token sequence and not a string.
    ///
    /// Returns how many tokens of `after` the rescan consumed: the replacement list can end in the name of a
    /// function-like macro, and the arguments of that call are then what follows the invocation.
    fn expand_body(
        &mut self,
        definition: &MacroDef,
        arguments: &[std::ops::Range<usize>],
        tokens: &[Marked],
        invocation: MacroInvocation,
        after: Option<&Tail<'_>>,
    ) -> usize {
        // Only the tokens the arguments are made of are copied. The run this call sits in can be a whole file's
        // worth, and copying all of it for every macro call made expansion quadratic in the length of a run.
        let (offset, wanted) = match (arguments.first(), arguments.last()) {
            (Some(first), Some(last)) => (first.start, &tokens[first.start..last.end]),
            _ => (0, &tokens[..0]),
        };
        let plain: Vec<Token> = wanted.iter().map(|marked| marked.token.clone()).collect();
        let arguments: Vec<std::ops::Range<usize>> = arguments
            .iter()
            .map(|range| range.start - offset..range.end - offset)
            .collect();
        let arguments = arguments.as_slice();
        let region = match (tokens.first(), tokens.last()) {
            (Some(first), Some(last)) => Some(SourceRange::new(
                first.token.range.start_offset,
                last.token.range.end_offset() - first.token.range.start_offset,
            )),
            _ => None,
        };
        // **Arguments are macro-expanded before they are substituted** (C11 6.10.3.1), except where they are the
        // operand of `#` or `##`. The difference is observable: `#define S(x) S_(x)` / `#define S_(x) #x` makes
        // `S(LEVEL)` the string of what `LEVEL` expands to — `"0"` — and a rescan-only expander stringizes the
        // name. That is `_STL_STRINGIZE(_ITERATOR_DEBUG_LEVEL)` in every MSVC STL translation unit.
        //
        // This runs **before** the macro being invoked is hidden: the argument is read in the context of the call.
        let expanded = self.expand_arguments(definition, arguments, &plain, region);
        self.active.push(definition.name.clone());
        self.invocations.push(invocation.clone());
        self.depth += 1;
        // A token `#` or `##` makes is placed at the **outermost** call: a call written inside another macro's body
        // has its call site in that `#define`, and a position in another file is no place to put a token of this one.
        let mut operators_at = invocation.clone();
        if let Some(outermost) = self.invocations.first() {
            operators_at.call_site = outermost.call_site;
        }
        let substituted = substitute(definition, arguments, &plain, &operators_at, &expanded);

        // The rescan. Doing it through `expand_into` rather than by re-running `expand` on the whole
        // thing is what keeps the origin chain: a token that came from an argument and is *also* a
        // macro gets the inner expansion's origin, which is the one whose call site is on screen.
        //
        // The region is the macro body, so a token of the body is outside it and a token that came from an
        // argument — written at the call site — is inside. That is how the two stay distinguishable all
        // the way down.
        let used = self.expand_into(&substituted, region, after);

        self.depth -= 1;
        self.active.pop();
        self.invocations.pop();
        used
    }
}

/// The tokens that follow a run being expanded: what an expansion's rescan reads on once its own replacement list
/// is spent. Innermost run first, each pointing at the run it was nested in.
struct Tail<'a> {
    tokens: &'a [Marked],
    outer: Option<&'a Tail<'a>>,
}

impl Tail<'_> {
    /// Is the next token that is not layout a `(`?
    fn starts_a_call(&self) -> bool {
        let mut at = Some(self);
        while let Some(run) = at {
            if let Some(next) = run.tokens.iter().find(|marked| !is_trivia(marked.token.kind)) {
                return next.token.kind == CppTokenKind::LeftParen;
            }
            at = run.outer;
        }
        false
    }

    /// Every token of the tail, in order, appended to `out`.
    fn copy_into(&self, out: &mut Vec<Marked>) {
        let mut at = Some(self);
        while let Some(run) = at {
            out.extend(run.tokens.iter().cloned());
            at = run.outer;
        }
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
#[derive(Clone)]
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

impl Marked {
    /// A token of a macro **body**, whose chain the caller fills in when it pushes it.
    ///
    /// `substitute` cannot say which invocation the token belongs to (it is handed one invocation, not the
    /// chain), and saying `Source` — which is what a body's tokens look like when they are read — is *wrong* in
    /// a way that used to be caught by comparing offsets: see [`Expander::origin_of`], where the comparison and
    /// why it cannot be trusted for an inherited definition are documented.
    fn from_a_body(token: Token) -> Self {
        Marked {
            token,
            origin: Origin::Expanded {
                invocations: Vec::new(),
            },
        }
    }
}

impl FromIterator<Marked> for Vec<Token> {
    /// Keep the tokens and drop the origins, for the callers that only need the spelling.
    fn from_iter<T: IntoIterator<Item = Marked>>(iter: T) -> Self {
        iter.into_iter().map(|marked| marked.token).collect()
    }
}

/// Read an argument list whose opening `(` is at `open`.
///
/// Returns `None` when the list is not closed before the input ends, which is the state of a call being
/// typed and not an error worth reporting.
fn split_arguments(tokens: &[Marked], open: usize) -> Option<Arguments> {
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
    expanded: &[Option<Vec<Marked>>],
) -> Vec<Marked> {
    let Some(params) = &definition.params else {
        // An object-like macro is its body. The layout around the body goes: it is the space after the
        // macro's name and the newline that ended the directive, and it belongs to the `#define` rather
        // than to what the macro expands to. Keeping it would make `VERSION` expand to `" 3\n"`.
        return significant_tokens(&definition.body.tokens)
            .into_iter()
            .map(Marked::from_a_body)
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
                Some(Marked::from_a_body(body_tokens[right_index].clone()))
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
            // The left operand of `##` is used as written, like the right one.
            let pre_expanded = params
                .iter()
                .position(|param| &*param.name == token.text())
                .filter(|_| !paste_at.contains(&(index + 1)))
                .and_then(|position| expanded.get(position))
                .and_then(|argument| argument.as_ref());
            match pre_expanded {
                Some(argument) => out.extend(argument.iter().cloned()),
                None => {
                    let argument = argument_for(params, arguments, tokens, token.text());
                    out.extend(argument.into_iter().map(Marked::from));
                }
            }

            // A variadic parameter often has no argument at all — `LOG("x")` for
            // `#define LOG(f, ...)`. Nothing is substituted, which is why `__VA_ARGS__` disappearing
            // is normal rather than a bug.
            index += 1;
            continue;
        }

        out.push(Marked::from_a_body(token.clone()));
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

    // A space is written where the argument had layout between two tokens — `sizeof(int)` stays `sizeof(int)`
    // and `a + b` stays `a + b` — which is what `cl` and `clang` print. Layout is either a trivia token (a stream
    // that kept them) or a gap between the two tokens' positions (one that did not).
    let mut previous: Option<&Token> = None;
    let mut saw_layout = false;
    for token in argument.iter() {
        if is_trivia(token.kind) {
            saw_layout = true;
            continue;
        }
        if let Some(previous) = previous
            && (saw_layout || previous.range.end_offset() != token.range.start_offset)
        {
            text.push(' ');
        }
        saw_layout = false;
        previous = Some(token);
        // Within the string, `"` and `\` have to be escaped or the literal does not re-lex.
        for character in token.text().chars() {
            match character {
                '"' => text.push_str("\\\""),
                '\\' => text.push_str("\\\\"),
                other => text.push(other),
            }
        }
    }

    text.push('"');

    Token::new(CppTokenKind::StringLiteral, text, call_site)
}

/// `a ## b`: join two tokens into one, then read the result back as a token.
///
/// The re-lex is the point. `+` pasted with `=` is `+=`, and a caller that concatenated the spelling
/// would hand the parser two tokens where the language has one.
///
/// The joined spelling exists nowhere in the file, so the token's range cannot cover it: it covers the
/// **left** token as written, which is the position a consumer reports against, and the joined text is
/// carried by the token itself. The origin is [`Origin::Pasted`], whose call site is what a diagnostic
/// points at.
///
/// It used to be the left token's start with the *joined* length, and that is a range over whatever happens
/// to follow the left token: `#define P(a, b) a##b` used as `P(x, y)` claimed a range over `x,` — the
/// argument and the comma that separates it — so a consumer slicing the source by range got a token's text
/// that was not the token's text, and a long enough paste would have sliced past the end of the file.
fn paste(left: &Token, right: &Token) -> Vec<Marked> {
    let joined = format!("{}{}", left.text(), right.text());
    let range = left.range;

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
