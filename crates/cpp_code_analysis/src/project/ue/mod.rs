//! **Unreal Engine** — everything the analysis knows about it, in one module.
//!
//! ```text
//! .cppls.toml                 [unreal] enable = true
//! Engine/Source/…             the engine's own headers, when the machine has an engine
//! Source/<Module>/…           the project's modules
//! Intermediate/Build/…/UHT    the reflection headers the build tool generated
//! ```
//!
//! # What is here today
//!
//! [`UnrealSection`] — the `[unreal]` section of `.cppls.toml`. One key so far (`enable`), and it is deliberately a
//! project's decision rather than a detection: a file that writes `UCLASS()` outside UE is a file with an unknown
//! macro, not a game, and running Epic's naming rules on an engine-agnostic project would report every `class Widget`
//! as a missing `A`/`U`/`F`.
//!
//! # What will be here, and why it is a module rather than a file
//!
//! UE support is not one rule; it is a way of reading a project — which engine, which modules, which reflection headers,
//! which macros the build tool would have defined — and every one of those answers is needed by several layers at once.
//! The work that belongs here:
//!
//! ```text
//! discovery     the engine's root (from .uproject, or the section), the module list (from *.Build.cs), the
//!               generated include directories (Intermediate/Build/<Platform>/<Config>/Inc/<Module>/UHT), and the
//!               macros those imply: MYGAME_API, WITH_EDITOR, WITH_EDITORONLY_DATA, PLATFORM_WINDOWS, UE_BUILD_*
//! reading       the reflection annotations — UCLASS/USTRUCT/UENUM/UINTERFACE/UPROPERTY/UFUNCTION and their
//!               specifiers — which are **empty macros** and therefore have to be read from the written text
//! members       what GENERATED_BODY() stands for: StaticClass(), GetPrivateStaticClass(), Super, ThisClass
//! stubs         a minimal engine header set, so a project without an installed engine still parses
//! ```
//!
//! **The rules themselves do not live here.** One check per code, in
//! [`crate::sema::check`](crate::sema::check) — `ue_naming`, `ue_generate_body` — like every other rule, because that is
//! where a rule can be found by its code and where its tests are. What this module owns is the *answers* those rules
//! read, so that two rules cannot disagree about which class a `UCLASS()` annotates.

use serde::Deserialize;

/// **`[unreal]`** — whether this project is Unreal Engine code.
///
/// ```toml
/// [unreal]
/// enable = true
/// ```
///
/// The one key, and it is deliberately a project's decision rather than a detection: a file that writes `UCLASS()`
/// outside UE is a file with an unknown macro, not a game, and running Epic's naming rules on an engine-agnostic
/// project would report every `class Widget` as a missing `A`/`U`/`F`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnrealSection {
    /// Run the Unreal conventions — see `ue-naming` and `ue-generate-body`.
    #[serde(default)]
    pub enable: bool,
}

impl UnrealSection {
    /// **Is the engine reading on?** — the one question every caller asks of this section.
    pub fn enabled(&self) -> bool {
        self.enable
    }
}
