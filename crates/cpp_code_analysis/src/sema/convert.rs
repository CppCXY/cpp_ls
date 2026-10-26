//! **The one question three checks ask**: can a value of this type be used where that type is expected?
//!
//! `assign-type-check`, `param-type-check` and `return-type-mismatch` each assembled the answer themselves, which is how one
//! of them lost a step without the others noticing. `docs/type-checking.md` records the four boundaries found that way, and
//! three of the four were **normalisation** — a reference standing in front of the named type, a pointer read as an
//! arithmetic value, a spelling carrying `const` into a comparison. That work belongs here, done once.
//!
//! # The one rule that must not be lost
//!
//! `Unknown` is a real answer, and a check that reports on it is a check a reader learns to ignore. A class nobody has read,
//! a template parameter, a dependent type — all of those are `Unknown`, and all of them are silent.

use crate::Known;

/// **What it takes to use a value of one type where another is expected.**
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Conversion {
    /// The same type.
    Same,
    /// A standard conversion: arithmetic widening, an array to a pointer, `T&` to `const T&`.
    Implicit,
    /// A class target that a constructor could build the value into.
    NeedsABuild,
    /// **No conversion exists.** The only arm a check reports on.
    Refused,
    /// Not this layer's to guess: an unread class, a template parameter, a dependent type.
    Unknown,
}

impl Conversion {
    /// **Is this a refusal?** — the one question a check asks of the answer.
    pub fn is_refused(self) -> bool {
        self == Conversion::Refused
    }
}

/// **A written parameter without its reference and top-level `const`** — `const std::string&` → `std::string`.
///
/// `Type::class_name()` answers `None` for `Reference { to: Named(..) }`, which is how the class question was never asked at
/// all: the parameter parsed to a reference, the class name came back empty, and the index was skipped. Every caller that
/// wants to ask about the class behind a parameter needs this first, so it lives here rather than in each check.
pub fn strip_references(parameter: &str) -> &str {
    let mut written = parameter.trim();
    while let Some(rest) = written.strip_prefix("const ") {
        written = rest.trim();
    }
    while let Some(rest) = written.strip_suffix('&') {
        written = rest.trim();
    }
    written
}

/// **Can a value of `from` be used where the parameter written `written` is expected?**
///
/// `written` is the **spelling** the declaring file wrote, not a parsed type: the class question needs the name under the
/// reference, and a pointer is a different question from a reference to the same type.
pub fn conversion(
    from: &crate::sema::types::Type,
    written: &str,
    answer_the_class_question: impl FnOnce(&str) -> Option<bool>,
) -> Conversion {
    let target = crate::sema::types::parse_type_spelling(written);

    // **The relation first.** It is exact for arithmetic, pointers and plain names, and it is the only half that knows about
    // `const` and references at all.
    match from.convertible_to(&target) {
        Known::Yes(()) => return Conversion::Implicit,
        Known::No => return Conversion::Refused,
        Known::Unknown(_) => {}
    }

    // **The class case**, which the relation cannot answer: it lives in the type algebra and never sees a constructor.
    let bare = crate::sema::types::parse_type_spelling(strip_references(written));
    let Some(class) = bare.class_name().filter(|class| !class.is_empty()) else {
        return Conversion::Unknown;
    };
    match answer_the_class_question(&class) {
        Some(true) => Conversion::NeedsABuild,
        Some(false) => Conversion::Refused,
        None => Conversion::Unknown,
    }
}
