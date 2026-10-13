//! **Every diagnostic probe in this crate, and the one place each is read from the environment.**
//!
//! # Why this module exists
//!
//! These are debugging switches: `CPPLS_TRACE_MEMBERS=1` prints why a member query answered what it answered,
//! `CPPLS_TRACE_DIAGNOSTICS=1` prints the stage table. They are worth keeping — two of the findings recorded in
//! `docs/status.md` came out of them — but they were written as
//!
//! ```text
//! if std::env::var_os("CPPLS_TRACE_COOKING").is_some() { … }
//! ```
//!
//! at the point of use, and that has two costs, both measured on an index of `EmmyLuaCodeStyle`:
//!
//! * **The read is not free.** `std::env::var_os` takes a lock of the process environment on Windows. On a path
//!   taken nine million times, hoisting one of these reads behind a `LazyLock` took a single function from
//!   **271 s to 192 s**, and removing the `to_string_lossy` beside it took it to **165 s**. The same shape in
//!   `Condition::holds` was another **40 s**.
//! * **It cannot be found.** The gates were in eight files and no listing showed them together, so the same mistake
//!   was made in four places before anyone grepped for it.
//!
//! So every probe is a `fn() -> bool` over a `OnceLock`, and the answer is an atomic load. `on` is not a
//! pessimisation of `on`: a variable cannot appear in the middle of a run, and every caller read it to decide the
//! same thing for the whole process anyway.
//!
//! # Which of these are hot
//!
//! `cooking`, `macro`, `guard`, `lookup`, `coordinates` and `members` are consulted per macro, per condition, or per
//! symbol — millions of times per index. The rest are per file or per stage. **Do not add a probe whose gate is not
//! one of these functions**, which is the whole point of the module: the read happens here, once, or not at all.
//!
//! # Turning them on
//!
//! Each is its own variable, and an unset or empty one means off. They print to the process's own diagnostics — most
//! use `eprintln!`, and `diagnostics` is read by `crate::stages` to print the stage table at the end of a run.

/// Read one switch, once, for the life of the process.
///
/// `On`/`Off` rather than a `bool` cache so that a caller can hold the answer: `probes::ON.are_we()`. The `OnceLock`
/// is what makes the second and later calls an atomic load rather than a lock of the environment.
/// **The rule every switch applies: unset or empty means off, anything else means on.** One function rather than an
/// `is_some_and` written into the macro, so the rule has a test — the six hot callers all depend on it, and a
/// variable set to nothing being read as "on" would print a trace nobody asked for, millions of times.
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
    cooking => "CPPLS_TRACE_COOKING", "What the cooker did to one unit: the directives it read and the facts it made.";
    macro_ => "CPPLS_TRACE_MACRO", "One macro name, at every lookup, with everything the expander will see.";
    guard => "CPPLS_TRACE_GUARD", "One condition, at every evaluation, on the walk's own path.";
    lookup => "CPPLS_TRACE_LOOKUP", "One name, at every lookup, on the walk's environment.";
    coordinates => "CPPLS_TRACE_COORDINATES", "Why a position resolved to the node it did.";
    members => "CPPLS_TRACE_MEMBERS", "Why a member query answered what it answered.";
    completion => "CPPLS_TRACE_COMPLETION", "What one completion request considered, region by region.";
    attributes => "CPPLS_TRACE_ATTR", "Attribute questions, as the walk asks them.";
    units => "CPPLS_TRACE_UNITS", "Which units the session walked, and how often each root.";
    diagnostics => "CPPLS_TRACE_DIAGNOSTICS", "The stage table, printed when a run ends.";
}

/// Where [`completion`]'s report goes: stderr rather than the default sink, for a client whose stdout is the protocol.
pub fn completion_to_stderr() -> bool {
    static HELD: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HELD.get_or_init(|| is_on(std::env::var_os("CPPLS_TRACE_COMPLETION_STDERR")))
}

/// **A trace that follows one name**, as three of these switches do.
///
/// `CPPLS_TRACE_MACRO=WINAPI_FAMILY` prints that name and no other, because the alternative — printing every macro
/// lookup in an index — is a gigabyte of output. The name is read once for the same reason the switch is: these
/// paths are taken millions of times, and a `String` built per call was measured as part of the cost this module
/// exists to remove.
///
/// `None` means "off", which an unset variable also means; a name is never empty, so an empty value is free to mean
/// the same thing.
macro_rules! named_switch {
    ($($name:ident => $variable:literal, $documentation:literal;)*) => {
        $(
            #[doc = $documentation]
            pub fn $name() -> Option<&'static str> {
                static HELD: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
                HELD.get_or_init(|| {
                    std::env::var_os($variable)
                        .filter(|value| !value.is_empty())
                        .map(|value| value.to_string_lossy().into_owned())
                })
                .as_deref()
            }
        )*
    };
}

named_switch! {
    macro_name => "CPPLS_TRACE_MACRO", "The one macro name to trace, at every lookup the expander makes.";
    guard_name => "CPPLS_TRACE_GUARD", "The one name to trace through every condition evaluation.";
    lookup_name => "CPPLS_TRACE_LOOKUP", "The one name to trace through the walk's macro environment.";
}

#[cfg(test)]
mod tests {
    use super::is_on;

    #[test]
    fn an_unset_variable_is_off_and_so_is_an_empty_one() {
        assert!(!is_on(None));
        assert!(!is_on(Some(std::ffi::OsString::from(""))));
        assert!(is_on(Some(std::ffi::OsString::from("1"))));
        // Anything non-empty counts: these are switches, not settings, and `CPPLS_TRACE_MEMBERS=0` asking for a
        // trace is the same kind of surprise as `0` being false in a shell.
        assert!(is_on(Some(std::ffi::OsString::from("0"))));
    }
}
