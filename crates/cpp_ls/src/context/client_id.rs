use crate::cmd_args;
use lsp_types::ClientInfo;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClientId {
    VSCode,
    Intellij,
    Neovim,
    Zed,
    #[default]
    Other,
}

#[allow(unused)]
impl ClientId {
    pub fn is_vscode(&self) -> bool {
        matches!(self, ClientId::VSCode)
    }

    pub fn is_intellij(&self) -> bool {
        matches!(self, ClientId::Intellij)
    }

    pub fn is_neovim(&self) -> bool {
        matches!(self, ClientId::Neovim)
    }

    pub fn is_zed(&self) -> bool {
        matches!(self, ClientId::Zed)
    }

    pub fn is_other(&self) -> bool {
        matches!(self, ClientId::Other)
    }
}

/// **Which editor this session is for** — the `--editor` argument when a caller passed one, the client's own
/// `clientInfo` otherwise.
///
/// One function, because there used to be two answers: `LspServer::new` built `LspFeatures` from `clientInfo` alone while
/// the client-configuration path consulted `--editor`, so `--editor vscode` spoke with one identity where a *setting* is
/// chosen and another where a *command* is chosen — a VS Code lens command and a Neovim semantic-token list from the same
/// session (`docs/worklog.md` P2-2).
pub fn the_effective_client_id(
    editor: Option<&cmd_args::Editor>,
    client_info: &Option<ClientInfo>,
) -> ClientId {
    match editor {
        // Editor is Copy at its call sites through the value form; a shared reference is matched rather than
        // copied so the signature can take &Editor without requiring Copy here.
        Some(cmd_args::Editor::Vscode) => ClientId::VSCode,
        Some(cmd_args::Editor::Intellij) => ClientId::Intellij,
        Some(cmd_args::Editor::Neovim) => ClientId::Neovim,
        None => get_client_id(client_info),
    }
}

pub fn get_client_id(client_info: &Option<ClientInfo>) -> ClientId {
    match client_info {
        Some(info) => match info.name.as_str() {
            "Visual Studio Code" => ClientId::VSCode,
            "Neovim" | "coc.nvim" => ClientId::Neovim,
            _ if check_vscode(info) => ClientId::VSCode,
            _ if check_lsp4ij(info) => ClientId::Intellij,
            _ if check_zed(info) => ClientId::Zed,
            _ => ClientId::Other,
        },
        None => ClientId::Other,
    }
}

fn check_vscode(client_info: &ClientInfo) -> bool {
    let name = &client_info.name;

    if name.contains("Visual Studio Code")
        || name.contains("Code - OSS")
        || name.contains("VSCodium")
        || name.contains("Antigravity")
    {
        return true;
    }

    matches!(name.as_str(), "Cursor" | "Windsurf" | "Trae" | "Qoder")
}

fn check_lsp4ij(client_info: &ClientInfo) -> bool {
    let name = &client_info.name;

    name.contains("IntelliJ")
        || name.contains("JetBrains")
        || name.contains("IDEA")
        || name.contains("PyCharm")
        || name.contains("CLion")
        || name.contains("GoLand")
        || name.contains("Rider")
        || name.contains("Fleet")
        || name.contains("Android Studio")
}

fn check_zed(client_info: &ClientInfo) -> bool {
    let name = &client_info.name;

    name.contains("Zed")
}
