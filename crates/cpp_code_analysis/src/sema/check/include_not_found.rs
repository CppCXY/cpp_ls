//! **`include-not-found`**: an `#include` whose file is not there.
//!
//! ```cpp
//! #include <vectr>        // no such header
//! #include "helpr.h"      // no such file next to this one
//! ```
//!
//! The first thing that goes wrong in a translation unit, and the one a user can act on without knowing anything
//! about C++: the file is not where the directive says it is.
//!
//! # Why this is a check and not part of the reading
//!
//! The reading already knows. [`IncludeFact::resolved`](crate::IncludeFact::resolved) is `None` for a header the
//! resolver did not find, and its documentation says so in as many words — *"a header that did not resolve keeps
//! its spelling, because 'this project includes something we cannot find' is a fact a consumer wants to see rather
//! than a gap to hide"*. What was missing is a channel that shows it. **An index is not a complaint.**
//!
//! # The one form that is not reported, and why it is the whole of the care here
//!
//! [`IncludeForm::Macro`] — `#include BOOST_VERSION_HEADER` — is **not** checked, and that is not a gap. The
//! directive names a macro, so its target is not known until the macro is expanded, and a resolver that has not
//! expanded it has learned nothing. Reporting it would be reporting the *shape* of the directive ("this is not a
//! literal path") as if it were a fact about the file system. `Unknown` is the honest answer there, and this layer
//! reports nothing on `Unknown`.
//!
//! The other two forms are literals. The resolver looked for `vectr` on the system path and for `helpr.h` next to
//! the file, and did not find it. That is `Known::No`.
//!
//! # And the branches nobody compiles
//!
//! **An `#include` in a branch this configuration does not take is not a missing file.** neovim writes
//!
//! ```c
//! #if defined(__linux__)
//! # include <execinfo.h>
//! #endif
//! ```
//!
//! and a Windows build never opens that header, so a resolver that looked and failed has answered a question about
//! a line that is not in the program. The file's summary stores its guards as **questions, never answers** (a
//! compilation decides them, and a summary is keyed without one), so the answer is asked where it lives —
//! [`ProjectIndex::the_guard_is_taken`] — and only a **decided** `Inactive` silences the finding: a guard this
//! layer cannot decide keeps it, because "I could not tell" must not become "nothing to see". Measured on the
//! neovim census: the four remaining include misses are glibc and sanitizer headers, and the platform-only ones
//! behind `#if defined(__linux__)` are silent.
//!
//! # Severity
//!
//! A **warning**, and the reason is whose claim it is: the search is this analysis's, built from the compile
//! database, the toolchain discovery and the project's own include graph. A build can pass `-I` paths through a
//! generator, or have an SDK where discovery cannot see it — so "I did not find it" is a warning about our search
//! rather than an error about the program. See [`DiagnosticCode::severity`].

use crate::IncludeForm;

use super::{Check, CheckContext, DiagnosticCode};

/// **`include-not-found`** — see the module documentation.
pub struct IncludeNotFound;

impl Check for IncludeNotFound {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::IncludeNotFound];

    /// Every `#include` in the file whose target was a literal and was not found.
    ///
    /// Answered from the file's **own summary**, so the answer cannot be wrong about another file: the resolver
    /// already did the looking, and what it recorded here is the outcome. Nothing is re-resolved, which is what
    /// keeps this check free at a keystroke.
    fn check(context: &mut CheckContext<'_>) {
        for include in &context.summary.includes {
            if include.resolved.is_some() || include.form == IncludeForm::Macro {
                continue;
            }
            // **A directive in a branch nobody compiles names no file** — see the module documentation. Asked of
            // the index, because that is where the guard's question is answered against the macros in force;
            // `Unknown` answers true and keeps the finding.
            if !context.model.index().the_guard_is_taken(
                context.path,
                include.guard,
                include.range.start_offset,
            ) {
                continue;
            }
            let message = match include.form {
                IncludeForm::Angle => {
                    format!("`<{}>` was not found on the include path", include.spelling)
                }
                // A quoted include is looked for beside the file first and then on the include path, so the
                // message says both rather than naming the one the resolver happened to try last.
                IncludeForm::Quote => format!(
                    "`\"{}\"` was not found beside this file or on the include path",
                    include.spelling
                ),
                // Refused above; written out rather than unreachable so that adding a fourth form is a compile
                // error here instead of a silently wrong message.
                IncludeForm::Macro => {
                    unreachable!("a macro target is not reported — see the module note")
                }
            };
            // **Already in the file''s coordinates**: this range came from the summary - see `CheckContext::written`.
            context.add_in_the_file(
                DiagnosticCode::IncludeNotFound,
                include.range,
                include.spelling.clone(),
                message,
            );
        }
    }
}
