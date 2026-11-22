//! **Shared helpers for this crate's integration tests.**
//!
//! One helper so far, and it exists because of a measured flake rather than for tidiness: see
//! [`a_temp_directory`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// **A temp directory no other test can be using**, created and empty.
///
/// A test that writes a fixture needs a directory of its own, and the two spellings the suites used were both wrong in
/// the same way:
///
/// ```text
/// cppls-hover-attributes-<pid>   every test in this file shares the process id
/// cppls-hover-template           every *run* shares it too
/// ```
///
/// One test binary runs its tests **in parallel**, and `cargo test` runs several binaries at once, so two tests can be
/// writing `a.cpp` in the same directory — one of them `remove_dir_all`s it while the other writes, and the write fails.
/// That is the failure this helper closes: four different `cpp_ls` suites failed once each under a full-workspace run
/// on 2026-11-22, every one of them at a `fs::write` or a `fs::create_dir_all` in a helper shaped like the above, and
/// every one passed when run alone.
///
/// The name keeps the process id (two concurrent `cargo test` invocations are rare but real) and adds a counter that is
/// unique within this binary.
pub fn a_temp_directory(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    let root = std::env::temp_dir().join(format!(
        "cppls-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a temp directory");
    root
}
