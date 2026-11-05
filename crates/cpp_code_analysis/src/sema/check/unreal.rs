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

use super::CheckContext;
use cpp_parser::CppAstNode;

/// **One declaration an engine macro annotates**, in both coordinate systems.
pub(crate) struct Annotated {
    /// The macro's name — `UCLASS`, `USTRUCT`, `UENUM`, `UINTERFACE`.
    pub macro_name: String,
    /// The name's spelling, for a message.
    pub name: String,
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
    // **Every top-level declaration of the rendering, not `own_syntax_nodes`.**
    //
    // `own_top_level_nodes` keeps a child only when its **whole** range was written in this file, and a class that uses
    // an engine macro is not: `GENERATED_BODY()` expands to declarations that come from the engine's header, so the
    // class's range holds tokens written elsewhere and the class is pruned — measured on the UE-like project, where
    // `struct MyData { GENERATED_BODY() … }` disappeared from the check's view while its fact was still in the summary.
    // That is the right rule for a check that reports on a header's code, and the wrong one here: the **annotation** is
    // what says this file owns the declaration, and it is read from this file's own text below.
    for node in context.tree().children() {
        if !cpp_parser::CppDeclaration::can_cast(node.kind().into()) {
            continue;
        }
        // **The first token decides ownership**: a declaration whose opening keyword is not this file's belongs to an
        // included header, whatever its annotation says.
        let Some(first) = node.first_token() else {
            continue;
        };
        if !context
            .view
            .was_written_here(cpp_parser::source_range(first.text_range()))
        {
            continue;
        }
        // **A class body or an enumeration body** — the two annotations that name a type. `CppEnumDef::get_body` is the
        // enumeration's own body node (`enum class State : uint8 { Idle }` puts it inside the specifier's builtin type), and
        // a class's is a `CppClassBody`.
        let enumeration = cpp_parser::typed_descendants::<cpp_parser::CppEnumDef>(&node)
            .find(|definition| definition.get_body().is_some());
        let body = enumeration
            .as_ref()
            .map(|definition| definition.syntax().clone())
            .or_else(|| {
                cpp_parser::typed_descendants::<cpp_parser::CppClassBody>(&node)
                    .next()
                    .map(|class_body| class_body.syntax().clone())
            })
            .or_else(|| {
                enumeration.as_ref()?;
                cpp_parser::typed_descendants::<cpp_parser::CppCompoundStat>(&node)
                    .next()
                    .map(|statement| statement.syntax().clone())
            });
        let Some(body) = body else {
            continue;
        };
        let Some((name, name_range)) = declared_name(&node) else {
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
        let name_in_the_file = context.in_the_file(name_range);
        let body_in_the_file = context.in_the_file(cpp_parser::source_range(body.text_range()));
        found.push(Annotated {
            macro_name,
            name: name.clone(),
            name_in_the_file,
            body_in_the_file: Some((body_in_the_file.start_offset, body_in_the_file.end_offset())),
            has_a_class_body: cpp_parser::CppClassBody::can_cast(body.kind().into()),
        });
    }
    found
}

/// **The identifier of the macro call that ends immediately before `at`** — `UCLASS` of `UCLASS(...)`, when the call is
/// the last thing written before the declaration.
pub(crate) fn the_annotation_before(written: &str, at: usize) -> Option<String> {
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
    // **A char offset, and a char's own width** — the same rule as `sema::declarations::the_annotation_before`, where
    // `offset + 1` panicked on a multi-byte character in the indexing thread.
    let start = head
        .char_indices()
        .rev()
        .find(|(_, ch)| !(ch.is_alphanumeric() || *ch == '_'))
        .map(|(offset, ch)| offset + ch.len_utf8())
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

/// **The name a class or enumeration definition declares**, with its range — its own name, not the first name in its type.
///
/// A base (`class A : public B`) and a named type in the specifier are names too, which is why the first `NameExpr` was the
/// wrong answer: the definition's own name is a token the typed layer keeps.
pub(crate) fn declared_name(
    declaration: &cpp_parser::CppSyntaxNode,
) -> Option<(String, cpp_parser::SourceRange)> {
    use cpp_parser::{CppAstNode, CppAstToken, CppClassDef, CppDeclaration, CppEnumDef};
    if let Some(class) = cpp_parser::typed_descendants::<CppClassDef>(declaration)
        .find(|class| class.get_name().is_some())
    {
        let name = class.get_name()?;
        return Some((
            name.get_name_text().to_string(),
            cpp_parser::source_range(name.get_range()),
        ));
    }
    if let Some(enumeration) = cpp_parser::typed_descendants::<CppEnumDef>(declaration)
        .find(|enumeration| enumeration.get_body().is_some())
    {
        let name = enumeration.get_name()?;
        return Some((
            name.get_name_text().to_string(),
            cpp_parser::source_range(name.get_range()),
        ));
    }
    let declaration = CppDeclaration::cast(declaration.clone())?;
    let name = declaration.get_name()?;
    Some((
        name.get_name_text().to_string(),
        cpp_parser::source_range(name.get_range()),
    ))
}

/// **The name a declaration declares**, not the first name in its type: `int32 Global = 0;` declares `Global`, and the
/// first `NameExpr` in the declaration is `int32` — a named type is a name too.
pub(crate) fn the_declared_name(
    declaration: &cpp_parser::CppSyntaxNode,
) -> Option<(String, cpp_parser::SourceRange)> {
    use cpp_parser::{CppAstNode, CppAstToken, CppDeclaration};
    let declaration = CppDeclaration::cast(declaration.clone())?;
    let name = declaration.get_name()?;
    Some((
        name.get_name_text().to_string(),
        cpp_parser::source_range(name.get_range()),
    ))
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
