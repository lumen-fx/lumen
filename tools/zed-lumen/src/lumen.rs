//! Zed extension for Lumen: registers the `.lmn` language and starts
//! `lumen-lsp` for it.
//!
//! The server is found the way the other Lumen editor integrations find it:
//! an explicit path in Zed settings wins, then `lumen-lsp` on `$PATH`. Zed
//! extensions run in a sandbox with no access to project files other than
//! through the worktree, so there is no probing of Cargo target directories
//! here; point `binary.path` at a locally built server instead.

use zed_extension_api::{self as zed, settings::LspSettings, LanguageServerId, Result};

const SERVER_NAME: &str = "lumen-lsp";

struct LumenExtension;

impl zed::Extension for LumenExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let mut args = None;

        if let Ok(settings) = LspSettings::for_worktree(SERVER_NAME, worktree) {
            if let Some(binary) = settings.binary {
                args = binary.arguments;
                if let Some(path) = binary.path {
                    return Ok(zed::Command {
                        command: path,
                        args: args.unwrap_or_default(),
                        env: worktree.shell_env(),
                    });
                }
            }
        }

        let command = worktree.which(SERVER_NAME).ok_or_else(|| {
            format!(
                "{SERVER_NAME} was not found on $PATH. Build it with \
                 `cargo build --release -p lumen-lsp` and put the binary on the \
                 path, or set lsp.{SERVER_NAME}.binary.path in your Zed settings."
            )
        })?;

        Ok(zed::Command {
            command,
            args: args.unwrap_or_default(),
            env: worktree.shell_env(),
        })
    }
}

zed::register_extension!(LumenExtension);
