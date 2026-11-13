//! **Why a name declared in a generated header is not found** — the four `undefined-name` findings the neovim
//! census reports (`server_teardown`, `signal_teardown`, `get_globvar_dict`, `tv_list_alloc`).
//!
//! ```text
//! cargo test --release -p cpp_code_analysis --test closure_symbol_probe -- --ignored --nocapture
//! ```
//!
//! Those four have no declaration in `src/` at all; their prototypes are in neovim's **generated** headers under
//! `build/include/…/*.h.generated.h`, which the sources include through ordinary `#include` lines. `inspect facts`
//! shows the fact exists, and `inspect define` from the call answers `NotDeclaredHere` — so the question is which
//! layer loses the edge: the summary (is the generated header indexed at all), the closure walk, or the lookup.

use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

#[test]
#[ignore = "a probe: needs the neovim checkout"]
fn why_a_generated_headers_declaration_is_not_found() {
    let root = std::env::var_os("CPPLS_NEOVIM")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Users\zc\Desktop\neovim_test\neovim"));
    let main = root.join("src/nvim/main.c");
    let generated = root.join("build/include/msgpack_rpc/server.h.generated.h");

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([main.clone()]);
    session.want_cooked_reading(&main);
    // **The order the pump uses, and the reason it matters.** `want_the_closure_cooked` reads the file's
    // *summary* to find its includes, so calling it before the file has been read queues nothing — the server
    // calls it once per idle drain and heals on the next one, while a probe that calls it once does not.
    for _ in 0..8 {
        session.index_everything();
        session.catch_up(&main);
        if session.pending_work() == 0 {
            break;
        }
    }
    session.want_the_closure_cooked(&main);
    for _ in 0..8 {
        session.index_everything();
        session.catch_up(&main);
        if session.pending_work() == 0 {
            break;
        }
    }

    eprintln!(
        "[closure] server.h.generated.h summary read: {}",
        session.index().summary(&generated).is_some()
    );
    let closure = session.index().include_closure(&main);
    eprintln!(
        "[closure] the closure holds the generated header: {} ({} file(s))",
        closure
            .iter()
            .any(|held| held.to_lowercase().ends_with("server.h.generated.h")),
        closure.len()
    );
    eprintln!(
        "[closure] definition server_teardown -> {:?}",
        session.index().definitions("server_teardown", &main)
    );
    let found = session.index().summary(&generated);
    eprintln!(
        "[closure] the generated summary's own facts: {:?}",
        found.map(|summary| {
            summary
                .declarations
                .iter()
                .find(|fact| fact.name == "server_teardown")
                .map(|fact| (fact.kind, fact.scope.clone(), fact.guard))
        })
    );
}
