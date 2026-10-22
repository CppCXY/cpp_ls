//! **A file's directives as data**: what it defines, what it undefines, what it includes, and the conditions it
//! reads its environment through — with the tokens needed to evaluate them.
//!
//! # Why this exists
//!
//! The index we have treats a file's reading as a function of its whole macro environment and computes that
//! environment by cooking the include closure: 131 s per project, recomputing the same environments hundreds of
//! times. But a file's contribution to a translation unit is **not its text** — the text never enters the including
//! file — it is the macro table it leaves behind, and that table is a function of **the few macro names its
//! conditionals read** (measured: median 5, p90 25, and 311 of 1 761 files read none at all).
//!
//! So the useful object is the directive list itself:
//!
//! ```text
//! surface(file)     = events in order: #if/#elif (with their condition tokens), #define, #undef, #include, other
//! effect(file, env) = those events interpreted against that environment
//! ```
//!
//! This module produces the first; [`crate::effect`] interprets it. Scanning the whole project (1 761 files) costs
//! **888 ms**, and its export names agree with what a parse reports for all 80 files sampled.
//!
//! # What it does not do
//!
//! It does not evaluate a condition. `#if` needs a macro table and an evaluator, and both live elsewhere
//! ([`crate::effect`]); what is kept here is the condition's own tokens, taken from the file so that evaluating it
//! later needs no re-lexing.

use std::path::{Path, PathBuf};

use cpp_parser::{CppTokenData, CppTokenKind, LexerConfig, SourceRange};

use crate::macros::MacroDef;
use crate::token::Token;

/// **What a conditional asks.** `#ifdef X` is not the expression `#if X`: the first asks whether the name is
/// defined at all, the second reads its *value*. Folding the first into the second is how `#ifdef X` with
/// `X` defined as nothing becomes false.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionFlavour {
    /// `#if` / `#elif` — an expression, evaluated with the macro table.
    Expression,
    /// `#ifdef` / `#elifdef` — is the name defined?
    Defined,
    /// `#ifndef` / `#elifndef` — is the name *not* defined?
    NotDefined,
}

/// One directive, in the order it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `#if <condition>` / `#ifdef NAME` / `#ifndef NAME`, with the condition's tokens.
    If {
        tokens: Vec<Token>,
        flavour: ConditionFlavour,
        /// The names the condition reads: `#if A && defined(B)` tests `A` and `B`.
        tests: Vec<String>,
        range: SourceRange,
    },
    /// `#elif <condition>`.
    Elif {
        tokens: Vec<Token>,
        flavour: ConditionFlavour,
        tests: Vec<String>,
        range: SourceRange,
    },
    /// `#else` / `#endif` — carried so that a condition's scope is known without re-lexing.
    Else { range: SourceRange },
    Endif { range: SourceRange },
    /// `#define` — the export, parsed by the same reader the preprocessor uses.
    Define { definition: MacroDef },
    /// `#undef NAME` — the anti-export, and why a surface is a sequence rather than a set.
    Undef { name: String, range: SourceRange },
    /// `#include "x"` / `#include <x>`.
    Include {
        /// As written: `"x.h"`, `<vector>`, or a macro name a caller has to expand.
        spelling: String,
        angled: bool,
        range: SourceRange,
    },
    /// `#pragma`, `#error`, `#include_next`, `#import`, anything else — kept in sequence so an interpreter sees it.
    Other { spelling: String, range: SourceRange },
}

/// **One file's directives**: everything an effect needs, and nothing a parse would add.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Surface {
    pub path: PathBuf,
    pub events: Vec<Event>,
    /// How many tokens the scan looked at — the cost model, kept for the probes.
    pub tokens: usize,
}

impl Surface {
    /// **The macro names this file reads its environment through**, sorted and deduplicated.
    ///
    /// This is the cache key's material: a file that tests nothing has one effect, whatever the environment.
    pub fn tests(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for event in &self.events {
            let tested = match event {
                Event::If { tests, .. } | Event::Elif { tests, .. } => tests,
                _ => continue,
            };
            for name in tested {
                if !names.contains(name) {
                    names.push(name.clone());
                }
            }
        }
        names.sort();
        names
    }

    /// The names the file can define, in order, with repeats — a surface is a sequence.
    pub fn exports(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Define { definition } => Some(definition.name.as_ref()),
                _ => None,
            })
            .collect()
    }

    pub fn undefinitions(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Undef { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    pub fn includes(&self) -> Vec<(&str, bool)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Include { spelling, angled, .. } => Some((spelling.as_str(), *angled)),
                _ => None,
            })
            .collect()
    }

    /// How many conditionals the file has — the shape of the interpreter's work.
    pub fn conditionals(&self) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event, Event::If { .. } | Event::Elif { .. }))
            .count()
    }

    /// **A hash of the directives and nothing else** — the edit-time question "can this file's *effect* have
    /// changed?".
    ///
    /// A function body, a comment, a statement: none of them is an event, so none of them moves this number. A
    /// define, an undef, an include or a condition does.
    pub fn signature(&self) -> u64 {
        // FNV-1a, the same arithmetic `cache::fnv1a64` uses — inlined here because this hash is over a *sequence*
        // and the shared helper takes bytes.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for event in &self.events {
            let (tag, text) = match event {
                Event::If { tokens, .. } => (1u8, spellings(tokens)),
                Event::Elif { tokens, .. } => (2, spellings(tokens)),
                Event::Else { .. } => (3, String::new()),
                Event::Endif { .. } => (4, String::new()),
                Event::Define { definition } => (
                    5,
                    format!(
                        "{}|{}|{}",
                        definition.name,
                        definition.is_function_like(),
                        spellings(&definition.body.tokens)
                    ),
                ),
                Event::Undef { name, .. } => (6, name.clone()),
                Event::Include { spelling, .. } => (7, spelling.clone()),
                Event::Other { spelling, .. } => (8, spelling.clone()),
            };
            for byte in text.as_bytes().iter().chain(std::iter::once(&tag)) {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
        }
        hash
    }
}

fn spellings(tokens: &[Token]) -> String {
    let mut out = String::new();
    for token in tokens {
        if crate::token::is_trivia(token.kind) {
            continue;
        }
        out.push_str(&token.text);
        out.push(' ');
    }
    out
}

/// **Scan one file's directives.** A lexer pass and one walk over its tokens — no tree, no expansion, no I/O.
pub fn surface_of(path: &Path, text: &str) -> Surface {
    let (tokens, _) = cpp_parser::lex(text, &LexerConfig::default());
    let mut surface = Surface {
        path: path.to_path_buf(),
        events: Vec::new(),
        tokens: tokens.len(),
    };

    let mut at = 0usize;
    while at < tokens.len() {
        if tokens[at].kind != CppTokenKind::Hash {
            at += 1;
            continue;
        }
        // **A directive is a `#` and the rest of its line.** The walk stops *on* the newline token rather than
        // past it, so the next `#` starts the next directive.
        let mut end = at + 1;
        while end < tokens.len() && tokens[end].kind != CppTokenKind::Newline {
            end += 1;
        }
        // Trivia is dropped before anything is indexed: the lexer keeps the spaces, so a raw `line[2]` would be a
        // `Whitespace` rather than the macro's name.
        let code: Vec<&CppTokenData> = tokens[at..end]
            .iter()
            .filter(|token| !crate::token::is_trivia(token.kind))
            .collect();

        let word = code.get(1).map(|token| spelling(text, token)).unwrap_or("");
        let range = tokens[at].range;
        match word {
            "define" => {
                // `parse_define` wants the tokens **from the name on**, and the whole directive's range — the same
                // reader the preprocessor uses, so a definition here is the definition a cook would get.
                // **Trivia is kept here, unlike everywhere else in this module.** `parse_body` keeps whitespace
                // because it is load-bearing: `#define F(x) +x` and `#define F(x) + x` expand differently, and the
                // body`s text is what a condition reads as a value. The `code` list above is for *finding* the name;
                // this one is for reading the body.
                let from_the_name: Vec<Token> = tokens[at + 2..end]
                    .iter()
                    .map(|token| Token::new(token.kind, spelling(text, token), token.range))
                    .collect();
                if let Some(definition) = crate::preprocess::macros::parse_define(&from_the_name, range) {
                    surface.events.push(Event::Define { definition });
                }
            }
            "undef" => {
                if let Some(name_token) = code.get(2) {
                    surface.events.push(Event::Undef {
                        name: spelling(text, name_token).to_string(),
                        range: name_token.range,
                    });
                }
            }
            "include" | "include_next" | "import" => {
                // **The spelling is the rest of the line, as written.** `#include <vector>` is `Less`, `Identifier`,
                // `Greater` to this lexer rather than one header-name token, and a `#include MACRO` is a name a
                // caller has to expand — so what is kept is the text after `include`, which is what a resolver wants
                // and what a reader sees.
                let body = body_of(text, &code[2.min(code.len())..]);
                surface.events.push(Event::Include {
                    spelling: body.clone(),
                    angled: body.starts_with('<'),
                    range: code.get(2).map(|token| token.range).unwrap_or(range),
                });
            }
            "if" | "ifdef" | "ifndef" | "elif" => {
                let condition: Vec<Token> = code
                    .iter()
                    .skip(2)
                    .map(|token| Token::new(token.kind, spelling(text, token), token.range))
                    .collect();
                let tests = names_in(&condition);
                let flavour = match word {
                    "ifdef" => ConditionFlavour::Defined,
                    "ifndef" => ConditionFlavour::NotDefined,
                    _ => ConditionFlavour::Expression,
                };
                surface.events.push(if word == "elif" {
                    Event::Elif {
                        tokens: condition,
                        flavour,
                        tests,
                        range,
                    }
                } else {
                    Event::If {
                        tokens: condition,
                        flavour,
                        tests,
                        range,
                    }
                });
            }
            "else" => surface.events.push(Event::Else { range }),
            "endif" => surface.events.push(Event::Endif { range }),
            _ => surface.events.push(Event::Other {
                spelling: word.to_string(),
                range,
            }),
        }

        at = if end < tokens.len() { end + 1 } else { tokens.len() };
    }

    surface
}

fn spelling<'a>(text: &'a str, token: &CppTokenData) -> &'a str {
    let start = token.range.start_offset;
    text.get(start..start + token.range.length).unwrap_or("")
}

/// The text of a body: everything after the name, trimmed.
fn body_of(text: &str, tokens: &[&CppTokenData]) -> String {
    let Some(first) = tokens.first() else {
        return String::new();
    };
    let start = first.range.start_offset;
    let last = tokens.last().unwrap_or(first);
    let end = last.range.start_offset + last.range.length;
    text.get(start..end).unwrap_or("").trim().to_string()
}

/// **The names a condition reads.** `defined` is an operator here rather than a name, and the directive's own word
/// never appears: `#if`/`#elif` were removed before the tokens were taken.
pub fn names_in(tokens: &[Token]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for token in tokens {
        if token.kind != CppTokenKind::Identifier {
            continue;
        }
        let written = token.text.as_ref();
        if written == "defined" || names.iter().any(|held| held == written) {
            continue;
        }
        names.push(written.to_string());
    }
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanned(text: &str) -> Surface {
        surface_of(Path::new("/p/a.h"), text)
    }

    #[test]
    fn directives_keep_the_order_they_were_written_in() {
        let surface = scanned("#define A 1\n#undef A\n#define B 2\n");
        assert_eq!(surface.exports(), vec!["A", "B"]);
        assert_eq!(surface.undefinitions(), vec!["A"]);
        assert!(
            matches!(&surface.events[1], Event::Undef { name, .. } if name == "A"),
            "the #undef is an event of its own, between the two defines: {:?}",
            surface.events
        );
    }

    #[test]
    fn a_function_like_macro_is_told_from_an_object_like_one() {
        let surface = scanned("#define F(x) ((x) + 1)\n#define G (1)\n");
        let defined: Vec<&MacroDef> = surface
            .events
            .iter()
            .filter_map(|event| match event {
                Event::Define { definition } => Some(definition),
                _ => None,
            })
            .collect();
        assert_eq!(defined.len(), 2, "{:?}", surface.events);
        assert!(defined[0].is_function_like(), "F(x) takes an argument");
        assert!(
            !defined[1].is_function_like(),
            "a space before the parenthesis makes G object-like"
        );
    }

    #[test]
    fn the_tests_are_the_names_a_conditional_reads() {
        let surface = scanned(
            "#if defined(_WIN32) && _ITERATOR_DEBUG_LEVEL > 0\nint x;\n#endif\n\
             #ifdef GUARD\n#endif\n\
             #ifndef __cplusplus\n#endif\n",
        );
        assert_eq!(
            surface.tests(),
            vec![
                "GUARD".to_string(),
                "_ITERATOR_DEBUG_LEVEL".to_string(),
                "_WIN32".to_string(),
                "__cplusplus".to_string(),
            ],
            "every name a condition asks about, and no operator"
        );
        assert_eq!(surface.conditionals(), 3);
    }

    #[test]
    fn a_file_that_tests_nothing_is_environment_insensitive() {
        let surface = scanned("#define A 1\n#include <vector>\n");
        assert!(surface.tests().is_empty());
        assert_eq!(surface.includes(), vec![("<vector>", true)]);
        assert_eq!(surface.exports(), vec!["A"]);
    }

    #[test]
    fn includes_are_kept_with_their_form() {
        let surface = scanned("#include <vector>\n#include \"local.h\"\n");
        let includes = surface.includes();
        assert_eq!(includes.len(), 2, "{includes:?}");
        assert_eq!(includes[0], ("<vector>", true));
        assert_eq!(includes[1], ("\"local.h\"", false));
    }

    #[test]
    fn a_hash_that_is_not_a_directive_is_not_an_event() {
        // A `#` inside a string or a comment is no directive; the lexer keeps both whole, so the scan sees neither.
        let surface = scanned("const char* s = \"#define X\";\n// #define Y\n/* #define Z */\n");
        assert!(surface.events.is_empty(), "{:?}", surface.events);
    }

    #[test]
    fn the_exports_are_the_macro_facts_a_parse_reports() {
        // **The crossing that makes this module worth having**: what the directive scan says a file defines must be
        // what a parse of the same text reports as macro facts. Checked on a file whose defines are all
        // unconditional, because a define inside an inactive `#if` is a define a parse does not report and this scan
        // cannot drop it yet — that is the interpreter's job.
        let text = "#define A 1\n#define F(x) ((x) + 1)\nint x;\n#undef A\n#define B\n";
        let surface = surface_of(Path::new("/p/a.h"), text);
        let mut from_surface: Vec<&str> = surface.exports();
        from_surface.sort();
        from_surface.dedup();

        let summary = crate::index::summarize(Path::new("/p/a.h"), text, crate::SummaryKey::new(0, 0));
        let mut from_parse: Vec<&str> = summary
            .macros
            .iter()
            .filter(|fact| fact.kind.is_definition())
            .map(|fact| fact.name.as_str())
            .collect();
        from_parse.sort();
        from_parse.dedup();

        assert_eq!(from_surface, from_parse, "the same names, by two readings");
    }

    #[test]
    fn a_body_edit_does_not_move_the_signature_and_a_define_does() {
        let before = scanned("#define A 1\nstruct S { int x; };\n");
        let body_changed = scanned("#define A 1\nstruct S { int x; int y; };\n");
        assert_eq!(
            before.signature(),
            body_changed.signature(),
            "a declaration in the file's body is not an event"
        );
        let macro_changed = scanned("#define A 2\nstruct S { int x; };\n");
        assert_ne!(
            before.signature(),
            macro_changed.signature(),
            "a different body for the same macro is a different effect"
        );
        let include_added = scanned("#define A 1\n#include <vector>\nstruct S { int x; };\n");
        assert_ne!(before.signature(), include_added.signature());
    }
}
