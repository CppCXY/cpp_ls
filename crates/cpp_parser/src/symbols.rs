//! The **external symbol table**: what a caller can tell the parser about names it has already resolved.
//!
//! # Why the parser wants one, and why it can do without
//!
//! Several parse questions in C++ are settled by looking a name up, and the two answers are the *same tokens*:
//!
//! ```text
//! Widget w(1, 2);    a variable initialised with two arguments — `Widget` is a type
//! g(1, 2);           a call with two arguments — `g` is a function
//!
//! MY_API Widget *p;  a declaration whose macro expands to a specifier
//! NUMBER_OPTION(x)   a macro invocation whose body is a whole statement, so no `;` follows
//! g(x)               a call with its `;` missing
//! ```
//!
//! A compiler resolves the name; this parser cannot, because it never sees another translation unit and does not
//! run the preprocessor. What it does instead is **decide from what the file says about itself** — the tables in
//! [`crate::parser::TypeNames`] and [`crate::parser::MacroNames`] — and, when the file says nothing, fall back to
//! shape preferences that are documented and pinned by tests. That is what makes the parser usable on a single
//! buffer with no context at all, and it is why it is **total**: every input produces a lossless, well-formed tree.
//!
//! A caller that *does* have more context — an editor with a project index, or `cpp_code_analysis` once it can
//! build one — can hand that context in through this trait, and the parser will prefer it to its own guesses.
//! Both halves are needed, and neither replaces the other:
//!
//! * the table knows what this file cannot (headers, other files, macros expanded elsewhere);
//! * the heuristics are what remains when the table is absent, stale, or simply does not know — which is the
//!   ordinary state of affairs while a file is being typed.
//!
//! # The three answers, and why `bool` is not enough
//!
//! [`SymbolTable::kind_of`] returns `Option<SymbolKind>`, and a `None` means **"this table does not know"** —
//! *not* "this name is not a type". Nothing in this interface can say "no": a table that knows `g` is a function
//! answers `Some(SymbolKind::Function)`, which is what refuses the declaration reading; a table that has never
//! heard of `Widget` answers `None`, and the parser falls back to its own evidence. Collapsing the two into a
//! `bool` would make a stale index silently change how valid code parses, which is the one failure mode this
//! crate's `A0` class is about.
//!
//! # Precedence, and the staleness that motivates it
//!
//! Callers of the grammar apply this order, and it is deliberate:
//!
//! 1. **what this file says** — [`crate::parser::TypeNames`], [`crate::parser::MacroNames`]: a name declared or
//!    `#define`d in the text being parsed. Always the freshest evidence there is, because it *is* the text;
//! 2. **the external table** — everything the file cannot see;
//! 3. **the shape preferences** — the documented guesses, unchanged.
//!
//! A deferred index lags the buffer, so it is consulted last among the sources of evidence rather than first: a
//! name the user has just renamed is answered by (1) or by nothing at all, and never by a stale (2).
//!
//! # What an implementation must guarantee
//!
//! * **It decides readings, not structure.** Whatever a table answers — including nonsense — the parse stays
//!   lossless and well-formed. A table can make the parser choose the wrong reading; it cannot make it drop
//!   tokens.
//! * **It is a pure function of the name.** `&self`, no interior state that changes an answer, no I/O: the same
//!   source and the same table must produce the same tree every time (`I3`), and the parser may ask thousands of
//!   times per file. `None` is always a safe answer, and an empty table must be indistinguishable from no table.
//! * **It is shared, not owned.** `Send + Sync`, so an editor can hold one index and parse several buffers
//!   against it.
//! * **It needs no checkpointing.** Because it is read-only and external, a speculative parse that is rolled back
//!   needs no restoring of anything — unlike the parser's own tables, which is why those live in `Checkpoint` and
//!   this one does not.
//!
//! # The format lives here
//!
//! The trait and its vocabulary are defined by the *consumer* — this crate — and implemented by whoever has the
//! index. That keeps the dependency pointing one way: the parser never learns what an index is, and
//! `cpp_code_analysis` never has to expose its internals. Extending the vocabulary means adding a variant or a
//! method with a default implementation, so an existing implementation keeps compiling and simply answers `None`
//! for what it does not know — which is exactly the fallback above.

/// What a caller can tell the parser about a name it has already resolved.
///
/// Implemented by whoever has the index — `cpp_code_analysis`, or a test. See the module documentation for the
/// contract, and [`NoSymbols`] / [`SymbolMap`] for the two implementations that ship with this crate.
pub trait SymbolTable: Send + Sync {
    /// What is `name`, as far as this table knows?
    ///
    /// `None` means **"this table does not know"**, never "this name is not a type" — see the module
    /// documentation. A name is asked for **as it is written** in the source, without qualification: resolving
    /// `f` inside `namespace n` to `n::f` is the implementation's business, not the parser's, which knows only
    /// the spelling in front of it.
    fn kind_of(&self, name: &str) -> Option<SymbolKind>;
}

/// A table that knows nothing.
///
/// Parsing against it is **exactly** parsing with no table at all — the equivalence is asserted in
/// `tests/symbols.rs`, which is what keeps "the table is optional" a fact rather than an intention. It exists so
/// that a caller can pass *something* (`Option<&dyn SymbolTable>` is otherwise awkward to fill) and so that the
/// equivalence test has a second operand.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSymbols;

impl SymbolTable for NoSymbols {
    fn kind_of(&self, _name: &str) -> Option<SymbolKind> {
        None
    }
}

/// A macro the file **includes** rather than writes, with the offset it becomes visible at.
///
/// The whole point of the offset is the measurement `docs/roadmap.md` §2.0 opens with: a **position-less** table of
/// the closure's macros made 46 files *worse*, because "some header defines this name" was read as "this name is a
/// macro **here**" — `_GLIBCXX_BEGIN_NAMESPACE_VERSION` is `namespace __8 {` in one place and nothing at all in
/// another, and a name's shape is not a property of the name.
///
/// What the index *does* know is the **translation order** of the file it is reading: which `#include` comes where,
/// and which `#define` each included file writes. Feeding that as offsets is what this type carries. It is the same
/// stream `docs/index-design.md` §"闭包自己的宏：按翻译顺序边走边喂" already builds for **conditions** — the parser
/// is the second consumer of it, not a new source of truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludedMacro {
    /// The offset in **this** file from which the entry applies: the end of the `#include` that brought it in.
    pub from_offset: usize,
    pub name: Box<str>,
    /// What the name is from that offset on — or `None` for an `#undef`, which takes it away again.
    pub definition: Option<SymbolKind>,
    /// The macro's **replacement list as text**, when the caller has it.
    ///
    /// The shape in `definition` says what *kind of thing* the name stands for; this says **what it says**, and
    /// it is what turns "this name is a macro" into a reading: `namespace __8 {` is a namespace head, `, noexcept`
    /// is a parameter-list fragment, `virtual HRESULT STDMETHODCALLTYPE method` is a declarator head whose name is
    /// the macro's own argument. See the expansion section of `docs/index-design.md`.
    ///
    /// `None` is the ordinary answer — an `#undef`, a body nobody stored, or a caller that only had shapes.
    pub body_text: Option<Box<str>>,
}

impl IncludedMacro {
    /// A macro that becomes visible at `from_offset`.
    pub fn defined_at(from_offset: usize, name: &str, function_like: bool, body: MacroBody) -> Self {
        Self {
            from_offset,
            name: name.into(),
            definition: Some(SymbolKind::Macro {
                function_like,
                body,
            }),
            body_text: None,
        }
    }

    /// A macro that stops being one at `from_offset` (an `#undef` in this file, which the parser sees anyway —
    /// kept for an `#undef` written by an included file).
    /// A macro that becomes visible at `from_offset`, **with its replacement list as text** — what expansion
    /// needs. See [`IncludedMacro::body_text`].
    pub fn defined_with_body(
        from_offset: usize,
        name: &str,
        function_like: bool,
        body: MacroBody,
        body_text: Option<&str>,
    ) -> Self {
        Self {
            from_offset,
            name: name.into(),
            definition: Some(SymbolKind::Macro {
                function_like,
                body,
            }),
            body_text: body_text.map(Box::from),
        }
    }

    pub fn undefined_at(from_offset: usize, name: &str) -> Self {
        Self {
            from_offset,
            name: name.into(),
            definition: None,
            body_text: None,
        }
    }
}

/// The macros a file's **includes** contribute, each with the offset it applies from.
///
/// Built once per file by the caller that knows the include graph (the index), queried by the parser at the offset
/// it is reading. The answer is positional by construction: [`MacroEnvironment::kind_of`] returns whatever was in
/// force **at that offset**, so the same name may be a macro in one region of a file and an ordinary identifier in
/// another — which is exactly what the flat table could not say, and the reason it lost.
///
/// Not `Clone` on purpose: it is built once and borrowed for the parse.
#[derive(Debug, Default)]
pub struct MacroEnvironment {
    /// One entry per name: `(offset, definition)` in offset order, with `None` for an `#undef`.
    ///
    /// Sorted per name rather than globally because a query is by name — the parser asks "what is `MY_API` here",
    /// never "what is visible here" — and because a name's own history is short.
    by_name: std::collections::HashMap<Box<str>, Vec<(usize, Option<SymbolKind>)>>,
    /// The replacement list per `(offset, name)`, kept beside the history rather than inside it so that the
    /// common lookup — "is this a macro here" — stays a binary search over a slice of small values.
    body_texts: std::collections::HashMap<(usize, Box<str>), Box<str>>,
    /// The replacement lists of macros whose definition is **conditional but in force** — read for *what they say*,
    /// never for *whether the name is a macro*.
    ///
    /// Two channels rather than one, and the split is **measured** rather than aesthetic: handing a conditional
    /// definition out as a definition switches off every rule that reads *shape* precisely because no table knows
    /// the name, which cost 3 files clean→failing on the corpus (`corecrt.h`, `swprintf.inl`, `types.h`, each a
    /// declaration headed by an `__MINGW_EXTENSION`-style macro) and gained none. A **body** cannot do that: no
    /// rule asks "is there a body" in order to refuse a reading, so this channel can only enable one.
    bodies_in_force: std::collections::HashMap<Box<str>, Box<str>>,
}

impl MacroEnvironment {
    /// Build the environment from what the file's includes contribute.
    ///
    /// Entries may arrive in any order; they are sorted per name here, once.
    pub fn from_included_macros(entries: impl IntoIterator<Item = IncludedMacro>) -> Self {
        let mut by_name: std::collections::HashMap<Box<str>, Vec<(usize, Option<SymbolKind>)>> =
            std::collections::HashMap::new();
        let mut body_texts = std::collections::HashMap::new();
        for entry in entries {
            if let Some(text) = entry.body_text {
                body_texts.insert((entry.from_offset, entry.name.clone()), text);
            }
            by_name
                .entry(entry.name)
                .or_default()
                .push((entry.from_offset, entry.definition));
        }
        for history in by_name.values_mut() {
            history.sort_by_key(|(offset, _)| *offset);
        }

        Self {
            by_name,
            body_texts,
            bodies_in_force: std::collections::HashMap::new(),
        }
    }

    /// Does this environment know **nothing at all** — neither a definition nor a body?
    ///
    /// Both channels count, and that is not a detail: an environment built only from
    /// [`MacroEnvironment::with_bodies_in_force`] answers `body_text_in_force` for every name it carries, so a
    /// caller that used this to decide whether to hand the environment to the parser would drop exactly the
    /// evidence whose whole purpose is to be readable without being a definition. That is not hypothetical — the
    /// in-force channel is the one MSVC's `_STD_BEGIN` arrives through (its `#define` is inside a conditional
    /// region of `yvals_core.h`, so it is a body and not a definition), and a `debug` tool that skipped the
    /// attach showed the invocation read as a declaration head, which is the defect the body was fetched to fix.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty() && self.bodies_in_force.is_empty()
    }

    /// How many names the includes contribute **as definitions**.
    ///
    /// Bodies in force are counted apart on purpose: they are what a rule may *read*, and [`MacroEnvironment::len`]
    /// is the number the seeding measurements report.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    /// What `name` is **at `offset`** — the last entry that has come into force by then, or `None` when the name is
    /// not a macro there (including when an `#undef` has taken it away).
    pub fn kind_of(&self, name: &str, offset: usize) -> Option<SymbolKind> {
        let history = self.by_name.get(name)?;
        // The last entry whose offset has been reached. `partition_point` on a sorted slice, so a query is
        // logarithmic and the parser can ask at every token without a cursor to keep in step.
        let in_force = history[..history.partition_point(|(from, _)| *from <= offset)].last()?;
        in_force.1
    }

    /// What `name` expands to **at `offset`**, as text, when the caller stored it.
    ///
    /// The second half of the positional answer, and the one expansion needs: a shape says which *rule* may look,
    /// this says what the rule will read. See [`IncludedMacro::body_text`].
    /// Add the bodies of macros whose definition is conditional **but in force** — the second channel. See the
    /// field's note for the measurement that keeps the two apart.
    pub fn with_bodies_in_force(
        mut self,
        bodies: impl IntoIterator<Item = (Box<str>, Box<str>)>,
    ) -> Self {
        self.bodies_in_force.extend(bodies);
        self
    }

    /// What a macro's replacement list says, when the only definition of it that is in force is a conditional one.
    ///
    /// For **reading only**: a rule that wants to know whether a name is a macro must ask
    /// [`MacroEnvironment::is_a_macro_at`], which does not consult this.
    pub fn body_text_in_force(&self, name: &str) -> Option<&str> {
        self.bodies_in_force.get(name).map(Box::as_ref)
    }

    pub fn body_text_of(&self, name: &str, offset: usize) -> Option<&str> {
        let history = self.by_name.get(name)?;
        let in_force = history[..history.partition_point(|(from, _)| *from <= offset)].last()?;
        self.body_texts
            .get(&(in_force.0, name.into()))
            .map(Box::as_ref)
    }

    /// Is `name` a macro at `offset`? The question almost every caller asks.
    pub fn is_a_macro_at(&self, name: &str, offset: usize) -> bool {
        matches!(
            self.kind_of(name, offset),
            Some(SymbolKind::Macro { .. })
        )
    }

    /// Does the environment have **any** entry for `name` — even one that says it is not a macro there?
    ///
    /// The difference matters at the call site: "no entry" means the includes say nothing and a weaker source of
    /// evidence may still be consulted, while "an entry that is not a macro" is an answer, and a table that
    /// contradicts it must not be.
    pub fn knows(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// What `name`'s replacement list says **at `at`** — the positional answer first, and the in-force one when
    /// position cannot answer.
    ///
    /// The order is [`MacroEnvironment::body_text_of`]'s and then [`MacroEnvironment::body_text_in_force`]'s, which
    /// is the order every reader of a *body* uses: a definition the include order puts in force here is better
    /// evidence than a branch some condition settled, and the second is only consulted when the first is silent.
    /// `None` means **nobody says** — never "the body is empty", which is a body of zero tokens and a different
    /// answer.
    pub fn body_text_at_or_in_force(&self, name: &str, at: usize) -> Option<&str> {
        self.body_text_of(name, at)
            .or_else(|| self.body_text_in_force(name))
    }

    /// What `name` stands for at `at`, **following a body that is another macro's name**.
    ///
    /// A replacement list whose whole body is one identifier is an *alias*, and the standard library is full of
    /// them: `iosfwd:27` says `#define _TRY_IO_BEGIN _TRY_BEGIN`, and `yvals.h` is where `_TRY_BEGIN` turns out to
    /// be `try {`. A reader that stops at the first hop sees `[Identifier]` and answers "nothing structural", which
    /// is how one `try` inside `<xstring>` cost the class body of `basic_string` and every `std::string` lookup
    /// with it (`docs/grammar-gaps.md` B122).
    ///
    /// Bounded and cycle-safe, because a header is not a proof: the chain is followed at most
    /// [`BODY_CHAIN_LIMIT`] hops, and a name that has already been asked about ends the walk with `None` — "nobody
    /// says", which is the answer that leaves the reading where it was.
    ///
    /// A body that is a name **and something else** (`NAME (args)`) is not a link in a chain: only a body that is
    /// exactly one identifier is, because anything longer is a replacement list in its own right.
    pub fn body_text_resolved<'s>(&'s self, name: &'s str, at: usize) -> Option<&'s str> {
        let mut current = name;
        let mut seen: Vec<&str> = Vec::new();

        loop {
            if seen.contains(&current) || seen.len() >= BODY_CHAIN_LIMIT {
                return None;
            }

            let text = self.body_text_at_or_in_force(current, at)?;

            match a_sole_name_in(text) {
                Some(next) => {
                    seen.push(current);
                    current = next;
                }
                None => return Some(text),
            }
        }
    }
}

/// How many `#define A B` hops [`MacroEnvironment::body_text_resolved`] follows before giving up.
///
/// Eight is far past anything a header writes (the measured chains are one and two hops: `_TRY_IO_BEGIN` →
/// `_TRY_BEGIN` → `try {`) and small enough that a pathological header cannot turn one parse question into a
/// walk of the whole macro table.
pub const BODY_CHAIN_LIMIT: usize = 8;

/// The single identifier a replacement list consists of, when that is the whole of it.
///
/// **Lexed, not matched on the string**, and the first version of this got that wrong in a way worth recording:
/// it trimmed whitespace and asked whether the rest looked like an identifier, which is false for
/// `_TRY_BEGIN // begin try block` — and `iosfwd` writes exactly that. A body is C++ tokens, so the answer comes
/// from the lexer that reads every other body: exactly one **significant** token, and it is a name. A trailing
/// comment is not part of the body's meaning, which is the same rule `kinds_of_a_body_text` applies.
///
/// A body that is a name **and something else** (`NAME (args)`) is not a link in a chain: only a body that is
/// exactly one identifier is, because anything longer is a replacement list in its own right.
fn a_sole_name_in(text: &str) -> Option<&str> {
    use crate::{CppLexer, LexerConfig};

    let mut errors = Vec::new();
    let mut lexer = CppLexer::new(text, LexerConfig::default(), &mut errors);

    let mut significant = lexer.tokenize().into_iter().filter(|token| {
        !matches!(
            token.kind,
            crate::CppTokenKind::Whitespace
                | crate::CppTokenKind::Newline
                | crate::CppTokenKind::LineContinuation
                | crate::CppTokenKind::LineComment
                | crate::CppTokenKind::BlockComment
        )
    });

    let first = significant.next()?;
    if first.kind != crate::CppTokenKind::Identifier {
        return None;
    }
    if significant.next().is_some() {
        return None;
    }

    text.get(first.range.start_offset..first.range.end_offset())
}

/// Does this token kind **head a braced block** — a construct whose body is a `{ … }` this file may not write?
///
/// The list is the keywords that can stand in front of a brace: a namespace or a class-like definition, a linkage
/// block, and the statements that own a body. It is a *closed* grammatical set, like `can_begin_a_declaration`
/// (`docs/grammar-gaps.md` convention 16): every entry says "a `{` may follow me", which is a different claim from
/// "this token may begin a statement".
///
/// **Measured before it was written**: the corpus writes `namespace X {` (`_STD_BEGIN`) and `try {` (`_TRY_BEGIN`,
/// reached through `_TRY_IO_BEGIN`), the second one only once the alias chain of B122 was followed. The statement
/// keywords are here because they are the same shape one construct along — `_CATCH_ALL` is `catch (…) {`, and
/// `do {`, `else {`, `if (…) {` are each written as a macro somewhere — and because a list that had to be extended
/// every time a header spelled one would be extended by whoever hit it next.
fn heads_a_braced_block(kind: Option<crate::CppTokenKind>) -> bool {
    use crate::CppTokenKind;

    matches!(
        kind,
        Some(
            CppTokenKind::NamespaceKeyword
                | CppTokenKind::ClassKeyword
                | CppTokenKind::StructKeyword
                | CppTokenKind::UnionKeyword
                | CppTokenKind::EnumKeyword
                | CppTokenKind::ExternKeyword
                | CppTokenKind::TryKeyword
                | CppTokenKind::CatchKeyword
                | CppTokenKind::DoKeyword
                | CppTokenKind::SwitchKeyword
                | CppTokenKind::IfKeyword
                | CppTokenKind::ElseKeyword
                | CppTokenKind::ForKeyword
                | CppTokenKind::WhileKeyword
        )
    )
}

/// What a macro's replacement list **stands for**, when it stands for one of the shapes a *construct* can be read
/// from — see [`shape_of_a_body`].
///
/// The vocabulary is deliberately two shapes wide, and it is the same two the parser's own rule accepts
/// (`crates/cpp_parser/src/grammar/cpp/stats.rs`, `body_shapes_the_braces`): a namespace definition whose head the
/// file wrote as an invocation, and the closing brace of one. Everything else — an empty body, a namespace *name*
/// (`__8`), a linkage block's head, a statement — is [`BodyShape::Other`], and a consumer must read that as "this
/// body says nothing about the structure", never as "there is no construct here".
///
/// Why the two are separate functions rather than one: the parser's rule reads the **token kinds** of the body
/// (for a `#define` in the file being parsed, kinds are what the directive rule records — the text is the file's
/// own, and re-lexing it would be a second reader free to disagree), while a consumer that has to *name* the
/// namespace needs the **spellings**, which only the text has. The safe direction is built in: a body this
/// classifier refuses opens nothing, so the worst a disagreement can do is leave a file read flat — the reading it
/// has today — and never invent a scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyShape {
    /// `namespace std {` — the segments of the name, outermost first, as the body spells them.
    ///
    /// Empty for `namespace {`, which is a namespace with no name: it opens a scope all the same, and a consumer
    /// must not read "no segments" as "no namespace".
    OpensANamespace(Vec<String>),

    /// `}` — the closing brace of a construct an earlier invocation opened.
    ClosesABlock,

    /// `try {` — a replacement list that **opens a braced block** without naming anything.
    ///
    /// A statement-level opener: `iosfwd`'s `_TRY_IO_BEGIN` (through `_TRY_BEGIN`) is `try {`, and `<xstring>`
    /// writes `_TRY_IO_BEGIN` / `if (…) { … }` / `_CATCH_IO_END` as one statement. Reading the invocation as
    /// anything else put the `if` where the expression rule expected a `;`, and the recovery ate a brace — the
    /// class body of `basic_string` never closed and every `std::string` lookup went with it (B122).
    ///
    /// Nothing is *named*, so a consumer opens no scope for it; what it does is keep a brace's worth of balance,
    /// which is what a statement-level reader needs.
    OpensABlock,

    /// `::std::` — a **nested-name-specifier**: the `::` a qualified name starts with, which this file does not
    /// write. The name after the invocation continues the same qualified name.
    ///
    /// MSVC's STL spells `std::` nowhere: `<vector>` writes `_STD addressof(*_Ptr)` and `yvals_core.h` says
    /// `#define _STD ::std::`. Two names in a row are not an expression in any reading, so without this the
    /// invocation ends the expression and the token after it is a syntax error — which is how one `return`
    /// statement inside a ternary swallowed the remaining 3 800 lines of `<vector>` (`docs/grammar-gaps.md`
    /// B121).
    QualifiesAName,

    /// Anything else, the empty body included.
    Other,
}

impl BodyShape {
    /// The segments of the namespace this body opens — `None` when it opens something else.
    ///
    /// A separate question from "is this an empty list": `namespace {` and `struct S {` are both *not* a named
    /// namespace, and only the first of them opens a scope a consumer may use.
    pub fn namespace_segments(&self) -> Option<&[String]> {
        match self {
            BodyShape::OpensANamespace(segments) => Some(segments),
            _ => None,
        }
    }

    /// Does this body put a **brace** in the file's token stream, without the file writing one?
    ///
    /// True for both openers: a namespace head and a statement-level `try {` each supply a `{` that a reader
    /// counting braces (and a reader keeping a stack of what an invocation opened) has to account for.
    pub fn opens_a_brace(&self) -> bool {
        matches!(self, BodyShape::OpensANamespace(_) | BodyShape::OpensABlock)
    }

    /// Is this the closing brace of a construct an earlier invocation opened?
    pub fn closes_a_block(&self) -> bool {
        matches!(self, BodyShape::ClosesABlock)
    }

    /// Does this body say that a construct opens or closes here — a **scope**, of either kind?
    pub fn is_structural(&self) -> bool {
        matches!(
            self,
            BodyShape::OpensANamespace(_) | BodyShape::ClosesABlock
        )
    }

    /// Does this body open **something braced**, whether or not it names it?
    ///
    /// The question a reader asks when it needs the brace's worth of balance and not a scope: a statement-level
    /// opener whose closer is another macro.
    pub fn opens_a_block(&self) -> bool {
        matches!(self, BodyShape::OpensABlock)
    }

    /// Does this body **continue a name** rather than stand for a construct of its own?
    ///
    /// One shape answers yes: a replacement list that ends at a `::`. A reader that has it can read the name
    /// written after the invocation as part of the same qualified name; a reader that does not must treat the
    /// invocation as the whole of what the file wrote there.
    pub fn qualifies_a_name(&self) -> bool {
        matches!(self, BodyShape::QualifiesAName)
    }

    /// Does a **reading** change if this body is known — either of the two questions above?
    ///
    /// The question a caller asks when it decides whether fetching the environment is worth anything for a file:
    /// [`crate::SummaryStore`](crate::SummaryStore)'s second pass asks it of every name the closure defines, and a
    /// body that answers `false` (`__declspec(dllexport)`, a statement, a type) is one no rule consults.
    pub fn a_reading_uses_this(&self) -> bool {
        !matches!(self, BodyShape::Other)
    }
}

/// What the replacement list `text` stands for: the two shapes of [`BodyShape`], and nothing else.
///
/// Lexed rather than pattern-matched on the string, because a body is C++ tokens and not text: `namespace\`
/// `std /* the standard library */ {` is the same body as `namespace std {`, and a `str::starts_with` would miss
/// it while a `str::contains` would accept `// namespace std {` in a comment. The lexer that reads the file reads
/// the body — the same call [`CppParser::record_macro_body`] makes on the other channel.
///
/// Accepted shapes, exactly:
///
/// ```text
/// namespace std {              → OpensANamespace(["std"])
/// namespace a :: b {           → OpensANamespace(["a", "b"])
/// namespace {                  → OpensANamespace([])          an unnamed namespace is still a scope
/// }                            → ClosesABlock
/// :: std ::                    → QualifiesAName               `_STD`, whose `::` the file does not write
/// namespace std                → Other    a name, not a head — it stands for a specifier, not a construct
/// __8                          → Other
/// inline namespace _V2 {       → Other    measured: not claimed, see BodyShape's note
/// (anything else, "" included) → Other
/// ```
pub fn shape_of_a_body(text: &str) -> BodyShape {
    use crate::{CppLexer, CppTokenKind, LexerConfig};

    let mut errors = Vec::new();
    let mut lexer = CppLexer::new(text, LexerConfig::default(), &mut errors);
    let tokens: Vec<_> = lexer
        .tokenize()
        .into_iter()
        .filter(|token| {
            !matches!(
                token.kind,
                CppTokenKind::Whitespace
                    | CppTokenKind::Newline
                    | CppTokenKind::LineContinuation
                    | CppTokenKind::LineComment
                    | CppTokenKind::BlockComment
            )
        })
        .collect();

    let kinds: Vec<CppTokenKind> = tokens.iter().map(|token| token.kind).collect();
    if kinds.as_slice() == [CppTokenKind::RightBrace] {
        return BodyShape::ClosesABlock;
    }

    // A body that **ends at a `::`** is a nested-name-specifier — `::std::`, `_STDEXT::` — and it is asked
    // *before* the namespace test because the two are distinguished by their last token rather than their first:
    // `namespace std {` ends at `{`, and `::std::` ends at `::`. A one-token body of just `::` is accepted too:
    // there is no other reading of it.
    if kinds.last() == Some(&CppTokenKind::Scope) {
        return BodyShape::QualifiesAName;
    }

    // Everything below **requires the body to end at a `{`**: a replacement list that does not is a name, a
    // specifier, or an expression, and it opens nothing. `namespace std` (no brace) is the near miss that says so.
    if kinds.last() != Some(&CppTokenKind::LeftBrace) {
        return BodyShape::Other;
    }

    // `namespace` first and `{` last, with nothing between them but the name's own tokens. The middle test is what
    // keeps `namespace std = other;` (an alias) and `namespace std;` out: both end somewhere else.
    if kinds.first() != Some(&CppTokenKind::NamespaceKeyword) {
        // …and a body that ends at a `{` without naming a namespace **opens a block**: `try {`, `do {`,
        // `switch (x) {`, `extern "C" {`. Nothing is named, and a statement-level reader needs exactly that much —
        // the brace's balance (B122).
        return if heads_a_braced_block(kinds.first().copied()) {
            BodyShape::OpensABlock
        } else {
            BodyShape::Other
        };
    }

    let mut segments = Vec::new();
    for token in &tokens[1..tokens.len() - 1] {
        match token.kind {
            CppTokenKind::Identifier => {
                segments.push(text[token.range.start_offset..token.range.end_offset()].to_string())
            }
            CppTokenKind::Scope => {}
            _ => return BodyShape::Other,
        }
    }

    BodyShape::OpensANamespace(segments)
}

/// A table backed by a map — the reference implementation, for tests and for a caller whose index is simple
/// enough that it does not need a type of its own.
///
/// ```no_run
/// use cpp_parser::{MacroBody, ParserConfig, SymbolKind, SymbolMap};
///
/// let mut symbols = SymbolMap::new();
/// symbols.insert("MY_API", SymbolKind::Macro { function_like: false, body: MacroBody::Specifier });
/// symbols.insert("Widget", SymbolKind::Type);
///
/// let config = ParserConfig::default().with_symbol_table(&symbols);
/// ```
#[derive(Debug, Default, Clone)]
pub struct SymbolMap {
    entries: std::collections::HashMap<Box<str>, SymbolKind>,
}

impl SymbolMap {
    pub fn new() -> Self {
        SymbolMap::default()
    }

    /// Record what `name` is, replacing any earlier answer for it.
    pub fn insert(&mut self, name: &str, kind: SymbolKind) {
        self.entries.insert(name.into(), kind);
    }

    /// [`SymbolMap::insert`], for building a table in one expression.
    pub fn with(mut self, name: &str, kind: SymbolKind) -> Self {
        self.insert(name, kind);
        self
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl SymbolTable for SymbolMap {
    fn kind_of(&self, name: &str) -> Option<SymbolKind> {
        self.entries.get(name).copied()
    }
}

impl<T: SymbolTable + ?Sized> SymbolTable for &T {
    fn kind_of(&self, name: &str) -> Option<SymbolKind> {
        (**self).kind_of(name)
    }
}

/// What a name is.
///
/// The vocabulary is the parser's, not the index's: each variant is here because a *reading* turns on it, and the
/// documentation of each says which one. A symbol the parser has no use for (a label, a parameter, a
/// `using`-declaration's target) does not belong here — an implementation that cannot classify a name answers
/// `None` and leaves the fallback in charge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    /// A class, struct, union, enum or alias — a name that can stand where a **type** goes.
    ///
    /// The single most valuable answer: it settles `Widget w(1, 2);` against `g(1, 2);`, and `(Widget)x` against
    /// `(x)`, without any of the shape heuristics that exist for the case where nobody knows.
    Type,

    /// A **template's** name: `std::vector`, a user's `Vec`.
    ///
    /// A `Type` and a template are the same name until a `<` follows it, and then they are not: a name that is
    /// known to be a template makes the `<` after it template arguments rather than a comparison, which is the
    /// question `a_matching_angle_bracket_follows` has to guess at today.
    Template,

    /// A **macro**, with the two properties the grammar can act on.
    ///
    /// A macro is expanded before the grammar runs, so an invocation leaves behind whatever its body produced —
    /// which is precisely what [`MacroBody`] describes, and the reason this variant carries data where the others
    /// do not.
    Macro {
        /// Is it invoked with arguments — `NAME(x)` — or used bare?
        ///
        /// An object-like macro (`#define MY_API __declspec(dllexport)`) stands where a *specifier* goes; a
        /// function-like one stands where a call does.
        function_like: bool,
        /// What the body expands to, as far as the table knows.
        body: MacroBody,
    },

    /// A free or member function.
    ///
    /// The **negative** answer that the parser cannot reach on its own: `g(1, 2);` is a call because `g` is a
    /// function, even where `g(1, 2)` would also parse as a declaration of a variable `g` with a direct
    /// initialiser. `None` cannot say this, which is why the vocabulary has variants the parser may only ever use
    /// to *refuse* a reading.
    Function,

    /// A variable, field, parameter or enumerator: a name that is a value, never a type.
    Variable,

    /// A namespace: a name that a `::` continues.
    Namespace,
}

/// What a macro's body expands to — the part of a macro the *grammar* cares about.
///
/// A macro whose body is unknown is still useful to know about (the name is a macro, so `NAME(x)` is not a call
/// to a function), but the shapes below are what turn a guess into a reading:
///
/// ```text
/// MacroBody::Specifier       #define MY_API __declspec(dllexport)      MY_API Widget *p;
/// MacroBody::Statement       #define NUMBER_OPTION(op) if (…) { … }   NUMBER_OPTION(x)      no `;`
/// MacroBody::Statement       #define ASSERT(c) do { } while (false)   ASSERT(x);            with `;`
/// MacroBody::Block           #define TEST(a, b) …                     TEST(A, B) { … }
/// MacroBody::Expression      #define MAX(a, b) ((a) > (b) ? (a) : (b))  auto m = MAX(1, 2);
/// MacroBody::Type            #define MY_INT int                       MY_INT x = 1;
/// ```
///
/// The distinction that earns the most is `Statement` against `Block`: both may be followed by a block, and only
/// the first may appear **without** a `;` and without anything after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroBody {
    /// Modifiers, attributes, an export marker: the macro stands where a *declaration specifier* goes.
    Specifier,

    /// A whole statement, `;` included or implied by a block of its own.
    ///
    /// The invocation may be written without a trailing `;`, and may be followed by a block that belongs to it —
    /// which is the shape `NUMBER_OPTION(x)` and `IF_EXIST(x) { … }` are.
    Statement,

    /// A statement that *requires* a block: the invocation is always followed by `{ … }`, which is part of what
    /// the macro wrote (gtest's `TEST(A, B)`, a `#define` that opens a scope).
    Block,

    /// An expression, so the invocation is an operand and nothing more: `auto m = MAX(1, 2);`.
    Expression,

    /// A type name or a type expression, so the invocation can stand where a type goes.
    Type,

    /// The table knows the name is a macro and nothing about its body.
    ///
    /// The ordinary answer for a macro defined in a header, and not a failure: it already rules out the *call*
    /// reading, which is what most of the value is.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::{
        BodyShape, IncludedMacro, MacroBody, MacroEnvironment, NoSymbols, SymbolKind, SymbolMap,
        SymbolTable, shape_of_a_body,
    };

    /// **Only two bodies are structural**, and everything next to them is not — this is the vocabulary the
    /// scope-building step reads a namespace's name out of, so the near misses matter as much as the hits.
    ///
    /// The two hits are what the corpora write: `_STD_BEGIN`/`_STDEXT_BEGIN` (MSVC) and libstdc++'s
    /// `_GLIBCXX_BEGIN_NAMESPACE_VERSION`, whose bodies are `namespace std {`, `namespace stdext {` and
    /// `namespace __8 {`. The misses are each one token away from a hit, and each would be a wrong scope if it
    /// were accepted: `namespace std` is the *name* half of a head (used where a specifier goes), `__8` is a
    /// namespace name, `inline namespace _V2 {` is a head the parser's own rule does not claim either, and an
    /// alias is a declaration rather than an opening brace.
    #[test]
    fn only_two_body_shapes_are_structural() {
        let segments = |text: &str| shape_of_a_body(text).namespace_segments().map(<[String]>::to_vec);

        // The hits. Whitespace, comments and a `\`-splice are not part of the answer: this is lexed, not matched.
        assert_eq!(segments("namespace std {"), Some(vec!["std".to_string()]));
        assert_eq!(segments("namespace {"), Some(Vec::new()), "unnamed, and still a scope");
        assert_eq!(
            segments("namespace a :: b {"),
            Some(vec!["a".to_string(), "b".to_string()]),
            "a nested name is one declaration that opens one scope per segment"
        );
        assert_eq!(
            segments("namespace std /* the library */ {"),
            Some(vec!["std".to_string()]),
            "a comment is not part of the body"
        );
        assert_eq!(
            segments("namespace\\\n std {"),
            Some(vec!["std".to_string()]),
            "and neither is a line continuation"
        );
        assert!(shape_of_a_body("}").closes_a_block());
        assert!(shape_of_a_body(" } ").closes_a_block(), "trivia is dropped first");

        // The misses.
        for other in [
            "",
            "std",
            "__8",
            "namespace std",
            "namespace std = other;",
            "inline namespace _V2 {",
            "};",
            "{",
            "((a) > (b) ? (a) : (b))",
        ] {
            assert_eq!(
                shape_of_a_body(other),
                BodyShape::Other,
                "{other:?} is not one of the shapes a construct is read from"
            );
        }

        // **A statement-level opener is a third shape** (B122): it names nothing, and it braces something. Read
        // from the same list of block heads, so the two vocabularies (`symbols.rs` here and the parser's
        // `body_shapes_the_braces`) cannot disagree about which bodies are openers.
        for opener in ["try {", "do {", "switch (x) {", "if (a) {", "extern \"C\" {"] {
            assert_eq!(
                shape_of_a_body(opener),
                BodyShape::OpensABlock,
                "{opener:?} opens a brace and names nothing"
            );
            assert!(shape_of_a_body(opener).opens_a_brace());
            assert!(!shape_of_a_body(opener).is_structural(), "…and opens no scope");
            assert!(shape_of_a_body(opener).namespace_segments().is_none());
        }

        // And the questions are not the same question: an unnamed namespace opens a *scope* and has no segment.
        assert_eq!(shape_of_a_body("namespace {").namespace_segments(), Some(&[][..]));
        assert!(shape_of_a_body("namespace {").is_structural());
        assert!(shape_of_a_body("namespace {").opens_a_brace());
        assert!(!shape_of_a_body("namespace std").is_structural());
        assert!(shape_of_a_body("namespace std").namespace_segments().is_none());
    }

    /// **A body that is another macro's name is followed** (B122), because one hop is what a header writes and one
    /// hop is the difference between "nothing structural" and `try {`.
    ///
    /// Both spellings the corpus has: a plain alias (`_TRY_IO_BEGIN` → `_TRY_BEGIN`) and one with a trailing
    /// comment (`iosfwd` writes `#define _TRY_IO_BEGIN _TRY_BEGIN // begin try block`), which the first version of
    /// this missed because it tested the *text* instead of lexing it.
    ///
    /// The negatives are the boundaries: a body that is a name **and something else** is not a link, a cycle ends
    /// the walk, and a chain longer than the limit gives up rather than looping.
    #[test]
    fn a_body_that_is_another_macros_name_is_followed() {
        let environment = MacroEnvironment::from_included_macros([]).with_bodies_in_force([
            (Box::from("_TRY_IO_BEGIN"), Box::from("_TRY_BEGIN // begin try block")),
            (Box::from("_TRY_BEGIN"), Box::from("try {")),
            (Box::from("_A"), Box::from("_B")),
            (Box::from("_B"), Box::from("_A")),
            (Box::from("_CALL"), Box::from("_CATCH_ALL _CATCH_END")),
        ]);

        assert_eq!(environment.body_text_resolved("_TRY_IO_BEGIN", 0), Some("try {"));
        assert_eq!(environment.body_text_resolved("_TRY_BEGIN", 0), Some("try {"));

        // A body with more than the name is a replacement list in its own right, not an alias.
        assert_eq!(
            environment.body_text_resolved("_CALL", 0),
            Some("_CATCH_ALL _CATCH_END")
        );

        // A cycle — `#define _A _B` and `#define _B _A` — answers "nobody says" instead of looping.
        assert_eq!(environment.body_text_resolved("_A", 0), None);

        // And a name nobody defines is still "nobody says".
        assert_eq!(environment.body_text_resolved("_UNKNOWN", 0), None);
    }

    /// **The evidence is positional**, which is the whole reason this exists: the same name is a macro in one
    /// region of a file and not in another, and a flat table cannot say so — feeding one cost 46 files in
    /// `docs/roadmap.md` §2.0.
    #[test]
    fn a_macro_from_an_include_is_in_force_only_from_its_own_offset() {
        let environment = MacroEnvironment::from_included_macros([
            IncludedMacro::defined_at(100, "BOOL_OPTION", true, MacroBody::Statement),
            IncludedMacro::undefined_at(300, "BOOL_OPTION"),
        ]);

        assert_eq!(environment.len(), 1, "one name, one history");
        assert!(!environment.is_a_macro_at("BOOL_OPTION", 99), "not yet included");
        assert!(
            environment.is_a_macro_at("BOOL_OPTION", 100),
            "the offset it becomes visible at is its own"
        );
        assert!(environment.is_a_macro_at("BOOL_OPTION", 299), "still in force");
        assert!(
            !environment.is_a_macro_at("BOOL_OPTION", 300),
            "an `#undef` takes it away again"
        );
        assert!(
            environment.knows("BOOL_OPTION"),
            "…and 'not a macro here' is an answer, not an absence"
        );
        assert!(!environment.knows("SOMETHING_ELSE"), "a name it never heard of");
    }

    /// Entries may arrive in any order — the index walks the include graph, not the offsets — and the answer must
    /// not depend on that.
    #[test]
    fn the_environment_sorts_what_it_is_given() {
        let environment = MacroEnvironment::from_included_macros([
            IncludedMacro::undefined_at(300, "M"),
            IncludedMacro::defined_at(100, "M", false, MacroBody::Specifier),
        ]);

        assert!(environment.is_a_macro_at("M", 150));
        assert!(!environment.is_a_macro_at("M", 350));
    }

    #[test]
    fn a_map_answers_what_was_inserted_and_nothing_else() {
        let mut symbols = SymbolMap::new();
        symbols.insert(
            "MY_API",
            SymbolKind::Macro {
                function_like: false,
                body: MacroBody::Specifier,
            },
        );
        symbols.insert("Widget", SymbolKind::Type);

        assert_eq!(symbols.kind_of("Widget"), Some(SymbolKind::Type));
        assert!(matches!(
            symbols.kind_of("MY_API"),
            Some(SymbolKind::Macro {
                function_like: false,
                body: MacroBody::Specifier
            })
        ));
        assert_eq!(
            symbols.kind_of("g"),
            None,
            "a name the table has never heard of is unknown, not a `no`"
        );
        assert_eq!(symbols.len(), 2);
        assert!(SymbolMap::new().is_empty());
    }

    #[test]
    fn the_interface_is_object_safe_and_shared() {
        // The two shapes the parser will hold: a trait object, and a reference that outlives the parse. Both are
        // checked here because neither can be discovered from a caller's error message once the grammar uses them.
        let table: &dyn SymbolTable = &NoSymbols;
        assert_eq!(table.kind_of("anything"), None);

        let map = SymbolMap::new().with("Widget", SymbolKind::Type);
        let borrowed: &dyn SymbolTable = &map;
        assert_eq!(borrowed.kind_of("Widget"), Some(SymbolKind::Type));

        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SymbolMap>();
        assert_send_sync::<NoSymbols>();
        assert_send_sync::<SymbolKind>();
    }
}
