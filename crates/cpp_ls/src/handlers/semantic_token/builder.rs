//! # The semantic-token builder — the shape `textDocument/semanticTokens/full` is assembled in.
//!
//! Learned from `emmylua_ls`'s `handlers/semantic_token`, whose split this follows deliberately: **a type
//! vocabulary that carries its own numbers**, a **bitflag modifier set**, and a **collector that sorts and encodes
//! once**. The point of the arrangement is that no caller ever thinks about ordering, overlap or deltas — it says
//! "this range is a type" and the builder is what makes that a valid protocol answer.
//!
//! ```text
//! 1. TypeKind::to_u32()   a closed enum, and the number IS the enum — not a search of the legend
//! 2. Modifier             a bitflag newtype with `|`, so modifiers combine without arithmetic at each site
//! 3. Builder::push_range  first write at an offset wins; a later claim on the same span is dropped
//! 4. Builder::build       sort by offset → split multi-line → the protocol's delta walk
//! ```
//!
//! # Why first-write-wins is the whole overlap rule
//!
//! The protocol requires the answer to be **ordered and non-overlapping**, and an overlap shifts every token after
//! it — the one error a client cannot recover from. A highlighter has several things to say about one file and they
//! do collide: `operator` is a keyword and begins the name `operator+`; a type name is written inside a parameter
//! whose own name is a token. Deciding those collisions *where the claims are made* would mean every caller knowing
//! what every other caller said.
//!
//! So the builder decides, with one rule: **the first claim on an offset keeps it.** The passes are run in priority
//! order — the analysis's names before the lexer's keywords — and what a pass could not claim was already claimed by
//! something that knew more about it. This replaces the hand-written "does any name cover this keyword" scan this
//! layer used to carry, which was the same rule written out once per caller and therefore once per bug.
//!
//! # Why the tokens are kept as offsets
//!
//! A line and a column are what the protocol sends, and they are **derived** here rather than stored: a claim is an
//! offset range, and the conversion happens once per emitted token. Splitting a multi-line token is then a walk
//! over the lines it covers rather than arithmetic on line lengths, and nothing has to be recomputed when a claim
//! moves — which is also what `emmylua_ls` does, and for the same reason.

use std::collections::HashSet;

use cpp_code_analysis::VfsFile;
use lsp_types::{SemanticToken, SemanticTokenModifier, SemanticTokenType};

use crate::util::position_in_file;

/// **Every token type this server can send** — the vocabulary, and the legend is derived from it.
///
/// A closed enum rather than a list of `SemanticTokenType`s, and the numbers are **written into the enum** rather
/// than taken from a position in the legend. That is `emmylua_ls`'s arrangement and the reason is worth stating:
/// when the index is a list position, adding a type in the middle renumbers every type after it, and the symptom
/// is every name in the file painted as whatever moved — an answer that looks exactly like a right one. Here a
/// number is a property of the type and [`Builder::legend`] is built from the same list, so the two cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeKind {
    Namespace,
    Type,
    Class,
    Enum,
    TypeParameter,
    Parameter,
    Variable,
    Property,
    EnumMember,
    Function,
    Method,
    Macro,
    Keyword,
    Modifier,
    Operator,
    /// **A Doxygen command** — the `@param` of `@param x the x`.
    ///
    /// Its own kind rather than `Keyword`, because it is not one: a command is *inside a comment*, and a theme
    /// that draws it exactly as it draws `return` is saying a comment contains code. The protocol's `decorator` is
    /// the closest standard type for a marker that introduces something — an annotation, an attribute, a doc tag —
    /// and it is the one a client can remap on its own: VS Code's `semanticTokenScopes` lets a client say "draw
    /// `decorator` from this server as `keyword.doxygen`", which is what the cppls extension does. Sending a
    /// *custom* type instead would make the server's answer unreadable to every client but that one.
    DocCommand,
    /// **A variable template** — `template <class T> constexpr bool is_trivially_assignable_v = …`.
    ///
    /// The protocol has no such type, and this is the one place this server sends a **custom** name. It earns the
    /// exception because a reader sees a difference worth colouring: a variable template is a *family* of values
    /// (you write `is_trivially_assignable_v<A, B>`, not `is_trivially_assignable_v`), and drawing it exactly like a
    /// `constexpr bool` says the bare name is a value that can be used. A client that does not know the name cannot
    /// draw it — [`Builder::push_range`] drops a type the client did not advertise — so the pass falls back to
    /// `variable`, which is what that client showed before this existed.
    VariableTemplate,
    /// **A format string's field** — the `{:.3f}` of `std::format`, the `%-8.2` of `printf`.
    ///
    /// One of the four types that colour *inside* a string literal, where no other pass has anything to say: the
    /// client draws a literal as one colour, and the parts of it that are a program of their own — a field, the
    /// argument it names, the conversion it ends in — are invisible. See
    /// [`crate::handlers::semantic_token::format`] for what decides which literal is one and `docs/format-strings.md`
    /// for the design.
    ///
    /// **Custom, and with a fallback the client does know**: a client that advertises none of these four gets
    /// `regexp` for a field, `parameter` for an argument reference and `keyword` for a conversion — the closest
    /// standard types — so the colouring works out of the box in an editor whose theme has never heard of them.
    FormatSpecifier,
    /// **A reference to an argument**: `{0}`, `{name}`, `%1$`, `*`.
    FormatArgument,
    /// **A conversion**: the `f` of `{:.3f}`, the `d` of `%d`, the `[a-z]` of `%[a-z]`.
    FormatConversion,
    /// **An escape**: `{{`, `}}`, `%%` — a literal brace or percent, which takes no argument.
    ///
    /// Its fallback is *nothing*: the client's own string colour already draws it, and a `string` token would be the
    /// same colour at the price of one token.
    FormatEscape,
    /// **A regular expression** — not this server's own, and here only as the fallback for a format field.
    ///
    /// The protocol has had `regexp` since 3.16 and every client advertises it; a theme draws it as the pattern
    /// language it is, and a format specifier *is* one. It is in the legend so that [`Builder::push_range`] can send
    /// it, which is the one thing this server needs from it.
    Regexp,
}

impl TypeKind {
    /// The number the encoding sends, which is the index into [`Builder::legend`].
    pub const fn to_u32(self) -> u32 {
        match self {
            TypeKind::Namespace => 0,
            TypeKind::Type => 1,
            TypeKind::Class => 2,
            TypeKind::Enum => 3,
            TypeKind::TypeParameter => 4,
            TypeKind::Parameter => 5,
            TypeKind::Variable => 6,
            TypeKind::Property => 7,
            TypeKind::EnumMember => 8,
            TypeKind::Function => 9,
            TypeKind::Method => 10,
            TypeKind::Macro => 11,
            TypeKind::Keyword => 12,
            TypeKind::Modifier => 13,
            TypeKind::Operator => 14,
            TypeKind::DocCommand => 15,
            TypeKind::VariableTemplate => 16,
            TypeKind::FormatSpecifier => 17,
            TypeKind::FormatArgument => 18,
            TypeKind::FormatConversion => 19,
            TypeKind::FormatEscape => 20,
            TypeKind::Regexp => 21,
        }
    }

    /// The protocol's name for it, as advertised in the legend.
    pub const fn to_semantic_token_type(self) -> SemanticTokenType {
        match self {
            TypeKind::Namespace => SemanticTokenType::NAMESPACE,
            TypeKind::Type => SemanticTokenType::TYPE,
            TypeKind::Class => SemanticTokenType::CLASS,
            TypeKind::Enum => SemanticTokenType::ENUM,
            TypeKind::TypeParameter => SemanticTokenType::TYPE_PARAMETER,
            TypeKind::Parameter => SemanticTokenType::PARAMETER,
            TypeKind::Variable => SemanticTokenType::VARIABLE,
            TypeKind::Property => SemanticTokenType::PROPERTY,
            TypeKind::EnumMember => SemanticTokenType::ENUM_MEMBER,
            TypeKind::Function => SemanticTokenType::FUNCTION,
            TypeKind::Method => SemanticTokenType::METHOD,
            TypeKind::Macro => SemanticTokenType::MACRO,
            TypeKind::Keyword => SemanticTokenType::KEYWORD,
            TypeKind::Modifier => SemanticTokenType::MODIFIER,
            TypeKind::Operator => SemanticTokenType::OPERATOR,
            TypeKind::DocCommand => SemanticTokenType::DECORATOR,
            // **A custom type**, and the only one this server sends: the protocol has no `variableTemplate`, while a
            // theme has every reason to colour one differently from a value. A client that does not know the name
            // cannot draw it — `Builder::push_range` drops it — so the caller falls back to `variable`, which is what
            // that client would have shown before this existed. See `push_names`.
            TypeKind::VariableTemplate => SemanticTokenType::new("variableTemplate"),
            // **The four format-string types**, and the reason they are custom: the protocol has no vocabulary for
            // the language inside a format string, and a theme that wants to draw `{:.3f}` apart from its
            // surrounding literal needs a name for it. Each has a standard fallback in the pass that sends it — see
            // `format::draw`.
            TypeKind::FormatSpecifier => SemanticTokenType::new("formatSpecifier"),
            TypeKind::FormatArgument => SemanticTokenType::new("formatArgument"),
            TypeKind::FormatConversion => SemanticTokenType::new("formatConversion"),
            TypeKind::FormatEscape => SemanticTokenType::new("formatEscape"),
            TypeKind::Regexp => SemanticTokenType::REGEXP,
        }
    }

    /// **Every type, in [`TypeKind::to_u32`] order** — the legend, and the list the client is told.
    pub const ALL: &'static [TypeKind] = &[
        TypeKind::Namespace,
        TypeKind::Type,
        TypeKind::Class,
        TypeKind::Enum,
        TypeKind::TypeParameter,
        TypeKind::Parameter,
        TypeKind::Variable,
        TypeKind::Property,
        TypeKind::EnumMember,
        TypeKind::Function,
        TypeKind::Method,
        TypeKind::Macro,
        TypeKind::Keyword,
        TypeKind::Modifier,
        TypeKind::Operator,
        TypeKind::DocCommand,
        TypeKind::VariableTemplate,
        TypeKind::FormatSpecifier,
        TypeKind::FormatArgument,
        TypeKind::FormatConversion,
        TypeKind::FormatEscape,
        TypeKind::Regexp,
    ];
}

/// **What a token's colour is qualified by** — a bitflag, so two modifiers are one `|` and not an add.
///
/// The protocol sends a bitset whose bits index the advertised modifier list, so each modifier must be a power of
/// two and the two orders must match. Both are properties of this type rather than of the call sites, which is the
/// point: the arithmetic that used to be written out at each claim (`bits |= 1 << 1`) lives here, and a modifier
/// inserted in the middle cannot silently renumber the ones after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifier(u32);

impl Modifier {
    pub const DECLARATION: Modifier = Modifier(1 << 0);
    pub const READONLY: Modifier = Modifier(1 << 1);
    pub const STATIC: Modifier = Modifier(1 << 2);
    pub const DEPRECATED: Modifier = Modifier(1 << 3);

    pub const fn none() -> Self {
        Modifier(0)
    }

    pub const fn to_u32(self) -> u32 {
        self.0
    }

    /// **The advertised list, in bit order** — the same arrangement [`TypeKind::ALL`] has, and for the same reason.
    pub const ALL: &'static [(Modifier, SemanticTokenModifier)] = &[
        (Modifier::DECLARATION, SemanticTokenModifier::DECLARATION),
        (Modifier::READONLY, SemanticTokenModifier::READONLY),
        (Modifier::STATIC, SemanticTokenModifier::STATIC),
        (Modifier::DEPRECATED, SemanticTokenModifier::DEPRECATED),
    ];

    pub fn all_modifiers() -> Vec<SemanticTokenModifier> {
        Modifier::ALL
            .iter()
            .map(|(_, advertised)| advertised.clone())
            .collect()
    }
}

impl std::ops::BitOr for Modifier {
    type Output = Modifier;

    fn bitor(self, rhs: Modifier) -> Modifier {
        Modifier(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Modifier {
    fn bitor_assign(&mut self, rhs: Modifier) {
        self.0 |= rhs.0;
    }
}

/// One **absolute** claim, before it becomes a delta.
#[derive(Debug, Clone, Copy)]
struct Claim {
    start: usize,
    end: usize,
    token_type: u32,
    modifiers: u32,
}

/// One token as the protocol wants it: a line, a column and a length on that line.
struct Placed {
    line: u32,
    column: u32,
    length: u32,
    token_type: u32,
    modifiers: u32,
}

/// **Collects claims, then answers with the protocol's walk.**
///
/// The whole public surface is `push_range` and `build`. A caller never sees a line, a column or a delta.
pub struct Builder<'a> {
    file: &'a VfsFile,
    /// What the client says it can draw, or `None` for "it said nothing", in which case everything is sent.
    drawable: Option<&'a [SemanticTokenType]>,
    /// Whether the client can draw a token that spans lines. When it cannot, such a claim is **split** into one
    /// token per line — see [`Builder::build`].
    multi_line_support: bool,
    claims: Vec<Claim>,
    /// Every offset already claimed. The overlap rule, and the reason the passes may be run in priority order
    /// without any of them knowing about the others.
    claimed: HashSet<usize>,
    /// Claims dropped because the file could not place them, counted so a caller can say so rather than guess.
    unplaceable: usize,
}

impl<'a> Builder<'a> {
    pub fn new(
        file: &'a VfsFile,
        drawable: Option<&'a [SemanticTokenType]>,
        multi_line_support: bool,
    ) -> Self {
        Self {
            file,
            drawable,
            multi_line_support,
            claims: Vec::new(),
            claimed: HashSet::new(),
            unplaceable: 0,
        }
    }

    /// **The legend**, in the order [`TypeKind::to_u32`] numbers.
    pub fn legend() -> Vec<SemanticTokenType> {
        TypeKind::ALL
            .iter()
            .map(|kind| kind.to_semantic_token_type())
            .collect()
    }

    /// **Claim a range for a type.** The one primitive; every other way of saying it is a spelling of this.
    ///
    /// Returns whether the claim was kept, so a caller with something else to try can tell — and so a test can ask
    /// the collision question directly rather than reading it out of an encoded answer.
    pub fn push_range(
        &mut self,
        start: usize,
        end: usize,
        token_type: TypeKind,
        modifiers: Modifier,
    ) -> bool {
        if end <= start {
            return false;
        }

        // A kind the client cannot draw is not sent: its number would map to nothing at the client's end, and the
        // protocol's own note says a client may render fewer types than the server sends. Asked **before** the claim
        // is recorded, so a type the client cannot draw does not block a later claim on the same span.
        if let Some(drawable) = self.drawable
            && !drawable.contains(&token_type.to_semantic_token_type())
        {
            return false;
        }

        // **First claim wins.** See the module documentation: this is the overlap rule, and it is why the passes can
        // be ordered by how much they know without any of them consulting the others.
        if !self.claimed.insert(start) {
            return false;
        }

        // A range the file cannot place — an offset past its end, which a stale view or a cooked reading can
        // produce. Dropping one claim is the only option that keeps the walk valid: a guessed position would move
        // every token after it.
        //
        // **A length check, not two line lookups.** This runs once per claim and a large file has tens of thousands
        // of them; `position_in_file` answered the same question by finding the line of the offset — twice — which
        // is a binary search and a slice each time. Every offset up to and including the text's length is a place
        // the file can put a position (the end of the text is a zero-width range at EOF), and every offset past it
        // is not, so the length *is* the test.
        if end > self.file.text.len() {
            self.unplaceable += 1;
            return false;
        }

        self.claims.push(Claim {
            start,
            end,
            token_type: token_type.to_u32(),
            modifiers: modifiers.to_u32(),
        });
        true
    }

    /// Claim a whole token, by its range — the spelling of [`Builder::push_range`] a caller with a range in hand
    /// wants, and the one `emmylua_ls` builds its token pass on.
    #[allow(dead_code)]
    pub fn push_at_range(
        &mut self,
        range: std::ops::Range<usize>,
        token_type: TypeKind,
        modifiers: Option<Modifier>,
    ) -> bool {
        self.push_range(
            range.start,
            range.end,
            token_type,
            modifiers.unwrap_or(Modifier::none()),
        )
    }

    /// Has this offset been claimed by an earlier pass?
    ///
    /// Kept although nothing calls it in production: it is how a test asks the collision question directly rather
    /// than reading it out of an encoded answer, and it is the rule the two passes are ordered by.
    #[allow(dead_code)]
    pub fn has_claimed(&self, start: usize) -> bool {
        self.claimed.contains(&start)
    }

    /// How many claims were dropped because the file could not place them.
    pub fn unplaceable(&self) -> usize {
        self.unplaceable
    }

    /// **The column just past the last character of `line`** — what a split needs, and the one thing a token's own
    /// range cannot give.
    ///
    /// From the file's own line index: the next line's start is this line's end, and a trailing newline (and a
    /// carriage return, which a file written on Windows has) is not part of the line's *text*, so it comes off.
    /// `None` for a line the file does not have; the last line of a file that ends without a newline is answered by
    /// the text's own length.
    fn end_of_line(file: &VfsFile, line: u32) -> Option<u32> {
        let index = &file.line_index;
        let start = usize::from(index.get_line_offset(line as usize)?);
        let end = index
            .get_line_offset(line as usize + 1)
            .map_or(file.text.len(), usize::from)
            .min(file.text.len());
        let text = file.text.get(start..end)?;
        let body = text.strip_suffix('\n').unwrap_or(text);
        let body = body.strip_suffix('\r').unwrap_or(body);
        Some(body.chars().count() as u32)
    }

    /// **The protocol's answer**: sorted, non-overlapping, no zero-length token, multi-line handled.
    pub fn build(self) -> Vec<SemanticToken> {
        // **Destructured first**, so the borrow of the file outlives the move of the claims: the split below needs
        // the file's line index while it consumes the claims, and a method taking `self` cannot borrow a field and
        // move another at once.
        let Builder {
            file,
            claims,
            multi_line_support,
            ..
        } = self;

        let mut claims = claims;
        claims.sort_unstable_by_key(|claim| (claim.start, claim.end));

        let mut placed: Vec<Placed> = Vec::with_capacity(claims.len());
        for claim in claims {
            let Some(from) = position_in_file(file, claim.start) else {
                continue;
            };
            // **The position of the last character, not of the end.** `claim.end` is exclusive, so asking the file
            // where it is gives the position *after* the token — which, for a token ending exactly at a line's end,
            // is the first column of the **next** line. Measured on `/* one\ntwo\nthree */`: the split produced a
            // fourth token on the line below the comment, whose whole content was the newline. The line a spanning
            // token ends on is the line its last character is on.
            let Some(to) = position_in_file(file, claim.end.saturating_sub(1)) else {
                continue;
            };

            if multi_line_support || from.line == to.line {
                // **One token.** A client that can draw a multi-line token is sent it whole: `length` then reaches
                // to the end position's column on a later line, which is what such a client expects and what the
                // protocol means by a multi-line token.
                //
                // The length is counted from the **text**, not subtracted from two columns: `to` is the position of
                // the last character, so `to.character - from.character` is one short. And the protocol counts
                // **UTF-16 code units**, not bytes and not characters — a name with a character outside the basic
                // plane is longer in bytes than in what the client measures the span with.
                let length = file
                    .text
                    .get(claim.start..claim.end)
                    .map(|text| text.encode_utf16().count() as u32)
                    .unwrap_or(0);
                placed.push(Placed {
                    line: from.line,
                    column: from.character,
                    length,
                    token_type: claim.token_type,
                    modifiers: claim.modifiers,
                });
                continue;
            }

            // **Split across the lines it covers**, for a client that cannot draw one spanning token. There is no
            // way in this protocol to say "this continues below", so the range becomes a run of single-line
            // tokens: the first from its column to the end of that line, the last from the start of its line, and
            // the whole of every line between. `emmylua_ls` writes the same three cases.
            //
            // Each piece needs its own line's **real length**, and the two ends come from different places: the
            // first line ends where the line does, which the file's own line index answers ([`Builder::end_of_line`]),
            // while the last ends one column past `to.character` — `to` is a *character position*, not a length.
            for line in from.line..=to.line {
                let (column, length) = if line == from.line {
                    (
                        from.character,
                        Self::end_of_line(file, line)
                            .map(|end| end.saturating_sub(from.character))
                            .unwrap_or(to.character + 1),
                    )
                } else if line == to.line {
                    (0, to.character + 1)
                } else {
                    (0, Self::end_of_line(file, line).unwrap_or(0))
                };
                placed.push(Placed {
                    line,
                    column,
                    length,
                    token_type: claim.token_type,
                    modifiers: claim.modifiers,
                });
            }
        }

        // Sorted by position, which the claims already were — but the split above appends one token per line, and a
        // claim that began earlier on a line must still come before one that began later on the same line.
        placed.sort_unstable_by_key(|token| (token.line, token.column));

        let mut answer = Vec::with_capacity(placed.len());
        let mut previous: Option<(u32, u32)> = None;
        for token in placed {
            let (delta_line, delta_start) = match previous {
                // Same line: measured from the previous token's column.
                Some((line, column)) if line == token.line => {
                    (0, token.column.saturating_sub(column))
                }
                // A new line: measured from the line's start, which the protocol encodes by sending the column.
                Some((line, _)) => (token.line - line, token.column),
                // The first token of the file: measured from the file's start.
                None => (token.line, token.column),
            };

            answer.push(SemanticToken {
                delta_line,
                delta_start,
                length: token.length,
                token_type: token.token_type,
                token_modifiers_bitset: token.modifiers,
            });
            previous = Some((token.line, token.column));
        }

        answer
    }
}
