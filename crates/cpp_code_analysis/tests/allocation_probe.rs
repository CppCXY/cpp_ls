//! **How many allocations a program reading makes, and — with `perf-probe` — which phase makes them.**
//!
//! ```text
//! Remove-Item C:\Users\zc\Desktop\cpp_modules\.cppls\cache.redb
//! $env:CPPLS_MODULES_PROJECT = "C:\Users\zc\Desktop\cpp_modules"
//! cargo test --release -p cpp_code_analysis --test allocation_probe -- --ignored --nocapture
//! cargo test --release -p cpp_code_analysis --features perf-probe --test allocation_probe -- --ignored --nocapture
//! ```
//!
//! Separate from `module_hover_probe` on purpose: the counter wraps the allocator, and two atomics per allocation cost
//! 1.7 s on a 4.7 s read — so a probe that counts cannot also be the probe that times. The count is what does not move
//! when the machine is loaded, which makes it the honest instrument for a change that removes allocations.
//!
//! The baseline before the `SmolStr` token, the borrowed name spellings and the tree-scan directive question was
//! **217 535 897 allocations, 11 814 MB**; the same read now makes **149 931 213 allocations, 10 458 MB**.

use std::path::PathBuf;

use mimalloc::MiMalloc;

struct Counting;

// **This thread's allocations for the current measurement.** The generation pair is what makes the count honest for a
// worker thread the index spawned before `perf::reset()`: a stale generation means "count from here", so a thread's
// first mark after a reset carries only its own work.
//
// A comment and not a doc comment: a `thread_local!` is a macro invocation, and rustdoc generates no documentation for
// one (`unused_doc_comments`, which `-D warnings` turns into an error).
#[cfg(feature = "perf-probe")]
thread_local! {
    static HERE: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

// Read by hand while a measurement is being read off, which is why nothing calls it: the probe prints the totals and a
// caller who wants this thread's share asks for it in the run being investigated.
#[cfg(feature = "perf-probe")]
#[allow(dead_code)]
fn allocations_here() -> usize {
    HERE.with(|cell| cell.get().1)
}

fn note_here(bytes: usize) {
    #[cfg(not(feature = "perf-probe"))]
    let _ = bytes;
    #[cfg(feature = "perf-probe")]
    {
        let generation = cpp_code_analysis::perf::generation();
        HERE.with(|cell| {
            let (seen, count) = cell.get();
            cell.set(if seen == generation {
                (seen, count + 1)
            } else {
                (generation, 1)
            });
        });
        cpp_code_analysis::perf::note(bytes);
    }
}

static ALLOCATIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static BYTES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        BYTES.fetch_add(layout.size(), std::sync::atomic::Ordering::Relaxed);
        // **And the library's own counter**, which is what the phase marks read. It exists only with the feature, so
        // a default build has neither the counter nor this call.
        note_here(layout.size());
        // SAFETY: the same layout is passed through to the allocator underneath.
        unsafe { MiMalloc.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        // SAFETY: the pointer and layout are the ones this allocator handed out.
        unsafe { MiMalloc.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        BYTES.fetch_add(new_size, std::sync::atomic::Ordering::Relaxed);
        note_here(new_size);
        // SAFETY: forwarded unchanged to the allocator underneath.
        unsafe { MiMalloc.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        BYTES.fetch_add(layout.size(), std::sync::atomic::Ordering::Relaxed);
        note_here(layout.size());
        // SAFETY: forwarded unchanged to the allocator underneath.
        unsafe { MiMalloc.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> PathBuf {
    PathBuf::from(
        std::env::var("CPPLS_MODULES_PROJECT")
            .unwrap_or_else(|_| r"C:\Users\zc\Desktop\cpp_modules".to_owned()),
    )
}

#[test]
#[ignore = "a probe, not a test: it measures the allocation count of a real project's unit read"]
fn a_program_read_allocates() {
    let root = project();
    let main = root.join("src").join("main.cpp");
    assert!(main.is_file(), "the project's main.cpp: {}", main.display());

    // **`Session::open` runs the toolchain discovery**, which is what gives the project its include paths — a plain
    // `with_config` has none, and then `import std;` resolves to nothing and the read is a no-op.
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.index_everything();
    let text = std::fs::read_to_string(&main).expect("main.cpp reads");
    session.did_open(&main, &text);
    session.index_everything();

    // The measurement starts here: whatever the project scan did is not the read.
    #[cfg(feature = "perf-probe")]
    cpp_code_analysis::perf::reset();
    let allocations = ALLOCATIONS.load(std::sync::atomic::Ordering::Relaxed);
    let bytes = BYTES.load(std::sync::atomic::Ordering::Relaxed);
    let read = session.read_the_modules_a_file_imports(&main);
    let allocations = ALLOCATIONS.load(std::sync::atomic::Ordering::Relaxed) - allocations;
    let bytes = BYTES.load(std::sync::atomic::Ordering::Relaxed) - bytes;
    println!("read {read} unit(s)");
    println!("allocations: {allocations}");
    println!(
        "bytes: {bytes} ({:.1} MB)",
        bytes as f64 / (1024.0 * 1024.0)
    );

    #[cfg(feature = "perf-probe")]
    {
        println!("per phase:");
        for (label, allocations, bytes) in cpp_code_analysis::perf::per_phase() {
            println!(
                "  {label:<20} {allocations:>12}  {:>7.1} MB",
                bytes as f64 / (1024.0 * 1024.0)
            );
        }
    }
}
