//! **Calls and layout** — the declaration a call means, its deduced arguments, and the size of a class.
//!
//! Moved here from `session/mod.rs` without a change.

use super::*;

impl<F: FileProvider + Clone> Session<F> {
    /// **The declarations a call at this offset can reach** — indices into `parameter_lists`.
    ///
    /// The same resolution the argument checks and the signature popup use, asked over the declarations a caller already
    /// holds: a hover has the facts of the name under the cursor and needs to know which of them the call can mean.
    ///
    /// ```text
    ///   one index     the resolution decided (`Choice::One`)
    ///   several       every one of them can accept the arguments, and nothing decides between them by type
    ///   None          the offset is not on a call, or no declaration can accept it
    /// ```
    ///
    /// The **several** case is the one a hover needs: printing the chosen declaration beside five others that cannot take
    /// this argument list answers a question the reader did not ask.
    pub fn the_declarations_a_call_means(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        parameter_lists: &[Option<String>],
        template_parameters: &[Vec<String>],
    ) -> Option<Vec<usize>> {
        let (types, candidates) =
            self.the_candidates_of_a_call(view, offset, parameter_lists, template_parameters)?;
        match crate::sema::overload::choose(&types, &candidates) {
            crate::sema::overload::Choice::One(chosen) => Some(vec![chosen]),
            // **Nothing decides between them**, so the viable set is the answer: every one of them can accept this call,
            // and the ones arity alone rules out are gone.
            crate::sema::overload::Choice::Ambiguous => Some(
                crate::sema::overload::viable_candidates(&types, &candidates),
            ),
            crate::sema::overload::Choice::None => None,
        }
    }

    /// **The one declaration a call at this offset means**, when the resolution decides — see
    /// [`Session::the_declarations_a_call_means`] for the answer that also covers the ambiguous case.
    pub fn the_declaration_a_call_means(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        parameter_lists: &[Option<String>],
        template_parameters: &[Vec<String>],
    ) -> Option<usize> {
        let (types, candidates) =
            self.the_candidates_of_a_call(view, offset, parameter_lists, template_parameters)?;
        match crate::sema::overload::choose(&types, &candidates) {
            crate::sema::overload::Choice::One(chosen) => Some(chosen),
            _ => None,
        }
    }

    /// **The declarations and the argument types of the call at `offset`** — the one walk the two questions above share.
    pub(crate) fn the_candidates_of_a_call(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        parameter_lists: &[Option<String>],
        template_parameters: &[Vec<String>],
    ) -> Option<(
        Vec<crate::Known<String>>,
        Vec<crate::sema::overload::Candidate>,
    )> {
        let token = cpp_parser::token_at(&view.root, offset.get())?;
        let call = token.parent_ancestors().find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::CallExpr
        })?;
        let arguments = crate::sema::call_syntax::arguments_of(&call)?;

        // The engine answers with a parsed `Type` and the file it came from; the resolver ranks spellings, which is the
        // same information the facts carry.
        let session: &Session<F> = self;
        let index: &crate::ProjectIndex = session.store.index();
        let resolver =
            Box::new(move |path: &Path| -> Option<FileView> { session.view_of_the_file(path) });
        let model = crate::sema::model::SemanticModel::new(
            index,
            &view.root,
            &view.scopes,
            &view.source,
            &view.path,
            resolver,
        );
        let types: Vec<crate::Known<String>> = arguments
            .iter()
            .map(|argument| match model.type_of(argument) {
                crate::Known::Yes((a_type, _)) => crate::Known::Yes(a_type.to_string()),
                crate::Known::No => crate::Known::No,
                crate::Known::Unknown(reason) => crate::Known::Unknown(reason),
            })
            .collect();

        let candidates: Vec<crate::sema::overload::Candidate> = parameter_lists
            .iter()
            .enumerate()
            .map(|(at, parameters)| {
                let (declared, variadic) = parameters
                    .as_deref()
                    .map(crate::sema::overload::parameters_in)
                    .unwrap_or_default();
                // **A template candidate is ranked as what the call instantiates**, when the caller
                // could read its header: `take(T*)` and `take(T)` say nothing about an `int*`, and
                // deduced they say `int*` and `int`.
                let names = template_parameters.get(at).cloned().unwrap_or_default();
                // Kept for the partial ordering, which compares what the *declarations* wrote, not what the call
                // substituted — and the substitution below consumes `declared`.
                let declared_for_ordering = declared.clone();
                let (types, is_a_template) =
                    match crate::sema::deduction::deduce(&names, &declared, &types) {
                        Some(arguments) => (
                            declared
                                .iter()
                                .map(|spelling| {
                                    crate::sema::types::substituted_spelling(
                                        spelling, &names, &arguments,
                                    )
                                })
                                .collect(),
                            true,
                        ),
                        None => (declared, !names.is_empty()),
                    };
                let candidate = crate::sema::overload::Candidate::new(
                    types,
                    variadic,
                    format!("declaration {at}"),
                );
                if is_a_template {
                    candidate.with_declaration(declared_for_ordering, names.clone())
                } else {
                    candidate
                }
            })
            .collect();

        Some((types, candidates))
    }

    /// **A file's facts, read from its own view** — the answer an index summary gives, without waiting for one.
    ///
    /// A request can beat the index: the file is open, its tree is parsed, and the summary that records a reflection
    /// annotation (`UCLASS(Blueprintable)`) is built by the pump some time later. A card that needs one field the
    /// binding does not carry cannot wait, so it asks the view — the same `build_facts` the summary is built from, over
    /// the same tree, which is why the two cannot disagree about what the file says.
    ///
    /// One parse and one fact-building pass, the price [`Session::outline`] already pays per request.
    pub fn facts_of_the_view(&self, view: &FileView) -> Vec<crate::DeclFact> {
        let preprocessing = crate::preprocess(&view.source, view.tree.get_tokens());
        let errors: Vec<cpp_parser::SourceRange> = view
            .tree
            .get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range))
            .collect();
        crate::build_facts(&view.scopes, &preprocessing, &view.root, &errors, true).0
    }

    /// **The declaration's own node**, for a fact that names one — the syntax a rendering needs.
    ///
    /// A [`DeclFact`] records the words of a declaration and not the structure of it: whether a class was written
    /// `class`, `struct` or `union` is the node's kind, and a template head's parameters are the node's children. A
    /// caller that has only the fact can render everything a fact carries and must guess the rest, so the card asks for
    /// the node where the fact says the declaration is.
    ///
    /// `None` when the file cannot be read (a header the project never loaded), when the offset is not inside a
    /// declaration, or when the declaration there is not the one asked about — all three are the caller's answer
    /// "render from the fact alone", never an error.
    ///
    /// # Why the answer is a syntax node and not a [`cpp_parser::CppDeclaration`]
    ///
    /// Because **a parameter is a declaration of a name this model does not accept as one**. The parser's declaration
    /// union is `Declaration | UsingDecl | TypedefDecl` (it is what a list of declarations iterates), and a
    /// `Parameter` node is none of them — so asking for the nearest `CppDeclaration` inside
    /// `[](auto const& value)` answered the **enclosing function**, whose specifiers say nothing about `value`. A card
    /// built from that printed `auto& value` for `auto const& value`, and `std::string& left` for
    /// `std::string const& left`. The node is what has the specifiers; a caller that needs the declaration API casts.
    pub fn the_declaration_node(
        &self,
        file: &Path,
        offset: usize,
    ) -> Option<cpp_parser::CppSyntaxNode> {
        let view = self.view_of_the_file(file)?;
        let token = cpp_parser::token_at(&view.root, offset)?;
        use cpp_parser::CppAstNode as _;
        token.parent_ancestors().find(|node| {
            let kind = cpp_parser::CppSyntaxKind::from(node.kind());
            kind == cpp_parser::CppSyntaxKind::Parameter
                || cpp_parser::CppDeclaration::can_cast(kind)
        })
    }

    /// **The template arguments a call fixes**, for a declaration the caller already holds.
    ///
    /// Deduction needs the call's argument types, which is what a `SemanticModel` is for; the names come from the
    /// declaration's own text ([`crate::sema::deduction::template_parameters_before`]) and the parameters from its fact.
    /// `None` when the declaration is not a template, when the offset is not on a call, or when the call does not fix
    /// every parameter — the three answers for which nothing should be shown as an instantiation.
    pub fn the_deduced_arguments(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        template_parameters: &[String],
        parameters: &[String],
    ) -> Option<Vec<String>> {
        if template_parameters.is_empty() {
            return None;
        }
        let token = cpp_parser::token_at(&view.root, offset.get())?;
        let call = token.parent_ancestors().find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::CallExpr
        })?;
        let arguments = crate::sema::call_syntax::arguments_of(&call)?;

        let session: &Session<F> = self;
        let index: &crate::ProjectIndex = session.store.index();
        let resolver =
            Box::new(move |path: &Path| -> Option<FileView> { session.view_of_the_file(path) });
        let model = crate::sema::model::SemanticModel::new(
            index,
            &view.root,
            &view.scopes,
            &view.source,
            &view.path,
            resolver,
        );
        let types: Vec<crate::Known<String>> = arguments
            .iter()
            .map(|argument| match model.type_of(argument) {
                crate::Known::Yes((a_type, _)) => crate::Known::Yes(a_type.to_string()),
                crate::Known::No => crate::Known::No,
                crate::Known::Unknown(reason) => crate::Known::Unknown(reason),
            })
            .collect();

        crate::sema::deduction::deduce(template_parameters, parameters, &types)
    }
    /// The members of a type, as a file's own scopes and the index together know them.
    ///
    /// `written_type` is a spelling — `Widget`, `ns::Widget`, `const Widget&` — and following it to a class is the
    /// same work [`Session::member_completions`] does for an object expression.
    pub fn members_of(&self, view: &FileView, written_type: &str) -> Known<MemberList> {
        let listed = members_of(
            self.store.index(),
            &view.scopes,
            &view.root,
            &view.path,
            written_type,
        );
        // **The class's arguments, written through its members** — see [`substituted_members`].
        let listed = substituted_members(self.store.index(), &view.path, written_type, listed);
        self.the_generated_members(written_type, &view.path, listed)
    }

    /// **What a class costs in memory**: its size, its alignment, and where each data member sits — see
    /// [`crate::sema::layout`].    ///
    /// `written_type` is a spelling (`Widget`, `ns::Widget`). `Known::No` when the name is not a class with a body,
    /// which is what "there is no layout to give" means here: a forward declaration has no size in this model, and
    /// `sizeof` of one is a compile error rather than a number.
    ///
    /// The layout is **withheld** when one of the members cannot be sized: [`ClassLayout::unknown`] names them, and a
    /// number beside a partial answer would be a guess. The one exception is a class with **no data members at all**,
    /// which is one byte by the language's own rule and is reported as such.
    pub fn the_layout_of(
        &self,
        view: &FileView,
        written_type: &str,
    ) -> Known<crate::sema::layout::ClassLayout> {
        self.the_layout_of_a_class(view, written_type, 0)
    }

    pub(crate) fn the_layout_of_a_class(
        &self,
        view: &FileView,
        written_type: &str,
        depth: usize,
    ) -> Known<crate::sema::layout::ClassLayout> {
        self.the_layout_of_a_class_with(view, written_type, &[], depth)
    }

    /// [`Session::the_layout_of_a_class`] for a class **template applied to arguments** — `std::vector<int>`.
    ///
    /// `arguments` are the ones the *use* wrote. An empty slice with a template-id spelling
    /// (`std::vector<int>` passed whole) splits it here; an empty slice with a plain name is the ordinary class.
    ///
    /// # Why the members are sized through a substitution
    ///
    /// A class template's data members are written in its parameters — `T value;` — and their sizes are the sizes of
    /// whatever the arguments make them. The member *facts* keep the declaration's spelling (`vector`'s `_Mypair` is
    /// `_Compressed_pair<_Alty, _Scary_val>`), so every member type is read with the parameters replaced by the
    /// arguments first, and a member whose type is a **member alias** of this class (`_Alty`) is followed to its
    /// target with the same substitution. The recursion is bounded by `depth`, and a member that is still written in
    /// a parameter after substitution is **unknown** rather than looked up by bare name — the mistake that made
    /// `std::vector<widget::Widget>`'s card say "size 9 byte(s), alignment 1" (see `docs/diagnostics-performance.md`
    /// and the note in `the_size_of_a_type_in`).
    pub(crate) fn the_layout_of_a_class_with(
        &self,
        view: &FileView,
        written_type: &str,
        arguments: &[String],
        depth: usize,
    ) -> Known<crate::sema::layout::ClassLayout> {
        if depth > 6 {
            return Known::No;
        }
        let (head, arguments) = match crate::index::project::split_template_arguments(written_type)
        {
            Some((head, written)) if arguments.is_empty() => (head, written),
            _ => (written_type.to_string(), arguments.to_vec()),
        };
        let full = if arguments.is_empty() {
            head.clone()
        } else {
            format!("{head}<{}>", arguments.join(", "))
        };
        let Known::Yes(list) = self.members_of(view, &full) else {
            return Known::No;
        };
        // **The class and its members as their file wrote them** — the text a union's keyword and a bitfield's `:` are
        // read out of, at file offsets. `None` when the index cannot place the class: the layout is still computed for
        // a class the asking file declares, and reported without the two spelling facts it cannot read.
        let as_written = self.the_class_as_its_file_wrote_it(&head, &view.path);
        // **A hover is a request, and a layout is a visibility walk per member.** A class with more than this many
        // data members is a registry — a header's `struct file_buffer` has eighty — and pricing every member costs
        // seconds for a card the reader scrolls past. Measured on neovim: `file_buffer`'s card took **7.2 s** with
        // no cap, and the layout it printed was incomplete anyway. The budget is per class and per request: what a
        // reader opens is a struct with a handful of members.
        let data_members = list
            .own()
            .filter(|member| {
                member.fact.kind == crate::DeclKind::Variable
                    && !member.fact.modifiers.contains(crate::DeclModifiers::STATIC)
            })
            .count();
        if data_members > LAYOUT_MEMBER_BUDGET {
            return Known::No;
        }
        let union = as_written.as_ref().is_some_and(|written| {
            let before = &written.text[..written.class_at.min(written.text.len())];
            // The last keyword before the name wins: `template <class T> union U` is a union, and a `class` earlier
            // in the text belongs to something else.
            ["union", "struct", "class"]
                .into_iter()
                .filter_map(|keyword| before.rfind(keyword).map(|at| (at, keyword)))
                .max_by_key(|(at, _)| *at)
                .is_some_and(|(_, keyword)| keyword == "union")
        });
        // **The environment a member's type is read in** — the class's parameters, the arguments the use wrote (or
        // the declaration's own defaults), and the class's own members, which is where a member alias lives.
        if crate::probes::layout() {
            eprintln!(
                "[layout] class {head:?} arguments {arguments:?} as-written {:?} members {}",
                as_written
                    .as_ref()
                    .map(|written| written.parameters.clone()),
                list.members.len()
            );
        }
        let no_names: Vec<String> = Vec::new();
        let instantiation = Instantiation {
            parameters: as_written
                .as_ref()
                .map_or(&no_names, |written| &written.parameters),
            defaults: as_written
                .as_ref()
                .map_or(&no_names, |written| &written.parameter_defaults),
            arguments: &arguments,
            members: &list.members,
        };
        let mut sized: Vec<(String, usize, usize)> = Vec::new();
        let mut unknown: Vec<String> = Vec::new();
        // **Declaration order**, which is what decides the offsets: `members_of` answers in the index's own order
        // (names sorted, because a lookup by name is what it is for), and a layout computed in that order is a
        // different class. A member's own declaration offset is the only thing that says where it was written.
        let mut own: Vec<&crate::index::ProjectMember> = list.own().collect();
        own.sort_by(|left, right| {
            left.file.cmp(&right.file).then(
                left.fact
                    .range
                    .start_offset
                    .cmp(&right.fact.range.start_offset),
            )
        });
        for member in own {
            let fact = &member.fact;
            // **Data members only**: a member function, a nested type, a `static` — none of them take a byte in the
            // object, and `sizeof` does not count them.
            if fact.kind != crate::DeclKind::Variable
                || fact.modifiers.contains(crate::DeclModifiers::STATIC)
            {
                continue;
            }
            let Some(type_of) = fact.type_of.as_deref() else {
                unknown.push(fact.name.to_string());
                continue;
            };
            // **A bitfield has no width in a fact**, and a layout computed without it puts the next member at the
            // wrong offset — so the class is not answered rather than answered plausibly. The `:` is found at the
            // member's **file** offset, which is why the class was read out of its summary above: a member fact that
            // came from a cooked reading carries the rendering's offsets instead.
            if as_written.as_ref().is_some_and(|written| {
                written
                    .members
                    .get(fact.name.as_str())
                    .is_some_and(|at| Self::a_bitfield(&written.text, *at))
            }) {
                unknown.push(fact.name.to_string());
                continue;
            }
            match self.the_size_of_a_type_in(view, type_of, depth, Some(&instantiation)) {
                Some((size, alignment)) => sized.push((fact.name.to_string(), size, alignment)),
                None => unknown.push(fact.name.to_string()),
            }
        }
        // **A base class takes bytes too, and this layer places none of them.** An *empty* base costs nothing — the
        // language's empty-base optimization is what every compiler does — while a non-empty one would have to be
        // laid at offset zero, with its own alignment, ahead of the members. That is a layout rule this layer does
        // not implement, so a class whose base is not empty is **withheld** rather than reported too small: the same
        // direction `sema::layout`'s note states, and the one a `size` that reads plausibly must not break.
        if let Some(written) = &as_written {
            for base in &written.bases {
                let empty = self
                    .the_layout_of_a_base(view, base, depth + 1, &instantiation)
                    .is_some_and(|layout| layout.fields.is_empty() && layout.size == 1);
                if !empty {
                    unknown.push(format!("base {base}"));
                }
            }
        }
        Known::Yes(crate::sema::layout::ClassLayout::of(&sized, union, unknown))
    }

    /// The layout of one of a class's **bases**, read in the class's own instantiation.
    fn the_layout_of_a_base(
        &self,
        view: &FileView,
        base: &str,
        depth: usize,
        instantiation: &Instantiation<'_>,
    ) -> Option<crate::sema::layout::ClassLayout> {
        let spelled = crate::sema::types::substituted_spelling_with_defaults(
            base,
            instantiation.parameters,
            instantiation.defaults,
            instantiation.arguments,
        );
        if mentions_a_parameter(&spelled, instantiation.parameters) {
            return None;
        }
        match self.the_layout_of_a_class_with(view, &spelled, &[], depth) {
            Known::Yes(layout) => Some(layout),
            _ => None,
        }
    }

    /// **The class as its own file wrote it** — the text, the offset of the class's name, and where each data member's
    /// name sits.
    ///
    /// A fact taken from a **cooked reading** carries offsets in that reading, which is a different text: the union's
    /// keyword and a bitfield's `:` are found in the file's own bytes. The summary's facts are the file's, so this is
    /// where a fact about spelling has to come from. One summary lookup per class, not per member.
    pub(crate) fn the_class_as_its_file_wrote_it(
        &self,
        written_type: &str,
        asked_from: &std::path::Path,
    ) -> Option<TheClassAsWritten> {
        let found = self
            .store
            .index()
            .definition(written_type, asked_from)
            .value()?;
        let text = self.view_of_the_file(&found.file)?.source;
        let summary = self.store.index().summary(&found.file)?;
        let simple = written_type.rsplit("::").next().unwrap_or(written_type);
        let class = summary.declarations.iter().find(|fact| {
            fact.name == simple
                && fact.kind == crate::DeclKind::Type
                && fact.parameter_list.is_none()
        })?;
        // **A member's `scope` is the class's qualified name**, which is the scope the class was written in plus its
        // own name — `Widget` at file scope, `ns::Widget` in a namespace.
        let qualified = match &class.scope {
            Some(ctxt) => format!("{ctxt}::{simple}"),
            None => simple.to_string(),
        };
        let members = summary
            .declarations
            .iter()
            .filter(|fact| {
                fact.kind == crate::DeclKind::Variable && fact.scope.as_deref() == Some(&qualified)
            })
            .map(|fact| (fact.name.to_string(), fact.name_range.end_offset()))
            .collect();
        Some(TheClassAsWritten {
            text: text.to_string(),
            class_at: class.name_range.start_offset,
            members,
            parameters: class.parameters.clone(),
            parameter_defaults: class.parameter_defaults.clone(),
            bases: class.bases.clone(),
        })
    }

    /// **Is this data member a bitfield?** — the text right after its name says so: `unsigned x : 3` has a `:` where
    /// an initializer would have `=`, and a ternary's `:` lives after the one that starts the initializer.
    pub(crate) fn a_bitfield(text: &str, at: usize) -> bool {
        let after = text.get(at..).unwrap_or_default();
        let end = after.find(';').unwrap_or(after.len().min(80));
        after[..end].trim_start().starts_with(':')
    }

    /// The size and alignment of a member's written type, **in the instantiation the member is written in**.
    ///
    /// `instantiation` is `None` for a class that is not a template — the ordinary member — and the class's own
    /// parameters/arguments/members otherwise. The steps, in the order that keeps each one from answering the next
    /// one's question:
    ///
    /// 1. **Substitute** the parameters, with the declaration's own defaults for the arguments a use did not write.
    ///    A spelling that still mentions a parameter afterwards is **unknown** — looking it up by bare name is what
    ///    found an unrelated `_Ty2` in another header and reported `std::vector<widget::Widget>` as 9 bytes.
    /// 2. **A builtin** — the scalars, a pointer, an array of one of them — is the answer.
    /// 3. **A member alias** of the class being laid out (`_Alty`, `_Scary_val`, `Ptr`) is followed to its target,
    ///    substituted in the same environment.
    /// 4. **A class template applied to arguments** is the instantiation: its own layout, computed from *its*
    ///    members with *its* parameters bound to these arguments.
    /// 5. **A typedef** is the type it names, and everything else is an ordinary class.
    pub(crate) fn the_size_of_a_type_in(
        &self,
        view: &FileView,
        spelling: &str,
        depth: usize,
        instantiation: Option<&Instantiation<'_>>,
    ) -> Option<(usize, usize)> {
        if depth > 8 {
            return None;
        }
        let spelled = match instantiation {
            Some(environment) => crate::sema::types::substituted_spelling_with_defaults(
                spelling,
                environment.parameters,
                environment.defaults,
                environment.arguments,
            ),
            None => spelling.to_string(),
        };
        let spelled = spelled.trim();
        if crate::probes::layout() {
            eprintln!("[layout] depth {depth} size of {spelled:?}");
        }
        if let Some(environment) = instantiation
            && mentions_a_parameter(spelled, environment.parameters)
        {
            if crate::probes::layout() {
                eprintln!("[layout]   still a parameter");
            }
            return None;
        }
        if let Some(builtin) = crate::sema::layout::a_builtin_in_memory(spelled) {
            return Some(builtin);
        }
        if let Some(environment) = instantiation
            && let Some(alias) = environment.members.iter().find(|member| {
                member.fact.kind == crate::DeclKind::Type
                    && member.fact.name == spelled
                    && member.fact.parameter_list.is_none()
            })
            && let Some(target) = alias.fact.type_of.clone()
        {
            return self.the_size_of_a_type_in(view, &target, depth + 1, instantiation);
        }
        // **A dependent type** — `typename allocator_traits<_Alloc>::template rebind_alloc<_Ty>`. Its owner is a
        // class template, and the member is an alias whose own parameters are the use's arguments; resolving it is
        // what turns a standard container's `_Alty` into a type this layer can size (or, for an empty allocator crop,
        // into nothing at all). Asked before the template-id branch because the tail after the last `::` is a
        // template-id too, and taking the head as a class would look up `rebind_alloc` as if it were one.
        if let Some((owner, member, arguments)) = split_a_dependent_name(spelled) {
            return self.the_size_of_a_dependent_type(view, &owner, &member, &arguments, depth);
        }
        if let Some((head, arguments)) = crate::index::project::split_template_arguments(spelled) {
            // **The arguments are read in the environment of the class being laid out.** `_Mypair`'s type is
            // `_Compressed_pair<_Alty, _Scary_val>`, and both spellings are *member aliases of `std::vector`* — so
            // they are resolved here, where that class's members are at hand, and `_Compressed_pair` is instantiated
            // with what they name.
            let resolved: Vec<String> = arguments
                .iter()
                .map(|argument| {
                    self.the_spelling_through_the_member_aliases(argument, instantiation)
                })
                .collect();
            match self.the_layout_of_a_class_with(view, &head, &resolved, depth + 1) {
                Known::Yes(layout) => {
                    return layout
                        .unknown
                        .is_empty()
                        .then_some((layout.size, layout.alignment));
                }
                // **Not a class: an alias template**, whose fact holds the spelling it expands to
                // (`_Rebind_alloc_t` is `typename allocator_traits<_Alloc>::template rebind_alloc<_Value_type>`).
                Known::No | Known::Unknown(_) => {}
            }
            if let Some(target) = self.the_target_of_an_alias_template(&head, &resolved, &view.path)
            {
                if crate::probes::layout() {
                    eprintln!("[layout]   alias template {head} -> {target}");
                }
                return self.the_size_of_a_type_in(view, &target, depth + 1, None);
            }
            if crate::probes::layout() {
                eprintln!("[layout]   no class and no alias template for {head} with {resolved:?}");
            }
            return None;
        }
        // **A typedef is the type it names.** Real C decides its members through one — neovim's `file_buffer` has
        // `pos_T b_op_start`, `handle_T handle`, `memline_T b_ml` — so without this the layout of most real structs
        // is "not computed". The alias's own fact holds the spelling it expands to.
        if let Known::Yes(aliased) = self.store.index().definition(spelled, &view.path)
            && let Some(target) = aliased.fact.type_of.as_deref()
            && target != spelled
            && depth <= 8
        {
            return self.the_size_of_a_type_in(view, target, depth + 1, instantiation);
        }
        let Known::Yes(nested) = self.the_layout_of_a_class_with(view, spelled, &[], depth + 1)
        else {
            return None;
        };
        if nested.unknown.is_empty() {
            return Some((nested.size, nested.alignment));
        }
        None
    }

    /// **A member type named through a class template instance** — `typename A<args>::template name<args2>`.
    ///
    /// The owner is instantiated by [`Session::members_of`] (which substitutes its parameters), and the member alias
    /// it names carries its own parameters — `allocator_traits<…>::rebind_alloc<_Other>` is `allocator<_Other>` — so
    /// the use's arguments are substituted into the target before it is sized.
    fn the_size_of_a_dependent_type(
        &self,
        view: &FileView,
        owner: &str,
        member: &str,
        arguments: &[String],
        depth: usize,
    ) -> Option<(usize, usize)> {
        let Known::Yes(list) = self.members_of(view, owner) else {
            return None;
        };
        let alias = list.members.iter().find(|candidate| {
            candidate.fact.name == member
                && candidate.fact.kind == crate::DeclKind::Type
                && candidate.fact.type_of.is_some()
        })?;
        let written = alias.fact.type_of.as_deref()?;
        let target = crate::sema::types::substituted_spelling_with_defaults(
            written,
            &alias.fact.parameters,
            &alias.fact.parameter_defaults,
            arguments,
        );
        self.the_size_of_a_type_in(view, &target, depth + 1, None)
    }

    /// **A spelling with the enclosing class's member aliases substituted into it** — `_Scary_val` becomes what
    /// `std::vector` declares it to be, in the instantiation being laid out.
    ///
    /// One alias, and only when the **whole spelling** is its name. Textual replacement of a name *inside* a longer
    /// spelling was measured and is not sound: the member aliases of a standard container name each other
    /// (`_Val_types`, `_Alvbase_traits`, `size_type`), and substituting one after another expands the spelling without
    /// bound — the trace printed megabytes for a single `std::vector<widget::Widget>`, splicing fragments of other
    /// classes' aliases wherever their names happened to occur. A name is resolved where its scope is known, which is
    /// the recursive class walk below, not a global text substitution.
    fn the_spelling_through_the_member_aliases(
        &self,
        spelling: &str,
        instantiation: Option<&Instantiation<'_>>,
    ) -> String {
        let Some(environment) = instantiation else {
            return spelling.to_string();
        };
        let spelled = crate::sema::types::substituted_spelling_with_defaults(
            spelling,
            environment.parameters,
            environment.defaults,
            environment.arguments,
        );
        let Some(alias) = environment.members.iter().find(|member| {
            member.fact.kind == crate::DeclKind::Type
                && member.fact.name == spelled.trim()
                && member.fact.parameter_list.is_none()
        }) else {
            return spelled;
        };
        match alias.fact.type_of.as_deref() {
            Some(target) => crate::sema::types::substituted_spelling_with_defaults(
                target,
                environment.parameters,
                environment.defaults,
                environment.arguments,
            ),
            None => spelled,
        }
    }

    /// **The target of an alias template applied to arguments** — `_Rebind_alloc_t<A, T>` is what its fact says,
    /// substituted.
    fn the_target_of_an_alias_template(
        &self,
        head: &str,
        arguments: &[String],
        path: &std::path::Path,
    ) -> Option<String> {
        let known = self.store.index().definition(head, path).value()?;
        let written = known.fact.type_of.as_deref()?;
        if known.fact.parameters.is_empty() {
            return None;
        }
        let target = crate::sema::types::substituted_spelling_with_defaults(
            written,
            &known.fact.parameters,
            &known.fact.parameter_defaults,
            arguments,
        );
        (target != head).then_some(target)
    }

    /// **The members `GENERATED_BODY()` writes that a macro cannot** — `Super` and `ThisClass`.
    ///
    /// Unreal's build tool writes `typedef <Base> Super;` and `typedef <TheClass> ThisClass;` into the generated code,
    /// so a project whose generated headers are present has them as ordinary members and this adds nothing. A project
    /// that is read from a **simulation** — the macro expanded to the name-independent half, `StaticClass()` and
    /// `GetPrivateStaticClass()` — has the annotation and not these, which is the case this exists for: measured on
    /// `UE-like`, `Super::BeginPlay()` and `ThisClass` were names nothing declared.
    ///
    /// The signal is `StaticClass`, which only the generated code (or a simulation of it) declares, and the absence of
    /// `ThisClass`, which says the generated code is not here. Outside `[unreal]` nothing is added: an ordinary C++
    /// class with a `StaticClass` member must not grow a `Super` it never declared.
    pub(crate) fn the_generated_members(
        &self,
        written_type: &str,
        path: &std::path::Path,
        listed: Known<crate::index::MemberList>,
    ) -> Known<crate::index::MemberList> {
        let unreal = self
            .project_config()
            .config
            .unreal
            .as_ref()
            .is_some_and(|unreal| unreal.is_on());
        if !unreal {
            return listed;
        }
        let Known::Yes(mut list) = listed else {
            return listed;
        };
        let declares_static_class = list
            .members
            .iter()
            .any(|member| member.fact.name == "StaticClass");
        let already_generated = list
            .members
            .iter()
            .any(|member| member.fact.name == "ThisClass");
        if !declares_static_class || already_generated {
            return Known::Yes(list);
        }
        let Known::Yes(declared) = self.store.index().definition(written_type, path) else {
            return Known::Yes(list);
        };
        // The synthetic members point at the class's own declaration: that is where the macro that generates them is
        // written, and it is the honest place for a hover to open.
        let class_fact = declared.fact.clone();
        let class_name = class_fact.name.to_string();
        for (name, type_of) in crate::project::ue::generated_members(&class_name, &class_fact.bases)
        {
            let mut fact = class_fact.clone();
            fact.name = name.into();
            fact.kind = crate::DeclKind::Type;
            fact.type_of = Some(type_of.into());
            // A generated member is not a member of a **namespace**: it belongs to the class it was generated for.
            fact.scope = Some(class_name.as_str().into());
            list.members.push(crate::index::ProjectMember {
                file: declared.file.clone(),
                fact,
                declared_in: class_name.to_string(),
                depth: 0,
                ambiguous: false,
            });
        }
        Known::Yes(list)
    }

    /// **A type name as a declaration wrote it, resolved where it was written — through a base if it has to.**
    ///
    /// [`ProjectIndex::definition_where_written`] answers for a name written in a scope: it follows an alias,
    /// and it looks outward through the enclosing scopes the way C++ does. What it cannot answer is a member the
    /// class **inherits**, because that is not a lookup in the index at all — it is a walk of the base chain,
    /// and the walk lives here, where the file's scope tree and tree are, rather than in the index.
    ///
    /// Measured, and it is what this wire exists for:
    ///
    /// ```text
    /// std::allocator_traits::pointer        NotDeclaredHere   the name is inherited, not declared
    /// members_of("std::allocator_traits")   44 members        …and `pointer` is one of them
    /// ```
    ///
    /// `pointer` is declared in `_Normal_allocator_traits`, which `allocator_traits` names only inside a
    /// `conditional_t<…>` — so finding it needs the alias step, the outward scope step **and** the base walk,
    /// and this is the one entry point that has all three.
    ///
    /// # A diamond is `Ambiguous`, and that answer comes from the walk rather than from here
    ///
    /// A member two bases declare at once is not uniquely resolved by the language, and the walk records it as
    /// [`ProjectMember::ambiguous`]. Picking one would be a jump to an entity the user cannot tell from the
    /// other, which is the answer this project refuses to give.
    pub fn definition_of_a_written_type(
        &self,
        view: &FileView,
        written: &str,
        in_scope: Option<&str>,
        at: usize,
    ) -> Known<ProjectDefinition> {
        let index = self.store.index();

        // **A declaration inside a body has no scope to be named by, and the file's own scopes can supply
        // one.** [`DeclFact::scope`] is `None` for a local — a function body contributes no segment to a
        // qualified name — so a name it wrote has nothing to be looked up from, and a local `Widget` in
        // `namespace app` is a name that cannot be found. The view's scope chain at the declaration's own
        // offset says which namespaces enclose it.
        //
        // **Literal namespaces only, and that is the honest limit here.** A view is built with no macro
        // evidence (see [`FileView::parse`]), so `namespace app {` is a scope in it and `_STD_BEGIN` — whose
        // replacement list is in a header nothing here has read — is not. A name written inside a namespace the
        // file spells out is therefore resolvable, and one inside a namespace a macro opened is not; the second
        // needs the fact to carry the namespace, which is a change to what a summary stores rather than to what
        // this query asks.
        let enclosing = match in_scope {
            Some(scope) => Some(scope.to_string()),
            None => enclosing_namespace_at(view, at),
        };

        let direct = index.definition_where_written(written, enclosing.as_deref(), &view.path);
        if !matches!(direct, Known::Unknown(UnknownReason::NotDeclaredHere(_))) {
            return direct;
        }
        let in_scope = enclosing.as_deref();

        // **The class half, then the member.** The class is handed to [`Session::members_of`] **as written**,
        // because that query is the one that already knows how to turn a spelling into a class: it follows an
        // alias, it looks outward through the enclosing scopes, and it walks the bases. Resolving it here first
        // would be a second implementation of that, and it was one — asking
        // `ProjectIndex::definition_where_written` for `std::allocator_traits` answers `Unknown(Ambiguous(…))`,
        // because the class is forward-declared **and** defined, and an `Ambiguous` read as "no class" is the
        // same mistake this file's own `is_declared` and `direct_members` each had to have fixed.
        let Some((class, member)) = written.rsplit_once("::") else {
            return direct;
        };

        // **Qualified by the scope first, then as written** — and the order is the whole of the care here. A
        // class named unqualified inside another one is written relative to it, so `_Mybase` in
        // `std::_Vb_const_iterator` means `std::_Vb_const_iterator::_Mybase` and not the bare name. Trying the
        // bare spelling first was the first attempt, and it stopped the walk on the wrong class: `members_of`
        // answered `Yes` for `_Mybase` — there is a class by that name somewhere — and the loop took that as the
        // class to look the member up in, so the scope-qualified spelling was never reached. Measured, that is
        // `_Mybase::_Mycont` at `std::_Vb_const_iterator`: unresolved with the bare spelling tried first, and
        // `Yes` when the same name is asked in full.
        let mut tried: Vec<String> = Vec::new();
        if let Some(scope) = in_scope.filter(|_| !class.starts_with("::") && !class.contains("::"))
        {
            tried.push(format!("{scope}::{class}"));
        }
        tried.push(class.to_string());

        let mut list = None;
        for spelling in &tried {
            if let Known::Yes(found) = self.members_of(view, spelling) {
                list = Some(found);
                break;
            }
        }

        let Some(list) = list else {
            return direct;
        };

        let mut found = list
            .members
            .iter()
            .filter(|found| found.fact.name == member);

        let Some(only) = found.next() else {
            // Nothing in the chain declares it, and the honest answer is the one the direct lookup gave: the
            // name is not here, rather than a name nobody wrote.
            return direct;
        };

        // **Two answers, or one answer the walk already called ambiguous.** A member that two bases declare at
        // once is not uniquely resolved by the language, and a jump to either is a jump to an entity the user
        // cannot tell from the other.
        if found.next().is_some() || only.ambiguous {
            return Known::Unknown(UnknownReason::Ambiguous(Box::from(written)));
        }

        Known::Yes(ProjectDefinition {
            file: only.file.clone(),
            fact: only.fact.clone(),
        })
    }

    /// **The file's declarations as a tree** — what an outline, a breadcrumb bar or a folding range is drawn from.
    ///
    /// # One source: the file's own text
    ///
    /// An outline is about the **file**, not about the program: a declaration in a branch nobody takes belongs in an
    /// outline, and a type a macro declares does not. So this reads the view it is given — the file's own parse
    /// ([`Session::view_of_the_file`]) — and never the index's summary.
    ///
    /// **That used to be worded "the index's summary when it has one, and the buffer's own parse
    /// when it does not", and the difference is no longer a fallback but a wrong answer.** While a
    /// summary was the file's own text reading, the two sources agreed about everything an outline
    /// shows. Measured in `cpp_ls`'s `the_outline_shows_the_file_and_not_the_compiler`, which is
    /// what says the two readings must not be mixed here.
    ///
    /// # What it costs, and why that is not a regression
    ///
    /// One parse of the file's own bytes per request (the buffer, which the VFS already holds), and no dependence on a
    /// rendering existing — so the "summary is gone between a keystroke and the next drain" case that the fallback
    /// existed for is now the only case, and the panel never waits for a cook.
    pub fn outline(&self, view: &FileView) -> Vec<OutlineSymbol> {
        let preprocessing = crate::preprocess(&view.source, view.tree.get_tokens());
        let errors: Vec<cpp_parser::SourceRange> = view
            .tree
            .get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range))
            .collect();
        let (facts, _guards) =
            crate::build_facts(&view.scopes, &preprocessing, &view.root, &errors, true);

        crate::outline_of(&facts)
    }
}

/// **A class as its own file wrote it** — the text, where its name sits, where each data member's name sits, and the
/// template parameters it declares.
///
/// The last field is what makes a layout honest: `_Ty2 _Mysecond;` inside `template <class _Ty1, class _Ty2> class
/// _Compressed_pair` has the size of whatever the instantiation puts there, and a lookup of `_Ty2` by bare name in
/// the asking file finds some *other* class spelled `_Ty2` — measured on `std::vector<widget::Widget>`, whose card
/// said "size 9 byte(s), alignment 1" from exactly that. A member written in one of the class's parameters is
/// therefore **unknown**, which withholds the layout rather than printing a number beside a guess (see
/// [`crate::sema::layout::ClassLayout::unknown`]).
pub(crate) struct TheClassAsWritten {
    pub text: String,
    pub class_at: usize,
    pub members: std::collections::HashMap<String, usize>,
    pub parameters: Vec<String>,
    pub parameter_defaults: Vec<String>,
    pub bases: Vec<String>,
}

/// **The instantiation a class's members are read in** — its parameters, the arguments a use wrote (or the
/// declaration's own defaults), and the class's members, which is where a *member alias* such as `_Alty` lives.
pub(crate) struct Instantiation<'a> {
    pub parameters: &'a [String],
    pub defaults: &'a [String],
    pub arguments: &'a [String],
    pub members: &'a [crate::index::ProjectMember],
}

/// Does a type spelling use one of these names as a **word**? _Ty2 matches _Ty2* and ector<_Ty2>; _Ty20 does
/// not match _Ty2.
/// **A type named through another type** — `typename A<args>::template name<args2>` is `(A<args>, name, [args2])`.
///
/// The `::` that matters is the last one at angle-bracket depth zero: `allocator_traits<allocator<int>>::rebind_alloc`
/// has three `:` pairs, and the last is the separator while the `std::`-shaped prefixes belong to the owner. Only a
/// name whose **owner is an instantiation** is a dependent type here; `ns::Widget` is a qualified name, which the
/// ordinary lookups already answer.
fn split_a_dependent_name(spelling: &str) -> Option<(String, String, Vec<String>)> {
    let trimmed = spelling.trim().trim_start_matches("typename ").trim();
    let (owner, tail) = split_at_the_last_qualifier(trimmed)?;
    if !owner.contains('<') {
        return None;
    }
    let tail = tail.trim().trim_start_matches("template ").trim();
    if tail.is_empty() {
        return None;
    }
    match crate::index::project::split_template_arguments(tail) {
        Some((member, arguments)) => Some((owner.to_string(), member, arguments)),
        None => Some((owner.to_string(), tail.to_string(), Vec::new())),
    }
}

/// The last `::` outside angle brackets, split into what is on either side.
fn split_at_the_last_qualifier(text: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let mut at = None;
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'<' => depth += 1,
            b'>' => depth = depth.saturating_sub(1),
            b':' if depth == 0 && bytes.get(index + 1) == Some(&b':') => at = Some(index),
            _ => {}
        }
    }
    let at = at?;
    Some((&text[..at], &text[at + 2..]))
}

fn mentions_a_parameter(spelling: &str, parameters: &[String]) -> bool {
    if parameters.is_empty() {
        return false;
    }

    spelling
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .any(|word| parameters.iter().any(|parameter| parameter == word))
}
