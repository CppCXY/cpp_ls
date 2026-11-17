//! **Completions** — what to offer after a `.`/`->`, and what names are in scope at the cursor.
//!
//! Moved here from `project/mod.rs` without a change. Both questions are asked on every keystroke, so the candidates
//! are collected under a budget and ordered stably rather than sorted into a perfect list.

use super::*;

/// What a completion at a member access should offer: the type, its members, and where to put the chosen one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberCompletions {
    /// The type of the object, as the **lookup key** the members were found under — `Widget`, `ns::Widget`,
    /// `Base` for `Base<int>`.
    ///
    /// The key rather than the spelling the file wrote, because a consumer showing "members of `…`" has to name
    /// something the user can find, and `Base<int>` is a type no member is declared in. It is also the fastest
    /// thing to look at when the list is wrong.
    pub class: String,
    pub members: MemberList,
    /// The range the member's name occupies, which is what a client **replaces** with the chosen one.
    ///
    /// Empty and just past the operator for `w.`, which is the state this query exists for. One field for both
    /// states on purpose: an insert-at-a-point and a replace-a-prefix are the same edit, and a consumer that had
    /// to branch on which one it is would get it wrong exactly once — on the keystroke right after the dot.
    pub member_range: cpp_parser::SourceRange,
    /// What of the name is already written **before the cursor**: `si` for `w.si|`, and empty for `w.|`.
    ///
    /// Not the whole written member, which is why it is computed here rather than taken from the shape: a cursor
    /// in the *middle* of a name — `w.si|ze` — filters by `si` while [`MemberCompletions::member_range`] covers
    /// all of `size`, because the range is what gets replaced and the prefix is what gets matched. A consumer that
    /// used the whole spelling for both would offer nothing while the user is plainly typing.
    pub prefix: String,
    /// **How many members the access level kept out of the list** — counted rather than dropped in silence, so that
    /// a `.` which offers nothing can say which of the two reasons it was: "the class has no members" and "every
    /// member of it is `private` where the cursor is" are different facts about the program, and only one of them
    /// has a fix the reader can apply. See [`DeclFact::access`].
    pub hidden_by_access: usize,
}

/// The members a completion at `offset` should offer: `w.` and `w.si`, answered with `Widget`'s members.
///
/// The third query that needs a **type**, and the one the other two were building towards: [`members_of`] answers
/// "what does this type have" given a type, and this answers "what should be offered here" given a cursor. The
/// steps are all ones that already existed, which is the whole reason it is short:
///
/// ```text
/// 1. read the shape   — the object expression and what of the member is written (completion::context_at)
/// 2. infer the type   — type_of_expression, the recursive inference the member *lookup* already uses
/// 3. list the members — members_of, including the ones the bases declare
/// ```
///
/// # The state it exists for
///
/// `w.` — the operator typed and nothing after it. That is the keystroke that *asks* the question, and the reader
/// above is the one place that decides it is a member access: the parser usually reads `w.` as an access with an
/// empty member, and where a recovery loses that shape the reader falls back to the **text**, which cannot be
/// ambiguous about an operator sitting at the cursor. The same answer covers `w.si`, where the work is only that
/// [`MemberCompletions::member_range`] is the written prefix instead of a point.
///
/// # The answers, and the one that matters most
///
/// * `Yes(completions)` — the members of the object's type.
/// * `Unknown(UnknownType)` — the object's type could not be worked out. **The common case in real code**, and
///   the honest one: `f().size` needs a return type computed, `(*p).size` needs a dereference followed, and
///   `arr[i].size` needs a subscript — see [`members_of`]'s note. A consumer shows
///   nothing rather than offering the members of some other type.
/// * `Unknown(NotDeclaredHere)` — the object's type is a class nothing visible declares.
/// * `Unknown(ConditionalCompilation)` — that class is only reachable through a guarded `#include`.
/// * `Unknown(UnparsableName)` — the cursor is not on a member access at all.
pub fn member_completions_at(
    index: &ProjectIndex,
    declaring: &mut dyn FnMut(&Path) -> Option<crate::FileView>,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<MemberCompletions> {
    let memo = TypeMemo::default();
    // **The shape comes from the completion context, which is the one reader of it.** This used to call
    // `sema::resolve::member_access_at`, which reads the access out of the tree alone, and the two disagree about
    // one offset a client really sends: with the caret drawn **on** the operator's own column (`full2|.`), the
    // grammar's access node ends before the operator, so the tree answers "not a member access" while the context
    // reader — which asks the text as well, for exactly this reason — answers "a member access with nothing
    // written". Measured on a live server: the member list and the diagnostic for the same keystroke said opposite
    // things, and a client asking at the caret's own column got the names in scope.
    //
    // Two readers of one fact is the shape of every bug in this area; the completion layer owns this question, and
    // every consumer of it — this query, `Session::member_completions`, the log line — goes through it.
    let crate::completion::CompletionContext::Member(access) =
        crate::completion::context_at(root, offset)
    else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    let written = match type_of_expression_memoed(
        &memo,
        &crate::sema::declarations::ShapeTable::new(),
        index,
        declaring,
        scopes,
        root,
        path,
        &access.object,
        0,
    ) {
        Known::Yes((written, _)) => written,
        Known::Unknown(reason) => return Known::Unknown(reason),
        // The only `No` the type layer produces is "nothing says what this is", which is the same answer as an
        // expression it cannot type: there is nothing to list members of.
        Known::No => {
            return Known::Unknown(UnknownReason::UnknownType(Box::from(
                access.object.text().to_string().trim(),
            )));
        }
    };

    let Some(class) = member_access_class(&written) else {
        return Known::Unknown(UnknownReason::UnknownType(Box::from(written.to_string())));
    };

    // **`config->` on a smart pointer lists the pointee's members**, not the handle's: `std::shared_ptr<T>` has
    // `operator->`, `get` and `reset`, and the names a reader is reaching for after an arrow are `T`'s (reported from
    // an editor: "and there is no completion either"). Only for an **arrow**, and only when the handle declares
    // `operator->` — an object whose `->` reaches a class it declares no `operator->` for is a pointer, which
    // [`member_access_class`] has already reached through.
    // **The *written* spelling**, not `class`: `member_access_class` has already stripped `shared_ptr<Config>` to
    // `shared_ptr`, and the template arguments are exactly what the handle's `operator->` return type is substituted
    // with. Measured: with the stripped spelling the step declined for want of arguments and the list was the
    // handle's.
    let spelled = written.to_string();
    // **One answer to "which class does this access reach".** The completion's `the_class_after_the_operator` is the
    // reader of that question — it follows a pointer, a class's own `operator->`, a base's (through the substitution
    // composed along the base chain), and the base's `element_type` typedef — and this query used a second
    // implementation that followed only the first two. The two disagreed exactly where a reader notices: on
    // `std::shared_ptr<std::string>`, the completion listed `std::string`'s sixty-eight members while a hover on one
    // of them answered "`std::shared_ptr<std::string>` has no member `c_str`". Asking the completion's answer first
    // makes the list and the hover one answer; `the_class_an_arrow_points_at` stays as the fallback for the shapes it
    // reaches that this one does not (an alias chain the substitution cannot follow).
    // **The shared answer is used only when it names a class**, and that is not a formality: the completion's reader
    // substitutes what it can and answers with the spelling that is left, so a member template's `_Ty2*` comes back
    // as `_Ty2` — a name nothing declares, which a member lookup would answer "no members" for with confidence. The
    // older reader below is the one that walks `element_type` and the aliases for exactly those shapes, so the two
    // are tried in that order rather than one replacing the other.
    let names_a_class = |name: &str| {
        let bare = base_type_name(name);
        index
            .files_declaring(bare, path)
            .iter()
            .any(|found| found.fact.kind == crate::DeclKind::Type && found.fact.type_of.is_none())
    };
    let class = match access.arrow {
        true => crate::completion::the_class_after_the_operator(
            index, scopes, root, path, &written, true,
        )
        .map(|(class, _)| class)
        .filter(|class| names_a_class(class))
        .or_else(|| the_class_an_arrow_points_at(index, scopes, root, path, &spelled, None))
        .unwrap_or(class),
        false => class,
    };

    match members_of(index, scopes, root, path, &class) {
        Known::Yes(mut members) => {
            let hidden_by_access =
                offerable_members(index, scopes, root, path, offset, &mut members);

            Known::Yes(MemberCompletions {
                class: class.to_string(),
                members,
                hidden_by_access,
                member_range: access.member_range,
                prefix: written_before_the_cursor(&access, offset),
            })
        }
        Known::Unknown(reason) => Known::Unknown(reason),
        // `members_of` reports a name nothing declares as `Unknown(NotDeclaredHere)` rather than `No`, so this arm
        // is for totality and says the same thing it would.
        Known::No => Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class))),
    }
}

/// The part of the member's spelling that lies **before** the cursor.
///
/// Separate from the range because the two answer different halves of the same edit: the range is what the client
/// **replaces** with the chosen name — all of `size`, so nothing of the old spelling is left behind — while this
/// is what a server **matches** against the candidates, and a cursor inside a name has only typed the part before
/// it. Taking the whole spelling for both is what makes `w.si|ze` offer nothing.
///
/// The slice is by byte offset into the member's own text, so it is exactly the written prefix; a cursor that is
/// not on a character boundary (possible in an identifier with non-ASCII letters) falls back to the whole
/// spelling, which is the safe direction — a filter that matches too much still shows something.
pub(crate) fn written_before_the_cursor(
    access: &crate::sema::resolve::MemberAccess,
    offset: usize,
) -> String {
    let start = access.member_range.start_offset;
    let end = access.member_range.end_offset().min(offset);

    if end <= start {
        return String::new();
    }

    access
        .member
        .get(..end - start)
        .unwrap_or(&access.member)
        .to_string()
}

/// One name a completion can offer: where it is declared, and how far out it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferedName {
    pub file: PathBuf,
    pub fact: DeclFact,
    /// How many scopes out from the cursor the declaration was found: `0` is a name declared in the scope the
    /// cursor is in — a local, or the scope the `::` named — and larger numbers are enclosing scopes, which C++
    /// only considers once the inner ones have been searched. The order of [`NameCompletions::names`].
    ///
    /// Recorded rather than left to be recomputed, for the reason [`ProjectMember::depth`] is: a consumer showing
    /// locals above globals, or grouping by where a name came from, would otherwise have to reconstruct the walk it
    /// was just handed the result of.
    pub depth: usize,
    /// **How far from the cursor the declaration is in the program text** — the field a consumer ranks by.
    ///
    /// [`OfferedName::depth`] counts *scopes*, which orders a lookup but does not answer the question a suggestion
    /// list is judged by: a local and a name from `zmmintrin.h` can both be depth 2. This says which of them is
    /// nearer — the buffer's own scope, the buffer, a header beside it, a header it includes, or a header only
    /// that header includes — and it is the same order C++ would find them in from the other end.
    ///
    /// Recorded here rather than re-derived by a consumer because the derivation needs the include *graph*, and
    /// the walk that produced this list already crossed it: see [`NameProvenance`].
    pub from: NameProvenance,
}

/// Where a name a completion offers was found, relative to the file the cursor is in.
///
/// Ordered by nearness, so that the ranking can use the enumeration itself as a tier and two consumers cannot
/// disagree about which of two provenances is closer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NameProvenance {
    /// A scope of the file being edited: a local, a parameter, a member of the class the cursor's body is in, or
    /// the names written directly in a `::`-qualified scope.
    Scope,
    /// Written at file scope in the file being edited.
    ThisFile,
    /// A header the file being edited includes directly.
    DirectInclude,
    /// A file reached only through another file — the standard library's own headers, mostly.
    IndirectInclude,
}

/// What a completion **in a name** should offer: the scope that was listed, and its names.
///
/// The sibling of [`MemberCompletions`], and the other half of "what can be typed here": that one answers after a
/// `.` or `->`, where the answer is a type's members; this one answers in a name position, where the answer is
/// what is visible — after a `::` (one scope), or with no qualifier at all (the scopes the cursor is inside, from
/// the innermost outward).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameCompletions {
    /// The scope the names were listed from, as the **lookup key** rather than as the file spelled it: `ns`,
    /// `ns::Widget`, `::` for the global name space, or empty when the cursor wrote no qualifier — there the names
    /// come from the scopes the cursor is inside rather than from one scope.
    pub scope: String,
    /// The names, innermost first and **deduplicated by name**: a declaration in an inner scope hides the same
    /// name in an outer one, which is what C++ does and what keeps a local `count` from being offered beside the
    /// header's. Two declarations of one name in the *same* scope are both kept — that is an overload, not a
    /// choice.
    pub names: Vec<OfferedName>,
    /// What of the name is already written **before the cursor**: `Wid` for `ns::Wid|`, empty for `ns::|`.
    ///
    /// The counterpart of [`MemberCompletions::prefix`], and separate from the range for the same reason.
    pub prefix: String,
    /// The range a client **replaces** with the chosen name: the written segment, or an empty range at the cursor
    /// when the segment has only just begun.
    pub name_range: cpp_parser::SourceRange,
}

/// The names a completion at `offset` should offer: `ns::`, `Widget::`, `::`, and a bare name.
///
/// # The four positions, and one query for all of them
///
/// ```text
/// ns::            one scope: the names written directly in `ns`
/// Widget::        one scope, and its **bases** — a typedef a base declares is nameable through the derived class
/// ::Widget        the global name space only, which is what the leading `::` asks for
/// loc             no qualifier: every name visible from the cursor, innermost scope first
/// ```
///
/// The first three are one question — list the declarations written in a scope — and the fourth is the same
/// question asked of a *chain* of scopes. What differs is only where the scopes come from, which is why this is one
/// query rather than four.
///
/// # The two layers, and when they are merged rather than chosen between
///
/// The file's own scope tree is asked first, because a buffer that has never been saved has no summary — the split
/// every cross-file query in this crate makes. What happens next depends on what the scope *is*, and the difference
/// is the language's:
///
/// * a **class** is defined once, so its members come from one place: the buffer's scope if it has one, the index
///   otherwise. Merging would list the same member twice;
/// * a **namespace** can be reopened, and a file that writes `namespace ns { … }` and includes a header that does
///   the same has both sets of names. So those are merged, the buffer winning a name they both declare;
/// * the **global name space** is a namespace, and is the namespace every included header writes into — the same
///   merge, with the file scope of the buffer as the first contributor.
///
/// # What it will not offer
///
/// A **local** declaration of another file is never in the list. The index cannot place a local in the function it
/// belongs to, so a name lookup there could only guess — and the guess would be wrong in the common case rather
/// than the rare one: the standard library's headers alone declare thousands of locals called `__first` and `n`.
/// See [`DeclFact::local`]. Locals of the file being edited *are* offered, from its own scope tree, which is the
/// only thing that knows which body they are in.
///
/// # The answers
///
/// * `Yes(completions)` — the names, which may be an empty list for a scope that is declared and empty.
/// * `Unknown(UnparsableName)` — the cursor is not in a name or a qualifier: on punctuation, on a keyword, or past
///   the end of what could be one.
/// * `Unknown(NotDeclaredHere)` — the `::` named a scope nothing here declares, so there is nothing to list and no
///   way to say it is empty.
/// * `Unknown(ConditionalCompilation)` — the scope is declared, but only in a file reached through a guarded
///   `#include`, so whether these names are visible depends on macros this layer does not have.
///
/// **The names a documentation comment's target may be**, listed from the index rather than from the tree.
///
/// A doc comment is **not a scope**: `@see Wid|` sits in prose, the parser builds no name node there, and the name
/// query at that offset answers "nothing is declared here" — which is true and useless. What the reader means is a
/// program name, and the names visible in this file are exactly that: the same global list a name position gets, with
/// the same prefix filter and the same ranking.
///
/// `range` is the word being typed, in the text the client holds — the whole of what the edit must cover. It is
/// carried here rather than computed later because the doc comment is the only reader that knows it: the tree has no
/// node whose range is a comment's word, and a caller that had to re-derive it would be a second reader of the same
/// rule, free to disagree.
pub fn names_for_a_doc_comment(
    index: &ProjectIndex,
    path: &Path,
    scope: &str,
    written: &str,
    range: cpp_parser::SourceRange,
) -> NameCompletions {
    let names = if scope.is_empty() {
        declarations_in_scope(index, path, None, written)
    } else {
        declarations_in_scope(index, path, Some(scope), written)
    };
    NameCompletions {
        scope: scope.to_string(),
        names: names
            .into_iter()
            .map(|candidate| OfferedName {
                fact: candidate.fact,
                file: candidate.file,
                depth: 0,
                from: candidate.from,
            })
            .collect(),
        prefix: written.to_string(),
        name_range: range,
    }
}

pub fn name_completions_at(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<NameCompletions> {
    let Some(position) = crate::sema::resolve::name_position_at(root, offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };
    let phase = std::time::Instant::now();
    if crate::probes::completion() {
        eprintln!(
            "[completion] the position: scope {:?}, written {:?}",
            position.scope, position.written
        );
    }

    // **What is typed decides how much is read**, and the difference is not small. The index half of this query
    // collects every declaration of every file the cursor can see — measured on a real file that includes
    // `<string>`, **1103** names at one cursor — and collecting them to then drop all but the ones beginning with
    // `w` is work with no answer in it. A client asks on every keystroke, so the prefix is passed down and applied
    // where the names are still borrowed: see [`ProjectIndex::visible_declarations`].
    //
    // An **empty prefix must not filter**: that is the state the whole feature exists for (`return |`, a blank
    // line), and a filter that treated "nothing written" as "nothing matches" would empty the list exactly when it
    // is most wanted.
    let names = if position.scope.is_empty() {
        match visible_names(index, scopes, root, path, offset, &position.written) {
            Known::Yes(names) => names,
            Known::Unknown(reason) => return Known::Unknown(reason),
            Known::No => return Known::Unknown(UnknownReason::UnparsableName),
        }
    } else {
        match names_in_a_scope(
            index,
            scopes,
            root,
            path,
            &position.scope,
            &position.written,
        ) {
            Known::Yes(names) => names,
            Known::Unknown(reason) => return Known::Unknown(reason),
            Known::No => return Known::Unknown(UnknownReason::UnparsableName),
        }
    };
    if crate::probes::completion() {
        eprintln!(
            "[completion] the listing took {:?} for {} name(s)",
            phase.elapsed(),
            names.len()
        );
    }

    // **The file's own macros**, which the index half cannot see. A `#define` is a preprocessing fact of *this* file, not a
    // declaration in any scope — measured at four cursors in one file: `MA` offered only `main`, `MAX` offered nothing, and
    // `WIDTH` offered 89 names without itself. A macro is offered with `MacroLike`, which the ranking already places after
    // variables and before keywords, and it is a `ThisFile` provenance because that is where a `#define` lives.
    let mut names = names;
    // **The macro tail belongs to an ordinary name position, not to a `::`-qualified one.** A macro
    // is not a member of anything: `std::NOATOM` is legal only because the preprocessor replaced
    // the name before the compiler saw it, and a reader who typed `std::` is asking about `std`.
    // Appending the macros there was not merely noise — it **crowded the list out**: with an empty
    // prefix the ranking falls through to the label, and every macro of the headers the file
    // includes sorts before every lowercase name, so `ITEM_BUDGET` was spent on `NOATOM`,
    // `_WINDOWS_`, `_GLIBCXX_…` and `string`, `format`, `expected` were cut.
    if position.scope.is_empty()
        && let Some(summary) = index.summary(path)
    {
        // **The file's own macros and those of the headers it includes directly.** A `#define` is a preprocessing fact of one
        // file, so the index half of this query cannot see either — and a header's macros are the common case: a project's
        // `config.h` defines the names its sources use. The direct includes are the boundary: a macro two headers away is
        // reachable only through the macro environment, which is the hover's query, not this one's.
        // **The position is carried, not just the name.**
        //
        // The item's identity — the `(file, offset)` a client echoes back to
        // `completionItem/resolve` — is the declaration's **name range**, and a macro has no
        // declaration fact: the item is built from a clone of the file's first declaration (see the
        // loop below). Without the macro's own range in hand the clone kept the template's, so
        // resolve looked up `int main()` and the popup showed `FULL_NAME_FORMAT` with the detail
        // **`int ()`**. Measured twice: once before the clone's fields were cleared, and again
        // after they were, because the range was the field nobody had replaced.
        // **The macro's own file, not the file that includes it.** A `#define` in `<atomic>` is a fact of `<atomic>`:
        // the identity a client echoes back to `completionItem/resolve` is a `(file, offset)` pair, so filing a
        // header's macro under the cursor's file pointed at an offset in a *different* file. And the provenance is
        // what the ranking reads — every one of these used to be `ThisFile`, which made a hundred MSVC macros tie
        // with the reader's own locals and rank **above `nullptr` and every other keyword**: measured at an argument
        // of a file including `<atomic>`, `<bitset>`, `<chrono>` and `<complex>`, the list opened with
        // `ATOMIC_BOOL_LOCK_FREE` and `nullptr` was past the twentieth item. See `NameProvenance` for the tiers.
        let mut offered_macros: Vec<(&str, bool, cpp_parser::SourceRange, &Path)> = Vec::new();
        for fact in &summary.macros {
            offered_macros.push((
                fact.name.as_str(),
                fact.function_like,
                fact.range,
                &summary.path,
            ));
        }
        for include in &summary.includes {
            let Some(resolved) = &include.resolved else {
                continue;
            };
            let Some(included) = index.summary(resolved) else {
                continue;
            };
            for fact in &included.macros {
                offered_macros.push((
                    fact.name.as_str(),
                    fact.function_like,
                    fact.range,
                    included.path.as_path(),
                ));
            }
        }
        offered_macros.sort_by(|left, right| left.0.cmp(right.0));
        offered_macros.dedup_by(|left, right| left.0 == right.0);

        for (name, function_like, range, written_in) in offered_macros {
            if !position.written.is_empty() && !name.starts_with(&position.written) {
                continue;
            }
            // A fact cloned from one of the file's own declarations rather than built from nothing: `DeclFact` has no
            // `Default` (its `DeclKind`, `SourceRange` and `FactGuard` do not), and adding one for this would put a
            // meaningless declaration shape in the type.
            //
            // **What the clone keeps is the shape and nothing else**, and that is a correction
            // rather than a tidy-up: everything the template said about *itself* — the return type,
            // the parameter list, the template parameters, the bases — belongs to a different
            // declaration, and this fact speaks for a macro.
            let Some(template) = summary.declarations.first() else {
                continue;
            };
            let mut declaration = template.clone();
            declaration.name = name.into();
            declaration.kind = crate::DeclKind::MacroLike;
            // Nothing of the template's *declaration* survives: a macro has no type, no return type, no
            // parameters of its own and no bases, and a `#define` is never local.
            declaration.type_of = None;
            declaration.returns = None;
            declaration.detail_mut().bases.clear();
            declaration.detail_mut().parameters.clear();
            declaration.local = false;
            // **The clone's position is the macro's own.** This is the field that decided what the popup *said*
            // about the macro: the identity a client echoes back to `completionItem/resolve` is a `(file, offset)`
            // pair, and with the template's range it named `int main()` — so resolving the item read `main`'s
            // declaration and the detail line under `FULL_NAME_FORMAT` was **`int ()`**. Clearing the clone's other
            // fields did not help, because the offset was never the macro's.
            //
            // A `#define` is at file scope and in no namespace of its own, whatever the template happened to be.
            declaration.name_range = range;
            declaration.range = range;
            declaration.scope = None;
            declaration.detail_mut().in_namespace = None;
            // **A function-like macro is offered with its parenthesis**, which is the only hint a completion item can carry
            // about how it is used: `MAX(` tells the reader the name takes arguments, and `MAX` alone does not.
            declaration.detail_mut().parameter_list = function_like.then(|| "(".to_string());
            let ours = written_in == summary.path.as_path();
            names.push(OfferedName {
                file: written_in.to_path_buf(),
                fact: declaration,
                depth: 0,
                from: match ours {
                    true => NameProvenance::ThisFile,
                    false => NameProvenance::DirectInclude,
                },
            });
        }
    }

    Known::Yes(NameCompletions {
        scope: position.scope,
        names,
        prefix: position.written,
        name_range: position.range,
    })
}

/// **One completion query, with its own measurement reported to the log** — see [`query_trace`].
///
/// The server's log line splits a completion into `prepare`/`catch up`/`modules` and then `view`/`completions`;
/// this is what goes inside `completions`, which is the half no lock can explain. A wrapper rather than a call to
/// asks and the query has several ways to return early.
pub fn name_completions_traced(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
) -> Known<NameCompletions> {
    name_completions_at(index, scopes, root, path, offset)
}

/// Every name visible from a cursor with no qualifier written: the scope chain, then the index.
///
/// The order is C++'s: the scope the cursor is in, then each enclosing scope outward, and only then the names other
/// files contribute. Each step's own answer is already a list — the tree's bindings, a class's members including
/// its bases, a namespace's declarations across the project — so this walks the chain and hands each depth to the
/// function that knows how to list it.
pub(crate) fn visible_names(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    offset: usize,
    written: &str,
) -> Known<Vec<OfferedName>> {
    let Some(innermost) = scopes.scope_at(offset) else {
        return Known::Unknown(UnknownReason::UnparsableName);
    };

    let chain = scopes.scope_chain(innermost);
    let mut names: Vec<OfferedName> = Vec::new();
    let phase = std::time::Instant::now();

    // **The class a separated definition belongs to.** `int Source::GetLine(int) { … }` in a `.cpp` declares no scope for
    // `Source` here — the class is in a header — so the scope chain below has no class to offer members from, and a
    // completion in that body named none of `Source`. The declaration node still says which class it is, and this reads it
    // from the parser's own nodes: the declarator's qualified name lives in the declaration's specifier sequence. Nothing is
    // synthesized in the scope tree, which is what keeps `resolve`'s rule intact (a qualifier names nothing until something
    // declares it).
    if let Some(class) = the_class_a_definition_belongs_to(root, offset)
        && let Known::Yes(members) = names_of_a_class(index, scopes, root, path, &class)
    {
        names.extend(offered(members, 0));
    }

    // The file the cursor is in, as the store spells paths, so that the declaration-order filter can tell this
    // file's offsets from another file's.
    let in_this_file = normalize(path);
    if crate::probes::completion() {
        eprintln!("[completion]   the class tier: {:?}", phase.elapsed());
    }
    let chain_phase = std::time::Instant::now();

    for (depth, scope) in chain.iter().copied().enumerate() {
        let Some(data) = scopes.scope(scope) else {
            continue;
        };

        // The file scope is the global name space, and it is the one scope whose names other files contribute to
        // as well — every included header is written there. It is listed below, once, rather than here.
        if data.kind == crate::ScopeKind::TranslationUnit {
            continue;
        }

        // A **class** in the chain — the cursor is inside a member function of it — brings its members, bases
        // included: `size` is nameable inside `Widget::g` without a qualifier, and so is anything a base declares.
        if matches!(data.kind, crate::ScopeKind::Class | crate::ScopeKind::Enum)
            && let Some(class) = scopes.qualification_prefix_of(scope)
        {
            let members = match names_of_a_class(index, scopes, root, path, &class) {
                Known::Yes(members) => members,
                Known::Unknown(reason) => return Known::Unknown(reason),
                Known::No => Vec::new(),
            };
            names.extend(offered(members, depth));
            continue;
        }

        // Anything else in the chain is a body, a block or a lambda: its own bindings, which the tree has — and a
        // **namespace**, whose bindings are neither: the kind is what decides, and it is also what decides whether
        // a name beginning with an underscore belongs to the user (a body) or to the global name space (a `_name`
        // at file scope is the implementation's, one inside a function is not).
        let body = matches!(
            data.kind,
            crate::ScopeKind::Block | crate::ScopeKind::Function | crate::ScopeKind::Lambda
        );
        names.extend(offered(
            // **In a body, only what is declared above the cursor is in scope.** A *scope* holds every binding
            // written in its braces, so a list built from the scope contains `int c = a + b;` at a cursor three
            // lines above it — a name that cannot be written there, because the language has not declared it yet.
            // It is the same rule a reader applies without thinking, and the one a user reported: "在我下面声明的
            // 变量不应该补全出来".
            //
            // Only bodies: a class's members and a namespace's names are visible in **declaration order
            // independent** fashion (`class C { void f() { x = 1; } int x; };` is legal), so the filter would be
            // wrong there. Nothing is lost by the narrower rule — a body is exactly where the *textual* order is
            // the language's rule.
            bindings_of(root, path, &data.bindings, None, body)
                .into_iter()
                .filter(|candidate| !body || declared_above(candidate, offset, &in_this_file))
                .collect(),
            depth,
        ));

        // …and for a **namespace**, the names other files write into it. A namespace is reopened by every file
        // that mentions it, so the buffer's list is never the whole answer.
        if let Some(prefix) = scopes.qualification_prefix_of(scope) {
            names.extend(offered(
                declarations_in(&prefix, index, path, written),
                depth,
            ));
        }
    }

    // The global name space: the buffer's own file scope, and every file it includes. One step past the chain, so
    // that the depth stays an ordinary count of how far the lookup walked — the file scope is a scope like any
    // other, it is just not one the chain walked through.
    if crate::probes::completion() {
        eprintln!("[completion]   the chain: {:?}", chain_phase.elapsed());
    }
    let global_phase = std::time::Instant::now();
    names.extend(global_offers(
        index,
        scopes,
        root,
        path,
        chain.len(),
        written,
    ));
    if crate::probes::completion() {
        eprintln!(
            "[completion]   the global tier: {:?}",
            global_phase.elapsed()
        );
    }

    let sort_phase = std::time::Instant::now();
    sort_and_hide(&mut names);
    if crate::probes::completion() {
        eprintln!("[completion]   the sort: {:?}", sort_phase.elapsed());
    }
    Known::Yes(names)
}

/// The names at **file scope**, ready to offer: the buffer's own and every included file's, deduplicated.
///
/// A helper rather than two lines at each call site because the deduplication is the part that is easy to forget
/// and impossible to notice: the buffer's file-scope names and the index's are the *same* declarations whenever
/// the file being edited has been saved, so a list that skipped this step offers every global name twice.
pub(crate) fn global_offers(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    depth: usize,
    written: &str,
) -> Vec<OfferedName> {
    let mut names = offered(global_names(index, scopes, root, path, written), depth);
    sort_and_hide(&mut names);
    names
}

/// **Does this spelling name a type?** — the question that decides whether a qualifier is completed as a class
/// (its members, `members_of`) or as a namespace (the guard-evaluating visibility walk, which the index's own note
/// measures at **1.2 s**).
///
/// # Why "every candidate is a `Type`" was wrong
///
/// It is the rule that let `std::vector<int>::` work (a class template has a forward declaration in `<iosfwd>` and a
/// definition in `<vector>`, so two `Type`s), and it is false for **every class that writes its own constructor**:
/// `class LSPHandle { public: LSPHandle(LanguageServer *); … };` puts a `Function` named `LSPHandle` (scope
/// `LSPHandle`) beside the class, so "all candidates are types" says no — and the completion after `LSPHandle::` fell
/// to the namespace route. Measured on `E:\EmmyLuaCodeStyle`, `CodeFormatServer/src/LSP/LSPHandle.cpp`:
/// **`name_completions` 1.14 s** against `members_of`'s **16 µs** for the same class.
///
/// # The rule this uses instead
///
/// A spelling names a type when **some** candidate is a `Type` and **every other candidate is a member of it** — its
/// `scope` is the spelling itself, which is exactly where a constructor, a destructor and a same-named nested name
/// are filed. A *namespace* of that name has no such candidate (its facts' scope is the namespace's parent), so it
/// keeps the namespace route; and the alias case (`std::string`) is untouched, because `definition` answers it
/// singularly today.
pub(crate) fn a_spelling_names_a_type(index: &ProjectIndex, spelling: &str, path: &Path) -> bool {
    let Some(found) = index.definitions(spelling, path).value() else {
        return false;
    };
    let mut any_type = false;
    for declaration in &found.found {
        if declaration.fact.kind == crate::DeclKind::Type {
            any_type = true;
            continue;
        }
        // Everything else has to be *inside* the type: a constructor, a destructor, or a nested name of the same
        // spelling. A namespace's own facts fail this, which is what keeps `ns::` on the namespace route.
        if declaration.fact.scope.as_deref() != Some(spelling) {
            return false;
        }
    }
    any_type
}

/// The names written directly in one `::`-qualified scope.
pub(crate) fn names_in_a_scope(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    scope: &str,
    written: &str,
) -> Known<Vec<OfferedName>> {
    names_in_a_scope_at(index, scopes, root, path, scope, written, 0)
}

/// The same query, with how many aliases have been followed — the guard is what makes a cycle in
/// `namespace a = b; namespace b = a;` terminate rather than recurse for ever.
#[allow(clippy::too_many_arguments)]
pub(crate) fn names_in_a_scope_at(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    scope: &str,
    written: &str,
    depth: usize,
) -> Known<Vec<OfferedName>> {
    // The global name space has no spelling of its own: `::` is what the file writes, and `None` is what a
    // declaration at file scope records.
    if scope == "::" {
        return Known::Yes(global_offers(index, scopes, root, path, 0, written));
    }

    // A leading `::` on a longer path — `::ns::Widget` — asks for the *global* one, and the spelling to look up is
    // the rest: a fact records `ns::Widget` and never `::ns::Widget`, for the same reason.
    let spelling = scope.trim_start_matches("::");

    let in_the_tree = scopes.scope_with_qualified_name(spelling);
    let kind = in_the_tree
        .and_then(|id| scopes.scope(id))
        .map(|scope| scope.kind);

    let mut names: Vec<OfferedName> = Vec::new();

    match kind {
        // A class is defined once, so its members come from one place — and the walk over its bases is
        // `members_of`'s job rather than this query's.
        Some(crate::ScopeKind::Class | crate::ScopeKind::Enum) => {
            let members = match names_of_a_class(index, scopes, root, path, spelling) {
                Known::Yes(members) => members,
                Known::Unknown(reason) => return Known::Unknown(reason),
                Known::No => Vec::new(),
            };
            names.extend(offered(members, 0));
        }
        // A namespace can be reopened, so the buffer's names and the project's are both part of the answer.
        Some(_) => {
            if let Some(data) = in_the_tree.and_then(|id| scopes.scope(id)) {
                // `false`: a namespace is not a body, so a `_name` in it is not a local — the reservation that
                // applies to a name beginning with an underscore is the *global* name space's, and the spelling
                // here is the namespace's own.
                names.extend(offered(
                    bindings_of(root, path, &data.bindings, Some(spelling), false),
                    0,
                ));
            }
            names.extend(offered(declarations_in(spelling, index, path, written), 0));

            // **A namespace alias is a scope with no members of its own**, and its target is where the names are.
            // `namespace views = ranges::views;` gives `std::views` an empty namespace scope — the walker needs one
            // for the name to resolve — and every adapter a reader types after `std::views::` is declared in
            // `std::ranges::views`. Measured on the corpus's `ranges.cpp`: completion after `std::views::` answered
            // **0** names, while `filter`, `transform`, `reverse`, `drop` and `take` are all there.
            //
            // Only on the empty answer, so an ordinary `std::` never pays for the tree walk this needs.
            if names.is_empty()
                && depth < 4
                && let Some(id) = in_the_tree
                && let Some(target) = the_target_of_a_namespace_alias(root, scopes, id)
            {
                let target = the_target_in_its_own_scope(scopes, spelling, &target);
                if target != spelling {
                    return names_in_a_scope_at(index, scopes, root, path, &target, written, 0);
                }
            }
        }
        // Not in the buffer at all: the project's answer, or nothing.
        None => {
            // **A qualifier that names a *type* rather than a scope in this file.** The scope tree is the buffer's
            // own, so `std::string` — an alias of a class template, declared in `<xstring>` — is not a scope it can
            // name, and `Some(Class)` above never sees it. A class is defined once, so its members are the answer,
            // and `members_of` is the query that resolves an alias, a template spelling and the bases: measured on
            // a file that includes `<string>`, completion after `std::string::` offered **0** names before this and
            // every member of `basic_string` after it.
            //
            // **Asked only of a name the index says is a type**, and that test is the whole reason this is not
            // simply tried first: `members_of` answers for a spelling that no class declares — it lists the
            // declarations whose *scope* is that name — so a namespace qualifier sent down this path (`ns::`,
            // `std::`) silently took the member route, which has no budget and no dedup by hiding. A namespace is
            // [`DeclKind::Namespace`] and is answered below, where the collection budget applies.
            // **Is the spelling a type?** — asked three ways, because one way is not enough for the standard library:
            //
            // ```text
            //   `definition` says Yes(Type)          an ordinary class, declared once and visible
            //   `definition` says Unknown(Ambiguous)  **the ordinary case for a class template**: `std::vector` is
            //                                        forward-declared in `<iosfwd>` and defined in `<vector>`, so the
            //                                        singular query declines. Every candidate being a Type is the
            //                                        answer, and it is what `std::vector<int>::` needed — it offered
            //                                        nothing while the definition query was the only test.
            //   an alias in this index                 `std::string` is `using string = basic_string<char, …>;`, so
            //                                        the name is an *alias* fact whose `scope` is `std` and whose
            //                                        target is the class that has the members. Measured: without this
            //                                        arm `std::string::` offered **0** items.
            // ```
            let names_a_type = a_spelling_names_a_type(index, spelling, path);
            if crate::probes::members() {
                let bare = spelling.rsplit("::").next().unwrap_or(spelling);
                let facts: Vec<String> = index
                    .every_fact_named(bare)
                    .into_iter()
                    .map(|(file, fact)| {
                        format!(
                            "{:?} scope {:?} kind {:?} @ {}",
                            fact.name,
                            fact.scope,
                            fact.kind,
                            file.file_name().unwrap_or_default().to_string_lossy()
                        )
                    })
                    .collect();
                eprintln!(
                    "    names_in_a_scope({spelling}): in the tree {}, names_a_type {names_a_type}, \
                     definition {:?} | facts named {bare:?}: {facts:?}",
                    in_the_tree.is_some(),
                    match index.definition(spelling, path) {
                        Known::Yes(found) =>
                            format!("Yes({} {:?})", found.fact.qualified_name(), found.fact.kind),
                        Known::Unknown(reason) => format!("Unknown({reason:?})"),
                        Known::No => "No".to_string(),
                    }
                );
                if spelling == "std::string" {
                    let xstring = index.summaries_named_like("xstring");
                    let string = index.summaries_named_like("string");
                    eprintln!(
                        "      index: {} summaries, {} distinct names | by_name: string {} basic_string {} \
                         vector {} | by_scope: std::basic_string {} | files: xstring {} string-ish {}",
                        index.len(),
                        index.distinct_names(),
                        index.name_postings("string"),
                        index.name_postings("basic_string"),
                        index.name_postings("vector"),
                        index.scope_postings("std::basic_string"),
                        xstring.len(),
                        string.len(),
                    );
                    for path in string.iter().take(6) {
                        eprintln!("        {}", path.display());
                    }

                    // **What the summary of `<xstring>` actually says**, which is the question that separates
                    // "the class was never recorded" from "it was recorded under a name nothing asks for".
                    // `basic_string` is written at `xstring:2344` as
                    // `template <class _Elem, class _Traits, class _Alloc> class basic_string`, so a reading that
                    // dropped it lost it in the *fact builder* rather than in any lookup.
                    for (path, summary) in index.all_summaries() {
                        let name = path.to_string_lossy();
                        if !name.ends_with("xstring") && !name.ends_with("include/string") {
                            continue;
                        }
                        let mut named: Vec<String> = summary
                            .declarations
                            .iter()
                            .filter(|fact| fact.name.contains("basic"))
                            .map(|fact| {
                                format!(
                                    "{:?} kind {:?} scope {:?} local {}",
                                    fact.name, fact.kind, fact.scope, fact.local
                                )
                            })
                            .collect();
                        named.sort();
                        named.dedup();
                        eprintln!(
                            "      {} has {} declaration(s); the {} named *basic*: {:?}",
                            name,
                            summary.declarations.len(),
                            named.len(),
                            named.iter().take(14).collect::<Vec<_>>()
                        );
                    }
                }
            }

            if names_a_type {
                // **An alias is followed to the class that has the members.** `std::string` is
                // `using string = basic_string<char, char_traits<char>, allocator<char>>;` — a fact whose `scope`
                // is `std`, not `std::string` — so `members_of("std::string")` looks for a class by that name and
                // finds nothing. Measured: `std::string::` offered **0** items, and it is the most-used class in
                // the library. `resolve_aliases` is the same resolution `members_of` does for a member's own type,
                // so the two cannot disagree about what the spelling names.
                let named = resolve_aliases(
                    &crate::sema::declarations::ShapeTable::new(),
                    index,
                    scopes,
                    root,
                    path,
                    spelling,
                );
                let named = named.as_ref();
                if crate::probes::members() {
                    eprintln!("    names_a_type({spelling}) -> alias resolved to {named:?}");
                }
                match names_of_a_class(index, scopes, root, path, named) {
                    Known::Yes(members) => names.extend(offered(members, 0)),
                    Known::Unknown(reason) => return Known::Unknown(reason),
                    Known::No => {}
                }
            } else {
                let found = declarations_in(spelling, index, path, written);
                if found.is_empty() {
                    // **A namespace alias, declared in a header the buffer does not hold.** The scope tree is the
                    // buffer's own, so `std::views` — `namespace views = ranges::views;` in `<ranges>` — is not a
                    // scope it can name, and the index records the target on the alias's own fact. Measured on the
                    // corpus's `ranges.cpp`: completion after `std::views::` answered **0** names, while every
                    // adapter is declared in `std::ranges::views`.
                    //
                    // The target is looked up where the alias sits (`std::ranges::views` for `ranges::views` in
                    // `std`) and then as written, because which of the two is right depends on the alias's scope.
                    if depth < 4
                        && let Known::Yes(alias) = index.definition(spelling, path)
                        && alias.fact.kind == crate::DeclKind::Namespace
                        && let Some(target) = alias.fact.type_of.as_deref()
                    {
                        let mut candidates: Vec<String> = Vec::new();
                        if let Some(scope) = alias.fact.scope.as_deref() {
                            candidates.push(format!("{scope}::{target}"));
                        }
                        candidates.push(target.trim_start_matches("::").to_string());
                        for candidate in candidates {
                            if candidate != spelling
                                && !declarations_in(&candidate, index, path, written).is_empty()
                            {
                                return names_in_a_scope_at(
                                    index, scopes, root, path, &candidate, written, 0,
                                );
                            }
                        }
                    }
                    // Empty is either "declared and empty" or "no such scope", and the difference is the *name* —
                    // the same distinction `direct_members` makes, for the same reason.
                    return match index.definition(spelling, path) {
                        Known::Yes(found) if found.fact.qualified_name() == spelling => {
                            Known::Yes(Vec::new())
                        }
                        Known::Unknown(reason) => Known::Unknown(reason),
                        _ => Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(spelling))),
                    };
                }
                names.extend(offered(found, 0));
            }
        }
    }

    sort_and_hide(&mut names);
    Known::Yes(names)
}

/// **What a namespace alias names**, or `None` when this scope is a namespace with a body of its own.
///
/// `namespace views = ranges::views;` parses as a `NamespaceDecl` with **no compound statement** and a `NameExpr`
/// after the `=`; the scope walker creates a scope for it so that the name resolves, and that scope is empty by
/// construction. The tree is asked rather than the text because the two are already in hand and the shape says it
/// without spelling rules: a namespace declaration with a body is an ordinary one, with a target and no body is an
/// alias.
pub(crate) fn the_target_of_a_namespace_alias(
    root: &cpp_parser::CppSyntaxNode,
    scopes: &crate::ScopeTree,
    id: crate::sema::symbol::ScopeId,
) -> Option<String> {
    let scope = scopes.scope(id)?;
    if scope.kind != crate::ScopeKind::Namespace {
        return None;
    }
    let range = scope.range?;
    let node = root.descendants().find(|node| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::NamespaceDecl
            && cpp_parser::source_range(node.text_range()) == range
    })?;
    if node.children().any(|child| {
        cpp_parser::CppSyntaxKind::from(child.kind()) == cpp_parser::CppSyntaxKind::CompoundStat
    }) {
        return None;
    }
    node.children()
        .find(|child| {
            cpp_parser::CppSyntaxKind::from(child.kind()) == cpp_parser::CppSyntaxKind::NameExpr
        })
        .map(|child| child.text().to_string())
}

/// **Where an alias's target is looked up from.** The language looks a target up in the scope the alias is declared
/// in, so `namespace views = ranges::views;` inside `std` names `std::ranges::views`; a target that already resolves
/// on its own — `namespace fs = std::filesystem;` at file scope — is used as written.
pub(crate) fn the_target_in_its_own_scope(
    scopes: &crate::ScopeTree,
    alias: &str,
    target: &str,
) -> String {
    let target = target.trim().trim_start_matches("::");
    if let Some(at) = alias.rfind("::") {
        let nearby = format!("{}::{target}", &alias[..at]);
        if scopes.scope_with_qualified_name(&nearby).is_some() {
            return nearby;
        }
    }
    if scopes.scope_with_qualified_name(target).is_some() {
        return target.to_string();
    }
    target.to_string()
}

/// **Is this declaration above the cursor**, so that the language has reached it?
///
/// The rule a reader applies without thinking, and the one a user reported as missing ("在我下面声明的变量不应该补全
/// 出来"): `int sum = a + b;` three lines *below* the cursor is not a name that can be written at the cursor, and a
/// list built from a scope's bindings contains it anyway — a scope holds every binding written inside its braces.
///
/// # Where it applies, and where it must not
///
/// Only in a **body** (a function, a block, a lambda), because that is the one place where textual order *is* the
/// language's rule. A class's members and a namespace's names are reachable regardless of where they are written —
/// `class C { void f() { x = 1; } int x; };` is perfectly legal — so filtering those by position would remove names
/// a reader can legitimately write.
///
/// # The one thing it will not judge
///
/// A binding from **another file**: its range is an offset into that file, so comparing it with this file's cursor
/// compares two different rulers. Those are kept — the conservative direction, and the same one the rest of this
/// module takes: a name that is in scope and not offered is a missing answer, while a name that is not in scope yet
/// and *is* offered is one the reader sees as a mistake.
///
/// In practice every binding here is the buffer's own — `build_scopes` walks one file's tree, and a name reached
/// through an include is a [`NameProvenance::DirectInclude`] fact from the index rather than a binding — so this is
/// the guard that keeps the rule from being wrong if that ever changes.
pub(crate) fn declared_above(candidate: &Candidate, offset: usize, in_this_file: &str) -> bool {
    if candidate.file != Path::new(in_this_file) {
        return true;
    }

    let at = candidate.fact.name_range.start_offset;

    // A zero-length range at the start of the file is what a recovery produces, and it is not a position.
    at == 0 || at < offset
}

/// One name on its way to being offered: where it was declared, how far inside one answer it was, and how near the
/// cursor that is.
///
/// Named rather than a tuple because there are four of them now and two of the four are `usize`; a call site that
/// had to remember which was which is how a ranking ends up ordered by the wrong number.
pub(crate) struct Candidate {
    pub(crate) file: PathBuf,
    pub(crate) fact: DeclFact,
    /// How far *inside* one answer the name was found — a base class's member is one step further than the class's
    /// own — which is added to the depth of the scope the answer came from.
    pub(crate) steps: usize,
    pub(crate) from: NameProvenance,
}

impl Candidate {
    pub(crate) fn new(file: PathBuf, fact: DeclFact, from: NameProvenance) -> Candidate {
        Candidate {
            file,
            fact,
            steps: 0,
            from,
        }
    }
}

/// How many declarations from **every visible file at once** one name query will collect.
///
/// A backstop on work rather than a statement about the answer, and the number is derived from what the answer can
/// be: `crate::completion` shows at most two hundred items, and every one of them is scored by *how near the cursor
/// its declaration is*. A query that has already collected this many names — all of them from files the cursor
/// reaches through includes, which is the tier that ranks last — cannot have its answer changed by the next one,
/// and on a file that includes `<string>` and the C++ standard library there are **26 728** declarations in the
/// files one cursor can see.
///
/// The file's **own** declarations are not capped: they come from the scope tree rather than from here, they are
/// the tier that ranks first, and there are as many of them as the reader wrote.
///
/// This is the budget for the **global** name space (`::`, and a cursor at file scope). For one named scope see
/// [`MAX_COLLECTED_IN_A_SCOPE`], which is a different question with a different answer.
pub(crate) const MAX_COLLECTED_NAMES: usize = 400;

/// How many declarations one **named scope** may contribute to a list — `std::`, `ns::`, `Widget::`.
///
/// # Why this is not [`MAX_COLLECTED_NAMES`]
///
/// Because that budget is spent in **file order**, and a scope's names are spread over the files that declare them.
/// Measured on a file that includes `<cstdio>`, `<iostream>`, `<optional>`, `<string>` and `<format>`: completion
/// after `std::` offered **175** names, and the ones it offered were whatever the first 400 declarations of the
/// visibility walk happened to be — `cin`, `cout`, `getline`, `make_optional`, `nullopt` — while `string` (declared
/// in `<xstring>`, reached through `<string>`) and everything in `<format>` were past the cut. The list a reader
/// saw therefore depended on the order of their own `#include` lines, which is not a property of the program.
///
/// The honest number is the size of the scope, because a namespace holds what it holds: `std` is **2 008**
/// declarations on this project, and every namespace in the C++ standard library is inside that. The display budget
/// ([`crate::completion`]'s `ITEM_BUDGET`, two hundred items) is what keeps the *message* small, and it reports
/// itself as `truncated` so the client re-asks as the reader types — where the prefix filter, which is applied
/// before anything is cloned, makes the list exact.
///
/// The cost of the larger budget is one `DeclFact` clone per surviving name, and only for a cursor whose prefix is
/// short enough to keep them: the walk itself is one pass over the closure.
pub(crate) const MAX_COLLECTED_IN_A_SCOPE: usize = 4096;

/// How many declarations one name may have before a workspace search stops ordering them — see
/// `ProjectIndex::open_group`.
pub(crate) const POPULAR_NAME: usize = 4096;

/// The bindings one of the file's own scopes holds, as names to offer. — `ns` for `namespace ns { … }`, a class's name for
/// its body — and `None` for a scope that has none: a function body, a block, a lambda. The distinction is the one
/// [`DeclFact::scope`] documents, and it is passed rather than derived because the caller is the one that knows
/// *why* it is listing these bindings.
///
/// `local` is the other half of that and it is **not** derivable from `scope`, which is why it is a parameter:
/// `None` is passed by two callers with different meanings — the **file scope** (whose names are the global name
/// space's) and a **body** (whose names are the user's locals) — and the difference decides what may be offered.
/// A `_name` at file scope is reserved to the implementation; the same spelling inside a function is the user's own
/// and is offered. Filling this in wrong is not visible anywhere else: the fact's `local` field is only read by
/// [`is_reserved_to_the_implementation`] on this path, and a local marked global silently disappears from the list.
pub(crate) fn bindings_of(
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    bindings: &[crate::Binding],
    scope: Option<&str>,
    local: bool,
) -> Vec<Candidate> {
    let spelling = scope.unwrap_or_default();

    bindings
        .iter()
        .map(|binding| {
            let mut fact = fact_from_binding(root, spelling, binding);
            fact.local = local;
            Candidate::new(path.to_path_buf(), fact, NameProvenance::Scope)
        })
        .collect()
}

/// The names written at **file scope**: this buffer's, and every included file's.
pub(crate) fn global_names(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    written: &str,
) -> Vec<Candidate> {
    let mut names: Vec<Candidate> = scopes
        .root()
        .and_then(|root_scope| scopes.scope(root_scope))
        // `false`: this **is** the global name space — the one scope where a name beginning with an underscore is
        // reserved to the implementation, which is why `Session::name_completions` does not offer `_Arg` or `_cprintf`.
        .map(|data| bindings_of(root, path, &data.bindings, None, false))
        .unwrap_or_default();

    // **The buffer's own file-scope names are the file's**, even though they live in the global name space: the
    // distinction the ranking needs is "did the reader write this here", not "which scope is it in".
    for name in &mut names {
        name.from = NameProvenance::ThisFile;
    }

    names.extend(declarations_in_scope(index, path, None, written));
    names
}

/// The facts the index holds for a scope spelling, or for the global name space when it is `None`.
///
/// The global case is the *absence* of a spelling rather than an empty one, which is why it cannot go through
/// [`ProjectIndex::declarations_in`]: that one compares a spelling, and a declaration at file scope records
/// `None`. It is the same distinction `matches` makes for a leading `::`. A **local** is left out here for the
/// reason [`DeclFact::local`] gives — a local also records `None`, and offering another file's would be the wrong
/// answer rather than a missing one.
pub(crate) fn declarations_in_scope(
    index: &ProjectIndex,
    visible_from: &Path,
    scope: Option<&str>,
    written: &str,
) -> Vec<Candidate> {
    // **The prefix is applied here, where the names are still borrowed.** Everything below this point is work per
    // declaration — a `DeclFact` clone, a provenance lookup, a `Vec` push — and on a file that includes `<string>`
    // there are over a thousand of them at one cursor position. Collecting them to drop all but those beginning
    // with `w` is the difference between a keystroke and a stutter, and it is a filter rather than a query: the
    // names it drops could not have been offered anyway. See [`crate::sema::resolve::name_position_at`].
    let accepts = |fact: &DeclFact| name_starts_with(fact, written);

    let found = match scope {
        Some(spelling) => index.declarations_in_where(spelling, visible_from, accepts),
        // **The full walk, and it has to be.** A cheap version of this arm (the `#include` graph plus a prefix over the
        // raw summaries) was measured and reverted: it cost 1.18 s either way — the time is in `bindings_of`'s type
        // inference, not here — and it changed what completion offers, because the walk's answer includes the **cooked**
        // reading's declarations (a name a macro writes) and excludes the branches a condition does not take. Two
        // handshake tests say so: `a_name_only_the_cooked_reading_declares_is_offered_over_the_wire` and
        // `a_c_reader_gets_completion_and_signature_help_for_what_the_file_defines`.
        None => index.visible_declarations_where(visible_from, |fact| {
            accepts(fact) && fact.scope.is_none() && !fact.local
        }),
    };

    // **Each of the two queries applied its own budget**, and this function must not apply a third: the global
    // name space is unbounded in size and the tier it produces ranks last ([`MAX_COLLECTED_NAMES`]), while one named
    // scope holds what it holds ([`MAX_COLLECTED_IN_A_SCOPE`]). A second cut here is what made `std::` offer 175 of
    // its 2 008 names.
    found
        .into_iter()
        .map(|declaration| {
            Candidate::new(
                declaration.file.clone(),
                declaration.fact.clone(),
                provenance_of(
                    index,
                    visible_from,
                    declaration.visibility,
                    &declaration.file,
                ),
            )
        })
        .collect()
}

/// Does this declaration's name begin with what is being typed?
///
/// Case-insensitive, and **`true` for an empty prefix** — the state the whole feature is for. The rule is a
/// *filter* and not a ranking: the offer list drops a name this rejects, because a name the reader has not started
/// typing cannot be what they meant and the client would filter it out anyway. What it must not do is decide
/// between two names that both match; that is `crate::completion`'s job.
pub(crate) fn name_starts_with(fact: &DeclFact, written: &str) -> bool {
    if written.is_empty() {
        return true;
    }

    let name = fact.name.as_str();
    name.len() >= written.len()
        && name
            .chars()
            .zip(written.chars())
            .all(|(have, wanted)| have.eq_ignore_ascii_case(&wanted))
}

/// [`declarations_in_scope`] for a named scope, spelled the way this module spells lookups.
pub(crate) fn declarations_in(
    spelling: &str,
    index: &ProjectIndex,
    visible_from: &Path,
    written: &str,
) -> Vec<Candidate> {
    declarations_in_scope(index, visible_from, Some(spelling), written)
}

/// **How near the cursor a file is**, for the ranking: the buffer, a project header beside it, a header the buffer
/// includes, or one only those headers include.
///
/// The three cross-file answers are exactly the three the visibility walk produced — the file was reached by
/// crossing includes — so this only has to tell *which* of them a fact is, and the question it asks the graph is
/// the cheap one: does the file that declares this name appear among the cursor file's **own** `#include`s.
///
/// A file that is reachable only through a guarded `#include` ([`IncludeVisibility::Conditional`]) is ranked as an
/// indirect one, which is the honest place for it: it is in the list — the reader may be writing for exactly that
/// configuration — but it is not one of the headers this file certainly includes.
pub(crate) fn provenance_of(
    index: &ProjectIndex,
    visible_from: &Path,
    visibility: IncludeVisibility,
    declaring: &Path,
) -> NameProvenance {
    if visibility == IncludeVisibility::Conditional {
        return NameProvenance::IndirectInclude;
    }

    let declaring = normalize(declaring);
    let mut direct = false;

    if let Some(summary) = index.summary(visible_from) {
        for include in &summary.includes {
            let Some(resolved) = &include.resolved else {
                continue;
            };
            if normalize(resolved) == declaring {
                direct = true;
                break;
            }
        }
    }

    if direct {
        return NameProvenance::DirectInclude;
    }

    NameProvenance::IndirectInclude
}

/// The members of a class, bases included, as names to offer.
pub(crate) fn names_of_a_class(
    index: &ProjectIndex,
    scopes: &crate::ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Known<Vec<Candidate>> {
    match members_of(index, scopes, root, path, class) {
        Known::Yes(members) => Known::Yes(
            members
                .members
                .into_iter()
                .map(|member| {
                    // A class's own members are reachable from a cursor inside the class without a qualifier, and
                    // they are ranked as what they are — the nearest thing there is, beside a local. Which is why
                    // the provenance is `Scope` and the *depth* carries the inheritance: see `Score::LOCAL`.
                    let mut candidate =
                        Candidate::new(member.file, member.fact, NameProvenance::Scope);
                    candidate.steps = member.depth;
                    candidate
                })
                .collect(),
        ),
        Known::Unknown(reason) => Known::Unknown(reason),
        Known::No => Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(class))),
    }
}

/// Wrap candidates as offers, adding the depth of the scope they were found in and dropping the declarations that
/// have no name to type — and the names the implementation owns.
///
/// [`Candidate::steps`] is how far *inside* one answer the name was — a base class's members are one step further
/// than the class's own — and the two are added because they mean the same thing to a consumer: how far the lookup
/// had to walk before it found this name, which is the order C++ walks in too.
pub(crate) fn offered(found: Vec<Candidate>, depth: usize) -> Vec<OfferedName> {
    found
        .into_iter()
        .filter(|candidate| !candidate.fact.name.is_empty())
        .filter(|candidate| !is_reserved_to_the_implementation(&candidate.fact))
        .map(|candidate| OfferedName {
            file: candidate.file,
            fact: candidate.fact,
            depth: depth.saturating_add(candidate.steps),
            from: candidate.from,
        })
        .collect()
}

/// Is this name **reserved to the implementation**, and therefore not something a completion should offer?
///
/// The standard's own three rules, and nothing else:
///
/// ```text
/// __name    reserved everywhere                `__crt_…`, `__imp_…`
/// _Name     reserved everywhere                `_ALLOC_MASK`, `_Alty`, `FILE`? no — `FILE` is not reserved
/// _name     reserved **in the global namespace**   `_Arg`, `_Address`, `_cprintf`
/// ```
///
/// The third is why this is asked of a **fact** rather than of a spelling: `_Address` at file scope is the
/// implementation's, while `Widget::_count` is the user's own member and a local `_i` is theirs too — and a
/// `DeclFact` says which it is (`local`, and `scope: None` for the global name space).
///
/// # What it is worth, measured
///
/// A completion is a suggestion list, and these are names the user may not use. On one real file, of the **1903**
/// names visible at a blank line inside a function body, **601** were reserved this way (`_Arg`, `_Arg1`,
/// `__crt_…`) — and the same rule takes `_ALLOC_MASK`, `_Alty` and `_Apply_annotation` off `basic_string`'s
/// member list, which is where a user completing `line.` meets them first.
///
/// # What it is *not*
///
/// Not a rule about what the analysis will answer: hover, a jump and a rename still find a reserved name — a reader
/// who points at `_Arg` in a header wants to know what it is — this only decides what to **offer**.
pub(crate) fn is_reserved_to_the_implementation(fact: &DeclFact) -> bool {
    let name = fact.name.as_str();
    let mut characters = name.chars();

    if characters.next() != Some('_') {
        return false;
    }

    match characters.next() {
        // `__name`, and a lone `_`.
        Some('_') | None => true,
        // `_Name`.
        Some(second) if second.is_uppercase() => true,
        // `_name`: the implementation's only in the global name space — and not for a local, whose scope is `None`
        // for the reason `DeclFact::scope` documents (a body contributes no segment to a qualified name).
        Some(_) => fact.scope.is_none() && !fact.local,
    }
}

/// Put the offers in the order a completion shows them: nearest scope first, then by name; and drop every name an
/// inner scope has already offered.
///
/// The hiding is what makes a *list* an answer rather than a pile: a local `count` and a header's `count` are two
/// declarations of one name, and C++ reaches only the first. Both would be wrong to show — one of them is not what
/// the user gets if they pick it — and the inner one is the one they get.
pub(crate) fn sort_and_hide(names: &mut Vec<OfferedName>) {
    // Stable, so that two declarations of one name at one depth keep the order they were found in: that is an
    // overload, and reordering it would be inventing a preference.
    names.sort_by(|one, other| {
        one.depth
            .cmp(&other.depth)
            .then_with(|| one.fact.name.cmp(&other.fact.name))
    });

    let mut seen: HashSet<String> = HashSet::new();
    names.retain(|name| seen.insert(name.fact.name.to_string()));
}
