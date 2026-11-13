//! **What an idle pump does after a closure was cooked** — the measurement behind the `containers.cpp` report.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test idle_pump_probe -- --ignored --nocapture
//! ```
//!
//! The report: opening `C:\Users\zc\Desktop\test_cpp\src\containers.cpp` in the real server takes private bytes from
//! ~190 MB to **7.5 GB** over twelve rounds of requests — and, measured afterwards, to **2.5 GB in 55 s with no
//! request at all**. The session's own tables stay near 40 MB the whole time, so the bytes are transient: something
//! the pump does every drain allocates and drops, and the allocator keeps the peaks.
//!
//! This probe answers *what*: the same session, in process, with a counting allocator. It opens the file, drains to
//! idle, then runs `idle` drains that ask nothing and reports the allocations and bytes of each, with the pending
//! work and the tables beside them.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(new_size, Ordering::Relaxed);
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn project() -> PathBuf {
    std::env::var_os("CPPLS_IDLE_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\test_cpp"))
}

#[test]
#[ignore = "a probe: reads a real project and drains its pump"]
fn what_an_idle_pump_does_after_the_closure_is_cooked() {
    let root = project();
    let file = root.join("src/containers.cpp");
    let text = std::fs::read_to_string(&file).expect("the reported file is here");
    let mut session = cpp_code_analysis::Session::open(
        root.clone(),
        cpp_code_analysis::SessionFiles::new(
            cpp_code_analysis::OpenDocuments::new(),
            cpp_code_analysis::DiskFiles,
        ),
        cpp_code_analysis::WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, &text);

    // **To idle**, the way the server's `drain` does it: advance until there is nothing left.
    let started = std::time::Instant::now();
    let mut rounds = 0;
    while session.pending_work() > 0 && rounds < 10_000 {
        session.advance(256);
        rounds += 1;
    }
    let (queued, open, rest) = session.queue_sizes();
    eprintln!("[idle] queue {queued} (open {open}, rest {rest}), refinements wanted {}", session.pending_work() - queued);
    eprintln!(
        "[idle] reached idle in {rounds} rounds, {} ms, pending {}",
        started.elapsed().as_millis(),
        session.pending_work()
    );
    let sizes = session.cache_sizes();
    eprintln!(
        "[idle] summaries {} cooked {} renderings {} views {}/{} MB",
        sizes.summaries,
        sizes.cooked_files,
        sizes.renderings,
        sizes.views,
        sizes.view_bytes / (1024 * 1024)
    );

    // **Then ask nothing, `idle` times**, and count what each drain allocates.
    let idle: usize = std::env::var("CPPLS_IDLE_DRAINS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20);
    for round in 0..idle {
        let allocations = ALLOCATIONS.load(Ordering::Relaxed);
        let bytes = BYTES.load(Ordering::Relaxed);
        session.advance(256);
        let allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations;
        let bytes = BYTES.load(Ordering::Relaxed) - bytes;
        let sizes = session.cache_sizes();
        let (queued, _, _) = session.queue_sizes();
        eprintln!(
            "[idle] queue {queued} refinements {}",
            session.pending_work() - queued
        );
        eprintln!(
            "[idle] drain {round}: {allocations} allocations, {} MB requested, pending {}, cooked {}, views {}/{} MB",
            bytes / (1024 * 1024),
            session.pending_work(),
            sizes.cooked_files,
            sizes.views,
            sizes.view_bytes / (1024 * 1024)
        );
    }
}
