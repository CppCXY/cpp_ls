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
                reported: cooked.diagnostic_range(),
                origin: cooked.origin.clone(),
            });
        }

        RenderedCooked { text, spans }
    }
}

/// Where a cooked token was written **in this file**, when it was written in it at all.
///
/// * a token the file wrote: its own range;
/// * a token out of a macro body the file itself wrote: the body token's range, which is inside that `#define`
///   — the **spelling** location, and the answer to "where did this come from";
/// * a token out of a macro body that came from **another file** (or from a definition reconstructed out of text
///   alone): `None`. The spelling is not in this file, this map is of one file, and the honest answer is that
///   nobody here can say — the caller that wants it follows the span's `origin`, whose invocations now carry
///   [`crate::macros::MacroFile`];
/// * a token `##` or `#` produced: the **call site**, because its spelling exists nowhere in the file and the
///   origin is the only thing that knows where the operator ran.
///
/// A position that is not in this file is never returned, and that is the fix rather than a nicety: a body token of
/// an inherited macro used to be reported at its offset in the *reconstruction* — 7 inside a 13-byte line — which a
/// consumer slices into the file it has, printing whatever happens to be there.
fn written_at(cooked: &ExpandedToken) -> Option<SourceRange> {
    match &cooked.origin {
        Origin::Pasted { call_site } | Origin::Stringized { call_site } => Some(*call_site),
        Origin::Source => Some(cooked.token.range),
        Origin::Expanded { invocations } => match invocations.first().map(|it| it.written_in) {
            // The outermost invocation is the call the reader can see, and it is in this file; the *spelling* is
            // in the body's file, and it is the body's file that decides whether this map can name it.
            Some(Some(crate::macros::MacroFile::Here)) => Some(cooked.token.range),
            _ => None,
        },
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

/// One token's place in the rendering, the place it was written, and the place to act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedSpan {
    /// Where the spelling is in [`RenderedCooked::text`].
    pub cooked: SourceRange,
    /// Where it was **written in this file**, when it was written in this file at all.
    ///
    /// `None` for a token whose spelling is not here: one that came out of a macro body belonging to another file,
    /// or out of a definition this crate reconstructed from text that carried no position. The span's `origin` is
    /// what says where it really is — its invocations carry [`crate::macros::MacroFile`] — and this map answers for
    /// *this* file only, which is why it stops at `None` rather than handing over an offset from somewhere else.
    pub written: Option<SourceRange>,
    /// Where to **report** this token: always a position in this file.
    ///
    /// The token's own range when the file wrote it, and the outermost call site when a macro produced it — the
    /// text the reader can see, rather than a line inside a `#define` three headers up. A diagnostic wants this
    /// one and a "where did this text come from" question wants [`RenderedSpan::written`]; the two are different
    /// questions and a single field answered both badly (see `written_at`, where the body offset of an inherited
    /// macro used to be handed out as a position in this file).
    pub reported: SourceRange,
    pub origin: Origin,
}

impl RenderedCooked {
    /// The place a token of the rendering was written **in this file**, by its offset in the rendering.
    ///
    /// `None` when the spelling is elsewhere — another file's macro body, or a reconstruction nobody can open.
    /// [`RenderedCooked::reported_at`] is the other question ("where do I point a diagnostic"), and it is the one
    /// a consumer that has a tree offset and an error wants.
    pub fn written_at(&self, cooked_offset: usize) -> Option<SourceRange> {
        self.span_at(cooked_offset)?.written
    }

    /// The place a token of the rendering should be **reported**, by its offset in the rendering.
    ///
    /// Always a position in this file: the call site for a token a macro produced, the token's own range
    /// otherwise. `None` only when the offset is past the last span.
    pub fn reported_at(&self, cooked_offset: usize) -> Option<SourceRange> {
        Some(self.span_at(cooked_offset)?.reported)
    }

    /// The span covering an offset of the rendering — the separator between two tokens belongs to the token after
    /// it, which is what makes an offset that landed on whitespace still answer.
    fn span_at(&self, cooked_offset: usize) -> Option<&RenderedSpan> {
        let index = self
            .spans
            .partition_point(|span| span.cooked.end_offset() <= cooked_offset);
        self.spans.get(index)
    }

    /// The file span a **node** of the rendered tree came from: from the first token it covers to the last.
    ///
    /// `None` when the node covers no token (an empty node is a real thing in a tolerant tree) or when none of
    /// its tokens has a known place — which is now a real state rather than a corner: a node built out of tokens
    /// that came from another file's macro bodies has no place **in this file**, and the caller that needs one
    /// asks the tokens' origins.
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

/// A body that only the **in-force** channel carries: its name, its parameter list, its text.
///
/// Kept as a tuple rather than a struct because it never leaves this function: the environment hands it over a
/// [`cpp_parser::BodyFacts`], this is what survives the filter, and the loop below destructures it in place.
type InForceDefinition<'a> = (&'a str, Option<std::sync::Arc<str>>, std::sync::Arc<str>);
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
    ///
    /// **The gap is now only the missing list, not the arity.** A function-like definition *with* its list is
    /// expanded like any other, because the list is what substitution needs and
    /// [`cpp_parser::MacroFacts::parameters_of`] supplies it; the count that used to be "every function-like
    /// definition in the closure" was measured at ... see the module's own census.
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

/// A `#define`, parsed **once per run** rather than once per file that sees it.
///
/// # The measurement this exists for
///
/// A cooked census of the 255-file SDK corpus spent **12.2 s** of its 40 s in
/// [`configuration_from_environment_with`] and another **3.9 s** copying what came out of it into the table the
/// expansion starts from — for 42 939 distinct macro events handed to 255 files, which is 2.5 million `format!`
/// calls, 2.5 million `lex` calls and 2.5 million `parse_define` calls, each producing the *same* definition as
/// the file before it. The definition does not depend on the file: only the **offset the binding is in force
/// from** does, and that lives on the table's binding rather than in the definition.
///
/// So a run holds one of these, and the second file to see a definition pays a hash lookup and a refcount bump.
#[derive(Default)]
pub struct ParsedDefinitions {
    /// Keyed by what identifies a definition: its name, its parameter list and its body. `Arc<str>` hashes by
    /// its contents, so the key is the text — the same thing [`definition_text`] would have built, without
    /// building it to find out whether it is already known.
    parsed: std::collections::HashMap<DefinitionKey, Option<std::sync::Arc<crate::macros::MacroDef>>>,
}

/// What identifies a definition: exactly what [`definition_text`] writes, in the three pieces it writes it from.
type DefinitionKey = (Box<str>, Option<std::sync::Arc<str>>, std::sync::Arc<str>);



impl ParsedDefinitions {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many distinct definitions have been parsed — the number that used to be the number of *files* times
    /// the number of definitions, and is now the corpus's own vocabulary.
    pub fn len(&self) -> usize {
        self.parsed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parsed.is_empty()
    }

    /// The definition `name`/`parameters`/`body` spells, parsed on the first ask and remembered afterwards.
    ///
    /// `None` is "this text is not a definition this reader can put back together" — a fact about the text, so it
    /// is remembered too: a body that does not read once does not read on the two hundredth file either.
    ///
    /// `pub(crate)` because a *unit's* definitions are read through it in one pass
    /// ([`crate::TranslationUnit::definitions`]): the same cache, one caller that knows the whole timeline
    /// instead of one caller per file.
    pub(crate) fn definition(
        &mut self,
        name: &str,
        parameters: Option<&std::sync::Arc<str>>,
        body: &std::sync::Arc<str>,
    ) -> Option<std::sync::Arc<crate::macros::MacroDef>> {
        let key: DefinitionKey = (Box::from(name), parameters.cloned(), std::sync::Arc::clone(body));

        if let Some(known) = self.parsed.get(&key) {
            return known.clone();
        }

        let definition = parse_a_definition(&definition_text(
            name,
            parameters.map(|list| &**list),
            body,
        ))
        .map(std::sync::Arc::new);

        self.parsed.insert(key, definition.clone());
        definition
    }
}

/// `#define NAME(params) body` → the definition it spells: the same reader the directive layer uses, so a
/// definition means the same thing wherever it came from.
///
/// **The ranges are the reconstruction's, and the answer says so** ([`MacroFile`]): this text was written out by
/// [`definition_text`] from a name, a parameter list and a body that arrived as *text*, so an offset in it is an
/// offset in no file. `parse_define` answers [`MacroFile::Here`] because the tokens a *file* wrote are positions
/// in that file; a caller that reconstructs has to say so itself, and this is the one caller that reconstructs.
fn parse_a_definition(text: &str) -> Option<crate::macros::MacroDef> {
    let (tokens, _) = cpp_parser::lex(text, &cpp_parser::LexerConfig::default());
    let range = SourceRange::new(0, text.len());
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

    let mut definition = crate::macros::parse_define(&tokens, range)?;
    definition.written_in = None;
    Some(definition)
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
pub fn configuration_from_environment(environment: &dyn cpp_parser::MacroFacts) -> Configuration {
    configuration_from_environment_with(environment, true)
}

/// [`configuration_from_environment`], with a decision the measurement left open.
///
/// `use_in_force_bodies` is whether the **in-force channel** may supply definitions for macros that take no
/// parameters. The channel exists because a `#define` inside a conditional region is a body and not a
/// definition, and it is where MSVC's `_ACRTIMP` arrives from `corecrt.h` — so turning it on is what makes the
/// `_ACRTIMP`-headed declarations of `ucrt` expand.
///
/// **It is on by default**, and that is a measurement rather than a preference. The switch was written off when it
/// cost clean files; what changed is the measurement, not the switch:
///
/// ```text
///                                        default (off)      in force (on)
/// 255 files: clean / messages / kinds    244 / 95 / 3       245 / 67 / 4
/// 109 files: clean / messages / kinds    101 / 72 / 2       103 / 50 / 3
/// ```
///
/// Both numbers moved at once, for two separate reasons. The **grammar** fixes removed the gaps that expansion
/// used to expose, and the **context** fix removed a lie from the census: the probe used to give every header the
/// macro state of the alphabetically first file that included it, and the six headers that looked *broken* by this
/// switch (`cstdint`, `utility`, `tuple`, `new`, `type_traits`) read cleanly, with no messages at all, when each
/// was run on its own. With the context taken from the chain the seed translation unit actually walks, the switch
/// wins on both primary numbers, so it is the default and [`configuration_from_environment_with`] is how a caller
/// asks for the conservative reading instead.
pub fn configuration_from_environment_with(
    environment: &dyn cpp_parser::MacroFacts,
    use_in_force_bodies: bool,
) -> Configuration {
    // A cache of one file's definitions: what a caller that has no run to share gets, and the honest default for
    // an API whose other entry point takes the cache a run keeps. See `ParsedDefinitions`.
    configuration_from_environment_and(
        environment,
        use_in_force_bodies,
        &mut ParsedDefinitions::new(),
    )
}

/// [`configuration_from_environment_with`], with the **run's** definition cache.
///
/// The difference is the whole 12.2 s measured on the 255-file corpus: without a shared cache, every file re-reads
/// every definition it has in force — 2.5 million lex-and-parse calls for 42 939 distinct definitions — and then
/// copies the result into its own table. A caller that cooks more than one file (a census, an editor's session,
/// an index build) passes the same cache each time.
pub fn configuration_from_environment_and(
    environment: &dyn cpp_parser::MacroFacts,
    use_in_force_bodies: bool,
    parsed: &mut ParsedDefinitions,
) -> Configuration {
    // A body in force is used **only when the caller said the macro is object-like** — and only when this
    // function was asked for it. One whose parameters are unknown cannot be substituted into (`None` is not
    // "object-like"), and one the caller says takes parameters must not be pasted. Everything left is counted:
    // see `in_force_without_a_parameter_list` and the function's own note for the measurement.
    // **Usable** means: object-like, or function-like *with* its parameter list — a body that takes arguments can
    // be expanded only when the arguments have names to be substituted into, and a body nobody classified cannot
    // be expanded at all. What is left is counted, not guessed at.
    let usable = |function_like: Option<bool>, parameters: Option<&std::sync::Arc<str>>| match function_like {
        Some(false) => true,
        Some(true) => parameters.is_some(),
        None => false,
    };
    // **Both channels in one pass each**: the visitor hands over the facts, and what it hands over is what the
    // two answers below are counted from — the bodies this run may use, and the ones it may not.
    let mut in_force: Vec<InForceDefinition<'_>> = Vec::new();
    let mut bodies_in_force = 0usize;
    environment.for_each_body_in_force(&mut |facts| {
        bodies_in_force += 1;
        if use_in_force_bodies && usable(facts.function_like, facts.parameters) {
            // Cloned `Arc`s rather than fresh copies of the text — see the note on the other channel below.
            in_force.push((facts.name, facts.parameters.cloned(), std::sync::Arc::clone(facts.body)));
        }
    });

    let mut out = Configuration {
        in_force_without_a_parameter_list: bodies_in_force - in_force.len(),
        ..Configuration::default()
    };

    // `(name, at, parameters, body, usable)`. The **text** is not built here: it is what the cache below keys on,
    // and building it for every entry of every file was one of the two costs this function's own measurement
    // named (2.5 million `format!` calls for 42 939 distinct definitions).
    //
    // **A function-like definition whose parameter list the evidence does not have is not usable**, and that is
    // the whole of the rule that used to be "every function-like definition is skipped": without the list
    // [`definition_text`] would write `#define NAME body`, the table would read the macro as **object-like**, and a
    // body that needs arguments would be pasted verbatim — the failure
    // `Configuration::in_force_without_a_parameter_list` records (`( "warning " # NUMBER ": " MESSAGE )`).
    //
    // With the list, substitution is exactly what a function-like macro means, and the skip is now a gap rather
    // than a policy: it was written when no channel carried parameters at all. Measured on the 255-file SDK
    // corpus, which is why it changed — the SAL annotations of `sal.h`/`specstrings.h` are function-like macros
    // that expand to nothing (`_Struct_size_bytes_(size)` on `ncrypt.h:267`), and reading them as names left
    // `typedef _Struct_size_bytes_ ( … ) struct …` in the stream.
    environment.for_each_definition(&mut |facts| {
        let usable = !facts.function_like || facts.parameters.is_some();
        if !usable {
            // No parameter names in the evidence — see the field's note.
            out.function_like_without_parameters += 1;
            return;
        }

        // A definition the evidence carries **without a body** is counted rather than parsed: there is nothing to
        // expand, and the count is what says how much of the corpus is in that state.
        let Some(body) = facts.body_text else {
            out.without_a_body += 1;
            return;
        };

        match parsed.definition(facts.name, facts.parameters, body) {
            // The **binding's** offset, not the definition's: see `MacroTable::define_shared_at`.
            Some(definition) => out.table.define_shared_at(definition, facts.at),
            None => out.unreadable += 1,
        }
    });

    // The **in-force** bodies take effect from offset 0: they are not positional evidence but a body a condition
    // settled, and the file sees them from its first line. See `Configuration::in_force_without_a_parameter_list`.
    for (name, parameters, body) in in_force {
        match parsed.definition(name, parameters.as_ref(), &body) {
            Some(definition) => out.table.define_shared_at(definition, 0),
            None => out.unreadable += 1,
        }
    }

    out
}
/// **What one file of a walked unit starts with**, as the cooker asks it: the unit's definitions *as that file
/// sees them*, with the compilation's own builtins underneath.
///
/// # The three layers, newest first
///
/// A cook's starting state is not one table, and the order is what makes it equal to the table it replaced. It
/// used to be built by inserting, in this order, into one flat table: the builtins (`-dM`), then the
/// environment's definitions, then the environment's **in-force bodies**; the table's lookup is
/// "the last inserted binding in force at this offset", so the later layers shadow the earlier ones. This type
/// answers in exactly that order:
///
/// 1. the **in-force channel** — a body a condition settled, in force from offset 0, and the newest layer, so it
///    wins outright when it is usable (see `Configuration::in_force_without_a_parameter_list`);
/// 2. the **definition channel** — the unit's own `#define`s, in force from the offset the fact came into force
///    at *in this file*, which is what [`crate::MacroView::visible_binding`] resolves;
/// 3. the **seed** — what the compiler predefines, the oldest layer, so a header that redefines a builtin is in
///    force over it.
///
/// # Why it is a view and not a table
///
/// Every file used to get its own: ~10 000 bindings copied per file, the same definitions differing only in the
/// offset each was in force from (255 files, 1.7 s of environment plus 4.1 s of table build in the census). None
/// of that is per file: the **definitions** are the unit's ([`crate::UnitDefinitions`], parsed once) and the
/// **offsets** are a property of the file, computed by the view when a query names one.
pub struct FileMacros<'unit> {
    view: crate::MacroView<'unit>,
    definitions: &'unit crate::UnitDefinitions,
    /// What the compiler predefines and the configuration decides — the oldest layer.
    seed: Option<&'unit MacroTable>,
    /// May the in-force channel be used? A decision the measurement left open, and the caller's to make: see
    /// [`configuration_from_environment_with`].
    use_in_force_bodies: bool,
}

impl<'unit> FileMacros<'unit> {
    /// What `view`'s file starts with, over the unit's definitions and the compilation's seed.
    pub fn new(
        view: crate::MacroView<'unit>,
        definitions: &'unit crate::UnitDefinitions,
        seed: Option<&'unit MacroTable>,
        use_in_force_bodies: bool,
    ) -> Self {
        FileMacros {
            view,
            definitions,
            seed,
            use_in_force_bodies,
        }
    }

    /// The definition that is in force from offset 0 because a condition settled it — layer 1.
    ///
    /// `None` for a name the in-force channel does not carry **or cannot use**: an unusable body is not in the
    /// table at all (it is counted instead), so it must not shadow the definitions underneath it.
    fn in_force(&self, name: &str) -> Option<&'unit std::sync::Arc<crate::macros::MacroDef>> {
        if !self.use_in_force_bodies {
            return None;
        }
        self.definitions.of(self.view.visible_body_in_force(name)?)
    }

    /// The unit's own definition of `name`, with the offset it is in force from **in this file** — layer 2.
    ///
    /// A fact whose last visible entry is not a usable definition (an `#undef`, a body nobody carried) answers
    /// `None` rather than searching further back: the last entry is what a name *is* there, and that is what the
    /// name's collapse meant before this type existed. `None` is therefore "the unit says nothing usable", which
    /// leaves the seed underneath.
    fn positional(&self, name: &str) -> Option<(usize, &'unit std::sync::Arc<crate::macros::MacroDef>)> {
        let (at, index) = self.view.visible_binding(name)?;
        Some((at, self.definitions.of(index)?))
    }
}

impl crate::macros::MacroBindings for FileMacros<'_> {
    fn definition_at(&self, name: &str, offset: usize) -> Option<&crate::macros::MacroDef> {
        if let Some(definition) = self.in_force(name) {
            // In force from offset 0, so every offset sees it — see the type's note on the layer order.
            return Some(definition);
        }
        if let Some((at, definition)) = self.positional(name)
            && at <= offset
        {
            return Some(definition);
        }
        self.seed?.definition_at(name, offset)
    }

    fn definition(&self, name: &str) -> Option<&crate::macros::MacroDef> {
        self.definition_at(name, usize::MAX)
    }
}

/// **The cook's running state**: the file's own directives over what it started with.
///
/// A file's `#define`s are the *newest* layer, which is why the walk over them is a layer of its own rather than
/// a copy: `cook_with` used to `clone()` the whole starting table (~10 000 bindings per file) to get a table it
/// could add to, and every one of those bindings was already shared and immutable.
struct Live<'base> {
    own: MacroTable,
    base: &'base dyn crate::macros::MacroBindings,
}

impl crate::macros::MacroBindings for Live<'_> {
    fn definition_at(&self, name: &str, offset: usize) -> Option<&crate::macros::MacroDef> {
        self.own
            .definition_at(name, offset)
            .or_else(|| self.base.definition_at(name, offset))
    }

    fn definition(&self, name: &str) -> Option<&crate::macros::MacroDef> {
        self.own
            .definition(name)
            .or_else(|| self.base.definition(name))
    }
}

/// **Two layers of starting state, newest first** — a materialised environment over the compiler's builtins.
///
/// The one-walk path does not need this: [`FileMacros`] carries its own three layers, because the unit knows what
/// is newest. The **closure** path does: what a file starts with there is a `Configuration` built for that file,
/// and underneath it the builtins are the same table for every file in the run. Copying one into the other is what
/// `MacroTable::extend_from` used to do per file — and it is a copy of bindings that are already shared.
pub struct Over<'a> {
    newest: &'a dyn crate::macros::MacroBindings,
    oldest: &'a dyn crate::macros::MacroBindings,
}

impl<'a> Over<'a> {
    /// `newest` wins wherever it has a binding in force; `oldest` answers everything else.
    pub fn new(
        newest: &'a dyn crate::macros::MacroBindings,
        oldest: &'a dyn crate::macros::MacroBindings,
    ) -> Self {
        Over { newest, oldest }
    }
}

impl crate::macros::MacroBindings for Over<'_> {
    fn definition_at(&self, name: &str, offset: usize) -> Option<&crate::macros::MacroDef> {
        self.newest
            .definition_at(name, offset)
            .or_else(|| self.oldest.definition_at(name, offset))
    }

    fn definition(&self, name: &str) -> Option<&crate::macros::MacroDef> {
        self.newest
            .definition(name)
            .or_else(|| self.oldest.definition(name))
    }
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
    initial: &dyn crate::macros::MacroBindings,
) -> CookedStream {
    let directives = crate::directive::scan_directives(source, tokens);

    // The macros this walk has actually reached, on top of what the configuration supplied. A `#define` in
    // the file shadows a builtin of the same name, which is exactly what the table's positional lookup does
    // when the file's definition is written later than offset 0 — and it is a **layer** rather than a copy of
    // what the file started with: every binding underneath is already shared, so there is nothing to copy.
    let mut live = Live {
        own: MacroTable::new(),
        base: initial,
    };
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
                    live.own.define(definition.clone());
                }
            }
            DirectiveKind::Undef => {
                if was_live
                    && let Directive::Undef { name: Some(name) } = &spanned.directive
                {
                    live.own.undefine(name, spanned.range.start_offset);
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
    live: &dyn crate::macros::MacroBindings,
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
    live: &dyn crate::macros::MacroBindings,
    token_index: usize,
    tokens: &[CppTokenData],
) -> bool {
    let offset = token_start(tokens, token_index);
    let unknown = |name: &str| live.definition_at(name, offset).is_none();

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
    live: &dyn crate::macros::MacroBindings,
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
