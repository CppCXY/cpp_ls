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

use crate::{FileSummary, ProjectIndex};

// **Written, and not yet declared**: the bodies are in the tree, their calls into the model are still written
// against APIs this crate does not have (Type::spelling, MemberAccess::name, Known::is_yes), so they are
// kept out of the compile until that is settled rather than left as a red build.
// pub mod argument_count;
// **Written, and not yet declared**: the bodies are in the tree, their calls into the model are still written
// against APIs this crate does not have (Type::spelling, MemberAccess::name, Known::is_yes), so they are
// kept out of the compile until that is settled rather than left as a red build.
// pub mod assign_type_check;
pub mod config;
pub mod diagnostic_code;
// **Being ported to the Check trait**: git mv'd to their new names, still written against the old
// Checks/Finding { check: … } shape, so they are declared again the moment their bodies are rewritten. Kept in
// the tree rather than deleted, because their reasoning (and the measured 119-findings defect they record) is what
// the ported versions have to preserve.
pub mod include_not_found;
pub mod macro_redefined;
// **Written, and not yet declared**: the bodies are in the tree, their calls into the model are still written
// against APIs this crate does not have (Type::spelling, MemberAccess::name, Known::is_yes), so they are
// kept out of the compile until that is settled rather than left as a red build.
// pub mod param_type_check;
// **Written, and not yet declared**: the bodies are in the tree, their calls into the model are still written
// against APIs this crate does not have (Type::spelling, MemberAccess::name, Known::is_yes), so they are
// kept out of the compile until that is settled rather than left as a red build.
// pub mod return_type_mismatch;
pub mod syntax_error;
// **Written, and not yet declared**: the bodies are in the tree, their calls into the model are still written
// against APIs this crate does not have (Type::spelling, MemberAccess::name, Known::is_yes), so they are
// kept out of the compile until that is settled rather than left as a red build.
// pub mod undefined_field;
// **Written, and not yet declared**: the bodies are in the tree, their calls into the model are still written
// against APIs this crate does not have (Type::spelling, MemberAccess::name, Known::is_yes), so they are
// kept out of the compile until that is settled rather than left as a red build.
// pub mod unnecessary_branch;
pub mod unused;

pub use config::CheckConfig;
pub use diagnostic_code::{DiagnosticCode, Severity};

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
    /// **The file's own text**, as the ranges in its summary are offsets into.
    pub source: &'a str,
    /// **The parse errors of the reading this run is about**, which is what the `syntax-error` check publishes.
    ///
    /// Passed in rather than asked for: the errors belong to the **tree**, and this context deliberately holds a
    /// *model* over it — a check that could reach the tree would be a check that could re-parse.
    pub errors: &'a [cpp_parser::CppParseError],
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
        errors: &'a [cpp_parser::CppParseError],
        config: &'a CheckConfig,
    ) -> Self {
        CheckContext {
            path,
            summary,
            model,
            source,
            errors,
            config,
            findings: Vec::new(),
        }
    }

    /// **Report a finding** — and the one place a code is filtered.
    ///
    /// A check says what it found and does not ask whether anyone wants to hear it: the filter belongs here
    /// because there are eleven checks and the twelfth would forget. This is also where a per-line suppression
    /// will go (a comment naming a code on the line the finding is on), for the same reason — one place, so that no
    /// check has to know it was suppressed.
    pub fn add(
        &mut self,
        code: DiagnosticCode,
        range: SourceRange,
        name: impl Into<String>,
        message: impl Into<String>,
    ) {
        if !self.config.is_enabled(code) {
            return;
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

    /// **The type of an expression** — the model's, remembered per node for as long as this request lasts.
    pub fn type_of(
        &self,
        expression: &cpp_parser::CppSyntaxNode,
    ) -> crate::Known<(crate::sema::types::Type, std::path::PathBuf)> {
        self.model.type_of(expression)
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

/// **The name a function declares** — the last declarator in its head, before the parameter list.
pub fn name_declared_by(function: &cpp_parser::CppSyntaxNode) -> Option<String> {
    let parameters = function
        .children()
        .find(|child| cpp_parser::CppSyntaxKind::from(child.kind()) == cpp_parser::CppSyntaxKind::ParameterList)?;
    let head: Vec<cpp_parser::CppSyntaxNode> = function
        .children()
        .take_while(|child| child != &parameters)
        .collect();
    spelling_of(head.last()?)
}

/// **A function's parameters as the declaration spells them** — `int count`, `const Widget& w`, `...`.
///
/// Read from the **tree** and never from [`DeclFact::parameters`](crate::DeclFact::parameters), which is the
/// template parameter list: the check this one replaces read that field and reported 119 findings on 200 MSVC
/// headers, every one of them a function whose real parameters were in the tree all along.
pub fn parameter_spellings_of(function: &cpp_parser::CppSyntaxNode) -> Vec<String> {
    let Some(parameters) = function
        .children()
        .find(|child| cpp_parser::CppSyntaxKind::from(child.kind()) == cpp_parser::CppSyntaxKind::ParameterList)
    else {
        return Vec::new();
    };
    parameters
        .children()
        .map(|parameter| parameter.text().to_string().trim().to_string())
        .filter(|spelling| !spelling.is_empty())
        .collect()
}
/// **Run every check, in the order a consumer should show them**, and answer what they found.
///
/// The list is the registry: a check that is not here does not run. See the module documentation for why these are
/// typed calls rather than closures with an index.
pub fn run(context: CheckContext<'_>) -> Vec<Finding> {
    let mut context = context;

    run_check::<syntax_error::SyntaxError>(&mut context);
    run_check::<include_not_found::IncludeNotFound>(&mut context);
    run_check::<macro_redefined::MacroRedefined>(&mut context);
    run_check::<unused::Unused>(&mut context);
    // macro_redefined and include_not_found are being ported — see the note above their declarations.

    context.into_findings()
}




