//! **The engine's macros are not in the tree** — and the two UE rules both need them.
//!
//! A UE project defines `UCLASS(...)`, `UPROPERTY(...)` and `GENERATED_BODY()` as **empty** macros, because that is what
//! they are to a compiler: the engine's build tool is what gives them meaning. The rendering this layer reads is made
//! after the preprocessor, so those calls are gone from it — measured on the UE-like project: with the simulated
//! `CoreMinimal.h` found, `UCLASS()` vanished from the tree and only the class remained.
//!
//! So the annotation is read from [`CheckContext::written`] — the file as the reader wrote it — by walking **back** from
//! the declaration's file offset to the macro call that ends there:
//!
//! ```text
//!   UCLASS(Blueprintable)      <- read from the written text
//!   class AMyActor : …         <- the declaration, in the tree
//! ```
//!
//! Everything else a rule needs — the name, the body — still comes from the tree and is mapped to the file with
//! [`CheckContext::in_the_file`], which is the one crossing this layer already makes for every finding.

use cpp_parser::CppSyntaxKind;

use super::CheckContext;

/// **One declaration an engine macro annotates**, in both coordinate systems.
pub(crate) struct Annotated {
    /// The macro's name — `UCLASS`, `USTRUCT`, `UENUM`, `UINTERFACE`.
    pub macro_name: String,
    /// The name as the tree has it, for a message and for the rendering range.
    pub name: cpp_parser::CppSyntaxNode,
    /// The name as the file has it — where a finding is placed.
    pub name_in_the_file: cpp_parser::SourceRange,
    /// The body's file range, when the declaration has one.
    pub body_in_the_file: Option<(usize, usize)>,
    /// Whether that body is a **class** body — a `USTRUCT`/`UCLASS` has one, a `UENUM` does not.
    pub has_a_class_body: bool,
}

/// **The declarations one of `macros` annotates**, from the written text and the tree together.
pub(crate) fn annotated_declarations(
    context: &CheckContext<'_>,
    macros: &[&str],
) -> Vec<Annotated> {
    let mut found = Vec::new();
    for node in context.own_syntax_nodes() {
        if CppSyntaxKind::from(node.kind()) != CppSyntaxKind::Declaration {
            continue;
        }
        // **A class body or an enumeration body** — the two annotations that name a type. An enumeration parses its
        // body as a `CompoundStat` inside the `BuiltinType`, so which kind is a body depends on the declaration being
        // an enum; measured on `enum class State : uint8 { Idle };`.
        let is_an_enum = node
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .any(|token| {
                cpp_parser::CppTokenKind::from(token.kind())
                    == cpp_parser::CppTokenKind::EnumKeyword
            });
        let body = node
            .descendants()
            .find(|child| match CppSyntaxKind::from(child.kind()) {
                CppSyntaxKind::ClassBody => true,
                CppSyntaxKind::EnumDef => true,
                CppSyntaxKind::CompoundStat => is_an_enum,
                _ => false,
            });
        let Some(body) = body else {
            continue;
        };
        let Some(name) = declared_name(&node) else {
            continue;
        };
        let declaration_in_the_file =
            context.in_the_file(cpp_parser::source_range(node.text_range()));
        let Some(macro_name) =
            the_annotation_before(context.written, declaration_in_the_file.start_offset)
        else {
            continue;
        };
        if !macros.contains(&macro_name.as_str()) {
            continue;
        }
        let name_in_the_file = context.in_the_file(cpp_parser::source_range(name.text_range()));
        let body_in_the_file = context.in_the_file(cpp_parser::source_range(body.text_range()));
        found.push(Annotated {
            macro_name,
            name: name.clone(),
            name_in_the_file,
            body_in_the_file: Some((body_in_the_file.start_offset, body_in_the_file.end_offset())),
            has_a_class_body: CppSyntaxKind::from(body.kind()) == CppSyntaxKind::ClassBody,
        });
    }
    found
}

/// **The identifier of the macro call that ends immediately before `at`** — `UCLASS` of `UCLASS(...)`, when the call is
/// the last thing written before the declaration.
fn the_annotation_before(written: &str, at: usize) -> Option<String> {
    let before = written.get(..at)?;
    let trimmed = before.trim_end();
    if !trimmed.ends_with(')') {
        return None;
    }
    // Walk back to the `(` that opened this call, counting the groups in between.
    let bytes = trimmed.as_bytes();
    let mut depth = 0usize;
    let mut index = bytes.len();
    while index > 0 {
        index -= 1;
        match bytes[index] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    let head = trimmed[..index].trim_end();
    let start = head
        .rfind(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
        .map(|offset| offset + 1)
        .unwrap_or(0);
    let identifier = &head[start..];
    if identifier.is_empty()
        || identifier
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit())
    {
        return None;
    }
    Some(identifier.to_string())
}

/// The name a class or enumeration definition declares — the first `NameExpr` before its body.
pub(crate) fn declared_name(
    declaration: &cpp_parser::CppSyntaxNode,
) -> Option<cpp_parser::CppSyntaxNode> {
    declaration
        .descendants()
        .find(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::NameExpr)
}

/// **The type a member is written with**, from the file's own text: from the declaration's start to the name's, with
/// the macro calls a UE member is annotated with (`UPROPERTY(...)`) cut off the front.
pub(crate) fn the_type_written_before(
    written: &str,
    start: usize,
    name_start: usize,
) -> Option<String> {
    let mut text = written.get(start..name_start)?.trim();
    loop {
        let trimmed = text.trim_start();
        let Some(paren) = trimmed.find('(') else {
            break;
        };
        let head = trimmed[..paren].trim_end();
        let is_a_macro_name = !head.is_empty()
            && head
                .chars()
                .all(|ch| ch.is_alphanumeric() || ch == '_' || ch == ':')
            && head.chars().next().is_some_and(|ch| !ch.is_ascii_digit());
        if !is_a_macro_name {
            break;
        }
        // The matching `)` of that call, so that `UPROPERTY(EditAnywhere)` is dropped as one piece.
        let mut depth = 0usize;
        let mut close = None;
        for (offset, ch) in trimmed.char_indices().skip(paren) {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(offset);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else {
            break;
        };
        text = &trimmed[close + 1..];
    }
    Some(text.trim().to_string())
}
