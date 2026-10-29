//! **The questions one file can be asked, with the answers remembered for as long as the request lasts.**
//!
//! A [`SemanticModel`] is a **borrowed view of one file plus the ability to look at another** — the index as it
//! stands, this file's tree and scopes, and a way to obtain the tree of a file this one's declarations point into.
//! It answers single-file semantic questions and caches every answer for its own lifetime.
//!
//! ```text
//!   one request  ─┬─  FileView      one file parsed          (already one per request)
//!                 └─  SemanticModel that view + the index + a resolver + a memo
//! ```
//!
//! # Why it exists, and the two numbers that made the case
//!
//! Four call sites asked [`type_of_expression`](crate::index::project::type_of_expression) and every one of them
//! passed the same resolver:
//!
//! ```text
//!   type_of_expression(index, &mut |_: &Path| None, scopes, root, path, …)
//! ```
//!
//! A resolver that always answers `None` is **the accurate spelling of "there is no session here"**: the inference
//! engine needs the tree of a *different* file as soon as a declaration it is reading points into a header, and it
//! gives up when it cannot have one. Measured on MSVC's `<vector>`, from the diagnostics channel:
//!
//! ```text
//!   1491 calls to type_of_expression, each 47–54 ms, for 3537 ms of one request
//! ```
//!
//! — and the overwhelming majority of those calls were asking about an initialiser whose type lives in another
//! file, which is exactly the question the closure had already refused. The model is the place where the tree of
//! *that* file can be obtained, once, and remembered for the rest of the request.
//!
//! # Why the memo needs no invalidation rule
//!
//! Because the model **is the request**. It borrows a view that was built for this call and it dies with it, so
//! there is nothing to invalidate and no staleness to reason about — the property this crate has paid for three
//! times is the one it must not have to reason about again (see `docs/status.md` §2). A long-lived cache of the
//! same answers would need a key, an eviction rule and a story about what happens when a file moves underneath it;
//! this needs none of the three.
//!
//! # What it is not
//!
//! **Not a second index.** Questions about the *project* — which file declares a name, what a symbol's references
//! are — stay with [`ProjectIndex`], which is the one implementation of them. The model's reason to exist is narrow
//! and hard: it is the only thing that holds both this file's tree **and** a way to get another's.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cpp_parser::CppSyntaxNode;

use crate::index::project::{self, MemberList, ProjectIndex};
use crate::sema::types::Type;
use crate::{Known, ScopeTree};

/// **A file, the index, and a way to read another file** — see the module documentation.
pub struct SemanticModel<'a> {
    index: &'a ProjectIndex,
    root: &'a CppSyntaxNode,
    scopes: &'a ScopeTree,
    source: &'a str,
    path: &'a Path,
    /// How to obtain a file's tree, when a declaration points into one. The caller owns what this *means*: a
    /// session hands over a closure that parses and holds; a test hands over one backed by its fixtures.
    ///
    /// Boxed and owned rather than borrowed, because the caller constructs it on the spot — `&mut resolver` for a
    /// local does not outlive the call that made it — and boxed rather than generic so that `SemanticModel` has one
    /// type and can appear in a signature without naming a closure.
    resolver: RefCell<Box<dyn FnMut(&Path) -> Option<crate::FileView> + 'a>>,
    /// What this model has already been asked, by node. `RefCell` because the queries are `&self` — a caller with a
    /// shared model is the ordinary case, and a check that had to hold `&mut` would not be able to ask two questions
    /// while iterating the file.
    types: crate::index::project::TypeMemo,
    /// **What `members_of` was asked, and what it answered** — a class's members do not change during one file's checks, and
    /// the same class is asked once per member access. See `members_of` for the measurement that made this necessary.
    members: RefCell<BTreeMap<String, Known<MemberList>>>,
    /// How many times the inference engine actually ran — the number that says whether the memo is working.
    inferred: std::cell::Cell<usize>,
    /// How many times a cached answer was handed back.
    reused: std::cell::Cell<usize>,
}

impl<'a> SemanticModel<'a> {
    /// **The public constructor** — cheap, because it borrows the data and owns only the resolver and an empty
    /// memo.
    ///
    /// `resolver` is what makes this a *model* rather than a view: it is how the questions that reach into another
    /// file are answered. A caller with no way to read another file passes `Box::new(|_| None)`, and gets exactly
    /// the behaviour the four `type_of_expression` call sites had before this type existed.
    pub fn new(
        index: &'a ProjectIndex,
        root: &'a CppSyntaxNode,
        scopes: &'a ScopeTree,
        source: &'a str,
        path: &'a Path,
        resolver: Box<dyn FnMut(&Path) -> Option<crate::FileView> + 'a>,
    ) -> SemanticModel<'a> {
        SemanticModel {
            index,
            root,
            scopes,
            source,
            path,
            resolver: RefCell::new(resolver),
            types: crate::index::project::TypeMemo::default(),
            members: RefCell::new(BTreeMap::new()),
            inferred: std::cell::Cell::new(0),
            reused: std::cell::Cell::new(0),
        }
    }

    /// **The type of an expression, remembered per node** — the query the checks and completion both ask.
    ///
    /// Cached by the node's **range**, not by its text: two occurrences of one spelling are two different types
    /// when they stand in different scopes, so a text key would answer about the wrong one. A range is unique
    /// within the file the model was built for, which is the only file it is asked about.
    ///
    /// **The memo is the engine's, not a cache in front of it.** This used to answer the top-level question from a
    /// map and pass the recursion through unfiltered — so `e.U.Start.Kind` re-deduced the `auto` declaration behind
    /// `e` eleven times, and a 2 800-line file was 382 questions that cost 5 923 ms. The map is now the same one
    /// [`project::type_of_expression_memoed`] consults for every sub-expression.
    pub fn type_of(&self, expression: &CppSyntaxNode) -> Known<(Type, PathBuf)> {
        // **A/B switch, kept**: `CPPLS_NO_INFERENCE=1` answers every question `Unknown`, which is the only way to
        // measure what the rules cost *without* the inference they do — the difference between this and the normal
        // run is the inference layer's share of a diagnostics pull, and it is what says whether the next work is in
        // the engine or in the walks. It is not a mode: the analysis it produces is deliberately wrong.
        if crate::probes::no_inference() {
            return Known::Unknown(crate::UnknownReason::UnknownType(Box::from(
                expression.text().to_string().trim(),
            )));
        }
        let range = cpp_parser::source_range(expression.text_range());
        let key = (range.start_offset as u32, range.end_offset() as u32);
        let asked_before = self.types.remembers(&key);
        if asked_before {
            self.reused.set(self.reused.get() + 1);
        } else {
            self.inferred.set(self.inferred.get() + 1);
        }

        let started = std::time::Instant::now();
        let answer = {
            // One borrow of the resolver for the whole inference, so a question that reaches into another file —
            // and the question *inside* that answer, recursively — all go through the same memo and the same
            // resolver. The `RefCell` is borrowed as a *field*, not as `self`, which is what keeps a second query
            // on this model possible while this one runs.
            let mut resolver = self.resolver.borrow_mut();
            project::type_of_expression_memoed(
                &self.types,
                self.index,
                &mut **resolver,
                self.scopes,
                self.root,
                self.path,
                expression,
                0,
            )
        };
        // **The slow questions, by name.** A pull answers about 150 of them (measured: "153 question(s) in 169 ms"),
        // and a total says nothing about which of them is worth the next hour — this does.
        if crate::probes::timing() {
            let took = started.elapsed();
            if took.as_micros() >= 500 {
                let text: String = expression.text().to_string().chars().take(70).collect();
                eprintln!(
                    "[question] {:>5.1} ms  {}",
                    took.as_secs_f64() * 1000.0,
                    text.split_whitespace().collect::<Vec<_>>().join(" ")
                );
            }
        }
        answer
    }

    /// **The members of a class, inherited ones included** — see [`project::members_of`].
    ///
    /// The substitution needs no file: the class's parameter names are in its **fact**, so the model's resolver —
    /// which hands out *renderings* — is not asked for anything, and the answer is the same one `Session::members_of`
    /// gives (`docs/status.md` §十五 ④: it was not, because this call passed the rendering to a function reading the
    /// file's own text at the fact's offsets).
    pub fn members_of(&self, class: &str) -> Known<MemberList> {
        // **Memoised, because the same class is asked dozens of times per file.** Measured on one header of a real project:
        // `std::vector<heap_entry>` was asked **64 times** and each answer cost about **589 ms** — 37 seconds of a file that
        // took 45, and the reason five different rules all looked slow. The memo is the same shape as the `types` one beside
        // it: a `RefCell` keyed by the question, because the queries are `&self` and a shared model is the ordinary case.
        if let Some(answer) = self.members.borrow().get(class) {
            return answer.clone();
        }
        let answer = self.members_of_uncached(class);
        self.members
            .borrow_mut()
            .insert(class.to_string(), answer.clone());
        answer
    }

    fn members_of_uncached(&self, class: &str) -> Known<MemberList> {
        let listed = project::members_of(self.index, self.scopes, self.root, self.path, class);
        // **The class's arguments, written through its members** — see [`project::substituted_members`].
        project::substituted_members(self.index, self.path, class, listed)
    }

    /// **The class a `->` reaches, and whether a pointer was followed to get there.**
    ///
    /// One implementation of the rule — a pointer's pointee, a class's own `operator->`, `element_type` — borrowed
    /// from [`crate::completion::the_class_after_the_operator`], which `ptr->` completion has used since the smart
    /// pointers landed. `undefined-field` asks it for an arrow, which it used to skip outright: "an arrow is not
    /// evidence" was the right answer while nothing here could follow one.
    pub fn the_class_after_an_arrow(
        &self,
        written: &crate::sema::types::Type,
    ) -> Option<(String, bool)> {
        crate::completion::the_class_after_the_operator(
            self.index,
            self.scopes,
            self.root,
            self.path,
            written,
            true,
        )
    }

    /// **The members written in the class itself** — no base walk, no substitution.
    ///
    /// [`SemanticModel::members_of`] is the reader's question (what can be written after `.`), and it walks the base
    /// chain: measured on `test_module`'s `import std;` file, `members_of("std::vector")` is **ten bases and 583 ms**
    /// of walking for a class whose own list is empty. A check that only needs the class's own constructors —
    /// `assign-type-check` — was paying that on every pull. See [`project::own_members_of`].
    pub fn own_members_of(&self, class: &str) -> Known<Vec<(std::path::PathBuf, crate::DeclFact)>> {
        project::own_members_of(self.index, self.scopes, self.root, self.path, class)
    }

    /// **Does `name` name a template parameter *where it is used*** — the `C` of `template <class C>`?
    ///
    /// The question a consumer must ask *before* treating a name as a class, and the reason it is not answered by
    /// [`project::members_of`] is worth stating: that function is asked about a **spelling**, and by the time it has
    /// one there is nothing left in it that says whether the spelling named a class or a parameter. A template
    /// parameter is recorded in the index as `DeclKind::Type` — the same kind a class gets, because `DeclFact` does
    /// not carry `BindingKind` — so the index cannot tell them apart either.
    ///
    /// The **scope tree** can, and this is the question asked of it: a parameter is bound in a
    /// `ScopeKind::TemplateParameters` scope which carries no name of its own (`sema::scopes` creates it with `None`,
    /// because a parameter declares nothing a `::` name could reach), so a walk of the chain outward from the use is
    /// what finds it.
    ///
    /// # Why it is asked at an offset rather than of the file
    ///
    /// Measured, and it is the whole reason for the parameter. Asking "is `C` a parameter anywhere in this file"
    /// answered **yes** for
    ///
    /// ```cpp
    /// template <class C> void f() { C x; x.m; }   // a parameter here
    /// struct C { int n; };
    /// void g() { C x; x.m; }                      // …and a class here, which must still be reported
    /// ```
    ///
    /// so the class's `x.m` went unreported: a suppression that silences correct code one scope away from where it
    /// was meant to apply. A name is looked up from where it is written, so that is where the question starts.
    pub fn names_a_template_parameter(&self, name: &str, at: usize) -> bool {
        if name.is_empty() {
            return false;
        }

        let Some(innermost) = self.scopes.scope_at(at) else {
            return false;
        };

        self.scopes.scope_chain(innermost).iter().any(|id| {
            self.scopes.scope(*id).is_some_and(|scope| {
                scope.kind == crate::sema::symbol::ScopeKind::TemplateParameters
                    && scope
                        .bindings
                        .iter()
                        .any(|binding| binding.name.text() == name)
            })
        })
    }

    /// What [`SemanticModel::type_of`] has cost so far: `(inferences run, answers reused)`.
    ///
    /// The two numbers that say whether a memo is working, and the reason this type has a counter at all: an
    /// instrument that cannot tell "cheap because cached" from "cheap because there was nothing to do" is the
    /// mistake this crate has already made once (see `docs/status.md` §6.1).
    pub fn inference_stats(&self) -> (usize, usize) {
        (self.inferred.get(), self.reused.get())
    }

    /// **What the engine has cost this model so far**: `(questions answered, nanoseconds)`.
    ///
    /// The split a diagnostics pull needs — the rules' own walks against the inference they ask for. Measured on
    /// `LuaSyntaxTree.cpp`: `CPPLS_NO_INFERENCE=1` (every answer `Unknown`) takes the whole check layer from
    /// **695 ms to 25 ms**, so this is the number that says whether the next work is in the engine.
    pub fn inference_cost(&self) -> (u64, u64) {
        self.types.spent()
    }

    /// The file this model is about.
    pub fn path(&self) -> &Path {
        self.path
    }

    /// The text this model is about.
    pub fn source(&self) -> &str {
        self.source
    }

    /// The tree this model is about.
    pub fn tree(&self) -> &CppSyntaxNode {
        self.root
    }

    /// The index behind this model.
    pub fn index(&self) -> &ProjectIndex {
        self.index
    }

    /// The scopes of the file this model is about.
    pub fn scopes(&self) -> &ScopeTree {
        self.scopes
    }
}
