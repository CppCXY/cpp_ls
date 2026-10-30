//! **`ue-generate-body`** — a `UCLASS`/`USTRUCT` whose body writes no `GENERATED_BODY()`.
//!
//! ```cpp
//! UCLASS()
//! class UMyComponent : public UActorComponent     // this rule says: GENERATED_BODY() is missing
//! {
//! public:
//!     UPROPERTY()
//!     int32 Count = 0;
//! };
//! ```
//!
//! In Unreal Engine the macro is not decoration: `GENERATED_BODY()` is what the build tool replaces with the reflection
//! data, the generated constructor and the `StaticClass()` accessor — a class that writes `UCLASS()` and not the macro
//! does not compile. That is why this rule is an **error** rather than a hint, and why the answer is certain: the
//! declaration parses either way, and the engine's own build fails.
//!
//! # Where the macro is read from
//!
//! The written text, because an empty macro leaves nothing in the tree — see [`super::unreal`]. `UCLASS()` is found by
//! walking back from the class declaration in the file, and `GENERATED_BODY()` by looking **inside the body's own text**.
//!
//! # What it does not judge
//!
//! Only the macros whose body the engine requires: `UCLASS()`, `USTRUCT()` and `UINTERFACE()`. A plain `class` or
//! `struct` is not engine code at all, and a `UENUM()` has no body macro. The older `GENERATED_UCLASS_BODY()` /
//! `GENERATED_USTRUCT_BODY()` / `GENERATED_IINTERFACE_BODY()` spellings are accepted: they are what a project written
//! against an older engine version has, and the macro they name does generate the body.

use super::unreal::annotated_declarations;
use super::{Check, CheckContext, DiagnosticCode};

pub struct UeGenerateBody;

impl Check for UeGenerateBody {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UeGenerateBody];

    fn check(context: &mut CheckContext<'_>) {
        for annotated in annotated_declarations(context, &["UCLASS", "USTRUCT", "UINTERFACE"]) {
            if !annotated.has_a_class_body {
                continue;
            }
            let Some((start, end)) = annotated.body_in_the_file else {
                continue;
            };
            let Some(body) = context.written.get(start..end) else {
                continue;
            };
            let generated = [
                "GENERATED_BODY",
                "GENERATED_UCLASS_BODY",
                "GENERATED_USTRUCT_BODY",
                "GENERATED_IINTERFACE_BODY",
            ]
            .iter()
            .any(|spelling| body.contains(spelling));
            if generated {
                continue;
            }
            let name = annotated.name.text().to_string();
            context.add_in_the_file(
                DiagnosticCode::UeGenerateBody,
                annotated.name_in_the_file,
                name.clone(),
                format!(
                    "`{name}` writes `{}` and no `GENERATED_BODY()` — the macro is what the engine's build replaces \
                     with the reflection data, so the class does not compile without it",
                    annotated.macro_name
                ),
            );
        }
    }
}
