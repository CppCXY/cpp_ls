# Reference material (not compiled)

This directory is **excluded from the crate's module graph** and is not built or tested. It is
checked in so we can consult it while implementing the C++ comment/documentation layer, and so the
API style of the Lua language server this project was ported from stays visible.

## `node-lua/`, `node-doc/`, `node-token/`

The Lua-era halves of `src/syntax/node/`, moved out when the C++ node layer was written. They are
the **style reference** for the live code: `CppAstNode` / `CppAstToken` / `CppAstChildren`, the
`cast` / `can_cast` pair, `get_*` accessors returning `Option`, and one sum type per node family
(`CppExpr`, `CppStat`). Keeping them here is what makes "we use it the same way as the Lua code"
checkable rather than a matter of memory.

- `node-lua/` — `LuaAstNode` implementations for expressions, statements and paths.
- `node-doc/` — LDoc comment nodes (the ancestor of a future Doxygen layer).
- `node-token/` — `number_analyzer.rs` and `string_analyzer.rs`: literal value extraction
  (hex floats, escape sequences). Worth consulting if the C++ layer ever needs literal *values*
  rather than literal text, though C++ adds digit separators and raw strings on top.

Their `test.rs` files were dropped rather than moved: they were written against the Lua grammar and
would not compile even as reference.

## `ldoc-grammar/`

The former `src/grammar/doc/` module: a LuaDoc/LDoc comment grammar inherited from the upstream Lua
language server this project was forked from. It parses `---`-style comments and `@class`,
`@alias`, `@field`, `@param`, `@return`, `@generic`, `@overload`, `@meta`, `@version`, `@see`,
`@diagnostic`, ... tags.

### Why it does not compile

It is written against the pre-migration `kind` layer and a `LuaDocParser`/`LuaDocLexer` pair that
are still commented out in `src/parser/cpp_doc_parser.rs` and `src/lexer/cpp_doc_lexer.rs`. It
references types that no longer exist:

- `crate::lexer::LuaDocLexerState`
- `crate::parser::LuaDocParser`
- `crate::parser_error::LuaParseError`
- `crate::kind::{LuaOpKind, LuaTypeBinaryOperator, LuaTypeUnaryOperator}`

### What it is useful for

The *structure* is what we want to keep, not the LDoc vocabulary:

- `mod.rs` — the state machine that walks a comment group (`Init` / `Tag` / `Description` /
  `LongDescription`) and the `parse_docs` / `parse_description` / `parse_normal_description` split.
- `tag.rs` — one `parse_tag_*` function per tag, each a small recursive-descent routine. This is the
  exact shape a Doxygen tag set needs (`@brief`, `@param[in,out]`, `@retval`, `@tparam`,
  `@exception`, `@note`, `@warning`, `@code`/`@endcode`, `@see`, `@deprecated`, `@since`).
- `types.rs` — an LDoc *type expression* grammar (unions `|`, tuples, fun types, generic
  parameters). Doxygen has no equivalent type grammar, so most of this will be dropped rather than
  ported; only the suffix/qualifier handling is likely to survive.

### Porting notes

1. Restore a C++ doc lexer/parser first (`cpp_doc_lexer.rs`, `cpp_doc_parser.rs`); they define the
   `LuaDocParser`-equivalent API the grammar functions call (`current_token`, `bump`, `set_state`,
   `push_error`, `mark`).
2. Tag vocabulary has to be rewritten: LDoc tags (`@class`, `@alias`, `@field`) do not exist in
   Doxygen, which uses `@brief`, `@param`, `@return`, `@tparam`, `@exception`, `@note`,
   `@warning`, `@see`, `@since`, `@deprecated`, `@code`, `@ingroup`, `@file`, ...
3. Comment syntax differs as well: LDoc uses repeated `---` lines, Doxygen uses `///` or `/** */`
   and includes commands that span blocks (`@code ... @endcode`), which the state machine needs to
   handle as a distinct lexical state.
