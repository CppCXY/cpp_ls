//! **What one macro lookup costs, at the size a real compiler's table has.**
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example lookup_probe
//! ```
//!
//! # The question
//!
//! `walk-macro-guard` is 68 s of a 186 s index of `EmmyLuaCodeStyle` — one `a_guard_is_in_force` per macro fact,
//! and 37 M conditions behind it. Everything else on that path has been measured and cleared:
//!
//! ```text
//! the expansion and parse        cached                       (worth 35%)
//! the closure building UnitMacros 0.7 s of 271 s             (measured)
//! re-lexing a condition's text    ~0                          (measured)
//! conditions_of's allocation      ~2 s                        (measured)
//! ```
//!
//! What is left is `eval` reaching `UnitMacros::lookup`, which has two branches: a `HashMap` on the walk's own state,
//! and `self.seed.lookup(name)` for everything else. The seed is `Marked`, and `Marked` keeps its bindings in a
//! **`Vec<(Box<str>, bool)>`** — so `is_defined` is a `rev().find()`, one string comparison per entry, on every
//! lookup that misses the walk's own map.
//!
//! A compiler predefines several hundred names, and a condition asks about names that are usually **not** among them
//! (`__cplusplus`, a project's own feature macro) — and for those the scan runs to the end.
//!
//! # What it measures
//!
//! The same lookup, at three table sizes and for both answers, so the result is a curve rather than a point: if the
//! cost tracks the table size, a linear scan is the answer and a hash index is the fix; if it does not, the seed is
//! not where the time goes and this file has said so once and for all.
//!
//! `Marked::define_on_the_command_line` is the public way to fill one, and it is the same call `Toolchain::macros`
//! goes through — so the shape of the table here is the shape of the real one.

use cpp_code_analysis::{Lookup, MacroValues, Marked};

/// The names a compiler of this size predefines, as `cl` and `gcc` spell them. Enough of them to be the right
/// *size*; the exact names do not matter because a lookup compares whole strings either way.
fn predefines(count: usize) -> Vec<String> {
    let mut names = vec![
        "_WIN32".to_string(),
        "_MSC_VER".to_string(),
        "__cplusplus".to_string(),
        "_MSVC_LANG".to_string(),
        "__x86_64__".to_string(),
    ];
    for index in names.len()..count {
        names.push(format!("__CPPLS_PREDEFINED_{index}"));
    }
    names.truncate(count);
    names
}

/// One table of `count` names, filled the way a discovered toolchain fills it.
fn seed(count: usize) -> Marked {
    let mut marked = Marked::default();
    for name in predefines(count) {
        marked.define_on_the_command_line(&name, Some("1"));
    }
    marked
}

/// How long `iterations` lookups of `name` take, in nanoseconds each.
fn per_lookup(marked: &Marked, name: &str, iterations: u32) -> u64 {
    // A first pass to make sure nothing is being measured before the table has been touched at all.
    for _ in 0..1_000 {
        std::hint::black_box(marked.lookup(name));
    }

    let started = std::time::Instant::now();
    for _ in 0..iterations {
        std::hint::black_box(marked.lookup(std::hint::black_box(name)));
    }
    started.elapsed().as_nanos() as u64 / u64::from(iterations)
}

fn main() {
    const ITERATIONS: u32 = 2_000_000;

    println!(
        "lookup cost against table size, {} iterations each, ns per lookup",
        ITERATIONS
    );
    println!();
    println!(
        "{:>8}  {:>18}  {:>18}",
        "bindings", "present (first)", "absent (scans all)"
    );

    for count in [5usize, 50, 200, 500] {
        let marked = seed(count);
        // The name is at the *front*, so `is_defined`'s reverse scan finds it last — which is the worst case for a
        // present name and the honest one: the table is filled in the compiler's order and a condition can ask about
        // any of it.
        let present = predefines(count)[0].clone();
        let absent = "__CPPLS_NEVER_DEFINED__";

        println!(
            "{count:>8}  {:>18}  {:>18}",
            per_lookup(&marked, &present, ITERATIONS),
            per_lookup(&marked, absent, ITERATIONS),
        );
    }

    println!();
    println!(
        "A probe of one name, for context: is the *shape* of the answer what the walk's own state gives?"
    );
    let marked = seed(500);
    for name in [
        "_WIN32",
        "_MSC_VER",
        "__cplusplus",
        "__CPPLS_NEVER_DEFINED__",
    ] {
        let answer = match marked.lookup(name) {
            Lookup::Defined(_) => "Defined",
            Lookup::DefinedWithoutAValue => "DefinedWithoutAValue",
            Lookup::Undefined => "Undefined",
            Lookup::Unanswered => "Unanswered",
        };
        println!("  {name:<28} {answer}");
    }
}
