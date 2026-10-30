//! **`ue-naming`** — Unreal Engine's prefixes, which an ordinary C++ project does not use.
//!
//! ```cpp
//! UCLASS()  class UMyComponent   : …   the U is the engine's, for a UObject-derived class
//! USTRUCT() struct FMyData       : …   F for a plain struct
//! UENUM()   enum class EMyState  : …   E for an enumeration
//! UPROPERTY() bool bIsReady;           a boolean member starts with `b`
//! ```
//!
//! Each check is a **convention** rather than a law — which is why the rule is a hint and why it runs only when the
//! project says `[unreal] enable = true`. Epic's coding standard is the source: `U` for a `UObject` class, `A` for an
//! `AActor` class, `F` for a plain struct, `E` for an enumeration, `I` for a native interface, `T` for a template, `b`
//! for a boolean.
//!
//! ```text
//!   reported   a type whose macro and name disagree about the prefix, a bool member with no `b`
//!   silent     a plain class/struct/enum (no engine macro), a parameter, a local, a function
//! ```
//!
//! # Why `A` is not judged, and why that is the honest line
//!
//! `UCLASS` covers both `UObject`-derived classes (`U…`) and `AActor`-derived ones (`A…`), and which one a class is
//! depends on its **base**, which a header may declare in another file. The rule therefore accepts either engine prefix
//! for a `UCLASS` rather than guessing the base: a wrong guess would report working engine code, and the prefix set is
//! small enough that accepting both is not a hole.
//!
//! # Where the text is read from
//!
//! Both texts, and the split is deliberate: the **annotation** (`UCLASS()`, `UPROPERTY()`) from the file's own text,
//! because an empty macro leaves nothing in the tree, and the **name and the type** from the tree with the file mapping
//! every finding already uses — see [`super::unreal`].

use cpp_parser::CppAstNode;
use cpp_parser::CppSyntaxKind;

use super::unreal::{annotated_declarations, the_type_written_before};
use super::{Check, CheckContext, DiagnosticCode};

pub struct UeNaming;

impl Check for UeNaming {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::UeNaming];

    fn check(context: &mut CheckContext<'_>) {
        // **The prefixes the engine's macro promises.** A `UCLASS` may be a `UObject` (`U…`) or an actor (`A…`), so
        // both are accepted; the other three are one prefix each.
        for (annotating, prefixes, what) in [
            ("UCLASS", &["U", "A"][..], "a class"),
            ("USTRUCT", &["F"][..], "a struct"),
            ("UENUM", &["E"][..], "an enumeration"),
            ("UINTERFACE", &["U", "I"][..], "an interface"),
        ] {
            for annotated in annotated_declarations(context, &[annotating]) {
                let spelling = annotated.name.text().to_string();
                if prefixes.iter().any(|prefix| spelling.starts_with(prefix)) {
                    continue;
                }
                context.add_in_the_file(
                    DiagnosticCode::UeNaming,
                    annotated.name_in_the_file,
                    spelling.clone(),
                    format!(
                        "`{spelling}` is {what} under `{annotating}`, and Unreal's prefix for one is `{}` — `{}{spelling}` \
                         is the engine's own convention",
                        prefixes.join("` or `"),
                        prefixes[0]
                    ),
                );
            }
        }

        // **A boolean member starts with `b`.** Asked of the class bodies rather than of the macro pair, because the
        // rule is about the member's type: `UPROPERTY() bool IsReady;` is the case a reader hits, and `bool bIsReady;`
        // is the one that passes.
        let bodies: Vec<cpp_parser::CppSyntaxNode> = context
            .own::<cpp_parser::CppClassBody>()
            .map(|body| body.syntax().clone())
            .collect();
        for body in bodies {
            for declaration in body.children() {
                if CppSyntaxKind::from(declaration.kind()) != CppSyntaxKind::Declaration {
                    continue;
                }
                let Some((name, spelled)) = bool_member(context, &declaration) else {
                    continue;
                };
                if spelled.starts_with('b')
                    && spelled.chars().nth(1).is_some_and(char::is_uppercase)
                {
                    continue;
                }
                let in_the_file = context.in_the_file(cpp_parser::source_range(name.text_range()));
                let suggested = spelled
                    .chars()
                    .next()
                    .map(|first| {
                        format!("b{}{}", first.to_uppercase(), &spelled[first.len_utf8()..])
                    })
                    .unwrap_or_default();
                context.add_in_the_file(
                    DiagnosticCode::UeNaming,
                    in_the_file,
                    spelled.clone(),
                    format!(
                        "`{spelled}` is a boolean member, and Unreal spells one `b` followed by a capital — \
                         `{suggested}`"
                    ),
                );
            }
        }
    }
}

/// The member a declaration declares, when it is a **boolean variable** — `None` for a function, an owning type, and
/// anything that is not a member.
fn bool_member(
    context: &CheckContext<'_>,
    declaration: &cpp_parser::CppSyntaxNode,
) -> Option<(cpp_parser::CppSyntaxNode, String)> {
    let declarator = declaration
        .children()
        .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::InitDeclarator)
        .and_then(|init| {
            init.children()
                .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declarator)
        })?;
    if declarator
        .children()
        .any(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::ParameterList)
    {
        return None;
    }
    let name = declarator
        .children()
        .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::NameExpr)?;
    let start = context
        .in_the_file(cpp_parser::source_range(declaration.text_range()))
        .start_offset;
    let name_start = context
        .in_the_file(cpp_parser::source_range(name.text_range()))
        .start_offset;
    let written = the_type_written_before(context.written, start, name_start)?;
    let head = written.split('<').next().unwrap_or_default().trim();
    let head = head.strip_prefix("const ").unwrap_or(head).trim();
    if head != "bool" {
        return None;
    }
    Some((name.clone(), name.text().to_string()))
}
