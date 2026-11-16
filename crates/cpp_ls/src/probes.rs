//! **Every diagnostic switch the server reads from the environment, read once each.**
//!
//! # Why this module exists
//!
//! The engine has one of these already ([`cpp_code_analysis::probes`]) and it was written because a `var_os` at a use
//! site cost **271 s → 192 s** on one hot function. The server then repeated the mistake in the one place it is worst:
//! ```text
//! if std::env::var_os("CPPLS_TRACE_ALLOCS").is_some() { … }   // inside the pump loop
//! ```
//! `std::env::var_os` takes the process environment lock on Windows and builds an `OsString`, and the pump loop runs
//! thousands of times a second — so *every* round paid for a lock and an allocation to answer a question whose answer
//! cannot change after start-up. It also made the loop's own trace slower than the stages it was measuring.
//!
//! So: one `OnceLock` per switch, one environment read for the life of the process, and an atomic load at the use
//! site. **Do not add a `std::env::var_os` at a use site in this crate.** Add it here.
//!
//! [`allocations`] is not a switch of its own: the allocator's counter already decides that question, once, at
//! start-up ([`cpp_code_analysis::alloc::turn_on_from_the_environment`]), so asking it is that state and nothing has to
//! be read at all.

/// Read one switch, once, for the life of the process.
///
/// Unset or empty means off, anything else means on — the same rule as the engine's probes, and one function for the
/// same reason: it makes the rule testable.
fn is_on(value: Option<std::ffi::OsString>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

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

switch! {
    no_program_read => "CPPLS_NO_PROGRAM_READ", "Do not read whole programs — an experiment switch for telling a program read's cost from the pump's.";
    no_cooks => "CPPLS_NO_COOKS", "Do not cook anything, so a run measures the per-file index alone.";
    no_watch => "CPPLS_NO_WATCH", "Do not register the file watchers.";
    memory_tables => "CPPLS_TRACE_MEMORY", "Print the session's table costs beside the pump's memory line.";
    hover => "CPPLS_TRACE_HOVER", "Where one hover request's milliseconds went, and what the card was built from.";
    debug_card => "CPPLS_DEBUG_CARD", "Write the hover card being built to `%TEMP%`, for a card that renders wrong.";
}

/// **Whether the allocator is counting** — the switch for every memory trace the pump prints.
///
/// Not a `var_os` here because the answer already exists: the binary installs a counting allocator and the engine turns
/// it on once at start-up. Asking is an atomic load, and the trace it gates cannot disagree with the numbers it prints,
/// which is the defect two separate reads of the same variable invite.
#[inline]
pub fn allocations() -> bool {
    cpp_code_analysis::alloc::counting()
}

#[cfg(test)]
mod tests {
    use super::is_on;

    #[test]
    fn an_unset_variable_is_off_and_so_is_an_empty_one() {
        assert!(!is_on(None));
        assert!(!is_on(Some(std::ffi::OsString::from(""))));
        assert!(is_on(Some(std::ffi::OsString::from("1"))));
        // Anything non-empty counts, the same rule as the engine's switches: `CPPLS_NO_WATCH=0` asking for no watchers
        // is the same kind of surprise as `0` being false in a shell.
        assert!(is_on(Some(std::ffi::OsString::from("0"))));
    }
}
