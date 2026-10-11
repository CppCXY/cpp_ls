//! **Every diagnostic this analysis can produce, as one code** — and the severity it is shown with.
//!
//! The shape follows `emmylua_code_analysis`'s `check/diagnostic_code.rs`: an enum whose variant names are the
//! codes in kebab case, one `///` line per variant saying what it means, and the answers (`all`, `name`,
//! `severity`) derived from the enum itself rather than from a table beside it.
//!
//! # Why the name is *derived* and not written twice
//!
//! ```text
//!   the module        param_type_check.rs        the check's own file, snake case
//!   the variant       DiagnosticCode::ParamTypeCheck
//!   the code          "param-type-check"         what a client shows and a config file names
//! ```
//!
//! Three spellings of one rule, and [`DiagnosticCode::name`] is the only place the third is written. A check that
//! reported a code it did not mean cannot be written — it reports a **variant**, not a string — and a new variant
//! without a name or a severity is a compile error rather than a rule that publishes an empty string.
//!
//! # Severity is a property of the rule
//!
//! Not of the finding: "this name is not used" and "this file does not parse" are different kinds of claim and the
//! difference belongs to the rule, stated once. [`DiagnosticCode::severity`] is what the LSP layer puts in
//! `Diagnostic::severity`, and it is the same answer for every finding of that rule.
//!
//! # Suppressing a rule, when it comes
//!
//! Two forms, and both are already shaped by this enum:
//!
//! ```text
//!   by name    a config lists codes; `from_name` turns each into a variant, and an unknown one is *reported*
//!              to the user rather than silently ignored
//!   by line    a comment on a line names a code — that belongs in `CheckContext::add_diagnostic`, which is the
//!              one place a finding becomes a diagnostic, so that a check never has to know it was suppressed
//! ```
//!
//! Nothing suppresses anything yet. [`CheckConfig`](super::config::CheckConfig) is where the first form will live.

/// **One rule this analysis has.**
///
/// See the module documentation for the three spellings of a rule and for why the code is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticCode {
    /// The file does not parse — see [`super::syntax_error`].
    SyntaxError,
    /// An argument's type does not convert to the parameter it is passed for — see [`super::param_type_check`].
    ParamTypeCheck,
    /// A value does not convert to the type it is assigned to or initialised as — see [`super::assign_type_check`].
    AssignTypeCheck,
    /// A member access on a type that does not declare that member — see [`super::undefined_field`].
    UndefinedField,
    /// A declaration nothing uses — see [`super::unused`].
    Unused,
    /// A branch the program cannot take, or a condition that is already decided — see
    /// [`super::unnecessary_branch`].
    UnnecessaryBranch,
    /// More arguments than the callee takes, or fewer than it needs — see [`super::argument_count`].
    ArgumentCount,
    /// A `return` whose expression does not convert to the function's return type — see
    /// [`super::return_type_mismatch`].
    ReturnTypeMismatch,
    /// A macro defined twice with different replacement lists — see [`super::macro_redefined`].
    MacroRedefined,
    /// A `#include` that names a file nobody can find — see [`super::include_not_found`].
    ///
    /// Separate from [`DiagnosticCode::SyntaxError`] for the reason a reader needs it separate: the file parses
    /// perfectly, and what is missing is a file it named.
    IncludeNotFound,
    /// A member reached from somewhere that may not reach it - see [`super::access_invisible`].
    AccessInvisible,
    /// A function declared to return something whose body never returns - see [`super::missing_return`].
    MissingReturn,
    /// `=` where the reader almost certainly meant `==` - see [`super::assignment_in_condition`].
    AssignmentInCondition,
    /// **A note about a module import**, which the analysis knows rather than complains about — see
    /// [`Session::notes_about_the_modules`](crate::Session::notes_about_the_modules).
    ///
    /// Not a check: nothing is wrong with the file, and the reader is told where a file they named is. It is a code
    /// because it is a diagnostic a client shows, and the point of the enum is that **everything shown has one**.
    ModuleNote,
}

impl DiagnosticCode {
    /// **Every code, in the order a list reads in.**
    ///
    /// The order is the one the module documentation of [`super`] describes: what the text *is* (the parse), then
    /// what it says about types, then what it says about a whole body, then the two that are about the reading
    /// rather than the program. A consumer that shows a settings screen, or a test that asserts every rule has a
    /// name and a module, iterates this.
    pub const ALL: [DiagnosticCode; 14] = [
        DiagnosticCode::SyntaxError,
        DiagnosticCode::ParamTypeCheck,
        DiagnosticCode::AssignTypeCheck,
        DiagnosticCode::UndefinedField,
        DiagnosticCode::Unused,
        DiagnosticCode::UnnecessaryBranch,
        DiagnosticCode::ArgumentCount,
        DiagnosticCode::ReturnTypeMismatch,
        DiagnosticCode::MacroRedefined,
        DiagnosticCode::IncludeNotFound,
        DiagnosticCode::AccessInvisible,
        DiagnosticCode::MissingReturn,
        DiagnosticCode::AssignmentInCondition,
        DiagnosticCode::ModuleNote,
    ];

    /// **The code a client is shown**, and the spelling a config file uses: kebab case, and the module's file name
    /// is the snake-case form of it (`param-type-check` ↔ `param_type_check.rs`).
    ///
    /// The `match` has no `_` arm on purpose. A new variant without a name is a compile error, which is the only
    /// way to be sure that no rule ever reaches a client unnamed.
    pub fn name(self) -> &'static str {
        match self {
            DiagnosticCode::SyntaxError => "syntax-error",
            DiagnosticCode::ParamTypeCheck => "param-type-check",
            DiagnosticCode::AssignTypeCheck => "assign-type-check",
            DiagnosticCode::UndefinedField => "undefined-field",
            DiagnosticCode::Unused => "unused",
            DiagnosticCode::UnnecessaryBranch => "unnecessary-branch",
            DiagnosticCode::ArgumentCount => "argument-count",
            DiagnosticCode::ReturnTypeMismatch => "return-type-mismatch",
            DiagnosticCode::MacroRedefined => "macro-redefined",
            DiagnosticCode::IncludeNotFound => "include-not-found",
            DiagnosticCode::AccessInvisible => "access-invisible",
            DiagnosticCode::MissingReturn => "missing-return",
            DiagnosticCode::AssignmentInCondition => "assignment-in-condition",
            DiagnosticCode::ModuleNote => "module-note",
        }
    }

    /// **The code as a rule**, or `None` for a code this build does not have.
    ///
    /// The inverse of [`DiagnosticCode::name`], and the round trip is asserted for every variant. A config reader
    /// uses this so that a misspelled rule is *reported* — treating an unknown name as "no rules" turns a typo into
    /// a silently disabled analysis, which is the failure mode this layer's contract is written against.
    pub fn from_name(name: &str) -> Option<DiagnosticCode> {
        DiagnosticCode::ALL.into_iter().find(|code| code.name() == name)
    }

    /// **How a client should draw it.**
    ///
    /// This crate does not speak LSP — the severities are its own, and the server maps them — for the reason it
    /// does not speak LSP anywhere else: the analysis is usable by a batch tool that has no protocol at all.
    pub fn severity(self) -> RuleSeverity {
        match self {
            // **A parse error stops everything.** The file does not mean anything until it is fixed, and no other
            // rule's finding can be trusted while the text does not parse.
            DiagnosticCode::SyntaxError => RuleSeverity::Error,
            // **A name that is not there, a call that cannot be made, a type that does not fit.** These are the
            // language's own complaints: the program will not build.
            DiagnosticCode::IncludeNotFound => RuleSeverity::Error,
            DiagnosticCode::ParamTypeCheck => RuleSeverity::Error,
            DiagnosticCode::AssignTypeCheck => RuleSeverity::Error,
            DiagnosticCode::UndefinedField => RuleSeverity::Error,
            DiagnosticCode::ArgumentCount => RuleSeverity::Error,
            DiagnosticCode::ReturnTypeMismatch => RuleSeverity::Error,
            // **A judgement rather than a law.** A macro redefined with a different body is legal in some
            // translation units and a bug in most; a branch that cannot run is dead code rather than a mistake.
            // Both draw attention without claiming the program is broken.
            DiagnosticCode::MacroRedefined => RuleSeverity::Warning,
            DiagnosticCode::UnnecessaryBranch => RuleSeverity::Warning,
            // **A hint, which is the quietest thing a client can draw** — a dotted underline in VS Code, nothing at
            // all in several others. "Nothing reads this name" is the least urgent claim this analysis makes: the
            // program builds, runs, and may be exactly what the author meant while they were writing it.
            DiagnosticCode::Unused => RuleSeverity::Hint,
            // **A member another class may not reach, and a body that never returns what it promises**: both are
            // complaints about a program that will not build.
            DiagnosticCode::AccessInvisible => RuleSeverity::Error,
            DiagnosticCode::MissingReturn => RuleSeverity::Error,
            // A judgement rather than a law: an assignment in a condition is deliberate in a great deal of code.
            DiagnosticCode::AssignmentInCondition => RuleSeverity::Warning,
            DiagnosticCode::ModuleNote => RuleSeverity::Information,
        }
    }

    /// **What a client may grey out** — the protocol's `DiagnosticTag`, as this crate's own vocabulary.
    ///
    /// A tag is not a severity: a warning can be tagged `Unnecessary` and so can a hint. It says *why* the reader
    /// is being told, which is what a client needs to render the code as struck through rather than merely
    /// underlined — the difference between "look at this" and "this does nothing".
    ///
    /// Per **code** rather than per finding, for the same reason severity is: "nothing uses this name" is what the
    /// rule says, and it says it about every finding it produces.
    pub fn tags(self) -> &'static [RuleTag] {
        match self {
            DiagnosticCode::Unused => &[RuleTag::Unnecessary],
            DiagnosticCode::UnnecessaryBranch => &[RuleTag::Unnecessary],
            _ => &[],
        }
    }

    /// Is this code a **claim that something is wrong**, as opposed to something the reader is told?
    ///
    /// [`DiagnosticCode::ModuleNote`] is the only one that is not, and the distinction is the one the diagnostics
    /// channel already makes: a note draws no squiggle and does not count towards "this file has problems".
    pub fn is_a_complaint(self) -> bool {
        !matches!(self, DiagnosticCode::ModuleNote)
    }
}

/// **What a client may grey out** — the protocol calls these tags, and this is the crate's own vocabulary for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuleTag {
    /// The code does nothing: an unused name, a branch that cannot run.
    Unnecessary,
}

/// **How a diagnostic is drawn**, in this crate's own vocabulary — see [`DiagnosticCode::severity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuleSeverity {
    /// The program will not build, or the text does not parse.
    Error,
    /// Something is wrong in a way the language permits — dead code, a redefinition.
    Warning,
    /// The quietest level: a suggestion a client may draw as a dotted underline.
    Hint,
    /// Not a complaint at all — a fact about where a file is.
    Information,
}




