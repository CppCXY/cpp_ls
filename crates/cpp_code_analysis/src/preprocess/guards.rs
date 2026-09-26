//! Include guards: recognising that a header protects itself, and remembering what a header did to the
//! macro table.
//!
//! # Why the guard is worth detecting
//!
//! A header included twice would otherwise be *analysed* twice, and — worse — its macros would be defined
//! twice, so a consumer asking "where is `FOO` defined" would get two answers for one definition and a
//! consumer following the second would land in the middle of a header that is only included once in
//! reality.
//!
//! Both spellings are recognised, and they behave differently in a way that matters:
//!
//! * `#pragma once` makes the file unconditionally visit-once. The compiler decides it by identity, and so
//!   does this.
//! * `#ifndef GUARD / #define GUARD ... #endif` is *conditional*: the guard macro is defined for the rest
//!   of the translation unit, so the file is skipped only when that macro is already defined — and only by
//!   a route that went through this header. So the check is "is `GUARD` defined **and was it defined by
//!   this header**", which is why [`Guard::Macro`] carries the name rather than a flag.
//!
//! Getting that second case wrong in the obvious way — treating an `#ifndef` header as unconditionally
//! visit-once — breaks the case where two headers use the same guard name by accident, which is common when
//! projects copy each other's headers.

use cpp_parser::{CppSyntaxKind, CppSyntaxNode, CppSyntaxTree};

use crate::{
    directive::{Directive, DirectiveKind, SpannedDirective},
    preprocess::{FilePreprocessing, preprocess},
};

/// How a header protects itself against being included twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guard {
    /// The header has no guard at all. Including it twice analyzes it twice, which is what a compiler
    /// does too.
    None,
    /// `#pragma once`.
    PragmaOnce,
    /// `#ifndef NAME` … `#endif`, with the `#define NAME` that makes the name its own.
    Macro(Box<str>),
}

impl Guard {
    /// Is this header protected from a second visit?
    ///
    /// `PragmaOnce` always is; a macro guard only while its macro is defined, which is a question for the
    /// caller that holds the macro table — see [`Guard::blocks_a_visit`].
    pub fn is_guarded(&self) -> bool {
        !matches!(self, Guard::None)
    }

    /// Would a second visit of this header be skipped?
    pub fn blocks_a_visit(&self, is_defined: impl Fn(&str) -> bool) -> bool {
        match self {
            Guard::None => false,
            Guard::PragmaOnce => true,
            Guard::Macro(name) => is_defined(name),
        }
    }
}

/// What analysing a header did to the macro table, so a second visit can be decided.
///
/// Recorded per *file*, not per include site, because both spellings of a guard are properties of the file:
/// `#pragma once` says nothing about who included it, and a macro guard names a macro that the first
/// visit defined for everything that follows. Which is also why this is not the whole story — a header
/// without a guard defines its macros again on every visit, and its entry is simply overwritten with the
/// same answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileGuard {
    pub guard: Guard,
    /// Every macro the file defined at its top level, in order.
    ///
    /// Kept because a `#pragma once` header's macros have to be *restored* by a later visit that is
    /// skipped: the skip means "do not analyse again", not "do not define again" — and a compiler that
    /// skipped the text would not define them a second time either, but it also would not have to, because
    /// the definitions from the first visit are still in force. A consumer rebuilding a per-file macro
    /// environment needs them to reconstruct that.
    pub defines: Vec<Box<str>>,
}

/// What was found in one file's directives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardAnalysis {
    pub guard: Guard,
    pub defines: Vec<Box<str>>,
}

/// Recognise a file's guard and collect the macros it defines.
///
/// `root` is needed for one question that the directives alone cannot answer: whether the header declares
/// anything *before* its guard. A guard only protects what is inside it, so a header that declares
/// something first is re-analysed on every visit however it is spelled — and calling that a guard would
/// hide the bug rather than describe the file.
pub fn analyse_guards(preprocessing: &FilePreprocessing, root: &CppSyntaxNode) -> GuardAnalysis {
    let defines: Vec<Box<str>> = preprocessing
        .directives
        .iter()
        .filter_map(|spanned| spanned.directive.defines())
        .map(|definition| definition.name.clone())
        .collect();

    GuardAnalysis {
        guard: detect_guard(&preprocessing.directives, root),
        defines,
    }
}

/// Recognise a file's guard, given its directives in source order.
/// Does the file write a **macro guard** — `#ifndef NAME` with its `#define NAME` immediately inside?
///
/// The question [`detect_guard`] cannot answer for a file that also writes `#pragma once`: it reports the pragma
/// (the stronger answer, and the right one for "may the file be skipped next time"), while a caller asking
/// "which region is this file's own guard" needs the *region*. Kept beside `detect_guard` so the two cannot
/// disagree about what a guard is.
pub(crate) fn has_a_macro_guard(directives: &[SpannedDirective], root: &CppSyntaxNode) -> bool {
    detect_macro_guard(directives, root).is_some()
}

pub fn detect_guard(directives: &[SpannedDirective], root: &CppSyntaxNode) -> Guard {
    if has_pragma_once(directives) {
        return Guard::PragmaOnce;
    }

    detect_macro_guard(directives, root).unwrap_or(Guard::None)
}

fn has_pragma_once(directives: &[SpannedDirective]) -> bool {
    directives.iter().any(|spanned| match &spanned.directive {
        Directive::Pragma { tokens } => tokens.first().is_some_and(|token| token.text() == "once"),
        _ => false,
    })
}

/// The `#ifndef NAME` at the outermost level of the file, with its `#define NAME` immediately inside.
///
/// # What counts
///
/// The guard has to be the **first** conditional region of the file, at depth 0, with no code before it,
/// and the `#define` has to be the next directive in it. A header whose first `#ifndef` appears after some
/// declarations is not guarded by it — those declarations are outside the region and are re-analysed on
/// every visit, which is the very bug the guard exists to prevent.
///
/// Nothing after the `#define` is examined. Whether the `#endif` is the last directive, and whether it
/// closes the region, is the preprocessor's business: a file that leaves a region unclosed is one being
/// edited, and its guard still works for as long as the `#ifndef` is false.
fn detect_macro_guard(directives: &[SpannedDirective], root: &CppSyntaxNode) -> Option<Guard> {
    let opener = directives.iter().position(|spanned| {
        spanned.condition_depth == 0 && matches!(spanned.directive.kind(), DirectiveKind::Ifndef)
    })?;

    // Code before the opener is outside the guard. Comments and directives are not code: a copyright banner
    // above a guard is the ordinary spelling of a header.
    if declares_something_before(root, directives[opener].range.start_offset) {
        return None;
    }

    let name = match &directives[opener].directive {
        Directive::Ifdef { name, .. } => name.clone(),
        _ => return None,
    };

    // The `#define` has to be the next directive, at depth 1 — inside the region the `#ifndef` opened.
    let definition = directives
        .get(opener + 1)
        .filter(|spanned| spanned.condition_depth == 1)
        .and_then(|spanned| spanned.directive.defines())?;

    if definition.name != name {
        // `#ifndef A` / `#define B` is a region that guards something other than itself. Common enough in
        // feature headers, and not a guard.
        return None;
    }

    Some(Guard::Macro(name))
}

/// Does the file declare anything before `offset`?
///
/// Asked of the tree rather than of the directive list, because the list holds only directives: a
/// declaration is not in it, and its absence would look exactly like a file that starts with its guard.
///
/// A **comment is not a declaration**, which is why it needs saying: the documentation layer gives every
/// comment a `DocComment` *node*, so a copyright banner above a guard is a child of the translation unit
/// like any other — and counting it would mean no header with a banner is ever recognised as guarded.
///
/// Only the *direct* children are examined. A declaration nested inside something else is inside that
/// something, which is itself a child, so looking one level down is enough — and looking further would
/// count the declarations *inside* the guard region, which is the opposite of the question.
fn declares_something_before(root: &CppSyntaxNode, offset: usize) -> bool {
    root.children().any(|child| {
        let kind = CppSyntaxKind::from(child.kind());
        let is_not_code = matches!(
            kind,
            CppSyntaxKind::PreprocessorDirective | CppSyntaxKind::DocComment
        );
        let starts_before = (u32::from(child.text_range().start()) as usize) < offset;

        !is_not_code && starts_before
    })
}

/// Analyse a parsed file's guards.
pub fn analyse(source: &str, tree: &CppSyntaxTree) -> GuardAnalysis {
    let preprocessing = preprocess(&tree.get_red_root());
    let _ = source;
    analyse_guards(&preprocessing, &tree.get_red_root())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpp_parser::{CppParser, ParserConfig};

    fn guards_of(source: &str) -> GuardAnalysis {
        let tree = CppParser::parse(source, ParserConfig::default());
        analyse(source, &tree)
    }

    #[test]
    fn a_header_with_no_guard_has_none() {
        assert_eq!(guards_of("int x;\n").guard, Guard::None);
    }

    #[test]
    fn pragma_once_is_recognised() {
        assert_eq!(guards_of("#pragma once\nint x;\n").guard, Guard::PragmaOnce);
    }

    /// The classic idiom.
    #[test]
    fn the_ifndef_idiom_is_recognised() {
        assert_eq!(
            guards_of("#ifndef FOO_H\n#define FOO_H\nint x;\n#endif\n").guard,
            Guard::Macro("FOO_H".into())
        );
    }

    /// A comment before the guard does not put code outside it, so the idiom still counts.
    #[test]
    fn a_comment_before_the_guard_is_allowed() {
        assert_eq!(
            guards_of("// a header\n#ifndef FOO_H\n#define FOO_H\nint x;\n#endif\n").guard,
            Guard::Macro("FOO_H".into())
        );
    }

    /// **Declarations before the guard are outside it**, so the file is not protected by it. Calling it a
    /// guard would hide a real bug — those declarations are re-analysed on every visit — rather than
    /// describe the file.
    #[test]
    fn code_before_the_guard_means_it_is_not_a_guard() {
        assert_eq!(
            guards_of("int outside;\n#ifndef FOO_H\n#define FOO_H\nint x;\n#endif\n").guard,
            Guard::None
        );
    }

    /// `#ifndef A` / `#define B` guards something other than itself, which is a feature header rather than
    /// a guard.
    #[test]
    fn a_region_that_defines_a_different_name_is_not_a_guard() {
        assert_eq!(
            guards_of("#ifndef A\n#define B\nint x;\n#endif\n").guard,
            Guard::None
        );
    }

    /// A guard whose `#define` never comes is a file being written. Not a guard, because there is nothing
    /// recording that the header has been seen.
    #[test]
    fn an_ifndef_without_its_define_is_not_a_guard() {
        assert_eq!(
            guards_of("#ifndef FOO_H\nint x;\n#endif\n").guard,
            Guard::None
        );
    }

    /// A `#define` nested one level deeper is not the guard's `#define` — it is inside another conditional,
    /// so a visit that took the other branch would not define the guard.
    #[test]
    fn a_nested_define_is_not_the_guards() {
        assert_eq!(
            guards_of("#ifndef FOO_H\n#if X\n#define FOO_H\n#endif\n#endif\n").guard,
            Guard::None
        );
    }

    /// `#pragma once` wins when both are present, which happens in headers that were migrated.
    #[test]
    fn pragma_once_wins_over_a_macro_guard() {
        assert_eq!(
            guards_of("#pragma once\n#ifndef FOO_H\n#define FOO_H\nint x;\n#endif\n").guard,
            Guard::PragmaOnce
        );
    }

    #[test]
    fn a_macro_guard_only_blocks_a_visit_once_its_macro_is_defined() {
        let guard = guards_of("#ifndef FOO_H\n#define FOO_H\nint x;\n#endif\n").guard;

        assert!(!guard.blocks_a_visit(|_| false), "not seen yet");
        assert!(guard.blocks_a_visit(|name| name == "FOO_H"), "already seen");
    }

    #[test]
    fn pragma_once_always_blocks_a_second_visit() {
        assert!(Guard::PragmaOnce.blocks_a_visit(|_| false));
        assert!(!Guard::None.blocks_a_visit(|_| true));
    }

    /// The macros a file defines are collected, because a consumer reconstructing a per-file macro
    /// environment needs them and cannot re-derive them from a skipped visit.
    #[test]
    fn the_macros_a_file_defines_are_collected() {
        let analysis = guards_of("#define A 1\n#define B 2\n#undef A\n#define C 3\n");

        assert_eq!(
            analysis.defines,
            vec!["A".into(), "B".into(), "C".into()],
            "every definition, in order, including one that was later undefined"
        );
    }
}
