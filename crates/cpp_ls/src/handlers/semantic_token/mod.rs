//! # `textDocument/semanticTokens/full` — what each name *is*, drawn as colour
//!
//! ```text
//! struct Widget { int size; };        `Widget` a class, `size` a field, both drawn from what declares them
//! #define LIMIT 8                     `LIMIT` a macro — the preprocessor's name, not a C++ declaration at all
//! int f(Widget& w) { return w.size; }  the use of `size` is the *declaration's* kind, because that is what it is
//! ```
//!
//! The classification comes from the analysis ([`Session::classified_names`]) and this layer does three things
//! with it, and nothing else:
//!
//! ```text
//! 1. the legend      which of the protocol's token types this server uses, fixed and advertised once
//! 2. the filter      a kind the client says it cannot draw is not sent — a number it would not understand
//! 3. the encoding    absolute offsets and lengths  →  the protocol's delta walk
//! ```
//!
//! # Why the numbers are a *walk* rather than a list
//!
//! The protocol sends five numbers per token, and the first two are deltas: how many lines since the previous
//! token, and how many characters since it (or from the line's start, on a new line). Nothing in the answer may be
//! out of order, overlap, or have zero length, and the client reconstructs absolute positions from the deltas
//! alone — so an off-by-one does not shift one token, it shifts **every token after it**. That is why the encoding
//! is a function of its own here rather than a loop inside the handler, and why it is tested against a file whose
//! tokens straddle lines.
//!
//! # What is not sent
//!
//! A name the analysis cannot place (see the `semantic` module: a name nothing declares, or one whose spelling
//! several unrelated declarations share). The client draws those identifiers with its own default colour, which is
//! the honest drawing of "this analysis has nothing to say about this name" — and it is also why this server sends
//! **no** token for `int`, `return`, or a string literal: a client that already lexes C++ draws keywords and
//! literals itself, and two answers for one span is a disagreement the user would see as flicker.
//!
//! # The one question this layer answers that the analysis does not
//!
//! **Nothing is published while the index is still reading.** A name a header declares is classified *through* the
//! index, so before that header has been read the name has no classification — and an answer now would be a
//! *shorter* list, which this protocol's client is entitled to keep until the next edit. The protocol has a word for
//! "ask again later", which is `null`, and that is what [`on_semantic_tokens`] answers. The cost is that a file
//! being edited while the project is still being read shows no colours until the drain finishes; the alternative
//! costs a client that shows the wrong ones.

use cpp_code_analysis::semantic::{Name, NameKind};

use cpp_code_analysis::{FileView, VfsFile};
use lsp_types::{
    ClientCapabilities, SemanticToken, SemanticTokenType, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensResult, SemanticTokensServerCapabilities, ServerCapabilities,
};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot, snapshot_query};
use crate::util::uri_to_file_path;

mod builder;

pub use builder::{Builder, Modifier, TypeKind};

/// The legend, as advertised in `initialize` and as the encoding numbers it — see [`Builder::legend`].
fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: Builder::legend(),
        token_modifiers: Modifier::all_modifiers(),
    }
}

/// **What a classified name is drawn as** — the one place the analysis's vocabulary meets the protocol's.
///
/// `emmylua_ls` splits its own answer three ways where this has one: a `TypeDef` of kind Class/Enum/Alias, a
/// function member against a property, and a fallback that guesses from the spelling. The first two are
/// subdivisions C++ does not need — a class and an alias are both a `type` here, and a data member and a member
/// function are told apart by the analysis rather than guessed — but the **third** is worth naming as absent on
/// purpose: `default_identifier_token_type` colours an unknown capitalised name as a class, and this server sends
/// nothing for a name it cannot place (see the module note on what is not sent). A guess is a colour the analysis
/// did not make.
fn type_of_a_name_kind(kind: NameKind) -> Option<TypeKind> {
    Some(match kind {
        NameKind::Namespace => TypeKind::Namespace,
        NameKind::Type => TypeKind::Type,
        NameKind::TypeParameter => TypeKind::TypeParameter,
        NameKind::EnumMember => TypeKind::EnumMember,
        NameKind::Function => TypeKind::Function,
        NameKind::Method => TypeKind::Method,
        NameKind::Variable => TypeKind::Variable,
        // **A custom type**, and the one place this server sends one — see [`TypeKind::VariableTemplate`]. A client
        // that cannot draw it is answered with `variable` by [`push_names`], which is what it showed before.
        NameKind::VariableTemplate => TypeKind::VariableTemplate,
        NameKind::Parameter => TypeKind::Parameter,
        NameKind::Macro => TypeKind::Macro,
    })
}

/// A name's modifiers, as the bitflag the legend indexes — see [`Modifier`].
fn modifiers_of(name: &Name) -> Modifier {
    use cpp_code_analysis::semantic::NameModifier;

    let mut bits = Modifier::none();
    if name.declaration {
        bits |= Modifier::DECLARATION;
    }
    for modifier in &name.modifiers {
        bits |= match modifier {
            NameModifier::ReadOnly => Modifier::READONLY,
            NameModifier::Static => Modifier::STATIC,
            NameModifier::Deprecated => Modifier::DEPRECATED,
        };
    }
    bits
}

/// **Pass 1 — the analysis's names.** What the file declares and what its names resolve to.
///
/// Run first, and that is the priority: a name the analysis placed knows more about a span than the lexer does, so
/// it takes the offset and the keyword pass below finds it claimed. See [`Builder`] for why "first claim wins" is
/// the whole overlap rule.
fn push_names(builder: &mut Builder<'_>, names: &[Name]) {
    for name in names {
        let Some(kind) = type_of_a_name_kind(name.kind) else {
            continue;
        };
        let pushed = builder.push_range(
            name.range.start_offset,
            name.range.end_offset(),
            kind,
            modifiers_of(name),
        );

        // **A custom type the client cannot draw falls back to the standard one it can.** The legend this server
        // advertises is its own, so a client only draws the names it lists in `tokenTypes` — and `variableTemplate`
        // is not part of the protocol's standard set. A client that does not know it would otherwise get *no* token
        // for the declaration, which is worse than the colour it had before: it would lose the grammar's own.
        if !pushed && kind == TypeKind::VariableTemplate {
            builder.push_range(
                name.range.start_offset,
                name.range.end_offset(),
                TypeKind::Variable,
                modifiers_of(name),
            );
        }
    }
}

/// **Pass 2 — the lexer's keywords**, which are the tokens no name is.
///
/// `int`, `return`, `const` and `noexcept` are the lexer's; the analysis classifies names rather than punctuation.
/// A file the parser read has these coloured by the client's own grammar, and a file being *edited* does not —
/// `struct`, `template` and `constexpr` are exactly what a partial parse leaves uncoloured, and they are the ones a
/// reader scanning for. So they are sent, and a name that covers one wins the span by having claimed it first.
///
/// The tokens come from the file's **own** token stream rather than from the rendered text: a keyword inside a
/// macro body is not in this file, and one written here is not in the rendering.
fn push_keywords(builder: &mut Builder<'_>, view: Option<&FileView>) {
    let Some(view) = view else {
        return;
    };

    for token in view.tree.get_tokens() {
        // **The kind is asked first, and the text only where the kind cannot answer.** A file has thousands of
        // tokens and the keywords are a minority of them: measured on the standard library's `<algorithm>`, 4 100
        // tokens of which 700 are keywords. The first version fetched every token's text before looking at its
        // kind, so 3 400 tokens paid for a `&str` to answer a question their kind had already answered, and every
        // identifier was handed to `is_contextual_keyword` on the off chance it was `requires`.
        let drawn = match token.kind {
            // The lexer deliberately leaves these as identifiers — `int requires;` compiles — so the **spelling**
            // is the only thing that can tell a keyword from a name, and only these tokens need it.
            cpp_parser::CppTokenKind::Identifier => {
                let Some(text) = view.get_token_text(token.range) else {
                    continue;
                };
                if !is_contextual_keyword(text) {
                    continue;
                }
                TypeKind::Keyword
            }
            // **`bool` shares its kind with `true`/`false`** — the lexer's table says `"bool" => BoolLiteral` and
            // the literals have kinds of their own only for `true`/`false`, so this is the one lexer keyword whose
            // colour lives in its spelling: the type is drawn as a type and the literals as values.
            cpp_parser::CppTokenKind::BoolLiteral => match view.get_token_text(token.range) {
                Some("bool") => TypeKind::Type,
                _ => TypeKind::Variable,
            },
            other if cpp_parser::is_keyword(other) => {
                kind_of_keyword(other).unwrap_or(TypeKind::Keyword)
            }
            // Punctuation, literals, comments: not this pass's business.
            _ => continue,
        };

        builder.push_range(
            token.range.start_offset,
            token.range.end_offset(),
            drawn,
            Modifier::none(),
        );
    }
}

/// **Pass 2b — a module declaration's own words**, which are neither the lexer's keywords nor names the analysis
/// classifies.
///
/// `module` and `import` are **context-sensitive keywords**: `int import = 1;` is a declaration of a variable called
/// `import`, so the lexer leaves them as identifiers and the grammar matches them by text. Colouring them by spelling
/// would underline that variable, which is why they are not in [`is_contextual_keyword`]. The **tree** knows the
/// difference, and this pass asks it: a token inside a `ModuleDecl` or an `ImportDecl` is that declaration's word.
///
/// ```text
/// module / import / export          keyword    the reader's reading of them
/// the module name after them        namespace  `math` in `import math;` names a scope, and a theme draws a
///                                              qualified-name root as one — the same colour as `std`
/// ```
///
/// Reported from an editor as "the module syntax has no colour at all": every token of `module;`, `export module
/// math;` and `import math;` was left to the client's TextMate grammar, which does not know C++20 modules.
fn push_module_declarations(builder: &mut Builder<'_>, view: Option<&FileView>) {
    let Some(view) = view else {
        return;
    };

    // **The directives first, then the tokens inside them.** A module file has one or two, so this walks a line or
    // two rather than the file. The first version walked every token and asked each one's ancestors which node it
    // was under — which is proportional to the file, and on a header that imports nothing (`<algorithm>`, 4 100
    // tokens) it was 4 100 ancestor walks to colour nothing at all.
    for node in view.root.descendants() {
        let directive = cpp_parser::CppSyntaxKind::from(node.kind());
        if !matches!(
            directive,
            cpp_parser::CppSyntaxKind::ModuleDecl | cpp_parser::CppSyntaxKind::ImportDecl
        ) {
            continue;
        }

        for element in node.descendants_with_tokens() {
            let Some(token) = element.into_token() else {
                continue;
            };
            let range = cpp_parser::source_range(token.text_range());
            let Some(text) = view.get_token_text(range) else {
                continue;
            };

            // The name is the token under a `ModuleName`, which is the only child that is a module's name: a
            // **header unit** (`import <vector>;`) has no `ModuleName`, so nothing inside it is coloured here.
            let in_a_module_name = token.parent().is_some_and(|parent| {
                cpp_parser::CppSyntaxKind::from(parent.kind())
                    == cpp_parser::CppSyntaxKind::ModuleName
            });

            let drawn = match text {
                "module" | "import" | "export" => TypeKind::Keyword,
                // A dotted module name is one `ModuleName` node whose parts are identifiers, and every part is part
                // of the name: `std.compat` is two segments of one module.
                _ if in_a_module_name
                    && token.kind() == cpp_parser::CppTokenKind::Identifier.into() =>
                {
                    TypeKind::Namespace
                }
                _ => continue,
            };

            builder.push_range(
                range.start_offset,
                range.end_offset(),
                drawn,
                Modifier::none(),
            );
        }
    }
}

/// The tokens, as the protocol's delta walk — see the module documentation for why nothing may be skipped or
/// reordered here.
///
/// `drawable` is the client's list of supported types, or `None` for "it said nothing", in which case everything
/// this server can say is sent.
///
/// `multi_line_support` is the client's own capability: a token whose range spans lines is sent whole to a client
/// that can draw one and split into one token per line for a client that cannot.
///
/// `view` is the file being drawn, and `None` means **names only** — which is what a caller with no parse in hand
/// has, and what the tests of the walk itself use. Keywords come from the view's own token stream, so a caller
/// without one sends classified names and nothing else, which is what this handler did before keywords were sent.
pub fn encode(
    file: &VfsFile,
    view: Option<&FileView>,
    names: &[Name],
    drawable: &Option<Vec<SemanticTokenType>>,
    multi_line_support: bool,
) -> Vec<SemanticToken> {
    let mut builder = Builder::new(file, drawable.as_deref(), multi_line_support);
    push_names(&mut builder, names);
    push_keywords(&mut builder, view);
    push_module_declarations(&mut builder, view);
    push_doc_commands(&mut builder, view);

    if builder.unplaceable() > 0 {
        log::debug!(
            "{} claim(s) dropped: the file cannot place their offsets",
            builder.unplaceable()
        );
    }

    builder.build()
}

/// **A word that is a keyword in this position but an ordinary identifier in another.**
///
/// The lexer does not make these keywords, and it is right not to: `requires` and `concept` are legal names —
/// `int requires;` compiles, and so does a variable called `concept` — so a lexer that reserved them would change
/// what a file means. The parser matches them **by text**, and [`cpp_parser::is_keyword`] says why in the same
/// words.
///
/// A highlighter is the other kind of consumer, and it wants the *reader's* answer rather than the grammar's: in
/// `template <class T> concept C = requires(T t) { t.f(); };` nobody is reading `concept` or `requires` as a name.
/// So the two are separated here, on the same list the grammar uses, and only for colour.
///
/// # Why the list is short
///
/// These are the words a C++20 reader meets constantly and that no reasonable code uses as identifiers. `module`
/// and `import` are deliberately absent although they are the same kind of word: `import` is a common variable
/// name in code that parses some other format, and underlining it as a keyword would be the false positive this
/// layer is written against. `final` and `override` are absent for the same reason — `final` is a routine method
/// name in code that predates C++11.
///
/// `consteval` and `constinit` are **not** here: the lexer already reserves them, so they arrive as keywords.
fn is_contextual_keyword(text: &str) -> bool {
    matches!(text, "requires" | "concept")
}

/// **What a keyword is drawn as** — which is not one answer, and that is the correction.
///
/// Reported from an editor: *"these are rendered as keywords, but keywords differ and should not share a colour —
/// `this` should be the same colour as `bool`"*. Every keyword was sent as `keyword`, so `bool`, `const`,
/// `constexpr`, `this`, `return` and `class` were one shade. A reader scans for the difference between them: a type,
/// a qualifier and a statement keyword are three different things to look at, and a theme colours them apart for
/// exactly that reason.
///
/// The classification is the C++ grammar's own, not a list of favourites:
///
/// ```text
/// Type      `bool` `char` `int` `void` …          names a type; a theme draws it as one
/// Modifier  `const` `constexpr` `static` `virtual`  qualifies a declaration
/// Variable  `this` `nullptr` `true` `false`         a value, not a word about the syntax around it
/// Keyword   everything else — `return`, `class`, `for`, `sizeof`
/// ```
///
/// # Why `this` is a variable and `nullptr` is too
///
/// They are *values*, and the theme's `variable` colour is what a reader's eye already reads a value with. Drawing
/// them as keywords says they are part of the syntax the way `return` is, which is the opposite of what they are:
/// `this` is a pointer the expression names, and it is the analogy with `bool` that the report makes.
///
/// # The one word whose kind is not enough
///
/// `bool` is a `BoolLiteral` in this lexer rather than a keyword of its own — the table says `"bool" =>
/// BoolLiteral` — so the type and the literal share a kind. **`bool` is answered by the caller**, which is the one
/// place that has the spelling in hand: [`push_keywords`] asks the token's text for exactly those two kinds, and
/// this function takes no text at all. Passing the spelling down — which the first version did — meant every caller
/// had to fetch one, and every caller was a loop over a file.
///
/// # What is deliberately not here
///
/// `class`, `struct`, `union`, `enum` introduce a type rather than naming one, and a theme that drew `struct` as a
/// type would be saying something `struct Widget` does not — so they stay keywords.
/// [`cpp_parser::is_type_keyword`] is wider than this question for the same reason, which is why the list is written
/// out rather than subtracted from it.
///
/// **`auto`, `decltype` and `typename` are modifiers, not types.** They are spellings in a declaration's specifier
/// sequence — `auto x = f();`, `decltype(a) b;`, `typename T::type c;` — so a reader sees them as part of *how* the
/// declaration is written, which is what a theme's `modifier` colour means. Calling them types would be the mistake
/// the note above refuses for `struct`: neither names a type, both stand for one.
///
/// **`final` and `override` are not here at all.** The lexer leaves them identifiers because code that predates
/// C++11 uses them as names ([`is_contextual_keyword`] says the same about `requires`), and reserving them for
/// colour would underline a variable called `final`.
fn kind_of_keyword(kind: cpp_parser::CppTokenKind) -> Option<TypeKind> {
    use cpp_parser::CppTokenKind;

    Some(match kind {
        CppTokenKind::CharKeyword
        | CppTokenKind::ShortKeyword
        | CppTokenKind::IntKeyword
        | CppTokenKind::LongKeyword
        | CppTokenKind::SignedKeyword
        | CppTokenKind::UnsignedKeyword
        | CppTokenKind::FloatKeyword
        | CppTokenKind::DoubleKeyword
        | CppTokenKind::VoidKeyword => TypeKind::Type,

        // A value the expression names rather than a word about the syntax around it.
        CppTokenKind::ThisKeyword | CppTokenKind::NullptrKeyword => TypeKind::Variable,

        // `true` and `false` are the same `BoolLiteral` kind the type shares, handled above.
        CppTokenKind::TrueKeyword | CppTokenKind::FalseKeyword => TypeKind::Variable,

        // **A qualifier is not a statement keyword.** `constexpr` in particular is written constantly in modern
        // C++ and reads as a *property of the declaration*, which is what a theme's `modifier` colour is for.
        CppTokenKind::ConstKeyword
        | CppTokenKind::ConstexprKeyword
        | CppTokenKind::ConstevalKeyword
        | CppTokenKind::ConstinitKeyword
        | CppTokenKind::VolatileKeyword
        | CppTokenKind::MutableKeyword
        | CppTokenKind::StaticKeyword
        | CppTokenKind::ExternKeyword
        | CppTokenKind::RegisterKeyword
        | CppTokenKind::ThreadLocalKeyword
        | CppTokenKind::InlineKeyword
        | CppTokenKind::VirtualKeyword
        | CppTokenKind::ExplicitKeyword
        | CppTokenKind::FriendKeyword
        | CppTokenKind::TypenameKeyword
        | CppTokenKind::AutoKeyword
        | CppTokenKind::DecltypeKeyword
        | CppTokenKind::NoexceptKeyword => TypeKind::Modifier,

        // Everything else: `return`, `if`, `for`, `class`, `sizeof`, `new`, `template` …  — the words that are
        // about the syntax itself. `None` for a kind this list does not name, and the caller decides.
        _ => return None,
    })
}

/// **Pass 3 — the Doxygen commands**, which the parser already reads and nothing was drawing.
///
/// ```cpp
/// /// @param x the x coordinate
/// /// ^^^^^^
/// ```
///
/// The grammar has had these since `docs.rs` was written: a `DocCommand` node with a `DocCommandName` token, and a
/// table of the commands Doxygen knows. **No consumer was reading them.** A doc comment is one colour to a
/// highlighter, so `@param` and the prose after it look the same, and the markers a reader scans a header's
/// documentation for are the ones that do not stand out.
///
/// # Why this is the server's and not the client's
///
/// A client could find `@word` in a comment with a regular expression, and that is the wrong place for it: which
/// comments *are* documentation (`///`, `/** */`, and not `//`), where a command ends and its argument begins, and
/// which words are commands at all rather than prose that happens to start with `@` — all of that is what the
/// parser decided, and re-deciding it in TypeScript would be a second implementation of a question this repository
/// has already answered once. The client's half is *how it looks*, which is a theme's business and is where the VS
/// Code extension remaps it.
///
/// The command's own token is the whole `@param` and not the `param` the parser stores: `DocCommandName` is the
/// name **without its introducer**, and the introducer is half of what a reader recognises.
fn push_doc_commands(builder: &mut Builder<'_>, view: Option<&FileView>) {
    use cpp_parser::{CppAstNode, CppAstToken, CppDocCommand, CppSyntaxKind};

    let Some(view) = view else {
        return;
    };
    let source: &str = view.source.as_ref();

    for node in view.root.descendants() {
        if CppSyntaxKind::from(node.kind()) != CppSyntaxKind::DocCommand {
            continue;
        }
        let Some(command) = CppDocCommand::cast(node) else {
            continue;
        };
        let Some(name) = command.get_name_token() else {
            continue;
        };

        let range = name.syntax().text_range();
        let start = usize::from(range.start());
        let mut end = usize::from(range.end());

        // The introducer, which is the character just before the name: `@` or `\`. Widened by one only when that
        // character really is there — a command written without one is a command this grammar accepted, and
        // claiming the character before it would take the space or the comment's own text.
        if start > 0 && matches!(source.as_bytes().get(start - 1), Some(b'@' | b'\\')) {
            end = end.max(start);
            let widened = start - 1;
            builder.push_range(widened, end, TypeKind::DocCommand, Modifier::none());
            continue;
        }

        builder.push_range(start, end, TypeKind::DocCommand, Modifier::none());
    }
}

/// **The two things this layer must decide that the analysis does not**, and they are both about the client.
///
/// Which kinds the client can draw is a capability, and a kind it cannot draw is not sent — its number would map to
/// nothing at the client's end. `None` for "it said nothing", in which case everything is sent: the protocol's own
/// note says a client may render fewer types than the server advertises, so answering with the full legend is
/// honest, while refusing to answer is not.
fn drawable_types(supported: Option<Vec<SemanticTokenType>>) -> Option<Vec<SemanticTokenType>> {
    match supported {
        Some(types) if !types.is_empty() => Some(types),
        _ => None,
    }
}

/// Whether the client can draw a token whose range spans lines, which the builder needs to split one for a client
/// that cannot. Read from the same capability block as [`drawable_types`], and `false` is the safe reading: a
/// split token is more tokens than necessary, while an unsplit one is a token the client draws wrongly.
fn multi_line_support(context: &ServerContextSnapshot) -> bool {
    context.lsp_features().supports_multiline_tokens()
}

pub async fn on_semantic_tokens(
    context: ServerContextSnapshot,
    params: SemanticTokensParams,
    cancel_token: CancellationToken,
) -> RequestOutcome<SemanticTokensResult> {
    let uri = params.text_document.uri;
    // **Which kinds the client can draw**, read before the query so that the closure below is a plain function of
    // the session: the client's capabilities are not part of the analysis, and the legend they filter is fixed.
    let drawable = drawable_types(context.lsp_features().semantic_token_types());
    let multi_line = multi_line_support(&context);

    if let Some(path) = uri_to_file_path(&uri) {
        context.analysis().prepare(&path).await;

        // **And then the query, which no longer waits for the project's index to drain.**
        //
        // The wait was `AnalysisState::settle`, asking whether the *whole project's* indexing queue was empty —
        // because a name a header declares is classified through the index, and before that header has been read the
        // name has no classification. The question that matters is this file's own closure, and
        // `AnalysisState::catch_up` is what reads it. See `docs/status.md` §5.2 — and note that this handler still
        // declines to answer while **its own** file's queue is not drained (see the refusal below), which is a
        // statement about the answer rather than a wait for it.
        context.analysis().catch_up(&path).await;
    }

    snapshot_query(context.analysis(), cancel_token, move |session| {
        let path = uri_to_file_path(&uri)?;
        let view = session.view_of_the_file(&path)?;
        let held = session.files().held(&view.path)?;

        // **No answer while the file's closure is still being read.** A name a header declares is classified
        // through the index, and before it has been read that name has no classification — so an answer now would
        // be a *shorter* list, which a client caches until the next edit. The protocol has a word for "ask again
        // later", and this is it: `null`.
        //
        // **The closure, not the direct includes.** `is_locally_ready` asks whether the file's own includes are
        // read — the question `catch_up` answers — and `std::endl` lives ten headers deeper than `<iostream>`, so a
        // request during startup passed that test and produced a file coloured almost entirely with the client's own
        // grammar: measured from an editor, `std::endl` was drawn as a variable at startup and as a function after
        // any edit. The refresh below is the other half — see `workspace/semanticTokens/refresh` in the idle loop.
        if !session.is_locally_ready(&view.path) || !session.is_closure_ready(&view.path) {
            log::debug!(
                "no semantic tokens yet: {} file(s) are queued, and a name in one of them would be uncoloured",
                session.pending()
            );
            return None;
        }

        let names = session.classified_names(&view);
        Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: encode(held, Some(&view), &names, &drawable, multi_line),
        }))
    })
    .await
}

pub struct SemanticTokenCapabilities;

impl RegisterCapabilities for SemanticTokenCapabilities {
    fn register_capabilities(
        server_capabilities: &mut ServerCapabilities,
        client: &ClientCapabilities,
    ) {
        // **Only if the client asks for the whole file, in the format this server speaks.** The protocol has a
        // second shape (a range at a time, and a `relative`/`delta` encoding) and this server implements neither:
        // advertising a provider for a request nobody will send, or in a format the client cannot read, is a
        // capability that lies.
        let semantic_tokens = client
            .text_document
            .as_ref()
            .and_then(|text_document| text_document.semantic_tokens.as_ref());

        let Some(semantic_tokens) = semantic_tokens else {
            return;
        };
        if semantic_tokens.requests.full.is_none() {
            return;
        }

        server_capabilities.semantic_tokens_provider = Some(
            SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                work_done_progress_options: Default::default(),
                legend: legend(),
                // No `range` provider: a range request would be the same walk over a slice, and the clients
                // that use it are also the ones that ask for the whole file.
                range: Some(false),
                full: Some(SemanticTokensFullOptions::Bool(true)),
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };
    use cpp_parser::SourceRange;

    /// **A template parameter is a `typeParameter`, and a variable template is a `variableTemplate`.**
    ///
    /// Two questions asked from an editor, and both are about what the *server* sends rather than about colour:
    ///
    /// * *"some template parameters are not a reasonable colour"* — a parameter is not a type, and a theme draws
    ///   `typeParameter` separately for exactly that reason: `template <class T>` names a parameter, and drawing it
    ///   as `int` is what `entity.name.type.cpp` means.
    /// * *"is a variable template a type's colour or a variable's?"* — this server first answered **a variable's**,
    ///   on the argument that `is_assignable_v` is a variable and the `template <…>` header changes only how it is
    ///   written. A reader disagreed with the consequence: the bare name of a variable template is **not a value**
    ///   (`is_assignable_v<A, B>` is), so drawing it exactly like a `constexpr bool` says it can be used as one.
    ///
    /// The protocol has no such type, so this is the one place the legend carries a **custom** name —
    /// `variableTemplate` — and the pass falls back to `variable` for a client that does not advertise it, which is
    /// the second half of this test.
    ///
    /// Asserted on the encoded tokens rather than on `classified_names`, because the wire is what a colour comes
    /// from and a mapping in between could still lose it.
    #[test]
    fn a_template_parameter_is_a_type_parameter_and_a_variable_template_is_a_variable_template() {
        let text = "\
template <class T>
constexpr bool is_trivially_destructible_v = __is_trivially_destructible(T);

template <class To, class From>
constexpr bool is_assignable_v = __is_assignable(To, From);
";
        let (session, path) = a_held_file(text);
        let view = session.view_of_the_file(&path).expect("a view");
        let file = session.files().held(&path).expect("the file is held");
        let names = session.classified_names(&view);

        let tokens = encode(file, Some(&view), &names, &None, false);
        let at_type = by_offset(text, &tokens);

        for (word, expected, why) in [
            (
                "T",
                TypeKind::TypeParameter,
                "a template parameter is not a type",
            ),
            ("To", TypeKind::TypeParameter, "and neither is this one"),
            ("From", TypeKind::TypeParameter, "nor this"),
            (
                "is_trivially_destructible_v",
                TypeKind::VariableTemplate,
                "a variable template names a family of values, not one",
            ),
            ("is_assignable_v", TypeKind::VariableTemplate, "and so does this one"),
        ] {
            let at = text[..text.find(word).expect("the fixture writes it")].len();
            assert_eq!(
                at_type.get(&at),
                Some(&index_of(expected)),
                "`{word}`: {why}"
            );
        }

        // **A client that does not advertise the custom type gets `variable`** — the same colour it had before the
        // type existed, rather than no token at all (which would take away the grammar's colour too).
        let standard = Some(vec![
            lsp_types::SemanticTokenType::VARIABLE,
            lsp_types::SemanticTokenType::TYPE_PARAMETER,
        ]);
        let fallback = by_offset(text, &encode(file, Some(&view), &names, &standard, false));
        let at = text[..text
            .find("is_trivially_destructible_v")
            .expect("the fixture writes it")]
            .len();
        assert_eq!(
            fallback.get(&at),
            Some(&index_of(TypeKind::Variable)),
            "a client that cannot draw `variableTemplate` is answered with `variable`"
        );

        // **The declaration is coloured and the uses inside a macro's argument list are not**, which is a separate
        // defect and is recorded rather than asserted away. Measured: `T` in `__is_trivially_destructible(T)` gets no
        // token, because a call to a name the preprocessor knows as a macro is not classified as an expression — so
        // a client draws the declaration as a `typeParameter` and the use by whatever TextMate guessed, which is
        // one name in two colours. The assertion below states the half that works; the other half is the next thing
        // to fix, and it belongs in the analysis layer where the macro is known.
        assert_eq!(
            at_type.get(&text.find("T>").expect("the fixture writes it")),
            Some(&index_of(TypeKind::TypeParameter)),
            "the declaration is a template parameter"
        );
        let in_the_macro = text.rfind("(T)").expect("the fixture writes it") + 1;
        assert_eq!(
            at_type.get(&in_the_macro),
            None,
            "NOT YET: a template parameter written as a macro's argument gets no token"
        );
    }

    fn a_held_file(text: &str) -> (Session<MemoryFiles>, std::path::PathBuf) {
        let files = MemoryFiles::new().with_file("/p/a.cpp", text);
        let providers = SessionFiles::new(OpenDocuments::new(), files);
        let mut session = cpp_code_analysis::Session::with_config(
            "/p",
            providers,
            WatchFilter::new("/p"),
            CompilerConfig::default(),
        );
        session.load("/p/a.cpp");
        (session, std::path::PathBuf::from("/p/a.cpp"))
    }

    /// **How much work one encode is** — a measurement, not an assertion, and `#[ignore]`d because the fixture is a
    /// quarter of a megabyte and the *setup* (a parse, a classification) costs more than the loop it measures.
    ///
    /// ```text
    /// cargo test --release -p cpp_ls --lib -- a_big_file --ignored --nocapture
    /// ```
    ///
    /// It prints the token count, how many of them the keyword pass has to fetch text for, and the time of
    /// `encode`. The first version fetched **every** token's text to decide a question the kind had already
    /// answered, and the line lookup behind each position scanned the file from byte zero: measured on 236 000
    /// bytes and 164 000 tokens, **2.1 s** per encode before and **24 ms** after.
    #[test]
    #[ignore = "a measurement: the fixture is deliberately huge"]
    fn a_big_file_measures_what_a_walk_costs() {
        // **Two shapes, because one of them is not a C++ file.** The first is ordinary code — two thousand small
        // functions in a namespace — and it parses without a diagnostic. The second is the one that made this test
        // 148 seconds: four thousand `for` statements at file scope, which is not a function body, so every one of
        // them is a recovery node. Kept beside the first because the difference between them *is* the finding.
        let mut valid = String::from("namespace demo {\n");
        for index in 0..2000 {
            valid.push_str(&format!(
                "int value_{index}(int a, int b) {{ return a + b; }}\n"
            ));
        }
        valid.push_str("}\n");

        for (name, text) in [
            ("a namespace of 2 000 functions", valid),
            (
                "4 000 `for` statements at file scope",
                std::iter::repeat_n(
                    "for (int i = 0; i < n; ++i) { total += values[i].size(); }\n",
                    4000,
                )
                .collect::<String>(),
            ),
        ] {
            measure_one(name, &text);
        }
    }

    /// One fixture through the three stages a `textDocument/semanticTokens/full` request pays for.
    fn measure_one(name: &str, text: &str) {
        let (session, path) = a_held_file(text);
        // **The three halves timed apart**, because a request pays all of them: the view (a cook and a parse of the
        // whole file), the classification — pass 1's input — and the encode itself.
        let started = std::time::Instant::now();
        let view = session.view_of_the_file(&path).expect("a view");
        let view_time = started.elapsed();
        let file = session.files().held(&path).expect("the file is held");
        let started = std::time::Instant::now();
        let names = session.classified_names(&view);
        let classify_time = started.elapsed();

        let tokens = view.tree.get_tokens().len();
        let mut fetched = 0;
        let mut keywords = 0;
        for token in view.tree.get_tokens() {
            let needs_text = matches!(
                token.kind,
                cpp_parser::CppTokenKind::Identifier | cpp_parser::CppTokenKind::BoolLiteral
            );
            let keyword = cpp_parser::is_keyword(token.kind);
            fetched += usize::from(needs_text && !keyword);
            keywords += usize::from(keyword);
        }

        let started = std::time::Instant::now();
        let rounds = 20;
        let mut drawn = 0;
        for _ in 0..rounds {
            drawn = encode(file, Some(&view), &names, &None, false).len();
        }
        let each = started.elapsed() / rounds;

        eprintln!(
            "PROBE {name}: {} bytes, {tokens} tokens ({keywords} keyword kinds, {fetched} need a text fetch), \
             {drawn} tokens drawn | view {view_time:?}, classification {classify_time:?}, \
             {each:?} per encode",
            text.len()
        );
    }

    /// **Every token the encoding sends, by absolute offset** — decoding the walk the way a client does.
    ///
    /// The deltas are relative, so a test that read them directly would be asserting about arithmetic rather than
    /// about colour. This rebuilds absolute positions from the line starts, which is what a client does with them.
    fn by_offset(
        text: &str,
        tokens: &[lsp_types::SemanticToken],
    ) -> std::collections::HashMap<usize, u32> {
        let mut line_starts = vec![0usize];
        for (at, ch) in text.char_indices() {
            if ch == '\n' {
                line_starts.push(at + 1);
            }
        }

        let mut out = std::collections::HashMap::new();
        let mut line = 0usize;
        let mut character = 0u32;
        for token in tokens {
            line += token.delta_line as usize;
            character = if token.delta_line == 0 {
                character + token.delta_start
            } else {
                token.delta_start
            };
            if let Some(start) = line_starts.get(line) {
                out.insert(start + character as usize, token.token_type);
            }
        }
        out
    }

    /// **The reported file: a constructor is the class's name, and `requires` is a keyword.**
    ///
    /// Reported from an editor, on the `ExpectedVoid` class: the constructors were drawn as `method` — which VS Code
    /// spells `entity.name.function.member`, the colour of a member function — and `requires` was not drawn at all.
    ///
    /// # Why the constructors were methods
    ///
    /// A constructor and a member function are spelled the same way, and the only thing that separates them is that
    /// a constructor's name is its class's — which `sema::scopes`' `function_binding_kind` cannot know while the
    /// class is still being built, and says so in its own note. So every constructor arrived as `BindingKind::Function`
    /// and was coloured as one. The class's name is known once its body has been read, and that is where the kind is
    /// corrected now.
    ///
    /// # Why the parameter's type was one too
    ///
    /// `ExpectedVoid(const ExpectedVoid& other)` writes the class name a second time, and a *use* of that spelling
    /// resolves through the scope chain to whichever binding is nearest — which was the constructor. The two
    /// positions are one question, and answering the first answers both.
    ///
    /// # `requires`
    ///
    /// It is a **contextual** keyword: `int requires;` is a legal declaration, so the lexer leaves it an identifier
    /// and the grammar matches it by text — the same position `final` and `override` are in. A highlighter wants the
    /// reader's answer rather than the grammar's, so it is recognised by text here and sent as a keyword.
    #[test]
    fn a_constructors_name_is_a_type_and_requires_is_a_keyword() {
        let text = "template <class Err>\n\
                    class ExpectedVoid {\n\
                    public:\n\
                    \x20   constexpr ExpectedVoid() {}\n\
                    \x20   ExpectedVoid(const ExpectedVoid& other) = delete;\n\
                    \x20   constexpr ~ExpectedVoid() requires is_trivially_destructible_v<Err> = default;\n\
                    };\n";
        let (session, path) = a_held_file(text);
        let view = session.view_of_the_file(&path).expect("a view");
        let file = session.files().held(&path).expect("the file is held");
        let names = session.classified_names(&view);
        let tokens = encode(file, Some(&view), &names, &None, false);
        let at_type = by_offset(text, &tokens);

        let a_type = index_of(TypeKind::Type);
        let a_keyword = index_of(TypeKind::Keyword);
        let a_parameter = index_of(TypeKind::Parameter);
        let a_type_parameter = index_of(TypeKind::TypeParameter);

        // **Every occurrence of the class's name**, including the three that are constructors and destructors and
        // the one that is a parameter's type. `match_indices` rather than `find`, because the whole point of this
        // test is that the *same* spelling is one colour wherever it appears.
        let written: Vec<usize> = text
            .match_indices("ExpectedVoid")
            .map(|(at, _)| at)
            .collect();
        assert_eq!(
            written.len(),
            5,
            "the class name, three constructors/destructors, and the parameter's type: {written:?}"
        );
        for at in written {
            assert_eq!(
                at_type.get(&at),
                Some(&a_type),
                "`ExpectedVoid` at {at} is the class's name — a constructor and a parameter's type are the class too"
            );
        }

        // The parameter *name* is a parameter, which is a different token from its type.
        let other = text.find("other").expect("the fixture names it");
        assert_eq!(
            at_type.get(&other),
            Some(&a_parameter),
            "the parameter's own name is a parameter"
        );

        // The template parameter, which was already right and must stay so.
        assert_eq!(
            at_type.get(&text.find("Err").expect("the fixture names it")),
            Some(&a_type_parameter)
        );

        // **`requires`**, which is the word a C++20 reader scans for and which arrived uncoloured.
        let requires = text.find("requires").expect("the fixture writes it");
        assert_eq!(
            at_type.get(&requires),
            Some(&a_keyword),
            "`requires` is a keyword to a reader even though the lexer leaves it an identifier"
        );
    }

    /// **`concept`, and the concept's own name.**
    ///
    /// The other half of "concept 系列关键词". `concept` is a contextual keyword for the same reason `requires` is, and
    /// is sent as a keyword for the same reason — a reader scanning a constraints header is looking for it.
    ///
    /// The name it introduces is the part that is *not* a keyword, and it is the part a theme distinguishes: a concept
    /// is a type-level entity, and VS Code's own grammar (`source.cpp`, the C++20 rules) tags `concept` as
    /// `storage.type` and the name after it as `entity.name.type`. That is what this asks: the word is a keyword, the
    /// name it declares is a type — and the name's declaration is the one place the parser and the scope tree already
    /// agree about it.
    #[test]
    fn a_concept_is_a_keyword_and_its_name_is_a_type() {
        // **A real newline.** The first version of this wrote `"…>\nconcept …"` in an ordinary string literal, where
        // `\n` is two characters and not a line break — so the whole thing was one line, the parser recovered by
        // nesting the concept inside the `TemplateDecl`, and the test was asking about a file nobody would write.
        let text = "template <class T>\nconcept UnaryCopyable = requires(T t) { t.f(); };\n";
        let (session, path) = a_held_file(text);
        let view = session.view_of_the_file(&path).expect("a view");
        let file = session.files().held(&path).expect("the file is held");
        let names = session.classified_names(&view);
        let tokens = encode(file, Some(&view), &names, &None, false);
        let at_type = by_offset(text, &tokens);

        let a_keyword = index_of(TypeKind::Keyword);
        let a_type = index_of(TypeKind::Type);

        assert_eq!(
            at_type.get(&text.find("concept").expect("the fixture writes it")),
            Some(&a_keyword),
            "`concept` introduces a concept, it is not one"
        );

        // **The name it declares.** What this colours is whatever a theme draws an entity name with; the assertion is
        // that the analysis has an opinion at all, because "no token" is the client's own TextMate colour and that is
        // what the report was about.
        assert_eq!(
            at_type.get(&text.find("UnaryCopyable").expect("the fixture writes it")),
            Some(&a_type),
            "the name a `concept` declares is the concept, and a theme colours that as a type"
        );

        // And `requires` again, in the position it most often appears.
        assert_eq!(
            at_type.get(&text.find("requires").expect("the fixture writes it")),
            Some(&a_keyword)
        );
    }

    fn name(start: usize, length: usize, kind: NameKind, declaration: bool) -> Name {
        Name {
            range: SourceRange::new(start, length),
            kind,
            declaration,
            // Empty: this helper builds a name with **no modifier**, which is the shape the encoder's own tests ask
            // about. The modifier half is checked by the tests that go through `Session::classified_names`.
            modifiers: Vec::new(),
            provenance: cpp_code_analysis::semantic::Provenance::DeclaredHere,
        }
    }

    /// **The walk, on tokens that straddle lines.** The deltas are relative — to the previous token on the same
    /// line, and to the line's start on a new one — so a client reconstructing positions from them is the only
    /// check that matters, and it is done here by rebuilding the absolute positions.
    #[test]
    fn the_encoding_is_a_walk_a_client_can_rebuild() {
        let text = "int alpha;\nint beta;\n    int gamma;\n";
        let (session, path) = a_held_file(text);
        let file = session.files().held(&path).expect("the file is held");

        // The offsets come from the text rather than from arithmetic: a hand-counted offset is a test that fails
        // when the fixture changes by one space, and the failure says nothing about the encoding.
        let at = |spelling: &str| {
            let start = text.find(spelling).expect("the fixture writes it");
            name(start, spelling.len(), NameKind::Variable, true)
        };
        let names = vec![at("alpha"), at("beta"), at("gamma")];
        let tokens = encode(file, None, &names, &None, false);

        let mut line = 0u32;
        let mut character = 0u32;
        let rebuilt: Vec<(u32, u32, u32)> = tokens
            .iter()
            .map(|token| {
                if token.delta_line == 0 {
                    character += token.delta_start;
                } else {
                    line += token.delta_line;
                    character = token.delta_start;
                }
                (line, character, token.length)
            })
            .collect();

        assert_eq!(
            rebuilt,
            vec![(0, 4, 5), (1, 4, 4), (2, 8, 5)],
            "the client's own reconstruction: {tokens:?}"
        );
        assert!(
            tokens
                .iter()
                .all(|token| token.token_type == index_of(TypeKind::Variable)),
            "every one is a variable"
        );
        assert!(
            tokens.iter().all(|token| token.token_modifiers_bitset == 1),
            "and a declaration"
        );
    }

    /// A kind the client cannot draw is left out — and leaving it out must not disturb the walk for the tokens
    /// around it, which is the trap: the skipped token's position is what the next delta would have been measured
    /// against.
    #[test]
    fn a_kind_the_client_cannot_draw_is_skipped_without_moving_the_others() {
        let text = "int alpha;\nnamespace ns { }\nint beta;\n";
        let (session, path) = a_held_file(text);
        let file = session.files().held(&path).expect("the file is held");

        let variables = || {
            ["alpha", "beta"].map(|spelling| {
                let start = text.find(spelling).expect("the fixture writes it");
                name(start, spelling.len(), NameKind::Variable, true)
            })
        };
        let namespace = {
            let start = text.find("namespace").expect("the fixture writes it");
            name(start, "namespace".len(), NameKind::Namespace, true)
        };

        let mut names = variables().to_vec();
        names.insert(1, namespace);

        // With everything drawable: three tokens.
        assert_eq!(encode(file, None, &names, &None, false).len(), 3);

        // With namespaces unsupported: two, and the second variable's delta is measured from the *first variable*
        // — the skipped token is not a step in the walk.
        let drawable = Some(vec![SemanticTokenType::VARIABLE]);
        let tokens = encode(file, None, &names, &drawable, false);
        assert_eq!(tokens.len(), 2);
        assert_eq!((tokens[0].delta_line, tokens[0].delta_start), (0, 4));
        assert_eq!(
            (tokens[1].delta_line, tokens[1].delta_start),
            (2, 4),
            "the namespace's token is not a step: {tokens:?}"
        );
    }

    /// A token whose position the file cannot place is dropped rather than guessed — and the walk stays valid for
    /// everything after it.
    #[test]
    fn a_token_past_the_end_of_the_file_is_dropped() {
        let text = "int alpha;\n";
        let (session, path) = a_held_file(text);
        let file = session.files().held(&path).expect("the file is held");

        let names = vec![
            name(4, 5, NameKind::Variable, true),
            name(text.len() + 40, 4, NameKind::Variable, true),
        ];

        assert_eq!(encode(file, None, &names, &None, false).len(), 1);
    }

    /// **The advertised legend and the numbers in an answer are one list**, and this is the assertion that keeps
    /// them so.
    ///
    /// The correction `emmylua_ls` makes and this now follows: a number is a **property of the type**
    /// ([`TypeKind::to_u32`]) and the legend is built from the same list ([`Builder::legend`]), so the two cannot
    /// drift. It used to be a hand-written `index` beside a hand-written list, with this test as the only thing
    /// holding them together — which is a bug waiting for the day someone adds a type and forgets the number. Now
    /// the property is structural and the test states it rather than enforcing it.
    #[test]
    fn the_advertised_legend_is_the_list_the_numbers_index() {
        let advertised = legend();

        // Every type the encoder can send is advertised, **at its own number** — the whole invariant, in one loop.
        for kind in TypeKind::ALL {
            let position = kind.to_u32() as usize;
            assert!(
                position < advertised.token_types.len(),
                "{kind:?} is numbered {position}, past the legend's {} entries",
                advertised.token_types.len()
            );
            assert_eq!(
                advertised.token_types[position],
                kind.to_semantic_token_type(),
                "the type at {position} is the one that number means"
            );
        }
        assert_eq!(advertised.token_types.len(), TypeKind::ALL.len());

        // The same for the modifiers: the bit a claim sets has to index the modifier the client was told about.
        assert_eq!(advertised.token_modifiers, Modifier::all_modifiers());
        assert_eq!(advertised.token_modifiers.len(), Modifier::ALL.len());

        // **Every kind the analysis can produce has somewhere to be drawn.** A kind with no `TypeKind` is a name
        // the analysis resolved and this layer silently drops, which is the failure this list exists to catch.
        for kind in [
            NameKind::Namespace,
            NameKind::Type,
            NameKind::TypeParameter,
            NameKind::EnumMember,
            NameKind::Function,
            NameKind::Method,
            NameKind::Variable,
            NameKind::Parameter,
            NameKind::Macro,
        ] {
            assert!(
                type_of_a_name_kind(kind).is_some(),
                "{kind:?} is classified by the analysis and has no colour"
            );
        }
    }

    /// **The two invariants a client cannot recover from**, asserted on a real file rather than argued for.
    ///
    /// Every token must be **in order** and **non-overlapping**. A client rebuilds absolute positions from the
    /// deltas alone, so one token out of order does not shift that token — it shifts *every token after it*, and
    /// the whole file's colours move. An overlap is the same failure with a different cause.
    ///
    /// This was the reason the builder exists (see its module documentation): the rule used to be enforced by a
    /// hand-written scan at each call site, which is one place to get it wrong per caller. Now the builder decides
    /// it — first claim wins — and this test asks the *answer* rather than any one caller, so it covers a caller
    /// that does not exist yet.
    #[test]
    fn every_answer_is_ordered_and_non_overlapping() {
        // A file with the collisions that make the rule hard: `operator` is a keyword that begins the name
        // `operator+`, `struct` is a keyword immediately before the type it introduces, and a parameter's type and
        // its name are two tokens written next to each other.
        let text = "struct Widget { int size; };\n\
                    Widget operator+(const Widget& left, const Widget& right);\n\
                    int f(Widget& w) { return w.size; }\n";
        let (session, path) = a_held_file(text);
        let view = session.view_of_the_file(&path).expect("a view");
        let file = session.files().held(&path).expect("the file is held");
        let names = session.classified_names(&view);
        let tokens = encode(file, Some(&view), &names, &None, false);

        assert!(!tokens.is_empty(), "the fixture must produce tokens");

        let mut line = 0u32;
        let mut character = 0u32;
        let mut previous_end: Option<(u32, u32)> = None;
        for (at, token) in tokens.iter().enumerate() {
            line += token.delta_line;
            character = if token.delta_line == 0 {
                character + token.delta_start
            } else {
                token.delta_start
            };

            assert!(
                token.length > 0,
                "token {at} has no length, which the protocol does not allow: {token:?}"
            );
            assert!(
                token.token_type < TypeKind::ALL.len() as u32,
                "token {at} names a type past the legend: {token:?}"
            );

            if let Some((previous_line, previous_column)) = previous_end {
                let ordered =
                    line > previous_line || (line == previous_line && character >= previous_column);
                assert!(
                    ordered,
                    "token {at} at {line}:{character} comes before the previous one ends at \
                     {previous_line}:{previous_column}"
                );
            }

            previous_end = Some((line, character + token.length));
        }
    }

    /// **A token that spans lines is split for a client that cannot draw one, and sent whole to a client that can.**
    ///
    /// The protocol has no way to say "this continues below", so a client without `multilineTokenSupport` would
    /// draw a spanning token as a run of characters on the wrong line. `emmylua_ls` splits it into one token per
    /// line — the first from its column to the end of that line, the last from the start of its line — and this
    /// follows, with the line's real length taken from the file's own index.
    ///
    /// A **raw string** is the fixture because that is what spans lines in C++ and is one token: a comment or a
    /// string literal written across several lines. Block comments are how a reader most often meets this, and a
    /// block comment is not a semantic token here — so the fixture is a string, which the analysis does not
    /// classify either… which is why this test drives the builder **directly** rather than through `encode`: what
    /// is under test is the split, and a claim is the only input it needs.
    #[test]
    fn a_spanning_token_is_split_for_a_client_that_cannot_draw_one() {
        // Three lines, counted from the file's start: `/* one` is line 1, `two` is 2, `three */` is 3.
        let text = "int a;\n/* one\ntwo\nthree */\nint b;\n";
        let (session, path) = a_held_file(text);
        let file = session.files().held(&path).expect("the file is held");

        let start = text.find("/*").expect("the fixture writes it");
        let end = text.find("*/").expect("the fixture writes it") + 2;

        // **Sent whole** to a client that can draw one: one token, beginning on the comment's own line.
        let mut whole = Builder::new(file, None, true);
        assert!(whole.push_range(start, end, TypeKind::Property, Modifier::none()));
        let whole = whole.build();
        assert_eq!(whole.len(), 1, "one claim, one token: {whole:?}");
        assert_eq!(
            whole[0].delta_line, 1,
            "the comment begins on the second line of the file: {whole:?}"
        );

        // **Split** for a client that cannot: one token per line it covers, which is three.
        let mut split = Builder::new(file, None, false);
        assert!(split.push_range(start, end, TypeKind::Property, Modifier::none()));
        let split = split.build();
        assert_eq!(
            split.len(),
            3,
            "the comment covers three lines and each gets its own token: {split:?}"
        );

        assert_eq!(
            (split[0].delta_line, split[0].delta_start),
            (1, 0),
            "the first piece is where the claim begins"
        );
        for (at, token) in split.iter().enumerate().skip(1) {
            assert_eq!(
                (token.delta_line, token.delta_start),
                (1, 0),
                "piece {at} begins at the start of the next line"
            );
        }

        // **The middle lines are whole lines and the first is not.** `/* one` is the comment's first six columns,
        // `two` is that whole line, and `three */` ends at column 8 — the pieces are the *lines*, not the token's
        // columns repeated on each of them, which is the shape a naive split gets wrong.
        assert_eq!(split[0].length, 6, "`/* one` is six columns");
        assert_eq!(split[1].length, 3, "`two` is the whole of its line");
        assert_eq!(split[2].length, 8, "`three */` ends at column 8");
    }

    /// **A Doxygen command is drawn, and the introducer is part of it.**
    ///
    /// The parser has read these since `docs.rs` was written — a `DocCommand` node with a `DocCommandName` token and a
    /// table of the commands Doxygen knows — and **no consumer was reading them**, so a doc comment was one colour to
    /// a highlighter and the markers a reader scans a header for did not stand out.
    ///
    /// The token is the whole `@param` and not the `param` the parser stores: `DocCommandName` is the name **without
    /// its introducer**, and the introducer is half of what a reader recognises. Both spellings are written here,
    /// because Doxygen takes both and a highlighter that knew only `@` would leave every `\param` in a real header
    /// undrawn.
    #[test]
    fn a_doxygen_command_is_drawn_with_its_introducer() {
        let text = "/// @brief The width.\n\
                    /// @param x the x coordinate\n\
                    /// \\returns the width\n\
                    int width(int x);\n";
        let (session, path) = a_held_file(text);
        let view = session.view_of_the_file(&path).expect("a view");
        let file = session.files().held(&path).expect("the file is held");
        let names = session.classified_names(&view);
        let tokens = encode(file, Some(&view), &names, &None, false);
        let at_type = by_offset(text, &tokens);

        let a_command = index_of(TypeKind::DocCommand);

        // **Found by `find`, and the `@` is asserted to be inside the span.** The first version of this test looked
        // up the offset of `param` — the *name*, which is what the parser stores — and would have passed on a token
        // that began there and left the `@` uncoloured, which is the half a reader actually sees.
        for introducer in ["@brief", "@param", "\\returns"] {
            let at = text.find(introducer).expect("the fixture writes it");
            assert_eq!(
                at_type.get(&at),
                Some(&a_command),
                "`{introducer}` is a command, introducer and all: {at_type:?}"
            );

            // And the whole of it, not just its first character.
            let after = at + introducer.len();
            assert!(
                at_type.get(&after).is_none_or(|kind| *kind != a_command),
                "the command's token ends after `{introducer}`"
            );
        }

        // **Prose is not a command.** `the x coordinate` follows `@param x`, and drawing a word of it would be the
        // false positive this whole layer is written against.
        let prose = text.find("coordinate").expect("the fixture writes it");
        assert_ne!(
            at_type.get(&prose),
            Some(&a_command),
            "a word of the description is not a command"
        );
    }

    /// **A module declaration's words are drawn, and a variable called `import` is not.**
    ///
    /// `module` and `import` are context-sensitive keywords, so the lexer leaves them identifiers and
    /// [`is_contextual_keyword`] deliberately does not colour them by spelling. The tree knows the difference: a
    /// token inside a `ModuleDecl`/`ImportDecl` is that declaration's word, and everywhere else it is a name. Without
    /// this pass a file of `module;` / `export module math;` / `import math;` had **no colour at all** on those
    /// lines, because the client's TextMate grammar does not know C++20 modules and the analysis sent nothing.
    #[test]
    fn a_module_declaration_is_drawn_and_a_variable_called_import_is_not() {
        let text = "export module math;\nimport math;\nint import = 1;\n";
        let (session, path) = a_held_file(text);
        let view = session.view_of_the_file(&path).expect("a view");
        let file = session.files().held(&path).expect("the file is held");
        let names = session.classified_names(&view);
        let tokens = encode(file, Some(&view), &names, &None, false);
        let at_offset = by_offset(text, &tokens);

        let at = |needle: &str| {
            let start = text.find(needle).expect("the fixture writes it");
            at_offset.get(&start).copied()
        };

        assert_eq!(
            at("export"),
            Some(index_of(TypeKind::Keyword)),
            "`export` in `export module math;`"
        );
        assert_eq!(
            at("module"),
            Some(index_of(TypeKind::Keyword)),
            "the module keyword"
        );
        assert_eq!(
            at("import"),
            Some(index_of(TypeKind::Keyword)),
            "and the import keyword"
        );
        assert_eq!(
            at("math"),
            Some(index_of(TypeKind::Namespace)),
            "the module name is a scope name, which is how a theme draws a qualified-name root"
        );

        // **The same spelling, one line down, is a variable.** `int import = 1;` compiles, and a colour that
        // matched on the word would underline it — the false positive this pass exists to avoid.
        let variable = text.rfind("import").expect("the fixture writes it");
        assert_eq!(
            at_offset.get(&variable).copied(),
            Some(index_of(TypeKind::Variable)),
            "a variable called `import` is a name, not a keyword: {:?}",
            at_offset
        );
    }

    /// A client that asks for semantic tokens but names no type this server uses gets an empty list rather than a
    /// legend it cannot read.
    /// **Keywords do not share one colour**, which is the report this answers.
    ///
    /// *"These are rendered as keywords, but keywords differ and should not share a colour — `this` should be the
    /// same colour as `bool`."* Every keyword used to be sent as `keyword`, so a type, a qualifier, a value and a
    /// statement word were one shade. The classification is [`kind_of_keyword`]'s, and this is the table it promises:
    ///
    /// ```text
    /// bool int char void      a type
    /// const constexpr static  a modifier
    /// this nullptr true false a value — `variable`, which is the analogy the report draws with `bool`
    /// return if class sizeof  a keyword
    /// ```
    ///
    /// The offsets are found with `find` rather than written down, so a fixture edit cannot leave the assertions
    /// pointing at different words.
    #[test]
    fn a_keyword_is_drawn_as_what_it_is() {
        // **`auto` is in the fixture on purpose**: a reader asked for "`auto`, `const` and their kind in the blue a
        // type has, not the keyword colour" — a control-flow word says something about the syntax, while these say
        // something about a declaration, which is what a theme's `modifier` colour is for. `return` and `for` are
        // the other side of the same line.
        let text = "constexpr bool ready() const { this->x = nullptr; return true; }\n\
                    auto y = 1; const int z = 2;\n";
        let (session, path) = a_held_file(text);
        let view = session.view_of_the_file(&path).expect("a view");
        let file = session.files().held(&path).expect("the file is held");
        let names = session.classified_names(&view);
        let tokens = encode(file, Some(&view), &names, &None, false);
        let at_type = by_offset(text, &tokens);

        for (word, expected, why) in [
            ("bool", TypeKind::Type, "`bool` names a type"),
            (
                "constexpr",
                TypeKind::Modifier,
                "a qualifier is a property of the declaration",
            ),
            (
                "this",
                TypeKind::Variable,
                "`this` is a value, and the report asks for it to match `bool`'s *kind* — a type it is not",
            ),
            ("nullptr", TypeKind::Variable, "and so is `nullptr`"),
            (
                "true",
                TypeKind::Variable,
                "`true` is the literal, not the type it shares a token kind with",
            ),
            (
                "return",
                TypeKind::Keyword,
                "`return` is about the syntax, which is what `keyword` means",
            ),
            ("const", TypeKind::Modifier, "the trailing `const` too"),
            (
                "auto",
                TypeKind::Modifier,
                "a declaration's placeholder is a property of the declaration, not a statement keyword",
            ),
            (
                "const int",
                TypeKind::Modifier,
                "and the leading `const` of `const int z`",
            ),
        ] {
            let at = text[..text.find(word).expect("the fixture writes it")].len();
            assert_eq!(
                at_type.get(&at),
                Some(&index_of(expected)),
                "`{word}`: {why}"
            );
        }

        // **`bool` and `true` are one token kind and two colours**, which is why the classification reads the
        // spelling as well as the kind: the lexer's table says `"bool" => BoolLiteral`, so the type and the literal
        // arrive as the same thing.
        let bool_at = text.find("bool").expect("the fixture writes it");
        let true_at = text.rfind("true").expect("the fixture writes it");
        assert_ne!(
            at_type.get(&bool_at),
            at_type.get(&true_at),
            "the type and its literal are not one colour"
        );
    }

    /// **A keyword is sent, and a name that covers it wins the span.**
    ///
    /// The module note used to say keywords are the client's to draw, which is right about a file the parser read and
    /// wrong about the file a reader is editing: `struct`, `constexpr` and `template` are exactly what a partial parse
    /// leaves uncoloured. This checks that they are sent, and that the walk stays ordered and non-overlapping — the
    /// one invariant a client cannot recover from, because an overlap shifts every token after it.
    #[test]
    fn a_keyword_is_coloured_and_nothing_overlaps() {
        let text = "struct Widget { int size; };\n";
        let (session, path) = a_held_file(text);
        let view = session.view_of_the_file(&path).expect("a view");
        let file = session.files().held(&path).expect("the file is held");
        let names = session.classified_names(&view);

        let tokens = encode(file, Some(&view), &names, &None, false);
        // **`int` is a type and `struct` is a keyword**, and this is the test that says which is which: a theme
        // colours `type` and `keyword` differently on purpose, and `int size` should have `int` the colour of a
        // type — the same colour as `Widget`, not the colour of `struct`.
        //
        // The columns are found rather than written down: `text.find` is the one place the offsets are stated, so a
        // fixture edit cannot leave the assertions pointing at different words.
        let at = |word: &str| text.find(word).expect("the fixture writes it") as u32;
        let mut by_column: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        let mut line = 0u32;
        let mut character = 0u32;
        for token in &tokens {
            line += token.delta_line;
            character = if token.delta_line == 0 {
                character + token.delta_start
            } else {
                token.delta_start
            };
            if line == 0 {
                by_column.insert(character, token.token_type);
            }
        }

        let keyword = index_of(TypeKind::Keyword);
        let a_type = index_of(TypeKind::Type);
        assert_eq!(
            by_column.get(&at("struct")),
            Some(&keyword),
            "`struct` introduces a type, it is not one"
        );
        assert_eq!(
            by_column.get(&at("int")),
            Some(&a_type),
            "`int` names a type"
        );
        assert_eq!(
            by_column.get(&at("Widget")),
            Some(&a_type),
            "and so does the class it names"
        );
        assert_ne!(
            keyword, a_type,
            "the two are different colours, which is the point"
        );

        // Every span, rebuilt from the deltas — which is what a client does, and the only way to see a length that
        // disagrees with the text.
        let mut spans = Vec::new();
        let (mut line, mut character) = (0u32, 0u32);
        for token in &tokens {
            line += token.delta_line;
            character = if token.delta_line == 0 {
                character + token.delta_start
            } else {
                token.delta_start
            };
            spans.push((line, character, token.length));
        }

        assert!(
            !spans.iter().any(|(_, _, length)| *length == 0),
            "a zero-length token is invalid in this encoding: {spans:?}"
        );
        // `struct` is first, at line 0 column 0, six characters; `Widget` is a name at column 7, also six.
        assert_eq!(spans[0], (0, 0, 6));
        assert_eq!(
            spans
                .iter()
                .find(|(line, character, _)| *line == 0 && *character == 7)
                .expect("`Widget` is coloured at column 7")
                .2,
            6
        );

        // **No two tokens overlap**, which the deltas alone cannot show.
        for pair in spans.windows(2) {
            let (line, character, length) = pair[0];
            let (next_line, next_character, _) = pair[1];
            assert!(
                next_line > line || next_character >= character + length,
                "tokens overlap: {pair:?}"
            );
        }
    }

    #[test]
    fn a_client_that_supports_none_of_our_types_gets_nothing_to_draw() {
        let text = "int alpha;\n";
        let (session, path) = a_held_file(text);
        let file = session.files().held(&path).expect("the file is held");

        let names = vec![name(4, 5, NameKind::Variable, true)];
        let drawable = Some(vec![SemanticTokenType::COMMENT, SemanticTokenType::STRING]);

        assert!(encode(file, None, &names, &drawable, false).is_empty());
    }

    /// The number the encoding sends for a type, for a test that wants to name it rather than spell it.
    ///
    /// Three lines where it used to be a search of the legend: the number is the type's own — see
    /// [`TypeKind::to_u32`] — so a test that asks for it cannot disagree with the encoder about it.
    fn index_of(kind: TypeKind) -> u32 {
        kind.to_u32()
    }
}
