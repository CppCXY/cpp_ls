//! # `workspace/executeCommand` — the one command this server offers
//!
//! **`cppls.configureCompilationDatabase`**: run the CMake configure that would write this project a
//! `compile_commands.json`, and pick it up.
//!
//! # Why it is a command and not something the server does
//!
//! Because the configure runs the project's own code. A `CMakeLists.txt` may invoke anything, so a server that
//! configured on open would be running a build script because somebody opened a file. The decision that a configure
//! would help is already made by discovery — see [`cpp_code_analysis::project::cmake`], which is where the command
//! is *chosen* and where the three reasons for choosing one are argued — and this module is where it is *offered*.
//!
//! # Why it is not shaped per client
//!
//! Because the client cannot run it either. LSP has no server-to-client request that invokes an editor command —
//! `window/showMessageRequest` and `workspace/applyEdit` are the whole of what a server may ask for, and neither
//! executes anything — so "let VS Code's CMake Tools do it" is not a design this server could implement. It is also
//! not one worth wanting: configuring the project here, with the cache the project already has, cannot disagree with
//! itself, which is the failure the project layer's own notes are about.
//!
//! # What it answers
//!
//! The protocol's result is `Option<Value>`, and this uses it for a sentence a client can show: what was run, and
//! what came of it. `null` would leave a reader who invoked the command with nothing to read, and a command whose
//! only outcome is a log line is a command nobody can tell worked.

use std::path::PathBuf;
use std::time::Duration;

use lsp_types::{
    ClientCapabilities, ExecuteCommandOptions, ExecuteCommandParams, ServerCapabilities,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::RegisterCapabilities;
use crate::context::{RequestOutcome, ServerContextSnapshot};
use crate::util::time_cancel_token;
use cpp_code_analysis::project::cmake::{ConfigureCommand, ConfigureReason};

/// The command a client invokes to have this project's compile database written.
///
/// Spelled with the server's own prefix rather than a bare verb: a client's command namespace is flat and shared
/// with every other extension, so `configure` would be a name somebody else may already have.
pub const CONFIGURE_COMPILATION_DATABASE: &str = "cppls.configureCompilationDatabase";

/// **What to run, if anything** — the whole of the session-side decision, in two parts.
///
/// Split in two so that the *decision* can be tested against an in-memory project with no client and no `cmake` on
/// the machine: [`decide`] takes the three things the decision reads rather than a session, and this is the one-line
/// adapter that finds them in one. Which provider is underneath does not matter — a test passes a `MemoryFiles`, the
/// server passes the real chain — so it is a parameter rather than a type the signature names.
pub fn decide(
    files: &impl cpp_code_analysis::FileProvider,
    root: &std::path::Path,
    discovery: &cpp_code_analysis::ProjectDiscovery,
) -> Option<ConfigureCommand> {
    cpp_code_analysis::project::cmake::configure_command(
        files,
        root,
        discovery.cmake.as_ref(),
        discovery.database.is_some(),
    )
}

/// The session's version of [`decide`]: where it reads the three things from.
pub fn proposal<F>(session: &cpp_code_analysis::Session<F>) -> Option<ConfigureCommand>
where
    F: cpp_code_analysis::FileProvider + Clone,
{
    decide(session.files().provider(), session.root(), session.discovery())
}

pub async fn on_execute_command(
    context: ServerContextSnapshot,
    params: ExecuteCommandParams,
    _cancel_token: CancellationToken,
) -> RequestOutcome<Option<Value>> {
    if params.command != CONFIGURE_COMPILATION_DATABASE {
        // **An unknown command is not this server's to answer.** A client may send commands meant for another
        // extension to the same server, and inventing a result for one of those would be answering on somebody
        // else's behalf.
        return RequestOutcome::Ready(Some(json!({
            "status": "unknown",
            "detail": format!("{} is not a command this server offers", params.command),
        })));
    }

    // The decision, and the configure, both belong on the analysis's blocking pool: reading the cache goes through
    // the session's file providers, and a configure takes as long as it takes. Neither may hold the message loop.
    let context_for_work = context.clone();
    let outcome = context
        .analysis()
        .run_blocking(move |session| {
            let proposal = proposal(session)?;
            Some(run_a_proposal(&context_for_work, session, proposal))
        })
        .await;

    match outcome {
        // **`Missing` is the honest answer for "no session"**, not a failure of the command: a client that invokes
        // this before a workspace is open has asked about a project that does not exist yet, and the protocol's
        // `null` says exactly that.
        None => RequestOutcome::Ready(None),
        Some(report) => {
            let kind = if report.started { "configured" } else { "idle" };
            context.client().show_message(lsp_types::ShowMessageParams {
                typ: if report.started {
                    lsp_types::MessageType::INFO
                } else {
                    lsp_types::MessageType::WARNING
                },
                message: report.message.clone(),
            });
            RequestOutcome::Ready(Some(json!({
                "status": kind,
                "message": report.message,
                "command": report.command,
            })))
        }
    }
}

/// What happened, as much for the response as for the log.
struct Report {
    /// Was a configure run at all? A refusal is not an error and reads differently to a reader.
    started: bool,
    message: String,
    /// The command, rendered, so a client can show exactly what was run.
    command: String,
}

/// Run a proposal and say what came of it, **reloading the session when a database appears**.
///
/// The reload is the half that makes the command worth having: `compile_commands.json` is in the watched-file list,
/// so a client that watches would pick it up on its own — but a client that does not, or one whose watch does not
/// cover a build directory, would otherwise leave the analysis reading the project it read before. Doing it here
/// costs one call and removes that from the client's list of things to get right.
fn run_a_proposal(
    context: &ServerContextSnapshot,
    session: &cpp_code_analysis::Session,
    proposal: ConfigureCommand,
) -> Report {
    let command = proposal.display();
    let outcome =
        cpp_code_analysis::project::cmake::run_configure(&cpp_code_analysis::DiskCommands, &proposal, session.files().provider());

    match outcome {
        Ok(true) => {
            // A database is on disk. `wake` is what makes a session re-read what configures it — the same call the
            // watched-file path makes when the client reports the file itself.
            context.analysis().wake();
            Report {
                started: true,
                message: format!(
                    "{command}\n\n{} — the compile database is now in effect.",
                    proposal.reason.message()
                ),
                command,
            }
        }
        Ok(false) => Report {
            started: true,
            message: format!(
                "{command}\n\nThe configure succeeded and wrote no compile database. This project does not export \
                 one however it is asked; [compile].args in .cppls.toml is the way to describe its compilation by \
                 hand."
            ),
            command,
        },
        Err(detail) => Report {
            started: true,
            message: format!("{command}\n\nThe configure failed:\n{detail}"),
            command,
        },
    }
}

/// **The explanation a client shows for a project that has a proposal and no database.**
///
/// A sentence of its own rather than the warning discovery already produces, because the two say different things:
/// discovery says what is missing, and this says there is now something to do about it — and names the command,
/// because a client that shows a message will not offer the command unless it is told which one it is.
///
/// Written and tested one step ahead of its only caller: the startup offer that would show it is not implemented
/// yet, and keeping the wording here makes that step a call rather than a decision.
#[allow(dead_code)]
pub fn offer_a_configure(proposal: &ConfigureCommand) -> String {
    format!(
        "{}.\n\nRun `{}` to write one.",
        proposal.reason.message(),
        CONFIGURE_COMPILATION_DATABASE
    )
}

/// The reason a proposal exists, for a caller that has one and wants its sentence.
///
/// `None` when the caller has already been told why — kept as a function so that "which reasons are worth
/// mentioning" stays in one place rather than being re-derived wherever a message is built.
///
/// Ahead of its caller for the same reason as [`offer_a_configure`].
#[allow(dead_code)]
pub fn reason_to_mention(reason: &ConfigureReason) -> Option<&'static str> {
    match reason {
        // **The two a cache explains are always worth mentioning**: the project really was configured, and the
        // database really is one flag away.
        ConfigureReason::TheCacheSaysOff | ConfigureReason::NoDatabaseWasWritten => Some(reason.message()),
        // **The guess is mentioned too, and worded as one.** A build directory this server picked is not something
        // to offer silently: a project configured with `-B /tmp/out` would get a second build tree, and the reader
        // is the only one who can say which is theirs.
        ConfigureReason::NeverConfigured => Some(
            "no CMake build tree was found, so this proposes a new one — change the directory if the project is \
             configured somewhere else",
        ),
        // **And the dead tree is named as one**, because the reader has a build directory and would otherwise read
        // this as the server having missed it. What they need to know is which tree is being left alone.
        ConfigureReason::TheCachedToolchainIsGone => Some(
            "the build tree that was found names a toolchain that is no longer installed, so re-configuring it would \
             fail; this proposes a new one",
        ),
    }
}

pub struct ExecuteCommandCapabilities;

impl RegisterCapabilities for ExecuteCommandCapabilities {
    fn register_capabilities(
        server_capabilities: &mut ServerCapabilities,
        _: &ClientCapabilities,
    ) {
        // **Advertising the command is what lets a client offer it** — in a command palette, a context menu, or a
        // code action. A server that answered the request without advertising it would work only for a user who
        // already knew the name.
        server_capabilities.execute_command_provider = Some(ExecuteCommandOptions {
            commands: vec![CONFIGURE_COMPILATION_DATABASE.to_string()],
            ..ExecuteCommandOptions::default()
        });
    }
}

/// The path a caller should watch to know the configure worked, for a client that watches files itself.
///
/// Ahead of its caller for the same reason as [`offer_a_configure`].
#[allow(dead_code)]
pub fn database_path(proposal: &ConfigureCommand) -> PathBuf {
    proposal.database.clone()
}

/// The button a user pressed on the offer, as a value rather than as a string compared in two places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// Run it.
    Run,
    /// Run it, and stop offering — for this session.
    RunAndDoNotAsk,
    /// Do not run it now.
    NotNow,
    /// The question was dismissed, or the client answered nothing.
    Dismissed,
}

impl Consent {
    /// The title a client shows for each button.
    ///
    /// "Don't ask again" rather than "Always": the setting is not written back — a server cannot write a client's
    /// settings — so what the button buys is silence for the rest of the session, and the offer says which setting
    /// makes it permanent. A button that promised "always" and delivered a session is a lie a user would only
    /// discover the next time they opened the project.
    pub const RUN: &'static str = "Run cmake";
    pub const RUN_AND_DO_NOT_ASK: &'static str = "Run and don't ask again";
    pub const NOT_NOW: &'static str = "Not now";

    /// Read back which one a client returned.
    pub fn from_title(title: &str) -> Consent {
        match title {
            t if t == Consent::RUN => Consent::Run,
            t if t == Consent::RUN_AND_DO_NOT_ASK => Consent::RunAndDoNotAsk,
            t if t == Consent::NOT_NOW => Consent::NotNow,
            _ => Consent::Dismissed,
        }
    }

    /// Should the configure be run?
    pub fn runs_it(self) -> bool {
        matches!(self, Consent::Run | Consent::RunAndDoNotAsk)
    }
}

/// **Offer to configure the project, once, and do what the user says.**
///
/// Called after the workspace is first indexed, because the offer is about a *project* and discovery is what knows
/// whether this one has a proposal at all. Three things decide whether a question is asked:
///
/// ```text
/// the setting       cppls.configureCmake = ask | auto | never        never -> nothing; auto -> no question
/// discovery         is there a command that would write a database?  none  -> nothing
/// already offered   has this session asked about this workspace?     yes   -> nothing
/// ```
///
/// # Why the answer is not written back
///
/// Because a server cannot write a client's settings: LSP has `workspace/configuration` for reading them and
/// nothing for changing them. So "don't ask again" is remembered for the session, and the offer names the setting
/// that makes it permanent — a worse offer than a button that just did it, and an honest one.
///
/// # Why it is a question at all
///
/// Because the configure runs the project's own code. A `CMakeLists.txt` may invoke anything, and `auto` exists for
/// a user who has decided they trust their own project; `ask` is the default for everyone else.
pub async fn offer_configure(context: &ServerContextSnapshot) {
    let (may_offer, have_asked, configured_automatically) = {
        let workspace_manager = context.workspace_manager().lock().await;
        let client_config = workspace_manager.client_config();
        (
            client_config.configure_cmake.may_offer(),
            workspace_manager.was_configure_offered(),
            client_config.configure_cmake.configures_without_asking(),
        )
    };
    if !may_offer || have_asked {
        return;
    }

    let Some((proposal, root)) = context
        .analysis()
        .with_snapshot(|session| Some((proposal(session)?, session.root().to_path_buf())))
        .flatten()
    else {
        // Nothing to propose, and nothing to remember either: a project that needs no configure today may need one
        // after an edit, and marking it "asked" would silence a question that was never asked.
        return;
    };

    if configured_automatically {
        log::info!(
            "configuring {} without asking: cppls.configureCmake is `auto`",
            root.display()
        );
        run_and_report(context, &proposal);
        return;
    }

    // Recorded before the question is shown rather than after the answer: a client that never answers must not be
    // asked again on every reload, and this record is what says so.
    context.workspace_manager().lock().await.note_configure_offered();

    let params = lsp_types::ShowMessageRequestParams {
        typ: lsp_types::MessageType::INFO,
        message: offer_a_configure(&proposal),
        actions: Some(
            [
                Consent::RUN,
                Consent::RUN_AND_DO_NOT_ASK,
                Consent::NOT_NOW,
            ]
            .into_iter()
            .map(|title| lsp_types::MessageActionItem {
                title: title.to_string(),
                properties: Default::default(),
            })
            .collect(),
        ),
    };
    let Some(answer) = context
        .client()
        .show_message_request(params, time_cancel_token(Duration::from_secs(120)))
        .await
    else {
        log::info!("the offer to configure {} was dismissed", root.display());
        return;
    };

    let consent = Consent::from_title(&answer.title);
    log::info!(
        "the offer to configure {} was answered {consent:?}",
        root.display()
    );
    if consent.runs_it() {
        run_and_report(context, &proposal);
    }
}

/// Run a proposal and report it, on the analysis's blocking pool: reading the cache goes through the session's
/// providers and a configure takes as long as it takes, so neither may hold the message loop.
///
/// Spawned rather than awaited, because its caller is the startup sequence and a configure that takes a minute must
/// not be a minute of `initialized` not returning.
fn run_and_report(context: &ServerContextSnapshot, proposal: &ConfigureCommand) {
    let context_for_work = context.clone();
    let proposal = proposal.clone();
    tokio::task::spawn(async move {
        // Two handles for one context: the closure needs one to wake the analysis after a database appears, and the
        // message afterwards needs one. Cloning rather than sharing the borrow, because the closure is `'static`.
        let context_for_message = context_for_work.clone();
        let context_for_waking = context_for_work.clone();
        let report = context_for_work
            .analysis()
            .run_blocking(move |session| {
                Some(run_a_proposal(&context_for_waking, session, proposal))
            })
            .await;
        let Some(report) = report else {
            return;
        };
        context_for_message
            .client()
            .show_message(lsp_types::ShowMessageParams {
                typ: if report.started {
                    lsp_types::MessageType::INFO
                } else {
                    lsp_types::MessageType::WARNING
                },
                message: report.message,
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpp_code_analysis::project::build::{CmakeCache, Database, DatabaseOrigin};
    use cpp_code_analysis::project::config::ConfigReport;
    use cpp_code_analysis::project::ProjectDiscovery;
    use cpp_code_analysis::MemoryFiles;
    use std::path::Path;

    /// A discovery holding exactly what a case wants it to hold.
    ///
    /// **Built by hand rather than by running discovery**, and the reason is worth keeping: `find_cmake_cache`
    /// searches for a cache with `std::fs::read_dir`, not through the `FileProvider` a session was opened over — so
    /// a cache that exists only in memory is invisible to it, however the session is built. That is the existing
    /// design and not this module's business; what is this module's business is the *decision*, and it reads three
    /// things, so the tests hand it those three.
    fn discovery_of(cache: Option<CmakeCache>, have_a_database: bool) -> ProjectDiscovery {
        ProjectDiscovery {
            root: PathBuf::from("/p"),
            config: ConfigReport::default(),
            database: have_a_database.then(|| Database {
                path: PathBuf::from("/p/compile_commands.json"),
                commands: Default::default(),
                origin: DatabaseOrigin::Root,
            }),
            cmake: cache,
            problems: Vec::new(),
        }
    }

    #[test]
    fn a_project_with_no_build_tree_is_offered_nothing_to_run() {
        // The command is advertised whether or not this project has a proposal, so the handler has to answer `null`
        // rather than inventing one. A `.cppls.toml`-only project is the case: nothing to configure, and saying so is
        // the answer.
        let files = MemoryFiles::new();
        let discovery = discovery_of(None, false);
        assert_eq!(decide(&files, Path::new("/p"), &discovery), None);
    }

    #[test]
    fn a_project_whose_cache_says_off_is_offered_the_flag() {
        let files = MemoryFiles::new();
        let cache = CmakeCache {
            path: PathBuf::from("/p/build/CMakeCache.txt"),
            home: Some(PathBuf::from("/p")),
            exports_compile_commands: false,
            ..CmakeCache::default()
        };
        let discovery = discovery_of(Some(cache), false);
        let proposal = decide(&files, Path::new("/p"), &discovery).expect("a cache that says OFF is a proposal");
        assert_eq!(proposal.reason, ConfigureReason::TheCacheSaysOff);
        assert_eq!(proposal.database, PathBuf::from("/p/build/compile_commands.json"));
        assert!(proposal.arguments[0].contains("CMAKE_EXPORT_COMPILE_COMMANDS=ON"));
    }

    #[test]
    fn a_project_that_already_has_a_database_is_offered_nothing() {
        // The whole decision, read from a discovery that has one: there is nothing to configure, and the command
        // answers `null` rather than re-running a build that already ran.
        let files = MemoryFiles::new();
        let cache = CmakeCache {
            path: PathBuf::from("/p/build/CMakeCache.txt"),
            home: Some(PathBuf::from("/p")),
            exports_compile_commands: false,
            ..CmakeCache::default()
        };
        let discovery = discovery_of(Some(cache), true);
        assert_eq!(decide(&files, Path::new("/p"), &discovery), None);
    }

    #[test]
    fn the_offer_names_the_command_a_client_would_have_to_invoke() {
        // A message that only says what is missing leaves the reader with nothing to do. Naming the command is the
        // whole difference, and it is the advertised name — a client offering it and a message naming it cannot
        // disagree, because both read this constant.
        let files = MemoryFiles::new();
        let cache = CmakeCache {
            path: PathBuf::from("/p/build/CMakeCache.txt"),
            home: Some(PathBuf::from("/p")),
            exports_compile_commands: false,
            ..CmakeCache::default()
        };
        let discovery = discovery_of(Some(cache), false);
        let proposal = decide(&files, Path::new("/p"), &discovery).unwrap();
        let offer = offer_a_configure(&proposal);
        assert!(offer.contains(CONFIGURE_COMPILATION_DATABASE), "{offer}");
        assert!(offer.contains("EXPORT_COMPILE_COMMANDS=OFF"), "{offer}");
    }

    #[test]
    fn the_guess_is_written_as_a_guess() {
        // `NeverConfigured` is the one reason that is a guess, and the wording has to carry that: a project
        // configured elsewhere would get a second build tree, and only the reader can say which is theirs.
        let wording = reason_to_mention(&ConfigureReason::NeverConfigured).unwrap();
        assert!(wording.contains("new one"), "{wording}");
        assert!(wording.contains("configured somewhere else"), "{wording}");
    }

    #[test]
    fn advertising_the_command_is_what_a_client_reads() {
        let mut capabilities = ServerCapabilities::default();
        ExecuteCommandCapabilities::register_capabilities(&mut capabilities, &ClientCapabilities::default());
        let advertised = capabilities
            .execute_command_provider
            .expect("the command is advertised");
        assert_eq!(advertised.commands, vec![CONFIGURE_COMPILATION_DATABASE.to_string()]);
    }

    #[test]
    fn the_two_cache_reasons_are_both_worth_mentioning() {
        // Both say a cache was read and the database is one flag away, which is the useful part of the message.
        assert!(reason_to_mention(&ConfigureReason::TheCacheSaysOff).is_some());
        assert!(reason_to_mention(&ConfigureReason::NoDatabaseWasWritten).is_some());
    }

    #[test]
    fn each_button_reads_back_as_the_answer_it_names() {
        // The titles are the contract with the client: a client returns the *title* of the button that was pressed,
        // so the reading and the offering cannot be allowed to drift apart.
        assert_eq!(Consent::from_title(Consent::RUN), Consent::Run);
        assert_eq!(
            Consent::from_title(Consent::RUN_AND_DO_NOT_ASK),
            Consent::RunAndDoNotAsk
        );
        assert_eq!(Consent::from_title(Consent::NOT_NOW), Consent::NotNow);
    }

    #[test]
    fn a_dismissed_question_is_not_a_yes() {
        // A client that shows a close box answers `null`, and a client that answers something unrecognised is a
        // client whose answer this server does not understand. Neither may be read as consent to run a build script,
        // which is the whole reason the two are matched by name rather than by position.
        assert_eq!(Consent::from_title("something else"), Consent::Dismissed);
        assert_eq!(Consent::from_title(""), Consent::Dismissed);
        assert!(!Consent::Dismissed.runs_it());
        assert!(!Consent::NotNow.runs_it());
        assert!(Consent::Run.runs_it());
        assert!(Consent::RunAndDoNotAsk.runs_it());
    }
}
