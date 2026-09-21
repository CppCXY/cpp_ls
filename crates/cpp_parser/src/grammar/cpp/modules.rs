//! C++20 modules.
//!
//! The grammar, from the standard's `module-declaration` / `import-declaration` / `export-declaration`
//! productions (see <https://en.cppreference.com/w/cpp/language/modules>):
//!
//! ```text
//! export? module module-name module-partition? attribute? ;      // (1) module declaration
//! export declaration                                             // (2) export declaration
//! export { declaration-seq? }                                    // (3) export block
//! export? import module-name attribute? ;                         // (4) import a module
//! export? import module-partition attribute? ;                    // (5) import a partition
//! export? import header-name attribute? ;                         // (6) import a header unit
//! module ;                                                        // (7) global module fragment
//! module : private ;                                              // (8) private module fragment
//! ```
//!
//! `module-name` is one or more identifiers separated by dots, and `module-partition` is `:`
//! followed by a module name.
//!
//! # Why `module` and `import` arrive as identifiers
//!
//! They are *contextual* keywords — the standard lists them as "identifier with special meaning",
//! not as keywords. That is deliberate on the committee's part: `module` and `import` remain usable
//! as ordinary names, and code written before C++20 must keep compiling. So the lexer hands them over
//! as [`CppTokenKind::Identifier`] and the decision is made here, from the text, in the contexts
//! where the standard says the meaning is special.
//!
//! By contrast `export` and `private` *are* real keywords, and are lexed as such.
//!
//! # Where these can appear
//!
//! A module declaration, if present, must be the first declaration in the translation unit — except
//! for a global module fragment, which comes before it and may contain only preprocessing
//! directives. Import declarations must be grouped after the module declaration and before any other
//! declaration. This module does not enforce those ordering rules: an editor has to parse the file
//! while it is being written, and a half-typed import in the wrong place is not worth an error. The
//! ordering is the semantic layer's business.

use crate::{
    grammar::ParseResult,
    kind::{CppSyntaxKind, CppTokenKind},
    parser::{CompleteMarker, CppParser, MarkerEventContainer},
    parser_error::CppParseError,
};

use super::{expect_token, exprs::parse_expr, stats::parse_stats};

/// Is the cursor at the start of any module-related declaration?
///
/// The single entry point the statement dispatcher needs, so it can route to the declaration parser
/// before any other rule examines the tokens. `module : private;` and `module A:B;` are otherwise
/// indistinguishable from a label statement.
pub fn starts_module_related_declaration(p: &CppParser) -> bool {
    is_contextual(p, "module")
        && (module_declaration_shape(p)
            || p.peek_token_kind_at(1..2)[0] == CppTokenKind::Colon
            || p.peek_token_kind_at(1..2)[0] == CppTokenKind::Semicolon)
        || starts_module_declaration(p)
        || starts_import_declaration(p)
}

/// Does this token start a module declaration or an import declaration?
///
/// `module` and `import` are only special at the start of a declaration, so this is the check that
/// keeps `int module = 1;` and `auto import = f();` working.
pub fn starts_module_declaration(p: &CppParser) -> bool {
    is_contextual(p, "module") && module_declaration_shape(p)
}

pub fn starts_import_declaration(p: &CppParser) -> bool {
    is_contextual(p, "import") && import_declaration_shape(p)
}

/// Is the cursor on a specific contextual keyword (an identifier with a given spelling)?
fn is_contextual(p: &CppParser, text: &str) -> bool {
    p.current_token() == CppTokenKind::Identifier && p.current_token_text() == text
}

/// `module` is a module declaration only when followed by a name, `:`, `;`, or `.`.
///
/// The negative case that matters is a variable or member actually *named* `module`:
/// `int module;` has `module` after `int`, which is not where this is asked, and `module = 5;` has
/// `=` after it, which fails this test.
fn module_declaration_shape(p: &CppParser) -> bool {
    matches!(
        p.peek_token_kind_at(1..2)[0],
        CppTokenKind::Identifier | CppTokenKind::Colon | CppTokenKind::Semicolon
    )
}

/// `import` is an import declaration when followed by a name, `:`, `<`, or a string.
///
/// `<` and a string literal are the header-unit forms. The negative case that matters is
/// `import = f();` or `import(x);`, both of which are ordinary uses of the name.
fn import_declaration_shape(p: &CppParser) -> bool {
    matches!(
        p.peek_token_kind_at(1..2)[0],
        CppTokenKind::Identifier
            | CppTokenKind::Colon
            | CppTokenKind::Less
            | CppTokenKind::StringLiteral
            | CppTokenKind::HeaderName
    )
}

/// Parse a declaration that `export` applies to: a module declaration, a re-exported import, or any
/// ordinary declaration.
///
/// `export` is a real keyword, so the caller has already established that the cursor is on it. What
/// follows decides the shape, and the word after `export` is the discriminator — `module` and
/// `import` are contextual keywords, so they are recognised by spelling here.
pub fn parse_exported_declaration(p: &mut CppParser) -> ParseResult {
    if is_contextual_at(p, 1, "module") {
        return parse_module_declaration(p);
    }
    if is_contextual_at(p, 1, "import") {
        return parse_import_declaration(p);
    }

    // `export <declaration>`: the keyword belongs to the declaration, so consume it and parse the
    // rest with the ordinary rules.
    p.bump();
    super::decls::parse_declaration(p)
}

/// Is the significant token at relative offset `offset` an identifier spelled `text`?
fn is_contextual_at(p: &CppParser, offset: usize, text: &str) -> bool {
    p.peek_token_kind_at(offset..offset + 1)[0] == CppTokenKind::Identifier
        && p.peek_token_text_at(offset) == text
}

/// Parse a module declaration: `export? module name : partition? ;`.
pub fn parse_module_declaration(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::ModuleDecl);

    // `export` is optional and has already been consumed by the caller if present; check again here
    // so the rule works when called directly.
    if p.current_token() == CppTokenKind::ExportKeyword {
        p.bump();
    }

    if !is_contextual(p, "module") {
        p.close_marks_above(base);
        return Err(CppParseError::syntax_error_from(
            "expected `module`",
            p.current_token_range(),
        ));
    }
    p.bump();

    parse_optional_module_name(p)?;
    parse_optional_module_partition(p)?;
    eat_attributes(p);
    expect_token(p, CppTokenKind::Semicolon)?;

    Ok(m.complete(p))
}

/// Parse an import declaration: `export? import name | :partition | <header> ;`.
pub fn parse_import_declaration(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::ImportDecl);

    if p.current_token() == CppTokenKind::ExportKeyword {
        p.bump();
    }

    if !is_contextual(p, "import") {
        p.close_marks_above(base);
        return Err(CppParseError::syntax_error_from(
            "expected `import`",
            p.current_token_range(),
        ));
    }
    p.bump();

    match p.current_token() {
        // `import :partition;`
        CppTokenKind::Colon => {
            parse_optional_module_partition(p)?;
        }
        // `import <header>;` — the lexer has to be asked for a header name, because `<` is far too
        // common to guess at during the ordinary sweep. See `CppParser::try_lex_header_name`.
        CppTokenKind::Less | CppTokenKind::StringLiteral | CppTokenKind::HeaderName => {
            parse_header_unit_name(p)?;
        }
        _ => {
            parse_optional_module_name(p)?;
        }
    }

    eat_attributes(p);
    expect_token(p, CppTokenKind::Semicolon)?;

    Ok(m.complete(p))
}

/// Parse an export block: `export { declaration-seq? }`.
///
/// An export block is a scope of its own for the declarations inside it, so it gets its own node
/// rather than borrowing the compound-statement one.
pub fn parse_export_block(p: &mut CppParser) -> ParseResult {
    let _ = p.open_marks();
    let m = p.mark(CppSyntaxKind::ExportBlock);

    expect_token(p, CppTokenKind::ExportKeyword)?;
    expect_token(p, CppTokenKind::LeftBrace)?;

    parse_stats(p);

    if p.current_token() == CppTokenKind::RightBrace {
        p.bump();
    } else {
        // A half-typed export block is normal while editing.
        p.emit_missing_node();
    }

    Ok(m.complete(p))
}

/// Parse the global module fragment header: `module ;`.
///
/// The fragment's contents are preprocessing directives, which the ordinary statement parser already
/// handles as leaf nodes, so the caller just keeps going.
pub fn parse_global_module_fragment(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::GlobalModuleFragment);

    if !is_contextual(p, "module") {
        p.close_marks_above(base);
        return Err(CppParseError::syntax_error_from(
            "expected `module`",
            p.current_token_range(),
        ));
    }
    p.bump();
    expect_token(p, CppTokenKind::Semicolon)?;

    Ok(m.complete(p))
}

/// Parse the private module fragment header: `module : private ;`.
pub fn parse_private_module_fragment(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::PrivateModuleFragment);

    if !is_contextual(p, "module") {
        p.close_marks_above(base);
        return Err(CppParseError::syntax_error_from(
            "expected `module`",
            p.current_token_range(),
        ));
    }
    p.bump();
    expect_token(p, CppTokenKind::Colon)?;

    // `private` is a real keyword, not a contextual one.
    if !is_contextual(p, "private") && p.current_token() != CppTokenKind::PrivateKeyword {
        p.close_marks_above(base);
        return Err(CppParseError::syntax_error_from(
            "expected `private`",
            p.current_token_range(),
        ));
    }
    p.bump();
    expect_token(p, CppTokenKind::Semicolon)?;

    Ok(m.complete(p))
}

/// Parse a dotted module name: `a`, `a.b`, `my.mod.part`.
fn parse_optional_module_name(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::ModuleName);

    if p.current_token() != CppTokenKind::Identifier {
        // An anonymous module is not valid, but a missing name is what a half-typed module
        // declaration looks like, so report and carry on rather than refusing to build a node.
        p.emit_missing_node();
        p.close_marks_above(base);
        return Ok(CompleteMarker::empty());
    }

    p.bump();
    while p.current_token() == CppTokenKind::Dot {
        p.bump();
        if p.current_token() == CppTokenKind::Identifier {
            p.bump();
        } else {
            p.emit_missing_node();
            break;
        }
    }

    Ok(m.complete(p))
}

/// Parse a module partition: `: name` or `:name.part`.
fn parse_optional_module_partition(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();

    if p.current_token() != CppTokenKind::Colon {
        p.close_marks_above(base);
        return Ok(CompleteMarker::empty());
    }

    let m = p.mark(CppSyntaxKind::ModulePartition);
    p.bump(); // `:`
    parse_optional_module_name(p)?;

    Ok(m.complete(p))
}

/// Parse a header unit name: `<header>` or `"header"`.
///
/// The parser folds the `<`, name and `>` tokens into one header-name token on request, which is the
/// only way this can be recognised — see `CppParser::try_lex_header_name`.
fn parse_header_unit_name(p: &mut CppParser) -> ParseResult {
    let base = p.open_marks();
    let m = p.mark(CppSyntaxKind::HeaderName);

    if p.try_lex_header_name() {
        return Ok(m.complete(p));
    }

    // A preprocessor macro or expression in place of a literal header name: `import HEADER;` is
    // legal because the preprocessor runs first. Take the tokens as they come.
    if let Err(err) = parse_expr(p) {
        p.close_marks_above(base);
        return Err(err);
    }

    Ok(m.complete(p))
}

/// Consume any `[[...]]` attributes.
fn eat_attributes(p: &mut CppParser) {
    while p.current_token() == CppTokenKind::LeftBracket
        && p.peek_next_token() == CppTokenKind::LeftBracket
    {
        if super::types::parse_attribute_specifier(p).is_err() {
            return;
        }
    }
}

/// Is the cursor at the start of a `module ;` global module fragment?
pub fn starts_global_module_fragment(p: &CppParser) -> bool {
    is_contextual(p, "module") && p.peek_token_kind_at(1..2)[0] == CppTokenKind::Semicolon
}

/// Is the cursor at the start of a `module : private ;` fragment header?
///
/// This has to be checked *before* the partition case, because both start with `module :`. The
/// difference is the word after the colon: `private` followed by `;` is the fragment header, while
/// anything else is a partition name.
pub fn starts_private_module_fragment(p: &CppParser) -> bool {
    // Note the slice comparison: `peek_token_kind_at` hands back a `Vec`, and `Vec<T> == [T; N]` is
    // always false, so comparing against an array literal here silently never matched.
    is_contextual(p, "module")
        && p.peek_token_kind_at(1..4).as_slice()
            == [
                CppTokenKind::Colon,
                CppTokenKind::PrivateKeyword,
                CppTokenKind::Semicolon,
            ]
}

