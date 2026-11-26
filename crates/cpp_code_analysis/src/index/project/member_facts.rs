//! **Member facts and template arguments** — a member's own declaration, the class template arguments deduction
//! gives it, and the bindings that follow.
//!
//! Moved here from `project/mod.rs` without a change.

use super::*;
use crate::BindingKind;
use crate::DeclKind;
use crate::FileView;
use crate::ScopeTree;
use crate::sema;

pub(crate) fn member_fact(
    shapes: &sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    member: &str,
    arguments: Option<&[String]>,
) -> Known<(DeclFact, PathBuf)> {
    member_fact_inner(shapes, index, scopes, root, path, class, member, arguments)
}

pub(crate) fn member_fact_inner(
    shapes: &sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    member: &str,
    // **The arguments the use wrote**, when the caller has them. See [`direct_member`]'s pattern rule: a class
    // template's partial specializations share their primary's scope, and this is what tells them apart. `None`
    // from a caller that does not know them, which answers the way this did before the rule existed.
    arguments: Option<&[String]>,
) -> Known<(DeclFact, PathBuf)> {
    // **The elaborated specifier is not part of the class's name, and it was not coming off here.**
    // `(union GCUnion *)…->cl` — a C cast spells the type that way, and every one of Lua 5.5.0's macros is written
    // that way — asked for `union GCUnion::cl` and answered `NotDeclaredHere`, while `members_of("union GCUnion")`
    // answered its 8 members: the member *list* query normalised the keyword and this one did not. Measured on the
    // expansion of `ci_func(ci)`, whose step `->cl` was the one that stopped the chain (`->p` after it never ran).
    let class = a_type_spelling_with_its_arguments(class).to_string();
    // Followed **before** anything else, so that both the lookup and the spelling this answer reports when it
    // fails name the class rather than the alias: `Widget::size` is the question that could not be answered, and
    // `Alias::size` would be a question nobody asked. `direct_member` follows it again for the bases it is handed,
    // where the step is idempotent — a class resolves to itself.
    let class = &resolve_aliases(shapes, index, scopes, root, path, &class);

    // **The arguments written on the class are what tells a partial specialization from its primary, and the caller
    // that has only a spelling has none to pass.** The nested-name walk is that caller: it arrives with
    // `enable_if<__is_same(int, int), double>` and asks for `type`, which is a member of the *specialization* — and
    // `a_pattern_matches` compares a candidate's pattern against what the use wrote, so with `None` it can only
    // answer "cannot be compared", which keeps every candidate and finds none. Measured: the member name itself came
    // back as the type.
    //
    // The spelling carries them, so they are read off it, and a caller that already knows them (from an object's
    // type) keeps its own — those are the ones paired with the class's parameters by `member_bindings` afterwards.
    let from_the_spelling: Option<Vec<String>> = class.split_once('<').map(|(_, rest)| {
        sema::builtins::split_arguments(rest.trim_end().strip_suffix('>').unwrap_or(rest))
    });
    let arguments = arguments.or(from_the_spelling.as_deref());

    if let Some(found) = direct_member(shapes, index, scopes, root, path, class, member, arguments)
    {
        return Known::Yes(found);
    }

    // **An unqualified class name has to be looked up in a scope, and this is where that was missing.**
    //
    // The spelling a member query arrives with is the one the *use* wrote, and a use inside a namespace writes the
    // class without it: MSVC's `<vector>` says `_Compressed_pair<_Alty, _Scary_val> _Mypair;` and then
    // `_Mypair._Myval2`, because the file is inside `_STD_BEGIN`. The fact is filed under `std::_Compressed_pair` —
    // correctly, from the rendering — and `declarations_in("_Compressed_pair")` therefore finds nothing at all.
    //
    // Measured on `<vector>`: **42 `auto` declarations refused with `NotDeclaredHere("_Compressed_pair::_Myval2")`**
    // — every `auto& _My_data = _Mypair._Myval2;` in the file — while `v.size()` on a `std::vector<int>` worked,
    // because *that* spelling was qualified by the reader who wrote it. The difference looked like an alias problem,
    // then a partial-specialization problem, for several rounds; it is neither.
    //
    // The scope is found the way the base-clause note above finds one for a base name — from the declaration, which
    // is the only thing that knows where it was written. [`ProjectIndex::files_declaring`] searches by **bare name**,
    // so an unqualified spelling reaches a declaration filed under any scope; the first visible one is taken, which
    // is the same choice [`resolve_aliases`] makes and for the same reason.
    //
    // Only for a **bare** name: `std::_Compressed_pair` and `_Compressed_pair` are different questions, and a
    // qualified spelling that found nothing has already said which scope it meant.
    //
    // **And the template arguments come off first**, which is what this step was missing for the second half of the
    // family. The spelling arrives as the type of the *object* — `_Locked_pointer<_Ty>` — and a declaration is never
    // keyed by that: measured on the same name, `_Locked_pointer` resolves to `std::_Locked_pointer` while
    // `_Locked_pointer<_Ty>` answers `NotDeclaredHere`. Ten `auto` declarations in `<memory>` were refused that way,
    // seven of them for `_Locked_pointer::_Lock_and_load`, and the fact had `scope=std::_Locked_pointer` the whole
    // time. The arguments are not lost: [`member_bindings`] pairs them with the class's parameters **after** the
    // class is found.
    if !class.contains("::") {
        let bare = without_arguments(class);
        if let Some(declared_in) = qualified_from_the_index(index, path, bare)
            && let Some(found) = direct_member(
                shapes,
                index,
                scopes,
                root,
                path,
                &declared_in,
                member,
                arguments,
            )
        {
            return Known::Yes(found);
        }
    }

    // Inherited members, **level by level**: a member of a direct base hides a member of that base's own base,
    // which is what C++ does, so the search stops at the first level that has any answer. Two answers at the same
    // level are ambiguous — a diamond where both sides declare the name — and reporting that is the honest
    // outcome: picking one would be a jump to an entity the language says is not uniquely named.
    //
    // As in `members_of`, each base is carried with the class whose base-clause wrote it, because that decides the
    // scope an unqualified base name is looked up in — `struct map : _Tree<…>` in `std` names `std::_Tree`.
    //
    // **And a template parameter is not a base**, for the same reason and with the same measurement as
    // [`members_of`]'s filter: `_Vector_base<_Tp, _Alloc>` contributes `_Tp` and `_Alloc` as "names a class can be
    // looked up by", and asking "who declares `_Tp`" is a whole-index name query. Measured on `LuaSyntaxTree.cpp`,
    // `member_fact(std::vector, size_t)` — a member the class does not have — cost **370–389 ms** per call, eight
    // times, which was 2 280 ms of the file's inference.
    let parameters = class_template_parameters(shapes, index, scopes, root, path, class);
    let mut level: Vec<(String, String)> = bases_of(index, scopes, root, path, class)
        .value()
        .unwrap_or_default()
        .into_iter()
        .filter(|base| !names_a_template_parameter(base, &parameters))
        .map(|base| (base, class.to_string()))
        .collect();
    let mut visited: Vec<String> = vec![class.to_string()];

    while !level.is_empty() {
        let mut found: Vec<(DeclFact, PathBuf)> = Vec::new();
        let mut next: Vec<(String, String)> = Vec::new();

        for (written, owner) in level {
            let base = resolved_in_the_enclosing_scopes(index, scopes, path, &owner, &written);
            if visited.contains(&base) {
                continue;
            }
            visited.push(base.clone());

            // A base that cannot be resolved contributes nothing *and* stops nothing: whatever it might inherit
            // from is unknown, not absent, and the levels below it are still worth asking.
            if let Some(member_found) =
                direct_member(shapes, index, scopes, root, path, &base, member, arguments)
            {
                found.push(member_found);
            }
            next.extend(
                bases_of(index, scopes, root, path, &base)
                    .value()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|base_of_base| !names_a_template_parameter(base_of_base, &parameters))
                    .map(|base_of_base| (base_of_base, base.clone())),
            );
        }

        match found.len() {
            0 => level = next,
            1 => return Known::Yes(found.remove(0)),
            _ => return Known::Unknown(UnknownReason::Ambiguous(Box::from(member))),
        }
    }

    // **A member reached through `operator->`** — `shared_ptr<T>::operator->` returns `T *`, so `config->Generate`
    // is `T::Generate`, and a lookup that stopped at the handle answered "`std::shared_ptr<LuaEditorConfig>` has no
    // member `Generate`" with no completion beside it (reported from an editor, on the ordinary way a smart pointer
    // is used). The rule is the language's: `->` on a class calls the class's `operator->` and looks at what it
    // returns, one level — the same one level [`member_access_class`] reaches through a pointer for.
    //
    // The return type is the *class's* parameter spelling (`_Ty *` for `shared_ptr`, `T *` for a hand-written
    // handle), so it is substituted with the arguments the use wrote before the lookup. A class with no
    // `operator->`, or one whose answer is not a class this analysis can name, answers as it did before.
    if let Some(found) =
        through_an_operator_arrow(index, scopes, root, path, class, member, arguments)
    {
        return Known::Yes(found);
    }

    Known::Unknown(UnknownReason::NotDeclaredHere(Box::from(format!(
        "{class}::{member}"
    ))))
}

/// **A class spelling without its template arguments** — `_Locked_pointer<_Ty>` becomes `_Locked_pointer`.
///
/// A **trailing** `<…>` comes off, which is the shape a use writes: the type of an object is the class with the
/// arguments that finish it, and it is exactly that spelling which is never a declaration's own key. The arguments
/// are still used — [`member_bindings`] pairs them with the class's parameters once the class is found — so
/// dropping them here loses nothing and is the only reason a lookup keyed on the class can succeed at all.
///
/// Only the **last** angle-bracket group is removed, and that is deliberate rather than a parser: `A<B>::C<D>` has
/// two, and cutting at the first `<` would turn the qualification into nonsense. Stripping from the end leaves
/// `A<B>::C`, which is still not a key the index holds — and a lookup that fails is this module's ordinary answer,
/// where a lookup that matched a mangled spelling would be a wrong one.
pub(crate) fn without_arguments(class: &str) -> &str {
    let trimmed = class.trim();
    if !trimmed.ends_with('>') {
        return trimmed;
    }

    // Backwards to the `<` that opened this group, counting nesting so that `Box<Pair<int, int>>` closes at the
    // right place.
    let mut depth = 0usize;
    for (at, character) in trimmed.char_indices().rev() {
        match character {
            '>' => depth += 1,
            '<' => {
                depth -= 1;
                if depth == 0 {
                    let name = trimmed[..at].trim();
                    // `operator<` and a stray `<` with nothing before it: nothing to strip, and the spelling as
                    // written is the honest answer.
                    return if name.is_empty() { trimmed } else { name };
                }
            }
            _ => {}
        }
    }

    trimmed
}

///
/// Two sources for the two-layer split every query in this module makes: a class the file being edited declares is
/// in its scope tree (and its parameters are in its syntax), while a class a header declares is a fact, and the
/// parameters travel with it. A class neither holds answers nothing, which is the ordinary case for an ordinary
/// class.
/// **The pairing that finishes a member's type**: the class's parameters against the use's arguments.
///
/// Its own function because two callers need the same pairing for two different questions — the member's own type
/// (`std::vector<int>::data` is `_Ty*` and therefore `int*`) and what a call of it returns (`_Ty*` again) — and a
/// second copy of "which names go with which arguments" is a second place to get the pairing wrong.
pub(crate) fn member_bindings(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
    arguments: &[Type],
) -> sema::types::TypeBindings {
    if arguments.is_empty() {
        return sema::types::TypeBindings::default();
    }

    let names = class_template_parameters(
        &sema::declarations::ShapeTable::new(),
        index,
        scopes,
        root,
        path,
        class,
    );
    sema::types::TypeBindings::new(names, arguments.to_vec())
}

/// **The arguments a class template deduces from an initializer** — C++17's implicit guide, for the one shape this can be
/// sure of.
///
/// `Box box(7);` where `Box` is `template <typename T> class Box { Box(T); … };` is a `Box<int>`: the standard says
/// the implicit guides are built from the constructors, and a constructor taking exactly the template parameter is the
/// common case. `None` — meaning *answer the name as written* — for two parameters, for a constructor whose parameter
/// names something else, and for a class with no constructor of that shape, because a guessed argument would print an
/// instantiation nobody asked for.
/// **The arguments a class template deduces from an initializer** — C++17's implicit guide, for the one shape this can be
/// sure of.
///
/// ``Box box(7);`` where ``Box`` is ``template <typename T> class Box { Box(T); … };`` is a ``Box<int>``: the standard
/// says the implicit guides are built from the constructors, and a constructor taking exactly the template parameter is
/// the common case. ``None`` — meaning *answer the name as written* — for two parameters, for a constructor whose
/// parameter names something else, and for a class with no constructor of that shape, because a guessed argument would
/// print an instantiation nobody asked for.
///
/// # Where the constructor is read, and why from two places
///
/// A constructor is **not a binding** of its class's scope: a scope is keyed by name and `Box::Box` is not a name any
/// lookup asks for. So it is read from the class's own body —
///
/// ```text
/// the tree    the class is declared in the file being edited        `class Box { explicit Box(T); }`
/// the index   it is declared in a header this file includes         the header's facts, scope `ns::Box`, name `Box`
/// ```
///
/// — and the second case is not a refinement: measured on a `.cpp` that includes a header holding the template, the
/// rendering inlined the *facts* (members answered 2 of 2) and not the *body*, so the tree held no `ClassDef` at all and
/// the deduction answered the bare template name.
pub(crate) fn deduced_class_template_arguments(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    declaring: &mut dyn FnMut(&Path) -> Option<FileView>,
    class: &str,
    initializer: &sema::types::Type,
) -> Option<String> {
    let parameters = class_template_parameters(
        &sema::declarations::ShapeTable::new(),
        index,
        scopes,
        root,
        path,
        class,
    );
    let [only] = parameters.as_slice() else {
        return None;
    };
    let simple = class.rsplit("::").next().unwrap_or(class);

    // **A hand-written deduction guide** — the language's own spelling of the rule, and the only source for a class
    // whose constructor does not take the parameter: `template <typename T> Wrap(T) -> Wrap<T>;`. Asked first, because
    // a guide that exists *is* the rule the language applies.
    if let Some(deduced) = the_deduction_guide_deduces(root, class, simple, only, initializer) {
        return Some(deduced);
    }
    if the_constructor_takes(root, class, simple, only) {
        return Some(format!("{class}<{initializer}>"));
    }
    // **The header that declares it.** A `.cpp` whose rendering did not inline the template's *body* has no `ClassDef`
    // to read, and the index keeps no fact for a constructor — so the file that declares the class is parsed and the same
    // scan runs over its tree. `declaring` is the request layer's file provider, the one the type queries already use.
    for declared in index.files_declaring(simple, path) {
        if declared.file == path {
            continue;
        }
        let Some(view) = declaring(&declared.file) else {
            continue;
        };
        if let Some(deduced) =
            the_deduction_guide_deduces(&view.root, class, simple, only, initializer)
        {
            return Some(deduced);
        }
        if the_constructor_takes(&view.root, class, simple, only) {
            return Some(format!("{class}<{initializer}>"));
        }
    }
    None
}

/// Does `class` declare a constructor whose first parameter **is** the template parameter `only`?
///
/// The two sources are tried in the order the answer is cheapest: the tree this file's reading already holds, then the
/// index's facts for the header that declares it.
pub(crate) fn the_constructor_takes(
    root: &cpp_parser::CppSyntaxNode,
    _class: &str,
    simple: &str,
    only: &str,
) -> bool {
    // **In this file's own tree.** The class is found by its own name rather than through a scope: a class scope holds
    // its members, and the class itself is bound in the scope that encloses it, so a walk from the scope is the long way
    // round.
    let in_the_tree = root.descendants().find(|node| {
        matches!(
            cpp_parser::CppSyntaxKind::from(node.kind()),
            cpp_parser::CppSyntaxKind::ClassDef
                | cpp_parser::CppSyntaxKind::StructDef
                | cpp_parser::CppSyntaxKind::UnionDef
        ) && node.children().any(|child| {
            cpp_parser::CppSyntaxKind::from(child.kind()) == cpp_parser::CppSyntaxKind::NameExpr
                && child.text().to_string().trim() == simple
        })
    });
    if let Some(node) = in_the_tree {
        for declaration in node.descendants() {
            if cpp_parser::CppSyntaxKind::from(declaration.kind())
                != cpp_parser::CppSyntaxKind::Declaration
            {
                continue;
            }
            // **A constructor, by its own text.** `explicit Box(T value) : value_(value) {}` is a declaration whose
            // specifier sequence may hold `explicit` alone with the name in the declarator, so reading only the
            // specifiers found the constructor in a class without a member-initializer list and not in one with it.
            // The shape matched is `[explicit] Name (`, which nothing else in a class body is: a member function starts
            // with its return type.
            let text = declaration.text().to_string();
            let head = text
                .trim_start()
                .trim_start_matches("explicit")
                .trim_start();
            if !(head.starts_with(simple) && head[simple.len()..].trim_start().starts_with('(')) {
                continue;
            }
            let Some(parameters) = declaration.descendants().find(|child| {
                cpp_parser::CppSyntaxKind::from(child.kind())
                    == cpp_parser::CppSyntaxKind::ParameterList
            }) else {
                continue;
            };
            if the_first_parameter_is(&parameters.text().to_string(), only) {
                return true;
            }
        }
        return false;
    }
    false
}

/// **What a hand-written deduction guide says**, for the identity form: `Name(T) -> Name<T>`.
///
/// ```cpp
/// template <typename T>
/// Wrap(T) -> Wrap<T>;      //  Wrap wrapped(7);   is a Wrap<int>
/// ```
///
/// The guide is a declaration whose text holds `->`: the head is `[template <…>] Name(params)` and the tail is the type
/// it deduces to. Only the identity form is read — the return type is exactly `Name<T>` for the class's single template
/// parameter — because anything else (`Name<T*>`, two parameters, a pack) is a pattern match this layer does not do, and a
/// wrong argument would print an instantiation nobody asked for.
pub(crate) fn the_deduction_guide_deduces(
    root: &cpp_parser::CppSyntaxNode,
    class: &str,
    simple: &str,
    only: &str,
    initializer: &sema::types::Type,
) -> Option<String> {
    let deduces_to = format!("{simple}<{only}>");
    for declaration in root.descendants() {
        if cpp_parser::CppSyntaxKind::from(declaration.kind())
            != cpp_parser::CppSyntaxKind::Declaration
        {
            continue;
        }
        let text = declaration.text().to_string();
        let Some((head, tail)) = text.split_once("->") else {
            continue;
        };
        let returns = tail.trim().trim_end_matches(';').trim();
        if returns != deduces_to {
            continue;
        }
        let head = head.trim();
        let Some(open) = head.rfind('(') else {
            continue;
        };
        // **The guide's name**, which is the last token before its parameter list: `template <typename T>` then `Wrap`.
        let name = head[..open]
            .rsplit(|character: char| character.is_whitespace() || character == '>')
            .find(|word| !word.is_empty())
            .unwrap_or_default();
        if name != simple {
            continue;
        }
        if the_first_parameter_is(&head[open..], only) {
            return Some(format!("{class}<{initializer}>"));
        }
    }
    None
}

/// Is the first parameter of `list` — `(T value)`, `(const T& value)` — spelled with the template parameter?
pub(crate) fn the_first_parameter_is(list: &str, only: &str) -> bool {
    list.trim_start_matches('(')
        .split(',')
        .next()
        .unwrap_or_default()
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .any(|word| word == only)
}

pub(crate) fn class_template_parameters(
    shapes: &sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Vec<String> {
    if let Some(scope) = scopes.scope_with_qualified_name(class)
        && let Some(binding) = scopes.scope(scope).and_then(|scope| {
            scope
                .bindings
                .iter()
                .find(|binding| binding.kind == BindingKind::Class)
        })
    {
        let found = sema::declarations::declared_template_parameters_with(
            sema::declarations::shapes_from(shapes, root),
            binding,
        );
        if !found.is_empty() {
            return found;
        }
    }

    index.template_parameters_of(class, path)
}

/// How many aliases deep a type spelling is followed before the walk gives up.
///
/// `using A = A;` and `using A = B; using B = A;` are both writable, and a walk that trusted the spelling would
/// follow them for ever. Eight is far past any real chain of aliases — the standard library's deepest are two or
/// three (`string` → `basic_string`, `size_type` → `size_t` → …) — and the answer when it is reached is the
/// spelling that was last resolved, which the caller then reports as not declared rather than as a wrong class.
pub(crate) const MAX_ALIAS_DEPTH: usize = 8;

/// The class a type spelling names, following `typedef`/`using` **aliases**.
///
/// `std::string` is `basic_string<char>`; nobody declares `substr` *in* `string`, because an alias has no members
/// of its own. So a spelling that resolves to an alias is replaced by the type the alias points at, and the
/// lookup carries on with that — which is one step, not a type system: no instantiation, no substitution, no
/// inference of what the target's template arguments mean.
///
/// # The target is resolved in the alias's own scope
///
/// ```cpp
/// namespace std { typedef basic_string<char> string; }   // the target is written *relative to* `std`
/// ```
///
/// Following it as a bare `basic_string` would look in the wrong place. The candidates are therefore
/// `<the alias's scope>::<target>` first and the bare target second — which is C++'s own enclosing-scope lookup,
/// done one step of it: the standard library's aliases are nearly all spelled this way, so the first candidate is
/// the one that answers, and a target that names something global still resolves through the second.
///
/// # Bounded, and honest when it stops
///
/// A spelling that cannot be resolved to a class comes back **unchanged**, so the caller reports
/// `NotDeclaredHere(<what the file wrote>)` rather than a name nobody wrote. See [`MAX_ALIAS_DEPTH`] for the bound
/// and why a bound is needed at all.
pub(crate) fn resolve_aliases(
    shapes: &sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> String {
    resolve_aliases_with_arguments(shapes, index, scopes, root, path, class).0
}

/// **The class a spelling names, following aliases — and the template arguments the alias carried.**
///
/// `std::string` is `basic_string<char, char_traits<char>, allocator<char>>`. Following the alias gives the class
/// `std::basic_string`, and the **arguments are the other half of the answer**: they are what makes `_Elem` mean
/// `char`, so a member whose type is written `_Elem&` is `char&` for a `std::string` and `wchar_t&` for a
/// `std::wstring`. Both come back from here because they come from the same step, and a caller that had to ask
/// twice would have two chances to disagree about which step it was.
///
/// # The arguments of the *last* step, and why that is right
///
/// Each alias step replaces the spelling with its target, so an earlier step's arguments have already been written
/// into the target the next step sees: `using A = B<int>; using B = vector<T>;` — no, more simply, the standard
/// library's shape is one step (`string` → `basic_string<char, …>`), and for two steps the second target's own
/// arguments are the ones that describe the class finally named. Arguments found on an earlier step are therefore
/// kept only if the later one carries none, which is the case where the later target is a bare name
/// (`using B = basic_string;`).
///
/// # What it does not do
///
/// No substitution: `MyVec<T>` → `std::vector<T>` returns the *argument as written* (`T`), and pairing it with
/// `_Ty` is the caller's step, because the caller is the one that knows what `T` stands for. See
/// [`TypeBindings::applied_to`].
pub(crate) fn resolve_aliases_with_arguments(
    shapes: &sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> (String, Vec<Type>) {
    let (name, carried, _unresolved) =
        resolve_aliases_or_unresolved(shapes, index, scopes, root, path, class);
    (name, carried)
}

/// [`resolve_aliases_with_arguments`], and **the class it could not find** when the chain ends at a spelling the
/// index does not declare.
///
/// That third answer is what tells a member query the difference between "this class has no members" and "this
/// spelling names a class I cannot see", and the two must not be answered the same way. Measured on
/// `CodeFormatCLib/src/CodeFormat.cpp` (libstdc++): `std::string` is `typedef basic_string<char> string;` inside
/// `std::__cxx11`, so the walk resolves the name `basic_string` from `std` — where nothing declares it — and used to
/// settle on `std::basic_string` anyway. `direct_members` then found no facts under that spelling, answered an
/// **empty, complete** list, and `undefined-field` reported `result.size()`, `formattedText.begin()` and eight more
/// members of `std::string` as undefined. Ten false positives on one file, all from this fallback.
pub(crate) fn resolve_aliases_or_unresolved(
    shapes: &sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> (String, Vec<Type>, Option<String>) {
    let mut current = class.to_string();
    let mut carried: Vec<Type> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    for _ in 0..MAX_ALIAS_DEPTH {
        if seen.contains(&current) {
            break;
        }
        seen.push(current.clone());

        // **The whole spelling is not an alias, but its prefix may be one.**
        //
        // `vector:2920` writes `using _Alvbase_traits = typename _Mybase::_Alvbase_traits;`, and `_Mybase` is
        // **another alias of `vector`** — so the target is a qualified name whose *first* segment is itself a
        // name to follow, not a class to stop at. Asking `alias_target_of` for the whole spelling answers `None`
        // and the walk used to end there, one alias short of `allocator_traits`: measured,
        // `members_of("std::vector::_Alvbase_traits")` reported
        // `NotDeclaredHere("std::vector::_Mybase::_Alvbase_traits")`.
        //
        // The prefix is what gets resolved, and the rest is carried along — one segment at a time, longest
        // prefix first, so a name nested two aliases deep is followed the same way. `seen` bounds it, so an
        // alias that leads back to itself stops here rather than looping.
        let followed = alias_target_of(shapes, index, scopes, root, path, &current).or_else(|| {
            let segments: Vec<&str> = current.split("::").collect();
            (1..segments.len()).rev().find_map(|take| {
                let prefix = segments[..take].join("::");
                let (target, scope) = alias_target_of(shapes, index, scopes, root, path, &prefix)?;

                // **The arguments come off and the scope does not go on.** Both are the rules the candidate
                // list below already applies, and applying them here as well compounded them: the first
                // attempt produced `std::vector::std::vector::_Vb_val<_Alloc>::_Alvbase_traits`, with the
                // enclosing scope twice over and a `_Vb_val<_Alloc>` no fact is named by.
                let target =
                    a_class_name_without_spaces(parse_type_spelling(&target).class_name()?);

                Some((format!("{target}::{}", segments[take..].join("::")), scope))
            })
        });

        let Some((target, scope)) = followed else {
            break;
        };

        // **Read as a type, not as a name**: the target's own arguments are what the class it names was written
        // with, and `base_type_name` — which the name-only walk used — is exactly the part that threw them away.
        let target_type = parse_type_spelling(&target);
        // **A `typedef` of a pointer names the class it points at.** Lua 5.5.0 declares
        // `typedef struct StackValue *StkId;` and writes its whole stack API with one, so `StkId` was read as "no
        // class", the walk broke, and every member question about it asked the *alias* for its members —
        // `NotDeclaredHere("StkId::val")`, and no `->` chain that starts at one could be followed (measured through
        // the expansion of `ci_func(ci)`, whose first step is `(ci)->func.p`, typed `StkId`). A member of a pointer is
        // a member of its pointee's class, which is the rule `member_access_class` already applies.
        let (named, arguments) = match target_type.class_name() {
            Some(named) => (Some(named.to_string()), target_type.arguments().to_vec()),
            None => match target_type.pointee() {
                Some(pointee) => (
                    pointee.class_name().map(str::to_string),
                    pointee.arguments().to_vec(),
                ),
                None => (None, Vec::new()),
            },
        };
        let Some(target) = named.map(|named| a_class_name_without_spaces(&named)) else {
            break;
        };
        if target.is_empty() {
            break;
        }
        if !arguments.is_empty() {
            carried = arguments;
        }

        // An unqualified target is looked for **from the scope the alias was declared in, outward** — which is
        // C++'s own rule for an unqualified name, and it is not one step of it.
        //
        // Measured on MSVC's `<string>`: `basic_string` declares `using _Alty_traits = allocator_traits<_Alty>;`,
        // and `allocator_traits` is a **free template at namespace scope in `std`** (`xmemory:754`). With only
        // the alias's own scope tried, the candidate is `std::basic_string::allocator_traits` — a spelling
        // nothing declares — and the walk gives up one scope short of the answer. `members_of` reported exactly
        // that: `NotDeclaredHere("std::basic_string::allocator_traits")`.
        //
        // **A qualified target's first segment takes the same walk**, and that is not the same as taking the
        // target as it stands. `vector` declares two of them:
        //
        // ```cpp
        // using _Alvbase_traits = allocator_traits<_Alvbase>;            // vector:2828, unqualified target
        // using _Alvbase_traits = typename _Mybase::_Alvbase_traits;     // vector:2920, qualified target
        // ```
        //
        // and `_Mybase` is **another alias of `vector`**, so `_Mybase::_Alvbase_traits` is not a spelling
        // anything declares — it is a name whose *first* segment has to be found the same way. Taken as it
        // stands it stopped the walk dead: `members_of("std::vector::_Alvbase_traits")` answered
        // `NotDeclaredHere("_Mybase::_Alvbase_traits")`, one alias short of `allocator_traits`.
        //
        // The bare name is kept last, for a target that names something at global scope. A target that begins
        // with `::` is absolute and is taken as it stands — that is what the leading `::` is for.
        let candidates: Vec<String> = match &scope {
            Some(prefix) if !target.starts_with("::") => {
                let prefix = a_class_name_without_spaces(prefix);
                let prefix = prefix.as_str();
                let (first, rest) = match target.split_once("::") {
                    Some((first, rest)) => (first, Some(rest)),
                    None => (target.as_str(), None),
                };
                let segments: Vec<&str> = prefix.split("::").collect();
                let mut candidates: Vec<String> = (1..=segments.len())
                    .rev()
                    .map(|take| match rest {
                        Some(rest) => format!("{}::{first}::{rest}", segments[..take].join("::")),
                        None => format!("{}::{first}", segments[..take].join("::")),
                    })
                    .collect();
                candidates.push(target.clone());
                candidates
            }
            _ => vec![target.clone()],
        };

        // **A target nothing declares is not an answer.** The candidates are the target qualified by the scopes the
        // alias was written in, outermost last; when every one of them is undeclared the chain has reached a name
        // this index cannot see (a namespace opened by a macro the index did not expand is the ordinary case) and
        // the spelling it would have returned describes nothing. Measured: `std::basic_string` for libstdc++'s
        // `std::string`, and ten false `undefined-field` findings on one file. The caller is told instead.
        // **A leading `::` says "the global scope", and the index does not key on it.** The rendering writes a macro's
        // expansion absolutely — `::std::basic_string` — so the accepted candidate is stripped before it becomes the
        // spelling every later lookup uses: measured, `members_of` answered 0 members with
        // `NotDeclaredHere("::std::basic_string")` while the very same class is filed as `std::basic_string`.
        // **A name the standard library brings into a namespace with a using-declaration, or one an inline namespace
        // holds, is one namespace in.** libstdc++'s ABI-1 `std::string` is `typedef basic_string<char> string;` in
        // `std`, and the class it names is `std::__cxx11::basic_string` — the `using __cxx11::basic_string;` that makes
        // `std::basic_string` a name is a *using-declaration*, which the index does not record as a declaration. The
        // candidate list above is exact spellings, so it finds neither, and the walk used to give up one namespace
        // short: measured on `import std;`, `members_of("std::string")` answered **0 members** with
        // `NotDeclaredHere("std::basic_string")` while `std::__cxx11::basic_string` was in the index all along.
        //
        // The search is C++'s own rule for inline namespaces (the name is visible in the enclosing namespace), aimed by
        // the candidate the alias's target was written in, and it prefers the **fewest extra segments** — the nearest
        // enclosing scope, which is the one lookup would have reached first.
        let nested = |candidate: &str| {
            let (prefix, name) = candidate.rsplit_once("::")?;
            let mut best: Option<(usize, String)> = None;
            for found in index.files_declaring(name, path) {
                if found.fact.kind != DeclKind::Type || found.fact.type_of.is_some() {
                    continue;
                }
                let Some(scope) = found.fact.scope.as_deref() else {
                    continue;
                };
                let Some(rest) = scope.strip_prefix(prefix)?.strip_prefix("::") else {
                    continue;
                };
                let depth = rest.split("::").count();
                if depth == 1 && best.as_ref().is_none_or(|(least, _)| depth < *least) {
                    best = Some((depth, format!("{scope}::{name}")));
                }
            }
            best.map(|(_, spelling)| spelling)
        };
        let found = candidates
            .iter()
            .find(|candidate| is_declared(index, scopes, path, candidate))
            .map(|candidate| candidate.trim_start_matches("::").to_string())
            .or_else(|| candidates.iter().find_map(|candidate| nested(candidate)));
        let Some(next) = found else {
            // **The target is still the answer, and the third slot says it was not found.** A caller that only wants
            // a spelling keeps the one the alias named — `animate_aliases_whose_target_is_not_indexed_reports_the_target`
            // pins that: the missing declaration is `Widget::size`, not `Alias::size` — while a caller that has to
            // decide whether "no members" means an empty class or an unreadable one reads the third slot.
            let named = candidates[0].trim_start_matches("::").to_string();
            return (named.clone(), carried, Some(named));
        };

        if next == current {
            break;
        }
        current = next;
    }

    // The arguments of the **first** step are the ones that describe the type as written when nothing later said
    // otherwise: `std::string` has none of its own, and the step that resolved it supplied them.
    (current, carried, None)
}

/// **A class name with the whitespace taken out** — the rendering writes a macro's expansion spaced, so a target read
/// out of it is spelled `:: std :: basic_string` rather than `::std::basic_string`.
///
/// Not a cosmetic step: every downstream lookup keys on the spelling. Measured on `std::string`'s alias:
/// `resolve_aliases_or_unresolved` produced `:: std :: basic_string`, `is_declared` answered false for it (nothing is
/// declared under that spelling), the chain gave up and `members_of` reported **0 members** — so `enenn.size()`, whose
/// object's type had just been deduced, offered no completion at all.
pub(crate) fn a_class_name_without_spaces(name: &str) -> String {
    name.chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}
/// What an alias points at, and the scope it was declared in, or `None` when the spelling is not an alias.
///
/// # How an alias is recognised
///
/// In the buffer, by the binding's kind, which is exact. Through the index, by the rule [`DeclFact::type_of`]
/// documents: a **type** fact with a `type_of` is an alias, and a class is a type fact without one. That is the
/// whole of the test, and it is one test rather than a new field on purpose — the field was already there for
/// variables, and what an alias *has* is a type in the same sense: the one it points at.
/// **Every balanced `<…>` group removed** — `outer<int>::handle_type` becomes `outer::handle_type`.
///
/// A type spelling, so the only `<` that can appear is a template argument list's. Nesting is counted forwards
/// because `Box<Pair<int, int>>` has to close at the right `>`.
pub(crate) fn without_template_arguments(written: &str) -> String {
    let mut stripped = String::with_capacity(written.len());
    let mut depth = 0usize;
    for character in written.chars() {
        match character {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => stripped.push(character),
            _ => {}
        }
    }
    stripped
}

pub(crate) fn alias_target_of(
    shapes: &sema::declarations::ShapeTable,
    index: &ProjectIndex,
    scopes: &ScopeTree,
    root: &cpp_parser::CppSyntaxNode,
    path: &Path,
    class: &str,
) -> Option<(String, Option<String>)> {
    // **A qualifier's arguments are not the alias's arguments, and they have to come off first.**
    //
    // The split below separates an alias's **own** template argument list (`enable_if_t<int, double>`) and takes the
    // first `<` it finds. In `outer<int>::handle_type` the first `<` opens the *qualifier's* list, so the head became
    // `outer` — the class itself, which is not an alias — and the walk never found the member typedef. Measured:
    // `outer<int>::handle_type h; h.` offered `outer`'s own members while `outer::handle_type h;` (the same class
    // without the argument) offered the target's. Stripping runs only when the `<` is left of the last `::`, which is
    // exactly the shape of a qualified name whose qualifier is a template-id.
    //
    // **And "left of the last `::`" is not that shape.** `ContainerOf<std::vector, int>` — an alias whose own
    // argument list *contains* a `::` — matched it, so the arguments were stripped before they were read: the head
    // became `ContainerOf`, `written_arguments` came back empty, the target `Container<T, std::allocator<T>>` was
    // substituted with nothing, and the walk settled on the class `Container`. Measured on `test_cpp`'s
    // `templates.cpp`: `members_of("ContainerOf<std::vector, int>")` answered *"0 member(s), 1 unlisted base
    // `Container`"* — the alias's target read as a base clause.
    //
    // What tells the two apart is where the list **closes**: a qualifier's closes before the `::` that continues the
    // name, while an alias's own is the end of the name (or what the rest of the spelling continues from).
    let qualifier_ends_before_the_last_scope = match class.find('<') {
        Some(open) => the_close_of(class, open).is_some_and(|close| class[close..].contains("::")),
        None => false,
    };
    let stripped_qualifier = without_template_arguments(class);
    let class = if qualifier_ends_before_the_last_scope {
        stripped_qualifier.as_str()
    } else {
        class
    };
    // **The arguments come off before the name is looked up, and go back on afterwards.**
    //
    // `enable_if_t<int, double>` is an alias *template*, and the index files it under the bare name `enable_if_t` —
    // so a lookup made with the whole spelling found nothing and the alias was never followed: measured, that
    // spelling came back as its own answer. The head is what the index holds, and the written arguments are what the
    // alias's own parameter names have to be replaced by in its target.
    let (head, written_arguments) = match class.split_once('<') {
        Some((head, rest)) => {
            let inside = rest.trim_end().strip_suffix('>').unwrap_or(rest);
            (
                head.trim().to_string(),
                sema::builtins::split_arguments(inside),
            )
        }
        None => (class.to_string(), Vec::new()),
    };
    let class = head.as_str();

    // The buffer first: the alias may be in a file that has never been written to disk.
    let (scope_name, short) = match class.rsplit_once("::") {
        Some((prefix, short)) => (prefix.to_string(), short.to_string()),
        None => (String::new(), class.to_string()),
    };

    if let Some(scope) = scopes.scope_with_qualified_name(&scope_name)
        && let Some(data) = scopes.scope(scope)
        && let Some(binding) = data
            .bindings
            .iter()
            .find(|binding| binding.name.identifier_text() == Some(short.as_str()))
        && let Some(target) = sema::declarations::declared_alias_target_with(
            sema::declarations::shapes_from(shapes, root),
            binding,
        )
    {
        // **The alias's own parameters replaced by what the use wrote.** A bare alias has no parameters and this
        // changes nothing; a template alias is the case the split above exists for.
        let target = sema::types::substituted_spelling(
            &target,
            &sema::declarations::declared_template_parameters_with(
                sema::declarations::shapes_from(shapes, root),
                binding,
            ),
            &written_arguments,
        );
        return Some((
            target,
            (!scope_name.is_empty()).then_some(scope_name.clone()),
        ));
    }

    // **Every declaration of the name, and the target comes from whichever one is the alias.**
    //
    // Asking the singular `definition` here refused the alias outright whenever the name had two declarations —
    // `Unknown(Ambiguous)` is one of the arms below — and `_Mybase` in `std::_Vb_iterator` is exactly that: one
    // alias, matched by its **bare** name in more than one class, because that is how the index narrows a
    // lookup. The walk then returned the spelling unchanged, `direct_members` found no members under a name
    // nothing declares, and `members_of("std::_Vb_iterator::_Mybase")` answered **zero members** for a class
    // whose base has twenty-two. The same shape as `bases_of`: a name declared more than once is not a name
    // that cannot be read, it is a name that has to be asked for in the plural.
    match index.definitions(class, path) {
        Known::Yes(found) => found.found.iter().find_map(|declaration| {
            (declaration.fact.kind == DeclKind::Type)
                .then(|| declaration.fact.type_of.clone())
                .flatten()
                .map(|target| target.to_string())
                .map(|target| {
                    (
                        sema::types::substituted_spelling(
                            &target,
                            declaration.fact.parameters(),
                            &written_arguments,
                        ),
                        declaration
                            .fact
                            .scope
                            .clone()
                            .map(|scope| scope.to_string()),
                    )
                })
        }),
        Known::Unknown(_) | Known::No => None,
    }
}

/// **The `>` that closes the `<` at `open`**, counting nested lists — `None` for a list that never closes.
///
/// Used to tell a *qualifier*'s argument list from an alias's own: `outer<int>::member` closes before the `::` that
/// continues the name, `ContainerOf<std::vector, int>` closes at the end of it.
fn the_close_of(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (at, character) in text.char_indices().skip_while(|(at, _)| *at < open) {
        match character {
            '<' => depth += 1,
            '>' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}
/// Is this spelling a declaration this analysis can see?
///
/// Used to choose between the two ways of reading an alias's target — relative to the alias's scope, or as a
/// spelling in its own right — and deliberately a *cheap* question: a scope in the buffer or one fact in the
/// index is answered without walking anything.
pub(crate) fn is_declared(
    index: &ProjectIndex,
    scopes: &ScopeTree,
    path: &Path,
    spelling: &str,
) -> bool {
    if scopes.scope_with_qualified_name(spelling).is_some() {
        return true;
    }

    // **`Ambiguous` is declared.** A class template with a forward declaration and a definition answers with
    // more than one declaration, and [`ProjectIndex::definition`] turns that into
    // `Unknown(Ambiguous(…))` — which `matches!(…, Known::Yes(_))` reads as "not here". The question this
    // function asks is *is the name declared*, not *is there exactly one declaration of it*, and the two
    // differ for most of the standard library.
    //
    // Measured: `alias_target_of` resolves `_Alty_traits` to `allocator_traits<_Alty>` declared in
    // `std::basic_string`, and the candidate that should answer is `std::allocator_traits` — a name the index
    // holds **twice** (`xmemory:554` forward-declares it, `755` defines it). Rejected here, the walk fell back
    // to `std::basic_string::allocator_traits`, a spelling nothing declares, and `members_of` reported
    // `NotDeclaredHere("std::basic_string::allocator_traits")` one scope short of the answer.
    matches!(
        index.definition(spelling, path),
        Known::Yes(_) | Known::Unknown(UnknownReason::Ambiguous(_))
    )
}

/// The declaration of `member` written **directly** in `class`, or `None`.
///
/// The qualified name `<class>::<member>`, which is why nothing here needs to know what a class *is*: the file
/// being edited is asked first through its scope tree, and the index second — the two-layer split
/// [`definition_across_files`] makes, for the same reason.
/// **A spelling with its namespace-alias prefixes followed** — `std::views::_Filter` → `std::ranges::views::_Filter`.
///
/// libstdc++ writes `namespace views = ranges::views;`, so every name inside it can be written through the alias.
/// `members_of` was taught to follow the alias for the **namespace itself**; a type, a member or a base spelled one level
/// deeper (`std::views::_Filter`, `std::views::filter`) needs the same substitution in its prefix.
///
/// Longest prefix first, so `a::b::c` asks about `a::b` before `a` — a namespace declared inside another one is the case
/// that needs it. A spelling with no alias in it comes back unchanged, which is what keeps this additive.
pub(crate) fn through_a_namespace_alias(
    index: &ProjectIndex,
    path: &Path,
    spelling: &str,
) -> String {
    const DEPTH: usize = 4;
    let mut rewritten = spelling.to_string();
    for _ in 0..DEPTH {
        let segments: Vec<&str> = rewritten.split("::").collect();
        if segments.len() < 2 {
            return rewritten;
        }
        let mut changed = false;
        for take in (1..segments.len()).rev() {
            let prefix = segments[..take].join("::");
            let Some(found) = index.definition(&prefix, path).value() else {
                continue;
            };
            if found.fact.kind != DeclKind::Namespace {
                continue;
            }
            let Some(target) = found.fact.type_of.as_deref() else {
                continue;
            };
            if target == prefix {
                continue;
            }
            let qualified = if target.contains("::") {
                target.to_string()
            } else {
                match &found.fact.scope {
                    Some(scope) => format!("{scope}::{target}"),
                    None => target.to_string(),
                }
            };
            if qualified == prefix {
                continue;
            }
            rewritten = format!("{qualified}::{}", segments[take..].join("::"));
            changed = true;
            break;
        }
        if !changed {
            return rewritten;
        }
    }
    rewritten
}

#[cfg(test)]
mod tests {
    use super::the_close_of;

    /// **The `>` that closes a `<`** — the question that tells a qualifier's argument list from an alias's own.
    ///
    /// `outer<int>::handle_type` closes before the `::`, so the list belongs to the qualifier and is stripped;
    /// `ContainerOf<std::vector, int>` closes at the end, so it is the alias's own list and must be **kept** — the
    /// walk used "is there a `::` after the `<`" as its test, which is true in both and stripped the second one's
    /// arguments before they were read.
    #[test]
    fn the_close_of_an_argument_list_is_its_matching_angle() {
        assert_eq!(the_close_of("outer<int>::handle_type", 5), Some(9));
        assert_eq!(the_close_of("ContainerOf<std::vector, int>", 11), Some(28));
        assert_eq!(
            the_close_of("A<B<C>>::D", 1),
            Some(6),
            "a nested list closes at its own matching angle, not its first `>`"
        );
        assert_eq!(the_close_of("Unclosed<int", 8), None);
    }
}
