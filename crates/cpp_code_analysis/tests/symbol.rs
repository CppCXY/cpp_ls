//! The symbol model: names, scopes, bindings, and the three-valued answer.
//!
//! The tests are organised by the question each type answers, because the types are not independent and the
//! mistakes are cross-cutting. Two themes run through them:
//!
//! * **`Unknown` must not collapse into `No`.** This is the distinction the whole analysis layer rests on —
//!   `No` makes an editor grey out code or report an error, `Unknown` makes it stay quiet — and it is the
//!   kind of distinction that erodes silently, one convenient `unwrap_or_default()` at a time.
//! * **A name is not a string.** A constructor, a destructor, and a conversion function have to stay
//!   distinguishable, because whether ordinary lookup finds them is decided by that difference.

use cpp_code_analysis::{
    Binding, BindingKind, BindingOrigin, DeclName, FileId, HeaderName, Known, Name, NameKind,
    PathInterner, QualifiedName, ScopeId, ScopeKind, SymbolTable, UnknownReason,
};
use cpp_parser::{CppParser, ParserConfig, source_range};
use std::path::Path;

/// A range at `start` with `length` bytes.
///
/// `SourceRange`'s fields are public but its offsets are documented as offsets into a *source*, so one is
/// built here the way the crate builds its own: from a parse. It keeps this test honest about what a range
/// means — an offset into a file — and keeps it from depending on a parser type it should not name.
fn range(start: usize, length: usize) -> cpp_parser::SourceRange {
    let whole = source_range(
        CppParser::parse(&" ".repeat(start + length), ParserConfig::default())
            .get_red_root()
            .text_range(),
    );

    cpp_parser::SourceRange {
        start_offset: whole.start_offset + start,
        length,
    }
}

/// A binding of `name` in `scope`, for the scope tests.
fn binding(name: Name, kind: BindingKind, scope: ScopeId, start: usize) -> Binding {
    Binding {
        name,
        kind,
        range: range(start, 4),
        name_range: range(start, 2),
        scope,
        origin: None,
    }
}

/// Real file ids, minted the only way they can be.
///
/// `FileId` has no public constructor on purpose — an id that outlives its interner is meaningless — so a
/// test that needs one interns a path, which is exactly what production code does.
fn file_ids(count: usize) -> (PathInterner, Vec<FileId>) {
    let mut interner = PathInterner::new(false);
    let ids = (0..count)
        .map(|index| interner.intern(Path::new(&format!("file{index}.h"))))
        .collect();

    (interner, ids)
}

// ============================================================================
// `Known`: the three-valued answer
// ============================================================================

/// The three states are three states, and the predicates say which is which.
///
/// Asserted together rather than separately, because the failure mode is a predicate that quietly answers for
/// the wrong state: `is_known` returning true for `Unknown`, or `is_definitely_absent` doing so, would each
/// make a consumer report something it has no basis for.
#[test]
fn the_three_states_are_distinguished() {
    let yes: Known<u32> = Known::Yes(7);
    let no: Known<u32> = Known::No;
    let unknown: Known<u32> = Known::Unknown(UnknownReason::DependentName);

    assert!(yes.is_known());
    assert!(!no.is_known(), "a definite no is not an answer");
    assert!(!unknown.is_known(), "nor is not knowing");

    assert!(no.is_definitely_absent());
    assert!(!yes.is_definitely_absent());
    assert!(
        !unknown.is_definitely_absent(),
        "not knowing must never read as 'definitely absent'"
    );

    assert_eq!(yes.value_ref(), Some(&7));
    assert_eq!(no.value_ref(), None);
    assert_eq!(unknown.value_ref(), None);

    assert_eq!(yes.reason(), None);
    assert_eq!(no.reason(), None);
    assert_eq!(unknown.reason(), Some(&UnknownReason::DependentName));
}

/// `map` transforms an answer and leaves the other two states exactly as they were.
///
/// The reason this is not `Option::map`: a chain of transformations must not launder `Unknown` into `No`. If
/// it could, a consumer several steps away from the cause would see a definite absence and report an error
/// about code that is merely unanalysable.
#[test]
fn map_preserves_the_other_two_states() {
    let doubled = Known::Yes(21).map(|value| value * 2);
    assert_eq!(doubled, Known::Yes(42));

    let no: Known<u32> = Known::No;
    assert_eq!(no.clone().map(|value| value * 2), Known::No);

    let unknown: Known<u32> = Known::Unknown(UnknownReason::UnresolvedInclude("x.h".into()));
    assert_eq!(
        unknown.clone().map(|value| value * 2),
        Known::Unknown(UnknownReason::UnresolvedInclude("x.h".into())),
        "the reason survives the transformation"
    );

    // And through a chain, which is where a laundering bug would show up.
    let chained = unknown
        .map(|value| value * 2)
        .map(|value| value.to_string())
        .map(|text| text.len());

    assert!(!chained.is_known());
    assert!(!chained.is_definitely_absent());
}

/// `then_unknown` refines a non-answer and never downgrades a definite one.
///
/// The subtle case is `No`: a question that already had an answer is not made uncertain by a later step
/// failing to refine it. Turning `No` into `Unknown` there would lose a fact that was established.
#[test]
fn then_unknown_does_not_erase_a_definite_answer() {
    let known: Known<u32> = Known::Yes(1);
    assert_eq!(
        known.then_unknown(UnknownReason::UnexpandedTemplate),
        Known::Unknown(UnknownReason::UnexpandedTemplate),
        "an answer that cannot be refined becomes unknown"
    );

    let no: Known<u32> = Known::No;
    assert_eq!(
        no.then_unknown(UnknownReason::UnexpandedTemplate),
        Known::No,
        "a definite no stays definite"
    );

    let unknown: Known<u32> = Known::Unknown(UnknownReason::UnparsableName);
    assert_eq!(
        unknown.then_unknown(UnknownReason::UnexpandedTemplate),
        Known::Unknown(UnknownReason::UnexpandedTemplate),
        "the new reason replaces the old one, since the new step is what failed"
    );
}

/// `value` consumes and `value_ref` borrows, and they agree.
#[test]
fn value_and_value_ref_agree() {
    assert_eq!(Known::Yes(3).value(), Some(3));
    assert_eq!(Known::<u32>::No.value(), None);
    assert_eq!(
        Known::<u32>::Unknown(UnknownReason::DependentName).value(),
        None
    );

    let owned = Known::Yes(String::from("x"));
    assert_eq!(owned.value_ref().map(String::len), Some(1));
    assert_eq!(owned.value().as_deref(), Some("x"));
}

/// Every reason for not knowing explains itself, and no two say the same thing.
///
/// A reason that could not be rendered would make a consumer's message useless; two reasons that rendered
/// identically would mean the enum is not actually distinguishing them, which is the whole point of having
/// one.
#[test]
fn every_unknown_reason_describes_itself_distinctly() {
    let reasons = [
        UnknownReason::MacroExpansion("DECLARE".into()),
        UnknownReason::ConditionalCompilation,
        UnknownReason::UnresolvedInclude("vector".into()),
        UnknownReason::UnresolvedModule("std".into()),
        UnknownReason::DependentName,
        UnknownReason::UnexpandedTemplate,
        UnknownReason::UnparsableName,
        UnknownReason::IncompleteMacroContext,
    ];

    let mut descriptions = Vec::new();

    for reason in &reasons {
        let description = reason.describe();
        assert!(!description.is_empty(), "{reason:?} must describe itself");
        descriptions.push(description);
    }

    let count = descriptions.len();
    descriptions.sort();
    descriptions.dedup();

    assert_eq!(
        descriptions.len(),
        count,
        "two reasons describe themselves identically, so the enum is not distinguishing them"
    );
}

/// A reason that names something says *what*, so the message is actionable rather than merely honest.
///
/// "I could not tell" is only useful if it says what would have made it decidable. The three variants that
/// carry a name are the ones where the user can act on the answer.
#[test]
fn a_reason_that_names_something_includes_the_name() {
    assert!(
        UnknownReason::MacroExpansion("DECLARE_WIDGET".into())
            .describe()
            .contains("DECLARE_WIDGET")
    );
    assert!(
        UnknownReason::UnresolvedInclude("boost/variant.hpp".into())
            .describe()
            .contains("boost/variant.hpp")
    );
    assert!(
        UnknownReason::UnresolvedModule("std".into())
            .describe()
            .contains("std")
    );
}

// ============================================================================
// Names
// ============================================================================

/// A destructor is spelled with the `~`, and ordinary lookup ignores it.
///
/// The distinction the model exists for: if `~Widget` were stored as the identifier `Widget`, then `~Widget`
/// would resolve to the class and go-to-definition on a destructor call would land on the class declaration.
#[test]
fn a_destructor_is_not_the_class_name() {
    let destructor = Name::destructor("Widget");
    let class = Name::identifier("Widget");

    assert_ne!(destructor, class);
    assert_eq!(destructor.text(), "~Widget");
    assert_eq!(class.text(), "Widget");

    assert!(
        destructor.is_ordinary(),
        "a destructor is looked up by name"
    );
    assert_eq!(
        destructor.identifier_text(),
        None,
        "but it names no identifier"
    );
    assert_eq!(destructor.base_text(), Some("Widget"));

    assert_eq!(class.identifier_text(), Some("Widget"));
    assert_eq!(class.base_text(), Some("Widget"));
}

/// An operator, a conversion, and a literal are never found by ordinary lookup; an identifier is.
///
/// Asserted as a property over every kind, because this predicate is what a lookup keys on and getting it
/// wrong makes a name either invisible or wrongly found. A constructor is the case worth noting: its name
/// *is* an identifier — that is genuinely how it is spelled — so it is ordinary, and what stops `Widget x;`
/// from resolving to it is that a constructor has no return type.
#[test]
fn only_identifiers_and_destructors_are_ordinary() {
    assert!(Name::identifier("Widget").is_ordinary());
    assert!(Name::destructor("Widget").is_ordinary());

    assert!(!Name::operator("+").is_ordinary());
    assert!(!Name::conversion("int").is_ordinary());
    assert!(!Name::literal("km").is_ordinary());
}

/// The spelling a consumer shows is the spelling in the source.
///
/// `operator+`, not `+`: a user searching a codebase for `operator+` has to find what the model reports, and
/// showing `+` would be showing something that does not appear in any file.
///
/// The space is the part worth pinning. The symbolic forms are written closed up and the word forms take a
/// space — `operator new`, not `operatornew` — and one rule for both would print a spelling that appears
/// nowhere in C++.
#[test]
fn a_name_prints_the_way_it_was_written() {
    assert_eq!(Name::operator("+").text(), "operator+");
    assert_eq!(Name::operator("[]").text(), "operator[]");
    assert_eq!(Name::operator("()").text(), "operator()");
    assert_eq!(Name::operator("==").text(), "operator==");
    assert_eq!(Name::operator("<=>").text(), "operator<=>");
    assert_eq!(Name::operator("new").text(), "operator new");
    assert_eq!(Name::operator("delete").text(), "operator delete");
    assert_eq!(Name::operator("new[]").text(), "operator new[]");
    assert_eq!(Name::conversion("int").text(), "operator int");
    assert_eq!(
        Name::conversion("std::string").text(),
        "operator std::string"
    );
    assert_eq!(Name::literal("km").text(), "operator\"\"_km");
    assert_eq!(Name::destructor("Widget").text(), "~Widget");
}

/// Two conversions to the same type are one name, and to different types are two.
///
/// `operator int` and `operator long` are different functions, so a model that keyed on the word `operator`
/// alone would report one overload where there are two — and `operator  int` (extra whitespace) is the same
/// name as `operator int`, which is the normalisation the extractor is responsible for.
#[test]
fn conversion_names_are_keyed_by_type() {
    assert_eq!(Name::conversion("int"), Name::conversion("int"));
    assert_ne!(Name::conversion("int"), Name::conversion("long"));
    assert_ne!(
        Name::conversion("int"),
        Name::operator("int"),
        "a conversion and an operator are different kinds"
    );
}

/// Names sort deterministically, which is what lets a scope keep its bindings ordered.
#[test]
fn names_have_a_stable_order() {
    let mut names = vec![
        Name::operator("+"),
        Name::identifier("zebra"),
        Name::destructor("Widget"),
        Name::identifier("apple"),
        Name::conversion("int"),
        Name::identifier("Widget"),
    ];

    let first = {
        let mut sorted = names.clone();
        sorted.sort();
        sorted
    };

    // Sorting the same input twice gives the same answer, and so does sorting the output.
    names.sort();
    assert_eq!(names, first);

    let mut again = names.clone();
    again.sort();
    assert_eq!(again, names, "sorting is idempotent");
}

/// The kind discriminates, so two names that print differently never compare equal.
#[test]
fn a_name_is_keyed_by_kind_and_spelling() {
    assert_eq!(Name::identifier("f"), Name::identifier("f"));
    assert_ne!(Name::identifier("f"), Name::identifier("g"));
    assert_ne!(
        Name::identifier("f"),
        Name::conversion("f"),
        "the kind is part of the key"
    );
    assert_eq!(
        Name::identifier("f").kind,
        NameKind::Identifier("f".to_string())
    );
}

// ============================================================================
// Qualified and module-shaped names
// ============================================================================

/// A qualified name keeps its components apart, because resolving them is the deferred question.
///
/// `ns::Inner` is not the string `"ns::Inner"`: `ns` has to be looked up before `Inner` means anything, and
/// flattening the two would make that lookup impossible while looking as though it had been done.
#[test]
fn a_qualified_name_keeps_its_components() {
    let qualified = QualifiedName::from_components(["ns", "Inner"].map(String::from));

    assert_eq!(qualified.components, vec!["ns", "Inner"]);
    assert_eq!(qualified.last(), Some("Inner"));
    assert_eq!(qualified.qualifiers(), ["ns"]);
    assert_eq!(qualified.text(), "ns::Inner");

    let single = QualifiedName::single("Widget");
    assert_eq!(single.last(), Some("Widget"));
    assert!(single.qualifiers().is_empty());

    let empty = QualifiedName::from_components(Vec::<String>::new());
    assert!(empty.is_empty());
    assert_eq!(empty.last(), None);
    assert!(empty.qualifiers().is_empty(), "an empty name has no parts");
}

/// A dotted module name is a qualified name, and `my.mod` is two components rather than one string.
#[test]
fn a_module_name_keeps_its_components() {
    let module = DeclName::Module(QualifiedName::from_components(
        ["my", "mod"].map(String::from),
    ));

    assert_eq!(
        module.text(),
        "my::mod",
        "shown with the separator the model uses"
    );
    assert_eq!(module.ordinary_name(), None, "a module is not in a scope");
}

/// A partition prints with its leading colon, because that is how it is written.
#[test]
fn a_partition_name_prints_with_its_colon() {
    let partition = DeclName::Partition(QualifiedName::single("part"));

    assert_eq!(partition.text(), ":part");
    assert_eq!(partition.ordinary_name(), None);
}

/// A header unit renders with the delimiters that decide where it is searched for.
///
/// `"local.h"` and `<local.h>` can be two different files — the quoted form searches the including file's own
/// directory first and the angle form does not — so the spelling is part of the name rather than decoration.
#[test]
fn a_header_unit_keeps_its_spelling() {
    let quoted = DeclName::HeaderUnit(HeaderName {
        name: "local.h".into(),
        is_angle: false,
    });
    let angled = DeclName::HeaderUnit(HeaderName {
        name: "local.h".into(),
        is_angle: true,
    });

    assert_eq!(quoted.text(), "\"local.h\"");
    assert_eq!(angled.text(), "<local.h>");
    assert_ne!(quoted, angled, "the spelling is part of the identity");
    assert_eq!(quoted.ordinary_name(), None);
}

/// A qualified declaration has no ordinary name until its qualifier is resolved, and saying so is the point.
#[test]
fn a_qualified_declaration_has_no_ordinary_name_yet() {
    let qualified = DeclName::Qualified(QualifiedName::from_components(
        ["ns", "count"].map(String::from),
    ));

    assert_eq!(
        qualified.ordinary_name(),
        None,
        "`ns::count` needs `ns` resolved first, so it is not an ordinary lookup key"
    );

    let plain = DeclName::Named(Name::identifier("count"));
    assert!(plain.ordinary_name().is_some());
}

/// A declaration whose name is a destructured operator still has no ordinary name, through `DeclName` too.
#[test]
fn an_operator_declaration_has_no_ordinary_name() {
    let operator = DeclName::Named(Name::operator("=="));

    assert_eq!(operator.text(), "operator==");
    assert_eq!(
        operator.ordinary_name(),
        None,
        "an operator is found by overload resolution, not by an ordinary name lookup"
    );
}

// ============================================================================
// Scopes
// ============================================================================

/// A table has one file scope, and only the first parentless scope becomes it.
///
/// Two roots would make "the file's declarations" ambiguous, which every consumer of this table depends on
/// being well defined.
#[test]
fn only_one_scope_is_the_file_scope() {
    let mut table = SymbolTable::new();
    assert!(table.root().is_none(), "an empty table has no root");

    let file = table.create_scope(ScopeKind::TranslationUnit, None, Some(range(0, 100)));
    assert_eq!(table.root(), Some(file));

    let second = table.create_scope(ScopeKind::Namespace, None, Some(range(0, 50)));
    assert_eq!(
        table.root(),
        Some(file),
        "the first root stands; a second parentless scope does not replace it"
    );
    assert_ne!(second, file);
}

/// A child scope is recorded on both sides, so a consumer can walk either way.
///
/// A parent link alone would make "what is declared inside this namespace" a full scan of the table.
#[test]
fn child_scopes_are_linked_both_ways() {
    let mut table = SymbolTable::new();
    let file = table.create_scope(ScopeKind::TranslationUnit, None, Some(range(0, 200)));
    let ns = table.create_scope(ScopeKind::Namespace, Some(file), Some(range(10, 100)));
    let class = table.create_scope(ScopeKind::Class, Some(ns), Some(range(20, 50)));

    assert_eq!(table.scope(file).unwrap().children, vec![ns]);
    assert_eq!(table.scope(ns).unwrap().children, vec![class]);
    assert!(table.scope(class).unwrap().children.is_empty());

    assert_eq!(table.scope(class).unwrap().parent, Some(ns));
    assert_eq!(table.scope(ns).unwrap().parent, Some(file));
    assert_eq!(table.scope(file).unwrap().parent, None);
}

/// The scope at an offset is the innermost one containing it.
///
/// Innermost, not outermost and not first-found: a cursor inside a class inside a namespace is in the class's
/// scope, and a consumer that got the namespace instead would offer the wrong completions everywhere.
#[test]
fn the_scope_at_an_offset_is_the_innermost() {
    let mut table = SymbolTable::new();
    let file = table.create_scope(ScopeKind::TranslationUnit, None, Some(range(0, 200)));
    let ns = table.create_scope(ScopeKind::Namespace, Some(file), Some(range(10, 100)));
    let class = table.create_scope(ScopeKind::Class, Some(ns), Some(range(20, 50)));

    assert_eq!(table.scope_at(5), Some(file), "before the namespace");
    assert_eq!(table.scope_at(15), Some(ns), "inside the namespace only");
    assert_eq!(table.scope_at(30), Some(class), "inside the class as well");
    assert_eq!(table.scope_at(150), Some(file), "after them all");

    // The boundary is inclusive at both ends, because a cursor sits *between* characters and a consumer
    // asking about the last character of a declaration must not fall outside its scope.
    assert_eq!(table.scope_at(20), Some(class));
    assert_eq!(table.scope_at(50), Some(class));
    assert_eq!(table.scope_at(100), Some(ns));
}

/// An offset no scope covers has no scope, rather than silently getting the file scope.
///
/// A table built from a different source, or an offset past the end of this one, would otherwise get the
/// file scope and a consumer would confidently complete names that are not in scope at all.
#[test]
fn an_uncovered_offset_has_no_scope() {
    let mut table = SymbolTable::new();
    table.create_scope(ScopeKind::TranslationUnit, None, Some(range(0, 10)));

    assert_eq!(table.scope_at(11), None);
    assert_eq!(table.scope_at(usize::MAX), None, "and does not overflow");

    // A synthesised scope with no range is never "at" an offset.
    let mut ranged = SymbolTable::new();
    ranged.create_scope(ScopeKind::TranslationUnit, None, None);
    assert_eq!(ranged.scope_at(0), None);
}

/// The scope chain runs outward, innermost first, and stops at the file scope.
///
/// Innermost-first because that is the order C++ lookup considers scopes, so a consumer walking the chain
/// walks in the order the language does.
#[test]
fn the_scope_chain_runs_outward() {
    let mut table = SymbolTable::new();
    let file = table.create_scope(ScopeKind::TranslationUnit, None, Some(range(0, 200)));
    let ns = table.create_scope(ScopeKind::Namespace, Some(file), Some(range(10, 100)));
    let function = table.create_scope(ScopeKind::Function, Some(ns), Some(range(20, 80)));
    let block = table.create_scope(ScopeKind::Block, Some(function), Some(range(30, 60)));

    assert_eq!(table.scope_chain(block), vec![block, function, ns, file]);
    assert_eq!(table.scope_chain(file), vec![file]);
}

/// A chain cannot loop, even if a table were built by hand with a cycle in it.
///
/// The type cannot construct one, but the walk is public and a consumer handing it a malformed table must not
/// hang an editor. Cheap to guarantee, expensive to debug otherwise.
#[test]
fn a_cyclic_scope_chain_terminates() {
    let mut table = SymbolTable::new();
    let a = table.create_scope(ScopeKind::Block, None, Some(range(0, 10)));
    let b = table.create_scope(ScopeKind::Block, Some(a), Some(range(0, 5)));

    // Point `a` back at `b`, which `create_scope` cannot do.
    table.scope_mut(a).unwrap().parent = Some(b);

    let chain = table.scope_chain(a);
    assert!(chain.len() <= 2, "the walk stopped: {chain:?}");
}

// ============================================================================
// Bindings
// ============================================================================

/// A name declared twice keeps both bindings, in declaration order.
///
/// Overloads, redeclarations, and a variable shadowing a function are all ordinary C++, so a scope that kept
/// only one binding per name would be making a resolution decision this layer is not entitled to make.
#[test]
fn one_name_can_have_several_bindings() {
    let mut table = SymbolTable::new();
    let file = table.create_scope(ScopeKind::TranslationUnit, None, Some(range(0, 100)));

    assert!(table.add_binding(
        file,
        binding(Name::identifier("f"), BindingKind::Function, file, 0)
    ));
    assert!(table.add_binding(
        file,
        binding(Name::identifier("f"), BindingKind::Function, file, 20)
    ));
    assert!(table.add_binding(
        file,
        binding(Name::identifier("g"), BindingKind::Variable, file, 40)
    ));

    let scope = table.scope(file).unwrap();
    let overloads: Vec<usize> = scope
        .bindings_of(&Name::identifier("f"))
        .map(|binding| binding.name_range.start_offset)
        .collect();

    assert_eq!(overloads, vec![0, 20], "both, in declaration order");

    assert_eq!(
        scope.declared_names(),
        vec![&Name::identifier("f"), &Name::identifier("g")],
        "the names are deduplicated"
    );
}

/// Bindings stay sorted by name, so a lookup can binary-search them.
///
/// Insertion order is not the storage order, which is the whole reason `add_binding` does a search rather
/// than a push. Asserted by inserting out of order and reading back.
#[test]
fn bindings_are_kept_sorted_by_name() {
    let mut table = SymbolTable::new();
    let file = table.create_scope(ScopeKind::TranslationUnit, None, Some(range(0, 100)));

    for name in ["zebra", "apple", "mango", "banana"] {
        table.add_binding(
            file,
            binding(Name::identifier(name), BindingKind::Variable, file, 0),
        );
    }

    let sorted: Vec<String> = table
        .scope(file)
        .unwrap()
        .bindings
        .iter()
        .map(|binding| binding.name.text())
        .collect();

    assert_eq!(sorted, vec!["apple", "banana", "mango", "zebra"]);
}

/// A binding whose scope does not exist is rejected rather than panicking.
///
/// This layer runs on malformed input and on tables being built while they are read; returning `false` leaves
/// a gap a consumer can see, where a panic stops the editor.
#[test]
fn a_binding_for_a_missing_scope_is_rejected() {
    let mut table = SymbolTable::new();
    let absent = ScopeId(99);

    let added = table.add_binding(
        absent,
        binding(Name::identifier("x"), BindingKind::Variable, absent, 0),
    );

    assert!(!added);
    assert!(table.is_empty(), "nothing was created on the way");
}

/// A label is only legal in a function or a block, and a namespace only at file or namespace scope.
///
/// Small rules with silent failures: a label added to a namespace would make `goto` completion offer names
/// that cannot be jumped to, and a namespace added to a function would make `namespace ns { }` inside a body
/// look like it declared something at file scope.
#[test]
fn a_scope_rejects_bindings_it_cannot_hold() {
    let mut table = SymbolTable::new();
    let file = table.create_scope(ScopeKind::TranslationUnit, None, Some(range(0, 200)));
    let ns = table.create_scope(ScopeKind::Namespace, Some(file), Some(range(10, 100)));
    let function = table.create_scope(ScopeKind::Function, Some(ns), Some(range(20, 80)));
    let block = table.create_scope(ScopeKind::Block, Some(function), Some(range(30, 60)));
    let parameters = table.create_scope(
        ScopeKind::TemplateParameters,
        Some(file),
        Some(range(120, 150)),
    );

    // Labels: legal in a function and a block, not in a namespace.
    assert!(table.add_binding(
        block,
        binding(Name::identifier("done"), BindingKind::Label, block, 30)
    ));
    assert!(table.add_binding(
        function,
        binding(Name::identifier("retry"), BindingKind::Label, function, 20)
    ));
    assert!(
        !table.add_binding(
            ns,
            binding(Name::identifier("bad"), BindingKind::Label, ns, 10)
        ),
        "a namespace cannot hold a label"
    );

    // Namespaces and `using namespace`: legal at file scope and inside a namespace, not in a block.
    assert!(table.add_binding(
        file,
        binding(Name::identifier("outer"), BindingKind::Namespace, file, 0)
    ));
    assert!(table.add_binding(
        ns,
        binding(Name::identifier("inner"), BindingKind::Namespace, ns, 10)
    ));
    assert!(
        !table.add_binding(
            block,
            binding(Name::identifier("nope"), BindingKind::Namespace, block, 30)
        ),
        "a block cannot hold a namespace"
    );
    assert!(!table.add_binding(
        block,
        binding(
            Name::identifier("std"),
            BindingKind::UsingDirective,
            block,
            30
        )
    ));

    // A template parameter list holds names, and a variable declared there is not a thing.
    assert!(table.add_binding(
        parameters,
        binding(
            Name::identifier("T"),
            BindingKind::TemplateParameter,
            parameters,
            120
        )
    ));
    assert!(
        !table.add_binding(
            parameters,
            binding(
                Name::identifier("v"),
                BindingKind::Variable,
                parameters,
                130
            )
        ),
        "a template parameter list is not a place for a variable"
    );
}

/// A binding records the declaration's range and the name's separately.
///
/// Collapsing them is the difference between a rename that edits one identifier and one that deletes a whole
/// declaration, so the two are asserted to be different and to be the ranges that were asked for.
#[test]
fn a_binding_keeps_the_declaration_and_the_name_apart() {
    let mut table = SymbolTable::new();
    let file = table.create_scope(ScopeKind::TranslationUnit, None, Some(range(0, 100)));

    let mut binding = binding(Name::identifier("count"), BindingKind::Variable, file, 10);
    binding.range = range(10, 20);
    binding.name_range = range(14, 5);
    table.add_binding(file, binding.clone());

    let stored = &table.scope(file).unwrap().bindings[0];

    assert_eq!(stored.range, range(10, 20), "the whole declaration");
    assert_eq!(stored.name_range, range(14, 5), "just the name");
    assert_ne!(stored.range, stored.name_range);
}

/// A binding says where it came from when that is not this file's own text.
///
/// A binding produced by a macro has a range inside the expansion, and a consumer that treated it as ordinary
/// text would offer a rename editing the macro's arguments. A binding reached through an `#include` has a
/// range that is an offset into *that* header, so the file is not optional information.
#[test]
fn a_binding_records_its_origin() {
    let (interner, files) = file_ids(4);
    assert_eq!(files.len(), 4, "the interner is what mints ids");

    let mut table = SymbolTable::new();
    let file = table.create_scope(ScopeKind::TranslationUnit, None, Some(range(0, 100)));

    let mut from_macro = binding(
        Name::identifier("FROM_MACRO"),
        BindingKind::Variable,
        file,
        0,
    );
    from_macro.origin = Some(BindingOrigin::MacroExpansion {
        macro_name: "DEFINE_FIELD".into(),
        definition: range(500, 30),
    });
    table.add_binding(file, from_macro);

    let mut from_header = binding(
        Name::identifier("from_header"),
        BindingKind::Class,
        file,
        10,
    );
    from_header.origin = Some(BindingOrigin::Included { file: files[3] });
    table.add_binding(file, from_header);

    let plain = binding(Name::identifier("plain"), BindingKind::Variable, file, 20);
    table.add_binding(file, plain);

    let scope = table.scope(file).unwrap();

    assert!(
        matches!(
            scope.bindings[0].origin,
            Some(BindingOrigin::MacroExpansion { ref macro_name, .. }) if macro_name == "DEFINE_FIELD"
        ),
        "{:?}",
        scope.bindings[0].origin
    );
    assert!(
        matches!(
            scope.bindings[1].origin,
            Some(BindingOrigin::Included { file }) if file == files[3]
        ),
        "the header a binding came from is recorded, because its range is an offset into that file"
    );
    assert_eq!(
        scope.bindings[2].origin, None,
        "a declaration written here has no origin"
    );

    // The point of recording it: two bindings of the same name from different files are not the same
    // declaration, and the origin is what tells them apart.
    let _ = interner;
}

// ============================================================================
// What the binding kinds mean
// ============================================================================

/// The binding kinds classify by *shape*, which is all the syntax can say.
///
/// `is_type_like` is what a consumer keying on "the thing after `::` must be a type or a namespace" needs. Its
/// answer is about what was written, not about what the entity is: an alias is a type because a type name can
/// be used where it appears.
#[test]
fn type_like_bindings_are_the_ones_a_type_name_can_use() {
    for kind in [
        BindingKind::Class,
        BindingKind::Enum,
        BindingKind::Alias,
        BindingKind::Typedef,
        BindingKind::Namespace,
    ] {
        assert!(
            kind.is_type_like(),
            "{kind:?} can be used as a name qualifier"
        );
    }

    for kind in [
        BindingKind::Variable,
        BindingKind::Function,
        BindingKind::Enumerator,
        BindingKind::Label,
        BindingKind::Constructor,
        BindingKind::UsingDirective,
        BindingKind::TemplateParameter,
    ] {
        assert!(!kind.is_type_like(), "{kind:?} cannot");
    }
}

/// A template parameter is its own kind, because `T::value_type` has to be answerable as "not yet".
///
/// The distinction the kind exists for: resolving `T` finds a parameter, and what `T::value_type` then means
/// depends on an argument that is not known — which is [`UnknownReason::DependentName`] rather than a
/// definite absence. Storing `T` as an alias would make it look like a known type and turn every dependent
/// name into a wrong answer.
#[test]
fn a_template_parameter_is_not_an_alias() {
    let parameter = BindingKind::TemplateParameter;

    assert_ne!(parameter, BindingKind::Alias);
    assert_ne!(parameter, BindingKind::Typedef);
    assert!(
        !parameter.is_type_like(),
        "a parameter is not a name a type qualifier can use yet"
    );
    assert!(
        !parameter.opens_a_scope(),
        "the parameter list is the scope; the parameter does not open another"
    );
}

/// A binding kind says whether it introduces a scope, which is what a consumer building the tree needs.
#[test]
fn scoped_binding_kinds_are_the_ones_that_open_a_scope() {
    for kind in [
        BindingKind::Namespace,
        BindingKind::Class,
        BindingKind::Enum,
        BindingKind::Function,
    ] {
        assert!(kind.opens_a_scope(), "{kind:?} opens a scope");
    }

    for kind in [
        BindingKind::Variable,
        BindingKind::Enumerator,
        BindingKind::Typedef,
        BindingKind::Label,
    ] {
        assert!(!kind.opens_a_scope(), "{kind:?} does not");
    }
}
