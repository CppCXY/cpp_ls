use crate::{
    grammar::parse_cpp_unit,
    kind::{CppSyntaxKind, CppTokenKind, Dialect},
    lexer::CppTokenData,
    parser_error::CppParseError,
    symbols::SymbolKind,
    syntax::{CppSyntaxTree, CppTreeBuilder},
    text::SourceRange,
};

use super::{
    marker::{MarkEvent, MarkerEventContainer},
    parser_config::ParserConfig,
    type_names::TypeNames,
};

/// A resumable point in the parse.
///
/// C++ cannot be parsed with a single token of lookahead (`a * b;` is either a declaration or a
/// multiplication; `T<U> x` is either a template-id or two comparisons), so the parser must be
/// able to *try* an interpretation and rewind cheaply. Because the parser is an append-only event
/// list plus a token cursor, rewinding is just truncating the list and restoring the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    events_len: usize,
    token_index: usize,
    open_marks: usize,
    /// The declaration type name in force when the checkpoint was taken.
    ///
    /// This is parser state that a rollback must restore, and it is easy to forget because it is
    /// not in the event stream. A speculative region — the argument list of an apparent call, say —
    /// parses its own nested "declaration" and calls [`CppParser::begin_declaration_type`], which
    /// parks the enclosing declaration's type name aside. Rolling the events back without rolling
    /// this back would silently lose that name, and it is exactly the input
    /// [`CppParser::is_a_known_type_name`] needs. See [`CppParser::rollback`].
    declaration_type_name: Option<Box<str>>,
    previous_declaration_type_name: Option<Box<str>>,
    /// Was the declaration's type written as a **qualified** name when the checkpoint was taken?
    ///
    /// Parser state like the two above, and restored for the same reason: a speculative region parses its own
    /// declaration, and whether *that* one's type was qualified must not leak back into the enclosing one.
    declaration_type_is_qualified: bool,
    /// How long [`CppParser`]'s closed-marks journal was when the checkpoint was taken.
    ///
    /// The one piece of bookkeeping that is neither an event nor a token: a rollback has to forget the markers
    /// closed since, and this is what names them — see the journal's own note for the measurement that made the
    /// difference between a linear parse and a quadratic one.
    closed_marks_journal_len: usize,    /// How many diagnostics had been reported when the checkpoint was taken.
    ///
    /// The fourth piece of state that is not in the event stream, and the one that is easiest to miss because it
    /// is not *parse* state at all: a reading that is tried and rewound reported its problems on the way, and a
    /// problem with a reading nobody kept is a problem the file does not have. [`CppParser::rollback`] truncates
    /// the list back to this, so a speculative attempt leaves the tree **and** the diagnostics as it found them.
    errors_len: usize,
    /// See [`CppParser::note_the_terminator_came_from_a_branch`] — the fifth piece of state outside the events.
    ///
    /// It is here for the reason the docs on that flag give, and it is worth reading beside `open_bodies`, which
    /// is *not* here: the difference is whether a speculative region can set the state and then rewind past it.
    /// For this one it can, so it must be restored.
    terminator_came_from_a_branch: bool,
    /// The bracket depth and the innermost template argument list's, when the checkpoint was taken.
    ///
    /// The sixth piece of state outside the event stream, and it is here for the reason the two above are: both are
    /// moved by *consuming a token*, so a speculative reading moves them just as surely as one that is kept, and
    /// [`CppParser::rollback`] truncating the events without putting them back would leave the next reading
    /// answering [`CppParser::greater_than_is_an_operator`] about a bracket that is no longer there. The measured
    /// shape is a declaration/expression speculation that reads a parenthesised expression and is rewound: the
    /// depth would come back one too high for every remaining `>` in the file.
    bracket_depth: usize,
    innermost_template_argument_list: Option<usize>,
}


/// A position in the parse, for a question asked later about the tokens around it.
///
/// Distinct from [`Checkpoint`], which is for *rewinding* to a position, and from the marker-stack length
/// [`CppParser::open_marks`] returns, which is for closing nodes. See [`CppParser::anchor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseAnchor {
    token_index: usize,
}

impl ParseAnchor {
    /// The token the cursor stood on when this anchor was taken.
    pub fn token_index(&self) -> usize {
        self.token_index
    }
}

/// Health of an event stream, used by tests to assert that recovery left the node stack balanced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventStreamAudit {
    /// Nodes opened and never closed. Must be zero: anything else means every token after them
    /// ends up in the wrong place.
    pub final_depth: isize,
    /// How far the open-node count fell below the number of nodes created.
    ///
    /// Must be zero. A negative value means a marker was closed twice while another was still
    /// open, which makes the next `NodeEnd` close somebody else's node — a whole subtree gets
    /// re-parented, silently.
    pub min_depth: isize,
    /// Number of zero-width nodes. Expected to be non-zero — `Marker::complete` drops empty nodes
    /// on purpose — but tracked so the count can be asserted to stay stable.
    pub empty_nodes: usize,
    /// Kinds of the `NodeStart` events that never received a matching `NodeEnd`. Must be empty.
    pub unclosed: Vec<crate::kind::CppSyntaxKind>,
}

impl EventStreamAudit {
    pub fn is_balanced(&self) -> bool {
        self.final_depth == 0 && self.min_depth == 0 && self.unclosed.is_empty()
    }
}

/// Which kind of braced body a `{` opened, for [`CppParser::is_at_class_member_level`].
///
/// Two kinds rather than a counter, because the rule that asks needs the *innermost* brace and the two answer
/// differently: `int bits : 3;` is a member only when the class's own body is the one being filled, while
/// everything inside a function body — a `for` header's `:`, a label — is a statement however many class bodies
/// enclose it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BodyKind {
    /// A class-like body: `class`, `struct`, `union`. Its members may declare bit-fields.
    Class,
    /// A statement or declaration block: a function body, a nested `{ … }`, an `extern "C" { … }`.
    Block,
}

pub struct CppParser<'a> {
    text: &'a str,
    events: Vec<MarkEvent>,
    /// **The token stream, immutable once lexed.**
    ///
    /// This was a *zipper* for one revision — the tokens before the cursor in order and the rest reversed — so that
    /// the parser could split a `>>` in place at `O(1)`: `Vec::insert` shifts every token after the cursor, which
    /// made a parse cost `O(k · n)` (2759 ms on the 3.3 MB cooked `<vector>` stream, with the per-byte cost climbing
    /// 244 → 847 µs/KB).
    ///
    /// Nothing splits any more. The `>`-family is broken up **once, at the end of maximal munch**
    /// ([`crate::lexer::CppLexer::tokenize`]), and the grammar puts the pieces back together by byte adjacency
    /// ([`CppParser::joined_operator_at_the_cursor`]). With no insertion left, the zipper was two `Vec`s doing a
    /// plain `Vec`'s job and paying a move per cursor step for it, so it is a plain `Vec` again — which is also the
    /// shape the tree and every consumer of `get`/`len`/`last` already expected.
    tokens: Vec<CppTokenData>,
    /// The cursor, as an absolute index into [`CppParser::tokens`].
    token_index: usize,
    current_token: CppTokenKind,
    /// Event position of every `NodeStart` that has not been closed yet.
    ///
    /// This is the single source of truth for "which nodes are open", and it is what makes error
    /// recovery structural instead of best-effort: a grammar function snapshots
    /// [`CppParser::open_marks`] on entry, and on any early return
    /// [`CppParser::finish_marks_to`] closes exactly the nodes it opened. Without this, a `?`
    /// return leaks an open marker, and because the leaked `NodeStart` sits *before* the ancestor
    /// that later closes, every following token gets swallowed into it. That failure mode is
    /// silent and produces a tree that is still internally consistent — only the shape is wrong.
    open_marks: Vec<usize>,
    /// Event positions that are closed, mapped to whether their `NodeEnd` was emitted.
    ///
    /// Two states have to be distinguished: a node closed normally has its event, while a node
    /// detached by recovery does not — and in the latter case its owner may still reach
    /// `complete()` and owe that event. Collapsing the two into one set is what makes the event
    /// stream go unbalanced in ways that are invisible in the tree.
    closed_marks: std::collections::HashMap<usize, bool>,
    /// The positions in [`CppParser::closed_marks`], **in the order they were first closed**.
    ///
    /// What makes a rollback proportional to the work it undoes instead of to the file. `rollback` has to forget
    /// every marker closed at or past the checkpoint, and it used to do that by *scanning the whole map*
    /// (`closed_marks.retain(...)`) — which is O(nodes so far), once per speculative attempt, and there is a
    /// speculative attempt per declaration. Measured on a rendering of `zmmintrin.h`: 0.29 ms/KB at 30 KB against
    /// 1.17 ms/KB at 244 KB, and a file of repeated `int f ( int a ) ;` grew the same way — the shape of a parse
    /// whose cost is the square of the file.
    ///
    /// The journal's tail past a checkpoint **is** the set to forget: an entry appears here exactly when its
    /// marker is closed, so truncating this list to the checkpoint's length names every position that was closed
    /// since — and removing those from the map is precisely what the scan computed.
    closed_marks_journal: Vec<usize>,
    /// **Whether a `>` at the cursor closes a list or compares** — the state clang keeps in
    /// `Parser::GreaterThanIsOperator`, held here as the one number that decides it.
    ///
    /// The rule is the standard's ([temp.names]): inside a *template-argument-list* the **first non-nested** `>`
    /// is the closing `>` rather than a greater-than operator. **"Non-nested" is the whole of it**, and it is why
    /// "am I inside template arguments" is the wrong question to ask: in
    ///
    /// ```cpp
    /// _Enable_if_bool_convertible<decltype(declval<const _Lhs&>() > declval<const _Rhs&>())>   xutility
    /// bc<(1 > 2)>                                                                              cl.exe: no syntax error
    /// ```
    ///
    /// the `>` is *nested* — it is inside parentheses — so it is a greater-than operator and the argument list runs
    /// on past it. A count of open lists cannot say that; a count of open **brackets** can, and clang's mechanism
    /// is exactly this pair: `BalancedDelimiterTracker` derives from `GreaterThanIsOperatorScope` and its
    /// constructor writes `p.GreaterThanIsOperator = true` for every `(`, `[` and `{`
    /// (`clang/include/clang/Parse/RAIIObjectsForParser.h`), while `ParseTemplateArgumentList` writes `false`.
    ///
    /// So: the bracket depth at which the **innermost** list was entered, and [`CppParser::bracket_depth`] now. A
    /// `>` at the cursor compares exactly when a bracket has been opened since the list began.
    ///
    /// Both numbers are maintained where token consumption is already centralised, so **no grammar rule has to
    /// remember to save and restore anything**: [`CppParser::bump`] moves the depth and [`CppParser::rollback`]
    /// puts both back. That is deliberate — the version before this one kept a single "am I in template arguments"
    /// count that no bracket could suspend, and it reported a syntax error on `bc<(1 > 2)>` where cl.exe reports
    /// none (C2974 is *semantic*: `1 > 2` is not a type).
    innermost_template_argument_list: Option<usize>,
    /// How many bracketed groups — `(` `[` `{` — the cursor is inside, as one number. clang's `ParenCount`,
    /// `BracketCount` and `BraceCount` are the same three, kept apart only because a diagnostic names which one
    /// ran out; nothing here ever needs to.
    ///
    /// Moved by [`CppParser::bump`] **alone**, so it is a function of what has been *consumed* rather than of which
    /// rule happens to be running — which is what makes it survive the grammar's speculative reads. An unbalanced
    /// `)` saturates rather than wrapping.
    bracket_depth: usize,
    /// Is the cursor inside a **constraint** — a requires-clause or a concept's expression?
    ///
    /// One thing changes there, and it is the reason this exists: a `{` after an expression is normally C++11's
    /// list-initialisation of a temporary (`Vec<int>{1, 2}`), but a constraint is followed by the **body** of the
    /// definition it constrains:
    ///
    /// ```text
    /// template <typename T> void f(T t) requires C<T> { }
    ///                                            ^ the constraint ends here, and this opens the body
    /// ```
    ///
    /// So inside a constraint the braced-initialiser reading is refused and the `{` is left for whoever owns it.
    /// Nothing is taken away: a constraint that genuinely wants list-initialisation writes it in parentheses —
    /// `requires (C<T>{})` — and a constraint can never *be* followed by a body it should swallow.
    ///
    /// A `usize` rather than a bool because the contexts nest: a requires-expression inside a constraint holds
    /// requirements, each of which is an expression, and leaving the innermost one must not clear the flag for the
    /// clause around it.
    constraint_depth: usize,
    /// May the declaration being parsed be **named by a bare template-id**?
    ///
    /// Usually not: `C<T> x;` gives the arguments to the *type*, and a declarator whose name was `C<T>` is the
    /// silent wrong tree [`crate::grammar::cpp::types::a_bare_template_id_is_here`] refuses. Three declarations
    /// are the exception, and all three are declarations that must say *which* template they are about:
    ///
    /// ```text
    /// extern template void f<int>(int);        an explicit instantiation
    /// template <> void f<int>(int);            an explicit specialization — the head is empty
    /// template <class T> bool v<T*> = true;    a partial specialization — the head is not empty
    /// ```
    ///
    /// Set by each of those three, and cleared at the start of every declaration, so it cannot outlive the one it
    /// belongs to and let a later declaration read a name it should not.
    a_template_id_may_be_the_name: bool,
    /// Did the declarator parsed most recently declare a function?
    ///
    /// Set by [`crate::grammar::cpp::types::parse_declarator`] and read by
    /// [`crate::grammar::cpp::decls::finish_init_declarator`], which has to decide whether the `{`
    /// at the cursor opens a function body or a brace initializer.
    ///
    /// This is recorded rather than inferred from the event stream because the distinguishing
    /// event can be *dropped*: `void f()` has an empty `ParameterList`, which `Marker::complete`
    /// discards as a zero-width node, leaving the events of `void f() {}` indistinguishable from
    /// those of `int x {}`. Reading it off the parse directly is the only answer that survives that.
    last_declarator_is_function: bool,
    /// The names this translation unit declares to be types.
    ///
    /// C++ settles several parse questions by looking up a name — `Widget w(1, 2);` against `g(1, 2);` — so a
    /// parser without this table has to guess. See [`crate::parser::TypeNames`] for what it records and why
    /// recording too little is the safe direction.
    type_names: crate::parser::TypeNames,
    /// **What [`a_specifier_follows_the_group`](crate::grammar) answered, by the absolute index of the name it was
    /// asked from.** The question is answered by reading ahead through a *run* — `M(a) M(b) M(c) … int x;` asks it of
    /// each `M` in turn, and each answer is the answer for the rest of the run — so without a memory a run of `n`
    /// annotations is `n²/2` walks of the run, and each walk is a chain of lookups: MSVC's `<intrin.h>` writes eight
    /// hundred of them in a row and took two and a half seconds to read. With one, each position is asked once.
    ///
    /// The answer depends only on the tokens ahead, so it is dropped whenever those change: a token split or folded.
    follower_memo: std::cell::RefCell<std::collections::HashMap<usize, bool>>,
    /// The names the **open template heads** declared as parameters that are types.
    ///
    /// The second half of [`CppParser::is_a_known_type_name`], and the construct that made it necessary:
    ///
    /// ```text
    /// template <typename _Tp, size_t _Nm>
    ///   constexpr bool __destructible<_Tp[_Nm]> = true;      <_Tp[_Nm]> is an **array type**
    /// ```
    ///
    /// `_Tp[_Nm]` is a type argument only if `_Tp` is a type, and the tokens cannot say: `S<a[0]>` — a non-type
    /// argument that is a subscript — has exactly the same shape. The file's own table answers such questions,
    /// but a template parameter is *scoped* to the declaration its head introduces, and that table's depth counts
    /// braced bodies rather than template declarations — so recording one there would make the name a type for
    /// the rest of the file, which is the one failure direction `TypeNames` refuses. Hence a list of its own,
    /// appended by each head and cut back when the declaration that owns it ends: see
    /// [`CppParser::truncate_template_parameters`], which `parse_declaration` calls for exactly that reason.
    ///
    /// Measured over the closure of six standard headers: **51** template arguments of the `_Tp[_Size]` / `_Tp[]`
    /// shape, every one of them a type argument, and not one instance of the subscript reading that makes the
    /// question ambiguous.
    template_parameters: Vec<Box<str>>,
    /// The first name written in type position by the declaration being parsed.
    ///
    /// `Widget` for `Widget w(1, 2);`, `None` for `int a(1);` — the keyword is not a name. Recorded while the
    /// specifier sequence is parsed, which is the only place that knows where the type ends and the declarator
    /// begins; a later walk back over the tokens finds the declarator's name instead.
    ///
    /// Read by [`crate::grammar::cpp::decls::a_declaration_is_the_better_reading`].
    declaration_type_name: Option<Box<str>>,
    /// The type name `begin_declaration_type` displaced, kept so a rollback can put it back.
    ///
    /// A single slot rather than a stack: parsers nest one declaration inside another only through a
    /// *speculative* region, and the checkpoint taken at the start of that region carries the value back.
    previous_declaration_type_name: Option<Box<str>>,
    /// Was the declaration's type written as a **qualified** name — `ns::C::method` rather than `method`?
    ///
    /// Read by the declarator for one decision: a qualified name in type position is the head of a definition, so
    /// the parameters after it belong to a function declarator whose name the specifier sequence has already
    /// taken. An unqualified name gets no such reading, which is what keeps `Foo(1, 2);` a call. See
    /// `a_qualified_name_is_the_type` in the type grammar.
    declaration_type_is_qualified: bool,
    /// The braced bodies currently open, **innermost last**, by the kind of brace each one is.
    ///
    /// For the one declaration rule that has to know: a `:` after a member declarator is a **bit-field**'s width,
    /// while after a function declarator it is a constructor's member-initializer list. See
    /// [`CppParser::is_at_class_member_level`].
    ///
    /// A *stack* rather than the counter this used to be, because the question is not "is a class body one of the
    /// enclosing braces" but "is the **innermost** one a class body". `int bits : 3;` is a member only at member
    /// level, and the counter answered yes for a statement inside a member function's body — so
    /// `for (auto &v: vec)` in a header's inline member was read as a bit-field of width `vec`, and a label
    /// (`again:`) was read as one too.
    open_bodies: Vec<BodyKind>,
    /// Did the specifier sequence just parsed finish a whole declaration, `;` and all?
    ///
    /// Exactly one specifier does: `friend`, whose payload *is* the declaration that follows it. The flag lets
    /// `parse_declaration` tell "the statement is already over" from "the specifiers were followed by nothing",
    /// which the cursor alone cannot say. Cleared when a declaration begins, so it always describes the
    /// sequence just parsed.
    declaration_ended_inside_specifiers: bool,
    /// Did a rule just consume the `;` that ends the declaration, because it stood **inside one branch of a
    /// conditional**?
    ///
    /// The shape is a declaration whose *tail* is written once per branch, so each branch writes the terminator
    /// itself:
    ///
    /// ```cpp
    ///     random_shuffle(_RAIter, _RAIter,                       // parallel/algorithmfwd.h:700
    /// #if __cplusplus >= 201103L
    ///            _RandomNumberGenerator&&);
    /// #else
    ///            _RandomNumberGenerator&);
    /// #endif
    /// ```
    ///
    /// The parameter list reads the second branch's tail as well (they are alternatives of the same list), and
    /// with it the branch's own `;` — so the declaration, which would otherwise ask for one and find `#endif`,
    /// has to be told. **A take**, like [`CppParser::take_declaration_ended_inside_specifiers`], because the
    /// answer describes the construct just parsed and must not leak into the next declaration.
    ///
    /// **Part of [`Checkpoint`]**, and that is the difference between this flag and
    /// [`CppParser::open_bodies`], which is deliberately not: a speculative region *does* run through the
    /// readers that set this one — `parse_declaration` is tried and rewound at every statement that might be an
    /// expression, and the parameter list inside it sets the flag on the way. Left out of the checkpoint, a
    /// rewound attempt would leave it set and the next declaration would skip its own `;`.
    terminator_came_from_a_branch: bool,
    /// How many conditionals the directives read so far have left open — see [`CppParser::open_conditionals`].
    ///
    /// Not part of [`Checkpoint`], and that is deliberate: unlike the flag above, this one describes *where in the
    /// file* the cursor is rather than what a construct decided, and a rollback rewinds the cursor through the
    /// same directives the count was taken over. Restoring a stale count would make it disagree with the tokens.
    open_conditionals: isize,
    pub parse_config: ParserConfig<'a>,
    pub(crate) errors: &'a mut Vec<CppParseError>,
}

impl MarkerEventContainer for CppParser<'_> {
    fn get_mark_level(&self) -> usize {
        self.open_marks.len()
    }

    fn push_mark(&mut self, position: usize) {
        self.open_marks.push(position);
    }

    fn drain_marks(&mut self, target: usize) -> Vec<usize> {
        self.open_marks.split_off(target)
    }

    fn close_mark(&mut self, position: usize, want_event: bool) -> bool {
        // Removing the mark from the open set and emitting the event happen together, so the two
        // can never disagree about whether a node is closed.
        self.open_marks.retain(|open| *open != position);

        match self.closed_marks.entry(position) {
            std::collections::hash_map::Entry::Occupied(_) => false,
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(want_event);
                // …and the journal, so that a rollback can undo exactly this insertion without scanning the map.
                self.closed_marks_journal.push(position);
                if want_event {
                    self.events.push(MarkEvent::NodeEnd);
                }
                want_event
            }
        }
    }

    fn mark_has_end_event(&self, position: usize) -> bool {
        self.closed_marks.get(&position).copied().unwrap_or(false)
    }

    fn mark_was_detached(&self, position: usize) -> bool {
        matches!(self.closed_marks.get(&position), Some(false))
    }

    fn mark_is_open(&self, position: usize) -> bool {
        self.open_marks.contains(&position)
    }

    fn get_events(&mut self) -> &mut Vec<MarkEvent> {
        &mut self.events
    }
}

impl<'a> CppParser<'a> {
    /// Parse `text` into a lossless syntax tree.
    ///
    /// This never fails: every input file, however broken or mid-edit, produces a tree covering
    /// all of `text` (invariant I1). Problems are reported through [`CppSyntaxTree::get_errors`]
    /// and through `ErrorNode`/`MissingNode` nodes, not through a `Result`.
    pub fn parse(text: &'a str, config: ParserConfig<'a>) -> CppSyntaxTree {
        Self::parse_inner(text, config).0
    }

    /// Like [`CppParser::parse`], but also reports the raw event stream's balance.
    ///
    /// Tests use this because the tree alone cannot reveal a recovery bug: an unclosed `NodeStart`
    /// still yields a well-formed tree, just one where a subtree swallowed its following siblings.
    pub fn parse_with_audit(
        text: &'a str,
        config: ParserConfig<'a>,
    ) -> (CppSyntaxTree, EventStreamAudit) {
        Self::parse_inner(text, config)
    }

    /// A parser over `text`, reporting problems into `errors`.
    ///
    /// **The one place the struct is built.** Both entry points below used to spell the twenty fields out, which
    /// is twenty chances for the two to drift — and a field that one of them forgot would be a parser that behaves
    /// differently depending on which door it was entered through. It is also what makes the rollback contract
    /// testable from inside the crate: a test can hold both halves, which the borrow checker forbids to a caller
    /// who only has [`CppParser::parse`].
    fn with_text(
        text: &'a str,
        config: ParserConfig<'a>,
        errors: &'a mut Vec<CppParseError>,
    ) -> CppParser<'a> {
        let (tokens, lexical_errors) = crate::lexer::lex(text, &config.lexer_config());
        errors.extend(lexical_errors);

        CppParser {
            text,
            events: Vec::new(),
            tokens,
            token_index: 0,
            current_token: CppTokenKind::None,
            open_marks: Vec::new(),
            closed_marks: std::collections::HashMap::new(),
            closed_marks_journal: Vec::new(),
            innermost_template_argument_list: None,
            bracket_depth: 0,
            constraint_depth: 0,
            a_template_id_may_be_the_name: false,
            last_declarator_is_function: false,
            type_names: TypeNames::new(),
            template_parameters: Vec::new(),
            follower_memo: std::cell::RefCell::new(std::collections::HashMap::new()),
            declaration_type_name: None,
            previous_declaration_type_name: None,
            declaration_type_is_qualified: false,
            open_bodies: Vec::new(),
            declaration_ended_inside_specifiers: false,
            terminator_came_from_a_branch: false,
            open_conditionals: 0,
            parse_config: config,
            errors,
        }
    }

    /// Like [`CppParser::parse`], but also returns the raw event stream.
    ///
    /// The event stream is the parser's real output and the tree is a fold of it, so when a tree is
    /// wrongly nested the stream is where the cause is visible. Kept public because diagnosing a
    /// shape bug otherwise means adding a temporary `println!` inside the parser.
    pub fn parse_with_events(
        text: &'a str,
        config: ParserConfig<'a>,
    ) -> (CppSyntaxTree, Vec<MarkEvent>) {
        let mut errors: Vec<CppParseError> = Vec::new();
        let mut parser = CppParser::with_text(text, config, &mut errors);

        parse_cpp_unit(&mut parser);

        let events = std::mem::take(&mut parser.events);
        let root = {
            let mut builder = crate::syntax::CppTreeBuilder::new(text, events.clone(), None);
            builder.build();
            builder.finish()
        };

        let tokens = std::mem::take(&mut parser.tokens);
        let tree = CppSyntaxTree::new(root, errors, tokens);
        (tree, events)
    }

    fn parse_inner(text: &'a str, config: ParserConfig<'a>) -> (CppSyntaxTree, EventStreamAudit) {
        let mut errors: Vec<CppParseError> = Vec::new();
        let mut parser = CppParser::with_text(text, config, &mut errors);

        parse_cpp_unit(&mut parser);

        let audit = parser.audit_events();

        debug_assert!(
            parser.open_marks.is_empty(),
            "the grammar leaked {} unclosed node(s)",
            parser.open_marks.len()
        );

        let root = {
            let mut builder = CppTreeBuilder::new(
                parser.text,
                std::mem::take(&mut parser.events),
                parser.parse_config.node_cache(),
            );
            builder.build();
            builder.finish()
        };

        let tokens = std::mem::take(&mut parser.tokens);
        (CppSyntaxTree::new(root, errors, tokens), audit)
    }

    /// Position the cursor on the first non-trivia token, emitting the leading trivia as events.
    ///
    /// Emitting the leading trivia matters: without it the whitespace and comments before the
    /// first real token would never be attached to the tree and the CST would silently stop being
    /// lossless.
    pub fn init(&mut self) {
        let mut next_index = self.token_index;
        self.skip_trivia(&mut next_index);
        // Leading trivia: everything before the first real token.
        self.parse_trivia_tokens(0, next_index);
        self.move_the_cursor_to(next_index);

        self.current_token = self
            .tokens
            .get(self.token_index)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::Eof);
    }

    pub fn is_eof(&self) -> bool {
        self.current_token == CppTokenKind::Eof
    }

    pub fn origin_text(&self) -> &'a str {
        self.text
    }

    /// How many tokens the parser was handed, trivia included.
    ///
    /// The bound for a scan that walks the tokens ahead of the cursor by index rather than by
    /// `peek_token_kind_at`, which counts *significant* tokens and so cannot step past an unknown
    /// amount of trivia one position at a time.
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    pub fn current_token(&self) -> CppTokenKind {
        self.current_token
    }

    pub fn current_token_index(&self) -> usize {
        self.token_index
    }

    /// Mark the current position for a later question about what has been parsed **here**.
    ///
    /// Two positions describe a moment in a parse and they are not interchangeable: [`CppParser::open_marks`] is
    /// a length of the marker stack, for closing the nodes opened since, while [`Checkpoint`] is where the event
    /// stream and cursor stood. Neither can answer a question about *tokens already consumed* — a marker records
    /// an event index and the marker stack is not one — and passing the wrong one is a silent mistake rather than
    /// a loud one, because all three are small integers. That is what this type exists to prevent.
    pub fn anchor(&self) -> ParseAnchor {
        ParseAnchor {
            token_index: self.token_index,
        }
    }

    pub fn current_token_range(&self) -> SourceRange {
        if self.token_index >= self.tokens.len() {
            if self.tokens.is_empty() {
                return SourceRange::EMPTY;
            } else {
                return self.tokens[self.tokens.len() - 1].range;
            }
        }

        self.tokens[self.token_index].range
    }

    pub fn current_token_text(&self) -> &str {
        match self.tokens.get(self.token_index) {
            Some(token) => &self.text[token.range.start_offset..token.range.end_offset()],
            // Cursor is past the end of the token stream: the previous token owns the tail.
            None => match self.tokens.last() {
                Some(token) => &self.text[token.range.start_offset..token.range.end_offset()],
                None => "",
            },
        }
    }

    /// Record a checkpoint that [`CppParser::rollback`] can restore.
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            events_len: self.events.len(),
            token_index: self.token_index,
            open_marks: self.open_marks.len(),
            declaration_type_name: self.declaration_type_name.clone(),
            previous_declaration_type_name: self.previous_declaration_type_name.clone(),
            declaration_type_is_qualified: self.declaration_type_is_qualified,
            errors_len: self.errors.len(),
            terminator_came_from_a_branch: self.terminator_came_from_a_branch,
            closed_marks_journal_len: self.closed_marks_journal.len(),
            bracket_depth: self.bracket_depth,
            innermost_template_argument_list: self.innermost_template_argument_list,
        }
    }

    /// Rewind to `checkpoint`, discarding every event, token **and diagnostic** produced since.
    ///
    /// Any markers opened after the checkpoint are dropped along with their events, so callers
    /// must not hold on to a `Marker` created inside a speculative region.
    ///
    /// # Why the diagnostics go too
    ///
    /// A rollback is always "that reading did not fit, try another" — `a.b < c > d` against `a.b<c> d`, a cast
    /// against a parenthesised expression, an init-declarator against an expression statement. Whatever the
    /// discarded reading complained about is not a fact about the file: it is a fact about a guess. Keeping it
    /// reports code the user did not write, which is worse than saying nothing — the file that comes out is valid
    /// C++ and the editor would underline it. The reading that *is* kept reports its own problems normally.
    pub fn rollback(&mut self, checkpoint: Checkpoint) {
        self.events.truncate(checkpoint.events_len);
        self.open_marks.truncate(checkpoint.open_marks);
        // Positions at or past the truncation point are gone from the event stream, so their
        // "already closed" bookkeeping must go too — otherwise a future marker reusing the same
        // position would be considered closed and its `NodeEnd` silently skipped.
        for position in self
            .closed_marks_journal
            .split_off(checkpoint.closed_marks_journal_len)
        {
            self.closed_marks.remove(&position);
        }
        self.declaration_type_name = checkpoint.declaration_type_name;
        self.previous_declaration_type_name = checkpoint.previous_declaration_type_name;
        self.declaration_type_is_qualified = checkpoint.declaration_type_is_qualified;
        self.errors.truncate(checkpoint.errors_len);
        self.terminator_came_from_a_branch = checkpoint.terminator_came_from_a_branch;
        self.bracket_depth = checkpoint.bracket_depth;
        self.innermost_template_argument_list = checkpoint.innermost_template_argument_list;
        self.move_the_cursor_to(checkpoint.token_index);
        self.current_token = self
            .tokens
            .get(self.token_index)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::Eof);
    }

    /// Run `f` speculatively: if it returns `None`, everything it consumed is rolled back.
    ///
    /// This is the primitive that makes C++'s declaration/expression ambiguity tractable without
    /// a symbol table.
    pub fn try_parse<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        let checkpoint = self.checkpoint();
        match f(self) {
            Some(value) => Some(value),
            None => {
                self.rollback(checkpoint);
                None
            }
        }
    }


    /// Try to consume a header name at the cursor, folding the `<`, name and `>` tokens the ordinary
    /// sweep produced into a single [`CppTokenKind::HeaderName`].
    ///
    /// Only the parser knows when a header name is expected (`#include` and friends), and only the
    /// parser can rewrite the token stream, so the two halves of header-name lexing live on either
    /// side of this call. Returns whether a header name was consumed; on `false` the caller should
    /// lex the token normally.
    pub fn try_lex_header_name(&mut self) -> bool {
        let Some(start) = self.tokens.get(self.token_index) else {
            return false;
        };

        // A quoted header name survives the ordinary sweep as a string literal, because the lexer
        // cannot tell it apart from a string at the time. `"local.h"` is already the right shape, so
        // this case only has to relabel it.
        if start.kind == CppTokenKind::StringLiteral {
            let text = &self.text[start.range.start_offset..start.range.end_offset()];
            // A header name has no escapes and no concatenation; anything else is a real string.
            if !text.contains('\\') {
                let token = self.tokens[self.token_index];
                self.follower_memo.get_mut().clear();
                self.tokens[self.token_index] =
                    CppTokenData::new(CppTokenKind::HeaderName, token.range);
                self.current_token = CppTokenKind::HeaderName;
                self.bump();
                return true;
            }
            return false;
        }

        if start.kind != CppTokenKind::Less {
            return false;
        }

        // Angle form: scan forward for the closing `>` at the same "line".
        let mut end_index = self.token_index + 1;
        let mut consumed = 0usize;
        loop {
            let Some(token) = self.tokens.get(end_index) else {
                return false;
            };

            match token.kind {
                CppTokenKind::Greater => break,
                // What ends the scan is only what ends the **line**. A header name runs to the closing `>` and
                // may contain anything else — the standard makes an h-char any character except newline and
                // `>` — so this is a list of what *cannot* be inside one rather than of what can.
                //
                // It was a whitelist, and the whitelist was missing `++`: `#include <bits/c++config.h>` did not
                // fold, which is the most common include in libstdc++ and therefore in every standard header
                // there is. The cost was not a worse tree but a *wrong target*: the analysis layer's fallback for
                // an unfolded angle include reconstructed the name from these tokens, and every file that
                // included one then resolved its includes to nothing and was never cached. See
                // A whitelist here is a rule about the lexer's token kinds pretending to
                // be a rule about header names.
                CppTokenKind::Newline | CppTokenKind::LineContinuation | CppTokenKind::Eof => return false,
                _ => {}
            }

            consumed += 1;
            end_index += 1;
        }

        if consumed == 0 {
            return false;
        }

        let close = self.tokens[end_index];
        let whole = CppTokenData::new(
            CppTokenKind::HeaderName,
            crate::text::SourceRange::new(
                start.range.start_offset,
                close.range.end_offset() - start.range.start_offset,
            ),
        );

        // Replace the whole run with the single header-name token and advance past it.
        self.follower_memo.get_mut().clear();
        self.tokens
            .splice(self.token_index..self.token_index + (end_index - self.token_index + 1), [whole]);
        self.current_token = CppTokenKind::HeaderName;
        self.bump();
        true
    }

    /// Does any node opened at or after `from_event` have one of these kinds?
    ///
    /// Used by the grammar to answer "what did I just parse?" without threading a return value
    /// through every level. `from_event` is an event index from
    /// [`CppParser::current_event_count`].
    pub fn events_contain_any(
        &self,
        from_event: usize,
        kinds: &[crate::kind::CppSyntaxKind],
    ) -> bool {
        self.events[from_event.min(self.events.len())..]
            .iter()
            .any(|event| {
                matches!(
                    event,
                    MarkEvent::NodeStart { kind, .. } if kinds.contains(kind)
                )
            })
    }

    /// **Move the cursor to `index`**, keeping [`CppParser::token_index`] and [`Tokens`]'s own boundary in step.
    ///
    /// **The only writer of the field.** The two have to agree or the split would rewrite the wrong token:
    /// `token_index` is what the grammar reads, while `Tokens::set_at_cursor` and `Tokens::insert_at_cursor` write
    /// through the zipper's boundary — so a site that moved one without the other would be a silent corruption
    /// rather than a crash. A method is what makes that impossible rather than merely unlikely, and it is why the
    /// five places that used to assign the field directly call this instead.
    ///
    /// `seek` costs `O(distance)` — the tokens crossed move one at a time between the halves — which is the order
    /// the cursor's own advance already had. The difference from the `Vec::insert` it replaced is *what* the
    /// distance is proportional to: one token, not every token remaining in the file.
    fn move_the_cursor_to(&mut self, index: usize) {
        self.token_index = index;
    }

    /// **The operator at the cursor**, with the `>`-family spellings the lexer broke up put back together.
    ///
    /// Returns the kind and **how many tokens it spans**: `1` for everything but `>>` (2), `>=` (2) and `>>=`
    /// (3), which the lexer splits at the end of maximal munch — see [`crate::lexer::CppLexer::tokenize`] for why
    /// the split is there. A caller that has taken the operator must advance the cursor by that many tokens.
    ///
    /// # Two questions, two answers, and neither knows the other's spelling
    ///
    /// A caller that wants an **operator** asks this. A caller that wants a `>` as a **bracket** — the
    /// template-argument reader closing its list — asks [`CppParser::current_token`] and gets a lone `>`, because
    /// the lexer left it that way. That separation is the whole gain: the version before this handed out
    /// `RightShift` and left three angle scanners to remember that one token was two brackets, and left
    /// `split_closing_angle` to take it apart again.
    ///
    /// It also settles a case the old spelling could not: **`C<D>= 3>`**. There the `>` closes the list — the
    /// template reader asks the bracket question, takes one token, and never joins — while in `a >= b` the
    /// expression loop asks the operator question and gets `GreaterEqual`. The context decides, which is what
    /// g++'s message ("'`>=`' should be '`> =`' to terminate a template argument list") is describing.
    ///
    /// Adjacency is by **byte offset**, not by position: `a > > b` is two greater-thans and only the characters
    /// being adjacent makes it a shift. Same test the lexer would have made, made here instead of baked in.
    pub fn joined_operator_at_the_cursor(&self) -> (CppTokenKind, usize) {
        let Some(first) = self.tokens.get(self.token_index) else {
            return (self.current_token, 1);
        };
        if first.kind != CppTokenKind::Greater {
            return (first.kind, 1);
        }

        let touches = |left: &CppTokenData, right: &CppTokenData| {
            left.range.end_offset() == right.range.start_offset
        };
        let second = self.tokens.get(self.token_index + 1);
        let third = self.tokens.get(self.token_index + 2);

        // `>>=` — the longest first, so `>` `>` `=` is not taken for a shift and a stray `=`.
        if let (Some(second), Some(third)) = (second, third)
            && second.kind == CppTokenKind::Greater
            && third.kind == CppTokenKind::Assign
            && touches(first, second)
            && touches(second, third)
        {
            return (CppTokenKind::RightShiftAssign, 3);
        }

        if let Some(second) = second
            && touches(first, second)
        {
            match second.kind {
                CppTokenKind::Greater => return (CppTokenKind::RightShift, 2),
                CppTokenKind::Assign => return (CppTokenKind::GreaterEqual, 2),
                _ => {}
            }
        }

        (CppTokenKind::Greater, 1)
    }

    /// Number of events recorded so far, for use as a `from_event` bound.
    pub fn current_event_count(&self) -> usize {
        self.events.len()
    }

    /// How many `{` tokens were consumed at or after `from_event` without a `}` to match them?
    ///
    /// The question a **recovery** has to ask about a construct it gave up on. Closing the markers of a failed
    /// sub-parse ([`MarkerEventContainer::finish_marks_to`]) keeps the abandonment from swallowing what follows,
    /// but it cannot un-consume the *tokens*: a member that failed after eating a `{` — a requires-expression's
    /// body, a function body, a block — leaves a brace behind that the enclosing body then spends its own `}` on.
    /// The class body ends early, every member after it is read at file scope, and the only diagnostic in the file
    /// lands on the leftover brace at the end (which is exactly what `bits/alloc_traits.h` did before the brace
    /// debt was paid).
    ///
    /// Counted from the **events**, not the token stream: a rollback truncates the events, so a token read, thrown
    /// away and read again counts once — the same reason [`CppParser::events_contain_any`] reads them.
    pub fn brace_balance_since(&self, from_event: usize) -> isize {
        let mut balance = 0isize;

        for event in self.events[from_event.min(self.events.len())..].iter() {
            if let MarkEvent::EatToken { kind, .. } = event {
                match kind {
                    CppTokenKind::LeftBrace => balance += 1,
                    CppTokenKind::RightBrace => balance -= 1,
                    _ => {}
                }
            }
        }

        balance
    }

    /// Has a node of one of these kinds been opened between `from_event` and the cursor?
    ///
    /// The counterpart to [`CppParser::events_contain_any`], which asks about everything from `from_event`
    /// *onwards*. This one is bounded on both sides, which is what lets a rule ask a question about the
    /// declaration it is in the middle of: "has this declarator named anything yet?" is answered by a
    /// [`CppSyntaxKind::NameExpr`] between where the declarator started and where the cursor stands, and the
    /// lower bound is what keeps the *type*'s own name — a `NameExpr` inside the specifier sequence, recorded
    /// before that point — from being mistaken for the declarator's.
    ///
    /// This matters because the two questions are asked by the same function for two different statements:
    /// `Widget w(1, 2, 3);` names `w` and must be a declaration, while `g(1, 2, 3);` names nothing and must
    /// stay a call. Both have a name in type position; only the first has one after it.
    pub fn events_contain_any_between(
        &self,
        from_event: usize,
        to_event: usize,
        kinds: &[crate::kind::CppSyntaxKind],
    ) -> bool {
        let from = from_event.min(self.events.len());
        let to = to_event.min(self.events.len());

        self.events[from..to].iter().any(|event| {
            matches!(
                event,
                MarkEvent::NodeStart { kind, .. } if kinds.contains(kind)
            )
        })
    }

    /// **Does a `>` at the cursor compare, or does it close the list the cursor is in?**
    ///
    /// The one question the expression grammar asks about template arguments, and the answer is the standard's:
    /// a `>` closes the list only when it is **not nested** in a bracketed group opened since that list began.
    /// See [`CppParser::innermost_template_argument_list`] for the mechanism and the input it was measured on.
    ///
    /// Outside every list the answer is `true`, which is what keeps `a > b` an ordinary comparison everywhere else
    /// in the file. `>=` and `>>=` are never this question's business — they are different tokens and stay
    /// operators either way — and `>>` is, which is why `a >> b` inside `C<(a >> b)>` reads as a shift.
    pub fn greater_than_is_an_operator(&self) -> bool {
        match self.innermost_template_argument_list {
            None => true,
            Some(entered_at) => self.bracket_depth > entered_at,
        }
    }

    /// Which compiler's own reserved spellings mean what — see [`Dialect`].
    ///
    /// Read by the grammar in exactly one place (`a_type_the_compiler_spells`), which is the point: the question
    /// this answers is not "how is this file written" but "what does the compiler reading it mean by
    /// `__int128`", and only the caller knows which compiler that is.
    pub fn dialect(&self) -> Dialect {
        self.parse_config.dialect
    }

    /// Enter a template argument list, recording the bracket depth it began at.
    ///
    /// Returns what was in force before, for the caller to hand back to [`CppParser::leave_template_arguments`].
    /// `Option<usize>` rather than `usize` because the two states are **not** the same question: `None` is "no list
    /// is open, so every `>` compares", while `Some(0)` is "a list began at the outermost bracket depth". The
    /// version before this one spelled both as the number zero, which is how `C<(1 > 2)>` came to be read as a
    /// mis-nested list rather than as a comparison inside parentheses.
    pub fn enter_template_arguments(&mut self) -> Option<usize> {
        let previous = self.innermost_template_argument_list;
        self.innermost_template_argument_list = Some(self.bracket_depth);
        previous
    }

    /// Leave a template argument list, restoring what [`CppParser::enter_template_arguments`] returned.
    pub fn leave_template_arguments(&mut self, previous: Option<usize>) {
        self.innermost_template_argument_list = previous;
    }

    /// Is the cursor inside a **constraint**, where a `{` is not a braced initialiser?
    ///
    /// See the field's documentation. Read by the postfix rule, which is the one that would otherwise take the
    /// `{` after a requires-clause for list-initialisation and swallow the body of the definition.
    pub fn is_in_a_constraint(&self) -> bool {
        self.constraint_depth > 0
    }

    /// Enter a constraint. Returns the previous depth so the caller can restore it.
    pub fn enter_constraint(&mut self) -> usize {
        let previous = self.constraint_depth;
        self.constraint_depth += 1;
        previous
    }

    /// Leave a constraint, restoring the depth `enter_constraint` returned.
    pub fn leave_constraint(&mut self, previous: usize) {
        self.constraint_depth = previous;
    }

    /// May the declaration being parsed be named by a bare template-id? See the field's documentation.
    ///
    /// Read by the declarator rule, which is the one that would otherwise refuse the template-id that names an
    /// instantiation or a specialization.
    pub fn a_template_id_may_be_the_name(&self) -> bool {
        self.a_template_id_may_be_the_name
    }

    /// Record whether this declaration may be named by a bare template-id, handing back the previous answer.
    pub fn set_a_template_id_may_be_the_name(&mut self, value: bool) -> bool {
        std::mem::replace(&mut self.a_template_id_may_be_the_name, value)
    }

    /// Record whether the declarator just parsed declared a function. See the field's docs.
    pub fn set_last_declarator_is_function(&mut self, is_function: bool) {
        self.last_declarator_is_function = is_function;
    }

    /// Did the declarator parsed most recently declare a function?
    pub fn last_declarator_is_function(&self) -> bool {
        self.last_declarator_is_function
    }

    /// Record that `name` was declared to be a type, at the current scope depth.
    ///
    /// Called by the grammar where a name sits in type position: `class Widget`, `typedef ... Integer`,
    /// `using Alias = ...`. See `TypeNames` for why the parser needs this at all.
    pub fn declare_type_name(&mut self, name: &str) {
        self.type_names.declare(name);
    }

    /// Is `name` a type as far as this file's own declarations say?
    ///
    /// The question that separates `Widget w(1, 2);` from `g(1, 2);`. A `false` is not "not a type" but "this
    /// file does not say it is one" — a type from an included header, a template parameter, or a builtin this
    /// table never saw. Callers must therefore treat it as one signal among several rather than as the answer.
    ///
    /// A **template parameter** counts, and the two sources are kept apart because they are scoped differently:
    /// see [`CppParser::template_parameters`].
    pub fn is_a_known_type_name(&self, name: &str) -> bool {
        self.type_names.is_a_type(name)
            || self
                .template_parameters
                .iter()
                .any(|parameter| &**parameter == name)
    }

    /// Record a template parameter that declares a **type** — `typename T`, `class C`.
    ///
    /// Called by the template parameter rule, which is the only place that knows which spelling of a parameter
    /// this is: `int N` and `auto N` declare *values*, and `N[4]` is not an array of anything.
    pub fn declare_template_parameter(&mut self, name: &str) {
        self.template_parameters.push(Box::from(name));
    }

    /// How many template parameters are in scope, so that [`CppParser::truncate_template_parameters`] can cut
    /// the list back to a caller's own state.
    pub fn template_parameter_count(&self) -> usize {
        self.template_parameters.len()
    }

    /// Forget every template parameter recorded since `count` names were in scope.
    ///
    /// The list is appended as heads are read and cut back by the **declaration** that owns them, which is what
    /// keeps a parameter from being a type name for the rest of the file. Nested declarations save and restore
    /// around themselves, so a class template's members still see the class's parameters: the length the member's
    /// own `parse_declaration` saved already includes them.
    pub fn truncate_template_parameters(&mut self, count: usize) {
        self.template_parameters.truncate(count);
    }

    /// Enter a braced body, for the table's scope approximation.
    pub fn enter_type_name_scope(&mut self) {
        self.type_names.enter_scope();
    }

    /// Leave a braced body.
    pub fn leave_type_name_scope(&mut self) {
        self.type_names.leave_scope();
    }

    /// The kind of the token the parser consumed most recently, skipping the trivia after it.
    ///
    /// The answer to "what did the rule that just ran end on?", which the cursor cannot give: after a `bump` the
    /// cursor is on the *next* token, so a rule that has to describe its own last token has to walk back. Used by
    /// the specifier sequence, which asks the question of the specifier it just consumed.
    ///
    /// `None` at the start of the file, where nothing has been consumed.
    pub fn last_consumed_token_kind(&self) -> Option<CppTokenKind> {
        let mut index = self.token_index;
        while index > 0 {
            index -= 1;
            let kind = self.tokens.get(index).map(|token| token.kind)?;
            if !is_trivia_kind(kind) {
                return Some(kind);
            }
        }
        None
    }

    /// The text of the token at `index`, or empty when there is none.
    ///
    /// The companion to [`CppParser::token_kind_at`], for the walks that go *backwards* over the tokens already
    /// consumed — which is how the grammar asks what a declaration began with. `peek_token_text_at` cannot
    /// answer that: it walks forward from the cursor.
    pub fn token_text_at(&self, index: usize) -> &str {
        self.tokens
            .get(index)
            .map(|token| {
                let range = token.range;
                &self.origin_text()[range.start_offset..range.end_offset()]
            })
            .unwrap_or("")
    }

    /// The unqualified name the declarator that began at `from_event` introduced, if it named anything.
    ///
    /// # Why the event stream and not a return value
    ///
    /// A declarator's name is not always where a rule can see it: `typedef WCHAR *PWCHAR;` has the name after a
    /// `*`, `typedef void (*F)(int);` has it inside parentheses, and `ns::Widget *w` has a qualified spelling
    /// whose *last* component is the name. Threading a `Option<String>` back through every return of
    /// `parse_declarator` and its six helpers would touch rules that have no interest in the answer, and the
    /// grammar already answers "what did I just parse?" from the events in
    /// [`CppParser::events_contain_any_between`]. This is the same question with the text attached.
    ///
    /// # How the name is picked
    ///
    /// The **first `NameExpr` at the declarator's own depth**, and then the last identifier inside it. Both halves
    /// are load-bearing:
    ///
    /// ```text
    /// typedef WCHAR *PWCHAR;        the NameExpr is one level in: `PWCHAR`
    /// typedef void (*F)(int);       the declarator's is two levels in: `F`
    /// typedef void (*F)(Widget w);  …and the *parameter's* NameExpr is deeper still, so `w` cannot win
    /// ns::Widget *w;                the NameExpr holds `ns :: Widget`, whose last identifier is the name
    /// ```
    ///
    /// A declarator that names nothing — an abstract declarator in a type-id, a structured binding — answers
    /// `None`, which is the honest answer and the one the callers act on.
    pub fn the_name_a_declarator_introduced(&self, from_event: usize) -> Option<String> {
        let events = self.events.get(from_event.min(self.events.len())..)?;

        let mut depth = 0isize;
        // Where the declarator's own name sits once it has been found: `None` until the first `NameExpr`.
        let mut name_depth: Option<isize> = None;
        let mut spelled = String::new();

        for event in events {
            match event {
                MarkEvent::NodeStart { kind, .. } => {
                    if name_depth.is_none() && *kind == CppSyntaxKind::NameExpr {
                        name_depth = Some(depth);
                    }
                    depth += 1;
                }
                MarkEvent::NodeEnd => depth -= 1,
                MarkEvent::EatToken { kind, range } => {
                    // Only the tokens *directly* inside that `NameExpr`: a nested one — a default argument, a
                    // parameter's own name — is deeper and must not overwrite the answer.
                    if name_depth == Some(depth - 1) && *kind == CppTokenKind::Identifier {
                        spelled = self.origin_text()[range.start_offset..range.end_offset()].to_string();
                    }
                }
                MarkEvent::Trivia => {}
            }
        }

        (!spelled.is_empty()).then_some(spelled)
    }

    /// The table itself, for a consumer that wants to audit what the parse recorded.
    pub fn type_names(&self) -> &crate::parser::TypeNames {
        &self.type_names
    }

    /// What was remembered for [`CppParser::follower_memo`], if anything.
    pub fn remembered_follower(&self, index: usize) -> Option<bool> {
        self.follower_memo.borrow().get(&index).copied()
    }

    /// Remember an answer for [`CppParser::follower_memo`].
    pub fn remember_follower(&self, index: usize, answer: bool) {
        self.follower_memo.borrow_mut().insert(index, answer);
    }

    /// The source range of the token at `index`, or `None` when there is none.
    ///
    /// Beside [`CppParser::token_text_at`] for the rules that ask the **evidence** about a token that is not at the
    /// cursor: a macro's body is in force from an offset, so "what does this name stand for here" needs to know
    /// where *here* is.
    pub fn token_range_at(&self, index: usize) -> Option<SourceRange> {
        self.tokens.get(index).map(|token| token.range)
    }

    /// What the caller's table says about `name`, if the caller supplied one.
    ///
    /// `None` is "no evidence from outside" — never "not a type". See [`crate::symbols`].
    pub fn symbol_kind(&self, name: &str) -> Option<SymbolKind> {
        self.parse_config.symbol_table()?.kind_of(name)
    }

    /// Start recording the declaration's leading type name; see the field's documentation.
    ///
    /// The name currently in force is parked rather than dropped, because a speculative region may
    /// run this and then be rolled back — see [`Checkpoint`].
    pub fn begin_declaration_type(&mut self) {
        self.previous_declaration_type_name = self.declaration_type_name.take();
        self.declaration_type_is_qualified = false;
        self.declaration_ended_inside_specifiers = false;
    }

    /// Record a name seen in type position, if none has been recorded for this declaration yet.
    ///
    /// Only the first: `unsigned int` has two type words and the declaration's type is the sequence, not either
    /// word — but what the reader needs is only "is the leading name a type this file declared", and the first
    /// name is the one a qualifier would precede.
    pub fn note_declaration_type_name(&mut self, name: String) {
        if self.declaration_type_name.is_none() && !name.is_empty() {
            self.declaration_type_name = Some(name.into_boxed_str());
        }
    }

    /// Record that the declaration's type was written as a qualified name.
    ///
    /// Called by the specifier sequence as it walks a name's `::`-separated segments. Read once, by the
    /// declarator, to tell `void ns::C::method()` — where the last segment *is* the function being defined —
    /// from `Foo(1, 2);`, where the name is a callee and the parentheses are an argument list.
    pub fn note_qualified_declaration_type(&mut self) {
        self.declaration_type_is_qualified = true;
    }

    /// Was the declaration's type written as a qualified name? See the field's documentation.
    pub fn has_qualified_declaration_type_name(&self) -> bool {
        self.declaration_type_is_qualified
    }

    /// The recorded declaration type name, **owned**, for a caller that has to hold it across a nested parse.
    ///
    /// The pair to [`CppParser::set_declaration_type_record`]; see that function for what is being saved and
    /// why the saving happens at the call site rather than inside the specifier sequence.
    pub fn declaration_type_record(&self) -> (Option<String>, bool) {
        (
            self.declaration_type_name
                .as_ref()
                .map(|name| name.to_string()),
            self.declaration_type_is_qualified,
        )
    }

    /// Put back a record saved by [`CppParser::declaration_type_record`].
    ///
    /// **A nested parse must not be allowed to answer for the declaration that contains it.** A template
    /// argument is read with [`crate::grammar::cpp::types::parse_type_id`], a type-id opens a specifier
    /// sequence, and that sequence begins by calling [`CppParser::begin_declaration_type`] — which parks the
    /// enclosing name and clears the field. Nothing restored it, so `G<double> d(…)` lost the `"G"` as soon as
    /// its argument was read, and the declarator afterwards found whatever the *argument* had left: `"S"` for
    /// `G<S>`, and nothing at all for `G<double>`, whose argument is a builtin keyword that no name-recording
    /// branch covers.
    ///
    /// The field answers "did this declaration write a name in type position" — the question that decides
    /// whether the `(` after the declarator is an initializer or a parameter list
    /// ([`crate::grammar::cpp::decls::a_declaration_is_the_better_reading`]) — so *both* answers were wrong for
    /// a declaration that had written `G` in plain sight. Measured: `G<double> d(static_cast<double>(x));` was
    /// refused while `S s(static_cast<double>(x));` parsed.
    ///
    /// **A local, not a parser field.** Giving the specifier sequence a matching "end" that restored a shared
    /// park slot was tried and is not safe: the slot is written on entry and was only read on the success path,
    /// so every sequence that returned an error left it occupied, and the next successful sequence restored a
    /// stale name. Fifty headers went from 5 diagnostics to **399**, and the parser's own tests from 21 green
    /// binaries to 11. A local around one call has no state to leak, and the error paths need it least — a
    /// failed argument fails the whole template-id, and the rollback that follows restores the field anyway.
    pub fn set_declaration_type_record(&mut self, record: (Option<String>, bool)) {
        self.declaration_type_name = record.0.map(String::into_boxed_str);
        self.declaration_type_is_qualified = record.1;
    }

    /// Enter a class body, for the declaration rule that distinguishes a bit-field from a member initializer.
    ///
    /// Pushed on a **stack** rather than counted, because what the rule needs is the innermost brace: see
    /// [`CppParser::is_at_class_member_level`]. Deliberately not part of [`Checkpoint`]: no speculative region
    /// opens a brace and then rewinds past it, since the braces it would be rewinding over are its own rather
    /// than a guess.
    pub fn enter_class_body(&mut self) {
        self.open_bodies.push(BodyKind::Class);
    }

    /// Enter a **statement or declaration block** — a function body, a nested `{ … }`, an `extern "C" { … }`.
    ///
    /// The other half of the stack above: a `:` inside one of these is never a bit-field's width, whatever class
    /// body encloses it.
    pub fn enter_block_body(&mut self) {
        self.open_bodies.push(BodyKind::Block);
    }

    /// Record that the specifier sequence finished a whole declaration — which only `friend` does.
    ///
    /// Called by the specifier rule for `friend`, whose payload is the declaration that follows it, including
    /// its `;`. See the field's documentation.
    pub fn note_declaration_ended_inside_specifiers(&mut self) {
        self.declaration_ended_inside_specifiers = true;
    }

    /// Take the flag above: was the statement already over when the specifiers ended?
    ///
    /// A *take* rather than a read, so the answer cannot leak into the next declaration: the flag describes the
    /// sequence just parsed, and the caller that asks is the one that parses the declaration around it.
    pub fn take_declaration_ended_inside_specifiers(&mut self) -> bool {
        std::mem::take(&mut self.declaration_ended_inside_specifiers)
    }

    /// Record that this rule consumed the `;` that ends the declaration, from inside a conditional branch.
    ///
    /// See the field's documentation: the readers that call this are the ones that read a construct's *tail* once
    /// per branch, and the `;` each branch writes is part of the tail they read.
    pub fn note_the_terminator_came_from_a_branch(&mut self) {
        self.terminator_came_from_a_branch = true;
    }

    /// Take the flag above: did the construct just parsed already consume the declaration's `;`?
    ///
    /// A *take* for the reason every flag here is: the answer describes one construct, and the caller that asks
    /// (the rule that would otherwise require a `;`) is the one that parses around it.
    pub fn take_the_terminator_came_from_a_branch(&mut self) -> bool {
        std::mem::take(&mut self.terminator_came_from_a_branch)
    }

    /// How many conditionals are **open** at the cursor, as far as the directives read so far say.
    ///
    /// A construct whose tail is written once per branch has to tell the `#else` that closes a conditional **it
    /// opened itself** from the `#else` of a conditional opened around it — the two spellings are otherwise
    /// identical (`void f(int a);` / `#else` / `void g(long a);` reads as parameters just as well as a per-branch
    /// tail does). Counting the directives a construct *read* does not answer that: a nested reading may have
    /// consumed one on its way past, which is exactly what happens in `type_traits:2269`, where the expression
    /// grammar's operator-position seam takes the `# if` before the conditional can see it.
    ///
    /// So the count is kept by the directive reader itself, and a construct records the value when it starts and
    /// compares it where the question is asked.
    pub fn open_conditionals(&self) -> isize {
        self.open_conditionals
    }

    /// Record the directive **name** at the cursor, for [`CppParser::open_conditionals`].
    ///
    /// Called by the directive rule, which is the one place a directive is read — including the ones read by rules
    /// that are not that rule's caller. It is called with the cursor on the name (not on the `#`), and only when
    /// the directive has one: the null directive (`#` alone on a line) names nothing.
    pub fn note_a_directive_name(&mut self) {
        let opens_or_closes = match self.current_token_text() {
            "if" | "ifdef" | "ifndef" => 1,
            "endif" => -1,
            _ => 0,
        };

        self.open_conditionals += opens_or_closes;
    }

    /// Leave a class body.
    pub fn leave_class_body(&mut self) {
        self.open_bodies.pop();
    }

    /// Leave a statement or declaration block.
    pub fn leave_block_body(&mut self) {
        self.open_bodies.pop();
    }

    /// Is the cursor at the **member level** of a class body — the innermost brace being the class's own?
    ///
    /// The question `finish_init_declarator` puts to it: `int bits : 3;` and `S() : a(1) {}` have a `:` in the
    /// same position, and only the enclosing construct says which they are — a member declarator's `:`
    /// introduces a width, while a *function* declarator's introduces the member-initializer list of a
    /// constructor. The function case is decided first and does not need this; what does is the member that
    /// names no function.
    ///
    /// **The innermost brace is what matters, not "is a class body somewhere above".** Asking the weaker question
    /// meant that everything inside a member function's body counted as member level, which turned
    /// `for (auto &v: vec)` in a header's inline member into a bit-field of width `vec` — and the same for a
    /// label (`again:`) or any other statement whose first name a declaration reading could take.
    pub fn is_at_class_member_level(&self) -> bool {
        matches!(self.open_bodies.last(), Some(BodyKind::Class))
    }

    /// The declaration's leading type name, when it has one.
    pub fn declaration_type_name(&self) -> Option<&str> {
        self.declaration_type_name.as_deref()
    }

    /// Has a name been recorded in type position for the declaration being parsed?
    ///
    /// The weaker form of [`CppParser::declaration_type_name`], for a caller that only needs to know whether
    /// the specifier sequence saw a name at all — a declarator left without one is then not an abstract
    /// declarator but the mark of a name that was taken for a type.
    pub fn has_declaration_type_name(&self) -> bool {
        self.declaration_type_name.is_some()
    }

    /// Is the cursor inside a braced body?
    ///
    /// The signal `a_declaration_is_the_better_reading` uses: a function declaration inside a function body is
    /// vanishingly rare, while a local variable with constructor arguments is everywhere, so a known type name
    /// inside a body is read as a declaration.
    ///
    /// A *parser* depth rather than a C++ scope — a class body counts the same as a function body. That is the
    /// approximation `TypeNames` documents, and it is on the safe side here: what the answer licenses is the
    /// declaration reading, which is the cheaper of the two mistakes.
    ///
    /// What it must **not** count is a brace that is not a body: a **linkage specification's** block
    /// (`extern "C++" { … }`) introduces no scope for names at all — see [`parse_linkage_block`] — so the depth
    /// does not move inside one, and the rules that ask "am I inside a body" (the macro-from-a-header rules of
    /// `stats.rs` and `decls.rs`) keep answering no there. Counting it was a bug with wide reach: most of
    /// libstdc++ is written inside `extern "C++" { namespace std { … } }`, so `_GLIBCXX_BEGIN_NAMESPACE_VERSION`
    /// was read as a name instead of as the macro it is, and the declaration behind it came out as an expression
    /// (`expected `;` after expression` at `using ::wint_t;` — the first error of `cwchar`, `cstdlib` and every
    /// other header with that shape).
    ///
    /// [`parse_linkage_block`]: crate::grammar::cpp::decls::parse_linkage_block
    pub fn is_inside_a_body(&self) -> bool {
        self.type_names.depth() > 0
    }

    /// Is the cursor at file scope, outside every braced body?
    ///
    /// Named separately from [`CppParser::is_inside_a_body`] because callers read better for it, and because the
    /// two together are exhaustive: a statement is either in a body or not.
    pub fn is_at_file_scope(&self) -> bool {
        self.type_names.depth() == 0
    }

    /// Source range of the significant token at relative offset `offset` from the cursor.
    ///
    /// The third of the `peek_token_*_at` family, and it exists for the questions that are about the *source*
    /// rather than about the tokens: whether two things are written together or apart. `operator T&&()` and
    /// `operator bool() &&` are the same three tokens in the same order, and only the spacing separates the
    /// type from the ref-qualifier.
    ///
    /// A zero-width range at the end of input, mirroring [`CppParser::current_token_range`], so a caller
    /// comparing offsets never has to handle absence separately.
    pub fn peek_token_range_at(&self, offset: usize) -> SourceRange {
        let mut index = self.token_index;

        for current in 0..=offset {
            self.skip_trivia(&mut index);
            if current == offset {
                return match self.tokens.get(index) {
                    Some(token) => token.range,
                    None => SourceRange::EMPTY,
                };
            }
            index += 1;
        }

        SourceRange::EMPTY
    }

    /// Text of the significant token at relative offset `offset` from the cursor. Empty past the end.
    ///
    /// The companion to [`CppParser::peek_token_kind_at`], needed because `module` and `import` are
    /// *contextual* keywords: they must be recognised by spelling, not by kind.
    pub fn peek_token_text_at(&self, offset: usize) -> &str {
        let mut index = self.token_index;

        for current in 0..=offset {
            self.skip_trivia(&mut index);
            if current == offset {
                return match self.tokens.get(index) {
                    Some(token) => &self.text[token.range.start_offset..token.range.end_offset()],
                    None => "",
                };
            }
            index += 1;
        }

        ""
    }

    /// Kind of the token at `index`, ignoring trivia. `None` past the end.
    pub fn token_kind_at(&self, index: usize) -> CppTokenKind {
        self.tokens
            .get(index)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::None)
    }

    /// **Record that a bracketed group opened or closed**, for the one question that needs the number:
    /// whether a `>` at the cursor compares or closes the list it is in.
    ///
    /// Called by [`CppParser::bump`] and [`CppParser::consume_current_token`] — the two ways a token leaves the
    /// cursor — and by nothing else, which is the point. This is the counterpart of clang's `ConsumeParen` /
    /// `ConsumeBracket` / `ConsumeBrace`, the only writers of `ParenCount` / `BracketCount` / `BraceCount`: the
    /// depth is a function of what has been **consumed**, so no grammar rule can forget to restore it, and a
    /// rewound reading rewinds it through [`Checkpoint`].
    ///
    /// An unbalanced `)` saturates at zero rather than wrapping. See
    /// [`CppParser::innermost_template_argument_list`] for what the number is for.
    fn note_brackets(&mut self, kind: CppTokenKind) {
        match kind {
            CppTokenKind::LeftParen | CppTokenKind::LeftBracket | CppTokenKind::LeftBrace => {
                self.bracket_depth += 1;
            }
            CppTokenKind::RightParen | CppTokenKind::RightBracket | CppTokenKind::RightBrace => {
                self.bracket_depth = self.bracket_depth.saturating_sub(1);
            }
            _ => {}
        }
    }

    pub fn bump(&mut self) {
        let consumed_index = self.token_index;

        // Trivia tokens are emitted by `parse_trivia_tokens`, which runs over the whole skipped
        // span; pushing them here as well would duplicate them in the tree.
        if consumed_index < self.tokens.len() && !is_trivia_kind(self.current_token) {
            let token = self.tokens[consumed_index];
            self.events.push(MarkEvent::EatToken {
                kind: token.kind,
                range: token.range,
            });
            self.note_brackets(token.kind);
        }

        let mut next_index = consumed_index + 1;
        self.skip_trivia(&mut next_index);
        // Trivia between the token we just consumed and the next real token. `next_index` is
        // clamped inside, so trailing trivia at end of file is covered too.
        self.parse_trivia_tokens(consumed_index + 1, next_index);
        self.move_the_cursor_to(next_index);

        self.current_token = self
            .tokens
            .get(self.token_index)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::Eof);
    }

    pub fn peek_next_token(&self) -> CppTokenKind {
        let mut next_index = self.token_index + 1;
        self.skip_trivia(&mut next_index);

        self.tokens
            .get(next_index)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::None)
    }

    /// Kinds of the next `range.len()` significant tokens, starting at the cursor.
    ///
    /// Trivia is skipped, so this is "the next few things the grammar will see". Shorter than
    /// `range` near end of input, and padded with [`CppTokenKind::None`] so callers can index it
    /// without a length check.
    ///
    /// # Comparing the result
    ///
    /// This returns a `Vec`, and `Vec<T> == [T; N]` is **always false** in Rust — there is no
    /// `PartialEq<[T; N]>` impl. A comparison written as `peek_token_kind_at(1..2) == [X]` therefore
    /// compiles and silently never matches, which is exactly the kind of bug that reads as "the
    /// branch is dead" rather than "the comparison is wrong". Call `.as_slice()` or use the
    /// single-token helpers ([`CppParser::peek_next_token`], [`CppParser::peek_token_text_at`]).
    pub fn peek_token_kind_at(&self, range: std::ops::Range<usize>) -> Vec<CppTokenKind> {
        // **The result is `range.len()` long and is indexed from zero**, so a caller asking for `1..2` reads `[0]`
        // to mean "the token after the cursor". That is what `resize(range.len(), …)` below upholds, and it is why
        // the reservation is `wanted` while the length is `range.len()`: the two are different numbers, the caller
        // can only read `wanted` of them, and reserving 128 slots to answer a question about `[0]` is what the old
        // `range.len()` reservation did.
        let wanted = range.len();
        let mut kinds = Vec::with_capacity(wanted);
        let mut index = self.token_index;
        let mut seen = 0usize;

        // **Stops as soon as `range.end` significant tokens are seen.**
        //
        // The scan exists to answer a question about a window, and the window never extends past `range.end`, so
        // anything after it is walked and thrown away. That is not a micro-optimisation here because of the shapes
        // the grammar asks with: `1..128` and `0..96` are lookahead *budgets*, not requests — a caller writes the
        // largest window it could possibly need and then reads one or two entries of it. Walking all 128 for an
        // answer about one is work bought with nothing.
        //
        // It matters most on what this crate actually parses: a **cooked** standard-library stream is one line and
        // half its tokens are trivia (measured: 1 236 821 tokens, 617 606 of them trivia), so 128 tokens ahead is
        // about 128 trivia-run crossings, and one parse of that program makes 1 302 560 lookahead calls.
        while seen < range.end {
            while let Some(token) = self.tokens.get(index) {
                if is_trivia_kind(token.kind) {
                    index += 1;
                } else {
                    break;
                }
            }

            let Some(token) = self.tokens.get(index) else {
                break;
            };
            if seen >= range.start {
                kinds.push(token.kind);
            }
            seen += 1;
            index += 1;
        }

        kinds.resize(wanted, CppTokenKind::None);
        kinds
    }

    fn skip_trivia(&self, index: &mut usize) {
        while let Some(token) = self.tokens.get(*index) {
            if is_trivia_kind(token.kind) {
                *index += 1;
            } else {
                break;
            }
        }
    }

    /// Emit every trivia token in `(self.token_index, next_index)`, clamped to the token stream.
    ///
    /// `next_index` is where `skip_trivia` stopped. When that is past the end of the stream the
    /// span also covers the *trailing* trivia of the file, which is exactly why the clamp lives
    /// here rather than at the call site: a file ending in `// comment\n` must keep that comment
    /// in the tree, and a file that is nothing but comments must not produce an empty tree.
    ///
    /// # Comments are not emitted as tokens
    ///
    /// Whitespace and newlines go into the stream as themselves. A comment does not: it is handed to
    /// the documentation layer ([`crate::grammar::doc::parse_comment_group`]), which re-lexes its text
    /// with the doc lexer and emits a `DocComment` node — and, in it, the Doxygen commands the comment
    /// contains. Those events go into this same stream, so a doc node is an ordinary child of whatever
    /// node was open when the comment was found.
    ///
    /// The invariant this must not break is I1: every byte of the file appears exactly once in the
    /// tree. It holds because the doc lexer tiles a comment's text exactly (asserted in
    /// `tests/doc_lexer.rs`), so replacing the comment's one token with its doc tokens is a
    /// lossless rewrite rather than a substitution.
    ///
    /// # One node per run of adjacent comments
    ///
    /// A document is written across several `///` lines, so the comments that belong together are
    /// parsed as one group and produce one `DocComment` node. "Adjacent" means nothing but whitespace
    /// between them and at most one line apart: a blank line ends the run, which is how Doxygen
    /// separates a declaration's documentation from the block above it.
    fn parse_trivia_tokens(&mut self, start: usize, next_index: usize) {
        let end = next_index.min(self.tokens.len());
        let mut index = start.min(end);

        while index < end {
            let token = self.tokens[index];

            if !is_comment_kind(token.kind) {
                self.events.push(MarkEvent::EatToken {
                    kind: token.kind,
                    range: token.range,
                });
                index += 1;
                continue;
            }

            let group = self.comment_group_at(index, end);
            let sources = self.comment_sources(&group);
            // The doc parse is total: it cannot fail, and it cannot leave a marker open, so there is
            // nothing to recover from and nothing to check. Anything it wants to report it reports
            // through `push_error`.
            let _ = crate::grammar::doc::parse_comment_group(self, &sources);

            index = *group.last().expect("a group has at least one comment") + 1;
        }
    }

    /// Build the doc layer's view of one comment group.
    ///
    /// The separators are the trivia *between* the comments, collected here because this is where the
    /// token list is. They are handed to the doc parser rather than emitted here so that they land
    /// inside the `DocComment` node — see [`crate::grammar::doc::CommentSource`].
    fn comment_sources(&self, group: &[usize]) -> Vec<crate::grammar::doc::CommentSource> {
        let mut sources = Vec::with_capacity(group.len());

        for (position, &token_index) in group.iter().enumerate() {
            let comment = self.tokens[token_index];
            let mut source = crate::grammar::doc::CommentSource::first(comment.range);

            if position > 0 {
                // Everything between the previous comment and this one. The group is built from
                // comment positions plus the layout between them, so this is that layout — and only
                // layout, since a non-layout token would have ended the group.
                let previous_end = self.tokens[group[position - 1]].range.end_offset();
                for token in self.tokens[..token_index].iter().filter(|token| {
                    is_line_layout_kind(token.kind) && token.range.start_offset >= previous_end
                }) {
                    if source.separator_len < crate::grammar::doc::MAX_SEPARATOR_TOKENS {
                        source.separator[source.separator_len] = Some(token.range);
                        source.separator_len += 1;
                    }
                }
            }

            sources.push(source);
        }

        sources
    }

    /// The comments forming one documentation group, starting at `start`.
    ///
    /// `start` must be a comment. The returned positions are ascending and non-empty, and the first is
    /// `start`.
    fn comment_group_at(&self, start: usize, end: usize) -> Vec<usize> {
        let mut group = vec![start];
        let mut index = start + 1;
        let mut previous = self.tokens[start];

        while index < end {
            let token = self.tokens[index];

            if is_comment_kind(token.kind) {
                if self.lines_between(previous.range.end_offset(), token.range.start_offset) <= 1 {
                    group.push(index);
                    previous = token;
                    index += 1;
                    continue;
                }
                break;
            }

            // Only whitespace may separate the comments of a group, and never more than one line.
            if !is_line_layout_kind(token.kind)
                || self.lines_between(previous.range.end_offset(), token.range.start_offset) > 1
            {
                break;
            }

            index += 1;
        }

        group
    }

    /// How many line breaks separate two offsets?
    ///
    /// `0` means "on the same line", which is how `/* a */ /* b */` stays one group. Counting the
    /// newlines between them rather than asking a line index is both simpler and more accurate here:
    /// the question is about the gap, not about where the lines are.
    fn lines_between(&self, from: usize, to: usize) -> usize {
        if to <= from {
            return 0;
        }

        self.text[from..to.min(self.text.len())]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
    }

    pub fn push_error(&mut self, err: CppParseError) {
        self.errors.push(err);
    }

    /// Forget the "unrecognised character" diagnostics that fall inside `from..to`.
    ///
    /// The one caller is the directive rule, and the rule it implements is that **a replacement list is not C++
    /// text**: `shared/apiset.h:64` writes `#define API_SET_BY_ORDINAL(X,O,PO) X @##O NONAME PRIVATE`,
    /// inside `#ifdef _API_SET_HOST` — a branch only Microsoft's apiset tooling takes — and the `@` is that tool's
    /// syntax rather than C++'s. A compiler never lexes the branch, and never lexes a replacement list until the
    /// macro is *used*; reporting the character anyway cost 42 messages over the Windows SDK corpus and told the
    /// reader about text no compiler would refuse.
    ///
    /// **Only the character diagnostics**, and only inside the directive: the same `@` in C++ position is still a
    /// gap in the lexer, and every other diagnostic inside a directive's line — an unterminated argument list, a
    /// malformed `#if` — is still reported. A rule that dropped all of them would be hiding defects rather than
    /// declining to report non-C++ text.
    pub(crate) fn forget_unrecognized_characters_within(&mut self, from: usize, to: usize) {
        self.errors.retain(|error| {
            let about_a_character_no_cpp_token_has = error.message.starts_with("unrecognized character")
                // A `\` that is neither a splice nor a universal character name is the other one, and it is the
                // same claim: the character has no reading *as C++*. `shared/driverspecs.h:401` carries one inside a
                // SAL macro's replacement list.
                || error.message.starts_with("stray");

            !(about_a_character_no_cpp_token_has
                && usize::from(error.range.start()) >= from
                && usize::from(error.range.end()) <= to)
        });
    }

    /// Append a token produced by the documentation layer to the event stream.
    ///
    /// The doc layer hands over a token that already carries a range in *file* coordinates, so this
    /// is the whole of the translation between the two layers: no offset arithmetic, no buffering.
    /// That is what lets doc nodes be ordinary children of the C++ tree.
    pub(crate) fn push_doc_token(&mut self, kind: CppTokenKind, range: crate::text::SourceRange) {
        self.events.push(MarkEvent::EatToken { kind, range });
    }

    /// Emit the current token and move on, **without** attaching the trivia that follows it.
    ///
    /// `bump` is the right call almost everywhere: trivia belongs to the construct it sits inside.
    /// A grammar rule that wants to close its node before that trivia is emitted — because the trivia
    /// belongs to the *enclosing* construct — advances with this and then calls
    /// [`CppParser::emit_trivia_after_current_token`].
    pub fn consume_current_token(&mut self) {
        let consumed_index = self.token_index;

        if consumed_index < self.tokens.len() && !is_trivia_kind(self.current_token) {
            let token = self.tokens[consumed_index];
            self.events.push(MarkEvent::EatToken {
                kind: token.kind,
                range: token.range,
            });
            self.note_brackets(token.kind);
        }

        self.move_the_cursor_to(consumed_index + 1);
        self.current_token = self
            .tokens
            .get(self.token_index)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::Eof);
    }

    /// Consume the current token with [`CppParser::consume_current_token`] if it has this kind.
    ///
    /// Returns whether it was consumed. Used for tokens a rule requires but wants to treat as
    /// optional at the point of consumption, so that a missing one is reported by the caller rather
    /// than aborting a construct that is otherwise complete.
    pub fn consume_current_token_if(&mut self, kind: CppTokenKind) -> bool {
        if self.current_token == kind {
            self.consume_current_token();
            true
        } else {
            false
        }
    }

    /// Emit the trivia the cursor is sitting on, without consuming any significant token.
    ///
    /// The counterpart to [`CppParser::consume_current_token`]: after a rule has closed its node, the
    /// layout it left behind still has to reach the tree — the CST is lossless — and this is what puts
    /// it into whichever node is open now.
    pub fn emit_trivia_after_current_token(&mut self) {
        let trivia_start = self.token_index;
        let mut next_index = trivia_start;
        self.skip_trivia(&mut next_index);
        self.parse_trivia_tokens(trivia_start, next_index);

        self.move_the_cursor_to(next_index);
        self.current_token = self
            .tokens
            .get(self.token_index)
            .map(|token| token.kind)
            .unwrap_or(CppTokenKind::Eof);
    }

    /// Emit a zero-width `MissingNode`, i.e. "a token was expected here but is not present".
    ///
    /// Zero-width nodes cost nothing in the tree and are what make completion work at a broken
    /// position: the cursor is inside a node of the expected kind rather than in an error blob.
    pub fn emit_missing_node(&mut self) {
        let m = self.mark(crate::kind::CppSyntaxKind::MissingNode);
        m.complete(self);
    }

    /// Snapshot the set of currently open nodes. Pass the result to
    /// [`CppParser::close_marks_above`] in every `?`-using grammar function's error path.
    ///
    /// The snapshot is a *count* rather than a depth, and it is only meaningful as long as the
    /// stack below it is untouched: closing the nodes above it is driven by the open-node stack
    /// itself, so a rule that already closed some of its own nodes cannot confuse it.
    pub fn open_marks(&self) -> usize {
        self.open_marks.len()
    }

    /// Close every node opened after [`CppParser::open_marks`] was snapshotted, as if its closing
    /// token had been present.
    ///
    /// Call this on every early return from a grammar function. A `?` return leaves markers open,
    /// and an open `NodeStart` sits *before* the ancestor that eventually closes, so all
    /// following tokens would be swallowed into it — a silent corruption of the whole rest of the
    /// file rather than a local error.
    pub fn close_marks_above(&mut self, base: usize) {
        self.finish_marks_to(base);
    }
    /// Close any node opened after `base`, keeping the consumed tokens in the tree.
    ///
    /// Unlike [`CppParser::rollback`], which erases events, this keeps the text and only
    /// re-balances the node stack. Used by statement-level recovery when the tokens are known to
    /// belong to the current block but the statement parser gave up part way through.
    ///
    /// The nodes are closed **with their end events** ([`MarkerEventContainer::end_marks_to`]), which is the
    /// whole difference between a local error and a lost block: a node detached without one is balanced by the
    /// tree builder at the **end of the stream**, so the statement that failed swallows everything written after
    /// it — including the `}` that closes its own block. That is what `bits/stl_map.h` pays for:
    /// `__glibcxx_function_requires(…)` is a macro written without its `;`, the expression statement failed, and
    /// the `ExpressionStat` left behind then took the rest of `operator[]`'s body *and* the rest of `class map`
    /// with it (`std::map` had no `find`).
    pub fn recover_to_level(&mut self, base: usize) {
        if self.open_marks.len() > base {
            self.emit_missing_node();
            self.end_marks_to(base);
        }
    }

    pub fn has_error(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn get_errors(&self) -> Vec<CppParseError> {
        self.errors.clone()
    }

    /// Audit the event stream for balance. Used by tests to assert that a particular input's
    /// recovery left no node dangling.
    ///
    /// This is the check that catches the failure mode the marker stack exists to prevent: an
    /// unclosed `NodeStart` does not make the tree ill-formed, it makes it *wrongly nested*, and
    /// every token after the leak ends up in the wrong node.
    ///
    /// The raw event stream, for debugging the parser's recovery. Tests assert on it; production
    /// code should use the tree.
    pub fn events(&self) -> &[MarkEvent] {
        &self.events
    }

    /// Is a node of this `kind` **open** at the cursor — one of the nodes being parsed right now?
    ///
    /// [`CppParser::open_marks`] is the parser's own record of what is open: the event positions of the nodes that
    /// have been started and not finished. The positions are private but the events are not, so the question is
    /// answered from both.
    ///
    /// It is asked by rules that need to know **where they are** rather than what they are parsing. A template
    /// argument list is the case it exists for: inside a constraint the `<` is read one way at the constraint's
    /// top level (where a *declaration* follows the whole constraint) and another way inside parentheses (where
    /// the constraint itself continues). See [`crate::grammar::cpp::exprs`].
    ///
    /// An empty node does not count: `Marker::complete` drops a node with nothing after its `NodeStart`, which is
    /// the same exclusion [`CppParser::audit_events`] makes.
    pub fn is_open(&self, kind: CppSyntaxKind) -> bool {
        let end_of_stream = self.events.len();

        self.open_marks.iter().any(|position| {
            *position + 1 != end_of_stream
                && matches!(
                    &self.events[*position],
                    MarkEvent::NodeStart { kind: started, .. } if *started == kind
                )
        })
    }

    /// Note that a `NodeStart` with no children legitimately has **no** matching `NodeEnd`:
    /// `Marker::complete` drops empty nodes so the tree does not fill up with zero-width wrappers.
    /// Those are tracked in [`EventStreamAudit::empty_nodes`] and excluded from
    /// [`EventStreamAudit::unclosed`] — everything left in `unclosed` is a genuine leak.
    pub fn audit_events(&self) -> EventStreamAudit {
        // `open_marks` is the parser's own record of which nodes are still open, and it is updated
        // by the same call that emits each event, so it cannot drift from the stream the way an
        // independent re-derivation can.
        let end_of_stream = self.events.len();

        let empty_nodes = self
            .closed_marks
            .values()
            .filter(|emitted| !**emitted)
            .count();

        let mut unclosed = Vec::new();
        let mut empty_unclosed = 0usize;
        for position in &self.open_marks {
            match &self.events[*position] {
                MarkEvent::NodeStart { kind, .. } => {
                    // Nothing was recorded after it, so `complete` would have dropped it.
                    if *position + 1 == end_of_stream {
                        empty_unclosed += 1;
                    } else {
                        unclosed.push(*kind);
                    }
                }
                other => unreachable!("an open mark must point at a NodeStart, found {other:?}"),
            }
        }

        EventStreamAudit {
            final_depth: unclosed.len() as isize,
            // `open_marks` never contains duplicates and `close_mark` removes by identity, so a
            // node still open here was never double-closed: the count that would go negative is
            // exactly the leak reported above.
            min_depth: 0,
            empty_nodes: empty_nodes + empty_unclosed,
            unclosed,
        }
    }
}

/// Is this token invisible to the grammar?
///
/// Trivia tokens still appear in the tree — the CST must stay lossless — but the parser skips over
/// them, and `bump` attaches them to whichever node is currently open.
///
/// [`CppTokenKind::LineContinuation`] belongs here even though it is not whitespace: translation
/// phase 2 removes `\`-newline before the grammar ever sees it, so `int \<newline> x;` is one
/// declaration. The preprocessor layer reads the splices back out of the tree when it needs to know
/// that a directive continued onto the next line.
fn is_trivia_kind(kind: CppTokenKind) -> bool {
    is_comment_kind(kind) || is_line_layout_kind(kind)
}

/// Is this a comment? These are the tokens the documentation layer takes over.
fn is_comment_kind(kind: CppTokenKind) -> bool {
    matches!(kind, CppTokenKind::LineComment | CppTokenKind::BlockComment)
}

/// Is this layout *within* a line, or the break that ends one?
///
/// Layout tokens are what may sit between two comments of the same documentation group. A line
/// continuation is included because a `\`-spliced comment is still one comment as far as the
/// preprocessor is concerned.
fn is_line_layout_kind(kind: CppTokenKind) -> bool {
    matches!(
        kind,
        CppTokenKind::Whitespace | CppTokenKind::Newline | CppTokenKind::LineContinuation
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A parser over `text`, with the diagnostics it reports, for the tests below that need to watch a rollback.
    ///
    /// Both halves are held here because the borrow checker is what makes them impossible to hold from outside:
    /// `errors` is borrowed by the parser for as long as it lives.
    fn parser_and_errors<'a>(
        text: &'a str,
        errors: &'a mut Vec<CppParseError>,
    ) -> CppParser<'a> {
        CppParser::with_text(text, ParserConfig::default(), errors)
    }

    #[test]
    fn a_rolled_back_region_takes_its_diagnostics_with_it() {
        // The contract at the centre of every C++ ambiguity this parser resolves by trying: a reading that is
        // thrown away must leave **nothing** behind, and "nothing" includes the problems it reported on the way.
        // A diagnostic about a guess that lost is not a fact about the file — it is a complaint about code the
        // user did not write, and an editor would underline it.
        //
        // Tested by driving the parser rather than by finding a file that triggers it, and that is a measured
        // choice: instrumented over the 583 files of both corpora plus 53 valid snippets, **no input reaches this
        // path at all** (0 rollbacks that had diagnostics to drop). So there is no file-shaped test to write —
        // what can be pinned is the rule, and the rule is what a future speculative rule will lean on.
        let mut errors = Vec::new();
        let mut parser = parser_and_errors("int x;", &mut errors);

        let lost: Option<()> = parser.try_parse(|p| {
            p.push_error(CppParseError::syntax_error_from(
                "a reading that did not fit",
                p.current_token_range(),
            ));
            None
        });

        assert!(lost.is_none(), "the closure returned `None`");
        assert!(
            parser.errors.is_empty(),
            "the discarded reading's diagnostic is still there: {:?}",
            parser.errors
        );
    }

    #[test]
    fn a_reading_that_is_kept_keeps_its_diagnostics() {
        // The other half, and the reason the rule above is about *rolling back* rather than about reporting: the
        // reading that survives reports its problems normally.
        let mut errors = Vec::new();
        let mut parser = parser_and_errors("int x;", &mut errors);

        let kept: Option<()> = parser.try_parse(|p| {
            p.push_error(CppParseError::syntax_error_from(
                "a reading that fit, with a problem in it",
                p.current_token_range(),
            ));
            Some(())
        });

        assert!(kept.is_some(), "the closure returned `Some`");
        assert_eq!(
            parser.errors.len(),
            1,
            "a kept reading's diagnostic must survive: {:?}",
            parser.errors
        );
    }

    #[test]
    fn a_rollback_restores_every_piece_of_state_that_is_not_in_the_event_stream() {
        // `Checkpoint` exists because four things the parser carries are invisible to `events.truncate`: the
        // declaration's type name and whether it was qualified (both parked by a nested speculative declaration),
        // and now the diagnostics. Each one was found the same way — by a reading that came out wrong in a way no
        // tree shape explained.
        let mut errors = Vec::new();
        let mut parser = parser_and_errors("Widget x;", &mut errors);

        let checkpoint = parser.checkpoint();
        parser.begin_declaration_type();
        parser.note_declaration_type_name("Widget".to_string());
        parser.note_qualified_declaration_type();
        parser.push_error(CppParseError::syntax_error_from(
            "reported inside the region",
            parser.current_token_range(),
        ));
        parser.bump();

        parser.rollback(checkpoint);

        assert!(parser.declaration_type_name().is_none(), "the type name came back");
        assert!(
            !parser.has_qualified_declaration_type_name(),
            "and so did the qualified flag"
        );
        assert!(parser.errors.is_empty(), "the diagnostic came back");
        assert_eq!(parser.current_token_index(), 0, "and so did the cursor");
    }
}
