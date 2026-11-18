//! **Completions and documentation** — the list after a `.`, the names in scope, and the doc comments a hover reads.
//!
//! Moved here from `session/mod.rs` without a change.

use super::super::*;

impl<F: FileProvider + Clone> Session<F> {
    /// What to offer after a member access at `offset`: the object's type, its members, and the edit.
    /// **The cursor is a FILE offset**, as it is for `Session::completions` and `Session::name_completions`: the three
    /// completion queries speak the coordinates a client sends and each crosses the seam itself. This one did not, and
    /// the cost was a panic rather than a wrong list — `rowan` was handed a file offset for a rendering tree.
    pub fn member_completions(
        &self,
        view: &FileView,
        offset: FileOffset,
    ) -> Known<MemberCompletions> {
        let Some(offset) = view.reading_offset_of(offset) else {
            // A cursor the rendering does not have - in a region a macro replaced, or past its end.
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from("")));
        };
        member_completions_at(
            self.store.index(),
            &mut |path| self.view(path),
            &view.scopes,
            &view.root,
            &view.path,
            offset.get(),
        )
    }

    /// What to offer at a cursor with no member access: the scope's names, then the index's.
    pub fn name_completions(&self, view: &FileView, offset: FileOffset) -> Known<NameCompletions> {
        // **The cursor is a FILE offset; the tree is the RENDERING's.** These two coincided while a view was a parse
        // of the file's own text, so no caller had to know they were two things — and the moment `view` became the
        // reading a compiler is handed they stopped coinciding, by exactly the macro expansion. Measured on the
        // completion fixture: `Bad offset: range 0..6 offset 12`, where 6 is the rendering's length (`int x;`) and 12
        // is a position in the 18-byte file.
        //
        // The translation belongs **here** rather than in each handler: every caller holds a view and a cursor, the
        // pair is always the same pair, and a handler that forgot would get a wrong answer rather than an error —
        // which is exactly what happened three times while this boundary went in. See [`FileView`]'s note on the two
        // coordinate systems: an offset in the file goes in through `reading_offset_of`, and a range in the rendering
        // comes back out through `file_offset_of`.
        // **Prose first, and asked of the file's own text.** A comment and a literal are things a rendering has none
        // of, so a cursor in one arrives at the scope walk as an ordinary position and the answer is the file's names,
        // offered into a sentence. Measured: `a_comment_and_a_literal_offer_nothing` failed against this query while
        // `completions` was already refusing.
        if let Some(written) = view.written_text()
            && crate::file::view::in_prose(written, offset.get())
        {
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from("")));
        }

        let Some(offset) = view.reading_offset_of(offset) else {
            // The cursor is not in the rendering at all — in a region a macro replaced, or past its end. There is no
            // name to offer, and `Unknown` says so rather than answering about the nearest thing that is.
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from("")));
        };
        crate::index::project::name_completions_traced(
            self.store.index(),
            &view.scopes,
            &view.root,
            &view.path,
            offset.get(),
        )
    }

    /// **The completion list for a cursor** — the layer that decides *which* question the cursor is asking.
    ///
    /// The two queries above answer "what is visible here" and "what does this type have"; this one reads the
    /// shape at the cursor, picks between them, adds the language's own vocabulary (keywords, snippets, directives,
    /// headers), and orders the result by how near each declaration is to the reader. See
    /// [`crate::completion`] for the whole of the reasoning; the short version is that "every name reachable
    /// through the include graph" is a *correct* answer and a useless one, and the difference is the order.
    ///
    /// # The one thing this needs that the session does not already hold
    ///
    /// An `#include`'s answer is a **file name**, and nothing in the index or the tree knows one: the index holds
    /// the files somebody has read, and a reader typing `#include <chro` wants the header that is on the search
    /// path whether or not any file has included it. So the search path is walked once, when the session is built,
    /// and the project's own headers are listed from [`Session::project_files`] — see
    /// [`crate::completion::header_index`].
    ///
    /// # `is_incomplete` is the caller's to decide
    ///
    /// This returns [`CompletionSet::truncated`](crate::CompletionSet::truncated) — "the budget cut this list" —
    /// and it is deliberately **not** [`Session::pending`]. The two are different claims and a client reacts to them
    /// differently: pending work means "ask me again as you type, a name may be missing for the only reason that
    /// its file has not been read yet", while a capped list means the opposite ("do not ask again, this is the best
    /// I have"). A caller that merged them would leave a client retrying a list that cannot change.
    /// # What the two halves cost
    ///
    /// The header index is **cloned** rather than borrowed — see [`Session::headers`] — and on a project the size of
    /// the latency fixture that clone measures **0 ms**, against 350–1270 ms for the query beside it while the index
    /// was filling. It is named here because it is the obvious suspect for a per-keystroke cost and it is not one.
    pub fn completions(&self, view: &FileView, offset: FileOffset) -> crate::CompletionSet {
        // **The list, then the edits that go with it.** A name offered from a header this file does not include is only
        // usable once the `#include` is there, so every item that carries an [`Inclusion`](crate::completion::Inclusion)
        // gets its insertion here — the position is a line in the **written** text, which the completion itself never
        // sees (it is computed on the rendering).
        let mut offered = self.completions_without_the_edits(view, offset);
        self.attach_the_inclusions(&mut offered, &view.path);
        offered
    }

    /// **The files this file could include to gain a name it does not have** — the completion side of
    /// `missing-include`.
    ///
    /// Only **strongly related** files, which is what keeps the list small and the offer believable: a file in the same
    /// directory, a header or module interface under a `[[library]]` the project names, or one under this file's own
    /// include directories. A file already reached by an `#include` is not a candidate (its names are visible), and
    /// `[completion] suggest_unincluded = false` turns the whole thing off.
    pub(crate) fn the_files_this_file_could_include(
        &self,
        path: &std::path::Path,
    ) -> Vec<crate::completion::Unincluded> {
        if !self
            .project_config()
            .config
            .completion
            .suggest_unincluded
            .unwrap_or(true)
        {
            return Vec::new();
        }
        let include_paths = self.config_for(path).include_paths;
        let reached = self.store.index().include_closure(path);
        let directory = path.parent().map(|parent| parent.to_path_buf());
        let libraries: Vec<PathBuf> = self
            .project_config()
            .config
            .libraries
            .iter()
            .map(|library| library.root(&self.root))
            .collect();

        const MOST: usize = 64;
        let mut found = Vec::new();
        for file in &self.project {
            if found.len() >= MOST {
                break;
            }
            if file == path {
                continue;
            }
            let extension = file
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_lowercase());
            let is_header = matches!(
                extension.as_deref(),
                Some("h" | "hpp" | "hxx" | "h++" | "inl" | "ipp")
            );
            let is_interface = matches!(extension.as_deref(), Some("cppm"));
            if !is_header && !is_interface {
                continue;
            }
            // **Already reached** by an `#include`, transitively: its names are visible, so there is nothing to add.
            let key = crate::file::paths::normalize_path(file, cfg!(windows));
            if reached.contains(&key) {
                continue;
            }
            let related = directory.as_deref() == file.parent()
                || libraries.iter().any(|root| file.starts_with(root))
                || include_paths
                    .iter()
                    .any(|held| file.starts_with(&held.directory));
            if !related {
                continue;
            }
            let inclusion = match self
                .store
                .index()
                .summary(file)
                .and_then(|summary| summary.modules.module.clone())
            {
                Some(module) => crate::completion::Inclusion::Import(module.to_string()),
                None => crate::completion::Inclusion::Include(
                    crate::completion::the_include_spelling(file, path, &include_paths),
                ),
            };
            found.push(crate::completion::Unincluded {
                file: file.clone(),
                inclusion,
            });
        }
        found
    }

    /// **Attach the `#include`/`import` each item carries**, as an edit at the file's include block.
    ///
    /// The completion layer knows *what* to add ([`crate::completion::Inclusion`]) and the session knows the file's
    /// text, which is where the *position* lives — after the last `#include` for one kind, after the module declaration
    /// or last `import` for the other. An item whose inclusion cannot be placed (a file whose text is not the one the
    /// completion was computed on) keeps its name and loses its edit, which is the harmless direction: the reader sees
    /// the name and no include, rather than an edit at an offset that may have moved.
    pub(crate) fn attach_the_inclusions(
        &self,
        offered: &mut crate::CompletionSet,
        path: &std::path::Path,
    ) {
        if !offered.items.iter().any(|item| item.inclusion.is_some()) {
            return;
        }
        let Some(text) = self.vfs.held(path).map(|held| held.text.to_string()) else {
            return;
        };
        let after_includes = after_the_includes(&text);
        let after_imports = after_the_module_imports(&text);
        for item in &mut offered.items {
            let Some(inclusion) = &item.inclusion else {
                continue;
            };
            let at = match inclusion {
                crate::completion::Inclusion::Include(_) => after_includes,
                crate::completion::Inclusion::Import(_) => after_imports,
            };
            item.additional_edits
                .push((cpp_parser::SourceRange::new(at, 0), inclusion.line()));
        }
    }

    /// **The completion before the `#include` edits are attached** — see [`Session::completions`].
    pub(crate) fn completions_without_the_edits(
        &self,
        view: &FileView,
        offset: FileOffset,
    ) -> crate::CompletionSet {
        // **The headers this file could add**, and what accepting one of their names has to edit — see
        // `Session::the_files_this_file_could_include`.
        let unincluded = self.the_files_this_file_could_include(&view.path);
        // **The cursor is a FILE offset; the tree is the RENDERING's** — see [`Session::name_completions`], where the
        // full note is. A cursor that is not in the rendering has nothing to offer, and an empty set is the honest
        // answer rather than a list computed from the nearest position that *is* in it.
        // **The shapes that live in the file''s own text, decided before anything crosses the seam.**
        //
        // A rendering has none of them in it: a `#include` line is replaced by what it includes, a `#define` by
        // nothing, a comment is removed before the parse. So a cursor in one arrives at the context reader as whatever
        // the renderer put in that place - measured, `#inc` reached the tree at offset 0, so the replacement range for
        // `include` came back at the **start of the file**.
        //
        // Their answers need nothing from the reading either, which is why this is a shortcut rather than a special
        // case: directive names are a constant table, a header name comes from the search path, prose has no
        // suggestion to make, and a `#define`''s argument is the program''s names. The offset handed over is the
        // **file''s**, so the replacement range a client edits is in the buffer it is showing.
        //
        // **`offset` here is still the parameter**: one statement lower it is the *reading''s*, and for `"#inc\nint
        // x;\n"` the cursor at file offset 4 maps into the rendering at 0 - the directive is gone - so the written
        // text was asked at 0, before the `#`, and the list came from the vocabulary''s `include` *snippet*.
        // **A module name is the file's own question, asked first.** `import ` names a file, so the answer needs the
        // *project* rather than the reading — and unlike a directive, the context is the same in both readings
        // (`import math;` survives the preprocess untouched), so the file's text is where it is read from. Asked
        // before the `written_text` branch below because a view of the file's own text has no `written_text` at all,
        // and that branch is what a *rendering* is answered through.
        {
            let source: &str = view.written_text().unwrap_or(&view.source);
            if let Some((written, range)) =
                crate::completion::context::module_name_from_the_text(source, offset.get())
            {
                return self.module_completions(&written, range);
            }
        }

        // **A comment is prose in whichever reading answered.** The check below used to live inside the
        // `written_text` branch, which meant a file with **no rendering yet** — a buffer just opened, and every file
        // `inspect` asks about — was never asked whether the cursor is in a comment at all: measured, `inspect
        // complete` in `/// Hello w` answered the file's classes and functions, and the same happens in an editor
        // between opening a file and the cook finishing. So the text is chosen once, and the tree whose text it is
        // is the view's own for a file view and a parse of the written text for a rendering.
        let written_text = view.written_text();
        let text: &str = written_text.unwrap_or(&view.source);
        {
            // **A documentation comment is prose to the renderer and a language of its own to a reader.**
            //
            // The refusal below is right for every other question — a `w.` in a comment is a member access only to
            // a tree — but a comment produces **no tokens at all** in the rendering (the cook removes it before the
            // parse), so the doc-comment position cannot be read from the reading at any offset. It is asked of the
            // written text instead, and the offset it answers with is the **file's**, which is the buffer the reader
            // is editing: same shortcut as a `#include`, and for the same reason.
            //
            // Asked before `in_prose`, which is the line that would otherwise turn the whole of a comment away.
            let written_root = match written_text {
                Some(written) => {
                    cpp_parser::CppParser::parse(written, cpp_parser::ParserConfig::default())
                        .get_red_root()
                }
                None => view.root.clone(),
            };
            if let Some(
                context @ (crate::completion::CompletionContext::DocComment { .. }
                | crate::completion::CompletionContext::DocLink { .. }),
            ) = crate::completion::doc_comment_at(&written_root, offset.get())
            {
                return crate::completion::completion_with(
                    self.store.index(),
                    &view.scopes,
                    &view.root,
                    &view.path,
                    offset.get(),
                    &self
                        .headers
                        .read()
                        .map(|headers| headers.clone())
                        .unwrap_or_default(),
                    context,
                    self.config.cuda(),
                    self.the_completion_budget(),
                    &[],
                );
            }

            // **A documentation comment's prose offers nothing**, and it is asked of the text rather than of the
            // lexer's tokens: a backtick is not a C++ token, so `` /// See [`Wid `` arrives as a comment followed by
            // an unrecognised character and an identifier — `in_prose` reads the identifier and answered "code",
            // which is how a link's question used to reach the code path.
            // **…and a code block is the exception**: `@code … @endcode` is an example of *code*, which is where a
            // name does belong, and it is asked before the refusal so an example keeps its completion.
            if crate::doc::inside_a_code_block(text, offset.get()) {
                return crate::completion::completion_at(
                    self.store.index(),
                    &view.scopes,
                    &view.root,
                    &view.path,
                    0,
                    &self
                        .headers
                        .read()
                        .map(|headers| headers.clone())
                        .unwrap_or_default(),
                    self.config.cuda(),
                    self.the_completion_budget(),
                    &[],
                );
            }

            if crate::doc::is_a_documentation_comment(text, offset.get())
                || crate::file::view::in_prose(text, offset.get())
            {
                return crate::CompletionSet::default();
            }
            let context = crate::completion::context_at(&written_root, offset.get());
            match context {
                // **The program is in the rendering; the buffer is in the file.** The `replace` range is carried by
                // the context (read from the written text) and the *lookup* needs a reading offset - the one the
                // cursor maps to, or the rendering''s end when it maps nowhere (measured: `one::` at the end of a
                // file has no position in the rendering at all, and the answer was an empty set).
                crate::completion::CompletionContext::DirectiveArgument { .. }
                | crate::completion::CompletionContext::Qualified(_) => {
                    let in_the_reading = view
                        .reading_offset_of(offset)
                        .map_or(view.source.len(), |at| at.get());
                    return crate::completion::completion_with(
                        self.store.index(),
                        &view.scopes,
                        &view.root,
                        &view.path,
                        in_the_reading,
                        &self
                            .headers
                            .read()
                            .map(|headers| headers.clone())
                            .unwrap_or_default(),
                        context,
                        self.config.cuda(),
                        self.the_completion_budget(),
                        &[],
                    );
                }
                // **`MacroBody` is here too**: a `#define` line is in the file and not in the rendering, and the
                // answer needs no reading position at all — the parameters come from the line, the macros from the
                // summary.
                context @ (crate::completion::CompletionContext::DirectiveName { .. }
                | crate::completion::CompletionContext::Include { .. }
                | crate::completion::CompletionContext::MacroBody { .. }
                | crate::completion::CompletionContext::Condition { .. }) => {
                    return crate::completion::completion_with(
                        self.store.index(),
                        &view.scopes,
                        &view.root,
                        &view.path,
                        offset.get(),
                        &self
                            .headers
                            .read()
                            .map(|headers| headers.clone())
                            .unwrap_or_default(),
                        context,
                        self.config.cuda(),
                        self.the_completion_budget(),
                        &[],
                    );
                }
                // **`Nothing` is an answer, not a miss**: prose, and a directive that takes no argument (`#endif `).
                crate::completion::CompletionContext::Nothing => {
                    return crate::CompletionSet::default();
                }
                _ => {}
            }
        }

        let Some(offset) = view.reading_offset_of(offset) else {
            return crate::CompletionSet::default();
        };

        // The header index through its lock, and **an empty one on a poisoned lock**: a completion is a suggestion
        // list, and a thread that panicked while adding project files is no reason to fail the request — the
        // declarations below are the answer the reader came for.
        let headers = self
            .headers
            .read()
            .map(|headers| headers.clone())
            .unwrap_or_default();

        let mut set = crate::completion::completion_at(
            self.store.index(),
            &view.scopes,
            &view.root,
            &view.path,
            offset.get(),
            &headers,
            self.config.cuda(),
            self.the_completion_budget(),
            &unincluded,
        );
        // **A member access on a constrained template parameter** — `auto f(A auto a) { a. }`. There is no class to
        // list members of: what the language gives `a` is what the concept's `requires` body names on it, so that is
        // the list. Asked only when the ordinary answer is empty, which is what makes it free for every other
        // completion — see [`Session::the_members_a_concept_requires`].
        if set.items.is_empty() {
            let required = self.the_members_a_concept_requires(view, offset.get(), set.replace);
            if !required.is_empty() {
                set.items = required;
            }
        }
        set
    }

    /// **The headers the search path can name** — what a `#include` is completed from.
    ///
    /// A clone rather than a borrow, because the answer is behind a lock that is written when the project's file
    /// list grows. See [`Session::completions`] for the whole reason.
    pub fn headers(&self) -> crate::HeaderIndex {
        self.headers
            .read()
            .map(|headers| headers.clone())
            .unwrap_or_default()
    }

    /// **The documentation comment above a declaration** — what a hover shows when a file writes one.
    ///
    /// `file` and `offset` are the declaration's own coordinates, which is why they are arguments rather than
    /// something read from `view`: the fact a query resolved carries them, and the declaration can be in a file
    /// other than the one the cursor is in.
    ///
    /// # The one file that is not re-parsed
    ///
    /// The declaration in the file being edited is answered from the view the request already built, so a hover in
    /// the current file costs nothing extra. A declaration in another file needs that file's tree, which this
    /// session does not keep — a parse per question is the honest price of not caching a tree per file — and it is
    /// paid **only when the text before the declaration could hold a comment at all**
    /// ([`crate::file::view::might_be_documented`]). A header with no documentation above the declaration is
    /// therefore never parsed to find that out.
    ///
    /// The answer is the parser's node, not text: what a comment *says* — delimiters, line markers, `@param` — is
    /// the documentation layer's reading, and every consumer asking this question wants that reading rather than a
    /// copy of it.
    pub fn documentation(
        &self,
        view: &FileView,
        file: &Path,
        offset: usize,
    ) -> Option<cpp_parser::CppDocComment> {
        if file == view.path {
            // **The file's own text, for exactly the reason the branch below parses another file's.**
            //
            // A rendering is what a compiler reads, and a compiler reads no comments: they are removed before the
            // parse, so a `DocComment` node cannot exist in `view.root` and this lookup could only ever answer
            // `None` for a documented declaration. Measured: `Bad offset: range 0..6 offset 9` on
            // `"/// Doc.\nint x;\n"`, whose rendering is the six bytes of `int x;` — the comment *and* its line are
            // gone, so the offset a caller has (the file's) is past the tree as well as beside the comment.
            //
            // The other file's branch already accepts this price — "a parse per question is the honest price of not
            // caching a tree per file" — and the current file is the one a person is most likely to hover, so the
            // same price is paid here rather than a different answer being given for it. `might_be_documented` is
            // asked first for the same reason it is asked there: a declaration with nothing above it costs no parse.
            let Some(written) = view.written_text() else {
                // No rendering: this view *is* a parse of the file's own text, and its comments are its own nodes.
                return crate::file::view::documentation_at(&view.root, offset);
            };
            if !crate::file::view::might_be_documented(written, offset) {
                return None;
            }
            let tree = cpp_parser::CppParser::parse(written, cpp_parser::ParserConfig::default());
            return crate::file::view::documentation_at(&tree.get_red_root(), offset);
        }

        let declared_in = self.vfs.held(file)?;
        if !crate::file::view::might_be_documented(&declared_in.text, offset) {
            return None;
        }

        let tree =
            cpp_parser::CppParser::parse(&declared_in.text, cpp_parser::ParserConfig::default());
        crate::file::view::documentation_at(&tree.get_red_root(), offset)
    }

    /// **The type of the expression at `offset`**, as far as this analysis can tell — what a hover shows when the
    /// cursor is not on a name.
    ///
    /// The three answers are three different facts and a caller should keep them apart:
    ///
    /// * [`Known::Yes`] with the expression's text, its type, and the file the type came from — `this` in a member
    ///   function, `*p`, `f(x)`, `w.size`, a plain name;
    /// * [`Known::No`] when the cursor is in **no expression at all** — a `#define`, the punctuation between two
    ///   statements, a declaration's `const`. The question does not apply, which is not the same as an answer that
    ///   is missing;
    /// * [`Known::Unknown`] with the infer layer's own reason when the expression is there but its type is not
    ///   knowable here: a template that was never instantiated, a name nothing declares, a type computed rather
    ///   than read.
    ///
    /// # What "the expression" is
    ///
    /// The **innermost** one containing the offset ([`crate::sema::resolve::expression_at`]), so a cursor on
    /// `this` in `this->size` asks about `this`. A cursor in whitespace inside a statement is in no expression, and
    /// that is also the honest answer.
    /// **The type of this expression** — [`Session::type_at`] for a node the caller already holds.
    ///
    /// The offset form asks about the **innermost** expression containing a byte, which is the right question for a
    /// cursor and the wrong one for a caller that already has the expression: `Pair<int, int>{1, 2}`'s last byte is
    /// inside the braced list, so the offset question answered the *list*'s type for the object whose members were
    /// being asked about — and a hover said "`int` has no member `sum`" for a `Pair<int, int>`.
    pub fn type_of_the_expression(
        &self,
        view: &FileView,
        expression: &cpp_parser::CppSyntaxNode,
    ) -> Known<ExpressionType> {
        let written = expression.text().to_string().trim().to_string();
        match crate::index::project::type_of_expression(
            self.store.index(),
            &mut |path| self.view(path),
            &view.scopes,
            &view.root,
            &view.path,
            expression,
            0,
        ) {
            Known::Yes((type_of, file)) => Known::Yes(ExpressionType {
                expression: written,
                type_of: type_of.to_string(),
                file,
            }),
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::No,
        }
    }

    pub fn type_at(&self, view: &FileView, offset: ReadingOffset) -> Known<ExpressionType> {
        let Some(expression) = crate::sema::resolve::expression_at(&view.root, offset.get()) else {
            return Known::No;
        };

        let written = expression.text().to_string().trim().to_string();

        match crate::index::project::type_of_expression(
            self.store.index(),
            &mut |path| self.view(path),
            &view.scopes,
            &view.root,
            &view.path,
            &expression,
            0,
        ) {
            Known::Yes((type_of, file)) => Known::Yes(ExpressionType {
                expression: written,
                type_of: type_of.to_string(),
                file,
            }),
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::No,
        }
    }

    /// **The member of the class the cursor is inside that `name` names** — C++'s unqualified member lookup.
    ///
    /// The hover's question, for a name the scope tree and the index cannot place: `shared_from_this()` inside a class
    /// deriving `std::enable_shared_from_this<Node>` is a member of the **base**, and no scope in the file declares it.
    /// `members_of` answers it, substituted, and this is the one-reader wrapper the hover uses. `None` when the cursor is
    /// not inside a class member, or the name is not a member of that class.
    pub fn member_of_the_enclosing_class(
        &self,
        view: &FileView,
        offset: ReadingOffset,
    ) -> Option<ProjectDefinition> {
        let callee = crate::sema::resolve::name_node_at(&view.root, offset.get())?;
        crate::index::project::a_member_of_the_enclosing_class(
            self.store.index(),
            &view.scopes,
            &view.root,
            &view.path,
            &callee,
            false,
        )
    }

    /// **The parameter names to draw at the calls in `range`** — see [`crate::inlay`].
    ///
    /// `range` is the range the client asked about, in the file's own coordinates. Everything this needs beyond
    /// the file in hand is the *callee's* file, which is why it is a session method: this is how a header a call
    /// reaches into is parsed, and it is parsed once per declaring file however many calls reach it.
    ///
    /// # Why the declaring file is read **as itself**, and not through [`Session::view`]
    ///
    /// Because `callee.name_offset` is an offset in the declaring file's **own text** — that is what a summary
    /// records — and `view` hands back the compiler's *rendering* whenever one is cached. A rendering has no line
    /// breaks and no comments, so the same number names a different place in it, `parameter_list_of` finds no
    /// declarator there, and the call yields **no hint**.
    ///
    /// Measured, and it is the whole of a report that said the hints came and went: an edit drops the rendering, so
    /// the request made **immediately** after a keystroke read the file as itself and hinted correctly; two seconds
    /// later the pump had rebuilt the rendering, the same call resolved to the same declaration with the same offset,
    /// and the parameter list could no longer be read. The refresh the pump sends then asks the client to ask again —
    /// and the second answer was the same broken one, which is why the hints did not come back.
    pub fn inlay_hints(
        &self,
        view: &FileView,
        range: cpp_parser::SourceRange,
    ) -> Vec<ParameterHint> {
        let mut hints = crate::inlay::parameter_hints(self.store.index(), view, range, |path| {
            self.view_of_the_file(path)
        });

        // **The range going in is the rendering's; the offsets coming out are the file's.**
        //
        // That asymmetry is not an accident and it is worth stating, because it is the shape of this seam: the walk
        // has to read a **tree**, so the range it is given is an offset in the rendering, while a hint is drawn in the
        // reader's own buffer — and the handler places it with `position_in_file(file, hint.offset)`, the *file's* line
        // index, as does `tests/inlay.rs`. Moving the offset here rather than in the handler is the same choice
        // `Session::classified_names` makes one function up, and for the same measured reason: the two consumers
        // agreeing is the contract, and a hint placed at an offset the cook shifted is a hint on **another line**.
        //
        // A hint the file cannot place is dropped, exactly as the handler drops one it cannot place ("a position the
        // file cannot place produces no hint rather than one at a guessed place").
        for hint in &mut hints {
            let Some(placed) = view.file_offset_of(ReadingOffset::new(hint.offset)) else {
                continue;
            };
            hint.offset = placed.get();
        }

        hints
    }

    /// **The types the file does not write** — `auto`, a range-for's element, each name of a structured binding, and
    /// the return type an `auto` function deduces. See [`crate::inlay::type_hints`] for the three shapes and the three
    /// refusals.
    ///
    /// The type of an expression and the return type of a declaration are both **this** session's questions, which is why
    /// the walk lives in `inlay` and the answers are handed to it: one reader of "what type is this", the same one the
    /// hover uses.
    ///
    /// The offsets are moved back into the file's coordinates exactly as [`Session::inlay_hints`] does, and for the same
    /// reason — a hint is drawn in the reader's own buffer.
    pub fn type_hints(
        &self,
        view: &FileView,
        range: cpp_parser::SourceRange,
    ) -> Vec<crate::inlay::TypeHint> {
        let mut hints = crate::inlay::type_hints(
            view,
            range,
            |at| match self.type_at(view, ReadingOffset::new(at)) {
                Known::Yes(found) => Some(found.type_of),
                _ => None,
            },
            // **The index's fact, not the binding's.** A name in this file is answered from the *binding* builder, which
            // records the return type the file wrote — and an `auto` return has none. The deduction
            // (`deduced_returns_of`) runs in the index's builder, so the fact that says `int` for `auto one()` is the
            // summary's; the local fact stays as the fallback for a file the index has not caught up with.
            |at| {
                let local = self.definition(view, ReadingOffset::new(at)).value();
                let indexed = local.as_ref().and_then(|found| {
                    self.store
                        .index()
                        .definition(&found.fact.qualified_name(), &view.path)
                        .value()
                });
                indexed
                    .and_then(|found| found.fact.returns.map(|returns| returns.to_string()))
                    .or_else(|| {
                        local
                            .and_then(|found| found.fact.returns.map(|returns| returns.to_string()))
                    })
            },
        );
        // **And a generic lambda's parameter**, whose type exists only at the call the lambda is an argument of —
        // see [`Session::a_generic_lambdas_parameter_type`], which is where the four rules live.
        hints.extend(crate::inlay::lambda_parameter_hints(
            view,
            range,
            |parameter| self.a_generic_lambdas_parameter_type(view, parameter),
        ));
        for hint in &mut hints {
            let Some(placed) = view.file_offset_of(ReadingOffset::new(hint.offset)) else {
                continue;
            };
            hint.offset = placed.get();
        }
        hints
    }
    /// **The type a generic lambda's `auto` parameter has at the call site it is passed to.**
    ///
    /// `foo.call([](auto &&x) {})` is the shape: the closure is an **argument**, so until the callee is read there is
    /// no type for `x` at all. The answer comes from the callee's body —
    /// `void call(Fn &&fn) const { fn(*this); }` — where the parameter is called with `*this`: `Fn` is deduced from
    /// what the body passes it, `auto&&` binds an lvalue, and `x` is `Foo&` (or `const Foo&` in the `const` overload,
    /// where `*this` is `const Foo`).
    ///
    /// # The four rules, each of them measured on `test_cpp`'s `strings.cpp`
    ///
    /// 1. **The object's constness decides which overloads can be called**: a `const` object reaches only the `const`
    ///    members, so `cfoo.call(…)` has one candidate.
    /// 2. **A non-`const` object prefers the non-`const` member** — the language's own ranking of the implicit object
    ///    parameter — so `foo.call(…)` is the non-`const` `call` and `x` is `Foo&`, not `const Foo&`.
    /// 3. **A `requires` clause this layer cannot evaluate leaves the choice open** while more than one candidate
    ///    survives: `Bar::call` declares both overloads with `requires requires { fn(*this); }`, and `bar.call(…)`
    ///    gets no hint. A single candidate is still answered when it carries one — `cbar.call(…)` reaches only the
    ///    `const` member, and the clause is not what chose it.
    /// 4. **The parameter is the one the body calls**, by name: `Fn &&fn` gives `fn`, and the body's `fn(…)` is where
    ///    the type comes from. A parameter the body never calls is not deduced.
    ///
    /// `None` for every other shape — the lambda is not passed to a call, the callee has no declaration the index can
    /// name, the parameter has no name, the body never calls it — and that is the ordinary answer: a hint that guessed
    /// would put a wrong type inside the reader's line, which is the failure this round is about.
    pub fn a_generic_lambdas_parameter_type(
        &self,
        view: &FileView,
        parameter: &cpp_parser::CppSyntaxNode,
    ) -> Option<String> {
        use cpp_parser::CppAstNode as _;

        // **What the parameter was written with** — the rule that turns a deduced type into a spelling: `auto`
        // decays, `auto&` is a reference, `auto&&` keeps the argument's value category. The **type** and not the
        // parameter's text: `auto &&a` carries its name, and a spelling that has to be trimmed is one that will be
        // trimmed wrongly somewhere.
        let declared = cpp_parser::CppParam::cast(parameter.clone())?;
        let binding = WrittenBinding::of(&declared.get_type_text())?;

        // **The call the lambda is an argument of.** Not the lambda's own body: the type comes from its caller.
        let lambda = parameter.ancestors().find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::LambdaExpr
        });
        let lambda = lambda?;
        let call = lambda.ancestors().find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::CallExpr
        })?;
        let arguments = crate::sema::call_syntax::arguments_of(&call)?;
        let at = arguments
            .iter()
            .position(|argument| argument.text_range() == lambda.text_range())?;

        // **A member call**, which is the shape a callback is normally passed to: `object.member(…)`.
        let callee = call.children().next()?;
        let access = crate::sema::check::member_access_above(&callee);
        let access = access?;
        let object_at = usize::from(access.object.text_range().start());
        let crate::Known::Yes(object) = self.type_at(view, ReadingOffset::new(object_at)) else {
            return None;
        };
        let object_is_const = self.the_object_is_const(view, object_at);
        let object_is_const = object_is_const?;

        let members = self.members_of(view, &object.type_of);
        let crate::Known::Yes(list) = members else {
            return None;
        };
        let mut candidates: Vec<&crate::index::project::ProjectMember> = list
            .members
            .iter()
            .filter(|member| member.fact.kind == crate::DeclKind::Function)
            .filter(|member| member.fact.name == access.member)
            // **The argument has to be a *template parameter* of the callee.** `call(std::function<void(Foo&)>)`
            // needs no deduction — the type is written — so a candidate with a concrete parameter is not this
            // question's.
            .filter(|member| {
                // **A parameter the callee *declares as a template parameter*** — its own head names it, which is a
                // question about that declaration rather than the naming convention a check reads
                // (`names_a_template_parameter_for_a_check` knows `T` and `_Ty`, and `Fn` is neither).
                let Some(list) = member.fact.parameter_list() else {
                    return false;
                };
                let types = crate::sema::overload::parameters_in(list).0;
                let Some(written) = types.get(at) else {
                    return false;
                };
                // **The type is the first word** — `parameters_in` keeps the parameter's name (`"Fn &&fn"`), and
                // `const` may stand in front of the type rather than after it.
                let mut words = written.split_whitespace();
                let mut first = words.next().unwrap_or_default();
                if first == "const" {
                    first = words.next().unwrap_or_default();
                }
                let core = first.trim_end_matches(['&', '*']);
                member
                    .fact
                    .parameters()
                    .iter()
                    .any(|parameter| parameter == core)
            })
            // Rule 1: a `const` object reaches only the `const` members.
            .filter(|member| {
                !object_is_const || member.fact.modifiers.contains(crate::DeclModifiers::CONST)
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        // Rule 3: a constraint that cannot be evaluated leaves the choice open, while more than one survives.
        let constrained = |member: &&crate::index::project::ProjectMember| {
            member
                .fact
                .requires_clause()
                .is_some_and(|clause| !clause.trim().is_empty())
        };
        if candidates.len() > 1 && candidates.iter().any(constrained) {
            return None;
        }
        // Rule 2: on a non-`const` object the non-`const` member is the better match.
        if !object_is_const
            && candidates
                .iter()
                .any(|member| !member.fact.modifiers.contains(crate::DeclModifiers::CONST))
        {
            candidates
                .retain(|member| !member.fact.modifiers.contains(crate::DeclModifiers::CONST));
        }
        if candidates.len() != 1 {
            return None;
        }
        let chosen = candidates[0];

        // Rule 4: the parameter's name, and the body's call of it.
        let name =
            crate::sema::call_syntax::parameter_names_from_text(chosen.fact.parameter_list()?)?
                .get(at)
                .cloned()
                .flatten()?;
        let declaring = self.view_of_the_file(&chosen.file)?;
        // **That member's own body**, not the file: the first call named `fn` in a file that declares two classes is
        // the other class's, and the report this answers is a project where it was — `cbar.call(…)` was answered with
        // the type `Foo::call` passes its callback.
        let declared_at = self
            .the_declaration_node(&chosen.file, chosen.fact.name_range.start_offset)
            .unwrap_or_else(|| declaring.root.clone());
        let (argument, spelled) = the_argument_the_body_passes(&declaring, &declared_at, &name)?;
        let crate::Known::Yes(passed) = self.type_at(
            &declaring,
            ReadingOffset::new(usize::from(argument.text_range().start())),
        ) else {
            return None;
        };
        // **`*this` is `const T` in a `const` member** — the one qualifier the type of `this` does not carry here, and
        // the difference between `Foo&` and `const Foo&`, which is the whole answer for `cfoo.call(…)`.
        let mut deduced = passed.type_of;
        if spelled.trim() == "*this" && chosen.fact.modifiers.contains(crate::DeclModifiers::CONST)
        {
            deduced = format!("const {deduced}");
        }
        let lvalue = spelled.trim().starts_with('*') || is_a_name_expression(&argument);
        binding.bind(&deduced, lvalue)
    }
}

/// **How a generic lambda's parameter was written**, which is what turns a deduced type into a name.
///
/// The three forms are three different answers for one deduced `T`: `auto` copies (and drops a reference),
/// `auto&` binds an lvalue only, and `auto&&` is a forwarding reference that keeps whatever the argument's value
/// category was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WrittenBinding {
    ByValue,
    Reference,
    Forwarding,
    ConstReference,
}

impl WrittenBinding {
    /// `None` for a parameter that is not written with `auto` at all.
    fn of(written: &str) -> Option<WrittenBinding> {
        let written: String = written
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        if !written.contains("auto") {
            return None;
        }
        Some(match written.as_str() {
            "auto" => WrittenBinding::ByValue,
            "auto&" => WrittenBinding::Reference,
            "auto&&" => WrittenBinding::Forwarding,
            "constauto&" => WrittenBinding::ConstReference,
            _ => return None,
        })
    }

    /// The spelling the parameter's type has, given the type the call site passes and whether it is an lvalue.
    fn bind(self, deduced: &str, lvalue: bool) -> Option<String> {
        match self {
            WrittenBinding::ByValue => Some(deduced.to_string()),
            WrittenBinding::Reference => lvalue.then(|| format!("{deduced}&")),
            WrittenBinding::Forwarding if lvalue => Some(format!("{deduced}&")),
            WrittenBinding::Forwarding => Some(format!("{deduced}&&")),
            WrittenBinding::ConstReference => Some(format!("const {deduced}&")),
        }
    }
}

/// **The argument a callee's body passes to its callback parameter**, and its text.
///
/// `void call(Fn&& fn) { fn(*this); }` — asked for `fn`, this is `*this`. The first call of that name **in a
/// statement of the body** is the answer: a callback invoked twice with different types would be one this layer
/// cannot spell, and a hint from the first call would be a guess about the second.
fn the_argument_the_body_passes(
    declaring: &FileView,
    declaration: &cpp_parser::CppSyntaxNode,
    name: &str,
) -> Option<(cpp_parser::CppSyntaxNode, String)> {
    for call in declaration.descendants().filter(|node| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::CallExpr
    }) {
        let Some(callee) = call.children().next() else {
            continue;
        };
        if callee.text().to_string().trim() != name {
            continue;
        }
        let Some(arguments) = crate::sema::call_syntax::arguments_of(&call) else {
            continue;
        };
        let Some(argument) = arguments.into_iter().next() else {
            continue;
        };
        let spelled = declaring
            .get_token_text(cpp_parser::source_range(argument.text_range()))
            .unwrap_or_default()
            .to_string();
        return Some((argument, spelled));
    }
    None
}

/// **Is this expression a name** — an identifier, a member access, a subscript, a dereference? Those are the shapes
/// that bind to a reference; a literal, a call and a braced initialisation are prvalues.
///
/// A *node* question rather than a text one, because `f() + 0` and `f()` share a text prefix and differ in category.
fn is_a_name_expression(argument: &cpp_parser::CppSyntaxNode) -> bool {
    matches!(
        cpp_parser::CppSyntaxKind::from(argument.kind()),
        cpp_parser::CppSyntaxKind::NameExpr | cpp_parser::CppSyntaxKind::MemberExpr
    )
}
