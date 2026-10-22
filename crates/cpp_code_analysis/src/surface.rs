//! **A file's directives, and therefore its *export surface*** — what it defines, what it undefines, what it
//! includes, and **which macro names it reads its environment through**.
//!
//! # Why this is the foundation rather than an optimisation
//!
//! The index we have treats a file's reading as a function of its whole macro environment, and computes that
//! environment by cooking the include closure. That is 131 s per project and it recomputes the same environments
//! hundreds of times, because a header's contribution to a translation unit is **not its text** — the text never
//! enters the including file — it is the macro table it leaves behind.
//!
//! So the useful object is not "the file as a compiler reads it" but:
//!
//! ```text
//! surface(file) = { events:      #if / #define / #undef / #include, in order
//!                   tests:       the macro names its conditionals read
//!                   exports:     the names it can define
//!                   includes:    what it pulls in }
//! effect(file, environment) = surface's events interpreted against that environment
//! ```
//!
//! A file's effect is a **function of the few names it tests**, not of the whole table. Measured on a real project
//! (1 761 files, EmmyLuaCodeStyle): a file tests a median of **5** macro names (p90 25), and **312 of 1 761** test
//! none at all — so the cache key is tiny and the same key recurs across translation units. The whole project's
//! surfaces are built by this module in **867 ms**, against 131 s for the exact index.
//!
//! # What this module does not do
//!
//! It does not evaluate anything. Deciding whether `#if` holds needs a macro table, and that belongs to
//! [`crate::surface::effect`]'s future caller (`docs/exact-index-design.md` §八). Here a condition is kept as its
//! own spelling and its `tests`, which is what a key needs.

use std::path::{Path, PathBuf};

use cpp_parser::{CppTokenData, CppTokenKind, LexerConfig, SourceRange};

/// One directive, in the order it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `#if <condition>` / `#ifdef NAME` / `#ifndef NAME`.
    If {
        spelling: String,
        /// The names the condition reads: `#if A && defined(B)` tests `A` and `B`.
        tests: Vec<String>,
        range: SourceRange,
    },
    /// `#elif <condition>`.
    Elif {
        spelling: String,
        tests: Vec<String>,
        range: SourceRange,
    },
    /// `#else` / `#endif` — carried so that a condition's scope is known without re-lexing.
    Else { range: SourceRange },
    Endif { range: SourceRange },
    /// `#define NAME body` — the export.
    Define {
        name: String,
        /// `#define F(x)` rather than `#define F`.
        function_like: bool,
        /// The body as written, trimmed. Empty for a bare `#define NAME`.
        body: String,
        range: SourceRange,
    },
    /// `#undef NAME` — the anti-export, and why a surface is a sequence rather than a set.
    Undef { name: String, range: SourceRange },
    /// `#include "x"` / `#include <x>`.
    Include {
        /// As written: `"x.h"` or `<vector>`.
        spelling: String,
        angled: bool,
        range: SourceRange,
    },
    /// `#pragma`, `#error`, `#line`, and anything else — kept in sequence so a caller that evaluates conditions can
    /// see it, and ignored by [`Surface::tests`] and [`Surface::exports`].
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
                Event::Define { name, .. } => Some(name.as_str()),
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

    /// How many conditionals the file has — the shape of the effect interpreter's work.
    pub fn conditionals(&self) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event, Event::If { .. } | Event::Elif { .. }))
            .count()
    }
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
        // **A directive is a `#` and the rest of its line.** A line splice continues it; a newline ends it before
        // the newline token itself is looked at, which is why the walk stops *on* it.
        let mut end = at + 1;
        while end < tokens.len() && tokens[end].kind != CppTokenKind::Newline {
            end += 1;
        }
        let line = &tokens[at..end];
        // **Trivia is dropped first**: the lexer keeps the spaces, so `line[2]` would be a `Whitespace` token.
        let code: Vec<&CppTokenData> = line
            .iter()
            .filter(|token| token.kind != CppTokenKind::Whitespace)
            .collect();
        let line = &code[..];

        let word = line.get(1).map(|token| spelling(text, token)).unwrap_or("");
        let range = tokens[at].range;
        match word {
            "define" => {
                if let Some(name_token) = line.get(2) {
                    let name = spelling(text, name_token).to_string();
                    // `#define F(x)` is function-like only when the `(` touches the name: `#define F (x)` is an
                    // object-like macro whose body starts with a parenthesis, and telling the two apart is what
                    // every consumer of a macro table has to get right.
                    let next = line.get(3);
                    let function_like = next.is_some_and(|token| {
                        token.kind == CppTokenKind::LeftParen
                            && name_token.range.start_offset + name_token.range.length
                                == token.range.start_offset
                    });
                    let body_start = if function_like { 4 } else { 3 };
                    let body = body_of(text, &line[body_start.min(line.len())..]);
                    surface.events.push(Event::Define {
                        name,
                        function_like,
                        body,
                        range: name_token.range,
                    });
                }
            }
            "undef" => {
                if let Some(name_token) = line.get(2) {
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
                let body = body_of(text, &line[2.min(line.len())..]);
                surface.events.push(Event::Include {
                    spelling: body.clone(),
                    angled: body.starts_with('<'),
                    range: line.get(2).map(|token| token.range).unwrap_or(range),
                });
            }
            "if" | "ifdef" | "ifndef" => {
                let (tests, spelling_text) = condition_of(text, line.get(1..).unwrap_or(&[]));
                surface.events.push(Event::If {
                    spelling: spelling_text,
                    tests,
                    range,
                });
            }
            "elif" => {
                let (tests, spelling_text) = condition_of(text, line.get(1..).unwrap_or(&[]));
                surface.events.push(Event::Elif {
                    spelling: spelling_text,
                    tests,
                    range,
                });
            }
            "else" => surface.events.push(Event::Else { range }),
            "endif" => surface.events.push(Event::Endif { range }),
            _ => surface.events.push(Event::Other {
                spelling: word.to_string(),
                range,
            }),
        }

        // Past the newline, so that a `#` inside a comment or a string is never mistaken for a directive: the lexer
        // produces comment and string tokens whole, and this walk only ever starts at a real `#`.
        at = if end < tokens.len() { end + 1 } else { tokens.len() };
    }

    surface
}

fn spelling<'a>(text: &'a str, token: &CppTokenData) -> &'a str {
    let start = token.range.start_offset;
    text.get(start..start + token.range.length).unwrap_or("")
}

/// The text of a body: everything after the name, trimmed, with the file's own line breaks kept out.
fn body_of(text: &str, tokens: &[&CppTokenData]) -> String {
    let Some(first) = tokens.first() else {
        return String::new();
    };
    let start = first.range.start_offset;
    let last = tokens.last().unwrap_or(first);
    let end = last.range.start_offset + last.range.length;
    text.get(start..end).unwrap_or("").trim().to_string()
}

/// **A condition, and the names it reads.** `#if A && defined(B)` answers `["A", "B"]`, and the spelling is kept
/// for the interpreter: `defined` is removed from the tests (it is an operator here, not a name), and the words of
/// the directive itself (`if`, `ifdef`, `ifndef`, `elif`) are excluded.
fn condition_of(text: &str, tokens: &[&CppTokenData]) -> (Vec<String>, String) {
    let mut names: Vec<String> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    for token in tokens {
        let written = spelling(text, token);
        words.push(written.to_string());
        if token.kind != CppTokenKind::Identifier {
            continue;
        }
        if matches!(written, "defined" | "if" | "ifdef" | "ifndef" | "elif") {
            continue;
        }
        if !names.iter().any(|held| held == written) {
            names.push(written.to_string());
        }
    }
    names.sort();
    (names, words.join(" "))
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
        let exports: Vec<&str> = surface.exports();
        assert_eq!(exports, vec!["A", "B"]);
        assert!(
            matches!(&surface.events[1], Event::Undef { name, .. } if name == "A"),
            "the #undef is an event of its own, between the two defines: {:?}",
            surface.events
        );
    }

    #[test]
    fn a_function_like_macro_is_told_from_an_object_like_one() {
        let surface = scanned("#define F(x) ((x) + 1)\n#define G (1)\n");
        let f = surface
            .events
            .iter()
            .find(|event| matches!(event, Event::Define { name, .. } if name == "F"))
            .expect("F is an event");
        let g = surface
            .events
            .iter()
            .find(|event| matches!(event, Event::Define { name, .. } if name == "G"))
            .expect("G is an event");
        assert!(
            matches!(f, Event::Define { function_like: true, body, .. } if body.contains("+ 1")),
            "{f:?}"
        );
        assert!(
            matches!(g, Event::Define { function_like: false, body, .. } if body == "(1)"),
            "a space before the parenthesis makes it object-like: {g:?}"
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
    fn the_exports_are_the_macro_facts_a_parse_reports() {
        // **The crossing that makes this module worth having**: what the directive scan says a file defines must be
        // what a parse of the same text reports as macro facts. Checked on a file whose defines are all
        // unconditional, because a define inside an inactive `#if` is a define a parse does not report and this
        // scan cannot yet drop — that is the interpreter`s job (`docs/exact-index-design.md` §八).
        let text = "#define A 1\n#define F(x) ((x) + 1)\nint x;\n#undef A\n#define B\n";
        let surface = surface_of(Path::new("/p/a.h"), text);
        let mut from_surface: Vec<&str> = surface.exports();
        from_surface.sort();
        from_surface.dedup();

        let summary = crate::index::summarize(
            Path::new("/p/a.h"),
            text,
            crate::SummaryKey::new(0, 0),
        );
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
    fn a_hash_that_is_not_a_directive_is_not_an_event() {
        // A `#` inside a string or a comment is no directive; the lexer keeps both whole, so the scan sees neither.
        let surface = scanned("const char* s = \"#define X\";\n// #define Y\n/* #define Z */\n");
        assert!(surface.events.is_empty(), "{:?}", surface.events);
    }
}