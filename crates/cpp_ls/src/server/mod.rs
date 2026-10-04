mod connection;
mod error;
mod lsp_server;
mod main_loop;
mod message_processor;

pub use connection::AsyncConnection;
pub use error::ExitError;

use lsp_types::InitializeParams;
use std::error::Error;

use crate::cmd_args::{self, CmdArgs};
use crate::handlers::server_capabilities;

const CRATE_NAME: &str = env!("CARGO_PKG_NAME");
const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[allow(unused)]
pub async fn run_ls(cmd_args: CmdArgs) -> Result<(), Box<dyn Error + Sync + Send>> {
    let (connection, threads) = match cmd_args.communication {
        cmd_args::Communication::Stdio => ::lsp_server::Connection::stdio(),
        cmd_args::Communication::Tcp => {
            let port = cmd_args.port;
            let ip = cmd_args.ip.clone();
            let addr = (ip.as_str(), port);
            ::lsp_server::Connection::listen(addr).unwrap()
        }
    };

    let (id, params) = connection.initialize_start()?;
    let initialization_params: InitializeParams = serde_json::from_value(params).unwrap();
    let server_capabilities = server_capabilities(&initialization_params.capabilities);
    let initialize_data = serde_json::json!({
        "capabilities": server_capabilities,
        "serverInfo": {
            "name": CRATE_NAME,
            "version": CRATE_VERSION
        }
    });

    connection.initialize_finish(id, initialize_data)?;

    // Create async connection wrapper
    let async_connection = AsyncConnection::from_sync(connection);
    let ended = main_loop::main_loop(async_connection, initialization_params, cmd_args).await;

    // **Why this process is ending, in the log the user can send.**
    //
    // A server that is *dead* and a server that is *slow* look identical from inside an editor — completion stops,
    // hints stop, "everything stalls" — and the difference is the whole of the diagnosis. It was a report of exactly
    // that shape which sent this line here: the log's last entry was two minutes old and `Get-Process cpp_ls` found
    // no process at all, with nothing anywhere saying whether it had exited cleanly (the client closed the pipes),
    // returned an error, or was killed. The `eprintln!` below has always announced a clean shutdown, but stderr goes
    // to the client's output panel and is not in the file a user sends.
    match &ended {
        Ok(()) => log::info!("the main loop returned: the client closed the connection, or asked to exit"),
        Err(error) => log::error!("the main loop ended with an error: {error}"),
    }

    // The I/O threads are deliberately **not** joined. They read and write the client's pipes, and the reader
    // blocks until the client closes stdin — so joining here would make a clean shutdown (`shutdown`, then `exit`)
    // hang until the client went away as well, which is the opposite of what `exit` asks for. Returning from `main`
    // is what closes the pipes; nothing after this point can be waiting for a message.
    drop(threads);

    eprintln!("Server shutting down.");
    ended
}
