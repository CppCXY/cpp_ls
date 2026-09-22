//! What a Doxygen command *is*, as opposed to how it is written.
//!
//! The command set is the one part of Doxygen support that is a list rather than logic, so it lives
//! on its own. Keeping it out of the grammar is what makes the grammar table-driven: adding `@since`
//! is a row here, not a new parse function and a new node kind.
//!
//! # Why a table rather than a match arm per command
//!
//! The original LDoc grammar this project grew out of had one `parse_tag_*` function per tag. That
//! works for twenty tags and collapses at two hundred: the functions are near-identical, the
//! differences are which arguments they take, and every one of them needs its own node kind and its
//! own test. Describing the *shape* of a command's arguments instead makes the parser one loop, and
//! makes "does `@param` take a direction?" a question with a single answer in a single place.

/// What kind of thing a command's arguments are.
///
/// This is what a parser needs in order to read the arguments correctly, and nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocCommandArgs {
    /// No arguments, and no body: `@endcode`, `@public`. Everything to the end of the line is
    /// ignored rather than recorded, because there is nothing to record.
    None,

    /// No arguments, then a body: `@brief`, `@returns`, `@note`.
    Body,

    /// One name, then a body: `@param x the x`, `@tparam T the type`, `@exception E when`.
    NameAndBody,

    /// One name, optionally preceded by a bracketed direction or range: `@param[in] x desc`,
    /// `@param[1,3] desc`.
    DirectionalNameAndBody,

    /// One reference, then a body: `@ref Foo`, `@sa Foo::bar`.
    ReferenceAndBody,

    /// A section title, then a body: `@section intro Introduction`.
    SectionTitle,

    /// Everything up to the matching `@endcode` is code, not documentation.
    CodeBlock,
}

/// What a command is known as.
///
/// Only used to classify the command for a consumer; the parse is driven by [`DocCommandArgs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocCommandKind {
    /// Describes the thing being documented: `@brief`, `@details`, `@return`.
    Description,
    /// Documents a parameter or template parameter: `@param`, `@tparam`.
    Parameter,
    /// Documents a thrown exception: `@exception`, `@throws`.
    Exception,
    /// Points at another entity: `@ref`, `@sa`, `@see`.
    Reference,
    /// Cross-cutting prose: `@note`, `@warning`, `@deprecated`, `@since`.
    Advisory,
    /// Affects how the documentation is processed: `@ingroup`, `@file`, `@internal`.
    Structural,
    /// A callout: `@todo`, `@bug`, `@test`.
    Callout,
}

/// A command the parser knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocCommandSpec {
    /// The name as written, without the introducer, in lower case.
    pub name: &'static str,
    pub args: DocCommandArgs,
    pub kind: DocCommandKind,
}

/// Look up a command by name.
///
/// The match is case-insensitive, which is what Doxygen does: `@Param` and `@param` are the same
/// command, and a file that uses `@Param` must not silently lose its parameter documentation.
///
/// Unknown names are *not* an error. Doxygen has aliases defined by the user (`ALIASES += ...`) and
/// adds commands between releases, so an unrecognised `@whatever` is still a command — it is simply
/// one whose arguments this table cannot describe, and it gets the permissive reading.
pub fn lookup(name: &str) -> Option<DocCommandSpec> {
    let lower = name.to_ascii_lowercase();
    DOC_COMMANDS
        .iter()
        .find(|spec| spec.name == lower)
        .copied()
}

/// How to read a command that is not in the table.
///
/// Permissive on purpose: an unknown command keeps its whole line as a body rather than losing it, so
/// a user-defined alias documents something instead of nothing.
pub const UNKNOWN_COMMAND_ARGS: DocCommandArgs = DocCommandArgs::Body;

/// A command that ends a code block. Handled by name rather than by the table, because it only ever
/// appears inside one.
pub const END_CODE_COMMAND: &str = "endcode";

/// Every command the parser understands.
///
/// Deliberately long rather than minimal: a command that is missing from the table still parses, but
/// its *arguments* are guessed, and `@param` without its name is the difference between a parameter
/// being documented and a `<dd>` full of prose.
const DOC_COMMANDS: &[DocCommandSpec] = &[
    // ---- Descriptions ----
    spec("brief", DocCommandArgs::Body, DocCommandKind::Description),
    spec("short", DocCommandArgs::Body, DocCommandKind::Description),
    spec("details", DocCommandArgs::Body, DocCommandKind::Description),
    spec("return", DocCommandArgs::Body, DocCommandKind::Description),
    spec("returns", DocCommandArgs::Body, DocCommandKind::Description),
    spec("result", DocCommandArgs::Body, DocCommandKind::Description),
    spec("summary", DocCommandArgs::Body, DocCommandKind::Description),
    spec("author", DocCommandArgs::Body, DocCommandKind::Description),
    spec("authors", DocCommandArgs::Body, DocCommandKind::Description),
    spec("date", DocCommandArgs::Body, DocCommandKind::Description),
    spec("version", DocCommandArgs::Body, DocCommandKind::Description),
    spec("copyright", DocCommandArgs::Body, DocCommandKind::Description),
    spec("remark", DocCommandArgs::Body, DocCommandKind::Callout),
    spec("remarks", DocCommandArgs::Body, DocCommandKind::Callout),
    // Doxygen's `@param` and `@tparam` are the two that carry a name, and getting that name out is
    // the whole point of parsing them.
    spec("param", DocCommandArgs::DirectionalNameAndBody, DocCommandKind::Parameter),
    spec("tparam", DocCommandArgs::NameAndBody, DocCommandKind::Parameter),
    spec("exception", DocCommandArgs::NameAndBody, DocCommandKind::Exception),
    spec("throw", DocCommandArgs::NameAndBody, DocCommandKind::Exception),
    spec("throws", DocCommandArgs::NameAndBody, DocCommandKind::Exception),
    // ---- References ----
    spec("ref", DocCommandArgs::ReferenceAndBody, DocCommandKind::Reference),
    spec("sa", DocCommandArgs::ReferenceAndBody, DocCommandKind::Reference),
    spec("see", DocCommandArgs::ReferenceAndBody, DocCommandKind::Reference),
    spec("overload", DocCommandArgs::ReferenceAndBody, DocCommandKind::Reference),
    spec("extends", DocCommandArgs::ReferenceAndBody, DocCommandKind::Reference),
    spec("implements", DocCommandArgs::ReferenceAndBody, DocCommandKind::Reference),
    // ---- Advisory ----
    spec("note", DocCommandArgs::Body, DocCommandKind::Advisory),
    spec("warning", DocCommandArgs::Body, DocCommandKind::Advisory),
    spec("attention", DocCommandArgs::Body, DocCommandKind::Advisory),
    spec("deprecated", DocCommandArgs::Body, DocCommandKind::Advisory),
    spec("since", DocCommandArgs::Body, DocCommandKind::Advisory),
    spec("pre", DocCommandArgs::Body, DocCommandKind::Advisory),
    spec("post", DocCommandArgs::Body, DocCommandKind::Advisory),
    spec("invariant", DocCommandArgs::Body, DocCommandKind::Advisory),
    spec("par", DocCommandArgs::Body, DocCommandKind::Advisory),
    // ---- Structural ----
    spec("file", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("headerfile", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("ingroup", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("defgroup", DocCommandArgs::NameAndBody, DocCommandKind::Structural),
    spec("addtogroup", DocCommandArgs::NameAndBody, DocCommandKind::Structural),
    spec("weakgroup", DocCommandArgs::NameAndBody, DocCommandKind::Structural),
    spec("name", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("namespace", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("class", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("struct", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("union", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("enum", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("fn", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("var", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("typedef", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("interface", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("protocol", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("category", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("property", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("relates", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("related", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("relatesalso", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("relatedalso", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("memberof", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("hideinitializer", DocCommandArgs::None, DocCommandKind::Structural),
    spec("showinitializer", DocCommandArgs::None, DocCommandKind::Structural),
    spec("nosubgrouping", DocCommandArgs::None, DocCommandKind::Structural),
    spec("internal", DocCommandArgs::None, DocCommandKind::Structural),
    spec("endinternal", DocCommandArgs::None, DocCommandKind::Structural),
    spec("public", DocCommandArgs::None, DocCommandKind::Structural),
    spec("protected", DocCommandArgs::None, DocCommandKind::Structural),
    spec("private", DocCommandArgs::None, DocCommandKind::Structural),
    spec("static", DocCommandArgs::None, DocCommandKind::Structural),
    spec("pure", DocCommandArgs::None, DocCommandKind::Structural),
    spec("virtual", DocCommandArgs::None, DocCommandKind::Structural),
    spec("nosubgrouping", DocCommandArgs::None, DocCommandKind::Structural),
    spec("callgraph", DocCommandArgs::None, DocCommandKind::Structural),
    spec("hidecallgraph", DocCommandArgs::None, DocCommandKind::Structural),
    spec("callergraph", DocCommandArgs::None, DocCommandKind::Structural),
    spec("hidecallergraph", DocCommandArgs::None, DocCommandKind::Structural),
    // ---- Sections and formatting ----
    spec("section", DocCommandArgs::SectionTitle, DocCommandKind::Structural),
    spec("subsection", DocCommandArgs::SectionTitle, DocCommandKind::Structural),
    spec("subsubsection", DocCommandArgs::SectionTitle, DocCommandKind::Structural),
    spec("paragraph", DocCommandArgs::SectionTitle, DocCommandKind::Structural),
    spec("anchor", DocCommandArgs::SectionTitle, DocCommandKind::Structural),
    spec("code", DocCommandArgs::CodeBlock, DocCommandKind::Structural),
    spec("endcode", DocCommandArgs::None, DocCommandKind::Structural),
    spec("verbatim", DocCommandArgs::CodeBlock, DocCommandKind::Structural),
    spec("endverbatim", DocCommandArgs::None, DocCommandKind::Structural),
    spec("dot", DocCommandArgs::CodeBlock, DocCommandKind::Structural),
    spec("enddot", DocCommandArgs::None, DocCommandKind::Structural),
    spec("htmlonly", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("endhtmlonly", DocCommandArgs::None, DocCommandKind::Structural),
    spec("latexonly", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("endlatexonly", DocCommandArgs::None, DocCommandKind::Structural),
    spec("xmlonly", DocCommandArgs::Body, DocCommandKind::Structural),
    spec("endxmlonly", DocCommandArgs::None, DocCommandKind::Structural),
    // ---- Callouts ----
    spec("todo", DocCommandArgs::Body, DocCommandKind::Callout),
    spec("bug", DocCommandArgs::Body, DocCommandKind::Callout),
    spec("test", DocCommandArgs::Body, DocCommandKind::Callout),
    spec("example", DocCommandArgs::Body, DocCommandKind::Callout),
    spec("attention", DocCommandArgs::Body, DocCommandKind::Callout),
    spec("cite", DocCommandArgs::ReferenceAndBody, DocCommandKind::Callout),
    spec("xrefitem", DocCommandArgs::NameAndBody, DocCommandKind::Callout),
];

const fn spec(
    name: &'static str,
    args: DocCommandArgs,
    kind: DocCommandKind,
) -> DocCommandSpec {
    DocCommandSpec { name, args, kind }
}
