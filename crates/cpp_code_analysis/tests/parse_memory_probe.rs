//! **What a parse costs — held bytes, churn and allocation count, by stage** — the measurement that decides whether
//! the tree, the tokens, the facts or the *allocation pattern* are what a unit read is paying for.
//!
//! # Why this exists
//!
//! The number every memory budget in this codebase was justified by is **~380 bytes per token** (`test_cpp`,
//! 737 525 tokens, ~280 MB). It was read off a counter that only ever *added* — no free was ever subtracted — so it
//! measured churn and was used as if it measured memory. This probe reports both, separately, for one file:
//!
//! ```text
//! text       the source, as read
//! lex        the token stream the parser is given
//! parse      the lossless green tree
//! preprocessing, scopes, facts
//! ```
//!
//! and for each: **held** (live bytes, the number a machine runs out of), **churn** (every byte asked for, the number
//! that decides how big the allocator's high-water gets) and **allocs** (how many requests it took).
//!
//! # What the three numbers say together
//!
//! A stage whose churn is much larger than what it holds is a stage that allocates and frees repeatedly: the peak is
//! fine and the *resident set* is not, because a general-purpose allocator keeps the pages it was given. `held /
//! allocs` is the average request size, and when that is tens of bytes the pattern is one allocation per node or per
//! token, which is the shape to look at first.
//!
//! # Running it
//!
//! ```text
//! $env:CPPLS_PROBE_FILE = "C:\...\include\c++\15.1.0\bits\stl_algo.h"
//! cargo test --release -p cpp_code_analysis --test parse_memory_probe -- --ignored --nocapture
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Bytes currently held — every allocation adds, every free subtracts.
static LIVE: AtomicUsize = AtomicUsize::new(0);
/// The high-water mark of [`LIVE`].
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// Every byte ever asked for, cumulative.
static TOTAL: AtomicUsize = AtomicUsize::new(0);
/// How many requests the allocator was given, cumulative.
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

impl Counting {
    #[inline]
    fn add(bytes: usize) {
        TOTAL.fetch_add(bytes, Ordering::Relaxed);
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        let now = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
        PEAK.fetch_max(now, Ordering::Relaxed);
    }

    #[inline]
    fn sub(bytes: usize) {
        LIVE.fetch_sub(bytes, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Counting::add(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        Counting::sub(layout.size());
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Counting::add(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size >= layout.size() {
            Counting::add(new_size - layout.size());
        } else {
            Counting::sub(layout.size() - new_size);
        }
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

fn total() -> usize {
    TOTAL.load(Ordering::Relaxed)
}

fn allocs() -> usize {
    ALLOCS.load(Ordering::Relaxed)
}

fn mb(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// One stage, measured as the difference between two points in all four numbers.
struct Stage {
    name: &'static str,
    live_before: usize,
    total_before: usize,
    allocs_before: usize,
    peak_before: usize,
    tokens: usize,
}
impl Stage {
    fn begin(name: &'static str, tokens: usize) -> Self {
        Stage {
            name,
            live_before: live(),
            total_before: total(),
            allocs_before: allocs(),
            peak_before: peak(),
            tokens,
        }
    }

    fn end(self, also_live: usize) -> usize {
        // A stage's **held** bytes are what is still allocated after it, minus anything the caller was already
        // holding from before it — which is why `also_live` exists: the parse holds the text and the token stream, and
        // counting them again would make the tree look three times its size.
        let held = live().saturating_sub(self.live_before);
        let churn = total() - self.total_before;
        let requests = allocs() - self.allocs_before;
        let average = if requests == 0 {
            0.0
        } else {
            churn as f64 / requests as f64
        };
        println!(
            "{:<10} held {:>11} B ({:>7.2} MB)  churn {:>11} B ({:>8.2} MB)  allocs {:>10}  avg {:>6.1} B  peak {:>7.2} MB{}",
            self.name,
            held,
            mb(held),
            churn,
            mb(churn),
            requests,
            average,
            mb(peak().saturating_sub(self.peak_before) + live()),
            if self.tokens == 0 {
                String::new()
            } else {
                format!(
                    "  | {:>6.1} B/token held, {:>8.1} B/token churn",
                    held as f64 / self.tokens as f64,
                    churn as f64 / self.tokens as f64
                )
            }
        );
        let _ = also_live;
        held
    }
}

#[test]
#[ignore = "a probe: needs CPPLS_PROBE_FILE, and reports rather than asserts"]
fn what_a_parse_costs() {
    let path = std::env::var("CPPLS_PROBE_FILE").expect("CPPLS_PROBE_FILE names the file to read");
    let text = std::fs::read_to_string(&path).expect("the probe file must be readable");
    println!(
        "\nfile       {path}\n           {} B ({:.1} KB)",
        text.len(),
        text.len() as f64 / 1024.0
    );

    // **The repeat, for the question the single pass cannot answer**: does the resident set follow *live* bytes or
    // *churn*? One read of this file holds 5 MB and churns 43 MB; fifty of them hold the same 5 MB and churn two
    // gigabytes, because every allocation is freed. If the process's working set follows the second number, that is
    // what "it uses four gigabytes" has been about all along, and no live-byte budget can bound it.
    let repeat: usize = std::env::var("CPPLS_PROBE_REPEAT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    if repeat > 1 {
        println!(
            "\n# {repeat} readings, one after another — the live bytes stay, the churn adds up"
        );
        for index in 0..repeat {
            let (tokens, _) = cpp_parser::lex(&text, &cpp_parser::LexerConfig::default());
            let tree = cpp_parser::CppParser::parse(&text, cpp_parser::ParserConfig::default());
            let root = tree.get_red_root();
            let preprocessing = cpp_code_analysis::preprocess::preprocess(&text, tree.get_tokens());
            let scopes = cpp_code_analysis::sema::scopes::build_scopes_with(
                &root,
                &cpp_code_analysis::sema::scopes::NoMacroBodies,
                false,
            );
            let errors: Vec<cpp_parser::SourceRange> = tree
                .get_errors()
                .iter()
                .map(|error| cpp_parser::source_range(error.range))
                .collect();
            let (facts, _) = cpp_code_analysis::sema::declarations::build_facts(
                &scopes,
                &preprocessing,
                &root,
                &errors,
                false,
            );
            let held = facts.len();
            drop(facts);
            drop(scopes);
            drop(root);
            drop(tree);
            drop(tokens);
            if index % 10 == 0 || index + 1 == repeat {
                println!(
                    "read {:3}  live {:6.2} MB  peak {:7.2} MB  churn {:8.2} MB  ({} declaration(s))",
                    index + 1,
                    mb(live()),
                    mb(peak()),
                    mb(total()),
                    held
                );
            }
        }
        println!(
            "done: live {:.2} MB, peak {:.2} MB, churn {:.2} MB in {} allocation(s)",
            mb(live()),
            mb(peak()),
            mb(total()),
            allocs()
        );
        return;
    }

    // **The token stream**, which the parser reads and the tree quotes. Lexed once: a first lex to count tokens would
    // make the stage's own numbers the second lex's, and the first one's cost would land in whichever stage ran next.
    let lexing = Stage::begin("lex", 0);
    let (tokens, _) = cpp_parser::lex(&text, &cpp_parser::LexerConfig::default());
    let token_count = tokens.len();
    println!(
        "tokens     {token_count} ({:.2} B of source per token)\n",
        text.len() as f64 / token_count as f64
    );
    let mut lexing = lexing;
    lexing.tokens = token_count;
    lexing.end(0);

    // **The parse.**
    let parsing = Stage::begin("parse", token_count);
    let tree = cpp_parser::CppParser::parse(&text, cpp_parser::ParserConfig::default());
    parsing.end(0);

    // **The reading**: preprocessing, scopes, facts.
    let root = tree.get_red_root();
    let preprocessing_stage = Stage::begin("preprocess", token_count);
    let preprocessing = cpp_code_analysis::preprocess::preprocess(&text, tree.get_tokens());
    preprocessing_stage.end(0);

    let scoping = Stage::begin("scopes", token_count);
    let scopes = cpp_code_analysis::sema::scopes::build_scopes_with(
        &root,
        &cpp_code_analysis::sema::scopes::NoMacroBodies,
        false,
    );
    scoping.end(0);

    let errors: Vec<cpp_parser::SourceRange> = tree
        .get_errors()
        .iter()
        .map(|error| cpp_parser::source_range(error.range))
        .collect();
    let making = Stage::begin("facts", token_count);
    let (facts, _) = cpp_code_analysis::sema::declarations::build_facts(
        &scopes,
        &preprocessing,
        &root,
        &errors,
        false,
    );
    making.end(0);

    println!(
        "\ntotal      held {:.2} MB, peak {:.2} MB, churn {:.2} MB in {} allocation(s) — {} declaration(s)",
        mb(live()),
        mb(peak()),
        mb(total()),
        allocs(),
        facts.len()
    );
    println!(
        "           {:.1} B held/token, {:.1} B churn/token, {:.1} allocation(s)/token",
        live() as f64 / token_count as f64,
        total() as f64 / token_count as f64,
        allocs() as f64 / token_count as f64
    );

    drop(facts);
    drop(scopes);
    drop(root);
    drop(tree);
    println!(
        "after the drops: held {:.2} MB (the allocator keeps the pages; this is the number that stays)",
        mb(live())
    );
    drop(tokens);
}
