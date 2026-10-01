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

use std::path::Path;

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
    /// **The names a class template declares its parameters with** — `["_Ty", "_Alloc"]` for `std::vector`, empty
    /// for anything that is not a class template.
    ///
    /// The one field in a fact that is not about the declaration's own name but about the types *inside* it, and it
    /// is here because a member's type is written with those names: `std::vector`'s `reference` is declared `_Ty&`,
    /// so a lookup of `std::vector<int>::reference` has a type that says `_Ty` and a binding that says `int`. Pairing
    /// the two is [`crate::Type::substituted`], and the pairing is positional — first parameter with first argument
    /// — which is why the order here is the declaration's order and not sorted.
    ///
    /// # Why it is stored rather than read when it is wanted
    ///
    /// Because a query holds one file and the parameters are written in another. `member_fact` is asked about
    /// `std::vector<int>::reference` from a `.cpp` that includes `<vector>`; the parameter list is in `<vector>`,
    /// whose text the index does not keep. Re-parsing that header per member query is the cost this field exists to
    /// avoid — and it is a *name*, not a conclusion, so it goes stale exactly when its own file changes, like every
    /// other field here.
    ///
    /// A **partial specialization** records its own list (`template <class T> struct vector<T*>` records `T`), which
    /// is right for it and wrong for the primary template — see [`crate::ProjectIndex::template_parameters_of`],
    /// which takes the first *template* it finds rather than the first declaration.
    pub parameters: Vec<String>,
    /// **The parameter list a function was declared with**, as the file spells it — `(_Ty* _First, size_type _Count)`,
    /// parentheses and all. `None` for everything that is not a function, and for a function whose declarator the
    /// shapes could not reach.
    ///
    /// # Why a spelling rather than a structured list
    ///
    /// Because the two consumers want different things from it and the text serves both. A **completion's detail
    /// line** shows it as written — that is the whole point, and a reader comparing `format(fmt, args)` with their
    /// call is comparing spellings. A **signature help** needs the parameters *split* and each one's span inside the
    /// label, and it already builds that by parsing the declaring file ([`crate::signature::signature_at`]), because
    /// splitting on commas is wrong the moment a default argument mentions `std::pair<int, int>`.
    ///
    /// What it replaces is the `(…)` a completion used to show for every function in another file: the fact had a
    /// return type and nothing else, so `std::format` and `f()` were described identically. Measured on the project
    /// this was written against, the list is what makes `format` recognizable in a hundred-name popup.
    ///
    /// Defaults, `...`, and the names are all kept as written for the same reason [`DeclFact::type_of`] is: this is
    /// what the file says, and a consumer that needs more reads the declaration.
    pub parameter_list: Option<String>,
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
    /// **Who may name this declaration** — the access an *access specifier* gave it in the class body it was
    /// written in.
    ///
    /// `None` for everything that is not a class member, and for a member whose class body the reading could not
    /// place: there is no access level to report for a namespace's name, and a guess would be worse than the
    /// admission — a consumer that treated `None` as "public" shows a name the reader may not write, and one that
    /// treated it as "private" hides half of every library.
    ///
    /// # Why it is recorded rather than asked
    ///
    /// Because the class body is in **another file**: a completion after `w.` on a header's class holds the
    /// cursor's file and the index's facts, and the `private:` label is in the header. The fact is already the
    /// thing that crosses that boundary, so the level travels with it — one byte on disk.
    ///
    /// # What a consumer is expected to do with it
    ///
    /// Offer a member when the reader can write it, and **only then**: [`crate::completion`] leaves out a private
    /// or protected member of a class the cursor is not in. Looking a private member *up* stays possible — a jump
    /// to a declaration the reader can see in the file is not the same question as offering a name they cannot
    /// write — which is why this is a field rather than a filter applied where the fact is built.
    pub access: Option<Access>,
    /// **Is this declaration exported from the module its file declares?**
    ///
    /// A module interface unit's declarations are visible to an importer **only** if exported: `export int f();`,
    /// anything inside `export { … }`, and everything a `export namespace n { … }` holds. This field is that
    /// reading, per declaration, and it is what stops a completion from offering a name that does not compile —
    /// measured on the module fixture (`tests/fixtures/modules`), whose `mathlib::hidden_helper` is written in the
    /// interface unit and deliberately not exported: it was offered to a file that says `import mathlib;` until this
    /// field existed.
    ///
    /// `false` for a file that declares no module, where "exported" means nothing: the visibility walk applies this
    /// field **only** to files it reached through an `import`, so an ordinary translation unit's declarations are
    /// unaffected whatever this says.
    pub exported: bool,
}

/// Who may name a declaration — see [`DeclFact::access`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    /// `public:`, or a `struct`'s members before any label.
    Public,
    /// `protected:`.
    Protected,
    /// `private:`, or a `class`'s members before any label.
    Private,
}

impl Access {
    /// The word a file writes — `public`, `protected`, `private` — for a report or a test.
    pub fn words(self) -> &'static str {
        match self {
            Access::Public => "public",
            Access::Protected => "protected",
            Access::Private => "private",
        }
    }

    /// Is this level visible to a consumer that is **not** in the class and not derived from it?
    ///
    /// The rule [`crate::completion`] applies, stated here so that the two cannot drift.
    pub fn is_open_to_everyone(self) -> bool {
        matches!(self, Access::Public)
    }
}

/// What a file declares about **modules** — the reading [`crate::ModuleInfo`] makes of its tree, kept here so that
/// the index can answer "what does this file's `import` bring in" without parsing anything again.
///
/// # Why it is in the summary rather than looked up per query
///
/// Because the answer is needed by the *visibility* walk ([`crate::ProjectIndex::visible_files`]), which runs over
/// every file a cursor can see on every cross-file question. That walk holds summaries and nothing else — no trees,
/// no text — so a module edge it cannot read from a summary is an edge it cannot follow at all.
///
/// # What is *not* here yet, and why the omission is in this direction
///
/// `export`: an `export import m;` re-exports `m`'s names, and one without `export` does not. That distinction is
/// **not** modelled yet, so a name an importer cannot in fact write may be offered — see `plan-units.md` §35 step 3.
/// The other direction (hiding a name the importer *can* write) is the one this project refuses: a missing answer is
/// smaller than a wrong one, but a wrong answer here is a completion the reader only discovers by compiling.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleReading {
    /// The module this file declares, without any partition: `my.mod` for `module my.mod;` and for
    /// `module my.mod:part;` alike. `None` for a file with no module declaration.
    pub module: Option<Box<str>>,
    /// Its partition, without the module and without the `:`.
    pub partition: Option<Box<str>>,
    /// Is this the unit importers see — `export module m;` rather than `module m;`?
    ///
    /// The one field that decides whether the file may be *reached* by an `import m;`: an implementation unit
    /// declares the module and is not what an importer gets, so resolving to one would attribute its declarations
    /// to a module that never exported them.
    pub is_interface: bool,
    /// The **modules** this file imports, in source order. Partitions (`import :part;`) and header units
    /// (`import <vector>;`) are deliberately not here: a partition belongs to the importing file's own module, and a
    /// header unit is a header — it is already reachable through the `#include` machinery, by the same search.
    pub imports: Vec<Box<str>>,
    /// **The headers this file imports as header units** — `import <vector>;`, `import "local.h";` — already
    /// **resolved** by the same search an `#include` goes through (`FileIndexer`'s resolver, at the moment the file
    /// is read).
    ///
    /// Resolved rather than spelt because the visibility walk holds summaries and can search nothing: a header unit
    /// whose header could not be found is **absent** from this list, and that is the honest state — nothing is known
    /// about it, rather than an empty header.
    ///
    /// The declarations of a header unit are all visible to an importer (a header unit exports what the header
    /// declares), so these are followed exactly like `#include`s and are **not** subject to [`DeclFact::exported`].
    pub header_units: Vec<std::path::PathBuf>,
    /// **The partitions of this file's own module that it imports** — `export import :area;` — already **resolved**
    /// to the file that declares them.
    ///
    /// A partition is not a module: `import :area;` is only legal inside `shapes`, and nothing outside may name it.
    /// But the names it exports **are** part of `shapes`'s interface when the import is an `export import`, so a
    /// reader of a file that says `import shapes;` must see them — and the file that declares them is one the
    /// visibility walk has to reach. Measured on the partition fixture
    /// (`tests/fixtures/modules/partitions.cpp`, built and run by `target/build_partitions.bat`): without this,
    /// `definition("perimeter")` resolved to the primary interface unit while `rectangle` and `square` — which the
    /// partition declares and `shapes.cppm` re-exports — answered "nothing declares it".
    ///
    /// Resolved rather than spelt, like [`ModuleReading::header_units`] and for the same reason: the walk holds
    /// summaries and can search nothing.
    pub partitions: Vec<std::path::PathBuf>,
}
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
        // **What the walk's own evaluator is told about a name**, behind an environment variable.
        //
        // The cook and the walk decide the same guards through different code, and when they disagree nothing
        // outside shows it: a guard the walk reads as `None` silently drops every fact inside it, and the only
        // symptom is content missing from a stream much later. `CPPLS_TRACE_LOOKUP=NAME` prints this path's answer.
        if let Some(wanted) = std::env::var_os("CPPLS_TRACE_LOOKUP")
            && wanted.to_string_lossy() == name
        {
            let from_state = self.state.definitions.get(name).map(|binding| {
                // **The shape, not just the name.** A definition whose parameter list or body did not survive the
                // walk is a definition the expander cannot use: `NAME (x)` stops being a call, and an `#if` that
                // asks for a *value* gets an identifier with none. Printing only "defined" hid that for several
                // rounds — the lookup answered `Defined` while the guard still came out `None`.
                let params = match &binding.definition.params {
                    None => "object-like".to_string(),
                    Some(list) => format!(
                        "fn-like({:?})",
                        list.iter().map(|held| held.name.as_ref()).collect::<Vec<_>>()
                    ),
                };
                let body: Vec<&str> = binding
                    .definition
                    .body
                    .significant()
                    .map(|token| token.text())
                    .collect();
                format!(
                    "state(defined_in={} at={} {params} body={:?})",
                    binding.defined_in.display(),
                    binding.defined_at,
                    body.join(" ")
                )
            });
            let from_seed = match self.seed.lookup(name) {
                crate::condition::Lookup::Defined(_) => "seed:Defined".to_string(),
                crate::condition::Lookup::DefinedWithoutAValue => {
                    "seed:DefinedWithoutAValue".to_string()
                }
                crate::condition::Lookup::Undefined => "seed:Undefined".to_string(),
                crate::condition::Lookup::Unanswered => "seed:Unanswered".to_string(),
            };
            println!(
                "cppls-lookup: {name} state={} {from_seed}",
                from_state.as_deref().unwrap_or("none")
            );
        }

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

    /// **Forwarded to the seed, because this is the table every condition in the walk is evaluated against.**
    ///
    /// A builtin operator is a question about the **compiler**, and the compiler's answers live on
    /// [`crate::Marked`] — the seed. Without this, the answer is the trait's default `None`, which becomes
    /// `Unknown`, and a condition that asks one is silently undecidable.
    ///
    /// # What it cost, measured
    ///
    /// `_HAS_MSVC_ATTRIBUTE(x)` is `__has_cpp_attribute(msvc::x)`, so every `#if _HAS_MSVC_ATTRIBUTE(…)` in the STL
    /// is a question about an attribute — and every one of them came out `None` on this path:
    ///
    /// ```text
    /// cook   __has_cpp_attribute(msvc::known_semantics)          -> Some(1)
    /// walk   _HAS_MSVC_ATTRIBUTE ( known_semantics )             -> None
    /// ```
    ///
    /// The walk then judged each guard false, so the `#define`s inside them — `_MSVC_KNOWN_SEMANTICS`,
    /// `_MSVC_INTRINSIC`, `_NO_SPECIALIZATIONS_MSG` and the rest of that family — **were never recorded at all**.
    /// That is why later queries answered `positional=no-binding`: the fact was not hidden, it had never been made.
    ///
    /// This is the same mistake as [`crate::preprocess::PositionalMacros`]'s note describes, on the other path: an
    /// implementation sitting one layer below the only caller that asks.
    fn builtin_operator(&self, name: &str, operand: &str) -> Option<crate::condition::Value> {
        self.seed.builtin_operator(name, operand)
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
    /// **The file writes `#pragma once`** — see [`SummaryGuards::visit_once`]. Carried on the frame because the
    /// renderer walks frames rather than summaries, and a file that is only directives has no tokens of its own to
    /// carry the pragma into the stream.
    pub(crate) visit_once: bool,
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
    fn enter(
        &mut self,
        file: &std::path::Path,
        parent: Option<u32>,
        from_in_parent: usize,
        visit_once: bool,
    ) -> u32 {
        let id = self.frames.len() as u32;
        self.frames.push(TuFrame {
            file: file.to_path_buf(),
            parent,
            from_in_parent,
            visit_once,
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
        search: Option<&dyn crate::preprocess::cooked::HeaderSearch>,
    ) -> RenderedUnit {
        let mut out = RenderedUnit {
            files: self.frames.iter().map(|frame| frame.file.clone()).collect(),
            file_lengths: self
                .frames
                .iter()
                .map(|frame| {
                    sources
                        .source_of(&frame.file)
                        .map(|text| text.len())
                        .unwrap_or_default()
                })
                .collect(),
            ..RenderedUnit::default()
        };
        let cook = UnitCook {
            unit: self,
            sources,
            definitions,
            seed,
            use_in_force_bodies,
            search,
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
    /// **What `__has_include` is answered from** — the include search, or `None` when the caller has none and the
    /// operator must answer `Unknown` rather than guess. See [`crate::preprocess::cooked::HeaderSearch`].
    search: Option<&'unit dyn crate::preprocess::cooked::HeaderSearch>,
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
        let cook = crate::preprocess::cooked::cook_with_search(
            text,
            &tokens,
            &crate::preprocess::cooked::FileMacros::new(
                self.unit.environment_at(frame),
                self.definitions,
                self.seed,
                self.use_in_force_bodies,
            ),
            self.search,
            // **The frame's own directory**, which is the file's directory: `#include "x.h"` means *the one beside
            // me*, and the same is true of `__has_include("x.h")`.
            self.unit.frames[frame as usize]
                .file
                .parent()
                .unwrap_or_else(|| Path::new(".")),
        );

        // **What this file said**, in its own coordinates. The messages are collected during the cook — a `#error`
        // in a branch nobody compiles is not said — and the range each carries is the directive's own, so the offset
        // here is a position in *this* file rather than in the rendering.
        for (kind, message, at) in &cook.messages {
            out.messages.push(UnitMessage {
                file: self.unit.frames[frame as usize].file.clone(),
                fatal: matches!(kind, crate::DirectiveKind::Error),
                message: message.clone(),
                at: *at,
            });
        }

        let mut next = 0usize;
        // **The frame's own tokens, counted before they are pushed.** A file whose text opens a brace it does not
        // close would swallow every file spliced after it, so this is the count that finds the file — and it is
        // **recorded rather than acted on**. See [`RenderedUnit::unbalanced`] for why the text goes in anyway.
        let mut depth = 0i64;
        for token in &cook.tokens {
            match token.text() {
                "{" => depth += 1,
                "}" => depth -= 1,
                _ => {}
            }
        }

        if depth != 0 {
            out.unbalanced.push(self.unit.frames[frame as usize].file.clone());
        }

        // **`#pragma once`, for a file that has nothing else to carry it.**
        //
        // A file with tokens of its own already has the pragma in them — the cook emits every live `#pragma`, and a
        // file that writes `#pragma once` writes it at the top, inside the guard, where it is live. So emitting one
        // here as well would print it twice, which is exactly what the first version of this did: `sal.h` came out
        // with `#pragma once` at line 1 *and* a synthetic copy, and a file that had been an exact match stopped
        // being one.
        //
        // What is missing is the other case: a header that is **nothing but directives** — `yvals_core.h` and its
        // neighbours — contributes no token to the rendering, so it is never cooked and its `#pragma once` never
        // reaches the stream. Every compiler keeps the line, and it is how a reader sees *which files a compilation
        // read*: measured on `#include <vector>`, `cl.exe` emits 54 of them and the first difference between the two
        // readings was a `#pragma`, because the next file in its stream was one we had emitted no line for.
        //
        // The range is the file's own first byte. A `#pragma once` written anywhere in a file means the same thing,
        // and a compiler prints it where the file begins; a zero-length range at offset 0 is honest about being a
        // *rendering* of the directive rather than a pointer into the text, and it sorts before every real token, so
        // an include that follows is spliced after it exactly as a preprocessor would.
        if self.unit.frames[frame as usize].visit_once && cook.tokens.is_empty() {
            let here = cpp_parser::SourceRange::new(0, 0);
            out.break_line_before_next();
            for spelling in ["#", "pragma", "once"] {
                out.push(spelling, frame, here);
            }
            out.break_line_before_next();
        }

        let mut lines = cook.pragma_lines.iter().peekable();
        for (index, token) in cook.tokens.iter().enumerate() {
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
            while lines.next_if(|line| line.end <= index).is_some() {}
            // A `#pragma` is a line of its own — see [`crate::CookedStream::pragma_lines`].
            if lines.peek().is_some_and(|line| line.start == index) {
                out.break_line_before_next();
            }
            out.push(token.text(), frame, at);
            if lines.peek().is_some_and(|line| line.end == index + 1) {
                out.break_line_before_next();
            }
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

/// **What a name is at one position in one file** — the question a preprocessor asks and the one this crate could
/// not previously answer about a file it had only *read*.
///
/// Everything needed to answer it was already here; what was missing was a public door. The only way to ask it was to
/// **copy the file, inject an `#error` probe, and read the compiler's complaint** — and that probe changes the thing
/// being measured. Measured, and the reason this type exists:
///
/// ```text
/// the real `xtr1common`                      → cl emits [[msvc::no_specializations(...)]] 17 times
/// a copy with `#error` probes inserted       → cl says the attribute is NOT supported (0)
/// ```
///
/// A copy is not the file: an include guard, an include order, or a `#pragma push_macro` further up all move when
/// the text moves. So the answer has to come from the analysis's own timeline, which is what this reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroAt {
    /// Whether the name is a macro there. `false` covers both "undefined" and "`#undef`ed above this line".
    pub defined: bool,
    /// **The shape** of the replacement list, when the walk recovered one.
    ///
    /// A [`cpp_parser::MacroBody`] rather than the body's text, because that is what a later parse needs to know:
    /// whether the name expands to a type, a statement, an expression or nothing. The text is in
    /// [`MacroAt::body_text`], which is what a *reader* wants. `None` with [`MacroAt::defined`] true is a name the
    /// walk saw defined but whose shape it could not read — an honest gap rather than a claim that the body is empty.
    pub body: Option<cpp_parser::MacroBody>,
    /// **The replacement list as written** — what answers "so what does this name expand to?".
    pub body_text: Option<String>,
    /// Whether the definition takes arguments.
    pub function_like: bool,
    /// Where the binding in force was written: the file, and the offset in it. `None` when the binding came from
    /// outside the unit — a compiler builtin or a command-line `-D`.
    pub written_in: Option<std::path::PathBuf>,
    /// The line in [`MacroAt::written_in`], one-based.
    pub written_at_line: Option<usize>,
}

impl TranslationUnit {
    /// **What `name` is, at byte `offset` of `file`, as this unit read it.**
    ///
    /// `None` when the file is not part of this unit's walk — which is a different answer from "not defined", and the
    /// distinction a caller needs in order to know whether it asked a question the unit can answer at all.
    ///
    /// # Why the offset is required
    ///
    /// A file's macro state is a **timeline**: `#define` and `#undef` take effect where they are written, and an
    /// include contributes its own definitions at the point it is included. Asking without a position means asking
    /// about the end of the file, which is how a name that `xtr1common:22` had `#undef`ed still read as defined — see
    /// [`crate::preprocess::cooked::attribute_support`], where that mistake cost the `[[msvc::…]]` attributes.
    pub fn macro_at(
        &self,
        file: &std::path::Path,
        name: &str,
        offset: usize,
    ) -> Option<MacroAt> {
        // **The file is matched the way this crate matches every path**, not by `Path` equality: a caller reaching for
        // a header from an `#include <...>` spelling hands over forward slashes and whatever case it typed, while the
        // walk recorded the path the filesystem gave it. Comparing raw paths made every question about a system header
        // answer "this session's walk never reaches that file" — the one answer that is not a reading of anything.
        let wanted = crate::file::paths::normalize_path(file, true);
        let frame = self
            .entered
            .iter()
            .find(|(held, _)| crate::file::paths::normalize_path(held, true) == wanted)
            .map(|(_, frame)| *frame)?;
        let view = self.environment_at(frame);

        // **A file this walk recorded facts for at all**, which is not the same as "a file it entered".
        //
        // A header that is *only* directives — every `#define` and `#if` and no declaration — contributes no token to
        // the rendering, and the unit's timeline can end up with no event of its frame. Measured, on a three-line
        // repro and again on MSVC's `yvals_core.h`:
        //
        // ```text
        // the cooked stream        FEATURE_FLAG is 1, VISIBLE_MACRO is defined   ← correct, and what parsing uses
        // this query               FEATURE_FLAG is "not a macro"                 ← wrong
        // ```
        //
        // The stream is right because a cook reads such a file through its own table; this query reads the unit's
        // timeline, which for that shape holds nothing. `None` says so, and the caller reports "no reading" instead of
        // a confident `false` — a tool whose answers are used to decide what to fix must not invent one.
        if !self.events.iter().any(|event| event.frame == frame) {
            return None;
        }

        // **A file's own facts are visible to it, and `MacroView::the_binding` cannot say so.**
        //
        // That method exists for the parser, which reads a file's own definitions **from its own parse** and so wants
        // the walk to report only inherited ones: `offset_of` answers `None` for an event of this very frame, and
        // `last_visible` turns that into "not visible". Asking through it produced the absurdity this code was written
        // to fix —
        //
        // ```text
        // at vadefs.h:30      _CRT_PACKING  →  not a macro      ← defined at :18, in this file
        // at vcruntime.h:100  _CRT_PACKING  →  #define 8        ← from another file, and visible
        // ```
        //
        // So both halves are read and the later one wins, which is the rule the whole table follows:
        //
        // * **this file's own last fact at or before `offset`**, out of the unit's name history, which is in walk
        //   order — a fact written by this file is in force from where it is written;
        // * **the inherited one**, if any, from `the_binding`, which knows about the files this one includes.
        let own = self
            .by_name
            .get(name)
            .into_iter()
            .flatten()
            .rev()
            .map(|index| &self.events[*index as usize])
            .find(|event| event.unconditional && view.applies_here(frame, offset, event));

        let inherited = view.the_binding(name);

        // The event in force, taken as the **later** of the two candidates: the file's own last fact at or before the
        // position, and whatever the inclusive walk says is in force there.
        let event: &TuEvent = match (own, inherited) {
            (Some(own), None) => own,
            (None, Some((at, event))) if at <= offset => event,
            (None, Some(_)) => {
                return Some(MacroAt {
                    defined: false,
                    body: None,
                    body_text: None,
                    function_like: false,
                    written_in: None,
                    written_at_line: None,
                });
            }
            (Some(own), Some(_)) => own,
            (None, None) => {
                return Some(MacroAt {
                    defined: false,
                    body: None,
                    body_text: None,
                    function_like: false,
                    written_in: None,
                    written_at_line: None,
                });
            }
        };

        let defined = event.body.is_some() || event.function_like.is_some();
        Some(MacroAt {
            defined,
            body: event.body,
            body_text: event.body_text.as_ref().map(|text| text.to_string()),
            function_like: event.function_like.is_some_and(|held| held),
            written_in: Some(self.frames[event.frame as usize].file.clone()),
            written_at_line: None,
        })
    }
}

impl<'unit> MacroView<'unit> {
    /// The file this view is of.
    pub fn file(&self) -> &'unit std::path::Path {
        &self.unit.frames[self.frame as usize].file
    }

    /// **Is this fact in force here, at `offset`?** — [`TranslationUnit::macro_at`]'s test, which needs the facts
    /// [`MacroView::offset_of`] deliberately hides.
    ///
    /// [`MacroView::offset_of`] answers `None` for an event of the file being asked about, because the only caller it
    /// was written for — the parser — reads a file's own definitions **from that file's own parse** and wants the
    /// walk to report inherited facts alone. A caller asking "what is this name *here*" wants the opposite, and has to
    /// make the same three-way decision with the own case included.
    ///
    /// The trap is the middle case: an event carries `at` in **its own file's** coordinates, so comparing it against
    /// the offset being asked about compares two different rulers. Which is what the previous version did —
    /// `event.frame == frame && event.at <= offset` — and a cross-file event that survived the frame test carried a
    /// number from another file into the comparison. It was right by luck for a fact inherited from an include and
    /// wrong for a fact the file wrote itself.
    fn applies_here(&self, frame: u32, offset: usize, event: &TuEvent) -> bool {
        let held = &self.unit.frames[frame as usize];

        if event.frame == frame {
            // The file's own text: its offsets are the ones being asked about.
            return event.at <= offset;
        }

        if event.frame > frame && event.frame < held.tout {
            // A file this one includes — the interval test is the frame subtree, since frames are in DFS preorder.
            // The fact is in force from the `#include` that brought it in, **in this file's coordinates**.
            return self.unit.offset_in(event.frame, frame) <= offset;
        }

        // Anything else was written after this file was left, which is not visible here at all.
        false
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
    ///
    /// # Why a fact a condition settled counts as a binding here
    ///
    /// This asked for `event.unconditional` alone, and that is **not** the same question as "is this fact in force":
    /// a `#define` inside `#if GUARD` is recorded as a *conditional* fact, and when the walk found `GUARD` true it is
    /// as much in force as one written at file scope. Skipping those lost the definition entirely.
    ///
    /// Measured, and it is what the whole `[[msvc::…]]` family came down to. A three-line repro:
    ///
    /// ```cpp
    /// // h2.h
    /// #if _HAS_MSVC_ATTRIBUTE(known_semantics)      // evaluates true, so the walk records the fact
    /// #define KM [[msvc::known_semantics]]
    /// #endif
    ///
    /// // g3.cpp
    /// #include "h2.h"
    /// KM int a;                                     // cl: [[msvc::known_semantics]] int a;
    /// ```
    ///
    /// We expanded nothing and left `KM` in the stream, for the reason `CPPLS_TRACE_MACRO=KM` printed:
    /// `in_force=no positional=no-binding seed=no`. The definition was in the timeline — the walk recorded it — and
    /// this filter is what hid it. The same masked every conditional definition in a compiler's own headers, which is
    /// why `_STL_STRINGIZE` (48 recordings) and the `_MSVC_*` attribute macros all read as undefined.
    ///
    /// A conditional fact with **no** body is still not a binding: that is an `#undef` in a branch, or a definition
    /// whose replacement list nobody carried, and [`UnitDefinitions::of`] answers `None` for it either way.
    pub(crate) fn visible_binding(&self, name: &str) -> Option<(usize, u32)> {
        self.last_visible_of(name, |event| event.unconditional || event.body.is_some())
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
    /// **The `#error` and `#warning` lines the compilation would have said**, in file order.
    ///
    /// A message is not program text, so the stream keeps no token for it — this is the only channel by which it
    /// survives, and it exists so that a caller can **ask a header a question**: put a `#error` at a position, render,
    /// and see whether it fires. That is how a real chain's branch is found when two layers disagree, and it is the
    /// question this crate kept answering by copying a header — which demonstrably does not work, because a copy's
    /// include guard, include order and `#pragma push_macro`s all move when the text moves.
    pub messages: Vec<UnitMessage>,
    /// One entry per token of `text`, in the same order.
    pub spans: Vec<UnitSpan>,
    /// The files the walk entered, in the order it entered them — what a span's `file` indexes.
    pub files: Vec<std::path::PathBuf>,
    /// **How long each file's own text is**, parallel to [`RenderedUnit::files`].
    ///
    /// The table that makes [`RenderedUnit::written_span`]'s answer checkable: the file it names and the range it
    /// gives are both about *that file's* text, so the range has to end at or before this length. A file the caller
    /// had no text for has length 0 — and a range in it is then impossible rather than merely unlikely.
    pub file_lengths: Vec<usize>,
    /// **The stream's own brace balance**: `{` minus `}` over every token in it.
    ///
    /// One number, and it separates two failures that look identical from outside: a stream that does not balance
    /// (a file's text opened a brace it never closed, so everything spliced after it is inside) and a *balanced*
    /// stream whose scopes the walk still pairs wrongly. The first is a fact about the rendering, the second about
    /// the reader, and no amount of looking at a wrong scope name tells them apart.
    pub braces: i64,
    /// How many files the unit reached that the caller had **no text** for.
    ///
    /// A hole rather than an empty file: the stream is missing whatever that file would have contributed, and a
    /// consumer that needs to know whether what it read is the whole program asks this.
    pub missing: usize,
    /// **Files whose own text does not balance its braces** — recorded, and **no longer dropped**.
    ///
    /// # Why this field exists
    ///
    /// A brace opened in one file and closed in another is not a defect in C++ — a header that opens a namespace
    /// for its includer to close is a real idiom — but in a **program** stream it is a hazard, and it is the hazard
    /// that made this field exist: measured on a real project, `CodeAnalysis/sourceannotations.h` (the Windows
    /// SDK's `/analyze` header, the one file a census has never read cleanly) opens `namespace vc_attributes {` and
    /// the close is not in the text we rendered. Everything spliced after it — `<cstdio>`, `<string>`, the whole
    /// standard library — then read as `vc_attributes::std`, and a reader asking about `std::size_t` got "the index
    /// has no such name" while the file in front of them declared it.
    ///
    /// # What used to be done about it, and why that was the wrong shape
    ///
    /// The frame's own tokens were **left out of the stream** (its includes were still spliced, being files with
    /// their own text) and the file was named here. That is the plan's gate ①, and §4 rule 1 is written against it:
    /// *"任何一层都不允许因为'不确定'而放弃已经确定的答案."* Dropping a file's text because its braces do not
    /// balance throws away every declaration in it — a file with one unbalanced brace and eight hundred good
    /// declarations contributed **none** of them — and it is the same all-or-nothing shape as the `crossings == 0`
    /// gate that used to sit one layer up.
    ///
    /// # What is done instead
    ///
    /// The text goes **in**, and the crossed pairing it creates is repaired where every other crossed pairing is
    /// repaired: [`crate::FileIndexer::index_unit_rendering`] finds the `{` this file opened paired with a `}` in
    /// another file, neutralises **that pair**, and reads this file on its own as well. So the declarations are
    /// filed at the scope their own file gives them, the file after this one is not swallowed, and this list is the
    /// **record of which files those were** — a reason to doubt the files named, not a reason to have read nothing.
    ///
    /// A file named here is still a file worth looking at: its text genuinely does not balance, which is either a
    /// real idiom (the open/close-across-includes shape above) or a defect in the file. What changed is that the
    /// answer to "which" is now visible in the index instead of being priced in silence.
    ///
    /// # The root cause is elsewhere, and is still registered
    ///
    /// An identifier nobody defines evaluates to **0** in `#if`, so `#if defined(_PREFAST_)` is decidable — and
    /// answering `Unknown` there is what pulls the `/analyze` header into a program that never asks for it. That is
    /// §3.2 item 1 (the builtin macro table), and it is the fix that stops this file being in the program at all.
    pub unbalanced: Vec<std::path::PathBuf>,
    /// The next token starts a new line: a `#pragma` directive ends at its newline, and `text` has no other.
    pub(crate) pending_break: bool,
}

/// **One `#error` or `#warning` the compilation would have said**, with where it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitMessage {
    /// The file that wrote it — a member of [`RenderedUnit::files`].
    pub file: std::path::PathBuf,
    /// `true` for `#error`, `false` for `#warning`.
    pub fatal: bool,
    /// The message, as written.
    pub message: String,
    /// The byte offset of the directive **in that file**, so a caller can say where it is.
    pub at: usize,
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
    /// **Where each file begins in the rendering**, as offsets into [`RenderedUnit::text`] — the boundaries between
    /// the files a translation unit was stitched from.
    ///
    /// The first token of each file that contributes one, in order; the first file's own start is omitted, because
    /// offset 0 is a boundary everything already has. A file that contributes **no token** — a header that is only
    /// `#define`s, whose whole body the preprocessor consumed — has no boundary, and that is right: it opened no
    /// scope either, so nothing needs closing at its end.
    ///
    /// # What this is for, and why nothing calls it yet
    ///
    /// It exists so a parser of this text can be told where one file ends and the next begins, and can therefore
    /// close a scope one file left open instead of letting it hold the next file. **That rule does not work as a
    /// tree-builder rule**, which was measured rather than assumed:
    ///
    /// ```text
    /// boundary None → TranslationUnit → NamespaceDecl → CompoundStat → Declaration
    /// boundary 0    → TranslationUnit → NamespaceDecl → CompoundStat → Declaration
    /// boundary 39   → TranslationUnit → NamespaceDecl → CompoundStat → Declaration
    /// ```
    ///
    /// Boundary `0` is satisfied by the first token, so it closes whatever is open at the start of the stream — and
    /// the shape does not move. The reason is that the holder is a `CompoundStat`: the brace of `namespace first {`
    /// was paired by the **grammar** with the `}` that ends the file, so the leak is a scope-pairing decision made
    /// while the tokens were being read, not an imbalance in the event stream that closing a node can repair.
    ///
    /// So the fix belongs in the grammar's own scope handling, and this stays as the piece such a fix will need:
    /// the offsets are a fact about the rendering, computed once, and nothing about them depends on how the parser
    /// chooses to use them. See `Session`'s tests
    /// (`a_file_that_does_not_balance_costs_nothing_but_a_name`) for what is currently true and asserted.
    pub fn file_boundaries(&self) -> Vec<usize> {
        let mut boundaries = Vec::new();
        let mut seen = vec![false; self.files.len()];

        // The spans are in cooked order, so the first time a file appears is where it begins.
        for span in &self.spans {
            if let Some(slot) = seen.get_mut(span.file as usize)
                && !*slot
            {
                *slot = true;
                if span.cooked.start_offset != 0 {
                    boundaries.push(span.cooked.start_offset);
                }
            }
        }

        boundaries
    }

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

    /// **Which file's cook produced the token at this offset** — the file it *stands in*.
    ///
    /// The question every caller that has to divide a stream between files is really asking, and the one
    /// [`RenderedUnit::written_at`] *looks* like it answers. It does not: the second field there is
    /// [`UnitSpan::written`], a **navigation hint** — for a token expanded out of a `#define` in another header it
    /// is a position in *that* header, so reading the file out of it names the file the body was written in rather
    /// than the file the token stands in.
    ///
    /// Measured, and it is why eight files of MSVC's standard library were quarantined as leaked:
    /// `brace_crossings` divided the stream with `written_at(..).map(|(file, _)| file)`, so a `{` written in a
    /// header's `#define` and its matching `}` written by the *invoking* file — one scope, one file — looked like a
    /// brace pair spanning two files. `<memory>`, `<atomic>`, `<variant>`, `<any>`, `<functional>`, `<bitset>`,
    /// `<chrono>` and `<format>` were taken out of the program for it, and the members of `std::unique_ptr`,
    /// `std::atomic` and `std::variant` went with them.
    pub fn file_standing_at(&self, cooked_offset: usize) -> Option<u32> {
        let index = self
            .spans
            .partition_point(|span| span.cooked.end_offset() <= cooked_offset);
        self.spans.get(index).map(|span| span.file)
    }

    /// Where a **span** of the rendering was written: the file it stands in, and the range to act on there.
    ///
    /// [`RenderedUnit::written_at`] answers for one token; this is what a *fact* needs, because a declaration is a
    /// span and a reader wants the whole of it. The two ends are asked separately and the answer is the span
    /// between them, which is the same arithmetic
    /// [`RenderedCooked::written_span`](crate::preprocess::cooked::RenderedCooked::written_span) does for one
    /// file's rendering.
    ///
    /// The two ends can stand in **different** files — a macro invocation that expands to text from two headers,
    /// or a declaration whose body came out of one and whose name was written in another — and there the honest
    /// answer is the first token's place: a range the reader can see, rather than a span across two texts.
    ///
    /// # Why this is the token's file and not its hint's
    ///
    /// `span.file` is which file's cook produced the token — the file it **stands in**, and the only thing that can
    /// say which file a declaration is in. `span.written` is a *navigation hint*, so reading the file out of it
    /// would be reading it out of the wrong field.
    ///
    /// That distinction turns out **not** to be a live bug, and the reason is worth recording because it is what
    /// [`RenderedUnit::span_lands_in`] now guards: `ExpandedToken::diagnostic_range` already answers with the
    /// **outermost call site** for a token that came out of a macro body — a position in the invoking file rather
    /// than the `#define` — so a hint is a range in the file the token stands in and the two fields agree.
    /// `a_declaration_starts_in_the_file_it_stands_in` is the shape that separates them, and it is asserted rather
    /// than argued because nothing else enforces it.
    pub fn written_span(
        &self,
        range: cpp_parser::SourceRange,
    ) -> Option<(u32, cpp_parser::SourceRange)> {
        let (file, first) = self.written_at(range.start_offset)?;
        let (last_file, last) = self.written_at(range.end_offset().saturating_sub(1))?;

        if last_file != file || last.end_offset() <= first.start_offset {
            return Some((file, first));
        }

        Some((
            file,
            cpp_parser::SourceRange {
                start_offset: first.start_offset,
                length: last.end_offset() - first.start_offset,
            },
        ))
    }

    /// **Does this span's answer land in the file the answer names?**
    ///
    /// The invariant [`RenderedUnit::written_span`]'s contract implies and that nothing performed: a range that
    /// names a file it does not fit in is a wrong answer of a kind measured on this project — a 93 971-byte class
    /// body filed under a 1.2 KB header, with the name, the scope and the range all correct for the file it named.
    /// [`crate::file_what_was_found`] counts what fails this rather than filing it.
    pub fn span_lands_in(&self, file: u32, written: cpp_parser::SourceRange) -> bool {
        match self.file_lengths.get(file as usize) {
            Some(&length) => written.end_offset() <= length,
            None => false,
        }
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
    ///
    /// `pub(crate)` rather than private because this crate's tests build a unit token by token to check the mapping,
    /// and a test that spelled the text itself would be testing its own spelling rather than this rule.
    pub(crate) fn push(&mut self, text: &str, file: u32, written: cpp_parser::SourceRange) {
        if !self.text.is_empty() {
            self.text.push(if self.pending_break { '\n' } else { ' ' });
        }
        self.pending_break = false;
        match text {
            "{" => self.braces += 1,
            "}" => self.braces -= 1,
            _ => {}
        }
        let start = self.text.len();
        self.text.push_str(text);
        self.spans.push(UnitSpan {
            cooked: cpp_parser::SourceRange::new(start, self.text.len() - start),
            file,
            written,
        });
    }

    /// Put the next token on a line of its own. See [`crate::CookedStream::pragma_lines`].
    pub(crate) fn break_line_before_next(&mut self) {
        self.pending_break = true;
    }

    /// **The same program without the tokens of `left_out` files** — the stream a fenced parse reads. Everything else
    /// is the original, in the original order; a left-out file's includes were spliced as files of their own and
    /// stay.
    ///
    /// # This is no longer what the indexer does, and the reason is the whole of M0
    ///
    /// It **is** still the right way to answer "what would this program be without that file", and its contract —
    /// the length is preserved, so the original span table still maps every fact — is the one
    /// [`RenderedUnit::neutralized`] shares. What changed is that the unit reader **stopped asking that question**:
    /// taking a file out of a program because one of its braces was mis-paired costs every declaration in the file,
    /// and measured on a real project that is how `<format>` and `<type_traits>` came to be missing from an index
    /// that had read them. The reader now gives up the **pairing** instead ([`RenderedUnit::neutralized`]) and reads
    /// the file on its own as well. `tests/translation_unit.rs` is where this is still exercised.
    ///
    /// # The tokens are blanked, not removed, and that is the whole point
    ///
    /// The text that comes out is **the same length** as the text that went in: every token of a left-out file is
    /// replaced by as many spaces as it occupied, byte for byte, and every other byte is copied as it stands. So an
    /// offset in the result is the same offset in the original — which is what lets a fact found in the blanked
    /// text be mapped back through [`RenderedUnit::written_span`], whose spans are still the original ones. This is
    /// documented on [`RenderedUnit::only`] as the property a file's own reading depends on, and `without` is the
    /// other half of the same contract.
    ///
    /// # What removing them cost
    ///
    /// The version before this one *deleted* the left-out tokens and rebuilt the span table as it went, which is
    /// the same thing the compiler does and was wrong here for a reason that has nothing to do with parsing:
    /// **the parse that follows maps its declarations back through the original span table**, so deleting six
    /// kilobytes from the middle of a 2 MB stream moved every later offset by six kilobytes and filed every
    /// declaration after it under the wrong file. Measured on a project that includes `<string>`:
    /// `basic_string`'s 93 971-byte class body — written in `<xstring>` — was filed under
    /// `<__msvc_formatter.hpp>`, the file whose *forward declaration* of the same name stands 6 100 bytes earlier.
    /// The reading looked right: the name was right, the scope was right (`std`), the reported range was right for
    /// its own file, and the file was another file's. Nothing else in the pipeline could notice.
    ///
    /// Spaces rather than removal also keeps the **braces** where they were, so the second parse sees the same
    /// token positions as the first and its scope pairing is comparable. Trivia is dropped by the parser either
    /// way, so a blanked file contributes nothing but its length.
    ///
    /// # The one remaining caller
    ///
    /// `tests/translation_unit.rs`'s `taking_a_file_out_of_the_stream_keeps_the_offsets`, which is where the three
    /// properties above are asserted. Nothing in the pipeline calls this any more: the cook no longer drops an
    /// unbalanced file ([`RenderedUnit::unbalanced`]) and the reader no longer drops a file whose brace crossed
    /// ([`RenderedUnit::neutralized`]). It stays because "what would this program be without that file" is a
    /// question the next mechanism will ask again, and because deleting a function whose contract is documented and
    /// tested is how the *reason* for not using it gets forgotten.
    pub fn without(&self, left_out: &std::collections::BTreeSet<u32>) -> RenderedUnit {
        let mut out = self.empty_like();
        let mut bytes = self.text.as_bytes().to_vec();

        for span in &self.spans {
            if !left_out.contains(&span.file) {
                continue;
            }
            for byte in &mut bytes[span.cooked.start_offset..span.cooked.end_offset()] {
                *byte = b' ';
            }
        }

        out.text = String::from_utf8(bytes).expect("only spaces replaced bytes of a UTF-8 stream");
        // **The spans are the original ones**, so an offset in this text is an offset in that one. That is a
        // stronger statement than the one `only` makes — it is the same table, not a copy — and it is what the
        // caller's mapping depends on.
        out.spans = self.spans.clone();
        out
    }


    /// **Only the tokens `file` itself wrote** (a token a macro of its own produced counts) — what a file whose brace
    /// had to be given up is read from, so that its own scopes are all its own.
    ///
    /// Its spans are the original ones, so a fact found here maps back through
    /// [`RenderedUnit::written_span`] exactly as it would from the whole program.
    pub fn only(&self, file: u32) -> RenderedUnit {
        let mut out = self.empty_like();
        for span in &self.spans {
            if span.file == file {
                if span.cooked.start_offset > 0 && self.text.as_bytes()[span.cooked.start_offset - 1] == b'\n' {
                    out.break_line_before_next();
                }
                out.push(&self.text[span.cooked.start_offset..span.cooked.end_offset()], span.file, span.written);
            }
        }
        out
    }

    fn empty_like(&self) -> RenderedUnit {
        RenderedUnit {
            files: self.files.clone(),
            // The lengths travel with the files: every one of these streams answers about the same texts, and a
            // `written_span` answer is only checkable while the two agree.
            file_lengths: self.file_lengths.clone(),
            missing: self.missing,
            unbalanced: self.unbalanced.clone(),
            ..RenderedUnit::default()
        }
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
    /// **Includes the walk did not enter because their guard was not in force.**
    ///
    /// Counted rather than silent, for the same reason [`Walked::conditional_facts`] is: it is the size of the
    /// difference between the closure this walk builds and "every file any `#include` names", and a reading that
    /// entered a file a compiler does not read is a reading of a different program.
    includes_skipped: usize,
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
    let frame = walked.timeline.as_mut().map(|timeline| {
        timeline.enter(path, parent, from_in_parent, file.guards.visit_once)
    });

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

        // **An `#include` is a guarded fact, and the walk asks whether its guard is in force.**
        //
        // It did not, and that is not a small omission on a compiler's own headers: *every* conditional include in
        // them was entered. `sal.h` writes
        //
        // ```cpp
        // #if _USE_ATTRIBUTES_FOR_SAL          // 1561 — which the cooker decides is **0**
        // #include "CodeAnalysis/sourceannotations.h"
        // ```
        //
        // and the walk descended anyway, pulling 1020 tokens of `/analyze`-only declarations into a program that
        // never asks for them — 99% of the reading of `#include <sal.h>`, and the first difference against cl.exe on
        // `#include <vector>`. Meanwhile the *cooker* answered the same guard correctly, which is what made the two
        // disagree: one layer asked, the other did not.
        //
        // A guard that cannot be decided is **`Unknown`, and `a_guard_holds` is false for it** — the same rule the
        // macro facts below already followed, and the reason `Visibility` has three states rather than two.
        if !crate::index::environment::a_guard_holds(file, include.guard, include.range.start_offset, |at| {
            UnitMacros {
                seed,
                state: unit,
                here: Some((path, at)),
            }
        }) {
            walked.includes_skipped += 1;
            continue;
        }

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
    /// **The file writes `#pragma once`**, and so is entered at most once per translation unit.
    ///
    /// Stored rather than derived at the point it is needed, because the two places that need it are far from the
    /// directives: the walk, which skips a second visit, and the **renderer**, which has to put the pragma into the
    /// stream. `#pragma once` is the one directive that is *recognised* and, until this field existed, never
    /// *produced* — the cook only ever sees the file it is cooking, and a header that is nothing but directives
    /// contributes no token to be cooked at all, so its `#pragma once` never reached the output.
    ///
    /// Measured on `#include <vector>`: the compiler's stream carries **54** of them and ours carried none of the
    /// ones from such files, which is what made the first difference a `#pragma`: every compiler emits the pragma of
    /// each file it enters, and the presence of that line is how a reader sees which files a compilation read.
    pub visit_once: bool,
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
    /// **What this file declares about modules** — see [`ModuleReading`], and `plan-units.md` §35 for why the
    /// visibility walk needs it *in* the summary rather than in a lookup beside it.
    pub modules: ModuleReading,
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
            modules: ModuleReading::default(),
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

/// What indexing a **whole unit's** rendering produced, file by file — see
/// [`crate::FileIndexer::index_unit_rendering`].
///
/// One parse, many files: the facts are grouped by the file each declaration was written in, so a caller files
/// them under their own paths rather than under the unit's root.
#[derive(Debug, Default)]
pub struct IndexedUnit {
    /// Each file's share, in the order the unit's frames hold them — which is include order.
    pub files: Vec<(std::path::PathBuf, crate::CookedFile)>,
    /// Facts and errors that could not be placed in any file.
    pub unplaced: usize,
    /// How many tokens the unit's stream has.
    pub tokens: usize,
    /// How many files the stream actually carries a token from.
    pub files_with_tokens: usize,
    /// How many files the walk reached that the caller had no text for.
    pub missing: usize,
    /// Files whose own text does not balance its braces — see [`RenderedUnit::unbalanced`].
    ///
    /// A **report**, not a filter: the files named here contributed their tokens to the stream like every other
    /// file, and the crossed pairing their imbalance caused was repaired by this very function. A reader sees the
    /// list to know which files to doubt, which is the plan's rule 3 — an uncertain answer that names what it is
    /// uncertain about beats both a silent wrong answer and a refusal to answer.
    pub unbalanced: Vec<std::path::PathBuf>,
    /// The stream's own brace balance — see [`RenderedUnit::braces`].
    pub braces: i64,
    /// Files **repaired**: the parser paired a brace of theirs with a brace in another file, so that one pairing was
    /// neutralised in the stream (`RenderedUnit::neutralized`) and the file was parsed on its own as well — see
    /// [`crate::FileIndexer::index_unit_rendering`]. What they declare is filed from the program; what their parse
    /// got wrong stays inside them.
    ///
    /// This used to be called *quarantine*, and the name was the mechanism: the file's tokens left the program
    /// entirely ([`RenderedUnit::without`]), so a file that leaked one brace lost every declaration in it. The plan's
        /// **How many braces were given up to repair a crossing, and none of them is still in the stream.**
    ///
    /// Counted as the offsets **actually neutralised** — not as the crossings a parse reported, which is a different
    /// number for a reason worth knowing: a parse that reports a crossing out of braces already given up has found
    /// nothing new to fix, and counting it would report a repair that did not happen. Only the new ones count.
    ///
    /// It is not a gate, and that is deliberate: a non-zero value here means the parse had to repair something, so
    /// the declarations nearest those braces are the ones to doubt — not a reason to file nothing. Compare
    /// [`RenderedUnit::unbalanced`], which records a file whose braces do not balance before the parse even runs;
    /// this is the after-the-parse half of the same policy, and it is the one that must not scale up to the whole
        /// **Crossings the repair could not cure**, one per crossing left in the final parse.
    ///
    /// The honest counterpart of [`IndexedUnit::repaired`], and it exists because the two can differ: a parser that
    /// reports a crossing out of braces that are *already* markers has nothing left for the repair to give up, so the
    /// loop stops and reports the remainder rather than spinning out its rounds pretending to fix it.
    ///
    /// Zero in every case measured, and non-zero is **not** a reason to refuse the reading — it is the same
    /// all-or-nothing gate this whole milestone removed, one level down. What it means is that the files named in
    /// [`IndexedUnit::quarantined`] are the ones whose scopes the program's parse got wrong, and that their own
        /// Errors the parse of the stream reported.
    ///
    /// Reported per file (each lands in the file it is in) and never the gate — see `repaired`. It was the gate
    /// while the only defence against one file's unclosed scope was to refuse the whole program.
    ///
    /// *The original account, kept because it is the measurement:*
    ///
    /// **This was the gate the unit reading needed**, and `braces` is not: measured on a real project the stream is
    /// *lexically* balanced (`braces: 0`) while `CodeAnalysis/sourceannotations.h` — the one file a census has never
    /// read cleanly, registered as "`/analyze`-only syntax, not a gap" — still leaves its `namespace vc_attributes`
    /// open **syntactically**, so every file spliced after it reads as `vc_attributes::std`. Equal counts of `{` and
    /// `}` do not make the parser pair them the way the file meant; only the parse does.
    pub errors: usize,
}

/// What reading one translation unit as a program produced — see [`crate::Session::read_the_unit`].
///
/// A report rather than the facts themselves: the facts went into the index (that is the point of the call), and
/// what a caller wants back is whether the reading happened and how much of the program it covered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitReading {
    /// The unit's root, in the index's own spelling.
    pub root: std::path::PathBuf,
    /// How many files the reading was filed under.
    pub files: usize,
    /// Declarations placed in a file.
    pub declared: usize,
    /// Tokens in the unit's stream.
    pub tokens: usize,
    /// Files the stream carries a token from — not `files`, because a file whose whole body is inside a branch
    /// nobody takes contributes nothing, and counting it as read is the mistake §7 records.
    pub files_with_tokens: usize,
    /// Files the walk reached that this session had no text for.
    pub missing: usize,
    /// Facts and errors that could not be placed in any file.
    pub unplaced: usize,
    /// Files whose own text does not balance its braces — see [`RenderedUnit::unbalanced`]. Named rather than
    /// counted: the file is the thing to look at.
    ///
    /// Being named here is **not** a claim that the file was skipped. Its declarations are in the index, filed at the
    /// scope its own text gives them; what the name says is that a brace of it could not be paired inside it, and
    /// that the pairing it got instead had to be given up.
    pub unbalanced: Vec<std::path::PathBuf>,
    /// The stream's own brace balance — see [`RenderedUnit::braces`].
    pub braces: i64,
    /// Errors the parse of the **program** reported, wherever they are — each is also filed against its own file.
    pub errors: usize,
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
