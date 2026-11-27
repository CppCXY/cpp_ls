//! `textDocument/inactiveRegions`: what the configuration does not compile.

use super::support::*;

/// **A client that asks for inactive regions gets them over the wire, versioned, and an empty list on close.**
///
/// The notification is clangd's `textDocument/inactiveRegions` (`docs/inactive-regions.md`), a **push**: no request
/// asks for it and a client that missed one never hears again, which is why the pump sends it from its idle drain
/// rather than from a handler. The three properties this test pins are the three the design note says a client
/// depends on:
///
/// * the capability is advertised only when the client asked (the opt-in is `initializationOptions` here, because
///   the protocol's capability list drops non-standard fields);
/// * the params carry the **document version**, so a client can drop one about text it no longer holds;
/// * `didClose` is answered with an **empty list**, because nothing else tells a client its decorations are stale.
#[test]
fn inactive_regions_are_sent_to_a_client_that_asks_for_them() {
    const DEAD_BRANCHES: &str = "\
#define ON 1
#if ON
int live;
#else
int dead;
#endif
";
    let project = Project::new("inactive-regions");
    project.write("a.cpp", DEAD_BRANCHES);
    let mut server = Server::start(project.root());
    let a_uri = uri_of(&project.root().join("a.cpp"));

    let initialized = server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            // **The opt-in**, in the one container the protocol keeps for an extension flag.
            "initializationOptions": { "inactiveRegions": true },
            "capabilities": {
                "workspace": { "configuration": true },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    assert_eq!(
        initialized["result"]["capabilities"]["inactiveRegionsProvider"],
        json!(true),
        "a client that asked is told the server sends them: {initialized}"
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": a_uri, "languageId": "cpp", "version": 1, "text": DEAD_BRANCHES }
        }),
    );

    // **The push**, which no request above asked for: the cook lands, the idle drain sees the open file, and the
    // regions go out. The deadline is the whole project's first read, which is one file here.
    let sent = server.wait_for_within(Duration::from_secs(60), |message| {
        message["method"] == json!("textDocument/inactiveRegions")
            && message["params"]["regions"]
                .as_array()
                .is_some_and(|regions| !regions.is_empty())
    });
    let params = &sent["params"];
    assert_eq!(
        params["textDocument"]["version"],
        json!(1),
        "the notification says which text it describes: {sent}"
    );
    let regions = params["regions"].as_array().expect("regions is a list");
    assert_eq!(regions.len(), 1, "one dead branch, one range: {sent}");
    // The span starts at the dead statement (line index 4) and ends where the line after it begins; the `#else` and
    // `#endif` lines are structure and stay bright.
    assert_eq!(regions[0]["start"]["line"], json!(4), "{sent}");
    assert_eq!(regions[0]["start"]["character"], json!(0), "{sent}");
    assert_eq!(regions[0]["end"]["line"], json!(5), "{sent}");
    assert_eq!(regions[0]["end"]["character"], json!(0), "{sent}");

    server.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": a_uri } }),
    );
    let cleared = server.wait_for_within(Duration::from_secs(30), |message| {
        message["method"] == json!("textDocument/inactiveRegions")
            && message["params"]["regions"]
                .as_array()
                .is_some_and(|regions| regions.is_empty())
    });
    assert_eq!(
        cleared["params"]["textDocument"]["uri"],
        json!(a_uri),
        "the empty list names the file it clears: {cleared}"
    );

    let _ = server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    let _ = server.wait_for_exit();
}

/// **A header template's `#ifdef` branch is painted too.**
///
/// `#ifdef NAME` asks a definedness question; an absent name is an exact answer, not C's undefined-is-zero
/// assumption. A CMake template such as neovim's `cmake.config/versiondef.h.in` is opened as C++, and its
/// `#ifdef ZIG_BUILD` branch is inactive when the project does not define `ZIG_BUILD`.
#[test]
fn inactive_regions_are_sent_for_a_header_template() {
    const SOURCE: &str = "\
#ifdef ZIG_BUILD
# define ZIG_BUILD_DEAD 1
int dead;
#endif
int live;
";
    let project = Project::new("inactive-regions-header-template");
    project.write("versiondef.h.in", SOURCE);
    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("versiondef.h.in"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "initializationOptions": { "inactiveRegions": true },
            "capabilities": {
                "workspace": { "configuration": true },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "cpp",
                "version": 1,
                "text": SOURCE,
            }
        }),
    );

    let sent = server.wait_for_within(Duration::from_secs(60), |message| {
        message["method"] == json!("textDocument/inactiveRegions")
            && message["params"]["regions"]
                .as_array()
                .is_some_and(|regions| !regions.is_empty())
    });
    assert_eq!(
        sent["params"]["textDocument"]["version"],
        json!(1),
        "the template's regions name its version: {sent}"
    );

    let _ = server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    let _ = server.wait_for_exit();
}

/// **One guessed branch does not suppress the exact branch beside it.**
///
/// A file with `#if ON` (exact) and `#if UNDEFINED` (the undefined-is-zero assumption) has one paintable region and
/// one withheld one. The notification carries the paintable region; sending an empty list would lose the answer the
/// configuration did know.
#[test]
fn inactive_regions_keep_the_exact_spans_when_another_branch_is_a_guess() {
    const SOURCE: &str = "\
#define ON 1
#if ON
int live;
#else
int exact_dead;
#endif
#if UNDEFINED_ANYWHERE
int guessed_dead;
#endif
";
    let project = Project::new("inactive-regions-mixed-exactness");
    project.write("a.cpp", SOURCE);
    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("a.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "initializationOptions": { "inactiveRegions": true },
            "capabilities": {
                "workspace": { "configuration": true },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": SOURCE }
        }),
    );

    let sent = server.wait_for_within(Duration::from_secs(60), |message| {
        message["method"] == json!("textDocument/inactiveRegions")
            && message["params"]["regions"]
                .as_array()
                .is_some_and(|regions| regions.len() == 1)
    });
    let regions = sent["params"]["regions"]
        .as_array()
        .expect("regions is a list");
    assert_eq!(regions.len(), 1, "the exact branch is painted: {sent}");
    assert_eq!(
        regions[0]["start"]["line"],
        json!(4),
        "the exact `#else` branch's code is the one painted: {sent}"
    );

    let _ = server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    let _ = server.wait_for_exit();
}

/// **A hover inside code this configuration does not compile says so, and a hover beside it does not.**
///
/// The dimming raises the question ("why is this grey?"), and a card that answers a name in dead code without
/// mentioning it is a confident answer about text the compiler never saw. Both gates are the notification's: the
/// reading must be exact, and the cursor must be inside a span.
#[test]
fn a_hover_inside_inactive_code_says_that_it_is_inactive() {
    const SOURCE: &str = "\
#define OFF 0
#if OFF
int dead_name = 1;
#else
int live_name = 2;
#endif
";
    let project = Project::new("inactive-hover");
    project.write("a.cpp", SOURCE);
    let mut server = Server::start(project.root());
    let a_uri = uri_of(&project.root().join("a.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": a_uri, "languageId": "cpp", "version": 1, "text": SOURCE }
        }),
    );

    // **Inside the dead branch**: the note is expected, and the first requests may arrive before the rendering that
    // knows the spans exists — so ask until it appears, which is what a client does when it asks again later.
    let dead = server.ask_until_it(
        200,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/hover",
                "params": {
                    "textDocument": { "uri": a_uri },
                    "position": { "line": 2, "character": 6 },
                },
            })
        },
        |response| {
            response["result"]["contents"]["value"]
                .as_str()
                .is_some_and(|markdown| markdown.contains("Inactive region"))
        },
    );
    let markdown = dead["result"]["contents"]["value"]
        .as_str()
        .expect("the hover is markdown");
    assert!(
        markdown.contains("dead_name"),
        "and the card it prefixes is the name's own: {markdown}"
    );

    // **Beside it, in the live branch**: no note, because nothing there is inactive.
    let live = server.request(
        400,
        "textDocument/hover",
        json!({
            "textDocument": { "uri": a_uri },
            "position": { "line": 4, "character": 6 },
        }),
    );
    let markdown = live["result"]["contents"]["value"]
        .as_str()
        .expect("the live name has a hover too");
    assert!(
        !markdown.contains("Inactive region"),
        "code the configuration compiles is not called inactive: {markdown}"
    );

    let _ = server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    let _ = server.wait_for_exit();
}

/// **Every publication describes the text its version names** — the gate the diagnostics channel already had.
///
/// `publish_all` took the version from the buffer store and the regions and text from two separate analysis snapshots,
/// so a keystroke could pair one version's `textDocument` with another version's ranges — a client that checks the
/// version (which the notification exists to let it do) would either drop a correct notification or paint a range over
/// text it never described (`docs/worklog.md` P1-3).
///
/// The edits here alternate between a text with a dead branch and one without, so the expected ranges differ by version:
/// a notification whose ranges are not the ones its own text produces is a pairing failure, whatever the interleaving.
#[test]
fn every_inactive_regions_publication_matches_the_version_it_names() {
    // **Large enough that the answer takes measurable time.** The window the gate closes is between the analysis
    // answering and the text being read; a four-line file makes it microseconds wide, and a test that cannot enter the
    // window is a pin rather than a reproducer. Measured on this fixture: the answer takes ~10 ms, which is the width.
    //
    // **Those are release numbers, and the suite is a measurement workspace** — `.cargo/config.toml` defines `cargo t`
    // as `test --release` for exactly this reason. The same single test, same fixture, same commit, measured on
    // Windows: **1.69 s** in release and **61.30 s** unoptimised, where the extra minute is the pump's program reads
    // and cook slices (~5 s each in the log), not a race. CI ran the unoptimised profile until this round, and this
    // budget was the first one that no longer fit.
    let filler: String = (0..3000)
        .map(|line| format!("int filler_{line} = {line};\n"))
        .collect();
    let with_a_dead_branch =
        format!("#define ON 1\n{filler}#if ON\nint live;\n#else\nint dead;\n#endif\n");
    let without = filler;

    let project = Project::new("inactive-regions-versions");
    project.write("a.cpp", &with_a_dead_branch);
    let mut server = Server::start(project.root());
    let a_uri = uri_of(&project.root().join("a.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "initializationOptions": { "inactiveRegions": true },
            "capabilities": {
                "workspace": { "configuration": true },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": a_uri, "languageId": "cpp", "version": 1, "text": with_a_dead_branch.clone() }
        }),
    );

    // **Each edit is followed by the publication it deserves**, and each answer is checked against its own text: a
    // dead branch is non-empty regions, and its absence is empty ones.
    let mut version = 1;
    for (index, text) in [
        without.as_str(),
        with_a_dead_branch.as_str(),
        without.as_str(),
        with_a_dead_branch.as_str(),
    ]
    .into_iter()
    .enumerate()
    {
        version += 1;
        server.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": a_uri, "version": version },
                "contentChanges": [{ "text": text }],
            }),
        );

        let published = server.wait_for_within(Duration::from_secs(60), |message| {
            message["method"] == json!("textDocument/inactiveRegions")
                && message["params"]["textDocument"]["version"] == json!(version)
        });
        let regions = published["params"]["regions"]
            .as_array()
            .unwrap_or_else(|| panic!("regions is a list: {published}"));
        let expected_a_dead_branch = index % 2 == 1;
        assert_eq!(
            !regions.is_empty(),
            expected_a_dead_branch,
            "version {version} names `{text:?}`, whose dead branch is {expected_a_dead_branch}: {published}"
        );
    }

    let _ = server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
    let _ = server.wait_for_exit();
}
