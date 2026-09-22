//! The typed AST layer.
//!
//! Layout and naming follow the Lua implementation this project grew out of (now under
//! `reference/`), so code written against the old API keeps working:
//!
//! ```text
//! node/
//!   traits.rs      CppAstNode, CppAstToken, CppAstChildren, CppAstTokenChildren
//!   sum.rs         CppStat, CppAst — the sum types over nodes
//!   docs.rs        documentation comments: CppDocComment, CppDocCommand, ...
//!   cpp/
//!     mod.rs       declarations, types
//!     expr.rs      expressions
//!     stat.rs      statements
//!     modules.rs   C++20 modules
//!   token/         typed token wrappers
//! ```
//!
//! # Using it
//!
//! ```ignore
//! use cpp_parser::{CppParser, CppParserConfig, CppTranslationUnit, CppAstNode};
//!
//! let tree = CppParser::parse(source, ParserConfig::default());
//! let unit = CppTranslationUnit::cast(tree.get_red_root()).unwrap();
//!
//! for decl in unit.get_declarations() {
//!     println!("{} {:?}", decl.kind_name(), decl.get_name_text());
//! }
//! ```
//!
//! Every accessor returns `Option`, because an editor parses files that are being written. Nothing
//! in this layer panics on malformed input, and nothing validates structure — a `get_name()` on a
//! half-typed declaration returns whatever name it can find.

mod cpp;
mod docs;
mod sum;
pub mod token;
pub mod traits;

pub use sum::*;

#[allow(unused_imports)]
pub use cpp::*;
#[allow(unused_imports)]
pub use docs::*;
#[allow(unused_imports)]
pub use token::*;
#[allow(unused_imports)]
pub use traits::*;
