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
    // **A malformed `initialize` must be answered, not fatal.** It used to be `unwrap()`: a capability object that
    // this build of `lsp_types` cannot deserialize — a required field a newer client omits, a field renamed — took
    // the whole server down before it had answered anything, which an editor shows as "the language server crashed
    // on startup" with no way to know why. Measured with a real client shape: `textDocument.semanticTokens` without
    // `requests` panicked with `missing field 'requests'`. The protocol's answer to a request the server cannot
    // understand is an **error response with `InvalidParams`**, and the client stays alive to report it.
    let initialization_params: InitializeParams = match serde_json::from_value(params) {
        Ok(params) => params,
        Err(error) => {
            // `InvalidParams`, as the protocol defines it (JSON-RPC **-32602**), and **not** `initialize_finish`: that
            // sends a successful response, and a client reading `result` out of an error payload is the failure a
            // wrong code would produce. `::lsp_server` with the leading `::` because this module has a *local*
            // `lsp_server` (the thin wrapper over the crate) that would otherwise be the one named.
            connection.sender.send(::lsp_server::Message::Response(
                ::lsp_server::Response::new_err(
                    id,
                    -32602,
                    format!("cannot read initialize params: {error}"),
                ),
            ))?;
            return Ok(());
        }
    };
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
    main_loop::main_loop(async_connection, initialization_params, cmd_args).await?;

    // The I/O threads are deliberately **not** joined. They read and write the client's pipes, and the reader
    // blocks until the client closes stdin — so joining here would make a clean shutdown (`shutdown`, then `exit`)
    // hang until the client went away as well, which is the opposite of what `exit` asks for. Returning from `main`
    // is what closes the pipes; nothing after this point can be waiting for a message.
    drop(threads);

    eprintln!("Server shutting down.");
    Ok(())
}
