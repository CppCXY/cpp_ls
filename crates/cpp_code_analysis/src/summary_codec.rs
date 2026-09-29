//! Reading and writing [`FileSummary`] as bytes, so the index survives a restart.
//!
//! # Why the format is written by hand
//!
//! A summary is a flat list of small records with five field types between them: a `u32`, a `u64`, a length-prefixed
//! string, a `SourceRange`, and a handful of enums. `serde` plus a binary format would generate the same bytes with
//! a build dependency, an attribute on every type, and a version negotiation nobody can read — and the schema is
//! That is the design's to define, not a macro's. The project has already made this trade once, in
//! `include/paths.rs`, for the same reason.
//!
//! # The one rule that matters
//!
//! **A byte stream that does not parse is an error, never a default.** Every read is checked against the length
//! that is left, and a truncated or corrupt entry is rejected rather than half-read into a summary with plausible
//! facts. A cache is an optimisation: a miss costs a re-parse, while a *wrong* entry costs a wrong answer that
//! nobody can see — which is the failure mode the crate's `A0` class is about.
//!
//! # What is not in the format
//!
//! No path is validated, no range is checked against a file length, and nothing here knows what a `DeclFact` means.
//! This module's contract is exactly "the values that went in come out", and it is tested by round-tripping a
//! summary that uses every field rather than by inspecting bytes — a byte-level test would pin the encoding without
//! saying anything about whether it is *correct*.

use std::path::PathBuf;

use cpp_parser::{MacroBody, SourceRange};
use cpp_parser::SymbolKind;

use crate::cache::SummaryKey;
use crate::preprocess::directive::IncludeForm;
use crate::summary::{
    ConditionalRegion, DeclFact, DeclKind, FactGuard, FileSummary, GuardBranch, IncludeFact,
    MacroFact, MacroKind, MacroScopeReading, SummaryGuards, TranslationUnit, TuEvent, TuFrame,
};

/// The eight bytes a summary file starts with.
///
/// A magic number so that reading the wrong file is a loud error rather than a plausible summary: a cache
/// directory can be pointed at a truncated write, a zero-length file, or something else entirely, and the one
/// thing this format must never do is invent facts from bytes that were never written as facts.
const MAGIC: &[u8; 8] = b"CPPLSSUM";

/// The version of the *byte format*, separate from [`crate::FORMAT_VERSION`].
///
/// `FORMAT_VERSION` is part of the cache key and answers "was this written by a producer whose answers are still
/// right?". This answers a narrower question — "can these bytes be read by this decoder at all?" — and the two
/// change for different reasons: adding a fact field bumps both, while reordering the fields of one record bumps
/// only this one.
///
/// It is written into the file as well as into the key, because a decoder can be handed a file it did not
/// choose: a caller that computed a key with the wrong format version, or a stale directory. Reading the number
/// back is what makes that a rejected read instead of a misread one.
///
/// # Version 2
///
/// The key lost its `macro_env_hash`, so the header record lost a `u64`. `FORMAT_VERSION` deliberately did **not**
/// move with it: removing a component from the key can only make an old entry unreachable, never mis-served,
/// because the stored key is compared against a freshly computed one before the summary is used. The two numbers
/// move for different reasons, and that is the reason this one exists.
///
/// # Version 3
///
/// An include fact gained `is_next`, so a `#include` record gained a flag. It is stored for one reason: a stored
/// `resolved` is re-checked against the filesystem before it is used — the key cannot name which candidate paths
/// exist — and a re-check that skipped candidates differently from the search that produced the answer would be
/// checking something else. See [`crate::summary::IncludeFact::as_include`].
///
/// # Version 4
///
/// A macro fact gained `kind`: an `#undef` is a fact about a macro name's history in exactly the way a `#define`
/// is, and a table that only remembers definitions cannot answer "is this name a macro here" — it can only
/// answer "was it ever one".
///
/// # Version 5
///
/// A declaration fact gained `type_of`: the type a variable was written with, which is what a member access has
/// to know to get from `widget` to `Widget`.
///
/// # Version 6
///
/// A declaration fact gained `clean` — that is the bump to version 7: whether a diagnostic fell inside the
/// declaration the fact was written in. It is a fact about the *file's text* rather than about the language, which
/// is why it belongs here and not in a query — and it is stored rather than recomputed because a consumer of a
/// summary no longer has the tree the errors came from.
///
/// # Version 7
///
/// A declaration fact gained `local` — the bump to version 8: whether the declaration was written inside a
/// function body, a block or a lambda, so that a name lookup across files can leave it out instead of offering a
/// name the reader cannot see. (The headings name the version a change came *from*; the number below is the one
/// the bytes carry.)
///
/// # Version 8
///
/// A declaration fact gained `returns` — the bump to version 9: the type a **function** returns, which is what a
/// *call* has where the function's own name has no type at all. A trailing return type is why it is a field of its
/// own rather than a reading of `type_of`: `auto make() -> Widget` spells the type after the parameter list.
/// (The headings name the version a change came *from*; the number below is the one the bytes carry.)
///
/// # Version 9
///
/// A macro fact gained `settles_the_name` — the bump to version 10: whether the conditional the fact is written in
/// cannot change **whether the name is a macro** afterwards (the `#ifndef NAME / #define NAME` idiom and a region
/// whose every branch agrees). A byte per macro fact, and the field is a conclusion drawn from this file's own
/// directives, so no other file's text can change it.
///
/// # Version 10
///
/// A summary's guards gained the **conditional structure** — the bump to version 11: for each region, the branches
/// written for it with the condition each asks, the body each guards, and the region it is nested in. Until now a
/// summary held a region's *span* and nothing else, which is enough to say "this fact is inside an `#if`" and not
/// enough to ask whether that `#if` was taken. The question is stored rather than answered because the answer
/// depends on the compilation (`-D`s, `-std=`, the compiler's predefined names) while the summary's key does not —
/// see [`crate::summary::SummaryGuards`]. Only [`CODEC_VERSION`] moves: an old entry is unreachable, not wrong.
///
/// # Version 11
///
/// A macro fact gained `value` — the bump to version 12: the macro's body **when it is one integer literal**,
/// which is the only body a condition can read a number out of (see [`crate::summary::MacroFact::value`]). It is
/// stored for the same reason the conditions are: the walk that evaluates them has to know what a header defined
/// before the point it is asking about, and a fact without a value answers `defined(NAME)` while leaving
/// `#if NAME` — and therefore `#if __cplusplus >= 201703L && _GLIBCXX_USE_CXX11_ABI` — undecidable. Only
/// [`CODEC_VERSION`] moves: this is a new field on a fact, not a different answer from the same text.
/// # Version 12
///
/// A summary's guards gained `own_guard` — the bump to version 13: which region the file's **own include guard**
/// opens, when it has one. The index already treated the facts guarded by exactly that region as unconditional
/// (`deguard_the_files_own_guard`); storing the index is what lets a *walk* extend the same rule to the facts
/// nested inside it, which is where a real header puts everything — `#ifndef GUARD / #define GUARD` and then a file
/// full of `#if __cplusplus` blocks. Without it those blocks are evaluated against a state in which the guard has
/// already defined its own name, and a `#ifndef GUARD` read second is false. Only [`CODEC_VERSION`] moves.
///
/// # Version 13
///
/// A summary gained `macro_readings` — the bump to version 14: the places where a **scope** came out of a macro's
/// replacement list rather than out of the file's own braces, each with the body it was read from. This is the one
/// field in a summary whose evidence is in another file (`_STD_BEGIN`'s `namespace std {` is in `yvals_core.h`,
/// and it scopes every declaration in MSVC's `<vector>`), so it is also the one field a consumer may need to
/// *check* rather than trust — see [`crate::summary::MacroScopeReading`]. Only [`CODEC_VERSION`] moves: the key is
/// still the text and the compilation context, so an entry written before this field existed is unreachable rather
/// than wrong, and `cache.rs`'s rule about the macro environment stands.
///
/// # Version 14
///
/// A fact's `type_of` is now read from the **syntax** rather than assembled out of the declaration's text — the
/// bump to version 16. The field is the same field, and the change is what it may contain: the text-based reader
/// left declaration specifiers in (`const [[nodiscard]] constexpr size_type`), cut a template argument list in the
/// wrong place (`Point>` for `std::vector<Point>`), and could put a keyword where a type goes
/// (`friend constexpr iter_difference_t`). Measured on the MinGW standard-library closure, 30 185 declarations:
/// **11 spellings that are not types before, 0 after**.
///
/// This one *does* move [`FORMAT_VERSION`] as well, and the reason is the opposite of the usual one: the bytes of an
/// old entry still decode, so an old entry is not unreachable — it is **wrong**, and every consumer of `type_of`
/// (a member access's class, a completion's list, a hover's answer) would be reading a spelling the current reader
/// would never write. "An entry that decodes but says something the producer no longer believes" is exactly what
/// [`FORMAT_VERSION`] is for, so both numbers move and the old cache is dropped on sight.
///
/// # Version 15
///
/// A fact gained `parameters` — the bump to version 17: **the names a class template declares its parameters with**,
/// which is what turns a member's own `_Ty&` into `int&` once a use says what `_Ty` is. It is stored rather than read
/// when it is wanted because the declaration is usually in *another file*: a member query holds `a.cpp`, the
/// parameters are written in `<vector>`, and the index keeps summaries rather than the text they came from.
///
/// Only [`CODEC_VERSION`] moves, which is the ordinary case for a new field: an entry written before it existed has
/// no parameters for its class templates, so its members' types stay `_Ty&` — a *weaker* answer rather than a wrong
/// one, and the reader rejects it on the version number anyway.
///
/// # Version 18
///
/// A fact gained `parameter_list` — the parameters a **function** was declared with, as the file spells them
/// (`(_Ty* _First, size_type _Count)`), which is what a completion's detail line shows beside a name from another
/// file. Until now every function in the index was described as `returns (…)`: the fact had a return type and
/// nothing else, so a popup listing a hundred standard-library names could not tell `std::format` from
/// `std::format_to`.
///
/// Only [`CODEC_VERSION`] moves: an entry written before it existed has no parameter list, and the detail line
/// falls back to the `(…)` it used to print — a weaker answer, not a wrong one.
pub const CODEC_VERSION: u32 = 18;

/// Write a summary as bytes.
///
/// The encoding is little-endian throughout, which is what every target this runs on uses in practice; the
/// alternative is a byte-order marker per file for a case that does not arise.
pub fn encode(summary: &FileSummary) -> Vec<u8> {
    let mut out = Vec::with_capacity(256 + summary.declarations.len() * 48);

    out.extend_from_slice(MAGIC);
    put_u32(&mut out, CODEC_VERSION);
    put_u32(&mut out, summary.key.format_version);
    put_u64(&mut out, summary.key.content_hash);
    put_u64(&mut out, summary.key.context_hash);
    put_u64(&mut out, summary.key.reading_fingerprint);

    put_path(&mut out, &summary.path);

    put_u32(&mut out, summary.declarations.len() as u32);
    for fact in &summary.declarations {
        put_str(&mut out, &fact.name);
        put_opt_str(&mut out, fact.scope.as_deref());
        put_u8(&mut out, decl_kind_code(fact.kind));
        put_opt_str(&mut out, fact.type_of.as_deref());
        put_opt_str(&mut out, fact.returns.as_deref());
        put_u32(&mut out, fact.bases.len() as u32);
        for base in &fact.bases {
            put_str(&mut out, base);
        }
        put_u32(&mut out, fact.parameters.len() as u32);
        for parameter in &fact.parameters {
            put_str(&mut out, parameter);
        }
        put_opt_str(&mut out, fact.parameter_list.as_deref());
        put_range(&mut out, fact.range);
        put_range(&mut out, fact.name_range);
        put_u8(&mut out, u8::from(fact.local));
        put_u8(&mut out, u8::from(fact.clean));
        put_u32(&mut out, guard_code(fact.guard));
    }

    put_u32(&mut out, summary.macros.len() as u32);
    for fact in &summary.macros {
        put_str(&mut out, &fact.name);
        put_u8(&mut out, macro_kind_code(fact.kind));
        put_u8(&mut out, u8::from(fact.function_like));
        put_u8(&mut out, macro_body_code(fact.body));
        put_opt_str(&mut out, fact.value.as_deref());
        put_range(&mut out, fact.range);
        match fact.body_range {
            Some(range) => {
                put_u8(&mut out, 1);
                put_range(&mut out, range);
            }
            None => put_u8(&mut out, 0),
        }
        put_u32(&mut out, guard_code(fact.guard));
        put_u8(&mut out, u8::from(fact.settles_the_name));
    }

    put_u32(&mut out, summary.includes.len() as u32);
    for fact in &summary.includes {
        put_u8(&mut out, include_form_code(fact.form));
        put_str(&mut out, &fact.spelling);
        put_u8(&mut out, u8::from(fact.is_next));
        match &fact.resolved {
            Some(path) => {
                put_u8(&mut out, 1);
                put_path(&mut out, path);
            }
            None => put_u8(&mut out, 0),
        }
        put_range(&mut out, fact.range);
        put_u32(&mut out, guard_code(fact.guard));
    }

    put_u32(&mut out, summary.guards.regions.len() as u32);
    for region in &summary.guards.regions {
        put_range(&mut out, *region);
    }

    put_u32(&mut out, summary.guards.conditionals.len() as u32);
    for conditional in &summary.guards.conditionals {
        put_u32(&mut out, conditional.branches.len() as u32);
        for branch in &conditional.branches {
            put_u8(&mut out, conditional_kind_code(branch.kind));
            put_opt_str(&mut out, branch.condition.as_deref());
            put_range(&mut out, branch.body);
            put_range(&mut out, branch.range);
        }

        // `None` is written as `u32::MAX`, the same spelling a fact's `FactGuard::Unconditional` uses for "no
        // region": an index no file can have, rather than a number that means something else in another field.
        put_u32(&mut out, conditional.parent.unwrap_or(u32::MAX));
    }

    put_u32(&mut out, summary.guards.own_guard.unwrap_or(u32::MAX));

    // The readings come last, and they are the only section whose absence a reader could not detect from the
    // others: a file with no macro-opened scope writes one zero. That is the same shape a reader written before
    // version 14 would produce for a summary that has them, which is why the version is what keeps the two apart
    // rather than the count.
    put_u32(&mut out, summary.macro_readings.len() as u32);
    for reading in &summary.macro_readings {
        put_range(&mut out, reading.range);
        put_str(&mut out, &reading.name);
        put_str(&mut out, &reading.body);
        match &reading.opens {
            Some(segments) => {
                put_u8(&mut out, 1);
                put_u32(&mut out, segments.len() as u32);
                for segment in segments {
                    put_str(&mut out, segment);
                }
            }
            None => put_u8(&mut out, 0),
        }
    }

    out
}

/// Read a summary back, or say why the bytes are not one.
///
/// Every failure is a [`DecodeError`] and not a partially built summary: the contract in the module
/// documentation is that bytes which do not parse are rejected, and the way to keep that contract is to have no
/// path that returns `Ok` with a field left at its default.
/// Write a **translation unit's timeline** as bytes.
///
/// The second format in this module, and it lives here rather than beside its type because the two share the
/// magic number, the codec version and every reader and writer below: two cache files with two private readers is
/// two places for a length to be written one way and read another.
///
/// What is here is the whole walk — every macro fact in translation order with the frame it was written in, and
/// every frame with where it was entered and what it contains — because that is what makes a file's environment
/// answerable without walking anything. `entered` is **not** written: it is the first frame per file, which the
/// frames already say.
///
/// A caller does not encode a timeline it has not validated: see [`crate::index::store`], where the entry carries
/// the closure's content hashes and is refused when one of them moved.
pub fn encode_translation_unit(unit: &TranslationUnit, closure: &[(std::path::PathBuf, u64)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + unit.events.len() * 24);

    out.extend_from_slice(MAGIC);
    put_u32(&mut out, CODEC_VERSION);
    put_u32(&mut out, crate::FORMAT_VERSION);
    put_u64(&mut out, crate::READING_FINGERPRINT);
    // **The closure, and the content hash of every file in it.** This is what makes an entry checkable: a decode
    // is only as good as the claim that these files still say what they said, and the caller re-checks that claim
    // against the filesystem — see `crate::tu_cache`. Without it a cache would serve the macros of a header
    // somebody has edited, which is the failure mode clang's dependency scanner removed a shared `FileManager`
    // over.
    put_u32(&mut out, closure.len() as u32);
    for (path, hash) in closure {
        put_path(&mut out, path);
        put_u64(&mut out, *hash);
    }

    put_u32(&mut out, unit.conditional_facts as u32);
    put_u32(&mut out, unit.facts_in_force as u32);

    put_u32(&mut out, unit.frames.len() as u32);
    for frame in &unit.frames {
        put_path(&mut out, &frame.file);
        match frame.parent {
            Some(parent) => {
                put_u8(&mut out, 1);
                put_u32(&mut out, parent);
            }
            None => put_u8(&mut out, 0),
        }
        put_u64(&mut out, frame.from_in_parent as u64);
        put_u32(&mut out, frame.entry_seq);
        put_u32(&mut out, frame.tout);
    }

    put_u32(&mut out, unit.events.len() as u32);
    for event in &unit.events {
        put_str(&mut out, &event.name);
        // Three states, not two: an `#undef` has no arity, and "nobody said" is not "object-like".
        match event.function_like {
            None => put_u8(&mut out, 0),
            Some(false) => put_u8(&mut out, 1),
            Some(true) => put_u8(&mut out, 2),
        }
        match event.body {
            Some(body) => {
                put_u8(&mut out, 1);
                put_u8(&mut out, macro_body_code(body));
            }
            None => put_u8(&mut out, 0),
        }
        put_opt_str(&mut out, event.body_text.as_deref());
        put_opt_str(&mut out, event.parameters.as_deref());
        // **Where the replacement list is in the file that wrote it** — a range, not text: it is what makes a
        // definition the decoder rebuilds carry real positions (`MacroDef::written_in`). `None` for an `#undef`
        // and for a `#define` with an empty replacement list, which is the same distinction the text keeps.
        match event.body_range {
            Some(range) => {
                put_u8(&mut out, 1);
                put_u64(&mut out, range.start_offset as u64);
                put_u64(&mut out, range.length as u64);
            }
            None => put_u8(&mut out, 0),
        }
        put_u32(&mut out, event.frame);
        put_u64(&mut out, event.at as u64);
        put_u8(&mut out, u8::from(event.unconditional));
    }

    out
}

/// Read a timeline written by [`encode_translation_unit`].
pub fn decode_translation_unit(
    bytes: &[u8],
) -> Result<(TranslationUnit, Vec<(std::path::PathBuf, u64)>), DecodeError> {
    let mut reader = Reader::new(bytes);
    if reader.take(MAGIC.len())? != MAGIC {
        return Err(DecodeError::NotASummary);
    }
    if reader.u32()? != CODEC_VERSION {
        return Err(DecodeError::UnsupportedVersion);
    }
    if reader.u32()? != crate::FORMAT_VERSION {
        return Err(DecodeError::UnsupportedVersion);
    }
    if reader.u64()? != crate::READING_FINGERPRINT {
        return Err(DecodeError::UnsupportedVersion);
    }

    let mut closure = Vec::new();
    for _ in 0..reader.count()? {
        let path = reader.path()?;
        closure.push((path, reader.u64()?));
    }

    let conditional_facts = reader.u32()? as usize;
    let facts_in_force = reader.u32()? as usize;

    let frame_count = reader.count()?;
    let mut frames = Vec::with_capacity(frame_count);
    for _ in 0..frame_count {
        let file = reader.path()?;
        let parent = match reader.u8()? {
            0 => None,
            1 => Some(reader.u32()?),
            _ => return Err(DecodeError::BadDiscriminant),
        };
        frames.push(TuFrame {
            file,
            parent,
            from_in_parent: reader.u64()? as usize,
            entry_seq: reader.u32()?,
            tout: reader.u32()?,
        });
    }

    let event_count = reader.count()?;
    let mut events = Vec::with_capacity(event_count);
    for _ in 0..event_count {
        let name: std::sync::Arc<str> = std::sync::Arc::from(reader.string()?);
        let function_like = match reader.u8()? {
            0 => None,
            1 => Some(false),
            2 => Some(true),
            _ => return Err(DecodeError::BadDiscriminant),
        };
        let body = match reader.u8()? {
            0 => None,
            1 => Some(macro_body_from(reader.u8()?)?),
            _ => return Err(DecodeError::BadDiscriminant),
        };
        let body_text = reader.optional_string()?.map(std::sync::Arc::from);
        let parameters = reader.optional_string()?.map(std::sync::Arc::from);
        let body_range = match reader.u8()? {
            0 => None,
            1 => Some(cpp_parser::SourceRange::new(
                reader.u64()? as usize,
                reader.u64()? as usize,
            )),
            _ => return Err(DecodeError::BadDiscriminant),
        };
        let frame = reader.u32()?;
        let at = reader.u64()? as usize;
        let unconditional = match reader.u8()? {
            0 => false,
            1 => true,
            _ => return Err(DecodeError::BadDiscriminant),
        };

        // A frame id is an index into `frames`, and an event that names one that is not there is a corrupt file
        // rather than an event to be dropped: every later view of this unit would index out of bounds.
        if frame as usize >= frames.len() {
            return Err(DecodeError::BadDiscriminant);
        }

        events.push(TuEvent {
            name,
            function_like,
            body,
            body_text,
            parameters,
            body_range,
            frame,
            at,
            unconditional,
        });
    }

    if !reader.is_empty() {
        return Err(DecodeError::TrailingBytes);
    }

    Ok((
        TranslationUnit::from_parts(events, frames, conditional_facts, facts_in_force),
        closure,
    ))
}
/// Could **this binary** serve the summary whose first bytes are `header`? — the question a cache sweep asks of a
/// file it will not otherwise read.
///
/// The header is the magic, the two version numbers, and the key; a file written by another decoder, another
/// producer or another reader (`READING_FINGERPRINT`) can never be served, because the key it would be looked up
/// under is computed from the same numbers and cannot equal it. Such a file is not a stale answer waiting to be
/// caught — it is unreachable, which is the definition of garbage.
pub fn is_servable(header: &[u8]) -> bool {
    let mut reader = Reader::new(header);

    let Ok(magic) = reader.take(MAGIC.len()) else {
        return false;
    };
    if magic != MAGIC {
        return false;
    }

    matches!(
        (reader.u32(), reader.u32(), reader.u64(), reader.u64(), reader.u64()),
        (Ok(codec), Ok(format), Ok(_), Ok(_), Ok(fingerprint))
            if codec == CODEC_VERSION
                && format == crate::FORMAT_VERSION
                && fingerprint == crate::READING_FINGERPRINT
    )
}

/// How many bytes at the start of a summary file [`is_servable`] needs.
pub const HEADER_LENGTH: usize = 8 + 4 + 4 + 8 + 8 + 8;

pub fn decode(bytes: &[u8]) -> Result<FileSummary, DecodeError> {
    let mut reader = Reader::new(bytes);

    if reader.take(MAGIC.len())? != MAGIC {
        return Err(DecodeError::NotASummary);
    }
    if reader.u32()? != CODEC_VERSION {
        return Err(DecodeError::UnsupportedVersion);
    }

    let key = SummaryKey {
        format_version: reader.u32()?,
        content_hash: reader.u64()?,
        context_hash: reader.u64()?,
        // Written and read back like the rest of the key, because a decoder can be handed a file it did not choose
        // — see `CODEC_VERSION`'s note on the header. The number itself is a property of the *binary*, so a
        // mismatch is a failed read rather than a value to keep: a summary produced by a different reader must not
        // be served, and the key comparison in `store` is what rejects it.
        reading_fingerprint: reader.u64()?,
    };
    let path = reader.path()?;

    let mut declarations = Vec::new();
    for _ in 0..reader.count()? {
        declarations.push(DeclFact {
            name: reader.string()?,
            scope: reader.optional_string()?,
            kind: decl_kind_from(reader.u8()?)?,
            type_of: reader.optional_string()?,
            returns: reader.optional_string()?,
            bases: {
                let mut bases = Vec::new();
                for _ in 0..reader.count()? {
                    bases.push(reader.string()?);
                }
                bases
            },
            parameters: {
                let mut parameters = Vec::new();
                for _ in 0..reader.count()? {
                    parameters.push(reader.string()?);
                }
                parameters
            },
            parameter_list: reader.optional_string()?,
            range: reader.range()?,
            name_range: reader.range()?,
            local: reader.u8()? != 0,
            clean: reader.u8()? != 0,
            guard: guard_from(reader.u32()?)?,
        });
    }

    let mut macros = Vec::new();
    for _ in 0..reader.count()? {
        macros.push(MacroFact {
            name: reader.string()?,
            kind: macro_kind_from(reader.u8()?)?,
            function_like: reader.u8()? != 0,
            body: macro_body_from(reader.u8()?)?,
            value: reader.optional_string()?.map(Box::from),
            range: reader.range()?,
            body_range: match reader.u8()? {
                0 => None,
                1 => Some(reader.range()?),
                _ => return Err(DecodeError::BadDiscriminant),
            },
            guard: guard_from(reader.u32()?)?,
            settles_the_name: reader.u8()? != 0,
        });
    }

    let mut includes = Vec::new();
    for _ in 0..reader.count()? {
        let form = include_form_from(reader.u8()?)?;
        let spelling = reader.string()?;
        let is_next = match reader.u8()? {
            0 => false,
            1 => true,
            _ => return Err(DecodeError::BadDiscriminant),
        };
        let resolved = match reader.u8()? {
            0 => None,
            1 => Some(reader.path()?),
            _ => return Err(DecodeError::BadDiscriminant),
        };
        includes.push(IncludeFact {
            form,
            spelling,
            resolved,
            is_next,
            range: reader.range()?,
            guard: guard_from(reader.u32()?)?,
        });
    }

    let mut guards = SummaryGuards::default();
    for _ in 0..reader.count()? {
        guards.regions.push(reader.range()?);
    }

    for _ in 0..reader.count()? {
        let mut branches = Vec::new();
        for _ in 0..reader.count()? {
            branches.push(GuardBranch {
                kind: conditional_kind_from(reader.u8()?)?,
                condition: reader.optional_string()?.map(Box::from),
                body: reader.range()?,
                range: reader.range()?,
            });
        }

        guards.conditionals.push(ConditionalRegion {
            branches,
            parent: match reader.u32()? {
                u32::MAX => None,
                parent => Some(parent),
            },
        });
    }

    guards.own_guard = match reader.u32()? {
        u32::MAX => None,
        region => Some(region),
    };

    let mut macro_readings = Vec::new();
    for _ in 0..reader.count()? {
        let range = reader.range()?;
        let name = reader.string()?;
        let body = reader.string()?;
        let opens = match reader.u8()? {
            0 => None,
            1 => {
                let mut segments = Vec::new();
                for _ in 0..reader.count()? {
                    segments.push(reader.string()?);
                }
                Some(segments)
            }
            _ => return Err(DecodeError::BadDiscriminant),
        };

        macro_readings.push(MacroScopeReading {
            range,
            name,
            body,
            opens,
        });
    }

    // Trailing bytes mean the file was written by something this decoder does not agree with — a newer producer,
    // or two records where one was expected. Ignoring them would be accepting a file whose *content* is not what
    // its own encoding says, which is the one thing a cache must not do.
    if !reader.is_empty() {
        return Err(DecodeError::TrailingBytes);
    }

    Ok(FileSummary {
        path,
        key,
        declarations,
        macros,
        includes,
        guards,
        macro_readings,
    })
}

/// Why a byte stream is not a summary.
///
/// A vocabulary rather than a string, because a caller has a decision to make about each: a truncated file and a
/// file from another producer are both "rebuild it", while `NotASummary` may mean the path was wrong and the
/// caller is reading something it should not touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The file does not begin with the format's magic number.
    NotASummary,
    /// The byte format is not [`CODEC_VERSION`].
    UnsupportedVersion,
    /// The bytes end in the middle of a value.
    Truncated,
    /// A length prefix or count is larger than the rest of the file could hold.
    Implausible,
    /// A field that is one of a small set held a value outside it.
    BadDiscriminant,
    /// Bytes remained after the last field.
    TrailingBytes,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            DecodeError::NotASummary => "not a summary file",
            DecodeError::UnsupportedVersion => "summary written by another format version",
            DecodeError::Truncated => "summary is truncated",
            DecodeError::Implausible => "summary declares more data than it contains",
            DecodeError::BadDiscriminant => "summary holds a value outside its vocabulary",
            DecodeError::TrailingBytes => "summary has bytes left over",
        };
        f.write_str(text)
    }
}

impl std::error::Error for DecodeError {}

// ---------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------

/// A cursor over the bytes, which is the whole of the decoder's state.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, at: 0 }
    }

    fn is_empty(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.at.checked_add(count).ok_or(DecodeError::Implausible)?;
        let slice = self.bytes.get(self.at..end).ok_or(DecodeError::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// A count, rejected when it could not possibly be satisfied by the bytes that are left.
    ///
    /// The check is what keeps a corrupt length from making the decoder allocate: a `u32` read out of the middle
    /// of a string can be four billion, and a `Vec::with_capacity` on it is a denial of service in an editor. Every
    /// record is at least one byte, so a count larger than the remaining length is a fact about the file rather
    /// than a guess about it.
    fn count(&mut self) -> Result<usize, DecodeError> {
        let count = self.u32()? as usize;
        if count > self.bytes.len() - self.at {
            return Err(DecodeError::Implausible);
        }
        Ok(count)
    }

    fn string(&mut self) -> Result<String, DecodeError> {
        let length = self.count()?;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::BadDiscriminant)
    }

    fn optional_string(&mut self) -> Result<Option<String>, DecodeError> {
        match self.u8()? {
            0 => Ok(None),
            1 => self.string().map(Some),
            _ => Err(DecodeError::BadDiscriminant),
        }
    }

    /// A path, as the bytes it was written with.
    ///
    /// Written as raw bytes rather than as a string because a path is not required to be UTF-8 and round-tripping
    /// it through a `String` would rename a file the user has. On the platforms this runs on the conversion is
    /// lossless for every path that exists.
    fn path(&mut self) -> Result<PathBuf, DecodeError> {
        let length = self.count()?;
        let bytes = self.take(length)?;
        Ok(path_from_bytes(bytes))
    }

    fn range(&mut self) -> Result<SourceRange, DecodeError> {
        let start = self.u64()? as usize;
        let length = self.u64()? as usize;
        Ok(SourceRange::new(start, length))
    }
}

/// A path from its raw bytes.
#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

/// A path from its raw bytes.
///
/// On Windows a path is UTF-16, so the bytes written are the UTF-8 of its lossy spelling — which round-trips
/// exactly for every path a user can type and for everything a Rust program produces from a `&str`. A path that
/// is genuinely not representable is stored as the replacement-character spelling, which is a wrong path rather
/// than a missing one; the alternative is a `Result` on every path read for a case that cannot arise from this
/// crate's own writers.
#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

fn put_u8(out: &mut Vec<u8>, value: u8) {
    out.push(value);
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, value: &str) {
    put_u32(out, value.len() as u32);
    out.extend_from_slice(value.as_bytes());
}

fn put_opt_str(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(text) => {
            put_u8(out, 1);
            put_str(out, text);
        }
        None => put_u8(out, 0),
    }
}

fn put_range(out: &mut Vec<u8>, range: SourceRange) {
    put_u64(out, range.start_offset as u64);
    put_u64(out, range.length as u64);
}

/// A path as the bytes it is written with.
#[cfg(unix)]
fn put_path(out: &mut Vec<u8>, path: &std::path::Path) {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    put_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

/// A path as the bytes it is written with. See [`path_from_bytes`] for what "its bytes" means on Windows.
#[cfg(not(unix))]
fn put_path(out: &mut Vec<u8>, path: &std::path::Path) {
    put_str(out, &path.to_string_lossy());
}

// ---------------------------------------------------------------------------------------------
// The small vocabularies
//
// Each is written as an explicit number and read back through a checked conversion, rather than as "the enum's
// discriminant". A `#[repr]`-less enum's numbers are the compiler's business, and a format that depends on them
// changes meaning when a variant is inserted in the middle — silently, and only for files written before the
// change.
// ---------------------------------------------------------------------------------------------

fn decl_kind_code(kind: DeclKind) -> u8 {
    match kind {
        DeclKind::Type => 1,
        DeclKind::Function => 2,
        DeclKind::Variable => 3,
        DeclKind::Namespace => 4,
        DeclKind::MacroLike => 5,
        DeclKind::Other => 6,
    }
}

fn decl_kind_from(code: u8) -> Result<DeclKind, DecodeError> {
    Ok(match code {
        1 => DeclKind::Type,
        2 => DeclKind::Function,
        3 => DeclKind::Variable,
        4 => DeclKind::Namespace,
        5 => DeclKind::MacroLike,
        6 => DeclKind::Other,
        _ => return Err(DecodeError::BadDiscriminant),
    })
}

fn macro_kind_code(kind: MacroKind) -> u8 {
    match kind {
        MacroKind::Definition => 1,
        MacroKind::Undefinition => 2,
    }
}

/// Which conditional directive wrote a branch.
///
/// Only the five that can write one, and a value outside them is a failed read rather than a default: a
/// `#define` in this position would mean the encoder and the decoder disagree about what a branch is.
fn conditional_kind_code(kind: crate::DirectiveKind) -> u8 {
    match kind {
        crate::DirectiveKind::If => 1,
        crate::DirectiveKind::Ifdef => 2,
        crate::DirectiveKind::Ifndef => 3,
        crate::DirectiveKind::Elif => 4,
        crate::DirectiveKind::Else => 5,
        // No other kind can reach here — a branch is only ever built from a directive that opens, continues or
        // closes a conditional — and writing a code for one would make the decoder accept a structure the walk
        // cannot produce.
        _ => 0,
    }
}

fn conditional_kind_from(code: u8) -> Result<crate::DirectiveKind, DecodeError> {
    Ok(match code {
        1 => crate::DirectiveKind::If,
        2 => crate::DirectiveKind::Ifdef,
        3 => crate::DirectiveKind::Ifndef,
        4 => crate::DirectiveKind::Elif,
        5 => crate::DirectiveKind::Else,
        _ => return Err(DecodeError::BadDiscriminant),
    })
}

fn macro_kind_from(code: u8) -> Result<MacroKind, DecodeError> {
    Ok(match code {
        1 => MacroKind::Definition,
        2 => MacroKind::Undefinition,
        _ => return Err(DecodeError::BadDiscriminant),
    })
}

fn guard_code(guard: FactGuard) -> u32 {
    match guard {
        FactGuard::Unconditional => u32::MAX,
        FactGuard::Region(index) => index,
    }
}

fn guard_from(code: u32) -> Result<FactGuard, DecodeError> {
    Ok(match code {
        u32::MAX => FactGuard::Unconditional,
        index => FactGuard::Region(index),
    })
}

fn macro_body_code(body: MacroBody) -> u8 {
    match body {
        MacroBody::Specifier => 1,
        MacroBody::Statement => 2,
        MacroBody::Block => 3,
        MacroBody::Expression => 4,
        MacroBody::Type => 5,
        MacroBody::Unknown => 6,
    }
}

fn macro_body_from(code: u8) -> Result<MacroBody, DecodeError> {
    Ok(match code {
        1 => MacroBody::Specifier,
        2 => MacroBody::Statement,
        3 => MacroBody::Block,
        4 => MacroBody::Expression,
        5 => MacroBody::Type,
        6 => MacroBody::Unknown,
        _ => return Err(DecodeError::BadDiscriminant),
    })
}

fn include_form_code(form: IncludeForm) -> u8 {
    match form {
        IncludeForm::Angle => 1,
        IncludeForm::Quote => 2,
        // `#include HEADER`, where the target is a macro. A form of its own rather than a spelling to be guessed:
        // the directive is well formed and its target is simply not a literal, which is what a consumer needs to
        // know before it reports "cannot find header".
        IncludeForm::Macro => 3,
    }
}

fn include_form_from(code: u8) -> Result<IncludeForm, DecodeError> {
    Ok(match code {
        1 => IncludeForm::Angle,
        2 => IncludeForm::Quote,
        3 => IncludeForm::Macro,
        _ => return Err(DecodeError::BadDiscriminant),
    })
}

/// The parser's symbol vocabulary, for a caller that wants to store what a name was found to be.
///
/// Here rather than in the summary, because nothing in a [`FileSummary`] is a resolved kind — the function exists
/// so that a *future* stored field has one obvious place to encode, and so the mapping is written down once. It is
/// deliberately not used by [`encode`]: a field that is not in the format cannot be forgotten by the decoder.
#[allow(dead_code)]
fn symbol_kind_code(kind: SymbolKind) -> u8 {
    match kind {
        SymbolKind::Type => 1,
        SymbolKind::Template => 2,
        SymbolKind::Macro { .. } => 3,
        SymbolKind::Function => 4,
        SymbolKind::Variable => 5,
        SymbolKind::Namespace => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::{CODEC_VERSION, DecodeError, MAGIC, decl_kind_code, decode, encode};
    use crate::summary::MacroKind;
    use crate::cache::SummaryKey;
    use crate::preprocess::directive::IncludeForm;
    use crate::summary::{
        ConditionalRegion, DeclFact, DeclKind, FactGuard, FileSummary, GuardBranch, IncludeFact,
        MacroFact, MacroScopeReading, SummaryGuards,
    };
    use cpp_parser::{MacroBody, SourceRange};

    fn range(start: usize, length: usize) -> SourceRange {
        SourceRange::new(start, length)
    }

    /// A summary that uses every field, so that a round trip covers the whole format rather than a sample of it.
    fn every_field() -> FileSummary {
        FileSummary {
            path: std::path::PathBuf::from("/project/src/widget.cpp"),
            key: SummaryKey::new(0x1111_2222_3333_4444, 0xaaaa_bbbb_cccc_dddd),
            declarations: vec![
                DeclFact {
                    name: "Widget".to_string(),
                    scope: None,
                    kind: DeclKind::Type,
                    // A class declares no type in `type_of`'s sense, so the `None` branch is covered here.
                    type_of: None,
                    // A class returns nothing either, so `None` — and the second fact below covers the other value.
                    returns: None,
                    // A base list, so that branch of the format is covered too: two bases, one of them qualified.
                    bases: vec!["Base".to_string(), "ns::Other".to_string()],
                    // And a parameter list, which is the other list-shaped field: a class template's parameter
                    // names, in the order the declaration wrote them.
                    parameters: vec!["_Ty".to_string(), "_Alloc".to_string()],
                    parameter_list: Some("(int, int)".to_string()),
                    range: range(10, 20),
                    name_range: range(17, 6),
                    // Both flags' `true` branch here, and the other facts below cover `false` — a round trip that
                    // only ever wrote one value of a `u8` flag would not notice a decoder that dropped it.
                    local: true,
                    clean: false,
                    guard: FactGuard::Unconditional,
                },
                DeclFact {
                    name: "make".to_string(),
                    scope: Some("ns::Widget".to_string()),
                    kind: DeclKind::Function,
                    // A function declares no `type_of` — the name has no type — and its `returns` is what a *call*
                    // has. Both halves of that distinction are in this fixture on purpose.
                    type_of: None,
                    returns: Some("ns::Container<int>".to_string()),
                    bases: Vec::new(),
                    parameters: Vec::new(),
                    // The one field a completion's detail line reads: a function with no parameters is written
                    // `()`, which is an answer, and `None` would be "nobody looked".
                    parameter_list: Some("()".to_string()),
                    range: range(40, 15),
                    name_range: range(48, 6),
                    local: false,
                    clean: true,
                    guard: FactGuard::Region(3),
                },
                DeclFact {
                    name: String::new(),
                    scope: None,
                    kind: DeclKind::Other,
                    type_of: None,
                    returns: None,
                    bases: Vec::new(),
                    parameters: Vec::new(),
                    parameter_list: None,
                    range: range(60, 8),
                    name_range: range(60, 0),
                    local: false,
                    clean: true,
                    guard: FactGuard::Unconditional,
                },
            ],
            macros: vec![MacroFact {
                name: "MY_API".to_string(),
                kind: MacroKind::Definition,
                function_like: false,
                body: MacroBody::Specifier,
                // A value rather than the common `None`, for the same reason `settles_the_name` is `true` here: a
                // field the encoder dropped and the decoder defaulted would round-trip a *default* and look fine.
                value: Some("1".into()),
                range: range(80, 30),
                // Present rather than `None`, for the same reason: a field the encoder dropped and the
                // decoder defaulted would round-trip a default and look fine.
                body_range: Some(range(95, 12)),
                guard: FactGuard::Region(0),
                settles_the_name: true,
            }],
            includes: vec![
                IncludeFact {
                    form: IncludeForm::Angle,
                    spelling: "vector".to_string(),
                    resolved: Some(std::path::PathBuf::from("/usr/include/c++/13/vector")),
                    is_next: false,
                    range: range(1, 18),
                    guard: FactGuard::Unconditional,
                },
                IncludeFact {
                    form: IncludeForm::Quote,
                    spelling: "missing.h".to_string(),
                    resolved: None,
                    // The one field no other part of the crate reads, and `every_field` exists so that the codec is
                    // tested on values a test wrote down rather than on values a round trip happened to produce: a
                    // field the encoder drops and the decoder defaults would round-trip a *default* and look fine.
                    is_next: true,
                    range: range(120, 20),
                    guard: FactGuard::Region(1),
                },
            ],
            guards: SummaryGuards {
                regions: vec![range(200, 12), range(240, 20), range(260, 9), range(300, 11)],
                conditionals: vec![
                    ConditionalRegion {
                        branches: vec![
                            GuardBranch {
                                kind: crate::DirectiveKind::Ifdef,
                                condition: Some("_WIN32".into()),
                                body: range(216, 20),
                                range: range(200, 12),
                            },
                            GuardBranch {
                                kind: crate::DirectiveKind::Else,
                                condition: None,
                                body: range(252, 0),
                                range: range(240, 20),
                            },
                        ],
                        parent: None,
                    },
                    ConditionalRegion {
                        branches: vec![GuardBranch {
                            kind: crate::DirectiveKind::If,
                            condition: Some("__cplusplus >= 201703L".into()),
                            body: range(275, 20),
                            range: range(260, 9),
                        }],
                        parent: Some(0),
                    },
                ],
                // A file whose first conditional is its guard, so that the field is not the default here either.
                own_guard: Some(0),
            },
            // Both halves of a reading: one that opens a scope (so `opens` is not the default) and one that closes
            // it, because the option is what tells a closer apart from a namespace with no name.
            macro_readings: vec![
                MacroScopeReading {
                    range: range(400, 10),
                    name: "_STD_BEGIN".to_string(),
                    body: "namespace std {".to_string(),
                    opens: Some(vec!["std".to_string()]),
                },
                MacroScopeReading {
                    range: range(500, 8),
                    name: "_STD_END".to_string(),
                    body: "}".to_string(),
                    opens: None,
                },
            ],
        }
    }

    #[test]
    fn a_summary_with_every_field_round_trips() {
        let original = every_field();
        let bytes = encode(&original);
        let decoded = decode(&bytes).expect("the bytes this module wrote must be readable by it");

        assert_eq!(
            decoded, original,
            "every field survives, including the ones no test above uses"
        );
    }

    #[test]
    fn an_empty_summary_round_trips() {
        let original = FileSummary::empty("/p/empty.h", SummaryKey::new(0, 0));
        let decoded = decode(&encode(&original)).expect("an empty summary is still a summary");

        assert_eq!(decoded, original);
        assert!(decoded.is_empty());
    }

    #[test]
    fn bytes_that_are_not_a_summary_are_rejected() {
        assert_eq!(decode(b"").unwrap_err(), DecodeError::Truncated);
        assert_eq!(decode(b"not a summary at all").unwrap_err(), DecodeError::NotASummary);
    }

    #[test]
    fn a_truncated_summary_is_rejected_rather_than_half_read() {
        // Every prefix of a valid encoding is either a shorter valid summary (if it happens to end on a record
        // boundary) or an error — never a summary with fields quietly missing. What must not happen is a *panic*
        // or a partially filled value, so the property asserted is that each call returns one or the other.
        let bytes = encode(&every_field());

        for cut in 0..bytes.len() {
            // Only reachable on a true record boundary, which for this input is a prefix that happens to end
            // between two records — and a shorter summary must not claim more facts than were written.
            if let Ok(partial) = decode(&bytes[..cut]) {
                assert!(partial.declarations.len() <= 3);
            }
        }
    }

    #[test]
    fn a_future_format_version_is_rejected() {
        let mut bytes = encode(&every_field());

        // The version sits right after the magic, so it can be overwritten without touching anything else.
        let version_at = MAGIC.len();
        bytes[version_at..version_at + 4].copy_from_slice(&(CODEC_VERSION + 1).to_le_bytes());

        assert_eq!(decode(&bytes).unwrap_err(), DecodeError::UnsupportedVersion);
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = encode(&every_field());
        bytes.push(0);

        assert_eq!(decode(&bytes).unwrap_err(), DecodeError::TrailingBytes);
    }

    #[test]
    fn an_implausible_count_is_rejected_without_allocating() {
        // A count read out of a corrupt file can be four billion. The decoder must notice that the file cannot
        // possibly hold that many records before it reserves room for them.
        let summary = every_field();
        let mut bytes = encode(&summary);

        // Overwrite the declaration count with a number larger than the remaining length.
        let count_at = declaration_count_at(&summary);
        bytes[count_at..count_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());

        assert_eq!(decode(&bytes).unwrap_err(), DecodeError::Implausible);
    }

    #[test]
    fn a_field_outside_its_vocabulary_is_rejected() {
        let summary = every_field();
        let mut bytes = encode(&summary);

        // The first declaration's kind, found by adding up the layout rather than by scanning for a byte that
        // looks like it. The scanning version passed for a while by corrupting a byte inside a hash: it found the
        // first `1` after the magic, which is not a field at all once the header's shape changes.
        let kind_at = first_declaration_kind_at(&summary);
        assert_eq!(bytes[kind_at], decl_kind_code(summary.declarations[0].kind));
        bytes[kind_at] = 99;

        assert_eq!(
            decode(&bytes).unwrap_err(),
            DecodeError::BadDiscriminant,
            "a vocabulary byte outside its vocabulary is a reject, not a default"
        );
    }

    /// The offset of the declaration count in an encoded summary.
    ///
    /// Spelled out from the layout `encode` writes: the magic and version, the key — which is a `u32` and three
    /// `u64`s, the reading fingerprint included — the path, then the count. It assumes the fixture's path is ASCII,
    /// which is what makes the path's byte length its `char` count. Keep it in step with `encode` — a test that
    /// computes an offset has to be told when the layout moves.
    fn declaration_count_at(summary: &FileSummary) -> usize {
        const HEADER: usize = MAGIC.len() + 4 + 4 + 8 + 8 + 8;

        HEADER + 4 + summary.path.to_string_lossy().len()
    }

    /// The offset of the first declaration's kind byte: past its name and its optional scope.
    fn first_declaration_kind_at(summary: &FileSummary) -> usize {
        let first = &summary.declarations[0];
        let mut at = declaration_count_at(summary) + 4 + 4 + first.name.len();

        at += match &first.scope {
            Some(scope) => 1 + 4 + scope.len(),
            None => 1,
        };

        at
    }
}

