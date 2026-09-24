fn main() -> std::io::Result<()> {
    if let Some(result) = persistent_workspace_rpc_harness::persistent_shell::run_shell_if_requested() {
        return result;
    }
    persistent_workspace_rpc_harness::persistent_journal::run_recorder_if_requested()
        .expect("The harness binary is only a journal recorder")
}
