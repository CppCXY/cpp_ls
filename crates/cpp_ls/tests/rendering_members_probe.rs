//! **`members_of` asked with the rendering's view** — the view a hover actually passes.
//!
//! ```text
//! cargo test --release -p cpp_ls --test rendering_members_probe -- --ignored --nocapture
//! ```
//!
//! Reported: `lua_State`'s card says nothing about its layout. The card asks `Session::the_layout_of(view, "lua_State")`
//! with the view it holds, and `the_layout_of` starts with `members_of(view, "lua_State")`. `inspect members` answers
//! 25 members — but it asks with the *written* view, and the hover holds the **rendering**. This prints both.

use std::path::PathBuf;

use cpp_code_analysis::file::paths::DiskFiles;
use cpp_code_analysis::index::watch::WatchFilter;
use cpp_code_analysis::session::{OpenDocuments, Session, SessionFiles};

#[test]
#[ignore = "a probe: asks a real project's session for one class's members, twice"]
fn members_asked_with_each_view() {
    let root =
        PathBuf::from(std::env::var("CPPLS_PROJECT").unwrap_or_else(|_| {
            r"C:\Users\zc\Desktop\github\lua_rt\lua_src\lua-5.5.0".to_string()
        }));
    let file = root.join("src/ldebug.c");
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("skipped: {} is not here", file.display());
        return;
    };
    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.did_open(&file, &text);
    session.want_cooked_reading(&file);
    let _ = session.cook(&file);
    let _ = session.index_everything();

    for (label, view) in [
        ("written", session.view_of_the_file(&file)),
        ("rendering", session.view(&file)),
    ] {
        let Some(view) = view else {
            println!("[members] {label}: no view");
            continue;
        };
        for class in [
            "lua_State",
            "CallInfo",
            "StkIdRel",
            "CallInfo::u",
            "CallInfo::u::l",
            "lua_State::transferinfo",
        ] {
            match session.the_layout_of(&view, class) {
                cpp_code_analysis::Known::Yes(layout) => println!(
                    "[each] {class}: size {} unknown {:?}",
                    layout.size, layout.unknown
                ),
                other => println!("[each] {class}: {other:?}"),
            }
        }
        match session.members_of(&view, "lua_State") {
            cpp_code_analysis::Known::Yes(list) => {
                println!("[members] {label}: {} member(s)", list.members.len())
            }
            other => println!("[members] {label}: {other:?}"),
        }
        let started = std::time::Instant::now();
        let measured = session.the_layout_of(&view, "lua_State");
        println!("[layout] {label}: took {:?}", started.elapsed());
        match measured {
            cpp_code_analysis::Known::Yes(layout) => println!(
                "[layout] {label}: size {} alignment {} unknown {:?}",
                layout.size, layout.alignment, layout.unknown
            ),
            other => println!("[layout] {label}: {other:?}"),
        }
    }
}
