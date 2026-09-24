//! Dependency-free VM runner for the production Unix terminal transport.
//! Also compiled as a test crate and an rlib for the real-tmux integration test.
#[path = "../../crates/remote_server/src/persistent_journal.rs"]
pub mod persistent_journal;
#[path = "../../crates/remote_server/src/persistent_shell.rs"]
pub mod persistent_shell;
#[path = "../../crates/remote_server/src/persistent_workspace.rs"]
pub mod persistent_workspace;

#[allow(dead_code)]
fn main() -> std::io::Result<()> {
    if let Some(result) = persistent_shell::run_shell_if_requested() { return result; }
    persistent_journal::run_recorder_if_requested()
        .expect("This executable only runs the production journal recorder")
}
