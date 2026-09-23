//! The module graph: which file declares which module, and which files import which modules.
//!
//! # Why this is not the include graph
//!
//! [`crate::graph`] walks `#include`s and carries a **macro environment** along its edges, because inclusion
//! is textual: the includer's `#define`s are in force inside the header. Modules do not work that way. A
//! module unit is compiled on its own, so the importer's macros have no effect on it and its macros have no
//! effect on the importer.
//!
//! That difference is not a refinement of the include walk — it is a reason not to reuse it. An import edge
//! that inherited the importer's macro environment would decide the imported module's `#if` conditions
//! against macros no compiler ever used, and every answer that followed would be confidently wrong. So the
//! module graph resolves names to files and records dependencies, and it deliberately **does not carry a
//! macro environment at all**: there is no field for one, which is the cheapest way to make sure nobody adds
//! one by accident.
//!
//! # What it does carry
//!
//! Two things, and both are needed to answer "what does this edit affect":
//!
//! * **Units** — the files that declare a module, indexed by module name so an `import m;` can be resolved.
//!   A module has exactly one primary interface unit; partitions add more units to the same module.
//! * **Edges** — one per import declaration, in source order, carrying the resolution outcome. Forward edges
//!   answer "what does this file need"; the reverse answer is [`ModuleGraph::importers_of`], which is what a
//!   cache invalidation calls when a module interface changes.
//!
//! # Resolution, and why it is honest about failing
//!
//! Resolving `import m;` means finding the file that declares `m` — and that file's *name* is not part of
//! the language. Build systems conventionally name the interface unit after the module (`m.cppm`, `m.ixx`),
//! but nothing enforces it, so a resolver that trusted the convention would silently miss modules and one
//! that demanded a scan would be too expensive to run per keystroke.
//!
//! Both are tried, in that order, and the outcome records which one worked. When neither does, the answer is
//! [`ModuleResolution::Unresolved`] and never a guess: an unresolved import means "this analysis does not
//! know what that module contains", which is a state a consumer renders differently from "the module is
//! empty". See [`ImportOutcome`] for the one case where the distinction is subtler than it looks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cpp_parser::CppSyntaxTree;

use crate::{
    config::CompilerConfig,
    directive::IncludeForm,
    include::{IncludeResolver, Resolution},
    module_info::{ImportTarget, ModuleInfo, ModuleUnit},
    paths::{FileId, FileProvider, PathInterner, join_normalized},
};

/// Extensions a module interface unit is commonly given.
///
/// Used only to *propose* candidates for the cheap path; a candidate is confirmed by parsing it, so a wrong
/// guess here costs a failed lookup and never a wrong answer.
pub const INTERFACE_UNIT_EXTENSIONS: &[&str] = &["cppm", "ixx", "ccm", "cxxm", "cpp", "cc", "cxx"];

/// One module unit: a file that declares a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleUnitEntry {
    pub file: FileId,
    pub path: PathBuf,
    /// The module it belongs to, without the partition.
    pub module_name: Box<str>,
    /// Its partition, if it has one.
    pub partition_name: Option<Box<str>>,
    pub unit: ModuleUnit,
}

impl ModuleUnitEntry {
    /// The name this unit is known by inside its module: `m` or `m:part`.
    pub fn qualified_name(&self) -> String {
        match &self.partition_name {
            Some(partition) => format!("{}:{}", self.module_name, partition),
            None => self.module_name.to_string(),
        }
    }
}

/// How an import resolved.
///
/// The variants are the states a consumer has to render differently, and the reason this is an enum rather
/// than an `Option<FileId>`: "I found the file" and "I cannot find the file" are different, and so are the
/// three flavours of not finding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// The declaring file was found.
    Resolved(FileId),
    /// Nothing in the project declares this module.
    ///
    /// The honest reading is "unknown", not "empty": the module may be provided by a prebuilt BMI the
    /// analysis has no source for, which is the normal state of affairs for `import std;` and for any
    /// library shipped as a module. A consumer must not conclude that the imported names do not exist.
    UnknownModule { name: Box<str> },
    /// The module is known but the named partition is not.
    ///
    /// Worth separating from [`ImportOutcome::UnknownModule`] because the module *is* here: the partition
    /// name is either misspelled or the partition's file is missing from the project, and the two lead to
    /// different fixes.
    UnknownPartition {
        module: Box<str>,
        partition: Box<str>,
    },
    /// A header unit whose header could not be found.
    ///
    /// Carries what was searched, so the message can say where it looked — the same reason
    /// [`crate::include::Unresolved`] keeps its candidates.
    UnknownHeaderUnit {
        name: Box<str>,
        searched: Vec<PathBuf>,
    },
    /// The name resolves to a file that does not declare it.
    ///
    /// The case that catches a real mistake: `import m;` finds `m.cppm` by the naming convention, and
    /// `m.cppm` turns out to declare `other`. Reporting a resolved edge here would attribute `other`'s
    /// declarations to `m` — so the mismatch is reported instead, and the file that was tried is kept so the
    /// message can name it.
    NotDeclaredHere { name: Box<str>, tried: PathBuf },
    /// A partition import from a file that is not part of the module.
    ///
    /// `import :part;` means "a partition of *my* module", so it can only appear in a file that declares a
    /// module — or in a partition of one. In a plain translation unit there is no module for it to belong to,
    /// and the import cannot be resolved at all.
    NoModuleToPartition,
    /// A partition import that named a partition which is not declared as such.
    ///
    /// Distinct from [`ImportOutcome::UnknownPartition`]: the file was found, and it is a unit of the right
    /// module, but it is not the partition the import asked for.
    PartitionMismatch {
        module: Box<str>,
        partition: Box<str>,
        tried: PathBuf,
    },
}

impl ImportOutcome {
    /// The file an import resolved to, if it did.
    pub fn file(&self) -> Option<FileId> {
        match self {
            ImportOutcome::Resolved(file) => Some(*file),
            _ => None,
        }
    }

    /// Did this resolve?
    pub fn is_resolved(&self) -> bool {
        matches!(self, ImportOutcome::Resolved(_))
    }

    /// Could names from this import be visible to the importer?
    ///
    /// `true` only for a resolved import. Everything else is *unknown* rather than absent, which is the
    /// distinction the whole enum exists for — an unresolved module may still have been compiled into a BMI
    /// the analysis cannot see, so hiding its names would be a guess and reporting them as missing would be
    /// a false positive.
    pub fn may_provide_names(&self) -> bool {
        self.is_resolved()
    }

    /// A sentence a diagnostic can show.
    ///
    /// Each phrasing says what was actually *established*, not merely that something failed. The header-unit
    /// case makes the point: "looked in 0 places" is the honest answer for an angle import in a project with
    /// no configured include paths, and it tells the user the fix is a build configuration rather than a
    /// missing file — which "cannot find vector" would not.
    pub fn describe(&self) -> String {
        match self {
            ImportOutcome::Resolved(_) => "resolved".to_string(),
            ImportOutcome::UnknownModule { name } => {
                format!("no file in this project declares module `{name}`")
            }
            ImportOutcome::UnknownPartition { module, partition } => {
                format!("module `{module}` declares no partition `:{partition}`")
            }
            ImportOutcome::UnknownHeaderUnit { name, searched } if searched.is_empty() => format!(
                "cannot build a header unit for `{name}`: no include directories are configured to search"
            ),
            ImportOutcome::UnknownHeaderUnit { name, searched } => format!(
                "cannot find a header for unit `{name}` (looked in {} place(s))",
                searched.len()
            ),
            ImportOutcome::NotDeclaredHere { name, tried } => format!(
                "`{}` was tried for module `{name}` but does not declare it",
                tried.display()
            ),
            ImportOutcome::NoModuleToPartition => {
                "`import :partition;` in a file that declares no module".to_string()
            }
            ImportOutcome::PartitionMismatch {
                module,
                partition,
                tried,
            } => format!(
                "`{}` is a unit of `{module}` but is not partition `:{partition}`",
                tried.display()
            ),
        }
    }
}

/// One import declaration and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportEdge {
    pub from: FileId,
    /// The module this import was written in, when the importing file declares one.
    ///
    /// The importer's own module, and the reason a partition import can be resolved at all: `import :part;`
    /// is looked up against this, not against the file system.
    pub from_module: Option<Box<str>>,
    pub target: ImportTarget,
    pub is_reexport: bool,
    pub outcome: ImportOutcome,
    pub range: cpp_parser::SourceRange,
}

/// Every module unit reached, and every import between them.
#[derive(Debug, Clone, Default)]
pub struct ModuleGraph {
    /// The units, in the order they were read.
    pub units: Vec<ModuleUnitEntry>,
    /// The imports, in the order they were read.
    pub edges: Vec<ImportEdge>,
    /// Every file that was read, in the order it was entered.
    pub analysed: Vec<FileId>,
}

impl ModuleGraph {
    /// The unit that declares `module`, if the project has one.
    ///
    /// Only a primary interface unit answers this: a partition interface unit is an interface, but nothing
    /// outside its module may import it by name, so returning one here would resolve an `import m;` to a file
    /// that does not export what the importer is asking for.
    pub fn interface_unit(&self, module: &str) -> Option<&ModuleUnitEntry> {
        self.units
            .iter()
            .find(|unit| &*unit.module_name == module && unit.unit.exports_to_importers())
    }

    /// The unit for a partition, if the project has one.
    pub fn partition_unit(&self, module: &str, partition: &str) -> Option<&ModuleUnitEntry> {
        self.units.iter().find(|unit| {
            &*unit.module_name == module
                && unit.partition_name.as_deref() == Some(partition)
                // An implementation unit for a partition does not *export* it. Which one is wanted depends
                // on the question, so the interface wins when both exist — that is the one a name from the
                // partition is visible through.
                && unit.unit.is_interface()
        })
    }

    /// Every unit of a module, including its partitions', in the order they were read.
    pub fn units_of(&self, module: &str) -> Vec<&ModuleUnitEntry> {
        self.units
            .iter()
            .filter(|unit| &*unit.module_name == module)
            .collect()
    }

    /// The imports a file makes, in source order.
    ///
    /// An iterator rather than a `Vec` because the caller almost always just walks it, and a `Vec` would make
    /// every read site allocate — including the ones that only want the first edge.
    pub fn imports_of(&self, file: FileId) -> impl Iterator<Item = &ImportEdge> {
        self.edges.iter().filter(move |edge| edge.from == file)
    }

    /// The resolved files a file imports — what it needs in order to be built.
    pub fn needs_of(&self, file: FileId) -> Vec<FileId> {
        self.imports_of(file)
            .filter_map(|edge| edge.outcome.file())
            .collect()
    }

    /// The files that import `file` **directly**, through a resolved import.
    ///
    /// The reverse edge, and the point of the module graph: a change to a module interface unit invalidates
    /// exactly these, and transitively whatever imports them. A consumer that had to re-scan the project to
    /// find them would be doing per-keystroke work proportional to the project.
    pub fn importers_of(&self, file: FileId) -> Vec<FileId> {
        let mut importers: Vec<FileId> = self
            .edges
            .iter()
            .filter(|edge| edge.outcome.file() == Some(file))
            .map(|edge| edge.from)
            .collect();

        importers.sort_unstable_by_key(|file| file.index());
        importers.dedup();
        importers
    }

    /// Every file that depends on `file`, directly or through other imports — what a cache invalidation
    /// has to discard.
    pub fn dependents_of(&self, file: FileId) -> Vec<FileId> {
        let mut out = Vec::new();
        let mut seen: std::collections::HashSet<FileId> = std::collections::HashSet::new();
        let mut stack = vec![file];

        while let Some(current) = stack.pop() {
            for importer in self.importers_of(current) {
                if seen.insert(importer) {
                    out.push(importer);
                    stack.push(importer);
                }
            }
        }

        out
    }

    /// The imports that did not resolve, with what was tried.
    pub fn unresolved_imports(&self) -> Vec<&ImportEdge> {
        self.edges
            .iter()
            .filter(|edge| !edge.outcome.is_resolved())
            .collect()
    }

    /// The entry for a file, if it was read.
    pub fn unit_of(&self, file: FileId) -> Option<&ModuleUnitEntry> {
        self.units.iter().find(|unit| unit.file == file)
    }
}

/// Reads files and resolves their imports.
///
/// Holds no macro environment, by design — see the module documentation. The interner is passed to each
/// resolution rather than held, because minting a `FileId` mutates it and a scanner that borrowed it for its
/// whole life would make the caller's other uses of the same table impossible.
pub struct ModuleScanner<'a, F: FileProvider> {
    files: &'a F,
    config: &'a CompilerConfig,
    /// Module name → the files that declare it, from the last scan.
    ///
    /// Built once per scan rather than per lookup: resolution asks the same question for every import in the
    /// project, and re-parsing every candidate file per import is the difference between one pass and
    /// quadratic work.
    index: HashMap<Box<str>, Vec<ModuleUnitEntry>>,
    /// The units read so far, in the order read — what becomes [`ModuleGraph::units`].
    scanned_units: Vec<ModuleUnitEntry>,
}

impl<'a, F: FileProvider> ModuleScanner<'a, F> {
    pub fn new(files: &'a F, config: &'a CompilerConfig) -> Self {
        ModuleScanner {
            files,
            config,
            index: HashMap::new(),
            scanned_units: Vec::new(),
        }
    }

    /// Read a file and add its unit to the index, returning what it declares.
    ///
    /// Parsing the file is the caller's business: it usually has the tree already, and this layer exists to
    /// answer questions about the tree, not to be a second parser.
    pub fn add_file(&mut self, file: FileId, path: &Path, tree: &CppSyntaxTree) -> ModuleInfo {
        let info = ModuleInfo::from_tree(&tree.get_red_root());

        if let (Some(unit), Some(module_name)) = (info.unit, info.module_name.clone()) {
            let entry = ModuleUnitEntry {
                file,
                path: path.to_path_buf(),
                module_name: module_name.clone(),
                partition_name: info.partition_name.clone(),
                unit,
            };

            self.index
                .entry(module_name)
                .or_default()
                .push(entry.clone());
            // A file read twice is one unit. The same path reached from two importers is the same file, and
            // a duplicate would make `units_of` report a module with two interface units — which is a
            // contradiction a consumer would have to defend against.
            if !self.scanned_units.iter().any(|unit| unit.file == file) {
                self.scanned_units.push(entry);
            }
        }

        info
    }

    /// Every unit read so far, in the order read.
    pub fn units(&self) -> &[ModuleUnitEntry] {
        &self.scanned_units
    }

    /// Resolve a module name to its primary interface unit.
    ///
    /// The two-step order, and why each step is there:
    ///
    /// 1. **The index** — the units already read, which is exact. A module name that appears here is
    ///    resolved with no I/O at all.
    /// 2. **The naming convention** — `m` → `m.cppm` and friends beside the importing file and along the
    ///    include paths. Only ever a *proposal*: the candidate is read and must declare `m`, so a file that
    ///    merely has the right name is rejected rather than accepted.
    ///
    /// Nothing else is tried. A full project scan would find modules with unconventional names, and it is
    /// the wrong trade for a per-keystroke analysis: the cost is proportional to the project on every lookup,
    /// and the case it fixes is one a build system has already solved by knowing where its own sources are.
    pub fn resolve_module(&mut self, name: &str, interner: &mut PathInterner) -> ImportOutcome {
        if let Some(entry) = self
            .index
            .get(name)
            .and_then(|units| units.iter().find(|unit| unit.unit.exports_to_importers()))
        {
            return ImportOutcome::Resolved(entry.file);
        }

        for candidate in self.candidate_paths(name) {
            let Some(source) = self.files.read(&candidate) else {
                continue;
            };

            let tree = cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default());
            let info = ModuleInfo::from_tree(&tree.get_red_root());

            if info.module_name.as_deref() != Some(name) {
                // The file exists and is not the module. Reported rather than skipped: this is the one
                // failure that indicates a real mistake — a renamed module, a copy-paste, a file in the
                // wrong directory — and silently moving on would leave the user with "module not found"
                // about a module that is right there.
                return ImportOutcome::NotDeclaredHere {
                    name: name.into(),
                    tried: candidate,
                };
            }

            if info.unit != Some(ModuleUnit::InterfaceUnit) {
                // It declares the module but is not the unit importers see.
                return ImportOutcome::NotDeclaredHere {
                    name: name.into(),
                    tried: candidate,
                };
            }

            let file = interner.intern(&candidate);
            self.add_file(file, &candidate, &tree);

            return ImportOutcome::Resolved(file);
        }

        ImportOutcome::UnknownModule { name: name.into() }
    }

    /// Resolve `import :partition;` against the importing file's own module.
    pub fn resolve_partition(
        &mut self,
        module: Option<&str>,
        partition: &str,
        interner: &mut PathInterner,
    ) -> ImportOutcome {
        let Some(module) = module else {
            return ImportOutcome::NoModuleToPartition;
        };

        if let Some(entry) = self.index.get(module).and_then(|units| {
            units.iter().find(|unit| {
                unit.partition_name.as_deref() == Some(partition) && unit.unit.is_interface()
            })
        }) {
            return ImportOutcome::Resolved(entry.file);
        }

        // The partition's file may exist and not have been read yet: `m:part` is named after the module, so
        // the same convention applies with the partition appended.
        let qualified = format!("{module}:{partition}");
        for candidate in self.candidate_paths(&qualified) {
            let Some(source) = self.files.read(&candidate) else {
                continue;
            };

            let tree = cpp_parser::CppParser::parse(&source, cpp_parser::ParserConfig::default());
            let info = ModuleInfo::from_tree(&tree.get_red_root());

            let matches = info.module_name.as_deref() == Some(module)
                && info.partition_name.as_deref() == Some(partition);

            if !matches {
                return ImportOutcome::PartitionMismatch {
                    module: module.into(),
                    partition: partition.into(),
                    tried: candidate,
                };
            }

            let file = interner.intern(&candidate);
            self.add_file(file, &candidate, &tree);

            return ImportOutcome::Resolved(file);
        }

        ImportOutcome::UnknownPartition {
            module: module.into(),
            partition: partition.into(),
        }
    }

    /// Resolve a header unit through the include search order.
    ///
    /// A header unit *is* built from a header, by the same search the preprocessor would use, so this
    /// delegates rather than reimplementing it — a second implementation of "which `vector` did you mean"
    /// is free to disagree with the first, and then the analysis and the compiler disagree.
    pub fn resolve_header_unit(
        &mut self,
        name: &str,
        is_angle: bool,
        including: &Path,
        interner: &mut PathInterner,
    ) -> ImportOutcome {
        let include = crate::directive::Include {
            target: name.into(),
            form: if is_angle {
                IncludeForm::Angle
            } else {
                IncludeForm::Quote
            },
            is_next: false,
        };

        let resolver = IncludeResolver::new(self.files, self.config);
        let outcome = resolver.resolve(&include, including, None, interner);

        match outcome {
            Resolution::Resolved(resolved) => ImportOutcome::Resolved(resolved.file),
            Resolution::Unresolved(unresolved) => ImportOutcome::UnknownHeaderUnit {
                name: name.into(),
                searched: unresolved.searched,
            },
        }
    }

    /// Candidate files for a module name, in the order they are worth trying.
    ///
    /// A module name maps to a file name by convention, and there is more than one convention in use, so
    /// several spellings are tried. Each is only a *proposal*: a candidate is read and must declare the
    /// module before it is accepted, so a wrong guess costs one failed read and never a wrong answer.
    ///
    /// The partition separator is where the conventions genuinely differ, and where a single rule would fail
    /// every real project:
    ///
    /// ```text
    /// m:part   ->  m-part.cppm     the common spelling; a colon is not portable in a file name
    ///          ->  m.part.cppm     used where the module name is kept verbatim
    ///          ->  m/part.cppm     used where partitions live in a directory of their own
    /// my.mod   ->  my/mod.cppm     dots as directories
    ///          ->  my.mod.cppm     kept flat
    /// ```
    fn candidate_paths(&self, name: &str) -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        let case_insensitive = self.files.is_case_insensitive();

        // `:` and `.` are both separators between a module and its partition, so both are expanded; the
        // difference between them is only which spelling of the *name* they came from.
        let dotted = name.replace(':', ".");
        let stems = [
            name.replace(':', "-").replace('.', "/"),
            name.replace(':', "-"),
            dotted.replace('.', "/"),
            dotted.clone(),
        ];

        for directory in self.search_directories() {
            for stem in &stems {
                for extension in INTERFACE_UNIT_EXTENSIONS {
                    let candidate = join_normalized(
                        &directory,
                        Path::new(&format!("{stem}.{extension}")),
                        case_insensitive,
                    );
                    if !candidates.contains(&candidate) {
                        candidates.push(candidate);
                    }
                }
            }
        }

        candidates
    }

    /// The directories a module named after its file would be found in.
    ///
    /// The empty first entry is the importing file's directory once resolution joins against it, which is
    /// where a project's own modules live. Putting it first matches the quoted form of `#include`, and for
    /// modules there is no angle form to distinguish from — a module name is never written in brackets.
    fn search_directories(&self) -> Vec<PathBuf> {
        let mut directories = vec![PathBuf::new()];
        directories.extend(self.config.user_include_paths().map(Path::to_path_buf));
        directories.extend(self.config.system_include_paths().map(Path::to_path_buf));
        directories
    }
}

/// How deep imports are followed before the scan gives up.
///
/// The module analogue of [`crate::graph::MAX_INCLUDE_DEPTH`], and there for the same reason: `import`
/// cycles are possible (two modules may import each other's partitions, and a partition may import its own
/// module), so a walk that trusted the graph would not terminate. The budget also bounds the work a single
/// keystroke can trigger.
pub const MAX_IMPORT_DEPTH: usize = 64;

/// Read a translation unit and everything it imports, building a module graph.
///
/// `tree` is the already-parsed root, handed over rather than re-parsed — the caller usually has it, and
/// parsing twice is the most expensive thing this could do. The file's *text* is not needed: every question
/// this layer asks is a question about the tree, and the two files it does need to read (a candidate
/// interface unit, a header unit) it reads itself.
///
/// # The root is taken as given; everything else is a candidate
///
/// Each import is *resolved*, which may read a candidate file to confirm it declares what its name claims,
/// and a resolved module is scanned in turn. The result records both the units found and every import with
/// its outcome, so a consumer can distinguish "this file imports nothing" from "this file imports things
/// this analysis could not find" — see [`ImportOutcome`].
///
/// # No macro environment is threaded
///
/// Deliberately, and the contrast with [`crate::graph::walk`] is the point of this module. An import does not
/// make the importer's macros visible to the imported module, so there is nothing to inherit and nothing to
/// propagate back. A consumer that needs a file's macros asks [`crate::graph`] about its `#include`s.
pub fn scan_imports<F: FileProvider>(
    tree: &CppSyntaxTree,
    root_path: &Path,
    files: &F,
    config: &CompilerConfig,
    interner: &mut PathInterner,
) -> ModuleGraph {
    let mut graph = ModuleGraph::default();
    let mut state = ScanState::default();
    let mut scanner = ModuleScanner::new(files, config);

    let root = interner.intern(root_path);
    graph.analysed.push(root);

    // The root's own unit is read first, so that a partition importing its own module resolves from the
    // index instead of hunting for a file that is the very one being read.
    let root_info = scanner.add_file(root, root_path, tree);

    follow_imports(
        &root_info,
        root,
        root_path,
        &mut scanner,
        &mut graph,
        &mut state,
        interner,
        0,
    );

    graph.units = scanner.units().to_vec();
    graph
}

/// Where the scan has been.
#[derive(Debug, Default)]
struct ScanState {
    /// Files whose imports have been followed, so a diamond does not re-read them.
    visited: std::collections::HashSet<FileId>,
    /// The chain currently being followed, for cycle detection.
    ///
    /// Separate from `visited` because the two answer different questions: a file already *visited* is a
    /// diamond and may be skipped safely, while a file on the *path* is a cycle and a compiler would reject
    /// it — a distinction worth keeping so a consumer can report the cycle rather than silently accepting it.
    on_path: Vec<FileId>,
}

/// Read one file's imports and follow the ones that resolved.
#[allow(clippy::too_many_arguments)]
fn follow_imports<F: FileProvider>(
    info: &ModuleInfo,
    file: FileId,
    path: &Path,
    scanner: &mut ModuleScanner<'_, F>,
    graph: &mut ModuleGraph,
    state: &mut ScanState,
    interner: &mut PathInterner,
    depth: usize,
) {
    state.visited.insert(file);
    state.on_path.push(file);

    let including = crate::paths::parent_normalized(path, scanner.files.is_case_insensitive());

    for declaration in &info.imports {
        let outcome = match &declaration.target {
            ImportTarget::Module(name) => scanner.resolve_module(name, interner),
            ImportTarget::Partition(partition) => {
                scanner.resolve_partition(info.module_name.as_deref(), partition, interner)
            }
            ImportTarget::HeaderUnit { name, is_angle } => {
                scanner.resolve_header_unit(name, *is_angle, &including, interner)
            }
        };

        graph.edges.push(ImportEdge {
            from: file,
            from_module: info.module_name.clone(),
            target: declaration.target.clone(),
            is_reexport: declaration.is_reexport,
            outcome: outcome.clone(),
            range: declaration.range,
        });

        let Some(target) = outcome.file() else {
            continue;
        };

        // A cycle, or a budget exhausted. The edge above is kept either way — the module *was* found, and
        // hiding the edge would lose the fact that the dependency exists. What stops is the recursion.
        if state.on_path.contains(&target) || depth + 1 > MAX_IMPORT_DEPTH {
            continue;
        }

        // A diamond, not a cycle: the file is in the graph once and its own imports were recorded the first
        // time. Re-reading it would duplicate every edge below it.
        if state.visited.contains(&target) {
            continue;
        }

        let Some(target_path) = interner.path(target).map(Path::to_path_buf) else {
            continue;
        };

        let Some(target_source) = scanner.files.read(&target_path) else {
            continue;
        };

        let target_tree =
            cpp_parser::CppParser::parse(&target_source, cpp_parser::ParserConfig::default());
        let target_info = scanner.add_file(target, &target_path, &target_tree);

        if !graph.analysed.contains(&target) {
            graph.analysed.push(target);
        }

        follow_imports(
            &target_info,
            target,
            &target_path,
            scanner,
            graph,
            state,
            interner,
            depth + 1,
        );
    }

    state.on_path.pop();
}
