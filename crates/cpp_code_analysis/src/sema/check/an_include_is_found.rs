//! **An `#include` whose file is not there.**
//!
//! ```cpp
//! #include <vectr>        // no such header
//! #include "helpr.h"      // no such file next to this one
//! ```
//!
//! The first thing that goes wrong in a translation unit, and the one a user can act on without knowing
//! anything about C++: the file is not where the directive says it is.
//!
//! # Why this is a check and not part of the reading
//!
//! The reading already knows. [`IncludeFact::resolved`] is `None` for a header the resolver did not find, and
//! its documentation says so in as many words — *"a header that did not resolve keeps its spelling, because
//! 'this project includes something we cannot find' is a fact a consumer wants to see rather than a gap to
//! hide"*. What was missing is a channel that shows it. An index is not a complaint.
//!
//! # The one form that is not reported, and why it is the whole of the care here
//!
//! [`IncludeForm::Macro`] — `#include BOOST_VERSION_HEADER` — is **not** checked, and that is not a gap.
//! The directive names a macro, so its target is not known until the macro is expanded, and a resolver that
//! has not expanded it has learned nothing. Reporting it would be reporting the *shape* of the directive
//! (`this is not a literal path`) as if it were a fact about the file system. `Unknown` is the honest answer
//! there, and this layer reports nothing on `Unknown` — see the module documentation.
//!
//! The other two forms are literals. The resolver looked for `vectr` on the system path and for `helpr.h`
//! next to the file, and did not find it. That is `Known::No`.

use crate::IncludeForm;

use super::{Checks, Finding};

/// The name this check reports under — see [`Finding::check`].
pub const CHECK: &str = "an_include_is_found";

/// Every `#include` in the file whose target was a literal and was not found.
///
/// Answered from the file's **own summary**, so the answer cannot be wrong about another file: the resolver
/// already did the looking, and what it recorded here is the outcome. Nothing is re-resolved, which is what
/// keeps this check free at a keystroke.
pub fn the_file_it_names_is_not_there(checks: &Checks<'_>) -> Vec<Finding> {
    checks
        .summary
        .includes
        .iter()
        .filter(|include| include.resolved.is_none())
        .filter(|include| include.form != IncludeForm::Macro)
        .map(|include| Finding {
            range: include.range,
            name: include.spelling.clone(),
            check: CHECK,
            message: match include.form {
                IncludeForm::Angle => format!(
                    "`<{}>` was not found on the include path",
                    include.spelling
                ),
                // A quoted include is looked for beside the file first and then on the include path, so the
                // message says both rather than naming the one the resolver happened to try last.
                IncludeForm::Quote => format!(
                    "`\"{}\"` was not found beside this file or on the include path",
                    include.spelling
                ),
                // Refused above; written out rather than unreachable so that adding a fourth form is a
                // compile error here instead of a silently wrong message.
                IncludeForm::Macro => unreachable!("a macro target is not reported — see the module note"),
            },
        })
        .collect()
}
