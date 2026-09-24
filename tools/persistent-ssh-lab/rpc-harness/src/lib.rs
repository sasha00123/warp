//! Compiles the production workspace backend and protocol without the GUI dependency graph.

#[allow(clippy::large_enum_variant)]
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/remote_server.rs"));
}

#[path = "../../../../crates/remote_server/src/persistent_journal.rs"]
pub mod persistent_journal;
#[path = "../../../../crates/remote_server/src/persistent_replay.rs"]
pub mod persistent_replay;
#[path = "../../../../crates/remote_server/src/persistent_shell.rs"]
pub mod persistent_shell;
#[path = "../../../../crates/remote_server/src/persistent_workspace.rs"]
pub mod persistent_workspace;
#[path = "../../../../crates/remote_server/src/persistent_workspace_rpc.rs"]
pub mod persistent_workspace_rpc;

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod tests;
