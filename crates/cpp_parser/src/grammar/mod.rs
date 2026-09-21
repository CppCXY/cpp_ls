use crate::{parser::CompleteMarker, parser_error::CppParseError};

mod cpp;

// NOTE: the former `doc` module (LDoc comment grammar) now lives in `reference/ldoc-grammar/`,
// outside the crate's module graph. It does not compile against the current `kind` layer and is
// kept as a model for the Doxygen comment grammar we will write later. See
// `reference/README.md`.

/// Outcome of parsing one construct.
///
/// `Err` means "this construct is not valid here"; the caller decides whether to recover, to try
/// another interpretation, or to surface it. The parser therefore never panics and never stops.
pub type ParseResult = Result<CompleteMarker, CppParseError>;

pub use cpp::parse_cpp_unit;

// Marker discipline, and why the `?` operator is not enough on its own.
//
// A grammar rule that returns early with `?` leaves the nodes it had already opened *open*. That
// is not a local problem: the stray `NodeStart` sits **before** the `NodeEnd` of the ancestor
// that eventually closes, so the tree builder nests it wrongly and every following token gets
// swallowed into it. The resulting tree is still internally well-formed — correct ranges, correct
// nesting — so nothing detects it except a shape-level test.
//
// The rule is therefore: **a rule that can fail must close its own nodes before propagating**.
// Rather than trusting every one of the ~20 rules to remember, the escape points are made
// explicit and few:
//
// * `CppParser::open_marks` snapshots the open-node count on entry.
// * `CppParser::finish_marks_to` closes exactly those nodes on the error path.
// * Statement-level rules are entered through `parse_stat` / `parse_compound_stat`, and the
//   recovery in `parse_stats` unwinds to its own snapshot after any `Err`, so a leak inside a rule
//   is contained by the rule that called it.
//
// New rules should follow the same shape:
//
//     fn parse_thing(p: &mut CppParser) -> ParseResult {
//         let base = p.open_marks();
//         let m = p.mark(CppSyntaxKind::Thing);
//         if let Err(err) = fallible_part(p) {
//             p.finish_marks_to(base);
//             return Err(err);
//         }
//         ...
//         Ok(m.complete(p))
//     }

