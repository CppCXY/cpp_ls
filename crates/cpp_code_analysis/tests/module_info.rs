//! Reading a file's module shape out of its syntax tree.
//!
//! The tests are organised around the question each unit kind answers, because the kinds are easy to
//! conflate and the differences are the whole point: a **partition interface unit** is an interface unit and
//! still cannot be imported by name, and a file with **no** module declaration is not a module
//! implementation unit — it is a plain translation unit, which is what most files are.

use cpp_code_analysis::{ImportTarget, ModuleInfo, ModuleUnit};
use cpp_parser::{CppParser, ParserConfig};

/// Read the module shape of a source string.
fn info(source: &str) -> ModuleInfo {
    let tree = CppParser::parse(source, ParserConfig::default());
    assert_eq!(
        tree.to_source_text(),
        source,
        "the parse must stay lossless before anything is read out of it"
    );

    ModuleInfo::from_tree(&tree.get_red_root())
}

/// The imports as `(description, is_reexport)` pairs, so a failure is readable.
fn imports(info: &ModuleInfo) -> Vec<(String, bool)> {
    info.imports
        .iter()
        .map(|import| (import.target.describe(), import.is_reexport))
        .collect()
}

// ============================================================================
// The unit kinds
// ============================================================================

/// The four unit kinds, told apart by `export` and by the partition.
#[test]
fn the_four_unit_kinds_are_told_apart() {
    let cases = [
        (
            "export module m;\n",
            ModuleUnit::InterfaceUnit,
            Some("m"),
            None,
        ),
        (
            "module m;\n",
            ModuleUnit::ImplementationUnit,
            Some("m"),
            None,
        ),
        (
            "export module m:part;\n",
            ModuleUnit::PartitionInterfaceUnit,
            Some("m"),
            Some("part"),
        ),
        (
            "module m:part;\n",
            ModuleUnit::PartitionImplementationUnit,
            Some("m"),
            Some("part"),
        ),
    ];

    for (source, unit, module_name, partition) in cases {
        let info = info(source);

        assert_eq!(info.unit, Some(unit), "{source:?}");
        assert_eq!(info.module_name.as_deref(), module_name, "{source:?}");
        assert_eq!(info.partition_name.as_deref(), partition, "{source:?}");
        assert!(info.is_module_unit(), "{source:?}");
    }
}

/// A plain translation unit has **no** unit kind, which is not the same as being an implementation unit.
///
/// The distinction matters because almost every `.cpp` is one of these: inferring "implementation unit"
/// from the absence of `export` would make every ordinary file a member of a module that does not exist.
#[test]
fn a_file_without_a_module_declaration_has_no_unit_kind() {
    for source in [
        "int x;\n",
        "#include <vector>\n",
        "export int f();\n",
        "import std;\n",
    ] {
        let info = info(source);

        assert_eq!(info.unit, None, "{source:?} declares no module");
        assert!(!info.is_module_unit(), "{source:?}");
        assert_eq!(info.module_name, None, "{source:?}");
        assert_eq!(info.qualified_name(), None, "{source:?}");
    }
}

/// A dotted module name is read as one name, not as its first identifier.
///
/// `my.mod` and `my` are different modules, so an extractor that stopped at the first identifier would
/// resolve `import my.mod;` to the wrong file — or to none.
#[test]
fn a_dotted_module_name_is_read_whole() {
    for (source, expected) in [
        ("export module my.mod;\n", "my.mod"),
        ("export module a.b.c.d;\n", "a.b.c.d"),
        ("export module my.mod:part.sub;\n", "my.mod"),
        ("module my.mod:part.sub;\n", "my.mod"),
    ] {
        let info = info(source);
        assert_eq!(info.module_name.as_deref(), Some(expected), "{source:?}");
    }
}

/// A partition's name is read from the partition, never from the module's own name node.
///
/// Both are `ModuleName` nodes, and the partition's is nested inside the partition's — so a walk that took
/// the first `ModuleName` it found would report the partition in one order and the module in another
/// depending on the traversal, which is the kind of bug that only shows up on the files with partitions.
#[test]
fn a_partition_name_is_not_the_module_name() {
    let info = info("export module my.mod:part;\n");

    assert_eq!(info.module_name.as_deref(), Some("my.mod"));
    assert_eq!(info.partition_name.as_deref(), Some("part"));
    assert_eq!(info.qualified_name().as_deref(), Some("my.mod:part"));
}

/// `module ;` and `module : private;` are recorded as flags rather than as unit kinds.
///
/// They are *fragments* of a unit, not units: a file has at most one module declaration and may have either
/// or both fragments around it. Folding them into the unit kind would make a file with a global fragment
/// unable to be an interface unit, which is every module that includes a header before its declaration.
#[test]
fn the_fragments_are_flags_on_the_unit() {
    let info =
        info("module;\n#include <vector>\nexport module m;\nmodule : private;\nint hidden;\n");

    assert_eq!(info.unit, Some(ModuleUnit::InterfaceUnit));
    assert!(info.has_global_fragment);
    assert!(info.has_private_fragment);
    assert_eq!(info.module_name.as_deref(), Some("m"));
}

/// A file with no fragments reports neither, so the flags mean something.
#[test]
fn a_plain_module_has_no_fragments() {
    let info = info("export module m;\n");

    assert!(!info.has_global_fragment);
    assert!(!info.has_private_fragment);
}

// ============================================================================
// Imports
// ============================================================================

/// The three import forms are three different targets.
#[test]
fn the_three_import_forms_are_distinguished() {
    let info = info(
        "export module m;\nimport std;\nimport :part;\nimport <vector>;\nimport \"local.h\";\n",
    );

    assert_eq!(
        info.imports
            .iter()
            .map(|i| i.target.clone())
            .collect::<Vec<_>>(),
        vec![
            ImportTarget::Module("std".into()),
            ImportTarget::Partition("part".into()),
            ImportTarget::HeaderUnit {
                name: "vector".into(),
                is_angle: true
            },
            ImportTarget::HeaderUnit {
                name: "local.h".into(),
                is_angle: false
            },
        ]
    );
}

/// `import :part;` is a partition of **this** module and carries no module name.
///
/// Recording it as a module named `part` is the mistake this pins: nothing outside `m` may import `m:part`,
/// so a resolver looking for a module called `part` finds nothing and reports a missing module that is
/// really right there in the same directory.
#[test]
fn a_partition_import_has_no_module_name_of_its_own() {
    let info = info("export module my.mod;\nimport :part;\n");

    let import = &info.imports[0];
    assert_eq!(import.target.partition_name(), Some("part"));
    assert_eq!(
        import.target.module_name(),
        None,
        "a partition is not a module"
    );
}

/// The angle/quote spelling of a header unit survives, because it decides where the header is searched for.
#[test]
fn a_header_unit_remembers_its_spelling() {
    let import = &info("import <vector>;\n").imports[0];
    assert_eq!(
        import.target,
        ImportTarget::HeaderUnit {
            name: "vector".into(),
            is_angle: true
        }
    );

    let import = &info("import \"local.h\";\n").imports[0];
    assert_eq!(
        import.target,
        ImportTarget::HeaderUnit {
            name: "local.h".into(),
            is_angle: false
        }
    );
}

/// A re-export is distinguished from a plain import.
///
/// `export import other;` makes `other`'s declarations part of *this* module's interface, so a consumer
/// building the module's export set has to follow it — while a plain `import` is private to the file.
#[test]
fn a_re_export_is_distinguished_from_a_plain_import() {
    let info = info("export module m;\nimport a;\nexport import b;\nexport import :part;\n");

    assert_eq!(
        imports(&info),
        vec![
            ("module `a`".to_string(), false),
            ("module `b`".to_string(), true),
            ("partition `:part`".to_string(), true),
        ]
    );

    assert_eq!(info.plain_imports().count(), 1);
    assert_eq!(info.reexports().count(), 2);
}

/// Imports keep their source order and their duplicates.
///
/// Both are facts about the file that a consumer may need — a duplicate import is worth reporting, and the
/// order is what a reader sees — so deduplicating here would destroy information to save a caller a step it
/// may not want to take.
#[test]
fn imports_keep_their_order_and_duplicates() {
    let info = info("export module m;\nimport b;\nimport a;\nimport b;\n");

    assert_eq!(
        imports(&info),
        vec![
            ("module `b`".to_string(), false),
            ("module `a`".to_string(), false),
            ("module `b`".to_string(), false),
        ]
    );
}

/// An import with no target is dropped rather than invented.
///
/// `import ;` is what a half-typed import looks like, and an editor parses it on the way to something else.
/// Reporting a target for it would put a name in the graph that no file can satisfy.
#[test]
fn a_targetless_import_is_dropped() {
    let info = info("export module m;\nimport ;\n");

    assert!(info.imports.is_empty(), "{:?}", info.imports);
}

// ============================================================================
// What a resolver needs
// ============================================================================

/// Only the primary interface unit answers a named import from outside.
///
/// Asserted as a property over all four kinds rather than as three examples, because this predicate is what
/// the resolver keys on and a wrong answer sends every `import m;` to a partition or to an implementation
/// unit — both of which exist on disk and would resolve to a file that does not export the name.
#[test]
fn only_the_primary_interface_unit_exports_to_importers() {
    assert!(ModuleUnit::InterfaceUnit.exports_to_importers());
    assert!(!ModuleUnit::ImplementationUnit.exports_to_importers());
    assert!(!ModuleUnit::PartitionInterfaceUnit.exports_to_importers());
    assert!(!ModuleUnit::PartitionImplementationUnit.exports_to_importers());

    // "Is an interface" is a strictly wider question, and the difference between the two is exactly the
    // partition case.
    assert!(ModuleUnit::PartitionInterfaceUnit.is_interface());
    assert!(!ModuleUnit::PartitionInterfaceUnit.exports_to_importers());
    assert!(ModuleUnit::PartitionInterfaceUnit.is_partition());
    assert!(!ModuleUnit::InterfaceUnit.is_partition());
}

/// Each unit kind describes itself, for a message that can say what the file is.
#[test]
fn unit_kinds_describe_themselves() {
    for unit in [
        ModuleUnit::InterfaceUnit,
        ModuleUnit::ImplementationUnit,
        ModuleUnit::PartitionInterfaceUnit,
        ModuleUnit::PartitionImplementationUnit,
    ] {
        assert!(!unit.describe().is_empty(), "{unit:?} must describe itself");
    }

    assert_eq!(
        ModuleUnit::InterfaceUnit.describe(),
        "module interface unit"
    );
}
