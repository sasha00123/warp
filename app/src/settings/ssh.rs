use settings::macros::define_settings_group;
use settings::{RespectUserSyncSetting, SupportedPlatforms, SyncToCloud};

#[derive(
    Default,
    Debug,
    serde::Serialize,
    serde::Deserialize,
    PartialEq,
    Copy,
    Clone,
    strum_macros::EnumIter,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[serde(rename_all = "snake_case")]
pub enum PersistentHistoryRetention {
    #[default]
    UntilWorkspaceDeleted,
    Rolling,
}

impl PersistentHistoryRetention {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::UntilWorkspaceDeleted => "Until workspace deletion",
            Self::Rolling => "Most recent 64 MiB",
        }
    }

    pub fn to_proto(self) -> remote_server::proto::PersistentHistoryRetention {
        match self {
            Self::UntilWorkspaceDeleted => {
                remote_server::proto::PersistentHistoryRetention::UntilWorkspaceDeleted
            }
            Self::Rolling => remote_server::proto::PersistentHistoryRetention::Rolling,
        }
    }
}

define_settings_group!(SshSettings,
    settings: [
        persistent_history_retention: PersistentHistoryRetentionSetting {
            type: PersistentHistoryRetention,
            default: PersistentHistoryRetention::default(),
            supported_platforms: SupportedPlatforms::ALL,
            sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
            surface: settings::SettingSurfaces::GUI,
            private: false,
            storage_key: "PersistentHistoryRetention",
            toml_path: "warpify.ssh.persistent_history_retention",
            description: "Remote output retention for newly created persistent workspaces. Existing workspaces keep their original policy.",
        },
        reuse_existing_control_master: ReuseExistingSshControlMaster {
            type: bool,
            default: false,
            supported_platforms: SupportedPlatforms::ALL,
            sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
            surface: settings::SettingSurfaces::GUI,
            private: false,
            storage_key: "ReuseExistingSshControlMaster",
            toml_path: "warpify.ssh.reuse_existing_control_master",
            description: "Whether the legacy SSH wrapper attaches to an existing SSH ControlMaster for the destination host instead of always creating its own.",
        },
    ]
);

#[cfg(test)]
#[path = "ssh_tests.rs"]
mod tests;
