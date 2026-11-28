//! **Navigation and presentation** — classified tokens, selection ranges, folding, signatures, and outline.
//!
//! Moved here from `session/mod.rs` without a change.

use super::*;
use crate::doc;
use crate::file;
use crate::folding;
use crate::format;
use crate::index;
use crate::sema;
use crate::semantic;
use crate::signature;

impl<F: FileProvider + Clone> Session<F> {
    /// **What each name in this file is** — the classification a semantic highlighter draws colours from.
    ///
    /// Read from the file's own bindings and macro directives, and from the index for a spelling the file does not
    /// declare itself; a name nothing can be said about is left out rather than guessed at. See
    /// [`crate::semantic`] for the four questions, in the order they are asked, and for the measurement behind
    /// asking the index **once per spelling** rather than once per identifier.
    ///
    /// Nothing here is a position question, so the answer does not depend on a cursor — but it does depend on the
    /// index, and a file whose includes have not been read yet answers with **fewer** classifications rather than
    /// with different ones. A caller that publishes these should therefore publish them again once
    /// [`Session::pending`] reaches zero.
    pub fn classified_names(&self, view: &FileView) -> Vec<semantic::Name> {
        use cpp_parser::CppTokenKind;
        let mut names =
            semantic::classified_names_in_family(self.store.index(), view, self.config.language());

        // **The predefined macros, which no file declares.** `__LINE__`, `__FILE__`, `__DATE__`, `__TIME__`,
        // `__cplusplus` and the compiler's own five hundred are defined by the *compiler*, so the walk above — which
        // classifies a name by the declaration it resolves to — has nothing to say about them, and an editor drew
        // `__LINE__` as "standard token type: Other" (reported, with the inspector's own reading). The two tables are
        // this session's: the standard's own (a constant, per language) and the toolchain's `-dM` output.
        //
        // The tokens are the **rendering's**, like the walk's, and this runs before the placement loop for the same
        // reason: a range still in rendering coordinates is what that loop translates.
        let language = self.config_for(&view.path).language_for(&view.path);
        let mut claimed: HashSet<usize> =
            names.iter().map(|name| name.range.start_offset).collect();
        for token in view.tree.get_tokens() {
            if token.kind != CppTokenKind::Identifier {
                continue;
            }
            // A predefined macro is spelled with leading underscores on both sides; nothing else in the file needs the
            // two table lookups below.
            let Some(text) = view.get_token_text(token.range) else {
                continue;
            };
            if !text.starts_with('_') {
                continue;
            }
            let start = token.range.start_offset;
            if claimed.contains(&start) {
                continue;
            }
            // **Three sources, because no one of them holds them all**: the standard's constant macros
            // (`__cplusplus`), the standard's position-dependent ones (`__LINE__`, which `-dM` never prints), and the
            // toolchain's `-dM` table (`__GNUC__`). Reported from an editor: `__LINE__ << __FILE__ << __DATE__ <<
            // __TIME__` was drawn as plain identifiers.
            let is_predefined = the_standards_own_macro(text, &language).is_some()
                || a_macro_whose_value_depends_on_the_use(text)
                || self.predefined_macro(text, &view.path).is_some();
            if !is_predefined {
                continue;
            }
            claimed.insert(start);
            names.push(semantic::Name {
                range: token.range,
                kind: semantic::NameKind::Macro,
                declaration: false,
                modifiers: Vec::new(),
                provenance: semantic::Provenance::Macro,
            });
        }

        // **The names the cook expanded away.** `__LINE__` is a number in the rendering, so the loop above cannot see
        // the name it was written as — and the name is what an editor colours. They are five spellings and they cannot
        // appear except as themselves, so the file's own text is where they are found: a word boundary search over a
        // text that is already in hand, once per file, and only for names nothing has claimed yet.
        if let Some(written) = view.written_text() {
            for name in [
                "__LINE__",
                "__FILE__",
                "__DATE__",
                "__TIME__",
                "__COUNTER__",
            ] {
                let mut from = 0usize;
                while let Some(found) = written[from..].find(name) {
                    let at = from + found;
                    let end = at + name.len();
                    from = end;
                    let a_word_starts_here = at == 0
                        || !written.as_bytes()[at - 1].is_ascii_alphanumeric()
                            && written.as_bytes()[at - 1] != b'_';
                    let a_word_ends_here = end == written.len()
                        || !written.as_bytes()[end].is_ascii_alphanumeric()
                            && written.as_bytes()[end] != b'_';
                    if !a_word_starts_here || !a_word_ends_here {
                        continue;
                    }
                    // **Back into the rendering's coordinates**, because the loop after this one moves every range out
                    // of the rendering and into the file; a range already in the file would be mapped a second time.
                    let Some(reading) = view.reading_offset_of(FileOffset::new(at)) else {
                        continue;
                    };
                    let at = reading.get();
                    if !claimed.insert(at) {
                        continue;
                    }
                    names.push(semantic::Name {
                        range: cpp_parser::SourceRange::new(at, name.len()),
                        kind: semantic::NameKind::Macro,
                        declaration: false,
                        modifiers: Vec::new(),
                        provenance: semantic::Provenance::Macro,
                    });
                }
            }
        }
        // **The ranges are moved out of the rendering and into the file, because that is what both consumers read.**
        //
        // The walk reads `view.root`, so every range it answers is an offset in the **rendering** — and the two
        // consumers of this query take it for an offset in the **file**: the semantic-tokens handler computes the
        // client's line and column with `position_in_file(file, name.range.start_offset)` and measures the token with
        // `&file.text[range]`, and `tests/semantic.rs` slices the fixture's own text the same way. Two independent
        // consumers agreeing is the contract; the query was the side that was wrong, and it is user-visible: a colour
        // drawn at an offset the cook shifted is a colour on **another word**.
        //
        // Measured on the suite: `the_header's_class` read `[("l", Function), ("de \"b.", Type), ("\n", Parameter)]`
        // — the tokens of a rendering sliced out of the file. A name whose start the file cannot place is **dropped**,
        // which is the same choice the handler makes one layer up ("dropping one token is the only option that keeps
        // the walk valid"): a name in a region the rendering does not cover — an untaken `#if` branch, the body of a
        // macro — has no file position at all, and keeping it would put a token at an offset that means something else.
        let mut placed = Vec::with_capacity(names.len());
        for mut name in names {
            let Some(start) = view.file_offset_of(ReadingOffset::new(name.range.start_offset))
            else {
                continue;
            };
            let end = view
                .file_offset_of(ReadingOffset::new(name.range.end_offset()))
                .unwrap_or(start);
            name.range =
                cpp_parser::SourceRange::new(start.get(), end.get().saturating_sub(start.get()));
            placed.push(name);
        }
        names = placed;

        // **And the identifiers only the file's own text has.**
        //
        // The walk iterates `view.tree.get_tokens()` — the **rendering's** tokens — so a name the cook removed is not
        // in it at all: an identifier in an untaken `#if` branch, or in the body of a function-like macro. Those are
        // names the reader is looking at, and the module's own words are that this reads "the file's own bindings and
        // macro directives". `a_macro_in_an_untaken_branch_is_still_a_macro` is the test that says so, and it answered
        // `None` against `Some(Macro)`.
        //
        // A parse of the file's own text sees them, **in the file's coordinates already** — so nothing here is
        // translated, and the two passes meet in one numbering. Deduplicated by where the name starts, with the
        // rendering's answer winning: it is the one that carries the macro and binding facts.
        //
        // The guard is "the two readings differ", because a file the cook did not change needs no second pass — and
        // the cost when they do differ is a second walk, which the `Classify` stage timer in
        // `examples/semantic_probe.rs` measures.
        if view
            .written_text()
            .is_some_and(|written| written != &*view.source)
            && let Some(own) = self.view_of_the_file(&view.path)
        {
            // **Where the two passes disagree about one name, the file's own text wins.** Measured both ways: keeping
            // the rendering's answer for a shared start lost `(Parameter, false)` — a use the cook rewrote — and
            // keeping *both* broke `the_classifications_are_sorted_and_disjoint`. A colour is drawn on the reader's
            // text, so for two answers about one offset the one read from that text is the one to draw.
            let mut at_offset: HashMap<usize, usize> = names
                .iter()
                .enumerate()
                .map(|(index, name)| (name.range.start_offset, index))
                .collect();
            for name in semantic::classified_names_in_family(
                self.store.index(),
                &own,
                self.config.language(),
            ) {
                match at_offset.get(&name.range.start_offset) {
                    Some(&index) => names[index] = name,
                    None => {
                        at_offset.insert(name.range.start_offset, names.len());
                        names.push(name);
                    }
                }
            }
        }

        // **And the symbol references in the file's documentation comments.** A comment is trivia, so the rendering
        // has none of it and neither walk above sees one — the answer is read from the **written** text, in the file's
        // own coordinates, and each target is resolved with the same query a jump and the hover use
        // (`ProjectIndex::definition`), so the colour, the card and the jump cannot disagree about what a reference
        // names.
        //
        // **Both spellings**, through one reader: a `[`Widget::grow`]` link and a `@see Widget::grow` command's
        // argument. They were two features — and the second was not a feature at all, only an argument the completion
        // mistook for a parameter name — until `crate::doc` made them one question.
        //
        // A target that resolves to nothing is **not** coloured, which is the same answer an identifier the analysis
        // cannot place gets: a colour would say the name is known.
        //
        // Read here rather than in the server's token pass because this is the layer with the index in hand: a
        // spelling in a comment has no tree position to resolve from, and the pass that draws tokens has no session.
        {
            let text: &str = view.written_text().unwrap_or(&view.source);
            for target in doc::targets(text) {
                if !doc::is_a_documentation_comment(text, target.range.start_offset) {
                    continue;
                }
                let spelling = target.spelled_for_a_lookup();
                let kind = self
                    .store
                    .index()
                    .definition(spelling, &view.path)
                    .value()
                    .and_then(|found| {
                        semantic::kind_of_index(index::project::IndexedKind::of(&found.fact))
                    })
                    // **A macro is a name too**, and the index holds no *declaration* for one: `[`MAX`]` names a
                    // `#define`, which is a fact about text. The environment the file is compiled in is the same
                    // question the classifier asks for a macro an `#include` brought in, so it is asked rather than
                    // duplicated — and only for a target the index could not describe, which is what keeps a
                    // reference to an ordinary declaration free.
                    .or_else(|| {
                        self.store
                            .index()
                            .macro_names_in_force(&view.path)
                            .contains(spelling)
                            .then_some(semantic::NameKind::Macro)
                    });
                let Some(kind) = kind else {
                    continue;
                };
                names.push(semantic::Name {
                    range: target.range,
                    kind,
                    declaration: false,
                    modifiers: Vec::new(),
                    provenance: semantic::Provenance::Found,
                });
            }
        }

        // **And the code inside a documentation comment's `@code` block**, which is an example rather than prose: its
        // keywords are drawn as keywords and its identifiers resolved the way any other name is. The block is *text*
        // — a comment produces no tokens, so the tree has nothing to say about it — and the lexer that reads it is the
        // same one every other spelling in this project is read by.
        {
            let text: &str = view.written_text().unwrap_or(&view.source);
            for block in doc::code_blocks(text) {
                let body = &text[block.start_offset..block.end_offset()];
                let (tokens, _) = cpp_parser::lex(body, &cpp_parser::LexerConfig::default());
                for token in tokens {
                    let Some(spelling) =
                        body.get(token.range.start_offset..token.range.end_offset())
                    else {
                        continue;
                    };
                    let is_a_keyword = cpp_parser::is_keyword(token.kind);
                    if !is_a_keyword && token.kind != CppTokenKind::Identifier {
                        continue;
                    }
                    let kind = if is_a_keyword {
                        Some(semantic::NameKind::Keyword)
                    } else {
                        self.store
                            .index()
                            .definition(spelling, &view.path)
                            .value()
                            .and_then(|found| {
                                semantic::kind_of_index(index::project::IndexedKind::of(
                                    &found.fact,
                                ))
                            })
                    };
                    let Some(kind) = kind else {
                        continue;
                    };
                    names.push(semantic::Name {
                        range: cpp_parser::SourceRange::new(
                            block.start_offset + token.range.start_offset,
                            token.range.length,
                        ),
                        kind,
                        declaration: false,
                        modifiers: Vec::new(),
                        provenance: semantic::Provenance::Found,
                    });
                }
            }
        }

        // **And re-sorted, because the crossing is not order-preserving.** The walk answers in the rendering's order,
        // and a token a macro produced maps back to *where the macro was invoked* — which can be before a name that
        // preceded it in the rendering. The consumer builds a **delta walk** (`SemanticTokens`, where every token is
        // relative to the one before it), so an out-of-order list does not merely look odd: the deltas would be
        // negative. Sorted here rather than by the caller, because this is where the order stopped being the
        // rendering's.
        names.sort_by_key(|name| (name.range.start_offset, name.range.end_offset()));

        names
    }

    /// **Every format string in this file, with what each part of it is** — see [`crate::format`].
    ///
    /// ```cpp
    /// std::println("值 = {:.3f}", x);    `{:.3f}` is a specifier and `f` a conversion
    /// printf("%-8.2f|%*d", y, w);        a specifier, a conversion, and a width taken from an argument
    /// scanf("%3d %[a-z]", &n, name);     and the scanset
    /// ```
    ///
    /// # Why the *written* reading, and only the written reading
    ///
    /// A colour is painted on the text the reader has in the buffer, and the answers here are **file offsets**. A
    /// rendering's offsets are its own (see [`FileView::file_offset_of`]), and a format string is a *literal*: the cook
    /// never expands one, but it does move everything after the macros it did expand. So this reads
    /// [`Session::view_of_the_file`] and refuses a rendering — the same rule `push_doc_commands` one layer up states
    /// for a comment, and for the same reason.
    ///
    /// # Why the tokens rather than the tree
    ///
    /// The format string is there either way, and where the parser put it is not a question this
    /// pass has an opinion about: the call is "a `(` whose arguments are…", which the token stream
    /// says exactly. See [`crate::format::literals_in`].
    pub fn format_literals(
        &self,
        view: &FileView,
        config: &format::FormatConfig,
    ) -> Vec<format::FormatLiteral> {
        // **A rendering is not this pass's text.** Its tree's tokens are the cook's, and a range in it names a place
        // in a string the reader has never seen.
        if view.written_text().is_some() {
            return Vec::new();
        }

        // **A name the file does not qualify is resolved, and only where it decides something**: `format("…")` after
        // `using namespace std;` is the standard one, and a project's own `format` is not.
        //
        // The instrument is the **binding** and its scope, not `definitions`: that query answers "where do I jump to"
        // and fills `DeclFact::scope` with the qualifier *the cursor wrote*, which for a bare name is no scope at
        // all — measured, a bare `format(…)` resolved to `format` with `scope: None` in both spellings of the using
        // (`using namespace std;` and `using std::format;`). What this needs is the namespace the declaration sits
        // in, which is the scope the binding lives in, and `ScopeTree::qualified_name_of` spells it.
        //
        // A **using-declaration** is the one shape that still answers with the wrong thing: `using std::format;`
        // binds the name to the using-declaration itself (which is where go-to-definition goes), so the spelling
        // resolves to `format` at file scope and is refused. `using namespace std;` is followed properly, and a
        // qualified `std::format(…)` needs neither.
        let mut qualified_name = |offset: usize| -> Option<String> {
            let reading = view.reading_offset_of(FileOffset::new(offset))?;
            let Known::Yes(binding) =
                sema::resolve::definition_at(&view.scopes, &view.root, reading.get())
            else {
                return None;
            };
            let name = binding.name.identifier_text()?;
            Some(match view.scopes.qualified_name_of(binding.scope) {
                Some(scope) if !scope.is_empty() => format!("{scope}::{name}"),
                _ => name.to_string(),
            })
        };

        format::literals_in(
            &view.source,
            view.tree.get_tokens(),
            config,
            &mut qualified_name,
        )
    }

    /// **The ranges to select at a cursor, in the file's own coordinates** — the third fact that belongs to the text
    /// the *reader* is looking at rather than to the text the compiler reads.
    ///
    /// A chain answers in the coordinates of the tree it walks, and every view this session holds is of a
    /// **rendering**. Translating in and out does not repair that: a cursor in whitespace maps *forward* to the next
    /// token — right for a cursor — so a walk of the rendering begins at a token that starts **after** the cursor and
    /// builds a rung that does not contain the position it was built for. Measured: the cursor at file offset 9 in
    /// `namespace ns` produced a chain whose innermost rung began at 10.
    ///
    /// Folding answers the same question the same way for the same reason, and so does documentation for comments.
    /// **Three facts, one rule**: a selection, a fold and a doc comment are things the reader sees in their buffer,
    /// so they are asked of the buffer.
    pub fn selection_chain(
        &self,
        view: &FileView,
        offset: FileOffset,
    ) -> Vec<cpp_parser::SourceRange> {
        let Some(written) = view.written_text() else {
            // No rendering: this view *is* a parse of the file's own text, so its offsets are already the file's.
            return view.selection_chain(ReadingOffset::new(offset.get()));
        };
        let tree = cpp_parser::CppParser::parse(written, cpp_parser::ParserConfig::default());
        file::view::selection_chain_of(&tree.get_red_root(), offset.get())
    }

    /// **The file's foldable regions** — see [`crate::folding`].
    ///
    /// Read from the **buffer's own tokens and directives**, which is the same source the outline uses and for the
    /// same reason: folding is about the file in front of the reader, so an unsaved edit is folded as it is typed,
    /// and a declaration in a branch nobody takes is a region like any other. Nothing here consults the index, the
    /// cooked reading, or any other file — a fold is a fact about this text.
    pub fn folding_ranges(&self, view: &FileView) -> Vec<folding::Fold> {
        // **The file in front of the reader, which is not the text this view parses any more.**
        //
        // The promise above — "the buffer's own tokens and directives" — was free while a view *was* the file's own
        // text: `view.source` and the written text were one string. They stopped being one when the raw parse was
        // deleted and a view became the compiler's reading, and this function was left reading the wrong one. A
        // rendering has no `#include` line, no comment and no `#if` region in it — those are what a preprocessor
        // removes — and it usually has fewer line breaks, so any fold it could still see is thrown away by the rule
        // that a region on one line hides nothing. Measured over the wire: `textDocument/foldingRange` on a
        // four-region fixture answered **`[]`**.
        //
        // Folding is a question about *layout* rather than about meaning — where the braces are, which lines a
        // comment covers, where an `#if` closes — so reading the file's own text is not the "semantic view of
        // unexpanded C++" that was deliberately removed; it is the only text those regions exist in. The lexer runs
        // here rather than being kept because only this query wants these tokens, and only when a client asks.
        let Some(written) = view.written_text() else {
            // No rendering: this view *is* the file's own text, and its tokens are already the right ones.
            return folding::folding_ranges(&view.source, view.tree.get_tokens());
        };
        let (tokens, _) = cpp_parser::lex(written, &cpp_parser::LexerConfig::default());
        folding::folding_ranges(written, &tokens)
    }

    /// **The signatures of the call the cursor is inside** — see [`crate::signature`].
    ///
    /// A list, because a call to an overloaded function has more than one declaration and the protocol's
    /// `SignatureHelp.signatures` is a list: `std::format(` is four of them, and answering one — or none, which is
    /// what this did while the name looked ambiguous — is a popup with nothing in it. The reader picks; this layer
    /// claims nothing about which overload a half-typed argument list means.
    ///
    /// The parameters come from the *callee's* declarations, so a call into a header parses that header for this
    /// answer — **once per file**, however many overloads it holds.
    ///
    /// The documentation above each declaration is read here rather than by the caller: it is the same declaration
    /// the signature came from, and asking twice would be two lookups for one popup.
    /// **The signature of a function-like macro call**, or `None` when the cursor is not inside one.
    ///
    /// A macro is expanded in a rendering, so the call the reader is typing exists only in the file's **own** text —
    /// which is why this takes a view of that text and a **file offset**, and not the rendering-view pair the
    /// function engine takes. `ADD(1, |)` is `((1) + ())` in the rendering: there is no call there to describe.
    ///
    /// A **file offset** rather than a reading offset is the other half of the same fact: mapping a position inside
    /// a macro expansion back to the file is not reliable (the expansion's bytes come from the body, not from the
    /// invocation), and a caller that already has the client's position has the file offset for free.
    pub fn macro_signature_at(
        &self,
        view: &FileView,
        offset: FileOffset,
    ) -> Option<signature::CallSignature> {
        signature::a_macro_signature(self.store.index(), view, offset.get(), &mut |path| {
            self.view(path)
        })
    }

    pub fn signatures_at(
        &self,
        view: &FileView,
        offset: ReadingOffset,
    ) -> Vec<signature::CallSignature> {
        let mut signatures =
            signature::signatures_at(self.store.index(), view, offset.get(), |path| {
                self.view(path)
            });

        for signature in &mut signatures {
            // **The declaration's offset is the *index's*, and the index reads the cooked rendering.** So
            // `declared_at` is a **reading** offset, while `Session::documentation` looks for the comment in the text
            // the reader **wrote** - and the two differ by exactly what the cook removed, which for a documented
            let declared = self.view(&signature.declared_in);
            let Some(in_the_file) = declared.as_ref().and_then(|declared| {
                declared.file_offset_of(ReadingOffset::new(signature.declared_at))
            }) else {
                continue;
            };
            signature.documentation =
                self.documentation(view, &signature.declared_in, in_the_file.get());
        }

        signatures
    }
}
