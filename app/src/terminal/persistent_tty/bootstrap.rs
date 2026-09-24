use anyhow::{Result, bail};
use remote_server::proto::PersistentShellBootstrap;
use warpui::AssetProvider;

use crate::terminal::shell::ShellType;

/// Send the same bundled assets used by ordinary Warp shells. The remote
/// creation worker fills the incarnation-specific session token and sources
/// these once, after its output recorder is ready.
pub fn profile_for_shell(shell_type: ShellType) -> Result<PersistentShellBootstrap> {
    let (shell, init_path) = match shell_type {
        ShellType::Bash => ("bash", "bundled/bootstrap/bash_init_shell.sh"),
        ShellType::Zsh => ("zsh", "bundled/bootstrap/zsh_init_shell.sh"),
        ShellType::Fish => ("fish", "bundled/bootstrap/fish_init_shell.sh"),
        ShellType::PowerShell => bail!("Persistent Unix workspaces do not support PowerShell"),
    };
    Ok(PersistentShellBootstrap {
        shell: shell.into(),
        init_script: crate::ASSETS.get(init_path)?.to_vec(),
        bootstrap_script: crate::terminal::bootstrap::script_for_shell(shell_type, &crate::ASSETS).into_owned(),
    })
}
