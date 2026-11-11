//! # `inspect` — one question about one project, from the command line
//!
//! ```text
//! cargo run -q -p inspect -- type   --project C:\…\test_cpp --file src/templates.cpp --needle "Pair<int, int>{1, 2}.sum" --shift 20
//! cargo run -q -p inspect -- hover  --project C:\…\test_cpp --file src/smart_pointers.cpp --needle "node->twin"
//! cargo run -q -p inspect -- members --project C:\…\test_cpp --file src/smart_pointers.cpp --class "std::shared_ptr<int>"
//! cargo run -q -p inspect -- hover  --fixture a.cpp='struct Widget { int size() const { return 1; } };' --needle size
//! ```
//!
//! The questions are in [`questions`]; the position a question is asked at is [`place::Place`]; the session it is asked
//! of is [`project`]. `tools/inspect/README.md` has the list, what each one answers, and the recipes this session used.
//!
//! # Why a binary and not a test
//!
//! Because a question costs a **run** instead of a **build**. Every answer here used to be a `tests/tmp_*.rs` file:
//! write it, compile it, read four lines, delete it — and the compile dominated. The analysis is the same either way;
//! what changes is that the question is an argument now, and the next question does not need a new crate target.

mod place;
mod project;
mod questions;

use clap::{Parser, Subcommand};

use place::Place;
use project::ProjectArgs;
use questions::{Answer, FactFilters};

#[derive(Parser)]
#[command(
    name = "inspect",
    about = "Ask the analysis one question about one project",
    version,
    after_help = "A position is `--at <byte>`, `--needle <text>` (+ `--occurrence`/`--last`/`--shift`), or `--line`/`--col`."
)]
struct Cli {
    #[command(flatten)]
    project: ProjectArgs,
    #[command(flatten)]
    place: Place,
    /// Print the answer as JSON, for a script rather than a reader.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The type of the expression at the position.
    Type,
    /// The hover card the server would show at the position.
    Hover,
    /// The declarations of the name at the position: the one the analysis picks, and all of them.
    Define,
    /// The members of a class, with the class that declares each and how many base steps away.
    Members {
        /// The class, as an editor would write it: `std::shared_ptr<int>`, `Widget`.
        #[arg(long)]
        class: String,
        /// How many to print; 0 is all of them.
        #[arg(long, default_value_t = 0)]
        limit: usize,
    },
    /// The macro in force for the name at the position.
    Macro,
    /// The completion labels offered at the position.
    Complete {
        /// How many to print; 0 is all of them.
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
    /// The facts a file's summary holds, filtered by name, kind or scope.
    Facts {
        /// Only facts whose name contains this.
        #[arg(long)]
        name: Option<String>,
        /// Only this kind: `type`, `function`, `variable`, `namespace`, `macro-like`, `other`.
        #[arg(long)]
        kind: Option<String>,
        /// Only facts whose scope contains this.
        #[arg(long)]
        scope: Option<String>,
        /// How many to print; 0 is all of them.
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
    /// The inlay hints an editor would draw for the file: parameter names and deduced types.
    Inlay,
    /// The findings a diagnostics channel would report for the file.
    Diagnostics,
    /// The summary's key, includes and counts.
    Summary,
    /// The syntax tree at the position, innermost node first.
    Tree {
        /// How many ancestors to print; 0 is all of them.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// The nodes of the syntax tree whose text contains this pattern — the whole tree, not a position.
    ///
    /// `tree` answers "what is around here", which cannot find a node the position does not contain: the question that
    /// needed this was whether a cooked translation unit's rendering holds a class declared in a header it includes.
    Grep {
        /// What the node's text must contain.
        pattern: String,
        /// How many nodes to print.
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
    /// The reading format the index on disk uses.
    Format,
    /// The session's own counters after the setup.
    Stats,
}

fn main() {
    let cli = Cli::parse();
    // **A worker thread with a large stack.** The analysis walks syntax trees and include graphs recursively, and a real
    // translation unit is deeper than a debug build's 1 MB main-thread stack survives — measured: `type` on
    // `templates.cpp` overflowed it. A release build does not, which is exactly the kind of difference a tool must not
    // have between two ways of running it.
    const THE_STACK: usize = 64 << 20;
    let worker = std::thread::Builder::new()
        .name("inspect".to_string())
        .stack_size(THE_STACK)
        .spawn(move || run(cli))
        .expect("a thread with a stack");
    match worker.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            eprintln!("inspect: {error}");
            std::process::exit(2);
        }
        Err(_) => {
            eprintln!("inspect: the worker panicked");
            std::process::exit(3);
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    // **A debug build answers the same questions, minutes later.** The analysis is tree walks and inference; optimised
    // out, a project whose closure reaches the standard library takes minutes instead of a second. Measured: `type` on
    // `test_cpp` did not finish in ten minutes in a debug build and answered in under a second in a release one. The
    // warning is printed rather than the tool refusing, because a small fixture is fine in debug and says so by
    // finishing.
    if cfg!(debug_assertions) {
        eprintln!(
            "[inspect] a debug build: answers that index a real project can take minutes. `cargo run --release -p inspect -- …` is the same tool at production speed."
        );
    }
    if let Command::Format = cli.command {
        let answer = questions::the_format();
        print(&answer, cli.json);
        return Ok(());
    }

    let opened = project::open(&cli.project)?;
    if cli.project.keep {
        eprintln!("[inspect] project root {}", opened.root.display());
    }
    if let Command::Stats = cli.command {
        println!("{}", questions::the_stats(&opened));
        return Ok(());
    }

    let answer = match &cli.command {
        Command::Type => questions::the_type(&opened, &cli.place)?,
        Command::Hover => questions::the_hover(&opened, &cli.place)?,
        Command::Define => questions::what_it_names(&opened, &cli.place)?,
        Command::Members { class, limit } => questions::the_members(&opened, class, *limit)?,
        Command::Macro => questions::the_macro(&opened, &cli.place)?,
        Command::Complete { limit } => questions::the_completions(&opened, &cli.place, *limit)?,
        Command::Facts {
            name,
            kind,
            scope,
            limit,
        } => questions::the_facts(
            &opened,
            &FactFilters {
                name: name.as_deref(),
                kind: kind.as_deref(),
                scope: scope.as_deref(),
                limit: *limit,
            },
        )?,
        Command::Inlay => questions::the_inlay(&opened)?,
        Command::Diagnostics => questions::the_diagnostics(&opened)?,
        Command::Summary => questions::the_summary(&opened)?,
        Command::Tree { limit } => questions::the_tree(&opened, &cli.place, *limit)?,
        Command::Grep { pattern, limit } => questions::the_grep(&opened, pattern, *limit)?,
        Command::Format | Command::Stats => unreachable!("handled above"),
    };
    print(&answer, cli.json);
    Ok(())
}

fn print(answer: &Answer, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&answer.value)
                .unwrap_or_else(|_| answer.value.to_string())
        );
    } else {
        print!("{}", answer.text);
        if !answer.text.ends_with('\n') {
            println!();
        }
    }
}
