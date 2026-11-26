//! **Views and renderings** — the text a request is answered from, and the macros it sees.
//!
//! Moved here from `session/mod.rs` without a change. Two texts exist per file and the difference is the whole design:
//! the *written* view is what the reader typed, and the *rendering* is what a compiler would read — includes inlined,
//! macros expanded, directives resolved.

use super::*;
use crate::cache;
use crate::preprocess;
use crate::summary;

/// **The regions of a file this configuration does not compile**, with the one fact a caller that paints them must
/// respect — the answer to "may I paint these?" in one value.
///
/// The spans are the preprocessor's own by-product: one range per dead branch, in the file's coordinates, directive
/// lines included on purpose so one dead branch is one range. `assumed_undefined` is what says whether the
/// configuration *knew* when it decided them — a branch nobody defined a name for is a branch that was **guessed**,
/// and a guess painted grey is the failure this feature must not have (`docs/inactive-regions.md` §8).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InactiveRegions {
    /// The dead regions, in source order, in the file's own coordinates.
    pub spans: Vec<cpp_parser::SourceRange>,
    /// Branches decided by C's "an undefined identifier is `0`" rule rather than by a definition the walk held.
    ///
    /// Zero means every span was decided from a definition the cook actually had. Non-zero means the project's
    /// configuration is incomplete — no compile database, an include nobody resolved — and a span may cover code the
    /// user's real build compiles.
    pub assumed_undefined: usize,
}

impl InactiveRegions {
    /// **May this answer be painted?** — every span was decided from a definition the configuration had.
    pub fn is_exact(&self) -> bool {
        self.assumed_undefined == 0
    }
}

impl<F: FileProvider + Clone> Session<F> {
    /// **The file's own tokens — the reading that means nothing and is for positions.**
    ///
    /// For a caller whose question is about the **text**: a fold, a selection range, a bracket, a highlight, the span
    /// of a diagnostic, the bytes a rename would edit. None of those needs a declaration to mean anything, and that
    /// is exactly why this is a different function from [`Session::view`] rather than a fallback inside it.
    ///
    /// Returns the file's text, its line index and its **tokens** — and deliberately **no tree and no scopes**: a
    /// caller that had those would eventually ask them what a name means, and the answer would be about unexpanded
    /// text. A caller that needs meaning calls [`Session::view`] and **defers** when it answers `None`.
    ///
    /// `None` only when the file is neither open nor readable.
    pub fn tokens_of(&self, path: impl AsRef<Path>) -> Option<TokensOf> {
        let file = self.vfs.held(path)?;
        Some(TokensOf {
            path: file.path.clone(),
            source: file.text.clone(),
            line_index: file.line_index.clone(),
            open: file.open,
            tokens: cpp_parser::lex(&file.text, &cpp_parser::LexerConfig::default()).0,
        })
    }

    /// Check whether the current text has a rendering without parsing a view or scheduling work.
    pub fn has_rendering(&self, path: impl AsRef<Path>) -> bool {
        let requested = path.as_ref();
        let key = self
            .store
            .index()
            .summary(requested)
            .map(|summary| summary.path.as_path())
            .unwrap_or(requested);
        self.vfs
            .held(key)
            .is_some_and(|file| self.known_rendering_of(&file.path, &file.text).is_some())
    }

    /// The file as the cursor is in it: the buffer when it is open, the disk otherwise.
    ///
    /// `None` when there is neither — a path that is not open and cannot be read. Everything below takes a view,
    /// so one parse per request serves every question about that request's file.
    ///
    /// The view is **of the session's VFS**: the text and its line index come from the file the VFS is holding, and
    /// the view shares both rather than copying either. What is done here is the parse and the scopes, which are
    /// the two things a position needs and a summary cannot hold.
    /// # Why a file with no rendering answers `None`, which is the whole point
    ///
    /// It used to answer from the file's own tokens — a parse of unexpanded text. That is not a lesser answer, it is
    /// an **untrue** one: C++ source with its macros unexpanded is not a program, and a tree built from it is shaped
    /// like a syntax tree and meaningless as one (measured on MSVC's headers: `<xutility>` 84 error nodes raw
    /// against 0 cooked, `<xstring>` 17 against 0 — see `examples/cooked_parse.rs`).
    ///
    /// So the answer is deferred instead. The work is queued exactly as before, and the caller is told **nothing**
    /// rather than told something false — which is this crate's own rule for an answer it does not have
    /// (`Known::Unknown` over a guess), applied to the reading rather than to a name. A caller that needs a
    /// *position*, a token or a fold does not want this function at all: see [`Session::tokens_of`].
    ///
    /// **The cost of this is real and has to be watched**: a file with no rendering is now answered on the request
    /// *after* the one that asked for it, where before it was answered immediately and wrongly. `pending_cooking`
    /// and the diagnostics channel's `isIncomplete` are how a caller states that, and the alternative is not
    /// "an answer sooner" — it is "a wrong answer sooner".
    /// # Why this takes only `&self`, and what it costs a caller
    ///
    /// A query must not pay a unit walk, a preprocess and a render, so this **asks** rather than builds: it queues
    /// into `macro_work` — the asking side's queue, which the pump drains into a cook — and answers `None`. That is
    /// the whole arrangement, and it is why the ask is a lock rather than a `&mut self`.
    ///
    /// The cost is one case it cannot serve: a file the index has **never described**. Queueing one for a cook is not
    /// enough, because a cook is built out of a summary ([`Session::cook`] answers `None` without one), so such a
    /// file must also be queued for **reading** — which is [`Session::want_cooked_reading`], and that needs
    /// `&mut self`. A caller holding one calls it first and then asks:
    ///
    /// ```text
    ///   session.want_cooked_reading(&path);   // read it *and* cook it
    ///   session.index_everything();           // the pump does both
    ///   let view = session.view(&path);       // the reading
    /// ```
    ///
    /// Measured without that first line, on a fixture that loads a file and pumps: `indexed false, pending 0,
    /// backlog 0` — the ask had nothing to name and every later `view` answered `None` for ever. Making `view`
    /// itself `&mut self` was tried and does not work: `vfs.held(path)` borrows the session immutably for the whole
    /// body, so the mutation cannot sit beside the lookup it depends on.
    pub fn view(&self, path: impl AsRef<Path>) -> Option<FileView> {
        // **The index's own spelling of the path, not the caller's.** A client sends `C:\…`, the resolver wrote
        // `c:/…`, and the rendering cache is keyed by *the index's* spelling — so a lookup by the caller's finds
        // nothing, and the miss is indistinguishable from "this file was never cooked". The same rule
        // [`Session::diagnostics`] and [`Session::cook`] already apply, for the same reason; it was missing here,
        // which is why a view could answer `None` for a file whose rendering was in the cache.
        let requested = path.as_ref();
        let key = self
            .store
            .index()
            .summary(requested)
            .map(|summary| summary.path.clone())
            .unwrap_or_else(|| requested.to_path_buf());
        // **A miss is otherwise indistinguishable from "never cooked"**, which is what made a file that *had* been
        // cooked answer `None` for every query about it. See `CPPLS_TRACE_VIEW`.

        let file = self.vfs.held(&key)?;

        // **The rendering, or a request for it — never the file's own tokens.**
        //
        // A rendering is what a compiler's parser is handed: the file with its macros replaced, so `_STD` is
        // `::std::` and `_STD_BEGIN` is `namespace std {`, and nothing in the text is an invocation any more. That
        // is the reading this view is **of**.
        //
        // Nothing here builds it. A rendering is a unit walk, a preprocess and a render — measured at 65–190 ms for
        // a standard-library header on an indexed project, and far more on a cold one — and a query must not pay
        // that. So a file with no rendering yet is put in the work loop's hands and this call answers `None`; the
        // next query about it finds the rendering built. The same "not now, next time" the diagnostics channel
        // states through `isIncomplete`.
        match self.known_rendering_of(&file.path, &file.text) {
            // **Parsed as the language the file is**, not as the parser's default: see
            // [`FileView::parse_rendering_in`] — `restrict`, C23's literals and the GNU extensions all depend on it.
            //
            // **And only once per (rendering, configuration).** The parse is the expensive half of every request that
            // asks a question about a position — measured at the parser's 6 MB/s, so 120 ms for a 0.74 MB unit — and
            // it was being paid on every call. The key is [`ViewKey`]: the rendered text's hash, the span count, the
            // file's identity, the language, `cuda` and whether the buffer is open. A miss parses; a hit is a few
            // reference-count bumps.
            Some(rendered) => {
                let config = self.config_for_file(&file.path);
                let key = ViewKey {
                    path: file.path.clone(),
                    rendering: Some(RenderingId(rendered.clone())),
                    // The rendering is the text: no second hash of the same bytes, and no `spans` — an object's
                    // identity covers both.
                    text: 0,
                    id: file.id,
                    cuda: config.cuda,
                    language: Some(config.language),
                    open: file.open,
                    // A rendering has no invocations left in it, so its parse takes no macro environment.
                    environment: None,
                };
                if let Some(held) = self.a_view_already_parsed(&key) {
                    return Some(held);
                }
                let view = FileView::parse_rendering_in(file, &rendered, &config.config);
                self.remember_a_view(key, &view);
                Some(view)
            }
            None => {
                self.ask_for_a_rendering(&file.path, &file.text);
                None
            }
        }
    }

    /// **Forget every cached rendering** — the test's stand-in for eviction, which the real cache does by size.
    #[cfg(test)]
    pub(crate) fn forget_the_renderings(&self) {
        if let Ok(mut known) = self.renderings.lock() {
            known.clear();
        }
    }

    /// **The regions of the file this configuration does not compile**, in the file's own coordinates — what an
    /// editor greys out.
    ///
    ///
    /// Answered from the file's **rendering** ([`Session::known_rendering_of`], which also reads a persisted record
    /// back), because the spans are a by-product of the walk that decided the branches: they are offsets into the
    /// file's own text, so a consumer turns one into a line/column range with the file's own line index and no
    /// mapping.
    ///
    /// `None` when no rendering is read yet: the call asks for one, and a later request answers. That is the same
    /// "not now, next time" contract [`Session::view`] documents, and it is why the caller must be the pump's idle
    /// drain or a request that can re-ask. **A file with no directives answers `Some(empty)`** — "nothing is
    /// inactive" is an answer, and `None` is reserved for "nobody has read this yet".
    pub fn inactive_regions(&self, path: impl AsRef<Path>) -> Option<InactiveRegions> {
        // The index's spelling, for the reason `view` gives: a client sends `C:\…`, the resolver wrote `c:/…`, and a
        // lookup by the caller's spelling finds nothing.
        let requested = path.as_ref();
        let key = self
            .store
            .index()
            .summary(requested)
            .map(|summary| summary.path.clone())
            .unwrap_or_else(|| requested.to_path_buf());
        let file = self.vfs.held(&key)?;
        if let Some(rendered) = self.known_rendering_of(&file.path, &file.text) {
            return Some(InactiveRegions {
                spans: rendered.inactive.clone(),
                assumed_undefined: rendered.assumed_undefined,
            });
        }
        self.ask_for_a_rendering(&file.path, &file.text);
        None
    }

    /// **Ask for a file's rendering, once per text** — see [`Session::renderings_asked`].
    ///
    /// The callers are queries that could not answer from the rendering cache, and the distinction this makes is
    /// between "nobody has cooked this" and "the cook was evicted": the second is not a reason to cook again. A rule
    /// that could not tell them apart cooked the same header once per drain — the measured shape of a server that
    /// never goes idle. A file whose text moves forgets its ask ([`Session::buffer_changed`]), because the rendering
    /// the cache lost may be of a text nobody has any more.
    pub(crate) fn ask_for_a_rendering(&self, path: &Path, text: &str) {
        // **A file the index cannot describe is still asked for**, because the ask is also the signal that something
        // wants it read — `Session::view`'s contract for a file nobody has indexed. It is only *remembered* when the
        // cook can be served; otherwise a later query, after the index has read it, must be free to ask again.
        if self.store.index().summary(path).is_some() {
            let key = (path.to_path_buf(), cache::content_hash(text));
            let first = self
                .renderings_asked
                .lock()
                .map(|mut asked| asked.insert(key))
                .unwrap_or(true);
            if !first {
                return;
            }
        }
        if let Ok(mut work) = self.macro_work.lock() {
            work.want(path);
        }
    }

    /// **Ask for a file's cooked reading**, whether or not a rendering of it is cached.
    ///
    /// `Session::view` asks only when the **rendering** is missing, which is the right question for a view and the
    /// wrong one for a query about a *name*: the rendering may be in the cache while the declarations it produced
    /// were evicted from the index's bounded map, and the name is answered from the declarations. Measured on
    /// `nlohmann/json.hpp` — rendered and indexed with **761** declarations, evicted by the wave that followed, and
    /// every later ask saw a cached rendering and stopped, so the file was never re-read.
    ///
    /// Nothing is forced here: the ask is the same `macro_work` queue `Session::view` uses, and the pump serves it
    /// (`advance` moves it into the cook queue and cooks it).
    pub fn ask_for_the_cooked_reading(&self, path: &Path) {
        if let Ok(mut work) = self.macro_work.lock() {
            let _queued = work.want(path);
        }
    }

    /// **The offset in the file's rendering a position in the file names** — the reverse of
    /// `RenderedCooked::written_at`, and the half a position query needs when the file's own text cannot answer.
    ///
    /// `None` when the file has no rendering read yet (nothing has cooked it), which is a state and not an error:
    /// the caller keeps the answer it had.
    pub fn the_rendering_offset(
        &self,
        path: impl AsRef<Path>,
        in_the_file: usize,
    ) -> Option<usize> {
        let requested = path.as_ref();
        // **The index's spelling**, for the same reason `view` asks by it: a client sends `C:\…`, the resolver
        // wrote `c:/…`, and a lookup by the caller's spelling finds nothing.
        let key = self
            .store
            .index()
            .summary(requested)
            .map(|summary| summary.path.clone())
            .unwrap_or_else(|| requested.to_path_buf());
        let file = self.vfs.held(&key)?;
        let rendered = self.known_rendering_of(&file.path, &file.text);

        let rendered = rendered?;
        rendered.the_rendering_offset_of(in_the_file)
    }

    /// **The view of what the file itself writes** — its own tokens, macros and all.
    ///
    /// The counterpart of [`Session::view`], and both are needed because they answer different questions:
    ///
    /// ```text
    /// view                the reading a compiler's parser is handed — offsets in the rendering
    /// view_of_the_file    the text the reader is editing — offsets in the file
    /// ```
    ///
    /// A question about **a position** belongs to the first, because that is where declarations, types and members
    /// are resolved. A question about **a macro** belongs to the second, and it is not a fallback: a rendering has
    /// the macro already replaced, so `#define API …`, every `API` written below it, and the node a rename would
    /// edit are all *gone* from it. Renaming, colouring or hovering a macro is a question about the spelling in the
    /// buffer, and the buffer is this view.
    ///
    /// The two agree on the file's lines wherever no macro expanded, which is most of most files — and where they
    /// disagree, that is exactly the region a macro wrote, so a caller that needs the file's own positions wants
    /// this one regardless.
    /// # Why this answers `None`, and what replaced it
    ///
    /// It used to build a `FileView` by parsing the file's own bytes, and that is the defect this whole layer has
    /// been paying for: **C++ source with its macros unexpanded is not a program.** A rendering has the macro
    /// already replaced, so `#define API …` and every `API` written below it are gone from it — which is why a
    /// question about **a macro** is a question about the buffer. But a question about a macro is a question about a
    /// **spelling**, and a spelling needs no tree: [`Session::tokens_of`] answers it with the lexer's own tokens, and
    /// a caller that needs a *position* or a *fold* wants that too.
    ///
    /// What is left is the honest answer, and it is the same one [`Session::view`] gives: a caller that needs
    /// **meaning** waits for the rendering. `None` therefore means "not now" for this file, and a caller must not
    /// read it as "no such file" — [`Session::is_indexed`] and [`Session::pending_cooking`] are how those are told
    /// apart.
    pub fn view_of_the_file(&self, path: impl AsRef<Path>) -> Option<FileView> {
        let requested = path.as_ref();
        let file = self.vfs.held(requested)?;
        // Queue it **only if there is nothing to read yet** — the rule `view` states, and the reason this one used to
        // make every request work: it asked unconditionally, so the file was cooked again on every query even though
        // its rendering was in the cache. Measured on a real project with the loop probe
        // (`crates/cpp_ls/tests/loop_probe.rs`): ten diagnostics pulls caused **five** cooks, the same two headers
        // each time (`types.h`, `stl_vector.h`), each a render and a parse — and raising the cache from 8 to 64
        // entries changed the count **not at all**, which is what named the cause: the ask, not the eviction.
        // **Is a rendering in memory?** — and deliberately not "is one *known*": `known_rendering_of` falls back to
        // the disk record, and reading and decoding one is **150 ms** for a header whose closure is large. This call
        // only decides whether to *ask* for a rendering; the view below is parsed from the file's own text either
        // way, so the disk was read for an answer nothing here uses. Measured on `channel.c`: four headers at
        // 152–162 ms each, on **every** inlay-hint request (`channel.h`, `proc.h`, `uv.h`, `typval.h`).
        if !self.a_rendering_is_known_in_memory(&file.path, &file.text) {
            self.ask_for_a_rendering(&file.path, &file.text);
        }

        // **And the plain view, which this used to leave out - the whole of the answer.**
        //
        // The arrangement `macro_environments` describes is *"a query cannot pay the 103.7 ms walk, so it asks for the
        // environment and reads the **plain view**, and the work loop builds what is missing"* - and this function is
        // the "reads the plain view" half, which was never written. It queued the work and returned `None`, so every
        // caller that wanted the text the reader is typing got nothing. Measured, on the handshake suite: the
        // completion handler's one call answered `None` on **every** attempt, the query logged `(answer missing)`, and
        // the server wrote `{"result":null}` to a client that re-asked for thirty seconds.
        //
        // The environment is the one **already read** for this content - `known_macros_of` is a hash and a lookup -
        // and an empty one otherwise, which is the honest answer for a file whose include closure has not been walked.
        let known = self.known_macros_of(&file.path, &file.text);
        // **The same cache as `view`**, because the question is the same one and the cost is the same shape: a parse
        // of this file's own text with an environment that has not moved. The identity is the environment *object* —
        // held in the key, so its address cannot be reused under it — and `None` for the empty one, which is one value
        // however often it is made.
        let key = buffer_view_key(file, self.config.cuda(), known.clone());
        if let Some(held) = self.a_view_already_parsed(&key) {
            return Some(held);
        }
        let macros = known.unwrap_or_default();
        let view = FileView::parse_with_cuda(
            file,
            macros.as_ref(),
            self.config.cuda(),
            Some(self.config_for_file(&file.path).language),
        );
        self.remember_a_view(key, &view);
        Some(view)
    }

    /// **The view of a file that a compiler's parser would see**, built if it has to be.
    ///
    /// [`Session::view`] gets the same reading **one query later** without ever blocking on it. This is for a caller
    /// that would rather wait: a batch, a test, or the first query after a file is opened.
    ///
    /// # What the reading is, and why it is the one to want
    ///
    /// A file's own tokens carry macros, and a macro is a spelling rather than a grammar: `_STD widget` is a name
    /// and a name to a reader of one file, and one qualified name to a compiler. The grammar grew a family of rules
    /// for that guess — `MacroCall`, `written_like_a_macro`, `is_a_macro`, a body-shape reader — and every one of
    /// them exists because the parser was being handed text the preprocessor had not finished with. A **rendering**
    /// has none of it left:
    ///
    /// ```text
    /// the file writes     _STD_BEGIN struct widget { … }; _STD_END
    /// the rendering has   namespace std { struct widget { … }; }
    /// ```
    ///
    /// Measured over eight standard-library headers, the rendering reads **24% more declarations with no parse
    /// errors at all**, where the file's own text reported 1 to 84 errors each and recovered from them by inventing
    /// declarations — a call inside a function body read as a declaration, a local read as a file-scope name.
    ///
    /// # The two coordinate systems
    ///
    /// The view's offsets are the **rendering's**. See [`FileView::parse_rendering`], whose note is the contract:
    /// [`FileView::file_offset_of`] and [`FileView::reading_offset_of`] are the way between the two, and a caller
    /// that reports a position to a user must go through them.
    pub fn view_of_the_rendering(&mut self, path: impl AsRef<Path>) -> Option<FileView> {
        let file = self.vfs.held(path)?.clone();
        let rendered = self.rendering_of(&file.path)?;
        // Remembered, so the next `view` — which cannot build one — finds it.
        self.known_rendering_of(&file.path, &file.text);
        Some(FileView::parse_rendering_in(
            &file,
            &rendered,
            &self.config_for(&file.path),
        ))
    }

    /// **Whether a rendering for this text is in memory**, without consulting the disk — see
    /// [`Session::view_of_the_file`], which wants to know only whether to ask for one.
    pub(crate) fn a_rendering_is_known_in_memory(&self, path: &Path, text: &str) -> bool {
        let text_hash = cache::content_hash(text);
        if let Ok(open) = self.open_renderings.lock()
            && let Some((hash, _)) = open.get(&queue_key(path))
            && *hash == text_hash
        {
            return true;
        }
        self.renderings
            .lock()
            .ok()
            .and_then(|mut known| known.get(&a_rendering_key(path, text_hash)).map(|_| ()))
            .is_some()
    }

    /// The rendering for `text`, **if it is already read** — a hash and a lookup, and nothing else.
    pub(crate) fn known_rendering_of(
        &self,
        path: &Path,
        text: &str,
    ) -> Option<std::sync::Arc<preprocess::cooked::RenderedCooked>> {
        let text_hash = cache::content_hash(text);
        // **The open document's own rendering first, and it is not in the budget** — see
        // [`Session::open_renderings`]: the LRU is filled by a pass over the closure, and the file the reader is
        // editing is the one rendering that must survive it. Measured: after twenty seconds of indexing, `ids.`
        // answered 0 members because the rendering had been evicted.
        if let Ok(open) = self.open_renderings.lock()
            && let Some((hash, rendered)) = open.get(&queue_key(path))
            && *hash == text_hash
        {
            return Some(rendered.clone());
        }
        let key = a_rendering_key(path, text_hash);
        if let Some(found) = self.renderings.lock().ok()?.get(&key).cloned() {
            return Some(found);
        }

        // **The disk is the second table, and the one that does not evict.** A rendering is persisted with the cooked
        // reading — it is exactly what `cook` serves a warm start from — while the in-memory table holds
        // [`crate::cache::cache_lru::RENDERING_ENTRIES`] of them, which a pass over a project's closure fills at once.
        // So a hover that arrives after an index pass found its file's rendering **evicted** and answered as if the
        // file had never been rendered: measured on Lua 5.5.0's `ldebug.c`, hovering `p` in `ci_func(ci)->p` said "the
        // type of `ci_func(ci)` is not known here", because the mapping from the file's offsets into the rendering had
        // nothing to map with ([`Session::the_rendering_offset`] was `None`).
        //
        // Read back only on a miss, and the miss is the case where the alternative is no answer at all. A record is
        // validated against the text it is asked for (its key carries the content hash), so a stale row is a miss
        // rather than a wrong rendering, and a session with no database answers `None` exactly as before.
        let spelling = self
            .store
            .index()
            .summary(path)
            .map(|summary| summary.path.clone())
            .unwrap_or_else(|| path.to_path_buf());
        let (_, rendered) = self
            .store
            .the_stored_cooked_reading_with_rendering(&spelling)?;
        let rendered = std::sync::Arc::new(rendered);
        if let Ok(mut known) = self.renderings.lock() {
            let weight = rendered.weight();
            known.insert(key, std::sync::Arc::clone(&rendered), weight);
        }
        Some(rendered)
    }

    /// **Remember a rendering under the key [`Session::known_rendering_of`] reads** — the normalized spelling, not
    /// the caller's. A CLI hands over what `std::fs::canonicalize` returned (`\\?\C:\…`) while the store writes
    /// the index's spelling (`c:/…`), and the two never met: measured through `tools/inspect`, a rendering was in
    /// the cache (`8 entries`) and `known_rendering_of` answered `None` for it, so hover's rendering fallback could
    /// not ask anything.
    pub(crate) fn remember_a_rendering(
        &self,
        path: &Path,
        text_hash: u64,
        rendered: std::sync::Arc<preprocess::cooked::RenderedCooked>,
    ) {
        // **An open document is pinned outside the budget** — see [`Session::open_renderings`]: the LRU is filled by
        // a pass over the closure, and evicting the rendering of the file being edited is what made member
        // completion answer with nothing on a warm index.
        if self.documents.is_open(path)
            && let Ok(mut open) = self.open_renderings.lock()
        {
            open.insert(
                queue_key(path),
                (text_hash, std::sync::Arc::clone(&rendered)),
            );
        }
        let weight = rendered.weight();
        if let Ok(mut known) = self.renderings.lock() {
            known.insert(a_rendering_key(path, text_hash), rendered, weight);
        }
    }

    /// **The same view, with the macros this file's own includes define, built if it has to be.**
    ///
    /// [`Session::view`] reads a file's tokens and nothing else, and the note on [`FileView::parse`] says why: a
    /// view is a buffer and its index entry, and "nothing here has read the include graph". **A session has.** The
    /// closure is in its index and the text is in its VFS, so the bodies are a walk away — and without them a
    /// buffer is read as a reader of one file reads it, where
    ///
    /// ```cpp
    /// _STD_BEGIN                       // `namespace std {` to a compiler
    /// struct widget { … };
    /// _STD_END
    /// ```
    ///
    /// puts `widget` at **file scope** instead of in `std`. Every MSVC header opens its namespaces this way, so
    /// this is the difference between a declaration being reachable as `std::widget` and not being reachable at
    /// all — and it is the reading the index itself uses for these files ([`crate::index::FileIndexer`] is given
    /// the same environment through `with_macro_bodies`).
    ///
    /// # The eager half, and when to want it
    ///
    /// [`Session::view`] gets the same reading **one query later** without ever blocking on it. This is for a
    /// caller that would rather wait: a batch, a test, or a first query after a file is opened. The cost is the
    /// one below.
    ///
    /// # Cost, measured
    ///
    /// Building the environment walks the file's whole closure and takes a copy of every file's text
    /// ([`Session::closure_with_text`]): on a closure of 151 files that is **103.7 ms**, beside **163.2 µs** for a
    /// view without it. Warm, from [`Session::macro_environments`], it is **186.6 µs** — 1.14× — which is what
    /// makes [`Session::view`]'s arrangement worth having: the walk is paid once, by the work loop, and every query
    /// after it pays a hash and a lookup.
    /// # Removed, and why it is not coming back
    ///
    /// This returned a `FileView` built from the file's own tokens with only the closure's **macro bodies** supplied.
    /// It was the third reading, and it had **zero product call sites** while its documentation elsewhere described
    /// it as the second — `FileIndexer::macro_facts` claimed its parse used `ParserConfig::with_macros_from_includes`
    /// and never did. A reading whose own documentation is wrong and which nothing calls is not a fallback, it is a
    /// trap for the next caller: supplying `#define` bodies changes what `_STD_BEGIN` expands to without taking the
    /// `#if` branches, so its scope tree was a third answer to a question that already had two.
    ///
    /// Deleted rather than kept as a fourth `None`. A caller that wants meaning has [`Session::view`] and waits; a
    /// caller that wants a spelling has [`Session::tokens_of`] and does not need a tree at all.
    /// The environment for `text`, **if it is already read** — a hash and a lookup, and nothing else.
    pub(crate) fn known_macros_of(
        &self,
        path: &Path,
        text: &str,
    ) -> Option<std::sync::Arc<cpp_parser::MacroEnvironment>> {
        let key = (path.to_path_buf(), cache::content_hash(text));
        self.macro_environments.lock().ok()?.get(&key).cloned()
    }

    /// The macros `path`'s own includes define, as both readers of a body want them.
    ///
    /// The walk is [`crate::summary::macros_from_the_closure_with_bodies`] — the same one
    /// [`crate::SummaryStore`] uses when it has no unit timeline — fed from this session's own closure and
    /// overlay, so an unsaved buffer is what the macros come from. The result is remembered against `text`, and a
    /// second call with the same text is a lookup.
    pub(crate) fn the_macros_of(
        &self,
        path: &Path,
        text: &str,
    ) -> Option<std::sync::Arc<cpp_parser::MacroEnvironment>> {
        if let Some(known) = self.known_macros_of(path, text) {
            return Some(known);
        }

        let summary = self.store.index().summary(path)?;
        let (closure, by_key) = self.closure_with_text(path);
        let mut definitions = summary::MacroDefinitions::default();

        let evidence = summary::macros_from_the_closure_with_bodies(
            summary,
            |wanted| {
                let key = normalize_path(wanted, cfg!(windows));
                let (defined_in, text) = closure.get(*by_key.get(&key)?)?;
                Some((self.store.index().summary(defined_in)?, text.as_str()))
            },
            self.store.index().macros(),
            &mut definitions,
        );

        let made = std::sync::Arc::new(
            cpp_parser::MacroEnvironment::from_included_macros(evidence.macros)
                .with_bodies_in_force(evidence.conditional_bodies),
        );

        // **Bounded**: typing in one file changes its content hash on every keystroke, so an unbounded map here is
        // one environment per keystroke — and an environment of a header-heavy closure is thousands of names.
        let weight = cpp_parser::MacroFacts::len(made.as_ref());
        if let Ok(mut known) = self.macro_environments.lock() {
            known.insert(
                (path.to_path_buf(), cache::content_hash(text)),
                made.clone(),
                weight,
            );
        }
        Some(made)
    }
}
