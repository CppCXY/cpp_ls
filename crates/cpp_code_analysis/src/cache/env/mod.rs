//! **The environment as a value** — the macros in force at one point of one translation unit, and the number that
//! identifies them.
//!
//! See `docs/module-cache.md` for why this exists and what it replaces. In one sentence: a file's reading is a
//! function of its own text and the macros in force where it sits, not of its translation unit, so the macros have to
//! be a **value** a cache can key on rather than a state a walk happens to be in.
//!
//! # The shapes
//!
//! ```text
//! Definition   one #define: the name, whether it is function-like, the replacement list as text, and a hash
//! Env          a persistent chain of changes, with an `EnvId` that hashes the chain
//! Skeleton     a file's directives in source order — the input a fold reads (see `skeleton`)
//! compose      folds a skeleton into an Env, recursing through its includes (see `compose`)
//! ```
//!
//! # What the id is, and what it is not
//!
//! `EnvId` identifies **this chain of changes**, in this order. It is the key a fold's result is memoised under
//! ([`crate::cache::env::compose`]) and the half of a `ParseKey` that says which environment a rendering was made in.
//! It is deliberately *not* order-insensitive: two folds that reach the same final table by different routes are two
//! values here, because the fold is deterministic per file and the cheaper hash is worth more than a property no
//! caller needs. The order-insensitive number — the one that decides whether two *programs* share a file's rendering
//! — is the **reachable** environment of `docs/module-cache.md` §4, a separate key computed over the names a file
//! mentions.
//!
//! # What the table says about a name it does not hold
//!
//! [`Env`] answers [`Lookup::Undefined`] for a name it has never seen, which is a statement that the *root* of the
//! chain is complete — that everything a compilation predefines was folded in before the file was read. Callers that
//! build an environment from a real session must seed it with the compiler's predefines and the `-D` flags; a caller
//! with an unknown seed gets a table that is too confident, which is why [`Env::empty`] is documented as "nothing is
//! defined and nothing is missing" rather than as a neutral default.

use std::sync::Arc;

use crate::cache::summary::MacroFact;
use crate::cache::summary::MacroKind;
use crate::preprocess::condition::{Lookup, MacroValues};
use cpp_parser::SourceRange;

mod compose;
#[cfg(test)]
mod differential;
mod skeleton;
#[cfg(test)]
mod tests;
mod unit;

pub use compose::{Composed, compose};
pub use skeleton::{Skeleton, Step};
pub use unit::{ComposedFile, UnitComposition, compose_unit};

/// **The number that identifies an environment** — a hash of the chain of changes, in the order they were applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnvId(u64);

impl EnvId {
    /// The id of the environment that has had nothing done to it.
    ///
    /// A constant rather than `0`, so that a value produced by a bug (a default-initialised id, an unwritten field)
    /// cannot be mistaken for the empty environment: `0` is not a hash of anything.
    pub const EMPTY: EnvId = EnvId(0x9e37_79b9_7f4a_7c15);

    /// The number itself, for a caller writing it into a record.
    pub fn number(self) -> u64 {
        self.0
    }
}

/// **One macro definition, as an environment holds it**: everything expansion reads, and nothing else.
///
/// The *definition site* is deliberately absent. Two copies of the same `#define` in two headers are one definition
/// to a consumer of the environment — expansion pastes the same tokens — and folding the site into the hash would
/// make every header's copy unique, which is the sharing this type exists to keep. A caller that needs the site
/// (hover, "go to definition") reads it from the [`MacroFact`], which is where it belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    name: Arc<str>,
    function_like: bool,
    /// The replacement list, exactly as written after the name and any parameters, or `None` for an object-like
    /// macro with no body (`#define FLAG`).
    body: Option<Arc<str>>,
    /// **The definition a condition can read a value from**, when the summary carried one — #define A 1 makes
    /// #if A decidable, and a body that is not a lone integer literal leaves this None and the answer
    /// `DefinedWithoutAValue`. Built here rather than on every question because the value is a property of the
    /// definition: macro_value reads it once and keeps it.
    evaluable: Option<Box<crate::macros::MacroDef>>,
    /// **The one name the body *is***, when the body is exactly one identifier --- `#define A B`'s B. The
    /// evaluator follows an alias through a body of tokens; an environment has the text and not the tokens, so it
    /// follows the name instead, which is the same hop when the body is one name.
    alias: Option<Arc<str>>,
    hash: u64,
}

impl Definition {
    /// **The definition a fact names**, with its body sliced out of the file that wrote it.
    ///
    /// `None` for an `#undef` (a fact that says the name *ceases* to be a macro) and for a fact whose body range
    /// does not fit the text it is claimed to be in — a summary and a text that disagree are not a definition this
    /// layer is willing to guess at.
    pub fn of(fact: &MacroFact, text: &str) -> Option<Self> {
        if fact.kind != MacroKind::Definition {
            return None;
        }
        // **The replacement list starts after the whitespace that separates it from the name**: `body_range` begins
        // at the byte after the name (or after the parameters), so `#define B 1` slices as ` 1`. The leading space is
        // not part of the definition — expansion re-lexes the body — and keeping it would make two spellings of one
        // definition two environments.
        let body = match fact.body_range {
            Some(range) => Some(Arc::from(slice(text, range)?.trim())),
            None => None,
        };
        let hash = hash_definition(fact.name.as_str(), fact.function_like, body.as_deref());
        let evaluable = build_macro_def(fact, text, body.as_deref()).map(Box::new);
        Some(Self {
            name: Arc::from(fact.name.as_str()),
            function_like: fact.function_like,
            body,
            evaluable,
            alias: fact.alias.as_deref().map(Arc::from),
            hash,
        })
    }

    /// A definition from parts — the constructor a decoder or a test uses when it has no [`MacroFact`].
    pub fn new(name: &str, function_like: bool, body: Option<Arc<str>>) -> Self {
        let hash = hash_definition(name, function_like, body.as_deref());
        Definition {
            name: Arc::from(name),
            function_like,
            body,
            evaluable: None,
            alias: None,
            hash,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_function_like(&self) -> bool {
        self.function_like
    }

    pub fn body(&self) -> Option<&str> {
        self.body.as_deref()
    }

    /// The hash of this definition, which is what an environment's id is built from.
    pub fn hash(&self) -> u64 {
        self.hash
    }

    /// **The definition a condition evaluator can read a value from**, when the summary carried one.
    pub(crate) fn evaluable(&self) -> Option<&crate::macros::MacroDef> {
        self.evaluable.as_deref()
    }

    /// The name this definition's body is, when the body is one identifier --- see the field.
    pub(crate) fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }
}

/// The slice `range` names, when it is inside `text` and on a character boundary.
fn slice(text: &str, range: SourceRange) -> Option<&str> {
    let start = range.start_offset;
    let end = start.checked_add(range.length)?;
    if end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return None;
    }
    Some(&text[start..end])
}

fn hash_definition(name: &str, function_like: bool, body: Option<&str>) -> u64 {
    let mut bytes = Vec::with_capacity(name.len() + body.map_or(0, str::len) + 3);
    bytes.extend_from_slice(name.as_bytes());
    bytes.push(0);
    // A body-less macro and a macro whose body is empty are different definitions ("defined, with no replacement
    // list" versus "defined, replacing with nothing"), and a one-byte kind is what keeps them apart.
    match body {
        Some(body) => {
            bytes.push(1);
            bytes.extend_from_slice(body.as_bytes());
        }
        None => bytes.push(2),
    }
    bytes.push(u8::from(function_like));
    crate::cache::fnv1a64(&bytes)
}

/// **An environment**: a persistent chain of changes, plus the id of the chain.
///
/// Cloning is an `Arc` bump, and applying a change allocates one frame — no table is copied. A lookup walks the chain
/// from the newest frame back, which is the length of the fold that built it (tens of frames for a header, hundreds
/// for a deep closure); §4 of the design note replaces the walk with a hash lookup once the ids are interned, and
/// that is a change to `lookup`, not to this type's shape.
#[derive(Debug, Clone)]
pub struct Env {
    frame: Option<Arc<Frame>>,
    id: EnvId,
}

#[derive(Debug)]
struct Frame {
    parent: Option<Arc<Frame>>,
    /// The changes this frame made, in the order they were applied. The last change to a name wins, which is what
    /// makes `#define A 1` / `#undef A` / `#define A 2` inside one file read as `A = 2`.
    changes: Vec<Change>,
}

#[derive(Debug, Clone)]
struct Change {
    name: Arc<str>,
    definition: Option<Arc<Definition>>,
    hash: u64,
}

impl Env {
    /// The environment nothing has been done to.
    pub fn empty() -> Self {
        Env {
            frame: None,
            id: EnvId::EMPTY,
        }
    }

    pub fn id(&self) -> EnvId {
        self.id
    }

    /// This environment with `definition` in force.
    pub fn define(&self, definition: Arc<Definition>) -> Self {
        self.with(Change::define(definition))
    }

    /// This environment with `name` no longer a macro.
    pub fn undef(&self, name: &str) -> Self {
        self.with(Change::undef(name))
    }

    fn with(&self, change: Change) -> Self {
        Self::with_changes(self, vec![change])
    }

    /// **Apply a run of changes as one frame** — the fold's fast path: a file's defines and undefs are one step
    /// rather than one frame each, which keeps the chain as long as the *files* rather than as long as the directives.
    ///
    /// The frame's id is the hash of its final state per name, **sorted by name**, so two runs that touch the same
    /// names in different orders are one frame. That is a property of a frame and not of the chain: the chain's id
    /// still says which state was reached when.
    pub fn apply(
        &self,
        changes: impl IntoIterator<Item = (Arc<str>, Option<Arc<Definition>>)>,
    ) -> Self {
        let mut changes: Vec<Change> = changes
            .into_iter()
            .map(|(name, definition)| Change::new(name, definition))
            .collect();
        // **The last write per name wins**, and the rest are dropped: a frame is a state, not a log.
        changes.reverse();
        let mut seen = std::collections::HashSet::new();
        changes.retain(|change| seen.insert(change.name.clone()));
        changes.reverse();
        Self::with_changes(self, changes)
    }

    fn with_changes(&self, changes: Vec<Change>) -> Self {
        if changes.is_empty() {
            return self.clone();
        }
        let mut hashes: Vec<(u64, u64)> = changes
            .iter()
            .map(|change| (hash_name(&change.name), change.hash))
            .collect();
        hashes.sort_unstable();
        let mut bytes = Vec::with_capacity(16 + hashes.len() * 16);
        bytes.extend_from_slice(&self.id.0.to_le_bytes());
        for (name, change) in &hashes {
            bytes.extend_from_slice(&name.to_le_bytes());
            bytes.extend_from_slice(&change.to_le_bytes());
        }
        let id = EnvId(crate::cache::fnv1a64(&bytes));
        Env {
            frame: Some(Arc::new(Frame {
                parent: self.frame.clone(),
                changes,
            })),
            id,
        }
    }

    /// The definition in force for `name`, or `None` when the name is not a macro here.
    pub fn get(&self, name: &str) -> Option<&Definition> {
        match self.find(name)? {
            Some(definition) => Some(definition),
            None => None,
        }
    }

    /// Is `name` a macro here?
    pub fn is_defined(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// **Every definition in force**, each name once, in no particular order.
    ///
    /// The walk of the chain reads newest first and skips a name it has already seen, so a name that was
    /// `#undef`ed later is reported as absent rather than as its older definition.
    pub fn for_each_in_force(&self, mut visit: impl FnMut(&Definition)) {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut frame = self.frame.as_deref();
        while let Some(current) = frame {
            for change in current.changes.iter().rev() {
                if !seen.insert(change.name.as_ref()) {
                    continue;
                }
                if let Some(definition) = change.definition.as_deref() {
                    visit(definition);
                }
            }
            frame = current.parent.as_deref();
        }
    }

    /// How many definitions are in force — the number a probe reports.
    pub fn len(&self) -> usize {
        let mut count = 0;
        self.for_each_in_force(|_| count += 1);
        count
    }

    pub fn is_empty(&self) -> bool {
        self.frame.is_none()
    }

    /// The chain lookup, with the three-way answer a caller needs: `None` when the chain never names the name,
    /// `Some(None)` when the last thing it says is an `#undef`, and `Some(Some(_))` for a definition.
    fn find(&self, name: &str) -> Option<Option<&Definition>> {
        let mut frame = self.frame.as_deref();
        while let Some(current) = frame {
            for change in current.changes.iter().rev() {
                if change.name.as_ref() == name {
                    return Some(change.definition.as_deref());
                }
            }
            frame = current.parent.as_deref();
        }
        None
    }
}

impl Default for Env {
    fn default() -> Self {
        Self::empty()
    }
}

/// **What a condition sees.**: an environment knows which names are macros, and behind it the compilation's own
/// definitions answer what only *they* can — the values.
///
/// Without the second half, `#if _MSC_VER >= 1930` is [`Lookup::DefinedWithoutAValue`]'s honest "defined, value
/// unknown" and every branch gated on a predefine is undecidable. With it, a fold decides exactly the conditions the
/// walk decides: a name the environment `#undef`ed answers `Undefined` and does **not** fall through to the seed (that
/// is what `#undef` means), a name the environment defines answers `DefinedWithoutAValue` (its body is a shape plus
/// one literal, not the token list the evaluator wants — `docs/module-cache.md` §8), and everything else is the
/// seed's answer.
/// How many alias hops a value may take before the answer is `defined, value unknown`.
pub const MAX_ALIAS_HOPS: usize = 8;

/// **What a condition sees**: an environment in front of the compilation's own definitions.
pub struct Over<'a> {
    env: Env,
    seed: &'a dyn MacroValues,
}

impl<'a> Over<'a> {
    /// **The value a definition has, following an alias when it is one.** `#define _GLIBCXX_HOSTED __STDC_HOSTED__`'s
    /// value is `__STDC_HOSTED__`'s, and that one is a compiler predefine with a body --- the walk follows the same hop
    /// through the body's tokens (macro_value re-expands and re-reads), and an environment follows the name. Bounded
    /// so a cycle of aliases ends in `defined, value unknown` rather than in a loop.
    /// **The seed's answer, with the completeness rule applied**: a name an incomplete table has never seen is not
    /// Undefined --- it may be defined in a file the walk has not reached, and answering `0` there reads a branch
    /// the compiler does not take.
    fn the_seed_answer(&self, name: &str) -> Lookup<'_> {
        match self.seed.lookup(name) {
            Lookup::Undefined if !self.seed.is_complete() => Lookup::Unanswered,
            answer => answer,
        }
    }

    fn through_aliases<'s>(&'s self, definition: &'s Definition) -> Lookup<'s> {
        if let Some(found) = definition.evaluable() {
            return Lookup::Defined(found);
        }
        let mut alias = definition.alias();
        for _ in 0..MAX_ALIAS_HOPS {
            let Some(name) = alias else { break };
            match self.env.get(name) {
                Some(next) => {
                    if let Some(found) = next.evaluable() {
                        return Lookup::Defined(found);
                    }
                    alias = next.alias();
                }
                None => return self.the_seed_answer(name),
            }
        }
        Lookup::DefinedWithoutAValue
    }
    /// The table for one point of a fold: `env` in front of `seed`.
    pub fn new(env: Env, seed: &'a dyn MacroValues) -> Self {
        Over { env, seed }
    }
}

impl MacroValues for Over<'_> {
    fn lookup(&self, name: &str) -> Lookup<'_> {
        match self.env.find(name) {
            Some(Some(definition)) => self.through_aliases(definition),
            Some(None) => Lookup::Undefined,
            None => self.the_seed_answer(name),
        }
    }

    fn builtin_operator(&self, name: &str, operand: &str) -> Option<crate::Value> {
        self.seed.builtin_operator(name, operand)
    }
}

impl MacroValues for Env {
    fn lookup(&self, name: &str) -> Lookup<'_> {
        match self.find(name) {
            Some(Some(_)) => Lookup::DefinedWithoutAValue,
            Some(None) | None => Lookup::Undefined,
        }
    }
}

impl Change {
    fn define(definition: Arc<Definition>) -> Self {
        Self::new(Arc::from(definition.name()), Some(definition))
    }

    fn undef(name: &str) -> Self {
        Self::new(Arc::from(name), None)
    }

    fn new(name: Arc<str>, definition: Option<Arc<Definition>>) -> Self {
        let hash = match &definition {
            Some(definition) => definition.hash(),
            None => 0,
        };
        Change {
            name,
            definition,
            hash,
        }
    }
}

fn hash_name(name: &str) -> u64 {
    crate::cache::fnv1a64(name.as_bytes())
}

/// **The definition a condition evaluator can expand**, built from the facts a summary keeps.
///
/// A value is one integer literal, and for `#define _GLIBCXX_HOSTED __STDC_HOSTED__` that is enough. A *function-like*
/// alias — `#define _GLIBCXX_HAS_BUILTIN(B) __has_builtin(B)` — is not a value at all: the evaluator can only follow it
/// if it has the parameter list and the body's tokens, and `MacroDef` is the shape that carries both. The summary
/// stores neither, but both are in the text it was built from: the parameter list sits between the name and the body,
/// and the body is the text `body_range` names. Building them here is what makes the `_GLIBCXX_HAS_BUILTIN` family
/// decidable, and with it most of libstdc++'s feature-test headers.
///
/// A macro with no body and no parameters (`#define FLAG`) has nothing to evaluate and stays
/// `Lookup::DefinedWithoutAValue` — answering `Defined` there would hand the evaluator an empty token list and a
/// syntax error where the standard says `0`.
fn build_macro_def(
    fact: &MacroFact,
    text: &str,
    body: Option<&str>,
) -> Option<crate::macros::MacroDef> {
    if body.is_none() && !fact.function_like {
        return None;
    }
    let params = if fact.function_like {
        let start = fact.range.end_offset();
        let end = fact.body_range.map_or(start, |range| range.start_offset);
        Some(parameters_in(text.get(start..end).unwrap_or("")))
    } else {
        None
    };
    let body_text = body.unwrap_or("");
    // **A body that is one integer needs no tokens.** `macro_value` answers from the value cache before it looks at
    // the body, so lexing `202002L` into three tokens for every feature macro is memory spent to be skipped — and a
    // session composes hundreds of closures, each with a few hundred such definitions.
    let value = fact
        .value
        .as_deref()
        .and_then(|value| value.trim().parse::<i128>().ok());
    let tokens: Vec<crate::token::Token> = if value.is_some() {
        Vec::new()
    } else {
        let (lexed, _) = cpp_parser::lex(body_text, &cpp_parser::LexerConfig::default());
        lexed
            .iter()
            .map(|data| {
                let piece = body_text
                    .get(data.range.start_offset..data.range.end_offset())
                    .unwrap_or("");
                crate::token::Token::new(data.kind, piece, data.range)
            })
            .collect()
    };
    let definition = crate::macros::MacroDef {
        name: Box::from(fact.name.as_str()),
        params,
        body: crate::macros::MacroBody {
            tokens,
            stringize: Vec::new(),
            paste: Vec::new(),
        },
        range: fact.range,
        name_range: fact.range,
        written_in: None,
        value: std::sync::OnceLock::new(),
    };
    if let Some(number) = value {
        let _ = definition.value.set(Some(number));
    }
    Some(definition)
}

/// **The parameters between a function-like macro's name and its body** — `"(a, b)"` or `"()"`.
///
/// Text rather than a parsed list because that is what the summary leaves behind: it records that the macro *is*
/// function-like and where its body starts, and the bytes between are the parameters. A variadic tail becomes
/// `__VA_ARGS__`, which is the spelling a body uses for it.
fn parameters_in(list: &str) -> Vec<crate::macros::Parameter> {
    let inner = list.trim();
    let inner = inner.strip_prefix('(').unwrap_or(inner);
    let inner = inner.strip_suffix(')').unwrap_or(inner);
    if inner.trim().is_empty() {
        return Vec::new();
    }
    inner
        .split(',')
        .map(|part| {
            let part = part.trim();
            if part == "..." {
                crate::macros::Parameter {
                    name: Box::from("__VA_ARGS__"),
                    kind: crate::macros::ParameterKind::Variadic,
                }
            } else {
                crate::macros::Parameter {
                    name: Box::from(part),
                    kind: crate::macros::ParameterKind::Ordinary,
                }
            }
        })
        .collect()
}
