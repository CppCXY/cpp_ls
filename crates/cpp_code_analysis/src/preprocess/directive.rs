//! Preprocessor directives, structured.
//!
//! The parser keeps a directive as one leaf holding its tokens — `#`, the name, and the rest of the
//! logical line — which is what a lossless tree needs and nothing more. Every consumer of a directive
//! then has to re-derive the same things: where the name ends, whether the next token is a parameter
//! list, what the condition expression is, whether `#include` used `<...>` or `"..."`. This module
//! derives them once.
//!
//! # What is *not* decided here
//!
//! Whether a condition is true. `#if` is not evaluated during this pass because evaluating it needs the
//! macro table at that point in the file, and the table is built *by* this pass — the two are one
//! traversal. So a condition is carried as its tokens, and [`crate::condition`] evaluates it against a
//! table when the caller has one.
//!
//! Nothing is reported as an error either. A directive whose arguments do not parse is
//! [`Directive::Malformed`], not a diagnostic: a file mid-edit has directives without arguments, and an
//! editor that flags `#if` on the line the user is still typing is worse than one that stays quiet.

use cpp_parser::{CppSyntaxKind, CppSyntaxNode, CppTokenKind, SourceRange};

use crate::{
    macros::{MacroDef, parse_define},
    token::{Token, is_trivia, tokens_of},
};

/// The directive's name, as a typed value.
///
/// The name is a plain identifier in the token stream — `include`, `define`, `if` are not keywords —
/// which is why this is a closed enum built from spelling rather than from token kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DirectiveKind {
    /// `#if` — a constant expression.
    If,
    /// `#ifdef` — a name.
    Ifdef,
    /// `#ifndef` — a name.
    Ifndef,
    /// `#elif` — a constant expression.
    Elif,
    /// `#else`
    Else,
    /// `#endif`
    Endif,
    /// `#define`
    Define,
    /// `#undef`
    Undef,
    /// `#include`
    Include,
    /// `#include_next` — a GCC extension used inside system headers.
    IncludeNext,
    /// `#pragma`
    Pragma,
    /// `#error`
    Error,
    /// `#warning`
    Warning,
    /// `#line`
    Line,
    /// A `#` alone on a line, which is legal and does nothing.
    Null,
    /// A name this layer does not know. Not an error — implementations add their own.
    Unknown,
}

impl DirectiveKind {
    /// Read a directive name.
    pub fn from_name(name: &str) -> DirectiveKind {
        match name {
            "if" => DirectiveKind::If,
            "ifdef" => DirectiveKind::Ifdef,
            "ifndef" => DirectiveKind::Ifndef,
            "elif" => DirectiveKind::Elif,
            "else" => DirectiveKind::Else,
            "endif" => DirectiveKind::Endif,
            "define" => DirectiveKind::Define,
            "undef" => DirectiveKind::Undef,
            "include" => DirectiveKind::Include,
            "include_next" => DirectiveKind::IncludeNext,
            "pragma" => DirectiveKind::Pragma,
            "error" => DirectiveKind::Error,
            "warning" => DirectiveKind::Warning,
            "line" => DirectiveKind::Line,
            _ => DirectiveKind::Unknown,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            DirectiveKind::If => "if",
            DirectiveKind::Ifdef => "ifdef",
            DirectiveKind::Ifndef => "ifndef",
            DirectiveKind::Elif => "elif",
            DirectiveKind::Else => "else",
            DirectiveKind::Endif => "endif",
            DirectiveKind::Define => "define",
            DirectiveKind::Undef => "undef",
            DirectiveKind::Include => "include",
            DirectiveKind::IncludeNext => "include_next",
            DirectiveKind::Pragma => "pragma",
            DirectiveKind::Error => "error",
            DirectiveKind::Warning => "warning",
            DirectiveKind::Line => "line",
            DirectiveKind::Null => "#",
            DirectiveKind::Unknown => "",
        }
    }

    /// Does this directive open a conditional region?
    pub fn opens_a_condition(self) -> bool {
        matches!(
            self,
            DirectiveKind::If | DirectiveKind::Ifdef | DirectiveKind::Ifndef
        )
    }

    /// Does this directive continue or close one?
    pub fn closes_a_condition(self) -> bool {
        matches!(
            self,
            DirectiveKind::Elif | DirectiveKind::Else | DirectiveKind::Endif
        )
    }

    /// Does this directive define or undefine a macro, and so change the macro table?
    pub fn changes_macros(self) -> bool {
        matches!(self, DirectiveKind::Define | DirectiveKind::Undef)
    }
}

/// How an `#include` spelled its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IncludeForm {
    /// `#include <vector>` — searched on the system path.
    Angle,
    /// `#include "local.h"` — searched relative to the including file first.
    Quote,
    /// `#include HEADER` — the target is a macro, and is not known until expansion.
    ///
    /// This is not rare in real code (`#include BOOST_VERSION_HEADER`), and treating it as a malformed
    /// include would be wrong: the directive is well formed, its target is just not a literal.
    Macro,
}

/// An `#include` or `#include_next`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Include {
    pub form: IncludeForm,
    /// The target without its delimiters, or the macro's spelling for [`IncludeForm::Macro`].
    pub target: Box<str>,
    /// Whether the directive was `#include_next`.
    pub is_next: bool,
}

/// A `#define`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Define {
    /// `None` when the directive is a `#define` whose arguments do not read — `#define 1 2`, or one
    /// being typed. The directive is still recognised, which is what lets a consumer see the line and
    /// a diagnostic say "this macro has no name" rather than "unknown directive".
    pub macro_def: Option<MacroDef>,
    /// A redefinition of a macro that is already defined, with a *different* body.
    ///
    /// Legal only when the two are identical, and a compiler warns when they are not — but this is the
    /// normal state of a header being edited, so it is recorded rather than reported as a parse
    /// failure.
    pub is_redefinition: bool,
}

/// One directive, with its arguments read out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Directive {
    /// `#if` / `#elif`: the condition's tokens, unevaluated.
    ///
    /// Unparsed as well as unevaluated: turning tokens into an expression needs to know which
    /// identifiers are macros, and that needs the table. See [`crate::condition::parse_condition`].
    Conditional {
        kind: DirectiveKind,
        condition: Vec<Token>,
    },
    /// `#ifdef` / `#ifndef`: the name tested.
    Ifdef {
        kind: DirectiveKind,
        name: Box<str>,
    },
    /// `#else` / `#endif`: no arguments to read.
    Marker {
        kind: DirectiveKind,
    },
    Define(Define),
    Undef {
        /// `None` when the directive has no readable name, which is the state of `#undef` with the
        /// cursor after it.
        name: Option<Box<str>>,
    },
    Include(Include),
    /// `#pragma ...`: the tokens after the name.
    ///
    /// Not interpreted. `#pragma once` and `#pragma pack` are the two that matter and neither needs a
    /// grammar; a layer that claimed to understand every pragma would have to understand every
    /// compiler's extensions.
    Pragma {
        tokens: Vec<Token>,
    },
    /// `#error` or `#warning`, with its message as written.
    Diagnostic {
        kind: DirectiveKind,
        message: String,
    },
    /// `#line ...`, with its arguments.
    Line {
        tokens: Vec<Token>,
    },
    /// A `#` alone on a line.
    Null,
    /// A name this layer does not know, or one whose arguments did not read.
    ///
    /// Carries the name so that a consumer can still tell `#pragma_custom` from ``.
    Other {
        name: Box<str>,
    },
}

impl Directive {
    pub fn kind(&self) -> DirectiveKind {
        match self {
            Directive::Conditional { kind, .. }
            | Directive::Ifdef { kind, .. }
            | Directive::Marker { kind }
            | Directive::Diagnostic { kind, .. } => *kind,
            Directive::Define(_) => DirectiveKind::Define,
            Directive::Undef { .. } => DirectiveKind::Undef,
            Directive::Include(include) => {
                if include.is_next {
                    DirectiveKind::IncludeNext
                } else {
                    DirectiveKind::Include
                }
            }
            Directive::Pragma { .. } => DirectiveKind::Pragma,
            Directive::Line { .. } => DirectiveKind::Line,
            Directive::Null => DirectiveKind::Null,
            Directive::Other { .. } => DirectiveKind::Unknown,
        }
    }

    /// The macro this directive introduces, for the table.
    pub fn defines(&self) -> Option<&MacroDef> {
        match self {
            Directive::Define(define) => define.macro_def.as_ref(),
            _ => None,
        }
    }

    /// The name this directive undefines, if it has one.
    pub fn undefines(&self) -> Option<&str> {
        match self {
            Directive::Undef { name } => name.as_deref(),
            _ => None,
        }
    }

    /// The include this directive is, if it is one.
    pub fn as_include(&self) -> Option<&Include> {
        match self {
            Directive::Include(include) => Some(include),
            _ => None,
        }
    }
}

/// A directive together with where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpannedDirective {
    pub directive: Directive,
    pub range: SourceRange,
    /// Nesting depth of the conditional regions this directive sits inside.
    ///
    /// `0` at file scope. A consumer drawing a file's structure — folding, an outline, the "which
    /// `#if` am I in" question — needs this and cannot derive it from the directive alone.
    pub condition_depth: usize,
}

/// Read every directive in a file, in source order.
///
/// Walks the tree's `PreprocessorDirective` nodes rather than re-scanning tokens. That is deliberate:
/// the parser already decided which `#` begins a directive and which is a token inside a macro body,
/// and re-deriving that would be a second implementation of the same rule, free to disagree with the
/// first.
pub fn scan_directives(root: &CppSyntaxNode) -> Vec<SpannedDirective> {
    let mut out = Vec::new();
    let mut depth = 0usize;

    for node in root.descendants() {
        if CppSyntaxKind::from(node.kind()) != CppSyntaxKind::PreprocessorDirective {
            continue;
        }

        let range = cpp_parser::source_range(node.text_range());
        let tokens = tokens_of(&node);
        let directive = parse_directive_tokens(&tokens, range);

        let kind = directive.kind();

        // `#endif` is the only directive that *ends* a region. `#elif` and `#else` continue the one they are
        // in — they are written at the same depth as the `#if` they belong to, and everything after them is
        // still inside it. Treating `#else` as a closer is a one-line mistake that corrupts the depth of every
        // directive after it in the file: the `#define` in an `#else` branch is recorded as being at file
        // scope, so a consumer looking for "which `#if` is this in" finds none.
        if kind == DirectiveKind::Endif {
            out.push(SpannedDirective {
                directive,
                range,
                // The depth it was written at, which is the one it closes.
                condition_depth: depth.saturating_sub(1),
            });
            depth = depth.saturating_sub(1);
            continue;
        }

        out.push(SpannedDirective {
            directive,
            range,
            condition_depth: depth,
        });

        if kind.opens_a_condition() {
            depth += 1;
        }
    }

    out
}

/// Read one directive from its tokens, including the leading `#`.
pub fn parse_directive_tokens(tokens: &[Token], range: SourceRange) -> Directive {
    let mut index = 0;

    // Leading whitespace and comments before the `#` are legal.
    skip_trivia(tokens, &mut index);

    match tokens.get(index).map(|token| token.kind) {
        Some(CppTokenKind::Hash) => index += 1,
        // The caller handed over tokens that are not a directive. `Null` is the honest answer: it is
        // what a bare `#` is, and it carries no arguments to misread.
        _ => return Directive::Null,
    }

    skip_trivia(tokens, &mut index);

    let Some(name) = tokens.get(index) else {
        // `#` alone on a line.
        return Directive::Null;
    };
    let kind = DirectiveKind::from_name(name.text());
    index += 1;

    // The rest of the directive, with the layout before it removed. Trivia *inside* the arguments is
    // kept: `#define A (1)` and `#define A(1)` differ only by whitespace, and so does a condition with
    // a line splice in it.
    let rest = strip_leading_trivia(&tokens[index.min(tokens.len())..]);

    match kind {
        DirectiveKind::If | DirectiveKind::Elif => Directive::Conditional {
            kind,
            condition: significant(rest),
        },
        DirectiveKind::Ifdef | DirectiveKind::Ifndef => match first_identifier(rest) {
            Some(name) => Directive::Ifdef { kind, name },
            None => Directive::Other {
                name: kind.name().into(),
            },
        },
        DirectiveKind::Else | DirectiveKind::Endif => Directive::Marker { kind },
        DirectiveKind::Define => match parse_define(rest, range) {
            Some(macro_def) => Directive::Define(Define {
                macro_def: Some(macro_def),
                // Redefinition is decided by the caller, which is the one holding the table.
                is_redefinition: false,
            }),
            // A `#define` whose arguments did not read is still a `#define`. Saying "unknown
            // directive" would lose the one fact that matters about the line.
            None => Directive::Define(Define {
                macro_def: None,
                is_redefinition: false,
            }),
        },
        DirectiveKind::Undef => Directive::Undef {
            name: first_identifier(rest),
        },
        DirectiveKind::Include | DirectiveKind::IncludeNext => parse_include(rest, kind),
        DirectiveKind::Pragma => Directive::Pragma {
            tokens: significant(rest),
        },
        DirectiveKind::Error | DirectiveKind::Warning => Directive::Diagnostic {
            kind,
            message: message_text(rest),
        },
        DirectiveKind::Line => Directive::Line {
            tokens: significant(rest),
        },
        DirectiveKind::Null | DirectiveKind::Unknown => Directive::Other {
            name: name.text.clone(),
        },
    }
}

/// Read the target of an `#include`.
fn parse_include(tokens: &[Token], kind: DirectiveKind) -> Directive {
    let is_next = kind == DirectiveKind::IncludeNext;

    let Some(first) = tokens.iter().find(|token| !is_trivia(token.kind)) else {
        return Directive::Other {
            name: kind.name().into(),
        };
    };

    // The lexer folds `<vector>` into one token when the parser asks it to, so both forms arrive as a
    // `HeaderName`. Reading the delimiters back out is still necessary for the *form*: whether a
    // header is searched relative to the including file first is the whole difference between the two
    // spellings, and the tree does not record it separately.
    let (form, target) = match first.kind {
        CppTokenKind::HeaderName => {
            let text = first.text();
            if text.starts_with('<') {
                (
                    IncludeForm::Angle,
                    text.trim_start_matches('<').trim_end_matches('>'),
                )
            } else {
                (
                    IncludeForm::Quote,
                    text.trim_start_matches('"').trim_end_matches('"'),
                )
            }
        }
        // `#include <a/b.h>` that the lexer did not fold — the tokens are still there, and the text
        // between them is the name. Reconstructing it from tokens rather than from a range keeps this
        // working for a header name that was never folded.
        CppTokenKind::Less => {
            let mut text = String::new();
            for token in tokens {
                if token.kind == CppTokenKind::Greater {
                    return Directive::Include(Include {
                        form: IncludeForm::Angle,
                        target: text.into(),
                        is_next,
                    });
                }
                text.push_str(token.text());
            }
            return Directive::Other {
                name: kind.name().into(),
            };
        }
        // `#include HEADER`, where `HEADER` is a macro. Well formed; the target is not a literal.
        CppTokenKind::Identifier => {
            return Directive::Include(Include {
                form: IncludeForm::Macro,
                target: first.text.clone(),
                is_next,
            });
        }
        _ => {
            return Directive::Other {
                name: kind.name().into(),
            };
        }
    };

    Directive::Include(Include {
        form,
        target: target.into(),
        is_next,
    })
}

/// The message of an `#error` or `#warning`, joined by single spaces.
///
/// The text as written rather than a token list: an error message is for a person to read, and every
/// consumer of it wants the string. Whitespace is normalised because the message is a diagnostic, and
/// a diagnostic whose rendering depends on the author's indentation is a worse diagnostic.
fn message_text(tokens: &[Token]) -> String {
    let mut out = String::new();
    for token in tokens.iter().filter(|token| !is_trivia(token.kind)) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(token.text());
    }
    out
}

fn first_identifier(tokens: &[Token]) -> Option<Box<str>> {
    tokens
        .iter()
        .find(|token| !is_trivia(token.kind))
        .filter(|token| token.is_identifier())
        .map(|token| token.text.clone())
}

fn significant(tokens: &[Token]) -> Vec<Token> {
    tokens
        .iter()
        .filter(|token| !is_trivia(token.kind))
        .cloned()
        .collect()
}

fn strip_leading_trivia(tokens: &[Token]) -> &[Token] {
    let mut index = 0;
    skip_trivia(tokens, &mut index);
    &tokens[index..]
}

fn skip_trivia(tokens: &[Token], index: &mut usize) {
    while tokens
        .get(*index)
        .is_some_and(|token| is_trivia(token.kind))
    {
        *index += 1;
    }
}
