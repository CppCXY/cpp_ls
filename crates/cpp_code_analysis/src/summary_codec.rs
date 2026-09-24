//! Reading and writing [`FileSummary`] as bytes, so the index survives a restart.
//!
//! # Why the format is written by hand
//!
//! A summary is a flat list of small records with five field types between them: a `u32`, a `u64`, a length-prefixed
//! string, a `SourceRange`, and a handful of enums. `serde` plus a binary format would generate the same bytes with
//! a build dependency, an attribute on every type, and a version negotiation nobody can read — and the schema is
//! `docs/index-design.md`'s to define, not a macro's. The project has already made this trade once, in
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
    DeclFact, DeclKind, FactGuard, FileSummary, IncludeFact, MacroFact, MacroKind, SummaryGuards,
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
pub const CODEC_VERSION: u32 = 10;

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
        put_range(&mut out, fact.range);
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

    out
}

/// Read a summary back, or say why the bytes are not one.
///
/// Every failure is a [`DecodeError`] and not a partially built summary: the contract in the module
/// documentation is that bytes which do not parse are rejected, and the way to keep that contract is to have no
/// path that returns `Ok` with a field left at its default.
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
            range: reader.range()?,
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
        DeclFact, DeclKind, FactGuard, FileSummary, IncludeFact, MacroFact, SummaryGuards,
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
                range: range(80, 30),
                guard: FactGuard::Region(0),
                // `true` rather than the common `false`, and on purpose: a round trip that only ever carries the
                // default value would pass with the field dropped from the encoding entirely.
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
            },
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
