//! **What a token *is*, where the token stream alone cannot say** — read from the syntax tree.
//!
//! A formatter that decides `<` by "the previous token looks like a type" gets `vector<int>` wrong the first time a
//! file writes `a < b`, and the same is true of `*` (`int *p` vs `a * b`) and of `{` (`void f() {` vs `T{1}`).
//! clang-format answers those by a trial parse of its own; this crate has a real parser, so it asks it: the tree
//! marks the token offsets that belong to a template argument list, to a pointer/reference declarator, and to a
//! block or an initializer list.
//!
//! The tree is used for **classification only** — never for the layout. A file that does not parse still formats
//! (the parser is lossless and total), it just falls back to the token-level heuristics.

use std::collections::HashSet;

use cpp_parser::{CppParser, CppSyntaxKind, CppTokenKind, ParserConfig};

/// The token offsets (byte offsets, as the formatter sees them) whose role the tree settles.
#[derive(Debug, Default, Clone)]
pub struct Roles {
    /// `<` and `>` of a template argument/parameter list: no space on either side.
    pub template_angle: HashSet<usize>,
    /// `*`, `&`, `&&` written as a declarator (`int *p`), not as an operator.
    pub declarator_mark: HashSet<usize>,
    /// `{` that opens a **block** (a compound statement, a class/enum body, a namespace body).
    pub block_brace: HashSet<usize>,
    /// `{` that opens an **initializer list** (`T{1}`, `= {1, 2}`).
    pub init_brace: HashSet<usize>,
}

impl Roles {
    pub fn is_template_angle(&self, offset: usize) -> bool {
        self.template_angle.contains(&offset)
    }

    pub fn is_declarator(&self, offset: usize) -> bool {
        self.declarator_mark.contains(&offset)
    }
}

/// Read the roles of a file. A file that does not parse yields whatever the tree did contain — every mark is an
/// *extra* fact, and the absence of one only sends the layout back to its heuristic.
pub fn of(source: &str) -> Roles {
    let mut roles = Roles::default();
    let tree = CppParser::parse(source, ParserConfig::default());
    let root = tree.get_red_root();

    for node in root.descendants() {
        let kind = CppSyntaxKind::from(node.kind());
        match kind {
            CppSyntaxKind::TemplateArgumentList | CppSyntaxKind::TemplateParameterList => {
                for token in node
                    .descendants_with_tokens()
                    .filter_map(|it| it.into_token())
                {
                    let token_kind = CppTokenKind::from(token.kind());
                    if matches!(token_kind, CppTokenKind::Less | CppTokenKind::Greater) {
                        roles.template_angle.insert(token_range_start(&token));
                    }
                }
            }
            CppSyntaxKind::PointerType
            | CppSyntaxKind::ReferenceType
            | CppSyntaxKind::RValueReferenceType => {
                for token in node
                    .descendants_with_tokens()
                    .filter_map(|it| it.into_token())
                {
                    // `&&` is the lexer's `LogicalAnd`, not `Ampersand`: a formatter that forgets it writes
                    // `T && x` where the file had `T&& x`.
                    if matches!(
                        CppTokenKind::from(token.kind()),
                        CppTokenKind::Star | CppTokenKind::Ampersand | CppTokenKind::LogicalAnd
                    ) {
                        roles.declarator_mark.insert(token_range_start(&token));
                    }
                }
            }
            CppSyntaxKind::CompoundStat
            | CppSyntaxKind::ClassBody
            | CppSyntaxKind::EnumDef
            | CppSyntaxKind::NamespaceDecl => {
                if let Some(brace) = first_brace(&node) {
                    roles.block_brace.insert(brace);
                }
            }
            CppSyntaxKind::InitListExpr => {
                if let Some(brace) = first_brace(&node) {
                    roles.init_brace.insert(brace);
                }
            }
            _ => {}
        }
    }

    roles
}

fn token_range_start(token: &cpp_parser::CppSyntaxToken) -> usize {
    u32::from(token.text_range().start()) as usize
}

/// The offset of the first `{` a node holds, if it holds one before any nested body.
fn first_brace(node: &cpp_parser::CppSyntaxNode) -> Option<usize> {
    node.descendants_with_tokens()
        .filter_map(|it| it.into_token())
        .find(|token| CppTokenKind::from(token.kind()) == CppTokenKind::LeftBrace)
        .map(|token| token_range_start(&token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_template_argument_list_marks_its_angles() {
        let roles = of("std::vector<int> v;\n");
        let less = "std::vector".len();
        assert!(roles.is_template_angle(less), "the `<` of `vector<int>`");
    }

    #[test]
    fn a_comparison_is_not_a_template_angle() {
        let roles = of("int a = b < c;\n");
        let less = "int a = b ".len();
        assert!(!roles.is_template_angle(less));
    }

    #[test]
    fn a_declarator_star_is_marked_and_a_product_is_not() {
        let roles = of("int *p = a * b;\n");
        let declarator = "int ".len();
        let product = "int *p = a ".len();
        assert!(roles.is_declarator(declarator));
        assert!(!roles.is_declarator(product));
    }

    #[test]
    fn a_function_body_brace_is_a_block_and_a_braced_init_is_not() {
        let roles = of("void f() {}\nint x = {1};\n");
        assert!(roles.block_brace.contains(&"void f() ".len()));
        assert!(roles.init_brace.contains(&"void f() {}\nint x = ".len()));
    }
}
