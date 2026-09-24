//! Fingerprint of the **readers** that produce a summary, for the cache key.
//!
//! `docs/index-design.md` fixes the rule this script implements: `format_version` **contains the parser's
//! version — the parser moves, the whole store is void**. That rule was written down and then relied on being
//! remembered, and it was not: the grammar has changed in dozens of rounds while the number stayed at 1, so every
//! summary on disk was written by an *older* reader and served as if it were current. A stale summary is not a
//! missing answer, it is a **wrong** one — the exact failure mode the key exists to prevent.
//!
//! So the invalidation is computed instead of remembered: this script hashes the text that decides what a summary
//! contains, and `cache::READING_FINGERPRINT` carries the result into the key. Nothing else changes; the component
//! is a constant of the binary, so a lookup is still possible **before** the file is parsed — which is the property
//! `cache.rs` spent a whole round restoring when the macro environment left the key.
//!
//! # What is hashed, and why each root is on the list
//!
//! * `../cpp_parser/src` — the readings themselves: the grammar, the lexer's token kinds, the tree builder.
//! * `src` — this crate: which facts are built from those readings (`sema`), what they contain (`summary.rs`), and
//!   what the preprocessor layer records (`preprocess`).
//!
//! Hashing whole directories rather than a list of files is deliberate: a new grammar file, a new module in
//! `sema`, must invalidate too, and a list would have to be updated by the same memory this script exists to
//! replace. A documentation-only edit therefore also invalidates the cache, which costs a re-index of the closure
//! (seconds) and buys the guarantee.
//!
//! What is **not** here: `Cargo.toml` files and dependency versions. A bumped dependency can change a reading
//! without a source edit, and that is what the manual `FORMAT_VERSION` is still for.

use std::path::{Path, PathBuf};

/// The directories whose text decides what a summary contains, relative to this crate's manifest.
const READING_ROOTS: &[&str] = &["src", "../cpp_parser/src"];

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));

    let mut files: Vec<PathBuf> = Vec::new();
    for root in READING_ROOTS {
        let directory = manifest.join(root);
        collect(&directory, &mut files);
        println!("cargo:rerun-if-changed={}", directory.display());
    }

    // Sorted, because a directory's read order is not part of what is being fingerprinted: the same sources must
    // produce the same number on every machine and every filesystem.
    files.sort();

    let mut hash = FNV_OFFSET;
    for file in &files {
        // The *path* as well as the content, so that moving a reading from one file to another is a change. Relative
        // to the manifest, so that two checkouts of the same commit agree.
        let name = file
            .strip_prefix(&manifest)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        hash = fnv1a(hash, name.as_bytes());
        hash = fnv1a(
            hash,
            &std::fs::read(file).unwrap_or_else(|why| panic!("reading {}: {why}", file.display())),
        );
    }

    println!("cargo:rustc-env=CPP_READING_FINGERPRINT={hash:016x}");

    // A file that is not compiled still counts (see the module documentation), so a directory with no `.rs` in it
    // is a mistake rather than an empty contribution.
    assert!(
        !files.is_empty(),
        "no sources under {READING_ROOTS:?}: the fingerprint would be a constant and the cache would never \
         invalidate"
    );
}

/// Collect every `.rs` file under `directory`, recursively, in a deterministic order.
fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(directory)
        .unwrap_or_else(|why| panic!("reading {}: {why}", directory.display()));

    let mut children: Vec<PathBuf> = entries
        .map(|entry| entry.expect("a directory entry").path())
        .collect();
    children.sort();

    for child in children {
        if child.is_dir() {
            collect(&child, files);
        } else if child.extension().is_some_and(|extension| extension == "rs") {
            files.push(child);
        }
    }
}

/// FNV-1a's 64-bit offset basis, and the hash itself — the same function `cache.rs` uses, and for the same reason:
/// it is fixed by definition, so the number does not move when the compiler is upgraded.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}
