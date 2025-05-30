use crate::{parser::CompleteMarker, parser_error::CppParseError};

// mod doc;
mod cpp;

type ParseResult = Result<CompleteMarker, CppParseError>;
// pub use doc::parse_comment;
pub use cpp::parse_cpp_unit;
 