//! **Diagnostics** — the parse errors, the checks, and the pull state a client sees.
//!
//! Moved here from `session/mod.rs` without a change. `diagnostics` is the entry point the LSP handler and the batch
//! tool both call, and it is the only place that decides which reading a finding's range belongs to.

use super::*;
use crate::FileSummary;
use crate::FileView;
use crate::ImportTarget;
use crate::ModuleInfo;
use crate::cache;
use crate::file;
use crate::sema;

impl<F: FileProvider + Clone> Session<F> {
    /// **What to report about one file, said in the file's own coordinates** — the answer the diagnostics channel
    /// publishes.
    ///
    /// Two readings can answer, and which one does is the whole point of this method:
    ///
    /// * the file has a **cooked** reading (the index holds one, built by [`Session::cook`]) → its parse errors, the
    ///   ones the parser found in the text a compiler actually parses. A declaration a macro writes stops being a
    ///   guess, and a branch nobody takes stops producing errors at all;
    /// * it does not → the file's own text, parsed here ([`Session::view`]).
    ///
    /// # Why the two are not merged
    ///
    /// Unioning them would publish, for a file that was cooked, exactly the errors the cooked reading exists to
    /// remove: the raw reading sees text the preprocessor never shows the compiler, so its errors include the ones
    /// inside untaken branches and the ones a macro's own shape provokes. Whoever has to choose — and this is the
    /// choice — should choose the reading that matches what the compiler sees.
    ///
    /// # What is *not* claimed
    ///
    /// A cooked answer is not automatically "clean": the rendering is the compiler's text, and a grammar gap is a
    /// grammar gap (MSVC's `sourceannotations.h` fails in both readings for a reason that is not the macros').
    /// Errors the rendering has and this file cannot show — text written in another file's macro body — are counted
    /// into [`FileDiagnostics::unplaced`] rather than dropped in silence, because "the list is empty" must not mean
    /// two different things.
    ///
    /// # Staleness, which is the caller's to handle
    ///
    /// A cooked reading is dropped when the file or its environment changes ([`ProjectIndex::forget_cooked`]),
    /// so between an edit and the next drain this falls back to the raw reading — and a caller that publishes that
    /// answer will publish the coarser one. The LSP's diagnostic service answers that by re-diagnosing once the
    /// queue drains; a caller that publishes and forgets would show the raw errors until the next edit.
    /// **Why this toolchain cannot see these modules — and this file's own partitions — one note each, pointing at
    /// the `import` line that wrote it.**
    ///
    /// The sentence the analysis owes a reader who wrote `import std;` and got nothing: "no file in this project
    /// declares module `std`" is true and useless, because the file *does* exist on the machine — it is outside the
    /// project, in a directory only the compiler knows about, and which directory that is **differs per compiler**.
    /// See [`crate::Toolchain::module_note`], where the per-compiler facts are written down with the measurement
    /// behind each one.
    ///
    /// # Partitions, which are the same failure and a different sentence
    ///
    /// `import :area;` names a partition of the module **this file declares**, so the question is not "which file
    /// declares module `area`" — nothing does, and asking it is why a partition import used to look like an import
    /// of a module nobody has. It is "which file declares partition `area` of `shapes`", and what the compiler
    /// needs is not a module built but the module's interface compiled *against* that partition's `.ifc`
    /// ([`crate::Toolchain::partition_note`], and `target/build_partitions.bat` for the three commands that
    /// establish it).
    ///
    /// Only a partition of the file's **own** module is asked about: `import :part;` in a file that declares no
    /// module is a different mistake — there is no module for the partition to belong to — and it is left to the
    /// reader of the syntax rather than described as a missing file.
    ///
    /// Empty when nothing is unreadable, which is the ordinary case for a project whose modules are its own files.
    ///
    /// # Where the range comes from, and why the tree rather than the summary
    ///
    /// A range is what makes the note *about* the line the reader wrote, and the summary does not keep one — it
    /// records the module names, because the visibility walk needs names and nothing else. So the file's own tree is
    /// read here, which the caller has already built ([`crate::FileView`]): the cost is one walk of a tree that
    /// exists, and the alternative is a second field on every import of every file in the project for a diagnostic
    /// that appears on a handful of them.
    ///
    /// # Why the notes are not [`FileDiagnostics`]
    ///
    /// They are not errors, and `FileDiagnostics` is the *errors* — a type whose severity is fixed at one value
    /// because everything in it is a parse failure. These are information, and merging them would mean a field whose
    /// only two values are "what everything else in the list is" and "this one", which is a field that says nothing.
    pub fn notes_about_the_modules(&self, view: &FileView) -> Vec<ModuleNote> {
        let info = ModuleInfo::from_tree(&view.root);
        let own_module = info.module_name.clone();
        let mut notes = Vec::new();

        for declaration in &info.imports {
            // **The note's range is a fact about the file the reader wrote, not about the rendering.** Its consumer is
            // the diagnostics channel, which turns it into a position with `position_in_file(held, note.start)` — the
            // *file's* text — so a range in the rendering's coordinates lands on the wrong line: measured,
            // `an_import_nothing_declares_gets_a_note_on_the_line_that_wrote_it` read `left: 0, right: 46`, the note
            // having started at the top of a rendering whose comments, blank lines and includes are gone. This is the
            // outbound half of the same crossing every other query makes at the seam, and a note that cannot be placed
            // in the file is dropped rather than shown somewhere wrong.
            // **Each end falls back on its own**, rather than the note being dropped when one of them has no span in
            // the rendering: a note is worth showing even if only one end can be placed, and dropping it turned a
            // wrong-line note into no note at all (measured: `a_partition_the_project_does_not_have_gets_a_note_about_its_module`
            // read `left: 0, right: 1`).
            let start = view
                .file_offset_of(ReadingOffset::new(declaration.range.start_offset))
                .map_or(declaration.range.start_offset, |at| at.get());
            let end = view
                .file_offset_of(ReadingOffset::new(declaration.range.end_offset()))
                .map_or(declaration.range.end_offset(), |at| at.get());
            let range = (start, end);

            // A partition of this file's own module: a file has to declare the module, and the project has to be
            // missing the partition's file, for there to be anything to say.
            if let Some(partition) = declaration.target.partition_name() {
                let Some(module) = own_module.as_deref() else {
                    continue;
                };

                if self.the_partition_is_read(module, partition) {
                    continue;
                }

                notes.push(ModuleNote {
                    message: match &self.toolchain {
                        Some(toolchain) => toolchain.partition_note(module, partition),
                        None => format!(
                            "no file this project contains declares partition `:{partition}` of module `{module}` \
                             — a partition is not a module, and until its file is read the names it exports are \
                             unknown rather than absent"
                        ),
                    },
                    start: range.0,
                    end: range.1,
                });

                continue;
            }

            let Some(module) = declaration.target.module_name() else {
                continue;
            };

            // A module this project declares is not a note: the reader can go to it, and telling them where their
            // compiler keeps modules would be an answer to a question they did not ask.
            if self.store.index().interface_unit_of(module).is_some() {
                continue;
            }

            let message = match &self.toolchain {
                Some(toolchain) => toolchain.module_note(module),
                // No compiler answered, so there is no per-compiler fact to give — and a sentence naming the
                // wrong compiler's switches would be worse than one naming none.
                None => format!(
                    "no file this project contains declares module `{module}` — a module outside the project is \
                     one only the compiler can see, and until then the names it exports are unknown rather than \
                     absent"
                ),
            };

            notes.push(ModuleNote {
                message,
                start: range.0,
                end: range.1,
            });
        }

        notes
    }

    /// **Did any file this session read turn out to be the interface unit of `module:partition`?**
    ///
    /// Asked of the **summaries** rather than of a resolution, so that the answer is "the project has this file"
    /// rather than "the naming convention proposes one": a partition's file is read by the index like any other
    /// module unit, and the reading records which module and partition it declares
    /// ([`crate::ModuleReading::partition`]).
    ///
    /// A linear scan over the summaries, which is the same shape as every other question the index cannot key — and
    /// it runs once per partition import of one file, which is a handful.
    pub(crate) fn the_partition_is_read(&self, module: &str, partition: &str) -> bool {
        self.store.index().summaries().any(|summary| {
            summary.modules.module.as_deref() == Some(module)
                && summary.modules.partition.as_deref() == Some(partition)
                && summary.modules.is_interface
        })
    }

    /// **Which interface unit a module name or a partition at this position names** — what a jump on an import
    /// goes to.
    ///
    /// `import :shapes;` names the partition `shapes` **of the module the file itself declares** ([module.unit]
    /// places the module declaration before every import), so the file it names is `geometry:shapes` — *not* a
    /// top-level module called `shapes`. Reported from an editor: a jump on `:shapes` in `geometry.cppm` landed in
    /// `shapes.cppm`, a different module that happens to share the partition's spelling.
    ///
    /// The resolution is the summary's: `modules.partitions` holds one path per partition import, in import order,
    /// resolved when the file was read by the same scanner that resolves an `import`. This maps the *position* to
    /// its entry, which is the half the summary cannot keep — it stores paths, not where each one was written.
    ///
    /// `in_the_file` is an offset in the file's own text (an import is not in the rendering — it is resolved),
    /// the same coordinate [`Session::header_at`] takes.
    pub fn module_unit_at(
        &self,
        view: &FileView,
        in_the_file: file::coordinates::FileOffset,
    ) -> Option<ModuleUnitTarget> {
        let summary = self.store.index().summary(&view.path)?;
        let info = ModuleInfo::from_tree(&view.root);
        let (position, import) = info.imports.iter().enumerate().find(|(_, import)| {
            let at = in_the_file.get();
            import.range.start_offset <= at && at < import.range.end_offset()
        })?;

        match &import.target {
            // **A partition**: the nth partition import of this file, because that is the order the summary
            // resolved them in. The module it belongs to is the file's own, by the language's own rule.
            ImportTarget::Partition(name) => {
                let nth = info.imports[..position]
                    .iter()
                    .filter(|earlier| earlier.target.partition_name().is_some())
                    .count();
                let file = summary.modules.partitions.get(nth)?.clone();
                let owner = summary.modules.module.as_deref().unwrap_or_default();
                Some(ModuleUnitTarget {
                    file,
                    name: format!("{owner}:{name}"),
                })
            }
            // **A module name**: the interface unit the index knows it by.
            ImportTarget::Module(name) => {
                let interface = self
                    .store
                    .index()
                    .interface_units()
                    .into_iter()
                    .find(|(module, _)| module.as_str() == name.as_ref())
                    .map(|(_, unit)| PathBuf::from(unit))?;
                Some(ModuleUnitTarget {
                    file: interface,
                    name: name.to_string(),
                })
            }
            // A header unit is a header: `header_at` answers it, and it declares no module.
            ImportTarget::HeaderUnit { .. } => None,
        }
    }

    pub fn diagnostics(&self, path: impl AsRef<Path>) -> Option<FileDiagnostics> {
        let path = path.as_ref();
        let _whole = DiagTrace::new("diagnostics");

        // The index's own spelling of the path, for the reason `cook` documents: a client's `C:\…` and the
        // resolver's `c:/…` are the same file, and a lookup by the client's spelling finds nothing.
        let key = self
            .store
            .index()
            .summary(path)
            .map(|summary| summary.path.clone())
            .unwrap_or_else(|| path.to_path_buf());

        // **The cooked reading first, and without parsing anything.** Its errors are already placed in the file
        // (that is what `index_rendering` did), so answering from it costs a lookup — while the raw answer needs the
        // file's own tree, which is a parse per request. A reading the index holds is a reading of the text the VFS
        // is holding now: a change to the file or to its environment drops it (`forget`, `forget_cooked`).
        //
        // **The module notes are taken from the same tree, when there is one.** A caller that asks
        // [`Session::diagnostics`] and then [`Session::notes_about_the_modules`] with a view of its own parses the
        // file twice, once for each answer, and the file's tree is the expensive half of both. So the notes are
        // attached here — from the tree this call already has, or from a view it reads once — and the method that
        // takes a view stays for a caller that has one and wants nothing else.
        let mut notes = Vec::new();

        if let Some(cooked) = self.store.index().cooked_reading(&key) {
            // The tree the raw answer would have needed, if the session can still read the file: the notes are about
            // the imports as they are *written*, and a file that has gone missing since it was indexed has no text to
            // point at. Reading it here rather than making the caller ask again is what keeps one request to one
            // parse — and the view is a parse of a file the VFS is already holding, because the cooked reading could
            // not exist for a file nothing read.
            //
            // The **checks** read it too, and for the same reason: they are asked of the file's facts and its tree,
            // and both come from this one view. A file that cannot be read back is a file with no checks, which is
            // the same answer as a file no check had anything to say about.
            let view = {
                let _t = DiagTrace::new("the file's tree (view)");
                self.view(path)
            };
            if let Some(view) = &view {
                let _t = DiagTrace::new("module notes");
                // **A note about a module says what the session has *read*, so it waits for the reading.**
                //
                // `no file this project contains declares module `std`` was published on the first pull after a
                // window opened and gone by the next edit, because on that pull the index had not yet read the
                // project`s module units — the answer "no file declares it" was true of what had been read and
                // false of the project. The same timing rule the checks already obey applies: nothing is said
                // while the session still has work, because absence of evidence is not evidence here.
                notes = if self.pending_work() == 0 {
                    self.notes_about_the_modules(view)
                } else {
                    Vec::new()
                };
            }
            // **The gate is asked before the rules run.** A finding may only be published once this file's closure
            // has been read ([`Session::is_closure_ready`]) — a check reads absence out of the index, and an index
            // still filling answers "no such member" about a member whose header nobody has read. The server used to
            // ask that question *after* this call, which meant every pull paid for four rules resolving types and
            // then threw the answer away: measured on a 4 MB translation unit, that is seconds per pull, on the
            // first pull of a window, for a file whose findings could not be shown anyway. Asking it here is the
            // same decision at the only place where not paying for the work is still possible.
            //
            // Errors and notes are *not* gated: an error is about the text the reader wrote and is the same answer
            // at any index state, and a note about a module is already withheld while the session has work.
            let trusted = self.is_closure_ready(&key);
            let checks = match (&view, trusted) {
                (Some(view), true) => self.checks_about(&key, view),
                _ => Vec::new(),
            };

            return Some(FileDiagnostics {
                reading: DiagnosticReading::Cooked,
                unplaced: cooked.unplaced,
                // **A file that says it cannot be read on its own.** Some headers are *fragments*: their body is a
                // piece of an initializer that another file opens and closes around them. libstdc++'s
                // `bits/text_encoding-data.h` is one — a list of `{ 3, "US-ASCII" }` rows, guarded by
                // `#error "This is not a public header, do not include it directly"`. Read as a translation unit it
                // is **895** parse errors, every one of them a consequence of the brace that has no opening; a
                // compiler says the same thing in one line, which is what its own `#error` is for.
                //
                // So the message is published and the cascade is not. Two signals together, and both are facts: the
                // file writes an `#error` line, **and** its rendering begins with a `{` or a `}` — a token no
                // declaration starts with. A file with an `#error` in a branch nobody takes, or a fragment with a
                // typo elsewhere, keeps its own errors: the pair has to hold.
                errors: match view.as_ref().and_then(Self::a_fragment_and_its_message) {
                    Some((start, end, message)) => vec![FileDiagnostic {
                        start,
                        end,
                        message,
                        code: sema::check::DiagnosticCode::SyntaxError,
                    }],
                    None => cooked
                        .diagnostics
                        .iter()
                        .map(|error| FileDiagnostic {
                            start: error.range.start_offset,
                            end: error.range.end_offset(),
                            message: error.message.clone(),
                            // **Every diagnostic carries its rule.** These came from the parser, so the rule is
                            // `syntax-error` — stated here rather than left for the client to guess from the message.
                            code: sema::check::DiagnosticCode::SyntaxError,
                        })
                        .collect(),
                },
                notes,
                checks,
                checks_withheld: !trusted,
            });
        }

        // **No cooked reading means no answer — and it also means no *view*, which is why there is no second arm.**
        //
        // This used to fall through to `self.view(path)` and report the errors of the file's own text. That is gone
        // for the reason [`DiagnosticReading::Cooked`] records: an untaken branch is not compiled, so an error found
        // inside one is an error about a program that does not exist. Measured on the fixture in
        // `a_branch_nobody_takes_reports_nothing_once_the_file_is_cooked`: the same `#if 0` branch produced **1**
        // error as the file's own text and **0** once cooked.
        //
        // The arm is deleted rather than kept returning `None`, because a file with a rendering always has a cooked
        // reading (`index_rendering` files one for every rendering it is given), so the fall-through was already
        // unreachable — and unreachable code that reports diagnostics is the shape of the defect this removed.
        None
    }

    /// **A file that declares itself a fragment**, with the message it wrote for that case.
    ///
    /// The two signals, both facts and both required: the file **writes an `#error` line**, and the **first token of its
    /// rendering is a brace**. A fragment's body begins with the `{` of the initializer its including file opens;
    /// nothing else starts a translation unit that way, and a file that has an `#error` in a branch nobody takes still
    /// starts with a declaration. See the call site for the header this was measured on.
    pub(crate) fn a_fragment_and_its_message(view: &FileView) -> Option<(usize, usize, String)> {
        let written = view.written_text().unwrap_or(&view.source);
        let (start, end, message) = Self::error_directive_line(written)?;

        // The rendering's first non-trivia token, read from the tree rather than from the text: comments and directives
        // are already gone from it, so a `{` here is the file's first real token.
        let first = view
            .root
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .map(|token| cpp_parser::CppTokenKind::from(token.kind()))
            .find(|kind| {
                !matches!(
                    kind,
                    cpp_parser::CppTokenKind::Whitespace | cpp_parser::CppTokenKind::Newline
                )
            })?;
        if !matches!(
            first,
            cpp_parser::CppTokenKind::LeftBrace | cpp_parser::CppTokenKind::RightBrace
        ) {
            return None;
        }

        Some((start, end, message))
    }

    /// The first `#error` line of a written text: where it is, and what it says.
    pub(crate) fn error_directive_line(text: &str) -> Option<(usize, usize, String)> {
        let mut at = 0;
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if let Some(rest) = trimmed.strip_prefix('#') {
                let rest = rest.trim_start();
                if let Some(message) = rest.strip_prefix("error") {
                    let message = message.trim().trim_matches('"').trim();
                    return Some((at + indent, at + line.trim_end().len(), message.to_string()));
                }
            }
            at += line.len();
        }
        None
    }

    /// The checks' answer for one file — see [`crate::sema::check`].
    ///
    /// An empty list when the index holds no summary for the path, which is the same answer as "no check had
    /// anything to say": a check is a claim about facts the analysis has, and a file it has no facts about is a
    /// file it has nothing to claim.
    ///
    /// Nothing is parsed here. Every check is answered from the summary the reading already produced, so asking
    /// for diagnostics is not a reason for a parse to happen — it is a reason to read what one produced.
    pub(crate) fn checks_about(&self, key: &Path, view: &FileView) -> Vec<sema::check::Finding> {
        let index = self.store.index();
        let Some(summary) = index.summary(key) else {
            return Vec::new();
        };

        // **The model is built here, and it is what the checks are handed.** One per `diagnostics` call, which is
        // the lifetime that makes its memo need no invalidation rule — see [`Session::semantic_model`]. The view is
        // already a parse this call paid for, so the model costs a `Box` and an empty `BTreeMap`.
        //
        // Owned by `Checks` rather than borrowed, because the model is a *view* — a handful of references and a memo
        // — so moving it into the value that uses it costs nothing and removes a borrow that could not outlive this
        // function's own frame.
        // **The second pull of an unchanged file answers from the first one's findings.** The key names every input a
        // finding can depend on — see [`Session::the_state_of_a_pull`], which no longer includes the store's global
        // revision.
        let state = self.the_state_of_a_pull(key, summary);
        if let Ok(memo) = self.checks_memo.lock()
            && let Some((remembered, findings)) = memo.as_ref()
            && *remembered == state
        {
            self.pull_hits
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return findings.clone();
        }
        self.pull_runs
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let model = self.semantic_model(view);

        // **The config, and the run.** The rules come from `.cppls.toml`'s `[diagnostics]` section when a project
        // writes one, and every rule is on otherwise — see `CheckConfig::default`. The filtering lives in
        // `CheckContext::add`, so that no check has to know a rule was switched off.
        //
        // `enable` is "only these" and `disable` is "not these", with `disable` winning: the pair a project wants —
        // `enable` for one that starts from nothing, `disable` for the single rule that is noise. Unknown names are
        // logged rather than ignored: a rule a user believes is on and that never runs is the failure this
        // mechanism exists to prevent.
        let config = sema::check::CheckConfig::from_names(
            &self.project_config().config.diagnostics.enable,
            &self.project_config().config.diagnostics.disable,
        );
        for name in config.unknown() {
            log::warn!("`[diagnostics]` names a rule this build does not have: `{name}`");
        }
        let inside = {
            let mut inside: Vec<std::path::PathBuf> = vec![self.root.clone()];
            inside.extend(
                self.project
                    .iter()
                    .filter_map(|file| file.parent().map(std::path::Path::to_path_buf)),
            );
            // **A library is the project's too**, as far as a suggestion is concerned: its headers are what a target
            // that depends on it may add an `#include` for. See `missing_include`.
            inside.extend(
                self.project_config()
                    .config
                    .libraries
                    .iter()
                    .map(|library| library.root(&self.root)),
            );
            inside.sort();
            inside.dedup();
            inside
        };
        let inactive = self
            .inactive_regions(key)
            .map(|regions| regions.spans)
            .unwrap_or_default();
        let findings = sema::check::run(sema::check::CheckContext::new(
            key,
            summary,
            model,
            &view.source,
            // **Both texts**: the checks read ranges from the tree (the rendering) and from the summary (the
            // file), and mixing the two is a measured defect - see `CheckContext::written`.
            view.written_text().unwrap_or(&view.source),
            view.tree.get_errors(),
            view,
            &inactive,
            config,
            // **The language this file is compiled as**, which is what makes a C-only rule quiet in C: the compile flags say
            // `-std=c11`, and `cstyle_cast` is then not a rule about this file at all. Asked through
            // [`Session::config_for_file`], which is also where **the client's `languageId`** is consulted for a file
            // the project says nothing about — a header — so that a C project's `.h` files are not answered with
            // C++ rules (`docs/c-support.md` §8).
            self.config_for_file(key).language,
            // **Whether the project is Unreal Engine code** — see `[unreal] enable`. Read here rather than inside a
            // check so that the switch has one home.
            self.project_config()
                .config
                .unreal
                .as_ref()
                .is_some_and(|unreal| unreal.is_on()),
            // **Which files this project may add an `#include` for** — the workspace root and the directories the
            // project's own files are in. (The second half is not redundancy: a session over a *virtual* filesystem —
            // a test, a remote client — has paths that do not start with its root, and the rule must work there too.)
            // Once `[library]` exists its paths are added here as well. See `CheckContext::inside`.
            &inside,
        ));
        // Stored, then answered: the next pull of this state is a lookup.
        if let Ok(mut memo) = self.checks_memo.lock() {
            *memo = Some((state, findings.clone()));
        }
        findings
    }

    /// **Everything a pull's findings are a function of**, as one number.
    ///
    /// # The revision is not in here
    ///
    /// It used to be, and it made the memo a one-slot cache for the *project* rather than for the file: `revision`
    /// moves on any summary insert, so a keystroke in one file invalidated every other file's findings. The review in
    /// `docs/perf-review.md` names it, and the numbers are one pull per keystroke instead of one pull per state.
    ///
    /// What replaces it is narrower and is enough:
    ///
    /// * **the file's own key** — content, context and environment, so its own edit invalidates;
    /// * **the closure's fingerprint** — `(path, content_hash)` for every file the closure reaches, so an edit to a
    ///   header this file includes invalidates, whether the edit is a macro, an include or a declaration;
    /// * **how many files the index holds** — the one thing a check reads outside the closure: `missing-include`
    ///   offers a header name from the directories the project is in, and a file *added* to or *removed* from the
    ///   project changes that answer. An edit to a file outside the closure does not, which is the narrowing this is
    ///   for;
    /// * the rules enabled, the file's language, and whether the project is Unreal code.
    ///
    /// A closure too large to fingerprint answers `None`, and then the revision goes back in: a key that is too
    /// coarse is a slow pull, while one that is too narrow is a wrong finding.
    pub(crate) fn the_state_of_a_pull(&self, key: &Path, summary: &FileSummary) -> u64 {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(format!("{:?}", summary.key).as_bytes());
        match self.store.the_closure_fingerprint(key) {
            Some(closure) => bytes.extend_from_slice(&closure.to_le_bytes()),
            None => bytes.extend_from_slice(&self.store.revision().to_le_bytes()),
        }
        bytes.extend_from_slice(&(self.store.index().len() as u64).to_le_bytes());
        let config = self.project_config();
        for rule in config
            .config
            .diagnostics
            .enable
            .iter()
            .chain(config.config.diagnostics.disable.iter())
        {
            bytes.extend_from_slice(rule.as_bytes());
            bytes.push(0x1f);
        }
        bytes.extend_from_slice(format!("{:?}", self.config_for(key).language_for(key)).as_bytes());
        bytes.push(u8::from(
            config
                .config
                .unreal
                .as_ref()
                .is_some_and(|unreal| unreal.is_on()),
        ));
        cache::fnv1a64(&bytes)
    }

    /// **Answer the pull for the files a reader has open, before it is asked for.**
    ///
    /// A pull's first answer for a new reading is the expensive one — measured on `CodeFormat.cpp`, **36.6 ms** (18.1 ms of
    /// inference) for the pull after an edit and **109 ms** for the first pull of a session, against **2.2 ms** for a
    /// repeat. Those numbers are the first answer for a state, and the pump is the only party that knows when a state
    /// exists and nobody is waiting for its answer: it has just committed the reading. So it asks the same question the
    /// client will ask, and the `checks_memo` it fills is what the client's pull finds.
    ///
    /// **At most two files** — a reader has one open file and the header beside it — and only what `diagnostics` can
    /// answer at all: an uncooked reading is a `None` here and a `None` there, so the warm costs a lookup.
    pub fn warm_the_pulls(&self) -> usize {
        let mut warmed = 0;
        for path in self.documents.paths().into_iter().take(2) {
            if self.diagnostics(&path).is_some() {
                warmed += 1;
            }
        }
        warmed
    }
}
