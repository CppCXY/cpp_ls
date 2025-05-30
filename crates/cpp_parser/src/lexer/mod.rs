mod lexer_config;
mod cpp_doc_lexer;
mod cpp_lexer;
mod test;
mod token_data;

pub use lexer_config::LexerConfig;
// pub use lua_doc_lexer::{LuaDocLexer, LuaDocLexerState};
pub use cpp_lexer::CppLexer;
pub use token_data::CppTokenData;

fn is_name_start(ch: char) -> bool {
    ch.is_alphabetic() || ch == '_'
}

fn is_name_continue(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}
