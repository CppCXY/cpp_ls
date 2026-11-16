//! **The two hashes every cache key is built from** — text to a number, and bytes to a number.
//!
//! # Why this is here rather than beside the cache
//!
//! Because the *toolchain* is the first thing that needs it: the toolchain cache is filed under a hash of the
//! compiler's own output, and the include configuration is compared by hashing the command line. A cache key is a
//! property of the inputs (a text, a set of flags), and the inputs belong to this crate.
//!
//! # Why FNV-1a and not something stronger
//!
//! The number never leaves the process: it is a key in a `HashMap` and in a per-session ledger, not a fingerprint a
//! client is asked to trust. What it has to be is **cheap** (a byte-at-a-time multiply, no allocation, no SIMD
//! dispatch) and **stable across runs**, and both hashes below are exactly that. A collision costs a cache entry
//! that is validated by the text it claims to be about — every caller compares the text as well, or keys on a
//! `(hash, length)` pair — so this is not the place a stronger hash buys anything.

/// **FNV-1a over bytes** — the hash a cache key is built from.
///
/// The constants are the algorithm's own, written out rather than taken from a dependency: this is twenty lines,
/// and a hash that changes when a crate updates is a cache that silently misses.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// **A file's text as one number** — the content half of a key.
///
/// Byte order is UTF-8's, which is what a file is read as, so two texts that are equal as bytes hash alike and two
/// that differ only in line endings do not. That is deliberate: `\r\n` and `\n` are different files to a cache — a
/// rewrite that changes every line changes every offset — and normalising them here would make two different
/// readings share an entry.
pub fn content_hash(text: &str) -> u64 {
    fnv1a64(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The published vectors**, so a re-implementation cannot quietly become a different hash.
    #[test]
    fn fnv1a64_matches_the_algorithms_own_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    /// A one-byte difference is a different key, and the same bytes hash alike however they were built.
    #[test]
    fn a_text_hashes_by_its_bytes() {
        assert_eq!(content_hash("abc"), fnv1a64(b"abc"));
        assert_ne!(content_hash("abc"), content_hash("abd"));
        assert_ne!(content_hash("a\nb"), content_hash("a\r\nb"));
    }
}
