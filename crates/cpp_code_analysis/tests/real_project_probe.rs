//! **The real project, through the session the server uses** — not through `inspect`.
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test real_project_probe -- --ignored --nocapture
//! ```
//!
//! `inspect` answers one file's own reading and does **not** inline its includes (measured: the "cooked" tree of
//! `LuaCodeFormat.cpp` was 10 086 bytes, the file's own size), so it cannot reproduce anything that depends on a header.
//! This opens the project the way the server does — every file, the closure pumped — and asks the two questions the
//! editor reports: the type of `result` at `LuaCodeFormat.cpp:304`, and how the qualifier of the definition at line 300
//! is classified.

use std::path::PathBuf;

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::{
    DeclFact, DeclKind, DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter,
};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\github\EmmyLuaCodeStyle"));
    named
        .join("CodeFormatLib")
        .join("src")
        .is_dir()
        .then_some(named)
}

fn sources_under(root: &std::path::Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if name == "out" || name == ".git" || name == ".cppls" || name == "3rd" {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let extension = path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_ascii_lowercase();
            if matches!(
                extension.as_str(),
                "cpp" | "cc" | "cxx" | "h" | "hpp" | "hxx"
            ) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

#[test]
#[ignore = "a probe: needs the real standard library"]
fn does_refining_the_header_itself_scope_it() {
    // **The mechanism under test, on its own.** `<vector>` is the file whose scopes come out of `yvals_core.h`'s
    // `_STD_BEGIN`. Nothing in the session ever re-read it, so the question this answers is: if the normal pump *did*
    // take it as a file being indexed — the ask served, the drain's `files` list naming it — would its summary come
    // out with `std` scopes? A no would mean the pass is broken for this input; a yes would mean the pass is right
    // and never reached.
    let msvc = PathBuf::from(
        r"C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.51.36231\include\vector",
    );
    if !msvc.is_file() {
        eprintln!("[header-probe] the standard library is not here");
        return;
    }
    let root = msvc.parent().expect("the include directory").to_path_buf();

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([msvc.clone()]);
    session.want_refinement(&msvc);
    for _ in 0..40 {
        session.index_everything();
        session.catch_up(&msvc);
        if session.pending_work() == 0 {
            break;
        }
    }

    match session.index().summary(&msvc) {
        Some(summary) => {
            let in_std = summary
                .declarations
                .iter()
                .filter(|fact| {
                    fact.scope
                        .as_deref()
                        .is_some_and(|scope| scope == "std" || scope.starts_with("std::"))
                })
                .count();
            eprintln!(
                "[header-probe] {} fact(s), {in_std} with a `std` scope, {} macro reading(s)",
                summary.declarations.len(),
                summary.macro_readings.len()
            );
        }
        None => eprintln!("[header-probe] no summary"),
    }
    match session.index().definitions("std::vector", &msvc) {
        Known::Yes(found) => {
            for definition in &found.found {
                eprintln!(
                    "[header-probe] std::vector -> {} (kind {:?}, scope {:?}, definition {})",
                    definition.file.display(),
                    definition.fact.kind,
                    definition.fact.scope,
                    definition.fact.definition
                );
            }
            eprintln!(
                "[header-probe] `std::vector`: {} declaration(s)",
                found.found.len()
            );
        }
        Known::Unknown(reason) => eprintln!("[header-probe] `std::vector`: unknown — {reason:?}"),
        Known::No => eprintln!("[header-probe] `std::vector`: no"),
    }
}

#[test]
#[ignore = "a probe: needs the external project"]
fn the_alias_before_and_after_the_index_is_read() {
    // **The server's answer and this crate's answer differ about one name, and the difference is the state of the
    // index.** Over stdio (`cpp_ls/tests/real_project_hover_probe.rs`), both the hover and the inlay hint at
    // `LuaCodeFormat.cpp:131` say `basic_string<char, char_traits<char>, allocator<char>>` — the alias expanded —
    // and they keep saying it for ninety seconds. In a session whose queue has drained, the same call says
    // `std::string`. So the question is which step of the index turns one into the other.
    let Some(root) = project() else {
        eprintln!("[alias-probe] the project is not here");
        return;
    };
    let file = root.join("CodeFormatLib/src/LuaCodeFormat.cpp");
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("[alias-probe] no LuaCodeFormat.cpp");
        return;
    };
    let Some(at) = text.find("auto formattedText") else {
        eprintln!("[alias-probe] the line is not there");
        return;
    };
    let cursor = FileOffset::new(at + "auto ".len());

    let mut session = Session::open_with_config_file(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        None,
    );
    session.add_project_files([file.clone()]);
    session.want_cooked_reading(&file);

    let ask = |session: &Session<DiskFiles>, label: &str| {
        let Some(view) = session.view(&file) else {
            eprintln!("[alias-probe] {label}: no view");
            return;
        };
        let reading = view
            .reading_offset_of(cursor)
            .unwrap_or(ReadingOffset::new(cursor.get()));
        match session.type_at(&view, reading) {
            Known::Yes(found) => eprintln!("[alias-probe] {label}: `{}`", found.type_of),
            Known::Unknown(reason) => eprintln!("[alias-probe] {label}: unknown — {reason:?}"),
            Known::No => eprintln!("[alias-probe] {label}: no"),
        }
    };

    // The state a request finds first: the file read and cooked, but **before** its includes are followed — which is
    // what the server's first answer looks like.
    session.index_everything();
    ask(
        &session,
        &format!(
            "the file alone ({} summaries)",
            session.index().summaries().count()
        ),
    );

    session.catch_up(&file);
    ask(
        &session,
        &format!(
            "after one catch_up ({} summaries)",
            session.index().summaries().count()
        ),
    );

    for round in 0..40 {
        session.index_everything();
        session.catch_up(&file);
        if round < 12 || session.pending_work() == 0 {
            ask(
                &session,
                &format!(
                    "round {round} ({} summaries, {} pending)",
                    session.index().summaries().count(),
                    session.pending_work()
                ),
            );
        }
        if session.pending_work() == 0 {
            break;
        }
    }
    ask(&session, "after the queue drains");
    eprintln!(
        "[alias-probe] {} summary(ies), pending {}",
        session.index().summaries().count(),
        session.pending_work()
    );

    // **The request a freshly opened file gets**, which is the state the server answers in: `didOpen`, the
    // `catch_up` a handler does, then the question — with no pump in between.
    let mut fresh = Session::open_with_config_file(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        None,
    );
    fresh.add_project_files([file.clone()]);
    fresh.did_open(&file, &text);
    fresh.catch_up(&file);
    let ask_in = |session: &Session<DiskFiles>, label: &str| {
        let Some(view) = session.view(&file) else {
            eprintln!("[alias-probe] {label}: no cooked view");
            if let Some(plain) = session.view_of_the_file(&file) {
                let reading = plain
                    .reading_offset_of(cursor)
                    .unwrap_or(ReadingOffset::new(cursor.get()));
                match session.type_at(&plain, reading) {
                    Known::Yes(found) => {
                        eprintln!("[alias-probe] {label}: plain view `{}`", found.type_of)
                    }
                    other => eprintln!("[alias-probe] {label}: plain view {other:?}"),
                }
            }
            return;
        };
        let reading = view
            .reading_offset_of(cursor)
            .unwrap_or(ReadingOffset::new(cursor.get()));
        match session.type_at(&view, reading) {
            Known::Yes(found) => eprintln!("[alias-probe] {label}: `{}`", found.type_of),
            Known::Unknown(reason) => eprintln!("[alias-probe] {label}: unknown — {reason:?}"),
            Known::No => eprintln!("[alias-probe] {label}: no"),
        }
    };
    ask_in(&fresh, "a fresh open + catch_up");
    fresh.index_everything();
    ask_in(&fresh, "and after one pump");
}

/// **How many deduced types are still library-internal spellings** — the generics survey.
///
/// Every `auto` declaration in the project's own sources is asked for its type, and the answers are split into
/// "a spelling a reader could write" and "an internal one" — `_Alty_traits::size_type`, `_Ty`, `_Mybase::iterator`.
/// An internal answer is a chain that stopped one step short, which is the shape this whole line of work is about;
/// the number is what says whether a fix helped beyond the one line it was measured on.
///
/// ```text
/// cargo test --release -p cpp_code_analysis --test real_project_probe -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a probe: needs the external project"]
fn how_many_deduced_types_are_internal_spellings() {
    let Some(root) = project() else {
        eprintln!("[generics] the project is not here");
        return;
    };
    let mut files = sources_under(&root);
    files.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cpp"))
    });
    files.sort();
    let Some(first) = files.first().cloned() else {
        eprintln!("[generics] no source files");
        return;
    };

    let mut session = Session::open_with_config_file(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        None,
    );
    session.add_project_files(files.iter().cloned());
    session.want_cooked_reading(&first);
    for _ in 0..40 {
        session.index_everything();
        session.catch_up(&first);
        if session.pending_work() == 0 {
            break;
        }
    }

    let mut total = 0usize;
    let mut internal = 0usize;
    let mut examples: Vec<String> = Vec::new();
    for path in files.iter().take(120) {
        let Some(view) = session.view_of_the_file(path) else {
            continue;
        };
        let len = view.source.len();
        let hints = session.type_hints(&view, cpp_parser::SourceRange::new(0, len));
        for hint in hints {
            total += 1;
            let label = hint.label.trim();
            let is_internal = label.starts_with('_')
                || label.contains("::_")
                || label.contains("_Ty")
                || label.contains("_Alloc")
                || label.contains("_Mybase");
            if !is_internal {
                continue;
            }
            internal += 1;
            if examples.len() < 12 {
                examples.push(format!(
                    "{}: {}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    label
                ));
            }
        }
    }
    eprintln!("[generics] {internal} of {total} deduced type(s) are internal spellings");
    for example in examples {
        eprintln!("[generics]   {example}");
    }
}

#[test]
#[ignore = "a probe: needs the external project"]
fn the_two_reported_enums_in_the_parser() {
    // Reported with screenshots: `BinOpr::OPR_SHR` in `LuaParser.cpp:1010` is drawn as a **property**, and
    // `case TK_LE:` in the same file at 1019 gets **no semantic token** (`variable.other.unknown`). The two enums are
    // in headers that file includes directly — `enum class BinOpr` in `LuaOperatorType.h`, `enum LuaTokenTypeDetail`
    // in `LuaTokenTypeDetail.h` — while the earlier fix was measured on `SpaceAnalyzer.cpp`, another file.
    let Some(root) = project() else {
        eprintln!("[enum2] the project is not here");
        return;
    };
    let file = root.join("LuaParser/src/Parse/LuaParser.cpp");
    if std::fs::read_to_string(&file).is_err() {
        eprintln!("[enum2] no LuaParser.cpp");
        return;
    }

    let mut session = Session::open_with_config_file(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        None,
    );
    session.add_project_files([file.clone()]);
    session.want_cooked_reading(&file);
    for _ in 0..40 {
        session.index_everything();
        session.catch_up(&file);
        if session.pending_work() == 0 {
            break;
        }
    }

    for spelling in ["TK_LE", "OPR_SHR", "BinOpr", "LuaTokenTypeDetail"] {
        match session.index().definitions(spelling, &file) {
            Known::Yes(found) => {
                eprintln!("[enum2] `{spelling}`: {} declaration(s)", found.found.len());
                for one in found.found.iter().take(3) {
                    eprintln!(
                        "[enum2]   {} kind {:?}, scope {:?}, type_of {:?}",
                        one.file.file_name().unwrap_or_default().to_string_lossy(),
                        one.fact.kind,
                        one.fact.scope,
                        one.fact.type_of
                    );
                }
            }
            other => eprintln!("[enum2] `{spelling}`: {other:?}"),
        }
        match session.index().kind_of(spelling, &file) {
            Known::Yes(kind) => eprintln!("[enum2] kind_of({spelling}): {kind:?}"),
            other => eprintln!("[enum2] kind_of({spelling}): {other:?}"),
        }
    }

    let Some(view) = session.view_of_the_file(&file) else {
        eprintln!("[enum2] no view");
        return;
    };
    let names = session.classified_names(&view);
    for spelling in ["TK_LE", "OPR_SHR"] {
        let found: Vec<_> = names
            .iter()
            .filter(|name| view.get_token_text(name.range) == Some(spelling))
            .collect();
        eprintln!(
            "[enum2] {spelling}: {} classification(s) {:?}",
            found.len(),
            found
                .iter()
                .map(|name| (name.kind, name.provenance))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
#[ignore = "a probe: needs the external project"]
fn what_a_size_call_returns() {
    // Reported with a screenshot: `auto pos = _events.size();` in `LuaParser.cpp:51` is drawn as
    // `_Alloc_traits::size_type` — an alias of an alias of a member of a class, not a type a reader can use. The
    // chain the standard library writes is `size_type` → `_Alloc_traits::size_type` → `allocator_traits<…>::size_type`
    // → `typename _Alloc::size_type` → `size_t`, and the question is which step stops.
    let Some(root) = project() else {
        eprintln!("[size-probe] the project is not here");
        return;
    };
    let file = root.join("LuaParser/src/Parse/LuaParser.cpp");
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("[size-probe] no LuaParser.cpp");
        return;
    };
    let Some(at) = text.find("auto pos = _events.size();") else {
        eprintln!("[size-probe] the line is not there");
        return;
    };
    let cursor = at + "auto ".len();

    let mut session = Session::open_with_config_file(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        None,
    );
    session.add_project_files([file.clone()]);
    session.want_cooked_reading(&file);
    for _ in 0..40 {
        session.index_everything();
        session.catch_up(&file);
        if session.pending_work() == 0 {
            break;
        }
    }

    // **The standard header has to be asked for.** The server's session resolved `std::vector<MarkEvent>` — its hover
    // says so — which means `<vector>` was refined there; nothing in a plain session asks for it, and the summary
    // alone has the class at file scope (see the `_STD_BEGIN` entry in `docs/open-questions.md`).
    let vector_header = PathBuf::from(
        r"C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.51.36231\include\vector",
    );
    session.want_refinement(&vector_header);
    for _ in 0..40 {
        session.index_everything();
        session.catch_up(&file);
        if session.pending_work() == 0 {
            break;
        }
    }

    {
        let Some(view) = session.view(&file) else {
            eprintln!("[size-probe] no view");
            return;
        };
        let reading = view
            .reading_offset_of(FileOffset::new(cursor))
            .unwrap_or(ReadingOffset::new(cursor));
        match session.type_at(&view, reading) {
            Known::Yes(found) => eprintln!("[size-probe] pos (first request): `{}`", found.type_of),
            Known::Unknown(reason) => {
                eprintln!("[size-probe] pos (first request): unknown — {reason:?}")
            }
            Known::No => eprintln!("[size-probe] pos (first request): no"),
        }
    }

    // **The ask the query left behind is served by the next pump**, which is what a server does between two
    // requests. This is the second request.
    for _ in 0..40 {
        session.index_everything();
        session.catch_up(&file);
        if session.pending_work() == 0 {
            break;
        }
    }
    let Some(view) = session.view(&file) else {
        eprintln!("[size-probe] no view");
        return;
    };
    let reading = view
        .reading_offset_of(FileOffset::new(cursor))
        .unwrap_or(ReadingOffset::new(cursor));
    match session.type_at(&view, reading) {
        Known::Yes(found) => eprintln!("[size-probe] pos (second request): `{}`", found.type_of),
        Known::Unknown(reason) => {
            eprintln!("[size-probe] pos (second request): unknown — {reason:?}")
        }
        Known::No => eprintln!("[size-probe] pos (second request): no"),
    }

    // **Which `allocator_traits` the member list would be made of**, and whether the qualified spelling exists at all.
    // The walk's trace says the list came back "0 listed, 1 unlisted", so the question is which declaration that class
    // resolved to — and whether refining one file would fix it.
    for spelling in [
        "allocator_traits",
        "std::allocator_traits",
        "_Alty",
        "_Alty_traits",
        "vector",
        "std::vector",
    ] {
        match session.index().definitions(spelling, &file) {
            Known::Yes(found) => {
                eprintln!(
                    "[size-probe] `{spelling}`: {} declaration(s)",
                    found.found.len()
                );
                for one in found.found.iter().take(3) {
                    eprintln!(
                        "[size-probe]   {} kind {:?}, scope {:?}",
                        one.file.file_name().unwrap_or_default().to_string_lossy(),
                        one.fact.kind,
                        one.fact.scope
                    );
                }
            }
            other => eprintln!("[size-probe] `{spelling}`: {other:?}"),
        }
    }
    // **What the index recorded about the class itself** — the base clause is the whole question: `allocator_traits`
    // has an empty body and inherits `size_type` from whichever `conditional_t` branch its allocator selects.
    for spelling in ["std::allocator_traits", "std::_Normal_allocator_traits"] {
        match session.index().definitions(spelling, &file) {
            Known::Yes(found) => {
                for one in &found.found {
                    eprintln!(
                        "[size-probe] `{spelling}` fact: kind {:?}, scope {:?}, bases {:?}, cooked file {}",
                        one.fact.kind,
                        one.fact.scope,
                        one.fact.bases,
                        session.index().cooked_reading(&one.file).is_some()
                    );
                }
            }
            other => eprintln!("[size-probe] `{spelling}`: {other:?}"),
        }
    }

    // **The trait that decides the branch**, as the index records it: the specialization has two parameters in
    // MSVC's `<xmemory>` (`_Is_default_allocator<allocator<_Ty>, void_t<…>>`), while the use writes one.
    for (path, fact) in session.index().every_fact_named("_Is_default_allocator") {
        eprintln!(
            "[size-probe] trait in {}: kind {:?}, pattern {:?}, bases {:?}, parameters {:?}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            fact.kind,
            fact.pattern,
            fact.bases,
            fact.parameters
        );
    }

    if let Some(summary) = session
        .index()
        .summary(&vector_header.with_file_name("xmemory"))
    {
        for fact in summary
            .declarations
            .iter()
            .filter(|fact| fact.name.contains("_Is_default_allocator"))
        {
            eprintln!(
                "[size-probe] xmemory fact: name {:?}, pattern {:?}, bases {:?}, kind {:?}, at {}",
                fact.name, fact.pattern, fact.bases, fact.kind, fact.range.start_offset
            );
        }
        // **Every fact near the two declarations**, not only the named ones: the specialization is written at line
        // 486, right after the principal at 483, and the index holds no fact named after it. What is actually there
        // says whether it was skipped or filed under another name.
        let mut near: Vec<_> = summary
            .declarations
            .iter()
            .filter(|fact| fact.range.start_offset >= 16800 && fact.range.start_offset <= 17800)
            .collect();
        near.sort_by_key(|fact| fact.range.start_offset);
        for fact in near {
            eprintln!(
                "[size-probe] near {}: {:?}, kind {:?}, scope {:?}, pattern {:?}, bases {:?}",
                fact.range.start_offset, fact.name, fact.kind, fact.scope, fact.pattern, fact.bases
            );
        }
    }
    let vector_header_path = vector_header.clone();
    eprintln!(
        "[size-probe] `<vector>` refined: {}, `<xmemory>` refined: {}",
        session.is_refined(&vector_header_path),
        session.is_refined(&vector_header_path.with_file_name("xmemory"))
    );

    // **The member list itself**, and the base it could not resolve: the trace says `0 listed, 1 unlisted`, and the
    // spelling of that base says which generic shape is missing.
    for class in ["allocator_traits", "std::allocator_traits", "std::vector"] {
        match cpp_code_analysis::index::project::members_of(
            session.index(),
            &view.scopes,
            &view.root,
            &file,
            class,
        ) {
            Known::Yes(list) => {
                eprintln!(
                    "[size-probe] members_of({class:?}): {} listed, {} unlisted",
                    list.members.len(),
                    list.unlisted.len()
                );
                for one in &list.unlisted {
                    eprintln!(
                        "[size-probe]   unlisted: `{}` — {:?}",
                        one.spelling, one.reason
                    );
                }
                for one in list.members.iter().take(4) {
                    eprintln!("[size-probe]   member: `{}`", one.fact.name);
                }
                for name in ["size_type", "difference_type"] {
                    for one in list.members.iter().filter(|one| one.fact.name == name) {
                        eprintln!(
                            "[size-probe]   `{name}`: type {:?}, ambiguous {}, declared in {}",
                            one.fact.type_of, one.ambiguous, one.declared_in
                        );
                    }
                }
            }
            other => eprintln!("[size-probe] members_of({class:?}): {other:?}"),
        }
    }
}

#[test]
#[ignore = "a probe: needs the external project"]
fn an_enum_member_from_a_header_at_its_use() {
    // Reported with a screenshot: `case TK_ELSEIF:` in `SpaceAnalyzer.cpp` is drawn as `variable.other.unknown` —
    // no semantic token at all — while the enumerator is declared in `LuaTokenTypeDetail.h`, which that file
    // includes directly. The same spelling is also declared in `3rd/lua-5.4.3/src/llex.h`, a different enum.
    let Some(root) = project() else {
        eprintln!("[enum-probe] the project is not here");
        return;
    };
    let file = root.join("CodeFormatCore/src/Format/Analyzer/SpaceAnalyzer.cpp");
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("[enum-probe] no SpaceAnalyzer.cpp");
        return;
    };
    let Some(at) = text.find("case TK_ELSEIF:") else {
        eprintln!("[enum-probe] the case label is not there");
        return;
    };
    let cursor = at + "case ".len();

    let mut session = Session::open_with_config_file(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        None,
    );
    session.add_project_files([file.clone()]);
    session.want_cooked_reading(&file);
    for _ in 0..40 {
        session.index_everything();
        session.catch_up(&file);
        if session.pending_work() == 0 {
            break;
        }
    }

    match session.index().definitions("TK_ELSEIF", &file) {
        Known::Yes(found) => {
            eprintln!(
                "[enum-probe] `TK_ELSEIF`: {} declaration(s)",
                found.found.len()
            );
            for one in &found.found {
                eprintln!(
                    "[enum-probe]   {} kind {:?}, scope {:?}",
                    one.file.display(),
                    one.fact.kind,
                    one.fact.scope
                );
            }
        }
        other => eprintln!("[enum-probe] `TK_ELSEIF`: {other:?}"),
    }
    // **The step the colour actually takes** — `kind_of`, which answers one `IndexedKind` rather than a list.
    match session.index().kind_of("TK_ELSEIF", &file) {
        Known::Yes(kind) => eprintln!("[enum-probe] kind_of: {kind:?}"),
        other => eprintln!("[enum-probe] kind_of: {other:?}"),
    }

    // **The view the colouring pass is given** — `semantic_token/mod.rs` asks `view_of_the_file`, because a token's
    // position must be in the file's own coordinates; the cooked view's ranges are the rendering's.
    let Some(view) = session.view_of_the_file(&file) else {
        eprintln!("[enum-probe] no view");
        return;
    };
    let names = session.classified_names(&view);
    // **By text, not by offset.** A `Name`'s range and a `ReadingOffset` are different coordinate systems; the
    // question here is what the enumerator's uses are drawn as, so every classification of the spelling is printed
    // with the text it covers.
    let uses: Vec<_> = names
        .iter()
        .filter(|name| view.get_token_text(name.range) == Some("TK_ELSEIF"))
        .collect();
    eprintln!(
        "[enum-probe] {} classification(s) of the spelling:",
        uses.len()
    );
    for name in &uses {
        eprintln!(
            "[enum-probe]   at {}, kind {:?}, declaration {}, provenance {:?}",
            name.range.start_offset, name.kind, name.declaration, name.provenance
        );
    }
    let reading = view
        .reading_offset_of(FileOffset::new(cursor))
        .unwrap_or(ReadingOffset::new(cursor));
    match session.definition(&view, reading) {
        Known::Yes(found) => eprintln!(
            "[enum-probe] definition: {} kind {:?}",
            found.file.display(),
            found.fact.kind
        ),
        other => eprintln!("[enum-probe] definition: {other:?}"),
    }
}

#[test]
#[ignore = "a probe: needs the external project"]
fn what_the_session_says_about_the_reported_line() {
    let Some(root) = project() else {
        eprintln!("[probe] the project is not here");
        return;
    };
    let file = root.join("CodeFormatLib/src/LuaCodeFormat.cpp");
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("[probe] no LuaCodeFormat.cpp");
        return;
    };

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let files = sources_under(&root);
    session.add_project_files(files.iter().cloned());
    session.want_cooked_reading(&file);
    // **`catch_up`, not only `index_everything`.** The includes a file names are read by `catch_up` — that is the
    // request the server makes when a file is opened — and without it a header that is merely *included* stays
    // unread no matter how many times the queue is pumped.
    for _ in 0..40 {
        session.index_everything();
        session.catch_up(&file);
        // **The refinement pass is the one that reads a file with its closure's macros in force** — the initial pass
        // builds a plain summary, so `_STD_BEGIN` is undefined there and the declarations land at global scope.
        session.want_refinement(&file);
        if session.pending_work() == 0 {
            break;
        }
    }

    let Some(view) = session.view(&file) else {
        eprintln!("[probe] no cooked reading");
        return;
    };

    // **Is the standard library in the rendering at all?** The renderings are big, so the questions are asked of the
    // whole text rather than per node.
    let rendered = view.root.text().to_string();
    eprintln!(
        "[probe] the rendering is {} byte(s); `class vector` {}; `emplace_back` {}",
        rendered.len(),
        if rendered.contains("class vector") {
            "present"
        } else {
            "absent"
        },
        if rendered.contains("emplace_back") {
            "present"
        } else {
            "absent"
        },
    );
    // **Is the closure read at all?** The standard header's own summary is the question: if it is absent, the session
    // never read the include; if it is present, the visibility answer is what fails.
    let msvc = PathBuf::from(
        r"C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.51.36231\include\vector",
    );
    eprintln!(
        "[probe] summaries: {}; the MSVC `vector` summary: {}",
        session.index().summaries().count(),
        if session.index().summary(&msvc).is_some() {
            "present"
        } else {
            "absent"
        }
    );
    // **What the summary actually holds**, without any visibility or qualified-name logic in the way: the
    // question is whether `_STD_BEGIN` opened `std` when this header was read, and a `scope` of `Some("std")` on
    // any one fact is the only evidence that would settle it.
    if let Some(summary) = session.index().summary(&msvc) {
        let total = summary.declarations.len();
        let in_std = summary
            .declarations
            .iter()
            .filter(|fact| {
                fact.scope
                    .as_deref()
                    .is_some_and(|scope| scope == "std" || scope.starts_with("std::"))
            })
            .count();
        let namespaces: Vec<&str> = summary
            .declarations
            .iter()
            .filter(|fact| fact.kind == DeclKind::Namespace)
            .map(|fact| fact.name.as_str())
            .collect();
        eprintln!(
            "[probe] MSVC vector: {total} fact(s), {in_std} with a `std` scope, namespace fact(s): {namespaces:?}"
        );
        for fact in summary.declarations.iter().filter(|f| f.name == "vector") {
            eprintln!(
                "[probe]   fact `vector`: kind {:?}, scope {:?}, local {}, definition {}",
                fact.kind, fact.scope, fact.local, fact.definition
            );
        }
        let emplace: Vec<&DeclFact> = summary
            .declarations
            .iter()
            .filter(|f| f.name == "emplace_back")
            .collect();
        eprintln!(
            "[probe]   `emplace_back` facts: {}; scopes: {:?}; returns: {:?}",
            emplace.len(),
            emplace
                .iter()
                .map(|f| f.scope.as_deref())
                .take(4)
                .collect::<Vec<_>>(),
            emplace
                .iter()
                .map(|f| f.returns.clone())
                .collect::<Vec<_>>()
        );
        // **The evidence the design says is stored for exactly this file**: a scope opened by a body written
        // somewhere else. If this is empty, the summary never read the invocation *as a macro at all*; if it is not,
        // the reading is recorded and a consumer only has to honour it.
        eprintln!(
            "[probe]   macro_readings: {}; {:?}",
            summary.macro_readings.len(),
            summary
                .macro_readings
                .iter()
                .map(|reading| (reading.name.as_str(), reading.opens.clone()))
                .take(4)
                .collect::<Vec<_>>()
        );
        // And whether the file that writes the body holds it.
        let yvals_core = msvc.with_file_name("yvals_core.h");
        if let Some(core) = session.index().summary(&yvals_core) {
            let defines: Vec<_> = core
                .macros
                .iter()
                .filter(|fact| fact.name == "_STD_BEGIN")
                .map(|fact| (fact.name.as_str(), fact.body_range.is_some()))
                .collect();
            eprintln!(
                "[probe]   yvals_core.h: {} fact(s), `_STD_BEGIN` define(s): {:?}",
                core.declarations.len(),
                defines
            );
        } else {
            eprintln!("[probe]   yvals_core.h: no summary");
        }
    } else {
        eprintln!("[probe] MSVC vector: no summary at all");
    }
    let style = root.join("CodeFormatCore/include/CodeFormatCore/Config/LuaStyle.h");
    eprintln!(
        "[probe] LuaStyle.h summary: {}",
        if session.index().summary(&style).is_some() {
            "present"
        } else {
            "absent"
        }
    );
    for spelled in ["vector", "std::vector", "begin"] {
        match session.index().definitions(spelled, &file) {
            Known::Yes(found) => eprintln!(
                "[probe] `{spelled}`: {} declaration(s), first from {}",
                found.found.len(),
                found.found[0].file.display()
            ),
            Known::Unknown(reason) => eprintln!("[probe] `{spelled}`: unknown — {reason:?}"),
            Known::No => eprintln!("[probe] `{spelled}`: no"),
        }
    }
    // **Does the index know `std::vector` from this file?** A qualified name, asked of the same visibility the type
    // query uses.
    match session.index().definitions("std::vector", &file) {
        Known::Yes(found) => {
            for definition in &found.found {
                eprintln!(
                    "[probe] std::vector -> {} (kind {:?}, range {:?})",
                    definition.file.display(),
                    definition.fact.kind,
                    definition.fact.name_range
                );
            }
            eprintln!("[probe] std::vector: {} declaration(s)", found.found.len());
        }
        Known::Unknown(reason) => eprintln!("[probe] std::vector: unknown — {reason:?}"),
        Known::No => eprintln!("[probe] std::vector: no"),
    }
    // **`result` in `auto &result = results.emplace_back();`**
    let Some(at) = text.find("auto &result = results.emplace_back") else {
        eprintln!("[probe] the line is not there");
        return;
    };
    let cursor = at + "auto &".len();
    let reading = view
        .reading_offset_of(FileOffset::new(cursor))
        .unwrap_or(ReadingOffset::new(cursor));
    match session.type_at(&view, reading) {
        Known::Yes(found) => eprintln!("[probe] result: `{}`", found.type_of),
        Known::Unknown(reason) => eprintln!("[probe] result: unknown — {reason:?}"),
        Known::No => eprintln!("[probe] result: no"),
    }

    // **The type of every name on that line**, because the reported answer named `LuaDiagnostic` — the *parameter's*
    // element type — for a variable declared from `results`, whose element type is `LuaDiagnosticInfo`. Either the
    // arrow is right and the line is misread, or the probe's cursor is somewhere else; both are worth knowing before
    // any conclusion is drawn about `std::vector`.
    for (needle, skip, inside, label) in [
        (
            "std::vector<LuaDiagnosticInfo> results",
            "std::vector<LuaDiagnosticInfo> ".len(),
            2,
            "`results` (declaration)",
        ),
        ("results.emplace_back", 1, 1, "`results` (use)"),
        (
            "for (auto &diagnostic: diagnostics)",
            "for (auto &diagnostic: ".len(),
            2,
            "`diagnostics` (use)",
        ),
        (
            "auto &result = results.emplace_back",
            "auto &".len(),
            2,
            "`result`",
        ),
    ] {
        let Some(at) = text.find(needle) else {
            eprintln!("[probe] {label}: the text is not there");
            continue;
        };
        let cursor = at + skip + inside;
        let reading = view
            .reading_offset_of(FileOffset::new(cursor))
            .unwrap_or(ReadingOffset::new(cursor));
        let written = &text[cursor..(cursor + 20).min(text.len())];
        match session.type_at(&view, reading) {
            Known::Yes(found) => {
                eprintln!(
                    "[probe] {label} at {cursor} ({written:?}): `{}`",
                    found.type_of
                )
            }
            Known::Unknown(reason) => {
                eprintln!("[probe] {label} at {cursor} ({written:?}): unknown — {reason:?}")
            }
            Known::No => eprintln!("[probe] {label} at {cursor} ({written:?}): no"),
        }
    }

    // **What the index stored for `result` itself**, because the answer the query gave is `spelling + &`, which is
    // what a *written* return type produces. If `result`'s own fact says `std::vector<LuaDiagnostic>`, the deduction
    // never ran and the wrong spelling came from the indexing pass.
    for fact in session.index().every_fact_named("result") {
        let (path, fact) = fact;
        if path.ends_with("LuaCodeFormat.cpp") && fact.name_range.start_offset < 9100 {
            eprintln!(
                "[probe] fact `result` in {}: kind {:?}, scope {:?}, local {}, type_of {:?}, returns {:?}, at {}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                fact.kind,
                fact.scope,
                fact.local,
                fact.type_of,
                fact.returns,
                fact.name_range.start_offset
            );
        }
    }
    for name in ["GetDiagnosticResults", "MakeDiagnosticInfo"] {
        for (path, fact) in session.index().every_fact_named(name) {
            if path.ends_with("LuaCodeFormat.cpp") || path.ends_with("DiagnosticBuilder.h") {
                eprintln!(
                    "[probe] fact `{name}` in {}: kind {:?}, returns {:?}, at {}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    fact.kind,
                    fact.returns,
                    fact.name_range.start_offset
                );
            }
        }
    }

    // **`auto formattedText = f.GetFormatResult(t);`** at line 131 — the reported inlay hint, whose deduced type was
    // drawn as `basic_string<char, char_traits<char>, allocator<char>>` where the header writes `std::string`.
    //
    // **Two views, because the two callers use different ones**: the hover asks the *cooked* view, and the inlay
    // handler asks `view_of_the_file` — the file's own tokens (`inlay_hint/mod.rs:105`, and it says why: hint
    // positions must be in the file's coordinates). If the two answer differently, that is the defect.
    if let Some(at) = text.find("auto formattedText") {
        let cursor = at + "auto ".len();
        let reading = view
            .reading_offset_of(FileOffset::new(cursor))
            .unwrap_or(ReadingOffset::new(cursor));
        match session.type_at(&view, reading) {
            Known::Yes(found) => {
                eprintln!("[probe] formattedText (cooked view): `{}`", found.type_of)
            }
            Known::Unknown(reason) => {
                eprintln!("[probe] formattedText (cooked view): unknown — {reason:?}")
            }
            Known::No => eprintln!("[probe] formattedText (cooked view): no"),
        }
        match session.view_of_the_file(&file) {
            Some(plain) => {
                let reading = plain
                    .reading_offset_of(FileOffset::new(cursor))
                    .unwrap_or(ReadingOffset::new(cursor));
                match session.type_at(&plain, reading) {
                    Known::Yes(found) => {
                        eprintln!("[probe] formattedText (plain view): `{}`", found.type_of)
                    }
                    Known::Unknown(reason) => {
                        eprintln!("[probe] formattedText (plain view): unknown — {reason:?}")
                    }
                    Known::No => eprintln!("[probe] formattedText (plain view): no"),
                }
                // And the inlay's own answer, through the same call the handler makes.
                for hint in session.type_hints(&plain, cpp_parser::SourceRange::new(cursor, 1)) {
                    eprintln!(
                        "[probe] inlay hint at {}: `{}` ({:?})",
                        hint.offset, hint.label, hint.kind
                    );
                }
            }
            None => eprintln!("[probe] no plain view"),
        }
    }

    // **The facts behind it**: what `GetFormatResult` is declared to return, and what the index says about
    // `std::string` itself.
    for (path, fact) in session.index().every_fact_named("GetFormatResult") {
        if path.ends_with("RangeFormatBuilder.h") || path.ends_with("FormatBuilder.h") {
            eprintln!(
                "[probe] GetFormatResult in {}: kind {:?}, returns {:?}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                fact.kind,
                fact.returns
            );
        }
    }

    // **The qualifier of `std::vector<...> LuaCodeFormat::MakeDiagnosticInfo(…)`, line 300.**
    let Some(at) = text.find("LuaCodeFormat::MakeDiagnosticInfo") else {
        eprintln!("[probe] the definition is not there");
        return;
    };
    let names = session.classified_names(&view);
    for name in &names {
        if name.range.start_offset <= at && at < name.range.end_offset() {
            eprintln!(
                "[probe] the qualifier `LuaCodeFormat` at {at} is classified {:?} (declaration: {})",
                name.kind, name.declaration
            );
        }
    }
    let count = names
        .iter()
        .filter(|name| name.range.start_offset == at)
        .count();
    eprintln!("[probe] {} name(s) start exactly at the qualifier", count);
}

/// **The member list the reported popup was drawn from**, asked of the real project.
///
/// `declarations_in(FormatRange)` answers **6** declarations now and answered **108** with eighteen of them named
/// `EndLine` while the project's cache held a summary whose `visit_once` was lost on the way to disk (see
/// `crates/cpp_code_analysis/src/summary_codec.rs`, version 39). `member_markdown` prints one line per candidate, so
/// eighteen copies of one declaration were eighteen identical lines `std::size_t EndLine` in one hover.
#[test]
#[ignore = "a probe: needs the external project"]
fn where_the_duplicate_declarations_come_from() {
    // `declarations_in(FormatRange)` is 6 with the file added to the project and **108** once the file is *opened*
    // (`did_open`), with 18 of them named `EndLine`. This prints each copy's file and range, so the question is
    // answered by the data: one fact repeated eighteen times, or eighteen summaries each holding it.
    let Some(root) = project() else {
        eprintln!("[dup] the project is not here");
        return;
    };
    let file = root.join("CodeFormatCLib/src/CodeFormatCLib.cpp");
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("[dup] no CodeFormatCLib.cpp");
        return;
    };

    let mut session = Session::open_with_config_file(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        None,
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, &text);
    for _ in 0..10 {
        session.index_everything();
        session.catch_up(&file);
        if session.pending_work() == 0 {
            break;
        }
    }

    let found = session.index().declarations_in("FormatRange", &file);
    eprintln!(
        "[dup] declarations_in(FormatRange): {} declaration(s)",
        found.len()
    );
    for (index, declaration) in found.iter().enumerate() {
        eprintln!(
            "[dup]   {index}: {} scope {:?} name {:?} range {:?}",
            declaration.file.display(),
            declaration.fact.scope,
            declaration.fact.name,
            declaration.fact.range
        );
    }

    // The same through the member list, which is what the hover prints a line per candidate from.
    if let Some(view) = session.view_of_the_file(&file) {
        match session.members_of(&view, "FormatRange") {
            Known::Yes(list) => {
                eprintln!(
                    "[dup] members_of(FormatRange): {} member(s)",
                    list.members.len()
                );
                for member in list
                    .members
                    .iter()
                    .filter(|member| member.fact.name == "EndLine")
                {
                    eprintln!(
                        "[dup]   member {} range {:?} declared_in {:?} depth {}",
                        member.file.display(),
                        member.fact.range,
                        member.declared_in,
                        member.depth
                    );
                }
            }
            other => eprintln!("[dup] members_of(FormatRange): {other:?}"),
        }
    }
}

/// **The stream the cache serves, counted** — one copy of a `#pragma once` header, not eighteen.
///
/// `RenderedUnit::text` is what the parse sees. A cold start of `EmmyLuaCodeStyle` renders 742 907 tokens and
/// enters 206 files; the cache that lost `visit_once` rendered 919 228 tokens, entered 1 404 files, and held
/// `struct FormatRange` **18 times** — which is the whole defect the hover reported.
#[test]
#[ignore = "a probe: needs the external project"]
fn does_the_cached_rendering_hold_the_header_eighteen_times() {
    // The reading with 450 declarations appears on the first pass and only with the project's on-disk cache: a clean
    // `Session` answers 25. So the question is what the *rendering* the cache serves holds — the cook's output for
    // this translation unit, as the cache kept it. `RenderedUnit::text` is the stream the parse sees, and a header
    // that appears in it eighteen times is a header the program declares eighteen times.
    let Some(root) = project() else {
        eprintln!("[render] the project is not here");
        return;
    };
    let file = root.join("CodeFormatCLib/src/CodeFormatCLib.cpp");

    let mut session = Session::open_with_config_file(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        None,
    );
    session.add_project_files([file.clone()]);
    for _ in 0..6 {
        session.index_everything();
        session.catch_up(&file);
        if session.pending_work() == 0 {
            break;
        }
    }

    match session.render_the_unit(&file) {
        Some(unit) => {
            eprintln!(
                "[render] {} token(s) of text, {} file entr(ies), {} byte(s)",
                unit.spans.len(),
                unit.files.len(),
                unit.text.len()
            );
            let copies = unit.text.matches("struct FormatRange").count();
            let slices = unit
                .files
                .iter()
                .filter(|path| {
                    path.file_name()
                        .is_some_and(|name| name.eq_ignore_ascii_case("Types.h"))
                })
                .count();
            eprintln!(
                "[render] `struct FormatRange` appears {copies} time(s); Types.h is a file entry {slices} time(s)"
            );
        }
        None => eprintln!("[render] no rendering"),
    }
}

/// **What the stored summary says about the file's own guard, after a round trip through the cache.**
///
/// Run twice: the first run writes the entry, the second reads it back. `visit_once: true` both times is the whole
/// fix — the encoder did not write the field, so every decoded summary said `false` and the walk re-entered every
/// `#pragma once` header at every inclusion.
#[test]
#[ignore = "a probe: needs the external project"]
fn what_guard_the_cached_summary_records() {
    // The walk skips a second visit to a file only when the **summary** says how the file guards itself
    // (`Guard::PragmaOnce`, `Guard::Macro`). A summary written before that rule was understood says `Guard::None`,
    // and then a `#pragma once` header is entered at every inclusion — measured as eighteen copies of `Types.h` in
    // one program's stream, and one member of a class per copy.
    let Some(root) = project() else {
        eprintln!("[guard] the project is not here");
        return;
    };
    let file = root.join("CodeFormatCLib/src/CodeFormatCLib.cpp");
    let header = root.join("CodeFormatCore/include/CodeFormatCore/Format/Types.h");

    let mut session = Session::open_with_config_file(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
        None,
    );
    session.add_project_files([file.clone()]);
    for _ in 0..6 {
        session.index_everything();
        session.catch_up(&file);
        if session.pending_work() == 0 {
            break;
        }
    }
    match session.index().summary(&header) {
        Some(summary) => eprintln!(
            "[guard] the summary of Types.h records {:?} ({} include(s) of its own, {} declaration(s))",
            summary.guards,
            summary.includes.len(),
            summary.declarations.len()
        ),
        None => eprintln!("[guard] no summary for Types.h"),
    }
}
