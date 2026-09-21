use serde::{Deserialize, Serialize};
use warp_core::features::FeatureFlag;
use warp_core::settings::Setting;
use warp_util::path::ShellFamily;

use crate::terminal::warpify::settings::WarpifySettings;

/// The different possible outcomes of detecting an interactive SSH session.
/// Also the payload for the [`crate::server::telemetry::TelemetryEvent::SshInteractiveSessionDetected`] event.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SshInteractiveSessionDetected {
    #[serde(rename = "feature_disabled")]
    FeatureDisabled,
    #[serde(rename = "host_denylisted")]
    HostDenylisted,
    #[serde(rename = "warpify_prompt")]
    ShouldPromptWarpification {
        #[serde(skip)]
        command: String,
        #[serde(skip)]
        host: Option<String>,
    },
}

/// Determines whether a host could be warpified.
pub fn evaluate_warpify_ssh_host(
    command: &str,
    ssh_host: Option<&str>,
    shell_family: ShellFamily,
    warpify_settings: &WarpifySettings,
) -> SshInteractiveSessionDetected {
    // PoC: force the tmux-based SSH wrapper so this build is easy to exercise without hidden
    // feature flags or settings UI state getting in the way.
    const FORCE_PERSISTENT_SSH_TMUX_POC: bool = true;
    let should_prompt_ssh_tmux_wrapper = FORCE_PERSISTENT_SSH_TMUX_POC
        || (*warpify_settings.enable_ssh_warpification.value()
            && *warpify_settings.use_ssh_tmux_wrapper.value()
            && FeatureFlag::SSHTmuxWrapper.is_enabled());
    let matches_subshell = warpify_settings.is_denylisted_subshell_command(command)
        || warpify_settings.is_compatible_subshell_command(command, shell_family);
    if !should_prompt_ssh_tmux_wrapper || matches_subshell {
        return SshInteractiveSessionDetected::FeatureDisabled;
    }

    if let Some(ssh_host) = ssh_host {
        if warpify_settings.is_ssh_host_denylisted(ssh_host) {
            return SshInteractiveSessionDetected::HostDenylisted;
        }
    }

    SshInteractiveSessionDetected::ShouldPromptWarpification {
        host: ssh_host.map(|host| host.to_owned()),
        command: command.to_string(),
    }
}
