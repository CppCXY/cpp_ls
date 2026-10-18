//! **Semantic checks**: what the analysis can say about a file that is *wrong*, as opposed to what it read.
//!
//! [`crate::FileDiagnostics::errors`] is the parser's answer — the text does not parse — and
//! [`crate::FileDiagnostics::notes`] is what the analysis knows but is not a complaint. This is the third thing a
//! diagnostics channel needs: a construct the grammar accepts and the language does not.
//!
//! # The shape of a check
//!
//! One rule, one file, and one `impl`:
//!
//! ```ignore
//! pub struct Unused;
//!
//! impl Check for Unused {
//!     const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::Unused];
//!
//!     fn check(context: &mut CheckContext<'_>) {
//!         // … and `context.add(code, range, name, message)` is how a finding is reported
//!     }
//! }
//! ```
//!
//! The file's name is the snake-case spelling of the code it reports — `unused.rs` reports `unused`,
//! `param_type_check.rs` reports `param-type-check` — so `grep unused` finds the rule, its tests and its
//! documentation in one pass. [`DiagnosticCode`] is the enum those codes come from, and its `name` is the only
//! place the kebab spelling is written.
//!
//! The shape follows `emmylua_code_analysis`'s `check/checker`: a [`Check`] trait carrying the codes it may report,
//! a [`CheckContext`] that collects findings and filters them through a [`CheckConfig`], and a `run` that is a flat
//! list of typed calls.
//!
//! # A decision comes from the tree; text is for messages
//!
//! The crate has a **lossless tree** and a **lexer**, and every question a check asks has a node or a token for its
//! answer: an access label is a `PublicAccess` node beside a `ClassBody`, a `class` is a `ClassKeyword` token on a
//! `ClassDef`, a declarator is a `Declarator` node, a condition is a `ParenExpr` node. Reaching past those for the
//! text is how this layer produced its first user-visible false positive: `assignment_in_condition` looked for the
//! first `(` and its match **in the text** of an `if`, counted a `<` in `i + 1 < argc` as an opening bracket, ran on
//! into the body, found an `=` in the first statement there, and reported *`=` in a condition is almost always a
//! `==`* on a line whose condition is two comparisons.
//!
//! So:
//!
//! ```text
//!   to decide    walk nodes (`children`, `descendants`) and tokens (`descendants_with_tokens`)
//!   to say       text: the message, and `source[range]` to quote what the reader wrote
//! ```
//!
//! A text comparison is allowed where the token **is** the answer - a keyword token compared against `"class"` has
//! the lexer's boundary around it, which is the whole difference from a substring search. Where a check still reads
//! text, the reason belongs in a comment beside it, and the honest reasons are narrow: a question about *layout*
//! (a directive's own line), or a conservative approximation of a control-flow question that a tree walk over one
//! file cannot answer.
//! # Why `run` is a list of `run_check::<T>` and not a list of closures
//!
//! It was the other shape — `timed(0, &|| …)`, `timed(1, &|| …)` — and the index was the defect: a check's name
//! lived in a second table (`crate::stages`), keyed by the same numbers, so adding a rule meant editing two lists
//! and keeping them in step by hand, with nothing checking that they were. The type carries the name now, the
//! table is gone, and a check that is not in `run` does not exist rather than silently never running.
//!
//! # No timer
//!
//! **The checks are not timed.** They were — a guard per check, with a stage name — and the instrumentation was
//! removed because it does not fit what it measures: diagnostics run on **every keystroke**, so a timer here is a
//! lock, an allocation and a line of output per check per keystroke, paid by every user to answer a question one
//! developer asks once. When the cost of this layer needs measuring, the measurement is a probe
//! (`examples/checks_timing.rs`), which can afford to be intrusive because it runs when somebody asks it to.
//!
//! # The contract: `Known::Yes` or `Known::No`, and nothing else
//!
//! **A check reports only what it knows.** [`Known`](crate::Known) has three values and this layer has two:
//!
//! ```text
//! Known::Yes(_)   the question was answered, and the answer is a problem      -> report
//! Known::No       the question was asked and the answer is a definite no      -> report, for a "not found"
//! Known::Unknown  the question was not answered, for a stated reason          -> report NOTHING
//! ```
//!
//! The third is not a gap in this layer, it is the design of the layer underneath it.
//! [`sema::resolve`](crate::sema::resolve) keeps the two "no"s apart on purpose: `Known::No` means a name is
//! **nowhere**, and [`UnknownReason::NotDeclaredHere`](crate::UnknownReason::NotDeclaredHere) means it is
//! *somewhere else* — usually a header the analysis has not read. The first is a fact about the file; the second
//! is a fact about us.
//!
//! # Why a check that reports nothing is the ordinary case, and why that is the point
//!
//! The failure mode this layer has to avoid is not missing a problem, it is **inventing one**, because an editor
//! that underlines correct code is one the user turns off — and then it reports nothing at all, forever. The
//! measurement that keeps it honest is a corpus that is *known good*: the standard library. A check that reports
//! anything at all on a header it read correctly is wrong, and that is a test that can fail.

use std::path::Path;

use cpp_parser::SourceRange;

use crate::file::view::FileView;
use crate::{FileSummary, ProjectIndex};

pub mod access_invisible;
pub mod argument_count;
pub mod assign_type_check;
pub mod assignment_in_condition;
// pub mod assign_type_check;
pub mod config;
pub mod diagnostic_code;
// **Being ported to the Check trait**: git mv'd to their new names, still written against the old
// Checks/Finding { check: … } shape, so they are declared again the moment their bodies are rewritten. Kept in
// the tree rather than deleted, because their reasoning (and the measured 119-findings defect they record) is what
// the ported versions have to preserve.
pub mod include_not_found;
pub mod macro_redefined;
pub mod missing_return;
pub mod param_type_check;
pub mod return_type_mismatch;
pub mod syntax_error;
pub mod undefined_field;
pub mod unnecessary_branch;
pub mod unused;
mod braces_around_statements;
mod use_nullptr;
mod use_override;
mod simplify_boolean_expr;
mod else_after_return;
mod cstyle_cast;

pub use config::CheckConfig;
pub use diagnostic_code::{DiagnosticCode, RuleSeverity, RuleTag};

/// **What a check found**, in the form a diagnostics channel shows it.
///
/// A `Finding` is a *claim about a place in a file*, so it carries the span, the code and the sentence together: a
/// consumer that had to assemble the message from the parts would be a second author of it, and the two would
/// drift. The check that knows why it fired is the one that says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The span to underline — the whole construct, not just its first token.
    pub range: SourceRange,
    /// The name the finding is about, as the file spells it. Empty when the finding is not about one name.
    pub name: String,
    /// **Which rule reported it** — the enum, not a string: see [`DiagnosticCode`].
    pub code: DiagnosticCode,
    /// The sentence shown to the user. Written by the check, because the check is what knows.
    pub message: String,
}

/// **A check**: one rule, and the codes it may report.
///
/// `CODES` is what makes a whole check skippable: [`run_check`] does not call one whose every code is off, which is
/// the only skip that saves the walk rather than the message.
pub trait Check {
    /// The codes this check can report — see [`CheckConfig::is_enabled`].
    const CODES: &'static [DiagnosticCode];

    /// **The rule.** Report through [`CheckContext::add`], which is also where a disabled code is dropped.
    fn check(context: &mut CheckContext<'_>);
}

/// **Run one check, unless every code it reports is turned off.**
///
/// The skip is here rather than inside each check because the expensive part of a check is the walk over the tree,
/// not the sentence it produces: a rule the user turned off should cost nothing at all.
pub fn run_check<T: Check>(context: &mut CheckContext<'_>) {
    if T::CODES.iter().any(|code| context.config.is_enabled(*code)) {
        T::check(context);
    }
}

/// **Everything a check is given, and where it puts what it found.**
///
/// Deliberately *not* a [`Session`](crate::Session). A check that could ask the session anything would eventually
/// ask it to parse something, and a diagnostics channel whose cost is "one parse per check per keystroke" is a
/// channel that gets removed. What is here is a view of **one file** and the index as it stands.
pub struct CheckContext<'a> {
    /// The file being checked, as the index spells it — the key to every answer below.
    pub path: &'a Path,
    /// The file's summary: the facts its own text produced. A check that needs only this is a check that cannot be
    /// wrong about another file.
    pub summary: &'a FileSummary,
    /// **The file's questions, and the only way a check asks them.**
    pub model: crate::sema::model::SemanticModel<'a>,
    /// **The text a range from the *tree* is an offset into** — the view's own reading, which is the **rendering**:
    /// macros expanded, directives resolved, headers inlined where their `#include` was.
    ///
    /// Settled by reading [`Session::view`](crate::Session::view) rather than by experiment, after a test of mine
    /// concluded the opposite from a fixture whose include had silently failed: its first line is *"The rendering, or
    /// a request for it — **never the file's own tokens**"*. A rendering is a unit walk, a preprocess and a render —
    /// 65–190 ms for a standard-library header, and megabytes of text — which is why it is built on request and why
    /// [`CheckContext::own_syntax_nodes`] prunes what it produces.
    ///
    /// So this text is **not** the file the reader is looking at; that one is [`CheckContext::written`].
    pub source: &'a str,
    /// **The text a range from the *summary* is an offset into** — the file as the reader wrote it.
    ///
    /// Two texts, because this layer reads ranges from two places and they are in two coordinate systems:
    ///
    /// ```text
    ///   a range from `context.tree()`     -> context.source    the rendering
    ///   a range from `context.summary`    -> context.written   the file
    /// ```
    ///
    /// Measured, and it is what this field was added for: `macro_redefined` compared two `#define` bodies through
    /// the *rendering* while the ranges came from the summary — so it read a fragment of the expanded function
    /// against nothing at all, and reported a macro as a redefinition of itself.
    ///
    /// What a finding is *published in* is the file's coordinates, because the server places a range with the VFS
    /// line index. That crossing belongs where a finding is added — see [`CheckContext::add`].
    pub written: &'a str,
    /// **The parse errors of the reading this run is about**, which is what the `syntax-error` check publishes.
    ///
    /// Passed in rather than asked for: the errors belong to the **tree**, and this context deliberately holds a
    /// *model* over it — a check that could reach the tree would be a check that could re-parse.
    pub errors: &'a [cpp_parser::CppParseError],
    /// **The view this run is about**, which is what turns a range from the tree into a range in the file.
    ///
    /// Held for that one crossing and nothing else: a check that reached through it could parse, which is the
    /// thing [`CheckContext`]'s own documentation says this layer must not be able to do.
    pub view: &'a FileView,
    /// Which codes may be reported — see [`CheckConfig`].
    pub config: &'a CheckConfig,
    findings: Vec<Finding>,
}

impl<'a> CheckContext<'a> {
    /// A context for one file — the session builds these; a test builds one to ask about a fixture.
    pub fn new(
        path: &'a Path,
        summary: &'a FileSummary,
        model: crate::sema::model::SemanticModel<'a>,
        source: &'a str,
        written: &'a str,
        errors: &'a [cpp_parser::CppParseError],
        view: &'a FileView,
        config: &'a CheckConfig,
    ) -> Self {
        CheckContext {
            path,
            summary,
            model,
            source,
            written,
            errors,
            config,
            view,
            findings: Vec::new(),
        }
    }

    /// **Report a finding whose range is in the *tree*.**
    ///
    /// The ordinary entry point, because most checks read the tree — and the range they have is therefore in the
    /// **rendering**, which is not the text a client is showing. The crossing happens here: see
    /// [`CheckContext::in_the_file`], and use [`CheckContext::add_in_the_file`] for a finding whose range already
    /// came from the summary.
    ///
    /// A check says what it found and does not ask whether anyone wants to hear it: the filter belongs here because
    /// there are eleven checks and the twelfth would forget. This is also where a per-line suppression will go (a
    /// comment naming a code on the line the finding is on), for the same reason — one place, so that no check has
    /// to know it was suppressed.
    pub fn add(
        &mut self,
        code: DiagnosticCode,
        range: SourceRange,
        name: impl Into<String>,
        message: impl Into<String>,
    ) {
        let in_the_reading = range;
        let range = self.in_the_file(range);
        self.push(code, range, Some(in_the_reading), name, message);
    }

    /// **Report a finding whose range is already in the file** — for the checks that read the *summary*, whose
    /// every range is a file offset (see [`CheckContext::written`]).
    ///
    /// A second method rather than a flag, because the difference is which text the range came from and a check
    /// that got it wrong would put a squiggle on another line rather than fail to compile.
    pub fn add_in_the_file(
        &mut self,
        code: DiagnosticCode,
        range: SourceRange,
        name: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.push(code, range, None, name, message);
    }

    /// **A range read from the tree, said in the file's coordinates.**
    ///
    /// The outbound half of the crossing this crate makes everywhere else: a range in the rendering is an offset
    /// into text the cook produced — macros expanded, a header inlined, a directive resolved away — and a client is
    /// showing the **file**. `file_offset_of` maps both ends, and a token a macro produced reports *where the macro
    /// was invoked*, which is the place the reader can see.
    ///
    /// An end that cannot be placed keeps the start, so a half-mappable range is a short one rather than a lost
    /// finding; a **start** that cannot be placed leaves the range alone, which is the only honest thing left to do
    /// and is also what the checks did before this crossing existed. Measured before it did: on a file with a
    /// header inlined, `unused`'s finding for a local after the `#include` was published **on the line the header
    /// occupied** — the same offset in a shorter text.
    fn in_the_file(&self, range: SourceRange) -> SourceRange {
        let Some(start) = self
            .view
            .file_offset_of(crate::file::coordinates::ReadingOffset::new(
                range.start_offset,
            ))
        else {
            return range;
        };
        // **The end is a different question from the start** - see `FileView::file_offset_of_end`: asking
        // `file_offset_of` about an end that falls in whitespace walks forward to the next token, which is how a
        // finding about the value `123` came out covering the rest of the line. Measured on the user's own code:
        // `std::string x = 123;` underlined from `123` to the end of the statement.
        let end = self
            .view
            .file_offset_of_end(crate::file::coordinates::ReadingOffset::new(
                range.end_offset(),
            ))
            .map_or(start, |end| end);
        SourceRange::new(start.get(), end.get().saturating_sub(start.get()))
    }

    /// The one place a finding becomes a [`Finding`] — see [`CheckContext::add`].
    fn push(
        &mut self,
        code: DiagnosticCode,
        range: SourceRange,
        in_the_reading: Option<SourceRange>,
        name: impl Into<String>,
        message: impl Into<String>,
    ) {
        if !self.config.is_enabled(code) {
            return;
        }

        // **A finding has to be about this file's own text.** A rendering has a header inlined where its `#include`
        // was, so a node a check finds may belong to **another file** — and its range, mapped back through the span
        // table, lands on the `#include` line. That is the coordinate defect a reader sees as a diagnostic drawn
        // over the wrong line, and the cook already answers the question per token: `RenderedSpan::written` is `None`
        // exactly where the text came from somewhere else. Asking it here is the same lesson as the AST layer — the
        // machinery existed, and this layer was re-deriving a worse version of it by comparing texts.
        if let Some(in_the_reading) = in_the_reading {
            if !self.view.was_written_here(in_the_reading) {
                return;
            }
        }
        self.findings.push(Finding {
            range,
            name: name.into(),
            code,
            message: message.into(),
        });
    }

    /// **The file's tree** — through the model, which is the only place it lives.
    pub fn tree(&self) -> &cpp_parser::CppSyntaxNode {
        self.model.tree()
    }

    /// **The scopes of the file's own text**, which is what turns a name in an expression into a declaration.
    pub fn scopes(&self) -> &crate::ScopeTree {
        self.model.scopes()
    }

    /// The index as it stands. Answers about *other* files are answers about whatever was indexed — which is why a
    /// check may only report a definite `Known::No`, and never an absence of an answer.
    pub fn index(&self) -> &ProjectIndex {
        self.model.index()
    }

    /// **The typed root of this file's tree** — where every check should start.
    ///
    /// The crate has a typed AST (`CppTranslationUnit`, `CppIfStat`, `CppClassDef`, `CppParamList`, …) and this
    /// layer was written without it: every check compared `CppSyntaxKind`s and re-implemented, by hand, the
    /// accessors the typed layer already has. The cost is not only verbosity - a hand-written kind comparison is a
    /// decision made by the check about a shape the parser owns, and when the shape moves the check goes quiet
    /// (measured twice: `int f() { }` is not a `FunctionDef`, and `struct` is a `StructDef` rather than a
    /// `ClassDef`) instead of failing to compile.
    ///
    /// The cast is cheap - the rowan node is handed back, not rebuilt - and `None` only for a tree that is not a
    /// translation unit, which is a state this analysis cannot be handed.
    pub fn root(&self) -> Option<cpp_parser::CppTranslationUnit> {
        // The trait has to be in scope for `cast`, which is where the typed layer puts it.
        use cpp_parser::CppAstNode;
        cpp_parser::CppTranslationUnit::cast(self.tree().clone())
    }
    /// **The type of an expression** — the model's, remembered per node for as long as this request lasts.
    pub fn type_of(
        &self,
        expression: &cpp_parser::CppSyntaxNode,
    ) -> crate::Known<(crate::sema::types::Type, std::path::PathBuf)> {
        self.model.type_of(expression)
    }

    /// **Every node of this file's own text** — the one traversal a check should walk.
    ///
    /// It exists because `self.tree().descendants()`, which is what every check in this layer used, is the wrong
    /// question twice over:
    ///
    /// ```text
    ///   the scope     the tree is a **rendering** — see `CheckContext::source` — so a full walk visits every node
    ///                 of every inlined header, and a node from another file is not a fact about this one. In a
    ///                 rendering the inlined text arrives as **top-level children** of the translation unit, and the
    ///                 cook marks exactly that per token (`RenderedSpan::written`), so pruning the children that were
    ///                 not written here removes a whole header for the cost of one question per child
    ///   the shape     a walk that compares `CppSyntaxKind` by hand is this layer guessing at a shape the parser owns
    ///                 — see `CheckContext::own`, which asks the typed layer instead
    /// ```
    ///
    /// Both are answered by the same observation: in a rendering, the text of an inlined header is spliced in where
    /// the directive was, so it arrives as **top-level children** of the translation unit. The cook marks exactly
    /// that per token (`RenderedSpan::written`), so pruning the children that were not written here removes the whole
    /// header at the cost of one question per child — and everything below a kept child is this file's own text.
    ///
    /// (A child that is only *partly* this file's — a declaration whose first tokens come from a macro defined
    /// elsewhere — is pruned whole, which is the safe direction: not reporting is a missed finding, reporting on
    /// another file's text is a wrong one.)
    /// **The top-level pieces of this file's own text** — what the two iterators below are built on.
    ///
    /// Kept separate because a token walk has to start here and not from every node: taking
    /// `descendants_with_tokens()` of each node of a tree yields every token once per ancestor, which inflated
    /// `unused`'s spelling counts until the rule never fired — measured, two of its tests went red.
    fn own_top_level_nodes(&self) -> Vec<cpp_parser::CppSyntaxNode> {
        let root = self.tree().clone();
        let mut kept = Vec::new();
        for child in root.children() {
            let range = cpp_parser::source_range(child.text_range());
            if self.view.was_written_here(range) {
                kept.push(child);
            }
        }
        kept
    }

    /// **Every node of this file's own text**, once each, in document order.
    /// **Every node of this file's own text**, once each, in document order.
    ///
    /// # The `once(child)` that made every declaration count twice
    ///
    /// This used to be `once(child.clone()).chain(child.descendants())`, and `descendants()` **already includes the
    /// node it is called on** — so every top-level child was yielded twice. Measured on `int unfinished() { }`, which
    /// holds one declaration: `own_syntax_nodes()` counted **9** where `descendants()` is 8, and the function
    /// definition list held the same `Declaration 0..20` at index 0 *and* index 1.
    ///
    /// What it cost depends on the check, which is why it went unnoticed. A check that filters for a **kind below** a
    /// declaration — every statement walk, every expression walk — never saw it, because a `Declaration` is not the
    /// thing it asks for. The two that ask about *declarations themselves* reported every finding **twice**:
    /// measured, `missing-return` gave two identical findings for `int unfinished() { }`. A duplicate that only
    /// surfaces on the one node type nothing else looks at.
    pub fn own_syntax_nodes(&self) -> impl Iterator<Item = cpp_parser::CppSyntaxNode> + '_ {
        self.own_top_level_nodes()
            .into_iter()
            .flat_map(|child| child.descendants())
    }

    /// **Every node of this file's own text, of one type** — what a check actually asks for.
    ///
    /// ```ignore
    ///   for statement in context.own::<CppIfStat>() { … }         // not: filter(|node| CppSyntaxKind::from(…))
    /// ```
    ///
    /// The typed layer is what makes this possible: `CppAstNode::cast` is the parser's own answer to "is this node
    /// that", and a check that asks it cannot be wrong about a shape the parser owns. The hand-written kind
    /// comparisons this replaces went quiet twice — a definition is not a `FunctionDef`, and `struct` is a
    /// `StructDef` — while compiling and looking correct.
    pub fn own<T: cpp_parser::CppAstNode + 'static>(&self) -> impl Iterator<Item = T> + '_ {
        self.own_syntax_nodes().filter_map(T::cast)
    }

    /// **Every member access in this file's own text** — `a.b`, `a->b`, by the definition the sema layer owns.
    ///
    /// The definition is [`crate::sema::resolve::member_access_of`] and not a node kind, because the two disagree
    /// and the sema layer is the one that is right: measured with `cpp_dump`, `w.secret` parses as
    ///
    /// ```text
    ///   Syntax(IndexExpr)
    ///     Syntax(IdentifierExpr) "w"    Token(Dot) "."    Token(Identifier) "secret"
    /// ```
    ///
    /// — an **`IndexExpr`**, exactly as `arr[0]` is, which is what `member_access_of` says in its own documentation
    /// (*"`arr[0]` is the same node kind with brackets instead of a dot"*). The typed layer's `CppMemberExpr` accepts
    /// `MemberExpr | ArrowExpr`, kinds this grammar does not appear to produce for a dot, so a check filtering on it
    /// finds **nothing at all** — measured: `access_invisible` counted 38 own nodes and **0** member expressions, and
    /// `undefined-field` has been blind to `.` for as long as it has existed.
    ///
    /// The kinds are still used as a cheap filter before the shape is read, so the cost is one kind test per node
    /// rather than a child list per node — `CppIndexExpr`/`CppMemberExpr` would be two casts per node, and this is
    /// the hot path of four checks.
    pub fn member_accesses(
        &self,
    ) -> Vec<(
        cpp_parser::CppSyntaxNode,
        crate::sema::resolve::MemberAccess,
    )> {
        use cpp_parser::CppSyntaxKind;
        self.own_syntax_nodes()
            .filter(|node| {
                matches!(
                    // **The kind here is a filter over *shapes*, not a claim about what a node means** — the claim
                    // is `member_access_of`'s, and it reads the operator to make it. That distinction is what makes
                    // this use legitimate: `.` arrives as `IndexExpr`, which the typed layer would call an index.
                    CppSyntaxKind::from(node.kind()),
                    CppSyntaxKind::IndexExpr | CppSyntaxKind::MemberExpr | CppSyntaxKind::ArrowExpr
                )
            })
            .filter_map(|node| {
                crate::sema::resolve::member_access_of(&node).map(|access| (node, access))
            })
            .collect()
    }

    /// **Every token of this file's own text, once each** — for the checks that count spellings rather than read
    /// structure.
    ///
    /// Pruned the same way as [`CheckContext::own_syntax_nodes`] and for the same reason: a token inside an inlined
    /// header is another file's spelling, and counting it would make `unused` answer about a name in `<string>`.
    pub fn own_tokens(&self) -> impl Iterator<Item = cpp_parser::CppTokenData> + '_ {
        self.own_top_level_nodes()
            .into_iter()
            .flat_map(|child| child.descendants_with_tokens().collect::<Vec<_>>())
            .filter_map(|element| element.into_token())
            .map(|token| cpp_parser::CppTokenData {
                kind: token.kind().into(),
                range: cpp_parser::source_range(token.text_range()),
            })
    }

    /// Take the findings, sorted the way a list reads — see [`run`].
    fn into_findings(mut self) -> Vec<Finding> {
        // **Sorted by position**, because the order the checks happen to be listed in is an implementation detail
        // and a diagnostics list sorted by it is a list that reshuffles when a check is added. Two findings at the
        // same offset keep the order they were produced in, which is the order of `run` — deliberate, so that the
        // more specific check is written first and reads first.
        self.findings
            .sort_by_key(|finding| (finding.range.start_offset, finding.range.length));
        self.findings
    }
}

/// **Every function *definition* in this file** - a node with both a parameter list and a body.
///
/// Recognised by **shape** rather than by node kind, and that is not tidiness: measured, `int count() { }` at file
/// scope is **not** a `FunctionDef` in this grammar - the dump shows a `NameExpr`, a `ParameterList` and a
/// `CompoundStat` with no function node around them - so every check that filtered on that kind reported nothing,
/// on every file, while looking correct. A definition is what has a parameter list and a body; which node the
/// grammar wrapped them in is its business.

/// **The condition of an `if`/`while`/`switch`, taken from the node and never from its text.**
///
/// A node's child is what the parser decided the condition is. This was a text scan in two checks at once - the
/// first `(` and its match - and counting parentheses through text means deciding what a `<` is, which is how a
/// condition holding `i + 1 < argc` was read past its own `)` and into the body.
pub fn condition_of(statement: &cpp_parser::CppSyntaxNode) -> Option<cpp_parser::CppSyntaxNode> {
    // **The typed layer's node for a parenthesised condition**, rather than "the first child that is not the body":
    // `CppParenExpr` is the parser's own answer, and `a < b` inside it cannot be mistaken for a bracket.
    use cpp_parser::CppAstNode;
    cpp_parser::CppParenExpr::cast(statement.clone())
        .is_some()
        .then(|| statement.clone())
        .or_else(|| {
            statement
                .children()
                .find_map(|child| cpp_parser::CppParenExpr::cast(child).map(|p| p.syntax().clone()))
        })
}

/// **The one word a condition is, when it is a bare literal** — `true`, `false`.
///
/// Decided from the **tokens**, through the typed layer's own vocabulary: the parentheses and the whitespace are
/// dropped by kind, and what is left has to be exactly one token that is the keyword. A string literal is a
/// different kind, so `if ("true")` cannot be read as the keyword.
///
/// The kind comes out as `CppTokenKind::from(CppKind::from(token.kind()))`, and the shape of that expression is the
/// point: this crate's rowan kind is [`CppKind`], the wrapper around `Node(CppSyntaxKind)` and `Token(CppTokenKind)`,
/// so a token's kind is only usable once unwrapped. `examples/token_kinds.rs` prints both halves for any source,
/// which is how a round spent believing the wrong thing about this API was settled.
pub fn literal_condition_of(condition: &cpp_parser::CppSyntaxNode) -> Option<String> {
    use cpp_parser::{CppKind, CppTokenKind};

    let mut words = Vec::new();
    for token in condition
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        match CppTokenKind::from(CppKind::from(token.kind())) {
            CppTokenKind::LeftParen
            | CppTokenKind::RightParen
            | CppTokenKind::Whitespace
            | CppTokenKind::Newline => {}
            CppTokenKind::TrueKeyword => words.push("true".to_string()),
            CppTokenKind::FalseKeyword => words.push("false".to_string()),
            // Anything else — a name, a literal, an operator — means the condition is not a bare literal.
            _ => return None,
        }
    }

    match words.as_slice() {
        [word] => Some(word.clone()),
        _ => None,
    }
}
/// **Is this node a class-like *definition*?** — `class`, `struct` or `union`.
///
/// Delegated to the typed layer, which already answers exactly this: `CppClassDef::can_cast` accepts `ClassDef`,
/// `StructDef` **and** `UnionDef`, and it carries `get_name_text`, `is_struct` and `is_union` with it.
///
/// This function used to compare `CppSyntaxKind::ClassDef` by hand, which is the defect the whole round is about: it
/// worked on every `class` and on **no `struct` at all** — and `struct` is what `std::string` and the user's own
/// types are. Measured with `cpp_dump`: `struct MyString { … }` is a **`StructDef`**. The typed layer had known that
/// all along; this layer was re-deriving a worse answer from a node kind.
pub fn is_a_class_definition(node: &cpp_parser::CppSyntaxNode) -> bool {
    use cpp_parser::CppAstNode;
    cpp_parser::CppClassDef::cast(node.clone()).is_some()
}

/// **Is this node a function definition?** See [`function_definitions`] for why the question is about shape.
pub fn is_a_function_definition(node: &cpp_parser::CppSyntaxNode) -> bool {
    use cpp_parser::{CppAstNode, CppCompoundStat, CppDeclaration, CppParamList};

    // **The body is a direct child and the parameters are not**, which is the asymmetry that made this predicate
    // wrong the first time and silent for three checks: measured, `int count() { }` is a `Declaration` whose
    // `CompoundStat` is a child while its `ParameterList` is a grandchild, inside `InitDeclarator > Declarator`. The
    // predicate is now written with the typed layer, where that asymmetry is the difference between `children` and
    // `descendants` rather than a fact a reader has to remember.
    let Some(declaration) = CppDeclaration::cast(node.clone()) else {
        return false;
    };
    let Some(body) = declaration.children::<CppCompoundStat>().next() else {
        return false;
    };
    let body = span_of(body.syntax());

    // **The parameters must belong to THIS declaration's declarator, not to anything inside it.**
    //
    // The version before asked only whether *some* parameter list in this node began before the body — and a class
    // definition satisfies that: measured, `struct S { ~S() {} };` is a `Declaration` whose `DeclSpecifierSeq` holds
    // the `StructDef`, so its `CompoundStat` child is **the class body** and a method's parameter list begins before
    // it. Every class with a defined member was therefore a "function definition", and the checks that ask this
    // question reported the class itself. Measured through `ctor_probe`: `struct S { ~S() {} };` at namespace scope
    // came out as a definition whose return type was the whole `struct S { … }` text.
    //
    // Anchoring on the declaration's **own** `InitDeclarator` is what makes the parameters the ones the body belongs
    // to. A definition has exactly one: `int f() { }` and `S::S() { }` each put their `()` inside it.
    let Some(parameters) = declaration
        .get_init_declarator()
        .and_then(|declarator| declarator.get_declarator())
        .and_then(|declarator| {
            declarator
                .syntax()
                .descendants()
                .find_map(CppParamList::cast)
        })
        .map(|parameters| span_of(parameters.syntax()))
    else {
        return false;
    };

    // The list is inside the head, so it starts before the body; and the body is not inside the list, which is what
    // `void f(int g(void))` would otherwise look like — a list inside a list.
    !contains(parameters, body) && parameters.0 < body.0
}

/// A node's span as `(start, end)` offsets — what the rest of this layer compares, and cheap to pass around.
fn span_of(node: &cpp_parser::CppSyntaxNode) -> (usize, usize) {
    let range = node.text_range();
    (usize::from(range.start()), usize::from(range.end()))
}

/// Does `outer` contain `inner`? Both are `(start, end)` in the same text's offsets.
fn contains(outer: (usize, usize), inner: (usize, usize)) -> bool {
    outer.0 <= inner.0 && outer.1 >= inner.1
}

/// **The body of a function definition**, when it has one.
///
/// Asked of the typed layer: a body is a `CppCompoundStat`, and where the parser put it is the parser's business —
/// which is the lesson of the two defects this layer has already had from hand-rolled shape tests.
pub fn body_of(function: &cpp_parser::CppSyntaxNode) -> Option<cpp_parser::CppSyntaxNode> {
    use cpp_parser::{CppAstNode, CppCompoundStat, CppDeclaration};
    CppDeclaration::cast(function.clone())?
        .children::<CppCompoundStat>()
        .next()
        .map(|body| body.syntax().clone())
}
/// **Every function *definition* in this file's own text.**
///
/// ```text
///   reported by  missing-return, param-type-check, return-type-mismatch
/// ```
///
/// It walks [`CheckContext::own_syntax_nodes`] — this file's own text, with the inlined headers pruned — and asks
/// [`is_a_function_definition`], which is the **one** place that question is answered.
///
/// It used to answer it here as well, with a second copy of the shape test that asked for the parameter list *and*
/// the body to be direct children. That copy was the broken one, the predicate beside it had already been fixed, and
/// the three checks that call this function therefore reported nothing on any file while looking correct — measured,
/// and the reason their positive tests were still ignored.
pub fn function_definitions(context: &CheckContext<'_>) -> Vec<cpp_parser::CppSyntaxNode> {
    context
        .own_syntax_nodes()
        .filter(is_a_function_definition)
        .collect()
}
/// **The return type a function definition writes**, as the text before its name.
///
/// Read as **text** rather than as nodes, because the recovery the grammar performs around a declarator is what
/// makes a node-shaped answer unreliable here: `int* f()`, `const std::string& f()` and `auto f()` each have to come
/// out as the spelling the reader wrote.
///
/// `None` for a definition with nothing before its name — a constructor — and for a head this cannot read.
/// **The specifier keywords that are not a return type.**
///
/// A declaration's specifier sequence holds the *declaration specifiers* — and several of them say how a function is
/// built rather than what it returns. `static int f()` puts `static` in that sequence, `constexpr ~S()` puts
/// `constexpr` there and **no type at all**, and both were being read out as the return type:
///
/// ```text
///   constexpr ~S() {}                return_type_written_by -> Some("constexpr")   should be None
///   static int f() {}                return_type_written_by -> Some("static int")  should be "int"
///   virtual int f() {}               return_type_written_by -> Some("virtual int") should be "int"
///   explicit S(int a) {}             return_type_written_by -> Some("explicit S")  should be None
/// ```
///
/// A list of spellings is the **weakest** form this rule can take, and it is chosen deliberately: the specifier
/// sequence is read as text (see [`return_type_written_by`] for why), and the alternative — a second reader of the
/// same sequence that decides by token kind — is the defect this whole function exists to remove. The list is
/// C++'s own set of decl-specifiers that cannot be part of a type, and a spelling that is missing from it shows up as
/// a wrong message rather than as a wrong finding, which is the direction to be wrong in.
const NOT_A_TYPE: &[&str] = &[
    "constexpr",
    "consteval",
    "constinit",
    "static",
    "inline",
    "virtual",
    "explicit",
    "friend",
    "extern",
    "mutable",
    "register",
    "thread_local",
    "typedef",
];

/// **The three parts of a function's head, read once**: what it returns, what it is called, and where that name is.
///
/// One function rather than three, because the three are answers about **one** span of text and three readers of it
/// disagreed — each in its own way, and each silently:
///
/// ```text
///   return_type_written_by read the whole head as the type   -> "explicit S", "int S::f", "static int"
///   name_declared_by       read the last child ending before
///                          the parameter list                 -> None for every definition
///   range_of_the_name      compared a node kind that does not exist -> None for every function
/// ```
///
/// Splitting the head once means a constructor cannot be given a return type by one reader while another reader
/// fails to see it is a constructor.
pub struct FunctionHead {
    /// What the definition writes before its name, with the specifiers above removed. `None` for a constructor, a
    /// destructor and an `operator` — the three shapes that write no return type.
    pub returns: Option<String>,
    /// The name as written: `f`, `~S`, `S`, `operator+`.
    pub name: String,
    /// Where that name is, for a diagnostic. The whole span for an `operator`, which is not one token.
    pub name_range: cpp_parser::SourceRange,
}

/// **The return type a function definition writes**, as [`function_head`] reads it.
///
/// Kept as a named function because it is the question three checks ask; it is one line over the head reader so that
/// the type, the name and the name's range cannot come from three readers that disagree.
pub fn return_type_written_by(function: &cpp_parser::CppSyntaxNode) -> Option<String> {
    function_head(function)?.returns
}

/// Read a function's head — see [`FunctionHead`].
pub fn function_head(function: &cpp_parser::CppSyntaxNode) -> Option<FunctionHead> {
    use cpp_parser::{CppAstNode, CppParamList};

    // **Anchored on the parameter list**, found through the typed AST: a function's name is written immediately
    // before its `(`, in every spelling. `CppParamList::cast` rather than a kind comparison, because the node is
    // `ParameterList` — a kind this grammar does not have under that name, so a kind comparison matched nothing.
    let parameters = function.descendants().find_map(CppParamList::cast)?;
    let open = usize::from(parameters.syntax().text_range().start());

    // Every non-trivia token of the head, in order: everything before the `(`.
    let head: Vec<cpp_parser::CppSyntaxToken> = function
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| usize::from(token.text_range().end()) <= open)
        .filter(|token| !cpp_parser::is_trivia(cpp_parser::CppTokenKind::from(token.kind())))
        .collect();

    // **The name.** An `operator` is spelled `operator` plus a symbol and is not an identifier at all; a destructor
    // is `~` plus the class name; everything else ends at its last identifier.
    // **The name, and where it starts — which is also where the return type ends.**
    //
    // An `operator` is spelled `operator` plus a symbol and is not an identifier at all; a destructor is `~` plus the
    // class name; everything else ends at its last identifier. In all three the name **begins** at the token this
    // finds, and the head the return type is read from is what comes before that.
    let (name, name_range, name_starts_at) =
        match head.iter().position(|token| token.text() == "operator") {
            Some(at) => {
                let last = head.last()?;
                (
                    head[at..]
                        .iter()
                        .map(|token| token.text().to_string())
                        .collect::<String>(),
                    cpp_parser::source_range(head[at].text_range().cover(last.text_range())),
                    at,
                )
            }
            None => {
                let at = head.iter().rposition(|token| {
                    token.kind() == cpp_parser::CppTokenKind::Identifier.into()
                })?;
                // A leading `~` belongs to the name: `~S` is a destructor, and a caller comparing the name with the
                // enclosing class has to see the same spelling the reader wrote.
                let start = if at > 0 && head[at - 1].text() == "~" {
                    at - 1
                } else {
                    at
                };
                (
                    head[start..=at]
                        .iter()
                        .map(|token| token.text().to_string())
                        .collect::<String>(),
                    cpp_parser::source_range(head[start].text_range().cover(head[at].text_range())),
                    start,
                )
            }
        };
    let before_the_name = &head[..name_starts_at];

    // What is left before the name, minus the specifiers that are not a type.
    //
    // **Rebuilt from the tokens with the separation the SOURCE has, not the rendering.** The rendering collapses
    // whitespace, so its text for `int S::f() { }` is `int S::f() { }` — the type `int` and the *name's* scope `S::`
    // standing next to each other with nothing between them, which no reading of that text can tell apart. Measured,
    // every attempt to slice it gave `Some("int S::")`.
    //
    // The token stream keeps what the file wrote: `int` and `S` are two tokens with a space between them in the
    // source, and a `::` is a token of its own. Reading each token's own text and inserting the **source's** gap
    // between neighbours reconstructs the head as the reader wrote it, which is what a message has to show.
    let mut returns = String::new();
    let mut previous_end: Option<usize> = None;
    let own_text = function.text().to_string();
    let function_starts = usize::from(function.text_range().start());
    for token in before_the_name {
        let range = token.text_range();
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        if let Some(previous_end) = previous_end {
            let gap = source_between(&own_text, function_starts, previous_end, start);
            returns.push_str(gap);
        }
        returns.push_str(&token.text().to_string());
        previous_end = Some(end);
    }
    for specifier in NOT_A_TYPE {
        // Whole words only: `static_cast` is not a storage class, and neither is a type whose name contains one of
        // these spellings.
        returns = returns
            .split(' ')
            .filter(|word| word.trim() != *specifier)
            .collect::<Vec<_>>()
            .join(" ");
    }
    let returns = returns.trim().to_string();

    // **A qualified definition writes its scope, and the scope is not the type.** `int S::f()` writes `S::` in the
    // head between the return type and the name, and a reader that took "everything before the name" collected it:
    // measured, `Some("int S::")`.
    //
    // The head this reads **ends at the name**, so the scope of the declared name is simply a `::` that the head ends
    // with, and what precedes it is the type. What must not be cut is the scope *within* a type: `const std::string&
    // f()` puts `const std::string&` here, and it does not end with a `::` at all.
    //
    // This is the second judgement written for it. The first asked whether the text after the `::` started with the
    // name — which is empty here by construction, so it matched nothing and `int S::f()` kept its `S::`; the second
    // asked whether the text before it ended with the name, which compares the *scope's* name (`S`) against the
    // *function's* (`f`) and so also matched nothing. Both were answers to a question this reader had already
    // answered by cutting the name off.
    let mut returns = returns.trim().to_string();
    if returns.ends_with("::") {
        returns = strip_trailing_scopes(&returns);
    }
    // `int*` and `int *` are one type; the `*` run is kept as written, so `int *` is the spelling given back.
    let returns = returns.trim().to_string();

    // **The three shapes C++ gives no return type to.** `~S` and `operator*` are spelled that way and no scope
    // table would name them; a constructor is an *ordinary identifier* whose name is the class's, which is why it
    // is the one case this cannot decide from the name alone and asks the enclosing class instead.
    let returns = if returns.is_empty()
        || name.starts_with('~')
        || name.starts_with("operator")
        || declares_the_enclosing_class_named(function, &name)
    {
        None
    } else {
        Some(returns)
    };

    Some(FunctionHead {
        returns,
        name,
        name_range,
    })
}

/// **The gap the source has between two offsets**, or a single space when it cannot be read.
///
/// Used by [`function_head`] to put the separation back between tokens: the rendering removes whitespace, so its text
/// has `int` and `S` adjacent where the file wrote a space, and `int S::f()` is unreadable from that text alone.
/// Plain bytes rather than a comment-aware scan, because what stands between two tokens of one function's head is
/// whitespace — anything else would be a token, and a token is one of the two ends.
fn source_between(function_text: &str, function_starts: usize, from: usize, to: usize) -> &str {
    let (from, to) = (
        from.saturating_sub(function_starts),
        to.saturating_sub(function_starts),
    );
    function_text.get(from..to).unwrap_or(" ")
}

/// **Drop every scope name a head ends with** — `int a::b::` becomes `int`.
///
/// The head a definition writes ends at the name it declares, so a qualified definition leaves the scopes it reached
/// the name through: `int S::f()` reads as `int S::`, and `int a::b::c()` as `int a::b::`. Each `word::` at the end is
/// one of those scopes and none of them is a type.
///
/// A **loop** rather than one pass, because a namespace nests: measured, one pass left `int a:` from `int a::b::c()`
/// — the first scope went and the second stayed, and the answer named a type that does not exist.
fn strip_trailing_scopes(head: &str) -> String {
    let mut head = head.trim_end().to_string();
    while head.ends_with("::") {
        head.truncate(head.len() - 2);
        head = head.trim_end().to_string();
        // The scope's own name goes with it — the last identifier-shaped word, removed as a whole.
        //
        // Splitting on "not alphanumeric or `_`" is what went wrong twice here: measured, `int a::b::` left `int a:`
        // and then `int a`, because the split ran on the *colons* and on the space instead of on the word boundary.
        // The word is what has to go, so the boundary is what it is found by.
        let word_starts = head
            .rfind(|character: char| !(character.is_alphanumeric() || character == '_'))
            .map(|at| at + 1)
            .unwrap_or(0);
        head.truncate(word_starts);
        head = head.trim_end().to_string();
    }
    head
}

/// See [`missing_return::declares_the_enclosing_class`]'s rule, applied to a name that has already been read.
///
/// Here rather than in the caller because the head reader is what has the name and the node, and a second walk of
/// the ancestors from outside would be a second answer to the same question.
fn declares_the_enclosing_class_named(function: &cpp_parser::CppSyntaxNode, name: &str) -> bool {
    use cpp_parser::CppSyntaxKind;

    for ancestor in function.ancestors().skip(1) {
        match CppSyntaxKind::from(ancestor.kind()) {
            CppSyntaxKind::StructDef | CppSyntaxKind::ClassDef | CppSyntaxKind::UnionDef => {
                let declared = ancestor
                    .children()
                    .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ClassBody)
                    .and_then(|body| {
                        ancestor
                            .descendants_with_tokens()
                            .filter_map(|element| element.into_token())
                            .take_while(|token| {
                                token.text_range().end() <= body.text_range().start()
                            })
                            .filter(|token| {
                                token.kind() == cpp_parser::CppTokenKind::Identifier.into()
                            })
                            .last()
                            .map(|token| token.text().to_string())
                    });
                return declared.as_deref() == Some(name.trim_start_matches('~'));
            }
            CppSyntaxKind::CompoundStat => return false,
            _ => {}
        }
    }
    false
}
/// **The last identifier written inside a node** — the name a call would spell, the name a declaration declares.
///
/// Shared by the two checks that pair a call with a declaration, because both need the same answer to "which name
/// is this": the *last* one written, so that a return type (`int scale(…)`), a scope (`w.scale(…)`) and a template
/// head (`vector<int>::push_back(…)`) all end at the name and not at something before it.
pub fn spelling_of(node: &cpp_parser::CppSyntaxNode) -> Option<String> {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == cpp_parser::CppTokenKind::Identifier.into())
        .last()
        .map(|token| token.text().to_string())
}

/// **The name token a declaration or definition writes** — the last non-trivia token before its parameter list.
///
/// This is the one place that answers *where the name is*, and everything that needs the name comes through it:
/// [`name_declared_by`] takes its text and a diagnostic takes its range. One question, one implementation — the
/// alternative is what this replaced, which was two readers of the same fact that disagreed.
///
/// # Why the parameter list is the anchor
///
/// Because a function's name is written immediately before its `(`, in every spelling this layer sees: `int f()`,
/// `void C::f()`, `Name::~Name()`, `operator int()`, `int (S::*p)()`. A reader that anchored on anything else had to
/// decide which of the tokens before the list is *the* name, and both earlier attempts got that wrong in a different
/// way:
///
/// ```text
///   "the last child ending before the parameter list starts"
///       `take_while` stops at the FIRST child that ends after it — and the first child of
///       `int unfinished() { }` is `DeclSpecifierSeq \"int \"`, which does not. So the head was
///       empty and name_declared_by answered None for every definition. Measured.
///   "the first identifier spelled like the name"
///       matched the return type, and produced a range from `int` to the end of the file. Measured.
/// ```
///
/// The list is found through the **typed** AST ([`CppParamList`](cpp_parser::CppParamList)) rather than by comparing
/// node kinds: the kind is `ParameterList`, a kind this grammar does not have under that name, so a kind comparison
/// silently matched nothing — which is the trap `status.md` §5 records and the reason its rule is to use `cast`.
///
/// The **token** is returned rather than its text, because the two callers want different halves of it and both are
/// cheaper from here: [`name_declared_by`] takes the spelling, and a diagnostic takes the range.
pub fn name_token_of(function: &cpp_parser::CppSyntaxNode) -> Option<cpp_parser::CppSyntaxToken> {
    use cpp_parser::{CppAstNode, CppParamList};

    let parameters = function.descendants().find_map(CppParamList::cast)?;
    let open = usize::from(parameters.syntax().text_range().start());

    function
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| usize::from(token.text_range().end()) <= open)
        .filter(|token| !cpp_parser::is_trivia(cpp_parser::CppTokenKind::from(token.kind())))
        .filter(|token| token.kind() == cpp_parser::CppTokenKind::Identifier.into())
        .last()
}

/// **The name a function declares** — see [`function_head`], which reads it together with the return type and
/// therefore cannot disagree with it about which name it found.
pub fn name_declared_by(function: &cpp_parser::CppSyntaxNode) -> Option<String> {
    Some(function_head(function)?.name)
}

/// **A function's parameters as the declaration spells them** — `int count`, `const Widget& w`, `...`.
///
/// Read from the **tree** and never from [`DeclFact::parameters`](crate::DeclFact::parameters), which is the
/// template parameter list: the check this one replaces read that field and reported 119 findings on 200 MSVC
/// headers, every one of them a function whose real parameters were in the tree all along.
pub fn parameter_spellings_of(function: &cpp_parser::CppSyntaxNode) -> Vec<String> {
    use cpp_parser::{CppAstNode, CppDeclaration, CppParam, CppParamList};
    let Some(parameters) = CppDeclaration::cast(function.clone())
        .and_then(|declaration| declaration.descendants::<CppParamList>().next())
    else {
        return Vec::new();
    };
    parameters
        .children::<CppParam>()
        .map(|parameter| parameter.syntax().text().to_string().trim().to_string())
        .filter(|spelling| !spelling.is_empty())
        .collect()
}
/// **Can a value of this type build an object of that class?** — asked of the **index**, never of a tree.
///
/// ```text
///   Some(true)    a constructor could take it: the class has a constructor whose parameter could accept this type
///   Some(false)   nothing in the class can: it has constructors and none of them could, or it has none at all
///   None          not this layer's question: the class is not in the index, or a parameter is written in a way
///                 that decides nothing (`_Ty`, a template parameter, a type from a header nobody indexed)
/// ```
///
/// # Why the facts and not the tree
///
/// `CppMember` carries the class's own [`DeclFact`], and a function's `DeclFact::parameter_list` is **the parameter
/// list as its file spells it** — stored, in the index's own words, *"because a query holds one file and the
/// parameters are written in another… re-parsing that header per member query is the cost this field exists to
/// avoid"*. That is exactly this question: `std::string x = 123;` is asked from a `.cpp` whose `<string>` is not its
/// own text, and the answer is a fact about `<string>` that the index already holds.
///
/// A check that instead walked the **rendering** for the class would be doing the work this field exists to prevent
/// — and would find nothing at all when the class comes from a header, which is the case that matters.
///
/// # The rule, and why it is this narrow
///
/// A value whose type is arithmetic (or a literal) is compatible with a parameter that is arithmetic, `bool`,
/// `char`-like, or a **template parameter** — anything a template could instantiate to. It is **not** compatible
/// with a pointer or reference to a non-arithmetic type: `MyString(const char*)` cannot be built from `1231`, which
/// is the whole of the `std::string x = 123;` case. Everything else — a class parameter, a name this cannot classify
/// — answers `None`, because a conversion this layer cannot see is not a conversion the program does not have.
pub fn a_class_can_be_built_from(
    context: &CheckContext<'_>,
    class: &str,
    value: &crate::sema::types::Type,
) -> Option<bool> {
    let crate::Known::Yes(members) = context.model.members_of(class) else {
        return None;
    };

    // **An empty member list is not "no constructors".** `members_of` answers from this file's scopes and the index,
    // so a class it knows of but has no members recorded for — a forward declaration, a class whose defining file the
    // index has not read — comes back **empty**, and reading that as "nothing can build it" is how this rule reported
    // a class that has exactly the constructor its value needs: measured, `Widget(int size)` in a second project file
    // produced `0 members []` and a finding. `unlisted` is the field that says whether a list is complete, and it is
    // what makes "unknown" distinguishable from "nothing".
    if members.members.is_empty() {
        return None;
    }

    // **The members declared in the class itself**: an inherited constructor is not this class's, and `depth` is
    // what says so — the same field a completion groups by.
    let mut constructors = 0usize;
    for member in members.own() {
        let name = member.fact.name.as_str();
        let is_a_constructor = name == class.rsplit("::").next().unwrap_or(class);
        // **A conversion operator is a way to build the class that has no parameter list to compare against** —
        // `operator Widget()` — so its presence means this question is not decided here.
        if name.starts_with("operator") {
            return None;
        }
        if !is_a_constructor {
            continue;
        }
        let Some(parameters) = member.fact.parameter_list.as_deref() else {
            continue;
        };
        constructors += 1;

        match parameters_that_could_accept(parameters, value) {
            // One constructor that could take the value is enough for this rule to have nothing to say.
            Some(true) => return Some(true),
            // A parameter written in a way that decides nothing: the class may still be buildable.
            None => return None,
            Some(false) => {}
        }
    }

    // **No constructor at all is a class no value can build.** A class with no constructor is an aggregate, and an
    // aggregate cannot be initialised from a single value: `struct S { int size; }; S s = 1231;` does not compile.
    // (A braced list of matching members is a different rule and a different shape - `S s{1231}`.)
    let _ = constructors;
    Some(false)
}

/// **Whether one written parameter list holds a parameter that could accept this value** — see
/// [`a_class_can_be_built_from`] for the rule and why it is this narrow.
fn parameters_that_could_accept(written: &str, value: &crate::sema::types::Type) -> Option<bool> {
    // The value has to be something this can classify. A class-to-class conversion is a question about two
    // declarations, and this is asked with one of them in hand.
    if !value.is_arithmetic() {
        return None;
    }

    let inner = written.trim().trim_start_matches('(').trim_end_matches(')');
    let mut depth = 0i32;
    let mut parameters: Vec<String> = Vec::new();
    let mut current = String::new();
    for character in inner.chars() {
        match character {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            // **A comma at depth zero separates parameters** — the whole reason this is counted rather than split:
            // `std::pair<int, int>` is one parameter with a comma in it.
            ',' if depth == 0 => {
                parameters.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    parameters.push(current);

    // **One value fills one parameter.** A constructor written with two parameters cannot be built from one value,
    // unless one of them has a default — `basic_string(size_type _Count, _Elem _Ch)` takes two, `_Ty(int, int = 0)`
    // takes one. Without this, `struct S { S(int, int); }; S s = 123;` was read as buildable because `int` accepts an
    // `int`, which is the arity question answered by the wrong rule.
    if parameters.len() > 1 {
        return if parameters.iter().any(|parameter| parameter.contains('=')) {
            None
        } else {
            Some(false)
        };
    }

    let mut any = false;
    for parameter in parameters {
        let Some(classified) = could_take_an_arithmetic_value(&parameter) else {
            // A parameter this cannot classify decides nothing — a template parameter, an unknown name.
            return None;
        };
        any = true;
        if classified {
            return Some(true);
        }
    }
    any.then_some(false)
}

/// **Could a parameter written like this accept an arithmetic value?**
///
/// `None` for a parameter that decides nothing: a template parameter (`_Ty`, `T`, `class _Elem`), an `auto`, or a
/// name that is neither an arithmetic type nor a pointer to one.
fn could_take_an_arithmetic_value(parameter: &str) -> Option<bool> {
    let mut written = parameter.trim();

    // Drop a default argument: `int n = 0` is an `int`, and the `=` and everything after it is not the type.
    if let Some(at) = written.find('=') {
        written = written[..at].trim();
    }
    if written.is_empty() || written == "..." || written.ends_with("...") {
        // `...` takes anything, which is a parameter that could accept the value.
        return Some(written == "..." || written.ends_with("..."));
    }

    // The type is everything before the name: `const Widget& w` and `const Widget&` are the same type.
    let words: Vec<&str> = written.split_whitespace().collect();
    let (type_words, _name) = match words.as_slice() {
        [] => return None,
        [only] => (vec![*only], ""),
        [rest @ .., last] => (rest.to_vec(), *last),
    };
    let type_written = type_words.join(" ");

    let arithmetic = |word: &str| {
        let word = word.trim_start_matches("const").trim();
        matches!(
            word,
            "int"
                | "long"
                | "short"
                | "char"
                | "signed"
                | "unsigned"
                | "float"
                | "double"
                | "bool"
                | "wchar_t"
                | "char8_t"
                | "char16_t"
                | "char32_t"
                | "size_t"
                | "std::size_t"
                | "ptrdiff_t"
                | "int64_t"
                | "uint64_t"
                | "int32_t"
                | "uint32_t"
                | "std::int64_t"
                | "std::uint64_t"
        )
    };

    // A pointer or reference **to something that is not arithmetic** cannot be built from a number:
    // `MyString(const char*)` is the constructor `std::string` has and `1231` does not fit it.
    let behind_the_operator = type_written
        .split(['*', '&'])
        .next()
        .unwrap_or_default()
        .trim()
        .trim_start_matches("const")
        .trim();
    let is_a_pointer_or_reference = type_written.contains('*') || type_written.contains('&');

    if is_a_pointer_or_reference {
        // `const int&` takes an `int`; `const char*` does not.
        return Some(arithmetic(behind_the_operator));
    }

    let bare = type_written.trim_start_matches("const").trim();
    if arithmetic(bare) {
        return Some(true);
    }

    // **A template parameter takes whatever it is instantiated with** — `_Ty`, `_Elem`, `class _Ty`, `typename T`.
    let looks_like_a_template_parameter = bare.starts_with('_')
        || bare == "T"
        || bare.len() == 1
        || bare.starts_with("class ")
        || bare.starts_with("typename ");
    if looks_like_a_template_parameter {
        return None;
    }

    // **A name with no `*` or `&` and no `(` is a class by value** — `MyString(Widget)` cannot take a number.
    Some(false)
}

/// **Run every check, in the order a consumer should show them**, and answer what they found.
///
/// The list is the registry: a check that is not here does not run. See the module documentation for why these are
/// typed calls rather than closures with an index.
pub fn run(context: CheckContext<'_>) -> Vec<Finding> {
    let mut context = context;

    // **`syntax_error` is NOT run, and this is where the duplicate was.**
    //
    // It is the one check that does not reason about the program — it republishes what the parser already answered —
    // and those answers are **already a channel of their own**: `FileDiagnostics::errors` carries
    // `cooked.diagnostics`, which `handlers::diagnostic::diagnose_file` publishes first. Running the check as well
    // put every parse error on the wire **twice**, which is what was reported and what
    // `handshake.rs`'s `a_pulling_client_gets_a_report_and_not_null` asserts against:
    //
    // ```text
    // PROBE channels: errors=1 checks=1 notes=0   →   final: 2 items, both `expected \`;\``
    // ```
    //
    // One error from the channel built for it, one from the check that copies it. The fix is not to stop publishing
    // either one — it is to stop having two. `errors` is the one kept, because it is the channel that carries the
    // parser's own diagnostics and the `unplaced` count with them; the check is left in the tree, compiled and
    // documented, for the reader who wants to know how the two ever came apart. `DiagnosticCode::SyntaxError` is
    // still the code every parse error is published under, and `syntax_error_is_reported` still passes, because it
    // collects `errors` as well as `checks`.
    //
    // The **order** that check's own note argues for is kept by construction: `diagnose_file` chains `found.errors`
    // before `check_findings`, so a reader is still told what to fix before what the fixed text will still be wrong
    // about.
    run_check::<include_not_found::IncludeNotFound>(&mut context);
    run_check::<macro_redefined::MacroRedefined>(&mut context);
    run_check::<unused::Unused>(&mut context);
    // **The style rules**, named after clang-tidy's own — see each module for what it will say and why it is worth saying.
    // They are registered and do nothing yet: a rule with a name, a code and a test is what the next pass fills in.
    run_check::<braces_around_statements::BracesAroundStatements>(&mut context);
    run_check::<use_nullptr::UseNullptr>(&mut context);
    run_check::<use_override::UseOverride>(&mut context);
    run_check::<simplify_boolean_expr::SimplifyBooleanExpr>(&mut context);
    run_check::<else_after_return::ElseAfterReturn>(&mut context);
    run_check::<cstyle_cast::CStyleCast>(&mut context);
    run_check::<access_invisible::AccessInvisible>(&mut context);
    run_check::<missing_return::MissingReturn>(&mut context);
    run_check::<assignment_in_condition::AssignmentInCondition>(&mut context);
    run_check::<unnecessary_branch::UnnecessaryBranch>(&mut context);
    run_check::<undefined_field::UndefinedField>(&mut context);
    run_check::<argument_count::ArgumentCount>(&mut context);
    run_check::<param_type_check::ParamTypeCheck>(&mut context);
    run_check::<assign_type_check::AssignTypeCheck>(&mut context);
    run_check::<return_type_mismatch::ReturnTypeMismatch>(&mut context);

    context.into_findings()
}

/// **The member access a callee is the name of** — `push_back` in `v.push_back(…)`.
///
/// `member_access_of` answers for the **node that holds the operator**, and a call's callee is the identifier to its
/// right: the access is the parent. Walking up a couple of levels rather than asserting the shape keeps this honest for
/// the grammar's other spellings (`(*p).f`, `a.b.f`), where the callee's parent is not always the outermost access.
pub fn member_access_above(node: &cpp_parser::CppSyntaxNode) -> Option<crate::sema::resolve::MemberAccess> {
    let mut current = Some(node.clone());
    for _ in 0..3 {
        let node = current?;
        if let Some(access) = crate::sema::resolve::member_access_of(&node) {
            return Some(access);
        }
        current = node.parent();
    }
    None
}