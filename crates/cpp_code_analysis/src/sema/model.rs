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

/// One node's identity: its range in the file the model was built for.
///
/// A range rather than the node itself, because a node is a cheap handle into the tree and *not* a stable key —
/// two handles to one node are not `Eq` in any useful sense — while a range is. It is unique within one file
/// because the tree is a tree: two nodes cannot occupy the same span.
type NodeKey = (u32, u32);

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
    types: RefCell<BTreeMap<NodeKey, Known<(Type, PathBuf)>>>,
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
            resolver: RefCell::new(resolver),            types: RefCell::new(BTreeMap::new()),
            inferred: std::cell::Cell::new(0),
            reused: std::cell::Cell::new(0),
        }
    }

    /// **The type of an expression, remembered per node** — the query the checks and completion both ask.
    ///
    /// Cached by the node's **range**, not by its text: two occurrences of one spelling are two different types
    /// when they stand in different scopes, so a text key would answer about the wrong one. A range is unique
    /// within the file the model was built for, which is the only file it is asked about.
    pub fn type_of(&self, expression: &CppSyntaxNode) -> Known<(Type, PathBuf)> {
        let range = cpp_parser::source_range(expression.text_range());
        let key = (range.start_offset as u32, range.end_offset() as u32);

        if let Some(known) = self.types.borrow().get(&key) {
            self.reused.set(self.reused.get() + 1);
            return known.clone();
        }

        self.inferred.set(self.inferred.get() + 1);
        let answer = {
            // One borrow of the resolver for the whole inference, so a question that reaches into another file —
            // and the question *inside* that answer, recursively — all go through the same memo and the same
            // resolver. The `RefCell` is borrowed as a *field*, not as `self`, which is what keeps a second query
            // on this model possible while this one runs.
            let mut resolver = self.resolver.borrow_mut();
            project::type_of_expression(
                self.index,
                &mut **resolver,
                self.scopes,
                self.root,
                self.path,
                expression,
                0,
            )
        };

        self.types.borrow_mut().insert(key, answer.clone());
        answer
    }

    /// **The members of a class, inherited ones included** — see [`project::members_of`].
    pub fn members_of(&self, class: &str) -> Known<MemberList> {
        project::members_of(self.index, self.scopes, self.root, self.path, class)
    }

    /// What [`SemanticModel::type_of`] has cost so far: `(inferences run, answers reused)`.
    ///
    /// The two numbers that say whether a memo is working, and the reason this type has a counter at all: an
    /// instrument that cannot tell "cheap because cached" from "cheap because there was nothing to do" is the
    /// mistake this crate has already made once (see `docs/status.md` §6.1).
    pub fn inference_stats(&self) -> (usize, usize) {
        (self.inferred.get(), self.reused.get())
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
