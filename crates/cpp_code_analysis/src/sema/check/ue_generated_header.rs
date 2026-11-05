//! **`ue-generated-header`** — a file with reflection annotations must include its generated header, and last.
//!
//! ```cpp
//! #pragma once
//!
//! #include "CoreMinimal.h"
//! #include "MyActor.generated.h"     // this rule: the *last* #include, after every other
//!
//! UCLASS()
//! class AMyActor : public AActor { … };
//! ```
//!
//! Unreal's build tool (UHT) reads a header, writes the reflection code for every annotated declaration into
//! `<File>.generated.h`, and the source must include it — otherwise `GENERATED_BODY()` has nothing to expand to and the
//! build fails. The **order** is part of the contract: UHT requires the generated header to be the last include, because
//! the code it writes is appended to what the other headers declared and the macros it defines are used by the
//! declarations below.
//!
//! # What it checks, and what it does not
//!
//! ```text
//!   reported   a file with UCLASS/USTRUCT/UENUM/UINTERFACE and no `*.generated.h` include at all
//!              a file whose generated include is not the last one
//!   silent     a file with no annotated declaration (a plain header, a .cpp)
//! ```
//!
//! **One finding per file**, not per declaration: the mistake is the file's, and a header with forty classes and no
//! generated include should not produce forty findings for one line to add.
//!
//! The include is read from the **summary's include facts** — the directives as the file wrote them, with their ranges
//! in the file — because a `#include` is consumed before the parse and is not in the rendering. The annotations come
//! from [`super::unreal`], which reads them from the written text for the same reason: an empty macro leaves nothing
//! behind.

use super::unreal::annotated_declarations;
use super::{Check, CheckContext, DiagnosticCode};

pub struct UeGeneratedHeader;

impl Check for UeGeneratedHeader {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UeGeneratedHeader];

    fn check(context: &mut CheckContext<'_>) {
        let annotated =
            annotated_declarations(context, &["UCLASS", "USTRUCT", "UENUM", "UINTERFACE"]);
        if annotated.is_empty() {
            return;
        }
        // **The last include is the one the build tool reads the position of.** `includes` is in document order, so the
        // last fact is the last directive — and the file's own guard (`#pragma once`) is not an include.
        let Some(last) = context.summary.includes.last() else {
            report_missing(context, &annotated[0]);
            return;
        };
        if is_a_generated_header(&last.spelling) {
            return;
        }
        // A generated header exists but is not last: the finding is at the include that came after it, because that is
        // the line a reader has to move above the generated one.
        let spelling = last.spelling.clone();
        context.add_in_the_file(
            DiagnosticCode::UeGeneratedHeader,
            last.range,
            spelling.clone(),
            format!(
                "`{spelling}` is the last `#include`, and the generated header has to be — Unreal's build tool writes \
                 the reflection code into `*.generated.h` and the macros it defines are used by the declarations below, \
                 so the build fails when another header follows it"
            ),
        );
    }
}

/// The "no generated header at all" finding, placed on the first annotated declaration — the line that promised the
/// build tool something it will not find.
fn report_missing(context: &mut CheckContext<'_>, first: &super::unreal::Annotated) {
    let name = first.name.clone();
    context.add_in_the_file(
        DiagnosticCode::UeGeneratedHeader,
        first.name_in_the_file,
        name.clone(),
        format!(
            "`{name}` is declared with `{}` and this file includes no `*.generated.h` — the build tool writes its \
             reflection code into that header, and the class does not build without it",
            first.macro_name
        ),
    );
}

/// **A spelling that names a generated header** — `MyActor.generated.h`, `MyGame.generated.h`.
fn is_a_generated_header(spelling: &str) -> bool {
    spelling.ends_with(".generated.h") || spelling.ends_with(".generated.hpp")
}
