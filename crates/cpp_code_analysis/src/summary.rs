//! What the index remembers about **one file** — the unit the whole cache is built from.
//!
//! This module is the shape; the builder that fills it from a tree lands next, and the encoder after that. What is
//! fixed here is the vocabulary and the rules that make a summary *indexable*:
//!
//! # Facts, not conclusions
//!
//! Everything below is something the file **says** — a declaration, a `#define`, an `#include`, a module. Nothing
//! is a resolved answer: no "this name refers to that declaration", no "this type is `int`", no expansion result.
//! That is an invariant, and the reason is invalidation: a `#define` changes the
//! meaning of every name below it in every file that includes it, so a stored conclusion would have to be
//! recomputed project-wide, while a stored *fact* goes stale exactly when its own file changes.
//!
//! # Every fact says which branch it is on
//!
//! A declaration inside `#if defined(_WIN32)` exists on some machines and not others, and an editor must be able
//! to say which — so every fact carries a [`FactGuard`]: the region it was written in, as an index into the file's
//! guard list. The list itself is per-file data (see [`SummaryGuards`]), which is what keeps a fact small and lets
//! two facts from the same region share one answer.
//!
//! # Names are stored as written
//!
//! A [`DeclFact`] carries the spelling in the source (`Widget`, `ns::Widget`) and how it was qualified, never a
//! resolved identity. Resolving is a query over this data plus the include graph, and it needs the *environment* the
//! file was entered with — which is exactly what the summary's key records.

use crate::cache::SummaryKey;
use crate::guard::{Branch, Region, Visibility};
use crate::index::environment::a_guard_is_in_force;
use crate::macros::MacroDef;
use crate::Marked;

/// A declaration the file writes: enough to find it, name it, and say what kind of thing it is.
///
/// Deliberately not a syntax tree and not a type: the fields are the ones a *query* needs — where to jump, what to
/// call the symbol, what to filter by, and which branch it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclFact {
    /// The name as written, without qualification: `Widget` in `ns::Widget`.
    pub name: String,
    /// The **qualified** name of the scope the declaration was written in, without the name itself: `ns::C` for a
    /// member of that class.
    ///
    /// The one field that makes a flat list of declarations indexable. A name alone does not identify a
    /// declaration — two files may each declare `Widget`, and one file may declare `f` at file scope and again as
    /// a member — so a lookup needs the scope, and the qualified spelling is the scope's identity that survives
    /// being written to disk. A [`ScopeId`] would not: it is only meaningful inside the tree it came from.
    ///
    /// `None` for a declaration at file scope, which is what distinguishes `f` from `ns::C::f`. It is also `None`
    /// for a declaration inside a construct that has no qualified name — a local in a function body, a member of an
    /// anonymous class — because those genuinely have none, and an empty string would be a name that matches
    /// nothing while looking like an answer.
    ///
    /// [`ScopeId`]: crate::ScopeId
    pub scope: Option<String>,
    /// Was this declaration written inside a **function body, a block or a lambda**?
    ///
    /// The other half of [`DeclFact::scope`], and the reason it is a field rather than something a consumer can
    /// work out: a fact whose scope is `None` is either a declaration at *file* scope — `void helper();`,
    /// `extern int errno;`, which every file that includes this one can name — or a **local**, which nothing
    /// outside its own body can name at all. The two are indistinguishable from the rest of the fact, and the
    /// difference decides answers: a name lookup that answered with another file's local would be offering a name
    /// the user cannot see, and `std::vector`'s headers alone declare thousands of them (`__first`, `__n`, `_Tp`),
    /// so the wrong answer is not a corner case — it is most of what a completion would show.
    ///
    /// `true` for a declaration whose scope chain reaches a function body, a block or a lambda, however deeply it
    /// is nested in there — a local class, a local `typedef`, a variable inside a loop inside a member function.
    /// `false` at file scope, in a namespace, and in a class body, including a member function's *declaration* —
    /// the body is what makes a declaration local, not the entity it belongs to.
    ///
    /// # What a consumer is expected to do with it
    ///
    /// The index is a per-file list of declarations, so it cannot place a local in the function it belongs to;
    /// [`crate::ProjectIndex::definition`] therefore **skips** these facts when it looks a name up across files,
    /// because the scope tree of the file being edited is the only thing that can resolve one. A consumer with the
    /// file's own scopes in hand (a completion in the open buffer) lists locals from there and never needs this
    /// field; a consumer listing *another* file's declarations uses it to leave them out.
    pub local: bool,
    pub kind: DeclKind,
    /// The type a variable-like declaration was written with, as the file spells it.
    ///
    /// The first field in a fact that is not simply "what the file says about this name" but "what the file says
    /// about the *type* of it", and it exists for one query: `widget.size` is answered by finding `Widget` and
    /// then `size` inside it, and nothing else in a fact says how to get from `widget` to `Widget`.
    ///
    /// Three limits, all deliberate:
    ///
    /// * **As written, not resolved.** `Widget`, `ns::Widget` and `std::vector<int>` are stored as spelled, so a
    ///   consumer resolves them with the qualified-name machinery that already exists — and gets `Unknown` where
    ///   that machinery cannot go, rather than a guess.
    /// * **Only for variables, fields and parameters.** A class or a function declares no type in this sense: a
    ///   class *is* one and a function *returns* one, and those spellings live in a different part of the syntax.
    ///   `None` is the honest answer for them.
    /// * **Declaration specifiers are stripped.** `static const Widget` records `Widget`, because a specifier is
    ///   not part of the type's name and a lookup by name is what this is for. `unsigned long` survives, because
    ///   there the words *are* the type.
    pub type_of: Option<String>,
    /// The type a **function** returns, as the file spells it.
    ///
    /// The other half of [`DeclFact::type_of`], and separate from it for the reason that field's documentation
    /// gives: a class *is* a type and a function *returns* one, so `Widget make();` has no `type_of` — `make` is
    /// not a `Widget` and has no members — while its `returns` is what the *call* `make()` has. Collapsing the two
    /// would make `make.size` resolve as if the function were the object.
    ///
    /// It exists for one query: `make().size` is answered by finding what `make` returns and then `size` inside
    /// that, and nothing else in a fact says how to get from a call to a type.
    ///
    /// Same limits as `type_of`: **as written**, so a consumer resolves it with the qualified-name machinery and
    /// gets `Unknown` where that cannot go; declaration specifiers stripped (`static inline Widget make()` records
    /// `Widget`); `None` for everything that is not a function.
    ///
    /// A **trailing return type wins**, and it is the reason this cannot be read out of the specifier sequence
    /// alone: `auto make() -> Widget` spells the type after the parameter list, so the specifiers say `auto`.
    /// `None` for a return type the file does not state — `auto make() { … }` is deduced, and `auto` is not a
    /// class anything can be looked up in.
    pub returns: Option<String>,
    /// For a class-like declaration, the base classes it was written with, in declaration order.
    ///
    /// Spelled as written — `B`, `ns::C`, `Base<int>` — for the same reason [`DeclFact::type_of`] is: a consumer
    /// resolves them with the machinery that already exists, and `Unknown` where it cannot go beats a guess.
    ///
    /// Empty for a class with no bases **and** for everything that is not a class, which is one answer because it
    /// is the same answer to the question a consumer is asking: a member that is not here is not inherited from
    /// anywhere this declaration knows about.
    ///
    /// Access and `virtual` are not recorded. They decide whether a member is *reachable* and how the class is
    /// laid out, and this is a fact about the text rather than a semantic property — a lookup that used them would
    /// be the first thing here to need real semantics, and it would need the whole of them.
    ///
    /// # What is *not* in a derived class's fact, and why
    ///
    /// The members `B` happens to have are **not** copied onto `D`. The list is walked at query time
    /// (`index::project::members_of`), and the reason is the one thing a per-file key cannot catch: whether an
    /// added member of `B` reaches `D` depends on `D`'s base list, and *which* `B` the spelling refers to depends
    /// on macros and includes `D` never mentions. A stored copy would therefore go stale while `D`'s own text and
    /// key stayed identical, so nothing would invalidate it. `bases` is a spelling; the chain is a query.
    pub bases: Vec<String>,
    /// The whole declaration, for a "go to definition" highlight.
    pub range: cpp_parser::SourceRange,
    /// Just the name, which is what a reference search matches. Separate from `range` for the reason
    /// [`crate::Binding`] documents: collapsing them renames whole declarations.
    pub name_range: cpp_parser::SourceRange,
    /// Was this declaration read **without a diagnostic touching it**?
    ///
    /// `false` when a parse error fell inside the declaration this fact was written in. The tokens are all there
    /// and the node is well formed — the parser is total — but the reading around it was *recovered from*, so
    /// what this declaration says is not to be trusted. This is the record 's third
    /// invariant asks for: the parser is tolerant, so the layers above have to be able to see how much was
    /// recovered.
    ///
    /// # Why per fact, and not per file
    ///
    /// Measured on the closure of `<vector>` (279 files from one compiler): **5388 of 8499** declarations come
    /// from files that do not parse cleanly, and a per-file rule would throw all of them away — including the 157
    /// `std::` types that are indexed correctly today (`std::allocator`, `std::pair`, `std::tuple`). Of those
    /// 5388, only **214 — 4% —** are marked unclean here, and the 3111 declarations of the clean files are all
    /// clean: **8285 of 8499** declarations survive the question. The question is therefore about a declaration
    /// rather than about a file, and by a factor of twenty-five.
    ///
    /// # The range the question is asked about
    ///
    /// Not the fact's own range, which is the declarator: a diagnostic in the **type** is outside the declarator
    /// and inside the declaration, and the type is what a consumer reads off `type_of`. What is asked about is
    /// the innermost declaration node containing this fact's name. Measured, that costs 108 declarations over the
    /// declarator rule (106 against 214, both against 5388 for the file), and the 108 are exactly the ones worth
    /// having: by node kind, 89 are `Declaration` and 19 are `TemplateDecl` — the type part and the template
    /// header. The looser rule, "any declaration whose range contains the name", marks **2907** — one error in a
    /// class body would condemn every member of it — which is why the innermost one is the rule.
    ///
    /// # What it does not mean
    ///
    /// `true` is **not** a promise that the declaration is right. A recovery can land badly *before* a
    /// declaration and change where it belongs: the closing `}` of a block eaten by an error turns everything
    /// after it into locals, and those declarations' own tokens are untouched, so they are clean while their
    /// *scope* is wrong. A diagnostic that falls inside no declaration at all — an unexpected token at file
    /// scope, between two declarations — marks neither of them, and neither does one that lands in a declaration
    /// the walk produced no fact for, which is what a failed declarator usually does: the fact is then missing
    /// rather than unclean. The field claims exactly one thing, and it is the one thing the parser can answer:
    /// whether a diagnostic fell inside the declaration this fact was written in.
    pub clean: bool,
    pub guard: FactGuard,
}

/// A place where a file's **structure** was read from a macro's replacement list rather than from its own tokens.
///
/// The one kind of fact in a summary whose evidence is not in the file it describes. MSVC's `<vector>` writes
/// `_STD_BEGIN` on a line of its own and `namespace std {` nowhere at all — the braces that scope its 165
/// declarations are in `yvals_core.h` — so "everything here is in `std`" is a reading of *another file's* text.
///
/// # Why this is stored rather than recomputed
///
/// The cache key is the text and the compilation context (`cache.rs`), and it deliberately does not name the macro
/// environment, because a key that had to be computed after the parse would make the disk cache save writes instead
/// of parses. So a summary *can* be read under an environment that has since changed — and the answer to that is not
/// to pretend it cannot happen but to make it **checkable**: the body that licensed the reading is kept verbatim, so
/// a consumer that can ask the include graph again (`ProjectIndex::macro_environment`, and
/// [`crate::macros_from_the_closure_with_bodies`] for a whole closure) can compare the two and rebuild instead of
/// trusting. A consumer that cannot ask still knows where the answer came from, which is strictly more than a bare
/// `scope: "std"` gives it.
///
/// # What it is not
///
/// Not a claim that the name is *declared* here: no binding is created for a scope opened by a body
/// ([`crate::build_scopes`]), so a rename of `std` cannot reach the `_STD_BEGIN` invocation. This records the
/// reading, and the reading is all it records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroScopeReading {
    /// Where the invocation is written in this file.
    pub range: cpp_parser::SourceRange,
    /// The macro's name, as this file spells it.
    pub name: String,
    /// The replacement list it was read as, verbatim — the evidence itself.
    pub body: String,
    /// The namespace it opened, outermost segment first; empty for `namespace {`.
    ///
    /// `None` for an invocation whose body is `}` — the closing half of a reading, which is why the two cases are
    /// distinguished by the option rather than by an empty list.
    pub opens: Option<Vec<String>>,
}

impl MacroScopeReading {
    /// Does this invocation open a scope — as opposed to closing one?
    pub fn opens_a_scope(&self) -> bool {
        self.opens.is_some()
    }
}

/// One entry in a file's **outline**: a declaration the file writes, and the declarations written inside it.
///
/// The fact travels whole rather than as a copy of the three fields a consumer happens to need today: an outline
/// entry is a declaration, and a caller that then asks "what kind", "where is the name", "what are its bases" is
/// asking about the same thing it is looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineSymbol {
    /// The declaration, with both ranges a consumer needs: [`DeclFact::range`] is the whole declaration (what a
    /// client folds or highlights) and [`DeclFact::name_range`] is the name (what it selects).
    pub fact: DeclFact,
    /// The declarations written **inside** this one, in source order.
    pub children: Vec<OutlineSymbol>,
}

impl FileSummary {
    /// **The file's declarations as a tree**, in source order — what an outline, a breadcrumb bar or a folding
    /// range is drawn from.
    ///
    /// # Which reading this is, and why it is not the cooked one
    ///
    /// The **raw** reading, deliberately. Every other query in this crate answers from the cooked one where it can,
    /// because a compiler's reading is the truth about what a name means — an outline is a claim about the **file**,
    /// and the two differ in both directions:
    ///
    /// * a declaration in a branch nobody takes is in an outline and was never compiled ✓ (this is the one reading
    ///   that *wants* the dead branches — a reader editing them is looking at them);
    /// * a type a macro declares is not: `DECLARE_HANDLE(HWND)`'s `HWND__` is in the index and in completions, and
    ///   an outline of `api.h` that listed it would be listing a name the file never writes.
    ///
    /// # The tree, from the facts themselves
    ///
    /// A declaration's [`DeclFact::scope`] is the **qualified name** of the scope it was written in, so a member's
    /// parent is the fact whose [`DeclFact::qualified_name`] equals it — one map lookup per fact, and a fact whose
    /// parent is not in this file is a root (`Widget`'s members are in `widget.h`, not in the file that includes
    /// it). Source order comes from [`DeclFact::range`]: a parent begins before everything written inside it, so one
    /// sorted pass places every fact under a parent that is already placed.
    ///
    /// Two facts of one qualified name — an overload — keep one entry in the map, so the second one's children (if
    /// it had any) would attach to the first: an overload writes no children of its own, and a name *is* what a
    /// scope's children are keyed by.
    ///
    /// # What is not in it
    ///
    /// **Locals** ([`DeclFact::local`]). An outline is the file's structure and not the inside of every function —
    /// and a summary cannot place a local in the function it belongs to anyway, since a local's scope is `None` by
    /// construction (see [`DeclFact::scope`]). A consumer that wants them has the file's own scope tree, which is
    /// what [`crate::FileView::scopes`] is for.
    pub fn outline(&self) -> Vec<OutlineSymbol> {
        outline_of(&self.declarations)
    }
}

/// **A list of declarations as a tree**, in source order — the builder behind [`FileSummary::outline`].
///
/// A free function because the facts do not have to come from a summary: the buffer's own parse produces the same
/// kind of list, and the session reaches for it when a file has just been edited and its summary is gone
/// (`Session::outline`). One builder, so the two readings cannot disagree about what a tree is.
pub fn outline_of(facts: &[DeclFact]) -> Vec<OutlineSymbol> {
    let mut facts: Vec<&DeclFact> = facts.iter().filter(|fact| !fact.local).collect();
    facts.sort_by_key(|fact| (fact.range.start_offset, fact.range.end_offset()));

    let mut roots: Vec<OutlineSymbol> = Vec::new();
    // Where each placed symbol is, by qualified name: the path of child indices from the root. A path rather
    // than a search, so that placing a fact is a walk down a few indices instead of a scan of the tree.
    let mut placed: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();

    for fact in facts {
        let qualified = fact.qualified_name();
        let symbol = OutlineSymbol {
            fact: fact.clone(),
            children: Vec::new(),
        };

        let path = match fact.scope.as_ref().and_then(|scope| placed.get(scope)) {
            Some(parent) => {
                let parent = parent.clone();
                let mut path = parent.clone();
                path.push(symbol_at(&mut roots, &parent).children.len());
                symbol_at(&mut roots, &parent).children.push(symbol);
                path
            }
            None => {
                roots.push(symbol);
                vec![roots.len() - 1]
            }
        };

        placed.insert(qualified, path);
    }

    roots
}

/// The symbol a path of child indices names — `[2, 0]` is the first child of the third root.
fn symbol_at<'a>(roots: &'a mut [OutlineSymbol], path: &[usize]) -> &'a mut OutlineSymbol {
    let (first, rest) = path.split_first().expect("a path has at least one index");
    let mut node = &mut roots[*first];

    for index in rest {
        node = &mut node.children[*index];
    }

    node
}

impl DeclFact {
    /// The declaration's full name, qualified by the scope it was written in.
    ///
    /// What a symbol search shows and what a lookup keys on. Joining is done here rather than stored, so the two
    /// halves cannot disagree — and a caller that wants the segments separately still has them.
    pub fn qualified_name(&self) -> String {
        match &self.scope {
            Some(scope) if !self.name.is_empty() => format!("{scope}::{}", self.name),
            Some(scope) => scope.clone(),
            None => self.name.clone(),
        }
    }

    /// The segments of [`DeclFact::qualified_name`], outermost first.
    ///
    /// For a consumer that needs to walk a qualification rather than print it — resolving `ns::C::f` one segment
    /// at a time is the ordinary case, and splitting a joined string at every step would be work done per query.
    pub fn qualified_segments(&self) -> Vec<&str> {
        let mut segments: Vec<&str> = Vec::new();

        if let Some(scope) = &self.scope {
            segments.extend(scope.split("::"));
        }
        if !self.name.is_empty() {
            segments.push(&self.name);
        }

        segments
    }
}

/// What kind of declaration a [`DeclFact`] is.
///
/// A **smaller** vocabulary than [`crate::BindingKind`] on purpose: this is what a summary stores, and a stored
/// value is read by consumers that were written before it. `Other` is not a failure — it is "a declaration is here,
/// and its kind is not one the index distinguishes", which still answers "jump to the definition".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeclKind {
    Type,
    Function,
    Variable,
    Namespace,
    MacroLike,
    Other,
}

/// The **parameter list** of a `#define`, read back from the file it was written in.
///
/// The fact records where the replacement list starts and nothing else, so the parameter list is the
/// parenthesised group that ends where the body begins. That is exact rather than a search: the standard's
/// parameter list holds identifiers, commas, `...` and whitespace, and nothing else — no string literal, no
/// comment, no nesting beyond a parameter's own name. A balanced scan back to the matching `(` is therefore the
/// whole rule, and `None` (no `)` there, or no body at all) is the honest answer for an object-like macro.
fn parameters_before(source: &str, body_start: usize) -> Option<&str> {
    let before = source.get(..body_start)?.trim_end();
    let bytes = before.as_bytes();
    if bytes.last() != Some(&b')') {
        return None;
    }

    let mut depth = 0usize;
    let mut at = bytes.len();
    while at > 0 {
        at -= 1;
        match bytes[at] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&before[at..]);
                }
            }
            _ => {}
        }
    }

    None
}

/// A conditional definition's body, as the closure evidence carries it: the fact's `function_like`, the parameter
/// list when the file it came from had one, and the replacement list.
pub type ConditionalBody = (Box<str>, bool, Option<Box<str>>, Box<str>);

/// **One event read as the `#define` it was, at the place it was written.**
///
/// The pieces are the three the walk copied out of the writing file — the name, the parameter list and the
/// replacement list — and each is lexed where it sits, so the definition's ranges are **positions in that file**
/// rather than offsets in a line this crate wrote. That is what
/// [`crate::macros::MacroDef::written_in`] promises, and what makes "go to definition" on an inherited macro land
/// in the header instead of nowhere.
///
/// The layout is the one the file has, and it is why the offsets are exact rather than searched for: a
/// function-like macro's parameter list follows its name **immediately** (that is the whole difference between
/// `#define A(x) …` and `#define A (x) …`), and the replacement list is the substring `body_range` names. One
/// separator token is inserted between the two, because [`MacroBody`] keeps whitespace and a body's first token
/// must stay separated from the `)` — the same thing `definition_text` spells as a space.
///
/// `None` when the replacement list does not read as a definition, which is the `unreadable` count.
fn definition_written_at(event: &TuEvent, body: &str) -> Option<crate::macros::MacroDef> {
    let mut tokens: Vec<crate::token::Token> = Vec::new();
    let name_at = cpp_parser::SourceRange::new(event.at, event.name.len());
    tokens.push(crate::token::Token::new(
        cpp_parser::CppTokenKind::Identifier,
        &*event.name,
        name_at,
    ));

    if let Some(parameters) = event.parameters.as_deref() {
        // Where the file has it: right after the name. See the note on why that is exact.
        let at = event.at + event.name.len();
        tokens.extend(lex_in_place(parameters, at));
    }

    let body_range = event.body_range?;
    tokens.push(crate::token::Token::new(
        cpp_parser::CppTokenKind::Whitespace,
        " ",
        cpp_parser::SourceRange::new(body_range.start_offset, 0),
    ));
    tokens.extend(lex_in_place(body, body_range.start_offset));

    // The macro's own text in that file: its name and its replacement list. Not the whole directive — the
    // `#define` head is not part of what a fact records — which is what "reveal this macro" wants anyway.
    let range = cpp_parser::SourceRange::new(
        event.at,
        body_range.end_offset().saturating_sub(event.at),
    );

    let mut definition = crate::macros::parse_define(&tokens, range)?;
    definition.written_in = Some(crate::macros::MacroFile::Frame(event.frame));
    Some(definition)
}

/// Lex a piece of a file's text **as if it were at `at`**: the tokens' ranges are shifted to where they are.
///
/// The piece is a substring the walk copied, so the shift is exact rather than a guess — see
/// [`definition_written_at`], which is the only caller.
fn lex_in_place(text: &str, at: usize) -> Vec<crate::token::Token> {
    let (tokens, _) = cpp_parser::lex(text, &cpp_parser::LexerConfig::default());
    tokens
        .iter()
        .map(|token| {
            crate::token::Token::new(
                token.kind,
                &text[token.range.start_offset..token.range.end_offset()],
                cpp_parser::SourceRange::new(
                    at + token.range.start_offset,
                    token.range.length,
                ),
            )
        })
        .collect()
}
/// The same three things while the walk is still collecting them, before the name is prepended.
type ConditionalBodyValue = (bool, Option<Box<str>>, Box<str>);

/// A `#define`, as the index needs it.
///
/// The body is **not** stored — only its shape, which is what the parser's rules ask for (`MacroBody`) and what the
/// analysis layer uses to decide whether an expansion is worth attempting. A caller that needs the tokens re-reads
/// the file: they are in the tree, and copying them into every cache entry would make the cache larger than the
/// project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroFact {
    pub name: String,
    /// Whether this is the name becoming a macro or ceasing to be one.
    pub kind: MacroKind,
    pub function_like: bool,
    pub body: cpp_parser::MacroBody,
    /// Where the macro's **replacement list** is, in the file that defines it — the one piece of a body expansion
    /// needs and the fact did not carry.
    ///
    /// `range` above is the *name*, deliberately ("go to macro definition" jumps to the name, and a rename edits
    /// it); this is everything after the parameters, and it is what lets a consumer slice the body's **text** out of
    /// the defining file rather than re-deriving it by searching the directive — the mistake
    /// `MacroDef::name_range`'s own note warns about ("a search is a second implementation of the same rule, free to
    /// disagree with the one that assigned the name").
    ///
    /// `None` for an `#undef` (there is no body) and for a `#define` with an empty replacement list.
    pub body_range: Option<cpp_parser::SourceRange>,
    /// The macro's value, when its body is **one integer literal**: `#define _GLIBCXX_USE_CXX11_ABI 1`.
    ///
    /// The one piece of a body a condition can read. `#if NAME` on a macro expands it and re-reads the result, and
    /// `condition::macro_value` only accepts a single integer literal — so a body of two tokens, a body
    /// that is a name, and no body at all are the same answer to a condition, and storing any of them would be a
    /// longer way of saying `Unknown`. This is what makes `#if __cplusplus >= 201703L && _GLIBCXX_USE_CXX11_ABI`
    /// decidable once the file that defines the second name has been walked.
    ///
    /// Stored as the literal's **text**, so that reading it back needs no lexer: whoever reads it knows what it is
    /// — the same reason the fact stores a name's range rather than a way to find it.
    pub value: Option<Box<str>>,
    /// Where the fact is, for "go to macro definition".
    pub range: cpp_parser::SourceRange,
    pub guard: FactGuard,
    /// Does this fact settle the name's macro state **whatever branch of its `#if` is taken**?
    ///
    /// False for a fact outside every conditional (there is nothing to settle), and false for the ordinary
    /// conditional fact, whose existence depends on a macro nobody has. True for the two shapes where the
    /// conditional *cannot* change the answer:
    ///
    /// ```text
    /// #ifndef NAME            the condition IS "NAME is not defined yet", and the body defines it:
    /// #define NAME 1          taken → defined here; not taken → it was already defined. Either way: a macro.
    /// #endif
    ///
    /// #if A                   every branch ends in the same kind of fact about the same name, and there
    /// #define NAME 1          is an `#else` — so one of them ran, and whichever it was, the name is a macro.
    /// #else
    /// #define NAME 2
    /// #endif
    /// ```
    ///
    /// `#ifdef NAME` whose body ends in `#undef NAME` is the mirror, and says the name is **not** a macro after
    /// the block. A single-branch `#if A` never settles anything, because the branch may not be taken at all — and
    /// a region nested inside an unsettled one settles nothing either, which is why this is computed for the whole
    /// chain of enclosing conditionals rather than for the innermost alone.
    ///
    /// # What it does *not* say
    ///
    /// **Not which `#define` is in force.** In the second shape the branches differ in what they define the name
    /// *as*, and in the first the name may have been defined by an earlier header that this file cannot see. So
    /// `macro_definition` — the question "where is this macro defined" — ignores this field, and the reference
    /// query, which asks the weaker "is this name a macro *here*", is what reads it. Two questions, two answers.
    ///
    /// # Why this is a fact and not a conclusion
    ///
    /// The invariant every field here is judged by is "would a change to another file make it stale?" — a resolved
    /// type would, a `FileId` would, an instantiation would. This one cannot: it is computed from **this file's own
    /// directives**, and no header can change how a file writes its `#if`s. That is also why it is stored rather
    /// than recomputed per query: the answer needs the branch structure, and a summary keeps only the regions.
    pub settles_the_name: bool,
}

/// Which of the two things a macro name's history is made of.
///
/// `#undef` is stored because a query that answers "where is this macro defined" has to be able to answer "it is
/// not a macro here" instead: a name `#undef`ed above the cursor is an ordinary identifier, and pointing at the
/// `#define` it used to have would be a wrong answer rather than a missing one. Both are *facts about the text* —
/// they say what the file does, not what any compilation concludes — so the same ordering rule settles them
/// together: whichever comes last in translation order wins, and it can be either kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroKind {
    /// `#define NAME ...`
    Definition,
    /// `#undef NAME`
    Undefinition,
}

impl MacroKind {
    /// Is this fact the name becoming a macro?
    pub fn is_definition(self) -> bool {
        matches!(self, MacroKind::Definition)
    }
}

/// An `#include`, resolved or not.
///
/// The *target* is stored as the **path** the resolver found, for the same reason [`FileSummary::path`] is: an
/// id is an index into a run's interner and means nothing once that run is over, while the path is what a
/// consumer opens and what a reverse-include map is built from. A header that did not resolve keeps its
/// spelling, because "this project includes something we cannot find" is a fact a consumer wants to see rather
/// than a gap to hide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeFact {
    pub form: IncludeForm,
    /// The spelling between the delimiters, as written.
    pub spelling: String,
    /// Where the include resolved to, when the resolver found it.
    pub resolved: Option<std::path::PathBuf>,
    /// Whether the directive was `#include_next`.
    ///
    /// Stored, although no *query* has ever wanted it, because the answer has to be **re-derivable**: a summary
    /// that says where an include resolved can only be trusted while re-running the search gives the same answer,
    /// and a search that skipped candidates differently is a different search. See [`IncludeFact::as_include`].
    pub is_next: bool,
    pub range: cpp_parser::SourceRange,
    pub guard: FactGuard,
}

impl IncludeFact {
    /// The directive this fact was made from, as the resolver takes one.
    ///
    /// The round trip back from a fact to a directive is what makes a stored `resolved` **checkable**: `#include`
    /// resolution depends on which paths *exist*, and that is a fact about the filesystem which no key computed
    /// from the text can name — so a cached summary is a candidate that has to be re-verified against the
    /// filesystem before it is used. That check is only faithful if every field the search reads is here, which is
    /// why [`is_next`](Self::is_next) is stored and not dropped.
    pub fn as_include(&self) -> crate::preprocess::directive::Include {
        crate::preprocess::directive::Include {
            form: self.form,
            target: Box::from(self.spelling.as_str()),
            is_next: self.is_next,
        }
    }
}

/// The macros a file's **direct includes** contribute, each in force from the end of its own `#include`.
///
/// This is the first half of the answer the positional half describes: the index
/// already stores, per file, where each macro is defined (`MacroFact`) and where each `#include` resolved
/// (`IncludeFact::resolved`) — this turns those two into the **positional** evidence the parser asks for, stamped
/// with the offset the include ended at.
///
/// Four rules, and each one is a decision rather than a detail:
///
/// * **direct includes only** — a header's own includes are *its* evidence, and following the graph here would need
///   the translation order to be read again, which is what `ProjectIndex` does for conditions;
/// * **the include's offset, not the macro's** — a macro becomes visible where it is brought in, which is what makes
///   the evidence positional at all;
/// * **unconditional facts only** — a `#define` inside an `#if` may not have run, and seeding it would be the flat
///   table's mistake in a new disguise (see the measured cost);
/// * **`#undef` is carried** — a name that stops being a macro is evidence too, and dropping it would leave the
///   earlier definition in force for the rest of the file.
///
/// This variant also carries the **replacement list as text**, when the caller can supply the defining file's
/// source — which is what expansion reads: `namespace __8 {` decides a namespace head, `, noexcept` a
/// parameter-list fragment, `virtual HRESULT STDMETHODCALLTYPE method` a declarator head.
///
/// The text is sliced out of the defining file by the fact's own range, so it stays **the file's text** — including
/// whatever whitespace and comments the body has — and nothing here re-spells a body.
pub fn macros_from_direct_includes_with_bodies<'a>(
    summary: &FileSummary,
    mut look_up: impl FnMut(&std::path::Path) -> Option<(&'a FileSummary, &'a str)>,
) -> Vec<cpp_parser::IncludedMacro> {
    let mut entries = Vec::new();

    for include in &summary.includes {
        let Some((included, source)) = include.resolved.as_deref().and_then(&mut look_up) else {
            continue;
        };
        // Where the name becomes visible: the end of the `#include` directive, which is where a consumer of the
        // tree would put a marker.
        let from_offset = include.range.start_offset + include.range.length;

        for fact in &included.macros {
            if !matches!(fact.guard, FactGuard::Unconditional) {
                continue;
            }

            // The body's **text**, sliced out of the file that defines it by the fact's own range — the piece
            // expansion reads. `None` when the body is empty (`#define FOO`), or when the range does not index
            // the source (a summary from a different revision of that file, which is the caller's to detect).
            let body_text = fact
                .body_range
                .and_then(|range| source.get(range.start_offset..range.start_offset + range.length));

            entries.push(if fact.kind.is_definition() {
                cpp_parser::IncludedMacro::defined_with_body_and_parameters(
                    from_offset,
                    &fact.name,
                    fact.function_like,
                    fact.body,
                    body_text,
                    fact.body_range
                        .and_then(|range| parameters_before(source, range.start_offset)),
                )
            } else {
                cpp_parser::IncludedMacro::undefined_at(from_offset, &fact.name)
            });
        }
    }

    entries
}

/// The macros a file's **whole include closure** contributes, each in force from the `#include` that brought it in.
///
/// [`macros_from_direct_includes_with_bodies`] is the honest minimum, and the measurement says it is not enough:
/// of the 21 remaining failures, **15 have a macro on their first-error line that the closure defines and no direct
/// include does** (`bits/refwrap.h` uses `_GLIBCXX_NOEXCEPT_PARM`, which `bits/c++config.h` defines — two includes
/// away through `bits/move.h`). Evidence that only ever reaches one hop answers for the wrong file.
///
/// So each direct include is walked **through its own closure**, and every macro found is in force from the offset
/// of that direct include — which is where a consumer of the tree would put a marker, and what keeps the answer
/// positional. Three things make this affordable rather than a second index:
///
/// * **one pass per direct include, names deduplicated before any text is sliced** — a closure defines the same
///   name in several files, and last-wins by translation order is the answer the preprocessor gives, so the
///   replacement lists that survive are one per *name*, not one per definition;
/// * **the walk stops at files already seen**, so an include cycle is not a hang;
/// * **nothing is re-spelled**: a body is the defining file's own bytes, sliced by the fact's range.
///
/// # Two channels
///
/// The evidence a file's include closure contributes — **two channels**, and the split is measured, not aesthetic.
///
/// `macros` is what says *whether a name is a macro here*: only unconditional definitions, because a name that is
/// a macro only inside a branch the file may not be in is not a fact about the file.
///
/// `conditional_bodies` is what a rule **reads**: the replacement list of a macro whose definition is conditional
/// but whose branch is in force. Feeding these into `macros` as well is what the first version did, and the census
/// said no — 3 files clean→failing, none the other way (`corecrt.h`, `swprintf.inl`, `types.h`, all three a
/// declaration headed by `__MINGW_EXTENSION`-style macro): several rules read *shape* **because** no table knows
/// the name, and an entry with an unreadable body switches exactly those rules off. Evidence is not monotone. A
/// body, on the other hand, can only ever *enable* a reading: no rule asks "is there a body" to refuse something.
pub struct ClosureEvidence {
    pub macros: Vec<cpp_parser::IncludedMacro>,
    /// `(name, replacement list)` — last definition in translation order wins, like everything else here.
    pub conditional_bodies: Vec<ConditionalBody>,
    /// Conditional facts the walk met, and how many of them the conditions put **in force**. Two numbers rather
    /// than one, because "nothing here was conditional" and "every condition was unanswerable" look the same in
    /// the evidence — and because the second number is what moves when the environment gets better at answering.
    pub conditional_facts: usize,
    pub facts_in_force: usize,
}

/// What the translation unit has defined **so far** — the state a condition written later is answered against.
///
/// This is the "边走边喂" half of: a condition is a question about what the unit had seen
/// when the preprocessor reached it, and the answer changes as the walk moves on. `#ifdef STDMETHOD` is false
/// before `objbase.h` and true after it, in the *same* translation unit.
///
/// Only **definedness** is carried, not values. A file's body is text whose meaning is the parser's business, so a
/// name the walk has seen defined answers `#ifdef`/`defined()` and leaves a *value* question (`#if FOO == 2`) to
/// the compilation's own table — which is a lost reading rather than a wrong branch, the direction this layer must
/// always fail in.
struct UnitState {
    /// What the unit has in force, as **definitions** rather than names — each carrying **where** it was
    /// written, because a definition is only in force from its own line (see [`UnitMacros::lookup`]).
    definitions: std::collections::HashMap<Box<str>, Binding>,
}

/// One definition the unit has in force: what it says, and where it was said.
struct Binding {
    definition: std::sync::Arc<MacroDef>,
    /// The file that wrote it — a position means nothing without it, and only the file being walked can have
    /// written something *below* the condition being answered.
    defined_in: Box<std::path::Path>,
    defined_at: usize,
}

impl UnitState {
    fn define(
        &mut self,
        name: &str,
        definition: std::sync::Arc<MacroDef>,
        file: &std::path::Path,
        at: usize,
    ) {
        self.definitions.insert(
            name.into(),
            Binding {
                definition,
                defined_in: Box::from(file),
                defined_at: at,
            },
        );
    }

    fn undefine(&mut self, name: &str) {
        self.definitions.remove(name);
    }
}

/// The `MacroDef`s a walk feeds into a translation unit's state, parsed **once per *(file, name)***.
///
/// Parsing is the cost of feeding definitions: a closure holds tens of thousands of `#define`s and every one of
/// them is read back out of the defining file's text. A definition does not depend on *which* file is being
/// seeded, so the cache is the difference between one parse per definition and one per definition per file —
/// 455 files in this corpus, which is the difference between seconds and minutes.
#[derive(Default)]
pub struct MacroDefinitions {
    /// Keyed by **where the definition is written** rather than by its name, and that is not a detail: a file may
    /// define the same name twice (`#define WIDTH 80` above an `#undef` and a second `#define WIDTH 120`), and a
    /// cache keyed by the name would hand the second fact the first one's definition — a wrong expansion rather than
    /// a missing one. A fact is identified by the range its name is written at, which is unique in its file.
    parsed: std::collections::HashMap<(std::path::PathBuf, usize), Option<std::sync::Arc<MacroDef>>>,
}

impl MacroDefinitions {
    /// The definition behind one fact, read from the file that wrote it.
    ///
    /// The `#define` is **put back together** from what the fact carries — its name, and the rest of its logical
    /// line, which is the parameter list and the body — and read by the ordinary `#define` reader, so a definition
    /// fed into a condition cannot mean something different from the same line read in the file. The offsets in the
    /// result are relative to that reconstructed line, which is all the evaluator does with them.
    fn definition_of(
        &mut self,
        path: &std::path::Path,
        source: &str,
        fact: &MacroFact,
    ) -> Option<std::sync::Arc<MacroDef>> {
        let key = (path.to_path_buf(), fact.range.start_offset);

        if let Some(known) = self.parsed.get(&key) {
            return known.clone();
        }

        let definition = read_back_a_define(source, fact).map(std::sync::Arc::new);
        self.parsed.insert(key, definition.clone());

        definition
    }

    /// Forget everything read out of a file whose text changed.
    ///
    /// Necessary rather than tidy: the keys are **offsets in that file**, so after an edit an old entry can be
    /// reached by a *different* fact that happens to start where the old one did — the cache would then hand back a
    /// definition nobody wrote. The whole file's entries go, not only the edited definition's: every offset after
    /// the edit moved.
    pub fn forget(&mut self, path: &std::path::Path) {
        self.parsed.retain(|(held, _), _| held != path);
    }

    /// How many distinct definitions have been read back — the number a caller watches to see the cache work.
    pub fn len(&self) -> usize {
        self.parsed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parsed.is_empty()
    }

    fn get_or_read(
        &mut self,
        path: &std::path::Path,
        source: &str,
        fact: &MacroFact,
    ) -> Option<std::sync::Arc<MacroDef>> {
        self.definition_of(path, source, fact)
    }
}

/// `#define NAME(params) body`, read back out of the file's own text.
///
/// A `\`-newline is **removed**, not copied: translation phase 2 does that before anything reads the line, and a
/// splice left in the middle of a body makes the body unreadable — the evaluator meets a line-continuation token
/// between two operands and answers `Unknown`. Measured, and it is what kept `STDMETHOD` out of `commdlg.h` for
/// three batches: `WINAPI_FAMILY_DESKTOP_APP` is written across two lines in `winapifamily.h`, so
/// `#if WINAPI_FAMILY_PARTITION (WINAPI_PARTITION_APP)` — the guard around `combaseapi.h`'s C++ `#define
/// STDMETHOD` — could never be answered.
fn read_back_a_define(source: &str, fact: &MacroFact) -> Option<MacroDef> {
    let raw = source.get(fact.range.start_offset..)?;

    // The fact's range may start at the **name** or at the directive — both spellings exist in the index — so the
    // line is taken from wherever `#define` ends, and the keyword is put back exactly once.
    let rest = match raw.strip_prefix('#') {
        Some(after_the_hash) => {
            let after_the_hash = after_the_hash.trim_start();
            match after_the_hash.strip_prefix("define") {
                Some(after_the_keyword) => after_the_keyword.trim_start(),
                None => raw,
            }
        }
        None => raw,
    };

    let bytes = rest.as_bytes();

    let mut logical = String::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if bytes.get(index + 1) == Some(&b'\n') => index += 2,
            b'\\' if bytes.get(index + 1) == Some(&b'\r') && bytes.get(index + 2) == Some(&b'\n') => {
                index += 3;
            }
            b'\n' => break,
            _ => {
                let character = rest[index..].chars().next()?;
                logical.push(character);
                index += character.len_utf8();
            }
        }
    }

    let line = format!("#define {logical}");
    let mut errors = Vec::new();
    let mut lexer =
        cpp_parser::CppLexer::new(&line, cpp_parser::LexerConfig::default(), &mut errors);
    let tokens: Vec<crate::token::Token> = lexer
        .tokenize()
        .into_iter()
        .map(|token| {
            let text = &line[token.range.start_offset..token.range.end_offset()];
            crate::token::Token::new(token.kind, text, token.range)
        })
        .collect();

    // `parse_define` reads from the **name**: the directive's own `#` and `define` are the directive scanner's
    // business and not the definition reader's, so the two leading significant tokens are dropped here.
    let mut significant = 0usize;
    let mut from = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        if crate::token::is_trivia(token.kind) {
            continue;
        }

        significant += 1;
        from = index + 1;
        if significant == 2 {
            break;
        }
    }

    crate::macros::parse_define(tokens.get(from..)?, fact.range)
}

/// The table a condition inside the unit is answered against: what the walk has seen, over what the compilation
/// started with.
#[derive(Clone, Copy)]
struct UnitMacros<'a> {
    seed: &'a Marked,
    state: &'a UnitState,
    /// The file the condition being answered was written in, and the offset it was written at. `None` asks the
    /// question **without a position** — "what does the unit say now, at the end of what has been read" — which is
    /// what the expansion *inside* a body wants.
    here: Option<(&'a std::path::Path, usize)>,
}

impl crate::condition::MacroValues for UnitMacros<'_> {
    fn lookup(&self, name: &str) -> crate::condition::Lookup<'_> {
        if let Some(binding) = self.state.definitions.get(name) {
            // **A definition written below this condition has not happened yet.** An include guard is
            // `#ifndef X / #define X`, so a table read at the end of the file answers "`X` is defined" — and that
            // makes the guard's own region inactive and every fact inside it invisible. Measured: it is what kept
            // `combaseapi.h`'s `STDMETHOD` out of `commdlg.h` after the condition itself had been made answerable
            //.
            let written_below = self
                .here
                .is_some_and(|(file, at)| &*binding.defined_in == file && binding.defined_at >= at);

            if !written_below {
                return crate::condition::Lookup::Defined(&binding.definition);
            }
        }

        self.seed.lookup(name)
    }
}

/// The macros a file's own include closure contributes.
///
/// `definitions` is the cache of parsed `#define`s the walk feeds into a unit's state; one cache serves every file
/// seeded from the same corpus, which is what keeps the feeding affordable (see [`MacroDefinitions`]).
pub fn macros_from_the_closure_with_bodies<'a>(
    summary: &'a FileSummary,
    look_up: impl FnMut(&std::path::Path) -> Option<(&'a FileSummary, &'a str)>,
    seed: &Marked,
    definitions: &mut MacroDefinitions,
) -> ClosureEvidence {
    walk_the_translation_unit(summary, None, None, look_up, seed, definitions)
}

/// The macros in force in a translation unit **before the point where it includes this file** — what a *header*
/// needs, and the one thing its own closure structurally cannot give it.
///
/// `commdlg.h:577` writes `STDMETHOD(QueryInterface) (…) PURE;` and never sees a `#define` of `STDMETHOD`: its own
/// includes are `winapifamily.h`, `_mingw_unicode.h`, `prsht.h`, `pshpack1.h`, `poppack.h`, and the definition
/// arrives because `windows.h:108` includes `commdlg.h` **after** `objbase.h` — the *includer's order* is the
/// translation unit, and a per-file walk stops at the file's own edge. Measured: the seed for that line is empty,
/// which is why the reading rules could not fix the file.
///
/// The entries are seeded at offset **0** and not at the includer's offsets: the offsets of a *different* file mean
/// nothing here, and every macro the unit had in force before the inclusion is in force from this file's first
/// token on. What this file does to those names afterwards is its own facts' business, which the parser reads
/// itself.
pub fn macros_in_force_before_the_include<'a>(
    includer: &'a FileSummary,
    before: usize,
    look_up: impl FnMut(&std::path::Path) -> Option<(&'a FileSummary, &'a str)>,
    seed: &Marked,
    definitions: &mut MacroDefinitions,
) -> ClosureEvidence {
    walk_the_translation_unit(includer, Some(before), Some(0), look_up, seed, definitions)
}

/// Walk a file's includes — or, with `only_before`, only the ones written **before** that offset — and collect what
/// the closure they reach defines.
///
/// # Why the walk carries a state
///
/// A condition is a question about what the translation unit had **seen** when the preprocessor reached it:
/// `#ifdef STDMETHOD` in `commdlg.h` is false until `objbase.h` has been read, and
/// `#if WINAPI_FAMILY_PARTITION (WINAPI_PARTITION_APP)` in `combaseapi.h` is a question about a macro
/// `winapifamily.h` defines. Answering every condition against one fixed table — which is what this walk did
/// before — reports `Unknown`, and an `Unknown` branch is a `#define` that never becomes evidence. That is why
/// nothing from `combaseapi.h` reached `commdlg.h`.
///
/// So the walk visits the closure in **include order** (depth first: the first `#include` of a file is read
/// before its second, and before the file's own next sibling) and feeds every definition it puts in force into
/// [`UnitState`]. Only **definedness** is carried, never values: a file's body is text whose meaning the parser
/// owns, so a name the walk has seen defined answers `#ifdef`/`defined()` and leaves a *value* question to the
/// compilation's own table — a lost reading rather than a wrong branch.
///
/// `all_from` moves every entry onto one offset, which is what reading a file *inside* another one needs; without it
/// each entry lands where its own `#include` ended.
fn walk_the_translation_unit<'a>(
    summary: &'a FileSummary,
    only_before: Option<usize>,
    all_from: Option<usize>,
    mut look_up: impl FnMut(&std::path::Path) -> Option<(&'a FileSummary, &'a str)>,
    seed: &Marked,
    definitions_of: &mut MacroDefinitions,
) -> ClosureEvidence {
    let mut walked = Walked::default();
    let mut unit = UnitState {
        definitions: std::collections::HashMap::new(),
    };

    for include in &summary.includes {
        // The includes are facts in source order, so the first one at or past the limit ends the walk.
        if only_before.is_some_and(|limit| include.range.start_offset >= limit) {
            break;
        }

        let Some(root) = include.resolved.as_deref() else {
            continue;
        };
        let from_offset = all_from.unwrap_or(include.range.start_offset + include.range.length);

        let mut seen: std::collections::HashSet<&std::path::Path> = std::collections::HashSet::new();
        walk_one_file(
            root,
            from_offset,
            None,
            0,
            &mut seen,
            &mut look_up,
            seed,
            definitions_of,
            &mut unit,
            &mut walked,
        );
    }

    walked.into_evidence()
}

/// **A translation unit walked once, kept as a timeline any file inside it can be read from.**
///
/// # The problem this exists for
///
/// The per-file reading — "walk *this* file's include closure and hand it the result" — costs one full walk of the
/// closure **per file**, and the census says what that is: 255 files of the Windows SDK corpus, 2 489 142 macro
/// entries materialised, **17 618 794 conditional facts evaluated**, 147 s of a 174 s run. Every header's `#if`s
/// are answered ~255 times, because each file's environment is built as if it were the only one.
///
/// # What a preprocessor does instead
///
/// It walks the unit **once** and keeps, per identifier, the history of its macro directives — each carrying the
/// **location** (file + offset) it was written at (clang: `IdentifierInfo`'s directive chain, whose entries hold a
/// `SourceLocation`; a query is "walk the chain and stop at this location"). Every later question is a *position*
/// in that one walk: a header's macro state is the state at the point it was included, not a separate walk.
///
/// This type is that timeline. `events` is the whole unit's macro history in walk order; `frames` records where
/// each file was entered, so a file's view is its **inherited prefix** (`entry_seq`), its **own facts**, and the
/// facts of the files it includes (visible from the offset the `#include` ended at). No walk is repeated, no body
/// is copied, and a condition is evaluated once for the whole corpus instead of once per file.
///
/// # Frames, and why the subtree is an interval
///
/// Frames are created in **DFS preorder** — a file's dependencies are walked before the next sibling — so the
/// subtree of frame `f` is exactly the frame indices `f..tout[f]`, and "is this event visible from that file" is
/// an integer comparison rather than a walk up the parent chain.
pub struct TranslationUnit {
    pub(crate) events: Vec<TuEvent>,
    pub(crate) frames: Vec<TuFrame>,
    /// **The name index**: one entry per macro name the walk saw, holding its events' indices in walk order.
    ///
    /// This is what makes a file's environment a *view* rather than a map. Without it, answering "what is `_STD`
    /// here" means looking at every event in the unit; with it, the question is a hash and a walk of that name's
    /// own history — and a name's history is short, because a header defines a name once.
    ///
    /// The key is the **shared** `Arc<str>` the event already holds, so indexing the unit allocates no strings;
    /// [`std::borrow::Borrow`] is what lets a query with a `&str` find it without building a key.
    ///
    /// Derived state, like [`TranslationUnit::paths`]: it is a function of `events`, so it is built by the
    /// constructors rather than stored in the cache format.
    pub(crate) by_name: std::collections::HashMap<std::sync::Arc<str>, Vec<u32>>,
    /// **The frame paths**: for each frame, the frames it is nested in, each with the offset in that ancestor at
    /// which this frame's text becomes live — `(outermost include's ancestor, offset)`, nearest ancestor first.
    ///
    /// One entry per (frame, ancestor) pair, so the whole table is a few thousand `(u32, usize)` for a corpus and
    /// it answers [`TranslationUnit::offset_in`] without walking the parent chain — a query the parser makes for
    /// every name it asks about, in every file.
    ///
    /// Derived state: the parents and the `from_in_parent` offsets already say all of it.
    pub(crate) paths: Vec<Vec<(u32, usize)>>,
    /// The frame a file was entered as, the **first** time the walk reached it. A file included twice is walked
    /// once (see `walk_one_file`), so a second entry has no frame of its own.
    entered: std::collections::HashMap<std::path::PathBuf, u32>,
    /// The same two counters [`ClosureEvidence`] reports, for the same reason: they say whether the corpus was
    /// conditional at all and whether the environment could answer.
    pub conditional_facts: usize,
    pub facts_in_force: usize,
}

/// One macro fact of the unit, with **where** it was written and **which frame** it belongs to.
///
/// `pub(crate)` for the codec, which writes these records to the cache: the type is the unit's own shape, and the
/// alternative — a second struct in the codec that mirrors it — is a second place for a field to be forgotten.
pub(crate) struct TuEvent {
    pub(crate) name: std::sync::Arc<str>,
    /// `None` for an `#undef`.
    pub(crate) function_like: Option<bool>,
    /// The shape of the replacement list, as the parser reads it.
    pub(crate) body: Option<cpp_parser::MacroBody>,
    /// The replacement list as text — **shared**, see [`cpp_parser::IncludedMacro::body_text`].
    pub(crate) body_text: Option<std::sync::Arc<str>>,
    pub(crate) parameters: Option<std::sync::Arc<str>>,
    /// Where the **replacement list** is, in the file that wrote it.
    ///
    /// Kept so that a definition this unit hands to the cooker can carry **real positions**: the body's text is
    /// already here, and with its range the tokens lexed from it land where they were written instead of inside a
    /// line this crate reconstructed ([`crate::macros::MacroDef::written_in`]). `None` for an `#undef` and for a
    /// `#define` whose replacement list is empty — there is nothing to place.
    pub(crate) body_range: Option<cpp_parser::SourceRange>,
    pub(crate) frame: u32,
    /// The offset **inside the file that wrote it**.
    pub(crate) at: usize,
    /// Was the fact unconditional? The two channels of [`ClosureEvidence`], which is a measured distinction and
    /// not this type's to collapse.
    pub(crate) unconditional: bool,
}

/// Where one file was entered, and how much of the timeline was already behind it.
pub(crate) struct TuFrame {
    pub(crate) file: std::path::PathBuf,
    pub(crate) parent: Option<u32>,
    /// Where this file's text comes into force **in its parent**: the end of the `#include` that brought it in.
    pub(crate) from_in_parent: usize,
    /// How many events the walk had emitted when this frame was entered — everything before it is in force from
    /// offset 0 of this file, which is what a header's own first line sees.
    pub(crate) entry_seq: u32,
    /// The end of this frame's subtree, as a frame index (see the type's note on preorder).
    pub(crate) tout: u32,
}

/// The timeline under construction — the sink [`Walked`] records into while the one walk runs.
#[derive(Default)]
struct TimelineBuilder {
    events: Vec<TuEvent>,
    frames: Vec<TuFrame>,
    entered: std::collections::HashMap<std::path::PathBuf, u32>,
}

impl TimelineBuilder {
    /// Open a frame for a file the walk is about to read, and return its id.
    fn enter(&mut self, file: &std::path::Path, parent: Option<u32>, from_in_parent: usize) -> u32 {
        let id = self.frames.len() as u32;
        self.frames.push(TuFrame {
            file: file.to_path_buf(),
            parent,
            from_in_parent,
            entry_seq: self.events.len() as u32,
            // Closed by `leave`; a frame that is never left is the last one, and its subtree runs to the end.
            tout: u32::MAX,
        });
        self.entered.entry(file.to_path_buf()).or_insert(id);
        id
    }

    fn leave(&mut self, frame: u32) {
        self.frames[frame as usize].tout = self.frames.len() as u32;
    }

    /// Record one fact the walk put in force.
    fn record(&mut self, frame: u32, fact: &MacroFact, source: &str, unconditional: bool) {
        let body_text = fact
            .body_range
            .and_then(|range| source.get(range.start_offset..range.start_offset + range.length))
            .map(std::sync::Arc::from);
        let parameters = fact
            .body_range
            .and_then(|range| parameters_before(source, range.start_offset))
            .map(std::sync::Arc::from);

        self.events.push(TuEvent {
            name: std::sync::Arc::from(&*fact.name),
            function_like: fact.kind.is_definition().then_some(fact.function_like),
            body: fact.kind.is_definition().then_some(fact.body),
            body_text,
            parameters,
            body_range: fact.body_range,
            frame,
            at: fact.range.start_offset,
            unconditional,
        });
    }
}

impl TranslationUnit {
    /// Walk a translation unit **once**, from the file that starts it.
    ///
    /// `look_up` answers for a path with the file's summary and its text, the same contract the per-file walks
    /// take; a file it cannot answer for is outside the analysis and defines nothing (see `walk_one_file`).
    pub fn walk<'a>(
        root: &'a FileSummary,
        mut look_up: impl FnMut(&std::path::Path) -> Option<(&'a FileSummary, &'a str)>,
        seed: &Marked,
        definitions_of: &mut MacroDefinitions,
    ) -> Self {
        let mut walked = Walked {
            timeline: Some(TimelineBuilder::default()),
            ..Walked::default()
        };
        let mut unit = UnitState {
            definitions: std::collections::HashMap::new(),
        };
        let mut seen: std::collections::HashSet<&std::path::Path> = std::collections::HashSet::new();

        // The root is walked by the same function every included file is: it *is* a file of the unit, and a
        // second entry point for it would be a second place for the include order to be got wrong.
        walk_one_file(
            &root.path,
            0,
            None,
            0,
            &mut seen,
            &mut look_up,
            seed,
            definitions_of,
            &mut unit,
            &mut walked,
        );

        let timeline = walked.timeline.take().expect("just built");
        TranslationUnit::from_parts(
            timeline.events,
            timeline.frames,
            walked.conditional_facts,
            walked.facts_in_force,
        )
    }

    /// Rebuild a timeline from its parts — the decoder's constructor, and the only caller that may hand this type
    /// a `frames` vector it did not build itself.
    ///
    /// Three fields are recomputed here rather than stored, because all three *are* statements about the parts:
    /// `entered` is "the frame each file was first entered as" (the frames say it), the name index is a function of
    /// the events, and the frame paths are a function of the parents and their offsets. The cache format stores the
    /// parts and nothing else — see [`crate::summary_codec`], whose whole contract is that a field is written only
    /// when it cannot be derived, since a derived field that was *written* could disagree with what it derives
    /// from.
    pub(crate) fn from_parts(
        events: Vec<TuEvent>,
        frames: Vec<TuFrame>,
        conditional_facts: usize,
        facts_in_force: usize,
    ) -> Self {
        let mut entered: std::collections::HashMap<std::path::PathBuf, u32> = std::collections::HashMap::new();
        for (index, frame) in frames.iter().enumerate() {
            entered.entry(frame.file.clone()).or_insert(index as u32);
        }

        let mut by_name: std::collections::HashMap<std::sync::Arc<str>, Vec<u32>> =
            std::collections::HashMap::new();
        for (index, event) in events.iter().enumerate() {
            // Walk order, which is the order the queries rely on: the last entry of a name's list is the last fact
            // the walk put in force for it.
            by_name
                .entry(std::sync::Arc::clone(&event.name))
                .or_default()
                .push(index as u32);
        }

        let mut paths: Vec<Vec<(u32, usize)>> = Vec::with_capacity(frames.len());
        for frame in &frames {
            // Nearest ancestor first, and each one's offset is the `from_in_parent` of the **child** on the path —
            // that is the frame whose text is this frame's, so a fact written here is in force in that ancestor
            // from exactly that offset. Read off the ancestor already built, so the whole table is one pass.
            let mut path = Vec::new();
            let mut offset = frame.from_in_parent;
            let mut parent = frame.parent;
            while let Some(ancestor) = parent {
                path.push((ancestor, offset));
                let reached = &frames[ancestor as usize];
                offset = reached.from_in_parent;
                parent = reached.parent;
            }
            paths.push(path);
        }

        TranslationUnit {
            events,
            frames,
            by_name,
            paths,
            entered,
            conditional_facts,
            facts_in_force,
        }
    }

    /// How many macro facts the unit's walk put in force — the size of the timeline.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// **Read every definition this unit carries, once** — with the positions it was written at.
    ///
    /// This is what replaced the per-file configuration build. Cooking a file used to walk its whole environment
    /// — every name the unit knows — parse each definition and copy the result into a table of that file's own,
    /// which the census measured at **4.1 s** for 255 files of the SDK corpus, on top of the map the environment
    /// itself built (1.7 s). The definitions are the *same* definitions every time: a fact's identity is its name,
    /// its parameters and its body, and none of those depend on which file is asking. Only the **offset** does,
    /// and the offset is resolved per query by [`MacroView`].
    ///
    /// So a run reads the unit once and every file's cook is a lookup. Nothing here is per file, and the counters
    /// below are therefore counted **once for the unit** rather than once per file: they say how much of the
    /// unit's vocabulary expansion cannot use, where the per-file version counted each unusable fact once per file
    /// that could see it (`without_a_body` × 200 is not a measurement of the corpus).
    ///
    /// # The positions are the file's, not a reconstruction's
    ///
    /// A definition is read out of the fact's **own** parameter list and replacement list — the two substrings the
    /// walk copied out of the file that wrote them — and lexed with their offsets shifted to where they sit:
    /// `TuEvent::body_range` for the body, and the name plus its length for the parameter list. So a body
    /// token's range is where the token is, `written_in` says which file that is
    /// ([`crate::macros::MacroFile::Frame`]), and a consumer can act on the answer instead of on an offset in a
    /// line this crate wrote. Definitions that arrive without a position ([`crate::macros::MacroFile`]'s third
    /// case) still exist — the closure path's `MacroEnvironment` carries text only — and they say so.
    pub fn definitions(&self) -> UnitDefinitions {
        let mut by_event: Vec<Option<std::sync::Arc<crate::macros::MacroDef>>> =
            vec![None; self.events.len()];
        let mut counted = UnitDefinitions::default();

        for (index, event) in self.events.iter().enumerate() {
            let Some(body) = event.body_text.as_ref() else {
                // A fact with no replacement list is not a definition this reader can paste — but only the
                // **definition** channel counts it: the in-force channel is defined by having a body.
                if event.unconditional && event.function_like.is_some() {
                    counted.without_a_body += 1;
                }
                continue;
            };

            if !event.unconditional {
                // The in-force channel: a body a condition settled, usable only when the caller classified the
                // macro as object-like or carried its parameter list — see `Configuration`'s note.
                match event.function_like {
                    Some(false) => {}
                    Some(true) if event.parameters.is_some() => {}
                    _ => {
                        counted.in_force_without_a_parameter_list += 1;
                        continue;
                    }
                }
            } else {
                // The definition channel: a definition whose parameter list nobody carried cannot be pasted, and
                // one whose shape the walk could not read is not a definition at all.
                match (event.function_like, event.body) {
                    (Some(true), _) if event.parameters.is_none() => {
                        counted.function_like_without_parameters += 1;
                        continue;
                    }
                    (Some(_), Some(_)) => {}
                    _ => continue,
                }
            }

            match definition_written_at(event, body) {
                Some(definition) => by_event[index] = Some(std::sync::Arc::new(definition)),
                None => counted.unreadable += 1,
            }
        }

        UnitDefinitions { by_event, ..counted }
    }

    /// The path of a frame — how a [`crate::macros::MacroFile::Frame`] becomes something a consumer can open.
    pub fn frame_file(&self, frame: u32) -> Option<&std::path::Path> {
        self.frames.get(frame as usize).map(|frame| frame.file.as_path())
    }

    /// How many of those facts carry a **replacement list**, and how many are in the **in-force** channel.
    ///
    /// The two numbers the census prints beside the entry count, because "the evidence arrived" and "the evidence
    /// arrived with bodies" are different states of the world and the second is what expansion needs. They are
    /// answered here rather than left to a caller to re-derive: a caller that counted them itself would be a
    /// second implementation of what an event is.
    pub fn bodies(&self) -> (usize, usize) {
        let with_a_body = self
            .events
            .iter()
            .filter(|event| {
                event
                    .body_text
                    .as_deref()
                    .is_some_and(|text| !text.trim().is_empty())
            })
            .count();
        let in_force = self.events.iter().filter(|event| !event.unconditional).count();
        (with_a_body, in_force)
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// The files the walk entered, in the order it entered them.
    pub fn files(&self) -> impl Iterator<Item = &std::path::Path> {
        self.frames.iter().map(|frame| frame.file.as_path())
    }

    /// Every macro fact the unit put in force **in one file**, in walk order, as `(name, offset in that file)`.
    ///
    /// The timeline itself rather than a view of it, and the difference is the whole contract of
    /// [`TranslationUnit::environment_of`]: this is what the file **wrote**, that is what the file **sees**. A
    /// caller asking "what does this header define" wants this one; a caller asking "is this name a macro here"
    /// wants the environment. Both are real questions about one record, which is why the record keeps the file and
    /// the offset of every fact rather than only the ones a view happens to use.
    pub fn facts_of<'unit>(
        &'unit self,
        path: &'unit std::path::Path,
    ) -> impl Iterator<Item = (&'unit str, usize)> {
        let frame = self
            .frames
            .iter()
            .position(|frame| frame.file == path)
            .map(|index| index as u32);

        self.events
            .iter()
            .filter(move |event| Some(event.frame) == frame)
            .map(|event| (&*event.name, event.at))
    }

    /// The environment of a file **as this unit sees it**, or `None` when the unit never reaches it.
    ///
    /// `None` is the honest answer for a file nothing includes: nothing in this translation unit says what it
    /// sees, and inventing a context for it is what the earlier per-file census did with a heuristic.
    pub fn environment_of(&self, path: &std::path::Path) -> Option<MacroView<'_>> {
        let &frame = self.entered.get(path)?;
        Some(self.environment_at(frame))
    }

    /// The environment of the file the unit starts with.
    pub fn root_environment(&self) -> MacroView<'_> {
        self.environment_at(0)
    }

    /// One file's environment as **a position in this timeline** — no walk, no copy of the bodies, and no map.
    ///
    /// The three kinds of event a file's environment is made of, and the offset each is in force from:
    ///
    /// * the facts of the files it **includes**, from the offset the `#include` ended at — a definition three
    ///   includes down is in force in this file from the **outermost** `#include` on the path, which is
    ///   [`TranslationUnit::offset_in`]'s question;
    /// * everything the walk had already emitted when this file was entered (the **inherited prefix**), from
    ///   offset 0 — the state a header's first line sees, which is what the old one-hop includer lookup was
    ///   approximating and what the real translation unit knows exactly;
    /// * and **not** the file's own facts.
    ///
    /// The exclusion is the caller's contract and not an optimisation: a file's own `#define`s are what *it* says,
    /// and the parser reads them itself out of the text it is parsing (that is what `MacroNames` is). Handing them
    /// back as "the includes contributed this" would double-count them, and — worse — would say a name is a macro
    /// from its own `#define` line in a branch the reader cannot evaluate. What the environment is *for* is what
    /// the file cannot see by looking at itself.
    ///
    /// # Why this is a view and not a map
    ///
    /// It used to build a `MacroEnvironment`: one entry per name in force, each with the offset it applies from.
    /// That is the whole environment materialised per file, and the census measured what it costs — **1.7 s of a
    /// 13.8 s run** for 255 files of the Windows SDK corpus, in map builds alone, with every file paying for the
    /// names it never asks about. A view answers the *same* questions out of the unit's name index, lazily: a query
    /// is a hash of the name and a walk of that name's own (short) history, which is what the parser does per token
    /// and all it ever needed.
    ///
    /// The answers are not merely equivalent, they are the same rule: for one name, everything visible to a file
    /// has an offset that **does not decrease** in walk order — the inherited prefix is all at offset 0, and the
    /// includes are walked in text order — so "the last one in walk order" *is* "the last one in force", and the
    /// final binding is the only one that can be in force at any offset. That is the same collapse the map did
    /// (last wins per name), read at the offset it is asked about instead of frozen at build time.
    fn environment_at(&self, frame: u32) -> MacroView<'_> {
        MacroView { unit: self, frame }
    }

    /// The offset in `consumer` at which `frame`'s text becomes live: the `#include` end of the **outermost**
    /// frame on the path from `consumer` to `frame`.
    ///
    /// Read out of the precomputed frame paths ([`TranslationUnit::paths`]) rather than walked per query: this is
    /// on the parser's per-token path (the offset of every inherited fact is this answer), and a chain walk per
    /// query would trade a map build for a pointer chase.
    fn offset_in(&self, frame: u32, consumer: u32) -> usize {
        self.paths[frame as usize]
            .iter()
            .find(|(ancestor, _)| *ancestor == consumer)
            .map(|(_, offset)| *offset)
            // A frame outside the consumer's subtree never gets here — the caller's interval test is what keeps it
            // out — so this is the root, and offset 0 is the only answer that means anything.
            .unwrap_or(0)
    }

    /// **Cook the whole unit into one stream** — every file the walk reached, in the order a compiler reads them.
    ///
    /// This is the other half of the readings: cooking one file at a time answers "does this header read on its
    /// own" ([`TranslationUnit::environment_of`] is the state, `cook_with` is the cook), and this answers "does
    /// the program read". The order is the walk's own (frames are in DFS preorder, which is include order) and the
    /// splice points are `from_in_parent` — the offset in the parent where the `#include` that brought the child
    /// in ended.
    ///
    /// `definitions` is the unit's once-read definition table ([`TranslationUnit::definitions`]) and `seed` is
    /// what the compilation predefines; `sources` is where the *text* comes from, which is the caller's business
    /// (a disk provider, an editor's buffers, a test's fixtures). A file the caller has no text for is a hole:
    /// counted in [`RenderedUnit::missing`] and its includes are still stitched, because a compilation that is
    /// missing one header still reads the ones below it.
    pub fn cook_the_unit(
        &self,
        sources: &dyn crate::preprocess::macros::UnitSources,
        definitions: &UnitDefinitions,
        seed: Option<&crate::macros::MacroTable>,
        use_in_force_bodies: bool,
    ) -> RenderedUnit {
        let mut out = RenderedUnit {
            files: self.frames.iter().map(|frame| frame.file.clone()).collect(),
            ..RenderedUnit::default()
        };
        let cook = UnitCook {
            unit: self,
            sources,
            definitions,
            seed,
            use_in_force_bodies,
            // Which frames each frame includes, in the order it includes them. Frames are created in preorder, so
            // a parent's children are already in include order and pushing in index order keeps it.
            children: {
                let mut children: Vec<Vec<u32>> = vec![Vec::new(); self.frames.len()];
                for (index, frame) in self.frames.iter().enumerate() {
                    if let Some(parent) = frame.parent {
                        children[parent as usize].push(index as u32);
                    }
                }
                children
            },
        };

        if !self.frames.is_empty() {
            cook.stitch(0, &mut out);
        }
        out
    }
}

/// One unit's cook, in progress: the invariants [`TranslationUnit::cook_the_unit`] was handed, and the include
/// tree the stitch walks.
///
/// A struct rather than six arguments because the stitch is recursive and every frame needs all of them — the
/// alternative is the same list at every call, which is how a recursion ends up disagreeing with itself.
struct UnitCook<'unit> {
    unit: &'unit TranslationUnit,
    sources: &'unit dyn crate::preprocess::macros::UnitSources,
    definitions: &'unit UnitDefinitions,
    seed: Option<&'unit crate::macros::MacroTable>,
    use_in_force_bodies: bool,
    /// For each frame, the frames it includes, in include order.
    children: Vec<Vec<u32>>,
}

impl UnitCook<'_> {
    /// Cook one frame and splice each include's stream in where the `#include` ended.
    fn stitch(&self, frame: u32, out: &mut RenderedUnit) {
        let included = &self.children[frame as usize];

        let Some(text) = self.sources.source_of(&self.unit.frames[frame as usize].file) else {
            // A file nobody has the text of: the includes it names are still stitched (the walk reached them
            // through it), and the hole is counted rather than cooked as an empty file.
            out.missing += 1;
            self.stitch_included(included, 0, out);
            return;
        };

        let (tokens, _) = cpp_parser::lex(text, &cpp_parser::LexerConfig::default());
        let cooked = crate::preprocess::cooked::cook_with(
            text,
            &tokens,
            &crate::preprocess::cooked::FileMacros::new(
                self.unit.environment_at(frame),
                self.definitions,
                self.seed,
                self.use_in_force_bodies,
            ),
        );

        let mut next = 0usize;
        for token in &cooked.tokens {
            // **Where the token stands in this file**, which is also what decides whether an include that has not
            // been spliced yet comes first: the call site for an expansion (the text the reader sees) and the
            // token's own range for text the file wrote. Both are monotone in stream order, and both are positions
            // in *this* file — see `ExpandedToken::diagnostic_range`.
            let at = token.diagnostic_range();
            while next < included.len()
                && self.unit.frames[included[next] as usize].from_in_parent <= at.start_offset
            {
                self.stitch(included[next], out);
                next += 1;
            }
            out.push(token.text(), frame, at);
        }

        self.stitch_included(included, next, out);
    }

    /// Splice the includes from `from` on: the tail of a file's stream, and everything a file with no text has.
    fn stitch_included(&self, included: &[u32], from: usize, out: &mut RenderedUnit) {
        for &child in &included[from..] {
            self.stitch(child, out);
        }
    }
}

/// **A unit's definitions, read once** — what [`TranslationUnit::definitions`] produces, and what every file's
/// cook then asks about by name.
///
/// It is a `Vec` parallel to the unit's events rather than a map: the question is never "what is this name", it is
/// "what does the fact at this index spell", and the index comes from the view — which already resolved *which*
/// fact is in force. A map here would be a second name lookup on the hot path and a second place for the two to
/// disagree.
#[derive(Default)]
pub struct UnitDefinitions {
    /// Parallel to `TranslationUnit::events`: the definition a fact becomes as a `#define` line, or `None` when
    /// that fact is not one this reader can put back together.
    by_event: Vec<Option<std::sync::Arc<crate::macros::MacroDef>>>,
    /// Definitions whose parameter list the evidence does not carry — a function-like macro that cannot be
    /// substituted into, so it is not in the table. Counted for the unit; see [`TranslationUnit::definitions`].
    pub function_like_without_parameters: usize,
    /// Definitions with no replacement list stored at all: nothing to expand.
    pub without_a_body: usize,
    /// Definitions whose replacement list did not read as a macro definition.
    pub unreadable: usize,
    /// Bodies that arrived through the in-force channel and could not be used — nobody said whether the macro
    /// takes parameters. See `Configuration::in_force_without_a_parameter_list`.
    pub in_force_without_a_parameter_list: usize,
}

impl UnitDefinitions {
    /// The definition the fact at `index` spells, when it spells one.
    pub(crate) fn of(&self, index: u32) -> Option<&std::sync::Arc<crate::macros::MacroDef>> {
        self.by_event.get(index as usize)?.as_ref()
    }

    /// How many facts of the unit spell a definition this reader could use.
    pub fn len(&self) -> usize {
        self.by_event.iter().filter(|it| it.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// **One file's environment, as a position in the unit's timeline** — the borrowed half of
/// `TranslationUnit::environment_at`.
///
/// A view is two words: the unit and the frame. Opening one is free, and that is the point: the census built 255
/// environments per corpus (1.7 s of materialised maps) for callers that ask about a handful of names each, and a
/// file's macro state is not a *thing* the unit owns — it is where the file sits in the walk.
///
/// The three questions a view answers, and the rule behind all of them:
///
/// * the file's **own** facts are invisible here (the parser reads those out of the text itself);
/// * a fact of a file this one **includes** is in force from the offset the outermost `#include` on the path ended
///   at — one `#include` is what a file's own text can name, however deep the definition was written;
/// * a fact from **before this file was entered** is in force from offset 0, which is what a header's first line
///   sees;
/// * and everything else — a later sibling, a file the unit reaches after this one — is not visible at all.
///
/// For one name, the offsets of everything it can see **do not decrease in walk order** (all of the inherited
/// prefix is at offset 0, and the walk follows `#include`s in text order), so the *last* visible fact for a name is
/// the only one that can be in force, and asking "what is it at this offset" is one comparison. That is what makes
/// a query a hash and a short walk rather than a materialised history — and it is why this answers exactly what the
/// map it replaced answered, which `tests/translation_unit.rs` asserts question by question.
///
/// `Copy` because a view **is** its two words: a caller that has one — to report about, to cook with — hands it on
/// without giving up its own, which is what a caller holding two readings of one file does.
#[derive(Clone, Copy)]
pub struct MacroView<'unit> {
    unit: &'unit TranslationUnit,
    frame: u32,
}

impl<'unit> MacroView<'unit> {
    /// The file this view is of.
    pub fn file(&self) -> &'unit std::path::Path {
        &self.unit.frames[self.frame as usize].file
    }

    /// The offset in **this file** at which the event at `index` comes into force, or `None` when this file cannot
    /// see it at all.
    fn offset_of(&self, index: usize, event: &TuEvent) -> Option<usize> {
        let frame = &self.unit.frames[self.frame as usize];

        if event.frame == self.frame {
            // The file's own text — see the type's note.
            None
        } else if event.frame > self.frame && event.frame < frame.tout {
            // A file this one includes: the interval test is the frame's subtree (frames are in DFS preorder), so
            // this is a descendant, and the offset is found by walking the precomputed path.
            Some(self.unit.offset_in(event.frame, self.frame))
        } else if (index as u32) < frame.entry_seq {
            // Emitted before this file was entered: the inherited prefix, in force from its first line.
            Some(0)
        } else {
            // A sibling's subtree, or something the walk reached after this file was left: not visible here.
            None
        }
    }

    /// The **last** fact for `name` this file can see that `wanted` accepts, with the offset it applies from.
    ///
    /// The search runs the name's own history backwards, which is the whole cost of a query: a name's events are
    /// appended in walk order, so the first one that is visible is the one in force. A name defined once — almost
    /// every name — costs one visibility test.
    ///
    /// The history arrives as a slice so that a caller **enumerating the whole environment** passes the one it is
    /// already holding: hashing a name to find a list the caller has in hand is work a per-file loop must not do
    /// twice, and `for_each_definition` runs for every name in the unit, in every file.
    fn last_visible(
        &self,
        indices: &[u32],
        wanted: impl Fn(&TuEvent) -> bool,
    ) -> Option<(usize, u32)> {
        indices.iter().rev().find_map(|&index| {
            let event = &self.unit.events[index as usize];
            if !wanted(event) {
                return None;
            }
            self.offset_of(index as usize, event).map(|at| (at, index))
        })
    }

    /// [`MacroView::last_visible`] for a caller that has a name and not the history — the query path.
    fn last_visible_of(
        &self,
        name: &str,
        wanted: impl Fn(&TuEvent) -> bool,
    ) -> Option<(usize, u32)> {
        self.last_visible(self.unit.by_name.get(name)?, wanted)
    }

    /// The fact that settles what `name` **is** in this file, on the channel that says what a name is.
    ///
    /// The definition channel, the last visible fact of it, and — with it — whether that fact is a definition at
    /// all: an `#undef`, or a `#define` whose shape the walk could not read, is a fact about the name whose
    /// *definition* is `None`. See [`cpp_parser::IncludedMacro::undefined_at`], which is where that distinction
    /// lives.
    fn the_binding(&self, name: &str) -> Option<(usize, &'unit TuEvent)> {
        let (at, index) = self.last_visible_of(name, |event| event.unconditional)?;
        Some((at, &self.unit.events[index as usize]))
    }

    /// The **in-force** channel: the last visible fact whose replacement list this file can read.
    fn the_body_in_force(&self, name: &str) -> Option<&'unit TuEvent> {
        let (_, index) = self.last_visible_of(name, |event| {
            !event.unconditional && event.body_text.is_some()
        })?;
        Some(&self.unit.events[index as usize])
    }

    /// [`MacroView::the_binding`] as **the timeline index** of that fact, with the offset it applies from.
    ///
    /// The index rather than the event because the cooker needs to reach what the fact was *parsed into*
    /// ([`UnitDefinitions`]), and that parse is per event rather than per file: one definition reaches every file
    /// that sees it, and re-parsing it per file is what the census measured at seconds of a run.
    pub(crate) fn visible_binding(&self, name: &str) -> Option<(usize, u32)> {
        self.last_visible_of(name, |event| event.unconditional)
    }

    /// [`MacroView::the_body_in_force`] as the timeline index of that fact.
    pub(crate) fn visible_body_in_force(&self, name: &str) -> Option<u32> {
        self.last_visible_of(name, |event| {
            !event.unconditional && event.body_text.is_some()
        })
        .map(|(_, index)| index)
    }
}

impl cpp_parser::MacroFacts for MacroView<'_> {
    fn kind_of(&self, name: &str, offset: usize) -> Option<cpp_parser::SymbolKind> {
        let (at, event) = self.the_binding(name)?;
        if at > offset {
            // Defined, but later than the offset being asked about: not a macro *here*.
            return None;
        }
        match (event.function_like, event.body) {
            (Some(function_like), Some(body)) => {
                Some(cpp_parser::SymbolKind::Macro { function_like, body })
            }
            // An `#undef`, or a definition whose shape the index could not read: the name is a fact, the shape is
            // not. See the type's note on the two channels.
            _ => None,
        }
    }

    fn body_text_of(&self, name: &str, offset: usize) -> Option<&str> {
        let (at, event) = self.the_binding(name)?;
        match (at <= offset, event.function_like, event.body) {
            (true, Some(_), Some(_)) => event.body_text.as_deref(),
            _ => None,
        }
    }

    fn parameters_of(&self, name: &str, offset: usize) -> Option<&std::sync::Arc<str>> {
        let (at, event) = self.the_binding(name)?;
        match (at <= offset, event.function_like, event.body) {
            (true, Some(_), Some(_)) => event.parameters.as_ref(),
            _ => None,
        }
    }

    fn body_text_in_force(&self, name: &str) -> Option<&str> {
        self.the_body_in_force(name)
            .and_then(|event| event.body_text.as_deref())
    }

    fn parameters_in_force(&self, name: &str) -> Option<&str> {
        self.the_body_in_force(name).and_then(|event| event.parameters.as_deref())
    }

    fn body_in_force_is_object_like(&self, name: &str) -> bool {
        matches!(
            self.the_body_in_force(name).map(|event| event.function_like),
            Some(Some(false))
        )
    }

    fn knows(&self, name: &str) -> bool {
        self.the_binding(name).is_some()
    }

    fn is_empty(&self) -> bool {
        self.unit.by_name.values().all(|indices| {
            self.last_visible(indices, |event| event.unconditional)
                .is_none()
                && self
                    .last_visible(indices, |event| {
                        !event.unconditional && event.body_text.is_some()
                    })
                    .is_none()
        })
    }

    /// How many names this file sees a fact about, of the channel that says what a name is.
    ///
    /// Counted rather than stored: a view owns nothing, and the number is a measurement (the census prints it),
    /// not something a query needs.
    fn len(&self) -> usize {
        self.unit
            .by_name
            .values()
            .filter(|indices| self.last_visible(indices, |event| event.unconditional).is_some())
            .count()
    }

    /// Every definition this file sees — **one per name**, the last visible fact of the definition channel.
    ///
    /// One per name because that is what the map it replaced held: a name's earlier facts are shadowed, and a
    /// consumer building a table out of these wants the definition, not the history. Emitted in no particular
    /// order (the unit's name index is a hash map), which is what the previous implementation did too.
    fn for_each_definition<'s>(&'s self, visit: &mut dyn FnMut(cpp_parser::DefinitionFacts<'s>)) {
        for (name, indices) in &self.unit.by_name {
            let Some((at, index)) = self.last_visible(indices, |event| event.unconditional) else {
                continue;
            };
            let event = &self.unit.events[index as usize];
            let (Some(function_like), Some(_)) = (event.function_like, event.body) else {
                continue;
            };
            visit(cpp_parser::DefinitionFacts {
                name,
                at,
                function_like,
                parameters: event.parameters.as_ref(),
                body_text: event.body_text.as_ref(),
            });
        }
    }

    fn for_each_body_in_force<'s>(&'s self, visit: &mut dyn FnMut(cpp_parser::BodyFacts<'s>)) {
        for (name, indices) in &self.unit.by_name {
            let Some((_, index)) = self.last_visible(indices, |event| {
                !event.unconditional && event.body_text.is_some()
            }) else {
                continue;
            };
            let event = &self.unit.events[index as usize];
            let Some(body) = event.body_text.as_ref() else {
                continue;
            };
            visit(cpp_parser::BodyFacts {
                name,
                function_like: event.function_like,
                parameters: event.parameters.as_ref(),
                body,
            });
        }
    }
}

/// **A whole translation unit cooked into one stream** — what a compiler parses, and the map back to the files.
///
/// # Why this exists
///
/// Cooking one file at a time answers "does this header read on its own", and the readings it produced (254 of
/// 255, 452 of 455) are the ones this project has been driving up. It cannot answer "does the *program* read":
/// a declaration in `vector` and a use in the source are two different streams, and nothing in either one says
/// they belong together. This is that stream.
///
/// # How it is stitched
///
/// The walk already knows the order — frames are in DFS preorder, which *is* include order — and each frame
/// records `from_in_parent`: the offset in its parent where its text comes into force, which is where the
/// `#include` that brought it in ended. So the unit's text is the root's cooked tokens, with each child's stream
/// spliced in at its own offset, recursively. A file the walk reached **once** appears **once**, whatever number
/// of `#include`s named it — the guard idiom means the second inclusion would contribute nothing anyway.
///
/// # What the map says
///
/// One [`UnitSpan`] per token of `text`, in order, each naming the **file it stands in** and the place in that
/// file a consumer should act on. The two are not the same thing, and the difference is the reason the map
/// exists: a token produced by expanding a macro *stands* in the file that invoked it, while the text it was
/// written in is inside some `#define` — see [`UnitSpan::written`].
#[derive(Debug, Clone, Default)]
pub struct RenderedUnit {
    /// The tokens of every file the unit reached, in the order a compiler would read them.
    pub text: String,
    /// One entry per token of `text`, in the same order.
    pub spans: Vec<UnitSpan>,
    /// The files the walk entered, in the order it entered them — what a span's `file` indexes.
    pub files: Vec<std::path::PathBuf>,
    /// How many files the unit reached that the caller had **no text** for.
    ///
    /// A hole rather than an empty file: the stream is missing whatever that file would have contributed, and a
    /// consumer that needs to know whether what it read is the whole program asks this.
    pub missing: usize,
}

/// One token's place in a unit's rendering, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitSpan {
    /// Where the spelling is in [`RenderedUnit::text`].
    pub cooked: cpp_parser::SourceRange,
    /// Which file of [`RenderedUnit::files`] the token **stands in**.
    pub file: u32,
    /// The place in that file a consumer should act on.
    ///
    /// The token's own range for a token the file wrote, and the **outermost call site** for one that came out of
    /// a macro body — the text the reader can see, rather than a line inside a `#define` three headers up. See
    /// `ExpandedToken::diagnostic_range`, which is where that choice is documented.
    pub written: cpp_parser::SourceRange,
}

impl RenderedUnit {
    /// Where a token of the rendering was written, by its offset in the rendering: the file, and the range in it.
    pub fn written_at(&self, cooked_offset: usize) -> Option<(u32, cpp_parser::SourceRange)> {
        let index = self
            .spans
            .partition_point(|span| span.cooked.end_offset() <= cooked_offset);
        self.spans.get(index).map(|span| (span.file, span.written))
    }

    /// The file a token of the rendering stands in, as a path.
    pub fn file_at(&self, cooked_offset: usize) -> Option<&std::path::Path> {
        let (file, _) = self.written_at(cooked_offset)?;
        self.file_of(file)
    }

    /// The path a span's file index names.
    pub fn file_of(&self, file: u32) -> Option<&std::path::Path> {
        self.files.get(file as usize).map(std::path::PathBuf::as_path)
    }

    /// How many tokens the unit's stream has.
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// How many files the stream actually carries a token from.
    ///
    /// Not [`RenderedUnit::files`]'s length: a header whose whole body is inside a branch nobody takes
    /// contributes no token, and one nobody had text for contributes a hole.
    pub fn files_with_tokens(&self) -> usize {
        let mut seen = vec![false; self.files.len()];
        for span in &self.spans {
            if let Some(slot) = seen.get_mut(span.file as usize) {
                *slot = true;
            }
        }
        seen.into_iter().filter(|it| *it).count()
    }

    /// Append one token — the separator rule `CookedStream::render` uses, so the two renderings spell the same
    /// text for the same tokens.
    fn push(&mut self, text: &str, file: u32, written: cpp_parser::SourceRange) {
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        let start = self.text.len();
        self.text.push_str(text);
        self.spans.push(UnitSpan {
            cooked: cpp_parser::SourceRange::new(start, self.text.len() - start),
            file,
            written,
        });
    }
}

/// What one walk collects, in the order it was walked.
#[derive(Default)]
struct Walked<'a> {
    /// `(where the name becomes visible, the fact, the file that wrote it)` in translation order, last wins.
    entries: Vec<(usize, &'a MacroFact, &'a str)>,
    /// Bodies of definitions a condition guards that came out **in force** — the second channel.
    /// The body, the fact's `function_like`, and the parameter list — the two things expansion needs and reading
    /// does not. A function-like body **with** its parameters is expandable; without them it is not.
    conditional_bodies: std::collections::BTreeMap<Box<str>, ConditionalBodyValue>,
    conditional_facts: usize,
    facts_in_force: usize,
    /// **The timeline, when this walk is building one** — every fact the walk puts in force is also recorded
    /// there, with the file and offset it was written at and the frame it belongs to. `None` for the per-file
    /// walks, which collapse their result into one file's evidence and throw the order away.
    ///
    /// A field on this struct rather than a second walk, because the rules that decide *what is in force* — the
    /// include order, the merge of macros and includes by offset, the guard evaluation — are the expensive and
    /// error-prone part, and a second copy of them is where the next exception gets missed.
    timeline: Option<TimelineBuilder>,
}

impl<'a> Walked<'a> {
    /// The two channels, and the preprocessor's rule for a name defined twice: the later definition wins.
    fn into_evidence(self) -> ClosureEvidence {
        let mut by_name: std::collections::BTreeMap<&str, (usize, &MacroFact, &str)> =
            std::collections::BTreeMap::new();
        for (from_offset, fact, source) in self.entries {
            by_name.insert(&fact.name, (from_offset, fact, source));
        }

        let macros = by_name
            .into_values()
            .map(|(from_offset, fact, source)| {
                let body_text = fact
                    .body_range
                    .and_then(|range| source.get(range.start_offset..range.start_offset + range.length));

                if fact.kind.is_definition() {
                    cpp_parser::IncludedMacro::defined_with_body_and_parameters(
                        from_offset,
                        &fact.name,
                        fact.function_like,
                        fact.body,
                        body_text,
                        fact.body_range
                            .and_then(|range| parameters_before(source, range.start_offset)),
                    )
                } else {
                    cpp_parser::IncludedMacro::undefined_at(from_offset, &fact.name)
                }
            })
            .collect();

        ClosureEvidence {
            macros,
            conditional_bodies: self
                .conditional_bodies
                .into_iter()
                .map(|(name, (function_like, parameters, body))| {
                    (name, function_like, parameters, body)
                })
                .collect(),
            conditional_facts: self.conditional_facts,
            facts_in_force: self.facts_in_force,
        }
    }
}

/// Walk **one file in translation order**: its macros and its includes **merged by offset**, descending into each
/// `#include` at the point it is written.
///
/// That order is the whole point, and getting it wrong is invisible until a condition depends on it: a header
/// includes `winapifamily.h` at its top and asks `#if WINAPI_FAMILY_PARTITION (…)` sixty lines later, so a walk
/// that read a file's macros *before* its includes judges that condition against a table which does not have the
/// macro yet. Measured: that is exactly why `combaseapi.h`'s `STDMETHOD` never came into force, and therefore
/// never reached `commdlg.h`.
#[allow(clippy::too_many_arguments)]
fn walk_one_file<'a>(
    path: &'a std::path::Path,
    from_offset: usize,
    parent: Option<u32>,
    from_in_parent: usize,
    seen: &mut std::collections::HashSet<&'a std::path::Path>,
    look_up: &mut impl FnMut(&std::path::Path) -> Option<(&'a FileSummary, &'a str)>,
    seed: &Marked,
    definitions_of: &mut MacroDefinitions,
    unit: &mut UnitState,
    walked: &mut Walked<'a>,
) {
    // Each file is read once per walk. A preprocessor would re-read one that is included twice, but the guard
    // idiom (`#ifndef X / #define X`) is what every header in this corpus uses, and a second read of a guarded
    // file defines nothing new.
    if !seen.insert(path) {
        return;
    }

    let Some((file, source)) = look_up(path) else {
        // A file this walk cannot read is a file **outside the analysis**: none of the files the caller indexed.
        // Pretending it defines nothing is the reading every walk here has always taken — a name nobody saw is
        // `Undefined`, which is what lets `#ifndef GUARD` open a header at all.
        //
        // Marking the unit incomplete instead — "this file may define anything" — was tried and measured: it turns
        // every `#ifndef` of every header into `Unknown`, and the conditional evidence the corpus produces
        // collapses from millions of facts to tens of thousands. The honest answer for one file must not cost the
        // whole unit's evidence.
        return;
    };

    // The frame this file occupies in the unit's timeline, when the walk is building one. Entered **after** the
    // two early returns above, so a file that is skipped leaves no frame behind — and closed at the end of this
    // function, which is after every file it includes has been walked, so the frame's subtree is an interval.
    let frame = walked
        .timeline
        .as_mut()
        .map(|timeline| timeline.enter(path, parent, from_in_parent));

    let mut macros = file.macros.iter().peekable();
    let mut includes = file.includes.iter().peekable();

    loop {
        // Whichever the file writes **first**: that is the order a preprocessor reads them in, and a condition
        // depends on it.
        let next_is_a_macro = match (macros.peek(), includes.peek()) {
            (Some(fact), Some(include)) => fact.range.start_offset <= include.range.start_offset,
            (Some(_), None) => true,
            (None, _) => false,
        };

        if next_is_a_macro {
            let fact = macros.next().expect("peeked just above");
            let in_force = if matches!(fact.guard, FactGuard::Unconditional) {
                true
            } else {
                walked.conditional_facts += 1;
                let taken = a_guard_is_in_force(file, fact, |at| UnitMacros {
                    seed,
                    state: unit,
                    here: Some((path, at)),
                });
                if taken {
                    walked.facts_in_force += 1;
                }
                taken
            };

            if !in_force {
                continue;
            }

            // What a preprocessor's table would have: the name with its body and parameter list, or taken away
            // again. Fed for conditional facts too — a `#define` in a branch that **was** taken is a definition like
            // any other, and it is what the next file's conditions are answered against.
            if fact.kind.is_definition() {
                if let Some(definition) = definitions_of.get_or_read(path, source, fact) {
                    unit.define(&fact.name, definition, path, fact.range.start_offset);
                } else {
                    // A `#define` this reader cannot put back together is a name the unit has seen and cannot
                    // describe: defined, with nothing more to say. Better than dropping it — an `#ifdef` about it
                    // is a real question with a real answer.
                    // A #define this reader cannot put back together is not fed at all: the compilation's own
                    // table answers instead, which is Undefined — a lost reading rather than a wrong one.
                }
            } else {
                unit.undefine(&fact.name);
            }

            // The **evidence** is still two channels: definitions only from what no condition guards, and
            // bodies from everything in force. The state above is a third, separate thing — it exists to answer
            // *conditions*, and it deliberately carries no bodies.
            if matches!(fact.guard, FactGuard::Unconditional) {
                walked.entries.push((from_offset, fact, source));
            } else if fact.kind.is_definition()
                && let Some(text) = fact
                    .body_range
                    .and_then(|range| source.get(range.start_offset..range.start_offset + range.length))
            {
                let parameters = fact
                    .body_range
                    .and_then(|range| parameters_before(source, range.start_offset))
                    .map(Box::from);
                walked.conditional_bodies.insert(
                    Box::from(&*fact.name),
                    (fact.function_like, parameters, Box::from(text)),
                );
            }

            // …and the **timeline**, when there is one: every fact in force, both channels, with the file and the
            // offset it was written at. This is the record a file's environment is later read out of, so it must
            // hold what the per-file walk would have collected — including the conditional-but-in-force bodies the
            // second channel carries.
            if let (Some(frame), Some(timeline)) = (frame, walked.timeline.as_mut()) {
                timeline.record(frame, fact, source, matches!(fact.guard, FactGuard::Unconditional));
            }

            continue;
        }

        let Some(include) = includes.next() else {
            break;
        };

        if let Some(nested) = include.resolved.as_deref() {
            walk_one_file(
                nested,
                from_offset,
                frame,
                include.range.start_offset + include.range.length,
                seen,
                look_up,
                seed,
                definitions_of,
                unit,
                walked,
            );
        }
    }

    if let (Some(frame), Some(timeline)) = (frame, walked.timeline.as_mut()) {
        timeline.leave(frame);
    }
}

/// The macros a file's **direct** includes define — the one-hop variant, kept as the measured comparison for the
/// closure walk above.
pub fn macros_from_direct_includes<'a>(
    summary: &FileSummary,
    mut look_up: impl FnMut(&std::path::Path) -> Option<&'a FileSummary>,
) -> Vec<cpp_parser::IncludedMacro> {
    let mut entries = Vec::new();

    for include in &summary.includes {
        let Some(included) = include.resolved.as_deref().and_then(&mut look_up) else {
            continue;
        };
        // Where the name becomes visible: the end of the `#include` directive, which is where a consumer of the
        // tree would put a marker.
        let from_offset = include.range.start_offset + include.range.length;

        for fact in &included.macros {
            if !matches!(fact.guard, FactGuard::Unconditional) {
                continue;
            }

            entries.push(if fact.kind.is_definition() {
                cpp_parser::IncludedMacro::defined_at(
                    from_offset,
                    &fact.name,
                    fact.function_like,
                    fact.body,
                )
            } else {
                cpp_parser::IncludedMacro::undefined_at(from_offset, &fact.name)
            });
        }
    }

    entries
}

/// `#include "local.h"` against `#include <system.h>`.
///
/// **Re-used from the preprocessor rather than re-declared here**: the two are searched for differently, the
/// directive reader is what knows that, and a second enum with the same two variants is how the two layers would
/// come to disagree about a spelling.
pub use crate::preprocess::directive::IncludeForm;

/// Which conditional region a fact was written in.
///
/// An index into the file's [`SummaryGuards`] rather than an inline condition: many facts share one region, and a
/// region is a small tree while a fact is meant to be tiny. [`FactGuard::Unconditional`] is the common case — code
/// outside every `#if` — and it is a variant rather than `0` so that a fact can never be misread as guarded by
/// forgetting to fill a field in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactGuard {
    Unconditional,
    Region(u32),
}

/// The conditional regions a file's facts refer to, in the order they were opened.
///
/// A region is stored as its **question**, never as its answer. Whether a region is entered depends on the
/// macros in force, and those are a property of the compilation rather than of the file: the `-D`s, the `-std=`
/// that fixes `__cplusplus`, and the five hundred names a compiler predefines. A summary is keyed without any of
/// that (see `cache.rs`), so the answer cannot live here — the same reasoning that keeps a resolved type out of a
/// declaration fact. A query that has the environment evaluates the stored question; one that does not answers
/// `Unknown`, which is what every consumer of this type did before the questions were stored at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SummaryGuards {
    /// One entry per region: the span of its conditions, which is its identity in [`FactGuard::Region`].
    pub regions: Vec<cpp_parser::SourceRange>,
    /// One entry per region, in the same order: the branches written for it, and what encloses it.
    pub conditionals: Vec<ConditionalRegion>,
    /// The region the file's **own include guard** opens, when it has one.
    ///
    /// Not a condition, and that is the whole point: `#ifndef _GLIBCXX_STRING` at the top of `string` is the file
    /// saying "read me once", not a feature test — entering the file at all is what the guard means, so a fact
    /// inside it is as visible as one written outside every `#if`. The index already treats the facts whose guard
    /// is *exactly* this region that way ([`crate::FileSummary`], `deguard_the_files_own_guard`); storing the
    /// index is what lets a walk treat the *nested* ones that way too, and a nested fact is the common case —
    /// `#ifndef GUARD / #define GUARD` followed by a file full of `#if __cplusplus` blocks.
    ///
    /// Without it those blocks read as "the guard is not taken", because by the time they are evaluated the guard
    /// has *defined* its own name — a file whose contents are `#ifndef X / #define X / … #endif` would answer
    /// "inactive" to everything inside it, which is exactly backwards.
    pub own_guard: Option<u32>,
}

/// One conditional region: the chain of branches a single `#if` opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionalRegion {
    /// The branches in source order; the first is the `#if`/`#ifdef`/`#ifndef` that opened the region.
    pub branches: Vec<GuardBranch>,
    /// The conditional this one is written inside, by region index.
    ///
    /// Stored rather than derived from the spans, because a region's span is its **condition**: a nested
    /// region's condition lies inside its parent's body, and so does the text of a sibling `#elif`. Which
    /// conditional encloses which is a fact about the nesting, and arithmetic on two ranges would be a second
    /// way of computing it — free to disagree with the walk that knew.
    ///
    /// Regions are numbered in opening order, so a parent always has a smaller index than its children.
    pub parent: Option<u32>,
}

/// One branch of a conditional: what it asks, and the body it guards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardBranch {
    /// Which directive wrote it: `#if`, `#ifdef`, `#ifndef`, `#elif` or `#else`.
    pub kind: crate::DirectiveKind,
    /// The condition as **text to evaluate**: the expression's tokens for `#if`/`#elif` (`__cplusplus >=
    /// 201703L`), the name for `#ifdef`/`#ifndef` (`_WIN32`), and `None` for `#else`, which has no condition and
    /// holds when nothing before it did.
    ///
    /// Text rather than tokens because a token list is four fields and a range per token in every cache entry,
    /// and because reading it back is one call to the lexer that read it the first time. Text rather than a
    /// *value* because there is nothing to evaluate it with here — see [`SummaryGuards`].
    pub condition: Option<Box<str>>,
    /// The body: from after this branch's directive to the next branch's directive, or to the `#endif`.
    ///
    /// Zero-length for an empty branch, which is what `#if A\n#else\n...` writes and what a consumer has to
    /// read as "no code here" rather than as "a body I could not find".
    pub body: cpp_parser::SourceRange,
    /// Where the directive is, so that a consumer explaining "this is not compiled" can point at the condition
    /// that decided it.
    pub range: cpp_parser::SourceRange,
}

/// One conditional that contains a position, and where its own condition is.
///
/// The two are different offsets and both are needed: which branch the position falls in is asked at the
/// position, while *what the condition means* is decided where the condition was written — a `#define NAME`
/// inside a region's own body changes the answer to `#ifndef NAME` if it is read at the wrong end of the region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConditionAt {
    /// Which entry of [`SummaryGuards::regions`] this is.
    pub region: u32,
    /// The offset of the region's own condition, which is where it must be evaluated.
    pub condition_at: usize,
}

impl SummaryGuards {
    /// The conditionals containing `offset`, **innermost first**.
    ///
    /// Empty for code outside every `#if`, which is the common case and the cheap one. This is the general
    /// answer, for a caller that has a position and no guard; a caller that *has* the guard — which is every
    /// caller in this crate, because a fact records one — should use [`SummaryGuards::conditions_of`] instead:
    /// it walks the nesting outwards from the region the guard names, where this has to look for it.
    pub fn conditions_at(&self, offset: usize) -> Vec<ConditionAt> {
        let mut found: Vec<ConditionAt> = Vec::new();

        // Innermost first: the smallest region that contains the position, then its parent, and so on. A region
        // contains the position when the position is inside one of its branch *bodies* — the region's own span
        // starts at its condition, and a position before the first branch's body is not in the region at all.
        let mut current = (0..self.conditionals.len())
            .filter(|index| self.conditionals[*index].contains(offset))
            .min_by_key(|index| self.span_of(*index as u32).map_or(usize::MAX, |span| span.length));

        while let Some(index) = current {
            let Some(span) = self.span_of(index as u32) else {
                break;
            };

            if Some(index as u32) != self.own_guard {
                found.push(ConditionAt {
                    region: index as u32,
                    condition_at: span.start_offset,
                });
            }

            current = self.conditionals[index].parent.map(|parent| parent as usize);
        }

        found
    }

    /// The conditionals around the region a fact's guard names, **innermost first**.
    ///
    /// The same answer as [`SummaryGuards::conditions_at`] for the region that guard was assigned from — and the
    /// walk is the *point*: the nesting is already recorded (each region names its parent), so this costs one
    /// step per level of nesting instead of a search over every conditional in the file. A file with two hundred
    /// conditionals would otherwise pay for all of them on every include it has, in every query that walks it.
    ///
    /// The file's own guard is left out: it is not a condition — see [`SummaryGuards::own_guard`].
    pub fn conditions_of(&self, region: u32) -> Vec<ConditionAt> {
        let mut found = Vec::new();
        let mut current = Some(region);

        while let Some(index) = current {
            let Some(span) = self.span_of(index) else {
                break;
            };

            if Some(index) != self.own_guard {
                found.push(ConditionAt {
                    region: index,
                    condition_at: span.start_offset,
                });
            }

            current = self
                .conditionals
                .get(index as usize)
                .and_then(|conditional| conditional.parent);
        }

        found
    }

    /// The region a fact's guard names, as the preprocessor's own value, with the branch **in force at
    /// `offset`** as its `active_branch`.
    ///
    /// `None` when the index names no region, which a summary whose guards were written by another producer can
    /// have. The caller evaluates the returned region against the macros in force where the *condition* was
    /// written — [`SummaryGuards::conditions_at`] says where that is.
    pub fn region_at(&self, region: u32, offset: usize) -> Option<Region> {
        let conditional = self.conditionals.get(region as usize)?;

        Some(Region {
            branches: conditional.branches.iter().map(GuardBranch::as_branch).collect(),
            active_branch: conditional.branch_at(offset)?,
        })
    }

    /// The span of a region's condition, when the index names one.
    pub fn span_of(&self, region: u32) -> Option<cpp_parser::SourceRange> {
        self.regions.get(region as usize).copied()
    }

    /// Was the code the guard names compiled, given the macros in force at each condition?
    ///
    /// `macros_at` is asked for a table **per condition**, and takes that condition's own offset — not
    /// `offset`. That is not a detail: a region's condition is decided where it is written, and the tokens that
    /// matter are the ones in force there. `#ifndef NAME` whose own body then writes `#define NAME` is the shape
    /// that makes the difference, and reading the condition at the *fact's* offset would decide it the other way
    /// round — for every include guard and every `#ifndef X / #define X` block in the corpus.
    ///
    /// The rule over the chain of enclosing regions is [`crate::Guard::visibility`]'s: one region known not to be
    /// taken makes the code `Inactive` whatever the others say, one that cannot be decided makes it `Unknown`, and
    /// only when every one of them is taken is it `Active`. A table that cannot speak about a name answers
    /// [`Lookup::Unanswered`](crate::Lookup), which the evaluator turns into `Unknown` — so a condition this
    /// index has no evidence about costs an answer, never a wrong one.
    pub fn visibility_of<M: crate::MacroValues>(
        &self,
        guard: FactGuard,
        offset: usize,
        macros_at: impl Fn(usize) -> M,
    ) -> Visibility {
        // The fast path, and it is the common one: code outside every conditional is compiled without anything
        // being evaluated, and asking would cost a walk per include of every file a query touches.
        let FactGuard::Region(region) = guard else {
            return Visibility::Active;
        };

        let mut unknown = false;

        for at in self.conditions_of(region) {
            let Some(region) = self.region_at(at.region, offset) else {
                // A guard naming a region this summary does not describe — a summary written before the regions
                // carried their conditions, or one whose bytes were produced by something else. Nothing can be
                // said about it, and `Unknown` is what every query said about every region before this existed.
                unknown = true;
                continue;
            };

            match region.visibility(&macros_at(at.condition_at)) {
                Some(true) => {}
                Some(false) => return Visibility::Inactive,
                None => unknown = true,
            }
        }

        if unknown {
            Visibility::Unknown
        } else {
            Visibility::Active
        }
    }
}

impl ConditionalRegion {
    /// Which branch's body holds `offset`.
    ///
    /// The last branch that starts at or before the position when no body contains it: the position can only be
    /// in the directives between two bodies (a fact's range starts after its directive, so this is the
    /// degenerate case of a zero-length body), and "the branch that was open there" is the reading
    /// [`crate::GuardStack`] itself takes — it makes the branch in force the last one it observed.
    ///
    /// `None` only for a region with no branches at all, which no walk produces and a foreign summary can.
    pub fn branch_at(&self, offset: usize) -> Option<usize> {
        if let Some(index) = self.branches.iter().position(|branch| {
            branch.body.start_offset <= offset && offset < branch.body.end_offset()
        }) {
            return Some(index);
        }

        self.branches
            .iter()
            .rposition(|branch| branch.body.start_offset <= offset)
            .or(if self.branches.is_empty() { None } else { Some(0) })
    }

    /// Is there an `#else`? Then one of the branches is taken whatever the conditions say.
    pub fn exhaustive(&self) -> bool {
        self.branches
            .iter()
            .any(|branch| branch.kind == crate::DirectiveKind::Else)
    }

    /// Does this region's body hold `offset`?
    fn contains(&self, offset: usize) -> bool {
        self.branches.iter().any(|branch| {
            branch.body.start_offset <= offset && offset < branch.body.end_offset()
        })
    }
}

impl GuardBranch {
    /// This branch as the guard layer reads it: the same condition, with its text read back into tokens.
    ///
    /// `#ifdef NAME` becomes a branch whose *name* is set and whose tokens are empty, which is how
    /// [`crate::Branch::holds`] reads it — so the two layers cannot disagree about what `#ifdef` means, and
    /// neither can they about `#else` or about an expression.
    pub fn as_branch(&self) -> Branch {
        match self.kind {
            crate::DirectiveKind::Ifdef | crate::DirectiveKind::Ifndef => Branch {
                kind: self.kind,
                tokens: Vec::new(),
                name: self.condition.clone(),
                range: self.range,
            },
            _ => Branch {
                kind: self.kind,
                tokens: self.tokens(),
                name: None,
                range: self.range,
            },
        }
    }

    /// The stored condition, read back into tokens whose ranges point into the file they came from.
    ///
    /// The lexer rather than a split on whitespace, for the reason the reference query gives: this has to be the
    /// same reader that produced the tokens the condition was written as, and a second reader would disagree
    /// about `'` digit separators, about a suffix, and about a `//` comment ending a condition early.
    ///
    /// Ranges are shifted by the directive's own position, so a consumer that follows a token to the file lands
    /// in the right place. A text that does not lex at all is not an error: the evaluator reads what it can, and
    /// a condition that cannot be read is `Unknown` rather than wrong — the same answer the guard layer gives for
    /// a condition it cannot parse.
    fn tokens(&self) -> Vec<crate::Token> {
        let Some(text) = self.condition.as_deref() else {
            return Vec::new();
        };

        let mut errors = Vec::new();
        let mut lexer = cpp_parser::CppLexer::new(text, cpp_parser::LexerConfig::default(), &mut errors);

        let base = self.range.start_offset;
        lexer
            .tokenize()
            .into_iter()
            .filter(|token| !cpp_parser::is_trivia(token.kind))
            .map(|token| {
                crate::Token::new(
                    token.kind,
                    text.get(token.range.start_offset..token.range.end_offset())
                        .unwrap_or_default(),
                    cpp_parser::SourceRange::new(base + token.range.start_offset, token.range.length),
                )
            })
            .collect()
    }
}

/// Everything the index remembers about one file.
///
/// [`FileSummary::is_empty`] is not a curiosity: an empty summary is what a file that failed to parse, or a header
/// with nothing but comments, produces — and a consumer that treats "no facts" as "no declarations exist" is making
/// a claim the index never made. See `Known` in [`crate::symbol`] for the vocabulary that keeps those apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSummary {
    /// Where the file is, as the path it was read from.
    ///
    /// A path rather than a [`FileId`], and the reason is what survives a restart: a `FileId` is an index into a
    /// [`PathInterner`] that is built fresh every run, so an id written to disk means something different — or
    /// nothing at all — when it is read back. The path is also what a consumer needs anyway: a "go to
    /// definition" in another file opens it by name.
    ///
    /// [`FileId`]: crate::FileId
    /// [`PathInterner`]: crate::PathInterner
    pub path: std::path::PathBuf,
    /// What this summary was built from — see [`SummaryKey`]. Stored so that a loaded entry can be *checked*
    /// against the file it claims to describe rather than trusted because it was found.
    pub key: SummaryKey,
    pub declarations: Vec<DeclFact>,
    pub macros: Vec<MacroFact>,
    pub includes: Vec<IncludeFact>,
    pub guards: SummaryGuards,
    /// Where a **scope** in this file came out of a macro's replacement list — see [`MacroScopeReading`].
    ///
    /// Empty for the overwhelming majority of files, and empty is the ordinary answer rather than a failure: a file
    /// that writes its own braces has nothing to record here. It is stored because it is the one part of a summary
    /// whose evidence is in *another* file, and a consumer that can ask that file again must be able to.
    pub macro_readings: Vec<MacroScopeReading>,
}

impl FileSummary {
    /// A summary with no facts, for the file at `path`, built under `key`.
    pub fn empty(path: impl Into<std::path::PathBuf>, key: SummaryKey) -> Self {
        FileSummary {
            path: path.into(),
            key,
            declarations: Vec::new(),
            macros: Vec::new(),
            includes: Vec::new(),
            guards: SummaryGuards::default(),
            macro_readings: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty() && self.macros.is_empty() && self.includes.is_empty()
    }

    /// Put a fact's guard into the file's region list, returning the index to store in the fact.
    ///
    /// The single entry point for guarding a fact, so that the "many facts, one region" sharing cannot be forgotten
    /// at one call site out of five — the same reason the parser's rules share their predicates.
    pub fn intern_region(&mut self, region: Option<cpp_parser::SourceRange>) -> FactGuard {
        let Some(range) = region else {
            return FactGuard::Unconditional;
        };
        match self.guards.regions.iter().position(|known| *known == range) {
            Some(index) => FactGuard::Region(index as u32),
            None => {
                self.guards.regions.push(range);
                FactGuard::Region((self.guards.regions.len() - 1) as u32)
            }
        }
    }

    /// **Turn a summary built from a rendering back into a summary of the file.**
    ///
    /// A rendering's offsets are not file offsets — they are positions in text this crate spelled out — so a
    /// summary built by parsing one describes a file that does not exist. This is what makes it usable: every
    /// range it carries is answered for by [`crate::RenderedCooked::reported_span`], which is the place in the file a
    /// reader can act on (the invocation when a macro produced the text, the file's own span otherwise), and the
    /// facts whose ranges cannot be placed at all are **dropped and counted** rather than left pointing into a
    /// rendering.
    ///
    /// # What is in a rendering's summary
    ///
    /// Only declarations, and that is a property of what a rendering is: the directives are gone, so there are no
    /// macros, no includes and no conditional regions to place. The other fields are mapped anyway — the function
    /// is about the *type*, and a caller that hands it a summary from something directive-bearing should get
    /// answers for every range rather than silent rendering offsets in one field.
    ///
    /// Dropping is the honest answer for a fact whose range cannot be placed: an empty node covers no token, and a
    /// declaration the parser recovered out of nothing has nothing to point at. It is counted so that a caller can
    /// say how much of a reading it could use — see [`MapReport`].
    pub fn map_into_the_file(&mut self, rendered: &crate::preprocess::cooked::RenderedCooked) -> MapReport {
        let mut report = MapReport::default();
        let place = |range: cpp_parser::SourceRange, report: &mut MapReport| {
            match rendered.reported_span(range) {
                Some(mapped) => {
                    report.placed += 1;
                    Some(mapped)
                }
                None => {
                    report.dropped += 1;
                    None
                }
            }
        };

        self.declarations.retain_mut(|fact| {
            let (Some(range), Some(name_range)) =
                (place(fact.range, &mut report), place(fact.name_range, &mut report))
            else {
                // One of the two did not place, and a declaration is kept **whole or not at all**: half of it in
                // the file and half of it in a rendering is not a smaller answer, it is a wrong one.
                return false;
            };
            fact.range = range;
            fact.name_range = name_range;
            true
        });

        self.macros.retain_mut(|fact| {
            let Some(range) = place(fact.range, &mut report) else {
                return false;
            };
            fact.range = range;
            fact.body_range = fact.body_range.and_then(|body| place(body, &mut report));
            true
        });

        self.includes.retain_mut(|fact| {
            let Some(range) = place(fact.range, &mut report) else {
                return false;
            };
            fact.range = range;
            true
        });

        self.macro_readings.retain_mut(|reading| {
            let Some(range) = place(reading.range, &mut report) else {
                return false;
            };
            reading.range = range;
            true
        });

        // Regions are mapped **in place or not at all**: a fact's guard is an *index* into this list, so dropping
        // one would renumber the guards of every fact after it — a silently wrong summary rather than a smaller
        // one. A rendering has none of them (see the note), which is why this can be this simple.
        for region in &mut self.guards.regions {
            if let Some(mapped) = place(*region, &mut report) {
                *region = mapped;
            }
        }

        report
    }
}

/// How a summary's ranges mapped back into a file — see [`FileSummary::map_into_the_file`].
///
/// Both numbers count **ranges**, which is what the mapping answers for: a fact with two ranges contributes two
/// answers, and a fact one of whose ranges fails is dropped whole — so [`MapReport::dropped`] is what was *lost*,
/// not what was kept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MapReport {
    /// Ranges that landed in the file.
    pub placed: usize,
    /// Ranges that landed nowhere, taking their facts with them.
    pub dropped: usize,
}

/// One thing the parse of a **rendering** found, said in the file's own coordinates.
///
/// A rendering's offsets are not file offsets, so an error the parser reports against one describes text that does
/// not exist. This is that error after [`crate::RenderedCooked::reported_span`] has answered for it: a position a
/// reader can act on — the invocation when a macro produced the text, the file's own span otherwise.
///
/// It is a type of its own rather than the parser's [`cpp_parser::CppParseError`] with a shifted range, because the
/// message and the range are no longer the same claim: the parser said "here, in this text", and this says "there,
/// in that file" — the two have different owners, and a consumer that published the first as if it were the second
/// would point at a line inside a `#define` three headers up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookedDiagnostic {
    /// Where to point the reader, **in the file** the rendering was cooked from.
    pub range: cpp_parser::SourceRange,
    pub message: String,
}

/// What indexing one file's rendering produced — see [`crate::FileIndexer::index_rendering`].
#[derive(Debug, Clone)]
pub struct IndexedRendering {
    /// The file's facts **as a compiler reads them**, with every range mapped back into the file.
    pub summary: FileSummary,
    /// How the mapping went: ranges placed, ranges that landed nowhere.
    pub mapped: MapReport,
    /// The parse errors of the rendering, placed in the file.
    pub diagnostics: Vec<CookedDiagnostic>,
    /// Errors the map could not place **in this file** — a grammar error inside another file's macro body, or in a
    /// token whose spelling exists nowhere (a paste). Counted rather than dropped in silence: the rendering is not
    /// clean, and a consumer that reports "no errors" while this is non-zero is claiming more than it knows.
    pub unplaced: usize,
}

impl DeclKind {
    /// The index's vocabulary for a binding, which is deliberately **coarser** than the analysis layer's.
    ///
    /// `parser_symbols` maps the same source vocabulary onto `cpp_parser::SymbolKind`, and that is not a duplicate
    /// of this: the parser is asked "which reading should I take" (five answers, in its own enum), while the index
    /// stores "what kind of thing is written here" and has to keep that answer readable by consumers written before
    /// the finer kinds existed. Two questions, two vocabularies — the same reason `IncludeForm` is re-used rather
    /// than re-declared.
    pub fn from_binding_kind(kind: crate::BindingKind) -> DeclKind {
        use crate::BindingKind as B;
        match kind {
            B::Class | B::Enum | B::Alias | B::Typedef | B::TemplateParameter => DeclKind::Type,
            B::Function
            | B::Constructor
            | B::Destructor
            | B::ConversionFunction
            | B::OperatorFunction
            | B::LiteralOperator => DeclKind::Function,
            B::Variable | B::Enumerator => DeclKind::Variable,
            B::Namespace => DeclKind::Namespace,
            // A label, a `using`, or something the walker could not classify: the declaration is real and worth a
            // "go to definition", so it is stored as `Other` rather than dropped.
            _ => DeclKind::Other,
        }
    }
}

// Declaration facts are built by [`crate::declarations::build_facts`], in the `sema` layer rather than here, and
// the reason is worth knowing before looking for it: a fact has to say which `#if` it was written in, which means
// the walk needs the *preprocessor* state as well as the scopes — and this module is the shape of what gets
// stored, not a place that knows about directives. There was a `build_declarations` here that took only a scope
// tree and filled every guard with `Unconditional`; it was deleted rather than kept, because a function whose
// contract is "the guards are wrong" is one a caller reaches for by accident.

