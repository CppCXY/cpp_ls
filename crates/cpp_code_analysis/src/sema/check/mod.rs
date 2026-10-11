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
pub mod assignment_in_condition;
pub mod assign_type_check;
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
    /// **The text a range from the *tree* is an offset into** — the view's own reading, which for an indexed file
    /// is the **rendering**: macros expanded, directives resolved.
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
    /// What a finding is *published in* is a third thing and it is the file's coordinates, because the server
    /// places a range with the VFS line index. That crossing belongs where a finding is added — see
    /// [`CheckContext::add`] — so that no check has to do it.
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
        let range = self.in_the_file(range);
        self.push(code, range, name, message);
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
        self.push(code, range, name, message);
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
    statement
        .children()
        .find(|child| cpp_parser::CppSyntaxKind::from(child.kind()) != cpp_parser::CppSyntaxKind::CompoundStat)
}

/// **The one word a condition is, when it is a bare literal** - `true`, `false`.
///
/// Decided from the **tokens**: the parentheses and the whitespace are dropped, and what is left has to be exactly
/// one token whose text is one of the two words. A string literal is one token as well, so `if ("true")` cannot be
/// read as the keyword, and neither can a name that happens to be spelled that way only in a comment.
pub fn literal_condition_of(condition: &cpp_parser::CppSyntaxNode) -> Option<String> {
    use cpp_parser::CppTokenKind;

    let words: Vec<String> = condition
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| {
            !matches!(
                CppTokenKind::from(token.kind()),
                CppTokenKind::LeftParen
                    | CppTokenKind::RightParen
                    | CppTokenKind::Whitespace
                    | CppTokenKind::Newline
            )
        })
        .map(|token| token.text().to_string())
        .collect();

    match words.as_slice() {
        [word] if word == "true" || word == "false" => Some(word.clone()),
        _ => None,
    }
}
/// **Is this node a class-like *definition*?** - `class`, `struct` or `union`.
///
/// Three node kinds, one question, and the reason this predicate exists rather than each check naming a kind:
/// measured, `struct MyString { int size; };` is a **`StructDef`** while `class Widget { … };` is a **`ClassDef`**,
/// and only the body (`ClassBody`) is shared. A check that filtered on `ClassDef` therefore worked on every `class`
/// and on **no `struct` at all** — which is what `std::string` and the user's own type are. It cost a round: the
/// probe printed ``not found among the ClassDef nodes`` while the definition was inlined into the rendering.
///
/// `EnumDef` is deliberately not here: an enumeration has no access specifiers and no constructors, which are the
/// two questions this predicate is asked for.
pub fn is_a_class_definition(node: &cpp_parser::CppSyntaxNode) -> bool {
    use cpp_parser::CppSyntaxKind;
    matches!(
        CppSyntaxKind::from(node.kind()),
        CppSyntaxKind::ClassDef | CppSyntaxKind::StructDef | CppSyntaxKind::UnionDef
    )
}
/// **Is this node a function definition?** See [`function_definitions`] for why the question is about shape.
pub fn is_a_function_definition(node: &cpp_parser::CppSyntaxNode) -> bool {
    use cpp_parser::CppSyntaxKind;

    // **The body is a direct child and the parameters are not**, which is the asymmetry that made this predicate
    // wrong the first time and silent for three checks: measured, `int count() { }` is a `Declaration` whose
    // `CompoundStat` is a child while its `ParameterList` is a grandchild, inside `InitDeclarator > Declarator`. A
    // predicate asking for both as children matched nothing at all.
    let Some(body) = node
        .children()
        .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::CompoundStat)
    else {
        return false;
    };

    // **And the parameters come before the body.** Asked by position rather than by depth, so that a node wrapping
    // a whole function — a namespace, a class, the translation unit — cannot claim to be one: what is looked for is
    // a parameter list in this node's *head*, and those wrappers have their bodies after it or not at all.
    node.descendants()
        .any(|child| {
            CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ParameterList
                && child.text_range().start() < body.text_range().start()
        })
}

pub fn function_definitions(context: &CheckContext<'_>) -> Vec<cpp_parser::CppSyntaxNode> {
    context
        .tree()
        .descendants()
        .filter(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) != cpp_parser::CppSyntaxKind::ParameterList
                && node.children().any(|child| {
                    cpp_parser::CppSyntaxKind::from(child.kind())
                        == cpp_parser::CppSyntaxKind::ParameterList
                })
                && node.children().any(|child| {
                    cpp_parser::CppSyntaxKind::from(child.kind())
                        == cpp_parser::CppSyntaxKind::CompoundStat
                })
        })
        .collect()
}
/// **The return type a function definition writes**, as the text before its name.
///
/// Read as **text** rather than as nodes, because the recovery the grammar performs around a declarator is what
/// makes a node-shaped answer unreliable here: `int* f()`, `const std::string& f()` and `auto f()` each have to come
/// out as the spelling the reader wrote.
///
/// `None` for a definition with nothing before its name — a constructor — and for a head this cannot read.
pub fn return_type_written_by(function: &cpp_parser::CppSyntaxNode) -> Option<String> {
    use cpp_parser::CppSyntaxKind;

    // **The type specifiers are a node**: a definition holds a `DeclSpecifierSeq` before its declarator, which is
    // where `int`, `const std::string` and `auto` live. This was the text before the name, which is the same
    // characters in most cases and the wrong ones in the cases that matter - a comment, a `[[nodiscard]]`, a
    // `virtual` keyword.
    let specifiers = function
        .children()
        .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::DeclSpecifierSeq)?;
    let mut written = specifiers.text().to_string().trim().to_string();

    // **And the pointer or reference belongs to the type while sitting outside that node**: `int* f()` writes its
    // `*` against the declarator, so the spelling this answers with has to put it back or `int*` reads as `int`.
    if let Some(declarator) = function.descendants().find(|node| {
        CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Declarator
            && node
                .descendants()
                .any(|inner| CppSyntaxKind::from(inner.kind()) == CppSyntaxKind::ParameterList)
    }) {
        for token in declarator
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
        {
            if CppSyntaxKind::from(token.kind()) == CppSyntaxKind::NameExpr {
                break;
            }
            match token.text().to_string().as_str() {
                "*" | "&" | "&&" => {
                    // Written against the type or against the name - the same type either way.
                    written.push_str(token.text().to_string().as_str());
                }
                _ => {}
            }
        }
    }

    if written.is_empty() {
        return None;
    }
    Some(written)
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
    run_check::<access_invisible::AccessInvisible>(&mut context);
    run_check::<missing_return::MissingReturn>(&mut context);
    run_check::<assignment_in_condition::AssignmentInCondition>(&mut context);
    run_check::<unnecessary_branch::UnnecessaryBranch>(&mut context);
    run_check::<undefined_field::UndefinedField>(&mut context);
    run_check::<argument_count::ArgumentCount>(&mut context);
    run_check::<param_type_check::ParamTypeCheck>(&mut context);
    run_check::<assign_type_check::AssignTypeCheck>(&mut context);
    run_check::<return_type_mismatch::ReturnTypeMismatch>(&mut context);
    // macro_redefined and include_not_found are being ported — see the note above their declarations.

    context.into_findings()
}



















