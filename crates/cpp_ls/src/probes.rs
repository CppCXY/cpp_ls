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
//! That particular variable is gone with the allocator counter it read (an allocator is not a place for a
//! measurement), but the lesson is why this module stays the only reader in this crate: one `OnceLock` per switch, one
//! environment read for the life of the process, and an atomic load at the use site. **Do not add a
//! `std::env::var_os` at a use site in this crate.** Add it here.

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
    memory_tables => "CPPLS_TRACE_MEMORY", "Print the session's own table costs when the queue drains — `Session::memory_report`, which is what the analysis estimated, not what the allocator handed out.";
    pump => "CPPLS_TRACE_PUMP", "The pump's own account: its rounds, the open file it reads next, whether a program read is owed, and the two queues once a second.";
    query => "CPPLS_TRACE_QUERY", "One line per query: what it waited for (a permit, the gate, the session) and how long its own work took.";
    hover => "CPPLS_TRACE_HOVER", "Where one hover request's milliseconds went, and what the card was built from.";
    debug_card => "CPPLS_DEBUG_CARD", "Write the hover card being built to `%TEMP%`, for a card that renders wrong.";
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
