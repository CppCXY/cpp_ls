use clap::Parser;
use cpp_ls::cmd_args::CmdArgs;
use mimalloc::MiMalloc;
use std::error::Error;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    let cmd_args = CmdArgs::parse();
    cpp_ls::run_ls(cmd_args).await
}
