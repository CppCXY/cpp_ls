//! **What the hover answers from the *rendering*** — the half the editor does not reach when the file's own text
//! answers first.
//!
//! ```text
//! $env:CPPLS_PROJECT = "C:\…\lua_src\lua-5.5.0"
//! $env:CPPLS_FILE = "src/ldebug.c"
//! $env:CPPLS_NEEDLE = "ci_func(ci)->p"
//! cargo test --release -p cpp_ls --test rendering_hover_probe -- --ignored --nocapture
//! ```
//!
//! Reported: hovering `p` in `return luaG_getfuncline(ci_func(ci)->p, currentpc(ci));` answers "the type of
//! `ci_func(ci)` is not known here". `ci_func` is a macro (`ldebug.h:18`), and the file's own text cannot type it —
//! the rendering can, because the invocation is already the cast it stands for there.

use std::path::PathBuf;

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::file::paths::DiskFiles;
use cpp_code_analysis::index::watch::WatchFilter;
use cpp_code_analysis::session::{OpenDocuments, Session, SessionFiles};

#[test]
#[ignore = "a probe: reads a real project and prints both readings' answers at one position"]
fn what_each_reading_says_at_a_position() {
    let root =
        PathBuf::from(std::env::var("CPPLS_PROJECT").unwrap_or_else(|_| {
            r"C:\Users\zc\Desktop\github\lua_rt\lua_src\lua-5.5.0".to_string()
        }));
    let relative = std::env::var("CPPLS_FILE").unwrap_or_else(|_| "src/ldebug.c".to_string());
    let needle = std::env::var("CPPLS_NEEDLE").unwrap_or_else(|_| "ci_func(ci)->p".to_string());
    let file = root.join(&relative);
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

    let Some(at) = text.find(&needle) else {
        eprintln!("skipped: `{needle}` is not in the file");
        return;
    };
    let at = at + needle.len() - 1;

    let written = session.view_of_the_file(&file).expect("the written text");
    let answer = cpp_ls::handlers::hover::hover(
        &session,
        &written,
        ReadingOffset::new(at),
        FileOffset::new(at),
    );
    println!(
        "[written] {}",
        answer
            .map(|hover| match hover.contents {
                lsp_types::HoverContents::Markup(markup) => markup.value,
                other => format!("{other:?}"),
            })
            .unwrap_or_else(|| "none".to_string())
    );

    let mapped = session.the_rendering_offset(&file, at);
    println!("[mapped] {mapped:?} for the file's offset {at}");
    // **Where the mapping jumps**: the offsets around the token, so a mapping that lands in the expansion rather than
    // on the token after it shows as a step.
    for delta in 0..12 {
        let probe = at.saturating_sub(6) + delta;
        let mapped = session.the_rendering_offset(&file, probe);
        let spelling = text
            .get(probe..probe + 1)
            .map(|byte| byte.escape_debug().to_string())
            .unwrap_or_default();
        println!("[map step] file {probe} {spelling:?} -> {mapped:?}");
    }
    let Some(mapped) = mapped else {
        return;
    };
    let rendered = session.view(&file).expect("the rendering");
    // **What stands at the mapped offset**, and which offsets hold the written `->p`: a mapping that lands inside the
    // expansion instead of on the token after it is the thing this prints.
    let around = rendered.source.len().min(mapped + 60);
    println!(
        "[around] {:?}",
        &rendered.source[mapped.saturating_sub(40)..around]
    );
    if let Some(found) = rendered.source.find("->p") {
        println!("[arrow] `->p` at {}", found);
        let end = rendered.source.len().min(found + 40);
        println!(
            "[arrow around] {:?}",
            &rendered.source[found.saturating_sub(120)..end]
        );
    }
    let answer = cpp_ls::handlers::hover::hover(
        &session,
        &rendered,
        ReadingOffset::new(mapped),
        FileOffset::new(mapped),
    );
    println!(
        "[rendering] {}",
        answer
            .map(|hover| match hover.contents {
                lsp_types::HoverContents::Markup(markup) => markup.value,
                other => format!("{other:?}"),
            })
            .unwrap_or_else(|| "none".to_string())
    );
}

/// **Every rendering offset near the written token, asked separately** — the question the mapping has to answer, asked
/// without it: if the rendering can type `->p` at some offset, the defect is the mapping; if it cannot anywhere, the
/// rendering's reading is what fails.
#[test]
#[ignore = "a probe: asks the rendering at each offset of a region"]
fn what_the_rendering_answers_offset_by_offset() {
    let root =
        PathBuf::from(std::env::var("CPPLS_PROJECT").unwrap_or_else(|_| {
            r"C:\Users\zc\Desktop\github\lua_rt\lua_src\lua-5.5.0".to_string()
        }));
    let relative = std::env::var("CPPLS_FILE").unwrap_or_else(|_| "src/ldebug.c".to_string());
    // The written position is documentation here: this probe asks every offset of the rendering itself.

    let file = root.join(&relative);
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
    let Some(rendered) = session.view(&file) else {
        eprintln!("no rendering");
        return;
    };
    let Some(arrow) = rendered.source.find("->p") else {
        eprintln!("`->p` is not in the rendering");
        return;
    };
    for offset in arrow..arrow + 4 {
        let answer = cpp_ls::handlers::hover::hover(
            &session,
            &rendered,
            ReadingOffset::new(offset),
            FileOffset::new(offset),
        );
        let text = answer
            .map(|hover| match hover.contents {
                lsp_types::HoverContents::Markup(markup) => markup.value.replace('\n', " "),
                other => format!("{other:?}"),
            })
            .unwrap_or_else(|| "none".to_string());
        let around = rendered
            .source
            .get(offset..rendered.source.len().min(offset + 12))
            .unwrap_or("");
        println!(
            "[offset {offset}] {around:?} -> {}",
            &text[..text.len().min(140)]
        );
    }
}
