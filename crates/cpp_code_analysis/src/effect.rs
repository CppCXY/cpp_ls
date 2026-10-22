//! **A file's *effect* on the macro table**: what its directives do to the environment they are read in.
//!
//! `surface_of` says what a file's directives are; this module interprets them. Together they are the replacement
//! for "cook the include closure to find out what a header does":
//!
//! ```text
//! effect(surface, environment) = define/undef events that are *active* under that environment, in order
//! ```
//!
//! # The three properties this buys (see `docs/exact-index-design.md` §七)
//!
//! ```text
//! ①  composition: a translation unit's macro table = fold(apply, effect of each included file). One hash lookup
//!    per include once the effect is cached, not a parse.
//! ②  a body does not move the effect: function bodies, statements and comments are not events, so
//!    `Surface::signature` is unchanged by them and the effect cannot change either.
//! ③  a guarded header's second inclusion is empty: its `#ifndef GUARD` is already false, so the effect has no
//!    folds at all — which is why including the same file three times costs one evaluation plus two lookups.
//! ```
//!
//! # What "cannot be said" means here, and why it is a first-class answer
//!
//! A condition the table cannot decide (`#if __has_include(<x>)` with no search paths, a value that is not a
//! number) gives [`Effect::certain`] `= false`. A caller must then fall back to the exact path — cooking the file —
//! rather than guess: an effect that quietly assumed `false` would drop every `#define` behind the condition, and
//! the index would be wrong rather than incomplete. That is the same distinction the preprocessor layer makes with
//! its `Value::Unknown`.

use std::collections::HashMap;

use crate::macros::MacroDef;
use crate::preprocess::condition::{self, Lookup, MacroValues, Value};
use crate::surface::{ConditionFlavour, Event, Surface};
use crate::token::Token;

/// **A macro table**: the environment an effect is interpreted against, and what an effect produces.
#[derive(Debug, Clone, Default)]
pub struct Macros {
    definitions: HashMap<String, MacroDef>,
}

impl Macros {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn define(&mut self, definition: MacroDef) {
        self.definitions.insert(definition.name.to_string(), definition);
    }

    pub fn undefine(&mut self, name: &str) {
        self.definitions.remove(name);
    }

    pub fn get(&self, name: &str) -> Option<&MacroDef> {
        self.definitions.get(name)
    }

    pub fn len(&self) -> usize {
        self.definitions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }

    /// **The signature of the environment an effect is read in**, over the names that matter — see
    /// [`Effect::key`].
    pub fn values_of(&self, names: &[String]) -> Vec<Option<&str>> {
        names
            .iter()
            .map(|name| self.definitions.get(name).map(body_text))
            .collect()
    }
}

impl MacroValues for Macros {
    fn lookup(&self, name: &str) -> Lookup<'_> {
        match self.definitions.get(name) {
            Some(definition) => Lookup::Defined(definition),
            // **`Undefined` rather than `Unanswered`.** This table is the whole environment a caller handed over; a
            // name it does not hold is not defined, which is what the standard's `0` means and what makes
            // `#ifndef GUARD` true.
            None => Lookup::Undefined,
        }
    }
}

/// One step an effect performs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fold {
    Define(MacroDef),
    Undef(String),
}

/// **What a file does to a macro table**, given the table it is read in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Effect {
    pub folds: Vec<Fold>,
    /// Whether every condition could be decided. `false` means a caller must not trust this effect as *the*
    /// effect — see the module documentation.
    pub certain: bool,
}

impl Effect {
    /// **The key this effect is cached under**: the file's directive signature plus the *values* of the names it
    /// tests. A file that tests nothing has the same key in every environment.
    pub fn key(signature: u64, values: &[Option<&str>]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |byte: u8| {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        };
        for byte in signature.to_le_bytes() {
            eat(byte);
        }
        for value in values {
            match value {
                Some(text) => {
                    for byte in text.as_bytes() {
                        eat(*byte);
                    }
                }
                None => eat(0xff),
            }
            eat(0xfe);
        }
        hash
    }

    /// Apply to a table: what "including this file" does.
    pub fn apply(&self, table: &mut Macros) {
        for fold in &self.folds {
            match fold {
                Fold::Define(definition) => table.define(definition.clone()),
                Fold::Undef(name) => table.undefine(name),
            }
        }
    }

    /// The names this effect defines or undefines — the *observable* part, for tests and for a caller that wants to
    /// know whether anything downstream can care.
    pub fn touched(&self) -> Vec<&str> {
        self.folds
            .iter()
            .map(|fold| match fold {
                Fold::Define(definition) => definition.name.as_ref(),
                Fold::Undef(name) => name.as_str(),
            })
            .collect()
    }
}

/// **Interpret a file's directives against an environment.**
pub fn effect_of(surface: &Surface, environment: &Macros) -> Effect {
    let mut effect = Effect {
        folds: Vec::new(),
        certain: true,
    };
    // One entry per `#if`: whether the enclosing region is active, whether a branch has been taken, and whether the
    // current branch is the active one.
    struct Region {
        outer_active: bool,
        taken: bool,
        active: bool,
    }
    let mut regions: Vec<Region> = Vec::new();
    let active = |regions: &[Region]| regions.last().map_or(true, |region| region.active);

    for event in &surface.events {
        match event {
            Event::If {
                tokens, flavour, ..
            } => {
                let outer_active = active(&regions);
                let decided = outer_active.then(|| decide(*flavour, tokens, environment));
                let holds = match decided {
                    Some(Value::Known(value)) => value != 0,
                    Some(_) => {
                        effect.certain = false;
                        false
                    }
                    None => false,
                };
                regions.push(Region {
                    outer_active,
                    taken: holds,
                    active: outer_active && holds,
                });
            }
            Event::Elif {
                tokens, flavour, ..
            } => {
                let Some(region) = regions.last_mut() else {
                    continue;
                };
                if region.taken || !region.outer_active {
                    region.active = false;
                    continue;
                }
                let holds = match decide(*flavour, tokens, environment) {
                    Value::Known(value) => value != 0,
                    _ => {
                        effect.certain = false;
                        false
                    }
                };
                region.taken = holds;
                region.active = holds;
            }
            Event::Else { .. } => {
                if let Some(region) = regions.last_mut() {
                    region.active = region.outer_active && !region.taken;
                    region.taken = true;
                }
            }
            Event::Endif { .. } => {
                regions.pop();
            }
            Event::Define { definition } => {
                if active(&regions) {
                    effect.folds.push(Fold::Define(definition.clone()));
                }
            }
            Event::Undef { name, .. } => {
                if active(&regions) {
                    effect.folds.push(Fold::Undef(name.clone()));
                }
            }
            // An `#include` is the caller's step, not this file's: a surface says *that* it includes something, and
            // the fold of the included file's own effect is what moves the table.
            Event::Include { .. } | Event::Other { .. } => {}
        }
    }

    effect
}

/// **One conditional, decided.** `#ifdef`/`#ifndef` ask whether a name exists; `#if`/`#elif` read an expression.
fn decide(flavour: ConditionFlavour, tokens: &[Token], environment: &Macros) -> Value {
    match flavour {
        ConditionFlavour::Expression => condition::evaluate(tokens, environment),
        ConditionFlavour::Defined | ConditionFlavour::NotDefined => {
            // The name is the first identifier of the line, and a condition the line does not spell is undecidable.
            let Some(name) = tokens
                .iter()
                .find(|token| token.kind == cpp_parser::CppTokenKind::Identifier)
            else {
                return Value::Unknown;
            };
            match environment.lookup(name.text.as_ref()).is_defined() {
                Some(defined) => Value::Known(i128::from(if flavour == ConditionFlavour::Defined {
                    defined
                } else {
                    !defined
                })),
                None => Value::Unknown,
            }
        }
    }
}

/// The spelling of a definition's body — what a name's *value* is for a condition, and for the key.
fn body_text(definition: &MacroDef) -> &str {
    definition
        .body
        .tokens
        .iter()
        .find(|token| !crate::token::is_trivia(token.kind))
        .map(|token| token.text.as_ref())
        .unwrap_or("")
}

/// The names a surface tests, with the values they hold in an environment — the key's material.
pub fn values_for(surface: &Surface, environment: &Macros) -> Vec<Option<String>> {
    surface
        .tests()
        .into_iter()
        .map(|name| {
            environment
                .get(&name)
                .map(|definition| body_text(definition).to_string())
        })
        .collect()
}

/// The tokens of a condition, exposed for callers that evaluate one themselves.
pub fn condition_tokens(tokens: &[Token]) -> &[Token] {
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn surface(text: &str) -> Surface {
        crate::surface::surface_of(Path::new("/p/a.h"), text)
    }

    fn table(defines: &[(&str, &str)]) -> Macros {
        let mut macros = Macros::new();
        for (name, body) in defines {
            let definition = crate::preprocess::macros::parse_define(
                &[
                    Token::new(
                        cpp_parser::CppTokenKind::Identifier,
                        *name,
                        cpp_parser::SourceRange::new(0, 0),
                    ),
                    Token::new(
                        cpp_parser::CppTokenKind::Whitespace,
                        " ",
                        cpp_parser::SourceRange::new(0, 0),
                    ),
                    Token::new(
                        cpp_parser::CppTokenKind::Identifier,
                        *body,
                        cpp_parser::SourceRange::new(0, 0),
                    ),
                ],
                cpp_parser::SourceRange::new(0, 0),
            );
            if let Some(definition) = definition {
                macros.define(definition);
            }
        }
        macros
    }

    #[test]
    fn an_unconditional_define_is_an_export_in_every_environment() {
        let surface = surface("#define A 1\n");
        let empty = Macros::new();
        let effect = effect_of(&surface, &empty);
        assert_eq!(effect.touched(), vec!["A"]);
        assert!(effect.certain);

        let mut applied = Macros::new();
        effect.apply(&mut applied);
        assert!(applied.get("A").is_some(), "the define reached the table");
    }

    #[test]
    fn a_conditional_define_follows_the_environment() {
        let surface = surface("#ifdef WIN32\n#define API __stdcall\n#endif\n");

        let without = Macros::new();
        let effect = effect_of(&surface, &without);
        assert!(effect.touched().is_empty(), "WIN32 is not defined here");

        let with = table(&[("WIN32", "1")]);
        let effect = effect_of(&surface, &with);
        assert_eq!(effect.touched(), vec!["API"], "certain={} folds={:?} tokens={:?}", effect.certain, effect.folds.len(), surface.events);
    }

    #[test]
    fn the_guarded_second_inclusion_is_empty() {
        // **Property ③**: a header that guards itself defines nothing the second time, and that is why including it
        // three times costs one evaluation and two empty ones.
        let surface = surface("#ifndef GUARD_H\n#define GUARD_H\n#define INSIDE 1\n#endif\n");
        let fresh = Macros::new();
        let first = effect_of(&surface, &fresh);
        assert_eq!(first.touched(), vec!["GUARD_H", "INSIDE"]);

        let mut table = Macros::new();
        first.apply(&mut table);
        let second = effect_of(&surface, &table);
        assert!(
            second.touched().is_empty(),
            "the guard is defined, so nothing is exported again: {:?}",
            second.touched()
        );
    }

    #[test]
    fn an_undef_takes_a_name_away() {
        let surface = surface("#define A 1\n#undef A\n");
        let mut table = Macros::new();
        effect_of(&surface, &Macros::new()).apply(&mut table);
        assert!(table.get("A").is_none(), "defined and then undefined");
    }

    #[test]
    fn a_condition_that_cannot_be_decided_is_not_certain() {
        // `#if TARGET` with `TARGET` undefined is `0` and decidable; `#if __has_include(<vector>)` is not: the
        // table has no search paths, so it answers `Unknown` and the effect must say so rather than guess.
        let surface = surface("#if __has_include(<vector>)\n#define A 1\n#endif\n");
        let effect = effect_of(&surface, &Macros::new());
        assert!(!effect.certain, "the condition cannot be decided here");
    }

    #[test]
    fn the_key_of_an_environment_insensitive_file_does_not_depend_on_it() {
        let plain = surface("#define A 1\n#include <vector>\n");
        assert!(plain.tests().is_empty());
        assert_eq!(
            Effect::key(plain.signature(), &[]),
            Effect::key(plain.signature(), &[])
        );
        // And a file that does test a name changes its key with that name's value.
        let sensitive = surface("#ifdef WIN32\n#define A 1\n#endif\n");
        let key_without = Effect::key(sensitive.signature(), &[None]);
        let key_with = Effect::key(sensitive.signature(), &[Some("1")]);
        assert_ne!(key_without, key_with);

    }
}
