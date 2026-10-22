//! **The layout: where the line breaks are, and how deep each line sits.**
//!
//! P0 scope (see `docs/cpp-format.md` §十): no wrapping. Every break this module emits is a **must-break** — a
//! statement end, a block brace, a directive, a label, a line comment. With `ColumnLimit: 0` that is the whole job;
//! with a column limit the wrapping pass (P2) will insert breaks *between* these and the indentation here has to
//! accept a "continuation" that is not a statement.
//!
//! # The decision model
//!
//! For each token, in order: the **space before it** is one of
//!
//! ```text
//! None            no space at all (`a[i]`, `x->y`, `f(`)
//! Spaces(n)       n spaces (`int x`, `a, b`)
//! Newline         a new line, at the computed indent
//! ```
//!
//! The indent comes from a stack of blocks. A `{` pushes a level (with adjustments for namespaces, classes, and
//! switch bodies); a `}` pops it; a `;` at paren depth 0 closes the statement; an access specifier or a case label
//! shifts only its own line.
//!
//! # Preprocessor lines are copied verbatim
//!
//! A `#` line — with its `\` continuations and its internal spacing — is sliced out of the source unchanged and
//! emitted at column 0. `docs/cpp-format.md` puts `#define` bodies in P4, and a P0 that re-spaced a macro body
//! would be worse than one that leaves it alone: the macro is not C++ until it is expanded.

use crate::config::Style;
use crate::tokens::{self, Gap, Stream, Token};
use cpp_parser::CppTokenKind;

/// What a block on the indent stack is, which decides what its body's indent is and how labels behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    /// A plain compound statement or initializer body that indents by `IndentWidth`.
    Plain,
    /// A `namespace` body; `NamespaceIndentation` decides whether it indents at all.
    Namespace,
    /// A class/struct/union body; access specifiers move to `AccessModifierOffset` from here.
    Class,
    /// A `switch` body; case labels and their statements stack differently.
    Switch,
}

#[derive(Debug, Clone)]
struct Level {
    kind: Block,
    /// The indent of the *body* — the column a statement inside this block starts at.
    indent: usize,
    /// The indent of a label (`case`, `public:`) in this block.
    label_indent: usize,
}

pub struct Layout<'a> {
    source: &'a str,
    stream: Stream<'a>,
    style: &'a Style,
    newline: &'static str,

    out: String,
    /// The column the output is at, in characters since the last newline.
    column: usize,
    at_line_start: bool,
    /// The indent the current logical line started at — where a continuation would line up.
    line_indent: usize,
    /// Blank lines still allowed before the next line (from `MaxEmptyLinesToKeep` and the input's own gaps).
    blank_lines: usize,

    levels: Vec<Level>,
    parens: usize,
    /// Set while the tokens of a `#` directive are being copied verbatim.
    in_directive: bool,
    /// The last significant token, for the space rules.
    previous: Option<Token<'a>>,
    /// The token before `previous`, for the rules that need two (`a::b`, `x * y` vs `int *p`).
    before_previous: Option<Token<'a>>,
    /// The first token of the current logical line, for "what kind of block is this `{`".
    line_head: Option<CppTokenKind>,
    /// Does the current class body have an access specifier in force? (For `AccessModifierOffset`.)
    saw_access_label: bool,
}

impl<'a> Layout<'a> {
    pub fn new(source: &'a str, style: &'a Style) -> Layout<'a> {
        Layout {
            source,
            stream: tokens::stream(source),
            style,
            newline: tokens::line_ending(source),
            out: String::with_capacity(source.len() + source.len() / 8),
            column: 0,
            at_line_start: true,
            line_indent: 0,
            blank_lines: 0,
            levels: Vec::new(),
            parens: 0,
            in_directive: false,
            previous: None,
            before_previous: None,
            line_head: None,
            saw_access_label: false,
        }
    }

    /// Run the layout and return the formatted text.
    pub fn run(mut self) -> String {
        let count = self.stream.tokens.len();
        for index in 0..count {
            self.token(index);
        }
        self.finish()
    }

    fn token(&mut self, index: usize) {
        let token = self.stream.tokens[index];
        let gap = self.stream.gaps[index].clone();

        // A directive: copy its own line(s) out of the source, unchanged.
        if token.kind == CppTokenKind::Hash && self.at_line_start {
            self.copy_directive(index, &gap);
            return;
        }

        // A `}` dedents before its own line is placed.
        let closing = token.kind == CppTokenKind::RightBrace && self.is_a_block_close();
        let level = if closing { self.levels.pop() } else { None };
        if closing {
            self.blank_lines = self.blank_lines.max(gap.newlines.saturating_sub(1));
        }

        // The space before this token, and any comments the gap holds.
        let want_break = self.breaks_before(index, closing);
        self.gap(index, &gap, want_break);

        // A token that starts a line and is not at one yet (a break the rules asked for).
        if want_break && !self.at_line_start {
            let indent = self.indent_for(token, level.as_ref(), closing);
            self.newline(indent);
        } else if want_break && self.at_line_start {
            // Already at the start of the line (a comment ended it): just set the column.
            let indent = self.indent_for(token, level.as_ref(), closing);
            self.pad_to(indent);
        } else if !want_break && !self.at_line_start {
            let spaces = self.space_before(index);
            self.spaces(spaces);
        } else if self.at_line_start {
            self.line_indent = 0;
        }

        self.column = self.column; // (kept explicit: the column is advanced by `push`)
        self.push(token.text);

        // …and the state the next token will read.
        self.after(index, token, level);
    }

    // ------------------------------------------------------------------------------------------------------------
    // Where the breaks are
    // ------------------------------------------------------------------------------------------------------------

    /// Does a new line start before `tokens[index]`?
    fn breaks_before(&mut self, index: usize, closing: bool) -> bool {
        let token = self.stream.tokens[index];
        if closing {
            return true;
        }
        // The first token of the file never starts with a newline.
        if index == 0 {
            return false;
        }
        let previous = self.stream.tokens[index - 1];
        match previous.kind {
            // A statement ends here (but not the `;` of a `for(;;)` head).
            CppTokenKind::Semicolon if self.parens == 0 => true,
            // A block opened: its body starts on the next line.
            CppTokenKind::LeftBrace if self.opened_a_block(index - 1) => true,
            // A block closed and the next token is not glued to it.
            CppTokenKind::RightBrace => !matches!(
                token.kind,
                CppTokenKind::Semicolon
                    | CppTokenKind::Comma
                    | CppTokenKind::RightParen
                    | CppTokenKind::RightBracket
                    | CppTokenKind::Dot
                    | CppTokenKind::Arrow
                    | CppTokenKind::Colon
                    | CppTokenKind::Question
                    | CppTokenKind::ElseKeyword
                    | CppTokenKind::CatchKeyword
                    | CppTokenKind::WhileKeyword
            ),
            // `public:` / `case 1:` and friends end their line.
            CppTokenKind::Colon => self.colon_ends_a_line(index - 1),
            _ => false,
        }
    }

    /// The number of spaces before `tokens[index]` when it stays on the same line.
    fn space_before(&self, index: usize) -> usize {
        let token = self.stream.tokens[index];
        let Some(previous) = self.previous else {
            return 0;
        };

        use CppTokenKind::*;
        match token.kind {
            // Never separated from what they follow.
            Comma | Semicolon | RightParen | RightBracket | Dot | Arrow | DotStar | ArrowStar => 0,
            Colon => {
                // `a::b` is not a colon token (it is `Scope`), so this is `case 1:`, `public:`, a bitfield, a
                // label or a ternary. A label/case/bitfield/ctor-init colon takes no space before it.
                if self.is_ternary_colon(index) { 1 } else { 0 }
            }
            // A call or an index: `f(`, `a[`, `T{`.
            LeftParen => {
                if self.space_before_paren(previous) { 1 } else { 0 }
            }
            LeftBracket => 0,
            LeftBrace => {
                // A braced initializer that follows `=`/`,`/`(`/`return` gets a space (it is a value); one that
                // follows a type or an identifier is a call (`T{1}`) or a block (handled as a break).
                if self.opened_a_block(index) { 1 } else { self.initializer_brace_space(index) }
            }
            RightBrace => 1,
            Scope => 0,
            // Operators: a binary operator has a space on both sides, a unary one none after it.
            Plus | Minus | Star | Slash | Percent | Assign | PlusAssign | MinusAssign | StarAssign
            | SlashAssign | PercentAssign | Equal | NotEqual | Less | LessEqual | Greater
            | GreaterEqual | Spaceship | LogicalAnd | LogicalOr | Ampersand | Pipe | Caret
            | LeftShift | RightShift | AmpersandAssign | PipeAssign | CaretAssign
            | LeftShiftAssign | RightShiftAssign | Question => {
                if self.is_pointer_or_reference(index) {
                    // `int *p` (Right) or `int* p` (Left).
                    match self.style.string("PointerAlignment", "Right") {
                        "Left" => 0,
                        _ => 1,
                    }
                } else if self.is_unary(index) {
                    // A prefix operator is glued to its operand but separated from what precedes it.
                    0
                } else {
                    1
                }
            }
            // After a prefix operator: no space.
            _ => self.space_after_previous(previous, index),
        }
    }

    fn space_after_previous(&self, previous: Token<'a>, index: usize) -> usize {
        use CppTokenKind::*;
        let token = self.stream.tokens[index];
        match previous.kind {
            LeftParen => {
                if self.style.flag("SpacesInParentheses", false) { 1 } else { 0 }
            }
            LeftBracket => {
                if self.style.flag("SpacesInSquareBrackets", false) { 1 } else { 0 }
            }
            Scope | Dot | Arrow | DotStar | ArrowStar => 0,
            Comma => 1,
            // A prefix operator glues to its operand.
            Plus | Minus | Star | Ampersand | LogicalNot | Tilde | PlusPlus | MinusMinus
                if self.is_unary_previous(index) => 0,
            // A pointer/reference declarator glues to the name when `PointerAlignment` is Right.
            Star | Ampersand if self.is_pointer_or_reference_before(index) => match self
                .style
                .string("PointerAlignment", "Right")
            {
                "Left" => 1,
                _ => 0,
            },
            // A `)` followed by `{`: `void f() {`, handled by the brace rules.
            _ => {
                if matches!(token.kind, RightParen | RightBracket) { 0 } else { 1 }
            }
        }
    }

    fn space_before_paren(&self, previous: Token<'a>) -> bool {
        use CppTokenKind::*;
        let mode = self.style.string("SpaceBeforeParens", "ControlStatements");
        if mode == "Always" {
            return true;
        }
        if mode == "Never" {
            return false;
        }
        matches!(
            previous.kind,
            IfKeyword
                | ForKeyword
                | WhileKeyword
                | SwitchKeyword
                | CatchKeyword
                | ReturnKeyword
                | DoKeyword
        ) && previous.kind != ReturnKeyword
            || (previous.kind == ReturnKeyword && mode == "ControlStatements")
    }

    fn initializer_brace_space(&self, index: usize) -> usize {
        // `= {`, `, {`, `({` and `return {` get a space; `T{` does not when `SpaceBeforeCpp11BracedList` is off.
        let Some(previous) = self.previous else {
            return 1;
        };
        use CppTokenKind::*;
        match previous.kind {
            Assign | Comma | LeftParen | LeftBracket | ReturnKeyword => 1,
            _ => {
                let _ = index;
                // `T{...}`: a space only when the style asks for one.
                if self.style.flag("SpaceBeforeCpp11BracedList", false) {
                    1
                } else {
                    0
                }
            }
        }
    }

    // ------------------------------------------------------------------------------------------------------------
    // Roles: the annotator, kept as small as P0 needs
    // ------------------------------------------------------------------------------------------------------------

    /// Is the `{` at `index` a **block** (a compound statement or a body) rather than an initializer list?
    fn opened_a_block(&self, index: usize) -> bool {
        let Some(previous) = self.previous else {
            return true;
        };
        use CppTokenKind::*;
        match previous.kind {
            RightParen => {
                // `void f() {` is a block; `T x(1) {` cannot happen. A `)` before `{` is a function body or a
                // control statement's body — both blocks — except `= std::vector<int>{...}` where `int>` preceded.
                true
            }
            Semicolon | RightBrace | Colon => true,
            ElseKeyword | DoKeyword | TryKeyword => true,
            Assign | Comma | LeftParen | LeftBracket | ReturnKeyword => false,
            Identifier | RightBracket | Greater => {
                // `struct X {`, `enum Y {`, `namespace {`, `class Z final {` — or a braced init `T{1}`. The
                // distinguishing token is the *line head*: a declaration body follows `class`/`struct`/`enum`/
                // `union`/`namespace`, an initializer follows an expression.
                matches!(
                    self.line_head,
                    Some(
                        ClassKeyword
                            | StructKeyword
                            | EnumKeyword
                            | UnionKeyword
                            | NamespaceKeyword
                    )
                ) || self.inherits(index)
            }
            _ => false,
        }
    }

    /// Is the `{` the opening brace of a base clause (`class X : public Y {`)?
    fn inherits(&self, index: usize) -> bool {
        let mut cursor = index;
        let mut budget = 16;
        while cursor > 0 && budget > 0 {
            cursor -= 1;
            budget -= 1;
            match self.stream.tokens[cursor].kind {
                CppTokenKind::Colon => return true,
                CppTokenKind::Semicolon | CppTokenKind::LeftBrace | CppTokenKind::RightBrace => {
                    return false
                }
                _ => {}
            }
        }
        false
    }

    fn is_a_block_close(&self) -> bool {
        !self.levels.is_empty()
    }

    /// Is the `:` at `index` one that ends a line (`public:`, `case 1:`, `private`, `default:`)?
    fn colon_ends_a_line(&self, index: usize) -> bool {
        let Some(previous) = self.previous else {
            return false;
        };
        use CppTokenKind::*;
        if matches!(
            previous.kind,
            PublicKeyword | PrivateKeyword | ProtectedKeyword | DefaultKeyword
        ) {
            return true;
        }
        // `case X:` — find the `case` before this colon on the same logical line.
        if self.line_head == Some(CaseKeyword) {
            return true;
        }
        // A label at the start of a line (`done:`).
        matches!(previous.kind, Identifier) && self.line_head == Some(previous.kind)
    }

    fn is_ternary_colon(&self, index: usize) -> bool {
        // A `?` before it, at the same paren depth, on the same line.
        let mut cursor = index;
        let mut budget = 64;
        while cursor > 0 && budget > 0 {
            cursor -= 1;
            budget -= 1;
            match self.stream.tokens[cursor].kind {
                CppTokenKind::Question => return true,
                CppTokenKind::Semicolon | CppTokenKind::LeftBrace | CppTokenKind::RightBrace => {
                    return false
                }
                _ => {}
            }
        }
        false
    }

    /// Is the `*`/`&`/`&&` at `index` a declarator (`int *p`, `T& r`) rather than a binary operator?
    fn is_pointer_or_reference(&self, index: usize) -> bool {
        use CppTokenKind::*;
        if !matches!(
            self.stream.tokens[index].kind,
            Star | Ampersand
        ) {
            return false;
        }
        // A declarator follows a type-ish token and is followed by a name, another `*`/`&`, or `(`.
        let previous = self.previous.map(|token| token.kind);
        let next = self.next_kind(index);
        let type_before = matches!(
            previous,
            Some(
                ConstKeyword
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
                    | BoolLiteral
                    | Identifier
                    | Greater
                    | Star
                    | Ampersand
                    | Scope
            )
        );
        let name_after = matches!(next, Some(Identifier) | Some(Star) | Some(Ampersand) | Some(LeftParen));
        type_before && name_after && !self.is_multiply_or_address_of(index)
    }

    /// The `*`/`&` *after* `tokens[index]` is a declarator (used for the space *after* it).
    fn is_pointer_or_reference_before(&self, index: usize) -> bool {
        self.is_pointer_or_reference(index.saturating_sub(1))
    }

    /// A `*`/`&` that is clearly a binary operator (both sides are values).
    fn is_multiply_or_address_of(&self, index: usize) -> bool {
        use CppTokenKind::*;
        let previous = self.previous.map(|token| token.kind);
        let next = self.next_kind(index);
        // `x * y`, `a & b` where the left side is a value: a name after `)`/`]`, a number, `this`.
        matches!(previous, Some(RightParen) | Some(RightBracket) | Some(IntegerLiteral))
            || matches!(next, Some(IntegerLiteral) | Some(ThisKeyword))
    }

    /// A prefix operator (`-x`, `!x`, `*p`, `&x`, `++i`).
    fn is_unary(&self, index: usize) -> bool {
        use CppTokenKind::*;
        let token = self.stream.tokens[index];
        let previous = self.previous.map(|previous| previous.kind);
        let prefix_context = matches!(
            previous,
            None | Some(LeftParen)
                | Some(LeftBracket)
                | Some(LeftBrace)
                | Some(Comma)
                | Some(Semicolon)
                | Some(Assign)
                | Some(ReturnKeyword)
                | Some(Colon)
                | Some(Question)
                | Some(LogicalNot)
                | Some(LogicalAnd)
                | Some(LogicalOr)
        );
        match token.kind {
            Star | Ampersand => prefix_context && !self.is_pointer_or_reference(index),
            Plus | Minus | LogicalNot | Tilde => prefix_context,
            PlusPlus | MinusMinus => false,
            _ => false,
        }
    }

    fn is_unary_previous(&self, index: usize) -> bool {
        self.is_unary(index.saturating_sub(1))
    }

    fn next_kind(&self, index: usize) -> Option<CppTokenKind> {
        self.stream.tokens.get(index + 1).map(|token| token.kind)
    }

    // ------------------------------------------------------------------------------------------------------------
    // Indentation
    // ------------------------------------------------------------------------------------------------------------

    fn width(&self) -> usize {
        self.style.int("IndentWidth", 2).max(0) as usize
    }

    fn body_indent(&self) -> usize {
        self.levels.last().map_or(0, |level| level.indent)
    }

    fn indent_for(&self, token: Token<'a>, popped: Option<&Level>, closing: bool) -> usize {
        use CppTokenKind::*;
        if closing {
            return popped.map_or(0, |level| level.indent.saturating_sub(self.width()));
        }
        // A label in a class or switch body sits at the level's label indent.
        if matches!(
            token.kind,
            PublicKeyword | PrivateKeyword | ProtectedKeyword
        ) {
            let offset = self.style.int("AccessModifierOffset", -1);
            let base = self.body_indent() as i64;
            return (base + offset).max(0) as usize;
        }
        if matches!(token.kind, CaseKeyword | DefaultKeyword) {
            return self
                .levels
                .last()
                .filter(|level| level.kind == Block::Switch)
                .map_or(self.body_indent(), |level| level.label_indent);
        }
        self.body_indent()
    }

    // ------------------------------------------------------------------------------------------------------------
    // Emitting
    // ------------------------------------------------------------------------------------------------------------

    fn push(&mut self, text: &str) {
        self.out.push_str(text);
        if let Some(last) = text.rsplit('\n').next() {
            if text.contains('\n') {
                self.column = last.chars().count();
                self.at_line_start = last.is_empty();
            } else {
                self.column += text.chars().count();
                self.at_line_start = false;
            }
        }
    }

    fn spaces(&mut self, count: usize) {
        if self.at_line_start || count == 0 {
            return;
        }
        for _ in 0..count {
            self.out.push(' ');
        }
        self.column += count;
    }

    fn newline(&mut self, indent: usize) {
        // At most `MaxEmptyLinesToKeep` blank lines, and only where the input had one.
        let keep = self.style.int("MaxEmptyLinesToKeep", 1).max(0) as usize;
        let blanks = self.blank_lines.min(keep);
        if !self.out.is_empty() {
            self.out.push_str(self.newline);
        }
        for _ in 0..blanks {
            self.out.push_str(self.newline);
        }
        self.blank_lines = 0;
        for _ in 0..indent {
            self.out.push(' ');
        }
        self.column = indent;
        self.at_line_start = true;
    }

    fn pad_to(&mut self, indent: usize) {
        while self.column < indent {
            self.out.push(' ');
            self.column += 1;
        }
        self.at_line_start = true;
    }

    fn finish(mut self) -> String {
        // The file ends with exactly one newline if it had any text.
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push_str(self.newline);
        }
        self.out
    }
}
