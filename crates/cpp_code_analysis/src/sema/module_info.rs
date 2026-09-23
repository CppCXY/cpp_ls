//! C++20 modules: what a file *declares itself to be*, and what it imports.
//!
//! # Why this is a layer of its own
//!
//! A module is not an `#include`, and the difference is not a detail of style — it changes what a file's
//! analysis can be based on:
//!
//! * **`#include` is textual.** The includer's macros are in force inside the header, so a header's `#if`
//!   conditions depend on *who included it*. That is the whole reason [`crate::graph`] carries a macro
//!   environment along its edges and why one header can be analysed under two different configurations.
//! * **`import` is not.** A module unit is compiled on its own, with its own macro environment, and what it
//!   exports is a set of declarations. The importer's macros are irrelevant to the module and the module's
//!   macros are irrelevant to the importer — except for the one thing that does cross, the declarations.
//!
//! So a module graph must **not** reuse the include walk's macro chaining. Doing so would be worse than
//! useless: it would inherit an environment that no compiler ever used and then decide the module's own
//! conditions against it, producing confident answers that are simply wrong. This module reads the shape a
//! file declares, and [`crate::modules`] resolves the imports between files without pretending they are
//! textual.
//!
//! # The unit kinds
//!
//! The standard names the shapes (`[module.unit]`), and each is a different answer to "what is this file":
//!
//! ```text
//! module;                        <- global module fragment: #includes that belong to no module
//! #include <vector>
//! export module m;               <- primary module interface unit
//! export module m:part;          <- module partition *interface* unit
//! module m;                      <- module implementation unit
//! module m:part;                 <- module partition implementation unit
//! module : private;              <- private module fragment: importers cannot see past it
//! ```
//!
//! A partition is not a module of its own: `m:part` belongs to `m`, is imported as `import :part` from
//! inside `m`, and cannot be imported from outside it. Keeping the partition separate from the module name
//! is therefore not bookkeeping — it is the difference between resolving an import to the right file and
//! resolving it to a file that does not exist.
//!
//! # Extracting module unit kinds
//!
//! [`ModuleUnit::InterfaceUnit`] is the one unit that answers an `import m;` from outside, which is why
//! [`ModuleUnit::exports_to_importers`] is the predicate a resolver needs and `is_interface` is not enough
//! on its own: a partition interface unit is an interface *and* not importable by name.

use cpp_parser::{CppSyntaxKind, CppSyntaxNode, CppTokenKind};

use crate::token::tokens_of;

/// What a file declares itself to be.
///
/// `None` in [`ModuleInfo::unit`] means the file has no module declaration at all, which is not the same as
/// any of these: a plain translation unit is a legal and extremely common thing to be, and calling it a
/// "module implementation unit" because it has no `export` would be a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModuleUnit {
    /// `export module m;` — the unit an `import m;` names.
    InterfaceUnit,
    /// `module m;` — attaches to `m` without exporting anything.
    ImplementationUnit,
    /// `export module m:part;` — an interface unit for a partition of `m`.
    PartitionInterfaceUnit,
    /// `module m:part;` — an implementation unit for a partition of `m`.
    PartitionImplementationUnit,
}

impl ModuleUnit {
    /// Is this the unit an `import <module-name>;` from *another* module resolves to?
    ///
    /// Only the primary interface unit is. A partition interface unit is importable, but only as `:part`
    /// from inside its own module — never by the dotted name `m:part`, which is not a module name at all.
    pub fn exports_to_importers(self) -> bool {
        matches!(self, ModuleUnit::InterfaceUnit)
    }

    pub fn is_interface(self) -> bool {
        matches!(
            self,
            ModuleUnit::InterfaceUnit | ModuleUnit::PartitionInterfaceUnit
        )
    }

    pub fn is_partition(self) -> bool {
        matches!(
            self,
            ModuleUnit::PartitionInterfaceUnit | ModuleUnit::PartitionImplementationUnit
        )
    }

    /// A short name for messages, so a diagnostic can say "interface unit" rather than a variant name.
    pub fn describe(self) -> &'static str {
        match self {
            ModuleUnit::InterfaceUnit => "module interface unit",
            ModuleUnit::ImplementationUnit => "module implementation unit",
            ModuleUnit::PartitionInterfaceUnit => "module partition interface unit",
            ModuleUnit::PartitionImplementationUnit => "module partition implementation unit",
        }
    }
}

/// What an `import` declaration names.
///
/// The three forms are genuinely different things and cannot be one name-shaped string:
///
/// ```text
/// import std;          <- a module
/// import :part;        <- a partition of *this* module
/// import <vector>;     <- a header unit, synthesised from a header
/// import "local.h";    <- a header unit from a quoted header
/// ```
///
/// A partition spelled `:part` has no module name of its own — it belongs to the module the importing file
/// declares itself to be part of. Recording it as a module named `part` would resolve it to a module that
/// does not exist, so the partition case carries no module name and the resolver looks it up against the
/// importing file's own module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportTarget {
    /// `import a.b.c;` — the primary interface unit of that module.
    Module(Box<str>),
    /// `import :part;` — a partition of the module the *importing* file belongs to.
    Partition(Box<str>),
    /// `import <vector>;` or `import "local.h";` — a header unit.
    ///
    /// `is_angle` records which spelling was used, because it selects the same search order an `#include`
    /// would: the quoted form looks beside the importing file first, the angle form does not.
    HeaderUnit { name: Box<str>, is_angle: bool },
}

impl ImportTarget {
    /// A name for messages.
    pub fn describe(&self) -> String {
        match self {
            ImportTarget::Module(name) => format!("module `{name}`"),
            ImportTarget::Partition(name) => format!("partition `:{name}`"),
            ImportTarget::HeaderUnit { name, is_angle } => {
                if *is_angle {
                    format!("header unit `<{name}>`")
                } else {
                    format!("header unit `\"{name}\"`")
                }
            }
        }
    }

    /// The partition name, for the case where a partition is spelled *as part of* a module name.
    pub fn partition_name(&self) -> Option<&str> {
        match self {
            ImportTarget::Partition(name) => Some(name),
            _ => None,
        }
    }

    /// The module name, for the case where this target names a module.
    pub fn module_name(&self) -> Option<&str> {
        match self {
            ImportTarget::Module(name) => Some(name),
            _ => None,
        }
    }
}

/// One `import` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDeclaration {
    pub target: ImportTarget,
    /// `export import ...;` — a re-export, which makes the imported declarations part of *this* module's
    /// interface. A consumer building a module's export set needs this, and it is easy to lose: the
    /// `export` is a token inside the same node as the `import`.
    pub is_reexport: bool,
    /// Where the declaration is, so a diagnostic can point at it.
    pub range: cpp_parser::SourceRange,
}

/// What a file declares about modules.
///
/// Every field is optional because every part of a module unit is optional: a file may have no module
/// declaration, a module declaration may have no partition, and a module may import nothing. `None` is a
/// fact about the file, not a failure to read it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleInfo {
    /// The unit kind, or `None` when the file has no module declaration.
    pub unit: Option<ModuleUnit>,
    /// The module name, without any partition: `my.mod` for both `module my.mod;` and `module my.mod:part;`.
    pub module_name: Option<Box<str>>,
    /// The partition name, without the module and without the `:`: `part` for `module my.mod:part;`.
    pub partition_name: Option<Box<str>>,
    /// Does the file open a global module fragment (`module ;`)?
    ///
    /// Worth recording separately because it changes what the `#include`s above the module declaration
    /// mean: they belong to the *global* module, not to this one, and a consumer that ignored the fragment
    /// would attribute a header's declarations to the named module.
    pub has_global_fragment: bool,
    /// Does the file have a private module fragment (`module : private;`)?
    ///
    /// Declarations after it are not visible to importers. A consumer answering "is this name exported"
    /// has to know where the fragment starts.
    pub has_private_fragment: bool,
    /// The imports, in source order — including duplicates, because the order and the repetition are both
    /// facts about the file and a consumer can deduplicate if it wants to.
    pub imports: Vec<ImportDeclaration>,
}

impl ModuleInfo {
    /// Read a file's module shape from its syntax tree.
    pub fn from_tree(root: &CppSyntaxNode) -> Self {
        let mut info = ModuleInfo::default();

        for node in root.descendants() {
            match CppSyntaxKind::from(node.kind()) {
                CppSyntaxKind::GlobalModuleFragment => info.has_global_fragment = true,
                CppSyntaxKind::PrivateModuleFragment => info.has_private_fragment = true,
                CppSyntaxKind::ModuleDecl => {
                    // The *first* module declaration wins. A file may only have one, and a second one is
                    // either a mistake or a half-typed edit; taking the first keeps the answer stable while
                    // the user is typing rather than having it flip to whatever is further down.
                    if info.unit.is_none() {
                        let (name, partition) = read_module_parts(&node);
                        // Read the partition's presence *before* moving it into the info: the unit kind
                        // depends on whether there is one, and asking after the move is what a borrow
                        // checker catches here and a reader would not.
                        let has_partition = partition.is_some();

                        info.unit = Some(classify_unit(&node, has_partition));
                        info.module_name = name;
                        info.partition_name = partition;
                    }
                }
                CppSyntaxKind::ImportDecl => {
                    if let Some(declaration) = read_import(&node) {
                        info.imports.push(declaration);
                    }
                }
                _ => {}
            }
        }

        info
    }

    /// The full name a partition is known by inside its module: `m:part`.
    ///
    /// Not a module name — nothing outside the module can import a partition this way — but the form a
    /// consumer wants for matching a partition *implementation* unit to its interface, and for showing the
    /// user which partition a file is.
    pub fn qualified_name(&self) -> Option<String> {
        match (&self.module_name, &self.partition_name) {
            (Some(module), Some(partition)) => Some(format!("{module}:{partition}")),
            (Some(module), None) => Some(module.to_string()),
            _ => None,
        }
    }

    /// Does this file declare a module at all?
    pub fn is_module_unit(&self) -> bool {
        self.unit.is_some()
    }

    /// Is this file's macro environment its own — decided by what *it* says and nothing its importers say?
    ///
    /// The question a consumer has to ask before trusting any `#if` inside the file, and the one place where
    /// modules and headers genuinely part company. Inclusion is textual, so a header's conditions depend on
    /// the includer and [`crate::graph`] threads a macro environment along its edges; a module unit is
    /// compiled on its own, so nothing the importer defines is in force inside it and nothing it defines
    /// escapes to the importer.
    ///
    /// `true` for any module unit: that is what being one means. `false` for a plain translation unit, which
    /// is not a statement that such a file's macros are unreliable — a `.cpp` owns its environment too — but
    /// that there is no module unit here to make the claim about. A consumer deciding whether an import may
    /// carry macros asks this of the *imported* file, and only a module unit answers yes.
    ///
    /// # The global module fragment is the case that looks like an exception
    ///
    /// A module unit may open with `module;` followed by `#include`s, and those includes **are** textual —
    /// they belong to the global module, not to the named one. So a file can have both: textually-included
    /// headers, whose macros behave as they always do, and a named module whose macro environment stands
    /// alone. The flag that matters for *this* question is unchanged, because what crosses an `import` is
    /// still nothing; a consumer that needs to know where the textual part ends asks
    /// [`ModuleInfo::has_global_fragment`].
    pub fn macros_are_self_contained(&self) -> bool {
        self.is_module_unit()
    }

    /// The imports that are not re-exports — what this file needs in order to be built.
    pub fn plain_imports(&self) -> impl Iterator<Item = &ImportDeclaration> {
        self.imports.iter().filter(|import| !import.is_reexport)
    }

    /// The imports this file re-exports.
    pub fn reexports(&self) -> impl Iterator<Item = &ImportDeclaration> {
        self.imports.iter().filter(|import| import.is_reexport)
    }
}

/// Read the module name and partition from a `ModuleDecl` node.
///
/// The two are separate children with separate kinds, which is what makes this unambiguous: the node text
/// alone cannot be split reliably, because `module m:part;` and `module m;` differ by one child rather than
/// by a spelling.
fn read_module_parts(node: &CppSyntaxNode) -> (Option<Box<str>>, Option<Box<str>>) {
    let mut name = None;
    let mut partition = None;

    // `ModuleName` is also the kind of the partition's own name, so the partition's child must not be
    // mistaken for the module's. Walking the *direct* children keeps the two apart; a `descendants` walk
    // would find the partition's name first and report `part` as the module.
    for child in node.children() {
        match CppSyntaxKind::from(child.kind()) {
            CppSyntaxKind::ModuleName if name.is_none() => name = Some(read_dotted_name(&child)),
            CppSyntaxKind::ModulePartition => {
                partition = child
                    .children()
                    .find(|inner| CppSyntaxKind::from(inner.kind()) == CppSyntaxKind::ModuleName)
                    .map(|inner| read_dotted_name(&inner));
            }
            _ => {}
        }
    }

    (name, partition)
}

/// Turn a `ModuleName` node into its text, with the dots and without the whitespace.
///
/// Built from the identifier tokens rather than from the node's text so that `my . mod` — which is legal,
/// if odd — reads as `my.mod`, the name that has to match an `import my.mod;` elsewhere.
fn read_dotted_name(node: &CppSyntaxNode) -> Box<str> {
    let mut name = String::new();
    let mut first = true;

    for token in tokens_of(node) {
        match token.kind {
            CppTokenKind::Identifier => {
                name.push_str(&token.text);
                first = false;
            }
            CppTokenKind::Dot if !first => name.push('.'),
            // Trivia and anything else is skipped: a name is its identifiers.
            _ => {}
        }
    }

    name.into_boxed_str()
}

/// Decide the unit kind from the declaration's own shape.
///
/// `export` and the partition are the only two things that distinguish the four kinds, and both are read
/// from the node rather than from the file, so a file with a global fragment and a private fragment is
/// classified by its module declaration alone.
fn classify_unit(node: &CppSyntaxNode, has_partition: bool) -> ModuleUnit {
    let is_exported = tokens_of(node)
        .iter()
        .any(|token| token.kind == CppTokenKind::ExportKeyword);

    match (is_exported, has_partition) {
        (true, false) => ModuleUnit::InterfaceUnit,
        (false, false) => ModuleUnit::ImplementationUnit,
        (true, true) => ModuleUnit::PartitionInterfaceUnit,
        (false, true) => ModuleUnit::PartitionImplementationUnit,
    }
}

/// Read one `ImportDecl` node.
///
/// `None` when the declaration names nothing this layer can resolve, which happens on malformed or
/// half-typed input: `import ;` has no target, and a consumer asking "what does this file import" is better
/// served by nothing than by an invented name.
fn read_import(node: &CppSyntaxNode) -> Option<ImportDeclaration> {
    let is_reexport = tokens_of(node)
        .iter()
        .any(|token| token.kind == CppTokenKind::ExportKeyword);

    let mut target = None;

    for child in node.children() {
        match CppSyntaxKind::from(child.kind()) {
            CppSyntaxKind::HeaderName => {
                let text = child.text().to_string();
                // The delimiters are part of the node's text, so the spelling is read from them: `is_angle`
                // is not cosmetic, it selects the include search order the header unit is built from.
                let is_angle = text.trim_start().starts_with('<');
                let name = text
                    .trim()
                    .trim_start_matches(['<', '"'])
                    .trim_end_matches(['>', '"']);

                target = Some(ImportTarget::HeaderUnit {
                    name: name.into(),
                    is_angle,
                });
            }
            CppSyntaxKind::ModulePartition => {
                let name = child
                    .children()
                    .find(|inner| CppSyntaxKind::from(inner.kind()) == CppSyntaxKind::ModuleName)
                    .map(|inner| read_dotted_name(&inner))
                    .unwrap_or_default();

                if !name.is_empty() {
                    target = Some(ImportTarget::Partition(name));
                }
            }
            // A `ModuleName` child of the import node itself. The partition's name is nested inside the
            // partition child and so is not reached here — which is what keeps `import :part;` from being
            // read as `import part;`.
            CppSyntaxKind::ModuleName => {
                let name = read_dotted_name(&child);
                if !name.is_empty() {
                    target = Some(ImportTarget::Module(name));
                }
            }
            _ => {}
        }
    }

    Some(ImportDeclaration {
        target: target?,
        is_reexport,
        range: cpp_parser::source_range(node.text_range()),
    })
}
