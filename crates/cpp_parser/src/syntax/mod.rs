mod id;
pub mod node;
mod tree;

pub use id::CppSyntaxId;
pub use node::*;
pub use tree::{CppGreenNodeBuilder, CppSyntaxTree, CppTreeBuilder};

// Re-exported so that `rowan` users of this crate can name the node/token types and the
// `Language` impl without depending on `kind` directly. Some of these are not referenced from
// inside the crate yet, which is exactly the point: they are the public API surface.
#[allow(unused_imports)]
pub use crate::kind::{
    CppLanguage, CppSyntaxElement, CppSyntaxElementChildren, CppSyntaxNode, CppSyntaxNodeChildren,
    CppSyntaxNodePtr, CppSyntaxToken,
};

// NOTE: `node/` (the typed AST accessor layer) and `traits.rs` are still Lua-era code and are
// deliberately not wired up yet. They are kept on disk as a reference for the accessor style we
// want once the grammar has real node kinds to expose. The previous `comment_trait.rs` /
// `LuaSyntaxId` style helpers are replaced by `id.rs` in the meantime.
