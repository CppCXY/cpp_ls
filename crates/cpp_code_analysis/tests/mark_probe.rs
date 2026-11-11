//! **What the analysis says the types are, on the two lines the report named** — Mark.cpp:19 and 49–51.
//!
//! The report: "我们依然无法正确的推断出 Union 的后续" at `Mark.cpp:19`, and a type that "极其夸张" at line 49
//! (`auto &event = p.GetEvents().at(Start);`) — `__gnu_cxx::__alloc_traits<…>::template rebind<MarkEvent>::other&`
//! where a reader expects `MarkEvent&`.
//!
//! What it measures, and what the numbers below mean:
//!
//! ```text
//! events[Pos]          _Tp_alloc_type&          the alias chain loses the class, so `U` cannot be resolved
//! events[Pos].U        NotDeclaredHere(…)       and neither can `.Start` behind it
//! event                the allocator rebind      `vector<T>::at` returns `reference`, and `T` `reference` is
//!                                                 `_Alloc_traits::reference` — see `operator_returns_of`'s note
//! ```
//!
//! ```text
//! $env:CPPLS_CODESTYLE_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_code_analysis --test mark_probe -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_CODESTYLE_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\EmmyLuaCodeStyle"));
    named.join("LuaParser").is_dir().then_some(named)
}

#[test]
#[ignore = "a probe: needs the external project"]
fn what_the_types_are_on_the_reported_lines() {
    let Some(root) = project() else {
        eprintln!("[probe] the project is not here");
        return;
    };
    let file = root.join("LuaParser/src/Parse/Mark.cpp");
    let text = std::fs::read_to_string(&file).expect("Mark.cpp");

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, &text);
    session.catch_up(&file);
    let _ = session.cook(&file);
    session.want_cooked_reading(&file);
    for _ in 0..12 {
        session.index_everything();
        if session.pending_work() == 0 {
            break;
        }
    }
    for _ in 0..12 {
        session.index_everything();
        if session.pending_work() == 0 {
            break;
        }
    }
    let view = session.view_of_the_file(&file).expect("a view");

    // Every member access in the file, asked the way the hover asks it.
    let mut asked = 0usize;
    let mut node = view.root.clone();
    let mut stack = vec![node.clone()];
    while let Some(node) = stack.pop() {
        for child in node.children() {
            stack.push(child);
        }
        let Some(access) = cpp_code_analysis::sema::resolve::member_access_of(&node) else {
            continue;
        };
        asked += 1;
        let object = access.object.text().to_string();
        let object = object.trim();
        let type_of = session.type_of_the_expression(&view, &access.object);
        let line = view
            .file_offset_of(cpp_code_analysis::file::coordinates::ReadingOffset::new(
                usize::from(node.text_range().start()),
            ))
            .map(|offset| text[..offset.get().min(text.len())].matches('\n').count() + 1)
            .unwrap_or(0);
        eprintln!(
            "[probe] line {line:>3} `{object}` . {}{:<12} → {}",
            if access.arrow { "->" } else { "" },
            access.member,
            match &type_of {
                Known::Yes(found) => format!("{}   (from {})", found.type_of, found.file.display()),
                Known::No => "no expression".to_string(),
                Known::Unknown(reason) => format!("unknown({reason:?})"),
            }
        );
    }
    eprintln!("[probe] {asked} member access(es)");

    for needle in ["events[Pos]", "auto &event", "event.U.Start", "p.GetEvents().at(Start)"] {
        if let Some(at) = text.find(needle) {
            let type_of = session.type_at(&view, cpp_code_analysis::file::coordinates::ReadingOffset::new(at));
            let line = text[..at].matches('\n').count() + 1;
            eprintln!(
                "[probe] type_at line {line} `{needle}` → {}",
                match &type_of {
                    Known::Yes(found) => format!("{} [{}]", found.type_of, found.expression),
                    Known::No => "no".to_string(),
                    Known::Unknown(reason) => format!("unknown({reason:?})"),
                }
            );
        }
    }

    // **The last link of the report's chain**: the member `Kind` of what `events[Pos].U.Start` is.
    match session.members_of(&view, "MarkEvent::U::Start") {
        Known::Yes(list) => {
            let names: Vec<&str> = list.members.iter().map(|one| one.fact.name.as_str()).collect();
            eprintln!(
                "[probe] MarkEvent::U::Start has {} member(s): {names:?} — `Kind` present: {}",
                names.len(),
                names.contains(&"Kind")
            );
        }
        other => eprintln!("[probe] MarkEvent::U::Start → {other:?}"),
    }
    let _ = &mut node;
}
