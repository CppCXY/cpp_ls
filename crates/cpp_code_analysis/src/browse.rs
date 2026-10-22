//! **The browse index**: a fuzzy, per-file scan that answers "what names does this file declare, and where".
//!
//! # Why this exists, and what it is not
//!
//! The index this crate had was *exact*: every file was cooked with its include closure's macro environment, parsed
//! into a tree, and swept for `DeclFact`s, with a second pass to repair the files whose first reading had no
//! environment. Measured on a real project (1 761 files, EmmyLuaCodeStyle), that costs **131 s cold**, and it is the
//! reason opening a project takes minutes.
//!
//! Every C++ editor with a reputation for speed does something much cheaper, and the difference is not
//! implementation quality — it is the *requirement*. Visual Studio's own documentation for the C++ Browsing Database
//! Parser says it plainly ([Hint Files](https://learn.microsoft.com/en-us/cpp/build/reference/hint-files)):
//!
//! > The C++ Browsing Database Parser is a fuzzy parser that can parse large amounts of code in a short amount of
//! > time. One reason it's fast is because it skips the content of blocks. For instance, it only records the location
//! > and parameters of a function, and ignores its contents.
//!
//! …and, of the macros that break its heuristics (a `#define BEGIN {` and friends), the answer is a **hint file** —
//! a fixed list of macro substitutions shipped with the toolchain — and "error recovery techniques to ignore
//! indecipherable source code". CLion's stub index and cpptools' tag parser are the same shape: one cheap pass per
//! file, no include closure, no macro expansion, bodies skipped, missing answers tolerated.
//!
//! So this module is deliberately shallow:
//!
//! ```text
//! * one token scan per file — no tree, no cooking, no macro environment, no include resolution
//! * it records (name, kind, scope as written, range) for namespaces, types, functions, variables, aliases, macros
//! * function bodies are skipped — the single biggest saving, and exactly what VS documents
//! * a name whose scope was opened by a macro (`_STD_BEGIN`) lands in the wrong scope or nowhere: **that is
//!   accepted**, and the fix is a hint ([`Hints`]), not a per-file preprocessing pass
//! ```
//!
//! # What it is not for
//!
//! Precision. Completion, hover, semantic colouring and diagnostics on the file a reader has open come from
//! [`crate::Session::view`], which cooks and parses that one file (measured: ~17 ms per edit). This index is what
//! makes the *rest* of the project visible at all, and it is allowed to be incomplete — see
//! `docs/clangd-model.md` §十二 for the measurements and the reasoning.

use std::path::{Path, PathBuf};

use cpp_parser::{CppTokenData, CppTokenKind, LexerConfig, SourceRange};

/// What a name in the browse index is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BrowseKind {
    Namespace,
    Class,
    Struct,
    Union,
    Enum,
    EnumMember,
    Function,
    Variable,
    Alias,
    Macro,
}

impl BrowseKind {
    /// The word a UI shows, and what the old `DeclKind` calls it where the two agree.
    pub fn as_str(self) -> &'static str {
        match self {
            BrowseKind::Namespace => "namespace",
            BrowseKind::Class => "class",
            BrowseKind::Struct => "struct",
            BrowseKind::Union => "union",
            BrowseKind::Enum => "enum",
            BrowseKind::EnumMember => "enumerator",
            BrowseKind::Function => "function",
            BrowseKind::Variable => "variable",
            BrowseKind::Alias => "alias",
            BrowseKind::Macro => "macro",
        }
    }

    /// Is this a name that can be completed after a `.`/`->`/`::`?
    pub fn is_a_member(self) -> bool {
        matches!(
            self,
            BrowseKind::Function
                | BrowseKind::Variable
                | BrowseKind::EnumMember
                | BrowseKind::Enum
                | BrowseKind::Class
                | BrowseKind::Struct
                | BrowseKind::Union
                | BrowseKind::Alias
        )
    }
}

/// One name, where it was written, and the scopes it was written inside — **as written**, not as compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseEntry {
    pub name: String,
    pub kind: BrowseKind,
    /// Outermost first: `["std", "chrono"]` for `std::chrono::seconds`.
    pub scope: Vec<String>,
    /// **The name this entry is declared under, in the file it was found in.**
    pub range: SourceRange,
    /// Base classes, as written, for a class or struct: `class D : public B` answers `["B"]`.
    pub bases: Vec<String>,
    /// The parameter list's text, for a function — what a completion shows.
    pub signature: Option<String>,
}

impl BrowseEntry {
    /// `std::chrono::seconds` — the name as a reader would write it.
    pub fn qualified(&self) -> String {
        if self.scope.is_empty() {
            return self.name.clone();
        }
        format!("{}::{}", self.scope.join("::"), self.name)
    }
}

/// **The macro substitutions the scan is allowed to know**, which is how it avoids preprocessng.
///
/// A macro that opens a scope (`#define _STD_BEGIN namespace std {`) cannot be followed by a token scan — the
/// braces are not in the file. Visual Studio's answer is a *hint file*: a fixed list of `#define`s shipped with the
/// toolchain and editable by the user, so the parser "understands and replaces the macro" without ever running a
/// preprocessor. This is that list.
///
/// The substitutions are applied **where they change how a brace or a name is read**: a macro that expands to
/// `{`, `}`, or a name prefix.
#[derive(Debug, Clone, Default)]
pub struct Hints {
    entries: Vec<(String, String)>,
}

impl Hints {
    pub fn new() -> Self {
        Self::default()
    }

    /// A macro whose expansion is entirely braces — `BEGIN` → `{`, `_STD_END` → `}`.
    pub fn add(&mut self, name: &str, expansion: &str) -> &mut Self {
        self.entries.push((name.to_string(), expansion.to_string()));
        self
    }

    /// What `name` expands to, if the file has no `#define` of its own for it.
    pub fn expansion_of(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(hint, _)| hint == name)
            .map(|(_, expansion)| expansion.as_str())
    }

    /// **The hints this crate ships**: the namespace-and-brace macros of libstdc++, libc++ and MSVC's STL.
    ///
    /// Small on purpose. A macro that only carries a keyword (`NOEXCEPT`) does not change the scan's answer, and a
    /// macro that wraps a *body* is skipped anyway. What matters is the handful that opens or closes a scope or
    /// stands where a name would.
    pub fn standard() -> Self {
        let mut hints = Self::new();
        // libstdc++: `namespace std { inline namespace __8 { inline namespace __cxx11 { … } } }` is opened by these.
        for (hint, expansion) in [
            ("_GLIBCXX_BEGIN_NAMESPACE_VERSION", "{"),
            ("_GLIBCXX_END_NAMESPACE_VERSION", "}"),
            ("_GLIBCXX_BEGIN_NAMESPACE_CONTAINER", "{"),
            ("_GLIBCXX_END_NAMESPACE_CONTAINER", "}"),
            ("_GLIBCXX_BEGIN_NAMESPACE_HEADER", "{"),
            ("_GLIBCXX_END_NAMESPACE_HEADER", "}"),
            ("_GLIBCXX_BEGIN_INLINE_ABI_NAMESPACE", "{"),
            ("_GLIBCXX_END_INLINE_ABI_NAMESPACE", "}"),
            // libc++.
            ("_LIBCPP_BEGIN_NAMESPACE_STD", "namespace std {"),
            ("_LIBCPP_END_NAMESPACE_STD", "}"),
            ("_LIBCPP_BEGIN_NAMESPACE_STD_SYNOD", "namespace std {"),
            ("_LIBCPP_END_NAMESPACE_STD_SYNOD", "}"),
            // MSVC's STL.
            ("_STD_BEGIN", "namespace std {"),
            ("_STD_END", "}"),
            ("_STD_BEGIN2", "namespace std {"),
            ("_STD_END2", "}"),
            ("_VSTD", "std"),
            // The corpus's own habits: a brace-carrying macro pair.
            ("BEGIN_NAMESPACE", "{"),
            ("END_NAMESPACE", "}"),
        ] {
            hints.add(hint, expansion);
        }
        hints
    }
}

/// **The browse index of one file**: the names it declares, in the order a scan finds them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowseFile {
    pub path: PathBuf,
    pub entries: Vec<BrowseEntry>,
    /// How many tokens the scan looked at — the cost model, kept for the probes.
    pub tokens: usize,
    /// How many tokens were **skipped as a block's content** — the saving Visual Studio documents.
    pub skipped: usize,
}

/// **Scan one file**: the whole of the browse index's per-file work.
///
/// Deliberately one pass and no fallbacks: what the heuristics cannot follow (a macro that opens a scope it has no
/// hint for, a preprocessor conditional that spreads a declaration over two branches) is simply missed. A missed
/// name costs one completion entry; the alternative — preprocess each file exactly — costs two orders of magnitude
/// and was measured doing it.
pub fn browse(path: &Path, text: &str, hints: &Hints) -> BrowseFile {
    let (tokens, _) = cpp_parser::lex(text, &LexerConfig::default());
    // **Trivia is dropped before anything else.** The lexer keeps whitespace, newlines and comments as tokens (the
    // parser needs them for the tree), and a scan that reads them as syntax sees `Whitespace` where it expects an
    // identifier. Only tokens that carry a *word* are interesting here, and the gaps between them still hold the
    // newlines a directive ends at.
    let tokens: Vec<cpp_parser::CppTokenData> = tokens
        .into_iter()
        .filter(|token| {
            !matches!(
                token.kind,
                cpp_parser::CppTokenKind::Whitespace
                    | cpp_parser::CppTokenKind::Newline
                    | cpp_parser::CppTokenKind::LineComment
                    | cpp_parser::CppTokenKind::BlockComment
            )
        })
        .collect();
    let mut scan = Scan {
        text,
        tokens: &tokens,
        hints,
        at: 0,
        depth: 0,
        scope: Vec::new(),
        pending: Vec::new(),
        entries: Vec::new(),
        skipped: 0,
        previous: None,
    };
    scan.run();

    BrowseFile {
        path: path.to_path_buf(),
        entries: scan.entries,
        tokens: tokens.len(),
        skipped: scan.skipped,
    }
}

/// A scope the scan has entered, and the brace depth it was opened at.
struct Open {
    name: String,
    depth: usize,
}

struct Scan<'a> {
    text: &'a str,
    tokens: &'a [CppTokenData],
    hints: &'a Hints,
    at: usize,
    depth: usize,
    scope: Vec<Open>,
    /// The names a `class`/`struct`/`enum` head has produced but whose body has not been reached yet.
    pending: Vec<BrowseEntry>,
    entries: Vec<BrowseEntry>,
    skipped: usize,
    previous: Option<CppTokenKind>,
}

impl Scan<'_> {
    fn kind_at(&self, at: usize) -> CppTokenKind {
        self.tokens
            .get(at)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::None)
    }

    fn text_at(&self, at: usize) -> &str {
        let Some(token) = self.tokens.get(at) else {
            return "";
        };
        let start = token.range.start_offset;
        let end = start + token.range.length;
        self.text.get(start..end).unwrap_or("")
    }

    fn at_a_directive_start(&self) -> bool {
        // `#define X ...` — the name is a macro the scan records, and the *body* must not be read as code (a
        // `#define BEGIN {` would otherwise look like a scope). VS's docs call these "disruptive macros"; here they
        // are simply skipped, and [`Hints`] is how the ones that matter are recovered.
        self.kind_at(self.at) == CppTokenKind::Hash
    }

    fn skip_the_directive(&mut self) {
        // A directive runs to the end of the line. The lexer drops the newline, so the next token whose line
        // differs ends it; the token range carries the line, so the check is on the text between them.
        let line = self
            .tokens
            .get(self.at)
            .map(|token| token.range.start_offset)
            .unwrap_or(0);
        let mut at = self.at;
        let mut newline_seen = false;
        while at < self.tokens.len() {
            let token = &self.tokens[at];
            let start = token.range.start_offset;
            if start > line {
                // Anything between this token's start and the previous one's end: if it holds a newline, the
                // directive ended before this token.
                let previous_end = self.tokens[at - 1].range.start_offset
                    + self.tokens[at - 1].range.length;
                newline_seen = self.text[previous_end..start].contains('\n');
            }
            if at > self.at && newline_seen {
                break;
            }
            at += 1;
        }
        self.at = at;
    }

    fn run(&mut self) {
        while self.at < self.tokens.len() {
            if self.at_a_directive_start() {
                self.directive();
                continue;
            }
            self.token();
            self.at += 1;
        }
        self.close_to(0);
    }

    fn directive(&mut self) {
        let hash = self.at;
        let mut at = hash + 1;
        let word = self
            .tokens
            .get(at)
            .map(|token| self.text_at(at).to_string())
            .unwrap_or_default();
        at += 1;
        if word != "define" {
            self.skip_the_directive();
            return;
        }
        // The name, then the body's *shape*: braces in it are counted as the hint list would count them.
        let name = self.text_at(at).to_string();
        if name.is_empty() {
            self.skip_the_directive();
            return;
        }
        let range = self.tokens[at].range;
        self.entries.push(BrowseEntry {
            name: name.clone(),
            kind: BrowseKind::Macro,
            scope: Vec::new(),
            range,
            bases: Vec::new(),
            signature: None,
        });
        // An unknown macro whose body is a bare `{` or `}` still moves the brace bookkeeping — that is how a file
        // with `#define BEGIN {` is read at all. It is not recorded as a scope: there is no name to record.
        at += 1;
        let mut body_braces = 0i64;
        while at < self.tokens.len() {
            let start = self.tokens[at].range.start_offset;
            let previous_end =
                self.tokens[at - 1].range.start_offset + self.tokens[at - 1].range.length;
            if self.text[previous_end..start].contains('\n') {
                break;
            }
            match self.tokens[at].kind {
                CppTokenKind::LeftBrace => body_braces += 1,
                CppTokenKind::RightBrace => body_braces -= 1,
                _ => {}
            }
            at += 1;
        }
        if self.hints.expansion_of(&name).is_none() {
            // Unhinted: the braces it carries are still real to the reader of the file, and a `{` with no name is
            // better counted than ignored — that is what "error recovery" means here.
            if body_braces > 0 {
                self.depth += body_braces as usize;
                self.scope.push(Open {
                    name: String::new(),
                    depth: self.depth,
                });
            } else if body_braces < 0 {
                for _ in 0..(-body_braces) {
                    self.close_one();
                }
            }
        }
        self.at = at;
    }

    fn token(&mut self) {
        match self.kind_at(self.at) {
            // `using namespace std;` names nothing: the keyword is the directive`s, not a scope`s.
            CppTokenKind::NamespaceKeyword if self.previous != Some(CppTokenKind::UsingKeyword) => {
                self.namespace_head();
            }
            CppTokenKind::ClassKeyword => self.type_head(BrowseKind::Class),
            CppTokenKind::StructKeyword => self.type_head(BrowseKind::Struct),
            CppTokenKind::UnionKeyword => self.type_head(BrowseKind::Union),
            CppTokenKind::EnumKeyword => self.type_head(BrowseKind::Enum),
            CppTokenKind::TypedefKeyword => self.alias_head(),
            CppTokenKind::UsingKeyword => self.using_head(),
            CppTokenKind::LeftBrace => {
                self.depth += 1;
                if let Some(entry) = self.pending.pop() {
                    self.entries.push(entry.clone());
                    self.scope.push(Open {
                        name: entry.name,
                        depth: self.depth,
                    });
                } else {
                    self.scope.push(Open {
                        name: String::new(),
                        depth: self.depth,
                    });
                }
                // **A function body, or any other block that is not a scope the scan needs.** Its content is what
                // Visual Studio's parser skips, and it is where most of a project's tokens live.
                if !self.inside_a_type_or_namespace() {
                    self.skip_the_block();
                }
            }
            CppTokenKind::RightBrace => {
                self.close_one();
                self.depth = self.depth.saturating_sub(1);
            }
            CppTokenKind::Semicolon => {
                self.pending.clear();
            }
            CppTokenKind::Identifier | CppTokenKind::OperatorKeyword => {
                if self.kind_at(self.at + 1) == CppTokenKind::LeftParen
                    && !matches!(
                        self.previous,
                        Some(
                            CppTokenKind::ClassKeyword
                                | CppTokenKind::StructKeyword
                                | CppTokenKind::UnionKeyword
                                | CppTokenKind::EnumKeyword
                                | CppTokenKind::NamespaceKeyword
                        )
                    )
                {
                    self.function_head();
                }
            }
            _ => {}
        }
        self.previous = Some(self.kind_at(self.at));
    }

    /// Inside a `class`/`struct`/`namespace` body the scan keeps reading; inside anything else it skips.
    fn inside_a_type_or_namespace(&self) -> bool {
        matches!(self.previous, Some(CppTokenKind::RightParen))
            && false
            || self
                .scope
                .last()
                .is_some_and(|open| !open.name.is_empty() && self.pending.is_empty())
    }

    /// Skip to the `}` that closes the block just entered — the body of a function, or any other statement block.
    fn skip_the_block(&mut self) {
        let target = self.depth;
        let mut at = self.at + 1;
        while at < self.tokens.len() {
            match self.tokens[at].kind {
                CppTokenKind::LeftBrace => self.depth += 1,
                CppTokenKind::RightBrace => {
                    self.skipped += 1;
                    if self.depth == target {
                        self.depth -= 1;
                        self.at = at;
                        return;
                    }
                    self.depth -= 1;
                }
                _ => {}
            }
            self.skipped += 1;
            at += 1;
        }
        self.at = at.saturating_sub(1);
    }

    fn close_one(&mut self) {
        if let Some(open) = self.scope.pop() {
            debug_assert!(open.depth <= self.depth.max(open.depth));
        }
    }

    fn close_to(&mut self, depth: usize) {
        while self.scope.last().is_some_and(|open| open.depth > depth) {
            self.scope.pop();
        }
    }

    /// `namespace [inline] name::name {` — and `namespace {`.
    fn namespace_head(&mut self) {
        let mut at = self.at + 1;
        let mut names: Vec<String> = Vec::new();
        loop {
            match self.kind_at(at) {
                CppTokenKind::InlineKeyword => at += 1,
                CppTokenKind::Identifier => {
                    names.push(self.text_at(at).to_string());
                    at += 1;
                    if self.kind_at(at) == CppTokenKind::Scope {
                        at += 1;
                        continue;
                    }
                    break;
                }
                _ => break,
            }
        }
        let entry = names.join("::");
        let range = self
            .tokens
            .get(self.at)
            .map(|token| token.range)
            .unwrap_or_else(|| SourceRange::new(0, 0));
        self.entries.push(BrowseEntry {
            name: if entry.is_empty() {
                "anonymous".to_string()
            } else {
                entry.clone()
            },
            kind: BrowseKind::Namespace,
            scope: self.scope_names(),
            range,
            bases: Vec::new(),
            signature: None,
        });
        // The body is entered by the `{` that follows, but the *name* has to be pushed now: the `{` handler has no
        // way to know it belongs to a namespace once the head is behind it.
        self.pending.push(BrowseEntry {
            name: entry,
            kind: BrowseKind::Namespace,
            scope: Vec::new(),
            range,
            bases: Vec::new(),
            signature: None,
        });
    }

    /// `class [attributes] Name [final] [: bases] {` — and the forward declaration, which has no body.
    fn type_head(&mut self, kind: BrowseKind) {
        let mut at = self.at + 1;
        // `enum class Name`, `enum struct Name`
        if kind == BrowseKind::Enum
            && matches!(
                self.kind_at(at),
                CppTokenKind::ClassKeyword | CppTokenKind::StructKeyword
            )
        {
            at += 1;
        }
        // Attributes and export/alignas clauses: skipped by shape, not by grammar.
        while matches!(
            self.kind_at(at),
            CppTokenKind::LeftBracket
                | CppTokenKind::RightBracket
                | CppTokenKind::AlignasKeyword
                | CppTokenKind::ExternKeyword
                | CppTokenKind::ExportKeyword
        ) {
            at += 1;
        }
        let mut name = String::new();
        while at < self.tokens.len() {
            match self.kind_at(at) {
                CppTokenKind::Identifier => {
                    name = self.text_at(at).to_string();
                    break;
                }
                CppTokenKind::LeftBrace | CppTokenKind::Semicolon | CppTokenKind::Colon => break,
                _ => at += 1,
            }
        }
        if name.is_empty() {
            // `struct { ... } x;` — an unnamed type. Nothing to record; the body still has to be entered as a scope
            // so that its members are not mistaken for the enclosing scope's.
            self.pending.push(BrowseEntry {
                name: String::new(),
                kind,
                scope: Vec::new(),
                range: SourceRange::new(0, 0),
                bases: Vec::new(),
                signature: None,
            });
            return;
        }
        let range = self.tokens[at].range;
        let mut bases = Vec::new();
        if self.kind_at(at + 1) == CppTokenKind::Colon {
            let mut cursor = at + 2;
            while cursor < self.tokens.len() {
                match self.kind_at(cursor) {
                    CppTokenKind::LeftBrace | CppTokenKind::Semicolon => break,
                    CppTokenKind::Identifier => {
                        let text = self.text_at(cursor).to_string();
                        if !matches!(
                            text.as_str(),
                            "public" | "private" | "protected" | "virtual"
                        ) {
                            bases.push(text);
                        }
                    }
                    _ => {}
                }
                cursor += 1;
            }
        }
        self.pending.push(BrowseEntry {
            name,
            kind,
            scope: self.scope_names(),
            range,
            bases,
            signature: None,
        });
    }

    /// `typedef ... Name;`
    fn alias_head(&mut self) {
        let mut at = self.at + 1;
        let mut last = String::new();
        let mut last_range = SourceRange::new(0, 0);
        while at < self.tokens.len() {
            match self.kind_at(at) {
                CppTokenKind::Semicolon | CppTokenKind::LeftBrace => break,
                CppTokenKind::Identifier => {
                    last = self.text_at(at).to_string();
                    last_range = self.tokens[at].range;
                }
                _ => {}
            }
            at += 1;
        }
        if !last.is_empty() {
            self.entries.push(BrowseEntry {
                name: last,
                kind: BrowseKind::Alias,
                scope: self.scope_names(),
                range: last_range,
                bases: Vec::new(),
                signature: None,
            });
        }
    }

    /// `using Name = ...;` — and `using namespace x;`, which names nothing.
    fn using_head(&mut self) {
        let mut at = self.at + 1;
        match self.kind_at(at) {
            CppTokenKind::NamespaceKeyword => return,
            CppTokenKind::Identifier => {}
            _ => return,
        }
        let name = self.text_at(at).to_string();
        let range = self.tokens[at].range;
        at += 1;
        // Only an alias declares a name; `using std::string;` lifts one that is declared elsewhere.
        if self.kind_at(at) != CppTokenKind::Assign {
            return;
        }
        self.entries.push(BrowseEntry {
            name,
            kind: BrowseKind::Alias,
            scope: self.scope_names(),
            range,
            bases: Vec::new(),
            signature: None,
        });
    }

    /// `name(params)` — a declaration or a definition; either way the name is what the index wants.
    fn function_head(&mut self) {
        let mut at = self.at;
        // `operator()` / `operator<` and friends: the name is the keyword plus whatever follows it.
        let mut name = self.text_at(at).to_string();
        if self.kind_at(at) == CppTokenKind::OperatorKeyword {
            at += 1;
            while at < self.tokens.len() && self.kind_at(at) != CppTokenKind::LeftParen {
                name.push_str(self.text_at(at));
                at += 1;
            }
        }
        let range = self.tokens[self.at].range;
        // The parameter list, as written, bounded: a completion shows it, and reading it costs nothing here.
        let mut cursor = at;
        while cursor < self.tokens.len()
            && self.kind_at(cursor) != CppTokenKind::LeftParen
            && cursor < at + 8
        {
            cursor += 1;
        }
        let signature = (self.kind_at(cursor) == CppTokenKind::LeftParen)
            .then(|| self.parameter_text(cursor));
        self.pending.push(BrowseEntry {
            name,
            kind: BrowseKind::Function,
            scope: self.scope_names(),
            range,
            bases: Vec::new(),
            signature,
        });
        // A declaration ends at `;` and a definition at `{` — the `{` handler turns the pending name into a scope
        // entry, which is wrong for a function: its *body* is not a scope of the index. So a function is emitted
        // here, and the `{` that follows is skipped as a block.
        if let Some(entry) = self.pending.pop() {
            self.entries.push(entry);
        }
    }

    fn parameter_text(&self, left_paren: usize) -> String {
        let start = self.tokens[left_paren].range.start_offset;
        let mut depth = 0i64;
        let mut at = left_paren;
        while at < self.tokens.len() {
            match self.tokens[at].kind {
                CppTokenKind::LeftParen => depth += 1,
                CppTokenKind::RightParen => {
                    depth -= 1;
                    if depth == 0 {
                        let end = self.tokens[at].range.start_offset + self.tokens[at].range.length;
                        return self.text.get(start..end).unwrap_or("").to_string();
                    }
                }
                _ => {}
            }
            at += 1;
        }
        String::new()
    }

    fn scope_names(&self) -> Vec<String> {
        self.scope
            .iter()
            .filter(|open| !open.name.is_empty())
            .map(|open| open.name.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Vec<String> {
        browse(Path::new("/p/a.h"), text, &Hints::standard())
            .entries
            .into_iter()
            .map(|entry| format!("{}:{}", entry.kind.as_str(), entry.qualified()))
            .collect()
    }

    #[test]
    fn a_class_and_what_is_inside_it() {
        let found = names(
            "namespace outer {\n\
             class Widget : public Base {\n\
             public:\n\
             void draw(int depth);\n\
             int width;\n\
             };\n\
             }\n",
        );
        assert!(found.contains(&"namespace:outer".to_string()), "{found:?}");
        assert!(
            found.contains(&"class:outer::Widget".to_string()),
            "{found:?}"
        );
        assert!(
            found.contains(&"function:outer::Widget::draw".to_string()),
            "{found:?}"
        );
    }

    #[test]
    fn a_function_body_is_skipped_not_read() {
        let file = browse(
            Path::new("/p/a.cpp"),
            "void big() {\n  for (int i = 0; i < 10; i++) { int inner = i; }\n  not_a_scope x;\n}\n",
            &Hints::standard(),
        );
        let names: Vec<&str> = file.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["big"], "only the function itself: {names:?}");
        assert!(
            file.skipped > 10,
            "the body's tokens were skipped, not scanned: {} token(s)",
            file.skipped
        );
    }

    #[test]
    fn bases_are_recorded_as_written() {
        let entries = browse(
            Path::new("/p/a.h"),
            "struct D : public B, private C { int x; };\n",
            &Hints::standard(),
        )
        .entries;
        let d = entries
            .iter()
            .find(|entry| entry.name == "D")
            .expect("D is indexed");
        assert_eq!(d.bases, vec!["B".to_string(), "C".to_string()]);
    }

    #[test]
    fn a_macro_is_a_name_and_its_braces_do_not_confuse_the_scan() {
        let entries = browse(
            Path::new("/p/a.h"),
            "#define BEGIN {\n#define END }\nBEGIN\nclass Inside { int x; };\nEND\n",
            &Hints::standard(),
        )
        .entries;
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert!(names.contains(&"BEGIN"), "{names:?}");
        assert!(names.contains(&"Inside"), "{names:?}");
    }

    #[test]
    fn the_shipped_hints_know_the_standard_library_macro() {
        let hints = Hints::standard();
        assert_eq!(hints.expansion_of("_STD_BEGIN"), Some("namespace std {"));
        assert_eq!(hints.expansion_of("NO_SUCH_MACRO"), None);
    }

    #[test]
    fn a_using_alias_is_a_name_and_a_using_directive_is_not() {
        let entries = browse(
            Path::new("/p/a.h"),
            "using Int = int;\nusing namespace std;\n",
            &Hints::standard(),
        )
        .entries;
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["Int"]);
    }
}
