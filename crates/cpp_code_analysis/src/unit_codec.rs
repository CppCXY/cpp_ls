//! **A translation unit's rendering, as bytes** — what makes the second read of a unit a decode instead of a cook.
//!
//! `RenderedUnit` is the whole program's cooked text: a compiler's view of the includes in order, with every macro
//! replaced by what it expands to. Producing one costs **899 ms** on a real project (1 991 163 bytes of text, 590 135
//! tokens), and the walk it comes from is a fixed function of the files' contents — which is why its key, computed in
//! `Session::render_the_unit`, is a hash over every `(path, content hash)` the closure entered. A rendering filed
//! under that key can be read back on the next start, and the second unit read pays a decode rather than a cook.
//!
//! # What is stored, and what is not
//!
//! ```text
//! text           the cooked stream
//! files          the paths the walk entered, in order — what a span's `file` indexes
//! file_lengths   how long each file's own text is, parallel to `files`
//! dead_macros    the `#define`s the walk decided a compiler would not read, by file
//! messages       the `#error`/`#warning` lines the compilation would have said
//! spans          one per token: where it is in `text`, which file it stands in, where to act on it
//! ```
//!
//! Every field is a function of the text and the closure, so the whole record is content-addressed by its key. There
//! is no `Origin` chain here, unlike a single file's rendering: a unit span says which file a token stands in and
//! where, which is what a consumer of the unit needs; the invocation chain is a per-file question.
//!
//! # Why a hand-written codec
//!
//! The same reason [`crate::summary_codec`] is one: the format is a file on disk that an older binary may be handed,
//! so it needs a magic number, a version, and a decoder that rejects rather than misreads. 590 135 spans is 590 135 ×
//! 20 bytes here, and a `serde`-derived format would spend more per field and version itself in a way this crate
//! cannot control.

use std::collections::HashMap;
use std::path::PathBuf;

use cpp_parser::SourceRange;

use crate::summary::{RenderedUnit, UnitMessage, UnitSpan};

/// What a unit record starts with — `3 665 265 456` in little endian is `UNIT`, and it is not a summary's magic.
const MAGIC: [u8; 4] = [b'U', b'N', b'I', b'T'];

/// Bumped when the layout changes; an older record is then unreachable rather than misread.
pub const UNIT_CODEC_VERSION: u32 = 1;

#[derive(Debug)]
pub enum DecodeError {
    /// Not a unit record at all.
    NotAUnit,
    /// A version this binary does not write.
    UnsupportedVersion,
    /// The bytes ran out or a count was implausible.
    Truncated,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::NotAUnit => write!(formatter, "not a unit rendering"),
            DecodeError::UnsupportedVersion => write!(formatter, "unit rendering from another version"),
            DecodeError::Truncated => write!(formatter, "unit rendering is truncated"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// **How large a record can be before its counts are treated as corruption.** A unit of two million bytes with six
/// hundred thousand spans is about 14 MB; a count beyond this is a torn write rather than a large program.
const MAX_SPANS: usize = 8_000_000;
const MAX_FILES: usize = 1_000_000;
const MAX_TEXT: usize = 512 * 1024 * 1024;

pub fn encode(unit: &RenderedUnit) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(64 + unit.text.len() + unit.spans.len() * 20);
    out.extend_from_slice(&MAGIC);
    put_u32(&mut out, UNIT_CODEC_VERSION);

    put_bytes(&mut out, unit.text.as_bytes());

    put_u32(&mut out, unit.files.len() as u32);
    for path in &unit.files {
        put_path(&mut out, path);
    }
    put_u32(&mut out, unit.file_lengths.len() as u32);
    for length in &unit.file_lengths {
        put_u32(&mut out, *length as u32);
    }

    put_u32(&mut out, unit.dead_macros.len() as u32);
    for (path, offsets) in &unit.dead_macros {
        put_path(&mut out, path);
        put_u32(&mut out, offsets.len() as u32);
        for offset in offsets {
            put_u32(&mut out, *offset as u32);
        }
    }

    put_u32(&mut out, unit.messages.len() as u32);
    for message in &unit.messages {
        put_path(&mut out, &message.file);
        put_u8(&mut out, u8::from(message.fatal));
        put_u32(&mut out, message.at as u32);
        put_bytes(&mut out, message.message.as_bytes());
    }

    put_u32(&mut out, unit.braces as u32);
    put_u32(&mut out, unit.missing as u32);
    put_u32(&mut out, unit.unbalanced.len() as u32);
    for path in &unit.unbalanced {
        put_path(&mut out, path);
    }

    put_u32(&mut out, unit.spans.len() as u32);
    for span in &unit.spans {
        put_u32(&mut out, span.cooked.start_offset as u32);
        put_u32(&mut out, span.cooked.length as u32);
        put_u32(&mut out, span.file);
        put_u32(&mut out, span.written.start_offset as u32);
        put_u32(&mut out, span.written.length as u32);
    }

    out
}

pub fn decode(bytes: &[u8]) -> Result<RenderedUnit, DecodeError> {
    let mut reader = Reader::new(bytes);
    if reader.take(4)? != MAGIC {
        return Err(DecodeError::NotAUnit);
    }
    if reader.u32()? != UNIT_CODEC_VERSION {
        return Err(DecodeError::UnsupportedVersion);
    }

    let text = String::from_utf8(reader.bytes(MAX_TEXT)?).map_err(|_| DecodeError::Truncated)?;

    let file_count = reader.count(MAX_FILES)?;
    let mut files: Vec<PathBuf> = Vec::with_capacity(file_count);
    for _ in 0..file_count {
        files.push(reader.path()?);
    }
    let length_count = reader.count(MAX_FILES)?;
    let mut file_lengths: Vec<usize> = Vec::with_capacity(length_count);
    for _ in 0..length_count {
        file_lengths.push(reader.u32()? as usize);
    }

    let dead_count = reader.count(MAX_FILES)?;
    let mut dead_macros: HashMap<PathBuf, Vec<usize>> = HashMap::with_capacity(dead_count);
    for _ in 0..dead_count {
        let path = reader.path()?;
        let offset_count = reader.count(MAX_SPANS)?;
        let mut offsets: Vec<usize> = Vec::with_capacity(offset_count);
        for _ in 0..offset_count {
            offsets.push(reader.u32()? as usize);
        }
        dead_macros.insert(path, offsets);
    }

    let message_count = reader.count(MAX_SPANS)?;
    let mut messages: Vec<UnitMessage> = Vec::with_capacity(message_count);
    for _ in 0..message_count {
        let file = reader.path()?;
        let fatal = reader.u8()? != 0;
        let at = reader.u32()? as usize;
        let message = String::from_utf8(reader.bytes(MAX_TEXT)?).map_err(|_| DecodeError::Truncated)?;
        messages.push(UnitMessage {
            file,
            fatal,
            message,
            at,
        });
    }

    let braces = reader.u32()? as i64;
    let missing = reader.u32()? as usize;
    let unbalanced_count = reader.count(MAX_FILES)?;
    let mut unbalanced: Vec<PathBuf> = Vec::with_capacity(unbalanced_count);
    for _ in 0..unbalanced_count {
        unbalanced.push(reader.path()?);
    }

    let span_count = reader.count(MAX_SPANS)?;
    let mut spans: Vec<UnitSpan> = Vec::with_capacity(span_count);
    for _ in 0..span_count {
        let cooked = SourceRange::new(reader.u32()? as usize, reader.u32()? as usize);
        let file = reader.u32()?;
        let written = SourceRange::new(reader.u32()? as usize, reader.u32()? as usize);
        spans.push(UnitSpan {
            cooked,
            file,
            written,
        });
    }

    Ok(RenderedUnit {
        text,
        dead_macros,
        messages,
        spans,
        files,
        file_lengths,
        braces,
        missing,
        unbalanced,
        // Neither is a property of the text: `pending_break` is a decision inside the walk that produced it, and
        // `previous` is the last span of the previous file, kept only while building. A decoded unit is a finished
        // one, so both are the empty answer.
        pending_break: false,
        previous: None,
    })
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader { bytes, at: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], DecodeError> {
        if self.at + length > self.bytes.len() {
            return Err(DecodeError::Truncated);
        }
        let slice = &self.bytes[self.at..self.at + length];
        self.at += length;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A count, rejected when it cannot possibly be true of the bytes that are left: a torn record must be a decode
    /// failure rather than an allocation of half a gigabyte.
    fn count(&mut self, most: usize) -> Result<usize, DecodeError> {
        let count = self.u32()? as usize;
        if count > most {
            return Err(DecodeError::Truncated);
        }
        Ok(count)
    }

    fn bytes(&mut self, most: usize) -> Result<Vec<u8>, DecodeError> {
        let length = self.u32()? as usize;
        if length > most {
            return Err(DecodeError::Truncated);
        }
        Ok(self.take(length)?.to_vec())
    }

    fn path(&mut self) -> Result<PathBuf, DecodeError> {
        let bytes = self.bytes(MAX_TEXT)?;
        Ok(PathBuf::from(String::from_utf8_lossy(&bytes).into_owned()))
    }
}

fn put_u8(out: &mut Vec<u8>, value: u8) {
    out.push(value);
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

fn put_path(out: &mut Vec<u8>, path: &std::path::Path) {
    put_bytes(out, path.to_string_lossy().as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_unit() -> RenderedUnit {
        RenderedUnit {
            text: "int x; namespace std { }".to_string(),
            dead_macros: HashMap::from([(PathBuf::from("/p/a.h"), vec![12, 40])]),
            messages: vec![UnitMessage {
                file: PathBuf::from("/p/a.h"),
                fatal: true,
                message: "no".to_string(),
                at: 7,
            }],
            spans: vec![
                UnitSpan {
                    cooked: SourceRange::new(0, 3),
                    file: 0,
                    written: SourceRange::new(0, 3),
                },
                UnitSpan {
                    cooked: SourceRange::new(4, 1),
                    file: 1,
                    written: SourceRange::new(16, 1),
                },
            ],
            files: vec![PathBuf::from("/p/a.h"), PathBuf::from("/p/b.h")],
            file_lengths: vec![10, 20],
            braces: 2,
            missing: 1,
            unbalanced: vec![PathBuf::from("/p/b.h")],
            pending_break: false,
            previous: None,
        }
    }

    #[test]
    fn a_unit_survives_a_round_trip() {
        let unit = a_unit();
        let decoded = decode(&encode(&unit)).expect("decodes");
        assert_eq!(decoded.text, unit.text);
        assert_eq!(decoded.files, unit.files);
        assert_eq!(decoded.file_lengths, unit.file_lengths);
        assert_eq!(decoded.dead_macros, unit.dead_macros);
        assert_eq!(decoded.messages.len(), 1);
        assert_eq!(decoded.messages[0].file, PathBuf::from("/p/a.h"));
        assert_eq!(decoded.spans.len(), 2);
        assert_eq!(decoded.spans[1].file, 1);
        assert_eq!(decoded.spans[1].written.start_offset, 16);
    }

    /// **Bytes that are not a unit are refused**, and a truncated record is a decode failure rather than a panic —
    /// the property that makes a cache safe to keep between runs.
    #[test]
    fn rubbish_and_torn_records_are_refused() {
        assert!(matches!(decode(b"not a unit"), Err(DecodeError::NotAUnit)));

        let mut bytes = encode(&a_unit());
        bytes.truncate(bytes.len() / 2);
        assert!(matches!(decode(&bytes), Err(DecodeError::Truncated)));
    }
}

// ---- a unit's index ---------------------------------------------------------------------------------

/// A second magic, because an `IndexedUnit` is not a rendering: the two live under different keys in the same
/// database and a decoder that confused them would answer with the other one.
const INDEX_MAGIC: [u8; 4] = [b'U', b'I', b'D', b'X'];

/// **A unit's facts, as bytes** — the sweep's output for every file the parse covered.
///
/// `read_the_unit` costs a render (now cached), a parse of the whole program (399 ms) and `index_unit_rendering`
/// (≈3.8 s on a real project) — and the last two are a pure function of the rendered text, which is what `key` is a
/// hash of. So the facts are stored under that hash, and the second read of a unit is a decode.
///
/// Each file's facts go through [`crate::summary_codec`] — the same encoder a summary uses, with the fields a
/// `CookedFile` does not carry (includes, guards, module readings) left empty rather than invented: those are the
/// per-file pass's output, and a unit read does not produce them.
pub fn encode_indexed_unit(unit: &crate::IndexedUnit) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(64 + unit.files.len() * 512);
    out.extend_from_slice(&INDEX_MAGIC);
    put_u32(&mut out, UNIT_CODEC_VERSION);

    put_u32(&mut out, unit.files.len() as u32);
    for (path, cooked) in &unit.files {
        put_path(&mut out, path);
        put_bytes(&mut out, &crate::summary_codec::encode(&as_a_summary(path, cooked)));
        put_u32(&mut out, cooked.unplaced as u32);
        put_u32(&mut out, cooked.diagnostics.len() as u32);
        for diagnostic in &cooked.diagnostics {
            put_u32(&mut out, diagnostic.range.start_offset as u32);
            put_u32(&mut out, diagnostic.range.length as u32);
            put_bytes(&mut out, diagnostic.message.as_bytes());
        }
    }

    put_u32(&mut out, unit.unplaced as u32);
    put_u32(&mut out, unit.tokens as u32);
    put_u32(&mut out, unit.files_with_tokens as u32);
    put_u32(&mut out, unit.missing as u32);
    put_u32(&mut out, unit.unbalanced.len() as u32);
    for path in &unit.unbalanced {
        put_path(&mut out, path);
    }
    put_u32(&mut out, unit.braces as u32);
    put_u32(&mut out, unit.errors as u32);
    out
}

pub fn decode_indexed_unit(bytes: &[u8]) -> Result<crate::IndexedUnit, DecodeError> {
    let mut reader = Reader::new(bytes);
    if reader.take(4)? != INDEX_MAGIC {
        return Err(DecodeError::NotAUnit);
    }
    if reader.u32()? != UNIT_CODEC_VERSION {
        return Err(DecodeError::UnsupportedVersion);
    }

    let file_count = reader.count(MAX_FILES)?;
    let mut files: Vec<(PathBuf, crate::CookedFile)> = Vec::with_capacity(file_count);
    for _ in 0..file_count {
        let path = reader.path()?;
        let summary = crate::summary_codec::decode(&reader.bytes(MAX_TEXT)?)
            .map_err(|_| DecodeError::Truncated)?;
        let unplaced = reader.u32()? as usize;
        let diagnostic_count = reader.count(MAX_SPANS)?;
        let mut diagnostics: Vec<crate::CookedDiagnostic> = Vec::with_capacity(diagnostic_count);
        for _ in 0..diagnostic_count {
            let range = SourceRange::new(reader.u32()? as usize, reader.u32()? as usize);
            let message =
                String::from_utf8(reader.bytes(MAX_TEXT)?).map_err(|_| DecodeError::Truncated)?;
            diagnostics.push(crate::CookedDiagnostic { range, message });
        }
        files.push((
            path,
            crate::CookedFile {
                declarations: summary.declarations,
                macros: summary.macros,
                diagnostics,
                unplaced,
            },
        ));
    }

    let unplaced = reader.u32()? as usize;
    let tokens = reader.u32()? as usize;
    let files_with_tokens = reader.u32()? as usize;
    let missing = reader.u32()? as usize;
    let unbalanced_count = reader.count(MAX_FILES)?;
    let mut unbalanced: Vec<PathBuf> = Vec::with_capacity(unbalanced_count);
    for _ in 0..unbalanced_count {
        unbalanced.push(reader.path()?);
    }
    let braces = reader.u32()? as i64;
    let errors = reader.u32()? as usize;

    Ok(crate::IndexedUnit {
        files,
        unplaced,
        tokens,
        files_with_tokens,
        missing,
        unbalanced,
        braces,
        errors,
    })
}

/// The fields a `FileSummary` needs and a `CookedFile` does not have, filled with the empty answer — see
/// [`encode_indexed_unit`].
fn as_a_summary(path: &PathBuf, cooked: &crate::CookedFile) -> crate::FileSummary {
    crate::FileSummary {
        path: path.clone(),
        key: crate::SummaryKey::new(0, 0),
        declarations: cooked.declarations.clone(),
        macros: cooked.macros.clone(),
        includes: Vec::new(),
        guards: crate::SummaryGuards::default(),
        macro_readings: Vec::new(),
        use_filter: Vec::new(),
        modules: crate::ModuleReading::default(),
    }
}

#[cfg(test)]
mod index_tests {
    use super::*;

    #[test]
    fn an_indexed_unit_survives_a_round_trip() {
        let unit = crate::IndexedUnit {
            files: vec![(
                PathBuf::from("/p/a.h"),
                crate::CookedFile {
                    declarations: Vec::new(),
                    macros: Vec::new(),
                    diagnostics: vec![crate::CookedDiagnostic {
                        range: SourceRange::new(3, 4),
                        message: "missing ;".to_string(),
                    }],
                    unplaced: 2,
                },
            )],
            unplaced: 1,
            tokens: 42,
            files_with_tokens: 1,
            missing: 0,
            unbalanced: vec![PathBuf::from("/p/a.h")],
            braces: 3,
            errors: 1,
        };
        let decoded = decode_indexed_unit(&encode_indexed_unit(&unit)).expect("decodes");
        assert_eq!(decoded.files.len(), 1);
        assert_eq!(decoded.files[0].0, PathBuf::from("/p/a.h"));
        assert_eq!(decoded.files[0].1.diagnostics.len(), 1);
        assert_eq!(decoded.files[0].1.diagnostics[0].message, "missing ;");
        assert_eq!(decoded.files[0].1.unplaced, 2);
        assert_eq!(decoded.tokens, 42);
        assert_eq!(decoded.braces, 3);
        assert_eq!(decoded.unbalanced.len(), 1);
    }
}
