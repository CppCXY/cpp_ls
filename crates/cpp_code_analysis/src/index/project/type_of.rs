//! **The type of an expression** — the engine behind a hover, an inlay hint, and every check that compares types.
//!
//! Moved here from `project/mod.rs` without a change. The shape is a memoised walk: the answer comes from the
//! declaration facts, the scopes and the standard library's own signatures, and `TypeMemo` keeps the answers that were
//! expensive so that a completion or a diagnostic asking twice pays once.

use super::*;
use smol_str::SmolStr;

/// How deep the type question may ask itself before it gives up.
///
/// See the guard in [`type_of_expression`]: this is a bound on an ill-formed file's recursion, not a statement
/// about how complex a type may be.
/// How many members deep a member's type is followed before the walk gives up.
///
/// A member declared with another member's name is the ordinary way a class writes its own types —
/// `typedef _Ty& reference; reference front;`, `typedef size_t size_type; size_type size();` — so one step is not
/// enough. The step is a **member lookup** rather than a substitution, which is why it needs a bound of its own:
/// a lookup can be answered by a declaration that names the member it was asked from, and `a b; b a;` is writable
/// text even though it is not a valid program. Eight is far past any real chain — the standard library's deepest
/// are two or three.
pub(crate) const MAX_NESTED_TYPE_DEPTH: usize = 8;

/// **How many expression levels one question may walk.** Raised from 8 after measuring what a *macro expansion*
/// costs, which is not a deep type but a deeply **parenthesised** one — and every parenthesis and every comma operator
/// is a level:
///
/// ```text
/// ci_func(ci)  →  ((((void)0),((((void)0),(&((union GCUnion *)(…)‐>cl.l))))))->p
/// ```
///
/// The chain `…->value_` inside that expansion needs **ten** levels (`ci` itself is asked at depth 10), and the whole
/// expansion more than the eight this used to allow — so the last two steps of `ci_func(ci)->p` answered
/// `UnknownType("ci")`, the spelling at the depth where the walk stopped. The bound is still a bound on an ill-formed
/// file's recursion rather than a statement about a type's complexity, and a cycle is caught by
/// [`TypeMemo::enter`] rather than by it.
pub(crate) const MAX_TYPE_DEPTH: usize = 24;

/// **How many inference answers the index keeps** — see `ProjectIndex::type_answers`.
pub(crate) const MAX_TYPE_ANSWERS: usize = 8192;

/// **What an inference answer is a function of**, as one number: the file, the expression's range, its kind and its text.
///
/// The range is part of it on purpose: the same spelling at two places is two questions (`Widget` inside a class and
/// outside one), and a `T` in a template is a parameter where a `T` at file scope is a class.
pub(crate) fn the_type_key(path: &Path, expression: &cpp_parser::CppSyntaxNode) -> u64 {
    let range = expression.text_range();
    let mut bytes: Vec<u8> = Vec::with_capacity(96);
    bytes.extend_from_slice(normalize_path(path, cfg!(windows)).as_bytes());
    bytes.push(0x1f);
    bytes.extend_from_slice(&u64::from(u32::from(range.start())).to_le_bytes());
    bytes.extend_from_slice(&u64::from(u32::from(range.end())).to_le_bytes());
    let kind = cpp_parser::CppSyntaxKind::from(expression.kind());
    bytes.extend_from_slice(&u64::from(kind as u16).to_le_bytes());
    bytes.push(0x1e);
    bytes.extend_from_slice(expression.text().to_string().as_bytes());
    crate::cache::fnv1a64(&bytes)
}
/// **What a `new` expression allocates** — the spelling of its type, which is the whole of the answer.
///
/// The type is the `TypeId` child the grammar built: `new` is the one expression whose operand is a **type**, and
/// `parse_new_type_and_initializer` reads it with the type grammar rather than the expression one. So this is a
/// lookup in a tree that is already there, not an inference.
///
/// A placement list comes first and parses as an `ArgumentList` rather than a `TypeId`, so taking the first
/// `TypeId` child skips it without a rule about parentheses. `None` for a `new` with no type, which is a parse
/// error rather than an allocation.
pub(crate) fn new_expression_allocates(expression: &cpp_parser::CppSyntaxNode) -> Option<String> {
    let allocated = expression.children().find(|child| {
        cpp_parser::CppSyntaxKind::from(child.kind()) == cpp_parser::CppSyntaxKind::TypeId
    })?;

    let written = allocated.text().to_string();
    let written = written.trim();

    // `new int[10]` parses its type as `int[10]`, and the suffix belongs to the declarator rather than to the
    // allocated type: the expression type is `int*`, not `int[10]*`.
    let name = match written.find('[') {
        Some(at) => written[..at].trim(),
        None => written,
    };

    (!name.is_empty()).then(|| name.to_string())
}

/// The type of an expression, as far as this layer can tell, and the file that declared it.
///
/// The core of the `infer` layer, and it is **recursive** because that is what an expression is: `a.b.size` is a
/// member access whose object is a member access, and the type of the inner one is the type recorded on the
/// declaration the outer one starts from.
///
/// # The three shapes it can type, and the boundary
///
/// ```text
/// a name          `widget`      — its declaration's `type_of`, from this file or through the index
/// `this`          `this->size`  — the class whose scope encloses the expression, which needs no inference
/// a member access `a.b`         — recursively: find `b` in the type of `a`, then read `b`'s own `type_of`
/// ```
///
/// Everything else is [`UnknownReason::UnknownType`] carrying the expression's spelling: a call (`f().size`), a
/// dereference (`(*p).size`), a subscript, an arithmetic expression. Each of those needs a type *computed* rather
/// than read off a declaration, which is a different and much larger problem — and answering `Unknown` is what
/// keeps this layer from being wrong in a way a consumer cannot see.
///
/// # One entry point, one memo — and the recursion shares it
///
/// The engine used to be **stateless** below its caller: this function called itself for every sub-expression, so a
/// question already answered was answered again — a `2 800`-line file asked 382 top-level questions and spent
/// **5 923 ms**, because one `e.U.Start.Kind` re-deduced the `auto` declaration behind `e` eleven times. The memo is
/// now a parameter ([`type_of_expression_memoed`]), so every recursive question hits it; a caller with many questions
/// to ask — [`crate::sema::model::SemanticModel`] — passes one map for the whole request.
pub(crate) fn type_of_expression(
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    expression: &cpp_parser::CppSyntaxNode,
    depth: usize,
) -> Known<(Type, PathBuf)> {
    // **One memo per entry point**, which is the smallest scope that already removes the duplication: the questions
    // asked *inside* one expression are the ones that repeat. A caller that asks about many expressions shares a
    // larger memo by calling [`type_of_expression_memoed`] itself — see `SemanticModel`.
    let memo = TypeMemo::default();
    let shapes = crate::sema::declarations::ShapeTable::new();
    type_of_expression_memoed(
        &memo, &shapes, index, declaring, scopes, root, path, expression, depth,
    )
}

/// **What an expression's type was, and what a member lookup answered** — the memo one request shares.
///
/// The engine was recursive over the tree and **stateless**, so every sub-expression it had already answered was
/// answered again: on a 2 800-line file, one `e.U.Start.Kind` re-deduced the `auto` declaration behind `e` eleven
/// times, and the whole file was 382 top-level questions but **5 923 ms**. A memo keyed by the node's range is sound
/// for the same reason [`crate::sema::model::SemanticModel`]'s is: a range is unique within one tree, and a tree is
/// not mutated while a query runs.
///
/// The second map answers the other repeated question. A member lookup is keyed by the **class spelling and the
/// member's name**, which is the whole of its input, and one file asks the same pair many times — `size()` on one
/// string is three lines in one file, and every rule that reads a member access asks the same question. It is a
/// memo of the *lookup*, not of the answer's use: each caller still gets its own `DeclFact` clone.
#[derive(Default)]
pub(crate) struct TypeMemo {
    types: std::cell::RefCell<std::collections::HashMap<(u32, u32), Known<(Type, PathBuf)>>>,
    /// **The questions being answered right now** — see [`TypeMemo::enter`]. Keyed like `types`, because a node is
    /// unique within one tree and a tree is not mutated while a query runs.
    in_flight: std::cell::RefCell<std::collections::HashSet<(u32, u32)>>,
    members:
        std::cell::RefCell<std::collections::HashMap<(String, String), Known<(DeclFact, PathBuf)>>>,
}

impl TypeMemo {
    /// **The type of a node already asked about, if it was.**
    pub(crate) fn remembered_type(&self, key: &(u32, u32)) -> Option<Known<(Type, PathBuf)>> {
        self.types.borrow().get(key).cloned()
    }

    /// **Claim a question, or be told that it is already being answered.**
    ///
    /// # Why this is a set and not the depth counter
    ///
    /// `depth` bounds a *chain of sub-expressions*, and several call sites deliberately start a **new** question at
    /// zero, because a type read out of another declaration is not part of the chain that asked for it. That is right
    /// for `auto x = f(); x.size()` and fatal for a **cycle**: measured on a real C++23 project, one member question
    /// recursed
    ///
    /// ```text
    /// text_of(site.file).content  →  text_of(site.file)          (a call: its return type is its body's)
    ///                             →  macro_definitions.lookup(token.text(…))
    ///                             →  token.text(text_of(site.file).content)   ← the same node again
    /// ```
    ///
    /// with every lap resetting `depth` to 0, until the stack was gone (32 MB did not help). The memo below could not
    /// stop it either: an answer is remembered **after** it is computed, so a question that re-enters itself finds an
    /// empty memo, forever.
    ///
    /// `false` means "this node is already on the stack above you", and the honest answer there is that the type is
    /// not knowable from these questions — which is what `Unknown` is for.
    pub(crate) fn enter(&self, key: (u32, u32)) -> bool {
        self.in_flight.borrow_mut().insert(key)
    }

    pub(crate) fn leave(&self, key: &(u32, u32)) {
        self.in_flight.borrow_mut().remove(key);
    }

    /// **One member lookup, remembered by what it was asked.**
    ///
    /// The lookup is taken as a closure so that the memo owns both the key and the miss path in one place; a caller
    /// that wanted to skip the closure on a hit would have to write the map's shape at every call site.
    pub(crate) fn member(
        &self,
        class: &str,
        member: &str,
        look: impl FnOnce() -> Known<(DeclFact, PathBuf)>,
    ) -> Known<(DeclFact, PathBuf)> {
        let key = (class.to_string(), member.to_string());
        // Taken out of the borrow before the miss path runs: a `match` on `borrow()` holds the `Ref` for the whole
        // match, and the `borrow_mut()` inside it panics with `RefCell already borrowed` — the same shape that bit
        // the members-free memo this one replaces.
        if let Some(known) = self.members.borrow().get(&key).cloned() {
            return known;
        }
        let answer = look();
        self.members.borrow_mut().insert(key, answer.clone());
        answer
    }
}

/// [`type_of_expression`] with the caller's memo — no new map, and every question asked below shares it.
pub(crate) fn type_of_expression_memoed(
    memo: &TypeMemo,
    shapes: &crate::sema::declarations::ShapeTable,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    expression: &cpp_parser::CppSyntaxNode,
    depth: usize,
) -> Known<(Type, PathBuf)> {
    let range = expression.text_range();
    let key = (
        u32::try_from(usize::from(range.start())).unwrap_or(u32::MAX),
        u32::try_from(usize::from(range.end())).unwrap_or(u32::MAX),
    );
    if let Some(known) = memo.remembered_type(&key) {
        return known;
    }
    // **An answer a previous request already computed**, for a *top-level* question: the answer is a function of this
    // file's expression and of the index, and every insert or forget in the index clears the memo — so a hit cannot be
    // about a world the index has left. A recursive question is a step toward an answer rather than one, and `depth`
    // bounds what it was allowed to see, so nothing below the top is filed.
    if depth == 0
        && let Some(known) = index.a_remembered_type(path, expression)
    {
        memo.types.borrow_mut().insert(key, known.clone());
        return known;
    }
    // **A node already on the stack above this call is a cycle, and no amount of depth bounds it** — see
    // [`TypeMemo::enter`] for the chain that was measured. The answer names the spelling rather than inventing a type.
    if !memo.enter(key) {
        let written = expression
            .text()
            .to_string()
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    }

    let started = std::time::Instant::now();
    // **The recursion, when it is asked to show itself.** `depth` is bounded (`MAX_TYPE_DEPTH`) *per question*, and
    // several call sites deliberately start a **new** question at 0 — so a cycle that goes through one of them is not
    // bounded by anything. Measured on a real C++23 project: `module_graph.cpp` overflowed the stack with 32 MB on
    // `text_of(site.file).content.substr(…).starts_with(name)`, and no log said which questions repeated. One line per
    // question answers that, and it is the only instrument that can.
    if crate::probes::types() {
        eprintln!(
            "[type_of] depth {depth:>2}  {:?}",
            expression
                .text()
                .to_string()
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(60)
                .collect::<String>()
        );
    }
    let answer = type_of_expression_inner(
        memo, shapes, index, declaring, scopes, root, path, expression, depth,
    );

    if crate::probes::timing() && started.elapsed().as_millis() > 5 {
        eprintln!(
            "[type_of] {:>5} ms  depth {depth}  {:?}",
            started.elapsed().as_millis(),
            expression
                .text()
                .to_string()
                .lines()
                .next()
                .unwrap_or_default()
        );
    }
    // **The claim is released before the answer is filed**, and both happen whatever the arm answered: a question
    // left in flight would make every later ask about the same node answer `Unknown`.
    memo.leave(&key);
    if depth == 0 {
        index.remember_a_type(path, expression, &answer);
    }
    memo.types.borrow_mut().insert(key, answer.clone());
    answer
}

/// The body of the inference — one arm per expression shape, and **every recursive question goes back through
/// [`type_of_expression_memoed`]** so that an answer already computed is never computed again.
pub(crate) fn type_of_expression_inner(
    memo: &TypeMemo,
    shapes: &crate::sema::declarations::ShapeTable,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    expression: &cpp_parser::CppSyntaxNode,
    depth: usize,
) -> Known<(Type, PathBuf)> {
    let text = expression.text().to_string();
    let written = text.trim();

    // **A lambda's closure type is unique and unnamed**, so there is nothing a card could write in its place —
    // and the honest sentence is the answer, not the expression's text. Before this, the initializer fell through
    // to the spelling-carrying fallback and the card printed the whole lambda body as if it were a type: measured
    // on `auto show_division = [](double a, double b) { … if (const auto result = safe_divide(a, b)) … };`, whose
    // hover read "the type of `[](double a, double b) { … }` is not known here" over five lines.
    //
    // It is not "not implemented": every lambda is a distinct type with no spelling, and a reader asking what the
    // variable is wants to hear exactly that.
    if cpp_parser::CppSyntaxKind::from(expression.kind()) == cpp_parser::CppSyntaxKind::LambdaExpr {
        return Known::Unknown(UnknownReason::UnnamedLambda);
    }

    // **A bound on the recursion, not a policy about types.** Every arm below can ask the same question about a
    // *smaller* expression, and one of them asks it about a *different declaration* — which is what makes a file
    // like `auto a = *b; auto b = *a;` a loop rather than a walk. Nobody writes that, and a language server meets
    // whatever is written: past this depth the answer carries the spelling and no type, which is the same answer
    // this layer gives everything it cannot read.
    if depth > MAX_TYPE_DEPTH {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    }

    // **`co_await e` has the type of `e`'s awaiter's `await_resume()`.**
    //
    // ```cpp
    // task<std::string> greet(std::string name);
    // task<std::string> f() { auto s = co_await greet("x"); s. }   // `s` is a std::string
    // ```
    //
    // Without this the awaited expression was read as a `UnaryExpr` and fell through to the name case, so `s` was a
    // type nobody knew: measured on `test_cpp/coroutine.cpp`, completion after `s.` on
    // `auto s = co_await greet("x");` answered **nothing**, while the coroutine itself parsed and hovered. The
    // awaiter is the operand's own type — `task<T>` is its own awaiter — and the value is what
    // `await_resume()` returns, which [`operator_returns_of`] already reads for any member *with the class's
    // template arguments substituted* (`task<std::string>`'s `T` becomes `std::string`), so the two questions are
    // one call and no second substitution rule.
    //
    // A type that declares no `await_resume` is not an error this layer can name — the compiler's diagnosis is
    // better — so it answers `Unknown`, which is what keeps the completion after an unknown awaited value empty
    // rather than wrong.
    if let Some(operand) = a_co_awaits_operand(expression) {
        let awaited = type_of_expression_memoed(
            memo,
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            &operand,
            depth + 1,
        );
        if let Known::Yes((awaiter, _)) = awaited
            && let Some(returns) = operator_returns_of(
                index,
                declaring,
                scopes,
                root,
                path,
                &awaiter,
                "await_resume",
            )
            && !returns.trim().is_empty()
        {
            let settled = crate::sema::types::parse_type_spelling(&returns);
            return Known::Yes((settled, path.to_path_buf()));
        }
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    }

    // **`^^T` is a `std::meta::info`**, whatever it reflects: the one expression in C++ whose type does not depend on
    // its operand. The operand may be a type, a member, a template or a concept — the standard tells them apart by
    // lookup, and this layer does not have to for this answer. Without the arm the reflection fell through to the
    // name case (the `^^` is not an expression it knew) and answered nothing, which is what a reader asking "what is
    // this" over the operand saw.
    //
    // The spelling is the standard's, and the header that declares it (`<meta>`) is not one this machine has: the
    // answer names the type rather than resolving it, exactly as it would for a type from a header nobody read.
    if cpp_parser::CppSyntaxKind::from(expression.kind()) == cpp_parser::CppSyntaxKind::ReflectExpr
    {
        let info = crate::sema::types::parse_type_spelling("std::meta::info");
        return Known::Yes((info, path.to_path_buf()));
    }

    // **A literal's type is the one thing about it nobody has to look up.**
    //
    // `auto x = 1;` is a declaration whose type the file spells in a form no name can carry, and the reader had
    // no arm for it: the literal fell through to the name case, was found not to be one, and came back
    // `UnparsableName` — which says the *spelling* could not be read when what is true is that it is not a name at
    // all. Measured, this is 8 of the `auto` declarations in MSVC's `<vector>` alone, and the shape a reader meets
    // before any other.
    //
    // Read from the **token**, not the text: `1` and `1.0f` and `"abc"` differ by their kind, and a suffix
    // (`10u`, `1.0L`) is part of the literal rather than a separate thing to interpret. A user-defined literal
    // (`10_km`) is a **lookup**: its type is the return type of `operator""_km`, and that is the arm below.
    if let Some((kind, written_as)) = expression
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .map(|token| {
            (
                cpp_parser::CppTokenKind::from(token.kind()),
                token.text().to_string(),
            )
        })
        .find(|(kind, _)| {
            !matches!(
                kind,
                cpp_parser::CppTokenKind::Whitespace | cpp_parser::CppTokenKind::Newline
            )
        })
    {
        let literal = match kind {
            // **The literal's own spelling is the type** — `1u` is `unsigned int`, `1.0f` is `float`, `L'x'` is
            // `wchar_t` — and one function reads it for every site that asks (`docs/worklog.md` P1-4).
            cpp_parser::CppTokenKind::IntegerLiteral
            | cpp_parser::CppTokenKind::FloatingLiteral
            | cpp_parser::CppTokenKind::CharLiteral => {
                crate::sema::literal::a_literal_type(&written_as)
            }
            cpp_parser::CppTokenKind::BoolLiteral
            | cpp_parser::CppTokenKind::TrueKeyword
            | cpp_parser::CppTokenKind::FalseKeyword => Some("bool"),
            // **A string literal's element type is its encoding prefix**, which is the one part of a literal's type
            // the text carries: `L"…"` is `const wchar_t*`, `u8"…"` is `const char*`, and the plain form is too.
            // Answering `const char*` for every literal made `std::format(L"x {}", 1)` deduce `string` — the width
            // rule that picks between `format_string` and `wformat_string` was reading the wrong half.
            // **The encoding prefix is the element type** — `L"…"` is `const wchar_t*`, `u8"…"` is `const char8_t*` —
            // and one function reads every literal's spelling (`docs/worklog.md` P1-4).
            cpp_parser::CppTokenKind::StringLiteral => {
                crate::sema::literal::a_literal_type(&written_as)
            }
            cpp_parser::CppTokenKind::NullptrLiteral | cpp_parser::CppTokenKind::NullptrKeyword => {
                Some("nullptr_t")
            }
            // **A user-defined literal's type is its operator's return type.** `"numbers"sv` is
            // `operator""sv("numbers", 1)`, whose declaration is where the type is written — a lookup, and one the index
            // answers by name. Measured on `test_cpp`'s `strings.cpp`: `"numbers"sv.size()` had no type at all, so the
            // hover on `size` said the type of `"numbers"sv` was not known and go-to-definition reached nothing.
            cpp_parser::CppTokenKind::UserDefinedLiteral => None,
            _ => None,
        };
        // **The one literal kind whose spelling is not its type.** Handled before the arm above so that the
        // `Some("int")`-style answers stay one match, and because this is the only case that needs the index.
        if kind == cpp_parser::CppTokenKind::UserDefinedLiteral {
            // **An unknown answer says what is unknown**: the literal, with its suffix, rather than the whole
            // expression, because the suffix is the part that did not resolve.
            let a_user_defined_literal_is = |returns: String| {
                Known::Yes((
                    crate::sema::types::parse_type_spelling(&returns),
                    path.to_path_buf(),
                ))
            };
            if let Some(suffix) = a_user_defined_literals_suffix(&written_as)
                && let Some(declared) = index
                    .files_declaring(&format!("operator\"\"{suffix}"), path)
                    .into_iter()
                    .find(|declared| declared.fact.kind == crate::DeclKind::Function)
                && let Some(returns) = declared.fact.returns.as_deref()
                && !returns.trim().is_empty()
            {
                return a_user_defined_literal_is(returns.to_string());
            }
            return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
        }
        if let Some(spelled) = literal {
            // **Parsed, not named.** `Type::named("int")` is a type whose *name* is `int` — a class as far as every
            // later question is concerned — and the same for `const char*`. Measured: a string literal came back as
            // `Named { name: "const char*" }`, so `has an arithmetic type` was false for `int` and the conversion
            // relation answered `Unknown` for `int count = "three";`, which is the one pair it is certain about.
            //
            // `parse_type_spelling` is the crate's own reader for a written type: it knows the builtins, the
            // qualifiers and the declarators, and it is the same function a declaration's own spelling goes through
            // — so a literal's type and a declared type are built by one implementation.
            return Known::Yes((
                crate::sema::types::parse_type_spelling(spelled),
                path.to_path_buf(),
            ));
        }
    }

    /// **The suffix of a user-defined literal** — `_kb` for `4_kb`, `sv` for `"numbers"sv`, `_f` for `1.0e5_f`.
    ///
    /// The token is one piece of text, so the boundary has to be found in it. The **last** run of identifier characters
    /// is the suffix's neighbourhood; inside it, a suffix begins at its last `_` (a numeric literal's suffix must start
    /// there, and `0x1F_kb`'s `F` is a hexadecimal digit, not a letter of the suffix) and otherwise at its first letter
    /// (`"x"sv`, `1.5s` — the reserved suffixes the standard library declares without an underscore).
    fn a_user_defined_literals_suffix(written: &str) -> Option<&str> {
        let written = written.trim_end();
        let run_start = written
            .rfind(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
            .map(|boundary| boundary + 1)
            .unwrap_or(0);
        let run = &written[run_start..];
        if run.is_empty() {
            return None;
        }
        if let Some(underscore) = run.rfind('_') {
            let suffix = &run[underscore..];
            return (suffix.len() > 1).then_some(suffix);
        }
        run.find(|character: char| character.is_ascii_alphabetic())
            .map(|at| &run[at..])
    }

    // **A functional cast of a template-id is that type.** `std::vector<int>()` — the initialiser behind every
    // `auto z = std::vector<int>();` — is a **call expression** whose callee is a *type with arguments in it*, so the
    // function lookup below cannot answer it: there is no function called `std::vector<int>`. The type is the
    // spelling, read off the expression, and the arguments are kept rather than resolved because `std::vector<int>`
    // is what a reader wants to see and substituting `int` into a template's parameters would be a second answer
    // free to disagree with the one the file wrote.
    //
    // Measured on a real editor session: `auto z = std::vector<int>();` had **no** type, so `z.append(…)` fell back
    // to "every `append` in the project" and hover answered with nine `basic_string::append` declarations.
    if cpp_parser::CppSyntaxKind::from(expression.kind()) == cpp_parser::CppSyntaxKind::CallExpr
        && let Some(callee) = written.strip_suffix("()")
        && callee.contains('<')
        // **A type spelling has no space in it.** `_STD h<int>()` is a macro followed by a name — two identifiers the
        // parse keeps as one name, because that is what the standard library writes — and it is not a type; a type-id
        // is one spelling. This is the shape that made the arm claim `_STD _Convert_size<int>` as a type.
        && !callee.chars().any(char::is_whitespace)
        // **…and the name before the arguments is a class.** `h<int>()` is a **function template** call, whose type is
        // `h`'s return type — the call path answers that, and answering `h<int>` here would shadow it. So the arm asks
        // the index one question: is the bare name a class? Only then is this a construction rather than a call.
        && index
            .files_declaring(callee.split('<').next().unwrap_or(callee).trim(), path)
            .iter()
            .any(|declared| declared.fact.kind == crate::DeclKind::Type)
        // **Not a macro.** `MAKE_T<int>()` is a macro invocation whose expansion the parse does not have here, and
        // `auto x = MAKE_T<int>();` is deduced from the macro's own body elsewhere — claiming the spelling as a type
        // would answer `MAKE_T<int>`, which is not a type. An all-uppercase first segment is the convention every C++
        // codebase follows for a macro, and it is the one piece of evidence this arm has.
        && !callee
            .split("::")
            .next()
            .unwrap_or(callee)
            .split('<')
            .next()
            .unwrap_or(callee)
            .chars()
            .all(|character| !character.is_ascii_lowercase())
    {
        return Known::Yes((Type::named(callee.trim()), path.to_path_buf()));
    }

    // `this` is the enclosing class, and no inference is involved: the scope chain already knows which class this
    // is, and it is the same answer inside every member function of it — and when the chain does not, the
    // definition's own qualified name does. See [`class_of_the_enclosing_definition`].
    if written == "this" {
        let offset = usize::from(expression.text_range().start());
        return match class_of_the_enclosing_definition(scopes, offset) {
            Some(class) => Known::Yes((Type::named(class), path.to_path_buf())),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // **A `new` expression is a pointer to what it allocates**, and that is C++'s rule rather than an inference:
    // `new T`, `new T(args)`, `new T[n]` and `new T{args}` all have the type `T*`. Nothing about `T`'s own type has
    // to be known, which is what makes this arm worth having where the callee's return type is not — and the shapes
    // it covers are exactly the ones a factory writes.
    //
    // Measured on MSVC's `<memory>`: **5** `auto` declarations refused with `UnknownType("new _Ref_count_obj2<_Ty>
    // (…)")` and its `_Ref_count_bounded_array` siblings — every one of them answerable from the spelling alone,
    // with the type already in the tree and nothing asking it. The template arguments are kept rather than resolved:
    // `_Ref_count_obj2<_Ty>*` is what a reader wants to see, and an attempt to resolve `_Ty` would turn a certain
    // answer into an uncertain one.
    if cpp_parser::CppSyntaxKind::from(expression.kind()) == cpp_parser::CppSyntaxKind::NewExpr {
        return match new_expression_allocates(expression) {
            Some(allocated) => Known::Yes((
                crate::sema::types::parse_type_spelling(&format!("{allocated}*")),
                path.to_path_buf(),
            )),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // A name: its declaration says what type it has. The file being edited is asked first, because a buffer that
    // has never been saved has no summary — the two-layer split the name query uses, for the same reason.
    //
    // **Qualified or bare is the same case**, and that is C++'s rule rather than a convenience: `lib::global` is one
    // entity, and the chain is part of its *spelling* — a scope to descend through, not an expression around a name.
    // Read the other way (a bare name only) every qualified name in every file was `UnknownType`: `lib::global`,
    // `std::cin`, `ns::make()` — measured, `a_qualified_name_in_this_file_has_the_type_its_declaration_wrote`.
    // **A name a structured binding introduced** — `for (auto [key, value] : map)`, `auto [a, b] = pair;`.
    //
    // The name is a variable with no written type, so the name rule below has nothing to read: the declaration says
    // `auto`, and the elementâ€™s type is whatever the *range* (or the initializer) produces. The answer is that
    // type's position-th component, which is the language's rule rather than an inference: `[a, b]` over a
    // `pair<A, B>` binds `a` to `A` and `b` to `B`, and a `map<K, V>`'s elements are `pair<const K, V>`.
    if let Some(answer) = structured_binding_type(
        memo, index, declaring, scopes, root, path, expression, depth,
    ) {
        return answer;
    }

    if writes_a_name(written) {
        return match declaration_of_expression(index, scopes, root, path, expression) {
            Known::Yes(named) => {
                let dt = declared_type(memo, index, declaring, scopes, root, path, &named, depth);
                match dt {
                    Known::Yes(type_of) => {
                        // **A declaration whose type is an alias template applied to arguments is
                        // finished here**, the same way the member arm finishes one.
                        // `enable_if_t<__is_same(int, int), double>` is a *spelling* the fact
                        // records, and the alias's target is `typename enable_if<…, double>::type`
                        // — a member of a class template applied to arguments, which
                        // `member_fact`'s pattern rule resolves once it is asked with those
                        // arguments.
                        let type_of =
                            a_trait_alias_resolved(index, scopes, root, path, &named, &type_of)
                                .unwrap_or(type_of);
                        Known::Yes((
                            substituted_at_the_use(
                                index, scopes, root, path, expression, &named, type_of,
                            ),
                            named.file(path),
                        ))
                    }
                    Known::Unknown(reason) => Known::Unknown(reason),
                    // The declaration is a function, a class or an alias: none of them has a type *as a name*, which
                    // is the distinction `DeclFact::returns` exists for.
                    Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
                }
            }
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // A **braced initializer**: `Widget{…}` is a temporary of the class it names, which is what makes
    // `auto w = Widget{}` a declaration of a `Widget`. Only the first child is read, and only when it *names* a
    // type: `{1, 2}` names nothing, and a braced list of values is a different thing that this layer does not
    // deduce (C++ deduces it as `std::initializer_list`, which is a library type, not a language one).
    if cpp_parser::CppSyntaxKind::from(expression.kind()) == cpp_parser::CppSyntaxKind::InitListExpr
    {
        let Some(first) = expression.children().next() else {
            return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
        };

        // **A construction has a head, a bare braced list does not.** `{1, 2}` has values only; `Builder<int>{}` is the
        // type's name and a nested list of values. The first branch below refuses *anything* whose first child has a
        // type, which is right for a value and wrong for a type: `Builder<int>{}.with(2)` answered "the type of
        // `Builder<int>{}` is not known" while the head was a class this file declares.
        let has_a_head = cpp_parser::CppSyntaxKind::from(first.kind())
            != cpp_parser::CppSyntaxKind::InitListExpr
            && expression.children().nth(1).is_some_and(|child| {
                cpp_parser::CppSyntaxKind::from(child.kind())
                    == cpp_parser::CppSyntaxKind::InitListExpr
            });

        return match type_of_expression_memoed(
            memo,
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            &first,
            depth + 1,
        ) {
            // **Not the first element's type.** `{1, 2}` is a braced list of values and C++ makes it a
            // `std::initializer_list<int>` — a library type, which this layer deliberately does not invent. Answering
            // `int` here claimed that the list *is* an `int`, and the hover that read the type at the object's last byte
            // then looked for members of `int` on a `Pair<int, int>`.
            Known::Yes(_) if !has_a_head => {
                Known::Unknown(UnknownReason::UnknownType(Box::from(written)))
            }
            _ => match declaration_of_expression(index, scopes, root, path, &first) {
                // **The spelling is read, not named.** `Type::named("Pair<int, int>")` is a class whose *name* is
                // `Pair<int, int>` and whose argument list is empty — so nothing downstream substitutes: the member
                // lookup is handed that spelling, `TypeBindings` gets no arguments, and `sum`'s written `T` comes back as
                // `T`. `parse_type_spelling` is the crate's reader for a written type and answers
                // `Named { name: "Pair", arguments: [int, int] }` for the same text.
                //
                // **And a class template with no written arguments deduces them** — `std::pair pair{1, 2}` is a
                // `pair<int, int>`; see [`the_arguments_a_braced_initialisation_deduces`].
                //
                // **The question is whether the head names a type, not whether its declaration has one.** This asked
                // `type_of(...).is_none()`, which is true for a class *and* for a `using`-alias declared in this file
                // (the declarator reader has no declarator to read) — but false for an alias the index answered, whose
                // `type_of` is its target. `ContainerOf<std::vector, int>` is an alias template in `test_cpp`'s
                // anonymous namespace, and the use is outside it: the file's scopes miss the binding, the index answers
                // it, the target `Container<T, std::allocator<T>>` is `Some`, and the head fell to the spelling arm
                // below — which refuses a spelling with a space in it, so a construction of an alias with two
                // arguments answered "the type ... is not known here". Where the alias was at file scope the binding
                // came from the tree instead and the same construction worked: one declaration's *file position*
                // decided the answer, which is not a rule about types.
                Known::Yes(named) if named.names_a_type() => {
                    let spelled = first.text().to_string();
                    let spelled = spelled.trim();
                    let elements = the_elements_of_a_braced_list(expression);
                    let settled = the_arguments_a_braced_initialisation_deduces(
                        memo, index, declaring, scopes, root, path, &elements, spelled, depth,
                    )
                    .unwrap_or_else(|| crate::sema::types::parse_type_spelling(spelled));
                    Known::Yes((settled, named.file(path)))
                }
                // **A qualified type name is a type even when the name query declines it.** `std::pair{1, 2}` is an
                // `InitListExpr` whose first child is an `IdentifierExpr "std::pair"` — an expression node, not a name in
                // a scope, so `declaration_of_expression` answers nothing and the list fell back to the bare spelling:
                // `auto t = std::tuple{1, 2, 3}` was a `std::tuple` with no arguments (measured with `inspect inlay`).
                // The spelling is asked of the index instead, and the deduction is the same one the arm above runs.
                _ => match a_type_the_expression_names(index, path, &first) {
                    Some(spelled) => {
                        let elements = the_elements_of_a_braced_list(expression);
                        let settled = the_arguments_a_braced_initialisation_deduces(
                            memo, index, declaring, scopes, root, path, &elements, &spelled, depth,
                        )
                        .unwrap_or_else(|| crate::sema::types::parse_type_spelling(&spelled));
                        Known::Yes((settled, path.to_path_buf()))
                    }
                    None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
                },
            },
        };
    }

    // A **call**: what the callee returns, or a temporary of the class it names.
    if cpp_parser::CppSyntaxKind::from(expression.kind()) == cpp_parser::CppSyntaxKind::CallExpr {
        return type_of_a_call(
            memo, shapes, index, declaring, scopes, root, path, expression, depth,
        );
    }

    // **A named cast has the type it names**, and that is C++'s own rule rather than an inference: `static_cast<T>`,
    // `dynamic_cast<T>`, `const_cast<T>` and `reinterpret_cast<T>` all have the type `T`, whatever the operand is —
    // the operand's type is what the *programmer* had to know, and the cast is the statement of the answer. So
    // nothing is walked and nothing can be wrong: the `TypeId` the grammar read is the type.
    //
    // Measured before this arm existed: 39 `auto` declarations across `<vector>` and `<xutility>` refused, every one
    // of them a cast — `UnknownType("static_cast<size_t>")`, `UnknownType("const_cast<char*>")` — with the cast
    // expression already in the tree and nothing asking it.
    if cpp_parser::CppSyntaxKind::from(expression.kind()) == cpp_parser::CppSyntaxKind::CastExpr {
        let target = expression
            .children()
            .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::TypeId);
        return match target {
            Some(target) => {
                let written = target.text().to_string();
                let written = written.trim();
                Known::Yes((
                    crate::sema::types::parse_type_spelling(written),
                    path.to_path_buf(),
                ))
            }
            // A `CastExpr` with no type in it is one the recovery built around a keyword with nothing after it,
            // which is a file being typed at rather than a cast.
            None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // **A conditional expression** has the type the two branches agree on: `cond ? 1 : 2.0` is a `double` — [expr.cond]
    // usual arithmetic conversions, the same rule a fold uses. Measured: `auto widened = true ? 1 : 2.0;` answered
    // *"the type of `true ? 1 : 2.0` is not known here"*.
    if cpp_parser::CppSyntaxKind::from(expression.kind()) == cpp_parser::CppSyntaxKind::TernaryExpr
    {
        let branches: Vec<cpp_parser::CppSyntaxNode> = expression.children().collect();
        if let (Some(one), Some(other)) = (branches.get(1), branches.get(2)) {
            let one_type = type_of_expression_memoed(
                memo,
                shapes,
                index,
                declaring,
                scopes,
                root,
                path,
                one,
                depth + 1,
            );
            let other_type = type_of_expression_memoed(
                memo,
                shapes,
                index,
                declaring,
                scopes,
                root,
                path,
                other,
                depth + 1,
            );
            if let (Known::Yes((one_type, file)), Known::Yes((other_type, _))) =
                (one_type, other_type)
            {
                let agreed = if one_type == other_type {
                    Some(one_type.clone())
                } else {
                    crate::sema::declarations::the_widest_of(&[one_type, other_type])
                        .map(|widest| crate::sema::types::parse_type_spelling(&widest))
                };
                if let Some(agreed) = agreed {
                    return Known::Yes((agreed, file));
                }
            }
        }
    }

    // **`sizeof` and `alignof` are a `size_t`**, whatever they are applied to — an expression or a type, complete or not.
    // Measured: `auto bytes = sizeof(int);` answered nothing at all, and the initialiser's tree is
    // `Initializer > UnaryExpr "sizeof(int)"`, so the arm is on the *shape* rather than on a kind this grammar does not
    // produce for it.
    {
        let spelled = expression.text().to_string();
        let spelled = spelled.trim_start();
        if spelled.starts_with("sizeof") || spelled.starts_with("alignof") {
            return Known::Yes((
                crate::sema::types::parse_type_spelling("size_t"),
                path.to_path_buf(),
            ));
        }
    }

    // A **parenthesised** expression has the type of what it wraps, and the parentheses are a node of their own:
    // `(*p).size` makes the object of the `.` a `ParenExpr`, so a recursion that did not step through it would
    // stop one level above the answer — which is exactly where it used to stop.
    if cpp_parser::CppSyntaxKind::from(expression.kind()) == cpp_parser::CppSyntaxKind::ParenExpr {
        return match expression.children().next() {
            Some(inner) => type_of_expression_memoed(
                memo,
                shapes,
                index,
                declaring,
                scopes,
                root,
                path,
                &inner,
                depth + 1,
            ),
            None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // **A binary operator**, which is where most of the `auto` declarations in a standard-library header get their
    // type: `auto x = a & b;`, `auto y = p << n;`. Measured, 466 of the 488 `auto`s this could not answer were
    // expressions of this shape — `(*_VbFirst & _FirstSourceMask) << _SourceShift` and the like.
    //
    // # Three rules, and only one of them is exact for free
    //
    // * **A comparison or a logical operator yields `bool`.** `==`, `<`, `&&` … That is C++'s own answer for every
    //   operand type there is, so nothing is inferred and nothing can be wrong.
    // * **An assignment yields its left operand's type**, which is also C++'s answer rather than an inference.
    // * **Everything else is the usual arithmetic conversion**, and that is where this stops.** `a + b` is `int`
    //   for two `int`s and `unsigned` for an `int` and an `unsigned`, so the answer is **exact when the two
    //   operands already agree** — the common case in the code that survives — and is refused otherwise. Ranking
    //   the conversions (`long long` beats `unsigned`, `double` beats everything) would be a table this analysis
    //   would then have to keep right for every platform's widths; a wrong type shown for an `auto` is worse than
    //   `auto`, because the second says the file did not spell it and the first says it did.
    if let Some((operator, left, right)) = binary_operands_of(expression) {
        // **The comma operator is its right operand's type**, and it is C++'s own answer rather than an inference: the
        // standard says the value of `a, b` is the value of `b`. It is not an arithmetic question and it is not the
        // left operand's type, so both rules below would answer it wrongly — which is what happened on a *rendering*:
        // Lua's `check_exp(c, e)` is `(lua_assert(c), (e))`, so `ci_func(ci)` expands to a comma expression whose type
        // is the right operand's, and without this the whole expansion had no type: measured on `ldebug.c:101`,
        // hovering `p` in `ci_func(ci)->p` said the type of the parenthesis was "not known here".
        if operator == ","
            && let Known::Yes(answer) = type_of_expression_memoed(
                memo,
                shapes,
                index,
                declaring,
                scopes,
                root,
                path,
                &right,
                depth + 1,
            )
        {
            return Known::Yes(answer);
        }
        // **A pipeline names a view, and the standard spells it.** `numbers | std::views::filter(pred)` is
        // `filter_view<all_t<decltype(numbers)>, Pred>`, and the standard fixes every one of those pieces: which
        // view class an adaptor produces, that the range is passed through `views::all_t`, and that a predicate
        // adaptor carries its callable as the second template argument.
        //
        // This is the *first* half of the pipe the project could answer: the closure's own type
        // (`__adaptor::_Partial<_Filter, …>`) is internal to the library and its second argument is an unnamed
        // closure type, so `views::filter(pred)` alone still has no type — but a *pipeline* has one, because the
        // view class is in the standard. See `docs/ranges.md`.
        if operator == "|"
            && let Some((view, file)) = the_view_a_pipeline_names(
                memo, shapes, index, declaring, scopes, root, path, &left, &right, depth,
            )
        {
            return Known::Yes((view, file));
        }

        // **A class's own operator** — `a + b` on a type that declares `operator+`, `a == b` on one that declares
        // `operator==`. It is tried **before** the builtin rules below, because those are answers about arithmetic
        // and a class that overloads an operator has said what it means: `Money + Money` is `Money`, not "the two
        // operands agree"; `Matrix == Matrix` is whatever `operator==` returns, not necessarily `bool`.
        //
        // Assignments are left out: `a = b` is the left operand's type, which the rule below already answers, and a
        // user `operator=` that returns something else is a spelling nobody relies on for `auto`.
        if !matches!(
            operator.as_str(),
            "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^=" | "<<=" | ">>="
        ) && let Known::Yes((left_class, file)) = type_of_expression_memoed(
            memo,
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            &left,
            depth + 1,
        ) && matches!(left_class, Type::Named { .. })
            && let Some(returns) = operator_returns_of(
                index,
                declaring,
                scopes,
                root,
                path,
                &left_class,
                &format!("operator{operator}"),
            )
        {
            return Known::Yes((crate::sema::types::parse_type_spelling(&returns), file));
        }

        if matches!(
            operator.as_str(),
            "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||"
        ) {
            return Known::Yes((Type::named("bool"), path.to_path_buf()));
        }

        let left_type = type_of_expression_memoed(
            memo,
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            &left,
            depth + 1,
        );
        if matches!(
            operator.as_str(),
            "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "&=" | "|=" | "^=" | "<<=" | ">>="
        ) {
            return left_type;
        }

        let Known::Yes((left_type, file)) = left_type else {
            return match left_type {
                Known::Unknown(reason) => Known::Unknown(reason),
                _ => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
            };
        };

        return match type_of_expression_memoed(
            memo,
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            &right,
            depth + 1,
        ) {
            Known::Yes((right_type, _)) if right_type == left_type => {
                // **Two pointers subtracted are not a pointer.** `p - q` is a *count of elements*, and C++ spells
                // it `ptrdiff_t` — so "the operands agree, therefore the answer is their type" holds for
                // arithmetic and is wrong for exactly this pair. Measured on a fixture: `auto d = q - raw();` with
                // both operands `int*` answered **`int*`**, which is not a missing answer but a wrong one, and a
                // wrong type shown for an `auto` is worse than a refusal — the refusal says the file did not spell
                // it, the wrong answer says it did.
                //
                // Refused rather than answered `ptrdiff_t`: a file that spells its own difference type — MSVC's
                // `difference_type`, a `_Distance` — would be shown a name from another library, and this layer has
                // no way to know which of them the code is written against.
                if operator == "-" && left_type.pointee().is_some() {
                    return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
                }
                Known::Yes((left_type, file))
            }
            // **A pointer plus an integer literal is the pointer** — C++'s own rule, [expr.add] — and it is
            // written here rather than left out because the case it is for is the one this layer cannot reach yet.
            //
            // `_Get_unwrapped` returns `_It + 0` in every branch: 54 `auto` declarations over eight MSVC headers,
            // each a template parameter plus a literal. The rule needs `left_type.pointee()` — and `_It`'s type is
            // the **name** `_Iter`, not a `T*` spelling, so the analysis cannot see that it is a pointer at all
            // until the call site's argument is substituted for it. It was written, measured (306 deduced before
            // and after — it never fired) and taken back out for that reason. Instantiation is what makes it fire.
            // **The two operands disagree, so the conversion decides and this does not rank conversions.** The
            // answer is the spelling, which is what every other refusal in this layer carries.
            Known::Yes(_) => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // A **dereference**: `*p` has the type `p` points at. Nothing is looked up — the pointer's own declaration
    // already spells the pointee, and the `*` is arithmetic on that spelling.
    if let Some(operand) = unary_operand_with(expression, "*") {
        let operand_type = type_of_expression_memoed(
            memo,
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            &operand,
            depth + 1,
        );
        return match operand_type {
            // **A `*` is the type model's own operation.** `Widget**` gives `Widget*`, an array gives its
            // element, and a class with an `operator*` gives nothing — that one needs the class's members and
            Known::Yes((type_of, file)) => match type_of.pointee() {
                // **Only a pointer's or an array's `*` is the model's own operation.** A class goes to the arms
                // below even though `pointee()` answers for it too: what `*` means for a class is what the class
                // declared, and a class that declared nothing is the iterator model's case.
                Some(pointee)
                    if !matches!(type_of, Type::Named { .. } | Type::TemplateParameter { .. }) =>
                {
                    Known::Yes(((*pointee).clone(), file))
                }
                // **A class's own `operator*`** — `*it` on a user iterator, whose return type is the element the
                // operator promises. Tried first: a class that defines it has said what `*` means for itself.
                Some(_) | None => {
                    if let Some(returns) = operator_returns_of(
                        index,
                        declaring,
                        scopes,
                        root,
                        path,
                        &type_of,
                        "operator*",
                    ) {
                        return Known::Yes((
                            crate::sema::types::parse_type_spelling(&returns),
                            file,
                        ));
                    }
                    // **The iterator model**, for a class with no `operator*` that came from `begin()`: it is a
                    // view of its container's element, so `*it` is the element the type already is.
                    match &type_of {
                        Type::Named { .. } | Type::TemplateParameter { .. } => {
                            Known::Yes((type_of, file))
                        }
                        // An operand whose type has no `*` on it: the program is ill-formed, and a type invented
                        // here would be a wrong answer rather than a missing one.
                        _ => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
                    }
                }
            },
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // An **address-of**: `&x` is a pointer to `x`, which is the same arithmetic the other way round. It is here
    // so that the two operators are one rule rather than one rule and one hole: `(&r)->size` is a member access
    // whose object is this, and the `->` already knows what to do with a pointer.
    if let Some(operand) = unary_operand_with(expression, "&") {
        let operand_type = type_of_expression_memoed(
            memo,
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            &operand,
            depth + 1,
        );
        return match operand_type {
            Known::Yes((type_of, file)) => {
                // `&x` is a pointer to `x`, and the decay first is what makes `&arr` a pointer to the array's
                // *element* — C++'s own rule, and the reason this is not simply "wrap it in a pointer".
                let pointed_at = type_of.decay();
                Known::Yes((
                    Type::Pointer {
                        to: Arc::new(pointed_at),
                    },
                    file,
                ))
            }
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // **A unary operator that a class may overload**: `-a`, `+a`, `~a`, `!a`.
    //
    // There was no arm for these at all, so `Money neg = -a;` on a class declaring `Money operator-() const` answered
    // "not a type I know" — and it was not the class case alone: `-count` on an `int` had no answer either, because
    // the binary arm's arithmetic rule never runs for a one-operand expression. The rules are the language's two:
    //
    // * a class operand goes to its own `operator-` (and `!` to `operator!`), which has said what the operator means
    //   for it — a member list names them now (`DeclFact::name` is the spelling);
    // * a builtin operand keeps its own type, except `!`, which is `bool`. C++ promotes `-`/`+`/`~` on an integer
    //   narrower than `int` to `int`, and the widths are the table the binary arm refuses to keep, so the operand's
    //   own type is the honest answer here too.
    for symbol in ["-", "+", "~", "!"] {
        let Some(operand) = unary_operand_with(expression, symbol) else {
            continue;
        };
        let operand_type = type_of_expression_memoed(
            memo,
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            &operand,
            depth + 1,
        );
        return match operand_type {
            Known::Yes((type_of, file)) => {
                if matches!(type_of, Type::Named { .. } | Type::TemplateParameter { .. })
                    && let Some(returns) = operator_returns_of(
                        index,
                        declaring,
                        scopes,
                        root,
                        path,
                        &type_of,
                        &format!("operator{symbol}"),
                    )
                {
                    return Known::Yes((crate::sema::types::parse_type_spelling(&returns), file));
                }
                if symbol == "!" {
                    Known::Yes((Type::named("bool"), path.to_path_buf()))
                } else {
                    Known::Yes((type_of, file))
                }
            }
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // A **conditional**: `c ? a : b` has the type the two arms agree on, and nothing else about it matters — the
    // condition is a `bool` and never part of the answer.
    //
    // That is C++'s rule for the case this layer can decide: when the arms have the same type, that is the type,
    // whichever arm runs. When they differ the *conversion* decides — `c ? 1 : 2.0` is a `double` — and this layer
    // does not rank conversions, which is the same refusal the binary arm makes and for the same reason.
    //
    // Measured: this is `_Val ? *_VbFirst : ~*_VbFirst`, seven `auto` declarations in MSVC's `<vector>`. Both arms
    // are the `_Vb_reference` the bit iterator's operators return, so they agree, and the answer is that type.
    if CppSyntaxKind::from(expression.kind()) == CppSyntaxKind::TernaryExpr {
        let arms: Vec<cpp_parser::CppSyntaxNode> = expression.children().collect();
        // Three nodes — the condition, the `?` arm and the `:` arm — with the two punctuation tokens between them.
        let (Some(then_arm), Some(else_arm)) = (arms.get(1), arms.get(2)) else {
            return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
        };
        let then_type = type_of_expression_memoed(
            memo,
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            then_arm,
            depth + 1,
        );
        let else_type = type_of_expression_memoed(
            memo,
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            else_arm,
            depth + 1,
        );

        return match (then_type, else_type) {
            (Known::Yes((then_type, file)), Known::Yes((else_type, _)))
                if then_type == else_type =>
            {
                Known::Yes((then_type, file))
            }
            (Known::Yes(_), Known::Yes(_)) => {
                Known::Unknown(UnknownReason::UnknownType(Box::from(written)))
            }
            // One arm unknown is the whole answer unknown, and the reason is the arm's: a caller reading
            // "`x` is not a type I know" should be told about `x`, not about the conditional around it.
            (Known::Unknown(reason), _) | (_, Known::Unknown(reason)) => Known::Unknown(reason),
            _ => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // A **subscript**: `arr[0]` has the array's element type. That is the whole of it for an array; a *class*
    // with an `operator[]` — `v[0]` on a `std::vector` — needs the template instantiated, and this answers
    // `Unknown` for the same reason it does everywhere else: see [`element_type_name`].
    if let Some(base) = subscript_base(expression) {
        let base_type = type_of_expression_memoed(
            memo,
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            &base,
            depth + 1,
        );
        return match base_type {
            // **An array's element is the model's own operation**, and a *class* with an `operator[]` is the case
            // this arm deliberately leaves alone: `v[0]` on a `std::vector` gives the template's `reference`, which
            // needs the class looking up — taking the first template argument instead would be right for a
            // `vector` and wrong for a `map`, and nothing here can tell them apart.
            Known::Yes((type_of, file)) => match type_of.pointee() {
                // **A pointer's subscript is the pointee.** `int* a; a[0]` is `*(a + 0)`, and C++ fixes its type as `int`
                // — no class, no instance, nothing this layer cannot read. Reported: `auto x = a[0];` answered "the type
                // of `a[0]` is not known here".
                Some(element) if matches!(type_of, Type::Pointer { .. }) => {
                    Known::Yes(((*element).clone(), file))
                }
                Some(element) if matches!(type_of, Type::Array { .. }) => {
                    Known::Yes(((*element).clone(), file))
                }
                // **The class case**: a `std::vector<int>`'s `operator[]` returns `T&`, and `members_of` has already
                // substituted the argument — so the answer is the member's own return type rather than a guess from
                // the first template argument (which would be right for a `vector` and wrong for a `map`).
                //
                // **The index's type is what chooses between overloads**: on a class declaring both
                // `int& operator[](int)` and `double& operator[](const char*)`, `s["z"]` is the second, and asking
                // without the argument answered the first (`int&` — measured). The type of the index expression is
                // this expression's own question, one step down, and the memo makes it one.
                _ => {
                    let argument_types: Vec<Type> = subscript_index(expression)
                        .map(|argument| {
                            type_of_expression_memoed(
                                memo,
                                shapes,
                                index,
                                declaring,
                                scopes,
                                root,
                                path,
                                &argument,
                                depth + 1,
                            )
                        })
                        .and_then(|argument| match argument {
                            Known::Yes((type_of, _)) => Some(vec![type_of]),
                            _ => None,
                        })
                        .unwrap_or_default();
                    match operator_returns_of_with_arguments(
                        index,
                        declaring,
                        scopes,
                        root,
                        path,
                        &type_of,
                        "operator[]",
                        &argument_types,
                        (!argument_types.is_empty()).then_some(1),
                    ) {
                        Some(returns) => {
                            // **The answer is a *type*, not the spelling the member wrote.** `reference` is the class's
                            // own alias for `T&`, and both halves — the alias and the parameter — are what this reads;
                            // see [`a_members_answer_as_a_type`] for the measurement.
                            let written = a_members_answer_as_a_type(
                                index, scopes, root, path, &type_of, &returns,
                            );
                            if crate::probes::inference() {
                                eprintln!(
                                    "[inference] subscript on {:?}: the member wrote {:?}, the type is {:?}",
                                    type_of.to_string(),
                                    returns,
                                    written
                                );
                            }
                            Known::Yes((crate::sema::types::parse_type_spelling(&written), file))
                        }
                        // A class with no `operator[]`: nothing this layer can say, and saying nothing is the rule.
                        None => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
                    }
                }
            },
            Known::Unknown(reason) => Known::Unknown(reason),
            Known::No => Known::Unknown(UnknownReason::UnknownType(Box::from(written))),
        };
    }

    // A member access: the type of the member, which is a fact on its declaration.
    if let Some(inner) = crate::sema::resolve::member_access_of(expression) {
        // **One question, one answer, and the shared memo.** This arm used to call the *entry point* twice — once
        // for the `Yes` and once again to get the `Unknown`'s reason — which both bypassed the memo (a fresh map per
        // call) and paid for the inference twice. On `LuaSource.cpp` this is the arm every `x.y` and `x.y()` goes
        // through, so it is the one that has to be a single memoised question.
        let (inner_type, declared_in) = match type_of_expression_memoed(
            memo,
            shapes,
            index,
            declaring,
            scopes,
            root,
            path,
            &inner.object,
            depth + 1,
        ) {
            Known::Yes((inner_type, declared_in)) => (inner_type, declared_in),
            Known::Unknown(reason) => return Known::Unknown(reason),
            // Not "no": a `Known::No` is this layer's answer for a name it cannot place at all, and the caller is
            // told which spelling it was — the same answer the `unreachable!` this replaces would have panicked on.
            Known::No => {
                return Known::Unknown(UnknownReason::UnknownType(Box::from(
                    inner.object.text().to_string().trim(),
                )));
            }
        };

        // **An arrow is followed as far as the type answers.** `p->` on a `Widget*` reaches `Widget`, on a
        // `std::shared_ptr<Node>` reaches `Node` — the pointer-like class says so with `element_type` (`operator->`'s
        // declared return type is a dependent `typename …::type`, which is a trait this layer cannot read). The
        // completion has resolved this for a while in [`crate::completion::the_class_after_the_operator`], including the
        // bound on a proxy whose `operator->` returns itself; this arm used to look the member up in the **handle** and
        // answered `Unknown(NotDeclaredHere("shared_ptr::twin"))` for `auto twin = node->twin();`.
        let reached_by_an_arrow = inner.arrow.then(|| {
            crate::completion::the_class_after_the_operator(
                index,
                scopes,
                root,
                path,
                &inner_type,
                true,
            )
        });
        let reached_by_an_arrow = reached_by_an_arrow.flatten();
        let Some(class) = reached_by_an_arrow
            .clone()
            .map(|(reached, _)| reached)
            .or_else(|| member_access_class(&inner_type))
        else {
            return Known::Unknown(UnknownReason::UnknownType(Box::from(
                inner_type.to_string(),
            )));
        };

        // **The class as the declaration spelled it, and then as the name that spelling resolves to.** A type
        // written inside a namespace is spelled there *without* its qualifier — MSVC's `<iostream>` writes
        // `extern istream cin;` inside `std` — so a member lookup for `std::cin.read` is handed `istream` while the
        // class it needs is `std::istream`. C++ resolves that spelling in the scope the declaration is in; this
        // asks the index for the **name** from the file the declaration is in, which is the same lookup whenever
        // that file sees exactly one such name — and no answer at all (with the first reason kept) when it sees
        // several, which is the direction this layer fails in.
        //
        // **The arguments the use wrote**, which is what turns a member's own _Ty& into int&: the parameter names
        // are the declaring class's ([DeclFact::parameters]) and the arguments are the object type's, and the pairing
        // is positional — see [crate::TypeBindings].
        //
        // **And when the use wrote none, the alias it named may have carried them.** `std::string` has no
        // arguments of its own — it is one name — and the class it resolves to was written with three:
        // `basic_string<char, char_traits<char>, allocator<char>>`. Without them the pairing is empty, `_Elem` has
        // nothing to be replaced by, and `x.back()` on a `std::string` answers `_Elem&`: measured, and it is the
        // last step between that and `char&`. See [`member_access_class_and_arguments`].
        let (class, arguments) = member_access_class_and_arguments(
            shapes,
            index,
            scopes,
            root,
            path,
            &class,
            &inner_type,
        );
        // **The pointee, by the handle's own `element_type` — substituted.** `std::shared_ptr` declares
        // `using element_type = _Tp;`, so the member is `_Tp` until the object type's argument is paired with it
        // (`shared_ptr<Node>` → `Node`). Asked first, because it is the protocol and it knows the arguments; the
        // completion's resolver is the fallback for the shapes it covers (`p->` on a real pointer, an `operator->`
        // whose return type is a class).
        let (class, arguments) = match inner.arrow {
            true => the_pointee_of_a_handle(index, scopes, root, path, &class, &arguments)
                .or_else(|| {
                    reached_by_an_arrow
                        .clone()
                        .map(|(reached, followed_a_pointer)| {
                            // **A class the arrow did not follow is the type's own class**, and its arguments are
                            // still the ones the member's spelling is written in. Dropping them made `it->second` on
                            // the element `std::pair<const std::string, std::string>` answer **`B`** — the letter
                            // MSVC's `pair` uses for its second parameter — because the member walk was handed the
                            // class with no arguments to pair with it. Measured on
                            // `CodeFormatCore/src/Config/LuaStyle.cpp:20`.
                            if followed_a_pointer {
                                (reached, Vec::new())
                            } else {
                                (reached, arguments.clone())
                            }
                        })
                })
                .unwrap_or((class, arguments)),
            false => (class, arguments),
        };
        // The arguments as **spellings**, which is what a pattern comparison is: a pattern is what the other
        // declaration wrote, and two spellings of one type would not compare equal. The direction this rule fails in
        // is "no match", which keeps an answer rather than removing one.
        let written_arguments: Vec<String> = arguments
            .iter()
            .map(|argument| argument.to_string())
            .collect();
        let found = memo.member(&class, &inner.member, || {
            member_fact(
                shapes,
                index,
                scopes,
                root,
                path,
                &class,
                &inner.member,
                Some(&written_arguments),
            )
        });
        let found = match found {
            Known::Yes(found) => Known::Yes(found),
            Known::Unknown(reason) => match declared_class(index, &declared_in, &class) {
                Some(qualified) => {
                    match memo.member(&qualified, &inner.member, || {
                        member_fact(
                            shapes,
                            index,
                            scopes,
                            root,
                            path,
                            &qualified,
                            &inner.member,
                            Some(&written_arguments),
                        )
                    }) {
                        Known::Yes(found) => Known::Yes(found),
                        Known::Unknown(_) | Known::No => Known::Unknown(reason),
                    }
                }
                None => Known::Unknown(reason),
            },
            Known::No => Known::No,
        };
        let Known::Yes((fact, file)) = found else {
            let Known::Unknown(reason) = found else {
                unreachable!("the first match established that this is an `Unknown`")
            };
            return Known::Unknown(reason);
        };

        // **Which question is being asked depends on where the cursor is, and both are real.**
        //
        // ```text
        // `v.data`    a member function named on its own   a function has no type *as a name*   nothing to give
        // `v.data()`  the callee of a call                 what a call of it has                the return type
        // `v.front`   a data member                        what its declaration says           `_Ty&`, substituted
        // ```
        //
        // A cursor inside `data` in `v.data()` is on the **callee**, and every consumer asking about it — a hover,
        // an inlay hint — wants the type of `v.data()`. Measured before this existed: the four offsets of `data`
        // answered `UnknownType("v.data")` while the `(` one byte later answered `int*`, so the answer depended on
        // which byte of one name the user was pointing at.
        let arguments = arguments.to_vec();
        let bindings = member_bindings(index, scopes, root, path, &class, &arguments);

        let being_called = expression.parent().is_some_and(|parent| {
            cpp_parser::CppSyntaxKind::from(parent.kind()) == cpp_parser::CppSyntaxKind::CallExpr
        });

        // **A member of a class template is finished here**, and this is the one place it can be: the argument list
        // is the object type's, which is what this arm just computed, and the parameter names are the declaring
        // class's. `v.front` on a `std::vector<int>` is `_Ty&` in the header and `int&` here — without this the
        // answer is a type called `_Ty`, which is not a class, so nothing can follow it.
        //
        // **A member whose type is another member of the same class**: `_Ty& reference; reference front;`. The
        // substitution has nothing to replace in `reference` — that spelling does not mention `_Ty` — so the name
        // is resolved as a member of the *same* class with the same argument pairing, and the walk repeats while
        // the type is still a bare name.
        //
        // A chain of two is real and common: `size_type` → `size_t` is how every standard container declares
        // `size()`, `capacity()`, `max_size()`. The walk is bounded by [`MAX_NESTED_TYPE_DEPTH`] rather than by an
        // argument about termination, because the argument is nearly — not exactly — true: each step asks about a
        // *different* name and a class has finitely many members, but two members can name each other (`a b; b a;`
        // is not valid C++ and *is* writable text), and a walk that trusted validity would not terminate on the
        // file a language server has to survive.
        //
        // **A call's return type is finished the same way**, which is what the closure is for: `x.back()` answers
        // `reference` in MSVC's `<xstring>` (`typedef _Ty& reference;`), and that is one more bare name standing for
        // a member of the same class. Without the second use of this walk, `auto y = x.back()` recorded the *name*
        // `reference` as `y`'s type — measured, and it is what a reader saw when hovering an `auto` variable: a type
        // that is not a type, on a declaration the compiler types as `char&`.
        if let Some(type_of) = fact.type_of.as_deref() {
            // **An alias template at the head of the spelling is followed here, and this is the exit that needed it.**
            //
            // A member whose type is `enable_if_t<__is_same(int, int), double>` answered with exactly that spelling:
            // the reader parsed the declaration's own text and returned it, and no step in between knew that the
            // head of it is an alias. Following it turns the type into `typename enable_if<__is_same(int, int),
            // double>::type` — a member of a **class template applied to arguments** — and the walk below is the one
            // that resolves such a member, by the pattern comparison that folds a decidable builtin to the `true`
            // the library wrote its `enable_if` for.

            if let Some((named, member)) = the_alias_behind(index, &file, type_of) {
                // **The chain is what has already been *resolved*, not what is about to be asked.** Pushing the member
                // here made the walk refuse its first question — `asked.contains("type")` — and the answer came back as
                // the member's own name (measured: `Some("type")` where the file means `double`).
                let chain = vec![fact.name.to_string()];
                // **The bindings of the class the member was found in**, not of the class the *use* names: the walk is
                // about to read `enable_if<…, double>::type`, whose declaration says `_Ty`, and `_Ty` is `double` here
                // because that class was applied to `double`. Passing the object's bindings (`Widget`'s, which pair
                // nothing with nothing) left the parameter's own name standing — measured: `Some("_Ty")`.
                // **The arguments split where the *language* splits them.** `Type::arguments` (from
                // `parse_type_spelling`) read `__is_same(int, int)` as two arguments — it does not count parentheses —
                // so the substitution paired `_Ty` with `int)` and the answer was `Some("int)")`. `split_arguments`
                // counts brackets and parentheses, which is the rule the pattern comparison already uses; the class
                // name is the bare one, because a class's parameters are filed under it.
                let inner = named
                    .split_once('<')
                    .map(|(_, rest)| rest.trim_end().strip_suffix('>').unwrap_or(rest))
                    .unwrap_or_default();
                let argument_types: Vec<crate::sema::types::Type> =
                    crate::sema::builtins::split_arguments(inner)
                        .iter()
                        .map(|one| crate::sema::types::parse_type_spelling(one))
                        .collect();
                let bare_named = named
                    .split_once('<')
                    .map(|(head, _)| head.trim())
                    .unwrap_or(named.as_str());
                let trait_bindings =
                    member_bindings(index, scopes, root, path, bare_named, &argument_types);
                let finished = finish_a_nested_name(
                    index,
                    scopes,
                    root,
                    path,
                    &named,
                    &trait_bindings,
                    parse_type_spelling(&member),
                    file,
                    chain,
                );

                return Known::Yes(finished);
            }

            let finished = bindings.applied_to(&parse_type_spelling(type_of));
            return Known::Yes(finish_a_nested_name(
                index,
                scopes,
                root,
                path,
                &class,
                &bindings,
                finished,
                file,
                vec![fact.name.to_string()],
            ));
        }

        // No `type_of`: the declaration is a function (or a class). For a function being **called**, the answer is
        // what the call has; for one merely named, there is no type and saying so is the honest answer.
        if being_called && let Some(returns) = what_a_call_has_in(&fact) {
            return Known::Yes(finish_a_nested_name(
                index,
                scopes,
                root,
                path,
                &class,
                &bindings,
                bindings.applied_to(&returns),
                file,
                vec![fact.name.to_string()],
            ));
        }

        return Known::Unknown(UnknownReason::UnknownType(Box::from(written)));
    }

    Known::Unknown(UnknownReason::UnknownType(Box::from(written)))
}

/// The type **every** visible declaration of `name` agrees on — the declaration to read it from, and its file.
///
/// The one question a type query can still answer when a *name* query cannot: `Ambiguous` means several
/// declarations are visible, and "which one is it" has no answer, while "what type does it have" does whenever
/// they all say the same thing — which redeclarations do by construction
/// (`extern istream cin;` written twice in MSVC's `<iostream>`, once of them under `extern "C++"`).
///
/// `None` when there is nothing visible, when any candidate is a function or a class (neither has a type *as a
/// name* — see [`DeclFact::returns`]), or when two candidates spell the type differently. A disagreement is
/// exactly what `Ambiguous` was about, so it is not resolved here.
pub(crate) fn agreeing_type(
    index: &ProjectIndex,
    path: &Path,
    name: &str,
) -> Option<(DeclFact, PathBuf)> {
    let candidates = index.files_declaring(name, path);
    let mut agreeing: Option<(DeclFact, PathBuf)> = None;

    for found in &candidates {
        // **A class's type is the class**, and reading `type_of` alone said otherwise.
        //
        // `DeclFact::type_of` is what a declaration wrote *as a type of something else* — a variable's type, an
        // alias's target — and a class has none, so the `?` below returned `None` for the whole list the moment one
        // candidate was a class. Measured on the report that started this: `std::vector` is declared thirteen times
        // (the primary template, `vector<bool>`, the deduction guides), every one of them a class, and
        // `auto x = std::vector<int>();` was refused as `UnknownType("std::vector<int>")` while `ns::Box<int>()` —
        // **one** declaration, in a fixture — deduced. The candidates were never disagreeing; they were being read
        // with a field that is not the answer for a class.
        //
        // A class candidate therefore agrees with every other class candidate **of the same qualified name**, which
        // is what makes `std::vector` and its partial specialisations one answer rather than thirteen.
        let type_of = match found.fact.kind {
            crate::DeclKind::Type => found.fact.qualified_name(),
            _ => found.fact.type_of.clone()?.to_string(),
        };

        match &agreeing {
            None => agreeing = Some((found.fact.clone(), found.file.clone())),
            Some((first, _)) if first.type_of.as_deref() == Some(type_of.as_str()) => {}
            Some((first, _))
                if first.kind == crate::DeclKind::Type && first.qualified_name() == type_of => {}
            Some(_) => return None,
        }
    }

    agreeing
}

/// The longer spelling a bare type name resolves to **where it was declared** — `Widget` → `lib::Widget`.
///
/// `None` in the three cases where the spelling as written is all this layer has: it is already qualified (a claim
/// about where the name lives, so there is nothing to look up), nothing declares it, or **more than one**
/// declaration is visible from the declaring file — where the honest answer is the caller's first reason rather
/// than one of the candidates.
///
/// The same one step of the same rule [`resolved_in_the_enclosing_scopes`] applies to a base clause, and the same
/// reason it exists: a name written inside a namespace is spelled without that namespace, and a lookup that stopped
/// at the spelling found `std::_Tree` neither as a base of `std::map` nor as the class of `std::cin`.
pub(crate) fn declared_class(
    index: &ProjectIndex,
    declaring: &Path,
    class: &str,
) -> Option<String> {
    if class.contains("::") {
        return None;
    }

    // **An alias template's name is not the class it stands for, and this is where that matters.**
    //
    // `enable_if_t<__is_same(int, int), double>` is a member's *type*, and the walk below asks this function which
    // class that spelling names. The index files an alias template under its bare name — `enable_if_t` — so the
    // lookup with the whole spelling answered nothing, the alias was never followed, and a member whose type is a
    // constrained alias came back as the spelling it was written with. That is the whole of what blocked
    // `enable_if`: the class on the other side of the alias is `enable_if<__is_same(int, int), double>`, and only
    // once *that* is the name being asked about does the specialization get chosen — by the pattern comparison,
    // which folds a decidable builtin to the `true` the library wrote its `enable_if` for.
    //
    // The arguments are substituted into the alias's own target here rather than in the sharing function below
    // (`alias_target_of`), because this path has no tree: the alias is a fact, its parameters are a field, and the
    // target is a spelling. Nothing else about the lookup changes.
    if let Some((head, rest)) = class.split_once('<')
        && let Some(inside) = rest.trim_end().strip_suffix('>')
        && let Known::Yes(found) = index.definition(head.trim(), declaring)
        && found.fact.kind == crate::DeclKind::Type
        && let Some(target) = found.fact.type_of.as_deref()
    {
        let written = crate::sema::builtins::split_arguments(inside);
        let target =
            crate::sema::types::substituted_spelling(target, found.fact.parameters(), &written);
        if let Some(named) = crate::sema::types::parse_type_spelling(&target).class_name() {
            return Some(named.to_string());
        }
    }

    match index.definition(class, declaring) {
        Known::Yes(found) => {
            let qualified = found.fact.qualified_name();
            (qualified != class).then_some(qualified)
        }
        Known::Unknown(_) | Known::No => None,
    }
}

/// The operand of the unary expression this node is, when its operator is `operator`.
///
/// `*p` and `&x` are `UnaryExpr` — the node every unary operator gets — so what makes one a dereference is the
/// **operator**, exactly as it is for a member access (`w.size` and `arr[0]` share a node kind too). The operator
/// is matched by text rather than by token kind for the same reason: a shape reader that had to know the token's
/// name would be a second place to update if the name changed.
///
/// `-x` and `!x` are `UnaryExpr`s as well and are deliberately not this: what they have is not a pointer.
/// **A binary expression's operator and its two operands**, or None when this is not one.
///
/// The node is left op right, so the operator is the one token between the two expression children — found by
/// position rather than by spelling, because >> is one token here and > > would be two other readings.
pub(crate) fn binary_operands_of(
    node: &cpp_parser::CppSyntaxNode,
) -> Option<(String, cpp_parser::CppSyntaxNode, cpp_parser::CppSyntaxNode)> {
    if cpp_parser::CppSyntaxKind::from(node.kind()) != cpp_parser::CppSyntaxKind::BinaryExpr {
        return None;
    }

    let mut operands = Vec::new();
    let mut operator: Option<String> = None;
    for element in node.children_with_tokens() {
        // A child that is not a token is an operand; the first token that is not trivia is the operator. The two
        // are told apart by `into_token`, which is this crate's idiom for the question — see `sema::semantic`.
        if let Some(token) = element.as_token() {
            if operator.is_none()
                && !matches!(
                    cpp_parser::CppTokenKind::from(token.kind()),
                    cpp_parser::CppTokenKind::Whitespace | cpp_parser::CppTokenKind::Newline
                )
            {
                operator = Some(token.text().to_string());
            }
        } else if let Some(child) = element.into_node() {
            operands.push(child);
        }
    }

    match (operator, operands.as_slice()) {
        (Some(operator), [left, right]) => Some((operator, left.clone(), right.clone())),
        _ => None,
    }
}

/// **The view a `range | adaptor` names** — `numbers | std::views::filter(pred)` is a `filter_view`, and the standard
/// fixes every piece of it.
///
/// # What the standard already says, and what this reads
///
/// ```text
/// numbers | std::views::filter(pred)      std::ranges::filter_view<views::all_t<R>, Pred>
/// numbers | std::views::take(2)           std::ranges::take_view<views::all_t<R>>
/// numbers | std::views::reverse           std::ranges::reverse_view<views::all_t<R>>
/// ```
///
/// So the answer needs three things, and only the third is an inference:
///
/// * **which view class** — a closed table, the standard's own (`filter`, `transform`, `take`, `drop`, `reverse`,
///   `take_while`, `drop_while`), keyed on the adaptor's **spelling**, which must contain `views::` so that a
///   project's own `filter(x)` is not one of them;
/// * **whether it carries the callable** — a predicate adaptor's second template argument *is* the callable, and the
///   others have none: `take_view`'s count is a constructor argument, not a template one;
/// * **`views::all_t<R>`** — `ref_view<R>` for an lvalue that is not itself a view, `owning_view<R>` for an rvalue,
///   and the view unchanged for the result of another adaptor.
///
/// # What is still not answered
///
/// A closure **on its own** — `std::views::filter(pred)` before a pipe — has the library's internal type
/// (`__adaptor::_Partial<std::ranges::views::_Filter, Pred>`), whose second argument is a closure type nothing can
/// spell; this rule answers only where the standard names the result. And a callable this rule cannot describe —
/// anything but a lambda written at the call — leaves the answer `None` rather than a view with a made-up argument.
#[allow(clippy::too_many_arguments)]
fn the_view_a_pipeline_names(
    memo: &TypeMemo,
    shapes: &crate::sema::declarations::ShapeTable,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    left: &cpp_parser::CppSyntaxNode,
    right: &cpp_parser::CppSyntaxNode,
    depth: usize,
) -> Option<(Type, PathBuf)> {
    let (view, carries_the_callable, callable) = the_adaptor_a_use_names(right)?;
    // **`views::all_t<R>`.** The left operand's type first; a pipeline is already a view because that is what the
    // call it stands for returns (checked by its spelling, which this layer wrote itself for the inner pipeline).
    let (left_type, file) = match type_of_expression_memoed(
        memo,
        shapes,
        index,
        declaring,
        scopes,
        root,
        path,
        left,
        depth + 1,
    ) {
        Known::Yes(found) => found,
        _ => return None,
    };
    let written = left_type.to_string();
    let all = match is_a_view_spelling(&written) {
        true => left_type,
        false => {
            // **The value category is the left operand's, not the type's.** A variable names an lvalue, and
            // `views::all_t` borrows it (`ref_view`); a call's result is a prvalue, and `all_t` *owns* it
            // (`owning_view`). Both are the standard's answers for the operand they are given.
            let borrowed = matches!(
                cpp_parser::CppSyntaxKind::from(left.kind()),
                cpp_parser::CppSyntaxKind::IdentifierExpr | cpp_parser::CppSyntaxKind::IndexExpr
            ) || (cpp_parser::CppSyntaxKind::from(left.kind())
                == cpp_parser::CppSyntaxKind::UnaryExpr
                && left.text().to_string().trim_start().starts_with('*'));
            let of = match borrowed {
                true => "std::ranges::ref_view",
                false => "std::ranges::owning_view",
            };
            Type::Named {
                name: of.to_string(),
                arguments: vec![left_type],
            }
        }
    };
    let mut arguments = vec![all];
    if carries_the_callable {
        arguments.push(callable?);
    }
    Some((
        Type::Named {
            name: view.to_string(),
            arguments,
        },
        file,
    ))
}

/// **The view class an adaptor use names, and the callable it carries** — `None` for anything that is not one of the
/// standard's adaptors.
///
/// The spelling is what decides, and it must contain `views::`: `numbers | filter(x)` is a project's own pipe, and
/// answering `filter_view` for it would be a type from another library. The callable is read only when it is a
/// **lambda written at the call** — a lambda has no name, so it is described by where it is
/// (`(lambda at a.cpp:12:34)`), which is the one spelling that designates it; a named function's type is a function
/// reference, which is a second deduction this rule does not do.
fn the_adaptor_a_use_names(
    right: &cpp_parser::CppSyntaxNode,
) -> Option<(&'static str, bool, Option<Type>)> {
    let kind = cpp_parser::CppSyntaxKind::from(right.kind());
    // `std::views::take(2)` — a call whose callee names the adaptor, and whose arguments are the adaptor's own.
    let (callee, arguments) = match kind {
        cpp_parser::CppSyntaxKind::CallExpr => {
            let mut arguments: Vec<cpp_parser::CppSyntaxNode> = Vec::new();
            let mut callee = None;
            for child in right.children() {
                match callee {
                    None => callee = Some(child),
                    Some(_) => arguments.push(child),
                }
            }
            (callee?, arguments)
        }
        _ => (right.clone(), Vec::new()),
    };
    let spelling = callee.text().to_string();
    let spelling = spelling.trim();
    if !spelling.contains("views::") {
        return None;
    }
    let written = spelling
        .rsplit("::")
        .next()
        .unwrap_or(spelling)
        .trim()
        .to_string();
    let (view, carries) = match written.as_str() {
        "filter" => ("std::ranges::filter_view", true),
        "transform" => ("std::ranges::transform_view", true),
        "take" => ("std::ranges::take_view", false),
        "drop" => ("std::ranges::drop_view", false),
        "reverse" => ("std::ranges::reverse_view", false),
        "take_while" => ("std::ranges::take_while_view", true),
        "drop_while" => ("std::ranges::drop_while_view", true),
        _ => return None,
    };
    if !carries {
        return Some((view, false, None));
    }
    // The callable is the adaptor's *first* argument (`take_while`'s predicate), and a lambda is the shape this
    // describes.
    let callable = arguments.first().and_then(a_lambdas_spelling)?;
    Some((view, true, Some(callable)))
}

/// **How a lambda is spelled when it is a type argument** — `(lambda)`.
///
/// A closure type has no name, and this says what kind of type the argument is without inventing one: the second
/// template argument of `filter_view` *is* the closure type of the predicate, and "(lambda)" is the honest description
/// of it. It is never a declaration's type — `auto g = [](int a) { … }` still answers "a lambda's closure type".
///
/// # Why not the position
///
/// `(lambda at a.cpp:7:17)` was written first and it was **context-dependent**, which was measured: the answer needs
/// the file's text, this layer has none, and asking the session for the view makes the answer depend on *who asked*.
/// The index pass resolves names through `&mut |_| None`, so the first query in that process answered `(lambda)` —
/// and `type_of` **remembers** an expression's answer in the index, so every later query in the same process was
/// served the fallback: `inspect inlay` alone printed `(lambda at a.cpp:7:17)`, and the same command with `--index`
/// printed `(lambda)`. A type that changes with the order of the questions is not a type; `(lambda)` is the same
/// answer from every caller.
fn a_lambdas_spelling(node: &cpp_parser::CppSyntaxNode) -> Option<Type> {
    if cpp_parser::CppSyntaxKind::from(node.kind()) != cpp_parser::CppSyntaxKind::LambdaExpr {
        return None;
    }
    Some(Type::named("(lambda)"))
}

/// **Is this spelling one this layer writes for a view?** — `std::ranges::filter_view<…>`.
///
/// The test is deliberately narrow: a view *class* from a header is not recognised here (it would need the class's
/// bases — `view_interface`), and treating one as a range value would add a `ref_view` around something that is
/// already a view. Only the spellings this rule and `element_of_a_range_expression` write are accepted.
fn is_a_view_spelling(written: &str) -> bool {
    let written = written.trim();
    written.starts_with("std::ranges::") && written.contains("_view<")
}

/// **What a class's `operator<name>` returns** — the member's own return type, read from the file that declares the
/// class, which is the only place its member list can be read from.
///
/// `None` for a class that declares no such operator, for one nobody declared (a template parameter, a name from a
/// header the project never read), and for an operator whose declaration did not spell a return type. Every `None`
/// means "this layer has no answer", never "the program has no answer".
pub(crate) fn operator_returns_of(
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class_type: &Type,
    operator: &str,
) -> Option<String> {
    operator_returns_of_with_arguments(
        index,
        declaring,
        scopes,
        root,
        path,
        class_type,
        operator,
        &[],
        None,
    )
}

/// [`operator_returns_of`] with **what the call passed**, which a member whose return type is `auto` needs: the
/// body may name the operator's own template parameters (`wrapper<Pred>`), and only the argument types say what
/// they stand for. See [`deduced_returns_of_the_member`].
pub(crate) fn operator_returns_of_with_arguments(
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class_type: &Type,
    operator: &str,
    argument_types: &[crate::sema::types::Type],
    // **How many arguments the call wrote** — the types can be fewer, because a lambda's type is unnamed and a
    // conversion that failed records nothing. The count is what picks an overload; the types are what fill it in.
    argument_count: Option<usize>,
) -> Option<String> {
    let class = class_type.class_name()?.to_string();

    // **A name that is an alias is not a class, and asking for its operator parses a file.**
    //
    // `files_declaring` answers with the declarations of the *name*, and the next step asks the session for the view
    // of the file the first one is in — a parse of that file. For `std::size_t` that declaration is a typedef in a
    // system header, so every binary expression with a `std::size_t` operand paid for that header's tree before the
    // answer ("a typedef has no `operator>`") was known.
    //
    // [`DeclFact::type_of`] is what tells the two apart: a class definition declares no type of its own, an alias
    // does. An alias's operators are its target's, which this cannot answer without resolving the target first — and
    // the callers answer a comparison `bool` and an arithmetic its operand's type, which is right for every
    // arithmetic alias.
    //
    // Measured on EmmyLuaCodeStyle's `LuaParser/src/File/LuaSource.cpp`: `implicit-bool-conversion`
    // **265 → 93 ms**.
    let declared = index.files_declaring(&class, path);
    let first = declared.first()?;
    if let Some(target) = first.fact.type_of.as_deref() {
        // **An alias is not a class, and its operator is the target's.** `nlohmann::json` is `using json =
        // basic_json<>;`, and the eight `operator[]` overloads the report needs are `basic_json`'s — stopping here is
        // what answered `UnknownType("json[\"data\"]")` while `members_of("nlohmann::json")`, which follows the alias,
        // listed 242 members.
        //
        // **One step, read out of the declaration's own `type_of`** — no file is read and no search is made, which is
        // the cost the note below was written for: what that measurement paid for was a *search* for `std::size_t`'s
        // operators, not the one substitution that turns an alias into the class it names. The recursion ends at a
        // class (its declaration carries no `type_of`), and a name that resolves to itself ends it here.
        let alias_arguments: Vec<String> =
            class_type.arguments().iter().map(Type::to_string).collect();
        let target = crate::sema::types::substituted_spelling(
            target,
            first.fact.parameters(),
            &alias_arguments,
        );
        let target_type = crate::sema::types::parse_type_spelling(&target);
        // **The alias's target does not carry the object's qualifier, and the recursion needs it.** `const
        // std::string& s` resolved to `basic_string<char, …>` — unqualified — so the recursive call saw a non-const
        // object and chose the non-const `operator[]`: `s[0]` answered `char&` where the compiler answers
        // `const char&`. A qualifier is written on the *use*, so it is put back on the target.
        let target_type = if class_type.is_const() && !target_type.is_const() {
            crate::sema::types::Type::Qualified {
                of: std::sync::Arc::new(target_type),
                qualifiers: crate::sema::types::Qualifiers::CONST,
            }
        } else {
            target_type
        };
        if target_type
            .class_name()
            .is_some_and(|named| named != class.as_str())
        {
            // **The alias's namespace is the target's.** `namespace nlohmann { using json = basic_json<>; }` —
            // `basic_json` means `nlohmann::basic_json`, and the two queries below are not the same question about the
            // two spellings: `files_declaring` searches by **name** and answers the unqualified one, while
            // `declarations_in` — the scope posting `member_candidates` needs — is keyed by the **qualified** name.
            // Measured: `member_candidates("basic_json", "operator[]")` answered **0 candidates** (so the overload
            // ranking never ran and the first member was taken), while `members_of("basic_json")` listed all 242
            // members of the class.
            //
            // C++ would look `basic_json` up in `nlohmann` first, so the qualified spelling is tried first and the
            // unqualified one is kept as the fallback — an alias whose target is a class at file scope has no such
            // qualifier, and qualifying it would find nothing.
            let qualified = match (
                class
                    .rsplit_once("::")
                    .map(|(qualifier, _)| qualifier.to_string()),
                target_type.class_name(),
            ) {
                (Some(qualifier), Some(bare)) if !bare.contains("::") => {
                    let mut named = target_type.clone();
                    if let crate::sema::types::Type::Named { name, .. } = &mut named {
                        *name = format!("{qualifier}::{bare}");
                    }
                    Some(named)
                }
                _ => None,
            };
            for candidate in qualified.iter().chain(std::iter::once(&target_type)) {
                if candidate
                    .class_name()
                    .is_some_and(|named| named == class.as_str())
                {
                    continue;
                }
                if let Some(answer) = operator_returns_of_with_arguments(
                    index,
                    declaring,
                    scopes,
                    root,
                    path,
                    candidate,
                    operator,
                    argument_types,
                    argument_count,
                ) {
                    return Some(answer);
                }
            }
        }
        return None;
    }
    let class_file = Some(first.file.clone());

    // **The single member first, and the whole list only as a fallback.** What this question wants is one
    // declaration's `returns`, and a member list is a far more expensive way to find it: building one reads every
    // member of the class and walks its bases, while `member_fact` asks the index's own scope postings. Measured on
    // `LuaSource.cpp`, where fourteen subscript expressions ask about `std::vector<…>::operator[]`: the member list
    // was **24 ms** each and the listing repeated fourteen times, for **340 ms of a 376 ms** `assign-type-check`
    // pull. The list stays as the fallback because it also answers for a member whose name the postings spell
    // differently (a base's, an operator's without its `operator` prefix).
    //
    // The two spellings are the two a fact can carry for an operator: the declarator's own (`operator[]`) and the
    // bare symbol a reader that recorded only the operator token answered with (`[]`).
    //
    // **A `Yes` ends the question, `None` or not.** The list below is built in the same order this query walks —
    // the class's own declarations first, then each level of bases — so the declaration this found is the one the
    // list's own `find` would return first, and asking the list again can only produce the same `returns`. That is
    // what makes the whole list unnecessary whenever the narrow query hits, which is every operator on every class
    // the index has read.
    let wanted: String = operator
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let symbol = wanted.strip_prefix("operator").unwrap_or(&wanted);
    // **The class's own parameter names, and the arguments written at the use** — what a return spelling like `T&`
    // has to be read through before it is a type. `member_fact` answers the *fact*, which carries the declaration's
    // own words (`T& operator[]`); the list fallback below substitutes them, so this path has to as well or the same
    // question answers `T&` here and `int&` there.
    // **The parameters of the declaration that has any.** `first` is the class's first declaration in insertion
    // order, and for a class template that is as often a forward declaration as the definition — measured on MSVC's
    // `std::shared_ptr`, whose first fact carries no parameter list at all, so `_Ty2 *` came back unsubstituted and a
    // member lookup was asked about a class called `_Ty2`. `template_parameters_of` already answers this question the
    // right way for the rest of the analysis (it skips empty lists), so the two cannot disagree.
    let written_arguments: Vec<String> = class_type
        .arguments()
        .iter()
        .map(|argument| argument.to_string())
        .collect();
    // **The declarations that match this use**, not just "the first one with parameters" — see
    // [`ProjectIndex::template_parameters_for`].
    let parameters = index.template_parameters_for(&class, path, &written_arguments);
    let parameter_names = &parameters;
    if crate::probes::inference() {
        eprintln!(
            "[inference] operator_returns_of({class}, {operator}): parameters {parameters:?}, arguments {written_arguments:?}"
        );
    }
    // **The class as a spelling a member query can be asked with** — a reference or a qualifier is the *binding's*
    // business, not the class's, and asking `members_of` about `std::vector<MarkEvent>&` finds no class of that name.
    // Measured on `EmmyLuaCodeStyle`'s `Mark.cpp`: `auto &events = p.GetEvents(); events[Pos]` answered
    // `Unknown(UnknownType("events[Pos]"))` for a `std::vector<MarkEvent>`, because the object's type carries the `&`
    // that `auto &` bound and both queries below were spelled with it.
    let class_spelling = the_class_spelling(class_type);
    // **The overload the call fits.** `member_fact` answers the *first* declaration of a name, and an adaptor
    // declares the two-argument form first:
    //
    // ```cpp
    // struct _Filter_fn {
    //     template <class Rng, class Pred> auto operator()(Rng&& rng, Pred&& pred) const { return view{}; }
    //     template <class Pred>            auto operator()(Pred&& pred) const { return range_closure<…>{}; }
    // };
    // auto c = filter([](int v) { return v > 0; });   // the first declaration's return type — `view`
    // ```
    //
    // — measured, and passing the argument count through to `member_fact` does not change the choice. So when the
    // caller knows what the call passed and the first fact's parameter count disagrees, the member list is asked for
    // the one whose count agrees. A call whose arity matches nothing keeps the first fact, which is the answer this
    // arm gave before.
    let arity = argument_count.or((!argument_types.is_empty()).then_some(argument_types.len()));
    // **The qualifier the object has**, for the choice among overloads of one name — see
    // [`a_member_with_that_arity`] and [`Type::is_const`].
    let object_is_const = class_type.is_const();
    // **The arguments choose among overloads, and the ranking that does it already exists.**
    //
    // `struct S { int get(int); double get(const char*); };` — `s.get("z")` answered `int` and `s["z"]` answered
    // `int&`, both the *first* declaration, because a member lookup's rule is "a jump takes the first, a list takes
    // them all" and a call is neither. `the_one_the_arguments_select` ranks free functions exactly this way
    // (`crate::sema::overload`); what it was missing is the candidate list, which is `member_candidates` — one posting
    // query for the name, not the member-list walk of every base.
    //
    // **Asked before the arity choice, not after it.** The arity choice builds a member list (`members_of`), so
    // asking it first and then overriding the answer would pay for both. It is also what makes this a *saving* on the
    // shape that used to pay the list: the ranking is facts, and the list is only asked when nothing was ranked.

    let ranked = (!argument_types.is_empty())
        .then(|| {
            let declared = crate::index::project::member_candidates(
                &crate::sema::declarations::ShapeTable::new(),
                index,
                scopes,
                root,
                path,
                &class_spelling,
                &wanted,
                Some(&written_arguments),
            );
            // **A `const` object can only call a `const` member**, which is the same rule the arity preference below
            // applies — a candidate it could not call is not a candidate. A non-const object may call both.
            let callable: Vec<(crate::DeclFact, std::path::PathBuf)> = declared
                .into_iter()
                .filter(|(fact, _)| {
                    !object_is_const || fact.modifiers.contains(crate::DeclModifiers::CONST)
                })
                .collect();
            if crate::probes::inference() {
                eprintln!(
                    "[rank] {class}::{wanted}: {} candidate(s): {:?}",
                    callable.len(),
                    callable
                        .iter()
                        .map(|(fact, _)| fact.parameter_list().unwrap_or("").to_string())
                        .collect::<Vec<_>>()
                );
            }
            if callable.len() < 2 {
                return None;
            }
            let types: Vec<Known<String>> = argument_types
                .iter()
                .map(|argument| Known::Yes(argument.to_string()))
                .collect();
            let at = the_one_the_arguments_select(&types, &callable, &[])?;
            // The shape the rest of this function uses: the fact, and where its body is as a **spelling** — the
            // declaration found by name carries the class it was declared in there, while a candidate carries the file.
            let (fact, file) = callable.get(at)?;
            Some((fact.clone(), file.display().to_string()))
        })
        .flatten();
    let mut matching = match (ranked, arity) {
        (Some(decided), _) => Some(decided),
        (None, Some(wanted_arity)) => a_member_with_that_arity(
            &mut *declaring,
            index,
            scopes,
            root,
            path,
            class_file.as_deref(),
            &class_spelling,
            &wanted,
            symbol,
            Some(wanted_arity),
            object_is_const,
        ),
        (None, None) => None,
    };
    // **A `const` object cannot call a non-const overload**, and the cheap query ([`member_fact`]) answers the first
    // declaration without saying what its qualifier is — so when the object is const and the declaration found is not,
    // the member list is asked for the one that fits. This is the only direction that needs the list: a non-const
    // object may call a `const` member too, so an answer found for it is never wrong.
    //
    // Measured: `const std::string& s; s[0]` answered `char&` where a compiler answers `const char&`. The subscript
    // passes no argument count, so `arity` is `None` and the two declarations — `reference operator[]` and
    // `const_reference operator[] const` — can only be told apart by the qualifier.
    if object_is_const
        && matching
            .as_ref()
            .is_none_or(|(fact, _)| !fact.modifiers.contains(crate::DeclModifiers::CONST))
    {
        matching = a_member_with_that_arity(
            &mut *declaring,
            index,
            scopes,
            root,
            path,
            class_file.as_deref(),
            &class_spelling,
            &wanted,
            symbol,
            arity,
            true,
        )
        .or(matching);
    }
    for spelling in [wanted.as_str(), symbol] {
        if let Some((fact, member_file)) = matching.clone()
            && let Some(answer) = the_members_return(
                &mut *declaring,
                index,
                scopes,
                root,
                path,
                &fact,
                // **The class's file, not the member's own "declared in" field** — a member list records the class
                // the member was found in there, not a path (measured: `"_Filter_fn"`). The member is read from the
                // file the class is declared in, which is where its body is.
                class_file
                    .as_deref()
                    .unwrap_or_else(|| Path::new(&member_file)),
                parameter_names,
                &written_arguments,
                argument_types,
            )
        {
            return Some(answer);
        }
        if let Known::Yes((fact, member_file)) = member_fact(
            &crate::sema::declarations::ShapeTable::new(),
            index,
            scopes,
            root,
            path,
            &class_spelling,
            spelling,
            None,
        ) {
            // **The spelling the fact refused, when the class is here to resolve it.** `reference operator[]` on a
            // `std::vector` has `returns: None`, because a fact's readers cannot resolve `reference` — but this
            // reader holds the class, and `a_members_answer_as_a_type` below is exactly the alias step that turns it
            // into `T&`. See [`crate::DeclFact::returns_in_the_class`].
            //
            // # What still does not resolve
            //
            // `std::vector<T>`'s `reference` is `_Alloc_traits::reference` → `__alloc_traits<_Tp_alloc_type>::reference`
            // → `value_type&`, and `_Tp_alloc_type` is an alias **of the class the walk started in** — not one of its
            // template parameters, so the substitution that pairs `_Tp` with `MarkEvent` leaves it standing, and the
            // walk has by then moved into another class. Following it needs two things this layer does not have: a
            // chain of the classes the walk visited (so a name that is not a member of the current one is looked up
            // where it was written) and `X::template rebind<T>::other` (a nested class template whose `other` is an
            // alias template's application). Measured on `EmmyLuaCodeStyle`'s `Mark.cpp`: `events[Pos]` answers
            // `_Tp_alloc_type&` — a spelling no reader can use — where `std::string`'s own `reference`, which is
            // written `_CharT&`, resolves to `char&` and works.
            // **A return type of `auto` is a body to read, not a spelling to substitute.** MSVC's and libc++'s range
            // adaptors are structs whose `operator()` is written `constexpr auto operator()(…)` with the answer in
            // the body — `return _Range_closure<_Filter_fn, decay_t<_Pr>>{…};` — so this arm used to answer the
            // *keyword* (or nothing at all when the fact recorded no return type), and every call of a niebloid
            // (`std::views::filter(pred)`, `transform(f)`) came back `UnknownType`. Measured on a fixture with the
            // same shape: `auto c = make_one(1);` had no type. See [`deduced_returns_of_the_member`].
            if let Some(answer) = the_members_return(
                &mut *declaring,
                index,
                scopes,
                root,
                path,
                &fact,
                member_file.as_path(),
                parameter_names,
                &written_arguments,
                argument_types,
            ) {
                return Some(answer);
            }
        }
    }

    let class_view = class_file.as_deref().and_then(declaring);
    let (class_scopes, class_root, class_path) = match &class_view {
        Some(view) => (view.scopes.as_ref(), &view.root, view.path.as_path()),
        None => (scopes, root, path),
    };

    let Known::Yes(list) = members_of(index, class_scopes, class_root, class_path, &class_spelling)
    else {
        return None;
    };

    // **The spelling is normalised**: the name a declaration records is whatever the declarator's tokens said, and
    // `operator *` and `operator*` are the same operator with a space in it. Comparing without whitespace makes the
    // two spellings one question, and the bare symbol is accepted as well because a reader that recorded only the
    // operator's own token answered `*`.
    //
    // **And the qualifier picks among them**, the same way [`a_member_with_that_arity`] does: a `const` object can
    // only call the `const` one, and C++ prefers it even when it could call both.
    let found = list
        .members
        .iter()
        .filter(|member| {
            let name: String = member
                .fact
                .name
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect();
            name == wanted || name == symbol
        })
        .min_by_key(|member| {
            member.fact.modifiers.contains(crate::DeclModifiers::CONST) != object_is_const
        })?;
    let returns = found
        .fact
        .returns
        .clone()
        .or(found.fact.returns_in_the_class().map(SmolStr::from))?;

    // **An operator inherited from a base answers in the base's spelling.** `template <class T> struct Base { T*
    // operator->(); }; template <class T> struct Ptr : Base<T> {};` — `Ptr<S>`'s arrow is `Base<S>::operator->`,
    // whose return type is written `T*`: read literally that is a class called `T`, which has no members, so the
    // completion after `ptr->` offered **nothing**. It has to be read through the base clause's arguments (and those
    // through the derived class's own), which is what this asks before the class's own parameters are tried.
    // Measured on that fixture: 0 items before, the pointee's members after; the same shape is libstdc++'s
    // `shared_ptr`, whose `operator->` is declared in `__shared_ptr<_Tp, _Lp>`.
    if found.declared_in != class
        && let Some(substituted) = a_base_operators_return(
            index,
            scopes,
            root,
            path,
            &class,
            &found.declared_in,
            &returns,
            parameter_names,
            &written_arguments,
        )
    {
        return Some(substituted);
    }

    Some(crate::sema::types::substituted_spelling(
        &returns,
        parameter_names,
        &written_arguments,
    ))
}

/// **What one member returns, by its written type or by its body.**
///
/// A member whose return type is written returns that spelling, substituted with the class's arguments; one whose
/// type is the placeholder `auto` — every range adaptor's `operator()` — has its body read instead. Both the
/// first-declaration path and the arity-matched path of [`operator_returns_of_with_arguments`] go through here, so
/// the two cannot disagree about the same declaration.
#[allow(clippy::too_many_arguments)]
fn the_members_return(
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    fact: &crate::DeclFact,
    member_file: &Path,
    parameter_names: &[String],
    written_arguments: &[String],
    argument_types: &[crate::sema::types::Type],
) -> Option<String> {
    let _ = (index, scopes, root, path);
    let returns = fact
        .returns
        .clone()
        .or(fact.returns_in_the_class().map(SmolStr::from));
    if returns.as_deref().is_none_or(is_a_placeholder)
        && let Some(deduced) = deduced_returns_of_the_member(
            declaring,
            fact,
            member_file,
            parameter_names,
            written_arguments,
            argument_types,
        )
    {
        return Some(deduced);
    }
    let returns = returns?;
    Some(crate::sema::types::substituted_spelling(
        &returns,
        parameter_names,
        written_arguments,
    ))
}

/// **How many parameters a fact's declaration takes**, from the names it recorded or the list it wrote.
fn a_facts_parameter_count(fact: &crate::DeclFact) -> Option<usize> {
    if !fact.parameters().is_empty() {
        return Some(fact.parameters().len());
    }
    let list = fact.parameter_list()?;
    let inside = list
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
    if inside.is_empty() || inside == "void" {
        return Some(0);
    }
    Some(inside.split(',').count())
}

/// **The member of that name whose parameter count is `arity`**, from the class's member list.
///
/// The member list rather than [`member_fact`], because that query answers the *first* declaration of a name and an
/// adaptor declares its two-argument `operator()` first — see the note at the call site.
#[allow(clippy::too_many_arguments)]
fn a_member_with_that_arity(
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class_file: Option<&Path>,
    class_spelling: &str,
    wanted: &str,
    symbol: &str,
    // **How many arguments the use wrote**, when it is known. `None` picks by qualifier alone, which is what a
    // subscript asks: its argument count is not in doubt and no overload's arity is either.
    arity: Option<usize>,
    // **Whether the object this operator is called on is `const`.** `const std::string& s; s[0]` answers
    // `const char&`, and both declarations are in the list (`reference operator[]` and `const_reference operator[]
    // const`) — so among the ones whose arity fits, the qualifier picks. A non-const object prefers the non-const
    // overload for the same reason C++ does, and a class with only one of the two still answers that one.
    object_is_const: bool,
) -> Option<(crate::DeclFact, String)> {
    let view = class_file.and_then(declaring);
    let (class_scopes, class_root, class_path) = match &view {
        Some(view) => (view.scopes.as_ref(), &view.root, view.path.as_path()),
        None => (scopes, root, path),
    };
    let Known::Yes(list) = members_of(index, class_scopes, class_root, class_path, class_spelling)
    else {
        return None;
    };
    let found = list
        .members
        .iter()
        .filter(|member| {
            let name: String = member
                .fact
                .name
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect();
            (name == wanted || name == symbol)
                && arity.is_none_or(|wanted| a_facts_parameter_count(&member.fact) == Some(wanted))
        })
        // **The one whose qualifier agrees, and the first one when none does** — `false` sorts before `true`, so an
        // agreeing declaration wins and a class with only the other qualifier keeps the answer it had.
        .min_by_key(|member| {
            member.fact.modifiers.contains(crate::DeclModifiers::CONST) != object_is_const
        })?;
    Some((found.fact.clone(), found.declared_in.clone()))
}

/// **What a member whose return type is `auto` returns**, read from its body.
///
/// MSVC's and libc++'s range adaptors are structs whose `operator()` is written `constexpr auto operator()(…)` with
/// the answer in the body:
///
/// ```cpp
/// template <class _Pr>
/// constexpr auto operator()(_Pr&& _Pred) const noexcept(…)
/// { return _Range_closure<_Filter_fn, decay_t<_Pr>>{::std::forward<_Pr>(_Pred)}; }
/// ```
///
/// so a call of one has a type the declaration never spells. The fact answers the keyword `auto` — or, for a
/// declaration whose reader could not spell it, nothing — and neither is a type. This is the same deduction a
/// *function* whose fact carries no usable return type goes through
/// ([`crate::sema::declarations::deduced_returns_of_substituted`], whose other caller is the `Indexed` arm of
/// [`what_a_call_has_in`]), applied to a member found through [`member_fact`].
///
/// The class's own template parameters are substituted from the type the call was made on. The operator's own
/// template parameters — `_Pr` above — are left as written: this layer has no call-site argument types at this
/// point, and a spelling that names them is still a class a reader can hover, which is what the stages after this
/// one build on.
fn deduced_returns_of_the_member(
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    fact: &crate::DeclFact,
    file: &Path,
    parameter_names: &[String],
    written_arguments: &[String],
    argument_types: &[crate::sema::types::Type],
) -> Option<String> {
    // **The fact's offset is a *file* offset; the tree speaks the *reading*.** The same translation the `Indexed`
    // arm of `what_a_call_has_in` makes, for the same reason: `name_range` comes from the index and counts the
    // file's own bytes, while `definition_at` walks a tree whose offsets count the rendered text (the file with its
    // includes expanded). For a file with no include the two agree, which is why this looked right; through a header
    // they do not. The reading offset is tried first and the raw one after it, because a region the rendering does
    // not contain still has to be looked at the way it was written.
    let declared_in = declaring(file)?;
    let offset = fact.name_range.start_offset;
    let found = [
        declared_in.reading_offset_of(crate::file::coordinates::FileOffset::new(offset)),
        Some(crate::file::coordinates::ReadingOffset::new(offset)),
    ]
    .into_iter()
    .flatten()
    .find_map(|in_the_reading| {
        match crate::sema::resolve::definition_at(
            &declared_in.scopes,
            &declared_in.root,
            in_the_reading.get(),
        ) {
            // **The name has to be the one the fact named.** A matched offset in the wrong coordinates lands on a
            // neighbouring declaration, and deducing *its* body would be a wrong answer that reads like a right one.
            Known::Yes(found) if found.name.identifier_text() == Some(fact.name.as_str()) => {
                Some(found)
            }
            _ => None,
        }
    });
    // **An operator's binding is not where a name's is.** `definition_at` answers for a *name*, and `operator()` is
    // a keyword and a pair of parentheses — the scope walker records the binding, but a probe on the keyword finds
    // nothing (measured: no binding in the ten bytes of `operator()`). What the deduction needs from a binding is
    // only the **name range it starts a search from** (`declaration_of` reads the token there and walks up to the
    // declaration) and the kind, so the nearest bindings are asked for one and the answer is re-pointed at this
    // declaration. The name is the fact's, which is the one thing that must not be guessed.
    let binding = match found {
        Some(binding) => binding,
        None => {
            let mut borrowed = None;
            for probe in (offset.saturating_sub(96)..=(offset + 96)).rev() {
                if let Known::Yes(found) = crate::sema::resolve::definition_at(
                    &declared_in.scopes,
                    &declared_in.root,
                    probe,
                ) {
                    borrowed = Some(found);
                }
            }
            let mut borrowed = borrowed?;
            borrowed.name = crate::symbol::Name::identifier(fact.name.as_str());
            borrowed.kind = crate::BindingKind::Function;
            borrowed.range = fact.range;
            borrowed.name_range = fact.name_range;
            borrowed
        }
    };
    // **The operator's own template parameters come from what the call passed** — `_Pr` in
    // `_Range_closure<_Filter_fn, decay_t<_Pr>>` is the argument's type, and without it the deduced spelling keeps
    // the parameter's name (`wrapper<Pred>` for a call of `make_one(1)`, measured). The class's arguments are
    // applied to the deduced spelling afterwards, because `TypeSubstitutions` pairs one list positionally and the
    // two lists are written with different names.
    let deduced_from_the_call =
        deduce_the_parameters(fact.parameters(), fact.parameter_list(), argument_types);
    let substitutions =
        crate::sema::types::TypeSubstitutions::new(fact.parameters(), &deduced_from_the_call);
    let deduced = crate::sema::declarations::deduced_returns_of_substituted(
        &declared_in.root,
        binding,
        &substitutions,
        argument_types,
    )?;
    // **The spelling the body wrote still carries the operator's parameter names.** The deduction answers with
    // what the file wrote (`wrapper<Pred>` for `return wrapper<Pred>{}`), so the argument types are substituted into
    // that spelling too — the parameter's name on its own is not a type any reader can use.
    let as_written: Vec<String> = deduced_from_the_call
        .iter()
        .map(|argument| argument.to_string())
        .collect();
    let deduced =
        crate::sema::types::substituted_spelling(&deduced, fact.parameters(), &as_written);
    Some(crate::sema::types::substituted_spelling(
        &deduced,
        parameter_names,
        written_arguments,
    ))
}

/// **The operand of a `co_await`, when this node is one.**
///
/// `co_await e` is parsed as a `UnaryExpr` whose first token is the keyword — see `exprs.rs`, where the arm exists
/// because the keyword was in `is_expression_keyword` and no rule consumed it. The operand is the node after the
/// keyword; a tree with none (a malformed `co_await` being typed at) answers `None`, and the caller falls through
/// to the shapes that do not need one.
pub(crate) fn a_co_awaits_operand(
    expression: &cpp_parser::CppSyntaxNode,
) -> Option<cpp_parser::CppSyntaxNode> {
    if cpp_parser::CppSyntaxKind::from(expression.kind()) != cpp_parser::CppSyntaxKind::UnaryExpr {
        return None;
    }
    let mut children = expression.children_with_tokens();
    let keyword = children
        .find_map(|element| element.into_token())
        .filter(|token| {
            cpp_parser::CppTokenKind::from(token.kind()) == cpp_parser::CppTokenKind::CoAwaitKeyword
        })?;
    let _ = keyword;
    children.find_map(|element| element.into_node())
}

/// **A return type that is one bare name plus its operators** — `element_type*` gives `("element_type", "*")`, and
/// `None` for anything else (a qualified name, a template-id, a reference). Only the bare shape can name a member
/// typedef of the class whose operator wrote it; anything richer is a type the substitution already handles.
pub(crate) fn a_bare_type_and_its_operators(written: &str) -> Option<(&str, String)> {
    let trimmed = written.trim();
    let name_len = trimmed
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .count();
    if name_len == 0 {
        return None;
    }
    let (name, rest) = trimmed.split_at(name_len);
    let suffix: String = rest
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if !suffix
        .chars()
        .all(|character| matches!(character, '*' | '&'))
    {
        return None;
    }
    Some((name, suffix))
}

/// **A base's operator return, read through the base clause's arguments.** `None` when the clause that names this
/// base cannot be found or its arguments cannot be read — the caller then answers with the spelling as written, which
/// is what this function refines.
pub(crate) fn a_base_operators_return(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    declared_in: &str,
    returns: &str,
    parameter_names: &[String],
    written_arguments: &[String],
) -> Option<String> {
    // The base list is asked for as a **precondition**: this returns before any substitution when the class's clause
    // cannot be read. The value is dropped on purpose — [`the_arguments_of_a_base`] below reads what is in it.
    let Known::Yes(_) = bases_of(index, scopes, root, path, class) else {
        return None;
    };
    // **The base list carries the clause's arguments as their own entries.** `bases_of(Ptr)` answers
    // `["Base", "T"]` for `struct Ptr : Base<T> {}` — the class and the names *inside* its clause, which is what the
    // lookup does with them (`_Tp` and `_Lp` really can be a base's name). So the arguments of the base we found are
    // the entries right after it, and how many there are is the base template's arity.
    // **The operator may be inherited through more than one level.** libstdc++'s `shared_ptr<_Tp> : __shared_ptr<_Tp,
    // _S_single> : __shared_ptr_access<_Tp, _Lp, false, false>` declares `operator->` two bases down, so looking only
    // at the direct base clause finds nothing and the return type stays the unsubstituted `element_type *`. The
    // substitution is therefore composed along the chain: each base clause's arguments are read through the previous
    // level's parameters.
    let (parameters, arguments) = the_arguments_of_a_base(
        index,
        scopes,
        root,
        path,
        class,
        declared_in,
        parameter_names,
        written_arguments,
        0,
    )?;
    // **The base's return type may name one of its own typedefs.** `__shared_ptr_access` writes
    // `element_type* operator->()` and `element_type` is `using element_type = _Tp;` *inside* it — so substituting
    // `_Tp` alone never reaches it: the pointee comes out as a class called `element_type`, which has no members, and
    // the completion after `shared_ptr<std::string>::operator->` is empty. A bare name is read as a member of the
    // class that declared the operator, substituted, and the operators around it put back on.
    if let Some((name, suffix)) = a_bare_type_and_its_operators(returns)
        && let Known::Yes((alias, _)) = member_fact(
            &crate::sema::declarations::ShapeTable::new(),
            index,
            scopes,
            root,
            path,
            declared_in,
            name,
            None,
        )
        && let Some(target) = alias.type_of.clone()
    {
        let substituted =
            crate::sema::types::substituted_spelling(&target, &parameters, &arguments);
        return Some(format!("{substituted}{suffix}"));
    }

    Some(crate::sema::types::substituted_spelling(
        returns,
        &parameters,
        &arguments,
    ))
}

/// **The substitution that reaches `wanted` from `class` along the base clauses** — its template parameters and their
/// arguments, composed one level at a time.
pub(crate) fn the_arguments_of_a_base(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    wanted: &str,
    parameters: &[String],
    arguments: &[String],
    depth: usize,
) -> Option<(Vec<String>, Vec<String>)> {
    if depth > 6 {
        return None;
    }
    let leaf = |spelling: &str| spelling.rsplit("::").next().unwrap_or(spelling).to_string();
    let Known::Yes(bases) = bases_of(index, scopes, root, path, class) else {
        return None;
    };
    let mut at = 0usize;
    while at < bases.len() {
        let written = &bases[at];
        let resolved = resolved_in_the_enclosing_scopes(index, scopes, path, class, written);
        let arity = index.template_parameters_for(&resolved, path, &[]).len();
        let of_the_base: Vec<String> = bases
            .iter()
            .skip(at + 1)
            .take(arity)
            .map(|argument| {
                crate::sema::types::substituted_spelling(argument, parameters, arguments)
            })
            .collect();
        let of_the_base_parameters = index.template_parameters_for(&resolved, path, &of_the_base);
        if leaf(&resolved) == leaf(wanted) {
            return Some((of_the_base_parameters, of_the_base));
        }
        if let Some(found) = the_arguments_of_a_base(
            index,
            scopes,
            root,
            path,
            &resolved,
            wanted,
            &of_the_base_parameters,
            &of_the_base,
            depth + 1,
        ) {
            return Some(found);
        }
        at += 1 + arity;
    }
    None
}

pub(crate) fn unary_operand_with(
    node: &cpp_parser::CppSyntaxNode,
    operator: &str,
) -> Option<cpp_parser::CppSyntaxNode> {
    if cpp_parser::CppSyntaxKind::from(node.kind()) != cpp_parser::CppSyntaxKind::UnaryExpr {
        return None;
    }

    let first = node.children_with_tokens().next()?;
    if first
        .as_token()
        .is_none_or(|token| token.text() != operator)
    {
        return None;
    }

    // The operand is the node after it, which is what a type is inferred *from*.
    node.children().next()
}

/// The base of a `[…]` expression, when that is what this node is.
///
/// `arr[0]` and `w.size` are both `IndexExpr` — the parser reads `w.size` as an index expression with a `.`
/// where the brackets would be — so the operator is
/// what tells them apart. A member access is not a subscript and never reaches the inference for one.
pub(crate) fn subscript_base(
    node: &cpp_parser::CppSyntaxNode,
) -> Option<cpp_parser::CppSyntaxNode> {
    if cpp_parser::CppSyntaxKind::from(node.kind()) != cpp_parser::CppSyntaxKind::IndexExpr {
        return None;
    }

    let has_brackets = node
        .children_with_tokens()
        .any(|element| element.as_token().is_some_and(|token| token.text() == "["));
    if !has_brackets {
        return None;
    }

    node.children().next()
}

/// **What stands between the brackets** — the argument an `operator[]` overload is chosen by.
///
/// [`subscript_base`] answers the object; this answers the index, and the two are the halves of one expression: on a
/// class with both `operator[](int)` and `operator[](const char*)`, `s["z"]` is the second — while a lookup that takes
/// the first declaration answers the first. See [`member_candidates`] and [`operator_returns_of_with_arguments`].
pub(crate) fn subscript_index(
    node: &cpp_parser::CppSyntaxNode,
) -> Option<cpp_parser::CppSyntaxNode> {
    if cpp_parser::CppSyntaxKind::from(node.kind()) != cpp_parser::CppSyntaxKind::IndexExpr {
        return None;
    }
    node.children().nth(1)
}
/// What a `*` on this spelling gives: `Widget*` → `Widget`, `Widget&` → `Widget`, `Widget**` → `Widget*`.
///
/// The operators written after the type are removed, and so is a cv-qualifier written after *them* — `Widget *
/// const` is a const pointer to a `Widget`, so what the `*` gives is the `Widget` and not `Widget * const`.
/// Everything else is kept as written, template arguments included: how much of a spelling names the *class* is
/// the member lookup's question, and it answers that one itself with [`base_type_name`].
///
/// `None` when there is no operator to remove, which is the honest answer for `*x` where the declaration says
/// `x` is an `int`: nothing in a declaration of `x` says otherwise, and a type invented here would be a wrong
/// answer rather than a missing one.
pub(crate) fn pointee_type_name(written: &str) -> Option<String> {
    let mut name = written.trim().to_string();

    // The qualifiers of the *pointer* come first because they are written last: `Widget* const`.
    loop {
        let trimmed = name.trim_end();
        let Some(rest) = trimmed
            .strip_suffix("const")
            .or_else(|| trimmed.strip_suffix("volatile"))
        else {
            break;
        };
        name = rest.trim_end().to_string();
    }

    let trimmed = name.trim_end();
    let stripped = trimmed
        .strip_suffix("&&")
        .or_else(|| trimmed.strip_suffix('*'))
        .or_else(|| trimmed.strip_suffix('&'))?;

    let pointee = stripped.trim().to_string();
    (!pointee.is_empty()).then_some(pointee)
}

/// The declaration an expression **names**, from the file being edited or from the index.
///
/// The reconciliation the two-layer split needs, in one place: the buffer answers with a [`crate::Binding`] whose
/// spellings are read out of the tree, while an indexed file answers with a [`DeclFact`] that carries them as
/// fields. Both are needed for the same two questions — what type does this name have, what does a call of it
/// give — and a second reconciliation would be a second rule book for the same question.
///
/// The answers are the ones the name query gives, and for the same reason: a name this file cannot place is asked
/// of the index, and a name *nothing* declares is [`UnknownType`] carrying the spelling rather than
/// [`UnknownReason::NotDeclaredHere`] — what is missing here is the type of an expression, which is a different
/// thing for a consumer to be told.
///
/// [`UnknownType`]: UnknownReason::UnknownType
// **A big variant, kept inline on purpose.** `Known` answers for every expression the engine types, and boxing the
// found case would put an allocation on the hot path of every member lookup to save the difference between 392 and
// 48 bytes in an enum that lives for the length of one question. The variants are also matched on by value in a
// dozen places, where the box would be a deref at each site.
#[allow(clippy::large_enum_variant)]
pub(crate) enum NamedDeclaration {
    /// A binding of the file being edited. Its spellings come from the tree the caller has.
    Here(crate::Binding),
    /// A declaration in an indexed file, with the file it is in — and, when it was found **through a class**
    /// that had template arguments, the pairing that finishes its type.
    ///
    /// The arguments belong to the *lookup* rather than to the declaration: `std::vector<int>::data` and
    /// `std::vector<float>::data` are one declaration whose type is `_Ty*`, and which of the two a use means is
    /// decided by the object expression. Carrying them here is what lets both questions a consumer asks — "what
    /// type does this have" and "what does a call of it give" — finish the same way, instead of only the first.
    Indexed(DeclFact, PathBuf, TypeBindings),
}

impl NamedDeclaration {
    /// Where the declaration is — the answer's own file, for a consumer that has to jump or to look further.
    pub(crate) fn file(&self, here: &Path) -> PathBuf {
        match self {
            NamedDeclaration::Here(_) => here.to_path_buf(),
            NamedDeclaration::Indexed(_, file, _) => file.clone(),
        }
    }

    /// **The kind of the thing this names**, which a caller needs to tell a member function from an object with an
    /// `operator()`. See [`type_of_a_call`]: it asked "does this callee have an `operator()`?" of a member call whose
    /// type was its *return* type, and answered the container's operator instead of the declared return.
    pub(crate) fn fact_kind(&self) -> crate::DeclKind {
        match self {
            // The two vocabularies are different enums (`BindingKind` is the symbol layer's), and the only
            // distinction this caller makes is "function or not": everything callable answers `Function` so that a
            // call of a *member function* is never mistaken for a call of an object with an `operator()`.
            NamedDeclaration::Here(binding) => match binding.kind {
                crate::sema::symbol::BindingKind::Function
                | crate::sema::symbol::BindingKind::Constructor
                | crate::sema::symbol::BindingKind::Destructor
                | crate::sema::symbol::BindingKind::ConversionFunction
                | crate::sema::symbol::BindingKind::OperatorFunction
                | crate::sema::symbol::BindingKind::LiteralOperator => crate::DeclKind::Function,
                _ => crate::DeclKind::Variable,
            },
            NamedDeclaration::Indexed(fact, _, _) => fact.kind,
        }
    }

    /// **Does this name a type?** — a class, an enumeration, an alias or a `typedef`.
    ///
    /// The question a *construction* asks of its head: `Widget{…}` and `ContainerOf<int>{…}` are temporaries of a type,
    /// and what that type is follows from the head being one. It is deliberately not [`Self::type_of`], which answers
    /// "the type this declaration was written with": an alias has one (its target) and a class does not, while both
    /// name a type, and a head that is a type has the same answer either way.
    ///
    /// A **namespace** is not a type here even though [`crate::BindingKind::is_type_like`] says it is: that predicate
    /// answers "may this name follow a `::`", and `ns{}` is not a temporary.
    pub(crate) fn names_a_type(&self) -> bool {
        match self {
            NamedDeclaration::Here(binding) => matches!(
                binding.kind,
                crate::BindingKind::Class
                    | crate::BindingKind::Enum
                    | crate::BindingKind::Alias
                    | crate::BindingKind::Typedef
            ),
            NamedDeclaration::Indexed(fact, _, _) => fact.kind == crate::DeclKind::Type,
        }
    }

    /// The type this declaration was written with.
    ///
    /// `None` for a function — `make` is not a `Widget` and has no members — which is the distinction
    /// [`DeclFact::type_of`] documents.
    pub(crate) fn type_of(
        &self,
        root: &cpp_parser::CppSyntaxNode,
        scopes: &crate::ScopeTree,
    ) -> Option<Type> {
        match self {
            // **Read from the syntax**, which is where a type is written: the specifier sequence and the
            // declarator, handed to the one reader that knows how to assemble them. For a declaration in this
            // file that is the better source by a wide margin — `const Widget* const p` is a shape with two
            // qualifier positions, and no parser of the *spelling* can put them back where the file had them.
            NamedDeclaration::Here(binding) => {
                // **A function's name has no type.** The declarator reader below assembles one from the declarator, and
                // for `shared_from_this()` that is the function type `T (*)()` — while what a call of it has is
                // [`DeclFact::returns`], which is the question a caller asks next. Measured: `auto p =
                // shared_from_this();` answered `T (*)()` for a member function this file declares.
                if matches!(
                    binding.kind,
                    crate::BindingKind::Function
                        | crate::BindingKind::Constructor
                        | crate::BindingKind::Destructor
                        | crate::BindingKind::ConversionFunction
                        | crate::BindingKind::OperatorFunction
                        | crate::BindingKind::LiteralOperator
                ) {
                    return None;
                }
                // **An enumerator's type is its enumeration.** `enum class Colour { Red };` writes no type beside `Red`,
                // so the reader below works from the *enum's* specifier sequence and cannot answer the enumerator — and
                // `auto colour = Colour::Red;` had no type at all. The enumeration is the scope the enumerator was bound
                // in, which is what its own declaration opened.
                if binding.kind == crate::BindingKind::Enumerator {
                    return scopes
                        .scope(binding.scope)
                        .and_then(|scope| scope.name.clone())
                        .map(Type::named);
                }
                let declarator =
                    crate::sema::types::declarator_declaring(root, binding.name_range)?;
                let specifiers = declarator.ancestors().find_map(|node| {
                    node.children().find(|child| {
                        CppSyntaxKind::from(child.kind()) == CppSyntaxKind::DeclSpecifierSeq
                    })
                })?;

                // *query* path only, deliberately. `struct { unsigned three : 3; } bits;` has a type with no spelling,
                // and the members live in the class scope that `build_scopes` names after this declarator, so the two
                // sides meet only if this answer uses the same key.
                //
                // The *fact* keeps the empty spelling, and that is not an oversight: a hover on a file-scope
                // `static const struct { … } priority[]` prints the declaration's own text, and naming that type
                // `priority` made the hover say `priority[] priority;` (measured: one handshake test went red). So the
                // key exists where a member question needs it and nowhere a reader reads a type.
                let written = crate::sema::types::type_of_declaration_named(
                    &specifiers,
                    Some(&declarator),
                    binding.name_range,
                    binding.name.identifier_text(),
                    // **The same rule the fact builder applies**: a variable's own parameter list is its constructor
                    // call, so the reader must not turn it into a function type. See `declared_type_of_with`.
                    matches!(binding.kind, crate::BindingKind::Variable),
                );
                // **Only inside a function body.** A file-scope `static const struct { … } priority[]` keeps the empty
                // spelling, because a reader hovering it is shown the declaration's own text — naming its type made
                // that hover print `priority[] priority;` (measured: `a_hover_names_the_instantiation_and_never_invents_a_void`
                // went red). The member questions this key exists for are all about locals.
                let keyed_by_the_declarator = scopes
                    .scope_at(binding.name_range.start_offset)
                    .is_some_and(|scope| {
                        scopes.scope_chain(scope).into_iter().any(|id| {
                            scopes
                                .scope(id)
                                .is_some_and(|data| data.kind == crate::ScopeKind::Function)
                        })
                    });
                match (
                    written.to_string().is_empty(),
                    an_unnamed_class_body(&specifiers),
                ) {
                    (true, Some(_)) if keyed_by_the_declarator => {
                        Some(Type::named(binding.name.identifier_text()?))
                    }
                    _ => Some(written),
                }
            }
            // **Read from the spelling**, which is all the index has: a fact is a `String` on disk and the file
            // it came from is not open. `parse_type_spelling` is the bridge, and `class_name` the one field of
            // it that a member query needs.
            NamedDeclaration::Indexed(fact, _, bindings) => {
                let _ = scopes;
                fact.type_of
                    .as_deref()
                    .map(|written| bindings.applied_to(&parse_type_spelling(written)))
            }
        }
    }

    /// What a **call** of this declaration has: the type it returns, or the class it declares.
    ///
    /// Two answers because C++ has two, and the tokens do not separate them: `make()` is a call of a function and
    /// has what it returns, while `Widget()` — the same shape — is a *temporary* of the class. Only the
    /// declaration says which, which is why this is asked here rather than of the shape.
    ///
    /// **The return type is finished the same way a member's type is** — [`finish_a_nested_name`], over the class
    /// the member is declared in (`DeclFact::scope`). That is the whole of how `auto y = x.back()` gets `y`'s type:
    /// this is the reader the deduction path goes through, and until it ran the walk it answered with the *name*
    /// `reference` while the other reader of the same question answered `_Elem&`. One question, one reader.
    pub(crate) fn what_a_call_has(
        &self,
        shapes: &crate::sema::declarations::ShapeTable,
        index: &ProjectIndex,
        declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
        scopes: &crate::ScopeTree,
        root: &cpp_parser::CppSyntaxNode,
        path: &Path,
        written_arguments: &[Type],
        argument_types: &[Type],
    ) -> Option<Type> {
        // **The template arguments the call did not write**, deduced from the types of the arguments it did.
        //
        // Writing `<size_type>` is one way to say what a function template's parameter stands for; passing an
        // argument whose type *is* that parameter is the other, and it is the one the standard library uses almost
        // everywhere: `_Get_unwrapped(_First)` never writes `<int*>`, it passes one. Until this, only the written
        // form substituted — so a deduced return that *is* the parameter came back as the parameter's **name**:
        // measured on a two-line fixture, `template <class _It> auto f(_It p) { return p; }` called as `f(a_pointer)`
        // deduced **`_It`**, which is not a missing answer but a wrong one, and worse than a refusal because it reads
        // like a type.
        //
        // The rule is C++'s own, restricted to the case this layer can be sure of: a function parameter **written as
        // exactly one template parameter** takes that parameter's place, so the argument passed there is what the
        // parameter stands for. A parameter written as `vector<_Ty> const&` needs the argument's own template-id
        // taken apart, and that is not attempted — it is a different deduction, and half of it would be a guess.
        let deduced;
        let written_arguments = if written_arguments.is_empty() && !argument_types.is_empty() {
            let (names, written_list) = self.parameters_of(shapes, index, scopes, root, path);
            deduced = deduce_the_parameters(&names, written_list.as_deref(), argument_types);
            &deduced[..]
        } else {
            written_arguments
        };

        match self {
            NamedDeclaration::Here(binding) => {
                if binding.kind == crate::BindingKind::Class {
                    return binding.name.identifier_text().map(Type::named);
                }
                // **A body that `if constexpr` decides, read again with what the call site knows.**
                //
                // `declared_returns_of` answers for a return type the file *wrote*; a function whose return type is
                // deduced has none there, and the fact is built with no call site in hand — so a body branching on a
                // template parameter has no single answer when the index is made. Here both halves exist, and the
                // body is in this very tree, so no other file has to be read: the branch whose condition holds under
                // the substitutions is the only one instantiated, and `_It + 0` becomes `int* + 0`.
                let names_for_the_call =
                    crate::sema::declarations::declared_template_parameters_with(
                        crate::sema::declarations::shapes_from(shapes, root),
                        binding,
                    );
                let returns = match crate::sema::declarations::declared_returns_of_with(
                    crate::sema::declarations::shapes_from(shapes, root),
                    binding,
                )
                .filter(|returns| !is_a_placeholder(returns))
                {
                    Some(returns) => parse_type_spelling(&returns),
                    None => {
                        let substitutions = crate::sema::types::TypeSubstitutions::new(
                            &names_for_the_call,
                            written_arguments,
                        );
                        let written = crate::sema::declarations::deduced_returns_of_substituted(
                            root,
                            binding.clone(),
                            &substitutions,
                            argument_types,
                        )?;
                        parse_type_spelling(&written)
                    }
                };

                // **The same pairing the indexed half does, for the declaration this file writes.**
                //
                // A function template declared in the buffer is a `Here` binding, and only the `Indexed` arm below
                // substituted — so `_Convert_size<size_type>(n)` in the file that declares `_Convert_size` answered
                // with the unsubstituted `_Size_type` while the identical call through a header's fact answered
                // `size_type`. One question, two answers, and the difference was which of the two arms the lookup
                // happened to land in. Measured on a fixture with both: the same-file call refused and the
                // header's call deduced.
                let names = crate::sema::declarations::declared_template_parameters_with(
                    crate::sema::declarations::shapes_from(shapes, root),
                    binding,
                );
                if names.is_empty() || written_arguments.is_empty() {
                    return Some(returns);
                }
                let substitutions =
                    crate::sema::types::TypeSubstitutions::new(&names, written_arguments);
                Some(returns.substituted(&substitutions))
            }
            // A **call of a member**: `v.data()` has what `data` returns, and what it returns is written with the
            // class template's parameters — so the same pairing that finishes a member's type finishes this, and
            // the same walk over the class's own member names finishes it too.
            NamedDeclaration::Indexed(fact, file, bindings) => {
                // **A fact with no return type whose body can be read again with what the call site knows.**
                //
                // `what_a_call_has_in` answers `None` for a function whose return type is deduced and whose body
                // this layer could not type when the index was built — and the index is built with **no call site**
                // in hand, so a body that branches on a template parameter has no single answer there. At the call
                // site both halves exist: the argument types say what the parameters stand for, and the declaring
                // file can be read again. So the body is typed a second time, **with the substitutions**, and that
                // is what lets `if constexpr` pick its branch: `_It + 0` becomes `int* + 0` and the two branches of
                // `_Get_unwrapped` finally agree.
                let name = fact.name.to_string();
                let offset = fact.name_range.start_offset;
                let returns = match what_a_call_has_in(fact) {
                    Some(returns) => returns,
                    None => {
                        let substitutions = crate::sema::types::TypeSubstitutions::new(
                            fact.parameters(),
                            written_arguments,
                        );
                        let declared_in = declaring(file.as_path())?;
                        // **The fact's offset is a *file* offset; the tree speaks the *reading*.**
                        //
                        // `name_range` comes from the index, which records where a token sits in the file it was
                        // written in. `definition_at` walks `declared_in.root`, whose offsets are the *rendering*'s —
                        // the file with its `#include`s expanded. For a file with no macro and no include the two are
                        // the same number, which is why this looked right; through a header that includes anything they
                        // are not, and the binding found at the raw offset is a different declaration. The translation
                        // is `reading_offset_of`, and the raw offset is tried after it because a region the rendering
                        // does not contain (`#if` branch nobody takes) still has to be looked at the way it was.
                        let found = [
                            declared_in.reading_offset_of(
                                crate::file::coordinates::FileOffset::new(offset),
                            ),
                            Some(crate::file::coordinates::ReadingOffset::new(offset)),
                        ]
                        .into_iter()
                        .flatten()
                        .find_map(|in_the_reading| {
                            match crate::sema::resolve::definition_at(
                                &declared_in.scopes,
                                &declared_in.root,
                                in_the_reading.get(),
                            ) {
                                // **The name has to be the one the fact named.** A matched offset in the wrong
                                // coordinates lands on a neighbouring declaration, and deducing *its* body would be a
                                // wrong answer that reads like a right one.
                                Known::Yes(found)
                                    if found.name.identifier_text() == Some(name.as_str()) =>
                                {
                                    Some(found)
                                }
                                _ => None,
                            }
                        });
                        let binding = found?;
                        let written = crate::sema::declarations::deduced_returns_of_substituted(
                            &declared_in.root,
                            binding,
                            &substitutions,
                            argument_types,
                        )?;
                        parse_type_spelling(&written)
                    }
                };
                let _ = &name;
                // **The class's arguments first, then the function's.** Both substitute into the same spelling and
                // they are written with disjoint names - the class's _Elem and the function's _Size_type in
                // asic_string<char>::_Convert_size<size_t> - so the order does not decide the answer; it decides
                // which name is left standing when the two lists overlap, and the function's is the nearer.
                let started = bindings.applied_to(&returns);
                let started = if fact.parameters().is_empty() || written_arguments.is_empty() {
                    started
                } else {
                    // **Written with names nobody had recorded until now.** _Convert_size returns _Size_type`r
                    // and is called _Convert_size<size_type>(_Length): the argument list is right there in the
                    // callee, the parameter names are in the fact, and the whole of the answer is the pairing - the
                    // same positional substitution a member's type already goes through. Measured on
                    // <vector>/<xutility>: 11 uto refusals were exactly this call, every one of them a return
                    // type that had been read and left unpaired.
                    let substitutions = crate::sema::types::TypeSubstitutions::new(
                        fact.parameters(),
                        written_arguments,
                    );
                    started.substituted(&substitutions)
                };

                // **An injected-class-name stands for the class with its own arguments.** `std::string::substr` returns
                // `basic_string` — the name a class uses for *itself* inside its own body — and a reader needs the
                // instantiation, not the bare name: measured, `auto cut = text.substr(1, 2);` answered `basic_string`
                // where `std::string` is the type. The arguments are the ones the receiver was used with, which is what
                // `bindings` holds.
                let started = match started.class_name() {
                    Some(name)
                        if started.arguments().is_empty()
                            && !bindings.arguments().is_empty()
                            && fact
                                .scope
                                .as_deref()
                                .is_some_and(|scope| scope.rsplit("::").next() == Some(name)) =>
                    {
                        let arguments: Vec<String> = bindings
                            .arguments()
                            .iter()
                            .map(|argument| argument.to_string())
                            .collect();
                        crate::sema::types::parse_type_spelling(&format!(
                            "{name}<{}>",
                            arguments.join(", ")
                        ))
                    }
                    _ => started,
                };

                let Some(class) = fact.scope.as_deref() else {
                    // A free function: there is no class whose members could name its return type. An alias at
                    // file scope (`using size_type = size_t;` and a function returning `size_type`) is resolved by
                    // the caller that has the file's aliases, not here.
                    return Some(started);
                };
                let (finished, _) = finish_a_nested_name(
                    index,
                    scopes,
                    root,
                    path,
                    class,
                    bindings,
                    started,
                    file.clone(),
                    vec![fact.name.to_string()],
                );

                Some(finished)
            }
        }
    }

    /// **What a call of this declaration may leave for the call site to say** — its template parameter names, and the
    /// parameter list as the declaration wrote it.
    ///
    /// Both halves are needed by the deduction [`what_a_call_has`] does, and neither is enough alone: the names say
    /// *which* words are placeholders, and the written list says *where* they stand among the function's parameters.
    /// A declaration that is not a template answers with no names, which is the ordinary case and costs nothing.
    pub(crate) fn parameters_of(
        &self,
        shapes: &crate::sema::declarations::ShapeTable,
        index: &ProjectIndex,
        _scopes: &crate::ScopeTree,
        root: &cpp_parser::CppSyntaxNode,
        _path: &Path,
    ) -> (Vec<String>, Option<String>) {
        let _ = index;
        match self {
            NamedDeclaration::Here(binding) => (
                crate::sema::declarations::declared_template_parameters_with(
                    crate::sema::declarations::shapes_from(shapes, root),
                    binding,
                ),
                crate::sema::declarations::parameter_list_at(root, binding.name_range.start_offset),
            ),
            NamedDeclaration::Indexed(fact, _, _) => (
                fact.parameters().to_vec(),
                fact.parameter_list().map(str::to_string),
            ),
        }
    }

    /// **What each of those parameters defaults to**, in the same order, `""` for none — the other half of
    /// [`NamedDeclaration::parameters_of`], and what a use that writes *fewer* arguments needs.
    ///
    /// `template <class T, class U = T> constexpr U twice = U(2);` used as `twice<float>` writes one argument for two
    /// parameters: `U`'s meaning is `T`'s, which the use wrote as `float`. Without this the answer is the parameter's
    /// own name — see `substituted_at_the_use`.
    pub(crate) fn parameter_defaults_of(
        &self,
        _index: &ProjectIndex,
        root: &cpp_parser::CppSyntaxNode,
    ) -> Vec<String> {
        let shapes = crate::sema::declarations::ShapeTable::new();
        match self {
            NamedDeclaration::Here(binding) => {
                crate::sema::declarations::declared_parameter_defaults_with(
                    crate::sema::declarations::shapes_from(&shapes, root),
                    binding,
                )
            }
            NamedDeclaration::Indexed(fact, _, _) => fact.parameter_defaults().to_vec(),
        }
    }
    ///
    /// A name range rather than the declaration's range, because the two answer different questions: a rename
    /// edits the name, and a caller looking for the declaration's own declarator has to be *at* the name — see
    /// [`callee_of_a_call`].
    pub(crate) fn name_offset(&self) -> usize {
        match self {
            NamedDeclaration::Here(binding) => binding.name_range.start_offset,
            NamedDeclaration::Indexed(fact, _, _) => fact.name_range.start_offset,
        }
    }

    /// **The declaration's parameter list, as its fact holds it** — `(const char* __format, ...)`.
    ///
    /// `None` for a binding in the file being edited, whose list is read from the tree the caller
    /// already has.
    pub(crate) fn parameter_list(&self) -> Option<String> {
        match self {
            NamedDeclaration::Here(_) => None,
            NamedDeclaration::Indexed(fact, _, _) => fact.parameter_list().map(str::to_string),
        }
    }
}

/// **A type that is still a member's name is followed to what that member declares** — one step at a time, with
/// the class's own template pairing applied at each one.
///
/// # Why it exists, and why it is one function
///
/// `_Ty& reference; reference front;` is how the standard library writes almost every type it has: the type of
/// `front` is the *name* `reference`, which mentions no parameter at all, so substituting the arguments into it
/// replaces nothing. The step that finishes it is a member lookup — `reference` in the same class, with the same
/// pairing — and the walk repeats while the type is still a name.
///
/// Measured, in MSVC's `<xstring>`: `back()` returns `reference`, which is `value_type&`, which is `_Elem&`, which
/// is `char&` for a `std::string`. **Two callers need this** — the member read as a value (`v.front`) and the
/// member *called* (`v.back()`) — and they had it twice with different answers: the second one skipped the walk
/// entirely, so `auto y = x.back()` recorded the *name* `reference` as `y`'s type. It is one function now, which
/// is this crate's rule for a question with two readers.
///
/// # What it looks up, and where it stops
///
/// The name looked up is the type's **base** rather than its whole spelling ([`Type::class_name`], to which a
/// reference is transparent): `value_type&` is a reference to the member alias `value_type`, and it is the alias
/// that has to be followed. A pointer is where the walk stops, because following one is a *decay* — a decision
/// about the expression rather than about the declaration, and the caller's to make.
///
/// The operators stay where they are written ([`Type::replacing`]): `value_type&` with `value_type` = `_Ty*` is
/// `_Ty*&`, and dropping either would answer a different type.
///
/// The walk is bounded by [`MAX_NESTED_TYPE_DEPTH`] rather than by an argument about termination: each step asks
/// about a *different* name and a class has finitely many members, but two members can name each other (`a b; b a;`
/// is not valid C++ and *is* writable text), and a walk that trusted validity would not terminate on the file a
/// language server has to survive.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_a_nested_name(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    bindings: &crate::sema::types::TypeBindings,
    mut current: Type,
    mut current_file: PathBuf,
    mut asked: Vec<String>,
) -> (Type, PathBuf) {
    for _ in 0..MAX_NESTED_TYPE_DEPTH {
        // **The class *with* the arguments the use wrote.** A return type's qualifier may name the class's own
        // parameters — `_Tree::find` answers `_Mybase::iterator`, and `_Mybase` is `_Tree<_Tmap_traits<_Kty, _Ty, …>>` —
        // and only the arguments turn those into types. The pairing arrives here as `bindings` (the object's type produced
        // it), so every caller of this walk gets the benefit: measured on a real project, without it the chain stopped at
        // `conditional<_Test, _Ty1, _Ty2>::type`.
        let class_with_arguments = if bindings.is_empty() || class.contains('<') {
            class.to_string()
        } else {
            let arguments: Vec<String> = bindings
                .arguments()
                .iter()
                .map(|argument| argument.to_string())
                .collect();
            format!("{class}<{}>", arguments.join(", "))
        };
        let class = class_with_arguments.as_str();
        // **A *qualified* name of the class's own aliases is followed here first.** `_Tree::find` is declared
        // `_Mybase::iterator find(...)` — a member alias of the very class the function belongs to — and the loop below
        // reads one *bare* name at a time, so this spelling was carried out unchanged and every later step asked about
        // a class called `_Mybase::iterator` (measured on a real project: `LuaReserved.find(text)` typed as exactly
        // that). The alias walk resolves it in the class's own scope, which is the same step a member's type goes
        // through.
        let spelling = current.to_string();
        if spelling.contains("::") && !asked.contains(&spelling) {
            let resolved = a_spelling_through_the_aliases(
                index,
                scopes,
                root,
                path,
                &crate::sema::types::parse_type_spelling(class),
                &[],
                &spelling,
                MAX_ALIAS_HOPS,
            );
            if resolved != spelling {
                asked.push(spelling);
                current = bindings.applied_to(&crate::sema::types::parse_type_spelling(&resolved));
                continue;
            }
        }

        let Some(nested) = current.class_name().map(str::to_string) else {
            break;
        };
        if !writes_a_name(&nested) || asked.contains(&nested) {
            break;
        }
        asked.push(nested.clone());

        // **A nested name whose head is another class.** `std::shared_ptr`'s `element_type` is
        // `typename __shared_ptr<_Tp>::element_type`: the alias is the **base's**, and the head of the spelling names the
        // base. Asking the outer class for a member spelled `__shared_ptr<Resource>::element_type` finds nothing, and the
        // walk stopped right there — measured on `test_cpp`'s `smart_pointers.cpp`, where `locked->identifier()` answered
        // "`shared_ptr<Resource>` has no member `identifier`" for a `shared_ptr<Resource>` that has one.
        //
        // The step is the same question [`the_pointee_of_a_handle`] asks, so it is the same `member_fact` call: the head
        // resolved in the enclosing scopes (a base name is not a member, so `resolve_aliases` is the reader that knows how
        // it is written) and the tail looked up in it, with the head's own arguments as the pairing.
        // **A name qualified by a *namespace* is not a member of a class, and is left as it is written.**
        //
        // This walk replaces a member's spelling with the member's type: `pointer` becomes `_Val*`, which is the
        // whole point. `std::string` is not that shape — `string` is a name in the namespace `std`, and the type it
        // is declared to have is the alias it stands for, so replacing it turns the name a header wrote into
        // `basic_string<char, char_traits<char>, allocator<char>>`. Measured over stdio on `EmmyLuaCodeStyle`'s
        // `LuaCodeFormat.cpp:131`, where the inlay hint of `auto formattedText = f.GetFormatResult(t);` drew exactly
        // that string, and in `tests/types.rs`' `a_deduced_type_keeps_the_alias_the_source_wrote`.
        if let Some((head, _)) = the_head_and_the_member_of(&nested)
            && names_a_namespace(index, path, head)
        {
            break;
        }

        let found = match member_fact(
            &crate::sema::declarations::ShapeTable::new(),
            index,
            scopes,
            root,
            path,
            class,
            &nested,
            None,
        ) {
            Known::Yes(found) => Known::Yes(found),
            miss => match the_head_and_the_member_of(&nested) {
                Some((head, tail)) => {
                    let head = resolve_aliases(
                        &crate::sema::declarations::ShapeTable::new(),
                        index,
                        scopes,
                        root,
                        path,
                        head,
                    )
                    .to_string();
                    let arguments: Vec<String> = parse_type_spelling(&head)
                        .arguments()
                        .iter()
                        .map(|argument| argument.to_string())
                        .collect();
                    match member_fact(
                        &crate::sema::declarations::ShapeTable::new(),
                        index,
                        scopes,
                        root,
                        path,
                        &head,
                        tail,
                        (!arguments.is_empty()).then_some(&arguments),
                    ) {
                        Known::Yes(found) => Known::Yes(found),
                        _ => miss,
                    }
                }
                None => miss,
            },
        };
        let Known::Yes((inner, inner_file)) = found else {
            break;
        };
        let Some(inner_type) = inner.type_of.as_deref() else {
            break;
        };

        let resolved = bindings.applied_to(&parse_type_spelling(inner_type));
        current = current.replacing(&nested, &resolved);
        current_file = inner_file;
    }

    (current, current_file)
}

/// **Does this spelling name a namespace in the index?** — the rule that keeps a namespace-qualified type from being
/// rewritten into what it stands for. See the two callers: [`a_spelling_through_the_aliases_in`] and
/// [`finish_a_nested_name`].
///
/// Every candidate must be a namespace: `std` is one, and `std::string` is not (`definitions` answers the alias, whose
/// kind is not `Namespace`). An empty or unknown answer is **not** a namespace — a name the index cannot place must not
/// be rewritten, but it must not stop a walk either, and `false` is the answer that leaves the decision where it was.
pub(crate) fn names_a_namespace(index: &ProjectIndex, path: &Path, spelling: &str) -> bool {
    index
        .definitions(spelling, path)
        .value()
        .is_some_and(|found| {
            !found.found.is_empty()
                && found
                    .found
                    .iter()
                    .all(|one| one.fact.kind == DeclKind::Namespace)
        })
}

/// **Split `A<B>::C` into its head and its last member** — `("A<B>", "C")` — at the last `::` outside angle brackets.
///
/// `None` when there is no `::` to split at, or when either half would be empty. The head keeps its arguments: a base
/// written `__shared_ptr<_Tp>` is looked up by the name `__shared_ptr` and substituted by `_Tp`'s argument, and the two
/// halves are what the caller passes on.
pub(crate) fn the_head_and_the_member_of(nested: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let mut boundary = None;
    let bytes = nested.as_bytes();
    let mut at = 0usize;
    while at + 1 < bytes.len() {
        match bytes[at] {
            b'<' => depth += 1,
            b'>' => depth = depth.saturating_sub(1),
            b':' if depth == 0 && bytes[at + 1] == b':' => {
                boundary = Some(at);
                at += 1;
            }
            _ => {}
        }
        at += 1;
    }
    let boundary = boundary?;
    let head = nested[..boundary].trim();
    let tail = nested[boundary + 2..].trim();
    (!head.is_empty() && !tail.is_empty()).then_some((head, tail))
}

/// **A declaration's type finished when it is an alias template applied to arguments.**
///
/// `enable_if_t<__is_same(int, int), double> when_the_same;` records the *spelling*, and what it stands for is
/// `typename enable_if<__is_same(int, int), double>::type` — a member of a class template applied to arguments. The
/// two steps are the ones the member arm already takes ([`the_alias_behind`] for the alias, then the member lookup,
/// which pairs the use's arguments with the class's parameters by pattern): this is the same pair, applied to the one
/// place they were missing.
///
/// `None` for every ordinary declaration — a spelling with no `<` is not an alias template, and that is the cheap
/// question this asks first.
pub(crate) fn a_trait_alias_resolved(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    named: &NamedDeclaration,
    written: &Type,
) -> Option<Type> {
    let spelling = written.to_string();
    if !spelling.contains('<') {
        return None;
    }
    let (class, member) = the_alias_behind(index, &named.file(path), &spelling)?;
    let Known::Yes((fact, _)) = member_fact(
        &crate::sema::declarations::ShapeTable::new(),
        index,
        scopes,
        root,
        path,
        &class,
        &member,
        None,
    ) else {
        return None;
    };
    let target = fact.type_of.as_deref()?;
    let arguments: Vec<String> = crate::sema::types::parse_type_spelling(&class)
        .arguments()
        .iter()
        .map(|one| one.to_string())
        .collect();
    let bare = crate::sema::types::parse_type_spelling(&class);
    let class_name = bare.class_name()?;
    let parameters = index.template_parameters_for(class_name, path, &arguments);
    let defaults = index.template_parameter_defaults_for(class_name, path, &arguments);
    let resolved = crate::sema::types::substituted_spelling_with_defaults(
        target,
        &parameters,
        &defaults,
        &arguments,
    );
    Some(crate::sema::types::parse_type_spelling(&resolved))
}
/// The type a declaration was written with — or, where it wrote `auto`, the type its **initializer** has.
///
/// # What `auto` means here
///
/// `auto n = count();` declares `n` with the type of `count()`, and until this existed the answer to "what is `n`"
/// was the word `auto` — which is not a type, and which every consumer then had to special-case or give up on:
/// `auto w = Widget{}; w.` listed nothing, hover said `auto`, and the parameter hints could not say what an
/// argument was. The deduction is the language's own rule read off the declaration the file wrote:
///
/// ```text
/// auto n = count();          the initializer's type                          `int`
/// const auto& r = n;         that type with what was written around `auto`    `const int&`
/// auto p = &r;               the initializer is `&r`, so `const int*`         `const int*`
/// auto q = Widget{};         a braced initializer names the type it makes     `Widget`
/// ```
///
/// # What it refuses, and why that is the rule rather than a shortcut
///
/// * **`auto&&`** — the language deduces a reference *or* a value depending on the initializer's value category
///   (`T&` for an lvalue, `T&&` for an rvalue), and nothing in a declaration's spelling says which. Reporting what
///   was written around `auto` would be wrong half the time, so the answer is `Unknown`.
/// * **a `*` the initializer does not have** — `auto* p = x;` where `x` is not a pointer is ill-formed; a type
///   invented to make the declaration work would hide that.
/// * **a declaration in another file** — `auto` at namespace scope in a header is deduced from that header's
///   syntax, and this layer holds one file's tree. The spelling in the index (`auto`) is not a type, so the answer
///   is `Unknown` rather than a guess.
/// * **a chain longer than [`MAX_TYPE_DEPTH`]** — see the guard in [`type_of_expression`].
pub(crate) fn declared_type(
    memo: &TypeMemo,
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    named: &NamedDeclaration,
    depth: usize,
) -> Known<Type> {
    // **An enumerator's type is its enumeration** — the *indexed* half of the rule `NamedDeclaration::type_of` carries for
    // a declaration in this file. `enum class Colour { Red };` writes no type beside `Red`, so the fact records
    // `type_of: None` and this answered nothing: measured, `auto colour = Colour::Red;` in a `.cpp` whose enum is in the
    // header had no type at all, while the same declaration in one file worked.
    if let NamedDeclaration::Indexed(fact, _, _) = named
        && fact.type_of.is_none()
        // **An enumerator only.** `DeclKind` has no enumerator — it is a `Variable` in the enum's scope — and a
        // *function* has the same (`type_of: None`, scope = a class). Without this guard `shared_from_this` declared in
        // `Base` answered the type `Base`, which is the class it is a member of, not what it is.
        && fact.kind == crate::DeclKind::Variable
        && let Some(scope) = fact.scope.as_deref()
        && let Known::Yes(enclosing) = index.definition(scope, path)
        && enclosing.fact.kind == crate::DeclKind::Type
    {
        return Known::Yes(Type::named(scope));
    }

    let declaration = named.type_of(root, scopes);
    let Some(declaration) = declaration else {
        return Known::No;
    };

    // **The declaration reads its own type**, and the only case left over is the placeholder: for anything else
    // the shape the reader built *is* the answer, qualifiers and operators and all.
    //
    // **An alias's name is not an intermediate to be substituted away here.** The obvious next step — seeing
    // `reference`, asking what it aliases, and answering `_Elem&` — was tried and is **wrong at this layer**: it
    // broke three tests that had been passing, because `basic_string<char>::reference` is a *member looked up by
    // that name*, and the class's own arguments are substituted into the target only once the lookup has found it.
    // Measured: `a_member_of_a_class_reached_through_an_alias_gets_the_aliass_arguments` expects `char&` and got
    // `_Elem&`, because the substitution had already consumed the name the lookup needed. See [`alias_target`] for
    // the step that is right here — an alias *declared in this file* has a target this can read directly — and the
    // note on [`the_alias_behind`] for where the other step belongs.

    let placeholder = declaration.to_string();
    // **C++17's *implicit* deduction guide** — `Box box(7);` where `Box` is a class template.
    //
    // The syntax reader answers the template's **name**, which is a type with an argument missing: `Box` is not
    // `Box<int>`, and a member query on it finds nothing because no declaration is called that. The language's rule for
    // this shape is the constructor's parameter list, and only that shape is deduced here — one template parameter, and
    // a constructor whose first parameter *is* that parameter. Anything else answers the name it wrote, because a
    // guessed argument would print an instantiation the reader never asked for.
    //
    // Measured on a C++20 project (`techniques.h` + `techniques.cpp`): `techniques::Box box(7)` typed as
    // `techniques::Box` while `techniques::Box<int> box(7)` typed correctly, so what was missing was the deduction
    // rather than the template.
    if let crate::sema::types::Type::Named { name, arguments } = &declaration
        && arguments.is_empty()
        && !name.contains('<')
    {
        // **The braced initialiser's elements are the constructor's arguments.** `std::pair pair{1, 2}` is a
        // `pair<int, int>` by the same C++17 rule the next block applies to `Box box(7)`, and the elements are where the
        // arguments are: the `initializer_type` below answers the *list's* type, which is a `std::initializer_list` this
        // layer deliberately does not invent.
        if let Some(initialisation) = the_braced_initialiser_of(root, named) {
            let elements: Vec<cpp_parser::CppSyntaxNode> = initialisation.children().collect();
            if let Some(with_arguments) = the_arguments_a_braced_initialisation_deduces(
                memo,
                index,
                declaring,
                scopes,
                root,
                path,
                &elements,
                name,
                depth + 1,
            ) {
                return Known::Yes(with_arguments);
            }
        }
        if let Known::Yes((from_the_initializer, _)) =
            initializer_type(memo, index, declaring, scopes, root, path, named, depth + 1)
            && let Some(with_arguments) = deduced_class_template_arguments(
                index,
                scopes,
                root,
                path,
                declaring,
                name,
                &from_the_initializer,
            )
        {
            return Known::Yes(crate::sema::types::parse_type_spelling(&with_arguments));
        }
    }

    // **A `typeof`/`__typeof__` specifier whose payload is an *expression*** — `typeof(value) copy = value;` — is
    // the type of that expression, and only the semantic layer can answer it: the reader of the *syntax* hands back
    // the spelling, `typeof(value)`, which no scope and no member list has heard of. The fact builder has the same
    // gap, so a summary records the spelling too.
    if let Some(payload) = a_typeof_payload(root, named)
        && let Known::Yes((type_of, _)) = type_of_expression(
            index,
            &mut |_: &Path| None,
            scopes,
            root,
            path,
            &payload,
            depth + 1,
        )
    {
        return Known::Yes(type_of);
    }
    if !writes_auto(&placeholder) {
        return Known::Yes(declaration);
    }

    let unknown = || Known::Unknown(UnknownReason::UnknownType(Box::from(placeholder.clone())));

    // The **written** spelling, with the qualifiers the reader drops: `const auto& r = x;` has to deduce a `const`
    // type, and `Type` models no qualifiers — so the spelling is carried beside the shape rather than inside it.
    // See `deduction_inputs`, which is the one place that reads a declaration's text for this reason.
    // **A range-based `for` head** — `for (auto x : range)`: the type is the range's **element**, not the range,
    // and there is no initializer for `deduction_inputs` to find. This is the ordinary spelling of a loop over a
    // container, and without it every such variable had no type: hovering `x` in `for (auto x : v)` answered "a
    // variable, declared in a function body" and nothing else, and `it->first` in `for (auto it : map)` could not
    // be resolved. See [`range_for_element_type`].
    if let Some(element) =
        range_for_element_type(memo, index, declaring, scopes, root, path, named, depth)
    {
        return element;
    }

    // **A generic lambda's parameter takes its argument's type.** `[](auto value)` is the placeholder because the
    // declaration has no initializer to deduce from — the argument is the lambda's *invoker*, and for the standard
    // adaptors and algorithms that invoker hands the range's element to the callable's first parameter. See
    // [`the_element_a_lambda_parameter_receives`], which is the table of the spellings this is proven for.
    if let NamedDeclaration::Here(binding) = named
        && let Some(the_type) = the_element_a_lambda_parameter_receives(
            memo,
            index,
            declaring,
            scopes,
            root,
            path,
            binding,
            &placeholder,
            depth,
        )
    {
        return Known::Yes(crate::sema::types::parse_type_spelling(&the_type));
    }

    let Some((as_written, initializer)) = deduction_inputs(root, named) else {
        return unknown();
    };

    // **The walk's own reason, rather than the placeholder again.** The initializer walk knows *which* expression it
    // could not type — `UnknownType("_Mypair._Myval2")` — and returning `UnknownType("auto")` in its place threw that
    // away: every refusal in a standard-library header read as the same four characters, and a reader could not tell
    // "the initializer is not there" from "the initializer is there and its third subexpression is a template
    // parameter". Measured on eight headers: 294 refusals, **246 of them reported as `UnknownType("auto")`** — a
    // payload that answers no question, and one that hid the real reason from a probe written to find it.
    //
    // `Known::No` still means the walk declined to take the question at all, which is what the placeholder reason is
    // for; only a walk that *tried and failed* has a better answer to give.
    let deduced =
        match initializer_type(memo, index, declaring, scopes, root, path, named, depth + 1) {
            Known::Yes((deduced, _)) => deduced,
            Known::Unknown(reason) => return Known::Unknown(reason),
            Known::No => return unknown(),
        };

    match auto_substituted(&as_written, &deduced, an_lvalue(&initializer)) {
        Some(type_of) => Known::Yes(parse_type_spelling(&type_of)),
        None => unknown(),
    }
}

/// **Is this initializer an lvalue?** — the one thing `auto&&` collapsing needs that a *type* cannot say.
///
/// `auto&& bound = base;` and `auto&& bound = 42;` deduce two different references, and both initializers type as
/// `int`: the difference is the expression's **value category**, not its type. The categories this layer can decide
/// from the shape are the ones that are decided by it — a name, a member of one, a dereference and a subscript are
/// lvalues; a literal, an object construction and a call that does not return a reference are not. A call *returning*
/// `T&` says so in its deduced type, which the caller has already put in hand.
pub(crate) fn an_lvalue(expression: &cpp_parser::CppSyntaxNode) -> bool {
    use cpp_parser::CppSyntaxKind::{CallExpr, InitListExpr, LiteralExpr, NewExpr, ParenExpr};
    // **Everything is an lvalue except what is definitely a prvalue.** A negative list rather than a positive one
    // because the grammar reaches a name in four ways (`IdentifierExpr`, `NameExpr`, `RefExpr`, and a member's own
    // node) and a positive list is a list to get wrong: measured, `auto&& bound = base;` answered `int&&` with the
    // identifier's kind missing from it.
    let kind = cpp_parser::CppSyntaxKind::from(expression.kind());
    if matches!(kind, LiteralExpr | CallExpr | InitListExpr | NewExpr) {
        return false;
    }
    // A parenthesised initializer keeps its inner expression's category.
    if kind == ParenExpr
        && let Some(inner) = expression.children().next()
    {
        return an_lvalue(&inner);
    }
    // Anything else — a name in any of its four shapes, a member, a dereference, a subscript — is an lvalue, which is
    // the safe direction here: a wrong `&` is still a reference to the right class, while a wrong `&&` names no class
    // at all.
    true
}

/// **Where the other half of the alias step belongs, and why it is not here.**
///
/// An `auto` whose initializer is a name of an alias type deduces that alias's **spelling** — `auto a = q;` where
/// `q` is a `pointer` answers `pointer`. Following it to the target (`int*`, or in the standard library
/// `typename _Alty_traits::pointer`) looks like the obvious completion of [`alias_target`], and it was written,
/// measured and **removed**:
///
/// * it moved the standard library's own count by **one** declaration in 631 — 130 deduced to 131 — because the
///   targets there are template-dependent names this cannot resolve anyway; and
/// * it **broke three tests** that had been passing, all of them about aliases, because substituting at this layer
///   destroys the alias *name* that a member lookup needs: `basic_string<char>::reference` is a member found by
///   that name, and the class's arguments are substituted into the target only after the lookup has found it.
///   Measured: `a_member_of_a_class_reached_through_an_alias_gets_the_aliass_arguments` wants `char&` and got
///   `_Elem&`.
///
/// The step belongs where a type's **shape** is wanted and its name is not — the operator and member queries — and
/// it belongs there with the class's arguments in hand, which is the generic instantiation this analysis does not
/// yet do. Recorded here rather than in a plan: the next reader of `alias_target` is the reader who will try it.
///
/// Does this spelling use the `auto` placeholder? A word, not a substring: `automatic` is a name.
pub(crate) fn writes_auto(written: &str) -> bool {
    written
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        // **`__auto_type` is GNU's spelling of the same placeholder**, and it is one *word* here, so comparing against
        // `auto` alone missed it: `__auto_type value = 41;` had a fact typed `int` — the fact builder deducts — while
        // `type_at(value)` answered `__auto_type`, because this predicate is what decides whether the deduction is
        // asked for at all. Measured on a nine-file C corpus, and pinned by
        // `gnu_auto_type_is_deduced_from_its_initializer`.
        .any(|word| word == "auto" || word == "__auto_type")
}
