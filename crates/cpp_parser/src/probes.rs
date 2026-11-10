//! **Every debugging switch this crate reads from the environment, read once each.**
//!
//! # Why this module exists
//!
//! Because a `std::env::var_os` call is not free, and in this crate one of them landed on the hottest path there is:
//! [`CppParser::bump`](crate::CppParser), which runs once per token.
//!
//! ```text
//! before   `assert!(std::env::var_os("CPPLS_TRIVIA").is_none() || !is_trivia_kind(..))` in `bump`
//!          a 4 MB unit, 1 091 827 tokens: 2.090 s — 1.93 MB/s
//! after    the same assertion over a `OnceLock<bool>`: 0.663 s — 6.07 MB/s
//!          the real-header corpus (44 files, 4.8 MB): 2.163 s → 0.591 s — 2.21 → 8.10 MB/s
//!          `int f(int a);` 16 000 times, which has no trivia at all: 417 ms → 118 ms
//! ```
//!
//! Measure it yourself: `cargo test --release -p cpp_parser --test parse_cost -- --ignored --nocapture` prints the
//! large-unit line (4 MB, 1 091 827 tokens), and `CPPLS_PROJECT_ROOT` picks the corpus.
//!
//! On Windows a `var_os` takes the process environment lock and builds an `OsString`; on every platform it is a
//! lookup in a map with a `String` key. Either way it is a function call that allocates, made once per token, to
//! answer a question whose answer cannot change during a run. The same defect was found and fixed in
//! `cpp_code_analysis::probes` first — a hoist there took one function from **271 s to 192 s** on a path taken nine
//! million times.
//!
//! # The rule
//!
//! A switch is a `fn() -> bool` over a `OnceLock`, so the first call reads the environment and every later one is an
//! atomic load. **Unset or empty means off, anything else means on** — one reading of an empty variable can not be
//! "on", because a shell that sets `CPPLS_TRIVIA=` has asked for nothing.
//!
//! Do not add a `std::env::var_os` at a use site. Add it here.

macro_rules! switch {
    ($($name:ident => $variable:literal, $documentation:literal;)*) => {
        $(
            #[doc = $documentation]
            pub fn $name() -> bool {
                static HELD: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                *HELD.get_or_init(|| is_on(std::env::var_os($variable)))
            }
        )*
    };
}

/// Read one switch: unset or empty is off, anything else is on.
fn is_on(value: Option<std::ffi::OsString>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

switch! {
    trivia => "CPPLS_TRIVIA",
        "Assert that the cursor never stands on trivia when `bump` emits — the live defect of \
         `docs/parser-review.md` §4, made to fail loudly instead of silently dropping bytes.";
    declaration_trace => "CPPLS_TRACE_DECLARATION",
        "Print one line per declaration reading that failed, with the offset and the error — the trace that named \
         the shape a construct was misread as.";
    stats => "CPPLS_PARSER_STATS",
        "Time every phase of a parse and print the table at the end of a run.";
}

#[cfg(test)]
mod tests {
    use super::is_on;

    #[test]
    fn an_unset_variable_is_off_and_so_is_an_empty_one() {
        assert!(!is_on(None));
        assert!(!is_on(Some(std::ffi::OsString::from(""))));
        assert!(is_on(Some(std::ffi::OsString::from("1"))));
    }
}
