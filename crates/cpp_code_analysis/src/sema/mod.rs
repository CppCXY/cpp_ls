//! Names, scopes and modules: what the parsed text *declares*.
//!
//! [`scopes`] builds the scope tree, [`symbol`] is the vocabulary it is built from — names, bindings, and the
//! three-valued [`Known`](crate::Known) answer that keeps "not found" apart from "cannot be known yet" —
//! [`modules`] and [`module_info`] cover C++20 modules, the one part of a translation unit whose meaning is not
//! local to it.

pub mod module_info;
pub mod modules;
pub mod parser_symbols;
pub mod scopes;
pub mod symbol;
