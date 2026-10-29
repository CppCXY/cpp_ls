use clap::Parser;
use cpp_ls::cmd_args::CmdArgs;
use mimalloc::MiMalloc;
use std::error::Error;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    let cmd_args = CmdArgs::parse();

    // **The runtime is built here rather than by `#[tokio::main]`, because the *end* of the process is the part that
    // needs a bound.** Dropping a runtime waits for its blocking tasks, and one of them can be a program being cooked
    // — a second of work after the client has already been told the server is shutting down. `shutdown_timeout` waits
    // for at most that long and then lets the process end. Measured before this: `shutdown` with the client holding
    // stdin open left the process alive, and the goodbye line showed the hang was not in the server's own code.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(cpp_ls::run_ls(cmd_args));
    runtime.shutdown_timeout(std::time::Duration::from_millis(250));
    result
}
