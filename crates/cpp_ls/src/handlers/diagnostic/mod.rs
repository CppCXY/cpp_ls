//! # Diagnostics — what the analysis reported about one file
//!
//! The C++ analysis reports rather than fails: a file being typed at parses into a tree with errors attached, and
//! those errors **are** the diagnostics. There is no separate "check" pass, because the parse is the check this layer
//! has today — semantic diagnostics need the index to be complete, which is the same boundary [`Session::pending`]
//! marks, and carries that work.
//!
//! ```text
//! path ──> session.diagnostics(path) ──> FileDiagnostic { start, end, message }
//!                                             │
//!                the file's own text, or the cooked reading when the index holds one
//!                                             ▼
//!                 VfsFile's line index ──> Diagnostic { range, severity, message }
//! ```
//!
//! **Which reading answers is not this layer's decision** — it is [`Session::diagnostics`]'s, and the reason it has
//! one is the whole point of the cooked reading: the file's own text contains macros the parser can only guess at and
//! branches the preprocessor removes, so errors read there include ones a compiler never sees. When the index holds a
//! cooked reading, that is the text the compiler parses, and its errors are placed back in the file by the same map
//! every declaration's range goes through.
//!
//! # One channel, and it is the client's
//!
//! Diagnostics are **pulled**: a client asks with `textDocument/diagnostic` and [`diagnose_file`] answers. There was
//! a push half beside it — a service that published `textDocument/publishDiagnostics` after an edit settled — and it
//! is deleted. Two channels for one answer is how a screen ends up holding a diagnostic about text that is gone: a
//! published answer cannot be recalled, while a pulled one is computed when it is asked for, from the index as it
//! stands at that moment.
//!
//! So the timing questions this layer used to have — how long to wait after a keystroke, what to do with an answer
//! that has been overtaken, whether a finishing pass may remove another's bookkeeping — do not have answers here any
//! more. They have no place to be asked.
mod document_diagnostic;

use std::path::Path;

use cpp_code_analysis::{DiskFiles, Session};
use lsp_types::{
    ClientCapabilities, Diagnostic, DiagnosticOptions, DiagnosticServerCapabilities,
    DiagnosticSeverity, DiagnosticTag, ServerCapabilities, Uri,
};

use super::RegisterCapabilities;
use crate::util::{path_to_uri, position_in_file};
pub use document_diagnostic::on_pull_document_diagnostic;
use log::debug;

/// The diagnostics for one file, and the URI to publish them under.
///
/// `None` when the file cannot be read or its path is not a URI this server can spell: there is nothing to diagnose
/// and nothing to publish under, which is not the same as a file with no diagnostics (`Some(vec![])` — the answer
/// that *clears* a client's list, and the reason a close must come through here too).
///
/// # Which reading answers is the analysis's decision
///
/// [`Session::diagnostics`] picks it: the file's **cooked** reading when the index holds one — the text a compiler
/// parses, where a declaration written by a macro is not a guess and a branch nobody takes contributes nothing —
/// and the file's own text otherwise. This layer's job is the wire format: offsets become positions through the
/// VFS's line index (not through a parse of the file, which the layered answer no longer needs) and the message
/// becomes a [`Diagnostic`].
pub fn diagnose_file(session: &Session<DiskFiles>, path: &Path) -> Option<(Uri, Vec<Diagnostic>)> {
    let uri = path_to_uri(path)?;
    // Held by the VFS: this is the text — the buffer when the file is open, the disk otherwise — the offsets below
    // are offsets into, and the index that turns them into a line and a column.
    let held = session.files().held(path)?;

    let found = session.diagnostics(path)?;
    // **What the answer is made of**, for the case that matters: an answer that is empty or absent right after an
    // edit. The numbers say which channel was empty and whether the reading was the cooked one, and without them a
    // pull that answers `[]` to a file with a missing `;` is indistinguishable from a file that parses.
    debug!(
        "diagnose {}: {} byte(s) held, reading {:?}, {} error(s), {} unplaced, {} note(s), {} check(s)",
        path.display(),
        held.text.len(),
        found.reading,
        found.errors.len(),
        found.unplaced,
        found.notes.len(),
        found.checks.len(),
    );
    if found.unplaced > 0 {
        // **Said out loud rather than dropped.** A reading whose errors land in another file's macro bodies cannot
        // show them here, and a client that shows an empty list must not be read as "this file is clean" when the
        // parser had something to say and the answer is "not here".
        debug!(
            "{}: {} error(s) of the cooked reading are not written in this file and are not published",
            path.display(),
            found.unplaced
        );
    }

    let diagnostics = found
        .errors
        .iter()
        .map(|error| {
            // The parser's range, both ends placed by the same index — so a file with a hundred errors is a hundred
            // binary searches rather than a hundred scans of the file.
            let range = match (
                position_in_file(held, error.start),
                position_in_file(held, error.end),
            ) {
                (Some(start), Some(end)) => lsp_types::Range::new(start, end),
                // An offset the text does not contain: point at the top of the file rather than dropping the
                // error. The message is the part a user needs, and losing it would hide a real parse failure
                // because of a range this layer could not map.
                _ => lsp_types::Range::default(),
            };

            Diagnostic {
                range,
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("cpp_ls".to_string()),
                // **The rule, in the field the protocol keeps for it.** A client shows this beside the message and
                // an editor lets the reader turn one rule off by it — which is why it is the analysis's enum rather
                // than anything this layer invents: `cpp_code_analysis::sema::check::Diagnostic::code` is the one
                // spelling a settings file and this wire format both agree on.
                code: Some(lsp_types::NumberOrString::String(
                    error.code.name().to_string(),
                )),
                message: error.message.clone(),
                ..Diagnostic::default()
            }
        })
        .chain(module_notes(&found.notes, held))
        // **And what the checks found**, which is the third channel of `FileDiagnostics` — see `check_findings`.
        //
        // **Withheld while *this file's* reading is incomplete**, and it is the only channel that has to be. A parse
        // error is about the text the reader wrote and is the same answer at any index state; a *check* reads the
        // index. `undefined-field` asks which members a class has, and a class whose header has not been read yet
        // answers "none" — reported from a lightweight project as
        //
        // ```text
        // anotherVec has no member `push_back`      cppls(undefined-field)
        // ```
        //
        // on the first report after a window opened, while hovering that same name answered **three declarations**
        // and editing any character made the finding go away. The member was in the index; the report was computed
        // before the index had finished, and a check turns every hole in it into a claim about correct code.
        //
        // What is withheld is now `checks_may_be_trusted`'s question and nothing coarser: the answer must not
        // disappear and come back, but it also must not wait for files this one cannot see.
        .chain(if checks_may_be_trusted(session, &path) {
            check_findings(&found.checks, held).collect::<Vec<Diagnostic>>()
        } else {
            Vec::new()
        })
        .collect();

    Some((uri, diagnostics))
}

/// **May a check's finding be published yet?** — the gate described where it is used.
///
/// A check reads the **index**, and an index still being built answers *absence* as if it were a fact:
/// `undefined-field` asks which members a class has, and a class whose header has not been read yet answers "none".
/// Reported against a lightweight project, on the first report after a window opened:
///
/// ```text
/// anotherVec has no member `push_back`      cppls(undefined-field)
/// ```
///
/// while hovering that same name answered **three declarations**, and editing any character made the finding go
/// away. The member was in the index; the report was computed before the index had finished.
///
/// # What the question is, and what it used to be
///
/// `Session::is_closure_ready` — the file's summary exists and every file it **unconditionally includes** has one —
/// plus "nothing about this file or its closure is queued for the second pass" (`its_closure_is_owed`). That is
/// exactly the set of facts a check about this file reads, which is why it is the right question rather than a
/// conservative one.
///
/// It used to be `pending() == 0 && pending_cooking() == 0 && !its_closure_is_owed(path)` — the **project's** queues.
/// On a real project those are non-zero for as long as it takes to read the whole project, so every check was
/// withheld for that whole window, and a pull that arrived during it carried parse errors only. The handler used to
/// paper over the difference by repeating the previous report; the repetition is gone (see
/// `document_diagnostic::the_report`) and this is the half that makes its absence affordable.
///
/// A named function rather than the expression at the call site, because it is the *decision* and it should be
/// readable and testable as one — the session state that drives it is not something a fixture can hold still.
pub fn checks_may_be_trusted(session: &Session<DiskFiles>, path: &std::path::Path) -> bool {
    session.is_closure_ready(path) && !session.its_closure_is_owed(path)
}

/// **What the semantic checks found** — one diagnostic per finding, with the rule's own code and severity.
///
/// The third channel, and the one that was missing: `FileDiagnostics::checks` has been filled by the analysis since
/// the layer was written and **nothing published it**, so every check ran on every keystroke and no user ever saw a
/// finding. This is the wire half of that.
///
/// `severity` comes from the **code** rather than from the finding — see [`DiagnosticCode::severity`] — because the
/// kind of claim belongs to the rule: "this name is not used" and "this file does not parse" are different things
/// to say, and the difference does not vary between two findings of one rule.
///
/// A range the file cannot place is **dropped** rather than pointed at the top of the file, which is the opposite
/// of what the errors above do: a parse error the reader cannot locate is still worth its message, while a check's
/// finding is *about a place* and one that cannot be placed is one this layer has nothing to say about.
fn check_findings<'a>(
    findings: &'a [cpp_code_analysis::Finding],
    held: &'a cpp_code_analysis::VfsFile,
) -> impl Iterator<Item = Diagnostic> + 'a {
    findings.iter().filter_map(move |finding| {
        let start = position_in_file(held, finding.range.start_offset)?;
        let end = position_in_file(held, finding.range.end_offset()).unwrap_or(start);
        Some(Diagnostic {
            range: lsp_types::Range::new(start, end),
            severity: Some(severity_of(finding.code)),
            tags: tags_of(finding.code),
            source: Some("cpp_ls".to_string()),
            code: Some(lsp_types::NumberOrString::String(
                finding.code.name().to_string(),
            )),
            message: finding.message.clone(),
            ..Diagnostic::default()
        })
    })
}

/// The protocol's severity for one of this analysis's codes.
fn severity_of(code: cpp_code_analysis::DiagnosticCode) -> DiagnosticSeverity {
    match code.severity() {
        cpp_code_analysis::sema::check::RuleSeverity::Error => DiagnosticSeverity::ERROR,
        cpp_code_analysis::sema::check::RuleSeverity::Warning => DiagnosticSeverity::WARNING,
        cpp_code_analysis::sema::check::RuleSeverity::Hint => DiagnosticSeverity::HINT,
        cpp_code_analysis::sema::check::RuleSeverity::Information => {
            DiagnosticSeverity::INFORMATION
        }
    }
}

/// **What a client may grey out**, from the rule's own tags — see `DiagnosticCode::tags`.
///
/// `None` rather than an empty list when there are none: the protocol distinguishes "no tags" from "an empty set
/// of tags", and a client that reads the second as the first is not a client this layer should be guessing about.
fn tags_of(code: cpp_code_analysis::DiagnosticCode) -> Option<Vec<DiagnosticTag>> {
    let tags: Vec<DiagnosticTag> = code
        .tags()
        .iter()
        .map(|tag| match tag {
            cpp_code_analysis::sema::check::RuleTag::Unnecessary => DiagnosticTag::UNNECESSARY,
        })
        .collect();
    (!tags.is_empty()).then_some(tags)
}
/// **The `import`s whose module this project does not contain** — as `INFORMATION` on the line that wrote them.
///
/// `import std;` in a project whose compiler ships the standard module's *source* is not a mistake, and a reader who
/// gets nothing for `std::` deserves to be told where that file is rather than to be left with a silent empty answer
/// (see [`Session::notes_about_the_modules`], which is where the sentence is put together, and
/// `FileDiagnostics::notes`, which is where the answer arrives — out of the same parse the errors came from, so a
/// consumer that asked separately would pay for the file's tree twice).
///
/// **Not an error, and deliberately not a warning either**: nothing here is wrong — the import will compile once the
/// build names the module — and a client that shows warnings as problems would be reporting the project's build
/// system rather than the file. They are appended **after** the errors so that a reader sees the parse failures
/// first.
fn module_notes<'a>(
    notes: &'a [cpp_code_analysis::ModuleNote],
    held: &'a cpp_code_analysis::VfsFile,
) -> impl Iterator<Item = Diagnostic> + 'a {
    notes.iter().map(move |note| {
        // `VfsFile` carries the line index of the text the offsets are into, which is the same index the errors
        // above went through: no second parse and no scan of the file.
        let range = match (
            position_in_file(held, note.start),
            position_in_file(held, note.end),
        ) {
            (Some(start), Some(end)) => lsp_types::Range::new(start, end),
            _ => lsp_types::Range::default(),
        };

        Diagnostic {
            range,
            severity: Some(DiagnosticSeverity::INFORMATION),
            source: Some("cpp_ls".to_string()),
            // A note is a diagnostic a client shows, so it has a code like everything else — see
            // `cpp_code_analysis::sema::check::DiagnosticCode::ModuleNote`.
            code: Some(lsp_types::NumberOrString::String(
                cpp_code_analysis::sema::check::DiagnosticCode::ModuleNote
                    .name()
                    .to_string(),
            )),
            message: note.message.clone(),
            ..Diagnostic::default()
        }
    })
}

pub struct DiagnosticCapabilities;

impl RegisterCapabilities for DiagnosticCapabilities {
    fn register_capabilities(server_capabilities: &mut ServerCapabilities, _: &ClientCapabilities) {
        server_capabilities.diagnostic_provider =
            Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
                identifier: Some("cpp_ls".to_string()),
                // **`true`, and it became true when the cooked reading did.** The answer for an open file is now
                // read off a rendering, and a rendering is made of the macros its translation unit's headers define:
                // editing one of them changes what this file compiles to, so a client that keeps a file's
                // diagnostics must re-ask when another file changes. Saying `false` would be claiming a locality the
                // answer no longer has — and the day it was `false` the claim was true, because the parse of one file
                // really was the whole answer.
                inter_file_dependencies: true,
                // **Not offered, and now for a simpler reason than before.** `workspace/diagnostic` would be the same
                // per-file answer over every indexed file, and a client does not need the server to walk the project
                // to learn about the files it is showing. It used to say this while *also* pushing a workspace pass
                // from the settle path — offering `false` and sending anyway, which is the contradiction the push
                // path's removal resolved.
                workspace_diagnostics: false,
                ..Default::default()
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{checks_may_be_trusted, diagnose_file};
    use cpp_code_analysis::{
        CompilerConfig, DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };
    use lsp_types::DiagnosticSeverity;
    use std::path::PathBuf;

    /// A project whose one file imports a module nothing in it declares — `import std;` on a machine where the
    /// standard module is the compiler's business, without naming `std` (a test that used it would start reading
    /// the real standard library on any machine that ships one).
    ///
    /// A **temporary directory** rather than `MemoryFiles`, and for a reason that is about the signature under test:
    /// [`diagnose_file`] takes a `Session<DiskFiles>` — it is the handler the server calls, and the server's session
    /// is over the disk — so a test that used an in-memory provider would be testing a different function.
    fn a_project_importing_a_module_nobody_declares(text: &str) -> (Session<DiskFiles>, PathBuf) {
        a_project_in_a_scratch_directory_named("cppls-diagnostic", text)
    }

    /// The same fixture under a **name of its own**, because two tests sharing one scratch directory race: this
    /// workspace's tests run in parallel, and a second test that rewrites `main.cpp` while the first is reading it is
    /// a failure in whichever one loses. Measured while adding the gate's test: it wrote its text over the module
    /// test's file and that test reported three diagnostics instead of one.
    fn a_project_in_a_scratch_directory_named(
        name: &str,
        text: &str,
    ) -> (Session<DiskFiles>, PathBuf) {
        let root = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch directory");

        let file = root.join("main.cpp");
        std::fs::write(&file, text).expect("the fixture");

        let providers = SessionFiles::new(OpenDocuments::new(), DiskFiles);
        let mut session = Session::with_config(
            root.clone(),
            providers,
            WatchFilter::new(&root),
            CompilerConfig::default(),
        );

        session.add_project_files([file.clone()]);
        // **The rendering is asked for before the pump**, because Session::view answers None until it has been:
        // this fixture indexed the file and then asked diagnose_file for a view, which is the same missing step
        // the rest of this workspace's tests had. Measured: 	he file is held panicked here.

        session.want_cooked_reading(&file);
        session.index_everything();

        (session, file)
    }

    /// **The checks are withheld while the index is still filling, and the errors are not.**
    ///
    /// Reported against a lightweight project, on the first diagnostics a window asked for:
    ///
    /// ```text
    /// anotherVec has no member `push_back`      cppls(undefined-field)
    /// ```
    ///
    /// while hovering that same name answered three declarations, and editing any character made the finding go
    /// away. The member was in the index; the report was computed before the index had finished, and a check reads
    /// *absence* out of that index as a fact.
    ///
    /// The split is the whole point: **a parse error is about the text and is the same answer at any index state**,
    /// so only the checks wait. Withholding both is what the first attempt did, and it hid an unclosed brace until
    /// the project finished loading — `a_pulling_client_gets_a_report_and_not_null` caught that.
    ///
    /// # What the gate asks, and what it asked
    ///
    /// `is_closure_ready` (this file and the headers it unconditionally includes are read) plus `!its_closure_is_owed`
    /// (nothing about them waits for the second pass) — the facts a check about *this* file reads, and no more. It
    /// used to be the project's queues (`pending()`, `pending_cooking()`), which on a real project are non-zero for
    /// as long as reading the project takes; the handler covered the difference by repeating its previous report,
    /// and that repetition is what showed a corrected file its own old findings. Both are gone, so the fixture here
    /// asserts the narrower question directly: the file's closure is read, so its checks are trusted — while its
    /// errors are published either way.
    ///
    /// It does not drive the *mid-index* state: that is a race against the pump, not a value a fixture can hold
    /// still, which is why the decision is a named function with a claim of its own.
    #[test]
    fn the_checks_are_gated_on_this_file_s_closure_and_the_errors_are_not() {
        let (session, path) = a_project_in_a_scratch_directory_named(
            "cppls-gate",
            // A `;` is missing, so this text really does not parse — and the second line shadows a standard name,
            // which is what the checks are for.
            "namespace std { template <class T> class vector; }\n\
             void f(std::vector<int>& anotherVec) { anotherVec.push_back(1231) }\n\
             int g() { return 1 }\n",
        );

        // **The closure, not the project.** The fixture's file includes nothing, so its closure is itself — read,
        // and not owed a re-read. A gate that also waited for `pending_work()` would answer the same thing *here*
        // and a different thing on a real project, which is exactly the difference that made findings vanish.
        assert!(
            session.is_closure_ready(&path),
            "the fixture's own file is read, so its closure is ready"
        );
        assert!(
            checks_may_be_trusted(&session, &path),
            "…so its checks are trusted"
        );

        let (_, found) = diagnose_file(&session, &path).expect("the file is held");
        assert!(
            found.iter().any(|diagnostic| matches!(
                diagnostic.code.as_ref(),
                Some(lsp_types::NumberOrString::String(name)) if name == "syntax-error"
            )),
            "a parse error is about the text and is never withheld: {found:?}"
        );
    }

    /// **An unreadable module reaches a client as `INFORMATION` on the `import` line.**
    ///
    /// The analysis layer answers with offsets and a sentence; what a *client* does with them is this layer's: the
    /// severity (nothing is wrong with the file), the range (the line the reader wrote, not the top of the file),
    /// and the source. Each of those is a decision made here and nowhere else, so each is asserted here.
    #[test]
    fn a_module_nothing_declares_becomes_an_information_diagnostic_on_its_line() {
        let text = "import mylib;\n\nint main() { return 0; }\n";
        let (session, path) = a_project_importing_a_module_nobody_declares(text);

        let (_, diagnostics) = diagnose_file(&session, &path).expect("the file is held");

        assert_eq!(
            diagnostics.len(),
            1,
            "one import, one diagnostic: {diagnostics:?}"
        );
        let note = &diagnostics[0];

        assert_eq!(
            note.severity,
            Some(DiagnosticSeverity::INFORMATION),
            "the file is not wrong — the module is outside the project"
        );
        assert_eq!(
            note.range.start.line, 0,
            "on the line that wrote the import"
        );
        assert_eq!(note.range.start.character, 0, "and at the declaration");
        assert!(
            note.message.contains("mylib"),
            "the message names the module: {}",
            note.message
        );

        // A file with no imports gets no notes at all, which is the ordinary case and the one a regression would
        // turn into a diagnostic on every file in a project.
        let (session, path) =
            a_project_importing_a_module_nobody_declares("int main() { return 0; }\n");
        let (_, diagnostics) = diagnose_file(&session, &path).expect("the file is held");
        assert!(
            diagnostics.is_empty(),
            "a file that imports nothing has nothing to be told: {diagnostics:?}"
        );

        // The scratch tree goes with the test that made it: a test that leaves a directory in the system's temp
        // directory is a test whose second run starts from somebody else's leftovers.
        let _ = std::fs::remove_dir_all(path.parent().expect("the scratch root"));
    }
}
