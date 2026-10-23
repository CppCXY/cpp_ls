//! **Where the spaces go between two tokens that stay on the same line.**
//!
//! This is the first half of the formatter (P0a of `docs/cpp-format.md` §十): line structure and indentation are left
//! exactly as the input had them, and only the whitespace *inside* a line is rewritten. The second half (breaks and
//! indent) is `layout.rs`, which will call into this module for the same-line decisions.
//!
//! # How a decision is made
//!
//! Three sources, in this order:
//!
//! ```text
//! 1. the role the syntax tree marked (template angle, declarator, block vs initializer brace) — see `roles.rs`
//! 2. the token kinds on either side (`,` never has a space before it)
//! 3. the style (SpaceBeforeParens, PointerAlignment, SpaceBeforeCpp11BracedList, SpacesIn*)
//! ```
//!
//! A question is asked about a *gap*, not about a token, so both neighbours are in hand and there is no
//! "pending space" state to keep.

use crate::config::Style;
use crate::options::StyleOption;
use crate::roles::Roles;
use crate::tokens::Stream;
use cpp_parser::CppTokenKind;

/// The number of spaces between `tokens[index - 1]` and `tokens[index]`.
pub fn between(stream: &Stream<'_>, index: usize, roles: &Roles, style: &Style) -> usize {
    use CppTokenKind::*;
    if index == 0 {
        return 0;
    }
    let current = stream.tokens[index];
    let previous = stream.tokens[index - 1];
    let offset = current.start;

    // **Two operator characters the source wrote together stay together.** `cpp_parser`s lexer splits `>=` into
    // `Greater` + `Assign` (the parser re-joins them contextually), so a spacing rule that looks only at token
    // *kinds* sees two binary operators and puts a space between them: `if (end > = start)`. Same for the runs a
    // declarator makes — `char **argv`, `T *&value`. The source is the evidence: a gap with no bytes in it was not
    // a place anyone wrote a space, and every one of these pairs is a *single* operator to clang-format, so keeping
    // it joined is both the conservative and the correct answer.
    //
    // Measured on the project corpus: this class alone is 3 files that the audit had to skip as "the token streams
    // differ", plus the `> =` lines in `LuaSyntaxNode.cpp` and `LuaSource.cpp`.
    //
    // The exception is a mark that belongs to a declarator: `const std::vector<int> &v` was written `>&v` in the source,
    // and gluing it is right for the *characters* but wrong for the *reference*, so the pointer rule below has to see it.
    let declarator_after_a_template = previous.kind == Greater && roles.is_template_angle(previous.start);
    if stream.gaps[index].is_empty()
        && !declarator_after_a_template
        && is_operator_punctuation(previous.kind)
        && is_operator_punctuation(current.kind)
    {
        return 0;
    }

    // **`->` is two different tokens in C++**, and the parenthesis before it tells them apart: a member access
    // (`it->second`, `find(text)->second`, glued) and a lambda's trailing return type (`[](int a) -> int`, one space
    // on each side). "Trailing" means the `)` closes a *lambda* introducer's parameter list — the `(` that follows a
    // `]` — which is what clang-format 22.1.3 writes and what keeps `find(text)->second` glued (measured: without the
    // `]` test the corpus lost a file to `return LuaReserved.find(text) -> second;`).
    if current.kind == Arrow || previous.kind == Arrow {
        // The token on the *other* side of the arrow from the gap being asked about.
        let before_arrow = if current.kind == Arrow { index - 1 } else { index - 2 };
        // A specifier may sit between the parameter list and the arrow: `[]() noexcept -> int`.
        if matches!(
            stream.tokens[before_arrow].text,
            "noexcept" | "const" | "mutable" | "override" | "final" | "constexpr"
        ) {
            return 1;
        }
        if before_arrow < stream.tokens.len() && stream.tokens[before_arrow].kind == RightParen {
            let mut depth = 0isize;
            let mut opener: Option<usize> = Option::None;
            for cursor in (0..before_arrow).rev() {
                match stream.tokens[cursor].kind {
                    RightParen => depth += 1,
                    LeftParen => {
                        if depth == 0 {
                            opener = Some(cursor);
                            break;
                        }
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            if opener.is_some_and(|at| at > 0 && stream.tokens[at - 1].kind == RightBracket) {
                return 1;
            }
        }
    }

    // **A cast takes no space after its parenthesis**: `(void)x`, `(int)y`. clang-format writes them glued
    // (measured on `(void)i;`, which this wrote as `(void) i;`). The parenthesis holds a type name and nothing else,
    // which is what separates a cast from a call followed by a value.
    if previous.kind == RightParen && is_a_cast_expression(stream, index - 1) {
        return 0;
    }

    // 1. Tokens that are glued to what they follow.
    match current.kind {
        Comma | Semicolon | RightParen | RightBracket | Dot | Arrow | DotStar | ArrowStar => {
            return 0;
        }
        Scope => return 0,
        _ => {}
    }
    match previous.kind {
        LeftParen => {
            // **`SpacesInParentheses` became `SpacesInParens` in clang-format 17**, and the preset table now comes
            // from 19.1.7's dump, where the old key does not exist at all. The value is an enum (`Never` by default)
            // rather than a flag, so the question is what it is *not*: `Never` is the only spelling that puts nothing
            // inside the parentheses.
            return if style.text(StyleOption::SpacesInParens) == "Never" {
                0
            } else {
                1
            };
        }
        LeftBracket => {
            return if style.flag(StyleOption::SpacesInSquareBrackets) {
                1
            } else {
                0
            };
        }
        Scope | Dot | Arrow | DotStar | ArrowStar => return 0,
        Comma | Semicolon => return 1,
        LogicalNot | Tilde => return 0,
        _ => {}
    }

    // **An ellipsis glues to what it packs**: `typename... Args`, `Args &&...args`, `sizeof...(args)`.
    // Measured on the variadic snippet, where we wrote `typename ... Args` and `sizeof... (args)`.
    if current.kind == Ellipsis {
        return match previous.kind {
            Comma | LeftParen => 1,
            _ => 0,
        };
    }
    if previous.kind == Ellipsis {
        return 0;
    }

    // 2. Template angle brackets: `vector<int>`, `map<int, string>` — no space on either side, and the comma inside
    //    still gets one after it (handled above). The gap *after* a closing `>` is **not** glued (`vector<int> v`):
    //    only a `>` as the current token, or a `<` as the previous one, belongs to the brackets.
    if roles.is_template_angle(offset) {
        // `template <class T>` keeps the space after the `template` keyword, unless `SpaceAfterTemplateKeyword`
        // says otherwise — the project's own config sets it to false, and that one key is **1 882** gaps on the
        // corpus.
        if previous.kind == TemplateKeyword {
            return if style.flag(StyleOption::SpaceAfterTemplateKeyword) {
                1
            } else {
                0
            };
        }
        return 0;
    }
    if roles.is_template_angle(previous.start) && previous.kind == Less {
        return 0;
    }
    // A cast's angle brackets: `static_cast<int>(x)` is not a template *argument list* in the tree, and a formatter
    // that misses it writes `static_cast < int >`. Measured: 12 of the corpus's 123 remaining spacing gaps.
    if current.kind == Less && is_a_cast_keyword(previous.kind) {
        return 0;
    }
    if previous.kind == Less && index >= 2 && is_a_cast_keyword(stream.tokens[index - 2].kind) {
        return 0;
    }
    if current.kind == Greater && angle_follows_a_cast(stream, index) {
        return 0;
    }

    // **After `operator` the punctuator is glued**: `operator=(T &&)`, `operator==(const T &)`, `operator[]`.
    // Measured against 22.1.3: the space before `=` turned `operator = (` into something clang-format never writes.
    if matches!(previous.kind, CppTokenKind::OperatorKeyword) {
        return 0;
    }
    // The `(` of an operator function is glued to the punctuator (`operator=(`), which is the token after the
    // `operator` keyword — not a call parenthesis.
    if current.kind == CppTokenKind::LeftParen
        && index >= 2
        && stream.tokens[index - 2].kind == CppTokenKind::OperatorKeyword
    {
        return 0;
    }
    // 3. An opening parenthesis: a control statement's parenthesis has a space before it (`if (x)`), a call's does
    //    not (`f(x)`), and `SpaceBeforeParens` decides whether that rule applies at all.
    if current.kind == LeftParen {
        return if a_space_before_a_paren(previous.kind, previous.text, style) {
            1
        } else {
            0
        };
    }
    if current.kind == LeftBracket {
        // **Not always glued**: `auto f = [&](int a)` needs the space after `=`, and gluing it wrote `auto f =[&]`,
        // which clang-format never produces (found by feeding its own output back through us). A `[` after an
        // operator, a keyword or an opening bracket keeps that space; everything else (`a[i]`, `[[attr]]`) glues.
        // **A declarator or an attribute starts with a space**: `auto [a, b] = p`, `struct [[nodiscard]] S`,
        // `class [[attr]] C`. A subscript (`a[i]`) and an attribute on a name (`int x [[attr]]`) do not.
        if matches!(previous.kind, StructKeyword | ClassKeyword | UnionKeyword | EnumKeyword | AutoKeyword) {
            return 1;
        }
        if previous.kind == Identifier && matches!(previous.text, "auto" | "struct" | "class" | "union" | "enum") {
            return 1;
        }
        return match previous.kind {
            Assign | Equal | ReturnKeyword | Comma | LeftParen | LeftBrace => 1,
            _ => 0,
        };
    }

    // 4. Braces.
    if current.kind == LeftBrace {
        // A block brace is always preceded by a space (`void f() {`, `else {`), and a `)` before a `{` can only be a
        // body — a braced initializer never follows a parenthesis — so the tree is consulted but not required.
        if roles.block_brace.contains(&offset) || previous.kind == RightParen {
            return 1;
        }
        // An initializer list: `= {`, `, {`, `({` read as a value; `T{` as a construction.
        return match previous.kind {
            Assign | Comma | LeftParen | LeftBracket | ReturnKeyword | Equal => 1,
            _ => {
                if style.flag(StyleOption::SpaceBeforeCpp11BracedList) {
                    1
                } else {
                    0
                }
            }
        };
    }
    /// Does the `}` at `index` close a `class`/`struct`/`enum`/`union` body written on one line?
    ///
    /// The roles classify such a brace as an *initializer* (the body is a list of declarations), which is right for the
    /// layout but wrong for the space before the closer: clang-format writes `enum class E { A, B };`, and the roles`
    /// answer would produce `{ A, B};`.
    fn closes_a_declaration_body(stream: &crate::tokens::Stream<'_>, index: usize) -> bool {
        let mut depth = 0isize;
        let mut opener: Option<usize> = Option::None;
        for cursor in (0..index).rev() {
            match stream.tokens[cursor].kind {
                CppTokenKind::RightBrace => depth += 1,
                CppTokenKind::LeftBrace => {
                    if depth == 0 {
                        opener = Some(cursor);
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        let Some(opener) = opener else {
            return false;
        };
        let mut cursor = opener;
        while cursor > 0 {
            cursor -= 1;
            match stream.tokens[cursor].kind {
                CppTokenKind::ClassKeyword
                | CppTokenKind::StructKeyword
                | CppTokenKind::EnumKeyword
                | CppTokenKind::UnionKeyword => return true,
                CppTokenKind::Semicolon | CppTokenKind::LeftBrace | CppTokenKind::RightBrace => {
                    return false;
                }
                _ => {}
            }
        }
        false
    }

    if current.kind == RightBrace {
        // A block kept on one line (`if (x) { return; }`) has a space inside it; an initializer list does not
        // (`{1, 2}`), and an empty block is `{}`.
        // A block kept on one line (`if (x) { return; }`) has a space inside it; an initializer list does not
        // (`{1, 2}`), and an empty block is `{}`. **An enumeration or class body counts as a block here** even when
        // the roles call it an initializer list: clang-format writes `enum class E { A, B };` with the space
        // (measured against 22.1.3), and that single-line case is what the 80-column stability check still reports.
        if previous.kind != LeftBrace
            && (roles.block_brace.contains(&offset) || closes_a_declaration_body(stream, index))
        {
            return 1;
        }
        return 0;
    }
    if previous.kind == LeftBrace {
        // The other side of the same rule, and the same exception: a `{` that follows a `)` opens a body
        // (`requires(T t) { t.size(); }`, `f() { g(); }`), and the roles call some of those list initializers, so the
        // parenthesis decides first.
        let body = index >= 2 && stream.tokens[index - 2].kind == CppTokenKind::RightParen;
        if (roles.block_brace.contains(&previous.start) || body) && current.kind != RightBrace {
            return 1;
        }
        return 0;
    }
    // A postfix `++`/`--` is glued to the value before it; a prefix one is glued to the operand after it.
    if matches!(
        current.kind,
        CppTokenKind::PlusPlus | CppTokenKind::MinusMinus
    ) && is_a_value(previous.kind)
    {
        return 0;
    }
    if matches!(
        previous.kind,
        CppTokenKind::PlusPlus | CppTokenKind::MinusMinus
    ) && !is_a_value(previous_before_previous(stream, index))
    {
        return 0;
    }
    // A parameter pack: `T... args` — the ellipsis is glued to the type before it.
    if current.kind == CppTokenKind::Ellipsis && is_a_value(previous.kind) {
        return 0;
    }

    // 5. `*` and `&`: a declarator follows `PointerAlignment`, an operator is a binary operator.
    //
    // **After a template argument list the mark is part of the declarator even when the tree does not say so**:
    // `const std::vector<int> &v` is a reference, and the parser leaves `&` unannotated there (measured: we wrote
    // `std::vector<int>&v`). A `>` that closes a template argument list is the evidence.
    let after_a_template = previous.kind == Greater && roles.is_template_angle(previous.start);
    let mark_after_a_template = index >= 2
        && stream.tokens[index - 2].kind == Greater
        && roles.is_template_angle(stream.tokens[index - 2].start);
    if is_a_mark(current.kind) && (roles.is_declarator(offset) || after_a_template) {
        return match style.text(StyleOption::PointerAlignment) {
            "Left" => 0,
            "Middle" => 1,
            _ => 1,
        };
    }
    if is_a_mark(previous.kind) && (roles.is_declarator(previous.start) || mark_after_a_template) {
        return match style.text(StyleOption::PointerAlignment) {
            "Left" => 1,
            "Middle" => 1,
            _ => 0,
        };
    }

    // 6. A binary operator has a space on both sides; a unary one is glued to its operand.
    if is_a_binary_operator(current.kind) {
        if is_prefix_operator(stream, index, roles) {
            return 1;
        }
        return 1;
    }
    if is_a_binary_operator(previous.kind) {
        if is_prefix_operator(stream, index - 1, roles) {
            return 0;
        }
        return 1;
    }
    if current.kind == Question || previous.kind == Question {
        return 1;
    }

    // 7. A colon. An access specifier (`public:`) and a case label (`case 1:`) take no space before them; an
    //    inheritance/base clause (`class X : public Y`), a constructor initializer (`Foo() : a(1)`) and a bitfield
    //    (`int x : 3`) do. Measured: getting this backwards put `class X: public Y` in front of every class in the
    //    corpus.
    if current.kind == Colon {
        if is_a_label_colon(stream, index) {
            return 0;
        }
        // A range-based `for (auto &x : v)` has its own key, and the project's config sets it to false.
        if in_a_for_head(stream, index) {
            return if style.flag(StyleOption::SpaceBeforeRangeBasedForLoopColon) {
                1
            } else {
                0
            };
        }
        // An inheritance clause, a constructor initializer and a bitfield all keep the space; their keys
        // (`SpaceBeforeInheritanceColon`, …) default to true and the corpus does not turn them off.
        return 1;
    }
    if previous.kind == Colon {
        return 1;
    }

    // 8. Everything else: one space (two names side by side are two tokens that need separating).
    1
}

fn is_a_value(kind: CppTokenKind) -> bool {
    use CppTokenKind::*;
    matches!(
        kind,
        Identifier
            | IntegerLiteral
            | FloatingLiteral
            | CharLiteral
            | StringLiteral
            | NullptrLiteral
            | ThisKeyword
            | RightParen
            | RightBracket
    )
}

/// Characters that can be part of a longer operator, or of a declarator`s run of `*`/`&`.
fn is_operator_punctuation(kind: CppTokenKind) -> bool {
    use CppTokenKind::*;
    matches!(
        kind,
        Assign
            | Equal
            | NotEqual
            | Less
            | Greater
            | LessEqual
            | GreaterEqual
            | Plus
            | Minus
            | Star
            | Slash
            | Percent
            | Ampersand
            | Pipe
            | Caret
            | Tilde
            | LogicalAnd
            | LogicalOr
            | LogicalNot
            | Question
            | Colon
            | LeftShift
            | RightShift
            | Spaceship
            | PlusAssign
            | MinusAssign
            | StarAssign
            | SlashAssign
            | PercentAssign
            | AmpersandAssign
            | PipeAssign
            | CaretAssign
            | LeftShiftAssign
            | RightShiftAssign
    )
}

fn is_a_mark(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Star | CppTokenKind::Ampersand | CppTokenKind::LogicalAnd
    )
}

/// Does the `)` at `close` end a cast — a parenthesis holding one type name and nothing else?
///
/// `(void)x`, `(int)y`, `(const char *)p`: the opener is found by matching, and the inside must be one of the built-in
/// type names (the lexer has no semantic information, so a user type inside parentheses is left alone).
fn is_a_cast_expression(stream: &Stream<'_>, close: usize) -> bool {
    let mut depth = 0isize;
    let mut opener: Option<usize> = Option::None;
    for cursor in (0..close).rev() {
        match stream.tokens[cursor].kind {
            CppTokenKind::RightParen => depth += 1,
            CppTokenKind::LeftParen => {
                if depth == 0 {
                    opener = Some(cursor);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    let Some(opener) = opener else {
        return false;
    };
    let inside = &stream.tokens[opener + 1..close];
    !inside.is_empty()
        && inside.iter().all(|token| {
            matches!(
                token.kind,
                CppTokenKind::VoidKeyword
                    | CppTokenKind::IntKeyword
                    | CppTokenKind::CharKeyword
                    | CppTokenKind::FloatKeyword
                    | CppTokenKind::DoubleKeyword
                    | CppTokenKind::BoolLiteral
                    | CppTokenKind::LongKeyword
                    | CppTokenKind::ShortKeyword
                    | CppTokenKind::UnsignedKeyword
                    | CppTokenKind::SignedKeyword
                    | CppTokenKind::Star
                    | CppTokenKind::Ampersand
                    | CppTokenKind::ConstKeyword
            )
        })
}

fn a_space_before_a_paren(previous: CppTokenKind, previous_text: &str, style: &Style) -> bool {
    use CppTokenKind::*;
    // **These names glue to their parenthesis** the way a call does: `static_assert(x)`, `sizeof(x)`,
    // `alignas(8)`, `decltype(x)`, `noexcept(true)`, `requires(C c)`. clang-format writes all of them without the
    // space (measured against 22.1.3), and they are identifiers to this lexer, not keywords.
    if matches!(
        previous_text,
        "static_assert" | "_Static_assert" | "alignas" | "alignof" | "decltype" | "sizeof" | "noexcept"
            | "typeid" | "requires"
    ) {
        return false;
    }
    use CppTokenKind::*;
    match style.text(StyleOption::SpaceBeforeParens) {
        "Always" => true,
        "Never" => false,
        // `ControlStatements`: a space after the keywords that take a condition, never after a name.
        //
        // The fallback matters as much: a `(` that follows an *operator* is a parenthesised expression
        // (`x = (a + b)`), not a call, and gluing it produced `x = (a + b)` → `x =(a + b)` — 411 gaps on the
        // corpus before this branch existed.
        _ => {
            if matches!(
                previous,
                IfKeyword
                    | ForKeyword
                    | WhileKeyword
                    | SwitchKeyword
                    | CatchKeyword
                    | DoKeyword
                    | ReturnKeyword
            ) {
                true
            } else {
                // A call's `(` follows a name, a `)`, a `]`, a `>` or `::` and is glued to it; anything else gets a
                // space. (`x = (a + b)` is a parenthesised expression, not a call.)
                let a_call = matches!(
                    previous,
                    Identifier
                        | RightParen
                        | RightBracket
                        | Greater
                        | Scope
                        | ThisKeyword
                        | OperatorKeyword
                );
                // A *type* name before a `(` is a function type or a cast (`std::function<bool(T)>`, `int(x)`) and
                // is glued the same way a call is; so is `sizeof`'s operand. Measured: 371 + 336 gaps.
                let a_type = matches!(
                    previous,
                    // **`bool` is a `BoolLiteral` in this lexer**, not a keyword: `cpp_lexer` maps `"bool"` to the same
                    // token as `true`/`false`, so a `bool` before `(` fell through to "not a call, not a type" and got
                    // a space — `std::function<bool (T)>` in two headers. A `true`/`false` before `(` is not C++ at all,
                    // so treating the literal kind as a type name here costs nothing.
                    BoolLiteral
                        | IntKeyword
                        | CharKeyword
                        | LongKeyword
                        | ShortKeyword
                        | DoubleKeyword
                        | FloatKeyword
                        | VoidKeyword
                        | UnsignedKeyword
                        | SignedKeyword
                        | AutoKeyword
                        | SizeofKeyword
                        | AlignofKeyword
                        | DecltypeKeyword
                        | TypeidKeyword
                        | NoexceptKeyword
                );
                !(a_call || a_type)
            }
        }
    }
}

/// The token before `tokens[index - 1]` (for prefix `++`/`--`, which need to know what came before them).
fn previous_before_previous(stream: &Stream<'_>, index: usize) -> CppTokenKind {
    if index >= 2 {
        stream.tokens[index - 2].kind
    } else {
        CppTokenKind::None
    }
}

fn is_a_binary_operator(kind: CppTokenKind) -> bool {
    use CppTokenKind::*;
    matches!(
        kind,
        Assign
            | PlusAssign
            | MinusAssign
            | StarAssign
            | SlashAssign
            | PercentAssign
            | AmpersandAssign
            | PipeAssign
            | CaretAssign
            | LeftShiftAssign
            | RightShiftAssign
            | Equal
            | NotEqual
            | Less
            | LessEqual
            | Greater
            | GreaterEqual
            | Spaceship
            | LogicalAnd
            | LogicalOr
            | Ampersand
            | Pipe
            | Caret
            | LeftShift
            | RightShift
            | Plus
            | Minus
            | Star
            | Slash
            | Percent
    )
}

/// Is the operator at `index` a prefix one (`-x`, `*p`, `!x`, `&x`)? Decided by what stands *before* it.
fn is_prefix_operator(stream: &Stream<'_>, index: usize, roles: &Roles) -> bool {
    use CppTokenKind::*;
    let token = stream.tokens[index];
    if !matches!(
        token.kind,
        Plus | Minus | Star | Ampersand | LogicalNot | Tilde
    ) {
        return false;
    }
    // A declarator is neither prefix nor infix; the caller handles it first.
    if roles.is_declarator(token.start) {
        return false;
    }
    // `CppTokenKind` has a `None` variant, so a plain `None =>` here would be read as a token kind.
    let Some(previous) = stream.tokens.get(index.wrapping_sub(1)) else {
        return true;
    };
    matches!(
        previous.kind,
        LeftParen
            | LeftBracket
            | LeftBrace
            | Comma
            | Semicolon
            | Assign
            | Colon
            | Question
            | LogicalAnd
            | LogicalOr
            | LogicalNot
            | ReturnKeyword
            | CaseKeyword
            | Equal
            | NotEqual
            | Less
            | Greater
            | LessEqual
            | GreaterEqual
            | Plus
            | Minus
            | Star
            | Slash
            | Percent
            | Pipe
            | Caret
            | Tilde
    )
}

/// Is the `:` at `index` a label's — an access specifier, a `case`/`default` label, or a `goto` target?
fn is_a_label_colon(stream: &Stream<'_>, index: usize) -> bool {
    use CppTokenKind::*;
    let Some(previous) = stream.tokens.get(index.wrapping_sub(1)) else {
        return false;
    };
    if matches!(
        previous.kind,
        PublicKeyword | PrivateKeyword | ProtectedKeyword | DefaultKeyword
    ) {
        return true;
    }
    // `goto done; ... done:` — an identifier alone at the start of a line. The token before it is on another line.
    if previous.kind == Identifier
        && stream
            .gaps
            .get(index - 1)
            .is_some_and(|gap| gap.newlines > 0)
    {
        return true;
    }
    // `case 'x':`, `case Foo::Bar:` — a `case` before it on the same statement, at the same depth.
    let mut cursor = index;
    while cursor > 0 {
        cursor -= 1;
        match stream.tokens[cursor].kind {
            CaseKeyword => return true,
            Semicolon | LeftBrace | RightBrace | LeftParen | RightParen => return false,
            _ => {}
        }
    }
    false
}

/// Is the `:` at `index` inside a `for (…)` head? (A range-based for's colon, which has its own style key.)
fn in_a_for_head(stream: &Stream<'_>, index: usize) -> bool {
    let mut cursor = index;
    let mut depth = 0usize;
    while cursor > 0 {
        cursor -= 1;
        match stream.tokens[cursor].kind {
            CppTokenKind::RightParen => depth += 1,
            CppTokenKind::LeftParen => {
                if depth == 0 {
                    return cursor > 0
                        && stream.tokens[cursor - 1].kind == CppTokenKind::ForKeyword;
                }
                depth -= 1;
            }
            CppTokenKind::Semicolon | CppTokenKind::LeftBrace | CppTokenKind::RightBrace => {
                return false;
            }
            _ => {}
        }
    }
    false
}

fn is_a_cast_keyword(kind: CppTokenKind) -> bool {
    use CppTokenKind::*;
    matches!(
        kind,
        StaticCastKeyword | DynamicCastKeyword | ConstCastKeyword | ReinterpretCastKeyword
    )
}

/// Does the `>` at `index` close a `static_cast<…>`-style list? Walk back to the `<` that opens this bracket.
fn angle_follows_a_cast(stream: &Stream<'_>, index: usize) -> bool {
    let mut depth = 0usize;
    let mut cursor = index;
    while cursor > 0 {
        cursor -= 1;
        match stream.tokens[cursor].kind {
            CppTokenKind::Greater => depth += 1,
            CppTokenKind::Less => {
                if depth == 0 {
                    return cursor > 0 && is_a_cast_keyword(stream.tokens[cursor - 1].kind);
                }
                depth -= 1;
            }
            CppTokenKind::Semicolon | CppTokenKind::LeftBrace => return false,
            _ => {}
        }
    }
    false
}

/// The text between `tokens[index - 1]` and `tokens[index]`, as the formatter would write it.
pub fn text(stream: &Stream<'_>, index: usize, roles: &Roles, style: &Style) -> String {
    " ".repeat(between(stream, index, roles, style))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Style;
    use crate::roles;
    use crate::tokens;

    fn spaces(source: &str, index: usize) -> usize {
        let stream = tokens::stream(source);
        let roles = roles::of(source);
        between(&stream, index, &roles, &Style::default_style())
    }

    #[test]
    fn a_comma_has_nothing_before_it_and_one_space_after() {
        // `f(a, b)`: tokens are f ( a , b )
        assert_eq!(spaces("f(a,b);", 3), 0, "before the comma");
        assert_eq!(spaces("f(a,b);", 4), 1, "after the comma");
    }

    #[test]
    fn a_call_is_glued_to_its_parenthesis_and_a_condition_is_not() {
        assert_eq!(spaces("f (a);", 1), 0);
        assert_eq!(spaces("if (a) {}", 1), 1);
    }

    #[test]
    fn a_template_argument_list_has_no_spaces_inside_it() {
        // std::vector<int> v;
        assert_eq!(spaces("std::vector < int > v;", 3), 0);
        assert_eq!(spaces("std::vector < int > v;", 4), 0);
    }

    #[test]
    fn a_declarator_star_follows_pointer_alignment() {
        // `int * p;` is `int` `*` `p` `;`: index 1 is the gap before the star, index 2 the one after it.
        // Under the LLVM preset `PointerAlignment` is Right: `int *p`.
        assert_eq!(spaces("int * p;", 1), 1, "before the star");
        assert_eq!(spaces("int * p;", 2), 0, "after the star");
    }

    #[test]
    fn a_product_keeps_its_spaces() {
        // `int x = a * b;` is `int` `x` `=` `a` `*` `b` `;`: the star is index 4.
        assert_eq!(spaces("int x = a * b;", 4), 1, "before the star");
        assert_eq!(spaces("int x = a * b;", 5), 1, "after the star");
    }
}