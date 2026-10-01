//! **Alignment against a real compiler**: run the true preprocessor over the same file, put the two token streams
//! side by side, and report where they differ — by kind, not as an opinion.
//!
//! # Why this module is the most valuable one in the crate
//!
//! Every other module here answers a question about a reading, and the answer is only as good as the reading.
//! "Did our preprocessor choose the same branch", "did it expand that macro the way a compiler does", "did it find
//! that header" — those are not questions an argument settles. The plan's §5.0 puts the method this way:
//!
//! ```text
//! cl.exe /E /d1PP main.cpp     →     clang -E -dM main.cpp
//!         ↓
//! 它展开出的 token 序列 / 宏表 / 存活的分支
//!         ↓
//! 和 cook 出来的 RenderedUnit 比:token 数、文件边界、每个 #if 分支的取舍
//!
//! So this module does one thing: it makes **the compiler the oracle**. Two streams of tokens, one from
//! [`crate::RenderedUnit`] and one from a real `-E`, and a list of the places they disagree.
//!
//! # What is compared, and what deliberately is not
//!
//! Compared: the **spelling of every token, in order, with where it came from** —file and line. That is the plan's
//! list (token count, file boundaries, which branch of each `#if` was taken), and all three fall out of it: a branch
//! chosen differently shows up as a run of tokens present on one side and not the other, and a header resolved to
//! the wrong file shows up as the same tokens carrying different origins.
//!
//! Not compared, and the list matters as much as the other one:
//!
//! * **Whitespace, and therefore exact positions inside a line.** A rendering spells tokens separated by single
//!   spaces (see [`crate::preprocess::cooked::CookedStream::render`]); a compiler keeps the file's own layout. Line
//!   numbers are compared because the line markers survive `-E`, and comparing them is what makes "the branch was
//!   taken from another file" visible.
//! * **The macro table itself**, for now. `-dM` prints it and `Toolchain::builtin_macros` already reads it; putting
//!   the two tables side by side is the next step and it is a different comparison (a set of definitions, not a
//!   sequence of tokens). This module is the sequence.
//! * **Layout of the marker syntax.** MSVC and GCC spell their line markers differently and some of them quote the
//!   path; both are read, and both are treated as *not tokens* —see [`tokenize_preprocessed`].
//!
//! # What "different" is allowed to mean
//!
//! A diff is a list of edits, and an edit is not a diagnosis. Turning it into one is [`Difference::reason`], whose
//! whole job is to say which **mechanism** produced a difference rather than which tokens are on either side —because
//! the mechanisms are the milestones. `BranchDisagreement` is a condition-evaluation gap (§3.2 item 1),
//! `MissingHeader` is the include resolver, `ExpansionDisagreement` is the expander. A reading of this module that
//! ends in "17 differences" is useless; one that ends in "17 differences, 14 of them one unexpanded macro" is a work
//! item.
//!
//! # An edit is finer than a mechanism, and that is the trap
//!
//! The first version of this module read one edit at a time, and got the most common case wrong. `#define SIZE 1024`
//! that we fail to expand is **three** edits — our `SIZE` substituted, their `1024` and the `*` around it inserted —//! and read one at a time they are `UnexpandedMacro` and `MissingExpansion`: two work items, and neither is what
//! happened. One macro is what happened.
//!
//! So a [`Difference`] carries the shape of the **contiguous run** it belongs to — how many tokens went out, how many
//! came in — and the classification reads that. A run is what a reader of the two streams would call one movement of
//! tokens, and the report's counts are counts of movements. `one_macro_that_expands_to_several_tokens_is_one_movement`
//! is the test that pins it.
//!
//! # Why the two entry points are pure functions
//!
//! [`tokenize_preprocessed`] takes text and returns tokens; [`align`] takes two token lists and returns differences.
//! Neither runs a process, reads a file or knows what a compiler is. That is what makes the whole thing testable
//! against **recorded** `-E` output —the tests here do exactly that —on a machine with no compiler installed, and
//! it is what keeps the one part that does touch the world (`examples/align_preprocessor.rs`) to a page.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One token, as the comparison sees it: what it spells, and where it came from.
///
/// The file and the line are `Option` because each of them is genuinely unknown in one of the two streams: our own
/// rendering knows a token's file for every token (that is what [`crate::UnitSpan::file`] is) but not its line until
/// the file is read back, and a compiler's `-E` output states both for every token —except when it prints none,
/// which it does for a token it synthesised (a `##` paste has no file of its own).
///
/// **The path is shared** ([`Arc`]) because it is the same `PathBuf` for every token of a 40 000-token header, and a
/// comparison of a standard library's closure holds a few million of these. Cloning a path per token is the kind of
/// cost this crate has measured before and does not need to measure again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamToken {
    /// The token's spelling, exactly as it would be written.
    pub spelling: Box<str>,
    /// The file the token stands in —a path as the *producing* side spells it.
    pub file: Option<Arc<Path>>,
    /// The 1-based line in [`StreamToken::file`].
    pub line: Option<u32>,
}

impl StreamToken {
    pub fn new(spelling: impl Into<Box<str>>, file: Option<Arc<Path>>, line: Option<u32>) -> Self {
        StreamToken {
            spelling: spelling.into(),
            file,
            line,
        }
    }

    /// Where this token is, as `path:line`, for a report. `?` for a part nobody stated.
    pub fn origin(&self) -> String {
        match (&self.file, self.line) {
            (Some(path), Some(line)) => format!("{}:{line}", path.display()),
            (Some(path), None) => path.display().to_string(),
            (None, Some(line)) => format!("line {line}"),
            (None, None) => "?".to_string(),
        }
    }
}

/// Which side a stream came from —and it is not decoration: a difference with no *ours* is the compiler having
/// tokens we do not have at all (a branch we did not take, a header we did not find), and one with no *theirs* is the
/// other direction. [`Difference`] keeps both, but a report that prints them has to say which is which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The stream this crate produced —[`crate::RenderedUnit`], rendered and tokenized.
    Ours,
    /// The stream a real preprocessor produced.
    Theirs,
}

/// **One edit of the shortest script that turns our stream into the compiler's.**
///
/// `ours` or `theirs` may be absent, and the absent half is the information: a difference with no `theirs` is a token
/// we have and the compiler does not.
///
/// # An edit is finer than a mechanism
///
/// The first version of this module classified one edit at a time, and the mistake that came out of it is worth
/// recording. `#define SIZE 1024` that we expand and the compiler does not is *three* edits — our `SIZE` substituted,
/// their `1024` inserted —and one at a time they say `UnexpandedMacro` and `MissingExpansion`: two different work
/// items, and neither is what happened. One macro is what happened.
///
/// So a `Difference` carries the run it belongs to ([`Difference::run`], [`Difference::run_ours`],
/// [`Difference::run_theirs`]), which is the same shape a reader of the streams would see: **a contiguous movement of
/// tokens**. [`Difference::reason`] reads that shape, and a report is a list of movements rather than of edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    /// Where in the comparison this is —the index in the aligned sequence, for a report that says "the 12th".
    pub at: usize,
    /// The token we have, when the edit is ours to explain.
    pub ours: Option<StreamToken>,
    /// The token the compiler has.
    pub theirs: Option<StreamToken>,
    /// **How many edits this one is part of** —the size of the contiguous run it belongs to, `1` for an isolated
    /// edit. Every difference in a run carries the whole run's size, because the run is what a mechanism acts on.
    pub run: usize,
    /// How many tokens the run takes out of **our** stream, or `0` when the run is the compiler having something we
    /// do not.
    ///
    /// The two counts are what separate the two mechanisms that look identical one edit at a time, and neither is
    /// derivable from [`Difference::run`]: a run of three edits can be *one* macro that expands to three tokens (one
    /// out, three in) or *three* names that survived (three out, none in), and those are different work items.
    pub run_ours: usize,
    /// How many tokens the run adds from **the compiler's** stream.
    pub run_theirs: usize,
}

impl Difference {
    /// The side this difference is *about*: ours when we have a token here, theirs otherwise.
    ///
    /// A difference always has at least one half, so this is total —and it is the side a report should quote,
    /// because it is the side the fix is in.
    pub fn side(&self) -> Side {
        if self.ours.is_some() {
            Side::Ours
        } else {
            Side::Theirs
        }
    }

    /// Is this one edit, rather than part of a run? What a report quotes in full.
    pub fn is_isolated(&self) -> bool {
        self.run <= 1
    }
}

/// **Why two streams differ at one place**, as a mechanism rather than a pair of tokens.
///
/// The list is the milestones, and that is the design: a report whose differences are classified is a work list, and
/// one whose differences are not is a wall of text. Each variant names something that can be *fixed*, which is what
/// makes this better than "these 300 tokens do not match".
///
/// Classification is from the shape of the pair and from what came before it. It is deliberately a **heuristic**, and
/// [`Difference::reason`]'s doc says so: a wrong guess here costs a report that points at the wrong milestone, while
/// refusing to guess costs a report nobody can use. What it must never do is claim a difference is benign.
///
/// `Ord` because a report groups by mechanism and a grouping has to be **stable between runs** —a `BTreeMap` over
/// this is what makes two runs of the same comparison print the same lines in the same order, which is the property a
/// recorded count per mechanism needs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    /// A name we have that the compiler does not: a macro that should have been expanded.
    UnexpandedMacro,
    /// A name the compiler has that we do not: its expansion came from a header we did not read.
    MissingExpansion,
    /// A numeric literal on one side where the other has a name —the classic fingerprint of `#if` disagreement:
    /// one side replaced `FOO` with `1`, the other with `0` or nothing.
    ConditionDisagreement,
    /// **A region one side compiled and the other did not** —tokens present in one stream and absent from the other,
    /// in a run.
    ///
    /// The variant that exists because a diff is finer than a mechanism. `#if _MSC_VER >= 1930` deciding differently
    /// moves a whole branch: dozens of tokens present on one side only, which read one edit at a time are "a name we
    /// have that they do not" (i.e. `UnexpandedMacro`) —the wrong work item for the right evidence.
    ///
    /// The plan's §3.2 item 1 is what this points at: *"内建宏表,来源优先? `compile_commands.json` ?`cl.exe` /
    /// `clang` 探测"*. A run of these is what a missing predefined macro looks like from here.
    BranchDisagreement,
    /// **One name on one side and its replacement list on the other** —both streams have tokens here, and not the
    /// same number of them.
    ///
    /// Distinct from [`Reason::UnexpandedMacro`], which is the same mechanism seen at its smallest: a body of one
    /// token looks like a name that did not expand, and a body of ten looks like this. Both are the expander's work
    /// item, and separating them says how big the gap is rather than only that there is one.
    ExpansionDisagreement,
    /// The compiler's stream carries an `#include` whose header we never read at all.
    ///
    /// The one variant that is *certain* rather than heuristic: `includes` is built from the compiler's own line
    /// markers, so a path that appears there and nowhere in ours is a file this reading never had.
    MissingHeader,
    /// Neither side's shape says which mechanism; the pair is quoted and a reader decides.
    Unknown,
}

impl Reason {
    /// What to do about it —the plan's rule 3, that every uncertain answer has to say what it is uncertain about
    /// *and* what that means for whoever reads it.
    ///
    /// `&self` rather than `self`: every variant is a unit, so this is a table lookup and there is nothing to move.
    pub fn advice(&self) -> &'static str {
        match self {
            Reason::UnexpandedMacro => {
                "a name survived into our stream: the expander had no body for it (see `Configuration`) \
                 or the invocation did not parse as a call"
            }
            Reason::MissingExpansion => {
                "the compiler expanded something we never had: the file that defines it is not in our closure \
                 (an unresolved include, or a header we read as empty)"
            }
            Reason::ConditionDisagreement => {
                "the two readings took different branches of an `#if`: a predefined macro is missing from our \
                 table, or one of them evaluated to the wrong value"
            }
            Reason::BranchDisagreement => {
                "a whole region moved, not one token: an `#if` was decided the other way, so look for the \
                 predefined macro it asks about before looking at the parser"
            }
            Reason::ExpansionDisagreement => {
                "one name on one side and its replacement list on the other: the expander had no body for it, or \
                 substituted differently —compare the two tables for the name quoted"
            }
            Reason::MissingHeader => {
                "the compiler read a header this reading never entered: the include resolver could not find it"
            }
            Reason::Unknown => "the shape of the pair does not identify a mechanism; read the quoted tokens",
        }
    }
}

impl Difference {
    /// **Which mechanism produced the movement this edit belongs to**, as far as its shape can say.
    ///
    /// Heuristic, and honestly so. What it reads is the run's shape —how many tokens went out, how many came in,
    /// and what the first of each side spells —because that is the coarsest thing a diff can see and the finest
    /// thing it can see *reliably*.
    ///
    /// The rules, in order:
    ///
    /// 1. a token only the compiler has, whose origin is a file our stream never mentions ?`MissingHeader`. The one
    ///    **certain** rule, which is why it outranks every shape below: whatever the tokens look like, the reason a
    ///    whole header is missing from our side is that we never read it;
    /// 2. one token out, none in, and it is a name ?`UnexpandedMacro`;
    /// 3. none out, one in, and it is a name ?`MissingExpansion`;
    /// 4. **one side of the run is empty** ?`BranchDisagreement`. Tokens present on one side and absent on the other,
    ///    in a run, is a region that one reading compiled and the other did not —the fingerprint of an `#if` decided
    ///    the other way, and the work item the plan's §3.2 item 1 (the builtin macro table) exists for;
    /// 5. **both sides have tokens but not the same number** ?`ExpansionDisagreement`: one name on one side and its
    ///    replacement list on the other;
    /// 6. a name against a **number** ?`ConditionDisagreement` (`#if FOO > 2` answered `1` on one side and `0` on
    ///    the other is exactly this shape);
    /// 7. anything else ?`Unknown`.
    ///
    /// `our_files` is the set of paths our stream mentions.
    ///
    /// # What this deliberately does not try to do
    ///
    /// It does **not** reconstruct which `#if` differed, or name the macro. A diff is a list of edits and has no
    /// memory of what produced them; a report that invented a condition would be guessing with a straight face, which
    /// is the one thing the plan's rule 3 forbids. [`Reason::advice`] says what to *look at* instead, and the per-file
    /// token counts in `align_preprocessor`'s report are what localise it.
    pub fn reason(&self, our_files: &std::collections::HashSet<&Path>) -> Reason {
        let is_a_name = |token: &Option<StreamToken>| {
            token.as_ref().is_some_and(|token| {
                let mut characters = token.spelling.chars();
                characters
                    .next()
                    .is_some_and(|first| first == '_' || first.is_alphabetic())
            })
        };
        let is_a_number = |token: &Option<StreamToken>| {
            token
                .as_ref()
                .is_some_and(|token| token.spelling.starts_with(|c: char| c.is_ascii_digit()))
        };

        // 1. A file the compiler read and we never entered.
        if let Some(theirs) = &self.theirs
            && let Some(file) = &theirs.file
            && !our_files.contains(file.as_ref())
        {
            return Reason::MissingHeader;
        }

        // 2 and 3: one token each way, and it is a name. The isolated case, which is what a single unexpanded
        // invocation looks like.
        if self.run_ours == 1 && self.run_theirs == 0 && is_a_name(&self.ours) {
            return Reason::UnexpandedMacro;
        }
        if self.run_ours == 0 && self.run_theirs == 1 && is_a_name(&self.theirs) {
            return Reason::MissingExpansion;
        }

        // 4. One side of the run is empty: a region, not a name. Read one edit at a time this is "a name we have and
        // they do not", which is why the run's shape has to be consulted rather than the pair's.
        if self.run_ours == 0 || self.run_theirs == 0 {
            return Reason::BranchDisagreement;
        }

        // 5. Both sides moved, different amounts: a substitution, which is a name and its expansion.
        if self.run_ours != self.run_theirs {
            return Reason::ExpansionDisagreement;
        }

        // 6. A name against a number: a condition that evaluated differently.
        if is_a_name(&self.ours) != is_a_name(&self.theirs)
            && (is_a_number(&self.ours) || is_a_number(&self.theirs))
        {
            return Reason::ConditionDisagreement;
        }

        Reason::Unknown
    }
}

/// **The result of putting two streams side by side**: where they differ, and how much of them agreed.
///
/// The two counts are what make a reading comparable between runs —the plan's §5.2 requires that the number of
/// differences only ever goes down, and a count of *matched* tokens is what keeps a run that got shorter from looking
/// like a run that got better.
///
/// # The order of `differences` is part of the contract
///
/// Sorted by [`Difference::at`], ascending, and every field derived from that order is only meaningful because of it:
/// a run is a group of *adjacent* positions, and adjacency is not a property of an unordered set. The traceback that
/// produces them does **not** guarantee this —it walks backwards and a run of deletions comes out in descending
/// order —so the sort is in [`align`] rather than left to whoever reads the list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Alignment {
    pub differences: Vec<Difference>,
    /// Tokens the two streams agreed on, spelling for spelling.
    pub matched: usize,
    /// Whether the comparison stopped early —see [`MAX_EDIT_DISTANCE`].
    ///
    /// **Load-bearing, and the one field a caller must read before believing [`Alignment::differences`] is
    /// exhaustive.** Two streams that share almost nothing (one of them empty, or the wrong file entirely) have a
    /// difference count proportional to their *length*, which for a standard library header is millions —so the
    /// walk is bounded, and a bounded walk has to say that it was bounded rather than present a prefix as a whole.
    pub truncated: bool,
}

impl Alignment {
    /// Differences grouped by mechanism, largest group first —what a report should print.
    ///
    /// The grouping is a `BTreeMap` over [`Reason`], so the order two mechanisms come out in does not depend on the
    /// order their differences happened to appear in the stream. That is what makes the line a report records —    /// `UnexpandedMacro=14 ConditionDisagreement=3` —comparable between two runs, which is the plan's §5.2
    /// requirement that the difference count only ever go down.
    pub fn by_reason(&self, our_files: &std::collections::HashSet<&Path>) -> Vec<(Reason, usize)> {
        let mut counts: std::collections::BTreeMap<Reason, usize> = std::collections::BTreeMap::new();
        for difference in &self.differences {
            *counts.entry(difference.reason(our_files)).or_default() += 1;
        }

        let mut counted: Vec<(Reason, usize)> = counts.into_iter().collect();
        counted.sort_by(|(left_reason, left), (right_reason, right)| {
            right
                .cmp(left)
                .then_with(|| left_reason.cmp(right_reason))
        });
        counted
    }

    /// Did the two streams agree as sequences of spellings?
    pub fn agrees(&self) -> bool {
        self.differences.is_empty() && !self.truncated
    }

    /// **The files the compiler read and this reading never entered**, each once, in path order.
    ///
    /// The names behind [`Reason::MissingHeader`], which is the one **certain** classification this module makes: it
    /// is asked of a difference whose token carries a file our stream does not mention at all, so it is a fact about
    /// the two readings rather than a shape being interpreted. A count of those differences says how big the gap is;
    /// this says *which* search went differently, and that is what a caller can act on.
    ///
    /// It earns its own method because of a measurement: on `#include <vector>` against MSVC's own STL, **46 files**
    /// came out this way while the two streams' total lengths differed by 252 tokens. So the gap was never "we read
    /// the same files slightly differently" — it was whole files one side read and the other did not, which is a
    /// different repair.
    pub fn headers_we_never_entered(
        &self,
        our_files: &std::collections::HashSet<&Path>,
    ) -> Vec<&Path> {
        let mut missing: Vec<&Path> = self
            .differences
            .iter()
            .filter(|difference| difference.reason(our_files) == Reason::MissingHeader)
            .filter_map(|difference| difference.theirs.as_ref()?.file.as_deref())
            .collect();

        missing.sort_unstable();
        missing.dedup();
        missing
    }
}

/// How far apart two streams may be before the comparison stops looking for the smallest diff.
///
/// The diff is Myers', whose cost is proportional to the **edit distance** and not to the stream lengths —which is
/// why it is the right algorithm here and why this bound is the only thing that makes an unbounded input safe. Two
/// streams that are nearly the same (the ordinary case, and the only interesting one) finish in a few passes. Two
/// streams that are unrelated do not converge at all: the distance is the length of the longer one, and for a
/// 40 000-token header that is 40 000 snapshots of a 40 000-wide array.
///
/// Past this the comparison returns what it has and sets [`Alignment::truncated`]. The bound is generous on purpose:
/// the differences worth classifying are the ones a *reading* produced, and a reading that is 4 000 tokens away from
/// the compiler is not being classified, it is being rewritten.
pub const MAX_EDIT_DISTANCE: usize = 4096;

/// **Put two token streams side by side** and report the smallest set of edits that explains their difference.
///
/// Myers' algorithm —the same one `git diff` uses —chosen over the textbook LCS table for a reason that is
/// entirely about this crate's inputs: the table is `O(n·m)` in **memory**, and the streams here are a standard
/// library's whole closure (millions of tokens). Myers' is `O((n + m)·d)` in time and `O(d²)`-ish in memory, where
/// `d` is the number of differences —which for the case this module exists for is tiny.
///
/// The result is a list of [`Difference`]s in stream order, each naming the token on one side or both, so a reader
/// can see a substitution as one entry rather than as a delete followed by an insert.
pub fn align(ours: &[StreamToken], theirs: &[StreamToken]) -> Alignment {
    let mut alignment = Alignment::default();

    // **The common prefix and suffix are taken off first**, and that is not a micro-optimisation: it is what makes
    // the bound in `MAX_EDIT_DISTANCE` irrelevant for the ordinary case. Two readings of one file agree for
    // thousands of tokens at a time, and the diff only ever has to run over the region between two disagreements.
    let mut prefix = 0usize;
    while prefix < ours.len() && prefix < theirs.len() && ours[prefix].spelling == theirs[prefix].spelling {
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < ours.len() - prefix
        && suffix < theirs.len() - prefix
        && ours[ours.len() - 1 - suffix].spelling == theirs[theirs.len() - 1 - suffix].spelling
    {
        suffix += 1;
    }

    // Settled below, once the script says how many of our tokens it had to touch.
    alignment.matched = 0;

    let our_middle = &ours[prefix..ours.len() - suffix];
    let their_middle = &theirs[prefix..theirs.len() - suffix];

    let (edits, truncated) = anchored_edit_script(our_middle, their_middle, 0);
    alignment.truncated = truncated;

    for (at, ours_at, theirs_at) in edits {
        alignment.differences.push(Difference {
            at: prefix + at,
            ours: ours_at.map(|index| our_middle[index].clone()),
            theirs: theirs_at.map(|index| their_middle[index].clone()),
            // Filled in below, once the whole script is in hand: a run is a property of the sequence and cannot be
            // known while the sequence is still being built.
            run: 1,
            run_ours: 0,
            run_theirs: 0,
        });
    }

    // **Sorted by position before anything reads them.** The traceback walks backwards and can emit a run of pure
    // deletions in *descending* order —`["a", "b"]` against `[]` comes back as "delete at 1, delete at 0" —and every
    // consumer here reasons about `at` as a position in the stream. Grouping an unsorted list would see two
    // movements where the streams moved once, and a report would count one `#if` as two.
    alignment
        .differences
        .sort_by_key(|difference| difference.at);

    alignment.matched = ours.len()
        - alignment
            .differences
            .iter()
            .filter(|difference| difference.ours.is_some())
            .count();

        // **Group the edits into runs**, which is what the three `run*` fields mean: every edit that sits one slot after
    // the one before it belongs to the same movement of tokens. Now that the list is ordered, one forward pass is
    // enough.
    group_into_runs(&mut alignment.differences);

    alignment
}

/// Give every difference in a contiguous group the size of that group, and the group's two token counts.
///
/// Contiguity is `at == previous.at + 1`: the streams agreed on nothing in between. A gap is a real boundary —the
/// two agreed for at least one token there, so whatever moved on either side of it moved for two different reasons.
///
/// The counts are what [`Difference::reason`] reads, and they are counted **over the group** rather than per edit:
/// one macro that expands to three tokens is one token out and three in, and no single edit of that movement knows
/// it.
fn group_into_runs(differences: &mut [Difference]) {
    let mut start = 0usize;
    while start < differences.len() {
        let mut end = start + 1;
        while end < differences.len() && differences[end].at == differences[end - 1].at + 1 {
            end += 1;
        }

        let run = end - start;
        let out = differences[start..end]
            .iter()
            .filter(|difference| difference.ours.is_some())
            .count();
        let added = differences[start..end]
            .iter()
            .filter(|difference| difference.theirs.is_some())
            .count();

        for difference in &mut differences[start..end] {
            difference.run = run;
            difference.run_ours = out;
            difference.run_theirs = added;
        }
        start = end;
    }
}

/// **The window widths anchors are looked for at**, widest first.
///
/// A window that occurs once in each stream is a place the two readings certainly agree about, and it is what lets
/// a multi-hundred-thousand-token comparison be cut into pieces the table below can afford. Wide first because a
/// wide window is unique by construction; narrower only for the gaps the wide one could not cut.
const ANCHOR_WIDTHS: [usize; 4] = [24, 12, 6, 3];

/// **A script over streams too long for one table**, made by cutting them at agreed places first.
///
/// [`shortest_edit_script`] is bounded at [`MAX_DP_CELLS`], and a standard library's closure is a half-million tokens
/// a side: handed whole, it gave up at the first disagreement and called **every token** a difference, which is not a
/// measurement. This finds windows that occur exactly once in each stream, keeps the longest chain of them that is in
/// the same order on both sides (a patience diff), and runs the table only between consecutive anchors. Where a gap
/// is still too large, it is cut again with a narrower window; where no window cuts it, the table's own honest
/// "truncated" answer stands.
fn anchored_edit_script(
    ours: &[StreamToken],
    theirs: &[StreamToken],
    width_index: usize,
) -> (Vec<(usize, Option<usize>, Option<usize>)>, bool) {
    if ours.len().saturating_mul(theirs.len()) <= MAX_DP_CELLS || width_index >= ANCHOR_WIDTHS.len() {
        return shortest_edit_script(ours, theirs);
    }

    let width = ANCHOR_WIDTHS[width_index];
    let anchors = unique_anchors(ours, theirs, width);
    if anchors.is_empty() {
        return anchored_edit_script(ours, theirs, width_index + 1);
    }

    let mut edits = Vec::new();
    let mut truncated = false;
    let (mut our_cursor, mut their_cursor) = (0usize, 0usize);

    let mut gap = |our_from: usize, our_to: usize, their_from: usize, their_to: usize| {
        let (found, cut) = anchored_edit_script(
            &ours[our_from..our_to],
            &theirs[their_from..their_to],
            width_index + 1,
        );
        truncated |= cut;
        for (at, our_index, their_index) in found {
            edits.push((
                at + our_from,
                our_index.map(|index| index + our_from),
                their_index.map(|index| index + their_from),
            ));
        }
    };

    for (our_at, their_at) in anchors {
        if our_at < our_cursor || their_at < their_cursor {
            continue;
        }
        gap(our_cursor, our_at, their_cursor, their_at);
        our_cursor = our_at + width;
        their_cursor = their_at + width;
        while our_cursor < ours.len()
            && their_cursor < theirs.len()
            && ours[our_cursor].spelling == theirs[their_cursor].spelling
        {
            our_cursor += 1;
            their_cursor += 1;
        }
    }
    gap(our_cursor, ours.len(), their_cursor, theirs.len());

    (edits, truncated)
}

/// The windows of `width` tokens that occur **exactly once** in each stream, as `(ours, theirs)` start positions in
/// our order, restricted to the longest chain that is also increasing in theirs.
fn unique_anchors(ours: &[StreamToken], theirs: &[StreamToken], width: usize) -> Vec<(usize, usize)> {
    use std::collections::HashMap;

    if ours.len() < width || theirs.len() < width {
        return Vec::new();
    }

    // `Some(position)` the first time a window is seen, `None` once it has been seen twice.
    let windows = |tokens: &[StreamToken]| -> HashMap<u64, Option<usize>> {
        let mut seen: HashMap<u64, Option<usize>> = HashMap::new();
        for (start, window) in tokens.windows(width).enumerate() {
            seen.entry(hash_window(window))
                .and_modify(|entry| *entry = None)
                .or_insert(Some(start));
        }
        seen
    };

    let theirs_windows = windows(theirs);
    let ours_windows = windows(ours);

    let mut pairs: Vec<(usize, usize)> = ours_windows
        .iter()
        .filter_map(|(hash, ours_at)| {
            let (ours_at, theirs_at) = ((*ours_at)?, (*theirs_windows.get(hash)?)?);
            // A hash agreeing is not the windows agreeing.
            let same = ours[ours_at..ours_at + width]
                .iter()
                .zip(&theirs[theirs_at..theirs_at + width])
                .all(|(one, other)| one.spelling == other.spelling);
            same.then_some((ours_at, theirs_at))
        })
        .collect();
    pairs.sort_unstable();

    // Longest increasing subsequence by `theirs`, with the predecessor links to read the chain back.
    let mut tails: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; pairs.len()];
    for index in 0..pairs.len() {
        let theirs_at = pairs[index].1;
        let slot = tails.partition_point(|&tail| pairs[tail].1 < theirs_at);
        previous[index] = slot.checked_sub(1).map(|before| tails[before]);
        if slot == tails.len() {
            tails.push(index);
        } else {
            tails[slot] = index;
        }
    }

    let mut chain = Vec::with_capacity(tails.len());
    let mut at = tails.last().copied();
    while let Some(index) = at {
        chain.push(pairs[index]);
        at = previous[index];
    }
    chain.reverse();
    chain
}

fn hash_window(window: &[StreamToken]) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for token in window {
        token.spelling.hash(&mut hasher);
    }
    hasher.finish()
}
/// Myers' shortest edit script over two slices, as `(position, our index, their index)` triples in stream order.
/// **How far the comparison may go before it stops looking for the smallest script.**
///
/// The diff below is a table whose size is the **product** of the two middle lengths, and the middle is what is left
/// after the common prefix and suffix are trimmed away —which for two readings of one file is a handful of tokens.
/// It is only large when the two streams are largely unrelated, and then the shortest script is not worth finding:
/// an unrelated stream is a bug in the comparison, not a reading to classify.
///
/// So the table is bounded, and past the bound the answer is the **honest** one rather than an approximation: every
/// token on one side deleted and every token on the other inserted, with `truncated` set. A caller that reports those
/// as differences to fix would be reporting an unrelated stream as a work list, which is why the flag is not optional.
///
/// Four million cells is 16 MB of `u32` —large enough that no *reading* of a real file reaches it.
const MAX_DP_CELLS: usize = 4_000_000;

/// **The shortest script that turns our stream into theirs**, as `(position, our index, their index)` triples in
/// stream order.
///
/// Returns `(edits, truncated)`. A triple with both indices is a **substitution** —one difference naming what
/// replaced what —and one with a single index is a token that only one side has.
///
/// # Why a table rather than Myers
///
/// The version before this was Myers' algorithm, and it was wrong in three separate ways that only showed up once the
/// tests ran: the diagonal array was sized for the grid rather than for the walk (so every one-token substitution
/// panicked on an index underflow), the traceback walked `x` and `y` back independently (so a deletion and an
/// insertion two slots apart were read as one substitution), and the "same slot" test that was supposed to group them
/// did not. Each fix was plausible and the next test found the next bug.
///
/// A longest-common-subsequence table has one invariant, and it is the one that matters: **the script it produces
/// replays**. Walking the table backwards from `(n, m)` to `(0, 0)` visits every cell once, and the moves are fixed by
/// the table —there is no diagonal arithmetic to get wrong and no snapshot to index. The cost is memory, which the
/// bound above is what makes acceptable, and the size is what the common prefix and suffix trimming is what makes
/// small.
///
/// The round-trip property is asserted by `a_script_replays_our_stream_as_theirs`, because that is the property a
/// difference list has to have and the one the Myers version silently violated.
fn shortest_edit_script(
    ours: &[StreamToken],
    theirs: &[StreamToken],
) -> (Vec<(usize, Option<usize>, Option<usize>)>, bool) {
    let (n, m) = (ours.len(), theirs.len());

    if n == 0 && m == 0 {
        return (Vec::new(), false);
    }
    if n == 0 {
        return ((0..m).map(|at| (at, None, Some(at))).collect(), false);
    }
    if m == 0 {
        return ((0..n).map(|at| (at, Some(at), None)).collect(), false);
    }

    if n.saturating_mul(m) > MAX_DP_CELLS {
        // Past the bound: everything is an edit, and the flag says so.
        let mut edits = Vec::with_capacity(n + m);
        for at in 0..n {
            edits.push((at, Some(at), None));
        }
        for at in 0..m {
            edits.push((n + at, None, Some(at)));
        }
        return (edits, true);
    }

    // `lengths[i][j]` is the length of the longest common subsequence of `ours[i..]` and `theirs[j..]`, filled from
    // the bottom right. `(n + 1) * (m + 1)` cells, one `u32` each.
    let width = m + 1;
    let mut lengths = vec![0u32; (n + 1) * width];
    let cell = |row: usize, column: usize| row * width + column;

    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lengths[cell(i, j)] = if ours[i].spelling == theirs[j].spelling {
                lengths[cell(i + 1, j + 1)] + 1
            } else {
                lengths[cell(i + 1, j)].max(lengths[cell(i, j + 1)])
            };
        }
    }

    // **Walk the table forwards**, which is what makes the result a script rather than a pile of edits: at each cell
    // either the two tokens are the same (they match, and neither is an edit) or one of them is deleted. A deletion
    // that happens where the *other* side also has to lose a token is published as a substitution, and that is the
    // grouping rule — `queue` collects one side's deletions and the other's insertions while they are contiguous.
    //
    // # The two coordinate systems, and the bug that came from mixing them
    //
    // A run of edits is published at a position in the **whole script**, and the tokens it names are indices in
    // `ours` and `theirs`. The first version of this computed the position from `i + j - edits` and the token indices
    // from `i`, which are the same number only while nothing has matched: it was right for a difference at the very
    // start and wrong by the number of matched tokens after it. So `matched` is tracked explicitly and both numbers
    // come from it — `from = i - deleted` is the position, `i - deleted` is the token, and they are equal *because*
    // a match advances both cursors and the script together.
    let mut edits: Vec<(usize, Option<usize>, Option<usize>)> = Vec::new();
    let mut queue: Vec<(bool, bool)> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut matched = 0usize;

    while i < n || j < m {
        let same = i < n && j < m && ours[i].spelling == theirs[j].spelling;

        if same {
            flush(&mut edits, &mut queue, i, matched, i, j);
            i += 1;
            j += 1;
            matched += 1;
            continue;
        }

        // Not a match: one of the two has to go. Which one is decided by the table, and the tie (`>=`) prefers
        // deleting from ours, which keeps a substitution's two halves adjacent rather than separated by a match.
        let delete_ours = if i >= n {
            false
        } else if j >= m {
            true
        } else {
            lengths[cell(i + 1, j)] >= lengths[cell(i, j + 1)]
        };

        if delete_ours {
            queue.push((true, false));
            i += 1;
        } else {
            queue.push((false, true));
            j += 1;
        }
    }

    flush(&mut edits, &mut queue, i, matched, i, j);
    (edits, false)
}

/// **Publish the edits queued for one position** as the smallest set of differences that explains them.
///
/// `our_run` and `their_run` are the cursors *after* the queued tokens were consumed, and `matched` is how many
/// tokens have matched so far. A deletion and an insertion at one position are **one** difference with both halves
/// — a substitution, which is what a reader wants: two entries that both say "ours: nothing, theirs: `NEW`" is one
/// difference reported twice, with neither half saying what replaced what.
///
/// Whatever is left over comes out one-sided, which is the truth about those tokens: a replacement list longer than
/// the name it replaced really is one name gone and three tokens arrived.
///
/// # The two coordinate systems, and the bug that came from mixing them
///
/// The position a difference is published at is a place in the **whole script**; the token it names is an index in
/// `ours` or `theirs`. The first version computed the position from the number of matched tokens *after* the run
/// instead of before it, which is the same number only while nothing has matched — right for a difference at the very
/// start, and wrong by the number of matched tokens for every one after that. The fix is not a different formula: a
/// match advances both cursors and the script together, so `cursor - run_length` is **both** the token index and the
/// position, and there is only one number to get right.
fn flush(
    edits: &mut Vec<(usize, Option<usize>, Option<usize>)>,
    queue: &mut Vec<(bool, bool)>,
    _cursor: usize,
    _matched: usize,
    our_run: usize,
    their_run: usize,
) {
    let deleted = queue.iter().filter(|(deleted, _)| *deleted).count();
    let inserted = queue.iter().filter(|(_, inserted)| *inserted).count();

    // Both numbers, from the one that cannot be wrong: a cursor minus the length of the run it just consumed.
    let our_first = our_run - deleted;
    let their_first = their_run - inserted;
    let from = our_first;

    if deleted > 0 && inserted > 0 {
        // One substitution for the pair, then whichever side has more left over.
        edits.push((from, Some(our_first), Some(their_first)));
        for offset in 1..deleted {
            edits.push((from + offset, Some(our_first + offset), None));
        }
        for offset in 1..inserted {
            edits.push((from + offset, None, Some(their_first + offset)));
        }
    } else if deleted > 0 {
        for offset in 0..deleted {
            edits.push((from + offset, Some(our_first + offset), None));
        }
    } else {
        for offset in 0..inserted {
            edits.push((from + offset, None, Some(their_first + offset)));
        }
    }

    queue.clear();
}

/// **The stream with every pragma taken out**, so two compilers' different ways of *carrying* one stop being
/// differences.
///
/// Measured against `cl /E` on `<format>`: the compiler prints `#pragma` lines at its own place in the output (a
/// header's `#pragma comment` arrives before the `#pragma detect_mismatch` that precedes it in the source), and it
/// rewrites `_Pragma("warning(push)")` into `__pragma(warning(push))`. Neither is a *reading* of the program — a
/// pragma is a message to the compiler, and no declaration depends on it — but together they were three thousand
/// differences that buried the ones that were.
///
/// What goes: a `#` `pragma` line (every following token on the same file and line), and a `_Pragma(…)` or
/// `__pragma(…)` operator with its balanced parenthesis. What stays: everything else, including the tokens either
/// side of them. A comparison that wants to notice a wrong `#pragma once` keeps the stream as it is and does not
/// call this.
pub fn without_pragmas(tokens: &[StreamToken]) -> Vec<StreamToken> {
    let mut kept = Vec::with_capacity(tokens.len());
    let mut index = 0usize;

    let same_line = |one: &StreamToken, other: &StreamToken| one.file == other.file && one.line == other.line;

    while index < tokens.len() {
        let token = &tokens[index];

        if &*token.spelling == "#"
            && tokens.get(index + 1).is_some_and(|next| &*next.spelling == "pragma" && same_line(token, next))
        {
            index += 2;
            while index < tokens.len() && same_line(token, &tokens[index]) {
                index += 1;
            }
            continue;
        }

        if matches!(&*token.spelling, "_Pragma" | "__pragma")
            && tokens.get(index + 1).is_some_and(|next| &*next.spelling == "(")
        {
            let mut depth = 0usize;
            let mut end = index + 1;
            while end < tokens.len() {
                match &*tokens[end].spelling {
                    "(" => depth += 1,
                    ")" => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                end += 1;
            }
            index = end + 1;
            continue;
        }

        kept.push(token.clone());
        index += 1;
    }

    kept
}
/// **Read the tokens out of a real preprocessor's output**, dropping its line markers and keeping the position they
/// state.
///
/// The markers are the whole reason this cannot be `cpp_parser::lex(text)` and done: a compiler's `-E` output is not
/// a file, it is a **concatenation of every file it read** —the directive's whole purpose is to say which one the
/// text after it came from. So the markers are parsed rather than skipped, and every token gets the file and line
/// they announced. That is exactly the plan's "file boundaries" comparison, and it is why this function returns
/// [`StreamToken`]s rather than plain spellings.
///
/// # The marker syntaxes, all of them
///
/// ```text
/// # 12 "C:\\…\\xstring" 2 3 4      GCC and Clang (the flags are the enter/leave/system hints)
/// #line 12 "C:\\…\\xstring"        MSVC, and C's own `#line`
/// # 12                            a line with no file: the path does not change
/// ```
///
/// Anything else beginning with `#` is **not** a marker and is kept as tokens —`#pragma` lines survive `-E`, and a
/// comparison that dropped every `#`-line would stop noticing a `#pragma once` this reading got wrong.
///
/// # The rule for the line number, which is the one thing here that is easy to get wrong
///
/// A marker says *"the next line of output came from line N of file F"*. So each token's line is the position the
/// output has reached **since** the marker, counting output newlines —not the marker's own number for everything
/// that follows it. Getting this wrong is invisible in a one-line fixture and wrong in every real file.
///
/// # What is dropped
///
/// Whitespace and comments, by the lexer, because neither side has them: our stream is a rendering with one space
/// between tokens and no comments at all. Comparing them would make every run differ on layout and say nothing about
/// preprocessor behaviour —which is the comparison being made.
pub fn tokenize_preprocessed(text: &str) -> Vec<StreamToken> {
    let (tokens, _) = cpp_parser::lex(text, &cpp_parser::LexerConfig::default());
    let mut out: Vec<StreamToken> = Vec::with_capacity(tokens.len());

    // What the last marker announced, and how many **newlines** have gone by since. `None` until a marker is seen,
    // which is the honest state: a compiler that printed none (a fragment, a hand-written fixture) says nothing about
    // where its tokens are.
    //
    // A counter of newlines rather than of *tokens*: the lexer emits `Whitespace` as a token of its own, so most lines
    // are several tokens long and counting them would put every token on a line of its own —which is the bug that
    // made `our_tokens_take_their_line_from_the_file_not_from_the_rendering` fail on the other side of this same
    // question.
    let mut file: Option<Arc<Path>> = None;
    let mut marked_line: Option<u32> = None;
    let mut newlines_since = 0u32;

    let mut index = 0usize;
    while index < tokens.len() {
        let token = &tokens[index];
        let start = token.range.start_offset;

        if token.kind == cpp_parser::CppTokenKind::Newline {
            newlines_since += 1;
            index += 1;
            continue;
        }

        // A marker is a `#` whose **line** holds nothing before it. The test is on the *text* from the last newline,
        // not on the previous token: the lexer emits `Whitespace`, so a `#` that really is first on its line is
        // preceded by a `Whitespace` token rather than by a `Newline` —and a test against the token before it calls
        // that "first", which would turn `#if` and `#define` lines in the compiler's output into markers.
        let line_begin = text[..start].rfind('\n').map_or(0, |at| at + 1);
        let is_first_on_its_line = text[line_begin..start].trim().is_empty();
        if is_first_on_its_line
            && token.kind == cpp_parser::CppTokenKind::Hash
            && let Some(marker) = read_a_line_marker(text, &tokens, index)
        {
            if let Some(named) = marker.file {
                file = Some(named);
            }
            marked_line = Some(marker.line);
            // **Zero, not one.** The marker's own line is the one it *names*, and the token right after the marker is
            // still on it: the newline that ends the marker line has not been seen yet, and it is the newline **of
            // line N**. Counting it would put every token one line too far down —which is wrong by exactly one for
            // the whole file, the kind of error that looks plausible in a report.
            newlines_since = 0;
            index = tokens
                .partition_point(|token| token.range.start_offset < marker.end)
                .max(index + 1);
            continue;
        }

        if !cpp_parser::is_trivia(token.kind) {
            out.push(StreamToken {
                spelling: text[start..token.range.end_offset()].into(),
                file: file.clone(),
                line: marked_line.map(|line| line + newlines_since.saturating_sub(1)),
            });
        }

        index += 1;
    }

    out
}

/// A line marker, as read: the file it names (when it names one), the line, and where it ends.
struct LineMarker {
    file: Option<Arc<Path>>,
    line: u32,
    /// The offset of the newline that ends the marker's line —so the next token's line starts counting from there.
    end: usize,
}

/// Read a line marker starting at the `#` at `tokens[index]`, if that is what this is.
///
/// The three shapes GCC and MSVC print are all accepted, and the answer's `file` is `None` for `# 12` —a marker that
/// names no path —which leaves the current file in place rather than clearing it.
fn read_a_line_marker(
    text: &str,
    tokens: &[cpp_parser::CppTokenData],
    index: usize,
) -> Option<LineMarker> {
    /// The next token that is not whitespace or a comment, at or after `from`.
    ///
    /// **The lexer keeps whitespace**, so this is not optional: `spelling_at(index + 1)` reads the *space* after the
    /// `#`, not the number, and a reader that took the token at a fixed offset would answer "not a marker" for every
    /// well-formed marker there is. That was the bug —a marker one token wide is a marker this never found.
    fn next_significant(
        text: &str,
        tokens: &[cpp_parser::CppTokenData],
        from: usize,
    ) -> Option<usize> {
        (from..tokens.len()).find(|at| !cpp_parser::is_trivia(tokens[*at].kind))
            .filter(|at| !text.is_empty() && *at < tokens.len())
    }

    let spelling_at = |at: usize| {
        tokens
            .get(at)
            .map(|token| &text[token.range.start_offset..token.range.end_offset()])
    };
    let significance = |from: usize| next_significant(text, tokens, from);

    // `# 12 …`, or MSVC's `# line 12 …` —the keyword is optional and is not a number.
    let mut at = significance(index + 1)?;
    if spelling_at(at) == Some("line") {
        at = significance(at + 1)?;
    }

    let number: u32 = spelling_at(at)?
        .trim_end_matches(['u', 'U', 'l', 'L'])
        .parse()
        .ok()?;

    // An optional quoted path: present in both compilers' spelling, absent for a bare `# 12`. Read at the next
    // **significant** token for the same reason as above, and left unread (the marker ends at the line anyway) when
    // it is not a string.
    //
    // **Through `shared`**, which is what makes this path comparable with ours: the resolver records a normalized
    // path and a compiler prints whatever it likes, so the two spellings have to meet in the middle or every file is
    // [`Reason::MissingHeader`]. Building the `Arc<Path>` here instead was a real bug — our side normalized and this
    // one did not.
    let mut file = None;
    if let Some(path_at) = significance(at + 1)
        && let Some(named) = spelling_at(path_at).and_then(unquote)
    {
        file = Some(shared(Path::new(&named)));
    }

    // Everything left on this output line is the marker's flags —GCC's `1 3 4`, MSVC's nothing. None of it is a
    // token of the program, so the marker runs to the end of the line it is written on.
    let line_begin = text[..tokens[index].range.start_offset]
        .rfind('\n')
        .map_or(0, |at| at + 1);
    let end = text[line_begin..]
        .find('\n')
        .map_or(text.len(), |at| line_begin + at);

    Some(LineMarker { file, line: number, end })
}

/// The contents of a C string literal, when it is one this reader can unquote.
///
/// A marker's path is written by the compiler, so the escapes are the compiler's: `\\` and `\"` are the two that
/// appear, and a path with a `\n` in it does not exist.
///
/// # Why an unknown escape keeps its backslash
///
/// The first version dropped it, on the reasoning that "a compiler that printed an unknown escape meant the
/// character". That is right for `\d` in a path and **wrong for the one case that actually occurs**: a Windows path
/// like `C:\Users\zc\src` is not a well-formed C string, and dropping backslashes turns it into `C:Userszcsrc` —a
/// path that does not exist but looks like one. Keeping the backslash turns it into a path that does not exist and
/// *looks wrong*, which is the failure mode to prefer, and it is also what a real C compiler does for an
/// unrecognised escape (a diagnostic, and the character kept as written).
///
/// So `\\` and `\"` are decoded because those two are real, and everything else is copied through verbatim. An
/// unterminated or differently-quoted literal is `None`, and the marker then names no file rather than a wrong one.
fn unquote(spelling: &str) -> Option<String> {
    let body = spelling.strip_prefix('"')?.strip_suffix('"')?;

    let mut out = String::with_capacity(body.len());
    let mut characters = body.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }

        match characters.next()? {
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }

    Some(out)
}

/// The set of files a stream mentions —what [`Difference::reason`] needs to tell "we never read that header" from
/// "we read it and disagreed about its contents".
pub fn files_of(stream: &[StreamToken]) -> std::collections::HashSet<&Path> {
    stream
        .iter()
        .filter_map(|token| token.file.as_deref())
        .collect()
}

/// **A whole comparison as one report**, with the differences grouped by mechanism.
///
/// The text a caller prints, and a type rather than a `String` so that a test can assert about the *counts* —which
/// are the numbers the plan's §5.2 requires to be monotone —without parsing a report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Tokens the two agreed on.
    pub matched: usize,
    /// Tokens we produced.
    pub ours: usize,
    /// Tokens the compiler produced.
    pub theirs: usize,
    /// Each mechanism, with how many differences it accounts for, largest first.
    pub by_reason: Vec<(Reason, usize)>,
    /// The differences themselves, capped —see [`Report::differences`].
    pub differences: Vec<Difference>,
    /// Whether the comparison was stopped early —see [`Alignment::truncated`].
    pub truncated: bool,
}

/// How many individual differences a [`Report`] carries, however many there are.
///
/// The counts are the report; the samples are what a reader looks at to believe the counts. Carrying a million
/// differences to print twenty of them is the shape this crate keeps having to fix.
const REPORTED_DIFFERENCES: usize = 40;

impl Report {
    /// Compare two streams and summarise.
    pub fn of(ours: &[StreamToken], theirs: &[StreamToken]) -> Report {
        let alignment = align(ours, theirs);
        let our_files = files_of(ours);

        Report {
            matched: alignment.matched,
            ours: ours.len(),
            theirs: theirs.len(),
            by_reason: alignment.by_reason(&our_files),
            differences: alignment
                .differences
                .iter()
                .take(REPORTED_DIFFERENCES)
                .cloned()
                .collect(),
            truncated: alignment.truncated,
        }
    }

    /// Did the two streams agree?
    pub fn agrees(&self) -> bool {
        self.by_reason.is_empty() && !self.truncated
    }

    /// **The files the compiler read and this reading never entered** — see
    /// [`Alignment::headers_we_never_entered`] for why the names matter more than the count.
    ///
    /// A `Report` carries only a **sample** of the differences (see [`REPORTED_DIFFERENCES`]), so this works from
    /// that sample: it is a list of *examples* of a search that went differently, not an exhaustive one. The count in
    /// [`Report::by_reason`] is the exhaustive number.
    pub fn headers_we_never_entered(
        &self,
        our_files: &std::collections::HashSet<&Path>,
    ) -> Vec<&Path> {
        let mut missing: Vec<&Path> = self
            .differences
            .iter()
            .filter(|difference| difference.reason(our_files) == Reason::MissingHeader)
            .filter_map(|difference| difference.theirs.as_ref()?.file.as_deref())
            .collect();

        missing.sort_unstable();
        missing.dedup();
        missing
    }

    /// The report as text: the counts, then one line per difference with its mechanism and both tokens.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "ours {} tokens | theirs {} tokens | matched {} | differences {}{}\n",
            self.ours,
            self.theirs,
            self.matched,
            self.by_reason.iter().map(|(_, count)| *count).sum::<usize>(),
            if self.truncated {
                " (the comparison stopped early, so this is a lower bound)"
            } else {
                ""
            }
        ));

        if self.by_reason.is_empty() {
            out.push_str("  the two streams agree\n");
            return out;
        }

        out.push_str("by mechanism:\n");
        for (reason, count) in &self.by_reason {
            out.push_str(&format!("  {count:>7}  {reason:?}\n"));
            out.push_str(&format!("           {}\n", reason.advice()));
        }

        out.push_str("first differences:\n");
        for difference in &self.differences {
            let ours = difference
                .ours
                .as_ref()
                .map_or_else(|| "-".to_string(), |token| format!("{} @ {}", token.spelling, token.origin()));
            let theirs = difference
                .theirs
                .as_ref()
                .map_or_else(|| "-".to_string(), |token| format!("{} @ {}", token.spelling, token.origin()));
            out.push_str(&format!("  #{}  ours: {ours}   theirs: {theirs}\n", difference.at));
        }
        if !self.truncated {
            let total: usize = self.by_reason.iter().map(|(_, count)| *count).sum();
            if total > self.differences.len() {
                out.push_str(&format!("  —{} more\n", total - self.differences.len()));
            }
        }

        out
    }
}

/// The path of a file as a [`StreamToken`] carries it: **normalized**, and shared.
///
/// # Why normalizing here, and not at the comparison
///
/// The two halves of a comparison get their paths from different places, and the raw spellings do **not** compare
/// equal:
///
/// ```text
/// ours     RenderedUnit::files   ?what the include resolver recorded —normalized, case folded on Windows
/// theirs   the compiler's marker ?whatever the compiler decided to print —often verbatim, often `\`-separated
/// ```
///
/// [`crate::file::paths::normalize_path`] is the crate's one answer to "are these two spellings the same file", so
/// both halves go through it and the two sets become comparable. Without this every file is
/// [`Reason::MissingHeader`] the moment the spellings differ, which is a report that is wrong in the direction that
/// matters: it invents a missing header where there is only a missing separator.
///
/// Case folding is the platform's rule (`cfg!(windows)`), the same one resolution uses, so this cannot disagree with
/// the resolver about whether two paths are one file.
///
/// A `Path` is unsized, so the shared form is an `Arc<PathBuf>`; the point is that this is built **once per file**
/// and every token of that file shares it. A comparison of a standard library's closure holds a few million tokens,
/// and cloning a path per token is the kind of cost this crate has measured before.
pub fn shared(path: &Path) -> Arc<Path> {
    Arc::from(normalized(path)) as Arc<Path>
}

/// [`shared`]'s answer as an owned path —the normalized spelling of `path`, for a caller that has to *compare* two
/// paths from the two halves of a comparison rather than carry them.
///
/// Public because a consumer doing its own comparison (a record file keyed by path, a report grouped by file) needs
/// the same answer the streams were built with; a second normalization is how two answers to one question start to
/// differ.
pub fn normalized(path: &Path) -> PathBuf {
    PathBuf::from(crate::file::paths::normalize_path(path, cfg!(windows)))
}

/// **Where the text of a unit's files comes from**, for turning a rendering into comparable tokens.
///
/// The same shape [`crate::preprocess::macros::UnitSources`] has, and for the same reason: the caller decides where
/// text comes from —a disk provider, an editor's buffers, a test's fixtures —and the conversion must not care.
/// A file this cannot read is a file whose tokens get **no line**, which is not the same answer as line 1: an offset
/// that cannot be placed is unknown, and a wrong line is a report pointing at the wrong place.
pub trait UnitTexts {
    /// The text of `path`, when this caller has it, and the line index of that text.
    ///
    /// Both together because they must agree: a line index is an index *of one text*, and building it here is what
    /// makes it impossible for a caller to hand over an index of the previous revision. [`DiskTexts`] reads and
    /// indexes in one step for exactly that reason.
    fn text_of(&self, path: &Path) -> Option<(String, cpp_parser::LineIndex)>;
}

/// [`UnitTexts`] over the filesystem, which is what the command-line tool uses.
///
/// A file that cannot be read is `None` rather than an empty string: the two are different answers to "what is in
/// this file", and the empty one would put every token on line 1.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiskTexts;

impl UnitTexts for DiskTexts {
    fn text_of(&self, path: &Path) -> Option<(String, cpp_parser::LineIndex)> {
        let text = std::fs::read_to_string(path).ok()?;
        let lines = cpp_parser::LineIndex::parse(&text);
        Some((text, lines))
    }
}

impl UnitTexts for std::collections::HashMap<PathBuf, String> {
    fn text_of(&self, path: &Path) -> Option<(String, cpp_parser::LineIndex)> {
        let text = self.get(path)?;
        Some((text.clone(), cpp_parser::LineIndex::parse(text)))
    }
}

/// **Our rendering as comparable tokens**: the same shape [`tokenize_preprocessed`] produces for a compiler's output.
///
/// This is the bridge between the two halves of the comparison, and it is the one piece of the tool that is worth
/// being in the library rather than in the example —because it is where a subtle mistake would be invisible. Each
/// token of a [`crate::RenderedUnit`] carries the file it **stands in** ([`crate::UnitSpan::file`]) and a range in
/// that file ([`crate::UnitSpan::written`]), so the bridge to the other side is a **line number**, which needs the
/// file's text.
///
/// # Two things this gets right that are easy to get wrong
///
/// * **The line comes from `written`, not from `cooked`.** `cooked` is an offset in the *rendering* —a text this
///   crate spelled out, whose offsets are not positions in any file (`RenderedCooked`'s documentation is emphatic
///   about it). Asking it for a line would number the lines of a text nobody has.
/// * **The text is read once per file, before the loop**, not once per token. A standard library header contributes
///   thousands of tokens, and a conversion that read the file per token would make the comparison quadratic in file
///   size for no reason at all.
///
/// A file the caller cannot read yields tokens with **no file and no line**, which the classifier reads as "nothing
/// is known about where this is" rather than as agreement. That case is real: a rendering can name a file that was
/// reached by the walk and never held.
pub fn unit_tokens(unit: &crate::RenderedUnit, texts: &dyn UnitTexts) -> Vec<StreamToken> {
    // Built once per file, in the rendering's own frame order —the order `UnitSpan::file` indexes.
    let held: Vec<Option<HeldFile>> = unit
        .files
        .iter()
        .map(|path| {
            texts.text_of(path).map(|(text, lines)| HeldFile {
                lines,
                text,
                path: shared(path),
            })
        })
        .collect();

    unit.spans
        .iter()
        .map(|span| {
            let file = held.get(span.file as usize).and_then(Option::as_ref);
            StreamToken {
                spelling: unit.text[span.cooked.start_offset..span.cooked.end_offset()].into(),
                file: file.map(|file| Arc::clone(&file.path)),
                line: file.and_then(|file| {
                    file.lines
                        .position_of(span.written.start_offset, &file.text)
                        // Zero-based to one-based, which is what every compiler's line markers use and therefore what
                        // the other side of the comparison speaks.
                        .map(|(line, _)| line as u32 + 1)
                }),
            }
        })
        .collect()
}

/// One file of a rendering, read: the text, its line index, and the path its tokens share.
struct HeldFile {
    lines: cpp_parser::LineIndex,
    text: String,
    path: Arc<Path>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpp_parser::SourceRange;

    fn plain(spellings: &[&str]) -> Vec<StreamToken> {
        spellings
            .iter()
            .map(|spelling| StreamToken::new(*spelling, None, None))
            .collect()
    }

    fn spelled(stream: &[StreamToken]) -> Vec<&str> {
        stream.iter().map(|token| &*token.spelling).collect()
    }

    /// The reason of the **first** difference at or after `at` —the movement's reason, rather than one edit's.
    ///
    /// Every difference in a run carries the run's shape ([`Difference::run_ours`], [`Difference::run_theirs`]), so
    /// every one of them classifies the same way; taking the first is how a test names a movement instead of an edit.
    fn reason_at(alignment: &Alignment, at: usize) -> Reason {
        alignment
            .differences
            .iter()
            .find(|difference| difference.at >= at)
            .expect("there is a difference at or after this position")
            .reason(&Default::default())
    }

    /// How many **movements** there are: each contiguous run of edits counted once.
    ///
    /// What a report counts and what a test should assert about, because it is the number that corresponds to work
    /// items rather than to diff output.
    fn movements(alignment: &Alignment) -> usize {
        let mut count = 0usize;
        let mut previous: Option<usize> = None;
        for difference in &alignment.differences {
            if previous != Some(difference.at.wrapping_sub(1)) {
                count += 1;
            }
            previous = Some(difference.at);
        }
        count
    }

    /// **Two spellings of one file are one file.** The two halves of a comparison get their paths from different
    /// places —ours from the include resolver, theirs from whatever the compiler printed —and the raw spellings do
    /// not compare equal. Without normalizing both through
    /// [`crate::file::paths::normalize_path`], every such file is classified [`Reason::MissingHeader`]: a report
    /// that invents a missing header where there is only a missing separator.
    #[test]
    fn two_spellings_of_one_path_are_one_file() {
        let ours = vec![StreamToken::new("x", Some(shared(Path::new("/p//a.h"))), Some(1))];
        let theirs = vec![StreamToken::new("x", Some(shared(Path::new("/p/a.h"))), Some(1))];

        let our_files = files_of(&ours);
        assert_eq!(
            our_files.len(),
            1,
            "the two spellings collapse to one path: {our_files:?}"
        );
        assert!(
            our_files.contains(Path::new("/p/a.h")),
            "and it is the normalized spelling: {our_files:?}"
        );

        // The comparison therefore agrees, rather than reporting a missing header for the same file.
        assert!(align(&ours, &theirs).agrees(), "the same file on both sides");
    }

    /// The ordinary case, and the one the plan requires to be reportable as zero: two readings of one file agree.
    #[test]
    fn two_streams_that_agree_have_no_differences() {
        let ours = plain(&["int", "main", "(", ")", "{", "}"]);
        let theirs = plain(&["int", "main", "(", ")", "{", "}"]);

        let alignment = align(&ours, &theirs);

        assert!(alignment.agrees(), "{alignment:?}");
        assert_eq!(alignment.matched, 6, "every token is counted as agreed");
        assert!(alignment.differences.is_empty());
    }

    /// A macro that should have been expanded is **one** difference, not a delete plus an insert —the shape a
    /// reader can act on.
    ///
    /// The fixture has no trailing `;` **on purpose**: with one, the diff cannot tell the declaration's semicolon
    /// from one the macro produced, so it merges the two into one run and this stops being the isolated case. That is
    /// the ambiguity `Difference::run_ours` exists to make visible rather than to hide —see
    /// `one_macro_that_expands_to_several_tokens_is_one_movement` for the run's side of it.
    #[test]
    fn a_name_that_survived_where_the_compiler_expanded_it_is_one_difference() {
        let ours = plain(&["int", "x", "=", "VALUE"]);
        let theirs = plain(&["int", "x", "=", "42"]);

        let alignment = align(&ours, &theirs);

        assert_eq!(alignment.matched, 3, "everything but the last token agreed");
        assert_eq!(alignment.differences.len(), 1, "{:?}", alignment.differences);
        let difference = &alignment.differences[0];
        assert_eq!(difference.ours.as_ref().map(|it| &*it.spelling), Some("VALUE"));
        assert_eq!(difference.theirs.as_ref().map(|it| &*it.spelling), Some("42"));
        assert_eq!(difference.at, 3, "and it is reported where it is in the stream");
        assert!(difference.is_isolated(), "{difference:?}");
        assert_eq!((difference.run_ours, difference.run_theirs), (1, 1));
        assert_eq!(
            difference.reason(&Default::default()),
            Reason::ConditionDisagreement,
            "a name against a number: this fixture is exactly that shape"
        );
    }

    /// A stream compared against itself agrees, token for token —the state §5.2 requires to be reachable, so a
    /// regression that made it unreachable fails here rather than in a log.
    #[test]
    fn a_stream_compared_against_itself_is_all_matches() {
        let stream = plain(&["#", "define", "FLAG", "1", "int", "n", ";"]);

        let alignment = align(&stream, &stream);

        assert!(alignment.agrees(), "{alignment:?}");
        assert_eq!(alignment.matched, 7);
        assert!(alignment.differences.is_empty());
    }

    /// A whole region on our side and nothing on theirs is **one movement**, not one per token —the count a report
    /// prints has to be work items, and a branch that moved forty tokens is one wrongly-decided `#if`.
    #[test]
    fn a_region_present_on_one_side_only_is_counted_as_one_movement() {
        let ours = plain(&["int", "a", ";", "int", "b", ";", "int", "c", ";"]);
        let theirs = plain(&["int", "a", ";"]);

        let alignment = align(&ours, &theirs);

        assert_eq!(alignment.differences.len(), 6, "six edits: {:?}", alignment.differences);
        assert_eq!(movements(&alignment), 1, "and one movement");
        assert_eq!(reason_at(&alignment, 3), Reason::BranchDisagreement);
    }

    /// **The differences come out in stream order**, which is the invariant every consumer of them rests on.
    ///
    /// This is a regression test for a real bug in the traceback: it walks backwards, so a run of pure deletions was
    /// emitted in *descending* order —`["a", "b", "c"]` against `[]` came back as "at 2, at 1, at 0". Everything that
    /// reasons about adjacency then saw three movements where the streams moved once, and a report counted one `#if`
    /// as three work items. `align` sorts, and this is what says so.
    #[test]
    fn the_differences_come_out_in_stream_order() {
        let ours = plain(&["a", "b", "c", "d"]);

        let alignment = align(&ours, &[]);

        let positions: Vec<usize> = alignment
            .differences
            .iter()
            .map(|difference| difference.at)
            .collect();
        assert_eq!(positions, vec![0, 1, 2, 3], "{:?}", alignment.differences);
        assert_eq!(movements(&alignment), 1, "one region left, not four movements");
    }

    /// **A macro that expands to several tokens is one movement, not two work items.** This is the misclassification
    /// the first version of this module produced: read one edit at a time, our `SIZE` says `UnexpandedMacro` and the
    /// compiler's `8 * 1024` says `MissingExpansion` —two mechanisms, and neither is what happened.
    #[test]
    fn one_macro_that_expands_to_several_tokens_is_one_movement() {
        let ours = plain(&["int", "n", "=", "SIZE", ";"]);
        let theirs = plain(&["int", "n", "=", "8", "*", "1024", ";"]);

        let alignment = align(&ours, &theirs);

        assert_eq!(movements(&alignment), 1, "one macro moved: {:#?}", alignment.differences);
        assert!(
            alignment.differences.iter().all(|difference| difference.run > 1),
            "and every edit of it knows it is part of a run: {:?}",
            alignment.differences
        );
        assert_eq!(
            reason_at(&alignment, 3),
            Reason::ExpansionDisagreement,
            "one token out, three in: a replacement list, not a name that survived"
        );
    }

    /// **A name only we have, in a one-sided run, is a branch and not an unexpanded macro.** One edit at a time these
    /// are identical, which is why the run's shape is what gets read.
    #[test]
    fn a_name_alone_in_a_one_sided_run_is_reported_as_a_region() {
        let ours = plain(&["void", "f", "(", ")", "{", "ONLY_HERE", "}", ";"]);
        let theirs = plain(&["void", "f", "(", ")", "{", "}", ";"]);

        let alignment = align(&ours, &theirs);

        assert_eq!(movements(&alignment), 1);
        assert_eq!(
            reason_at(&alignment, 5),
            Reason::UnexpandedMacro,
            "one name on our side and nothing where theirs is: {:?}",
            alignment.differences
        );
    }

    /// A `#if` decided the other way is a **run** of tokens present on one side only, and the reason names the
    /// mechanism rather than the tokens.
    #[test]
    fn a_branch_taken_on_one_side_only_is_a_run_of_one_sided_differences() {
        let ours = plain(&["int", "a", ";", "int", "b", ";"]);
        let theirs = plain(&["int", "a", ";"]);

        let alignment = align(&ours, &theirs);

        assert_eq!(alignment.matched, 3);
        assert_eq!(alignment.differences.len(), 3, "{:?}", alignment.differences);
        assert!(
            alignment
                .differences
                .iter()
                .all(|difference| difference.theirs.is_none()),
            "every difference is ours alone: {:?}",
            alignment.differences
        );
    }

    /// A **substitution** is reported as one entry with both sides, which is what makes a report readable: the old
    /// pairwise diff produces two entries for the same place.
    #[test]
    fn a_substitution_is_one_difference_with_both_halves() {
        let alignment = align(&plain(&["a", "OLD", "b"]), &plain(&["a", "NEW", "b"]));

        assert_eq!(alignment.differences.len(), 1, "{:?}", alignment.differences);
        assert!(alignment.differences[0].ours.is_some() && alignment.differences[0].theirs.is_some());
    }

    /// An identifier against a number is the fingerprint of `#if` disagreement, and it is the reason
    /// `ConditionDisagreement` exists: the two streams chose differently, which is a different work item from a macro
    /// that was not expanded.
    #[test]
    fn a_name_against_a_number_is_classified_as_a_condition_disagreement() {
        // One token out, one in, name against number, and nothing else moved —which is what makes it *one* movement
        // and not a run. A fixture with a trailing `;` would not: the diff cannot tell the declaration's semicolon
        // from one the macro produced, so it would merge the two into a run and read a different reason.
        let ours = plain(&["int", "n", "=", "WIDTH"]);
        let theirs = plain(&["int", "n", "=", "19"]);

        let alignment = align(&ours, &theirs);

        assert_eq!(movements(&alignment), 1, "{:?}", alignment.differences);
        assert_eq!(reason_at(&alignment, 3), Reason::ConditionDisagreement);
    }

    /// A token whose file our stream never mentions is a header we never read —certain, not heuristic, which is why
    /// it outranks every shape rule.
    #[test]
    fn a_token_from_a_file_we_never_read_is_a_missing_header() {
        let ours = plain(&["int", "x", ";"]);
        let theirs = vec![
            StreamToken::new("int", Some(shared(Path::new("/p/a.cpp"))), None),
            StreamToken::new("helper", Some(shared(Path::new("/missing/header.h"))), None),
            StreamToken::new(";", Some(shared(Path::new("/p/a.cpp"))), None),
        ];
        let our_files = files_of(&ours);

        let reason = align(&ours, &theirs).differences[0].reason(&our_files);

        assert_eq!(reason, Reason::MissingHeader);
    }

    /// A name that is the same **name** on the other side is not a condition disagreement: it is one name against
    /// another, which is the shape rule 7 leaves `Unknown` rather than guessing at.
    #[test]
    fn a_name_against_a_name_is_not_a_condition_disagreement() {
        let ours = plain(&["int", "n", "=", "WIDTH"]);
        let theirs = plain(&["int", "n", "=", "HEIGHT"]);

        let alignment = align(&ours, &theirs);

        assert_eq!(movements(&alignment), 1);
        assert_eq!(
            reason_at(&alignment, 3),
            Reason::Unknown,
            "the diff cannot say which mechanism moved two names: {:?}",
            alignment.differences
        );
    }

    /// **The line markers are read, not skipped.** `-E`'s output is a concatenation of every file, and the marker is
    /// the only thing that says which one —so a comparison that dropped them could not compare file boundaries at
    /// all, which is one of the three things the plan asks this tool to compare.
    #[test]
    fn the_line_markers_of_a_preprocessed_file_are_read_for_position() {
        // The shape both compilers print, with GCC's trailing flags and a quoted path.
        let output = "# 1 \"C:/src/main.cpp\"\nint main ( ) { }\n# 1 \"C:/inc/xstring\" 1 3\nnamespace std {\n}\n# 7 \"C:/src/main.cpp\" 2\nreturn 0 ;\n";

        let tokens = tokenize_preprocessed(output);

        assert_eq!(
            spelled(&tokens),
            ["int", "main", "(", ")", "{", "}", "namespace", "std", "{", "}", "return", "0", ";"],
            "the markers are not tokens: {tokens:?}"
        );
        assert_eq!(tokens[0].origin(), "c:/src/main.cpp:1");
        assert_eq!(
            tokens[6].origin(),
            "c:/inc/xstring:1",
            "the header's own tokens carry the header's name and line: {tokens:?}"
        );
        assert_eq!(
            tokens[11].origin(),
            "c:/src/main.cpp:7",
            "and the file comes back when the marker says so"
        );
    }

    /// MSVC spells the marker `#line`, and the same reader has to accept it —the two compilers are both oracles and
    /// the tool must not need to know which one answered.
    ///
    /// The path comes back **normalized**, which on Windows means case-folded: `C:/src/main.cpp` is `c:/src/main.cpp`,
    /// the same spelling the include resolver records. That is the point of normalizing here rather than at the
    /// comparison —see [`shared`] —and the expectation is written for the platform the test is on.
    #[test]
    fn msvcs_line_marker_spelling_is_read_too() {
        let output = "#line 12 \"C:\\\\src\\\\main.cpp\"\nint x ;\n";

        let tokens = tokenize_preprocessed(output);

        assert_eq!(spelled(&tokens), ["int", "x", ";"]);
        let expected = if cfg!(windows) {
            "c:/src/main.cpp:12"
        } else {
            "C:/src/main.cpp:12"
        };
        assert_eq!(tokens[0].origin(), expected, "{tokens:?}");
    }

    /// **A path with a backslash that is not a C escape keeps its backslash, and is normalized like every other
    /// path.** `cl` doubles its separators, but a marker's path is not required to be a well-formed C string and a
    /// compiler is free to print `C:\Users\zc\src` — in which `\U`, `\z` and `\s` are not escapes at all.
    ///
    /// The first version of `unquote` dropped the backslash, turning that into `C:Userszcsrc`: a path that does not
    /// exist but *looks* like one, so a report would point at a file a reader might believe. The separators are
    /// therefore kept; unifying them (and folding case on Windows) is [`shared`]'s job, and the assertion below is
    /// the two together.
    #[test]
    fn a_windows_path_that_is_not_a_c_string_keeps_its_backslashes() {
        let output = "# 1 \"C:\\Users\\zc\\src\\main.cpp\"\nint x ;\n";

        let tokens = tokenize_preprocessed(output);

        // The separators are unified rather than dropped: `C:Userszcsrc` is what the bug produced, and this is not
        // that.
        assert_eq!(
            tokens[0].origin(),
            "c:/users/zc/src/main.cpp:1",
            "the path survives with its separators, in the spelling comparisons are made in: {tokens:?}"
        );
    }

    /// **A `#pragma` line is not a marker.** It survives `-E` precisely because it is meaningful to whatever reads
    /// the output, and a comparison that dropped every `#`-leading line would stop noticing a `#pragma once` this
    /// reading got wrong.
    #[test]
    fn a_pragma_line_survives_as_tokens() {
        let output = "#pragma once\nint x ;\n";

        let tokens = tokenize_preprocessed(output);

        assert_eq!(spelled(&tokens), ["#", "pragma", "once", "int", "x", ";"]);
    }

    /// Comments and layout are dropped, because the other side of the comparison has neither: our stream is a
    /// rendering with one space between tokens. Comparing them would make every run differ on nothing.
    #[test]
    fn comments_and_layout_are_not_part_of_the_comparison() {
        let tokens = tokenize_preprocessed("int   /* why */  x ;\n// and\nint y ;");

        assert_eq!(spelled(&tokens), ["int", "x", ";", "int", "y", ";"]);
    }

    /// **The bound is reported, not hidden.** Two streams with nothing in common have a difference count proportional
    /// to their length; a report that presented a truncated comparison as a whole would be the exact kind of silent
    /// overclaim the plan's rule 3 forbids.
    #[test]
    fn a_comparison_that_ran_out_of_budget_says_so() {
        let ours: Vec<StreamToken> = (0..MAX_EDIT_DISTANCE + 200)
            .map(|at| StreamToken::new(format!("left{at}"), None, None))
            .collect();
        let theirs: Vec<StreamToken> = (0..MAX_EDIT_DISTANCE + 200)
            .map(|at| StreamToken::new(format!("right{at}"), None, None))
            .collect();

        let alignment = align(&ours, &theirs);

        assert!(alignment.truncated, "an unbounded diff must report that it stopped");
        assert!(!alignment.agrees(), "and must not be mistaken for agreement");
    }

    /// **The report's counts are the numbers the plan requires to be monotone**, so they are what a test asserts
    /// about rather than the rendered text. The composition is asserted exactly: a total alone would pass while the
    /// mechanisms were mixed up, which is the failure this classification exists to prevent.
    ///
    /// The three movements are what the classifier has to tell apart, and this is the shape each one has. Two movements that touch
    /// are one run as far as a diff can tell —a replacement list substituted for a name and an extra token right
    /// after it are indistinguishable from one macro that produced both —so a fixture that wants three reasons has
    /// to make three runs, and a run ends where the streams agree again.
    #[test]
    fn the_report_counts_differences_by_mechanism() {
        let ours = plain(&[
            "int", "n", "=", "WIDTH", ";", // a name where the compiler has a number
            ";", "EXTRA", ";", // ours alone, and it costs one token
            ";", "A", "B", // ours alone, and it costs two
        ]);
        let theirs = plain(&[
            "int", "n", "=", "19", ";", //
            ";", ";", //
            ";", "C", //
        ]);

        let report = Report::of(&ours, &theirs);

        assert_eq!((report.ours, report.theirs), (11, 9));
        assert!(!report.agrees());
        assert!(!report.truncated, "a handful of edits: {report:?}");

        let mut counted = report.by_reason.clone();
        counted.sort_by_key(|(reason, _)| format!("{reason:?}"));
        assert_eq!(
            counted,
            vec![
                (Reason::ConditionDisagreement, 1),
                (Reason::ExpansionDisagreement, 2),
                (Reason::UnexpandedMacro, 1),
            ],
            "one movement each, counted where it belongs: {report:?}"
        );
        assert!(report.render().contains("by mechanism"), "{}", report.render());
    }

    /// A report of two streams that agree says so, and carries no differences at all —the state §5.2 requires to be
    /// reachable, so a regression that made it unreachable would fail here rather than in a log.
    #[test]
    fn a_report_of_two_agreeing_streams_says_so() {
        let report = Report::of(&plain(&["a", "b"]), &plain(&["a", "b"]));

        assert!(report.agrees());
        assert!(report.render().contains("agree"), "{}", report.render());
    }

    /// **[`unit_tokens`] reads a line from the file the token was written in**, not from the rendering's own text.
    ///
    /// This is the piece of the bridge that a mistake would hide in: `UnitSpan::cooked` is an offset in a text this
    /// crate spelled out, and asking it for a line would number the lines of a text nobody has. The fixture is built
    /// so the two could not be confused —the rendering puts one token per line, while the file's own text has them
    /// spread over five lines with the interesting ones on 4 and 5.
    ///
    /// The `written` offsets are the **real ones in `text`**, which is the whole point of the fixture: `c` is at byte
    /// 14 and not at 12, because 12 is the `int` of that line. An offset that is right for the wrong reason would
    /// make this test pass while the mapping it checks was broken.
    #[test]
    fn our_tokens_take_their_line_from_the_file_not_from_the_rendering() {
        let file = "/p/a.h";
        // Line 1 blank, then `int a;` on 2, `int b;` on 3, `int c;` on 4, `int d;` on 5.
        let text = "\nint a;\nint b;\nint c;\nint d;\n";

        let mut unit = crate::RenderedUnit {
            files: vec![PathBuf::from(file)],
            file_lengths: vec![text.len()],
            ..crate::RenderedUnit::default()
        };
        // The rendering is a flat line of tokens —a different text, with different offsets, on purpose.
        unit.push("int", 0, SourceRange::new(1, 3));
        unit.push("a", 0, SourceRange::new(5, 1));
        unit.push("int", 0, SourceRange::new(8, 3));
        unit.push("c", 0, SourceRange::new(19, 1));

        let texts: std::collections::HashMap<PathBuf, String> =
            [(PathBuf::from(file), text.to_string())].into_iter().collect();
        let tokens = unit_tokens(&unit, &texts);

        let placed: Vec<(&str, u32)> = tokens
            .iter()
            .map(|token| (&*token.spelling, token.line.expect("the file's text was given")))
            .collect();
        assert_eq!(
            placed,
            vec![("int", 2), ("a", 2), ("int", 3), ("c", 4)],
            "each token's line is where the file wrote it: {tokens:?}"
        );
        assert_eq!(
            tokens[0].file.as_deref(),
            Some(Path::new(file)),
            "and its file is the one it stands in"
        );
    }

    /// A file the caller has no text for yields tokens with **no position**, not with line 1 —the difference between
    /// "unknown" and a report that points at the wrong line.
    #[test]
    fn a_file_nobody_has_the_text_of_yields_tokens_with_no_position() {
        let mut unit = crate::RenderedUnit {
            files: vec![PathBuf::from("/p/a.h")],
            file_lengths: vec![0],
            ..crate::RenderedUnit::default()
        };
        unit.push("x", 0, SourceRange::new(0, 1));

        let tokens = unit_tokens(&unit, &std::collections::HashMap::<PathBuf, String>::new());

        assert_eq!(tokens.len(), 1);
        assert!(tokens[0].file.is_none(), "{tokens:?}");
        assert!(tokens[0].line.is_none(), "{tokens:?}");
        assert_eq!(tokens[0].origin(), "?", "and the report says so rather than guessing");
    }
}
