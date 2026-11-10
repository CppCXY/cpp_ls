//! **Where a parse spends its time, by phase** — the instrument that answers "the grammar is 88 % of the parse" with
//! *which part of the grammar*.
//!
//! Off unless `CPPLS_PARSER_STATS=1`, and free when off (one `OnceLock` load per instrumented call). On, every
//! instrumented function takes a timestamp on entry and adds its **inclusive** elapsed time on exit — inclusive, not
//! self, because a phase's children are the interesting part of it, and a tree of inclusive numbers is what says so:
//! `declaration 790 ms`, of which `specifiers 197 ms`, means the declaration machinery around the specifier sequence
//! is 590 ms.
//!
//! # One mutex rather than a thread-local
//!
//! A thread-local would be faster and is what this started as; `tests/no_global_state.rs` rejected it, correctly — the
//! project's rule is that state belongs to an owner, not to a thread. The table is therefore **process-wide** behind a
//! mutex, with the consequence stated rather than discovered: the numbers are only meaningful while one parse is being
//! timed (two parsers on two threads interleave, and the totals are nonsense). Every instrumented call pays one
//! uncontended lock **only when the probe is on**, and nothing at all otherwise.
//!
//! A guard that borrows the parser cannot work — `let _p = p.during(…); parse_x(p)?;` is a double borrow — so the
//! state cannot be a field of it either. Hence the one static.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

/// The phase table, allocated on first use and cleared by [`report`].
static STATE: Mutex<Option<State>> = Mutex::new(None);

#[derive(Default)]
struct State {
    open: Vec<(&'static str, Instant)>,
    /// Inclusive nanoseconds, and how many times the phase ran.
    nanos: BTreeMap<&'static str, u64>,
    calls: BTreeMap<&'static str, u64>,
}

/// Is the instrument on? Read once; a probe variable set to nothing is off.
fn on() -> bool {
    crate::probes::stats()
}

/// **Time one run of a phase**, from construction to drop.
///
/// ```ignore
/// let _phase = phases::during("expression");
/// ```
pub struct Phase {
    timed: bool,
}

/// Start timing `name` — see [`Phase`].
pub fn during(name: &'static str) -> Phase {
    if !on() {
        return Phase { timed: false };
    }
    if let Ok(mut state) = STATE.lock() {
        let state = state.get_or_insert_with(State::default);
        state.open.push((name, Instant::now()));
    }
    Phase { timed: true }
}

impl Drop for Phase {
    fn drop(&mut self) {
        if !self.timed {
            return;
        }
        let Ok(mut state) = STATE.lock() else {
            return;
        };
        let Some(state) = state.as_mut() else {
            return;
        };
        let Some((name, started)) = state.open.pop() else {
            return;
        };
        let took = started.elapsed().as_nanos() as u64;
        *state.nanos.entry(name).or_default() += took;
        *state.calls.entry(name).or_default() += 1;
    }
}

/// **Print what was timed, and forget it** — called once at the end of a parse.
pub fn report() {
    if !on() {
        return;
    }
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    let Some(state) = state.as_mut() else {
        return;
    };
    let mut lines: Vec<String> = state
        .nanos
        .iter()
        .map(|(name, nanos)| {
            let calls = state.calls.get(name).copied().unwrap_or(0);
            format!(
                "  {name:<20} {:>9.1} ms  {:>9} call(s)  {:>7.1} µs each",
                *nanos as f64 / 1_000_000.0,
                calls,
                *nanos as f64 / calls.max(1) as f64 / 1000.0,
            )
        })
        .collect();
    lines.sort();
    eprintln!("parser phases (inclusive):\n{}", lines.join("\n"));
    state.nanos.clear();
    state.calls.clear();
}
