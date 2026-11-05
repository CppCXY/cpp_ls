//! **`ue-macro-scope`** — a reflection annotation on something the build tool does not reflect.
//!
//! ```cpp
//! UCLASS()
//! class AMyActor : public AActor
//! {
//!     GENERATED_BODY()
//!
//! public:
//!     UPROPERTY(EditAnywhere)      // a member: reflected, this rule is silent
//!     bool bIsReady = false;
//!
//!     void Tick(float DeltaSeconds)
//!     {
//!         UPROPERTY(EditAnywhere)  // a local: the build tool does not reflect it, and nothing says so
//!         int32 Scratch = 0;
//!     }
//! };
//! ```
//!
//! Unreal's reflection works on **class members**: UHT reads the annotated declarations inside a `UCLASS`/`USTRUCT`
//! body and writes their registration into the generated code. A `UPROPERTY`/`UFUNCTION` on a local variable, a
//! parameter or a file-scope declaration is therefore not an error the compiler reports — it is a declaration the
//! reflection system never sees, and a reader who wrote the annotation believes it means something.
//!
//! # Why a warning rather than an error
//!
//! The program compiles. What is wrong is the *expectation* the annotation sets — a property that will not appear in
//! the editor, a function that cannot be called from Blueprint — and that is what a warning is for. The two rules that
//! do fail a build (`ue-generate-body`, `ue-generated-header`) are errors for exactly that reason.
//!
//! # What it reads
//!
//! The annotation from the written text (an empty macro leaves nothing in the tree), and the declaration's position in
//! the tree (a member is inside a `ClassBody`, anything else is not). Both halves are needed: the written text says
//! which declarations are annotated, and the tree says which of those are members.

use cpp_parser::CppSyntaxKind;

use super::unreal::{the_annotation_before, the_declared_name};
use super::{Check, CheckContext, DiagnosticCode};
use cpp_parser::CppAstNode;

/// The annotations the build tool reflects — `UPROPERTY` and `UFUNCTION` are the two that name a declaration's role.
const ANNOTATIONS: &[&str] = &["UPROPERTY", "UFUNCTION"];

pub struct UeMacroScope;

impl Check for UeMacroScope {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UeMacroScope];

    fn check(context: &mut CheckContext<'_>) {
        // **The rendering's declarations, not `own_syntax_nodes`**: a declaration whose body contains macro-expanded
        // tokens is not wholly written here, and the annotation is what proves ownership — the same rule
        // [`super::unreal`] documents.
        for node in context.tree().children() {
            if !cpp_parser::CppDeclaration::can_cast(node.kind().into()) {
                continue;
            }
            let Some(first) = node.first_token() else {
                continue;
            };
            if !context
                .view
                .was_written_here(cpp_parser::source_range(first.text_range()))
            {
                continue;
            }
            check_declaration(context, &node);
        }
    }
}

/// One top-level declaration: its nested declarations, and whether each annotated one is a member.
fn check_declaration(context: &mut CheckContext<'_>, declaration: &cpp_parser::CppSyntaxNode) {
    for node in declaration.descendants() {
        if !cpp_parser::CppDeclaration::can_cast(node.kind().into()) {
            continue;
        }
        let Some(name) = the_declared_name(&node) else {
            continue;
        };
        let in_the_file = context.in_the_file(cpp_parser::source_range(node.text_range()));
        let Some(annotation) = the_annotation_before(context.written, in_the_file.start_offset)
        else {
            continue;
        };
        if !ANNOTATIONS.contains(&annotation.as_str()) {
            continue;
        }
        // **A member is declared *directly* in a class body.** Walking up to the first class body is not enough: a local
        // inside an inline member function has one above it too, so the walk stops at the first construct that says
        // "not a member" — a function body or a parameter list — before it reaches the class.
        let mut is_a_member = false;
        for parent in node.ancestors() {
            if cpp_parser::CppClassBody::can_cast(parent.kind().into()) {
                is_a_member = true;
                break;
            }
            match CppSyntaxKind::from(parent.kind()) {
                CppSyntaxKind::CompoundStat | CppSyntaxKind::ParameterList => break,
                _ => {}
            }
        }
        if is_a_member {
            continue;
        }
        let (spelled, name_in_the_file) = (name.0.clone(), context.in_the_file(name.1));
        context.add_in_the_file(
            DiagnosticCode::UeMacroScope,
            name_in_the_file,
            spelled.clone(),
            format!(
                "`{spelled}` is annotated `{annotation}` and is not a class member — the build tool reflects members \
                 of a `UCLASS`/`USTRUCT`, so this annotation is never read: the property does not appear in the editor, \
                 the function is not callable from Blueprint"
            ),
        );
    }
}
