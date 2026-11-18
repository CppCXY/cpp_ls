//! **The questions a client asks about a name** — definition, references, macros, modules, constants.
//!
//! Moved here from `session/mod.rs` without a change.

use super::*;

/// **A specifier written before a declaration but kept outside its node** — see
/// [`Session::the_attributes_before_a_declaration`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenAttribute {
    /// The text as the file wrote it: `__attribute__((cold))`, `[[nodiscard]]`, `NESTED`.
    pub written: String,
    /// What a macro invocation expands to, when the file defines it — one line, whitespace collapsed.
    pub expansion: Option<String>,
}

impl<F: FileProvider + Clone> Session<F> {
    /// The closure of `path` **with the text of each file**, in the order the graph is walked, and a map from the
    /// key every path is compared by to its place in that list.
    ///
    /// Read from the index's own edges, so it is the same closure the visibility answers use, and through this
    /// session's overlay, so an unsaved buffer is what gets cooked. One entry per file, and a file whose text this
    /// session cannot get is left out — the walk then treats it as a file that defines nothing, which is what an
    /// unreadable include is.
    ///
    /// **The map is the walk's lookup**, and it is why this returns two things: the unit walk asks "give me this
    /// path's summary and text" once per file it visits, and answering that by scanning the list with `same_file` is
    /// quadratic in the closure — a thousand files of standard library, for each of a hundred files being cooked.
    pub(crate) fn closure_with_text(
        &self,
        root: &Path,
    ) -> (Vec<(PathBuf, String)>, HashMap<String, usize>) {
        let seed = self.store.index().summary(root);
        self.closure_with_text_from(root, seed)
    }

    /// [`Session::closure_with_text`] for a root whose summary the caller has **instead of** the index.
    ///
    /// The one caller that has one is the file a reader is editing: its summary is dropped by the edit, so the
    /// index can describe its *directives* and nothing else — and those are what names its includes. See
    /// [`Session::unit_and_state_of`], where the order `directives → unit → parse` is written out.
    pub(crate) fn closure_with_text_from(
        &self,
        root: &Path,
        seed: Option<&FileSummary>,
    ) -> (Vec<(PathBuf, String)>, HashMap<String, usize>) {
        let mut out: Vec<(PathBuf, String)> = Vec::new();
        let mut by_key: HashMap<String, usize> = HashMap::new();

        for path in self.closure_paths_from(root, seed) {
            if let Some(text) = self.text(&path) {
                by_key.insert(queue_key(&path), out.len());
                out.push((path, text));
            }
        }

        (out, by_key)
    }

    /// The files `root` includes, **transitively**, `root` first and then by level — the order a reader would read
    /// them in, and the order the cooked readings are built in.
    ///
    /// The edges come from the index, which is the only place that knows them: a summary records where each
    /// `#include` resolved to, and a file the index has not read contributes no edges. A file is listed once,
    /// whatever number of paths reach it, which is what keeps a header included by everything from being cooked
    /// per includer.
    ///
    /// `seed` is the caller's own reading of the **root**, for the one caller whose root the index cannot describe —
    /// the file a reader is editing. See [`Session::closure_with_text_from`].
    pub(crate) fn closure_paths_from(
        &self,
        root: &Path,
        seed: Option<&FileSummary>,
    ) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        // **By the key every other queue in this crate compares by.** This used to ask `same_file` against every
        // file already listed, which normalizes two paths per comparison — and the closure of a standard-library
        // header is around a thousand files, so the walk was quadratic in it: measured as 100 s to cook a 108-file
        // corpus against 4.5 s for 255 files, because the closure here is the whole standard library and there it is
        // not. One hash per file is the same answer without the arithmetic.
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: std::collections::VecDeque<PathBuf> = std::collections::VecDeque::new();
        queue.push_back(root.to_path_buf());

        // **The root's edges come from `seed` when there is one**, because the index has none for it: a file that
        // was just edited describes nothing but its directives, and its directives are exactly its includes.
        let mut edges: Option<&FileSummary> = seed;

        while let Some(path) = queue.pop_front() {
            // One entry per **file**: two spellings of it are one file to cook, and a closure that listed both would
            // cook it twice, once under a name the index does not use.
            if !seen.insert(queue_key(&path)) {
                continue;
            }
            out.push(path.clone());

            let summary = match edges.take() {
                Some(seed) => Some(seed),
                None => self.store.index().summary(&path),
            };
            if let Some(summary) = summary {
                for include in &summary.includes {
                    if let Some(resolved) = &include.resolved {
                        queue.push_back(resolved.clone());
                    }
                }
            }
        }

        out
    }

    /// **The closure a file's own directives name, and whether every file in it has been read.**
    ///
    /// `None` when one of them has not.
    ///
    /// The root itself is exempt: the whole reason this exists is a file whose summary the index does not have.
    pub(crate) fn the_closure_of_the_directives(
        &self,
        root: &Path,
        directives: &FileSummary,
    ) -> Option<Vec<PathBuf>> {
        let index = self.store.index();
        let root_key = queue_key(root);
        let reached = self.closure_paths_from(root, Some(directives));

        reached
            .iter()
            .all(|path| queue_key(path) == root_key || index.summary(path).is_some())
            .then_some(reached)
    }

    /// **The name a declaration gives itself at `offset`**, when the cursor is exactly on it.
    ///
    /// The one name a reader can point at that no *name resolution* can read: an **operator**. `operator()` is spelled
    /// with a keyword and punctuation, so the token at the cursor is the `OperatorKeyword` and
    /// [`crate::sema::resolve::definitions_at`] answers `UnparsableName` — measured on
    /// `test_cpp/src/containers.cpp:63`, where hovering `operator` showed the keyword's meaning ("this function defines
    /// what an operator means for a type") for a reader pointing at a whole function. The scope tree has the name —
    /// a binding's `name_range` starts at the keyword, and its spelling is composed (`operator()`, `operator+`,
    /// `operator bool`) — so the declaration is one lookup away, and the index files its fact under exactly that
    /// spelling.
    pub fn the_name_declared_at(&self, view: &FileView, offset: usize) -> Option<String> {
        view.scopes
            .binding_declared_at(offset)
            .map(|binding| binding.name.text())
    }

    /// Which declaration the name at `offset` means, using this file's scopes and then the index.
    ///
    /// The *one* projection of [`Session::definitions`]: `Yes` only when the answer is a single declaration, so a
    /// consumer that can show only one location gets nothing rather than an arbitrary one. A client that can show a
    /// list — `textDocument/definition` — should ask that instead.
    pub fn definition(&self, view: &FileView, offset: ReadingOffset) -> Known<ProjectDefinition> {
        let found = definition_across_files(
            self.store.index(),
            &mut |path| self.view(path),
            &view.scopes,
            &view.root,
            &view.path,
            offset.get(),
        );
        let found = self.with_the_reflection(view, found);
        // **The answer came from L1, so ask for L2** — phase 1 of `docs/indexing-design.md`. A declaration the reader
        // jumped into is a file they are now looking at, and its refined reading is what its own members, types and
        // macro-shaped declarations are answered from. Asking here costs a `HashSet` insert under its own lock; the pump
        // then refines that one file (measured: **93.6 ms**, one timeline) while this answer is already on screen.
        if let Known::Yes(found) = &found {
            self.want_refinement(&found.file);
        }
        // **A qualified name the shape reading does not hold: ask for the declaring file's *rendering*.**
        //
        // The reading a compiler gets is the rendered one — conditionals evaluated, macros expanded — and a summary
        // built from the file's own text is not it: `nlohmann/json.hpp`'s `basic_json` ends up as one 333 362-byte
        // declaration because `#if JSON_DIAGNOSTICS` is **0** in a header the file includes (dead code a raw parse
        // still reads), so `basic_json::object` is absent from L1 and `nlohmann::json::object` has no answer.
        //
        // `Session::view` is the asking side of that contract: on a miss it records the file for a rendering and
        // answers `None` (`ask_for_a_rendering`), and the pump renders it. Nothing is guessed here — the file asked
        // for is the one the index says declares the class.
        if let Known::Unknown(UnknownReason::NotDeclaredHere(name)) = &found
            && let Some((qualifier, _)) = name.rsplit_once("::")
            && let Known::Yes(class) = self.store.index().definition(qualifier, &view.path)
        {
            let _ = self.view(&class.file);
        }
        found
    }

    /// **Is the object this name denotes `const`?** — the one question a member access's overload set depends on, and
    /// the one a *type's spelling* cannot answer.
    ///
    /// [`crate::DeclFact::type_of`] is a name for lookup and drops the declaration's specifiers on purpose (`static
    /// const Widget` records `Widget`, so that a lookup by name finds the class), and [`Session::type_at`]'s answer
    /// follows it: measured on `test_cpp/src/containers.cpp:57`, inside
    /// `bool operator()(std::string const& left, std::string const& right)`, `type_at` answers **`std::string&`** for
    /// `left`. A card that filtered the overloads by the type would therefore keep both.
    ///
    /// The declaration's own specifier sequence is the reading, and it is available for a name this file declares —
    /// which is the case that matters here (a parameter, a local, a file-scope object). `None` when the name resolves
    /// to nothing here or to a declaration this file does not write: a constness nobody can see must not become
    /// `false`, which is what a caller filtering a list would read it as.
    pub fn the_object_is_const(&self, view: &FileView, offset: usize) -> Option<bool> {
        let Known::Yes(binding) =
            crate::sema::resolve::definition_at(&view.scopes, &view.root, offset)
        else {
            return None;
        };
        let shapes = crate::sema::declarations::DeclarationShapes::of(&view.root);
        let specifiers = shapes.specifiers_at(binding.name_range.start_offset)?;
        let written = specifiers.text().to_string();
        Some(
            written
                .split(|character: char| !character.is_alphanumeric() && character != '_')
                .any(|word| matches!(word, "const" | "constexpr" | "consteval")),
        )
    }

    /// **Is this file's reading the refined one?** — `false` for a summary that is still the shape-only reading of the
    /// file's own text, which is what a start leaves behind (see `want_refinement`).
    pub fn is_refined(&self, path: &Path) -> bool {
        // Two ways to be refined, and both are real: a reading served from the refinement cache keeps its environment
        // key, while one the pass built in this run is inserted under the file's own key (the store resets it, so that
        // the entry the first pass wrote stays addressable). The set is what makes the second kind visible.
        if self
            .refined_readings
            .lock()
            .is_ok_and(|refined| refined.contains(&queue_key(path)))
        {
            return true;
        }
        self.store.is_marked_refined(path)
            || self
                .store
                .index()
                .summary(path)
                .is_some_and(|summary| summary.key.env_hash != 0)
    }

    /// **The reflection annotation the summary recorded**, when a same-file answer came from the binding instead.
    ///
    /// A definition inside the file being read is built from the scope tree, which holds the declaration's shape and no
    /// annotation: `UPROPERTY(EditAnywhere)` is an empty macro and was expanded away before the tree existed. The
    /// **summary** of the same file recorded it from the text, so the two answers are merged here rather than duplicating
    /// the text walk in the binding path. A member declared in *another* file already carries it — that fact came from
    /// the index.
    pub(crate) fn with_the_reflection(
        &self,
        view: &FileView,
        found: Known<ProjectDefinition>,
    ) -> Known<ProjectDefinition> {
        let Known::Yes(mut definition) = found else {
            return found;
        };
        if definition.fact.reflection().is_some() {
            return Known::Yes(definition);
        }
        let Some(in_the_file) =
            view.file_offset_of(ReadingOffset::new(definition.fact.name_range.start_offset))
        else {
            return Known::Yes(definition);
        };
        let Some(summary) = self.parsed_summary(&view.path) else {
            return Known::Yes(definition);
        };
        // **The name and its offset**, because an overload set shares the first and not the second.
        let recorded = summary.declarations.iter().find(|fact| {
            fact.name == definition.fact.name && fact.name_range.start_offset == in_the_file.get()
        });
        if let Some(recorded) = recorded {
            definition.fact.detail_mut().reflection = recorded.reflection().cloned();
        }
        Known::Yes(definition)
    }

    /// **Every declaration the name at `offset` refers to** — an overload set, a redeclaration, or one entity.
    ///
    /// See [`crate::ProjectIndex::definitions`] for what the list means and why one namespace collapses to one
    /// entry. This is the query behind `textDocument/definition`: measured on one real file, **46** identifiers
    /// answered `Ambiguous` through the single-answer form, and every one of them had a usable list behind it.
    pub fn definitions(&self, view: &FileView, offset: ReadingOffset) -> Known<ProjectDefinitions> {
        let mut answer = definitions_across_files(
            self.store.index(),
            &mut |path| self.view(path),
            &view.scopes,
            &view.root,
            &view.path,
            offset.get(),
        );
        if let Known::Yes(definitions) = &mut answer {
            for definition in &mut definitions.found {
                if let Some(path) = self.project_paths.get(&queue_key(&definition.file)) {
                    definition.file.clone_from(path);
                }
            }
        }
        answer
    }

    /// **Every place the symbol at `offset` is written** — what `textDocument/references` asks.
    ///
    /// # Why it resolves first rather than searching for the spelling
    ///
    /// A reference list is a claim about the **whole project**, and the protocol gives it no way to say "and there
    /// may be more". So the question is not "where is this word written" but "where is *this declaration* named",
    /// and the two differ wherever a project has two things spelled alike — a local shadowing a file-scope name, a
    /// member of one class and a free function of the same name. The cursor is therefore resolved the way
    /// go-to-definition resolves it, and only then is the name searched for. A cursor that resolves to nothing
    /// answers `Unknown(NotDeclaredHere)` rather than an empty list, which is the difference between "the analysis
    /// cannot tell you" and "there are none".
    ///
    /// The search itself is [`crate::index::references::symbol_references`], whose note states the candidate rule
    /// and the one case it does not cover.
    pub fn symbol_references(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        budget: crate::ReferenceBudget,
    ) -> Known<crate::SymbolReferences> {
        let Known::Yes(found) = self.definitions(view, offset) else {
            // **Which name the cursor is on**, for the refusal's message: a caller that gets `NotDeclaredHere("")`
            // learns nothing, and the name is what a person would look up next.
            let written = crate::sema::resolve::name_position_at(&view.root, offset.get())
                .map(|name| name.written.clone())
                .unwrap_or_default();
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(written)));
        };

        // **One declaration, and the list is all of them.** `definitions` answers a namespace as one entry and an
        // overload set as several; a reference list over an overload set is the union, and every entry shares the
        // spelling, so the first is the one that names the symbol.
        let Some(first) = found.found.first() else {
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from("")));
        };

        let symbol = crate::SymbolToFind::of(&first.fact, first.file.clone());

        crate::index::references::symbol_references(
            self.store.index(),
            &self.files,
            &symbol,
            budget,
        )
    }

    /// **The references of a declaration the caller already holds** — no offset, and so no coordinate to convert.
    ///
    /// [`Session::symbol_references`] finds the declaration by looking an offset up in the *rendering's* scope tree; a
    /// caller holding a summary fact has a **file** offset, and in a file whose rendering differs from its text (any
    /// header with includes behind it) the two name different positions. Measured on `EmmyLuaCodeStyle`: the CodeLens
    /// over `GetState` in `DiagnosticBuilder.h` said `0 references`, because the fact's file offset was read as a
    /// rendering offset.
    pub fn symbol_references_of(
        &self,
        fact: &crate::DeclFact,
        file: std::path::PathBuf,
        budget: crate::ReferenceBudget,
    ) -> Known<crate::SymbolReferences> {
        let symbol = crate::SymbolToFind::of(fact, file);
        crate::index::references::symbol_references(
            self.store.index(),
            &self.files,
            &symbol,
            budget,
        )
    }

    /// Which `#define` or `#undef` settles the macro name at `offset`.
    /// **The macro a use names, asked in the file's own coordinates.**
    ///
    /// `macro_definition` takes the **rendering's** offset, and a macro has no footprint there: the cook expanded every use,
    /// so the tree at `int n = MAX;` says `((a) > (b) ? (a) : (b))` and the reader's `MAX` is not in it. The writer's text still
    /// has it — `written_text()` — and the macro's definition ranges are file offsets, so this is the query the two agree on.
    ///
    /// Measured: at a use of a file's own `#define MAX(a, b)`, `macro_definition` answered `NotDeclaredHere("MAX")` because the
    /// rendering offset it was given (`"MAX"`) named a position the *file* does not have.
    pub fn macro_at_a_file_offset(
        &self,
        view: &FileView,
        offset: crate::file::coordinates::FileOffset,
    ) -> Known<ProjectMacro> {
        // **The file's own text, from whichever view this is** — a handler may hold the *plain* view, whose root is the
        // file's own text, and asking only for `written_text()` made every macro question in the editor answer "no
        // written text". See `FileView::the_written_text`.
        let Some(written) = view.the_written_text() else {
            return Known::Unknown(UnknownReason::NotDeclaredHere(Box::from("no written text")));
        };
        let at = offset.get();
        // The word under the cursor, in the file's own bytes — the same walk `hover`'s `name_at_the_cursor` makes for a fix.
        let is_a_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
        let bytes = written.as_bytes();
        let mut start = at.min(bytes.len());
        while start > 0 && is_a_word(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = at.min(bytes.len());
        while end < bytes.len() && is_a_word(bytes[end]) {
            end += 1;
        }
        let name = &written[start..end];
        if name.is_empty() {
            return Known::Unknown(UnknownReason::UnparsableName);
        }
        self.store.index().macro_definition(name, &view.path, at)
    }

    /// **A `#define`/`#undef` this file writes itself**, read from its text — the answer to a macro question about a
    /// file the index has not caught up with yet.
    ///
    /// [`Session::macro_at_a_file_offset`] asks the **index**, whose summaries are pushed by the work loop: a file that
    /// was opened a moment ago has a view and no summary, so a hover on that file's own `#define MAX(a, b)` answered
    /// nothing at all. Measured through the real server on a two-file C fixture: `CORPUS_STANDARD` at its definition
    /// answered `Unknown(NotDeclaredHere)` while the text plainly writes it.
    ///
    /// Returns the **directive's text** — `#define CORPUS_STANDARD __STDC_VERSION__`, continuation lines joined — which
    /// is what a reader hovering a definition wants to see, and `None` when the name is not defined (or undefined) in
    /// this file before `offset`.
    pub fn macro_definition_text(&self, path: &Path, offset: usize, name: &str) -> Option<String> {
        let text = self.text(path)?;
        the_directive_defining(name, &text, offset)
    }

    /// **Every module this project can name**, for a completion after `import`.
    ///
    /// Two sources, cheapest first, and neither parses anything:
    ///
    /// ```text
    /// the index      `interface_unit_of` — exact, for a module whose interface unit has been read
    /// the file list  the project's own `.cppm`/`.ixx`/… files, named by the convention the resolver proposes with —
    ///                so a name offered here is a name `import` will resolve, even before the unit is read
    /// ```
    ///
    /// A file named `math.cppm` is module `math`; a dotted one (`std.compat.ixx`) is `std.compat`. That is the
    /// convention [`crate::ModuleScanner`] searches by, and taking it from the *files* rather than from a scan is
    /// what keeps this cheap enough to answer on every keystroke.
    pub fn module_names(&self) -> Vec<(String, PathBuf)> {
        let mut found: Vec<(String, PathBuf)> = self
            .store
            .index()
            .interface_units()
            .into_iter()
            .map(|(name, unit)| (name, PathBuf::from(unit)))
            .collect();

        // **Only the extensions that are a module unit by naming convention.** `INTERFACE_UNIT_EXTENSIONS` also
        // holds `cpp`/`cc`/`cxx`, because a `.cpp` *may* declare a module — but a completion built from that list
        // offers `main` for `main.cpp` and `math` three times for a project with `math.cpp` and `math.cppm`.
        // Measured on the C++23 fixture: six items for two modules. A `.cpp` that really is an interface unit is
        // found by the **index** once it is read, which is the exact source and the one above.
        const A_MODULE_FILE: &[&str] = &["cppm", "ixx", "ccm", "cxxm"];
        for path in &self.project {
            let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
                continue;
            };
            if !A_MODULE_FILE
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
            {
                continue;
            }
            let Some(name) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.is_empty())
            else {
                continue;
            };
            found.push((name.to_string(), path.clone()));
        }

        // **One row per name**, and the index's comes first: `sort_by` is stable, so a name the index knows keeps
        // its exact unit and the file-list entry for the same name is dropped.
        found.sort_by(|left, right| left.0.cmp(&right.0));
        found.dedup_by(|later, earlier| later.0 == earlier.0);
        found
    }

    /// **The module names a reader can type at this cursor** — the answer to a [`crate::completion::context::CompletionContext::ModuleName`]
    /// position.
    ///
    /// A list of names, not of declarations: nothing declares a module name, so the items carry the name as their
    /// label and the unit as their detail, and the client replaces the partial word the reader has typed.
    pub(crate) fn module_completions(
        &self,
        written: &str,
        range: cpp_parser::SourceRange,
    ) -> crate::CompletionSet {
        let mut items: Vec<crate::completion::CompletionItem> = Vec::new();

        for (name, unit) in self.module_names() {
            if !written.is_empty()
                && !name
                    .to_ascii_lowercase()
                    .starts_with(&written.to_ascii_lowercase())
            {
                continue;
            }
            items.push(crate::completion::CompletionItem {
                label: name.clone(),
                insert: name.clone(),
                snippet: false,
                // A module name is a scope name, which is the kind a theme draws a qualified-name root with.
                kind: crate::completion::ItemKind::Namespace,
                detail: Some(format!("module {}", unit.display())),
                identity: None,
                filter: None,
                fact: None,
                replace: range,
                inclusion: None,
                additional_edits: Vec::new(),
            });
        }

        crate::CompletionSet {
            scope: String::new(),
            prefix: written.to_string(),
            replace: range,
            truncated: false,
            // A module name is a file on disk, not a fact the index has to have read: nothing here can be missing.
            incomplete: false,
            items,
        }
    }

    /// **The integer value of the constant declared at `offset`**, when this layer can fold it — an enumerator, or a
    /// `const`/`constexpr` variable. See [`crate::sema::constant`], which does the folding and says what it refuses.
    ///
    /// The macro question — `UCHAR_MAX` is `0xff` — is answered here rather than there because a macro may be defined
    /// in a header: the index has the body of every `#define` it has read, and the constants of a real header are
    /// written in terms of the ones `<climits>` brings in.
    pub fn constant_value_at(&self, view: &FileView, offset: ReadingOffset) -> Option<i64> {
        let path = view.path.clone();
        crate::sema::constant::value_at(view, offset.get(), &|name| {
            self.macro_body_at(&path, offset.get(), name)
        })
    }

    /// **An object-like macro's body, as text**, for a name visible at `offset` in this file — `0xff` for
    /// `UCHAR_MAX`. `None` for a function-like macro (its body is not a value) and for a name that is not a macro
    /// here.
    /// **The value of a declaration, folded in the unit where it is read.**
    ///
    /// [`Session::constant_value_at`] takes one view and one offset, which is right when a reader is looking at the
    /// file that declares the constant. A **header** is the case it cannot answer: `UCHAR_MAX` is defined in the
    /// toolchain's `limits.h`, and a header that uses it does not necessarily include that header itself — every
    /// `.cpp` including it does. So the *names* are read from a real translation unit (`reader` when it can see the
    /// declaration, otherwise the first file that includes it), while the **declaration's own text** still comes from
    /// the file that declares it.
    ///
    /// Measured on `EmmyLuaCodeStyle`: `const int FIRST_RESERVED = UCHAR_MAX + 1;` in `LuaTokenTypeDetail.h` showed no
    /// value, while the same fold in `LuaSyntaxTree.cpp` gave 256 — the header alone cannot see `<climits>`, and the
    /// declaration is only ever compiled as part of some `.cpp`.
    pub fn constant_value_of_a_declaration(
        &self,
        declared_in: &Path,
        name_range: cpp_parser::SourceRange,
        reader: &Path,
    ) -> Option<i64> {
        let view = self.view(declared_in)?;
        // The unit whose macros the declaration is read with.
        let mut unit = reader.to_path_buf();
        if declared_in != reader
            && !self
                .index()
                .visible_files(reader)
                .iter()
                .any(|(path, _)| std::path::Path::new(path) == declared_in)
            && let Some(includer) = self.index().includers_of(declared_in).first()
        {
            unit = includer.clone();
        }
        // The end of that unit: every `#include` it writes has been pasted by then, which is the widest set of names
        // it can offer. A macro redefined after the declaration's own line is not something this can know without
        // the declaration's line in *that* file, and the answer is `None` rather than a guess in that case only when
        // the two definitions disagree — see [`Session::macro_body_at`].
        let at = self.text(&unit).map(|text| text.len()).unwrap_or(0);
        crate::sema::constant::value_at(&view, name_range.start_offset, &|name| {
            self.macro_body_at(&unit, at, name)
        })
    }

    pub fn macro_body_at(&self, path: &Path, offset: usize, name: &str) -> Option<String> {
        let environment = self.store.index().macro_environment(name, path);
        // **The strict answer first**, which is the preprocessor's: the `#define` in force.
        if let Known::Yes(found) = environment.at(offset) {
            return self.the_macro_body(found);
        }
        // **Then what every definition in force agrees on.** `at` refuses a name whose region cannot be decided, and
        // that is right for a jump — one `#define` of two is a wrong destination. A *value* is a different question:
        // when every definition in force folds to the same number, that number is the compilation's, whatever the
        // preprocessor took. Measured on `EmmyLuaCodeStyle`: `const int FIRST_RESERVED = UCHAR_MAX + 1;` showed no
        // value, because `UCHAR_MAX`'s definition sits inside a conditional in the toolchain's `limits.h` and `at`
        // answered `Unknown(ConditionalCompilation)`.
        let mut agreed: Option<String> = None;
        for candidate in environment.definitions_in_force(offset) {
            let Some(body) = self.the_macro_body(candidate) else {
                // A definition this layer cannot read makes agreement unprovable: two bodies may be the same, and a
                // third unreadable one may not be.
                return None;
            };
            match &agreed {
                None => agreed = Some(body),
                Some(known) if *known == body => {}
                Some(_) => return None,
            }
        }
        agreed
    }

    /// **One macro candidate's replacement list, as text** — `0xff` for `UCHAR_MAX`.
    pub(crate) fn the_macro_body(
        &self,
        candidate: &crate::index::project::MacroCandidate,
    ) -> Option<String> {
        let fact = candidate.fact();
        if fact.function_like {
            return None;
        }
        // **The body is a range in the defining file**, because a replacement list is text in *that* file and the fact
        // deliberately stores a shape rather than a copy of the tokens (see `MacroFact::body_range`).
        let range = fact.body_range?;
        let text = self.text(candidate.file())?;
        let body = text.get(range.start_offset..range.end_offset())?;
        (!body.trim().is_empty()).then(|| body.trim().to_string())
    }

    pub fn macro_definition(&self, view: &FileView, offset: ReadingOffset) -> Known<ProjectMacro> {
        macro_across_files(self.store.index(), &view.root, &view.path, offset.get())
    }

    /// **A macro the *compiler* predefines** — `__STDC_VERSION__`, `__GNUC__`, `_WIN32` — with its value.
    ///
    /// These are not in any file, so no summary holds them and [`Session::macro_at_a_file_offset`] answers nothing for
    /// one; a reader hovering `#ifdef __STDC_VERSION__` was told nothing at all, which reads as "this name is unknown"
    /// for the one kind of name a compiler defines by itself.
    ///
    /// # Where the answer comes from, in order
    ///
    /// ```text
    /// 1. the standard's own macros             `__STDC__`, `__STDC_VERSION__`, `__STDC_HOSTED__`, `__cplusplus`
    /// 2. the toolchain's `-dM` table           `__GNUC__`, `_WIN32`, `__linux__`
    /// ```
    ///
    /// **The standard's first, and that is not an optimisation.** A test project with no `compile_commands.json` and no
    /// compiler on `PATH` has no toolchain at all, and `__STDC_VERSION__` is defined by *every* C compiler — the *value*
    /// is what a compiler decides, and the file's language already says which standard it is reading. Measured: a
    /// handshake test on a two-file C project answered no hover for `__STDC_VERSION__`, because the toolchain was the
    /// only source and it was absent.
    ///
    /// `path` is the file the question is about, because the standard is per file.
    pub fn predefined_macro(&self, name: &str, path: &std::path::Path) -> Option<String> {
        let language = self.config_for(path).language_for(path);
        if let Some(value) = crate::session::the_standards_own_macro(name, &language) {
            return Some(value);
        }

        let toolchain = self.toolchain.as_ref()?;
        if toolchain.builtin_macros.is_empty() {
            return None;
        }
        toolchain
            .macros()
            .into_iter()
            .find(|(defined, _)| *defined == name)
            .map(|(_, value)| value.unwrap_or_default().to_string())
    }

    /// **What a macro invocation at `offset` expands to**, by the preprocessor's own rules: the replacement list with
    /// the arguments substituted (`#` and `##` applied), and every macro the result names expanded again.
    ///
    /// `None` when the name is not a macro there, when it is a function-like macro **not followed by a call** (writing
    /// its name alone does not expand it), or when the argument list cannot be read (an unbalanced `(` in the text).
    ///
    /// # Why this is a cook and not a substitution
    ///
    /// A one-pass textual substitution is what this used to be, and it answered a different question from the one a
    /// reader asks: `PASTE(foo, bar)` came back `foo ## bar` instead of `foobar`, a body naming another macro came back
    /// with that macro's *name*, and `#x` stayed a hash. Every one of those is a difference from the compiler's answer
    /// on a card whose whole purpose is to show that answer.
    ///
    /// So the invocation is handed to the same [`crate::preprocess::cooked::cook_with`] a file's reading goes through,
    /// with the definitions this file has **in force before the use** and the closure's environment underneath — the
    /// state a compiler is in at that line. The result is formatted (`cpp_formatter`), because an expansion is code and
    /// the reader is going to read it as code.
    pub fn the_expansion_of_a_macro(
        &self,
        view: &FileView,
        offset: crate::file::coordinates::FileOffset,
    ) -> Option<String> {
        let Known::Yes(found) = self.macro_at_a_file_offset(view, offset) else {
            return None;
        };
        let fact = &found.fact;
        if fact.kind != crate::MacroKind::Definition {
            return None;
        }
        let written = view.the_written_text()?;

        // **The invocation**: the name's own token, and for a function-like macro its argument list. A function-like
        // macro not followed by `(` is not an invocation at all — a compiler keeps the name, and so does this.
        let bytes = written.as_bytes();
        let is_a_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
        let mut start = offset.get().min(bytes.len());
        while start > 0 && is_a_word(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = offset.get().min(bytes.len());
        while end < bytes.len() && is_a_word(bytes[end]) {
            end += 1;
        }
        if written.get(start..end)? != fact.name {
            return None;
        }
        if fact.function_like {
            let after = &written[end..];
            let paren = after.find(|character: char| !character.is_whitespace())?;
            if !after[paren..].starts_with('(') {
                return None;
            }
            let open = end + paren;
            end = Self::the_matching_paren(&written, open)?;
        }
        let invocation = written.get(start..end)?;

        // **The file's own definitions, in force at this line, then the invocation.** The cook executes them in order —
        // the same sequence its own reading of the file passes — so the invocation is expanded in the state a compiler
        // is in there. The offsets come from the hovered file's summary, which is the text `written` is.
        let mut source = String::new();
        let summary = self.store.index().summary(&view.path)?;
        for defined in &summary.macros {
            if defined.range.start_offset >= offset.get() {
                break;
            }
            match defined.kind {
                crate::MacroKind::Definition => {
                    let Some(body_range) = defined.body_range else {
                        continue;
                    };
                    let Some(body) = written.get(body_range.start_offset..body_range.end_offset())
                    else {
                        continue;
                    };
                    // The parameter list is the text between the name and the body — the one piece a macro fact does
                    // not carry, and the same rule the directive layer reads it by.
                    let parameters = defined.function_like.then(|| {
                        written
                            .get(defined.range.end_offset()..body_range.start_offset)
                            .map(str::trim)
                            .unwrap_or("()")
                    });
                    source.push_str("#define ");
                    source.push_str(&crate::preprocess::cooked::definition_text(
                        &defined.name,
                        parameters,
                        body.trim(),
                    ));
                    source.push('\n');
                }
                crate::MacroKind::Undefinition => {
                    source.push_str("#undef ");
                    source.push_str(&defined.name);
                    source.push('\n');
                }
            }
        }
        source.push_str(invocation);

        // **The environment underneath**: the closure's unconditional definitions, and the compiler's own — the same
        // two layers a file's cook starts from.
        let environment = self.store.the_indexed_environment(&view.path)?;
        let seed = crate::preprocess::macros::MacroTable::from_marked(self.store.index().macros());
        let configuration = {
            let mut parsed = self
                .parsed_definitions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            crate::preprocess::cooked::configuration_from_environment_and(
                &environment,
                true,
                &mut parsed,
            )
        };
        let start_state = crate::preprocess::cooked::Over::new(&configuration.table, &seed);
        let (tokens, _) = cpp_parser::lex(&source, &cpp_parser::LexerConfig::default());
        let rendered =
            crate::preprocess::cooked::cook_with(&source, &tokens, &start_state).render();
        let text = rendered.text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        Some(cpp_formatter::reflow(
            &text,
            &cpp_formatter::Style::fallback_style(),
        ))
    }

    /// **The specifiers written before a declaration, which the grammar keeps as its siblings** — the attributes and
    /// the macro invocations that stand for them.
    ///
    /// `__attribute__((cold)) int f();`, `[[nodiscard]] int f();` and `NESTED int f();` parse with the attribute in an
    /// `AttributeList` (or, for a macro, a `MacroCall`) **beside** the `Declaration`, not inside it. The declaration's
    /// fact therefore holds no trace of either, and the hover card — generated from the fact — dropped them: measured on
    /// a CUDA project, `#define NESTED AI __attribute__((cold))` followed by `NESTED int f_nested(int x)` hovered as
    /// `int f_nested(int x)`, with nothing about the attributes the source line carries.
    ///
    /// Walked **backwards from the declaration's own node** while the previous sibling is one of those two kinds, then
    /// reversed, so the answer is in source order. A macro's expansion is asked at its own offset — the same query the
    /// macro card uses — and a macro the file does not define (a CUDA `__global__`, which the grammar reads as a macro)
    /// answers with `None` for it rather than inventing one.
    pub fn the_attributes_before_a_declaration(
        &self,
        file: &std::path::Path,
        offset: usize,
    ) -> Vec<WrittenAttribute> {
        use cpp_parser::CppAstNode as _;

        let Some(view) = self.view_of_the_file(file) else {
            return Vec::new();
        };
        let Some(token) = cpp_parser::token_at(&view.root, offset) else {
            return Vec::new();
        };
        let Some(declaration) = token
            .parent_ancestors()
            .find_map(cpp_parser::CppDeclaration::cast)
        else {
            return Vec::new();
        };
        let mut found: Vec<WrittenAttribute> = Vec::new();
        // **Only a run written against the declaration belongs to it.** A macro on an earlier line with a blank line
        // or a comment in between is a statement of its own: `_GLIBCXX_END_NAMESPACE_CXX11` stands above
        // `std::string`'s declaration past a blank line and the doc comment, and the walk that ignored the gap took it
        // as an annotation — the hover then drew the macro in front of the declaration. The gap is measured in the
        // file's own text, where comments and blank lines are visible.
        let source = view.written_text().unwrap_or(&view.source);
        let mut boundary = usize::from(declaration.syntax().text_range().start());
        let mut sibling = declaration.syntax().prev_sibling();
        while let Some(node) = sibling {
            let kind = cpp_parser::CppSyntaxKind::from(node.kind());
            let is_a_macro = kind == cpp_parser::CppSyntaxKind::MacroCall;
            if !is_a_macro && kind != cpp_parser::CppSyntaxKind::AttributeList {
                break;
            }
            let start = usize::from(node.text_range().start());
            let end = usize::from(node.text_range().end());
            let written = node.text().to_string().trim().to_string();
            // **A node whose text carries a comment is not just the macro.** The tree files a macro invocation and the
            // documentation of the declaration *below* it as one node — `stringfwd.h` has
            // `_GLIBCXX_END_NAMESPACE_CXX11\n\n  /// A string of @c char` as the sibling before `std::string`'s
            // declaration — so the comment's presence says the macro is a statement of its own. Left in, it is read as
            // a specifier and drawn in front of the declaration.
            if written.contains("//") || written.contains("/*") || written.contains("\n\n") {
                break;
            }
            let between = source
                .get(end..boundary)
                .unwrap_or_default()
                .replace("\r\n", "\n");
            if between.contains("\n\n") || between.contains("//") || between.contains("/*") {
                break;
            }
            boundary = start;
            let expansion = is_a_macro
                .then(|| {
                    self.the_expansion_of_a_macro(
                        &view,
                        crate::file::coordinates::FileOffset::new(usize::from(
                            node.text_range().start(),
                        )),
                    )
                })
                .flatten()
                .map(|expanded| expanded.split_whitespace().collect::<Vec<_>>().join(" "));
            found.push(WrittenAttribute { written, expansion });
            sibling = node.prev_sibling();
        }
        found.reverse();
        found
    }

    /// The index of the `)` that closes the `(` at `open`, counting nesting and skipping literals.
    ///
    /// The argument list of a nested call is inside the first `(`, and a `)` in a string literal closes nothing:
    /// `f("a)b", g(1))` has one argument. `None` is an unbalanced `(` — the text is being edited.
    pub(crate) fn the_matching_paren(text: &str, open: usize) -> Option<usize> {
        let bytes = text.as_bytes();
        if bytes.get(open) != Some(&b'(') {
            return None;
        }
        let mut depth = 0i32;
        let mut quote: Option<u8> = None;
        let mut at = open;
        while at < bytes.len() {
            let byte = bytes[at];
            match quote {
                Some(ending) => {
                    if byte == b'\\' {
                        at += 2;
                        continue;
                    }
                    if byte == ending {
                        quote = None;
                    }
                }
                None => match byte {
                    b'"' | b'\'' => quote = Some(byte),
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(at + 1);
                        }
                    }
                    _ => {}
                },
            }
            at += 1;
        }
        None
    }

    /// **Every `import m;` this file writes, with the file each one reaches** — the module's answer to what
    /// [`crate::IncludeFact`] is for `#include`.
    ///
    /// The range is the **module name** and not the declaration, for the same reason a document link covers
    /// `vector` rather than `#include <vector>`: a link is a clickable span, and the name is the clickable thing.
    ///
    /// # Where the answer comes from, in order
    ///
    /// ```text
    /// the index        `interface_unit_of(m)` — exact, and free, for a module whose unit has been read
    /// the scanner      the naming convention beside this file and along the include paths — which is how the
    ///                  FIRST request in a module file learns where the module lives, before any of it is indexed
    /// ```
    ///
    /// `None` for a module neither can name, which is the honest answer: the link leads nowhere, and an underline
    /// that leads nowhere is worse than none. A **partition** (`import :part;`) and a **header unit**
    /// (`import <vector>;`) are not module names and are skipped here — the header unit is a header, and the
    /// partition belongs to the file's own module.
    pub fn module_imports_of(&self, view: &FileView) -> Vec<ModuleImport> {
        let mut imports: Vec<ModuleImport> = Vec::new();
        let directory =
            crate::paths::parent_normalized(&view.path, self.files.is_case_insensitive());
        let mut interner = PathInterner::new(self.files.is_case_insensitive());
        let mut scanner: Option<ModuleScanner<'_, OverlayFiles<OpenDocuments, F>>> = None;
        // **A partition is named against the module its unit declares.** `export import :area;` names
        // `geometry:area`, not a module called `area` — and asking for the bare name is not a miss, it is the *wrong
        // module*: a project with a `shapes` module of its own had `export import :shapes;` resolve to that file while
        // `export import :area;` resolved to nothing, which is the reported pair of adjacent lines with a link on one
        // and not the other. The file's own module is what the leading `:` is relative to.
        let declared = self
            .store
            .index()
            .summary(&view.path)
            .and_then(|summary| summary.modules.module.clone());
        let written = view.written_text().unwrap_or(&view.source);

        for node in view.root.descendants() {
            if cpp_parser::CppSyntaxKind::from(node.kind()) != cpp_parser::CppSyntaxKind::ImportDecl
            {
                continue;
            }
            let Some(name_node) = node.descendants().find(|inner| {
                cpp_parser::CppSyntaxKind::from(inner.kind())
                    == cpp_parser::CppSyntaxKind::ModuleName
            }) else {
                continue;
            };

            let name = name_node.text().to_string().trim().to_string();
            if name.is_empty() {
                continue;
            }
            let range = cpp_parser::source_range(name_node.text_range());
            let name = match (
                a_partition_is_written_before(written, range.start_offset),
                &declared,
            ) {
                (true, Some(module)) => format!("{module}:{name}"),
                _ => name,
            };

            let resolved = match self.store.index().interface_unit_of(&name) {
                Some(path) => Some(PathBuf::from(path)),
                None => {
                    let scanner = scanner
                        .get_or_insert_with(|| ModuleScanner::new(&self.files, &self.config));
                    match scanner.resolve_module(&name, &directory, &mut interner) {
                        ImportOutcome::Resolved(unit) => scanner
                            .units()
                            .iter()
                            .find(|entry| entry.file == unit)
                            .map(|entry| entry.path.clone()),
                        _ => None,
                    }
                }
            };

            imports.push(ModuleImport {
                name,
                range,
                resolved,
            });
        }

        imports
    }

    /// **Where the `#include` under the cursor points** — the fourth answer a jump can give.
    ///
    /// The three the analysis already had are a declaration (the scope walk), an overload set (the index by name)
    /// and a macro (`#define`). A **header name** is none of them: nothing in the file declares `vector`, the scope
    /// walker never sees `<vector>` (the grammar folds it into one token inside a directive), and the macro table
    /// has no entry for it. So `textDocument/definition` on `#include <vector>` had nothing to answer with, and a
    /// reader who ctrl-clicked it was told there is no definition — which is true of every question that was asked
    /// and false about the file it names.
    ///
    /// The answer is the resolved path, from the same summary field the resolver wrote when the file was read, so
    /// this is a lookup rather than a second implementation of the search: see [`crate::header_at`].
    /// # The offset is the **file's**, because a header name is a directive's argument
    ///
    /// `#include "near.h"` is a fact about the text the reader wrote, and a rendering has the *included file's*
    /// content where that line was — so neither the directive nor the name exists in the tree a `FileView` of a
    /// rendering parses. This query therefore takes a `FileOffset` and parses the written text, exactly as
    /// `Session::documentation` does for comments, `Session::selection_chain` for selections and
    /// `Session::folding_ranges` for folds.
    pub fn header_at(&self, view: &FileView, offset: FileOffset) -> Known<crate::HeaderTarget> {
        let index = self.store.index();
        let Some(written) = view.written_text() else {
            // No rendering: this view *is* a parse of the file's own text, so its offsets are already the file's.
            return crate::header_at(index, &view.root, &view.path, offset.get());
        };
        let tree = cpp_parser::CppParser::parse(written, cpp_parser::ParserConfig::default());
        crate::header_at(index, &tree.get_red_root(), &view.path, offset.get())
    }

    /// Everywhere the macro name written at `offset` is used, across the project — the query a rename starts from.
    ///
    /// The name comes from the cursor rather than from the caller, because that is what a client has: a position,
    /// not a spelling. Everything after that is [`macro_references`], which reads file **text** — through this
    /// session's overlay, so an unsaved buffer is searched as the user typed it rather than as it is on disk.
    ///
    /// # What the answer depends on, in a session
    ///
    /// The candidates are the files the index holds. A file whose summary was dropped by an edit and has not been
    /// read again is not one of them, so its own uses are missing from the answer until [`Session::advance`] gets
    /// to it — which is the same lazy-indexing boundary every query here has, and the same rule applies: **do not
    /// show "no references" while [`Session::pending`] is non-zero.**
    ///
    /// A cursor on an ordinary identifier — a variable, a function — answers `Unknown(NotDeclaredHere)`, which says
    /// exactly what is true: nothing defines that name as a macro. This query is about macros, and references of a
    /// *name* need the scope of every candidate file, which is a parse per candidate rather than a lex.
    pub fn macro_references(
        &self,
        view: &FileView,
        offset: ReadingOffset,
    ) -> Known<MacroReferences> {
        // `name_at_including_directives` rather than `name_at`: the place a user asks this question is the name
        // itself, and half the time that name is written in a `#define` — which is tokens in a directive, not a
        // name node the scope walker sees.
        let Some((name, _)) =
            crate::sema::resolve::name_at_including_directives(&view.root, offset.get())
        else {
            return Known::Unknown(UnknownReason::UnparsableName);
        };

        macro_references(
            self.store.index(),
            &self.files,
            &name,
            ReferenceBudget::default(),
        )
    }
}

/// **Is the module name at `at` a partition** — the `:area` of `export import :area;`?
///
/// The tree keeps the partition's name without its colon (`ModuleName "area"`), and the colon is a token before it;
/// so the question is answered by the nearest non-whitespace byte before the name (`import : shapes;` is the same
/// import with a space in it). A module's own name is written with dots, never a colon, so a `:` here is the
/// partition marker and nothing else.
fn a_partition_is_written_before(written: &str, at: usize) -> bool {
    let bytes = written.as_bytes();
    let mut cursor = at.min(bytes.len());
    while cursor > 0 && bytes[cursor - 1].is_ascii_whitespace() {
        cursor -= 1;
    }
    cursor > 0 && bytes[cursor - 1] == b':'
}
