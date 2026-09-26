//! The **cooked token stream**: what a compiler would hand to its parser.
//!
//! A file's own stream has directives in it, both branches of every `#if`, and macro names where the
//! language wants their expansions. A compiler removes all three before parsing, and so does this — it is
//! the step the architecture calls "the preprocessor produces cooked tokens, the tree is a view of them".
//!
//! # What is dropped, and what is kept
//!
//! * **Directives** are consumed. They are lines, not constructs; nothing downstream parses them a second
//!   time, and a parser that met one would have to have a rule for it.
//! * **A branch that is not compiled is dropped**, with its span reported in [`CookedStream::inactive`].
//!   Which branch that is comes from the same [`Branch::holds`] the rest of the layer uses, evaluated
//!   against the macros that are **in force at that point in the file** — not against the file's final
//!   table, which is a different question.
//! * **A `#define` in a branch that is not compiled does not take effect.** This stream keeps its *own*
//!   [`MacroTable`], fed only by definitions the walk actually reached. That is the difference from
//!   [`crate::preprocess()`], which records every definition because a consumer asking "where is this name
//!   defined" wants to see the ones that are not compiled too.
//! * **Macro invocations are replaced by their expansions**, recursively, with the hide set, `#` and `##`
//!   ([`crate::expand()`]). Every token that comes out of a body carries its [`Origin`] chain, so a consumer
//!   can still tell where it was written.
//! * **Trivia is dropped**, because it is not part of what a compiler passes on. The expander keeps
//!   `space_before` for a caller that has to re-spell the stream.
//! * **`#include` is a boundary**: the included file's tokens are not in this stream. Cooking a
//!   *translation unit* means cooking each file and stitching them in include order, which needs the
//!   include graph — that is the next step, and this module deliberately stops at the file.
//!
//! # A condition nobody can decide
//!
//! With no toolchain, most conditions name macros that live in headers nobody has read, so they are
//! `None` — "cannot say". The convention here is C's: **an identifier that is not defined evaluates to
//! `0`**, so an undecided branch is treated as not compiled, and the count is reported in
//! [`CookedStream::assumed_undefined`] rather than being silently believed. With a real configuration (the
//! architecture's ladder, levels 1 and 2) the count is zero, and the stream is the compiler's own.

use cpp_parser::{CppTokenData, SourceRange};

use crate::{
    Origin,
    directive::{Directive, DirectiveKind},
    expand::{Diagnostic, ExpandedToken, expand},
    guard::Branch,
    macros::MacroTable,
    token::is_trivia,
};

/// One file, preprocessed: the tokens a compiler would parse, and what was removed on the way.
#[derive(Debug, Clone, Default)]
pub struct CookedStream {
    /// The tokens of the compiled configuration, in order, trivia removed.
    pub tokens: Vec<ExpandedToken>,
    /// Why something was left unexpanded — a macro nobody holds the body of, a budget that ran out.
    pub diagnostics: Vec<Diagnostic>,
    /// The spans of the regions that were **not** compiled, in source order.
    pub inactive: Vec<SourceRange>,
    /// How many conditional branches were decided by **C's rule that an identifier which is not defined
    /// evaluates to `0`**, rather than by a definition this file has.
    ///
    /// Zero means every branch in the file was decided from what the file itself says, which is as exact as a
    /// stream can be. Non-zero means the stream is one *possible* configuration — the one a compiler with no
    /// `-D` flags would pick — and a consumer should say so rather than pretend otherwise. With a toolchain and
    /// an include closure (the architecture's levels 1 and 2) the number falls, because the names these
    /// conditions ask about are the ones the headers define.
    pub assumed_undefined: usize,
    /// Did the expansion budget run out? When it did, `tokens` is a prefix rather than the whole file.
    pub exhausted: bool,
}

impl CookedStream {
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// The cooked tokens as plain tokens, for a consumer that does not need the origins.
    pub fn plain(&self) -> Vec<crate::token::Token> {
        self.tokens.iter().map(|it| it.token.clone()).collect()
    }

    /// The spellings, separated by a single space — what a `-E` comparison wants.
    pub fn spellings(&self) -> String {
        self.tokens
            .iter()
            .map(|it| it.token.text())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Render the stream as **text a parser can read**, with the map back to the file.
    ///
    /// This is what turns a cooked token stream into a real syntax tree without changing the parser: the
    /// grammar reads `&str`, so the stream is spelled out once, and every token's place in that rendering is
    /// recorded next to the place it was *written*. See [`RenderedCooked`] for what the rendering is and is
    /// not — the short version is that its offsets are **not** file offsets, and the map is what says where
    /// anything actually came from.
    pub fn render(&self) -> RenderedCooked {
        let mut text = String::new();
        let mut spans = Vec::with_capacity(self.tokens.len());

        for cooked in &self.tokens {
            // One space between tokens, always. The parser reads kinds and not layout, so this costs nothing
            // there — and it cannot merge two spellings into one token, which is the one way layout *can*
            // change a parse. (Where tokens really do touch, like `+` and `=` pasted into `+=`, they are
            // already one token by the time they get here.)
            if !text.is_empty() {
                text.push(' ');
            }

            let start = text.len();
            text.push_str(cooked.token.text());
            spans.push(RenderedSpan {
                cooked: SourceRange::new(start, text.len() - start),
                written: written_at(cooked),
                origin: cooked.origin.clone(),
            });
        }

        RenderedCooked { text, spans }
    }
}

/// Where a cooked token was written in the file.
///
/// * a token the file wrote: its own range;
/// * a token out of a macro body: the body token's range, which is inside the `#define`. That is the
///   **spelling** location — where the text actually is — and it is the answer to "where did this come from";
///   a consumer that wants the place a reader can act on (a diagnostic) takes the *call site* from the span's
///   `origin` instead, which is why the span carries both;
/// * a token `##` or `#` produced: the **call site**, because its spelling exists nowhere in the file and the
///   origin is the only thing that knows where the operator ran.
fn written_at(cooked: &ExpandedToken) -> Option<SourceRange> {
    match &cooked.origin {
        Origin::Pasted { call_site } | Origin::Stringized { call_site } => Some(*call_site),
        Origin::Source | Origin::Expanded { .. } => Some(cooked.token.range),
    }
}

/// A cooked stream spelled out as text, and where each token in it came from.
///
/// # What the text is, and what it is not
///
/// It is a **rendering artifact**: the tokens' spellings in order, separated by single spaces. It is what the
/// grammar needs, and nothing else may be read out of it. In particular its offsets are **not** positions in
/// the file — a token out of a macro body stands where its body was written, and the same spelling may appear
/// in a thousand places. [`RenderedCooked::written_at`] and [`RenderedCooked::written_span`] are the only
/// correct way to ask where something came from, and a consumer that treats a tree offset as a file offset is
/// wrong in a way nothing will catch.
///
/// It is also **not lossless**, and cannot be: the directives are gone, one branch of every conditional is
/// gone, and a pasted token's spelling exists nowhere. The file's own bytes are the token stream's business,
/// not this text's.
#[derive(Debug, Clone, Default)]
pub struct RenderedCooked {
    pub text: String,
    /// One entry per token of the stream it was rendered from, in the same order.
    pub spans: Vec<RenderedSpan>,
}

/// One token's place in the rendering, and the place it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedSpan {
    /// Where the spelling is in [`RenderedCooked::text`].
    pub cooked: SourceRange,
    /// Where it was written in the file, when that is known. `None` for a token whose origin says nothing — a
    /// level-2 stream will also produce `None` for a token that came out of another file, until files are
    /// identified rather than ranged.
    pub written: Option<SourceRange>,
    pub origin: Origin,
}

impl RenderedCooked {
    /// The place a token of the rendering was written, by its offset in the rendering.
    ///
    /// A diagnostic inside the rendered tree is reported at *this* range — the file the reader can open —
    /// which is the whole reason the map exists.
    pub fn written_at(&self, cooked_offset: usize) -> Option<SourceRange> {
        let index = self
            .spans
            .partition_point(|span| span.cooked.end_offset() <= cooked_offset);
        self.spans.get(index).and_then(|span| span.written)
    }

    /// The file span a **node** of the rendered tree came from: from the first token it covers to the last.
    ///
    /// `None` when the node covers no token (an empty node is a real thing in a tolerant tree) or when none of
    /// its tokens has a known place.
    pub fn written_span(&self, cooked: SourceRange) -> Option<SourceRange> {
        let first = self
            .spans
            .partition_point(|span| span.cooked.end_offset() <= cooked.start_offset);
        let last = self
            .spans
            .partition_point(|span| span.cooked.start_offset < cooked.end_offset());

        let chosen = self.spans[first..last]
            .iter()
            .filter_map(|span| span.written)
            .collect::<Vec<_>>();
        let start = chosen.iter().map(|range| range.start_offset).min()?;
        let end = chosen.iter().map(|range| range.end_offset()).max()?;
        Some(SourceRange::new(start, end.saturating_sub(start)))
    }
}

/// A conditional region the walk is inside.
///
/// `live` is whether the tokens in it are compiled, which is a question about the whole stack: a region
/// inside a region that is not compiled is not compiled either, whatever its own condition says.
struct Region {
    /// Was the code *around* this `#if` compiled?
    outer_is_live: bool,
    /// A branch of this region has already been taken.
    taken: bool,
    /// Are the tokens of the current branch compiled?
    live: bool,
}

/// Cook one file, with nothing defined before it — the architecture's **level 0**.
///
/// See the module documentation for what is dropped, what is kept, and what an undecidable condition means.
/// A name that comes from a header, from a builtin or from a `-D` is left alone here (and reported in
/// `diagnostics` when it is invoked); [`cook_with`] is the same walk with a configuration behind it.
pub fn cook(source: &str, tokens: &[CppTokenData]) -> CookedStream {
    cook_with(source, tokens, &MacroTable::new())
}

/// A definition written the way a directive writes it: `NAME(params) body`.
///
/// The parameter list is what makes a function-like definition expandable — substitution is by parameter, and a
/// body without one would leave the parameter names standing where arguments belong. `None` is an object-like
/// macro (or a caller that stores no parameters), and the text is then just the name and the body.
fn definition_text(name: &str, parameters: Option<&str>, body: &str) -> String {
    let head = match parameters {
        Some(list) => format!("{name}{list}"),
        None => name.to_string(),
    };
    if body.is_empty() {
        head
    } else {
        format!("{head} {body}")
    }
}

/// What a file's **includes** contribute, as the table [`cook_with`] starts from.
#[derive(Debug, Clone, Default)]
pub struct Configuration {
    pub table: MacroTable,
    /// Definitions the evidence carries as **function-like without their parameter names**.
    ///
    /// A function-like macro is substituted *by parameter*, so a body without a parameter list cannot be
    /// expanded: the names in it would be left as themselves instead of being replaced by the arguments. Those
    /// definitions are therefore not in the table, and this counts them — it is the size of the gap between the
    /// evidence the index keeps and what expansion needs.
    pub function_like_without_parameters: usize,
    /// Bodies that arrive through the **in-force channel only** — no definition, so nothing that says whether the
    /// macro takes parameters.
    ///
    /// They are **not** used, and the measurement is why. A body is not a definition: `#define _STL_MSG(...)` and
    /// `#define _STD_BEGIN` look the same to a channel that carries only text, and pasting the first one verbatim
    /// puts its `#` and its parameter names into the stream — `( "warning " # NUMBER ": " MESSAGE )` is what the
    /// cooker produced, and the parser then reported `expected ; after expression` in three headers (`concepts`,
    /// `compare`, `bit`) that had been clean. Pasting a body that may need arguments is exactly the guess this
    /// module refuses to make, so those bodies are counted and left alone: the channel was built for *reading* a
    /// body (`_STD_BEGIN` tells a rule that a namespace is being opened), and telling a rule what a body says is a
    /// weaker claim than being able to substitute into it.
    pub in_force_without_a_parameter_list: usize,
    /// Definitions with no body stored at all — nothing to expand.
    pub without_a_body: usize,
    /// Definitions whose replacement list did not read as a macro definition.
    pub unreadable: usize,
}

/// Build the table a level-2 cook starts from, out of what the includes contribute.
///
/// The environment's offsets are positions in **this** file ("from the end of the `#include` that brought it
/// in"), which is exactly what [`MacroTable`] looks a definition up by, so they are carried over unchanged: a
/// converted definition comes into force here at the point it came into force, not where it was written. Its
/// `range` says the same thing and is not a position in the header it came from — nothing reads it as one.
///
/// **Bodies that only the in-force channel has are used only when the caller classified them as object-like**
/// — see [`Configuration::in_force_without_a_parameter_list`] for the measurement that decided it, and
/// [`cpp_parser::InForceBody`] for why the flag is an `Option`.
pub fn configuration_from_environment(environment: &cpp_parser::MacroEnvironment) -> Configuration {
    configuration_from_environment_with(environment, false)
}

/// [`configuration_from_environment`], with a decision the measurement left open.
///
/// `use_in_force_bodies` is whether the **in-force channel** may supply definitions for macros that take no
/// parameters. The channel exists because a `#define` inside a conditional region is a body and not a
/// definition, and it is where MSVC's `_ACRTIMP` arrives from `corecrt.h` — so turning it on is what makes the
/// `_ACRTIMP`-headed declarations of `ucrt` expand.
///
/// **It is off by default, and that is a measurement rather than a preference.** With the flag on
/// (`std_probe … --seeds --closure --cooked`):
///
/// ```text
/// 255 files: clean 242 → 241 | messages 99 → 81 | kinds 3 → 6
/// 109 files: clean 100 →  98 | messages 74 → 76 | kinds 2 → 6
/// ```
///
/// It **fixes the seven `_ACRTIMP`-headed files** (`corecrt_wstring.h`, `corecrt_wio.h`, `corecrt_wtime.h`,
/// `stat.h`, `stdlib.h`, `ctype.h`, `winnt.h`) and **breaks others** (`xstring` among them) — expanding a body
/// exposes the grammar gaps behind it, which is the same story the four real gaps already tell. A regression in
/// *clean files* is not a trade this layer gets to make on its own, so the switch stays off until those gaps are
/// fixed; the code is here, tested both ways, for whoever does that.
pub fn configuration_from_environment_with(
    environment: &cpp_parser::MacroEnvironment,
    use_in_force_bodies: bool,
) -> Configuration {
    // A body in force is used **only when the caller said the macro is object-like** — and only when this
    // function was asked for it. One whose parameters are unknown cannot be substituted into (`None` is not
    // "object-like"), and one the caller says takes parameters must not be pasted. Everything left is counted:
    // see `in_force_without_a_parameter_list` and the function's own note for the measurement.
    // **Usable** means: object-like, or function-like *with* its parameter list — a body that takes arguments can
    // be expanded only when the arguments have names to be substituted into, and a body nobody classified cannot
    // be expanded at all. What is left is counted, not guessed at.
    let usable = |function_like: Option<bool>, parameters: Option<&str>| match function_like {
        Some(false) => true,
        Some(true) => parameters.is_some(),
        None => false,
    };
    let in_force: Vec<(&str, String)> = if use_in_force_bodies {
        environment
            .bodies_in_force()
            .filter(|(_, function_like, parameters, _)| usable(*function_like, *parameters))
            .map(|(name, _, parameters, body)| {
                (name, definition_text(name, parameters, body))
            })
            .collect()
    } else {
        Vec::new()
    };

    let mut out = Configuration {
        in_force_without_a_parameter_list: environment.bodies_in_force().count() - in_force.len(),
        ..Configuration::default()
    };

    let definitions: Vec<(&str, usize, bool, Option<String>)> = environment
        .definitions()
        .map(|(name, at, function_like, body)| {
            let text = body.map(|body| definition_text(name, environment.parameters_of(name, at), body));
            (name, at, function_like, text)
        })
        .chain(
            in_force
                .iter()
                .map(|(name, text)| (*name, 0usize, false, Some(text.clone()))),
        )
        .collect();

    for (_, at, function_like, body) in definitions {
        if function_like {
            // No parameter names in the evidence — see the field's note.
            out.function_like_without_parameters += 1;
            continue;
        }

        let Some(text) = body else {
            out.without_a_body += 1;
            continue;
        };

        // `parse_define` reads a definition written the way a directive writes it: the name, an optional
        // parameter list, then the replacement list. That is what `definition_text` builds — the same reader the
        // directive layer uses, so a definition means the same thing wherever it comes from.
        let (tokens, _) = cpp_parser::lex(&text, &cpp_parser::LexerConfig::default());
        let range = SourceRange::new(at, text.len());
        let tokens: Vec<crate::token::Token> = tokens
            .iter()
            .map(|token| {
                crate::token::Token::new(
                    token.kind,
                    &text[token.range.start_offset..token.range.end_offset()],
                    token.range,
                )
            })
            .collect();

        match crate::macros::parse_define(&tokens, range) {
            Some(definition) => out.table.define(definition),
            None => out.unreadable += 1,
        }
    }

    out
}
/// Cook one file with the macros a **configuration** starts it with.
///
/// `initial` is what the compilation defines before the file is read: the compiler's own builtins (`-dM`),
/// the project's `-D`s, and — at the architecture's level 2 — the macros the include closure has in force at
/// the file's first line. It is a [`MacroTable`] rather than a list of values because expansion needs bodies
/// and not only definedness: `#if __cplusplus >= 201703L` asks for a value, and `__cplusplus` in the file's
/// own text is replaced by one.
///
/// This is what makes the difference between a stream that follows C's rule for names nobody defines — see
/// [`CookedStream::assumed_undefined`] — and the stream a compiler would actually parse.
pub fn cook_with(
    source: &str,
    tokens: &[CppTokenData],
    initial: &MacroTable,
) -> CookedStream {
    let directives = crate::directive::scan_directives(source, tokens);

    // The macros this walk has actually reached, on top of what the configuration supplied. A `#define` in
    // the file shadows a builtin of the same name, which is exactly what the table's positional lookup does
    // when the file's definition is written later than offset 0.
    let mut live = initial.clone();
    let mut regions: Vec<Region> = Vec::new();

    let mut out = CookedStream::default();
    let mut run_from = 0usize;
    let mut directive_index = 0usize;

    for token_index in 0..=tokens.len() {
        let is_a_directive = directives
            .get(directive_index)
            .is_some_and(|spanned| spanned.range.start_offset == token_start(tokens, token_index));
        if !is_a_directive {
            continue;
        }
        let spanned = &directives[directive_index];
        let was_live = is_live(&regions);

        // Everything before the directive is one run of ordinary tokens. No `#define` can take effect
        // inside a run, so the table is constant across it, and the lookup offset is the run's start.
        if run_from < token_index {
            if was_live {
                expand_run(source, tokens, run_from, token_index, &live, &mut out);
            } else {
                let span = span_of(tokens, run_from, token_index);
                merge_span(&mut out.inactive, span);
            }
        }

        let kind = spanned.directive.kind();
        match kind {
            DirectiveKind::If | DirectiveKind::Ifdef | DirectiveKind::Ifndef => {
                let holds = decide(&spanned.directive, spanned.range, &live, tokens, token_index);
                regions.push(Region {
                    outer_is_live: was_live,
                    taken: holds == Some(true),
                    live: was_live && holds == Some(true),
                });
                if holds.is_none() || rests_on_an_undefined_name(&spanned.directive, &live, token_index, tokens) {
                    out.assumed_undefined += 1;
                }
            }
            DirectiveKind::Elif => {
                let holds = decide(&spanned.directive, spanned.range, &live, tokens, token_index);
                if let Some(region) = regions.last_mut() {
                    region.live = region.outer_is_live && !region.taken && holds == Some(true);
                    if holds == Some(true) {
                        region.taken = true;
                    }
                }
                if holds.is_none() || rests_on_an_undefined_name(&spanned.directive, &live, token_index, tokens) {
                    out.assumed_undefined += 1;
                }
            }
            DirectiveKind::Else => {
                if let Some(region) = regions.last_mut() {
                    region.live = region.outer_is_live && !region.taken;
                    region.taken = true;
                }
            }
            DirectiveKind::Endif => {
                regions.pop();
            }
            DirectiveKind::Define => {
                if was_live
                    && let Directive::Define(define) = &spanned.directive
                    && let Some(definition) = &define.macro_def
                {
                    live.define(definition.clone());
                }
            }
            DirectiveKind::Undef => {
                if was_live
                    && let Directive::Undef { name: Some(name) } = &spanned.directive
                {
                    live.undefine(name, spanned.range.start_offset);
                }
            }
            _ => {}
        }

        // A directive belongs to the region it is written in: the `#if` that opens a branch nobody takes
        // and the `#endif` that closes it are part of the span a consumer greys out, which is also what
        // makes the pieces of one dead branch join into a single range.
        if !was_live || !is_live(&regions) {
            merge_span(&mut out.inactive, spanned.range);
        }

        directive_index += 1;
        run_from = token_index_after(tokens, spanned.range.end_offset());
    }

    // What is left after the last directive — and the whole file when there is none.
    if run_from < tokens.len() {
        if is_live(&regions) {
            expand_run(source, tokens, run_from, tokens.len(), &live, &mut out);
        } else {
            let span = span_of(tokens, run_from, tokens.len());
            merge_span(&mut out.inactive, span);
        }
    }

    out
}

/// Are the tokens of the innermost region compiled?
fn is_live(regions: &[Region]) -> bool {
    regions.last().is_none_or(|region| region.live)
}

/// The first token index at or after `offset`.
fn token_index_after(tokens: &[CppTokenData], offset: usize) -> usize {
    tokens.partition_point(|token| token.range.start_offset < offset)
}

/// The start offset of the token at `index`, or the end of the stream.
fn token_start(tokens: &[CppTokenData], index: usize) -> usize {
    tokens
        .get(index)
        .map_or(usize::MAX, |token| token.range.start_offset)
}

/// The span covering the tokens `from..to`.
fn span_of(tokens: &[CppTokenData], from: usize, to: usize) -> SourceRange {
    match (tokens.get(from), tokens.get(to.saturating_sub(1))) {
        (Some(first), Some(last)) => SourceRange::new(
            first.range.start_offset,
            last.range.end_offset().saturating_sub(first.range.start_offset),
        ),
        _ => SourceRange::new(0, 0),
    }
}

/// Add a span to a list of them, joining it to the previous one when they touch.
///
/// A region is dropped in as many pieces as it has directives in it, and a consumer showing "this was not
/// compiled" wants one range per branch rather than one per line.
fn merge_span(spans: &mut Vec<SourceRange>, span: SourceRange) {
    match spans.last_mut() {
        Some(last) if last.end_offset() >= span.start_offset => {
            let start = last.start_offset;
            let end = last.end_offset().max(span.end_offset());
            *last = SourceRange::new(start, end.saturating_sub(start));
        }
        _ => spans.push(span),
    }
}

/// Would the directive's condition hold, in the state the walk is in?
fn decide(
    directive: &Directive,
    range: SourceRange,
    live: &MacroTable,
    tokens: &[CppTokenData],
    token_index: usize,
) -> Option<bool> {
    let branch = branch_of(directive, range)?;
    let offset = token_start(tokens, token_index);
    let macros = crate::preprocess::PositionalMacros {
        table: live,
        offset,
    };
    branch.holds(&macros)
}

/// Does deciding this condition need a name the file never defines?
///
/// `#if FOO` where nothing defines `FOO` is answered by C's rule — an identifier that is not defined
/// evaluates to `0` — and the answer is only as good as that assumption. Counting the branches that rest on
/// it is what [`CookedStream::assumed_undefined`] reports, and it is the number a configuration (the
/// architecture's ladder) is expected to drive to zero.
fn rests_on_an_undefined_name(
    directive: &Directive,
    live: &MacroTable,
    token_index: usize,
    tokens: &[CppTokenData],
) -> bool {
    let offset = token_start(tokens, token_index);
    let unknown = |name: &str| live.get_at(name, offset).is_none();

    match directive {
        Directive::Ifdef { name, .. } => unknown(name),
        Directive::Conditional { condition, .. } => condition.iter().any(|token| {
            token.is_identifier() && !matches!(token.text(), "defined" | "true" | "false") && unknown(token.text())
        }),
        _ => false,
    }
}

/// The conditional branch a directive writes, if it writes one.
///
/// The same construction [`crate::preprocess`] uses, so a condition is asked the same way in both places.
fn branch_of(directive: &Directive, range: SourceRange) -> Option<Branch> {
    use crate::directive::DirectiveKind as Kind;

    let kind: Kind = directive.kind();
    if !(kind.opens_a_condition() || kind.closes_a_condition()) {
        return None;
    }

    let (tokens, name) = match directive {
        Directive::Conditional { condition, .. } => (condition.clone(), None),
        Directive::Ifdef { name, .. } => (Vec::new(), Some(name.clone())),
        _ => (Vec::new(), None),
    };

    Some(Branch {
        kind,
        tokens,
        name,
        range,
    })
}

/// Expand the tokens `from..to` against the macros in force there, and keep what is not trivia.
fn expand_run(
    source: &str,
    tokens: &[CppTokenData],
    from: usize,
    to: usize,
    live: &MacroTable,
    out: &mut CookedStream,
) {
    let run = crate::directive::to_tokens(source, &tokens[from..to]);
    let offset = token_start(tokens, from);
    let macros = crate::preprocess::PositionalMacros {
        table: live,
        offset,
    };

    let expansion = expand(&run, &macros);
    out.diagnostics.extend(expansion.diagnostics);
    out.exhausted |= expansion.exhausted;
    out.tokens.extend(
        expansion
            .tokens
            .into_iter()
            .filter(|token| !is_trivia(token.token.kind)),
    );
}
