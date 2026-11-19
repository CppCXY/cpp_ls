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
    /// **What each block `{` opens** — a function body, a class, a namespace, a control statement — for the
    /// `BreakBeforeBraces` table. Absent when the tree could not say, and then the brace keeps the author's line.
    pub brace_kind: std::collections::HashMap<usize, BraceKind>,
    /// **The name of a namespace, keyed by the offset of its opening brace** — what `FixNamespaceComments` writes
    /// after the closing brace. An anonymous namespace has no name and is absent from this map.
    pub namespace_names: std::collections::HashMap<usize, String>,
}

/// What a block brace opens, in the vocabulary `BraceWrapping` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BraceKind {
    Function,
    Class,
    Struct,
    Union,
    Enum,
    Namespace,
    /// `if`, `for`, `while`, `do` and `switch` bodies — one option (`BraceWrapping.AfterControlStatement`) governs
    /// all of them in clang-format, and `Always`/`Never`/`MultiLine` is what it says.
    Control,
    Try,
    Catch,
    Lambda,
    ExternBlock,
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
                    // **The name, for `FixNamespaceComments`.** Read from the tokens the declaration holds: the word
                    // after `namespace`, when it is an identifier. `namespace ns::inner {` records `inner`, which is
                    // the name clang-format writes in the comment (`} // namespace inner`).
                    if let Some(name) = namespace_name(&node) {
                        roles.namespace_names.insert(brace, name);
                    }
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

    // **What each block brace opens**, which is what `BreakBeforeBraces` and its `BraceWrapping` table are about:
    // `AfterFunction` is not `AfterClass`, and Microsoft's style wraps one and attaches the other. Read from the
    // tree, and from the **token's own ancestors**: the token before a `{` cannot tell a lambda's body (`]` or `)`)
    // from a function's, `else`'s from an `if`'s, or a `switch`'s body — whose braces hang directly off `SwitchStat`,
    // not off a `CompoundStat` — from any of them.
    for element in root.descendants_with_tokens() {
        let Some(token) = element.into_token() else {
            continue;
        };
        if CppTokenKind::from(token.kind()) != CppTokenKind::LeftBrace {
            continue;
        }
        let brace = token_range_start(&token);
        // **A braced initializer is not a block**, and the token before the brace is what tells the two apart where
        // the tree cannot: `= {`, `return {`, `f({` and `{1, 2}` are values, while `) {` is a body — and a
        // **constructor's initializer list** is the case that needs the exception, because the tree can name an
        // `InitListExpr` that covers `: x_(x) { … }` and `first_brace` then points at the body's brace.
        let previous = previous_non_trivia(&token);
        let introduced = matches!(
            previous,
            Some(
                CppTokenKind::Assign
                    | CppTokenKind::ReturnKeyword
                    | CppTokenKind::Comma
                    | CppTokenKind::LeftParen
                    | CppTokenKind::LeftBracket
            )
        );
        let named_an_initializer = roles.init_brace.contains(&brace)
            && !matches!(
                previous,
                Some(CppTokenKind::RightParen | CppTokenKind::RightBracket)
            );
        if introduced || named_an_initializer {
            continue;
        }
        let Some(parent) = token.parent() else {
            continue;
        };
        // **An out-of-line definition is not wrapped in a `FunctionDef`** — this grammar gives
        // `A::A(int x) : x_(x) { … }` a `Declaration` whose child is the body, with no function node at all (the same
        // shape `breaks.rs` documents for `int count() { }` at file scope). So when no construct owns the body, the
        // token before the `{` decides: `)` is a function's parameter list (every control statement has its own
        // ancestor and was answered above), `]` is a lambda's capture list.
        let brace_kind = body_kind_of(&parent).or(match previous {
            Some(CppTokenKind::RightParen) => Some(BraceKind::Function),
            Some(CppTokenKind::RightBracket) => Some(BraceKind::Lambda),
            _ => None,
        });
        if let Some(brace_kind) = brace_kind {
            roles.brace_kind.insert(brace, brace_kind);
        }
    }

    roles
}

/// **What the `{` under this node opens** — the nearest enclosing construct the wrapping table names.
///
/// The walk climbs because the parser nests as the grammar does: a function's body brace sits in a `CompoundStat`
/// whose ancestors reach the declaration, an `if`'s body's reaches the `IfStat`, a `switch`'s body brace is a direct
/// child of `SwitchStat`, and a class's is a `ClassBody` inside `ClassDef`. The first construct on the list wins.
///
/// **A nested compound statement is not a function body**, and `compounds` is what says so: the body's own brace
/// reaches the `FunctionDef` with one `CompoundStat` between them, a bare block inside that body with two — and the
/// wrapping rule for a bare block is the control one (attach), not the function one (wrap in Microsoft's style).
fn body_kind_of(open: &cpp_parser::CppSyntaxNode) -> Option<BraceKind> {
    let mut compounds = 0usize;
    for ancestor in open.ancestors() {
        match CppSyntaxKind::from(ancestor.kind()) {
            CppSyntaxKind::CompoundStat => compounds += 1,
            CppSyntaxKind::FunctionDef => {
                return Some(if compounds <= 1 {
                    BraceKind::Function
                } else {
                    BraceKind::Control
                });
            }
            CppSyntaxKind::LambdaExpr => return Some(BraceKind::Lambda),
            CppSyntaxKind::IfStat
            | CppSyntaxKind::ForStat
            | CppSyntaxKind::WhileStat
            | CppSyntaxKind::DoStat
            | CppSyntaxKind::SwitchStat => return Some(BraceKind::Control),
            CppSyntaxKind::TryStat => return Some(BraceKind::Try),
            CppSyntaxKind::CatchStat | CppSyntaxKind::CatchHandler => {
                return Some(BraceKind::Catch);
            }
            CppSyntaxKind::ExternSpec => return Some(BraceKind::ExternBlock),
            CppSyntaxKind::NamespaceDecl => return Some(BraceKind::Namespace),
            CppSyntaxKind::ClassBody => {
                return match ancestor
                    .parent()
                    .map(|parent| CppSyntaxKind::from(parent.kind()))
                {
                    Some(CppSyntaxKind::ClassDef) => Some(BraceKind::Class),
                    Some(CppSyntaxKind::StructDef) => Some(BraceKind::Struct),
                    Some(CppSyntaxKind::UnionDef) => Some(BraceKind::Union),
                    Some(CppSyntaxKind::EnumDef) => Some(BraceKind::Enum),
                    _ => None,
                };
            }
            _ => {}
        }
    }
    None
}

/// **The name a namespace declaration gives itself**, or `None` for an anonymous one.
fn namespace_name(node: &cpp_parser::CppSyntaxNode) -> Option<String> {
    let mut after_the_keyword = false;
    for element in node.descendants_with_tokens() {
        let Some(token) = element.into_token() else {
            continue;
        };
        let kind = CppTokenKind::from(token.kind());
        if kind == CppTokenKind::NamespaceKeyword {
            after_the_keyword = true;
            continue;
        }
        if !after_the_keyword || cpp_parser::is_trivia(kind) {
            continue;
        }
        return match kind {
            CppTokenKind::Identifier => Some(token.text().to_string()),
            CppTokenKind::LeftBrace => None,
            _ => None,
        };
    }
    None
}

/// The kind of the token before this one that is not trivia — `)`, `=`, a name.
fn previous_non_trivia(token: &cpp_parser::CppSyntaxToken) -> Option<CppTokenKind> {
    let mut previous = token.prev_token();
    while let Some(token) = previous {
        let kind = CppTokenKind::from(token.kind());
        if !cpp_parser::is_trivia(kind) {
            return Some(kind);
        }
        previous = token.prev_token();
    }
    None
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
    fn a_namespace_brace_knows_its_name() {
        let source = "namespace ns {\nint x;\n}\n";
        let roles = of(source);
        let brace = source.find('{').expect("the body is in the fixture");
        assert_eq!(roles.brace_kind.get(&brace), Some(&BraceKind::Namespace));
        assert_eq!(roles.namespace_names.get(&brace), Some(&"ns".to_string()));
    }
    #[test]
    fn a_constructor_body_is_a_function_brace() {
        let source = "A::A(int x) : x_(x) {\n}\n";
        let roles = of(source);
        let brace = source.find('{').expect("the body is in the fixture");
        assert_eq!(roles.brace_kind.get(&brace), Some(&BraceKind::Function));
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
