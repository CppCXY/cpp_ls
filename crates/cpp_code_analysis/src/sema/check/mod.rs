//! **Semantic checks**: what the analysis can say about a file that is *wrong*, as opposed to what it read.
//!
//! [`crate::FileDiagnostics::errors`] is the parser's answer — the text does not parse — and
//! [`crate::FileDiagnostics::notes`] is what the analysis knows but is not a complaint. This is the third
//! thing a diagnostics channel needs and the only one that was missing: a construct the grammar accepts and
//! the language does not.
//!
//! # One check, one file
//!
//! A check is a module of its own in this directory, and [`Checks::run`] is the list. The reason is not
//! tidiness: a check is a **claim**, and a claim is only worth showing if the reasoning behind it can be read.
//! A single `diagnostics` function with twelve `if`s in it is a place where the twelfth is written by whoever
//! is in a hurry, and the reader cannot tell which of the twelve were measured.
//!
//! # The contract: `Known::Yes` or `Known::No`, and nothing else
//!
//! **A check reports only what it knows.** [`Known`] has three values and this layer has two:
//!
//! ```text
//! Known::Yes(_)   the question was answered, and the answer is a problem      -> report
//! Known::No       the question was asked and the answer is a definite no      -> report, for a "not found"
//! Known::Unknown  the question was not answered, for a stated reason          -> report NOTHING
//! ```
//!
//! The third is not a gap in this layer, it is the whole design of the layer underneath it.
//! [`sema::resolve`](crate::sema::resolve) keeps the two "no"s apart on purpose, and its module documentation
//! says why: `Known::No` means a name is **nowhere**, and [`UnknownReason::NotDeclaredHere`] means it is
//! *somewhere else* — usually a header the analysis has not read. The first is a fact about the file; the
//! second is a fact about us.
//!
//! # Why a check that reports nothing is the ordinary case, and why that is the point
//!
//! The failure mode this layer has to avoid is not missing a problem, it is **inventing one**, because an
//! editor that underlines correct code is one the user turns off — and then it reports nothing at all, forever.
//! The measurement that keeps it honest is a corpus that is *known good*: the standard library. A check that
//! reports anything at all on a header it read correctly is wrong, and that is a test that can fail.
//!
//! The defect this rule was written from, measured: MSVC's `inline namespace __p2286` was not modelled, so 117
//! declarations of `<format>` were filed under `std::__p2286` instead of `std` and `std::format` could not be
//! found by its own name. A check that reported "unknown type name" would have fired on **all 117**, and been
//! wrong about all 117. The name was not missing; the scope was.
//!
//! # What a check is handed
//!
//! [`Checks`] — and it is deliberately *not* a [`Session`](crate::Session). A check that could ask the session
//! anything would eventually ask it to parse something, and a diagnostics channel whose cost is "one parse per
//! check per keystroke" is a channel that gets removed. What is here is a view of **one file** and the index as
//! it stands, which is what the answers above are made of.

use std::path::Path;

use cpp_parser::SourceRange;

use crate::{FileSummary, ProjectIndex};

pub mod a_macro_is_not_redefined;
pub mod an_argument_does_not_convert;
pub mod an_initializer_does_not_convert;
pub mod an_error_the_file_asks_for;
pub mod an_include_is_found;

/// **What a check found**, in the form a diagnostics channel shows it.
///
/// A `Finding` is a *claim about a place in a file*, so it carries the span and the sentence together: a
/// consumer that had to assemble the message from the parts would be a second author of it, and the two would
/// drift. The check that knows why it fired is the one that says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The span to underline — the whole construct, not just its first token.
    pub range: SourceRange,
    /// The name the finding is about, as the file spells it. Empty when the finding is not about one name.
    pub name: String,
    /// Which check reported it, as the module is named. For a consumer that filters, and for a test that has to
    /// say *which* claim it just refuted.
    pub check: &'static str,
    /// The sentence shown to the user. Written by the check, because the check is what knows.
    pub message: String,
}

/// # A fact defect this layer found, and could not check around
///
/// `no_object_has_type_void` was written — a variable whose type is `void`, which no object can have — and it was
/// **removed**, because the corpus test refused it: ten findings on MSVC's `omp_llvm.h` and its neighbours, all of
/// them this declaration:
///
/// ```cpp
/// extern void   __KAI_KMPC_CONVENTION  kmp_set_stacksize_s        (size_t);
/// ```
///
/// A function returning `void`, where the calling-convention **macro** sits between the type and the name. The
/// reading produces a `DeclFact` for it whose `kind` is `Variable`, whose `type_of` is `void`, and whose
/// `parameters` is **empty** — so every part of the declaration that would have told a check it is a function is
/// gone by the time the fact exists. The check was right about the fact it was handed; **the fact is wrong.**
///
/// Kept here rather than in a task list because the corpus test is what found it, and the next check about a
/// declared type will meet the same declaration. The fix belongs in the reading, not in a check.
///
/// Everything a check is given, and nothing else.
pub struct Checks<'a> {
    /// The file being checked, as the index spells it — the key to every answer below.
    pub path: &'a Path,
    /// The file's summary: the facts its own text produced. A check that needs only this is a check that
    /// cannot be wrong about another file.
    pub summary: &'a FileSummary,
    /// **The file's questions, and the only way a check asks them.**
    ///
    /// This replaced three fields — `index`, `tree` and `scopes` handed over separately — and the replacement is
    /// the point rather than a tidy-up. A check that could reach the index and the tree directly could ask
    /// `type_of_expression` with a resolver of its own, and four call sites did exactly that:
    ///
    /// ```text
    ///   type_of_expression(index, &mut |_: &Path| None, …)      inference gives up at the first header
    /// ```
    ///
    /// Measured on MSVC's `<vector>`: **1491 calls, 47–54 ms each, 3537 ms of one request**, nearly all of them
    /// asking about an initialiser whose type is written in a file the closure had already refused to open. Through
    /// the model the same question can be answered — and, once answered, is remembered for the rest of the request.
    /// A future check cannot repeat the mistake because there is nothing else to call.
    pub model: crate::sema::model::SemanticModel<'a>,
    /// **The file's own text**, as the ranges in its summary are offsets into.
    ///
    /// Here because a fact stores a *range* rather than the text it covers — deliberately, since a summary is
    /// written to disk and a range is smaller and cannot drift from the file it describes. A check that has to
    /// compare two pieces of the file's text, rather than merely point at them, is the caller that closes the
    /// loop: [`a_macro_is_not_redefined`] asks whether two `#define`s wrote the same replacement list, and the
    /// answer is in these bytes.
    pub source: &'a str,
}

impl<'a> Checks<'a> {
    /// **The file's tree** — through the model, which is the only place it lives now.
    ///
    /// Kept as an accessor rather than a field so that the three questions a check asks (`tree`, `scopes`,
    /// `index`) all come from one owner: a check cannot be handed a tree that disagrees with the scopes beside it.
    ///
    /// The returned reference is tied to `&self` rather than to `'a`, because the model now **owns** the borrow —
    /// the tree lives as long as this value does, and a caller that outlived it would be holding a tree whose view
    /// had been dropped.
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
    ///
    /// The one way a check asks this question. See [`Checks::model`] for what the alternative cost.
    pub fn type_of(
        &self,
        expression: &cpp_parser::CppSyntaxNode,
    ) -> crate::Known<(crate::sema::types::Type, std::path::PathBuf)> {
        self.model.type_of(expression)
    }
    /// Run every check, in the order a consumer should show them.
    ///
    /// **Sorted by position**, because the order checks happen to be listed in is an implementation detail and
    /// a diagnostics list sorted by it is a list that reshuffles when a check is added. Two findings at the
    /// same offset keep the order they were produced in, which is the order of this list — deliberate, so that
    /// the more specific check is written first and reads first.
    pub fn run(&self) -> Vec<Finding> {
        let mut findings = Vec::new();

        // **The timer wraps the check, not the append.** The first version of this took the check's answer as an
        // argument, so the call had already run by the time the guard was created and every check reported 0.00 ms
        // while the layer reported 3.5 s — the instrument was measuring a `Vec::append`. A closure of the *work*
        // is what makes the guard enclose it. See `crate::stages::check_trace`.
        let mut timed = |which: usize, work: &dyn Fn() -> Vec<Finding>| {
            let _t = crate::stages::check_trace::timing(which);
            let mut produced = work();
            findings.append(&mut produced);
        };

        timed(0, &|| an_include_is_found::the_file_it_names_is_not_there(self));
        timed(1, &|| {
            a_macro_is_not_redefined::no_name_is_defined_twice_with_a_different_body(self)
        });
        timed(2, &|| {
            an_error_the_file_asks_for::an_error_the_file_asks_for_is_reported(self)
        });
        // **The first check about a type.** Everything above is about the reading — a file that is not there, a
        // macro written twice, a directive that asks to fail. This one asks whether the program means what it says
        // about a type, and it is one line of `run` rather than a check per pair of types because it asks
        // [`Type::convertible_to`], the crate's single answer to *can a value of this type initialise one of that
        // type*. The parameter check will ask the same relation at a call site.
        //
        // Measured before it was enabled, on the corpus that keeps this layer honest — 100 MSVC headers, each
        // check counted on its own:
        //
        // ```text
        //   an_error_the_file_asks_for        9    the headers really do write `#error` for another target
        //   an_include_is_found               1    `<mscoree.h>` is not on this machine
        //   a_macro_is_not_redefined          2
        //   an_initializer_does_not_convert   0    <- this one, and zero is the number it has to be
        // ```
        //
        // **The count has to be taken per check.** It was read as a total once, the twelve belonged to the three
        // checks above, and this one was disabled for a day on the strength of it — the opposite mistake from the
        // one this layer's contract is written against, and the same lesson: a number is only an answer to the
        // question it was measured for.
        //
        // It cost **302 ms per file** when first written, because it searched the whole tree once *per
        // declaration*; that is fixed (one walk, then a lookup), and over these headers the whole layer now costs
        // about 21 ms per file.
        timed(3, &|| {
            an_initializer_does_not_convert::an_initializer_does_not_convert(self)
        });
        // **And the same relation at a call site.** The first version read `DeclFact::parameters`, which is the
        // *template* list, so every non-template function looked like it took nothing and the check reported 119
        // findings over 200 MSVC headers. It reads `parameter_list` — the list as written — and the corpus count is
        // the thing to check it by, per check and not in total.
        timed(4, &|| {
            an_argument_does_not_convert::an_argument_does_not_convert(self)
        });

        findings.sort_by_key(|finding| finding.range.start_offset);
        findings
    }
}
