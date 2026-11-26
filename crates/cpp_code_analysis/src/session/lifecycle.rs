//! **The session's own state** — construction, the configuration a file is read with, and the caches' sizes.
//!
//! Moved here from `session/mod.rs` without a change.

use super::*;
use crate::FileSummary;
use crate::MacroDefinitions;
use crate::ParsedDefinitions;
use crate::completion;
use crate::cuda_source_extensions;
use crate::effect;
use crate::file;
use crate::index;
use crate::project;

impl<F: FileProvider + Clone> Session<F> {
    /// A session with the configuration the caller already has: no compile database is read and no compiler is run.
    ///
    /// The path for a caller that knows how the project is built — a test, a build-system integration — and the
    /// only one that works over a provider that is not the disk.
    ///
    /// The project's **own** file is still read, because the two answer different questions and only one of them
    /// was given away here: `config` is what the compiler was told (the caller's, and it wins over `[compile]`),
    /// while `.cppls.toml` also says which files are the project's source and where its cache goes
    /// (`workspace.exclude`, `workspace.source_extensions`, `index.cache_dir`). A session that ignored those
    /// would analyse a different project from the one [`Session::open`] analyses.
    pub fn with_config(
        root: impl Into<PathBuf>,
        files: SessionFiles<F>,
        filter: WatchFilter,
        config: CompilerConfig,
    ) -> Session<F> {
        let root = root.into();
        // The project's own file is read here too, and by the same code: a session the caller configured still
        // analyses *this* project (`workspace.exclude`, `index.cache_dir`), and the build-system half is skipped
        // because the caller has said what the compilation is.
        let config_report = project::load_config(&files, &root, None);
        let filter = apply_project_config(filter, &config_report);

        let discovery = ProjectDiscovery {
            root: root.clone(),
            config: config_report,
            database: None,
            problems: Vec::new(),
        };

        Session::assemble(root, files, filter, config, None, discovery)
    }

    pub(crate) fn assemble(
        root: PathBuf,
        files: SessionFiles<F>,
        filter: WatchFilter,
        config: CompilerConfig,
        toolchain: Option<Toolchain>,
        discovery: ProjectDiscovery,
    ) -> Session<F> {
        let project_config = discovery.config.clone();
        let database = discovery
            .database
            .as_ref()
            .map(|database| &database.commands);
        // **The project's own statement of its standard, when the configuration file has none.**
        //
        // This is where the *seed* comes from: `compilation_environment` below builds the macro table every file's
        // cook starts from, and without a standard that table has no `__cplusplus` and no `_MSVC_LANG` — so MSVC's
        // headers, which put everything inside `#if _HAS_CXX20`, render as their fallback stub. Measured on a
        // `<ranges>` fixture whose only database entry says `/std:c++latest`: the cook of the header produced **12
        // bytes** (`#pragma once`) and zero declarations, and `std::views` did not exist — while the same header
        // cooked with the standard present was **1.8 MB and 22 345 declarations**. A project whose entries disagree
        // answers `None` (see `CompileCommands::a_unanimous_standard`), and the configuration file still wins.
        let mut config = config;
        if config.standard.is_none()
            && let Some(standard) = database.and_then(|commands| commands.a_unanimous_standard())
        {
            config.standard = Some(standard.into());
        }
        let documents = files.overlay.clone();
        // **A CUDA project's scan takes `.cu` and `.cuh`**, and a project that is not CUDA does not: the engine's own
        // list is what a C++ project is made of, and a `.cu` in one is either a vendored file or an accident. With
        // `[compile.cuda]` (or an `nvcc` toolchain) it is the project's own source, and asking every CUDA project to
        // write `workspace.source_extensions = ["cu", "cuh"]` in its configuration file would be asking it to restate
        // what the toolchain already said.
        let mut extra_extensions = project_config.config.workspace.source_extensions.clone();
        if config.cuda() {
            extra_extensions.extend(
                cuda_source_extensions()
                    .iter()
                    .map(|extension| (*extension).to_string()),
            );
        }
        // **A library's headers and module interfaces join the project's file list**, and its `.cpp` files never do —
        // a dependency is used through what it exports, and reading its implementation would multiply the index by
        // every dependency's sources for an answer nobody reading *this* project can act on. See `LibrarySection`.
        let from_libraries = library_files_to_read(&project_config.config.libraries, &root);
        let mut project = project_files(
            &files,
            &filter,
            &root,
            database,
            &extra_extensions,
            project_config
                .config
                .index
                .max_files
                .unwrap_or(MAX_PROJECT_FILES),
        );
        for file in from_libraries {
            if !project.iter().any(|held| held == &file) {
                project.push(file);
            }
        }

        // **What makes the macro environment complete: the compiler answered.**
        //
        // The flag is one claim — *a name none of the witnesses mentions is not defined* — and the witnesses are
        // the compiler's predefined names, the command line's `-D`s, and the file's own `#define`s. The second is
        // what `discovery` reads (`.cppls.toml`, `compile_commands.json`) and what
        // `toolchain::discover_with` asks the compiler with. The **first** is the compiler's own answer and exists
        // nowhere else: `_MSC_VER`, `__cplusplus` and five hundred more are built in, not written down. So the
        // claim is made whenever those macros came back, and the project's build description is a *source of
        // flags* rather than the licence for the claim.
        //
        // What it is not: a promise that every flag of the project's build is known. A build description nobody
        // can read — a `Makefile` with `-DFOO`, which no discovery in this crate parses — is a hole. The ordinary
        // such project has `-I` flags in the same file, and those are *self-reporting*: `Marked::mark_incomplete`
        // takes the claim back the moment an `#include` fails to resolve, and nothing is decided after that.
        //
        // **Measured on the workspace that asked for this** (`C:\work\one_file_project`: one `main.cpp`,
        // 2678 bytes, no build system of any kind). With the claim tied to a compile database — which is what this
        // line said before — MSVC's whole standard library was unreachable: `declarations_in("std")` was **13**,
        // every one of them from a C header that writes `namespace std` literally; `definition("std::string")`,
        // `std::cin`, `std::cout`, `std::basic_string` were all `NotDeclaredHere`; 46 of the file's identifiers had
        // no answer and 26 more were `ConditionalCompilation`; and the cooked reading of `<string>` rendered to
        // **nothing at all** (0 declarations, 0 mapped ranges). The cause is one name: `yvals_core.h` defines
        // `_STL_COMPILER_PREPROCESSOR` under `#if defined(RC_INVOKED) || defined(Q_MOC_RUN) || defined(__midl)`,
        // and MSVC's headers put *everything* inside `#if _STL_COMPILER_PREPROCESSOR` — so with `RC_INVOKED`
        // `Unknown`, nothing in the library is in force. With the claim made on the compiler's answer: **2600**
        // declarations in `std`, the three names above resolved, `<xstring>`'s 129 facts became **1631**, the
        // cooked reading of `<string>` 1000 declarations over 2000 ranges, and the identifiers with no answer fell
        // to **9**: three macro names (`stdout`, `stderr` ×2), one keyword (`static_cast`), one local, one member
        // reached through an object whose type is still unknown (`std::cin.read`), and `std::string::npos` ×3. The
        // cost is the cold read of that closure: 3.5 s → 16.5 s (warm 8.5 s), which is the price of reading four
        // megabytes of headers instead of whitespace.
        let configured = the_compilation_is_known(toolchain.as_ref());
        let mut store = SummaryStore::with_provider(root.clone(), config.clone(), files.clone())
            .with_macros(index::environment::compilation_environment(
                &config,
                toolchain.as_ref(),
                configured,
            ));

        if let Some(cache_dir) = &project_config.config.index.cache_dir {
            store = store.with_cache_directory(cache_dir);
        }

        // The search path, read once. Before the struct literal rather than inside it because it borrows `config`,
        // which the literal moves.
        let headers = completion::header_index(&config, &root, project.clone());

        let mut session = Session {
            root: root.clone(),
            identity: Arc::new(()),
            changes: 0,
            config,
            toolchain,
            discovery,
            vfs: Vfs::new(files.clone()),
            files,
            documents,
            store,
            filter,
            share_contributions: false,
            project_paths: project
                .iter()
                .map(|path| (queue_key(path), path.clone()))
                .collect(),
            project: project.clone(),
            queue: Work::default(),
            stamps: HashMap::new(),
            parsed_since_the_last_pass: Vec::new(),
            refined_on_request: std::sync::Mutex::new(std::collections::HashSet::new()),
            refinement_misses: std::sync::Mutex::new(std::collections::HashSet::new()),
            refined_readings: std::sync::Mutex::new(std::collections::HashSet::new()),
            cooking: Cooking::default(),
            definitions: std::sync::Mutex::new(MacroDefinitions::default()),
            units: std::sync::Mutex::new(UnitTable::default()),
            walked_by_content: std::sync::Mutex::new(cache::cache_lru::Cache::new(
                MAX_WALKS_BY_CONTENT_ENTRIES,
                MAX_WALKS_BY_CONTENT_BYTES,
            )),
            units_read: std::collections::HashSet::new(),
            programs_wanted: std::collections::HashSet::new(),
            units_attempted: std::collections::HashMap::new(),
            checks_memo: std::sync::Mutex::new(None),
            pull_hits: std::sync::atomic::AtomicU64::new(0),
            pull_runs: std::sync::atomic::AtomicU64::new(0),
            unit_index: std::sync::Mutex::new(None),
            rendered_unit: std::sync::Mutex::new(None),
            unit_roots: std::sync::Mutex::new(HashMap::new()),

            last_edit: std::time::Instant::now(),
            directive_signatures: std::collections::HashMap::new(),
            cook_persistence: std::sync::Mutex::new(std::collections::HashMap::new()),

            prefetched: HashMap::new(),
            prefetched_order: std::collections::VecDeque::new(),
            headers,
            macro_work: std::sync::Mutex::new(Cooking::default()),
            parsed_definitions: std::sync::Mutex::new(ParsedDefinitions::new()),
            configurations: std::sync::Mutex::new(HashMap::new()),
            config_for_file: std::sync::Mutex::new(HashMap::new()),
            macro_environments: std::sync::Mutex::new(cache::cache_lru::Cache::macro_environments()),
            surfaces: std::sync::Mutex::new(effect::Surfaces::new()),
            effects: std::sync::Mutex::new(HashMap::new()),
            renderings: std::sync::Mutex::new(cache::cache_lru::Cache::renderings()),
            open_renderings: std::sync::Mutex::new(HashMap::new()),
            feature_cache: cpp_toolchain::FeatureCache::default(),
            views: std::sync::Mutex::new(cache::cache_lru::Cache::views()),
            rendering_views: std::sync::Mutex::new(cache::cache_lru::Cache::rendering_views()),
            rendering_summaries: std::sync::Mutex::new(cache::cache_lru::Cache::new(
                RENDERING_SUMMARY_ENTRIES,
                RENDERING_SUMMARY_BYTES,
            )),
            cooking_refused: std::sync::Mutex::new(std::collections::HashSet::new()),
            cooked_or_wanted: std::sync::Mutex::new(std::collections::HashSet::new()),
            renderings_asked: std::sync::Mutex::new(std::collections::HashSet::new()),
            focused_document: std::sync::Mutex::new(None),
            client_says_c: std::sync::Mutex::new(std::collections::HashSet::new()),
            stored_cooks: std::sync::atomic::AtomicUsize::new(0),
            unit_from_memory: std::sync::atomic::AtomicUsize::new(0),
            unit_from_disk: std::sync::atomic::AtomicUsize::new(0),
            unit_walked: std::sync::atomic::AtomicUsize::new(0),
            walks_by_root: std::sync::Mutex::new(std::collections::HashMap::new()),
        };

        // The scan is the queue's **seed**, not a list to consult later: opening a project is the caller saying
        // "index this", and the order it is indexed in is the whole of what this type is for. Doing it here rather
        // than leaving it to the caller is what keeps `pending()` an honest answer from the first moment.
        for path in session.project.clone() {
            session.queue.add(path, Priority::Rest, 0);
        }

        session
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn revision(&self) -> SessionRevision {
        SessionRevision {
            session: self.identity.clone(),
            changes: self.changes,
            documents: self.documents.revision(),
        }
    }

    pub fn is_current(&self, revision: &SessionRevision) -> bool {
        self.owns_revision(revision)
            && self.changes == revision.changes
            && self.documents.revision() == revision.documents
    }

    pub fn owns_revision(&self, revision: &SessionRevision) -> bool {
        Arc::ptr_eq(&self.identity, &revision.session)
    }

    pub(crate) fn changed_input(&mut self) {
        self.changes = self
            .changes
            .checked_add(1)
            .expect("session revision overflow");
    }

    pub(crate) fn forget_refinement(&self, path: &Path) {
        self.refined_readings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&queue_key(path));
    }

    // The configuration every summary in this session is keyed against.

    /// **The configuration one file is analysed with** — its own command from the compilation database, plus its
    /// target's include directories and defines.
    ///
    /// [`Session::config`] cannot answer this and should not pretend to: a database has one entry per translation
    /// unit and a project with several targets has no single compilation, so the project-wide answer is *a*
    /// compilation rather than the project's. This is the question the include resolver asks, and the one a caller
    /// debugging "why did this file resolve that header" wants.
    pub fn config_for(&self, file: &std::path::Path) -> CompilerConfig {
        self.config().for_file(file)
    }

    /// **A file's configuration and its language, worked out once per session** — see [`Session::config_for_file`]
    /// for the field and the measurement.
    ///
    /// The same answers [`Session::config_for`] and [`CompilerConfig::language_for`] give. Both are memoized together
    /// because both were on the same path and each was the whole cost of a *cached* view: `for_file` clones the
    /// configuration and looks the file up in the database, and `language_for` looks it up **again** and re-parses
    /// `-std=` out of the command line — measured together at **132 µs per call**, which was 90 % of what a view-cache
    /// hit cost until this existed.
    ///
    /// Bounded by count, and the bound clears the whole memo: a reader meets a few dozen files, and evicting one entry
    /// at a time would buy nothing for the bookkeeping it costs.
    pub(crate) fn config_for_file(&self, file: &std::path::Path) -> std::sync::Arc<PerFileConfig> {
        if let Ok(known) = self.config_for_file.lock()
            && let Some(held) = known.get(file)
        {
            return std::sync::Arc::clone(held);
        }
        let config = std::sync::Arc::new(self.config().for_file(file));
        // **The client's language mode, when the project states none.** A header is the case this is for: its
        // family is deliberately nobody's guess (`for_file`), and "not told is not C" then lets C++-only rules fire
        // in a C project's `.h` files — 73 `cstyle-cast` findings on redis 8.x, all of them in headers
        // (`docs/c-support.md` §8). What the client said when it opened the document is a statement about the file,
        // and it is used only where the project said nothing.
        let mut language = config.language_for(file);
        // **The client's language mode, when the project has not stated this file's.** A header is the case this is
        // for: with no `-std=` of its own it inherits the project-wide default (`c++23` in the editor's settings),
        // so an unconfigured C project's `.h` files are read as C++ and "not told is not C" turns C++ rules on —
        // measured on redis 8.x: **73 `cstyle-cast` findings, every one in a `.h`** (`docs/c-support.md` §8).
        //
        // The database's own command is a fact and wins: a header it names has a stated language. The project-wide
        // default is not a fact about this file, and the editor's mode for the document the reader has open is.
        if !config.is_in_the_database(file)
            && self
                .client_says_c
                .lock()
                .is_ok_and(|said| said.contains(&queue_key(file)))
        {
            language = cpp_parser::features::LanguageConfig::c();
        }
        let worked_out = std::sync::Arc::new(PerFileConfig {
            language,
            cuda: config.cuda(),
            config,
        });
        if let Ok(mut known) = self.config_for_file.lock() {
            if known.len() >= CONFIG_FOR_FILE_ENTRIES {
                known.clear();
            }
            known.insert(file.to_path_buf(), std::sync::Arc::clone(&worked_out));
        }
        worked_out
    }

    pub fn config(&self) -> &CompilerConfig {
        &self.config
    }

    /// What the project's own configuration file said, and everything wrong with it.
    ///
    /// A report with no path is a project without a `.cppls.toml`, which is the ordinary case and not a problem.
    /// The sections a language server reads live here too — see [`Session::discovery`].
    pub fn project_config(&self) -> &ConfigReport {
        &self.discovery.config
    }

    /// **What this analysis thinks the project is** — its configuration file, its build, and
    /// everything that could not be worked out.
    ///
    /// The first question anybody asks about a wrong answer, and the reason the discovery is kept rather than
    /// consumed: "it read the wrong compilation" is diagnosable from here and from nowhere else. It is what
    /// `cpp_code_analysis`'s `discover` example prints, and what a server should log at startup.
    pub fn discovery(&self) -> &ProjectDiscovery {
        &self.discovery
    }

    pub fn toolchain(&self) -> Option<&Toolchain> {
        self.toolchain.as_ref()
    }

    pub fn compile_database(&self) -> Option<&CompileCommands> {
        self.discovery
            .database
            .as_ref()
            .map(|database| &database.commands)
    }

    pub fn documents(&self) -> &OpenDocuments {
        &self.documents
    }

    /// Every summary loaded so far — what the session currently knows, as opposed to what it will.
    pub fn index(&self) -> &ProjectIndex {
        self.store.index()
    }

    /// **What the file directs, without parsing it** — the reading a timeline walk needs, available *before* a file
    /// is indexed. See [`crate::index::FileIndexer::directive_summary`], and
    /// [`SummaryStore::directive_summary`](crate::SummaryStore::directive_summary) for why the store remembers it.
    pub fn directive_summary(&self, path: &Path) -> Option<FileSummary> {
        self.store
            .directive_summary(path)
            .map(|summary| (*summary).clone())
    }

    /// **Parse one file now, from its own text** — see [`SummaryStore::parsed_summary`]: the raw reading, whatever
    /// the index holds (after a cook it holds the *rendering's* reading).
    pub fn parsed_summary(&self, path: &Path) -> Option<FileSummary> {
        self.store.parsed_summary(path)
    }

    /// **What the session is holding** — the instrument for a report that says the server grew to gigabytes.
    ///
    /// Each field is a thing a cache *adds* to the process, and the numbers are the ones a report has to name before
    /// anything can be called a leak. Measured shape of the question this answers: a reader edited
    /// `EmmyLuaCodeStyle`'s `LuaSource.cpp` for a few minutes and the server sat at **3.3 GB** and 26% of a core.
    ///
    /// `rendering_bytes` is the text of every cached rendering, which is the cache with **no cap** — its key is
    /// `(path, content hash)`, so a keystroke adds an entry and nothing ever removes one. `summaries` and `facts`
    /// are the index, which is bounded by the project and is the part that is supposed to be large.
    /// **The summary database, when `CPPLS_CACHE_DB` turned it on** — rows and bytes.
    ///
    pub fn summary_database_sizes(&self) -> Option<cache::cache_db::DatabaseSizes> {
        self.store.summary_database_sizes()
    }

    /// **Collect and compact the summary database** — the idle step, never a query.
    pub fn collect_the_cache(&mut self) -> Option<cache::cache_db::Collected> {
        self.store.collect_the_cache()
    }

    /// Commit whatever summaries the database is holding — the end of a slice.
    pub fn flush_summaries(&self) {
        self.store.flush_summaries();
    }

    pub fn cache_sizes(&self) -> CacheSizes {
        let (renderings, rendering_bytes) = self
            .renderings
            .lock()
            .map(|known| {
                let bytes = known
                    .values()
                    .map(|rendering| rendering.text.len())
                    .sum::<usize>();
                (known.len(), bytes)
            })
            .unwrap_or((0, 0));

        let summaries = self.store.index().summaries().count();
        let facts = self
            .store
            .index()
            .summaries()
            .map(|summary| summary.declarations.len())
            .sum();
        let macros = self
            .store
            .index()
            .summaries()
            .map(|summary| summary.macros.len())
            .sum();

        let (cooked_files, cooked_declarations) = self.store.index().cooked_size();
        // **Both populations**, and the split is why the number is read from two caches: a rendering view is a
        // whole unit's tree and a buffer view is a page, and a report that added only one of them would
        // understate what a reader with a big file open is paying for.
        let counted = |cache: &std::sync::Mutex<
            cache::cache_lru::Cache<ViewKey, std::sync::Arc<file::view::ParsedView>>,
        >| {
            cache
                .lock()
                .map(|known| (known.len(), known.weight()))
                .unwrap_or((0, 0))
        };
        let (buffer_views, buffer_view_bytes) = counted(&self.views);
        let (rendering_views, rendering_view_bytes) = counted(&self.rendering_views);
        let (views, view_bytes) = (
            buffer_views + rendering_views,
            buffer_view_bytes + rendering_view_bytes,
        );
        CacheSizes {
            renderings,
            rendering_bytes,
            units: self.held_units().held.len(),
            summaries,
            facts,
            macros,
            cooked_files,
            cooked_declarations,
            views,
            view_bytes,
        }
    }
}
