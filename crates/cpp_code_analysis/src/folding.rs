//! # Folding — the regions of a file a client may hide
//!
//! Four kinds of region, and the reason each one is read where it is:
//!
//! ```text
//! Code      a `{` … `}` pair on different lines      the token stream — every brace is in it, even in a file
//!                                                    the parser recovered from
//! Comment   a run of comments, or one multi-line one  the token stream, where a comment is *one* token
//! Region    `#if` … `#endif`                          the directives, which is where the conditionals are
//! Imports   a run of adjacent `#include`s             the directives too
//! ```
//!
//! # "More than one line" is asked of the text, not of a line index
//!
//! A fold of one line hides nothing, so every rule below asks whether the region's **text contains a newline** —
//! one slice of the source, and it is the same answer a client would reach from the same bytes. Nothing here needs
//! a line index, and nothing here can disagree with one.
//!
//! # The tolerant half
//!
//! A file being typed has unbalanced braces and unterminated `#if`s, and the honest answer there is **no fold**: an
//! unclosed `{` is not the start of a region that reaches the end of the file, it is a region that has not been
//! written yet, and a client that folded to the last line would hide everything the user is working on. So each
//! rule pairs what it can and drops what it cannot — the same shape as the parser's own recovery, one layer down.

use cpp_parser::{CppTokenData, CppTokenKind, SourceRange};

use crate::preprocess::directive::{DirectiveKind, SpannedDirective, scan_directives};

/// What a fold hides.
///
/// The protocol has names for these (`comment`, `imports`, `region`, and nothing at all for code), and the mapping
/// happens in the LSP layer: this enum exists so that the analysis does not have to know what a client calls things.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldKind {
    /// A brace pair: a class, a function body, a block.
    Code,
    /// A run of comments — what a client shows as a comment block.
    Comment,
    /// An `#if` … `#endif`, which a C++ reader folds to get the branch that is not theirs out of the way.
    Region,
    /// A run of `#include`s.
    Imports,
}

/// One foldable region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    /// The **offsets** of the region, from its first token to its last. The text between them has a newline in it,
    /// so the two ends are on different lines.
    pub range: SourceRange,
    pub kind: FoldKind,
}

/// Every region of a file a client may fold, in source order.
///
/// Source order rather than grouped by kind, because that is the order a client draws them in and the order a
/// reader expects to find them: the rules run independently and the result is sorted once.
pub fn folding_ranges(source: &str, tokens: &[CppTokenData]) -> Vec<Fold> {
    let mut folds = brace_folds(source, tokens);
    folds.extend(comment_folds(source, tokens));
    folds.extend(directive_folds(source, &scan_directives(source, tokens)));

    folds.sort_by_key(|fold| (fold.range.start_offset, fold.range.end_offset()));
    folds
}

/// `{` … `}` pairs that span more than one line.
///
/// The stack is the whole rule: a `}` closes the most recent `{`, which is what makes nesting work without asking
/// the grammar. An unmatched `{` is dropped rather than closed at the end of the file — see the module note.
fn brace_folds(source: &str, tokens: &[CppTokenData]) -> Vec<Fold> {
    let mut open: Vec<usize> = Vec::new();
    let mut folds = Vec::new();

    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            CppTokenKind::LeftBrace => open.push(index),
            CppTokenKind::RightBrace => {
                let Some(start) = open.pop() else {
                    continue;
                };
                let range = SourceRange::new(
                    tokens[start].range.start_offset,
                    tokens[index].range.end_offset() - tokens[start].range.start_offset,
                );
                if spans_lines(source, range) {
                    folds.push(Fold {
                        range,
                        kind: FoldKind::Code,
                    });
                }
            }
            _ => {}
        }
    }

    folds
}

/// Runs of comments on adjacent lines, and single comments that are themselves multi-line.
///
/// A "run" is a maximal sequence of comment tokens with at most one newline between neighbours: a blank line ends
/// it, which is what a reader means by two separate comment blocks. Each comment's **own text** counts as part of
/// the run for the newline test, so one `/* … */` spread over five lines is a fold on its own.
fn comment_folds(source: &str, tokens: &[CppTokenData]) -> Vec<Fold> {
    let mut folds = Vec::new();
    let mut index = 0usize;

    while index < tokens.len() {
        if !is_a_comment(tokens[index].kind) {
            index += 1;
            continue;
        }

        let first = index;
        let mut last = index;
        let mut at = index + 1;

        // Walk forward through trivia: up to one newline keeps the run going, two end it, and a token that is
        // neither a newline nor another comment (whitespace) is transparent.
        let mut newlines = 0usize;
        while at < tokens.len() {
            match tokens[at].kind {
                kind if is_a_comment(kind) && newlines <= 1 => {
                    last = at;
                    newlines = 0;
                }
                CppTokenKind::Newline => newlines += 1,
                kind if cpp_parser::is_trivia(kind) => {}
                _ => break,
            }
            at += 1;
        }

        let range = SourceRange::new(
            tokens[first].range.start_offset,
            tokens[last].range.end_offset() - tokens[first].range.start_offset,
        );
        if spans_lines(source, range) {
            folds.push(Fold {
                range,
                kind: FoldKind::Comment,
            });
        }

        index = last + 1;
    }

    folds
}

/// `#if` … `#endif` regions and runs of adjacent `#include`s.
///
/// The conditionals are paired with a stack over the directive list — `#if`, `#ifdef` and `#ifndef` open, `#endif`
/// closes, and the `#else`/`#elif` in between belong to the same region (a client folds the whole thing to get it
/// out of the way, which is what a C++ reader wants from a branch that is not their platform's). An `#endif`
/// without its `#if` and an `#if` without its `#endif` are both dropped.
fn directive_folds(source: &str, directives: &[SpannedDirective]) -> Vec<Fold> {
    let mut folds = Vec::new();
    let mut open: Vec<usize> = Vec::new();

    for directive in directives {
        match directive.directive.kind() {
            DirectiveKind::If | DirectiveKind::Ifdef | DirectiveKind::Ifndef => {
                open.push(directive.range.start_offset)
            }
            DirectiveKind::Endif => {
                let Some(start) = open.pop() else {
                    continue;
                };
                // **To the `#endif`'s own logical line**, not to the end of its span: a span rides the trivia that
                // follows — the newline, a blank line, and any comment (a comment is trivia too, which is how a
                // fold built from the span ends up several lines below the region). `SpannedDirective::line` is the
                // directive's own extent, derived where the rule that decides it lives.
                let range = SourceRange::new(
                    start,
                    directive.line.end_offset().saturating_sub(start),
                );
                if spans_lines(source, range) {
                    folds.push(Fold {
                        range,
                        kind: FoldKind::Region,
                    });
                }
            }
            _ => {}
        }
    }

    // A run of `#include`s on **consecutive lines**, two or more. "Consecutive" is asked of the text from one
    // directive's start to the next's: exactly one newline means the next line, two means a blank line ended the
    // run. Measured start to start rather than end to start, because a directive's span carries its own trailing
    // trivia — the newline after `#include <a>` belongs to *it* — so the gap between two spans is empty by
    // construction and counting it would answer zero for every pair.
    let includes: Vec<&SpannedDirective> = directives
        .iter()
        .filter(|directive| {
            matches!(
                directive.directive.kind(),
                DirectiveKind::Include | DirectiveKind::IncludeNext
            )
        })
        .collect();

    let mut at = 0usize;
    while at < includes.len() {
        let first = at;
        let mut last = at;

        while last + 1 < includes.len()
            && newlines_between(source, &includes[last].range, &includes[last + 1].range) == 1
        {
            last += 1;
        }

        if last > first {
            folds.push(Fold {
                range: SourceRange::new(
                    includes[first].line.start_offset,
                    includes[last]
                        .line
                        .end_offset()
                        .saturating_sub(includes[first].line.start_offset),
                ),
                kind: FoldKind::Imports,
            });
        }

        at = last + 1;
    }

    folds
}

/// Does the text of `range` contain a newline?
///
/// The one line test this module has, and it is deliberately about the **text** rather than about a line index: a
/// single `/* … */` comment holds its newlines inside one token, and a brace pair holds them in the trivia between
/// two, so a test that looked at tokens would need two rules where the text needs one.
fn spans_lines(source: &str, range: SourceRange) -> bool {
    source
        .get(range.start_offset..range.end_offset())
        .is_some_and(|text| text.contains('\n'))
}

/// How many newlines the text between two directives' **starts** has — `0` when the second starts first.
fn newlines_between(source: &str, one: &SourceRange, other: &SourceRange) -> usize {
    let (from, to) = if one.start_offset <= other.start_offset {
        (one.start_offset, other.start_offset)
    } else {
        (other.start_offset, one.start_offset)
    };

    source
        .get(from..to)
        .map(|text| text.matches('\n').count())
        .unwrap_or(0)
}

/// A comment token, either spelling.
///
/// Two kinds rather than one, and neither is `is_trivia`'s business alone: a comment *is* trivia to the grammar
/// (the parser skips it) and is exactly what this module is looking for, which is why the question is asked by name.
fn is_a_comment(kind: CppTokenKind) -> bool {
    matches!(kind, CppTokenKind::LineComment | CppTokenKind::BlockComment)
}
