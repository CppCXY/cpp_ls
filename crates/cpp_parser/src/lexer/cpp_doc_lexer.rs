// use crate::{
//     kind::CppTokenKind,
//     text::{Reader, SourceRange},
// };

// use super::{is_name_continue, is_name_start};

// #[derive(Debug, Clone)]
// pub struct LuaDocLexer<'a> {
//     origin_text: &'a str,
//     origin_token_kind: CppTokenKind,
//     pub state: LuaDocLexerState,
//     pub reader: Option<Reader<'a>>,
// }

// #[derive(Debug, Clone, Copy, PartialEq, Eq)]
// pub enum LuaDocLexerState {
//     Init,
//     Tag,
//     Normal,
//     FieldStart,
//     Description,
//     LongDescription,
//     Trivia,
//     See,
//     Version,
//     Source,
//     NormalDescription,
// }

// impl LuaDocLexer<'_> {
//     pub fn new(origin_text: &str) -> LuaDocLexer<'_> {
//         LuaDocLexer {
//             origin_text,
//             reader: None,
//             origin_token_kind: CppTokenKind::None,
//             state: LuaDocLexerState::Init,
//         }
//     }

//     pub fn is_invalid(&self) -> bool {
//         match self.reader {
//             Some(ref reader) => reader.is_eof(),
//             None => true,
//         }
//     }

//     pub fn reset(&mut self, kind: CppTokenKind, range: SourceRange) {
//         self.reader = Some(Reader::new_with_range(self.origin_text, range));
//         self.origin_token_kind = kind;
//     }

//     pub fn lex(&mut self) -> CppTokenKind {
//         let reader = self.reader.as_mut().unwrap();
//         reader.reset_buff();

//         if reader.is_eof() {
//             return CppTokenKind::TkEof;
//         }

//         match self.state {
//             LuaDocLexerState::Init => self.lex_init(),
//             LuaDocLexerState::Tag => self.lex_tag(),
//             LuaDocLexerState::Normal => self.lex_normal(),
//             LuaDocLexerState::FieldStart => self.lex_field_start(),
//             LuaDocLexerState::Description => self.lex_description(),
//             LuaDocLexerState::LongDescription => self.lex_long_description(),
//             LuaDocLexerState::Trivia => self.lex_trivia(),
//             LuaDocLexerState::See => self.lex_see(),
//             LuaDocLexerState::Version => self.lex_version(),
//             LuaDocLexerState::Source => self.lex_source(),
//             LuaDocLexerState::NormalDescription => self.lex_normal_description(),
//         }
//     }

//     pub fn current_token_range(&self) -> SourceRange {
//         self.reader.as_ref().unwrap().saved_range()
//     }

//     fn lex_init(&mut self) -> CppTokenKind {
//         let reader = self.reader.as_mut().unwrap();
//         match reader.current_char() {
//             '-' if reader.is_start_of_line() => {
//                 let count = reader.consume_char_n_times('-', 3);
//                 match count {
//                     2 => {
//                         if self.origin_token_kind == CppTokenKind::TkLongComment {
//                             reader.bump();
//                             reader.eat_when('=');
//                             reader.bump();

//                             match reader.current_char() {
//                                 '@' => {
//                                     reader.bump();
//                                     CppTokenKind::TkDocLongStart
//                                 }
//                                 _ => CppTokenKind::TkLongCommentStart,
//                             }
//                         } else {
//                             CppTokenKind::TkNormalStart
//                         }
//                     }
//                     3 => {
//                         reader.eat_while(is_doc_whitespace);
//                         match reader.current_char() {
//                             '@' => {
//                                 reader.bump();
//                                 CppTokenKind::TkDocStart
//                             }
//                             _ => CppTokenKind::TkNormalStart,
//                         }
//                     }
//                     _ => {
//                         reader.eat_while(|_| true);
//                         CppTokenKind::TKDocTriviaStart
//                     }
//                 }
//             }
//             _ => {
//                 reader.eat_while(|_| true);
//                 CppTokenKind::TkDocTrivia
//             }
//         }
//     }

//     fn lex_tag(&mut self) -> CppTokenKind {
//         let reader = self.reader.as_mut().unwrap();
//         match reader.current_char() {
//             ch if is_doc_whitespace(ch) => {
//                 reader.eat_while(is_doc_whitespace);
//                 CppTokenKind::TkWhitespace
//             }
//             ch if is_name_start(ch) => {
//                 reader.bump();
//                 reader.eat_while(is_name_continue);
//                 let text = reader.current_saved_text();
//                 to_tag(text)
//             }
//             _ => {
//                 reader.eat_while(|_| true);
//                 CppTokenKind::TkDocTrivia
//             }
//         }
//     }

//     fn lex_normal(&mut self) -> CppTokenKind {
//         let reader = self.reader.as_mut().unwrap();
//         match reader.current_char() {
//             ch if is_doc_whitespace(ch) => {
//                 reader.eat_while(is_doc_whitespace);
//                 CppTokenKind::TkWhitespace
//             }
//             ':' => {
//                 reader.bump();
//                 CppTokenKind::TkColon
//             }
//             '.' => {
//                 reader.bump();
//                 if reader.current_char() == '.' && reader.next_char() == '.' {
//                     reader.bump();
//                     reader.bump();
//                     CppTokenKind::TkDots
//                 } else {
//                     CppTokenKind::TkDot
//                 }
//             }
//             ',' => {
//                 reader.bump();
//                 CppTokenKind::TkComma
//             }
//             ';' => {
//                 reader.bump();
//                 CppTokenKind::TkSemicolon
//             }
//             '(' => {
//                 reader.bump();
//                 CppTokenKind::TkLeftParen
//             }
//             ')' => {
//                 reader.bump();
//                 CppTokenKind::TkRightParen
//             }
//             '[' => {
//                 reader.bump();
//                 CppTokenKind::TkLeftBracket
//             }
//             ']' => {
//                 reader.bump();
//                 if self.origin_token_kind == CppTokenKind::TkLongComment {
//                     match reader.current_char() {
//                         '=' => {
//                             reader.eat_when('=');
//                             reader.bump();
//                             return CppTokenKind::TkLongCommentEnd;
//                         }
//                         ']' => {
//                             reader.bump();
//                             return CppTokenKind::TkLongCommentEnd;
//                         }
//                         _ => (),
//                     }
//                 }

//                 CppTokenKind::TkRightBracket
//             }
//             '{' => {
//                 reader.bump();
//                 CppTokenKind::TkLeftBrace
//             }
//             '}' => {
//                 reader.bump();
//                 CppTokenKind::TkRightBrace
//             }
//             '<' => {
//                 reader.bump();
//                 CppTokenKind::TkLt
//             }
//             '>' => {
//                 reader.bump();
//                 CppTokenKind::TkGt
//             }
//             '|' => {
//                 reader.bump();
//                 CppTokenKind::TkDocOr
//             }
//             '&' => {
//                 reader.bump();
//                 CppTokenKind::TkDocAnd
//             }
//             '?' => {
//                 reader.bump();
//                 CppTokenKind::TkDocQuestion
//             }
//             '+' => {
//                 reader.bump();
//                 CppTokenKind::TkPlus
//             }
//             '-' => {
//                 let count = reader.eat_when('-');
//                 match count {
//                     1 => CppTokenKind::TkMinus,
//                     3 => {
//                         reader.eat_while(is_doc_whitespace);
//                         match reader.current_char() {
//                             '@' => {
//                                 reader.bump();
//                                 CppTokenKind::TkDocStart
//                             }
//                             '|' => {
//                                 reader.bump();
//                                 // compact luals
//                                 if matches!(reader.current_char(), '+' | '>') {
//                                     reader.bump();
//                                 }
//                                 CppTokenKind::TkDocContinueOr
//                             }
//                             _ => CppTokenKind::TkDocContinue,
//                         }
//                     }
//                     _ => CppTokenKind::TkDocTrivia,
//                 }
//             }
//             '#' | '@' => {
//                 reader.eat_while(|_| true);
//                 CppTokenKind::TkDocDetail
//             }
//             ch if ch.is_ascii_digit() => {
//                 reader.eat_while(|ch| ch.is_ascii_digit());
//                 CppTokenKind::TkInt
//             }
//             ch if ch == '"' || ch == '\'' => {
//                 reader.bump();
//                 reader.eat_while(|c| c != ch);
//                 if reader.current_char() == ch {
//                     reader.bump();
//                 }

//                 CppTokenKind::TkString
//             }
//             ch if is_name_start(ch) || ch == '`' => {
//                 let (text, str_tpl) = read_doc_name(reader);
//                 if str_tpl {
//                     return CppTokenKind::TkStringTemplateType;
//                 }
//                 to_token_or_name(text)
//             }
//             _ => {
//                 reader.eat_while(|_| true);
//                 CppTokenKind::TkDocTrivia
//             }
//         }
//     }

//     fn lex_field_start(&mut self) -> CppTokenKind {
//         let reader = self.reader.as_mut().unwrap();
//         match reader.current_char() {
//             ch if is_name_start(ch) => {
//                 let (text, _) = read_doc_name(reader);
//                 to_modification_or_name(text)
//             }
//             _ => self.lex_normal(),
//         }
//     }

//     fn lex_description(&mut self) -> CppTokenKind {
//         let reader = self.reader.as_mut().unwrap();
//         match reader.current_char() {
//             ch if is_doc_whitespace(ch) => {
//                 reader.eat_while(is_doc_whitespace);
//                 CppTokenKind::TkWhitespace
//             }
//             '-' if reader.is_start_of_line() => {
//                 let count = reader.consume_char_n_times('-', 3);
//                 match count {
//                     2 => {
//                         if self.origin_token_kind == CppTokenKind::TkLongComment {
//                             reader.bump();
//                             reader.eat_when('=');
//                             reader.bump();

//                             match reader.current_char() {
//                                 '@' => {
//                                     reader.bump();
//                                     CppTokenKind::TkDocLongStart
//                                 }
//                                 _ => CppTokenKind::TkLongCommentStart,
//                             }
//                         } else {
//                             CppTokenKind::TkNormalStart
//                         }
//                     }
//                     3 => {
//                         reader.eat_while(is_doc_whitespace);
//                         match reader.current_char() {
//                             '@' => {
//                                 reader.bump();
//                                 CppTokenKind::TkDocStart
//                             }
//                             '|' => {
//                                 reader.bump();
//                                 // compact luals
//                                 if matches!(reader.current_char(), '+' | '>') {
//                                     reader.bump();
//                                 }

//                                 CppTokenKind::TkDocContinueOr
//                             }
//                             _ => CppTokenKind::TkNormalStart,
//                         }
//                     }
//                     _ => {
//                         reader.eat_while(|_| true);
//                         CppTokenKind::TKDocTriviaStart
//                     }
//                 }
//             }
//             _ => {
//                 reader.eat_while(|_| true);
//                 CppTokenKind::TkDocDetail
//             }
//         }
//     }

//     fn lex_long_description(&mut self) -> CppTokenKind {
//         let reader = self.reader.as_mut().unwrap();
//         let text = reader.get_source_text();
//         let mut chars = text.chars().rev().peekable();
//         let mut trivia_count = 0;
//         while let Some(&ch) = chars.peek() {
//             if ch != ']' && ch != '=' {
//                 break;
//             }
//             chars.next();
//             trivia_count += 1;
//         }
//         let end_pos = text.len() - trivia_count;

//         if reader.get_current_end_pos() < end_pos {
//             while reader.get_current_end_pos() < end_pos {
//                 reader.bump();
//             }
//             CppTokenKind::TkDocDetail
//         } else {
//             reader.eat_while(|_| true);
//             CppTokenKind::TkDocTrivia
//         }
//     }

//     fn lex_trivia(&mut self) -> CppTokenKind {
//         let reader = self.reader.as_mut().unwrap();
//         reader.eat_while(|_| true);
//         CppTokenKind::TkDocTrivia
//     }

//     fn lex_see(&mut self) -> CppTokenKind {
//         let reader = self.reader.as_mut().unwrap();
//         match reader.current_char() {
//             ' ' | '\t' => {
//                 reader.eat_while(is_doc_whitespace);
//                 CppTokenKind::TkWhitespace
//             }
//             _ => {
//                 reader.eat_while(|_| true);
//                 CppTokenKind::TkDocSeeContent
//             }
//         }
//     }

//     fn lex_version(&mut self) -> CppTokenKind {
//         let reader = self.reader.as_mut().unwrap();
//         match reader.current_char() {
//             ',' => {
//                 reader.bump();
//                 CppTokenKind::TkComma
//             }
//             '>' => {
//                 reader.bump();
//                 if reader.current_char() == '=' {
//                     reader.bump();
//                     CppTokenKind::TkGe
//                 } else {
//                     CppTokenKind::TkGt
//                 }
//             }
//             '<' => {
//                 reader.bump();
//                 if reader.current_char() == '=' {
//                     reader.bump();
//                     CppTokenKind::TkLe
//                 } else {
//                     CppTokenKind::TkLt
//                 }
//             }
//             ch if is_doc_whitespace(ch) => {
//                 reader.eat_while(is_doc_whitespace);
//                 CppTokenKind::TkWhitespace
//             }
//             ch if ch.is_ascii_digit() => {
//                 reader.eat_while(|ch| ch.is_ascii_digit() || ch == '.');
//                 CppTokenKind::TkDocVersionNumber
//             }
//             ch if is_name_start(ch) => {
//                 let (text, _) = read_doc_name(reader);
//                 match text {
//                     "JIT" => CppTokenKind::TkDocVersionNumber,
//                     _ => CppTokenKind::TkName,
//                 }
//             }
//             _ => self.lex_normal(),
//         }
//     }

//     fn lex_source(&mut self) -> CppTokenKind {
//         let reader = self.reader.as_mut().unwrap();
//         match reader.current_char() {
//             ch if is_doc_whitespace(ch) => {
//                 reader.eat_while(is_doc_whitespace);
//                 CppTokenKind::TkWhitespace
//             }
//             ch if is_name_start(ch) => {
//                 reader.bump();
//                 reader.eat_while(is_source_continue);
//                 CppTokenKind::TKDocPath
//             }
//             ch if ch == '"' || ch == '\'' => {
//                 reader.bump();
//                 reader.eat_while(|c| c != '\'' && c != '"');
//                 if reader.current_char() == '\'' || reader.current_char() == '"' {
//                     reader.bump();
//                 }

//                 CppTokenKind::TKDocPath
//             }
//             _ => self.lex_normal(),
//         }
//     }

//     fn lex_normal_description(&mut self) -> CppTokenKind {
//         let reader = self.reader.as_mut().unwrap();
//         match reader.current_char() {
//             ch if is_doc_whitespace(ch) => {
//                 reader.eat_while(is_doc_whitespace);
//                 CppTokenKind::TkWhitespace
//             }
//             ch if ch.is_ascii_alphabetic() => {
//                 reader.eat_while(|c| c.is_ascii_alphabetic());
//                 let text = reader.current_saved_text();
//                 match text {
//                     "region" => CppTokenKind::TkDocRegion,
//                     "endregion" => CppTokenKind::TkDocEndRegion,
//                     _ => {
//                         reader.eat_while(|_| true);
//                         CppTokenKind::TkDocDetail
//                     }
//                 }
//             }
//             '-' if reader.is_start_of_line() => {
//                 let count = reader.consume_char_n_times('-', 3);
//                 match count {
//                     2 => {
//                         if self.origin_token_kind == CppTokenKind::TkLongComment {
//                             reader.bump();
//                             reader.eat_when('=');
//                             reader.bump();

//                             match reader.current_char() {
//                                 '@' => {
//                                     reader.bump();
//                                     CppTokenKind::TkDocLongStart
//                                 }
//                                 _ => CppTokenKind::TkLongCommentStart,
//                             }
//                         } else {
//                             CppTokenKind::TkNormalStart
//                         }
//                     }
//                     3 => {
//                         reader.eat_while(is_doc_whitespace);
//                         match reader.current_char() {
//                             '@' => {
//                                 reader.bump();
//                                 CppTokenKind::TkDocStart
//                             }
//                             _ => CppTokenKind::TkNormalStart,
//                         }
//                     }
//                     _ => {
//                         reader.eat_while(|_| true);
//                         CppTokenKind::TKDocTriviaStart
//                     }
//                 }
//             }
//             _ => {
//                 reader.eat_while(|_| true);
//                 CppTokenKind::TkDocDetail
//             }
//         }
//     }
// }

// fn to_tag(text: &str) -> CppTokenKind {
//     match text {
//         "class" => CppTokenKind::TkTagClass,
//         "enum" => CppTokenKind::TkTagEnum,
//         "interface" => CppTokenKind::TkTagInterface,
//         "alias" => CppTokenKind::TkTagAlias,
//         "module" => CppTokenKind::TkTagModule,
//         "field" => CppTokenKind::TkTagField,
//         "type" => CppTokenKind::TkTagType,
//         "param" => CppTokenKind::TkTagParam,
//         "return" => CppTokenKind::TkTagReturn,
//         "return_cast" => CppTokenKind::TkTagReturnCast,
//         "generic" => CppTokenKind::TkTagGeneric,
//         "see" => CppTokenKind::TkTagSee,
//         "overload" => CppTokenKind::TkTagOverload,
//         "async" => CppTokenKind::TkTagAsync,
//         "cast" => CppTokenKind::TkTagCast,
//         "deprecated" => CppTokenKind::TkTagDeprecated,
//         "private" | "protected" | "public" | "package" | "internal" => {
//             CppTokenKind::TkTagVisibility
//         }
//         "readonly" => CppTokenKind::TkTagReadonly,
//         "diagnostic" => CppTokenKind::TkTagDiagnostic,
//         "meta" => CppTokenKind::TkTagMeta,
//         "version" => CppTokenKind::TkTagVersion,
//         "as" => CppTokenKind::TkTagAs,
//         "nodiscard" => CppTokenKind::TkTagNodiscard,
//         "operator" => CppTokenKind::TkTagOperator,
//         "mapping" => CppTokenKind::TkTagMapping,
//         "namespace" => CppTokenKind::TkTagNamespace,
//         "using" => CppTokenKind::TkTagUsing,
//         "source" => CppTokenKind::TkTagSource,
//         _ => CppTokenKind::TkTagOther,
//     }
// }

// fn to_modification_or_name(text: &str) -> CppTokenKind {
//     match text {
//         "private" | "protected" | "public" | "package" => CppTokenKind::TkDocVisibility,
//         "readonly" => CppTokenKind::TkDocReadonly,
//         _ => CppTokenKind::TkName,
//     }
// }

// fn to_token_or_name(text: &str) -> CppTokenKind {
//     match text {
//         "true" => CppTokenKind::TkTrue,
//         "false" => CppTokenKind::TkFalse,
//         "keyof" => CppTokenKind::TkDocKeyOf,
//         "extends" => CppTokenKind::TkDocExtends,
//         "as" => CppTokenKind::TkDocAs,
//         "and" => CppTokenKind::TkAnd,
//         "or" => CppTokenKind::TkOr,
//         _ => CppTokenKind::TkName,
//     }
// }

// fn is_doc_whitespace(ch: char) -> bool {
//     ch == ' ' || ch == '\t' || ch == '\r' || ch == '\n'
// }

// fn read_doc_name<'a>(reader: &'a mut Reader) -> (&'a str, bool /* str tpl */) {
//     reader.bump();
//     let mut str_tpl = false;
//     while !reader.is_eof() {
//         match reader.current_char() {
//             ch if is_name_continue(ch) => {
//                 reader.bump();
//             }
//             // donot continue if next char is '.' or '-' or '*' or '`'
//             '.' | '-' | '*' => {
//                 let next = reader.next_char();
//                 if next == '.' || next == '-' || next == '*' {
//                     break;
//                 }

//                 reader.bump();
//             }
//             '`' => {
//                 str_tpl = true;
//                 reader.bump();
//             }
//             _ => break,
//         }
//     }

//     (reader.current_saved_text(), str_tpl)
// }

// fn is_source_continue(ch: char) -> bool {
//     is_name_continue(ch)
//         || ch == '.'
//         || ch == '-'
//         || ch == '/'
//         || ch == ' '
//         || ch == ':'
//         || ch == '#'
//         || ch == '\\'
// }

// #[cfg(test)]
// mod tests {
//     use crate::kind::CppTokenKind;
//     use crate::lexer::LuaDocLexer;
//     use crate::text::SourceRange;

//     #[test]
//     fn test_lex() {
//         let text = r#"-- comment"#;
//         let mut lexer = LuaDocLexer::new(text);
//         lexer.reset(CppTokenKind::TkShortComment, SourceRange::new(0, 10));
//         let k1 = lexer.lex();
//         assert_eq!(k1, CppTokenKind::TkNormalStart);
//         let k2 = lexer.lex();
//         let range = lexer.current_token_range();
//         let text = lexer.origin_text[range.start_offset..range.end_offset()].to_string();
//         assert_eq!(text, " comment");
//         assert_eq!(k2, CppTokenKind::TkDocTrivia);
//     }
// }
