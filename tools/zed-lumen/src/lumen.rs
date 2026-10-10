//! Zed extension for Lumen: registers the `.lmn` language and starts
//! `lumen-lsp` for it.
//!
//! The server is found the way the other Lumen editor integrations find it:
//! an explicit path in Zed settings wins, then `lumen-lsp` on `$PATH`, then
//! the `lumen-lsp` beside the `lumenc` on `$PATH`, which is where every Lumen
//! toolchain archive puts it. Zed extensions run in a sandbox with no access
//! to project files other than through the worktree, so there is no probing
//! of Cargo target directories here; point `binary.path` at a locally built
//! server instead.

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

        let command = worktree
            .which(SERVER_NAME)
            .or_else(|| worktree.which("lumenc").map(|lumenc| beside(&lumenc)))
            .ok_or_else(|| {
                format!(
                    "{SERVER_NAME} was not found on $PATH or beside lumenc. It ships \
                     with the Lumen toolchain; install Lumen, or set \
                     lsp.{SERVER_NAME}.binary.path in your Zed settings."
                )
            })?;

        Ok(zed::Command {
            command,
            args: args.unwrap_or_default(),
            env: worktree.shell_env(),
        })
    }
}

/// The server's path in the directory holding `lumenc`. Split by hand rather
/// than through `std::path`, because the extension runs as WebAssembly and
/// the path it is handed may be a Windows one.
fn beside(lumenc: &str) -> String {
    let (dir, exe) = match lumenc.rfind(['/', '\\']) {
        Some(at) => (&lumenc[..=at], &lumenc[at + 1..]),
        None => ("", lumenc),
    };
    let suffix = if exe.to_ascii_lowercase().ends_with(".exe") {
        ".exe"
    } else {
        ""
    };
    format!("{dir}{SERVER_NAME}{suffix}")
}

zed::register_extension!(LumenExtension);

#[cfg(test)]
mod tests {
    use super::beside;

    #[test]
    fn the_server_is_looked_for_in_lumenc_s_own_directory() {
        assert_eq!(beside("/home/u/.lumen/bin/lumenc"), "/home/u/.lumen/bin/lumen-lsp");
        assert_eq!(
            beside(r"C:\Users\u\AppData\Local\Programs\Lumen\bin\lumenc.exe"),
            r"C:\Users\u\AppData\Local\Programs\Lumen\bin\lumen-lsp.exe"
        );
        assert_eq!(beside("lumenc"), "lumen-lsp");
    }
}
