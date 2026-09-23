//! Names, scopes and modules: what the parsed text *declares*, and what a name written in it refers to.
//!
//! [`scopes`] builds the scope tree, [`symbol`] is the vocabulary it is built from — names, bindings, and the
//! three-valued [`Known`](crate::Known) answer that keeps "not found" apart from "cannot be known yet" —
//! [`declarations`] turns the tree into the **facts an index stores**, [`resolve`] is the first *query* over the
//! tree rather than a producer of it, and [`modules`] and [`module_info`] cover C++20 modules, the one part of a
//! translation unit whose meaning is not local to it.

pub mod declarations;
pub mod module_info;
pub mod modules;
pub mod parser_symbols;
pub mod resolve;
pub mod scopes;
pub mod symbol;
